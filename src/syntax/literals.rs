use super::*;

// The first operand of an expression, parsed without a task when it is a
// single token.
pub(super) enum Leaf {
    Done(Expr),
    Tail(Expr, Suffix),
    Nested,
}

impl<M: super::recovery::Mode> Parsing<'_, M> {
    pub(super) async fn hash_group(&self) -> Result<Expr> {
        let work = self.p().work;
        work.charge(1)?;
        let mut entries = Buffer::new();
        let closed = {
            let mut p = self.p();
            p.groups += 1;
            p.line_breaks()?;
            p.take_p('}')
        };
        if !closed {
            loop {
                let (key, shorthand) = self.p().hash_label()?;
                let value = match shorthand {
                    Some(value) => value,
                    None => self.expr(0).await?,
                };
                entries.push(work, (key, value))?;
                let mut p = self.p();
                p.line_breaks()?;
                if p.take_p('}') {
                    break;
                }
                match p.token() {
                    Token::P(',') => (),
                    Token::Eof => return p.expected(Label::Char('}')),
                    _ => return p.err(INVALID_HASH_PAIR),
                }
                p.expect_p(',')?;
                p.line_breaks()?;
                if p.take_p('}') {
                    break;
                }
            }
        }
        let d = 1 + entries.iter().map(|(_, e)| e.depth).max().unwrap_or(0);
        let mut p = self.p();
        p.groups -= 1;
        p.make(Node::Hash(entries), d)
    }

    pub(super) async fn interpolated(&self) -> Result<Expr> {
        let expr = self.line_expr(0).await?;
        self.p().expression_separator()?;
        Ok(expr)
    }
}

impl<'a> Parser<'a> {
    // Most expressions start with a single-token operand. Parse it and look
    // for a suffix here, so that only nested syntax costs a task.
    pub(super) fn leaf_expression(&mut self, min: u8) -> Result<Leaf> {
        let leaf = match self.token() {
            Token::Int(_)
            | Token::BigInt(..)
            | Token::Float(_)
            | Token::Regex(..)
            | Token::Bytes(_)
            | Token::Template(_)
            | Token::Words(..)
            | Token::Symbol(_)
            | Token::QuotedSymbol(_) => true,
            Token::Word(w) => {
                !keyword(w) || matches!(w.as_str(), "nil" | "true" | "false" | "self" | "then")
            }
            _ => false,
        };
        if !leaf {
            return Ok(Leaf::Nested);
        }
        self.work.charge(1)?;
        self.enter()?;
        self.work.charge(1)?;
        let offset = self.tokens[self.pos].offset as u32;
        let line = self.tokens[self.pos].line;
        let mut lhs = match self.bump()? {
            Token::Word(w) => match w.as_str() {
                "nil" => self.make(Node::Literal(Value::nil()), 1)?,
                "true" => self.make(Node::Literal(Value::boolean(true)), 1)?,
                "false" => self.make(Node::Literal(Value::boolean(false)), 1)?,
                name => self.variable_name(name)?,
            },
            token => self.leaf(token)?,
        };
        lhs.offset = offset;
        Ok(match self.expression_suffix(&lhs, min, Some(line))? {
            Some(suffix) => Leaf::Tail(lhs, suffix),
            None => {
                self.depth -= 1;
                Leaf::Done(lhs)
            }
        })
    }

    // Build expressions that need no nested parsing from their first token.
    pub(super) fn leaf(&mut self, token: Token<'a>) -> Result<Expr> {
        match token {
            Token::Int(n) => self.make(Node::Integer(n), 1),
            Token::BigInt(text, radix) => self.make(Node::BigInteger(text, radix), 1),
            Token::Float(n) => self.make(Node::Literal(Value::float(n)), 1),
            Token::Regex(pattern, flags) => self.make(Node::Regex(pattern, flags), 1),
            Token::Bytes(b) => self.make(Node::Literal(b.into_value(false)), 1),
            Token::Template(parts) => {
                let offset = self.tokens[self.pos - 1].offset;
                self.template(parts, false, offset)
            }
            Token::Words(words) => {
                let offset = self.tokens[self.pos - 1].offset;
                // Go lexes a `%` after an operand as modulo and reads it again
                // as a percent literal only for a command argument.
                if words.ambiguous && self.pos - 1 != self.percent_argument {
                    return Err(Error::syntax(self.work, offset, "unexpected token \"%\""));
                }
                self.words(words.into_inner(), offset)
            }
            Token::Symbol(name) => {
                let name = Bytes::from_slice(self.work, name.as_bytes())?;
                self.make(Node::Literal(name.into_value(true)), 1)
            }
            Token::QuotedSymbol(name) => self.make(Node::Literal(name.into_value(true)), 1),
            Token::Invalid(invalid) if invalid.failure == Failure::Character => {
                self.unexpected(self.pos - 1)
            }
            Token::Invalid(invalid) => Err(Error::syntax(
                self.work,
                invalid.offset,
                invalid.message.as_str(),
            )),
            Token::Op("->") => Err(Error::syntax(
                self.work,
                self.position(self.pos - 1),
                "lambda literals are not supported; executable code is not a value. Define a \
                 named function and call it, or attach a block to the call that runs it, as in \
                 `people.map { |person| person.name }`",
            )),
            Token::Eof => self.unexpected(self.pos),
            _ => self.unexpected(self.pos - 1),
        }
    }

    /// Reads the name just consumed. Like Go, a variable sigil at the end of
    /// input names nothing.
    pub(super) fn variable_name(&self, name: &str) -> Result<Expr> {
        if name.starts_with('@') {
            self.binding_name(name, self.tokens[self.pos - 1].offset)?;
        } else {
            self.read_spelling(name, self.tokens[self.pos - 1].offset)?;
            self.suffix_read(name)?;
        }
        if matches!(name, "@" | "@@") {
            let (expected, got) = if name == "@" {
                ("instance variable name", "instance variable")
            } else {
                ("class variable name", "class variable")
            };
            return Err(Error::syntax(
                self.work,
                self.tokens[self.pos - 1].offset,
                format_args!("expected {expected}, got {got}"),
            ));
        }
        self.block_reference(name, self.tokens[self.pos - 1].offset)?;
        self.make(Node::Var(Name::new(self.work, name)?), 1)
    }

    /// Reports whether the minus at `sign` folds into the adjacent number.
    pub(super) fn negative_literal(&self, sign: usize) -> Result<bool> {
        // An adjacent minus belongs to the numeric receiver; power keeps the
        // outer sign.
        Ok(self.tokens[sign].end == self.tokens[sign + 1].offset
            && matches!(
                self.tokens[sign + 1].token,
                Token::Int(_) | Token::BigInt(..) | Token::Float(_)
            )
            && !self
                .tokens
                .find(sign + 2..self.tokens.len(), self.work, |next| {
                    next.token != Token::EndLine || next.line == next.end_line
                })?
                .is_some_and(|next| next.token == Token::Op("**")))
    }

    fn hash_label(&mut self) -> Result<(Bytes, Option<Expr>)> {
        let offset = self.tokens[self.pos].offset as u32;
        let labeled = matches!(self.token(), Token::Word(w) if !w.starts_with('@'))
            || matches!(self.token(), Token::Bytes(_));
        let colon = self.significant(self.pos + 1);
        if !labeled || self.tokens[colon].token != Token::P(':') {
            return self.err(INVALID_HASH_PAIR);
        }
        let (key, label) = match self.bump()? {
            Token::Word(w) => (Bytes::from_slice(self.work, w.as_bytes())?, Some(w)),
            Token::Bytes(b) => (b, None),
            _ => unreachable!(),
        };
        self.line_breaks()?;
        self.expect_p(':')?;
        self.line_breaks()?;
        let shorthand = if matches!(self.token(), Token::P(',' | '}') | Token::Eof) {
            let Some(name) = label else {
                let name = String::from_utf8_lossy(key.as_ref());
                let mut end = name.len().min(64);
                while !name.is_char_boundary(end) {
                    end -= 1;
                }
                let ellipsis = if end < name.len() { "..." } else { "" };
                return self.err(format_args!(
                    "missing value for hash key {}{ellipsis}",
                    &name[..end]
                ));
            };
            Some(self.make_at(Node::Var(Name::new(self.work, &name)?), 1, offset)?)
        } else {
            None
        };
        Ok((key, shorthand))
    }

    fn words(&mut self, words: lexer::Words<'a>, offset: usize) -> Result<Expr> {
        self.work.charge(1)?;
        let mut values = Buffer::with_capacity(self.work, words.entries.len())?;
        for word in words.entries {
            values.push(self.work, self.template(word, words.symbol, offset)?)?;
        }
        let depth = 1 + values.iter().map(|v| v.depth).max().unwrap_or(0);
        self.make(Node::Array(values), depth)
    }

    /// Builds a string or symbol from its parts. Like Go, an interpolation's
    /// failure is reported at the literal, at `offset`.
    fn template(
        &mut self,
        parts: crate::compilation::Buffer<Part<'a>>,
        symbol: bool,
        offset: usize,
    ) -> Result<Expr> {
        self.work.charge(1)?;
        if !parts.iter().any(|part| matches!(part, Part::Expr(..))) {
            let bytes = lexer::plain(parts, self.work)?;
            let value = bytes.into_value(symbol);
            return self.make(Node::Literal(value), 1);
        }
        let mut values = Buffer::with_capacity(self.work, parts.len())?;
        for part in parts {
            values.push(
                self.work,
                match part {
                    Part::Text(bytes) => self.make(Node::Literal(bytes.into_value(false)), 1)?,
                    Part::Expr(tokens, span) => {
                        self.interpolations.push(self.work, span)?;
                        let text = &self.source[span.0 as usize..span.1 as usize - 1];
                        if text.trim().is_empty() {
                            return Err(Error::syntax(
                                self.work,
                                offset,
                                "empty string interpolation",
                            ));
                        }
                        self.interpolation(tokens, offset)?
                    }
                },
            )?;
        }
        let depth = 1 + values.iter().map(|v| v.depth).max().unwrap_or(0);
        self.make(Node::Template(values, symbol), depth)
    }

    fn interpolation(
        &mut self,
        mut tokens: crate::compilation::Buffer<Lexeme<'a>>,
        offset: usize,
    ) -> Result<Expr> {
        self.work.charge(1)?;
        while tokens.len() >= 2 {
            self.work.charge(1)?;
            let tail = &tokens[tokens.len() - 2];
            if tail.token != Token::EndLine || tail.line == tail.end_line {
                break;
            }
            tokens.remove(tokens.len() - 2);
        }
        let mut parser = Parser {
            work: self.work,
            source: self.source,
            lex_depth: self.lex_depth + 1,
            tokens: Tokens::new(tokens, self.work)?,
            pos: 0,
            depth: self.depth,
            groups: 0,
            line_exprs: 0,
            command_depth: 0,
            ternaries: Buffer::new(),
            command_group: 0,
            loop_condition: None,
            then_stop: None,
            locals: std::mem::take(&mut self.locals),
            declared_it: self.declared_it,
            type_call: false,
            type_argument: false,
            type_structural_error: false,
            interpolations: Buffer::new(),
            suffixed: RefCell::new(self.suffixed.take()),
            target_start: None,
            namespace: self.namespace,
            namespace_body: self.namespace_body,
            // Go parses interpolations without the member probe.
            record: None,
            inside_class: false,
            nesting: 0,
            call_end: 0,
            percent_argument: 0,
            block_name: self.block_name.clone(),
            type_names: std::mem::take(&mut self.type_names),
            alias_names: std::mem::take(&mut self.alias_names),
            additions: std::mem::take(&mut self.additions),
        };
        while parser.token() == &Token::EndLine
            && parser.tokens[parser.pos].line != parser.tokens[parser.pos].end_line
        {
            self.work.charge(1)?;
            parser.pos += 1;
        }
        // Interpolation depth is bounded by the lexer, so each level can run
        // its own task stack.
        let parsing = Parsing::<recovery::FailFast>::new(parser);
        let result = parsing.run(Call::Interpolation);
        let parser = parsing.parser.into_inner();
        let complete = parser.token() == &Token::Eof;
        self.locals = parser.locals;
        self.declared_it = parser.declared_it;
        self.type_names = parser.type_names;
        self.alias_names = parser.alias_names;
        self.additions = parser.additions;
        self.interpolations
            .extend(self.work, parser.interpolations)?;
        self.suffixed = parser.suffixed;
        let expr = match result {
            Ok(Parsed::Expr(expr)) => expr,
            Ok(_) => unreachable!(),
            Err(error) if error.kind == crate::ErrorKind::Syntax => {
                return Err(if error.message == ADJACENT_EXPRESSIONS {
                    error
                } else if error.message == TOO_DEEP {
                    Error::syntax(self.work, offset, TOO_DEEP)
                } else {
                    Error::syntax(
                        self.work,
                        offset,
                        format_args!("invalid string interpolation: {}", error.message),
                    )
                });
            }
            Err(error) => return Err(error),
        };
        if !complete {
            return Err(Error::syntax(
                self.work,
                offset,
                "string interpolation must contain a single expression",
            ));
        }
        Ok(expr)
    }

    pub(super) fn expand_modulo(&mut self) -> Result<()> {
        self.work.charge(1)?;
        // A quoted index can extend past a tentative percent-literal delimiter.
        // Re-lex its suffix through the next intact token boundary.
        let limit = self.tokens.last().unwrap().offset;
        let tokens = lexer::modulo(
            self.source,
            &self.tokens[self.pos],
            limit,
            self.lex_depth,
            self.work,
        )?;
        self.replace_lexed(tokens, limit)
    }

    pub(super) fn expand_regex(&mut self) -> Result<()> {
        self.work.charge(1)?;
        let limit = self.tokens.last().unwrap().offset;
        let tokens = lexer::regex(
            self.source,
            &self.tokens[self.pos],
            limit,
            self.lex_depth,
            self.work,
        )?;
        self.replace_lexed(tokens, limit)
    }

    fn replace_lexed(
        &mut self,
        mut tokens: crate::compilation::Buffer<Lexeme<'a>>,
        limit: usize,
    ) -> Result<()> {
        self.work.charge(1)?;
        let mut cursor = tokens.pop().unwrap();
        let mut finish = self.pos + 1;
        loop {
            while self.tokens[finish].offset < cursor.offset {
                self.work.charge(1)?;
                finish += 1;
            }
            let end = self.tokens[finish - 1].end.max(self.tokens[finish].offset);
            if end <= cursor.offset {
                break;
            }
            let mut suffix = lexer::resume(
                self.source,
                &cursor,
                end,
                limit,
                self.lex_depth,
                tokens.last(),
                self.work,
            )?;
            cursor = suffix.pop().unwrap();
            tokens.extend(self.work, suffix)?;
        }
        self.tokens.replace(self.pos..finish, tokens, self.work)?;
        Ok(())
    }
}

/// Go's rejection of a hash entry that is not a labeled or quoted key and its value.
const INVALID_HASH_PAIR: &str = "invalid hash pair: expected key like name: or \"name\":";
