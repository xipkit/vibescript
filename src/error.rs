use std::fmt;

/// The category of a compile or execution failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ErrorKind {
    Syntax,
    Type,
    Name,
    Argument,
    Arithmetic,
    Json,
    Steps,
    Memory,
    Recursion,
    Cancelled,
    Deadline,
    Host,
}

/// A recoverable interpreter failure, with a byte offset for syntax errors.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Error {
    pub kind: ErrorKind,
    pub message: String,
    pub offset: Option<usize>,
}

impl Error {
    /// Creates an error without a source location.
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            offset: None,
        }
    }

    pub(crate) fn syntax(offset: usize, message: impl Into<String>) -> Self {
        Self {
            kind: ErrorKind::Syntax,
            message: message.into(),
            offset: Some(offset),
        }
    }

    pub(crate) fn exhaustion(&self) -> bool {
        matches!(
            self.kind,
            ErrorKind::Steps
                | ErrorKind::Memory
                | ErrorKind::Recursion
                | ErrorKind::Cancelled
                | ErrorKind::Deadline
        )
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(offset) = self.offset {
            write!(f, "{} at byte {}", self.message, offset)
        } else {
            f.write_str(&self.message)
        }
    }
}

impl std::error::Error for Error {}

/// An interpreter operation that may fail.
pub type Result<T> = std::result::Result<T, Error>;
