use super::*;

impl<'a> Parser<'a> {
    pub(super) fn token(&self) -> &Token<'a> {
        &self.tokens[self.pos].token
    }
    /// Adds a tooling fact when this parse is recording.
    pub(super) fn note(&mut self, add: impl FnOnce(&mut record::Record)) {
        if let Some(record) = self.record.as_deref_mut() {
            add(record);
        }
    }
    pub(super) fn err<T>(&self, message: impl std::fmt::Display) -> Result<T> {
        self.work.charge(1)?;
        Err(Error::syntax(self.work, self.position(self.pos), message))
    }
    /// The source offset at which Go reports the token at `index`. Its lexer
    /// stamps a multi-character operator at the operator's last character,
    /// except `<=>` and `===`.
    pub(super) fn position(&self, index: usize) -> usize {
        let lexeme = &self.tokens[index];
        match lexeme.token {
            Token::Op(op) if !matches!(op, "<=>" | "===") => lexeme.offset + op.len() - 1,
            _ => lexeme.offset,
        }
    }
    /// The index of the token Go's parser sees at `index`, which has no
    /// token for a line break.
    pub(super) fn significant(&self, mut index: usize) -> usize {
        while index + 1 < self.tokens.len()
            && self.tokens[index].token == Token::EndLine
            && self.source.as_bytes().get(self.tokens[index].offset) != Some(&b';')
        {
            index += 1;
        }
        index
    }
    /// Go's diagnostic name for the token at `index`.
    fn label(&self, index: usize) -> Label<'a> {
        let lexeme = &self.tokens[index];
        match &lexeme.token {
            Token::Word(word) if word.starts_with("@@") => Label::Text("class variable"),
            Token::Word(word) if word.starts_with('@') => Label::Text("instance variable"),
            Token::Word(word) => Label::word(word.as_str()),
            Token::Int(_) | Token::BigInt(..) => Label::Text("integer"),
            Token::Float(_) => Label::Text("float"),
            Token::Bytes(_) | Token::Template(_) => Label::Text("string"),
            Token::Regex(..) => Label::Quoted("regex"),
            Token::Words(words) => {
                let interpolated = words
                    .entries
                    .iter()
                    .flat_map(|entry| entry.iter())
                    .any(|part| matches!(part, Part::Expr(..)));
                Label::Text(match (interpolated, words.symbol) {
                    (false, false) => "percent word array",
                    (false, true) => "percent symbol array",
                    (true, false) => "percent interpolated word array",
                    (true, true) => "percent interpolated symbol array",
                })
            }
            Token::Symbol(_) | Token::QuotedSymbol(_) => Label::Text("symbol"),
            Token::Invalid(invalid) => match invalid.failure {
                Failure::Literal(label) => Label::Text(label),
                Failure::Diagnostic | Failure::Character => Label::Text("invalid token"),
            },
            Token::P(c) => Label::Char(*c),
            Token::Op(op) => Label::Quoted(op),
            Token::EndLine if self.source.as_bytes().get(lexeme.offset) == Some(&b';') => {
                Label::Char(';')
            }
            Token::EndLine | Token::Eof => Label::Text("end of input"),
        }
    }
    /// A lexer diagnostic that Go reports in place of any expectation at `index`.
    pub(super) fn diagnostic(&self, index: usize) -> Option<Error> {
        let token = &self.tokens[index];
        if token.token == Token::P('?')
            && !suffixes::lenient()
            && self.source.as_bytes().get(token.end) == Some(&b'=')
            && index.checked_sub(1).is_some_and(|previous| {
                let name = &self.tokens[previous];
                name.end == token.offset
                    && matches!(&name.token, Token::Word(word) if !keyword(word)
                    || previous.checked_sub(1).is_some_and(|separator| {
                        matches!(self.tokens[separator].token, Token::P('.') | Token::Op("&."))
                    }))
            })
        {
            let mut error = self.name_suffix_error(token.offset);
            // Removing the `?` leaves an assignment, which only a statement
            // can be: in `f(ok?=(x))` it would be a new syntax error.
            if !self.assignment_starts(index - 1) {
                let mut diagnostics = error.take_diagnostics();
                for diagnostic in &mut diagnostics {
                    diagnostic.fixes.clear();
                }
                error = error.with_diagnostics(diagnostics);
            }
            return Some(error);
        }
        match &self.tokens[index].token {
            Token::Invalid(invalid) if invalid.failure == Failure::Diagnostic => Some(
                Error::syntax(self.work, invalid.offset, invalid.message.as_str()),
            ),
            _ => None,
        }
    }
    /// Reports Go's failed expectation of `expected` at the current token.
    pub(super) fn expected<T>(&self, expected: Label<'_>) -> Result<T> {
        self.work.charge(1)?;
        let index = self.significant(self.pos);
        if let Some(error) = self.diagnostic(index) {
            return Err(error);
        }
        Err(Error::syntax(
            self.work,
            self.position(index),
            format_args!("expected {expected}, got {}", self.label(index)),
        ))
    }
    /// Reports Go's refusal of a token that cannot start an expression.
    pub(super) fn unexpected<T>(&self, index: usize) -> Result<T> {
        self.work.charge(1)?;
        let index = self.significant(index);
        if let Some(error) = self.diagnostic(index) {
            return Err(error);
        }
        Err(Error::syntax(
            self.work,
            self.position(index),
            format_args!("unexpected token {}", self.label(index)),
        ))
    }
    pub(super) fn bump(&mut self) -> Result<Token<'a>> {
        self.work
            .bytes(self.tokens[self.pos].end - self.tokens[self.pos].offset)?;
        let t = self.token().copy(self.work)?;
        if !matches!(t, Token::Eof) {
            self.pos += 1;
        }
        Ok(t)
    }
    pub(super) fn word(&mut self, w: &str) -> bool {
        if matches!(self.token(),Token::Word(s) if s==w) {
            self.pos += 1;
            true
        } else {
            false
        }
    }
    pub(super) fn expect_word(&mut self, w: &str) -> Result<()> {
        self.work.charge(1)?;
        if self.word(w) {
            Ok(())
        } else if w == "end" {
            // Go checks a block's closing `end` by name rather than by token.
            self.expected(Label::Text("end"))
        } else {
            self.expected(Label::word(w))
        }
    }
    pub(super) fn take_p(&mut self, c: char) -> bool {
        if self.token() == &Token::P(c) {
            self.pos += 1;
            true
        } else {
            false
        }
    }
    pub(super) fn expect_p(&mut self, c: char) -> Result<()> {
        self.work.charge(1)?;
        if self.take_p(c) {
            Ok(())
        } else {
            self.expected(Label::Char(c))
        }
    }
    /// Returns the end offset of the last token a declaration beginning at
    /// token `first` consumed, excluding line breaks and separators it ended on.
    pub(super) fn declaration_end(&self, first: usize) -> usize {
        let mut last = self.pos.max(first + 1);
        while last > first + 1 && matches!(self.tokens[last - 1].token, Token::EndLine) {
            last -= 1;
        }
        self.tokens[last - 1].end
    }
    pub(super) fn lines(&mut self) -> Result<()> {
        while matches!(self.token(), Token::EndLine) {
            self.work.charge(1)?;
            self.pos += 1;
        }
        Ok(())
    }
    pub(super) fn expression_separator(&self) -> Result<()> {
        let previous = &self.tokens[self.pos - 1];
        let next = &self.tokens[self.pos];
        if previous.token != Token::EndLine
            && previous.end_line == next.line
            && self.prefix(self.pos)
            && !matches!(&next.token, Token::Word(word) if matches!(word.as_str(), "unless" | "until" | "do"))
        {
            let message = ADJACENT_EXPRESSIONS;
            let diagnostic = crate::diagnostic::Diagnostic::error(
                crate::diagnostic::Code::SYNTAX,
                crate::diagnostic::Span::new(previous.end, next.offset),
                message,
            );
            return Err(Error::syntax(self.work, previous.end, message).with_diagnostic(diagnostic));
        }
        Ok(())
    }
    pub(super) fn line_breaks(&mut self) -> Result<()> {
        while self.token() == &Token::EndLine
            && self.tokens[self.pos].line != self.tokens[self.pos].end_line
        {
            self.work.charge(1)?;
            self.pos += 1;
        }
        Ok(())
    }
    /// Go's test that a token has a prefix parser: whether the token at
    /// `index` can start an expression.
    pub(super) fn prefix(&self, index: usize) -> bool {
        match &self.tokens[index].token {
            Token::Word(w) if w.starts_with('@') => true,
            Token::Word(w) if keyword(w) => matches!(
                w.as_str(),
                "then"
                    | "true"
                    | "false"
                    | "nil"
                    | "self"
                    | "yield"
                    | "if"
                    | "unless"
                    | "case"
                    | "begin"
                    | "for"
                    | "while"
                    | "until"
            ),
            Token::Word(_)
            | Token::Int(_)
            | Token::BigInt(..)
            | Token::Float(_)
            | Token::Bytes(_)
            | Token::Template(_)
            | Token::Words(_)
            | Token::Regex(..)
            | Token::Symbol(_)
            | Token::QuotedSymbol(_)
            | Token::P('(' | '[' | '{')
            | Token::Op("!" | "-" | "+" | "->") => true,
            Token::Invalid(invalid) => matches!(invalid.failure, Failure::Literal(_)),
            _ => false,
        }
    }
    pub(super) fn enter(&mut self) -> Result<()> {
        self.work.charge(1)?;
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            self.err(TOO_DEEP)
        } else {
            Ok(())
        }
    }
    /// Refuses a syntax tree deeper than Go's limit, at the offending node as Go does.
    pub(super) fn check_depth(&self, depth: u32, offset: u32) -> Result<()> {
        if depth as usize > MAX_DEPTH {
            self.work.charge(1)?;
            Err(Error::syntax(self.work, offset as usize, TOO_DEEP))
        } else {
            Ok(())
        }
    }
    /// Consumes a comma that follows, skipping line breaks as Go's lookahead does.
    pub(super) fn comma_follows(&mut self) -> bool {
        let comma = self.significant(self.pos);
        if self.tokens[comma].token != Token::P(',') {
            return false;
        }
        self.pos = comma + 1;
        true
    }
    pub(super) fn make(&self, node: Node, depth: u32) -> Result<Expr> {
        let offset = self.tokens[self.pos.saturating_sub(1)].offset as u32;
        self.make_at(node, depth, offset)
    }
    pub(super) fn make_at(&self, node: Node, depth: u32, offset: u32) -> Result<Expr> {
        self.work.charge(1)?;
        self.check_depth(depth, offset)?;
        Ok(Expr {
            node,
            depth,
            offset,
        })
    }
    /// The index of the last token before the current one, skipping line breaks.
    pub(super) fn previous_index(&self) -> Result<usize> {
        let mut index = self.pos;
        while index > 0 {
            self.work.charge(1)?;
            index -= 1;
            if self.tokens[index].token != Token::EndLine {
                return Ok(index);
            }
        }
        Ok(0)
    }
    pub(super) fn previous(&self) -> Result<&Lexeme<'a>> {
        Ok(self
            .tokens
            .find((0..self.pos).rev(), self.work, |t| {
                t.token != Token::EndLine
            })?
            .unwrap())
    }
}

/// Go's diagnostic name for a token in a parse error.
#[derive(Clone, Copy)]
pub(super) enum Label<'a> {
    Text(&'a str),
    Quoted(&'a str),
    Keyword(&'a str),
    Char(char),
}

impl<'a> Label<'a> {
    /// Names a word token: Go quotes its keywords, and spells the statement
    /// keywords its lexer lists by name in double quotes.
    pub(super) fn word(w: &'a str) -> Self {
        match w {
            "def" | "class" | "enum" | "export" | "self" | "private" | "property" | "getter"
            | "setter" | "end" | "raise" | "return" | "yield" | "do" | "then" | "for" | "in"
            | "if" | "unless" | "elsif" | "else" | "true" | "false" | "nil" => Self::Keyword(w),
            _ if keyword(w) => Self::Quoted(w),
            _ => Self::Text("identifier"),
        }
    }
}

impl std::fmt::Display for Label<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Text(text) => f.write_str(text),
            Self::Quoted(text) => write!(f, "\"{text}\""),
            Self::Keyword(text) => write!(f, "'{text}'"),
            Self::Char(c) => write!(f, "\"{c}\""),
        }
    }
}

/// Go's bound on source text quoted in a diagnostic: at most 64 bytes, cut at
/// a character boundary and marked.
pub(super) struct SourceText<'a>(&'a str);

pub(super) fn source_text(text: &str) -> SourceText<'_> {
    SourceText(text)
}

impl std::fmt::Display for SourceText<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.0.len() <= 64 {
            return f.write_str(self.0);
        }
        let mut end = 64;
        while !self.0.is_char_boundary(end) {
            end -= 1;
        }
        write!(f, "{}...", &self.0[..end])
    }
}
