//! Checked access to the tables that vibescript indexes by its own invariants.
//!
//! The compiler and VM index frames, locals, constants and members by
//! positions they computed themselves, so an index out of range is a bug in
//! vibescript, not in the script. These accessors report such a bug as an
//! [`ErrorKind::Internal`] error, which reaches the host like any other
//! failure of the call, instead of a panic.
//!
//! A helper on a hot path returns [`Access`] rather than [`crate::Result`]:
//! [`Violation`] is small and `Copy`, so after inlining `?` decides on a
//! register-held value and the [`Error`] is built only on the failing path,
//! out of line, by `From`. The hot path then costs no more than indexing did.

use crate::budget::Buffer;
use crate::{Error, ErrorKind};
use std::panic::Location;

/// Builds the error for an invariant that did not hold, naming where.
#[cold]
#[inline(never)]
#[track_caller]
pub(crate) fn internal(message: impl std::fmt::Display) -> Error {
    Error::new(
        ErrorKind::Internal,
        format!("internal error at {}: {message}", Location::caller()),
    )
}

/// An invariant that did not hold, and where it was relied on.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Violation {
    what: What,
    at: &'static Location<'static>,
}

#[derive(Clone, Copy, Debug)]
enum What {
    Index {
        index: usize,
        len: usize,
    },
    Span {
        start: usize,
        end: usize,
        len: usize,
    },
    Message(&'static str),
}

/// The violation for a shape the compiler should have ruled out.
#[inline]
#[track_caller]
pub(crate) fn violation(message: &'static str) -> Violation {
    Violation {
        what: What::Message(message),
        at: Location::caller(),
    }
}

impl From<Violation> for Error {
    #[cold]
    #[inline(never)]
    fn from(violation: Violation) -> Self {
        let at = violation.at;
        let message = match violation.what {
            What::Index { index, len } => {
                format!("internal error at {at}: index {index} is out of range for length {len}")
            }
            What::Span { start, end, len } => {
                format!(
                    "internal error at {at}: range {start}..{end} is out of range for length {len}"
                )
            }
            What::Message(message) => format!("internal error at {at}: {message}"),
        };
        Error::new(ErrorKind::Internal, message)
    }
}

/// What a checked access returns: the value, or the violation.
pub(crate) type Access<T> = Result<T, Violation>;

/// Indexing that reports an [`ErrorKind::Internal`] error instead of
/// panicking when the position is out of range.
pub(crate) trait Checked<T> {
    /// The element at `index`.
    fn at(&self, index: usize) -> Access<&T>;
    /// The element at `index`, mutably.
    fn at_mut(&mut self, index: usize) -> Access<&mut T>;
    /// The elements from `start` to the end.
    fn tail(&self, start: usize) -> Access<&[T]>;
    /// The elements from `start` up to `end`.
    fn span(&self, start: usize, end: usize) -> Access<&[T]>;
    /// The elements from `start` up to `end`, mutably.
    fn span_mut(&mut self, start: usize, end: usize) -> Access<&mut [T]>;
}

impl<T> Checked<T> for [T] {
    #[inline]
    #[track_caller]
    fn at(&self, index: usize) -> Access<&T> {
        match self.get(index) {
            Some(value) => Ok(value),
            None => Err(Violation {
                what: What::Index {
                    index,
                    len: self.len(),
                },
                at: Location::caller(),
            }),
        }
    }

    #[inline]
    #[track_caller]
    fn at_mut(&mut self, index: usize) -> Access<&mut T> {
        let len = self.len();
        match self.get_mut(index) {
            Some(value) => Ok(value),
            None => Err(Violation {
                what: What::Index { index, len },
                at: Location::caller(),
            }),
        }
    }

    #[inline]
    #[track_caller]
    fn tail(&self, start: usize) -> Access<&[T]> {
        match self.get(start..) {
            Some(values) => Ok(values),
            None => Err(Violation {
                what: What::Span {
                    start,
                    end: self.len(),
                    len: self.len(),
                },
                at: Location::caller(),
            }),
        }
    }

    #[inline]
    #[track_caller]
    fn span(&self, start: usize, end: usize) -> Access<&[T]> {
        match self.get(start..end) {
            Some(values) => Ok(values),
            None => Err(Violation {
                what: What::Span {
                    start,
                    end,
                    len: self.len(),
                },
                at: Location::caller(),
            }),
        }
    }

    #[inline]
    #[track_caller]
    fn span_mut(&mut self, start: usize, end: usize) -> Access<&mut [T]> {
        let len = self.len();
        match self.get_mut(start..end) {
            Some(values) => Ok(values),
            None => Err(Violation {
                what: What::Span { start, end, len },
                at: Location::caller(),
            }),
        }
    }
}

impl<T> Checked<T> for Buffer<T> {
    #[inline]
    #[track_caller]
    fn at(&self, index: usize) -> Access<&T> {
        self.data.at(index)
    }

    #[inline]
    #[track_caller]
    fn at_mut(&mut self, index: usize) -> Access<&mut T> {
        self.data.at_mut(index)
    }

    #[inline]
    #[track_caller]
    fn tail(&self, start: usize) -> Access<&[T]> {
        self.data.tail(start)
    }

    #[inline]
    #[track_caller]
    fn span(&self, start: usize, end: usize) -> Access<&[T]> {
        self.data.span(start, end)
    }

    #[inline]
    #[track_caller]
    fn span_mut(&mut self, start: usize, end: usize) -> Access<&mut [T]> {
        self.data.span_mut(start, end)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn in_range_access_succeeds() {
        let mut values = vec![1, 2, 3];
        assert_eq!(values.at(1).unwrap(), &2);
        *values.at_mut(0).unwrap() = 7;
        assert_eq!(values.tail(1).unwrap(), &[2, 3]);
        assert_eq!(values.tail(3).unwrap(), &[] as &[i32]);
        assert_eq!(values.span(0, 2).unwrap(), &[7, 2]);
        values.span_mut(1, 3).unwrap().fill(0);
        assert_eq!(values, [7, 0, 0]);
        let mut buffer = Buffer::untracked(values);
        *buffer.at_mut(2).unwrap() = 9;
        assert_eq!(buffer.at(2).unwrap(), &9);
        assert_eq!(buffer.tail(2).unwrap(), &[9]);
    }

    #[test]
    fn out_of_range_access_is_an_internal_error() {
        let values = vec![1, 2, 3];
        let error = Error::from(values.at(3).unwrap_err());
        assert_eq!(error.kind, ErrorKind::Internal);
        assert!(error.class().is_none(), "{error}");
        assert!(
            error
                .message
                .starts_with("internal error at src/checked.rs:")
                && error
                    .message
                    .ends_with("index 3 is out of range for length 3"),
            "{}",
            error.message
        );
        let error = Error::from(values.tail(4).unwrap_err());
        assert!(
            error
                .message
                .ends_with("range 4..3 is out of range for length 3"),
            "{}",
            error.message
        );
        let error = Error::from(values.span(2, 1).unwrap_err());
        assert!(
            error
                .message
                .ends_with("range 2..1 is out of range for length 3"),
            "{}",
            error.message
        );
        let error = Error::from(violation("frame has no function"));
        assert_eq!(error.kind, ErrorKind::Internal);
        assert!(
            error.message.ends_with(": frame has no function"),
            "{}",
            error.message
        );
        let error = internal(format!("slot {} unbound", 4));
        assert_eq!(error.kind, ErrorKind::Internal);
        assert!(
            error.message.ends_with(": slot 4 unbound"),
            "{}",
            error.message
        );
    }

    #[test]
    fn question_mark_converts_to_error() {
        fn read(values: &[u8], index: usize) -> crate::Result<u8> {
            Ok(*values.at(index)?)
        }
        assert_eq!(read(&[4, 5], 1).unwrap(), 5);
        assert_eq!(read(&[4, 5], 2).unwrap_err().kind, ErrorKind::Internal);
    }
}
