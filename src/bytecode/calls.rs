use super::*;

impl<'x> Compiling<'_, 'x> {
    /// Calls the function or host function `name`, as the expression `whole`.
    pub(super) async fn named_call(
        &self,
        whole: &'x Expr,
        name: &str,
        args: &'x [Argument],
    ) -> Result<()> {
        let target = {
            let mut c = self.c();
            let work = c.work;
            work.charge(1)?;
            c.global(name);
            if c.program.file || c.namespace.is_some() {
                let slot = c.locals.get(work, name)?.copied().map_or(NO_SLOT, narrow);
                let name = c.call_site(name, false).name;
                c.emit(Op::ResolveCall(slot, name));
                None
            } else if let Some(&slot) = c.locals.get(work, name)? {
                let name = c.call_site(name, false).name;
                c.emit(Op::ResolveCall(narrow(slot), name));
                None
            } else if c.program.declaration_names.contains_key(name) {
                Some(Invocation::NonCallable)
            } else if let Some(&fun) = c.program.names.get(name) {
                Some(Invocation::Function(narrow(fun)))
            } else if let Some(host) = c.host_position(name)? {
                Some(Invocation::Host(narrow(host)))
            } else if let Some(global) = c.global(name) {
                c.emit(Op::ResolveGlobalCall(narrow(global)));
                None
            } else {
                let site = c.call_site(name, false);
                c.emit(Op::ResolveCall(NO_SLOT, site.name));
                None
            }
        };
        let Some(target) = target else {
            self.argument_values(args).await?;
            let mut c = self.c();
            let ip = c.emit(Op::Invoke(Invocation::Resolved));
            let plain = c.facts.plain(whole);
            c.mark_plain(ip, plain);
            return Ok(());
        };
        {
            let mut c = self.c();
            let name = c.call_site(name, false).name;
            let listed = expanded(args, c.work)?;
            c.emit(Op::RootCall(name, listed));
        }
        let ip = if expanded(args, self.c().work)? {
            self.argument_values(args).await?;
            self.c().emit(Op::InvokeRoot(target))
        } else {
            for arg in args {
                self.expr(&arg.value).await?;
            }
            self.c().emit(match target {
                Invocation::Function(fun) => Op::Call(fun, narrow(args.len())),
                Invocation::Host(host) => Op::Host(host, narrow(args.len())),
                Invocation::NonCallable => Op::NonCallable(narrow(args.len())),
                _ => unreachable!(),
            })
        };
        let mut c = self.c();
        let plain = c.facts.plain(whole);
        c.mark_plain(ip, plain);
        Ok(())
    }

    pub(super) async fn computed_call(
        &self,
        call: &'x Expr,
        args: &'x [Argument],
        block: Option<usize>,
    ) -> Result<()> {
        {
            let mut c = self.c();
            c.work.charge(1)?;
            c.emit(Op::Arguments);
        }
        self.call_target(call, args.len()).await?;
        self.argument_values(args).await?;
        let mut c = self.c();
        if let Some(block) = block {
            c.emit(Op::Attach(narrow(block)));
        }
        c.emit(Op::Invoke(Invocation::Resolved));
        Ok(())
    }

    /// Compiles the target of a computed call passing `arguments` argument values.
    pub(super) async fn call_target(&self, expr: &'x Expr, arguments: usize) -> Result<()> {
        let previous = {
            let mut c = self.c();
            c.work.charge(1)?;
            std::mem::replace(&mut c.offset, expr.offset)
        };
        let result = self.call_target_at(expr, arguments).await;
        self.c().offset = previous;
        result
    }

    async fn call_target_at(&self, expr: &'x Expr, arguments: usize) -> Result<()> {
        match &expr.node {
            Node::Try(attempt) if attempt.modifier => {
                framed(self.work, self.attempt(attempt, Some(arguments)))?.await?;
                self.c().emit(Op::Pop);
            }
            Node::Var(name)
                if !name.starts_with('@') && !matches!(name.as_str(), "self" | "block_given?") =>
            {
                let mut c = self.c();
                c.global(name);
                let slot = c
                    .locals
                    .get(c.work, name.as_str())?
                    .copied()
                    .map_or(NO_SLOT, narrow);
                let name = c.call_site(name, false).name;
                c.emit(Op::CallName(slot, name));
            }
            Node::Member(receiver, name) | Node::SafeMember(receiver, name) => {
                let receiving = self.receiving(receiver, name, CallForm::Parenthesized, arguments);
                self.member_receiver(receiver, receiving).await?;
                let mut c = self.c();
                let skip =
                    matches!(expr.node, Node::SafeMember(..)).then(|| c.emit(Op::JumpNil(0)));
                let site = c.call_site(name, false);
                c.emit(Op::CallMember(site));
                if let Some(skip) = skip {
                    let done = c.emit(Op::Jump(0));
                    let end = c.code.len();
                    c.patch(skip, end);
                    c.emit(Op::CallValue);
                    let end = c.code.len();
                    c.patch(done, end);
                }
            }
            Node::Scope(receiver, name, None) => {
                self.expr(receiver).await?;
                let mut c = self.c();
                let mut site = c.call_site(name, false);
                site.scope = true;
                c.emit(Op::CallMember(site));
            }
            Node::Shape(ty, Some(fallback), names) => {
                let done = {
                    let mut c = self.c();
                    c.work.names(names)?;
                    let index = c.program.type_guards.len();
                    c.program
                        .type_guards
                        .push(names.iter().map(|name| name.as_str().to_owned()).collect());
                    let guard = c.emit(Op::TypeShadowed(narrow(index), 0));
                    let shape = crate::shapes::compile(ty.compile(c.work)?);
                    c.constant(shape);
                    c.emit(Op::CallValue);
                    let done = c.emit(Op::Jump(0));
                    let end = c.code.len();
                    c.patch(guard, end);
                    done
                };
                // A shape's fallback is a bare name, so this recursion stays shallow.
                framed(self.work, self.call_target(fallback, arguments))?.await?;
                let mut c = self.c();
                let end = c.code.len();
                c.patch(done, end);
            }
            _ => {
                self.expr(expr).await?;
                self.c().emit(Op::CallValue);
            }
        }
        Ok(())
    }
}
