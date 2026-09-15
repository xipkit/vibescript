use super::*;

impl Compiler<'_> {
    pub(super) fn named_call(&mut self, name: &str, args: &[Argument]) -> Result<()> {
        self.global(name);
        if let Some(&slot) = self.locals.get(name) {
            let name = self.call_site(name, false).name;
            self.emit(Op::ResolveCall(slot, name));
            self.argument_values(args)?;
            self.emit(Op::Invoke(Invocation::Resolved));
            return Ok(());
        }
        let target = if self.program.declaration_names.contains_key(name) {
            Invocation::NonCallable
        } else if let Some(&fun) = self.program.names.get(name) {
            Invocation::Function(fun)
        } else if let Some(host) = self.program.hosts.iter().position(|h| h == name) {
            Invocation::Host(host)
        } else if let Some(global) = self.global(name) {
            self.emit(Op::ResolveGlobalCall(global));
            self.argument_values(args)?;
            self.emit(Op::Invoke(Invocation::Resolved));
            return Ok(());
        } else {
            let site = self.call_site(name, false);
            if self.namespace.is_some() {
                self.emit(Op::ResolveCall(usize::MAX, site.name));
                self.argument_values(args)?;
                self.emit(Op::Invoke(Invocation::Resolved));
            } else {
                self.emit(Op::Unbound(site.name));
            }
            return Ok(());
        };
        if expanded(args) {
            self.call_arguments(args)?;
            self.emit(Op::Invoke(target));
        } else {
            for arg in args {
                self.expr(&arg.value)?;
            }
            self.emit(match target {
                Invocation::Function(fun) => Op::Call(fun, args.len()),
                Invocation::Host(host) => Op::Host(host, args.len()),
                Invocation::NonCallable => Op::NonCallable,
                _ => unreachable!(),
            });
        }
        Ok(())
    }

    pub(super) fn computed_call(
        &mut self,
        call: &Expr,
        args: &[Argument],
        block: Option<usize>,
    ) -> Result<()> {
        self.emit(Op::Arguments);
        self.call_target(call)?;
        self.argument_values(args)?;
        if let Some(block) = block {
            self.emit(Op::Attach(block));
        }
        self.emit(Op::Invoke(Invocation::Resolved));
        Ok(())
    }

    pub(super) fn call_target(&mut self, expr: &Expr) -> Result<()> {
        let previous = std::mem::replace(&mut self.offset, expr.offset);
        match &expr.node {
            Node::Try(attempt) if attempt.modifier => {
                self.attempt(attempt, true)?;
                self.emit(Op::Pop);
            }
            Node::Var(name)
                if !name.starts_with('@') && !matches!(name.as_str(), "self" | "block_given?") =>
            {
                self.global(name);
                let slot = self.locals.get(name).copied().unwrap_or(usize::MAX);
                let name = self.call_site(name, false).name;
                self.emit(Op::CallName(slot, name));
            }
            Node::Member(receiver, name) | Node::SafeMember(receiver, name) => {
                self.member_receiver(receiver, name != "call")?;
                let skip =
                    matches!(expr.node, Node::SafeMember(..)).then(|| self.emit(Op::JumpNil(0)));
                let site = self.call_site(name, false);
                self.emit(Op::CallMember(site));
                if let Some(skip) = skip {
                    let done = self.emit(Op::Jump(0));
                    self.patch(skip, self.code.len());
                    self.emit(Op::CallValue);
                    self.patch(done, self.code.len());
                }
            }
            Node::Scope(receiver, name, None) => {
                self.expr(receiver)?;
                let mut site = self.call_site(name, false);
                site.scope = true;
                self.emit(Op::CallMember(site));
            }
            Node::Shape(ty, Some(fallback), names) => {
                let index = self.program.type_guards.len();
                self.program.type_guards.push(names.clone());
                let guard = self.emit(Op::TypeShadowed(index, 0));
                self.constant(crate::shapes::compile((**ty).clone()));
                self.emit(Op::CallValue);
                let done = self.emit(Op::Jump(0));
                self.patch(guard, self.code.len());
                self.call_target(fallback)?;
                self.patch(done, self.code.len());
            }
            _ => {
                self.expr(expr)?;
                self.emit(Op::CallValue);
            }
        }
        self.offset = previous;
        Ok(())
    }
}
