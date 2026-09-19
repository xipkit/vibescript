use super::{Buffer, Work};
use crate::{Error, ErrorKind, Result, budget::Charge};
use std::{fmt, sync::Arc};

pub(crate) fn error(work: &dyn Work, offset: Option<usize>, args: fmt::Arguments<'_>) -> Error {
    let build = || {
        let mut charge = work.reserve(size_of::<Charge>() + 2 * size_of::<usize>())?;
        let (message, storage) = formatted(work, args)?;
        Charge::merge(&mut charge, storage);
        let mut error = Error::new(ErrorKind::Syntax, message);
        error.offset = offset;
        error.retained_charge = charge.map(Arc::new);
        Ok::<_, Error>(error)
    };
    build().unwrap_or_else(|error| error)
}

pub(crate) fn formatted(
    work: &dyn Work,
    args: fmt::Arguments<'_>,
) -> Result<(String, Option<Charge>)> {
    let mut length = 0usize;
    write(work, args, |bytes| {
        work.bytes(bytes.len())?;
        length = length
            .checked_add(bytes.len())
            .ok_or_else(|| work.allocation_error("compiler diagnostic size overflow"))?;
        Ok(())
    })?;
    let mut buffer = Buffer::with_capacity(work, length)?;
    write(work, args, |bytes| buffer.extend_from_slice(work, bytes))?;
    work.checkpoint()?;
    let (bytes, charge) = buffer.into_parts();
    Ok((String::from_utf8(bytes).unwrap(), charge))
}

fn write(
    work: &dyn Work,
    args: fmt::Arguments<'_>,
    append: impl FnMut(&[u8]) -> Result<()>,
) -> Result<()> {
    struct Writer<F> {
        append: F,
        error: Option<Error>,
    }
    impl<F: FnMut(&[u8]) -> Result<()>> fmt::Write for Writer<F> {
        fn write_str(&mut self, text: &str) -> fmt::Result {
            for chunk in text.as_bytes().chunks(4096) {
                if let Err(error) = (self.append)(chunk) {
                    self.error = Some(error);
                    return Err(fmt::Error);
                }
            }
            Ok(())
        }
    }
    work.checkpoint()?;
    let mut writer = Writer {
        append,
        error: None,
    };
    match fmt::write(&mut writer, args) {
        Ok(()) => Ok(()),
        Err(_) => Err(writer
            .error
            .unwrap_or_else(|| work.allocation_error("compiler diagnostic formatting failed"))),
    }
}
