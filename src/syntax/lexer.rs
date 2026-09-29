use crate::{
    Error, Result,
    compilation::{Boxed, Buffer, Bytes, Text, Work},
};
use std::cell::RefCell;

#[derive(Clone, Copy, Debug, Eq)]
pub(super) struct Word<'a>(&'a str);

impl<'a> Word<'a> {
    pub fn as_str(&self) -> &'a str {
        self.0
    }
}

impl std::ops::Deref for Word<'_> {
    type Target = str;

    fn deref(&self) -> &str {
        self.0
    }
}

impl AsRef<str> for Word<'_> {
    fn as_ref(&self) -> &str {
        self.0
    }
}

impl<T: AsRef<str> + ?Sized> PartialEq<T> for Word<'_> {
    fn eq(&self, other: &T) -> bool {
        self.0 == other.as_ref()
    }
}

#[derive(Debug, PartialEq)]
pub(super) enum Token<'a> {
    Regex(Bytes, u8),
    Word(Word<'a>),
    /// A symbol spelled by name or operator, without its colon.
    Symbol(Word<'a>),
    /// A quoted symbol's decoded name.
    QuotedSymbol(Bytes),
    Int(u64),
    BigInt(Text, u32),
    Float(f64),
    Bytes(Bytes),
    Template(Buffer<Part<'a>>),
    Words(Boxed<Words<'a>>),
    Invalid(Boxed<Invalid>),
    P(char),
    Op(&'static str),
    EndLine,
    Eof,
}

/// A token the parser rejects.
#[derive(Debug, PartialEq)]
pub(super) struct Invalid {
    pub offset: usize,
    pub message: Text,
    pub failure: Failure,
}

/// How Go reports an invalid token.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Failure {
    /// A lexer diagnostic, reported wherever the parser meets it.
    Diagnostic,
    /// A character no token starts with, which Go names an invalid token.
    Character,
    /// A literal Go lexes as valid and rejects only when parsing it, named
    /// by its Go label elsewhere.
    Literal(&'static str),
}

#[derive(Debug, PartialEq)]
pub(super) enum Part<'a> {
    Text(Bytes),
    /// An interpolation's tokens and the byte span of its content between `#{` and `}`.
    Expr(Buffer<Lexeme<'a>>, (u32, u32)),
}

#[derive(Debug, PartialEq)]
pub(super) struct Words<'a> {
    pub entries: Buffer<Buffer<Part<'a>>>,
    pub symbol: bool,
    pub ambiguous: bool,
}

#[derive(Debug, PartialEq)]
pub(super) struct Lexeme<'a> {
    pub token: Token<'a>,
    pub offset: usize,
    pub end: usize,
    pub line: usize,
    pub end_line: usize,
}

impl Lexeme<'_> {
    fn copy(&self, work: &dyn Work) -> Result<Self> {
        Ok(Self {
            token: self.token.copy(work)?,
            offset: self.offset,
            end: self.end,
            line: self.line,
            end_line: self.end_line,
        })
    }
}

impl Token<'_> {
    pub(super) fn copy(&self, work: &dyn Work) -> Result<Self> {
        work.checkpoint()?;
        Ok(match self {
            Self::Regex(bytes, flags) => Self::Regex(bytes.clone(), *flags),
            Self::Word(word) => Self::Word(*word),
            Self::Symbol(word) => Self::Symbol(*word),
            Self::QuotedSymbol(bytes) => Self::QuotedSymbol(bytes.clone()),
            Self::Int(value) => Self::Int(*value),
            Self::BigInt(value, radix) => Self::BigInt(value.clone(), *radix),
            Self::Float(value) => Self::Float(*value),
            Self::Bytes(bytes) => Self::Bytes(bytes.clone()),
            Self::Template(parts) => Self::Template(copy_parts(parts, work)?),
            Self::Words(words) => {
                let entries = words
                    .entries
                    .copy_with(work, |parts| copy_parts(parts, work))?;
                Self::Words(Boxed::new(
                    work,
                    Words {
                        entries,
                        symbol: words.symbol,
                        ambiguous: words.ambiguous,
                    },
                )?)
            }
            Self::Invalid(invalid) => Self::Invalid(Boxed::new(
                work,
                Invalid {
                    offset: invalid.offset,
                    message: invalid.message.clone(),
                    failure: invalid.failure,
                },
            )?),
            Self::P(value) => Self::P(*value),
            Self::Op(value) => Self::Op(value),
            Self::EndLine => Self::EndLine,
            Self::Eof => Self::Eof,
        })
    }
}

fn copy_parts<'a>(parts: &Buffer<Part<'a>>, work: &dyn Work) -> Result<Buffer<Part<'a>>> {
    parts.copy_with(work, |part| {
        Ok(match part {
            Part::Text(bytes) => Part::Text(bytes.clone()),
            Part::Expr(tokens, span) => {
                Part::Expr(tokens.copy_with(work, |token| token.copy(work))?, *span)
            }
        })
    })
}

/// Go's message for a string whose interpolations nest too deeply.
pub(super) const INTERPOLATION_TOO_DEEP: &str = "string interpolation nesting too deep";
/// The lexer's message for a character no token starts with.
pub(super) const UNSUPPORTED_CHARACTER: &str = "unsupported character";
const UNTERMINATED_STRING: &str = "unterminated string";

/// Go's operator symbols, matched against the source in this order.
const OPERATOR_SYMBOLS: [&str; 22] = [
    "[]=", "[]", "===", "<=>", "**", "<<", "<=", ">=", "==", "!=", "&&", "||", "+", "-", "*", "/",
    "%", "<", ">", "&", "|", "!",
];

// Go's label-colon scans stack one lexer per pending keyword label.
const MAX_SCANS: usize = 128;

/// A token as Go's lexer sees it when it classifies a colon. Line breaks are
/// not Go tokens; semicolons are.
#[derive(Clone, Copy)]
struct Recent {
    label: bool,
    ends: bool,
    string: bool,
    mark: u8,
    offset: usize,
    end: usize,
    line: usize,
    end_line: usize,
}

impl Recent {
    fn of(lexeme: &Lexeme<'_>, source: &str) -> Option<Self> {
        let (label, ends, string, mark) = match &lexeme.token {
            Token::EndLine if source.as_bytes().get(lexeme.offset) == Some(&b';') => {
                (false, false, false, b';')
            }
            Token::EndLine | Token::Eof => return None,
            Token::Word(word) if word.starts_with('@') => (false, true, false, 0),
            Token::Word(word) => (
                true,
                !super::keyword(word)
                    || matches!(word.as_str(), "true" | "false" | "nil" | "self" | "end"),
                false,
                0,
            ),
            Token::Bytes(_) => (false, true, true, 0),
            Token::Regex(..)
            | Token::Symbol(_)
            | Token::QuotedSymbol(_)
            | Token::Int(_)
            | Token::BigInt(..)
            | Token::Float(_)
            | Token::Template(_)
            | Token::Words(_)
            | Token::P(')' | ']' | '}') => (false, true, false, 0),
            Token::Invalid(invalid) => (
                false,
                matches!(invalid.failure, Failure::Literal(_)),
                false,
                0,
            ),
            Token::P(c @ (',' | '{' | '(')) => (false, false, false, *c as u8),
            Token::P(_) | Token::Op(_) => (false, false, false, 0),
        };
        Some(Self {
            label,
            ends,
            string,
            mark,
            offset: lexeme.offset,
            end: lexeme.end,
            line: lexeme.line,
            end_line: lexeme.end_line,
        })
    }
}

/// The previous lexeme, which decides regex and percent-literal readings.
#[derive(Clone, Copy)]
struct Tail {
    ends: bool,
    def: bool,
    end_line: usize,
}

impl Tail {
    fn of(lexeme: &Lexeme<'_>) -> Self {
        Self {
            ends: ends_expression(&lexeme.token),
            def: matches!(&lexeme.token, Token::Word(name) if name == "def"),
            end_line: lexeme.end_line,
        }
    }
}

/// The bracket and ternary nesting Go's lexer tracks to tell a separator
/// colon from a symbol, and the tokens before the current one.
#[derive(Default)]
struct Nesting {
    /// Each open bracket and whether it follows an expression on its line:
    /// a `(` then opens call arguments, and a `{` a block.
    brackets: Buffer<(u8, bool)>,
    /// Each pending ternary's bracket depth and whether a label colon inside
    /// it belongs to a parenless keyword call.
    ternaries: Buffer<(usize, bool)>,
    last: Option<Recent>,
    previous: Option<Recent>,
    before: Option<Recent>,
}

impl Nesting {
    fn copy(&self, work: &dyn Work) -> Result<Self> {
        Ok(Self {
            brackets: self.brackets.copy_with(work, |frame| Ok(*frame))?,
            ternaries: self.ternaries.copy_with(work, |frame| Ok(*frame))?,
            last: self.last,
            previous: self.previous,
            before: self.before,
        })
    }

    fn remember(&mut self, recent: Recent) {
        self.before = self.previous;
        self.previous = self.last;
        self.last = Some(recent);
    }
}

/// Answers to Go's label-colon scans, by colon offset and ternary depth.
type Memo = RefCell<Buffer<(usize, usize, bool)>>;

struct Lexer<'a, 'w> {
    work: &'w dyn crate::compilation::Work,
    source: &'a str,
    pos: usize,
    limit: usize,
    depth: usize,
    speculative: usize,
    tail: Option<Tail>,
    nesting: Nesting,
    memo: &'w Memo,
    /// The ternary depth a label-colon scan waits to fall below, and how many
    /// scans enclose this one.
    scan: Option<(usize, usize)>,
    found: bool,
}

pub(super) fn lex<'a>(
    source: &'a str,
    work: &dyn crate::compilation::Work,
) -> Result<Buffer<Lexeme<'a>>> {
    if source.len() > super::MAX_SOURCE {
        return Err(Error::syntax(work, 0, "source exceeds 8 MiB"));
    }
    let memo = RefCell::new(Buffer::new());
    Lexer::new(
        source,
        0,
        source.len(),
        0,
        source.len().saturating_mul(4),
        work,
        &memo,
    )
    .tokens(source.len(), 0, false, false, None)
}

pub(super) fn modulo<'a>(
    source: &'a str,
    start: &Lexeme<'a>,
    limit: usize,
    depth: usize,
    work: &dyn crate::compilation::Work,
) -> Result<Buffer<Lexeme<'a>>> {
    let memo = RefCell::new(Buffer::new());
    let speculative = limit.saturating_sub(start.offset).saturating_mul(4);
    Lexer::new(source, start.offset, limit, depth, speculative, work, &memo)
        .tokens(start.end, start.line, false, true, None)
}

pub(super) fn resume<'a>(
    source: &'a str,
    start: &Lexeme<'a>,
    until: usize,
    limit: usize,
    depth: usize,
    previous: Option<&Lexeme<'a>>,
    work: &dyn crate::compilation::Work,
) -> Result<Buffer<Lexeme<'a>>> {
    let memo = RefCell::new(Buffer::new());
    let speculative = limit.saturating_sub(start.offset).saturating_mul(4);
    Lexer::new(source, start.offset, limit, depth, speculative, work, &memo)
        .tokens(until, start.line, false, false, previous)
}

pub(super) fn regex<'a>(
    source: &'a str,
    start: &Lexeme<'a>,
    limit: usize,
    depth: usize,
    work: &dyn crate::compilation::Work,
) -> Result<Buffer<Lexeme<'a>>> {
    let memo = RefCell::new(Buffer::new());
    Lexer::new(source, start.offset, limit, depth, 0, work, &memo).tokens(
        start.offset + 1,
        start.line,
        false,
        false,
        None,
    )
}

impl<'a, 'w> Lexer<'a, 'w> {
    fn new(
        source: &'a str,
        pos: usize,
        limit: usize,
        depth: usize,
        speculative: usize,
        work: &'w dyn crate::compilation::Work,
        memo: &'w Memo,
    ) -> Self {
        Self {
            work,
            source,
            pos,
            limit,
            depth,
            speculative,
            tail: None,
            nesting: Nesting::default(),
            memo,
            scan: None,
            found: false,
        }
    }

    fn tokens(
        &mut self,
        until: usize,
        mut line: usize,
        interpolation: bool,
        skip_first_percent: bool,
        previous: Option<&Lexeme<'a>>,
    ) -> Result<Buffer<Lexeme<'a>>> {
        let source = self.source;
        let s = &source.as_bytes()[..self.limit];
        let mut out = Buffer::<Lexeme<'a>>::new();
        // Like Go, an interpolation ends at a `}` outside every bracket it opened.
        let (mut braces, mut brackets, mut parens) = (0usize, 0usize, 0usize);
        if let Some(previous) = previous {
            self.tail = Some(Tail::of(previous));
            if let Some(recent) = Recent::of(previous, source) {
                self.nesting.remember(recent);
            }
        }
        let first = self.pos;
        while self.pos < until {
            self.work.charge(1)?;
            let start = self.pos;
            let mut i = start;
            if interpolation && s[i] == b'}' && braces == 0 && brackets == 0 && parens == 0 {
                out.push(
                    self.work,
                    Lexeme {
                        token: Token::Eof,
                        offset: i,
                        end: i,
                        line,
                        end_line: line,
                    },
                )?;
                self.pos += 1;
                return Ok(out);
            }
            match s[i] {
                b' ' | b'\t' | b'\r' => {
                    self.pos += 1;
                    continue;
                }
                b'#' => {
                    while self.pos < self.limit && s[self.pos] != b'\n' {
                        self.work.charge(1)?;
                        self.pos += 1;
                    }
                    continue;
                }
                b'=' if self.block_comment_starts(start) => match self.block_comment(&mut line) {
                    Ok(()) => continue,
                    Err(error) if error.kind == crate::ErrorKind::Syntax && !interpolation => {
                        let token = self.invalid(start, &error.message, Failure::Diagnostic)?;
                        out.push(
                            self.work,
                            Lexeme {
                                token,
                                offset: start,
                                end: self.limit,
                                line,
                                end_line: line,
                            },
                        )?;
                        self.pos = self.limit;
                        break;
                    }
                    Err(error) => return Err(error),
                },
                _ => (),
            }
            let before = self.nesting.ternaries.len();
            let scanned = (|| -> Result<Token<'a>> {
                let initial = source[i..self.limit].chars().next().unwrap();
                if initial == '@' {
                    let token = self.variable(start);
                    i = self.pos;
                    return Ok(token);
                }
                if initial == '_' || super::unicode::letter(initial) {
                    i += initial.len_utf8();
                    while let Some(c) = source[i..self.limit].chars().next() {
                        self.work.charge(1)?;
                        if !identifier(c) {
                            break;
                        }
                        i += c.len_utf8();
                    }
                    i = self.name_end(i);
                    return Ok(Token::Word(Word(&source[start..i])));
                }
                if super::unicode::digit(initial) {
                    let token = self.number(start)?;
                    i = self.pos;
                    return Ok(token);
                }
                Ok(match s[i] {
                    b'/' if self
                        .tail
                        .is_none_or(|last| !last.def && (last.end_line < line || !last.ends)) =>
                    {
                        let token = self.regex()?;
                        i = self.pos;
                        token
                    }
                    b'\n' | b';' => {
                        i += 1;
                        Token::EndLine
                    }
                    b'\'' | b'"' => {
                        let parts = self.quoted(s[i], line)?;
                        i = self.pos;
                        if parts.iter().any(|p| matches!(p, Part::Expr(..))) {
                            Token::Template(parts)
                        } else {
                            Token::Bytes(plain(parts, self.work)?)
                        }
                    }
                    b':' if s.get(i + 1) != Some(&b':') => {
                        let token = self.colon(start, line)?;
                        i = self.pos;
                        token
                    }
                    b'%' if !(skip_first_percent && i == first)
                        && !super::canonical()
                        && self.percent_kind().is_some() =>
                    {
                        let ambiguous = self
                            .tail
                            .is_some_and(|previous| previous.end_line == line && previous.ends);
                        if ambiguous && self.speculative == 0 {
                            i += 1;
                            Token::Op("%")
                        } else {
                            match self.words(line, ambiguous) {
                                Ok(token) => {
                                    if interpolation
                                        && ambiguous
                                        && s.get(self.pos)
                                            .is_some_and(|c| matches!(c, b'\'' | b'"'))
                                    {
                                        self.pos = start;
                                        i += 1;
                                        Token::Op("%")
                                    } else {
                                        i = self.pos;
                                        token
                                    }
                                }
                                Err(error)
                                    if error.kind == crate::ErrorKind::Syntax
                                        && ambiguous
                                        && error.message != INTERPOLATION_TOO_DEEP =>
                                {
                                    self.speculative =
                                        self.speculative.saturating_sub(self.pos - start);
                                    self.pos = start;
                                    i += 1;
                                    Token::Op("%")
                                }
                                Err(error) => return Err(error),
                            }
                        }
                    }
                    _ => {
                        let mut found = None;
                        // A slash reaches this arm only where it divides, so `//`
                        // after an operand is floor division.
                        for op in [
                            "=>", "...", "..", "===", "<=>", "||=", "&&=", "**=", "//=", "==",
                            "!=", "<=", ">=", "&&", "||", "+=", "-=", "*=", "/=", "%=", "**", "//",
                            "<<", "::", "->", "=~", "!~", "&.",
                        ] {
                            if s[i..].starts_with(op.as_bytes()) {
                                found = Some(op);
                                break;
                            }
                        }
                        if let Some(op) = found {
                            i += op.len();
                            Token::Op(op)
                        } else {
                            i += 1;
                            match s[start] {
                                b'+' => Token::Op("+"),
                                b'-' => Token::Op("-"),
                                b'*' => Token::Op("*"),
                                b'/' => Token::Op("/"),
                                b'%' => Token::Op("%"),
                                b'=' => Token::Op("="),
                                b'<' => Token::Op("<"),
                                b'>' => Token::Op(">"),
                                b'!' => Token::Op("!"),
                                b'&' => Token::Op("&"),
                                b'(' | b')' | b'[' | b']' | b'{' | b'}' | b',' | b'.' | b':'
                                | b'?' | b'|' => Token::P(s[start] as char),
                                _ => {
                                    return Err(Error::syntax(
                                        self.work,
                                        start,
                                        UNSUPPORTED_CHARACTER,
                                    ));
                                }
                            }
                        }
                    }
                })
            })();
            let token = match scanned {
                Ok(token) => token,
                Err(error) if error.kind == crate::ErrorKind::Syntax && !interpolation => {
                    // Resolving an earlier percent token may require re-lexing this suffix.
                    i = self.limit;
                    if super::recovery::lexical() {
                        i = source[start..self.limit]
                            .find('\n')
                            .map_or(self.limit, |end| start + end);
                    }
                    let failure = if error.message == UNSUPPORTED_CHARACTER {
                        Failure::Character
                    } else {
                        Failure::Diagnostic
                    };
                    self.invalid(error.offset.unwrap_or(start), &error.message, failure)?
                }
                Err(error) => return Err(error),
            };
            self.work.bytes(i - start)?;
            let start_line = line;
            line += s[start..i].iter().filter(|&&b| b == b'\n').count();
            match token {
                Token::P('{') => braces += 1,
                Token::P('}') => braces = braces.saturating_sub(1),
                Token::P('[') => brackets += 1,
                Token::P(']') => brackets = brackets.saturating_sub(1),
                Token::P('(') => parens += 1,
                Token::P(')') => parens = parens.saturating_sub(1),
                _ => (),
            }
            let lexeme = Lexeme {
                token,
                offset: start,
                end: i,
                line: start_line,
                end_line: line,
            };
            self.follow(&lexeme)?;
            self.pos = i;
            if let Some((outer, _)) = self.scan {
                let stops = matches!(lexeme.token, Token::Invalid(_))
                    || (lexeme.token == Token::EndLine && s[start] == b';');
                if stops {
                    return Ok(out);
                }
                if before >= outer && self.nesting.ternaries.len() < outer {
                    self.found = true;
                    return Ok(out);
                }
                continue;
            }
            out.push(self.work, lexeme)?;
        }
        if interpolation {
            return Err(Error::syntax(
                self.work,
                self.pos,
                "unterminated string interpolation",
            ));
        }
        out.push(
            self.work,
            Lexeme {
                token: Token::Eof,
                offset: self.pos,
                end: self.pos,
                line,
                end_line: line,
            },
        )?;
        Ok(out)
    }

    fn invalid(&self, offset: usize, message: &str, failure: Failure) -> Result<Token<'a>> {
        Ok(Token::Invalid(Boxed::new(
            self.work,
            Invalid {
                offset,
                message: Text::new(self.work, message)?,
                failure,
            },
        )?))
    }

    /// Updates the bracket, ternary and token state Go's lexer keeps as it
    /// reads a token.
    fn follow(&mut self, lexeme: &Lexeme<'a>) -> Result<()> {
        let nesting = &mut self.nesting;
        match lexeme.token {
            Token::P(open @ ('(' | '[' | '{')) => {
                let call = open != '['
                    && nesting
                        .last
                        .is_some_and(|last| last.ends && last.end_line == lexeme.line);
                nesting.brackets.push(self.work, (open as u8, call))?;
            }
            Token::P(')' | ']' | '}') => {
                nesting.brackets.pop();
                let depth = nesting.brackets.len();
                while nesting
                    .ternaries
                    .last()
                    .is_some_and(|&(ternary, _)| ternary > depth)
                {
                    nesting.ternaries.pop();
                }
            }
            Token::P('?') => {
                let depth = nesting.brackets.len();
                nesting.ternaries.push(self.work, (depth, false))?;
            }
            _ => (),
        }
        self.tail = Some(Tail::of(lexeme));
        if let Some(recent) = Recent::of(lexeme, self.source) {
            self.nesting.remember(recent);
        }
        Ok(())
    }

    /// Reports whether a `=begin` at `start` opens a block comment: it must
    /// lead its line and end at whitespace, as in Go.
    fn block_comment_starts(&self, start: usize) -> bool {
        self.marker(start, "=begin")
    }

    fn marker(&self, start: usize, marker: &str) -> bool {
        let s = &self.source.as_bytes()[..self.limit];
        s[start..].starts_with(marker.as_bytes())
            && s.get(start + marker.len())
                .is_none_or(|c| matches!(c, b' ' | b'\t' | b'\r' | b'\n'))
            && s[..start]
                .iter()
                .rev()
                .take_while(|&&c| c != b'\n')
                .all(|c| matches!(c, b' ' | b'\t' | b'\r'))
    }

    fn block_comment(&mut self, line: &mut usize) -> Result<()> {
        let s = &self.source.as_bytes()[..self.limit];
        let start = self.pos;
        let mut rest_of_line = |lexer: &mut Self, newline: bool| -> Result<()> {
            while lexer.pos < lexer.limit && s[lexer.pos] != b'\n' {
                lexer.work.charge(1)?;
                lexer.pos += 1;
            }
            if newline && lexer.pos < lexer.limit {
                lexer.pos += 1;
                *line += 1;
            }
            Ok(())
        };
        rest_of_line(self, true)?;
        loop {
            while self.pos < self.limit && matches!(s[self.pos], b' ' | b'\t' | b'\r') {
                self.work.charge(1)?;
                self.pos += 1;
            }
            if self.pos >= self.limit {
                return Err(Error::syntax(
                    self.work,
                    start,
                    "unterminated block comment",
                ));
            }
            if self.marker(self.pos, "=end") {
                return rest_of_line(self, false);
            }
            rest_of_line(self, true)?;
        }
    }

    /// Reads an instance or class variable as Go does: the character after
    /// the sigils always belongs to the name, followed by identifier characters.
    fn variable(&mut self, start: usize) -> Token<'a> {
        let source = &self.source[..self.limit];
        let mut i = start + 1;
        if source.as_bytes().get(i) == Some(&b'@') {
            i += 1;
        }
        if let Some(c) = source[i..].chars().next() {
            i += c.len_utf8();
            while let Some(c) = source[i..].chars().next().filter(|&c| identifier(c)) {
                i += c.len_utf8();
            }
        }
        i = self.name_end(i);
        self.pos = i;
        Token::Word(Word(&self.source[start..i]))
    }

    // `=` and `~` end the scan, so only its last character can hide `!=`,
    // `?=` or `!~`. Keep other malformed names intact for the parser's fix.
    fn name_end(&self, end: usize) -> usize {
        let bytes = &self.source.as_bytes()[..self.limit];
        let last = bytes.get(end.wrapping_sub(1));
        let assigns = matches!(last, Some(b'?' | b'!'))
            && bytes.get(end) == Some(&b'=')
            && !matches!(bytes.get(end + 1), Some(b'=' | b'~'));
        if assigns || (last == Some(&b'!') && bytes.get(end) == Some(&b'~')) {
            end - 1
        } else {
            end
        }
    }

    /// Reads a numeric literal with Go's rules.
    fn number(&mut self, start: usize) -> Result<Token<'a>> {
        let source = &self.source[..self.limit];
        let at = |i: usize| source.get(i..).and_then(|rest| rest.chars().next());
        let first = at(start).unwrap();
        if first == '0'
            && let Some(radix) = at(start + 1).and_then(|marker| match marker {
                'x' | 'X' => Some(16),
                'b' | 'B' => Some(2),
                'o' | 'O' => Some(8),
                'd' | 'D' => Some(10),
                _ => None,
            })
        {
            let mut i = start + 2;
            let mut text = Buffer::new();
            let valid = loop {
                self.work.charge(1)?;
                match at(i) {
                    Some('_') => {
                        if !text.is_empty() && at(i + 1).is_some_and(|c| base_digit(c, radix)) {
                            i += 1;
                            continue;
                        }
                        break false;
                    }
                    Some(c) if base_digit(c, radix) => {
                        text.push(self.work, c as u8)?;
                        i += 1;
                    }
                    next => {
                        break !text.is_empty()
                            && !next.is_some_and(numeric_trail)
                            && !(next == Some('.')
                                && at(i + 1).is_some_and(|c| c.is_ascii_digit()));
                    }
                }
            };
            if !valid {
                while at(i).is_some_and(numeric_trail) {
                    self.work.charge(1)?;
                    i += at(i).unwrap().len_utf8();
                }
                if at(i) == Some('.') && at(i + 1).is_some_and(super::unicode::digit) {
                    i += 1;
                    while at(i).is_some_and(numeric_trail) {
                        self.work.charge(1)?;
                        i += at(i).unwrap().len_utf8();
                    }
                }
                self.pos = i;
                return Err(Error::syntax(self.work, start, "invalid numeric literal"));
            }
            self.pos = i;
            return self.integer(text, radix, start, 2);
        }
        let mut text = Buffer::new();
        let push = |text: &mut Buffer<u8>, c: char| {
            text.extend_from_slice(self.work, c.encode_utf8(&mut [0; 4]).as_bytes())
        };
        let mut i = start;
        let mut last = first;
        let mut dot = false;
        let mut exponent = false;
        let mut malformed = None;
        push(&mut text, first)?;
        i += first.len_utf8();
        loop {
            self.work.charge(1)?;
            let Some(c) = at(i) else { break };
            if c == '_' {
                if super::unicode::digit(last) && at(i + 1).is_some_and(super::unicode::digit) {
                    i += 1;
                    last = c;
                    continue;
                }
                break;
            } else if c == '.' && !dot && !exponent && at(i + 1).is_some_and(super::unicode::digit)
            {
                dot = true;
                push(&mut text, c)?;
                i += 1;
                last = c;
            } else if matches!(c, 'e' | 'E')
                && !exponent
                && at(i + 1).is_some_and(|n| super::unicode::digit(n) || matches!(n, '+' | '-'))
            {
                exponent = true;
                push(&mut text, c)?;
                i += 1;
                last = c;
                if let Some(sign @ ('+' | '-')) = at(i) {
                    push(&mut text, sign)?;
                    i += 1;
                    last = sign;
                }
                if !at(i).is_some_and(super::unicode::digit) {
                    while at(i).is_some_and(identifier) {
                        self.work.charge(1)?;
                        i += at(i).unwrap().len_utf8();
                    }
                    malformed = Some(if c == 'e' {
                        "malformed exponent in numeric literal: expected digits after 'e'"
                    } else {
                        "malformed exponent in numeric literal: expected digits after 'E'"
                    });
                    break;
                }
                loop {
                    self.work.charge(1)?;
                    match at(i) {
                        Some('_') => {
                            if super::unicode::digit(last)
                                && at(i + 1).is_some_and(super::unicode::digit)
                            {
                                i += 1;
                                last = '_';
                                continue;
                            }
                            i += 1;
                            while at(i).is_some_and(identifier) {
                                self.work.charge(1)?;
                                i += at(i).unwrap().len_utf8();
                            }
                            malformed = Some(
                                "malformed exponent in numeric literal: underscore must sit between exponent digits",
                            );
                            break;
                        }
                        Some(d) if super::unicode::digit(d) => {
                            push(&mut text, d)?;
                            i += d.len_utf8();
                            last = d;
                        }
                        _ => break,
                    }
                }
                if malformed.is_some() {
                    break;
                }
            } else if super::unicode::digit(c) {
                push(&mut text, c)?;
                i += c.len_utf8();
                last = c;
            } else {
                break;
            }
        }
        if malformed.is_none()
            && let Some(c) = at(i).filter(|&c| c == '_' || super::unicode::letter(c))
        {
            // A name may abut a number only when the whole name is a keyword
            // (`5if cond`, `1e3end`).
            let mut end = i + c.len_utf8();
            while let Some(c) = at(end).filter(|&c| identifier(c)) {
                self.work.charge(1)?;
                end += c.len_utf8();
            }
            if !super::keyword(&source[i..end]) {
                i = end;
                malformed = Some(
                    "malformed numeric literal: identifier cannot immediately follow a number",
                );
            }
        }
        self.pos = i;
        if let Some(message) = malformed {
            return Err(Error::syntax(self.work, start, message));
        }
        let text_bytes = text;
        let text = std::str::from_utf8(&text_bytes).unwrap();
        if dot || exponent {
            return match text.parse() {
                Ok(value) if text.is_ascii() => Ok(Token::Float(value)),
                _ => self.invalid(start, "invalid float literal", Failure::Literal("float")),
            };
        }
        if !text.is_ascii() {
            // Go's integer conversion reports an overflow before it reaches a
            // non-ASCII digit, and then only rejects a literal by its length.
            let digits = text.len() - text.trim_start_matches(|c: char| c.is_ascii_digit()).len();
            let overflows = text[..digits].parse::<u64>().is_err() && digits > 0;
            let message = if overflows && text.len() > 100_000 {
                "integer literal exceeds 100000 digits"
            } else {
                "invalid integer literal"
            };
            return self.invalid(start, message, Failure::Literal("integer"));
        }
        self.integer(text_bytes, 10, start, 0)
    }

    fn integer(
        &self,
        text: Buffer<u8>,
        radix: u32,
        offset: usize,
        prefix: usize,
    ) -> Result<Token<'a>> {
        let parsed = u64::from_str_radix(std::str::from_utf8(&text).unwrap(), radix);
        if text.len() + prefix > 100_000 && !parsed.as_ref().is_ok_and(|&n| n <= i64::MAX as u64) {
            return self.invalid(
                offset,
                "integer literal exceeds 100000 digits",
                Failure::Literal("integer"),
            );
        }
        Ok(match parsed {
            Ok(n) => Token::Int(n),
            Err(_) => Token::BigInt(
                Text::from_bytes(Bytes::new(self.work, text)?).unwrap(),
                radix,
            ),
        })
    }

    /// Reads a colon as Go's lexer does: the separator of a label, hash key or
    /// ternary, or the start of a symbol.
    fn colon(&mut self, start: usize, line: usize) -> Result<Token<'a>> {
        let source = &self.source[..self.limit];
        let closes = self.colon_closes_ternary(start, line)?;
        if closes {
            self.nesting.ternaries.pop();
        }
        let next = source[start + 1..].chars().next();
        let symbol = !closes && !self.colon_separates_value(start);
        if symbol && matches!(next, Some('"' | '\'')) {
            self.pos = start + 1;
            let parts = match self.quoted(next.unwrap() as u8, line) {
                Ok(parts) => parts,
                Err(error) if error.kind == crate::ErrorKind::Syntax => {
                    return Err(Error::syntax(self.work, start, &error.message));
                }
                Err(error) => return Err(error),
            };
            if parts.iter().any(|p| matches!(p, Part::Expr(..))) {
                return Err(Error::syntax(
                    self.work,
                    start,
                    "interpolation is not allowed in a symbol literal",
                ));
            }
            return Ok(Token::QuotedSymbol(plain(parts, self.work)?));
        }
        if symbol
            && let Some(op) = OPERATOR_SYMBOLS
                .into_iter()
                .find(|op| source[start + 1..].starts_with(op))
        {
            self.pos = start + 1 + op.len();
            return Ok(Token::Symbol(Word(&self.source[start + 1..self.pos])));
        }
        if !closes && !symbol {
            self.pos = start + 1;
            return Ok(Token::P(':'));
        }
        let mut end = start + 1;
        while let Some(c) = source[end..].chars().next().filter(|&c| identifier(c)) {
            self.work.charge(1)?;
            end += c.len_utf8();
        }
        end = self.name_end(end);
        if end > start + 1 {
            self.pos = end;
            return Ok(Token::Symbol(Word(&self.source[start + 1..end])));
        }
        self.pos = start + 1;
        Ok(Token::P(':'))
    }

    fn colon_closes_ternary(&mut self, start: usize, line: usize) -> Result<bool> {
        let Some(&(depth, parenless)) = self.nesting.ternaries.last() else {
            return Ok(false);
        };
        if depth != self.nesting.brackets.len() {
            return Ok(false);
        }
        let label = self.nesting.last.is_some_and(|last| last.label)
            && (parenless || self.label_follows_callee() || self.label_follows_comma());
        if label && self.label_precedes_separator(start, line)? {
            self.nesting.ternaries.last_mut().unwrap().1 = true;
            return Ok(false);
        }
        Ok(self.nesting.last.is_some_and(|last| last.ends))
    }

    fn colon_separates_value(&self, start: usize) -> bool {
        let nesting = &self.nesting;
        let Some(last) = nesting.last else {
            return false;
        };
        if last.label {
            let abuts = self.source[..start]
                .chars()
                .next_back()
                .is_some_and(|c| !c.is_whitespace());
            return abuts
                || self.label_follows_bracket()
                || self.label_follows_callee()
                || self.label_follows_comma();
        }
        last.string
            && nesting
                .previous
                .is_some_and(|previous| matches!(previous.mark, b'{' | b','))
            && nesting
                .brackets
                .last()
                .is_some_and(|&(kind, block)| kind == b'{' && !block)
    }

    fn label_follows_bracket(&self) -> bool {
        let nesting = &self.nesting;
        let (Some(&(kind, call)), Some(previous)) = (nesting.brackets.last(), nesting.previous)
        else {
            return false;
        };
        // A block's statements start with no label, so `{ break :done }`
        // after a call breaks with a symbol.
        match previous.mark {
            b'{' => kind == b'{' && !call,
            b'(' => kind == b'(' && call,
            b',' => (kind == b'{' && !call) || (kind == b'(' && call),
            _ => false,
        }
    }

    fn label_follows_callee(&self) -> bool {
        let nesting = &self.nesting;
        let (Some(last), Some(previous)) = (nesting.last, nesting.previous) else {
            return false;
        };
        previous.ends && previous.end_line == last.line && previous.end < last.offset
    }

    fn label_follows_comma(&self) -> bool {
        let nesting = &self.nesting;
        let (Some(last), Some(previous), Some(before)) =
            (nesting.last, nesting.previous, nesting.before)
        else {
            return false;
        };
        previous.mark == b','
            && before.ends
            && before.end_line == previous.line
            && previous.end_line == last.line
            && previous.end < last.offset
    }

    /// Reports whether a later colon closes the pending ternary, which makes
    /// this one a keyword label. Like Go, it lexes ahead and remembers the answer.
    fn label_precedes_separator(&mut self, start: usize, line: usize) -> Result<bool> {
        let outer = self.nesting.ternaries.len();
        let key = (start, outer);
        let found = {
            let memo = self.memo.borrow();
            memo.binary_search_by(|&(offset, depth, _)| (offset, depth).cmp(&key))
                .map(|index| memo[index].2)
        };
        if let Ok(answer) = found {
            return Ok(answer);
        }
        let scans = self.scan.map_or(0, |(_, scans)| scans) + 1;
        if scans > MAX_SCANS {
            return Err(Error::syntax(self.work, start, super::TOO_DEEP));
        }
        let mut scan = Lexer {
            work: self.work,
            source: self.source,
            pos: start + 1,
            limit: self.limit,
            depth: self.depth,
            speculative: self.speculative,
            tail: self.tail,
            nesting: self.nesting.copy(self.work)?,
            memo: self.memo,
            scan: Some((outer, scans)),
            found: false,
        };
        scan.tokens(self.limit, line, false, false, None)?;
        let answer = scan.found;
        let mut memo = self.memo.borrow_mut();
        let index = memo
            .binary_search_by(|&(offset, depth, _)| (offset, depth).cmp(&key))
            .unwrap_or_else(|index| index);
        memo.insert(self.work, index, (start, outer, answer))?;
        Ok(answer)
    }

    fn quoted(&mut self, quote: u8, mut line: usize) -> Result<Buffer<Part<'a>>> {
        let start = self.pos;
        self.pos += 1;
        let mut parts = Buffer::new();
        let mut text = Buffer::new();
        while self.pos < self.limit {
            self.work.charge(1)?;
            let before = self.pos;
            match self.source.as_bytes()[self.pos] {
                b if b == quote => {
                    self.pos += 1;
                    flush(&mut parts, &mut text, self.work)?;
                    return Ok(parts);
                }
                b'\\' if quote == b'"' => self.escape(&mut text, Some(start))?,
                b'\\' => {
                    self.pos += 1;
                    if self.pos < self.limit
                        && matches!(self.source.as_bytes()[self.pos], b'\'' | b'\\')
                    {
                        text.push(self.work, self.source.as_bytes()[self.pos])?;
                        self.pos += 1;
                    } else {
                        text.push(self.work, b'\\')?;
                    }
                }
                b'#' if quote == b'"'
                    && self.source.as_bytes().get(self.pos + 1) == Some(&b'{') =>
                {
                    flush(&mut parts, &mut text, self.work)?;
                    match self.interpolation(line) {
                        Ok(part) => parts.push(self.work, part)?,
                        // Go's lexer reports a string whose interpolation it
                        // cannot read as unterminated, at the string.
                        Err(error) if error.kind == crate::ErrorKind::Syntax => {
                            let message = if error.message == INTERPOLATION_TOO_DEEP {
                                INTERPOLATION_TOO_DEEP
                            } else {
                                UNTERMINATED_STRING
                            };
                            return Err(Error::syntax(self.work, start, message));
                        }
                        Err(error) => return Err(error),
                    }
                }
                0 => return Err(Error::syntax(self.work, start, UNTERMINATED_STRING)),
                byte => {
                    text.push(self.work, byte)?;
                    self.pos += 1;
                }
            }
            line += self.source.as_bytes()[before..self.pos]
                .iter()
                .filter(|&&b| b == b'\n')
                .count();
        }
        Err(Error::syntax(self.work, start, UNTERMINATED_STRING))
    }

    fn regex(&mut self) -> Result<Token<'a>> {
        let start = self.pos;
        let bytes = &self.source.as_bytes()[..self.limit];
        self.pos += 1;
        let body = self.pos;
        let mut class = false;
        let mut members = 0;
        while self.pos < self.limit {
            self.work.charge(1)?;
            match bytes[self.pos] {
                0 | b'\n' => break,
                b'\\' => {
                    self.pos += 1;
                    if self.pos == self.limit || matches!(bytes[self.pos], 0 | b'\n') {
                        break;
                    }
                    self.pos += self.source[self.pos..self.limit]
                        .chars()
                        .next()
                        .unwrap()
                        .len_utf8();
                    members += 1;
                }
                b'[' if !class => {
                    class = true;
                    members = 0;
                    self.pos += 1;
                    if bytes.get(self.pos) == Some(&b'^') {
                        self.pos += 1;
                    }
                }
                b'[' if class && bytes.get(self.pos + 1) == Some(&b':') => {
                    let mut end = self.pos + 2;
                    if bytes.get(end) == Some(&b'^') {
                        end += 1;
                    }
                    let name = end;
                    while bytes.get(end).is_some_and(u8::is_ascii_alphabetic) {
                        self.work.charge(1)?;
                        end += 1;
                    }
                    if end > name && bytes.get(end..end + 2) == Some(b":]") {
                        self.pos = end + 2;
                    } else {
                        self.pos += 1;
                    }
                    members += 1;
                }
                b']' if class => {
                    if members != 0 {
                        class = false;
                    }
                    members += 1;
                    self.pos += 1;
                }
                b'/' if !class => {
                    let pattern = Bytes::from_slice(self.work, &bytes[body..self.pos])?;
                    self.pos += 1;
                    let mut flags = 0;
                    let mut failure = None;
                    while bytes.get(self.pos).is_some_and(u8::is_ascii_alphabetic) {
                        self.work.charge(1)?;
                        let flag = bytes[self.pos];
                        let bit = match flag {
                            b'i' => 1,
                            b'm' => 2,
                            _ => 0,
                        };
                        // Go validates the flags in order when it parses the literal.
                        if failure.is_none() {
                            if bit == 0 {
                                failure = Some(format!(
                                    "unsupported regex flag \"{}\"; supported flags are i and m",
                                    flag as char
                                ));
                            } else if flags & bit != 0 {
                                failure = Some(format!("repeated regex flag \"{}\"", flag as char));
                            }
                        }
                        flags |= bit;
                        self.pos += 1;
                    }
                    if let Some(message) = failure {
                        self.work.bytes(message.len())?;
                        return self.invalid(start, &message, Failure::Literal("\"regex\""));
                    }
                    return Ok(Token::Regex(pattern, flags));
                }
                _ => {
                    self.pos += self.source[self.pos..self.limit]
                        .chars()
                        .next()
                        .unwrap()
                        .len_utf8();
                    members += 1;
                }
            }
        }
        Err(Error::syntax(
            self.work,
            start,
            "unterminated regex literal",
        ))
    }

    fn interpolation(&mut self, line: usize) -> Result<Part<'a>> {
        if self.depth >= 8 {
            return Err(Error::syntax(self.work, self.pos, INTERPOLATION_TOO_DEEP));
        }
        self.pos += 2;
        let start = self.pos as u32;
        self.depth += 1;
        // Go reads an interpolation with a fresh lexer.
        let tail = self.tail.take();
        let nesting = std::mem::take(&mut self.nesting);
        let scan = self.scan.take();
        let result = self.tokens(self.limit, line, true, false, None);
        self.tail = tail;
        self.nesting = nesting;
        self.scan = scan;
        self.depth -= 1;
        Ok(Part::Expr(result?, (start, self.pos as u32)))
    }

    fn percent_kind(&self) -> Option<(u8, char, char)> {
        let kind = *self.source.as_bytes().get(self.pos + 1)?;
        if !matches!(kind, b'w' | b'i' | b'W' | b'I') {
            return None;
        }
        let open = self.source.get(self.pos + 2..self.limit)?.chars().next()?;
        if open == '\0'
            || open == '_'
            || open.is_whitespace()
            || super::unicode::letter_or_digit(open)
        {
            return None;
        }
        let close = match open {
            '[' => ']',
            '(' => ')',
            '{' => '}',
            '<' => '>',
            _ => open,
        };
        Some((kind, open, close))
    }

    fn words(&mut self, mut line: usize, ambiguous: bool) -> Result<Token<'a>> {
        let start = self.pos;
        let (kind, open, close) = self.percent_kind().unwrap();
        self.pos += 2 + open.len_utf8();
        let mut nesting = 1;
        let mut words = Buffer::new();
        let mut parts = Buffer::new();
        let mut text = Buffer::new();
        let mut in_word = false;
        let interpolating = kind.is_ascii_uppercase();
        while self.pos < self.limit {
            self.work.charge(1)?;
            let before = self.pos;
            let c = self.source[self.pos..self.limit].chars().next().unwrap();
            if c == '\0' {
                break;
            }
            if c == '\\' {
                if interpolating {
                    self.escape(&mut text, None)?;
                } else {
                    self.pos += 1;
                    if let Some(c) = self.source[self.pos..self.limit].chars().next() {
                        if !(c.is_whitespace() || c == '\\' || c == open || c == close) {
                            text.push(self.work, b'\\')?;
                        }
                        text.extend_from_slice(self.work, c.encode_utf8(&mut [0; 4]).as_bytes())?;
                        self.pos += c.len_utf8();
                    } else {
                        text.push(self.work, b'\\')?;
                    }
                }
                in_word = true;
            } else if interpolating
                && c == '#'
                && close != '#'
                && self.source.as_bytes().get(self.pos + 1) == Some(&b'{')
            {
                flush(&mut parts, &mut text, self.work)?;
                match self.interpolation(line) {
                    Ok(part) => parts.push(self.work, part)?,
                    Err(error) if error.kind == crate::ErrorKind::Syntax => {
                        let message = if error.message == INTERPOLATION_TOO_DEEP {
                            INTERPOLATION_TOO_DEEP
                        } else {
                            "unterminated string interpolation in percent array literal"
                        };
                        return Err(Error::syntax(self.work, start, message));
                    }
                    Err(error) => return Err(error),
                }
                in_word = true;
            } else {
                self.pos += c.len_utf8();
                if c == close {
                    nesting -= 1;
                    if nesting == 0 {
                        if in_word {
                            flush(&mut parts, &mut text, self.work)?;
                            words.push(self.work, parts)?;
                        }
                        return Ok(Token::Words(Boxed::new(
                            self.work,
                            Words {
                                entries: words,
                                symbol: matches!(kind, b'i' | b'I'),
                                ambiguous,
                            },
                        )?));
                    }
                } else if open != close && c == open {
                    nesting += 1;
                }
                if c.is_whitespace() {
                    if in_word {
                        flush(&mut parts, &mut text, self.work)?;
                        words.push(self.work, std::mem::take(&mut parts))?;
                        in_word = false;
                    }
                } else {
                    text.extend_from_slice(self.work, c.encode_utf8(&mut [0; 4]).as_bytes())?;
                    in_word = true;
                }
            }
            line += self.source.as_bytes()[before..self.pos]
                .iter()
                .filter(|&&b| b == b'\n')
                .count();
        }
        Err(Error::syntax(
            self.work,
            start,
            "unterminated percent array literal",
        ))
    }

    /// Decodes an escape. A string reports a malformed hexadecimal escape at
    /// its opening quote, as Go does; a word array keeps it literally.
    fn escape(&mut self, out: &mut Buffer<u8>, string: Option<usize>) -> Result<()> {
        self.pos += 1;
        let Some(c) = self.source[self.pos..self.limit].chars().next() else {
            out.push(self.work, b'\\')?;
            return Ok(());
        };
        self.pos += c.len_utf8();
        match c {
            'a' => out.push(self.work, 7)?,
            'b' => out.push(self.work, 8)?,
            'e' => out.push(self.work, 27)?,
            'f' => out.push(self.work, 12)?,
            'n' => out.push(self.work, b'\n')?,
            'r' => out.push(self.work, b'\r')?,
            't' => out.push(self.work, b'\t')?,
            'v' => out.push(self.work, 11)?,
            'x' | 'u' => {
                let start = self.pos;
                let mut value = 0;
                let max = if c == 'x' { 2 } else { 4 };
                while self.pos < self.limit && self.pos - start < max {
                    self.work.charge(1)?;
                    let Some(digit) = (self.source.as_bytes()[self.pos] as char).to_digit(16)
                    else {
                        break;
                    };
                    value = value * 16 + digit;
                    self.pos += 1;
                }
                let digits = self.pos - start;
                let failure = if c == 'x' {
                    (digits == 0).then_some("invalid hex escape in string")
                } else if digits != 4 {
                    Some("invalid 4-digit hex escape in string")
                } else {
                    char::from_u32(value)
                        .is_none()
                        .then_some("invalid Unicode escape in string")
                };
                if let Some(message) = failure {
                    if let Some(string) = string {
                        return Err(Error::syntax(self.work, string, message));
                    }
                    self.pos = start;
                    out.push(self.work, c as u8)?;
                } else if c == 'x' {
                    out.push(self.work, value as u8)?;
                } else {
                    out.extend_from_slice(
                        self.work,
                        char::from_u32(value)
                            .unwrap()
                            .encode_utf8(&mut [0; 4])
                            .as_bytes(),
                    )?;
                }
            }
            _ => out.extend_from_slice(self.work, c.encode_utf8(&mut [0; 4]).as_bytes())?,
        }
        Ok(())
    }
}

fn identifier(c: char) -> bool {
    matches!(c, '_' | '?' | '!') || super::unicode::letter_or_digit(c)
}

fn base_digit(c: char, radix: u32) -> bool {
    c.is_ascii() && c.to_digit(16).is_some_and(|digit| digit < radix)
}

/// A character that makes a based literal malformed when it follows the digits.
fn numeric_trail(c: char) -> bool {
    c == '_' || super::unicode::letter_or_digit(c)
}

fn flush(parts: &mut Buffer<Part<'_>>, text: &mut Buffer<u8>, work: &dyn Work) -> Result<()> {
    if !text.is_empty() {
        let bytes = Bytes::new(work, std::mem::take(text))?;
        parts.push(work, Part::Text(bytes))?;
    }
    Ok(())
}

pub(super) fn plain(mut parts: Buffer<Part<'_>>, work: &dyn Work) -> Result<Bytes> {
    if parts.len() == 1 {
        let Part::Text(bytes) = parts.pop().unwrap() else {
            unreachable!()
        };
        work.bytes(bytes.len())?;
        return Ok(bytes);
    }
    let mut bytes = Buffer::new();
    for part in parts {
        let Part::Text(text) = part else {
            unreachable!()
        };
        bytes.extend_from_slice(work, &text)?;
    }
    Bytes::new(work, bytes)
}

fn ends_expression(token: &Token<'_>) -> bool {
    match token {
        Token::Word(w) => {
            !super::keyword(w) || matches!(w.as_str(), "true" | "false" | "nil" | "self" | "end")
        }
        Token::Int(_)
        | Token::BigInt(..)
        | Token::Float(_)
        | Token::Bytes(_)
        | Token::Regex(..)
        | Token::Symbol(_)
        | Token::QuotedSymbol(_)
        | Token::Template(_)
        | Token::Words(..)
        | Token::P(')' | ']' | '}') => true,
        Token::Invalid(invalid) => matches!(invalid.failure, Failure::Literal(_)),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CallContext, CallOptions, compilation::Meter};

    #[test]
    fn compilation_invalid_token_copies_share_the_accounted_message() {
        let mut context = CallContext::new(CallOptions::default());
        let work = Meter(std::cell::RefCell::new(&mut context));
        let tokens = lex("$", &work).unwrap();
        let copy = tokens[0].token.copy(&work).unwrap();
        let (Token::Invalid(original), Token::Invalid(duplicate)) = (&tokens[0].token, &copy)
        else {
            panic!("expected deferred lexer errors");
        };
        assert_eq!(original.message.as_str(), "unsupported character");
        assert_eq!(original.message.as_ptr(), duplicate.message.as_ptr());
        drop(tokens);
        assert!(context.stats().retained_memory_bytes > 0);
        let Token::Invalid(duplicate) = &copy else {
            unreachable!()
        };
        assert_eq!(duplicate.message.as_str(), "unsupported character");
        drop(copy);
        assert_eq!(context.stats().retained_memory_bytes, 0);
    }
    use std::cell::RefCell;

    #[test]
    fn numbers_abutting_keywords_split_before_the_keyword() {
        for (source, number, keyword) in [
            ("5if", Token::Int(5), "if"),
            ("5end", Token::Int(5), "end"),
            ("5else", Token::Int(5), "else"),
            ("5elsif", Token::Int(5), "elsif"),
            ("5ensure", Token::Int(5), "ensure"),
            ("5enum", Token::Int(5), "enum"),
            ("5true", Token::Int(5), "true"),
            ("1_0unless", Token::Int(10), "unless"),
            ("1e3if", Token::Float(1000.0), "if"),
            ("1E+3end", Token::Float(1000.0), "end"),
            ("1e1_0if", Token::Float(1e10), "if"),
            ("2.5end", Token::Float(2.5), "end"),
            ("2.5e-1while", Token::Float(0.25), "while"),
        ] {
            let mut context = CallContext::new(CallOptions::default());
            let tokens = lex(source, &Meter(RefCell::new(&mut context))).unwrap();
            assert_eq!(tokens.len(), 3, "{source}");
            assert_eq!(tokens[0].token, number, "{source}");
            assert_eq!(tokens[0].offset, 0, "{source}");
            assert_eq!(tokens[0].end, source.len() - keyword.len(), "{source}");
            assert_eq!(tokens[1].token, Token::Word(Word(keyword)), "{source}");
            assert_eq!(tokens[1].offset, source.len() - keyword.len(), "{source}");
            assert_eq!(tokens[1].end, source.len(), "{source}");
            assert_eq!(tokens[2].token, Token::Eof, "{source}");
        }
    }

    #[test]
    fn numbers_abutting_names_or_malformed_exponents_stay_invalid() {
        const SUFFIX: &str =
            "malformed numeric literal: identifier cannot immediately follow a number";
        for (source, message) in [
            ("5ifx", SUFFIX),
            ("5if_foo", SUFFIX),
            ("5if?", SUFFIX),
            ("5end!", SUFFIX),
            ("5ifé", SUFFIX),
            ("5end名", SUFFIX),
            ("5elf", SUFFIX),
            ("5_if", SUFFIX),
            ("123abc", SUFFIX),
            ("1.5x", SUFFIX),
            ("1e3foo", SUFFIX),
            ("1e", SUFFIX),
            ("1e_3", SUFFIX),
            ("1__0", SUFFIX),
            (
                "1e+",
                "malformed exponent in numeric literal: expected digits after 'e'",
            ),
            (
                "1E+end",
                "malformed exponent in numeric literal: expected digits after 'E'",
            ),
            (
                "1e3_",
                "malformed exponent in numeric literal: underscore must sit between exponent digits",
            ),
            (
                "1e3__4",
                "malformed exponent in numeric literal: underscore must sit between exponent digits",
            ),
            ("0x5if", "invalid numeric literal"),
            ("0d", "invalid numeric literal"),
            ("0b102", "invalid numeric literal"),
            ("0x1١", "invalid numeric literal"),
        ] {
            let mut context = CallContext::new(CallOptions::default());
            let input = format!("{source} 1");
            let tokens = lex(&input, &Meter(RefCell::new(&mut context))).unwrap();
            let Token::Invalid(invalid) = &tokens[0].token else {
                panic!(
                    "{source}: expected an invalid literal, got {:?}",
                    tokens[0].token
                );
            };
            assert_eq!(invalid.offset, 0, "{source}");
            assert_eq!(invalid.message.as_str(), message, "{source}");
            assert_eq!(invalid.failure, Failure::Diagnostic, "{source}");
        }
        for (source, message, label) in [
            ("1١", "invalid integer literal", "integer"),
            ("١", "invalid integer literal", "integer"),
            ("1.0١", "invalid float literal", "float"),
            ("1e2١", "invalid float literal", "float"),
        ] {
            let mut context = CallContext::new(CallOptions::default());
            let input = format!("{source} 1");
            let tokens = lex(&input, &Meter(RefCell::new(&mut context))).unwrap();
            let Token::Invalid(invalid) = &tokens[0].token else {
                panic!(
                    "{source}: expected a deferred literal, got {:?}",
                    tokens[0].token
                );
            };
            assert_eq!(invalid.message.as_str(), message, "{source}");
            assert_eq!(invalid.failure, Failure::Literal(label), "{source}");
            assert_eq!(tokens[1].token, Token::Int(1), "{source}");
        }
    }

    #[test]
    fn colons_follow_go_symbol_and_separator_rules() {
        let symbol = |name| Token::Symbol(Word(name));
        for (source, expected) in [
            (":1", vec![symbol("1")]),
            (":0xFF", vec![symbol("0xFF")]),
            (":->", vec![symbol("-"), Token::Op(">")]),
            (":=~", vec![Token::P(':'), Token::Op("=~")]),
            (":[]=", vec![symbol("[]=")]),
            (
                "a:b",
                vec![
                    Token::Word(Word("a")),
                    Token::P(':'),
                    Token::Word(Word("b")),
                ],
            ),
            (
                "c ?1:2",
                vec![
                    Token::Word(Word("c")),
                    Token::P('?'),
                    Token::Int(1),
                    symbol("2"),
                ],
            ),
            (
                "c ? 1 : 2",
                vec![
                    Token::Word(Word("c")),
                    Token::P('?'),
                    Token::Int(1),
                    Token::P(':'),
                    Token::Int(2),
                ],
            ),
        ] {
            let mut context = CallContext::new(CallOptions::default());
            let tokens = lex(source, &Meter(RefCell::new(&mut context))).unwrap();
            let got: Vec<_> = tokens
                .iter()
                .map(|lexeme| lexeme.token.copy(&()).unwrap())
                .filter(|token| *token != Token::Eof)
                .collect();
            assert_eq!(got, expected, "{source}");
        }
    }

    #[test]
    fn long_numeric_suffixes_obey_work_limits_and_release_storage() {
        for source in [
            format!("1{}", "x".repeat(4096)),
            format!("1e3{}", "if".repeat(2048)),
            format!("5{}", "名".repeat(4096)),
        ] {
            let mut context = CallContext::new(CallOptions {
                limits: crate::Limits {
                    steps: Some(64),
                    ..crate::Limits::default()
                },
                ..CallOptions::default()
            });
            let error = lex(&source, &Meter(RefCell::new(&mut context))).unwrap_err();
            assert_eq!(error.kind, crate::ErrorKind::Steps);
            assert_eq!(context.charge(0).unwrap_err().kind, crate::ErrorKind::Steps);
            assert_eq!(context.stats().retained_memory_bytes, 0);
        }
    }

    #[test]
    fn identifier_tokens_borrow_the_source_without_copying_long_names() {
        let mut peaks = Vec::new();
        for length in [1, 8192] {
            let source = "名".repeat(length);
            let mut context = CallContext::new(CallOptions::default());
            let tokens = lex(&source, &Meter(RefCell::new(&mut context))).unwrap();
            let Token::Word(word) = &tokens[0].token else {
                panic!("expected identifier");
            };
            assert_eq!(word.as_str(), source);
            assert_eq!(word.as_ptr(), source.as_ptr());
            peaks.push(context.stats().peak_memory_bytes);
            drop(tokens);
            assert_eq!(context.stats().retained_memory_bytes, 0);
        }
        assert_eq!(peaks[0], peaks[1]);
    }
}
