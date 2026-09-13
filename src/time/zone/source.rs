// ZIP and Android tzdata layouts adapt the Go standard library's time package.
// Copyright 2009 The Go Authors. All rights reserved.
// See licenses/Go-BSD-3-Clause.txt.

use super::MAX_ZONE_BYTES;
use crate::{CallContext, ErrorKind, Result, budget::Buffer};
use std::io::{self, Read, Seek, SeekFrom};

pub(super) struct Source<R> {
    reader: R,
    size: u64,
}

pub(super) enum Format {
    File,
    Zip,
    #[cfg(any(target_os = "android", test))]
    Android,
}

fn little2(bytes: &[u8], start: usize) -> u64 {
    u64::from(u16::from_le_bytes(
        bytes[start..start + 2].try_into().unwrap(),
    ))
}

fn little4(bytes: &[u8], start: usize) -> u64 {
    u64::from(u32::from_le_bytes(
        bytes[start..start + 4].try_into().unwrap(),
    ))
}

#[cfg(any(target_os = "android", test))]
fn big4(bytes: &[u8], start: usize) -> u64 {
    u64::from(u32::from_be_bytes(
        bytes[start..start + 4].try_into().unwrap(),
    ))
}

impl<R: Read + Seek> Source<R> {
    pub fn new(reader: R, size: u64) -> Self {
        Self { reader, size }
    }

    pub fn load(
        &mut self,
        ctx: &mut CallContext,
        format: Format,
        name: &[u8],
    ) -> Result<Option<Buffer<u8>>> {
        match format {
            Format::File => self.file(ctx),
            Format::Zip => self.zip(ctx, name),
            #[cfg(any(target_os = "android", test))]
            Format::Android => self.android(ctx, name),
        }
    }

    fn contains(&self, start: u64, size: u64) -> bool {
        start.checked_add(size).is_some_and(|end| end <= self.size)
    }

    fn read(&mut self, ctx: &mut CallContext, start: u64, bytes: &mut [u8]) -> Result<bool> {
        ctx.charge(1)?;
        ctx.checkpoint()?;
        if !self.contains(start, bytes.len() as u64) {
            return Ok(false);
        }
        let position = self.reader.seek(SeekFrom::Start(start));
        ctx.checkpoint()?;
        if position.is_err() {
            return Ok(false);
        }
        for chunk in bytes.chunks_mut(4096) {
            let mut remaining = chunk;
            while !remaining.is_empty() {
                ctx.work_bytes(remaining.len())?;
                let result = self.reader.read(remaining);
                ctx.checkpoint()?;
                match result {
                    Ok(0) => return Ok(false),
                    Ok(n) => remaining = &mut remaining[n..],
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                    Err(_) => return Ok(false),
                }
            }
        }
        Ok(true)
    }

    fn equal(&mut self, ctx: &mut CallContext, mut start: u64, bytes: &[u8]) -> Result<bool> {
        let mut scratch = [0; 1024];
        for chunk in bytes.chunks(scratch.len()) {
            let scratch = &mut scratch[..chunk.len()];
            if !self.read(ctx, start, scratch)? || scratch != chunk {
                return Ok(false);
            }
            start += chunk.len() as u64;
        }
        Ok(true)
    }

    fn data(&mut self, ctx: &mut CallContext, start: u64, size: u64) -> Result<Option<Buffer<u8>>> {
        if !self.contains(start, size) {
            return Ok(None);
        }
        let Ok(size) = usize::try_from(size) else {
            return ctx.fail(ErrorKind::Memory, "timezone allocation size overflow");
        };
        let mut bytes = Buffer::with_capacity(ctx, size)?;
        // Fill in bounded chunks so zero-initialization is also cancellable.
        while bytes.data.len() < size {
            let begin = bytes.data.len();
            let end = begin + (size - begin).min(4096);
            ctx.work_bytes(end - begin)?;
            bytes.data.resize(end, 0);
            if !self.read(ctx, start + begin as u64, &mut bytes.data[begin..end])? {
                return Ok(None);
            }
        }
        Ok(Some(bytes))
    }

    pub fn file(&mut self, ctx: &mut CallContext) -> Result<Option<Buffer<u8>>> {
        if self.size > MAX_ZONE_BYTES as u64 {
            return Ok(None);
        }
        self.data(ctx, 0, self.size)
    }

    pub fn zip(&mut self, ctx: &mut CallContext, name: &[u8]) -> Result<Option<Buffer<u8>>> {
        let Some(tail) = self.size.checked_sub(22) else {
            return Ok(None);
        };
        let mut end = [0; 22];
        if !self.read(ctx, tail, &mut end)? || end[..4] != *b"PK\x05\x06" {
            return Ok(None);
        }
        let count = little2(&end, 10);
        let mut position = little4(&end, 16);
        let limit = position + little4(&end, 12);
        if limit > tail {
            return Ok(None);
        }
        let mut central = [0; 46];
        let mut local = [0; 30];
        for _ in 0..count {
            if position + central.len() as u64 > limit
                || !self.read(ctx, position, &mut central)?
                || central[..4] != *b"PK\x01\x02"
            {
                return Ok(None);
            }
            let length = little2(&central, 28);
            let start = position + central.len() as u64;
            position = start + length + little2(&central, 30) + little2(&central, 32);
            if position > limit {
                return Ok(None);
            }
            if length != name.len() as u64 || !self.equal(ctx, start, name)? {
                continue;
            }
            let offset = little4(&central, 42);
            if little2(&central, 10) != 0
                || !self.read(ctx, offset, &mut local)?
                || local[..4] != *b"PK\x03\x04"
                || little2(&local, 8) != 0
                || little2(&local, 26) != length
                || !self.equal(ctx, offset + local.len() as u64, name)?
            {
                return Ok(None);
            }
            let start = offset + local.len() as u64 + length + little2(&local, 28);
            return self.data(ctx, start, little4(&central, 24));
        }
        Ok(None)
    }

    #[cfg(any(target_os = "android", test))]
    pub fn android(&mut self, ctx: &mut CallContext, name: &[u8]) -> Result<Option<Buffer<u8>>> {
        let mut header = [0; 24];
        if name.len() > 40 || !self.read(ctx, 0, &mut header)? || header[..6] != *b"tzdata" {
            return Ok(None);
        }
        let index = big4(&header, 12);
        let data = big4(&header, 16);
        if data < index || data > self.size {
            return Ok(None);
        }
        let mut entry = [0; 52];
        for i in 0..(data - index) / entry.len() as u64 {
            if !self.read(ctx, index + i * entry.len() as u64, &mut entry)? {
                return Ok(None);
            }
            // Android's reference reader compares the supplied name as a prefix.
            if entry[..name.len()] == *name {
                return self.data(ctx, data + big4(&entry, 40), big4(&entry, 44));
            }
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CallOptions, ErrorKind, Limits};
    use std::io::Cursor;

    fn archive(entries: &[(&[u8], &[u8])]) -> Vec<u8> {
        let mut data = Vec::new();
        let mut directory = Vec::new();
        for &(name, content) in entries {
            let mut local = [0; 30];
            local[..4].copy_from_slice(b"PK\x03\x04");
            local[26..28].copy_from_slice(&(name.len() as u16).to_le_bytes());
            let mut central = [0; 46];
            central[..4].copy_from_slice(b"PK\x01\x02");
            central[24..28].copy_from_slice(&(content.len() as u32).to_le_bytes());
            central[28..30].copy_from_slice(&(name.len() as u16).to_le_bytes());
            central[42..46].copy_from_slice(&(data.len() as u32).to_le_bytes());
            data.extend_from_slice(&local);
            data.extend_from_slice(name);
            data.extend_from_slice(content);
            directory.extend_from_slice(&central);
            directory.extend_from_slice(name);
        }
        let mut tail = [0; 22];
        tail[..4].copy_from_slice(b"PK\x05\x06");
        tail[10..12].copy_from_slice(&(entries.len() as u16).to_le_bytes());
        tail[12..16].copy_from_slice(&(directory.len() as u32).to_le_bytes());
        tail[16..20].copy_from_slice(&(data.len() as u32).to_le_bytes());
        data.extend_from_slice(&directory);
        data.extend_from_slice(&tail);
        data
    }

    fn read_zip(ctx: &mut CallContext, bytes: &[u8], name: &[u8]) -> Result<Option<Buffer<u8>>> {
        Source::new(Cursor::new(bytes), bytes.len() as u64).zip(ctx, name)
    }

    #[test]
    fn zip_lookup_keeps_only_the_selected_payload_and_releases_failures() {
        let large = vec![b'x'; 1 << 20];
        let bytes = archive(&[
            (b"large", &large),
            (b"wanted", b"zone data"),
            (b"wanted", b"later"),
        ]);
        let mut ctx = CallContext::new(CallOptions::default());
        let value = read_zip(&mut ctx, &bytes, b"wanted").unwrap().unwrap();
        assert_eq!(value.data, b"zone data");
        assert_eq!(ctx.stats().peak_memory_bytes, 9);
        assert_eq!(ctx.stats().retained_memory_bytes, 9);
        drop(value);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
        assert!(read_zip(&mut ctx, &bytes, b"missing").unwrap().is_none());
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }

    #[test]
    fn large_archive_payloads_use_call_limits_instead_of_the_file_limit() {
        let payload = vec![b'x'; MAX_ZONE_BYTES + 1];
        let mut ctx = CallContext::new(CallOptions::default());
        assert!(
            Source::new(Cursor::new(&payload), payload.len() as u64)
                .file(&mut ctx)
                .unwrap()
                .is_none()
        );
        assert_eq!(ctx.stats().peak_memory_bytes, 0);
        let bytes = Source::new(Cursor::new(&payload), MAX_ZONE_BYTES as u64)
            .file(&mut ctx)
            .unwrap()
            .unwrap();
        assert_eq!(bytes.data, payload[..MAX_ZONE_BYTES]);
        drop(bytes);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
        for format in [Format::Zip, Format::Android] {
            let data = match format {
                Format::Zip => archive(&[(b"zone", &payload)]),
                Format::Android => {
                    let mut data = vec![0; 24 + 52];
                    data[..6].copy_from_slice(b"tzdata");
                    data[12..16].copy_from_slice(&24u32.to_be_bytes());
                    data[16..20].copy_from_slice(&76u32.to_be_bytes());
                    data[24..28].copy_from_slice(b"zone");
                    data[68..72].copy_from_slice(&(payload.len() as u32).to_be_bytes());
                    data.extend_from_slice(&payload);
                    data
                }
                Format::File => unreachable!(),
            };
            let mut source = Source::new(Cursor::new(&data), data.len() as u64);
            for limit in [payload.len() - 1, payload.len()] {
                let mut ctx = CallContext::new(CallOptions {
                    limits: Limits {
                        memory_bytes: Some(limit),
                        ..Limits::default()
                    },
                    ..CallOptions::default()
                });
                let result = match format {
                    Format::Zip => source.zip(&mut ctx, b"zone"),
                    Format::Android => source.android(&mut ctx, b"zone"),
                    Format::File => unreachable!(),
                };
                if limit < payload.len() {
                    assert!(matches!(result, Err(e) if e.kind == ErrorKind::Memory));
                    assert_eq!(ctx.stats().peak_memory_bytes, 0);
                    assert_eq!(ctx.charge(0).unwrap_err().kind, ErrorKind::Memory);
                } else {
                    let bytes = result.unwrap().unwrap();
                    assert_eq!(bytes.data, payload);
                    assert_eq!(ctx.stats().peak_memory_bytes, payload.len());
                    drop(bytes);
                }
                assert_eq!(ctx.stats().retained_memory_bytes, 0);
            }
        }
    }

    #[test]
    fn truncated_and_corrupt_zip_fields_never_panic_or_allocate_declared_sizes() {
        let original = archive(&[(b"zone", b"payload")]);
        for length in 0..original.len() {
            let mut ctx = CallContext::new(CallOptions::default());
            assert!(
                read_zip(&mut ctx, &original[..length], b"zone")
                    .unwrap()
                    .is_none(),
                "length {length}"
            );
        }
        let central = 30 + 4 + 7;
        let tail = original.len() - 22;
        for (start, width) in [
            (0, 4),
            (8, 2),
            (26, 2),
            (28, 2),
            (central, 4),
            (central + 10, 2),
            (central + 24, 4),
            (central + 28, 2),
            (central + 30, 2),
            (central + 32, 2),
            (central + 42, 4),
            (tail, 4),
            (tail + 12, 4),
            (tail + 16, 4),
        ] {
            let mut bytes = original.clone();
            bytes[start..start + width].fill(255);
            let mut ctx = CallContext::new(CallOptions::default());
            assert!(
                read_zip(&mut ctx, &bytes, b"zone").unwrap().is_none(),
                "field {start}"
            );
            assert_eq!(ctx.stats().peak_memory_bytes, 0, "field {start}");
        }
    }

    #[test]
    fn archive_tables_payloads_and_io_retries_obey_limits() {
        let entries = vec![(b"a".as_slice(), b"x".as_slice()); 10000];
        let bytes = archive(&entries);
        let mut ctx = CallContext::new(CallOptions {
            limits: Limits {
                steps: Some(64),
                ..Limits::default()
            },
            ..CallOptions::default()
        });
        assert!(matches!(read_zip(&mut ctx, &bytes, b"z"), Err(e) if e.kind==ErrorKind::Steps));
        assert_eq!(ctx.stats().peak_memory_bytes, 0);
        let bytes = archive(&[(b"a", &[0; 4096])]);
        let mut ctx = CallContext::new(CallOptions {
            limits: Limits {
                memory_bytes: Some(4095),
                ..Limits::default()
            },
            ..CallOptions::default()
        });
        assert!(matches!(read_zip(&mut ctx, &bytes, b"a"), Err(e) if e.kind==ErrorKind::Memory));
        assert_eq!(ctx.stats().peak_memory_bytes, 0);
        let mut ctx = CallContext::new(CallOptions::default());
        ctx.cancellation().cancel();
        assert!(matches!(read_zip(&mut ctx, &bytes, b"a"), Err(e) if e.kind==ErrorKind::Cancelled));
        struct Interrupted;
        impl Read for Interrupted {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                Err(io::ErrorKind::Interrupted.into())
            }
        }
        impl Seek for Interrupted {
            fn seek(&mut self, _: SeekFrom) -> io::Result<u64> {
                Ok(0)
            }
        }
        let mut ctx = CallContext::new(CallOptions {
            limits: Limits {
                steps: Some(64),
                ..Limits::default()
            },
            ..CallOptions::default()
        });
        assert!(
            matches!(Source::new(Interrupted, 100).zip(&mut ctx, b"a"), Err(e) if e.kind==ErrorKind::Steps)
        );
    }

    #[test]
    fn android_index_selects_bounded_payloads_and_checks_offsets() {
        let mut data = vec![0; 24 + 52];
        data[..6].copy_from_slice(b"tzdata");
        data[12..16].copy_from_slice(&24u32.to_be_bytes());
        data[16..20].copy_from_slice(&76u32.to_be_bytes());
        data[24..33].copy_from_slice(b"Area/Zone");
        data[68..72].copy_from_slice(&7u32.to_be_bytes());
        data.extend_from_slice(b"payload");
        for name in [b"Area/Zone".as_slice(), b"Area/"] {
            let mut ctx = CallContext::new(CallOptions::default());
            let value = Source::new(Cursor::new(&data), data.len() as u64)
                .load(&mut ctx, Format::Android, name)
                .unwrap()
                .unwrap();
            assert_eq!(value.data, b"payload");
            assert_eq!(ctx.stats().peak_memory_bytes, 7);
        }
        for length in 0..data.len() {
            let mut ctx = CallContext::new(CallOptions::default());
            assert!(
                Source::new(Cursor::new(&data[..length]), length as u64)
                    .android(&mut ctx, b"Area/Zone")
                    .unwrap()
                    .is_none()
            );
        }
        for start in [12, 16, 64, 68] {
            let mut corrupt = data.clone();
            corrupt[start..start + 4].fill(255);
            let mut ctx = CallContext::new(CallOptions::default());
            assert!(
                Source::new(Cursor::new(&corrupt), corrupt.len() as u64)
                    .android(&mut ctx, b"Area/Zone")
                    .unwrap()
                    .is_none()
            );
            assert_eq!(ctx.stats().peak_memory_bytes, 0);
        }
    }

    #[test]
    fn cancellation_during_io_propagates_before_source_fallback() {
        struct Cancel {
            cursor: Cursor<Vec<u8>>,
            token: crate::CancellationToken,
            during_seek: bool,
        }
        impl Read for Cancel {
            fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
                self.token.cancel();
                self.cursor.read(bytes)
            }
        }
        impl Seek for Cancel {
            fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
                if self.during_seek {
                    self.token.cancel();
                }
                self.cursor.seek(position)
            }
        }
        for during_seek in [false, true] {
            let mut ctx = CallContext::new(CallOptions::default());
            let reader = Cancel {
                cursor: Cursor::new(vec![0; 64]),
                token: ctx.cancellation().clone(),
                during_seek,
            };
            assert!(
                matches!(Source::new(reader,64).file(&mut ctx),Err(e) if e.kind==ErrorKind::Cancelled)
            );
            assert_eq!(ctx.stats().retained_memory_bytes, 0);
            assert_eq!(ctx.charge(0).unwrap_err().kind, ErrorKind::Cancelled);
        }
    }
}
