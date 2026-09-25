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
    /// Root-relative module filename bytes, or none for an unnamed source.
    pub filename: Option<Arc<[u8]>>,
    pub position: Position,
}

/// Source context and script call frames for a failure, without retained script values.
#[doc = include_str!("../docs/diagnostics.md")]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Diagnostic {
    /// Root-relative module filename bytes, or none for an unnamed source.
    pub filename: Option<Arc<[u8]>>,
    pub position: Position,
    pub code_frame: String,
    pub frames: Vec<StackFrame>,
}

/// The category of a compile or execution failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ErrorKind {
    /// An explicitly raised script exception.
    Runtime,
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
    /// An attached block transferred control outside the host callback.
    ControlFlow,
}

/// A script-visible exception class, independent of the host error category.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ErrorClass {
    Runtime,
    Standard,
    Assertion,
    Limit,
    Type,
    ZeroDivision,
    LocalJump,
    Argument,
}

impl ErrorClass {
    /// Returns the canonical Vibescript class name.
    pub fn name(self) -> &'static str {
        match self {
            Self::Runtime => "RuntimeError",
            Self::Standard => "StandardError",
            Self::Assertion => "AssertionError",
            Self::Limit => "LimitError",
            Self::Type => "TypeError",
            Self::ZeroDivision => "ZeroDivisionError",
            Self::LocalJump => "LocalJumpError",
            Self::Argument => "ArgumentError",
        }
    }

    /// Recognizes case-insensitive class names and the `Error` alias.
    pub fn from_name(name: &str) -> Option<Self> {
        let same = |canonical: &str| {
            name.chars()
                .map(|c| {
                    if c == 'ſ' {
                        's'
                    } else {
                        c.to_ascii_lowercase()
                    }
                })
                .eq(canonical.bytes().map(|b| b.to_ascii_lowercase() as char))
        };
        if same("Error") {
            return Some(Self::Runtime);
        }
        [
            Self::Runtime,
            Self::Standard,
            Self::Assertion,
            Self::Limit,
            Self::Type,
            Self::ZeroDivision,
            Self::LocalJump,
            Self::Argument,
        ]
        .into_iter()
        .find(|class| same(class.name()))
    }

    /// Tests a rescue filter against an exception class.
    ///
    /// `RuntimeError` includes every class; `StandardError` excludes `LimitError`.
    /// Execution exhaustion and host cancellation prohibit rescue independently.
    pub fn matches(self, error: Self) -> bool {
        match self {
            Self::Runtime => true,
            Self::Standard => error != Self::Limit,
            _ => self == error,
        }
    }
}

/// An interpreter failure with optional source context and a byte offset.
#[derive(Clone)]
pub struct Error {
    pub kind: ErrorKind,
    pub message: String,
    pub offset: Option<usize>,
    pub diagnostic: Option<Arc<Diagnostic>>,
    class: ErrorClass,
    required_syntax: bool,
    // A thin pointer leaves room for the reservation without enlarging Error.
    extra: Option<Arc<Extra>>,
    pub(crate) retained_charge: Option<Arc<crate::budget::Charge>>,
}

/// What an error carries beyond its message, rarely enough to share one
/// pointer.
#[derive(Debug, PartialEq, Eq)]
enum Extra {
    /// The original message bytes when they are not UTF-8.
    Raw(Box<[u8]>),
    /// The diagnostics behind a failed compilation.
    Diagnostics(Box<[crate::diagnostic::Diagnostic]>),
}

impl PartialEq for Error {
    fn eq(&self, other: &Self) -> bool {
        self.kind == other.kind
            && self.message == other.message
            && self.offset == other.offset
            && self.diagnostic == other.diagnostic
            && self.class == other.class
            && self.required_syntax == other.required_syntax
            && self.extra == other.extra
    }
}

impl Eq for Error {}

impl fmt::Debug for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Error")
            .field("kind", &self.kind)
            .field("message", &self.message)
            .field("offset", &self.offset)
            .field("diagnostic", &self.diagnostic)
            .field("class", &self.class)
            .field("required_syntax", &self.required_syntax)
            .field("extra", &self.extra)
            .finish()
    }
}

impl Error {
    /// Creates an error without a source location.
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            offset: None,
            diagnostic: None,
            required_syntax: false,
            extra: None,
            retained_charge: None,
            class: if matches!(
                kind,
                ErrorKind::OutputLimit
                    | ErrorKind::Steps
                    | ErrorKind::Memory
                    | ErrorKind::Recursion
            ) {
                ErrorClass::Limit
            } else {
                ErrorClass::Runtime
            },
        }
    }

    /// Returns the script exception class, or none for compile and host control errors.
    ///
    /// Syntax failures encountered by `require` are catchable runtime exceptions;
    /// syntax failures from host-side compilation have no script exception class.
    pub fn class(&self) -> Option<ErrorClass> {
        match self.kind {
            ErrorKind::Cancelled | ErrorKind::Deadline | ErrorKind::ControlFlow => None,
            ErrorKind::Syntax if !self.required_syntax => None,
            _ => Some(self.class),
        }
    }

    pub(crate) fn in_required_file(mut self) -> Self {
        self.required_syntax = self.kind == ErrorKind::Syntax;
        self
    }

    /// Replaces the message, keeping the kind, class and location.
    pub(crate) fn with_message(mut self, message: String) -> Self {
        self.message = message;
        if matches!(self.extra.as_deref(), Some(Extra::Raw(_))) {
            self.extra = None;
        }
        self
    }

    /// Sets the script exception class without changing the execution's budget state.
    pub fn with_class(mut self, class: ErrorClass) -> Self {
        self.class = class;
        self
    }

    /// Returns the original message bytes, including non-UTF-8 script strings.
    /// The public `message` field and Display use replacement characters for invalid UTF-8.
    pub fn message_bytes(&self) -> &[u8] {
        match self.extra.as_deref() {
            Some(Extra::Raw(bytes)) => bytes,
            _ => self.message.as_bytes(),
        }
    }

    /// The compile diagnostics behind a failed compilation, such as every type
    /// error the static checker found, in source order. Empty for other errors.
    ///
    /// The error's own message, offset and code frame describe the first error.
    pub fn diagnostics(&self) -> &[crate::diagnostic::Diagnostic] {
        match self.extra.as_deref() {
            Some(Extra::Diagnostics(diagnostics)) => diagnostics,
            _ => &[],
        }
    }

    /// Builds a compile error from diagnostics, of which at least one should be
    /// an error. The first error sets the message, offset and code frame.
    pub(crate) fn from_diagnostics(
        kind: ErrorKind,
        diagnostics: Vec<crate::diagnostic::Diagnostic>,
        source: &crate::source::Source,
    ) -> Self {
        let first = diagnostics
            .iter()
            .find(|diagnostic| diagnostic.is_error())
            .or(diagnostics.first());
        let (message, offset) = match first {
            Some(first) => (format!("{first}"), Some(first.span.start)),
            None => (String::from("compilation failed"), None),
        };
        let diagnostic = offset.map(|offset| {
            let offset = u32::try_from(offset).unwrap_or(u32::MAX);
            Arc::new(Diagnostic {
                filename: source.filename.clone(),
                position: source.position(offset),
                code_frame: source.frame(offset),
                frames: Vec::new(),
            })
        });
        Self {
            offset,
            diagnostic,
            extra: Some(Arc::new(Extra::Diagnostics(diagnostics.into()))),
            ..Self::new(kind, message).with_class(ErrorClass::Type)
        }
    }

    pub(crate) fn allocation_bytes(&self) -> usize {
        self.message.capacity()
            + match self.extra.as_deref() {
                Some(Extra::Raw(bytes)) => {
                    bytes.len()
                        + std::mem::size_of::<Box<[u8]>>()
                        + 2 * std::mem::size_of::<usize>()
                }
                _ => 0,
            }
    }

    pub(crate) fn from_bytes(ctx: &mut crate::CallContext, bytes: &[u8]) -> Result<Self> {
        struct Lossy<'a>(&'a [u8]);
        impl fmt::Display for Lossy<'_> {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                let mut bytes = self.0;
                loop {
                    match std::str::from_utf8(bytes) {
                        Ok(text) => return f.write_str(text),
                        Err(error) => {
                            let valid = error.valid_up_to();
                            f.write_str(std::str::from_utf8(&bytes[..valid]).unwrap())?;
                            f.write_str("\u{fffd}")?;
                            match error.error_len() {
                                Some(length) => bytes = &bytes[valid + length..],
                                None => return Ok(()),
                            }
                        }
                    }
                }
            }
        }
        ctx.work_bytes(bytes.len())?;
        let (message, _charge) = crate::source::formatted(ctx, format_args!("{}", Lossy(bytes)))?;
        let raw_message = if std::str::from_utf8(bytes).is_err() {
            let _charge = ctx.reserve(
                bytes.len() + std::mem::size_of::<Box<[u8]>>() + 2 * std::mem::size_of::<usize>(),
            )?;
            Some(Arc::new(Extra::Raw(bytes.into())))
        } else {
            None
        };
        Ok(Self {
            extra: raw_message,
            ..Self::new(ErrorKind::Runtime, message)
        })
    }

    pub(crate) fn syntax(
        work: &dyn crate::compilation::Work,
        offset: usize,
        message: impl fmt::Display,
    ) -> Self {
        crate::compilation::error(work, Some(offset), format_args!("{message}"))
    }

    pub(crate) fn limit(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self::new(kind, message).with_class(ErrorClass::Limit)
    }

    pub(crate) fn argument(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Argument, message).with_class(ErrorClass::Argument)
    }

    pub(crate) fn local_jump(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Argument, message).with_class(ErrorClass::LocalJump)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(diagnostic) = &self.diagnostic {
            if self.kind == ErrorKind::Syntax {
                write!(
                    f,
                    "parse error at {}: {}",
                    crate::source::Location {
                        filename: diagnostic.filename.as_deref(),
                        position: diagnostic.position,
                    },
                    self.message
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
                    "\n  at {} ({})",
                    frame.function,
                    crate::source::Location {
                        filename: frame.filename.as_deref(),
                        position: frame.position,
                    }
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
