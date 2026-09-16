use crate::{Error, Result};

#[derive(Clone, Debug, PartialEq)]
pub(super) enum Token {
    Regex(Vec<u8>, u8),
    Word(String),
    Int(u64),
    BigInt(String, u32),
    Float(f64),
    Bytes(Vec<u8>),
    Template(Vec<Part>),
    Words(Box<Words>),
    Invalid(Box<(usize, String)>),
    P(char),
    Op(&'static str),
    EndLine,
    Eof,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) enum Part {
    Text(Vec<u8>),
    Expr(Vec<Lexeme>),
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Words {
    pub entries: Vec<Vec<Part>>,
    pub symbol: bool,
    pub ambiguous: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Lexeme {
    pub token: Token,
    pub offset: usize,
    pub end: usize,
    pub line: usize,
    pub end_line: usize,
}

struct Lexer<'a> {
    work: &'a dyn crate::compilation::Work,
    source: &'a str,
    pos: usize,
    limit: usize,
    depth: usize,
    speculative: usize,
}

pub(super) fn lex(source: &str, work: &dyn crate::compilation::Work) -> Result<Vec<Lexeme>> {
    if source.len() > super::MAX_SOURCE {
        return Err(Error::syntax(0, "source exceeds 8 MiB"));
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

pub(super) fn modulo(
    source: &str,
    start: &Lexeme,
    limit: usize,
    depth: usize,
    work: &dyn crate::compilation::Work,
) -> Result<Vec<Lexeme>> {
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

pub(super) fn resume(
    source: &str,
    start: &Lexeme,
    until: usize,
    limit: usize,
    depth: usize,
    previous: Option<&Lexeme>,
    work: &dyn crate::compilation::Work,
) -> Result<Vec<Lexeme>> {
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

pub(super) fn regex(
    source: &str,
    start: &Lexeme,
    limit: usize,
    depth: usize,
    work: &dyn crate::compilation::Work,
) -> Result<Vec<Lexeme>> {
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

impl Lexer<'_> {
    fn tokens(
        &mut self,
        until: usize,
        mut line: usize,
        interpolation: bool,
        skip_first_percent: bool,
        previous: Option<&Lexeme>,
    ) -> Result<Vec<Lexeme>> {
        let source = self.source;
        let s = &source.as_bytes()[..self.limit];
        let mut out = Vec::<Lexeme>::new();
        let mut braces = 0usize;
        let first = self.pos;
        while self.pos < until {
            self.work.charge(1)?;
            let start = self.pos;
            let mut i = start;
            if interpolation && s[i] == b'}' && braces == 0 {
                out.push(Lexeme {
                    token: Token::Eof,
                    offset: i,
                    end: i,
                    line,
                    end_line: line,
                });
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
            let scanned = (|| -> Result<Token> {
                let initial = source[i..self.limit].chars().next().unwrap();
                if initial == '@' {
                    i += 1;
                    if s.get(i) == Some(&b'@') {
                        i += 1;
                    }
                    let Some(first) = source[i..self.limit].chars().next() else {
                        return Err(Error::syntax(start, "expected variable name after @"));
                    };
                    if first != '_' && !super::unicode::letter(first) {
                        return Err(Error::syntax(start, "expected variable name after @"));
                    }
                    i += first.len_utf8();
                    while let Some(c) = source[i..self.limit].chars().next() {
                        self.work.charge(1)?;
                        if c != '_' && !super::unicode::letter_or_digit(c) {
                            break;
                        }
                        i += c.len_utf8();
                    }
                    return Ok(Token::Word(source[start..i].to_owned()));
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
                    return Ok(Token::Word(source[start..i].to_owned()));
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
                                return Err(Error::syntax(start, "invalid integer literal"));
                            }
                            let text = &source[digits..i];
                            if text.is_empty()
                                || text.starts_with('_')
                                || text.ends_with('_')
                                || text.contains("__")
                            {
                                return Err(Error::syntax(start, "invalid integer literal"));
                            }
                            integer(text.replace('_', ""), radix, start, 2)?
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
                            if s.get(i).is_some_and(|b| matches!(b, b'e' | b'E')) {
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
                            if source[i..self.limit]
                                .chars()
                                .next()
                                .is_some_and(|c| super::unicode::letter(c) || c == '_')
                            {
                                return Err(Error::syntax(start, "invalid numeric literal"));
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
                                    return Err(Error::syntax(start, "invalid numeric separator"));
                                }
                            }
                            let text = raw.replace('_', "");
                            if float {
                                Token::Float(
                                    text.parse()
                                        .map_err(|_| Error::syntax(start, "invalid float"))?,
                                )
                            } else {
                                integer(text, 10, start, 0)?
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
                                _ => return Err(Error::syntax(start, "unsupported character")),
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
                    Token::Invalid(Box::new((error.offset.unwrap_or(start), error.message)))
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
            out.push(Lexeme {
                token,
                offset: start,
                end: i,
                line: start_line,
                end_line: line,
            });
            self.pos = i;
        }
        if interpolation {
            return Err(Error::syntax(self.pos, "unterminated string interpolation"));
        }
        out.push(Lexeme {
            token: Token::Eof,
            offset: self.pos,
            end: self.pos,
            line,
            end_line: line,
        });
        Ok(out)
    }

    fn quoted(&mut self, quote: u8, mut line: usize) -> Result<Vec<Part>> {
        let start = self.pos;
        self.pos += 1;
        let mut parts = Vec::new();
        let mut text = Vec::new();
        while self.pos < self.limit {
            self.work.charge(1)?;
            let before = self.pos;
            match self.source.as_bytes()[self.pos] {
                b if b == quote => {
                    self.pos += 1;
                    flush(&mut parts, &mut text);
                    return Ok(parts);
                }
                b'\\' if quote == b'"' => self.escape(&mut text, true)?,
                b'\\' => {
                    self.pos += 1;
                    if self.pos < self.limit
                        && matches!(self.source.as_bytes()[self.pos], b'\'' | b'\\')
                    {
                        text.push(self.source.as_bytes()[self.pos]);
                        self.pos += 1;
                    } else {
                        text.push(b'\\');
                    }
                }
                b'#' if quote == b'"'
                    && self.source.as_bytes().get(self.pos + 1) == Some(&b'{') =>
                {
                    flush(&mut parts, &mut text);
                    parts.push(Part::Expr(self.interpolation(line)?));
                }
                0 => return Err(Error::syntax(self.pos, "unterminated string")),
                byte => {
                    text.push(byte);
                    self.pos += 1;
                }
            }
            line += self.source.as_bytes()[before..self.pos]
                .iter()
                .filter(|&&b| b == b'\n')
                .count();
        }
        Err(Error::syntax(start, "unterminated string"))
    }

    fn regex(&mut self) -> Result<Token> {
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
                    let pattern = bytes[body..self.pos].to_vec();
                    self.pos += 1;
                    let mut flags = 0;
                    while bytes.get(self.pos).is_some_and(u8::is_ascii_alphabetic) {
                        self.work.charge(1)?;
                        let bit = match bytes[self.pos] {
                            b'i' => 1,
                            b'm' => 2,
                            _ => return Err(Error::syntax(start, "unsupported regex flag")),
                        };
                        if flags & bit != 0 {
                            return Err(Error::syntax(start, "repeated regex flag"));
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
        Err(Error::syntax(start, "unterminated regex literal"))
    }

    fn interpolation(&mut self, line: usize) -> Result<Vec<Lexeme>> {
        if self.depth >= 8 {
            return Err(Error::syntax(
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

    fn words(&mut self, mut line: usize, ambiguous: bool) -> Result<Token> {
        let (kind, open, close) = self.percent_kind().unwrap();
        self.pos += 2 + open.len_utf8();
        let mut nesting = 1;
        let mut words = Vec::new();
        let mut parts = Vec::new();
        let mut text = Vec::new();
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
                            text.push(b'\\');
                        }
                        text.extend_from_slice(c.encode_utf8(&mut [0; 4]).as_bytes());
                        self.pos += c.len_utf8();
                    } else {
                        text.push(b'\\');
                    }
                }
                in_word = true;
            } else if interpolating
                && c == '#'
                && close != '#'
                && self.source.as_bytes().get(self.pos + 1) == Some(&b'{')
            {
                flush(&mut parts, &mut text);
                parts.push(Part::Expr(self.interpolation(line)?));
                in_word = true;
            } else {
                self.pos += c.len_utf8();
                if c == close {
                    nesting -= 1;
                    if nesting == 0 {
                        if in_word {
                            flush(&mut parts, &mut text);
                            words.push(parts);
                        }
                        return Ok(Token::Words(Box::new(Words {
                            entries: words,
                            symbol: matches!(kind, b'i' | b'I'),
                            ambiguous,
                        })));
                    }
                } else if open != close && c == open {
                    nesting += 1;
                }
                if c.is_whitespace() {
                    if in_word {
                        flush(&mut parts, &mut text);
                        words.push(std::mem::take(&mut parts));
                        in_word = false;
                    }
                } else {
                    text.extend_from_slice(c.encode_utf8(&mut [0; 4]).as_bytes());
                    in_word = true;
                }
            }
            line += self.source.as_bytes()[before..self.pos]
                .iter()
                .filter(|&&b| b == b'\n')
                .count();
        }
        Err(Error::syntax(
            self.pos,
            "unterminated percent array literal",
        ))
    }

    fn escape(&mut self, out: &mut Vec<u8>, strict: bool) -> Result<()> {
        self.pos += 1;
        let Some(c) = self.source[self.pos..self.limit].chars().next() else {
            out.push(b'\\');
            return Ok(());
        };
        self.pos += c.len_utf8();
        match c {
            'a' => out.push(7),
            'b' => out.push(8),
            'e' => out.push(27),
            'f' => out.push(12),
            'n' => out.push(b'\n'),
            'r' => out.push(b'\r'),
            't' => out.push(b'\t'),
            'v' => out.push(11),
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
                        return Err(Error::syntax(start, "invalid hexadecimal string escape"));
                    }
                    self.pos = start;
                    out.push(c as u8);
                } else if c == 'x' {
                    out.push(value as u8);
                } else {
                    out.extend_from_slice(
                        char::from_u32(value)
                            .unwrap()
                            .encode_utf8(&mut [0; 4])
                            .as_bytes(),
                    );
                }
            }
            _ => out.extend_from_slice(c.encode_utf8(&mut [0; 4]).as_bytes()),
        }
        Ok(())
    }
}

fn flush(parts: &mut Vec<Part>, text: &mut Vec<u8>) {
    if !text.is_empty() {
        parts.push(Part::Text(std::mem::take(text)));
    }
}

pub(super) fn plain(parts: Vec<Part>, work: &dyn crate::compilation::Work) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    for part in parts {
        let Part::Text(text) = part else {
            unreachable!()
        };
        work.bytes(text.len())?;
        bytes.extend(text);
    }
    Ok(bytes)
}

fn ends_expression(token: &Token) -> bool {
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

fn integer(text: String, radix: u32, offset: usize, prefix: usize) -> Result<Token> {
    if !text.bytes().all(|c| (c as char).is_digit(radix)) {
        return Err(Error::syntax(offset, "invalid integer literal"));
    }
    let parsed = u64::from_str_radix(&text, radix);
    if text.len() + prefix > 100_000 && !parsed.as_ref().is_ok_and(|&n| n <= i64::MAX as u64) {
        return Err(Error::syntax(
            offset,
            "integer literal exceeds 100000 digits",
        ));
    }
    Ok(match parsed {
        Ok(n) => Token::Int(n),
        Err(_) => Token::BigInt(text, radix),
    })
}
