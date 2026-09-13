// Windows local-time and filename conversion adapt Go's time and syscall packages.
// Copyright 2009, 2023 The Go Authors. All rights reserved.
// See licenses/Go-BSD-3-Clause.txt.

use super::{Zone, calendar};
use crate::{CallContext, Result, budget::Buffer};
use std::{cmp::Ordering, sync::Arc};

#[path = "data/windows.rs"]
mod abbreviations;

/// Converts filename bytes with Go's Windows WTF-8 rules.
pub(super) fn filename_units(ctx: &mut CallContext, mut bytes: &[u8]) -> Result<Buffer<u16>> {
    let mut units = Buffer::with_capacity(ctx, bytes.len())?;
    while !bytes.is_empty() {
        ctx.charge(1)?;
        if bytes.len() >= 3
            && bytes[0] == 0xed
            && (0xa0..=0xbf).contains(&bytes[1])
            && (0x80..=0xbf).contains(&bytes[2])
        {
            units.data.push(
                (u16::from(bytes[0] & 15) << 12)
                    | (u16::from(bytes[1] & 63) << 6)
                    | u16::from(bytes[2] & 63),
            );
            bytes = &bytes[3..];
        } else {
            let (rune, width, _) = crate::scan::rune(bytes);
            let mut scratch = [0; 2];
            units
                .data
                .extend_from_slice(rune.encode_utf16(&mut scratch));
            bytes = &bytes[width..];
        }
    }
    Ok(units)
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct SystemTime {
    year: u16,
    month: u16,
    weekday: u16,
    day: u16,
    hour: u16,
    minute: u16,
    second: u16,
    milliseconds: u16,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
struct Information {
    bias: i32,
    standard_name: [u16; 32],
    standard_date: SystemTime,
    standard_bias: i32,
    daylight_name: [u16; 32],
    daylight_date: SystemTime,
    daylight_bias: i32,
}

#[cfg(any(windows, test))]
#[repr(C)]
struct DynamicInformation {
    information: Information,
    key: [u16; 128],
    disabled: u8,
}

#[cfg(any(windows, test))]
impl Default for DynamicInformation {
    fn default() -> Self {
        Self {
            information: Information::default(),
            key: [0; 128],
            disabled: 0,
        }
    }
}

#[derive(Clone, Copy)]
struct Names {
    standard: [u8; 32],
    standard_len: usize,
    daylight: [u8; 32],
    daylight_len: usize,
}

fn terminated(input: &[u16]) -> &[u16] {
    &input[..input.iter().position(|&c| c == 0).unwrap_or(input.len())]
}

fn abbreviate(ctx: &mut CallContext, name: &[u16]) -> Result<Option<Names>> {
    let name = terminated(name);
    let table = abbreviations::ABBREVIATIONS;
    let (mut lo, mut hi) = (0, table.len());
    while lo < hi {
        ctx.charge(1)?;
        let mid = lo + (hi - lo) / 2;
        match table[mid].0.encode_utf16().cmp(name.iter().copied()) {
            Ordering::Less => lo = mid + 1,
            Ordering::Greater => hi = mid,
            Ordering::Equal => {
                let (_, standard, daylight) = table[mid];
                let mut names = Names {
                    standard: [0; 32],
                    standard_len: standard.len(),
                    daylight: [0; 32],
                    daylight_len: daylight.len(),
                };
                names.standard[..standard.len()].copy_from_slice(standard.as_bytes());
                names.daylight[..daylight.len()].copy_from_slice(daylight.as_bytes());
                return Ok(Some(names));
            }
        }
    }
    Ok(None)
}

fn capitals(info: &Information) -> Names {
    fn extract(input: &[u16], output: &mut [u8]) -> usize {
        let mut length = 0;
        for &c in terminated(input) {
            if (u16::from(b'A')..=u16::from(b'Z')).contains(&c) {
                output[length] = c as u8;
                length += 1;
            }
        }
        length
    }
    let mut names = Names {
        standard: [0; 32],
        standard_len: 0,
        daylight: [0; 32],
        daylight_len: 0,
    };
    names.standard_len = extract(&info.standard_name, &mut names.standard);
    names.daylight_len = extract(&info.daylight_name, &mut names.daylight);
    names
}

fn pseudo_unix(year: i64, date: SystemTime) -> i64 {
    let first = calendar::normalized([
        year,
        i64::from(date.month),
        1,
        i64::from(date.hour),
        i64::from(date.minute),
        i64::from(date.second),
    ]);
    let weekday = calendar::civil(first, 0).weekday;
    let mut day = 1 + (i64::from(date.weekday) - weekday).rem_euclid(7);
    if date.day < 5 {
        day += (i64::from(date.day) - 1) * 7;
    } else {
        day += 28;
        if day > calendar::days_in(year, i64::from(date.month)) {
            day -= 7;
        }
    }
    first + (day - 1) * 86400
}

fn build(ctx: &mut CallContext, info: &Information, names: Names, year: i64) -> Result<Arc<Zone>> {
    let standard_name = &names.standard[..names.standard_len];
    if info.standard_date.month == 0 {
        return Zone::fixed(ctx, standard_name, info.bias.wrapping_mul(-60));
    }
    let daylight_name = &names.daylight[..names.daylight_len];
    let offsets = [
        info.bias.wrapping_add(info.standard_bias).wrapping_mul(-60),
        info.bias.wrapping_add(info.daylight_bias).wrapping_mul(-60),
    ];
    let name_len = standard_name.len() + daylight_name.len() + 2;
    let capacity = 44 + 6 + name_len + 44 + 400 * 9 + 12 + name_len;
    let mut buffer = Buffer::with_capacity(ctx, capacity)?;
    fn header(buffer: &mut Vec<u8>, transitions: u32, zones: u32, names: u32) {
        let mut header = [0; 44];
        header[..5].copy_from_slice(b"TZif2");
        header[32..36].copy_from_slice(&transitions.to_be_bytes());
        header[36..40].copy_from_slice(&zones.to_be_bytes());
        header[40..44].copy_from_slice(&names.to_be_bytes());
        buffer.extend_from_slice(&header);
    }
    fn names_into(buffer: &mut Vec<u8>, standard: &[u8], daylight: &[u8]) {
        buffer.extend_from_slice(standard);
        buffer.push(0);
        buffer.extend_from_slice(daylight);
        buffer.push(0);
    }
    header(&mut buffer.data, 0, 1, name_len as u32);
    buffer.data.extend_from_slice(&offsets[0].to_be_bytes());
    buffer.data.extend_from_slice(&[0, 0]);
    names_into(&mut buffer.data, standard_name, daylight_name);
    header(&mut buffer.data, 400, 2, name_len as u32);
    let mut dates = [(info.standard_date, 0usize), (info.daylight_date, 1usize)];
    if dates[0].0.month > dates[1].0.month {
        dates.swap(0, 1);
    }
    // Go freezes the current rules into 100 years on either side of initialization.
    for year in year - 100..year + 100 {
        ctx.charge(1)?;
        ctx.checkpoint()?;
        for (date, index) in dates {
            let when = pseudo_unix(year, date) - i64::from(offsets[1 - index]);
            buffer.data.extend_from_slice(&when.to_be_bytes());
        }
    }
    for _ in 0..200 {
        ctx.charge(1)?;
        buffer
            .data
            .extend_from_slice(&[dates[0].1 as u8, dates[1].1 as u8]);
    }
    for (index, offset) in offsets.iter().enumerate() {
        buffer.data.extend_from_slice(&offset.to_be_bytes());
        buffer.data.extend_from_slice(&[
            index as u8,
            if index == 0 {
                0
            } else {
                standard_name.len() as u8 + 1
            },
        ]);
    }
    names_into(&mut buffer.data, standard_name, daylight_name);
    Zone::from_buffer(ctx, buffer)?.ok_or_else(super::invalid)
}

#[cfg(windows)]
mod native {
    use super::*;
    use std::sync::OnceLock;

    const _: () = {
        assert!(std::mem::size_of::<Information>() == 172);
        assert!(std::mem::size_of::<DynamicInformation>() == 432);
        assert!(std::mem::offset_of!(DynamicInformation, key) == 172);
        assert!(std::mem::offset_of!(DynamicInformation, disabled) == 428);
    };

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetTimeZoneInformation(info: *mut Information) -> u32;
        fn GetDynamicTimeZoneInformation(info: *mut DynamicInformation) -> u32;
    }
    #[link(name = "advapi32")]
    unsafe extern "system" {
        fn EnumDynamicTimeZoneInformation(index: u32, info: *mut DynamicInformation) -> u32;
    }

    struct Cached {
        information: Information,
        names: Names,
        year: i64,
    }
    static LOCAL: OnceLock<Option<Cached>> = OnceLock::new();

    fn matching(info: &Information, candidate: &Information) -> bool {
        terminated(&info.standard_name) == terminated(&candidate.standard_name)
            && (terminated(&info.daylight_name) == terminated(&candidate.daylight_name)
                || terminated(&info.daylight_name) == terminated(&info.standard_name))
    }

    fn capture(ctx: &mut CallContext) -> Result<Option<Cached>> {
        ctx.checkpoint()?;
        let mut information = Information::default();
        // SAFETY: the writable structure has the Windows C layout and remains live for the call.
        let status = unsafe { GetTimeZoneInformation(&mut information) };
        ctx.checkpoint()?;
        if status == u32::MAX {
            return Ok(None);
        }
        let mut names = abbreviate(ctx, &information.standard_name)?;
        if names.is_none() {
            let mut dynamic = DynamicInformation::default();
            // SAFETY: the writable structure matches DYNAMIC_TIME_ZONE_INFORMATION and outlives the call.
            let status = unsafe { GetDynamicTimeZoneInformation(&mut dynamic) };
            ctx.checkpoint()?;
            if status != u32::MAX && matching(&information, &dynamic.information) {
                names = abbreviate(ctx, &dynamic.key)?;
            }
        }
        if names.is_none() {
            let mut index = 0u32;
            loop {
                ctx.charge(1)?;
                ctx.checkpoint()?;
                let mut dynamic = DynamicInformation::default();
                // SAFETY: the writable output has the documented C layout; index is an ordinary enumeration index.
                let status = unsafe { EnumDynamicTimeZoneInformation(index, &mut dynamic) };
                ctx.checkpoint()?;
                if status != 0 {
                    break;
                }
                if matching(&information, &dynamic.information) {
                    names = abbreviate(ctx, &dynamic.key)?;
                    if names.is_some() {
                        break;
                    }
                }
                let Some(next) = index.checked_add(1) else {
                    break;
                };
                index = next;
            }
        }
        Ok(Some(Cached {
            names: names.unwrap_or_else(|| capitals(&information)),
            information,
            year: calendar::civil(crate::time::Stamp::now().seconds(), 0).year,
        }))
    }

    pub(super) fn local(ctx: &mut CallContext) -> Result<Arc<Zone>> {
        if LOCAL.get().is_none() {
            let snapshot = capture(ctx)?;
            let _ = LOCAL.set(snapshot);
        }
        match LOCAL.get().unwrap() {
            Some(cached) => build(ctx, &cached.information, cached.names, cached.year),
            None => Zone::fixed(ctx, b"UTC", 0),
        }
    }
}

#[cfg(windows)]
pub(super) fn local(ctx: &mut CallContext) -> Result<Arc<Zone>> {
    native::local(ctx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CallOptions, ErrorKind, Limits};

    #[test]
    fn windows_filename_decoding_preserves_surrogates_and_replaces_each_invalid_byte() {
        for (input, expected) in [
            (
                b"Area/Zone".as_slice(),
                vec![65, 114, 101, 97, 47, 90, 111, 110, 101],
            ),
            ("é界🦀".as_bytes(), vec![0x00e9, 0x754c, 0xd83e, 0xdd80]),
            (b"\xf1\x80", vec![0xfffd, 0xfffd]),
            (b"\xe1\x80A", vec![0xfffd, 0xfffd, 65]),
            (b"\xc0\x80", vec![0xfffd, 0xfffd]),
            (b"\xff\xed\xa0\x80", vec![0xfffd, 0xd800]),
            (b"\xed\xa0\x80\xed\xb0\x80", vec![0xd800, 0xdc00]),
            (b"\xed\xa0", vec![0xfffd, 0xfffd]),
            (b"\0", vec![0]),
            (b"", vec![]),
        ] {
            let mut ctx = CallContext::new(CallOptions::default());
            let units = filename_units(&mut ctx, input).unwrap();
            assert_eq!(units.data, expected, "{input:?}");
            #[cfg(windows)]
            {
                use std::os::windows::ffi::{OsStrExt, OsStringExt};
                let name = std::ffi::OsString::from_wide(&units.data);
                assert_eq!(name.encode_wide().collect::<Vec<_>>(), expected);
            }
            drop(units);
            assert_eq!(ctx.stats().retained_memory_bytes, 0);
        }
        for unit in 0xd800u16..=0xdfff {
            let encoded = [
                0xed,
                0x80 | ((unit >> 6) & 63) as u8,
                0x80 | (unit & 63) as u8,
            ];
            let mut ctx = CallContext::new(CallOptions::default());
            assert_eq!(filename_units(&mut ctx, &encoded).unwrap().data, [unit]);
        }
        for (limits, kind) in [
            (
                Limits {
                    steps: Some(64),
                    ..Limits::default()
                },
                ErrorKind::Steps,
            ),
            (
                Limits {
                    memory_bytes: Some(8191),
                    ..Limits::default()
                },
                ErrorKind::Memory,
            ),
        ] {
            let mut ctx = CallContext::new(CallOptions {
                limits,
                ..CallOptions::default()
            });
            assert!(matches!(filename_units(&mut ctx, &[255; 4096]), Err(e) if e.kind == kind));
            assert_eq!(ctx.stats().retained_memory_bytes, 0);
            assert_eq!(ctx.charge(0).unwrap_err().kind, kind);
        }
        let mut ctx = CallContext::new(CallOptions::default());
        ctx.cancellation().cancel();
        assert!(
            matches!(filename_units(&mut ctx, b"name"), Err(e) if e.kind == ErrorKind::Cancelled)
        );
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }

    fn name(input: &str) -> [u16; 32] {
        let mut output = [0; 32];
        for (slot, code) in output.iter_mut().zip(input.encode_utf16()) {
            *slot = code;
        }
        output
    }

    fn pacific() -> Information {
        Information {
            bias: 480,
            standard_name: name("Pacific Standard Time"),
            daylight_name: name("Pacific Daylight Time"),
            standard_date: SystemTime {
                month: 11,
                day: 1,
                hour: 2,
                ..SystemTime::default()
            },
            daylight_date: SystemTime {
                month: 3,
                day: 2,
                hour: 2,
                ..SystemTime::default()
            },
            daylight_bias: -60,
            ..Information::default()
        }
    }

    #[test]
    fn windows_layouts_and_all_abbreviations_match_the_declared_contract() {
        assert_eq!(std::mem::size_of::<SystemTime>(), 16);
        assert_eq!(std::mem::size_of::<Information>(), 172);
        assert_eq!(std::mem::size_of::<DynamicInformation>(), 432);
        assert_eq!(std::mem::offset_of!(DynamicInformation, key), 172);
        assert_eq!(std::mem::offset_of!(DynamicInformation, disabled), 428);
        let mut ctx = CallContext::new(CallOptions::default());
        for &(key, standard, daylight) in abbreviations::ABBREVIATIONS {
            let units: Vec<_> = key.encode_utf16().collect();
            let names = abbreviate(&mut ctx, &units).unwrap().unwrap();
            assert_eq!(&names.standard[..names.standard_len], standard.as_bytes());
            assert_eq!(&names.daylight[..names.daylight_len], daylight.as_bytes());
        }
        let info = Information {
            standard_name: name("Custom 界🦀 Standard Time"),
            daylight_name: name("Custom Daylight Time"),
            ..Information::default()
        };
        assert!(abbreviate(&mut ctx, &info.standard_name).unwrap().is_none());
        let names = capitals(&info);
        assert_eq!(&names.standard[..names.standard_len], b"CST");
        assert_eq!(&names.daylight[..names.daylight_len], b"CDT");
        assert_eq!(ctx.stats().peak_memory_bytes, 0);
    }

    #[test]
    fn native_rule_conversion_handles_seasons_and_disabled_dst() {
        let mut ctx = CallContext::new(CallOptions::default());
        let north = pacific();
        let south = Information {
            bias: -600,
            standard_name: name("AUS Eastern Standard Time"),
            standard_date: SystemTime {
                month: 4,
                day: 1,
                hour: 3,
                ..SystemTime::default()
            },
            daylight_date: SystemTime {
                month: 10,
                day: 1,
                hour: 2,
                ..SystemTime::default()
            },
            daylight_bias: -60,
            ..Information::default()
        };
        for (info, cases) in [
            (
                north,
                vec![
                    (1710064799, -28800, false),
                    (1710064800, -25200, true),
                    (1730624399, -25200, true),
                    (1730624400, -28800, false),
                ],
            ),
            (
                south,
                vec![
                    (1712419199, 39600, true),
                    (1712419200, 36000, false),
                    (1728143999, 36000, false),
                    (1728144000, 39600, true),
                ],
            ),
        ] {
            let names = abbreviate(&mut ctx, &info.standard_name).unwrap().unwrap();
            let zone = build(&mut ctx, &info, names, 2026).unwrap();
            for (seconds, offset, dst) in cases {
                let actual = zone.lookup(&mut ctx, seconds).unwrap();
                assert_eq!((actual.seconds, actual.dst), (offset, dst), "{seconds}");
            }
        }
        let mut disabled = north;
        disabled.standard_date.month = 0;
        disabled.standard_bias = 120;
        let zone = build(&mut ctx, &disabled, capitals(&disabled), 2026).unwrap();
        assert_eq!(zone.lookup(&mut ctx, 1719792000).unwrap().seconds, -28800);
    }

    #[test]
    fn calendar_rules_and_transition_storage_are_bounded() {
        for (year, month, day) in [(2024, 2, 29), (2023, 2, 23), (2000, 2, 24), (1900, 2, 22)] {
            let date = SystemTime {
                month,
                weekday: 4,
                day: 5,
                hour: 12,
                ..SystemTime::default()
            };
            assert_eq!(
                pseudo_unix(year, date),
                calendar::normalized([year, i64::from(month), day, 12, 0, 0])
            );
        }
        let info = pacific();
        let names = capitals(&info);
        for (limits, kind) in [
            (
                Limits {
                    steps: Some(64),
                    ..Limits::default()
                },
                ErrorKind::Steps,
            ),
            (
                Limits {
                    memory_bytes: Some(1024),
                    ..Limits::default()
                },
                ErrorKind::Memory,
            ),
        ] {
            let mut ctx = CallContext::new(CallOptions {
                limits,
                ..CallOptions::default()
            });
            assert_eq!(build(&mut ctx, &info, names, 2026).unwrap_err().kind, kind);
            assert_eq!(ctx.stats().retained_memory_bytes, 0);
        }
        let mut ctx = CallContext::new(CallOptions::default());
        ctx.cancellation().cancel();
        assert_eq!(
            build(&mut ctx, &info, names, 2026).unwrap_err().kind,
            ErrorKind::Cancelled
        );
        let mut ctx = CallContext::new(CallOptions::default());
        let zone = build(&mut ctx, &info, names, 2026).unwrap();
        assert!(ctx.stats().peak_memory_bytes < 5000);
        drop(zone);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
}
