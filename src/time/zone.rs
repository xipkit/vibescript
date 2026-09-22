// TZif loading and transition selection adapt the Go standard library's time package.
// Copyright 2009 The Go Authors. All rights reserved.
// See licenses/Go-BSD-3-Clause.txt.

use super::calendar;
use crate::{
    CallContext, Error, ErrorKind, Result, Value,
    budget::{Buffer, Charge},
};
use std::{
    fs::File,
    io::Cursor,
    mem::size_of,
    path::{Path, PathBuf},
    sync::{Arc, OnceLock},
};

mod rules;
mod source;
#[cfg(any(windows, test))]
mod windows;

#[cfg(not(any(
    windows,
    target_os = "android",
    target_os = "ios",
    target_family = "wasm"
)))]
const SOURCES: [&str; 4] = [
    "/usr/share/zoneinfo",
    "/usr/share/lib/zoneinfo",
    "/usr/lib/locale/TZ",
    "/etc/zoneinfo",
];
#[cfg(any(windows, target_os = "ios", target_family = "wasm"))]
const SOURCES: [&str; 0] = [];
#[cfg(target_os = "android")]
const SOURCES: [&str; 2] = [
    "/system/usr/share/zoneinfo/tzdata",
    "/data/misc/zoneinfo/current/tzdata",
];
const MAX_ZONE_BYTES: usize = 10 << 20;
const BUNDLED: &[u8] = include_bytes!("zone/data/zoneinfo.zip");

static ZONEINFO: OnceLock<Option<std::ffi::OsString>> = OnceLock::new();
#[cfg(not(any(
    windows,
    target_os = "android",
    target_os = "ios",
    all(target_family = "wasm", not(target_os = "wasi"))
)))]
static LOCAL_TZ: OnceLock<Option<std::ffi::OsString>> = OnceLock::new();

#[derive(Clone, Copy, Debug)]
struct Tzif {
    width: usize,
    times: usize,
    count: usize,
    indices: usize,
    zones: usize,
    zone_count: usize,
    names: usize,
    name_count: usize,
    first: usize,
}

#[derive(Debug)]
pub(super) struct Zone {
    bytes: Value,
    info: Option<Tzif>,
    rules: Option<rules::Rules>,
    offset: i32,
    header: Option<Charge>,
}

#[derive(Clone, Copy)]
pub(super) struct Offset<'a> {
    pub name: &'a [u8],
    pub seconds: i32,
    pub dst: bool,
    pub start: i64,
    pub end: i64,
}

impl<'a> Offset<'a> {
    pub fn utc() -> Self {
        Self {
            name: b"UTC",
            seconds: 0,
            dst: false,
            start: i64::MIN,
            end: i64::MAX,
        }
    }
}

fn invalid() -> Error {
    Error::new(ErrorKind::Argument, "invalid timezone")
}

fn int(bytes: &[u8], index: usize, width: usize) -> i64 {
    if width == 8 {
        i64::from_be_bytes(bytes[index..index + 8].try_into().unwrap())
    } else {
        i64::from(i32::from_be_bytes(
            bytes[index..index + 4].try_into().unwrap(),
        ))
    }
}

impl Tzif {
    fn parse(ctx: &mut CallContext, bytes: &[u8]) -> Result<(Self, Option<rules::Rules>)> {
        if bytes.len() < 44 || &bytes[..4] != b"TZif" || !matches!(bytes[4], 0 | b'2' | b'3') {
            return Err(invalid());
        }
        let counts = |start: usize| -> Result<[usize; 6]> {
            let raw = bytes.get(start..start + 24).ok_or_else(invalid)?;
            Ok(std::array::from_fn(|i| {
                u32::from_be_bytes(raw[i * 4..i * 4 + 4].try_into().unwrap()) as usize
            }))
        };
        let length = |n: [usize; 6], width: usize| -> Result<usize> {
            let total = n[0] as u64
                + n[1] as u64
                + n[2] as u64 * (width as u64 + 4)
                + n[3] as u64 * (width as u64 + 1)
                + n[4] as u64 * 6
                + n[5] as u64;
            usize::try_from(total).map_err(|_| invalid())
        };
        let mut n = counts(20)?;
        let (start, width) = if bytes[4] == 0 {
            (44usize, 4)
        } else {
            let header = 44usize.checked_add(length(n, 4)?).ok_or_else(invalid)?;
            n = counts(header.checked_add(20).ok_or_else(invalid)?)?;
            (header.checked_add(44).ok_or_else(invalid)?, 8)
        };
        let end = start.checked_add(length(n, width)?).ok_or_else(invalid)?;
        if end > bytes.len() || n[4] == 0 {
            return Err(invalid());
        }
        let mut info = Self {
            width,
            times: start,
            count: n[3],
            indices: start + n[3] * width,
            zones: start + n[3] * (width + 1),
            zone_count: n[4],
            names: start + n[3] * (width + 1) + n[4] * 6,
            name_count: n[5],
            first: 0,
        };
        let first_transition = if info.count == 0 {
            0
        } else {
            bytes[info.indices] as usize
        };
        let mut first_standard = None;
        let mut before_transition = None;
        for i in 0..info.zone_count {
            ctx.charge(1)?;
            if bytes[info.zones + i * 6 + 5] as usize >= info.name_count {
                return Err(invalid());
            }
            if !info.dst(bytes, i) {
                first_standard.get_or_insert(i);
                if i < first_transition {
                    before_transition = Some(i);
                }
            }
        }
        let mut zero_used = false;
        for i in 0..info.count {
            ctx.charge(1)?;
            let zone = bytes[info.indices + i] as usize;
            if zone >= info.zone_count {
                return Err(invalid());
            }
            zero_used |= zone == 0;
        }
        if zero_used {
            let before_dst = if info.dst(bytes, first_transition) {
                before_transition
            } else {
                None
            };
            info.first = before_dst.or(first_standard).unwrap_or(0);
        }
        let extension =
            if bytes.len() > end + 2 && bytes[end] == b'\n' && bytes.last() == Some(&b'\n') {
                rules::Rules::parse(ctx, bytes, end + 1, bytes.len() - 1)?
            } else {
                None
            };
        Ok((info, extension))
    }

    fn dst(self, bytes: &[u8], index: usize) -> bool {
        bytes[self.zones + index * 6 + 4] != 0
    }
    fn when(self, bytes: &[u8], index: usize) -> i64 {
        int(bytes, self.times + index * self.width, self.width)
    }

    fn offset<'a>(
        self,
        ctx: &mut CallContext,
        bytes: &'a [u8],
        index: usize,
    ) -> Result<Offset<'a>> {
        let record = self.zones + index * 6;
        let start = self.names + bytes[record + 5] as usize;
        let names = &bytes[start..self.names + self.name_count];
        let mut length = 0;
        for chunk in names.chunks(1024) {
            ctx.charge(1)?;
            ctx.checkpoint()?;
            if let Some(i) = chunk.iter().position(|&b| b == 0) {
                length += i;
                break;
            }
            length += chunk.len();
        }
        Ok(Offset {
            name: &names[..length],
            seconds: int(bytes, record, 4) as i32,
            dst: self.dst(bytes, index),
            start: i64::MIN,
            end: i64::MAX,
        })
    }
}

impl Zone {
    fn new(
        ctx: &mut CallContext,
        bytes: Value,
        info: Option<Tzif>,
        rules: Option<rules::Rules>,
        offset: i32,
    ) -> Result<Arc<Self>> {
        let header = ctx.reserve(size_of::<Self>() + 2 * size_of::<usize>())?;
        Ok(Arc::new(Self {
            bytes,
            info,
            rules,
            offset,
            header,
        }))
    }

    pub fn fixed(ctx: &mut CallContext, name: &[u8], offset: i32) -> Result<Arc<Self>> {
        let bytes = ctx.bytes(name)?;
        Self::new(ctx, bytes, None, None, offset)
    }

    pub fn import(ctx: &mut CallContext, zone: &Arc<Self>) -> Result<Arc<Self>> {
        if ctx.owns(&zone.header) || ctx.options.limits.memory_bytes.is_none() {
            return Ok(zone.clone());
        }
        let bytes = ctx.import(&zone.bytes)?;
        Self::new(ctx, bytes, zone.info, zone.rules.clone(), zone.offset)
    }

    fn from_buffer(ctx: &mut CallContext, buffer: Buffer<u8>) -> Result<Option<Arc<Self>>> {
        let (info, rules) = match Tzif::parse(ctx, &buffer.data) {
            Ok(parsed) => parsed,
            Err(error) if error.kind == ErrorKind::Argument => return Ok(None),
            Err(error) => return Err(error),
        };
        let bytes = Value::from_bytes(ctx, buffer)?;
        Self::new(ctx, bytes, Some(info), rules, 0).map(Some)
    }

    fn read(
        ctx: &mut CallContext,
        path: &Path,
        format: source::Format,
        name: &[u8],
    ) -> Result<Option<Arc<Self>>> {
        ctx.checkpoint()?;
        let Ok(file) = File::open(path) else {
            return Ok(None);
        };
        let Ok(metadata) = file.metadata() else {
            return Ok(None);
        };
        if !metadata.is_file() {
            return Ok(None);
        }
        let mut source = source::Source::new(file, metadata.len());
        match source.load(ctx, format, name)? {
            Some(buffer) => Self::from_buffer(ctx, buffer),
            None => Ok(None),
        }
    }

    fn file(ctx: &mut CallContext, path: &Path) -> Result<Option<Arc<Self>>> {
        Self::read(ctx, path, source::Format::File, b"")
    }

    fn dir_or_zip(ctx: &mut CallContext, root: &Path, name: &[u8]) -> Result<Option<Arc<Self>>> {
        let bytes = root.as_os_str().as_encoded_bytes();
        if bytes.len() > 4 && bytes.ends_with(b".zip") {
            return Self::read(ctx, root, source::Format::Zip, name);
        }
        #[cfg(unix)]
        let name = {
            use std::os::unix::ffi::OsStrExt;
            std::ffi::OsStr::from_bytes(name)
        };
        #[cfg(windows)]
        let normalized;
        #[cfg(windows)]
        let _name_charge;
        #[cfg(windows)]
        let name = match std::str::from_utf8(name) {
            Ok(text) => std::ffi::OsStr::new(text),
            Err(_) => {
                use std::os::windows::ffi::OsStringExt;
                let units = windows::filename_units(ctx, name)?;
                // Reserve the geometric growth bound before Rust converts UTF-16 to its OS string.
                let reservation = ctx.reserve(units.data.len().saturating_mul(6).max(8))?;
                normalized = std::ffi::OsString::from_wide(&units.data);
                drop(reservation);
                _name_charge = ctx.reserve(normalized.capacity())?;
                normalized.as_os_str()
            }
        };
        #[cfg(not(any(unix, windows)))]
        let normalized;
        #[cfg(not(any(unix, windows)))]
        let name = {
            let text = match std::str::from_utf8(name) {
                Ok(text) => text,
                Err(_) => {
                    let mut buffer = Buffer::empty();
                    for chunk in name.utf8_chunks() {
                        buffer.extend(ctx, chunk.valid().as_bytes())?;
                        for _ in chunk.invalid() {
                            buffer.extend(ctx, "�".as_bytes())?;
                        }
                    }
                    normalized = buffer;
                    std::str::from_utf8(&normalized.data).unwrap()
                }
            };
            std::ffi::OsStr::new(text)
        };
        let capacity = root
            .as_os_str()
            .len()
            .saturating_add(name.len())
            .saturating_add(1);
        let mut reservation = ctx.reserve(capacity)?;
        let mut path = PathBuf::with_capacity(capacity);
        if path.capacity() != capacity {
            reservation = ctx.reserve(path.capacity())?;
        }
        path.push(root);
        if !root.as_os_str().is_empty() {
            path.as_mut_os_string().push("/");
        }
        path.as_mut_os_string().push(name);
        let result = Self::file(ctx, &path)?;
        drop(path);
        drop(reservation);
        Ok(result)
    }

    fn bundled(ctx: &mut CallContext, name: &[u8]) -> Result<Option<Arc<Self>>> {
        let mut source = source::Source::new(Cursor::new(BUNDLED), BUNDLED.len() as u64);
        match source.zip(ctx, name)? {
            Some(buffer) => Self::from_buffer(ctx, buffer),
            None => Ok(None),
        }
    }

    fn search(ctx: &mut CallContext, name: &[u8], custom: bool) -> Result<Option<Arc<Self>>> {
        if custom {
            let env = ZONEINFO.get_or_init(|| std::env::var_os("ZONEINFO"));
            let _config = ctx.reserve(env.as_ref().map_or(0, |v| v.len()))?;
            if let Some(root) = env.as_deref().filter(|s| !s.is_empty()) {
                if let Some(zone) = Self::dir_or_zip(ctx, Path::new(root), name)? {
                    return Ok(Some(zone));
                }
            }
        }
        for root in SOURCES {
            #[cfg(target_os = "android")]
            let result = Self::read(ctx, Path::new(root), source::Format::Android, name)?;
            #[cfg(not(target_os = "android"))]
            let result = Self::dir_or_zip(ctx, Path::new(root), name)?;
            if result.is_some() {
                return Ok(result);
            }
        }
        #[cfg(target_os = "ios")]
        if let Some(zone) = Self::read(ctx, Path::new("zoneinfo.zip"), source::Format::Zip, name)? {
            return Ok(Some(zone));
        }
        Self::bundled(ctx, name)
    }

    #[cfg(windows)]
    pub fn local(ctx: &mut CallContext) -> Result<Arc<Self>> {
        windows::local(ctx)
    }

    #[cfg(any(
        target_os = "android",
        target_os = "ios",
        all(target_family = "wasm", not(target_os = "wasi"))
    ))]
    pub fn local(ctx: &mut CallContext) -> Result<Arc<Self>> {
        Self::fixed(ctx, b"UTC", 0)
    }

    /// Unix and WASI select the local zone from `TZ`. A WASI guest has no
    /// system zone, so it uses UTC when `TZ` is unset rather than reading an
    /// `/etc/localtime` that a host happened to expose.
    #[cfg(not(any(
        windows,
        target_os = "android",
        target_os = "ios",
        all(target_family = "wasm", not(target_os = "wasi"))
    )))]
    pub fn local(ctx: &mut CallContext) -> Result<Arc<Self>> {
        let tz = LOCAL_TZ.get_or_init(|| std::env::var_os("TZ"));
        let _config = ctx.reserve(tz.as_ref().map_or(0, |v| v.len()))?;
        if tz.is_none() {
            #[cfg(not(target_os = "wasi"))]
            if let Some(zone) = Self::file(ctx, Path::new("/etc/localtime"))? {
                return Ok(zone);
            }
        } else if let Some(tz) = tz.as_ref() {
            let tz = tz.as_encoded_bytes();
            let tz = tz.strip_prefix(b":").unwrap_or(tz);
            if tz.starts_with(b"/") {
                #[cfg(any(unix, target_os = "wasi"))]
                let path = {
                    #[cfg(unix)]
                    use std::os::unix::ffi::OsStrExt;
                    #[cfg(target_os = "wasi")]
                    use std::os::wasi::ffi::OsStrExt;
                    Some(Path::new(std::ffi::OsStr::from_bytes(tz)))
                };
                #[cfg(not(any(unix, target_os = "wasi")))]
                let path = std::str::from_utf8(tz).ok().map(Path::new);
                if let Some(zone) = match path {
                    Some(path) => Self::file(ctx, path)?,
                    None => None,
                } {
                    return Ok(zone);
                }
            } else if !tz.is_empty() && tz != b"UTC" {
                if let Some(zone) = Self::search(ctx, tz, false)? {
                    return Ok(zone);
                }
            }
        }
        Self::fixed(ctx, b"UTC", 0)
    }

    pub fn parse(ctx: &mut CallContext, value: &Value) -> Result<Option<Arc<Self>>> {
        use crate::value::Kind;
        let bytes = match &value.0 {
            Kind::Nil => return Ok(None),
            Kind::Bytes(b) => b.data.as_slice(),
            _ => return Err(Error::new(ErrorKind::Argument, "invalid timezone spec")),
        };
        // Go quotes the unknown name; the scan below has already charged for reading it.
        let unknown = || {
            let mut message = b"invalid timezone ".to_vec();
            crate::shapes::quote(bytes, &mut message);
            Error::new(
                ErrorKind::Argument,
                String::from_utf8_lossy(&message).into_owned(),
            )
        };
        let mut previous = 0;
        for chunk in bytes.chunks(1024) {
            ctx.charge(1)?;
            ctx.checkpoint()?;
            for &byte in chunk {
                if byte == b'.' && previous == b'.' {
                    return Err(unknown());
                }
                previous = byte;
            }
        }
        // Installed zone databases use filesystem paths; longer paths cannot name an entry.
        if bytes.len() > 4096 {
            return Err(unknown());
        }
        if bytes.is_empty()
            || [b"UTC".as_slice(), b"GMT", b"Z"]
                .iter()
                .any(|key| bytes.eq_ignore_ascii_case(key))
        {
            return Ok(None);
        }
        if bytes.eq_ignore_ascii_case(b"LOCAL") {
            return Self::local(ctx).map(Some);
        }
        if bytes.len() == 6 && matches!(bytes[0], b'+' | b'-') && bytes[3] == b':' {
            let pair = |part: &[u8]| -> Result<i32> {
                std::str::from_utf8(part)
                    .ok()
                    .and_then(|s| s.parse().ok())
                    .ok_or_else(|| Error::new(ErrorKind::Argument, "invalid timezone offset"))
            };
            let offset = (pair(&bytes[1..3])? * 3600 + pair(&bytes[4..])? * 60)
                * if bytes[0] == b'-' { -1 } else { 1 };
            return Self::fixed(ctx, bytes, offset).map(Some);
        }
        if matches!(bytes.first(), Some(b'/' | b'\\')) {
            return Err(unknown());
        }
        Self::search(ctx, bytes, true)?
            .map(Some)
            .ok_or_else(unknown)
    }

    pub fn lookup(&self, ctx: &mut CallContext, seconds: i64) -> Result<Offset<'_>> {
        ctx.charge(1)?;
        let bytes = self.bytes.as_bytes().unwrap();
        let Some(info) = self.info else {
            return Ok(Offset {
                name: bytes,
                seconds: self.offset,
                ..Offset::utc()
            });
        };
        if info.count != 0 && seconds < info.when(bytes, 0) {
            return Ok(Offset {
                end: info.when(bytes, 0),
                ..info.offset(ctx, bytes, info.first)?
            });
        }
        let (mut lo, mut hi) = (0, info.count);
        let mut end = i64::MAX;
        while hi.saturating_sub(lo) > 1 {
            ctx.charge(1)?;
            let mid = lo + (hi - lo) / 2;
            let when = info.when(bytes, mid);
            if seconds < when {
                end = when;
                hi = mid;
            } else {
                lo = mid;
            }
        }
        let start = if info.count == 0 {
            i64::MIN
        } else {
            info.when(bytes, lo)
        };
        if info.count == 0 || lo == info.count - 1 {
            if let Some(rules) = &self.rules {
                return Ok(rules.lookup(bytes, start, seconds));
            }
        }
        let index = if info.count == 0 {
            0
        } else {
            bytes[info.indices + lo] as usize
        };
        Ok(Offset {
            start,
            end,
            ..info.offset(ctx, bytes, index)?
        })
    }

    pub fn calendar(&self, ctx: &mut CallContext, parts: [i64; 6]) -> Result<i64> {
        let mut unix = calendar::normalized(parts);
        let offset = self.lookup(ctx, unix)?;
        if offset.seconds != 0 {
            let utc = unix.wrapping_sub(i64::from(offset.seconds));
            let seconds = if utc < offset.start || utc >= offset.end {
                self.lookup(ctx, utc)?.seconds
            } else {
                offset.seconds
            };
            unix = unix.wrapping_sub(i64::from(seconds));
        }
        Ok(unix)
    }

    pub fn equal_name(ctx: &mut CallContext, left: &[u8], right: &[u8]) -> Result<bool> {
        if left.len() != right.len() {
            return Ok(false);
        }
        for (a, b) in left.chunks(1024).zip(right.chunks(1024)) {
            ctx.charge(1)?;
            ctx.checkpoint()?;
            if a != b {
                return Ok(false);
            }
        }
        Ok(true)
    }

    pub fn lookup_name(
        &self,
        ctx: &mut CallContext,
        name: &[u8],
        seconds: i64,
    ) -> Result<Option<i32>> {
        let bytes = self.bytes.as_bytes().unwrap();
        let Some(info) = self.info else {
            return Ok(Self::equal_name(ctx, bytes, name)?.then_some(self.offset));
        };
        let mut fallback = None;
        for index in 0..info.zone_count {
            ctx.charge(1)?;
            let candidate = info.offset(ctx, bytes, index)?;
            if Self::equal_name(ctx, candidate.name, name)? {
                fallback.get_or_insert(candidate.seconds);
                let active =
                    self.lookup(ctx, seconds.wrapping_sub(i64::from(candidate.seconds)))?;
                if Self::equal_name(ctx, active.name, name)? {
                    return Ok(Some(active.seconds));
                }
            }
        }
        Ok(fallback)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CallOptions, Limits};

    #[test]
    fn bundled_zones_match_the_pinned_go_database_at_all_recorded_instants() {
        let cases: serde_json::Value =
            serde_json::from_str(include_str!("../../tests/timezone-cases.json")).unwrap();
        assert_eq!(cases.as_array().unwrap().len(), 598);
        for case in cases.as_array().unwrap() {
            let name = case["name"].as_str().unwrap();
            let mut ctx = CallContext::new(CallOptions::default());
            let zone = Zone::bundled(&mut ctx, name.as_bytes()).unwrap().unwrap();
            for sample in case["samples"].as_array().unwrap() {
                let instant = sample[0].as_i64().unwrap();
                let actual = zone.lookup(&mut ctx, instant).unwrap();
                assert_eq!(
                    i64::from(actual.seconds),
                    sample[1].as_i64().unwrap(),
                    "{name} at {instant}"
                );
                // The language's canonical GMT spelling selects UTC before a database lookup.
                let expected_name = if name == "GMT" {
                    b"GMT".as_slice()
                } else {
                    sample[2].as_str().unwrap().as_bytes()
                };
                assert_eq!(actual.name, expected_name, "{name} at {instant}");
                assert_eq!(
                    actual.dst,
                    sample[3].as_bool().unwrap(),
                    "{name} at {instant}"
                );
            }
            let size = zone.bytes.as_bytes().unwrap().len();
            assert!(
                ctx.stats().peak_memory_bytes < size + 512,
                "{name}: {:?}",
                ctx.stats()
            );
            drop(zone);
            assert_eq!(ctx.stats().retained_memory_bytes, 0);
        }
    }

    #[test]
    fn fallback_loading_propagates_limits_and_never_caches_failed_calls() {
        for (limits, kind) in [
            (
                Limits {
                    steps: Some(32),
                    ..Limits::default()
                },
                ErrorKind::Steps,
            ),
            (
                Limits {
                    memory_bytes: Some(0),
                    ..Limits::default()
                },
                ErrorKind::Memory,
            ),
        ] {
            let mut ctx = CallContext::new(CallOptions {
                limits,
                ..CallOptions::default()
            });
            assert_eq!(
                Zone::bundled(&mut ctx, b"Pacific/Auckland")
                    .unwrap_err()
                    .kind,
                kind
            );
            assert_eq!(ctx.stats().retained_memory_bytes, 0);
            assert_eq!(ctx.charge(0).unwrap_err().kind, kind);
        }
        let mut ctx = CallContext::new(CallOptions::default());
        assert!(
            Zone::bundled(&mut ctx, b"Pacific/Auckland")
                .unwrap()
                .is_some()
        );
        assert!(Zone::bundled(&mut ctx, b"Not/AZone").unwrap().is_none());
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }

    fn data(offset: i32, extension: &str) -> Vec<u8> {
        let mut bytes = vec![0; 44];
        bytes[..4].copy_from_slice(b"TZif");
        bytes[36..40].copy_from_slice(&1u32.to_be_bytes());
        bytes[40..44].copy_from_slice(&4u32.to_be_bytes());
        bytes.extend_from_slice(&offset.to_be_bytes());
        bytes.extend_from_slice(&[0, 0]);
        bytes.extend_from_slice(b"STD\0");
        if !extension.is_empty() {
            bytes.push(b'\n');
            bytes.extend_from_slice(extension.as_bytes());
            bytes.push(b'\n');
        }
        bytes
    }

    #[test]
    fn independent_transition_instants_cover_both_hemispheres() {
        for (extension, cases) in [
            (
                "STD0DST,M3.2.0/2,M11.1.0/2",
                vec![
                    (1710035999, 0, false),
                    (1710036000, 3600, true),
                    (1730595599, 3600, true),
                    (1730595600, 0, false),
                ],
            ),
            (
                "AEST-10AEDT,M10.1.0,M4.1.0/3",
                vec![(1704067200, 39600, true), (1719792000, 36000, false)],
            ),
        ] {
            let bytes = data(0, extension);
            let mut ctx = CallContext::new(CallOptions::default());
            let (info, rules) = Tzif::parse(&mut ctx, &bytes).unwrap();
            let bytes = ctx.bytes(&bytes).unwrap();
            let zone = Zone::new(&mut ctx, bytes, Some(info), rules, 0).unwrap();
            for (seconds, expected, dst) in cases {
                let actual = zone.lookup(&mut ctx, seconds).unwrap();
                assert_eq!(
                    (actual.seconds, actual.dst),
                    (expected, dst),
                    "{extension} at {seconds}"
                );
            }
        }
    }

    #[test]
    fn malformed_zone_counts_and_indices_return_errors_without_panicking() {
        let original = data(0, "");
        for len in 0..original.len() {
            let mut ctx = CallContext::new(CallOptions::default());
            assert!(
                Tzif::parse(&mut ctx, &original[..len]).is_err(),
                "length {len}"
            );
        }
        for field in [20, 24, 28, 32, 36, 40] {
            let mut bytes = original.clone();
            bytes[field..field + 4].copy_from_slice(&u32::MAX.to_be_bytes());
            let mut ctx = CallContext::new(CallOptions::default());
            assert!(Tzif::parse(&mut ctx, &bytes).is_err(), "count at {field}");
        }
        let mut bytes = original.clone();
        bytes[49] = 4;
        let mut ctx = CallContext::new(CallOptions::default());
        assert!(Tzif::parse(&mut ctx, &bytes).is_err());
    }

    #[test]
    fn zone_parsing_and_name_lookup_bound_work() {
        let mut bytes = data(0, "");
        bytes[36..40].copy_from_slice(&10000u32.to_be_bytes());
        bytes.truncate(44);
        bytes.extend_from_slice(&[0; 60000]);
        bytes.extend_from_slice(b"STD\0");
        let options = CallOptions {
            limits: Limits {
                steps: Some(64),
                ..Limits::default()
            },
            ..CallOptions::default()
        };
        let mut ctx = CallContext::new(options.clone());
        assert_eq!(
            Tzif::parse(&mut ctx, &bytes).unwrap_err().kind,
            ErrorKind::Steps
        );

        let bytes = data(0, &format!("STD{}0", "a".repeat(131072)));
        let mut ctx = CallContext::new(options.clone());
        assert_eq!(
            Tzif::parse(&mut ctx, &bytes).unwrap_err().kind,
            ErrorKind::Steps
        );

        let mut bytes = data(0, "");
        bytes[40..44].copy_from_slice(&131072u32.to_be_bytes());
        bytes.truncate(50);
        bytes.resize(50 + 131072, b'a');
        let mut setup = CallContext::new(CallOptions::default());
        let (info, rules) = Tzif::parse(&mut setup, &bytes).unwrap();
        let bytes = setup.bytes(&bytes).unwrap();
        let zone = Zone::new(&mut setup, bytes, Some(info), rules, 0).unwrap();
        let mut ctx = CallContext::new(options);
        assert!(matches!(zone.lookup(&mut ctx, 0), Err(error) if error.kind == ErrorKind::Steps));
    }

    #[test]
    fn timezone_import_charges_full_host_capacity_and_releases_it() {
        let mut bytes = Vec::with_capacity(65536);
        bytes.extend_from_slice(&data(3600, ""));
        let mut setup = CallContext::new(CallOptions::default());
        let (info, rules) = Tzif::parse(&mut setup, &bytes).unwrap();
        let host = Arc::new(Zone {
            bytes: Value::bytes(bytes),
            info: Some(info),
            rules,
            offset: 0,
            header: None,
        });
        let mut ctx = CallContext::new(CallOptions::default());
        let imported = Zone::import(&mut ctx, &host).unwrap();
        assert!(ctx.stats().retained_memory_bytes >= 65536);
        let clone = Zone::import(&mut ctx, &imported).unwrap();
        drop(imported);
        assert!(ctx.stats().retained_memory_bytes >= 65536);
        drop(clone);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
        let mut small = CallContext::new(CallOptions {
            limits: Limits {
                memory_bytes: Some(65535),
                ..Limits::default()
            },
            ..CallOptions::default()
        });
        assert_eq!(
            Zone::import(&mut small, &host).unwrap_err().kind,
            ErrorKind::Memory
        );
    }

    #[test]
    fn duplicate_abbreviations_resolve_the_active_offset_before_falling_back() {
        let mut bytes = vec![0; 44];
        bytes[..4].copy_from_slice(b"TZif");
        bytes[32..36].copy_from_slice(&2u32.to_be_bytes());
        bytes[36..40].copy_from_slice(&3u32.to_be_bytes());
        bytes[40..44].copy_from_slice(&8u32.to_be_bytes());
        bytes.extend_from_slice(&0i32.to_be_bytes());
        bytes.extend_from_slice(&1710000000i32.to_be_bytes());
        bytes.extend_from_slice(&[0, 1]);
        for (offset, dst, name) in [(3600i32, 0, 0), (7200, 1, 0), (1800, 0, 4)] {
            bytes.extend_from_slice(&offset.to_be_bytes());
            bytes.extend_from_slice(&[dst, name]);
        }
        bytes.extend_from_slice(b"XXX\0YYY\0");
        let mut ctx = CallContext::new(CallOptions::default());
        let (info, rules) = Tzif::parse(&mut ctx, &bytes).unwrap();
        let bytes = ctx.bytes(&bytes).unwrap();
        let zone = Zone::new(&mut ctx, bytes, Some(info), rules, 0).unwrap();
        for (name, seconds, expected) in [
            (b"XXX", 1704067200, Some(3600)),
            (b"XXX", 1719792000, Some(7200)),
            (b"YYY", 1719792000, Some(1800)),
            (b"ZZZ", 1719792000, None),
        ] {
            assert_eq!(zone.lookup_name(&mut ctx, name, seconds).unwrap(), expected);
        }
    }

    #[test]
    fn abbreviation_lookup_bounds_long_names_and_zone_tables() {
        let mut setup = CallContext::new(CallOptions::default());
        let options = CallOptions {
            limits: Limits {
                steps: Some(64),
                ..Limits::default()
            },
            ..CallOptions::default()
        };
        let name = vec![b'X'; 131072];
        let zone = Zone::fixed(&mut setup, &name, 0).unwrap();
        let mut ctx = CallContext::new(options.clone());
        assert_eq!(
            zone.lookup_name(&mut ctx, &name, 0).unwrap_err().kind,
            ErrorKind::Steps
        );

        let mut bytes = data(0, "");
        bytes[36..40].copy_from_slice(&10000u32.to_be_bytes());
        bytes.truncate(44);
        bytes.extend_from_slice(&[0; 60000]);
        bytes.extend_from_slice(b"STD\0");
        let (info, rules) = Tzif::parse(&mut setup, &bytes).unwrap();
        let bytes = setup.bytes(&bytes).unwrap();
        let zone = Zone::new(&mut setup, bytes, Some(info), rules, 0).unwrap();
        let mut ctx = CallContext::new(options);
        assert_eq!(
            zone.lookup_name(&mut ctx, b"XXX", 0).unwrap_err().kind,
            ErrorKind::Steps
        );
    }
}
