//! Editor features for one document, usable without JSON-RPC.

use super::analysis::{self, Program};
use super::catalog::catalog;
use super::completion::{self, Index};
use super::{hover, navigation, signature, text};
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use vibescript::tooling::Outline;
use vibescript::{CancellationToken, Limits};

/// A zero-based line and UTF-16 code-unit offset, as the protocol counts them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Position {
    pub line: u32,
    pub character: u32,
}

impl Position {
    /// Creates a position from a zero-based line and UTF-16 offset.
    pub fn new(line: u32, character: u32) -> Self {
        Self { line, character }
    }
}

/// A range between two positions, exclusive of its end.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Range {
    pub start: Position,
    pub end: Position,
}

/// How serious a [`Diagnostic`] is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Severity {
    /// A compile error or a contradiction the static checker proved.
    Error,
    /// The static check stopped at its limits, so it may have missed errors.
    Warning,
    /// Code the static checker cannot analyze yet.
    Information,
}

impl Severity {
    /// The protocol's `DiagnosticSeverity` number.
    pub fn code(self) -> u8 {
        match self {
            Self::Error => 1,
            Self::Warning => 2,
            Self::Information => 3,
        }
    }
}

/// A problem in a document. The server publishes every diagnostic with the
/// source `vibes-lsp`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Diagnostic {
    pub range: Range,
    pub severity: Severity,
    pub message: String,
}

/// What a [`CompletionItem`] offers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum CompletionKind {
    /// A builtin member method.
    Method,
    /// A builtin or document function.
    Function,
    /// A parameter, local or rescue binding.
    Variable,
    /// A builtin namespace such as `JSON`.
    Module,
    Keyword,
    /// A builtin value that is not callable.
    Constant,
}

impl CompletionKind {
    /// The protocol's `CompletionItemKind` number.
    pub fn code(self) -> u8 {
        match self {
            Self::Method => 2,
            Self::Function => 3,
            Self::Variable => 6,
            Self::Module => 9,
            Self::Keyword => 14,
            Self::Constant => 21,
        }
    }
}

/// One completion.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct CompletionItem {
    pub label: String,
    pub kind: CompletionKind,
    /// A short description: a signature, `keyword`, `function`, `parameter`,
    /// `local`, or the receiver kinds that provide a member.
    pub detail: String,
    /// Markdown documentation.
    pub documentation: Option<String>,
}

/// What a [`Symbol`] declares.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum SymbolKind {
    Module,
    Class,
    Method,
    Enum,
    Function,
    Constant,
    EnumMember,
}

impl SymbolKind {
    /// The protocol's `SymbolKind` number.
    pub fn code(self) -> u8 {
        match self {
            Self::Module => 2,
            Self::Class => 5,
            Self::Method => 6,
            Self::Enum => 10,
            Self::Function => 12,
            Self::Constant => 14,
            Self::EnumMember => 22,
        }
    }
}

/// One entry of a document outline.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Symbol {
    /// The declared name; class methods read `self.name`.
    pub name: String,
    pub kind: SymbolKind,
    /// From the declaration line to the end of the last child's range.
    pub range: Range,
    /// The declaration line.
    pub selection_range: Range,
    pub children: Vec<Symbol>,
}

/// Parameter hints for the call around a position.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SignatureHelp {
    /// The signature, such as `charge(amount: int, currency = …) -> money`.
    pub label: String,
    pub parameters: Vec<String>,
    /// The zero-based parameter the position is in.
    pub active_parameter: u32,
}

/// Bounds for the static check a [`Document`] runs.
#[derive(Clone, Debug)]
pub struct Options {
    /// Step and memory quotas for the check. The defaults allow 20 million
    /// steps and 64 MiB, more than `vibes check` allows by default.
    pub limits: Limits,
    /// The check's deadline, measured from the start of each analysis.
    pub timeout: Duration,
    /// Cancels checks in progress, which then report no checker findings.
    pub cancellation: CancellationToken,
    /// Directories required files resolve from during the check. `None` uses
    /// the directory of a `file:` URI and resolves nothing for other URIs.
    pub module_paths: Option<Vec<PathBuf>>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            limits: Limits {
                steps: Some(20_000_000),
                memory_bytes: Some(64 << 20),
                ..Limits::default()
            },
            timeout: Duration::from_secs(2),
            cancellation: CancellationToken::new(),
            module_paths: None,
        }
    }
}

/// One analyzed version of a document: its diagnostics, and the declarations
/// that hover, completion, definitions, symbols and signature help use.
///
/// [`Document::update`] carries declarations across versions. When a new text
/// does not parse, navigation keeps the last parsed declarations, re-anchored
/// to the lines that still declare them, and completion and signature help
/// keep the last compiled ones.
///
/// ```
/// use vibescript_tools::lsp::{Document, Position, Severity};
///
/// let source = "def add(a: int, b: int) -> int\n  a + b\nend\n\nadd(1, \"2\")\n";
/// let document = Document::new("file:///tmp/add.vibe", source);
/// let diagnostic = &document.diagnostics()[0];
/// assert_eq!(diagnostic.severity, Severity::Error);
/// assert_eq!(diagnostic.message, r#""add": argument "b": expected int, got string"#);
///
/// let hover = document.hover(Position::new(4, 1)).unwrap();
/// assert_eq!(hover, "```vibe\ndef add(a: int, b: int) -> int\n```");
///
/// let items = document.completion(Position::new(1, 2));
/// assert!(items.iter().any(|item| item.label == "b" && item.detail == "parameter"));
///
/// let symbols = document.symbols();
/// assert_eq!(symbols[0].name, "add");
/// ```
#[derive(Debug)]
pub struct Document {
    pub(crate) uri: String,
    pub(crate) text: String,
    pub(crate) lines: Vec<String>,
    /// The last compiled declarations.
    pub(crate) compiled: Option<Arc<Outline>>,
    /// The last parsed declarations.
    pub(crate) program: Option<Arc<Outline>>,
    pub(crate) diagnostics: Vec<Diagnostic>,
    pub(crate) interrupted: bool,
    /// Built on first use, since the diagnostics path runs on every edit.
    pub(crate) completion: OnceLock<Option<Arc<Index>>>,
    pub(crate) symbols: OnceLock<Arc<Vec<Symbol>>>,
}

impl Document {
    /// Analyzes a document with the default [`Options`]. The URI locates
    /// required files: a `file:` URI resolves them from its directory.
    pub fn new(uri: &str, text: &str) -> Self {
        Self::analyze(uri, text, &Options::default())
    }

    /// Analyzes a document with explicit check bounds.
    pub fn analyze(uri: &str, text: &str, options: &Options) -> Self {
        let mut document = Self {
            uri: uri.to_owned(),
            text: String::new(),
            lines: Vec::new(),
            compiled: None,
            program: None,
            diagnostics: Vec::new(),
            interrupted: false,
            completion: OnceLock::new(),
            symbols: OnceLock::new(),
        };
        document.replace(text, options);
        document
    }

    /// Replaces the text and analyzes it, unless it is the text already
    /// analyzed to completion.
    pub fn update(&mut self, text: &str, options: &Options) {
        if text == self.text && !self.interrupted && !self.lines.is_empty() {
            return;
        }
        self.replace(text, options);
    }

    fn replace(&mut self, text: &str, options: &Options) {
        self.text = text.to_owned();
        self.lines = text::split_lines(text);
        self.completion = OnceLock::new();
        self.symbols = OnceLock::new();
        let analysis = analysis::analyze(&self.uri, text, options);
        match analysis.program {
            Program::Parsed(outline) => {
                let outline = Arc::new(outline);
                if analysis.compiled {
                    self.compiled = Some(outline.clone());
                }
                self.program = Some(outline);
            }
            Program::Kept => (),
            Program::Missing => {
                self.compiled = None;
                self.program = None;
            }
        }
        self.diagnostics = analysis.diagnostics;
        self.interrupted = analysis.cancelled;
    }

    /// The document's URI.
    pub fn uri(&self) -> &str {
        &self.uri
    }

    /// The analyzed text.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Compile errors, or when the text compiles, the static checker's
    /// findings in the document itself.
    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }

    /// Whether cancellation stopped the static check, so the diagnostics
    /// lack the checker's findings.
    pub fn interrupted(&self) -> bool {
        self.interrupted
    }

    /// Markdown documentation for the word at a position: builtin, member,
    /// namespace or keyword documentation, then the document's own
    /// declarations with their comments, then a one-line classification.
    pub fn hover(&self, position: Position) -> Option<String> {
        let (line, character) = (i64::from(position.line), i64::from(position.character));
        let word = text::word_at(&self.lines, line, character);
        if word.is_empty() {
            return None;
        }
        Some(hover::markdown(
            catalog(),
            self.program.as_deref(),
            &self.lines,
            line,
            character,
            &word,
        ))
    }

    /// Completions at a position: the receiver's members after a `.`,
    /// narrowed to its kind when a literal or an annotated parameter decides
    /// it; otherwise keywords, builtins, the document's functions and the
    /// enclosing function's parameters and locals.
    pub fn completion(&self, position: Position) -> Vec<CompletionItem> {
        self.completion_items(i64::from(position.line), i64::from(position.character))
            .iter()
            .map(|item| (**item).clone())
            .collect()
    }

    pub(crate) fn completion_items(&self, line: i64, character: i64) -> completion::Entries {
        let index = self.completion.get_or_init(|| {
            self.compiled
                .as_ref()
                .map(|compiled| Arc::new(Index::new(compiled, &self.lines, completion::builtins())))
        });
        completion::Request {
            source: &self.text,
            lines: &self.lines,
            line,
            character,
        }
        .entries(index.as_deref())
    }

    /// The declaration of the word at a position: a top-level function, a
    /// class or module and its members, or an enum and its members.
    pub fn definition(&self, position: Position) -> Option<Range> {
        let (line, character) = (i64::from(position.line), i64::from(position.character));
        let word = text::word_at(&self.lines, line, character);
        navigation::definition(self.program.as_deref(), &self.lines, &word)
    }

    /// The outline: functions, classes and modules with their methods,
    /// constants and nested modules, and enums with their members.
    pub fn symbols(&self) -> Vec<Symbol> {
        self.symbol_tree().as_ref().clone()
    }

    pub(crate) fn symbol_tree(&self) -> &Arc<Vec<Symbol>> {
        self.symbols
            .get_or_init(|| Arc::new(navigation::symbols(self.program.as_deref(), &self.lines)))
    }

    /// Parameter hints for the call around a position on its line.
    pub fn signature_help(&self, position: Position) -> Option<SignatureHelp> {
        signature::help(
            catalog(),
            self.compiled.as_deref(),
            &self.lines,
            i64::from(position.line),
            i64::from(position.character),
        )
    }
}
