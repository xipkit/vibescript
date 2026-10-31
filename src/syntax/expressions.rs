use super::*;

impl<M: super::recovery::Mode> Parsing<'_, M> {
    /// Parses a line expression that may take a `do` block from a later line.
    pub(super) async fn block_line_expr(&self) -> Result<Expr> {
        let expr = self.line_expr(0).await?;
        self.trailing_block(expr).await
    }

    pub(super) async fn line_expr(&self, min: u8) -> Result<Expr> {
        {
            let mut p = self.p();
            p.work.charge(1)?;
            p.line_exprs += 1;
        }
        let result = self.expr(min).await;
        self.p().line_exprs -= 1;
        result
    }

    pub(super) async fn expression(&self, min: u8) -> Result<Expr> {
        {
            let mut p = self.p();
            p.work.charge(1)?;
            p.enter()?;
        }
        let line = {
            let p = self.p();
            p.tokens[p.pos].line
        };
        let lhs = self.prefix().await?;
        let result = self.expr_tail(lhs, min, None, Some(line), false).await;
        self.p().depth -= 1;
        result
    }

    async fn prefix(&self) -> Result<Expr> {
        let (offset, grouped) = {
            let p = self.p();
            p.work.charge(1)?;
            // Go locates a folded negative number at its digits.
            let folded = p.token() == &Token::Op("-") && p.negative_literal(p.pos)?;
            (
                p.tokens[p.pos + usize::from(folded)].offset as u32,
                p.token() == &Token::P('('),
            )
        };
        let mut expr = self.prefix_node().await?;
        if !grouped {
            expr.offset = offset;
        }
        Ok(expr)
    }

    async fn prefix_node(&self) -> Result<Expr> {
        let (offset, token) = {
            let mut p = self.p();
            p.work.charge(1)?;
            let offset = p.tokens[p.pos].offset as u32;
            (offset, p.bump()?)
        };
        match token {
            Token::Word(w) => framed(self.work, self.word_expression(w.as_str(), offset))?.await,
            Token::P('(') => self.group_expression().await,
            Token::P('[') => self.array_expression().await,
            Token::P('{') => framed(self.work, self.hash_expr())?.await,
            Token::Op(op @ (".." | "...")) => {
                framed(self.work, self.open_range_expression(op))?.await
            }
            Token::Op(op @ ("-" | "+" | "!")) => framed(self.work, self.unary_prefix(op))?.await,
            token => self.p().leaf(token),
        }
    }

    async fn group_expression(&self) -> Result<Expr> {
        {
            let mut p = self.p();
            p.groups += 1;
            p.line_breaks()?;
        }
        let e = self.expr(0).await?;
        let mut p = self.p();
        p.line_breaks()?;
        p.expect_p(')')?;
        p.groups -= 1;
        Ok(e)
    }

    async fn array_expression(&self) -> Result<Expr> {
        let a = self.arguments(']', true).await?;
        let d = 1 + a.iter().map(|e| e.depth).max().unwrap_or(0);
        self.p().make(Node::Array(a), d)
    }

    async fn open_range_expression(&self, op: &str) -> Result<Expr> {
        {
            let mut p = self.p();
            let next = p.significant(p.pos);
            if !p.prefix(next) {
                p.pos -= 1;
                return p.err("range is missing end expression");
            }
            p.pos = next;
        }
        let end = self.expr(8).await?;
        let depth = end.depth + 1;
        let p = self.p();
        p.make(
            Node::Range(None, Some(Boxed::new(p.work, end)?), op == "..."),
            depth,
        )
    }

    async fn word_expression(&self, w: &str, offset: u32) -> Result<Expr> {
        self.p().work.charge(1)?;
        match w {
            "nil" => self.p().make(Node::Literal(Value::nil()), 1),
            "true" => self.p().make(Node::Literal(Value::boolean(true)), 1),
            "false" => self.p().make(Node::Literal(Value::boolean(false)), 1),
            "if" => self.if_expr(false).await,
            "unless" if !canonical() => self.if_expr(true).await,
            "case" => self.case_expr().await,
            "yield" => self.yield_expr().await,
            "begin" => self.begin_expression(offset).await,
            "while" | "for" => self.loop_expression(w, offset).await,
            "until" if !canonical() => self.loop_expression(w, offset).await,
            // Like Go, only `self` and `then` among the remaining keywords
            // start an expression.
            _ if keyword(w) && !matches!(w, "self" | "then") => Err(Error::syntax(
                self.p().work,
                offset as usize,
                format_args!("unexpected token {}", Label::word(w)),
            )),
            _ => self.p().variable_name(w),
        }
    }

    pub(super) async fn begin_expression(&self, offset: u32) -> Result<Expr> {
        let body = self.block(&["rescue", "else", "ensure", "end"]).await?;
        let attempt = self.rescue_tail(body, false, offset).await?;
        let depth = attempt.depth();
        let p = self.p();
        p.make(Node::Try(Boxed::new(p.work, attempt)?), depth)
    }

    async fn unary_prefix(&self, op: &'static str) -> Result<Expr> {
        let literal = {
            let p = self.p();
            p.work.charge(1)?;
            op == "-" && p.negative_literal(p.pos - 1)?
        };
        let value = if literal {
            self.prefix().await?
        } else {
            self.p().line_breaks()?;
            self.expr(13).await?
        };
        let depth = value.depth + 1;
        let p = self.p();
        p.make(Node::Unary(op, Boxed::new(p.work, value)?), depth)
    }

    /// Applies suffixes to `lhs`, starting with `next` if given. `line` is the
    /// line a prefix spanning lines started on, which Go keeps as the limit
    /// for the first suffix; later suffixes are limited to the line their
    /// predecessor's last token starts on. An `unlimited` tail takes suffixes
    /// from any line, as Go's `continueExpressionParse` does without a limit.
    pub(super) async fn expr_tail(
        &self,
        mut lhs: Expr,
        min: u8,
        mut next: Option<Suffix>,
        mut line: Option<usize>,
        unlimited: bool,
    ) -> Result<Expr> {
        self.p().work.charge(1)?;
        loop {
            let suffix = match next.take() {
                Some(suffix) => Some(suffix),
                None if unlimited => self.p().unlimited_suffix(&lhs, min)?,
                None => self.p().expression_suffix(&lhs, min, line.take())?,
            };
            let Some(suffix) = suffix else {
                return Ok(lhs);
            };
            lhs = match suffix {
                Suffix::Rescue => framed(self.work, self.rescue_modifier(lhs))?.await,
                Suffix::Command => self.command_expression(lhs).await,
                Suffix::Block(brace) => framed(self.work, self.block_expression(lhs, brace))?.await,
                Suffix::Call => {
                    let args = self.call_arguments().await?;
                    self.p().parenthesized_call(lhs, args)
                }
                Suffix::Scope => framed(self.work, self.scoped_expression(lhs))?.await,
                Suffix::Member(safe) => self.member_expression(lhs, safe).await,
                Suffix::Index(offset) => self.index_expression(lhs, offset).await,
                Suffix::Ternary(offset) => {
                    framed(self.work, self.ternary_expression(lhs, offset))?.await
                }
                Suffix::Binary(op, right, offset) => {
                    self.binary_expression(lhs, op, right, offset).await
                }
            }?;
            let p = self.p();
            line = Some(p.tokens[p.previous_index()?].line);
        }
    }

    async fn index_expression(&self, lhs: Expr, offset: u32) -> Result<Expr> {
        {
            let mut p = self.p();
            let close = p.significant(p.pos);
            if p.tokens[close].token == Token::P(']') {
                p.pos = close;
                return p.err("index expression requires at least one selector");
            }
        }
        let indexes = self.arguments(']', false).await?;
        let p = self.p();
        let d = 1 + lhs
            .depth
            .max(indexes.iter().map(|e| e.depth).max().unwrap_or(0));
        p.make_at(Node::Index(Boxed::new(p.work, lhs)?, indexes), d, offset)
    }

    async fn binary_expression(
        &self,
        lhs: Expr,
        op: &'static str,
        right: u8,
        offset: u32,
    ) -> Result<Expr> {
        let work = self.p().work;
        self.p().bump()?;
        if matches!(op, ".." | "...") {
            let has_end = {
                let mut p = self.p();
                if p.groups > 0 {
                    p.lines()?;
                }
                // Go ends a range at a pending condition's `then` even inside groups.
                let stop = p.then_stop.is_some()
                    && matches!(p.token(), Token::Word(word) if word == "then");
                p.starts_expression() && !stop
            };
            let end = if has_end {
                Some(Boxed::new(work, self.expr(right).await?)?)
            } else {
                None
            };
            let depth = 1 + lhs.depth.max(end.as_ref().map_or(0, |e| e.depth));
            return self.p().make_at(
                Node::Range(Some(Boxed::new(work, lhs)?), end, op == "..."),
                depth,
                offset,
            );
        }
        self.p().line_breaks()?;
        let rhs = self.expr(right).await?;
        let depth = 1 + lhs.depth.max(rhs.depth);
        self.p().make_at(
            Node::Binary(op, Boxed::new(work, lhs)?, Boxed::new(work, rhs)?),
            depth,
            offset,
        )
    }

    async fn ternary_expression(&self, condition: Expr, offset: u32) -> Result<Expr> {
        let work = self.p().work;
        {
            let mut p = self.p();
            work.charge(1)?;
            p.lines()?;
            let groups = p.groups;
            p.ternaries.push(work, groups)?;
        }
        let yes = self.expr(0).await?;
        {
            let mut p = self.p();
            p.ternaries.pop();
            p.ternary_separator()?;
            p.expect_p(':')?;
            p.lines()?;
        }
        let no = self.expr(2).await?;
        let depth = 1 + condition.depth.max(yes.depth).max(no.depth);
        self.p().make_at(
            Node::Conditional(
                Buffer::from_array(work, [(condition, yes)])?,
                Boxed::new(work, no)?,
            ),
            depth,
            offset,
        )
    }

    async fn scoped_expression(&self, lhs: Expr) -> Result<Expr> {
        let work = self.p().work;
        work.charge(1)?;
        let offset = lhs.offset;
        let (name, parenthesized) = {
            let mut p = self.p();
            p.bump()?;
            p.line_breaks()?;
            if !p.ident(p.pos) && !matches!(p.token(), Token::Word(w) if w == "enum") {
                return p.expected(Label::Text("identifier"));
            }
            let at = p.tokens[p.pos].offset;
            let Token::Word(name) = p.bump()? else {
                unreachable!()
            };
            // A name after `::` is spelled as one after `.` is; only one with
            // a `?` or `!` can be spelled wrong.
            if name.contains(['?', '!']) {
                p.read_spelling(&name, at)?;
            }
            let continues = matches!(
                p.tokens[p.significant(p.pos)].token,
                Token::Op("::") | Token::P('.')
            );
            let called = p.token() == &Token::P('(');
            p.scoped_read(&lhs, &name, at, continues, called)?;
            (Name::new(work, &name)?, p.take_p('('))
        };
        let args = if parenthesized {
            Some(self.call_arguments().await?)
        } else {
            None
        };
        let arguments = args.as_ref().map_or(0, |args| {
            args.iter().map(|arg| arg.value.depth).max().unwrap_or(0)
        });
        let receiver = if args.is_some() {
            1 + lhs.depth
        } else {
            lhs.depth
        };
        let depth = 1 + receiver.max(arguments);
        self.p().make_at(
            Node::Scope(Boxed::new(work, lhs)?, name, args),
            depth,
            offset,
        )
    }

    async fn member_expression(&self, lhs: Expr, safe: bool) -> Result<Expr> {
        let work = self.p().work;
        work.charge(1)?;
        let offset = lhs.offset;
        let (name, parenthesized) = {
            let mut p = self.p();
            p.bump()?;
            p.line_breaks()?;
            let name = p.member_name()?;
            p.note(|record| record.member(&name, &lhs));
            // `M.X?` reads a namespace's member as `M::X?` does.
            let at = p.tokens[p.pos - 1].offset;
            let called = p.token() == &Token::P('(');
            p.scoped_read(&lhs, &name, at, false, called)?;
            (name, p.take_p('('))
        };
        if parenthesized {
            self.p().type_call = name == "as"
                || (name == "parse_as"
                    && matches!(&lhs.node, Node::Var(receiver) if receiver == "JSON"));
            let mut args = self.call_arguments().await?;
            if name == "as" {
                self.p().nil_type_argument(&mut args)?;
            }
            // Go nests the member access below the call.
            let depth =
                1 + (1 + lhs.depth).max(args.iter().map(|a| a.value.depth).max().unwrap_or(0));
            let method = if safe { Node::SafeMethod } else { Node::Method };
            self.p().make_at(
                method(Boxed::new(work, lhs)?, name, args, CallForm::Parenthesized),
                depth,
                offset,
            )
        } else {
            let depth = lhs.depth + 1;
            let member = if safe { Node::SafeMember } else { Node::Member };
            self.p()
                .make_at(member(Boxed::new(work, lhs)?, name), depth, offset)
        }
    }

    /// Parses a comma-separated list through `close`, as Go reads array
    /// elements, or index selectors when `trailing` refuses a trailing comma.
    pub(super) async fn arguments(&self, close: char, trailing: bool) -> Result<Buffer<Expr>> {
        let work = self.p().work;
        work.charge(1)?;
        let mut args = Buffer::new();
        {
            let mut p = self.p();
            p.groups += 1;
            p.line_breaks()?;
            if p.take_p(close) {
                p.groups -= 1;
                return Ok(args);
            }
        }
        loop {
            args.push(work, self.expr(0).await?)?;
            let mut p = self.p();
            p.line_breaks()?;
            if p.take_p(close) {
                break;
            }
            if p.token() != &Token::P(',') {
                return p.expected(Label::Char(close));
            }
            p.expect_p(',')?;
            p.line_breaks()?;
            if trailing && p.take_p(close) {
                break;
            }
        }
        self.p().groups -= 1;
        Ok(args)
    }
}

impl Parser<'_> {
    // Like Go, the separator may start a later line.
    fn ternary_separator(&mut self) -> Result<()> {
        let mut next = self.pos;
        while self.tokens[next].token == Token::EndLine
            && self.tokens[next].line != self.tokens[next].end_line
        {
            self.work.charge(1)?;
            next += 1;
        }
        if self.tokens[next].token == Token::P(':') {
            self.pos = next;
        }
        Ok(())
    }
    pub(super) fn negate(&self, expr: Expr) -> Result<Expr> {
        self.work.charge(1)?;
        let depth = expr.depth + 1;
        let offset = expr.offset;
        self.make_at(
            Node::Unary("!", Boxed::new(self.work, expr)?),
            depth,
            offset,
        )
    }
    fn member_name(&mut self) -> Result<Name> {
        match self.token() {
            Token::Word(name) if !name.starts_with('@') => {
                let name = *name;
                let offset = self.tokens[self.pos].offset;
                self.bump()?;
                self.read_spelling(&name, offset)?;
                Name::new(self.work, &name)
            }
            Token::Op("<=>") => {
                self.bump()?;
                Name::new(self.work, "<=>")
            }
            _ => self.expected(Label::Text("member name")),
        }
    }
    pub(super) fn starts_expression(&self) -> bool {
        match self.token() {
            Token::Word(w) if w == "then" => self.then_stop != Some(self.groups),
            Token::Word(w) => {
                !reserved(w)
                    || matches!(
                        w.as_str(),
                        "if" | "unless" | "case" | "while" | "until" | "for" | "yield" | "begin"
                    )
            }
            Token::Int(_)
            | Token::BigInt(..)
            | Token::Float(_)
            | Token::Bytes(_)
            | Token::Regex(..)
            | Token::Template(_)
            | Token::Words(..)
            | Token::Symbol(_)
            | Token::QuotedSymbol(_) => true,
            Token::Invalid(invalid) => matches!(invalid.failure, Failure::Literal(_)),
            Token::P('(' | '[' | '{') | Token::Op("+" | "-" | "!") => true,
            _ => false,
        }
    }
}
