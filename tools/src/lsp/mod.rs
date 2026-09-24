//! A Language Server Protocol server for Vibescript, and the editor features
//! behind it as plain functions.
//!
//! The server follows the Go reference's `vibes lsp`: full-text document
//! sync, diagnostics on every open and change, hover documentation,
//! completion with member narrowing, signature help, go-to-definition,
//! document symbols and formatting. Diagnostics report compile errors and,
//! beyond the reference, the static checker's findings.
//!
//! Three layers serve different hosts:
//!
//! - [`Document`] analyzes one text and answers hover, completion,
//!   definition, symbol and signature queries without any protocol.
//! - [`Server`] takes one JSON-RPC message at a time and returns the messages
//!   to send, so an editor or web IDE can carry it over any transport.
//! - [`serve`] runs a server over a Content-Length-framed byte stream pair,
//!   as `vibes lsp` does over stdin and stdout.
//!
//! Positions are zero-based lines and UTF-16 offsets, as in the protocol.
//! See `docs/lsp.md` for the protocol details and the differences from the
//! reference.
//!
//! ```
//! use vibescript_tools::lsp::{Document, Position};
//!
//! let document = Document::new("file:///project/main.vibe", "def run\n  puts(\"hi\")\nend\n");
//! assert!(document.diagnostics().is_empty());
//! let hover = document.hover(Position::new(1, 3)).unwrap();
//! assert!(hover.contains("Writes each value"));
//! ```

mod analysis;
mod catalog;
mod completion;
mod contracts;
mod docs;
mod document;
mod hover;
mod json;
mod navigation;
mod serve;
mod server;
mod signature;
mod text;
mod transport;

#[cfg(test)]
mod tests;

pub use document::{
    CompletionItem, CompletionKind, Diagnostic, Document, Options, Position, Range, Severity,
    SignatureHelp, Symbol, SymbolKind,
};
pub use serve::serve;
pub use server::Server;
