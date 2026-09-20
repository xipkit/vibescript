use crate::{
    Error, Result,
    compilation::{Boxed, Buffer, Bytes, Text, Work},
};

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
    Int(u64),
    BigInt(Text, u32),
    Float(f64),
    Bytes(Bytes),
    Template(Buffer<Part<'a>>),
    Words(Boxed<Words<'a>>),
    Invalid(Boxed<(usize, Text)>),
    P(char),
    Op(&'static str),
    EndLine,
    Eof,
}

#[derive(Debug, PartialEq)]
pub(super) enum Part<'a> {
    Text(Bytes),
    Expr(Buffer<Lexeme<'a>>),
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
            Self::Invalid(error) => Self::Invalid(Boxed::new(work, (error.0, error.1.clone()))?),
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
            Part::Expr(tokens) => Part::Expr(tokens.copy_with(work, |token| token.copy(work))?),
        })
    })
}

struct Lexer<'a, 'w> {
    work: &'w dyn crate::compilation::Work,
    source: &'a str,
    pos: usize,
    limit: usize,
    depth: usize,
    speculative: usize,
}

pub(super) fn lex<'a>(
    source: &'a str,
    work: &dyn crate::compilation::Work,
) -> Result<Buffer<Lexeme<'a>>> {
    if source.len() > super::MAX_SOURCE {
        return Err(Error::syntax(work, 0, "source exceeds 8 MiB"));
    }
    Lexer {
        work,
        source,
        pos: 0,
        limit: source.len(),
        depth: 0,
        speculative: source.len().saturating_mul(4),
    }
    .tokens(source.len(), 0, false, false, None)
}

pub(super) fn modulo<'a>(
    source: &'a str,
    start: &Lexeme<'a>,
    limit: usize,
    depth: usize,
    work: &dyn crate::compilation::Work,
) -> Result<Buffer<Lexeme<'a>>> {
    Lexer {
        work,
        source,
        pos: start.offset,
        limit,
        depth,
        speculative: limit.saturating_sub(start.offset).saturating_mul(4),
    }
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
    Lexer {
        work,
        source,
        pos: start.offset,
        limit,
        depth,
        speculative: limit.saturating_sub(start.offset).saturating_mul(4),
    }
    .tokens(until, start.line, false, false, previous)
}

pub(super) fn regex<'a>(
    source: &'a str,
    start: &Lexeme<'a>,
    limit: usize,
    depth: usize,
    work: &dyn crate::compilation::Work,
) -> Result<Buffer<Lexeme<'a>>> {
    Lexer {
        work,
        source,
        pos: start.offset,
        limit,
        depth,
        speculative: 0,
    }
    .tokens(start.offset + 1, start.line, false, false, None)
}

impl<'a> Lexer<'a, '_> {
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
        let mut braces = 0usize;
        let first = self.pos;
        while self.pos < until {
            self.work.charge(1)?;
            let start = self.pos;
            let mut i = start;
            if interpolation && s[i] == b'}' && braces == 0 {
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
                _ => (),
            }
            let scanned = (|| -> Result<Token<'a>> {
                let initial = source[i..self.limit].chars().next().unwrap();
                if initial == '@' {
                    i += 1;
                    if s.get(i) == Some(&b'@') {
                        i += 1;
                    }
                    let Some(first) = source[i..self.limit].chars().next() else {
                        return Err(Error::syntax(
                            self.work,
                            start,
                            "expected variable name after @",
                        ));
                    };
                    if first != '_' && !super::unicode::letter(first) {
                        return Err(Error::syntax(
                            self.work,
                            start,
                            "expected variable name after @",
                        ));
                    }
                    i += first.len_utf8();
                    while let Some(c) = source[i..self.limit].chars().next() {
                        self.work.charge(1)?;
                        if c != '_' && !super::unicode::letter_or_digit(c) {
                            break;
                        }
                        i += c.len_utf8();
                    }
                    return Ok(Token::Word(Word(&source[start..i])));
                }
                if initial == '_' || super::unicode::letter(initial) {
                    i += initial.len_utf8();
                    while let Some(c) = source[i..self.limit].chars().next() {
                        self.work.charge(1)?;
                        if !matches!(c, '_' | '?' | '!') && !super::unicode::letter_or_digit(c) {
                            break;
                        }
                        i += c.len_utf8();
                    }
                    return Ok(Token::Word(Word(&source[start..i])));
                }
                Ok(match s[i] {
                    b'/' if out.last().or(previous).is_none_or(|last| {
                        if matches!(&last.token, Token::Word(name) if name == "def") {
                            return false;
                        }
                        if last.token == Token::P(':') && last.end == i {
                            let label = out
                                .len()
                                .checked_sub(2)
                                .and_then(|index| out.get(index))
                                .is_some_and(|word| {
                                    matches!(word.token, Token::Word(_)) && word.end == last.offset
                                });
                            if !label {
                                return false;
                            }
                        }
                        last.end_line < line || !ends_expression(&last.token)
                    }) =>
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
                        if parts.iter().any(|p| matches!(p, Part::Expr(_))) {
                            Token::Template(parts)
                        } else {
                            Token::Bytes(plain(parts, self.work)?)
                        }
                    }
                    b'%' if !(skip_first_percent && i == first)
                        && self.percent_kind().is_some() =>
                    {
                        let ambiguous = out.last().or(previous).is_some_and(|previous| {
                            previous.end_line == line && ends_expression(&previous.token)
                        });
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
                                        && !error.message.contains("nesting") =>
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
                    b'0'..=b'9' => {
                        i += 1;
                        if s[start] == b'0'
                            && s.get(i).is_some_and(|b| {
                                matches!(b, b'x' | b'X' | b'b' | b'B' | b'o' | b'O' | b'd' | b'D')
                            })
                        {
                            let radix = match s[i] {
                                b'x' | b'X' => 16,
                                b'b' | b'B' => 2,
                                b'd' | b'D' => 10,
                                _ => 8,
                            };
                            i += 1;
                            let digits = i;
                            while i < self.limit && (s[i].is_ascii_alphanumeric() || s[i] == b'_') {
                                self.work.charge(1)?;
                                i += 1;
                            }
                            if source[i..self.limit]
                                .chars()
                                .next()
                                .is_some_and(super::unicode::letter)
                            {
                                return Err(Error::syntax(
                                    self.work,
                                    start,
                                    "invalid integer literal",
                                ));
                            }
                            let text = &source[digits..i];
                            if text.is_empty()
                                || text.starts_with('_')
                                || text.ends_with('_')
                                || text.contains("__")
                            {
                                return Err(Error::syntax(
                                    self.work,
                                    start,
                                    "invalid integer literal",
                                ));
                            }
                            integer(numeric(self.work, text)?, radix, start, 2, self.work)?
                        } else {
                            while i < self.limit && (s[i].is_ascii_digit() || s[i] == b'_') {
                                self.work.charge(1)?;
                                i += 1;
                            }
                            let mut float = false;
                            if s.get(i) == Some(&b'.')
                                && s.get(i + 1).is_some_and(u8::is_ascii_digit)
                            {
                                float = true;
                                i += 1;
                                while i < self.limit && (s[i].is_ascii_digit() || s[i] == b'_') {
                                    self.work.charge(1)?;
                                    i += 1;
                                }
                            }
                            // An e/E only opens an exponent before a sign or digit; otherwise
                            // it starts a trailing name such as the `end` in `5end`.
                            if s.get(i).is_some_and(|b| matches!(b, b'e' | b'E'))
                                && s.get(i + 1)
                                    .is_some_and(|b| b.is_ascii_digit() || matches!(b, b'+' | b'-'))
                            {
                                float = true;
                                i += 1;
                                if s.get(i).is_some_and(|b| matches!(b, b'+' | b'-')) {
                                    i += 1;
                                }
                                while i < self.limit && (s[i].is_ascii_digit() || s[i] == b'_') {
                                    self.work.charge(1)?;
                                    i += 1;
                                }
                            }
                            if let Some(c) = source[i..self.limit]
                                .chars()
                                .next()
                                .filter(|&c| super::unicode::letter(c) || c == '_')
                            {
                                // A name may abut a number only when the whole name is a
                                // keyword (`5if cond`, `1e3end`); `5ifx` and `123abc` are
                                // malformed literals rather than a number and an identifier.
                                let mut end = i + c.len_utf8();
                                while let Some(c) = source[end..self.limit].chars().next() {
                                    self.work.charge(1)?;
                                    if !matches!(c, '_' | '?' | '!')
                                        && !super::unicode::letter_or_digit(c)
                                    {
                                        break;
                                    }
                                    end += c.len_utf8();
                                }
                                if !super::keyword(&source[i..end]) {
                                    return Err(Error::syntax(
                                        self.work,
                                        start,
                                        "invalid numeric literal",
                                    ));
                                }
                            }
                            let raw = &source[start..i];
                            for (j, b) in raw.bytes().enumerate() {
                                self.work.charge(1)?;
                                if b == b'_'
                                    && (j == 0
                                        || j + 1 == raw.len()
                                        || !raw.as_bytes()[j - 1].is_ascii_digit()
                                        || !raw.as_bytes()[j + 1].is_ascii_digit())
                                {
                                    return Err(Error::syntax(
                                        self.work,
                                        start,
                                        "invalid numeric separator",
                                    ));
                                }
                            }
                            let text = numeric(self.work, raw)?;
                            if float {
                                Token::Float(std::str::from_utf8(&text).unwrap().parse().map_err(
                                    |_| Error::syntax(self.work, start, "invalid float"),
                                )?)
                            } else {
                                integer(text, 10, start, 0, self.work)?
                            }
                        }
                    }
                    _ => {
                        let mut found = None;
                        for op in [
                            "=>", "...", "..", "===", "<=>", "||=", "&&=", "**=", "==", "!=", "<=",
                            ">=", "&&", "||", "+=", "-=", "*=", "/=", "%=", "**", "<<", "::", "->",
                            "=~", "!~", "&.",
                        ] {
                            // An operator symbol ends before its member or range separator.
                            if op == "&."
                                && out.last().or(previous).is_some_and(|last| {
                                    last.token == Token::P(':') && last.end == i
                                })
                            {
                                continue;
                            }
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
                                        "unsupported character",
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
                    Token::Invalid(Boxed::new(
                        self.work,
                        (
                            error.offset.unwrap_or(start),
                            Text::new(self.work, &error.message)?,
                        ),
                    )?)
                }
                Err(error) => return Err(error),
            };
            self.work.bytes(i - start)?;
            let start_line = line;
            line += s[start..i].iter().filter(|&&b| b == b'\n').count();
            match token {
                Token::P('{') => braces += 1,
                Token::P('}') => braces = braces.saturating_sub(1),
                _ => (),
            }
            out.push(
                self.work,
                Lexeme {
                    token,
                    offset: start,
                    end: i,
                    line: start_line,
                    end_line: line,
                },
            )?;
            self.pos = i;
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
                b'\\' if quote == b'"' => self.escape(&mut text, true)?,
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
                    parts.push(self.work, Part::Expr(self.interpolation(line)?))?;
                }
                0 => return Err(Error::syntax(self.work, self.pos, "unterminated string")),
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
        Err(Error::syntax(self.work, start, "unterminated string"))
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
                    while bytes.get(self.pos).is_some_and(u8::is_ascii_alphabetic) {
                        self.work.charge(1)?;
                        let bit = match bytes[self.pos] {
                            b'i' => 1,
                            b'm' => 2,
                            _ => {
                                return Err(Error::syntax(
                                    self.work,
                                    start,
                                    "unsupported regex flag",
                                ));
                            }
                        };
                        if flags & bit != 0 {
                            return Err(Error::syntax(self.work, start, "repeated regex flag"));
                        }
                        flags |= bit;
                        self.pos += 1;
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

    fn interpolation(&mut self, line: usize) -> Result<Buffer<Lexeme<'a>>> {
        if self.depth >= 8 {
            return Err(Error::syntax(
                self.work,
                self.pos,
                "string interpolation nesting exceeds 8",
            ));
        }
        self.pos += 2;
        self.depth += 1;
        let result = self.tokens(self.limit, line, true, false, None);
        self.depth -= 1;
        result
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
                    self.escape(&mut text, false)?;
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
                parts.push(self.work, Part::Expr(self.interpolation(line)?))?;
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
            self.pos,
            "unterminated percent array literal",
        ))
    }

    fn escape(&mut self, out: &mut Buffer<u8>, strict: bool) -> Result<()> {
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
                let valid = if c == 'x' {
                    digits != 0
                } else {
                    digits == 4 && char::from_u32(value).is_some()
                };
                if !valid {
                    if strict {
                        return Err(Error::syntax(
                            self.work,
                            start,
                            "invalid hexadecimal string escape",
                        ));
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
        Token::Word(w) => !super::reserved(w) || matches!(w.as_str(), "self" | "end"),
        Token::Int(_)
        | Token::BigInt(..)
        | Token::Float(_)
        | Token::Bytes(_)
        | Token::Regex(..)
        | Token::Template(_)
        | Token::Words(..)
        | Token::P(')' | ']' | '}') => true,
        _ => false,
    }
}

fn numeric(work: &dyn Work, text: &str) -> Result<Buffer<u8>> {
    let mut result = Buffer::with_capacity(work, text.len())?;
    for byte in text.bytes() {
        work.charge(1)?;
        if byte != b'_' {
            result.push(work, byte)?;
        }
    }
    Ok(result)
}

fn integer(
    text: Buffer<u8>,
    radix: u32,
    offset: usize,
    prefix: usize,
    work: &dyn Work,
) -> Result<Token<'static>> {
    for &byte in &*text {
        work.charge(1)?;
        if !(byte as char).is_digit(radix) {
            return Err(Error::syntax(work, offset, "invalid integer literal"));
        }
    }
    let parsed = u64::from_str_radix(std::str::from_utf8(&text).unwrap(), radix);
    if text.len() + prefix > 100_000 && !parsed.as_ref().is_ok_and(|&n| n <= i64::MAX as u64) {
        return Err(Error::syntax(
            work,
            offset,
            "integer literal exceeds 100000 digits",
        ));
    }
    Ok(match parsed {
        Ok(n) => Token::Int(n),
        Err(_) => Token::BigInt(Text::from_bytes(Bytes::new(work, text)?).unwrap(), radix),
    })
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
        assert_eq!(original.1.as_str(), "unsupported character");
        assert_eq!(original.1.as_ptr(), duplicate.1.as_ptr());
        drop(tokens);
        assert!(context.stats().retained_memory_bytes > 0);
        let Token::Invalid(duplicate) = &copy else {
            unreachable!()
        };
        assert_eq!(duplicate.1.as_str(), "unsupported character");
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
        for (source, message) in [
            ("5ifx", "invalid numeric literal"),
            ("5if_foo", "invalid numeric literal"),
            ("5if?", "invalid numeric literal"),
            ("5end!", "invalid numeric literal"),
            ("5ifé", "invalid numeric literal"),
            ("5end名", "invalid numeric literal"),
            ("5elf", "invalid numeric literal"),
            ("5_if", "invalid numeric separator"),
            ("123abc", "invalid numeric literal"),
            ("1.5x", "invalid numeric literal"),
            ("1e3foo", "invalid numeric literal"),
            ("1e", "invalid numeric literal"),
            ("1e_3", "invalid numeric literal"),
            ("1e+", "invalid float"),
            ("1e+end", "invalid float"),
            ("1e3_", "invalid numeric separator"),
            ("1e3__4", "invalid numeric separator"),
            ("0x5if", "invalid integer literal"),
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
            assert_eq!(invalid.0, 0, "{source}");
            assert_eq!(invalid.1.as_str(), message, "{source}");
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
