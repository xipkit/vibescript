use super::*;

impl<M: super::recovery::Mode> Parsing<'_, M> {
    pub(super) async fn command_expression(&self, lhs: Expr) -> Result<Expr> {
        let group = {
            let mut p = self.p();
            p.work.charge(1)?;
            p.command_depth += 1;
            if p.command_depth > 64 {
                return p.err("parenless call nesting too deep");
            }
            let groups = p.groups;
            let group = std::mem::replace(&mut p.command_group, groups);
            if matches!(p.token(), Token::Op("/" | "//")) {
                p.expand_regex()?;
            }
            group
        };
        let args = self.command_arguments().await?;
        let mut p = self.p();
        p.command_group = group;
        p.command_depth -= 1;
        let depth = 1 + call_depth(&lhs).max(args.iter().map(|a| a.value.depth).max().unwrap_or(0));
        let offset = lhs.offset;
        p.suffix_call(&lhs)?;
        let node = match lhs.into_node() {
            Node::Var(name) => Node::Call(name, args, CallForm::Bare),
            Node::Member(receiver, name) => Node::Method(receiver, name, args, CallForm::Bare),
            Node::SafeMember(receiver, name) => {
                Node::SafeMethod(receiver, name, args, CallForm::Bare)
            }
            Node::Scope(receiver, name, None) => Node::Scope(receiver, name, Some(args)),
            _ => unreachable!(),
        };
        p.make_at(node, depth, offset)
    }

    pub(super) async fn yield_expr(&self) -> Result<Expr> {
        let work = self.p().work;
        let (line, parenthesized) = {
            let mut p = self.p();
            work.charge(1)?;
            let line = p.previous()?.line;
            let mut next = p.pos;
            while p.tokens[next].token == Token::EndLine
                && p.tokens[next].line != p.tokens[next].end_line
            {
                work.charge(1)?;
                next += 1;
            }
            if p.tokens[next].token == Token::P('(') {
                p.pos = next;
            }
            (line, p.take_p('('))
        };
        let args = if parenthesized {
            self.arguments(')', true).await?
        } else {
            let mut args = Buffer::new();
            let starts = {
                let p = self.p();
                p.tokens[p.pos].line == line && p.starts_expression()
            };
            if starts {
                args.push(work, self.line_expr(0).await?)?;
                loop {
                    {
                        let mut p = self.p();
                        if !(p.token() == &Token::P(',')
                            && p.tokens[p.pos].line == line
                            && p.tokens[p.pos + 1].line == line)
                        {
                            break;
                        }
                        p.bump()?;
                    }
                    args.push(work, self.line_expr(0).await?)?;
                }
            }
            args
        };
        let depth = 1 + args.iter().map(|arg| arg.depth).max().unwrap_or(0);
        self.p().make(Node::Yield(args), depth)
    }

    async fn command_arguments(&self) -> Result<Buffer<Argument>> {
        let work = self.p().work;
        work.charge(1)?;
        let mut args = Buffer::new();
        let mut keywords = false;
        let start = self.p().pos;
        let mut hash = None;
        loop {
            if keywords {
                self.p().keyword_order(
                    "positional arguments cannot follow bare keyword arguments in parenless calls",
                )?;
            }
            let argument = self.call_argument(false, false).await?;
            let mut p = self.p();
            keywords |= matches!(
                argument.kind,
                ArgumentKind::Keyword(_) | ArgumentKind::KeywordSplat
            );
            args.push(work, argument)?;
            let last = p.previous()?;
            if p.token() != &Token::P(',')
                || p.tokens[p.pos].line != last.line
                || p.tokens[p.pos + 1].line != last.line
            {
                break;
            }
            // Read a hash literal after a comma, to refuse it with a fix.
            let brace = p.tokens[p.pos + 1].token == Token::P('{');
            if !brace && !p.command_argument_start(p.pos + 1, true) {
                break;
            }
            if brace {
                hash.get_or_insert(p.tokens[p.pos + 1].offset);
            }
            p.bump()?;
        }
        if let Some(brace) = hash {
            return Err(self.p().hash_argument(start, brace));
        }
        Ok(args)
    }

    pub(super) async fn call_arguments(&self) -> Result<Buffer<Argument>> {
        let work = self.p().work;
        work.charge(1)?;
        let mut args = Buffer::new();
        let mut keywords = false;
        let types = std::mem::take(&mut self.p().type_call);
        {
            let mut p = self.p();
            p.groups += 1;
            p.line_breaks()?;
            if p.take_p(')') {
                p.groups -= 1;
                return Ok(args);
            }
        }
        loop {
            if keywords {
                self.p()
                    .keyword_order("positional arguments cannot follow keyword arguments")?;
            }
            let argument = self.call_argument(true, types).await?;
            let mut p = self.p();
            keywords |= matches!(
                argument.kind,
                ArgumentKind::Keyword(_) | ArgumentKind::KeywordSplat
            );
            args.push(work, argument)?;
            p.line_breaks()?;
            if p.take_p(')') {
                break;
            }
            if p.token() != &Token::P(',') {
                return p.expected(Label::Char(')'));
            }
            p.expect_p(',')?;
            p.line_breaks()?;
            if p.take_p(')') {
                break;
            }
        }
        let mut p = self.p();
        p.groups -= 1;
        p.call_end = p.pos;
        Ok(args)
    }

    /// Parses one argument; one of a call that takes `types` may be a tuple
    /// type.
    async fn call_argument(&self, parenthesized: bool, types: bool) -> Result<Argument> {
        let ampersand = {
            let mut p = self.p();
            let offset = p.tokens[p.pos].offset;
            (p.token() == &Token::Op("&")).then(|| {
                p.pos += 1;
                offset
            })
        };
        if let Some(offset) = ampersand {
            // Go parses the operand before refusing the removed block argument.
            self.expr(0).await?;
            return Err(Error::syntax(
                self.p().work,
                offset,
                "block arguments are not supported; a block is not a value. Write the block at \
                 the call that runs it, as in `words.map { |word| word.upcase }`",
            ));
        }
        let (kind, literal) = {
            let mut p = self.p();
            p.work.charge(1)?;
            let kind = p.argument_kind()?;
            if parenthesized || matches!(kind, ArgumentKind::Splat | ArgumentKind::KeywordSplat) {
                p.line_breaks()?;
            }
            let literal = p.literal_argument(&kind, parenthesized, types)?;
            p.type_argument = types
                && parenthesized
                && matches!(kind, ArgumentKind::Positional)
                && p.token() == &Token::P('{');
            (kind, literal)
        };
        let value = match literal {
            Some(value) => value,
            None => self.expr(0).await?,
        };
        Ok(Argument { kind, value })
    }
}

impl Parser<'_> {
    pub(super) fn parenthesized_call(&mut self, lhs: Expr, args: Buffer<Argument>) -> Result<Expr> {
        self.work.charge(1)?;
        let origin = lhs.offset;
        let argument_depth = args.iter().map(|a| a.value.depth).max().unwrap_or(0);
        let d = 1 + call_depth(&lhs).max(argument_depth);
        if !matches!(
            lhs.node,
            Node::Var(_) | Node::Member(..) | Node::SafeMember(..) | Node::Scope(_, _, None)
        ) {
            let node = Node::ComputedCall(Boxed::new(self.work, lhs)?, args);
            return self.make_at(node, d, origin);
        }
        self.suffix_call(&lhs)?;
        let node = match lhs.into_node() {
            Node::Var(name) => Node::Call(name, args, CallForm::Parenthesized),
            Node::Member(receiver, name) => {
                Node::Method(receiver, name, args, CallForm::Parenthesized)
            }
            Node::SafeMember(receiver, name) => {
                Node::SafeMethod(receiver, name, args, CallForm::Parenthesized)
            }
            Node::Scope(receiver, name, None) => Node::Scope(receiver, name, Some(args)),
            _ => unreachable!(),
        };
        self.make_at(node, d, origin)
    }

    /// Refuses a hash literal, at offset `brace`, among the arguments of a
    /// parenless call whose first argument starts at token `start`. The fix
    /// gives the call parentheses; without them a hash first after the call
    /// would read as a block.
    fn hash_argument(&self, start: usize, brace: usize) -> Error {
        let callee = &self.tokens[start - 1];
        let name = match &callee.token {
            Token::Word(name) => name.as_str(),
            _ => "the call",
        };
        let message = format!(
            "a hash literal passed to `{name}` needs parentheses, as in `{name}(..., {{ ... }})`; after a call, `{{` starts a block"
        );
        let span = crate::diagnostic::Span::new(brace, brace + 1);
        let edits = vec![
            crate::diagnostic::Edit {
                span: crate::diagnostic::Span::new(callee.end, self.tokens[start].offset),
                replacement: "(".to_owned(),
            },
            crate::diagnostic::Edit {
                span: crate::diagnostic::Span::at(self.previous().map_or(brace, |last| last.end)),
                replacement: ")".to_owned(),
            },
        ];
        let diagnostic = crate::diagnostic::Diagnostic::error(
            crate::diagnostic::Code::HASH_ARGUMENT,
            span,
            &message,
        )
        .with_fix(crate::diagnostic::Fix::edits(
            "give the call parentheses",
            edits,
        ));
        Error::syntax(self.work, brace, message).with_diagnostic(diagnostic)
    }

    /// Refuses an argument after keywords unless Go lets it follow them: a
    /// keyword, a keyword splat or a block argument.
    fn keyword_order(&self, message: &str) -> Result<()> {
        if matches!(self.token(), Token::Op("**" | "&")) || self.keyword_label(self.pos) {
            return Ok(());
        }
        self.err(message)
    }

    pub(super) fn keyword_label(&self, pos: usize) -> bool {
        matches!(&self.tokens[pos].token, Token::Word(word) if !word.starts_with('@'))
            && self
                .tokens
                .get(pos + 1)
                .is_some_and(|t| t.token == Token::P(':'))
    }

    pub(super) fn command_start(&self, lhs: &Expr, min: u8) -> Result<bool> {
        self.work.charge(1)?;
        if self.line_exprs == 0
            || min > 14
            || !matches!(
                lhs.node,
                Node::Var(_) | Node::Member(..) | Node::SafeMember(..) | Node::Scope(_, _, None)
            )
        {
            return Ok(false);
        }
        let local = match &lhs.node {
            // Like Go, only an identifier or member can take arguments.
            Node::Var(name) if name == "self" || name.starts_with('@') => return Ok(false),
            Node::Var(name) => self.locals.contains(self.work, name)?,
            // A scoped function takes arguments, as in `Math::sqrt 9`; a
            // constant, nested type or enum member takes none.
            Node::Scope(_, name, _) if name.chars().next().is_some_and(unicode::upper) => {
                return Ok(false);
            }
            _ => false,
        };
        let previous = self.previous()?;
        let next = &self.tokens[self.pos];
        if next.line != previous.end_line {
            return Ok(false);
        }
        if self.keyword_label(self.pos) {
            return Ok(true);
        }
        Ok(match next.token {
            Token::P('[') => !local && previous.end != next.offset,
            // Only a declared local makes `%w` a modulo. An implicit block `it`
            // still calls a function named `it`, as in Go.
            Token::Words(..) => {
                let implicit =
                    matches!(&lhs.node, Node::Var(name) if name == "it") && !self.declared_it;
                (!local || implicit) && previous.end != next.offset
            }
            Token::Regex(..) => !local && previous.end != next.offset,
            // A command's argument may start with a regex, such as `puts /x/`,
            // which the lexer read as division. Floor division needs an
            // operand, so `puts //` at the end of a line passes an empty regex.
            Token::Op(op @ ("/" | "//")) => {
                !local
                    && previous.end != next.offset
                    && (self
                        .source
                        .as_bytes()
                        .get(next.end)
                        .is_some_and(|byte| !matches!(byte, b' ' | b'\t' | b'\r' | b'\n'))
                        || (op == "//"
                            && matches!(
                                self.tokens[self.pos + 1].token,
                                Token::EndLine | Token::Eof
                            )))
            }
            Token::Op(op @ ("*" | "**" | "&")) => {
                // Go v0.70.0 locates a power token at its second star.
                let start = next.offset + usize::from(op == "**");
                !local
                    && previous.end != start
                    && self.tokens.get(self.pos + 1).is_some_and(|t| {
                        !matches!(t.token, Token::EndLine | Token::Eof)
                            && t.line == next.end_line
                            && t.offset == next.end
                    })
            }
            _ => self.command_argument_start(self.pos, false),
        })
    }

    fn command_argument_start(&self, pos: usize, after_comma: bool) -> bool {
        if self.keyword_label(pos) {
            return true;
        }
        match &self.tokens[pos].token {
            Token::Word(w) if w == "then" => self.then_stop != Some(self.groups),
            Token::Word(w) => {
                !reserved(w) || matches!(w.as_str(), "case" | "for" | "yield" | "begin")
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
            Token::Op("!") => true,
            Token::P('[') | Token::Op("*" | "**" | "&") => after_comma,
            _ => false,
        }
    }

    fn argument_kind(&mut self) -> Result<ArgumentKind> {
        Ok(if self.token() == &Token::Op("**") {
            self.bump()?;
            ArgumentKind::KeywordSplat
        } else if self.keyword_label(self.pos) {
            let Token::Word(name) = self.bump()? else {
                unreachable!()
            };
            self.bump()?;
            ArgumentKind::Keyword(Name::new(self.work, &name)?)
        } else if self.token() == &Token::Op("*") {
            self.bump()?;
            ArgumentKind::Splat
        } else {
            ArgumentKind::Positional
        })
    }

    fn literal_argument(
        &mut self,
        kind: &ArgumentKind,
        parenthesized: bool,
        types: bool,
    ) -> Result<Option<Expr>> {
        self.work.charge(1)?;
        if let ArgumentKind::Keyword(name) = kind {
            let shorthand = self.token() == &Token::P(',')
                || (parenthesized && self.token() == &Token::P(')'))
                || (!parenthesized
                    && (self.token() == &Token::Eof
                        || (self.token() == &Token::EndLine
                            && self.tokens[self.pos].line != self.tokens[self.pos].end_line)
                        || self.tokens[self.pos].line != self.tokens[self.pos - 1].end_line));
            if shorthand {
                return Ok(Some(self.make(Node::Var(name.clone()), 1)?));
            }
        } else if parenthesized && matches!(kind, ArgumentKind::Positional) {
            return self.argument_type_literal(types);
        }
        Ok(None)
    }
}

// Go nests a method's member access, or a function's name, below its call.
pub(super) fn call_depth(callee: &Expr) -> u32 {
    match &callee.node {
        Node::Var(_) => 1,
        Node::Member(receiver, _) | Node::SafeMember(receiver, _) => 1 + receiver.depth,
        Node::Scope(receiver, _, None) => 1 + receiver.depth,
        _ => callee.depth,
    }
}
