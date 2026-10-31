use super::*;

impl<M: super::recovery::Mode> Parsing<'_, M> {
    /// Attaches a block to a whole statement-level expression, as Go's
    /// `canAttachPeekBlock` does after a line expression: a `do` block from
    /// any line, or a brace block on the same line after a call.
    pub(super) async fn trailing_block(&self, expr: Expr) -> Result<Expr> {
        let brace = {
            let mut p = self.p();
            let next = p.significant(p.pos);
            let brace = match &p.tokens[next].token {
                Token::Word(w) if w == "do" && !canonical() => {
                    (next != p.pos || p.can_attach_do()).then_some(false)
                }
                Token::P('{') => (next == p.pos && p.block_follows(&expr)?).then_some(true),
                _ => None,
            };
            if brace.is_some() {
                p.pos = next;
            }
            brace
        };
        let Some(brace) = brace else {
            return Ok(expr);
        };
        framed(self.work, self.block_expression(expr, brace))?.await
    }

    pub(super) async fn block_expression(&self, mut lhs: Expr, brace: bool) -> Result<Expr> {
        self.p().work.charge(1)?;
        if brace {
            framed(self.work, self.hash_block(&lhs))?.await?;
        }
        let offset = lhs.offset;
        let block = self.attached_block(brace).await?;
        if matches!(lhs.node, Node::BlockCall(..)) {
            let Node::BlockCall(call, _) = lhs.into_node() else {
                unreachable!()
            };
            lhs = call.into_inner();
        }
        // Go counts the block literal as a node below its call.
        let mut params = 0;
        for target in block.params.iter() {
            params = params.max(target.depth(self.work)?);
        }
        let body = block.body.iter().map(|s| s.depth).max().unwrap_or(0);
        let depth = 1 + lhs.depth.max(1 + body.max(params));
        let p = self.p();
        p.suffix_call(&lhs)?;
        p.make_at(
            Node::BlockCall(Boxed::new(p.work, lhs)?, block),
            depth,
            offset,
        )
    }

    /// Refuses braces after a call that hold a hash literal, `puts { a: 1 }`,
    /// rather than a block's statements. A `{` after a call starts its
    /// block, so a hash argument needs the call's parentheses; the fix adds
    /// them when the call has no other arguments.
    async fn hash_block(&self, lhs: &Expr) -> Result<()> {
        // Only a call without arguments takes the hash as its first.
        let (name, bare) = match &lhs.node {
            Node::Var(name) | Node::Member(_, name) | Node::SafeMember(_, name) => (name, true),
            Node::Call(name, ..) | Node::Method(_, name, ..) | Node::SafeMethod(_, name, ..) => {
                (name, false)
            }
            _ => return Ok(()),
        };
        let open = {
            let mut p = self.p();
            let open = p.pos;
            let first = p.significant(open + 1);
            let key = match &p.tokens[first].token {
                Token::Word(word) if !word.starts_with('@') => {
                    p.pos = first;
                    let typed = p.typed_local_ahead();
                    p.pos = open;
                    !typed?
                }
                Token::Bytes(_) | Token::QuotedSymbol(_) => true,
                _ => false,
            };
            // A key is never the last token, which is the end of input.
            if !key
                || p.tokens[first + 1].token != Token::P(':')
                || p.tokens[first + 1].offset != p.tokens[first].end
            {
                return Ok(());
            }
            open
        };
        // Read the braces as the hash they hold, to find where it ends.
        self.p().pos = open + 1;
        let end = match self.hash_expr().await {
            Ok(_) => Some(self.p().pos),
            Err(error) if error.kind == crate::ErrorKind::Syntax => None,
            Err(error) => return Err(error),
        };
        let p = self.p();
        let brace = p.tokens[open].offset;
        let message = format!(
            "a hash literal passed to `{name}` needs parentheses, as in `{name}({{ ... }})`; after a call, `{{` starts a block"
        );
        let close = end.map(|end| p.tokens[end - 1].end);
        let span = crate::diagnostic::Span::new(brace, close.unwrap_or(brace + 1));
        let mut diagnostic = crate::diagnostic::Diagnostic::error(
            crate::diagnostic::Code::HASH_ARGUMENT,
            span,
            message.clone(),
        );
        let ends_call = |index: usize| match &p.tokens[index].token {
            Token::EndLine | Token::Eof | Token::P('}' | ')' | ']') => true,
            Token::Word(word) => matches!(word.as_str(), "if" | "unless" | "while" | "until"),
            _ => false,
        };
        if let (Some(end), Some(close), true) = (end, close, bare)
            && ends_call(end)
        {
            diagnostic = diagnostic.with_fix(crate::diagnostic::Fix::edits(
                "pass the hash in parentheses",
                vec![
                    crate::diagnostic::Edit {
                        span: crate::diagnostic::Span::new(p.tokens[open - 1].end, brace),
                        replacement: "(".to_owned(),
                    },
                    crate::diagnostic::Edit {
                        span: crate::diagnostic::Span::at(close),
                        replacement: ")".to_owned(),
                    },
                ],
            ));
        }
        Err(Error::syntax(p.work, brace, message).with_diagnostic(diagnostic))
    }

    async fn attached_block(&self, brace: bool) -> Result<Block> {
        let _scope = self.recovery_scope()?;
        let (offset, outer, outer_it) = {
            let mut p = self.p();
            p.work.charge(1)?;
            let offset = p.tokens[p.pos].offset as u32;
            p.bump()?;
            p.line_breaks()?;
            (offset, p.locals.copy(p.work)?, p.declared_it)
        };
        let infer_it = !outer_it;
        let (params, explicit) = self.block_parameters().await?;
        let (previous_loop, previous_then, command_depth) = {
            let mut p = self.p();
            (
                p.loop_condition.take(),
                p.then_stop.take(),
                std::mem::replace(&mut p.command_depth, 0),
            )
        };
        let body = self.block(if brace { &["}"] } else { &["end"] }).await?;
        let mut p = self.p();
        p.command_depth = command_depth;
        p.then_stop = previous_then;
        p.loop_condition = previous_loop;
        if brace {
            if !p.take_p('}') {
                return p.expected(Label::Text("}"));
            }
        } else {
            p.expect_word("end")?;
        }
        p.locals = outer;
        p.declared_it = outer_it;
        Ok(Block {
            offset,
            params,
            body,
            implicit: !explicit,
            infer_it,
        })
    }

    /// Parses a block's parameter list, as Go's `parseBlockParameters` does.
    async fn block_parameters(&self) -> Result<(Buffer<Target>, bool)> {
        let work = self.p().work;
        let mut params = Buffer::new();
        let explicit = if self.p().token() == &Token::Op("||") {
            self.p().bump()?;
            true
        } else if self.p().take_p('|') {
            self.p().line_breaks()?;
            if !self.p().take_p('|') {
                loop {
                    let target = self.block_parameter().await?;
                    let mut p = self.p();
                    p.declare_block_parameter(&target, &params)?;
                    params.push(work, target)?;
                    let comma = p.significant(p.pos);
                    if p.tokens[comma].token != Token::P(',') {
                        break;
                    }
                    p.pos = comma + 1;
                    p.line_breaks()?;
                    if p.token() == &Token::P('|') {
                        return p.err("trailing comma in block parameter list");
                    }
                }
                let mut p = self.p();
                p.line_breaks()?;
                if !p.take_p('|') {
                    return p.expected(Label::Char('|'));
                }
            }
            true
        } else {
            false
        };
        if !explicit {
            let mut p = self.p();
            p.locals.insert(work, Name::new(work, "it")?, 0)?;
            for n in ["_1", "_2", "_3", "_4", "_5", "_6", "_7", "_8", "_9"] {
                p.locals.insert(work, Name::new(work, n)?, 0)?;
            }
        }
        Ok((params, explicit))
    }

    /// Parses one block parameter, as Go's `parseBlockParameter` does.
    async fn block_parameter(&self) -> Result<Target> {
        let work = self.p().work;
        let (close, open) = {
            let mut p = self.p();
            let open = p.tokens[p.pos].offset;
            let close = match p.token() {
                Token::P('(') => ')',
                Token::P('[') => ']',
                _ if p.ident(p.pos) => {
                    let name = p.name()?;
                    let target = Target::Value(p.make(Node::Var(name), 1)?);
                    let colon = p.significant(p.pos);
                    if p.tokens[colon].token != Token::P(':') {
                        return Ok(target);
                    }
                    p.pos = colon + 1;
                    p.line_breaks()?;
                    return Ok(Target::Typed(
                        Boxed::new(work, target)?,
                        p.type_expr(0, true)?,
                    ));
                }
                _ => return p.expected(Label::Text("block parameter")),
            };
            p.enter()?;
            p.bump()?;
            let next = p.significant(p.pos);
            if p.tokens[next].token == Token::P(close) {
                p.pos = next;
                return p.expected(Label::Text("destructuring assignment target"));
            }
            p.line_breaks()?;
            (close, open)
        };
        let (target, tuple) = self.nested_target(true).await?;
        let mut p = self.p();
        p.line_breaks()?;
        p.expect_p(close)?;
        p.depth -= 1;
        let target = if tuple {
            target
        } else {
            Target::Tuple(Buffer::from_array(work, [(Some(target), false)])?)
        };
        if !target.is_binding(work)? {
            return Err(Error::syntax(
                work,
                open,
                "invalid block parameter destructuring target",
            ));
        }
        Ok(target)
    }
}

impl Parser<'_> {
    /// Whether the `{` at the current token starts `lhs`'s block: it follows
    /// a `)`, or `lhs` is a call.
    pub(super) fn block_follows(&self, lhs: &Expr) -> Result<bool> {
        Ok(self.tokens[self.pos - 1].token == Token::P(')') || self.block_owner(lhs)?)
    }
    /// Whether `lhs` is a call that a `{` after it gives a block: a function
    /// or method name or a call with arguments, and not a value such as a
    /// local, a constant or a literal. After a value, the brace is left for
    /// the parenless call whose last argument the value is.
    fn block_owner(&self, lhs: &Expr) -> Result<bool> {
        Ok(match &lhs.node {
            Node::Var(name) => {
                !(name == "self"
                    || name.starts_with('@')
                    || name.chars().next().is_some_and(unicode::upper)
                    || self.locals.contains(self.work, name)?)
            }
            Node::Scope(_, name, args) => {
                args.is_some() || !name.chars().next().is_some_and(unicode::upper)
            }
            Node::Call(..)
            | Node::Method(..)
            | Node::SafeMethod(..)
            | Node::Member(..)
            | Node::SafeMember(..)
            | Node::ComputedCall(..) => true,
            _ => false,
        })
    }
    pub(super) fn can_attach_do(&self) -> bool {
        (self.command_depth == 0 || self.groups > self.command_group)
            && self.loop_condition.is_none_or(|group| self.groups > group)
    }
}
