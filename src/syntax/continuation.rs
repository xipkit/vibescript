use super::*;

pub(super) enum Suffix {
    Rescue,
    Command,
    Block(bool),
    Call,
    Scope,
    Member(bool),
    Index(u32),
    Ternary(u32),
    Binary(&'static str, u8, u32),
}

impl Parser<'_> {
    /// Finds the next suffix as outside any line expression, where Go ignores
    /// line breaks.
    pub(super) fn unlimited_suffix(&mut self, lhs: &Expr, min: u8) -> Result<Option<Suffix>> {
        let (line_exprs, groups) = (self.line_exprs, self.groups);
        self.line_exprs = 0;
        self.groups += 1;
        let suffix = self.expression_suffix(lhs, min, None);
        (self.line_exprs, self.groups) = (line_exprs, groups);
        suffix
    }
    /// Reports whether a compound statement continues as an expression, as
    /// Go's `continueStatementExpression` does: an operator or other suffix
    /// on the line of its `end`.
    pub(super) fn statement_continues(&self) -> Result<bool> {
        let next = &self.tokens[self.pos];
        let continues = match &next.token {
            Token::P('.' | '(' | '[' | '?') | Token::Op("&." | "::") => true,
            Token::Op(op) => binding_power(op).is_some(),
            Token::Words(words) => words.ambiguous,
            Token::Word(w) => w == "rescue" || (w == "do" && !canonical()),
            _ => false,
        };
        Ok(continues && next.line == self.previous()?.end_line)
    }
    pub(super) fn expression_suffix(
        &mut self,
        lhs: &Expr,
        min: u8,
        line: Option<usize>,
    ) -> Result<Option<Suffix>> {
        let resumed = self.continuation_position(min)?;
        if let Some(next) = resumed {
            self.pos = next;
        } else if line.is_some_and(|line| self.tokens[self.pos].line > line)
            && self.line_exprs > 0
            && self.token() != &Token::EndLine
            && !self.limit_continues(self.pos)?
            && !self.begin_call(lhs)
        {
            // Go limits a line expression to the line its prefix started on,
            // so a prefix spanning lines takes only a continuation token.
            return Ok(None);
        }
        if min == 0
            && (self.command_depth == 0 || self.groups > self.command_group)
            && (resumed.is_some() || self.tokens[self.pos].line == self.previous()?.end_line)
            && matches!(self.token(), Token::Word(w) if w == "rescue")
        {
            // `rescue:` labels a parenless call's keyword argument, as in Go.
            if !self.keyword_label(self.pos) {
                self.pos += 1;
                return Ok(Some(Suffix::Rescue));
            }
            if !matches!(
                lhs.node,
                Node::Var(_) | Node::Member(..) | Node::SafeMember(..)
            ) {
                return self.err("rescue modifier requires fallback expression");
            }
        }
        let offset = self.tokens[self.pos].offset as u32;
        if self.command_start(lhs, min)? {
            if matches!(self.token(), Token::Words(_)) {
                self.percent_argument = self.pos;
            }
            return Ok(Some(Suffix::Command));
        }
        // A brace on the line of a call starts its block; anywhere else it
        // starts a hash literal, which cannot follow an expression.
        if self.token() == &Token::P('{')
            && self.tokens[self.pos].line == self.previous()?.end_line
            && self.block_follows(lhs)?
        {
            return Ok(Some(Suffix::Block(true)));
        }
        if matches!(self.token(), Token::Word(w) if w == "do" && !canonical())
            && (self.can_attach_do()
                || (self.pos != self.call_end && self.significant(self.call_end) == self.pos))
        {
            return Ok(Some(Suffix::Block(false)));
        }
        if self.take_p('(') {
            return Ok(Some(Suffix::Call));
        }
        if self.token() == &Token::Op("::") {
            return Ok(Some(Suffix::Scope));
        }
        let safe = self.token() == &Token::Op("&.");
        if safe || self.token() == &Token::P('.') {
            return Ok(Some(Suffix::Member(safe)));
        }
        if self.take_p('[') {
            return Ok(Some(Suffix::Index(offset)));
        }
        if self.token() == &Token::P('?')
            && let Some(error) = self.diagnostic(self.pos)
        {
            return Err(error);
        }
        if min <= 2 && self.take_p('?') {
            return Ok(Some(Suffix::Ternary(offset)));
        }
        if matches!(self.token(), Token::Words(words) if words.ambiguous) {
            self.expand_modulo()?;
        }
        let Token::Op(op) = self.token() else {
            return Ok(None);
        };
        let op = *op;
        let Some((left, right)) = binding_power(op) else {
            return Ok(None);
        };
        // Go locates a binary expression at its operator's token; `//`,
        // which Go lacks, is located at its start.
        let offset = if op == "//" {
            self.tokens[self.pos].offset
        } else {
            self.position(self.pos)
        } as u32;
        Ok((left >= min).then_some(Suffix::Binary(op, right, offset)))
    }
    /// Reports whether parentheses call the value of a begin expression. The
    /// selected computed-call policy keeps this call where Go's line limit
    /// would end the expression; see docs/computed-calls.md.
    fn begin_call(&self, lhs: &Expr) -> bool {
        self.token() == &Token::P('(')
            && matches!(&lhs.node, Node::Try(attempt) if !attempt.modifier)
    }
    /// Reports whether the token at `index` continues a line expression past
    /// its line, as Go's `lineLimitedContinuationToken` does.
    fn limit_continues(&self, index: usize) -> Result<bool> {
        Ok(match &self.tokens[index].token {
            Token::P('.' | '?') => true,
            // An index that abuts the end of an expression spanning lines, as
            // in `(case x ... end)[0]`, indexes it; adjacent expressions are
            // an error.
            Token::P('[') => index > 0 && self.tokens[index - 1].end == self.tokens[index].offset,
            Token::Words(words) => words.ambiguous,
            Token::Op("*") => !self.splat_assignment_ahead(index)?,
            Token::Op("+" | "-") => {
                let sign = &self.tokens[index];
                let next = &self.tokens[self.significant(index + 1)];
                next.token != Token::Eof
                    && (next.line > sign.line || (next.line == sign.line && next.offset > sign.end))
            }
            Token::Op(op) => matches!(
                *op,
                "&." | "::"
                    | "/"
                    | "//"
                    | "**"
                    | "%"
                    | ".."
                    | "..."
                    | "=="
                    | "==="
                    | "!="
                    | "=~"
                    | "!~"
                    | "<"
                    | "<="
                    | ">"
                    | ">="
                    | "<=>"
                    | "&&"
                    | "||"
                    | "<<"
                    | "&"
            ),
            _ => false,
        })
    }
    fn continuation_position(&self, min: u8) -> Result<Option<usize>> {
        if self.token() != &Token::EndLine {
            return Ok(None);
        }
        let mut next = self.pos;
        while self.tokens[next].token == Token::EndLine {
            self.work.charge(1)?;
            if self.tokens[next].line == self.tokens[next].end_line {
                return Ok(None);
            }
            next += 1;
        }
        let lexeme = &self.tokens[next];
        let continues = match lexeme.token {
            // Go continues a line-limited expression onto a `do` only right
            // after a parenthesized call with arguments.
            Token::Word(ref word) if word == "do" && !canonical() => {
                (self.line_exprs == 0 && self.can_attach_do()) || self.pos == self.call_end
            }
            Token::P('.') | Token::Op("::" | "&.") => true,
            Token::P('?') => min <= 2,
            // Outside a line expression, Go reads any suffix after a line
            // break, but a block starts on its call's line.
            Token::P('(' | '[') => self.line_exprs == 0 && self.groups > 0,
            Token::Word(ref word) if word == "rescue" => {
                self.line_exprs == 0 && self.groups > 0 && min == 0 && !self.keyword_label(next)
            }
            Token::Op(op) => {
                let Some((left, _)) = binding_power(op) else {
                    return Ok(None);
                };
                if left < min {
                    return Ok(None);
                }
                if self.line_exprs == 0 {
                    if matches!(op, "/" | "//") {
                        return Ok(None);
                    }
                    return Ok((self.groups > 0).then_some(next));
                }
                match op {
                    "+" | "-" => self
                        .tokens
                        .find(next + 1..self.tokens.len(), self.work, |t| {
                            t.token != Token::EndLine || t.line == t.end_line
                        })?
                        .is_some_and(|operand| {
                            !matches!(operand.token, Token::Eof | Token::EndLine)
                                && (operand.line > lexeme.end_line || operand.offset > lexeme.end)
                        }),
                    "*" => !self.splat_assignment_ahead(next)?,
                    "/" | "//" => false,
                    _ => true,
                }
            }
            _ => false,
        };
        Ok(continues.then_some(next))
    }
    fn splat_assignment_ahead(&self, start: usize) -> Result<bool> {
        self.work.charge(1)?;
        let mut groups = 0usize;
        let mut previous = &self.tokens[start];
        let operand = &self.tokens[start + 1];
        let shaped = matches!(operand.token, Token::P(',') | Token::Op("="))
            || operand.offset == previous.end;
        let mut comma = false;
        for token in self.tokens.from(start + 1) {
            self.work.charge(1)?;
            if token.line > self.tokens[start].line + 64 {
                return Ok(false);
            }
            if token.token == Token::EndLine {
                if token.line == token.end_line {
                    return Ok(false);
                }
                continue;
            }
            if token.line > previous.end_line
                && groups == 0
                && previous.token != Token::P(',')
                && !((shaped || comma)
                    && (token.token == Token::Op("=")
                        || (matches!(token.token, Token::P('.') | Token::Op("&."))
                            && matches!(previous.token, Token::Word(_) | Token::P(')' | ']')))))
            {
                return Ok(false);
            }
            if groups == 0 && token.token != Token::Op("=") {
                let allowed = if matches!(previous.token, Token::P('.') | Token::Op("&.")) {
                    matches!(token.token, Token::Word(_))
                } else {
                    match &token.token {
                        Token::Word(w) => !reserved(w),
                        Token::P(',' | '.' | '(' | ')' | '[' | ']') | Token::Op("*" | "&.") => true,
                        _ => false,
                    }
                };
                if !allowed {
                    return Ok(false);
                }
            }
            match token.token {
                Token::P('(' | '[') => groups += 1,
                Token::P(')' | ']') => {
                    let Some(next) = groups.checked_sub(1) else {
                        return Ok(false);
                    };
                    groups = next;
                }
                Token::Op("=") if groups == 0 => return Ok(true),
                Token::P(',') if groups == 0 => comma = true,
                Token::Eof => return Ok(false),
                _ => (),
            }
            previous = token;
        }
        Ok(false)
    }
}

fn binding_power(op: &str) -> Option<(u8, u8)> {
    Some(match op {
        "||" => (3, 4),
        "&&" => (4, 5),
        "==" | "!=" | "===" | "=~" | "!~" => (5, 6),
        "<" | "<=" | ">" | ">=" | "<=>" => (6, 7),
        ".." | "..." => (7, 8),
        "&" => (9, 10),
        "<<" => (10, 11),
        "+" | "-" => (11, 12),
        "*" | "/" | "//" | "%" => (12, 13),
        "**" => (14, 14),
        _ => return None,
    })
}
