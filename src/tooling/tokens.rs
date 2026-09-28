//! The token stream the parser reads, for tools that rewrite source in place.

use std::ops::Range;

/// One token of a parsed source, with its byte span.
///
/// Whitespace and comments fall between tokens, so a tool that edits only
/// token spans keeps them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Token {
    pub kind: TokenKind,
    /// The token's bytes in the source.
    pub span: Range<usize>,
    /// The zero-based line the token starts on.
    pub line: usize,
}

/// What a [`Token`] is.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum TokenKind {
    /// An identifier, keyword, instance variable or constant; the source
    /// span holds its text.
    Word,
    /// A symbol literal and its decoded name. `quoted` marks `:"name"`.
    Symbol {
        name: Vec<u8>,
        quoted: bool,
    },
    Integer,
    Float,
    /// A string without interpolation, and its decoded bytes.
    String(Vec<u8>),
    /// A string with interpolation: the byte span of each interpolation's
    /// content between `#{` and `}`.
    Template(Vec<Range<usize>>),
    /// A percent literal such as `%w[a b]`: whether it makes symbols, and
    /// each entry's decoded bytes, or `None` for an interpolated entry.
    Words {
        symbols: bool,
        entries: Vec<Option<Vec<u8>>>,
    },
    Regex,
    /// A bracket or separator: `(`, `)`, `[`, `]`, `{`, `}`, `,`, `.`, `:`,
    /// `?` or `|`.
    Punct(char),
    /// An operator, such as `+`, `==`, `&.`, `::` or `=>`.
    Operator(&'static str),
    Newline,
    Semicolon,
    /// A token the parser rejects where it appears.
    Invalid,
    Eof,
}

/// Parses source and returns the tokens the parser read, in order.
///
/// These are the lexer's tokens after the parser's own re-reading, where a
/// `/` or `%` that the lexer read as an operator starts a regex or percent
/// literal, so each token is what the compiler saw. Tokens inside a string
/// interpolation are not listed; [`TokenKind::Template`] gives the byte span
/// of each interpolation, which parses as source of its own. Parse failures
/// return the same error as [`Engine::compile`](crate::Engine::compile).
///
/// ```
/// use vibescript::tooling::{TokenKind, tokens};
/// let source = "puts /a b/\nx = y / 2 # half\n";
/// let kinds: Vec<_> = tokens(source)?.into_iter().map(|token| token.kind).collect();
/// assert_eq!(kinds[1], TokenKind::Regex);
/// assert_eq!(kinds[6], TokenKind::Operator("/"));
/// assert_eq!(kinds.last(), Some(&TokenKind::Eof));
/// # Ok::<(), vibescript::Error>(())
/// ```
pub fn tokens(source: &str) -> crate::Result<Vec<Token>> {
    crate::syntax::record::tokens(source)
        .map_err(|error| crate::source::parse_error(source, None, error, &()))
}
