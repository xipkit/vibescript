use std::{fmt, sync::Arc};

/// A one-based line and Unicode character column in script source.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Position {
    pub line: usize,
    pub column: usize,
}

/// A script function and the source position associated with its invocation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StackFrame {
    pub function: Arc<str>,
    pub position: Position,
}

/// Source context and script call frames for a failure, without retained script values.
#[doc = include_str!("../docs/diagnostics.md")]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Diagnostic {
    pub position: Position,
    pub code_frame: String,
    pub frames: Vec<StackFrame>,
}

/// The category of a compile or execution failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ErrorKind {
    Syntax,
    Type,
    Name,
    Argument,
    Arithmetic,
    Json,
    /// A fixed language limit on formatted output was exceeded.
    OutputLimit,
    Steps,
    Memory,
    Recursion,
    Cancelled,
    Deadline,
    Host,
}

/// An interpreter failure with optional source context and a byte offset.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Error {
    pub kind: ErrorKind,
    pub message: String,
    pub offset: Option<usize>,
    pub diagnostic: Option<Arc<Diagnostic>>,
}

impl Error {
    /// Creates an error without a source location.
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            offset: None,
            diagnostic: None,
        }
    }

    pub(crate) fn syntax(offset: usize, message: impl Into<String>) -> Self {
        Self {
            kind: ErrorKind::Syntax,
            message: message.into(),
            offset: Some(offset),
            diagnostic: None,
        }
    }

    pub(crate) fn exhaustion(&self) -> bool {
        matches!(
            self.kind,
            ErrorKind::Steps
                | ErrorKind::OutputLimit
                | ErrorKind::Memory
                | ErrorKind::Recursion
                | ErrorKind::Cancelled
                | ErrorKind::Deadline
        )
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(diagnostic) = &self.diagnostic {
            if self.kind == ErrorKind::Syntax {
                write!(
                    f,
                    "parse error at {}:{}: {}",
                    diagnostic.position.line, diagnostic.position.column, self.message
                )?;
            } else {
                f.write_str(&self.message)?;
            }
            write!(f, "\n{}", diagnostic.code_frame)?;
            let count = diagnostic.frames.len();
            for (i, frame) in diagnostic.frames.iter().enumerate() {
                if count > 16 && (8..count - 8).contains(&i) {
                    if i == 8 {
                        write!(f, "\n  ... {} frames omitted ...", count - 16)?;
                    }
                    continue;
                }
                write!(
                    f,
                    "\n  at {} ({}:{})",
                    frame.function, frame.position.line, frame.position.column
                )?;
            }
            Ok(())
        } else if let Some(offset) = self.offset {
            write!(f, "{} at byte {}", self.message, offset)
        } else {
            f.write_str(&self.message)
        }
    }
}

impl std::error::Error for Error {}

/// An interpreter operation that may fail.
pub type Result<T> = std::result::Result<T, Error>;
