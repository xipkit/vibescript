use super::*;

impl<M: super::recovery::Mode> Parsing<'_, M> {
    pub(super) async fn block_task(&self, stop: &[&str]) -> Result<Buffer<Stmt>> {
        let work = self.p().work;
        work.charge(1)?;
        let mut body = Buffer::new();
        self.p().nesting += 1;
        loop {
            {
                let mut p = self.p();
                p.lines()?;
                if matches!(p.token(),Token::Word(w) if stop.contains(&w.as_str()))
                    || (p.token() == &Token::P('}') && stop.contains(&"}"))
                {
                    break;
                }
                if matches!(p.token(), Token::Eof) {
                    return p.expected(Label::Text(if stop.contains(&"}") { "}" } else { "end" }));
                }
            }
            let checkpoint = self.recovery_checkpoint();
            match self.statement().await {
                Ok(statement) => body.push(work, statement)?,
                Err(error) => self.recover(checkpoint, stop, error)?,
            }
        }
        self.p().nesting -= 1;
        Ok(body)
    }

    /// Parses a statement that declares nothing.
    pub(super) async fn plain(&self) -> Result<Stmt> {
        let offset = {
            let mut p = self.p();
            p.work.charge(1)?;
            p.enter()?;
            p.tokens[p.pos].offset as u32
        };
        let stmt = self
            .modified_statement(offset)
            .await?
            .at(self.work, offset)?;
        let mut p = self.p();
        p.expression_separator()?;
        p.check_depth(stmt.depth, stmt.offset)?;
        p.depth -= 1;
        Ok(stmt)
    }

    async fn modified_statement(&self, offset: u32) -> Result<Statement> {
        self.p().work.charge(1)?;
        let starts_begin = matches!(self.p().token(), Token::Word(word) if word == "begin");
        let mut stmt = if starts_begin {
            framed(self.work, self.begin_statement())?.await?
        } else {
            self.plain_statement().await?
        };
        if matches!(
            stmt,
            Statement::If(..) | Statement::While(..) | Statement::For(..)
        ) {
            stmt = framed(self.work, self.continued_statement(stmt, offset))?.await?;
        }
        let modifier = match self.p().token() {
            Token::Word(w) if matches!(w.as_str(), "if" | "while") => *w,
            Token::Word(w) if matches!(w.as_str(), "unless" | "until") && !canonical() => *w,
            _ => return Ok(stmt),
        };
        let bare_begin = starts_begin
            && matches!(&stmt, Statement::Expr(Expr { node: Node::Try(attempt), .. }) if !attempt.modifier);
        if bare_begin
            || !matches!(
                stmt,
                Statement::Expr(_)
                    | Statement::Raise(..)
                    | Statement::Retry
                    | Statement::Assign(..)
                    | Statement::Return(_)
                    | Statement::Break(_)
                    | Statement::Next(_)
            )
        {
            framed(self.work, self.reject_modifier())?.await?;
            return Ok(stmt);
        }
        let keyword = {
            let mut p = self.p();
            let keyword = p.tokens[p.pos].offset as u32;
            p.bump()?;
            keyword
        };
        let mut condition = self.line_expr(0).await?;
        let p = self.p();
        let work = p.work;
        if matches!(modifier.as_str(), "unless" | "until") {
            condition = p.negate(condition)?;
        }
        let body = Buffer::from_array(work, [stmt.at(work, offset)?])?;
        Ok(if matches!(modifier.as_str(), "while" | "until") {
            Statement::While(condition, body, Some(keyword))
        } else {
            Statement::If(
                Buffer::from_array(work, [(condition, body)])?,
                Buffer::new(),
                Some(keyword),
            )
        })
    }

    // Like Go, an operator or member access after a compound statement's
    // `end` on the same line continues the statement as an expression.
    async fn continued_statement(&self, stmt: Statement, offset: u32) -> Result<Statement> {
        let expr = {
            let p = self.p();
            p.work.charge(1)?;
            if !p.statement_continues()? {
                return Ok(stmt);
            }
            let stmt = stmt.at(p.work, offset)?;
            let depth = stmt.depth;
            p.make_at(Node::Compound(Boxed::new(p.work, stmt)?), depth, offset)?
        };
        Ok(Statement::Expr(self.statement_tail(expr).await?))
    }

    // Go parses a statement-position begin as a statement, which costs no
    // expression nesting, and then continues any expression after its end.
    async fn begin_statement(&self) -> Result<Statement> {
        let offset = {
            let mut p = self.p();
            let offset = p.tokens[p.pos].offset as u32;
            p.pos += 1;
            offset
        };
        let mut attempt = self.begin_expression(offset).await?;
        attempt.offset = offset;
        if !self.p().statement_continues()? {
            return Ok(Statement::Expr(attempt));
        }
        Ok(Statement::Expr(self.statement_tail(attempt).await?))
    }

    /// Continues a compound statement as an expression. Go reads this
    /// continuation without its line limit, and the operands inside it too
    /// when no enclosing line expression limits them.
    async fn statement_tail(&self, expr: Expr) -> Result<Expr> {
        let open = {
            let mut p = self.p();
            let open = p.line_exprs == 0;
            if open {
                p.groups += 1;
            }
            open
        };
        let result = self.expr_tail(expr, 0, None, None, true).await;
        if open {
            self.p().groups -= 1;
        }
        result
    }

    async fn plain_statement(&self) -> Result<Statement> {
        let keyword = {
            let mut p = self.p();
            p.work.charge(1)?;
            let keyword = [
                "raise", "retry", "if", "unless", "while", "until", "for", "return", "break",
                "next",
            ]
            .into_iter()
            .find(|keyword| matches!(p.token(), Token::Word(w) if w == keyword))
            .filter(|keyword| !matches!(*keyword, "unless" | "until") || !canonical());
            if keyword.is_some() {
                p.pos += 1;
            }
            keyword
        };
        match keyword {
            Some("raise") => self.raise_statement().await,
            Some("retry") => {
                let mut p = self.p();
                let line = p.tokens[p.pos - 1].line;
                if !p.ends_after(p.pos - 1, line) && !p.modifier_follows(line) {
                    p.pos = p.significant(p.pos);
                    return p.err("retry does not accept a value");
                }
                Ok(Statement::Retry)
            }
            Some(keyword @ ("if" | "unless")) => self.if_stmt(keyword == "unless").await,
            Some(keyword @ ("while" | "until")) => self.while_stmt(keyword == "until").await,
            Some("for") => self.for_stmt().await,
            Some(flow) => self.flow_statement(flow).await,
            None if self.p().typed_local_ahead()? => self.typed_local().await,
            None if self.p().assertion() => self.assertion().await,
            None if self.p().token() == &Token::Op("*") || self.p().assignment_ahead()? => {
                self.assignment_statement().await
            }
            None => {
                let start = self.p().pos;
                let expr = self.line_expr(0).await?;
                // Go reads an expression followed by a comma as a destructuring
                // target list, and one followed by an assignment operator as a target.
                let target = {
                    let p = self.p();
                    match &p.tokens[p.significant(p.pos)].token {
                        Token::P(',') => true,
                        Token::Op(op) => assignment(op),
                        _ => false,
                    }
                };
                if target {
                    self.p().pos = start;
                    return self.assignment_statement().await;
                }
                Ok(Statement::Expr(self.trailing_block(expr).await?))
            }
        }
    }

    /// Parses `assert` and its arguments, as Go's `parseAssertStatement` does.
    async fn assertion(&self) -> Result<Statement> {
        let work = self.p().work;
        let (callee, offset) = {
            let mut p = self.p();
            let offset = p.tokens[p.pos].offset as u32;
            let line = p.tokens[p.pos].line;
            p.bump()?;
            let callee = p.variable_name("assert")?;
            if p.ends_after(p.pos - 1, line) {
                return Ok(Statement::Expr(callee));
            }
            p.line_breaks()?;
            (callee, offset)
        };
        let mut args = Buffer::new();
        loop {
            let value = self.line_expr(0).await?;
            args.push(
                work,
                Argument {
                    kind: ArgumentKind::Positional,
                    value,
                },
            )?;
            let mut p = self.p();
            if !p.comma_follows() {
                break;
            }
            p.line_breaks()?;
        }
        let depth =
            1 + call_depth(&callee).max(args.iter().map(|a| a.value.depth).max().unwrap_or(0));
        let call = Node::Call(Name::new(work, "assert")?, args, CallForm::Bare);
        Ok(Statement::Expr(self.p().make_at(call, depth, offset)?))
    }

    async fn flow_statement(&self, flow: &str) -> Result<Statement> {
        let value = {
            let mut p = self.p();
            let line = p.tokens[p.pos - 1].line;
            let value = !p.ends_after(p.pos - 1, line) && !p.modifier_follows(line);
            if value {
                p.line_breaks()?;
            }
            value
        };
        let value = if value {
            let first = self.line_expr(0).await?;
            Some(if flow == "return" {
                self.return_values(first).await?
            } else {
                first
            })
        } else {
            None
        };
        Ok(match flow {
            "return" => Statement::Return(value),
            "break" => Statement::Break(value),
            _ => Statement::Next(value),
        })
    }

    async fn assignment_statement(&self) -> Result<Statement> {
        let start = self.p().pos;
        let (target, _) = self.target(Place::Statement).await?;
        let op = {
            let mut p = self.p();
            let next = p.significant(p.pos);
            if matches!(&p.tokens[next].token, Token::Op(op) if assignment(op)) {
                p.pos = next;
            }
            let op = match p.token() {
                Token::Op(op) if assignment(op) => Some(*op),
                _ => None,
            };
            // Go parses the statement's expression first and makes it a target only
            // when a comma or an assignment operator follows.
            match (op, &target) {
                (None, Target::Tuple(_)) => {
                    let last = p.previous_index()?;
                    return Err(Error::syntax(
                        p.work,
                        p.position(last),
                        "parallel assignment targets require '='",
                    ));
                }
                (None, _) => p.pos = start,
                (Some(op), Target::Tuple(_)) if op != "=" => {
                    return Err(Error::syntax(
                        p.work,
                        p.position(p.pos),
                        "compound assignment is not supported for destructuring targets",
                    ));
                }
                (Some(_), Target::Value(expr)) => {
                    p.assignment_member(expr)?;
                    if let Some(offset) = expr.safe_navigation() {
                        return Err(Error::syntax(
                            p.work,
                            offset,
                            "safe navigation cannot be used as an assignment target",
                        ));
                    }
                    if !expr.assignable() {
                        return p.unexpected(p.pos);
                    }
                }
                _ => (),
            }
            if op.is_some() {
                p.pos += 1;
                p.line_breaks()?;
            }
            op
        };
        let Some(op) = op else {
            return Ok(Statement::Expr(self.block_line_expr().await?));
        };
        let first = self.block_line_expr().await?;
        let rhs = if matches!(target, Target::Tuple(_)) && self.p().comma_follows() {
            let work = self.p().work;
            let mut items = Buffer::from_array(work, [first])?;
            loop {
                {
                    // Like Go, a comma the line ends on closes the list.
                    let mut p = self.p();
                    let comma = p.pos - 1;
                    if p.ends_after(comma, p.tokens[comma].line) {
                        break;
                    }
                    p.line_breaks()?;
                }
                items.push(work, self.block_line_expr().await?)?;
                if !self.p().comma_follows() {
                    break;
                }
            }
            let depth = 1 + items.iter().map(|e| e.depth).max().unwrap_or(0);
            self.p().make(Node::Array(items), depth)?
        } else {
            first
        };
        self.p().declare_target(&target)?;
        Ok(Statement::Assign(target, op, rhs))
    }

    async fn return_values(&self, first: Expr) -> Result<Expr> {
        let work = self.p().work;
        work.charge(1)?;
        if !self.p().comma_on_line()? {
            return Ok(first);
        }
        let mut items = Buffer::from_array(work, [first])?;
        while self.p().comma_on_line()? {
            {
                let mut p = self.p();
                p.bump()?;
                p.line_breaks()?;
            }
            items.push(work, self.line_expr(0).await?)?;
        }
        let depth = 1 + items.iter().map(|e| e.depth).max().unwrap_or(0);
        self.p().make(Node::Array(items), depth)
    }
}

impl Parser<'_> {
    /// Reports whether `assert` starts Go's assertion statement, which takes a
    /// comma-separated argument list unless parentheses follow directly.
    fn assertion(&self) -> bool {
        matches!(self.token(), Token::Word(w) if *w == "assert")
            && !(self.tokens[self.pos + 1].token == Token::P('(')
                && self.tokens[self.pos + 1].offset == self.tokens[self.pos].end)
    }
    fn comma_on_line(&self) -> Result<bool> {
        Ok(self.token() == &Token::P(',') && self.tokens[self.pos].line == self.previous()?.line)
    }
    fn assignment_ahead(&self) -> Result<bool> {
        self.work.charge(1)?;
        let mut nesting = 0usize;
        let mut comma = false;
        let mut after_member_separator = false;
        for (i, lexeme) in self.tokens.from(self.pos).enumerate() {
            self.work.charge(1)?;
            match &lexeme.token {
                Token::P('(' | '[' | '{') => nesting += 1,
                Token::P(')' | ']' | '}') => {
                    if nesting == 0 {
                        return Ok(false);
                    }
                    nesting -= 1;
                }
                Token::Op(op) if nesting == 0 && assignment(op) => return Ok(true),
                Token::EndLine if nesting == 0 => {
                    if lexeme.line == lexeme.end_line {
                        return Ok(false);
                    }
                    if after_member_separator {
                        continue;
                    }
                    let next =
                        self.tokens
                            .find(self.pos + i + 1..self.tokens.len(), self.work, |l| {
                                !matches!(l.token, Token::EndLine)
                            })?;
                    if !comma
                        && !next.is_some_and(|l| {
                            matches!(l.token, Token::P('.') | Token::Op("&."))
                                || matches!(l.token, Token::Op(op) if assignment(op))
                        })
                    {
                        return Ok(false);
                    }
                }
                Token::Word(w)
                    if nesting == 0 && reserved(w) && w != "then" && !after_member_separator =>
                {
                    return Ok(false);
                }
                Token::Eof => return Ok(false),
                _ => (),
            }
            if !matches!(lexeme.token, Token::EndLine) {
                comma = lexeme.token == Token::P(',');
                after_member_separator = matches!(lexeme.token, Token::P('.') | Token::Op("&."));
            }
        }
        Ok(false)
    }
}

pub(super) fn assignment(op: &str) -> bool {
    matches!(
        op,
        "=" | "+=" | "-=" | "*=" | "/=" | "//=" | "%=" | "**=" | "||=" | "&&="
    )
}
