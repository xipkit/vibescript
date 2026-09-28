use crate::{CallContext, Error, ErrorKind, Result, Value, budget::Buffer};
use std::{
    fs,
    io::{self, Read},
    time::SystemTime,
};

mod platform;
mod root;
#[cfg(target_os = "wasi")]
mod wasi;
pub(super) use root::{Opened, Root, escape};

#[cfg(test)]
mod tests;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Stamp {
    pub modified: SystemTime,
    pub size: u64,
}

#[derive(Debug)]
pub(super) struct Source {
    pub contents: Value,
    pub stamp: Stamp,
}

pub(super) fn stamp(ctx: &mut CallContext, file: &fs::File) -> Result<Stamp> {
    ctx.checkpoint()?;
    ctx.charge(1)?;
    let metadata = file.metadata();
    ctx.checkpoint()?;
    checked_stamp(&metadata.map_err(|e| io_error("checking module source", e))?)
}

pub(super) fn read(ctx: &mut CallContext, mut file: fs::File, limit: usize) -> Result<Source> {
    let stamp = stamp(ctx, &file)?;
    if u128::from(stamp.size) > limit as u128 {
        return Err(too_large(limit));
    }
    let contents = read_contents(ctx, &mut file, limit)?;
    Ok(Source { contents, stamp })
}

fn checked_stamp(metadata: &fs::Metadata) -> Result<Stamp> {
    if !metadata.is_file() {
        return Err(not_regular());
    }
    Ok(Stamp {
        modified: metadata
            .modified()
            .map_err(|e| io_error("reading module source time", e))?,
        size: metadata.len(),
    })
}

fn read_contents(ctx: &mut CallContext, reader: &mut impl Read, limit: usize) -> Result<Value> {
    let mut output = Buffer::empty();
    let mut chunk = [0u8; 8192];
    loop {
        ctx.checkpoint()?;
        ctx.charge(1)?;
        let room = limit - output.data.len();
        let requested = if room < chunk.len() {
            room + 1
        } else {
            chunk.len()
        };
        let read = reader.read(&mut chunk[..requested]);
        ctx.checkpoint()?;
        let count = match read {
            Ok(count) => count,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(io_error("reading module source", error)),
        };
        if count > requested {
            return Err(Error::new(
                ErrorKind::Runtime,
                "require: invalid module source read count",
            ));
        }
        ctx.charge(count as u64)?;
        if count > room {
            return Err(too_large(limit));
        }
        if count == 0 {
            break;
        }
        let needed = output.data.len() + count;
        if needed > output.data.capacity() {
            ctx.charge(output.data.len() as u64)?;
            let capacity = output
                .data
                .capacity()
                .saturating_mul(2)
                .max(needed)
                .min(limit);
            output.ensure(ctx, capacity)?;
        }
        output.extend(ctx, &chunk[..count])?;
    }
    output.shrink(ctx)?;
    Value::from_bytes(ctx, output)
}

fn spelling_from_directory(
    ctx: &mut CallContext,
    parent: &platform::Dir,
    name: &std::ffi::OsStr,
) -> Result<bool> {
    ctx.checkpoint()?;
    let _scratch =
        ctx.reserve(32768 + platform::PATH_WORKSPACE + size_of::<platform::ReadDir>())?;
    let directory = parent.entries();
    ctx.checkpoint()?;
    let mut directory = directory.map_err(|e| io_error("checking module filename", e))?;
    loop {
        ctx.checkpoint()?;
        ctx.charge(1)?;
        let entry = directory.next();
        ctx.checkpoint()?;
        match entry {
            Some(Ok(entry)) => {
                let filename = entry.file_name();
                ctx.charge(filename.as_encoded_bytes().len() as u64)?;
                if filename == name {
                    return Ok(true);
                }
            }
            Some(Err(error)) => return Err(io_error("reading module filenames", error)),
            None => return Ok(false),
        }
    }
}

fn not_regular() -> Error {
    Error::new(
        ErrorKind::Runtime,
        "require: module source is not a regular file",
    )
}

pub(super) fn too_large(limit: usize) -> Error {
    Error::new(
        ErrorKind::Runtime,
        format!("require: source exceeds maximum size ({limit} bytes)"),
    )
}

fn io_error(operation: &str, error: io::Error) -> Error {
    let kind = if error.kind() == io::ErrorKind::NotFound {
        ErrorKind::Name
    } else {
        ErrorKind::Runtime
    };
    Error::new(kind, format!("require: {operation}: {error}"))
}
