use super::{Buffer, Work};
use crate::{Result, Value, budget::Charge, value::Kind};
use std::{fmt, ops::Deref};

#[derive(Clone)]
pub(crate) struct Bytes(Value);

impl Bytes {
    pub fn new(work: &dyn Work, bytes: Buffer<u8>) -> Result<Self> {
        let header = work.reserve(crate::value::Bytes::header_bytes())?;
        let (data, storage) = bytes.into_parts();
        Ok(Self(Value(Kind::Bytes(crate::value::Bytes::from_parts(
            data, storage, header,
        )))))
    }

    pub fn from_slice(work: &dyn Work, bytes: &[u8]) -> Result<Self> {
        Self::new(work, Buffer::from_slice(work, bytes)?)
    }

    pub fn compiler_constant(&self) -> Value {
        self.0.compiler_constant()
    }

    pub fn into_value(self, symbol: bool) -> Value {
        let Value(Kind::Bytes(bytes)) = self.0 else {
            unreachable!()
        };
        Value(if symbol {
            Kind::Symbol(bytes)
        } else {
            Kind::Bytes(bytes)
        })
    }
}

impl Deref for Bytes {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        self.0.as_bytes().unwrap()
    }
}

impl AsRef<[u8]> for Bytes {
    fn as_ref(&self) -> &[u8] {
        self
    }
}

impl PartialEq for Bytes {
    fn eq(&self, other: &Self) -> bool {
        **self == **other
    }
}

impl Eq for Bytes {}

impl fmt::Debug for Bytes {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.deref().fmt(formatter)
    }
}

#[derive(Clone)]
pub(crate) struct Text(Bytes);

impl Text {
    pub fn new(work: &dyn Work, text: &str) -> Result<Self> {
        Ok(Self(Bytes::from_slice(work, text.as_bytes())?))
    }

    pub fn from_bytes(bytes: Bytes) -> Option<Self> {
        std::str::from_utf8(&bytes).ok()?;
        Some(Self(bytes))
    }

    pub fn as_str(&self) -> &str {
        std::str::from_utf8(&self.0).unwrap()
    }

    pub fn compiler_constant(&self) -> Value {
        self.0.compiler_constant()
    }
}

impl Deref for Text {
    type Target = str;

    fn deref(&self) -> &str {
        self.as_str()
    }
}

impl AsRef<str> for Text {
    fn as_ref(&self) -> &str {
        self
    }
}

impl<T: AsRef<str> + ?Sized> PartialEq<T> for Text {
    fn eq(&self, other: &T) -> bool {
        self.as_str() == other.as_ref()
    }
}

impl Eq for Text {}

impl fmt::Debug for Text {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.as_str().fmt(formatter)
    }
}

pub(crate) struct Boxed<T> {
    data: Box<T>,
    _charge: Option<Charge>,
}

impl<T> Boxed<T> {
    pub fn new(work: &dyn Work, value: T) -> Result<Self> {
        let charge = work.reserve(size_of::<T>())?;
        Ok(Self {
            data: Box::new(value),
            _charge: charge,
        })
    }

    pub fn into_inner(self) -> T {
        *self.data
    }
}

impl<T> Deref for Boxed<T> {
    type Target = T;

    fn deref(&self) -> &T {
        &self.data
    }
}

impl<T: PartialEq> PartialEq for Boxed<T> {
    fn eq(&self, other: &Self) -> bool {
        self.data == other.data
    }
}

impl<T: fmt::Debug> fmt::Debug for Boxed<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.data.fmt(formatter)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CallContext, CallOptions, ErrorKind, compilation::Meter};
    use std::{cell::RefCell, sync::Arc, time::Instant};

    #[test]
    fn shared_payloads_stay_charged_until_syntax_releases_them() {
        for symbol in [false, true] {
            let mut context = CallContext::new(CallOptions::default());
            let memory = Arc::downgrade(&context.identity());
            let work = Meter(RefCell::new(&mut context));
            let bytes = Bytes::from_slice(&work, b"shared payload").unwrap();
            let retained = work.0.borrow().stats().retained_memory_bytes;
            assert!(retained >= bytes.len() + crate::value::Bytes::header_bytes());
            let alias = bytes.clone();
            let value = bytes.into_value(symbol);
            let compiled = value.compiler_constant();
            assert_eq!(compiled.as_bytes().unwrap().as_ptr(), alias.as_ptr());
            assert_eq!(matches!(compiled.0, Kind::Symbol(_)), symbol);
            assert_eq!(work.0.borrow().stats().retained_memory_bytes, retained);
            drop(value);
            assert_eq!(work.0.borrow().stats().retained_memory_bytes, retained);
            drop(alias);
            assert_eq!(work.0.borrow().stats().retained_memory_bytes, 0);
            drop(context);
            assert!(memory.upgrade().is_none());
            assert_eq!(compiled.as_bytes().unwrap(), b"shared payload");
        }
    }

    #[test]
    fn header_failures_release_bytes_and_preserve_termination() {
        for kind in [ErrorKind::Memory, ErrorKind::Cancelled, ErrorKind::Deadline] {
            let mut context = CallContext::new(CallOptions::default());
            let work = Meter(RefCell::new(&mut context));
            let bytes = Buffer::from_slice(&work, b"payload").unwrap();
            match kind {
                ErrorKind::Memory => work.0.borrow_mut().options.limits.memory_bytes = Some(7),
                ErrorKind::Cancelled => work.0.borrow().cancellation().cancel(),
                ErrorKind::Deadline => {
                    work.0.borrow_mut().options.deadline = Some(Instant::now());
                }
                _ => unreachable!(),
            }
            assert_eq!(Bytes::new(&work, bytes).unwrap_err().kind, kind);
            assert_eq!(work.checkpoint().unwrap_err().kind, kind);
            assert_eq!(work.0.borrow().stats().retained_memory_bytes, 0);
        }
    }

    #[test]
    fn boxed_storage_is_reserved_before_ownership_changes() {
        let mut context = CallContext::new(CallOptions::default());
        let work = Meter(RefCell::new(&mut context));
        let bytes = Bytes::from_slice(&work, b"payload").unwrap();
        let retained = work.0.borrow().stats().retained_memory_bytes;
        work.0.borrow_mut().options.limits.memory_bytes = Some(retained);
        let result = Boxed::new(&work, bytes.clone());
        assert_eq!(result.unwrap_err().kind, ErrorKind::Memory);
        assert_eq!(&*bytes, b"payload");
        assert_eq!(work.0.borrow().stats().retained_memory_bytes, retained);
        drop(bytes);
        assert_eq!(work.0.borrow().stats().retained_memory_bytes, 0);
    }
}
