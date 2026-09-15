use super::*;

#[derive(Debug, Default)]
pub(crate) struct TrySpec {
    pub body: usize,
    pub body_locals: Vec<usize>,
    pub rescues: Vec<RescueSpec>,
    pub alternate: Option<usize>,
    pub alternate_locals: Vec<usize>,
    pub ensure: Option<usize>,
    pub end: usize,
}

#[derive(Debug)]
pub(crate) struct RescueSpec {
    pub classes: Vec<crate::ErrorClass>,
    pub binding: Option<usize>,
    pub locals: Vec<usize>,
    pub start: usize,
    pub empty: bool,
}

impl Compiler<'_> {
    pub(super) fn attempt(&mut self, attempt: &syntax::Try, target: bool) -> Result<()> {
        let index = self.program.handlers.len();
        self.program.handlers.push(TrySpec::default());
        self.emit(Op::TryBegin(index));
        let mut spec = TrySpec {
            body: self.code.len(),
            body_locals: self.statement_bindings(&attempt.body),
            ..TrySpec::default()
        };
        self.attempt_block(&attempt.body, target)?;
        self.emit(Op::TryBody);
        for clause in &attempt.rescues {
            let saved_offset = std::mem::replace(&mut self.offset, clause.offset);
            let previous = clause
                .binding
                .as_ref()
                .and_then(|name| self.locals.get(name).copied());
            let parameter = clause
                .binding
                .as_ref()
                .is_some_and(|name| self.parameters.contains(name));
            let binding = clause.binding.as_ref().map(|name| {
                let slot = self.slot(&format!("\0rescue{}:{name}", self.slots));
                self.locals.insert(name.clone(), slot);
                self.parameters.insert(name.clone());
                slot
            });
            let locals = self
                .statement_bindings(&clause.body)
                .into_iter()
                .filter(|slot| Some(*slot) != binding)
                .collect();
            let start = self.code.len();
            self.attempt_block(&clause.body, target)?;
            self.emit(Op::TryEnd);
            if let Some(name) = &clause.binding {
                if let Some(slot) = previous {
                    self.locals.insert(name.clone(), slot);
                } else {
                    self.locals.remove(name);
                }
                if !parameter {
                    self.parameters.remove(name);
                }
            }
            self.offset = saved_offset;
            spec.rescues.push(RescueSpec {
                classes: clause.classes.clone(),
                binding,
                locals,
                start,
                empty: clause.body.is_empty(),
            });
        }
        spec.alternate_locals = self.statement_bindings(&attempt.alternate);
        if !attempt.alternate.is_empty() {
            spec.alternate = Some(self.code.len());
            self.block(&attempt.alternate)?;
            self.emit(Op::TryEnd);
        }
        if !attempt.ensure.is_empty() {
            spec.ensure = Some(self.code.len());
            self.block(&attempt.ensure)?;
            self.emit(Op::Pop);
            self.emit(Op::EnsureEnd);
        }
        spec.end = self.code.len();
        self.program.handlers[index] = spec;
        Ok(())
    }

    fn attempt_block(&mut self, body: &[Stmt], target: bool) -> Result<()> {
        if target {
            let [
                Stmt {
                    node: Statement::Expr(expr),
                    ..
                },
            ] = body
            else {
                unreachable!()
            };
            self.call_target(expr)?;
            self.emit(Op::Nil);
            Ok(())
        } else {
            self.block(body)
        }
    }

    pub(super) fn raise(&mut self, value: Option<&Expr>, message: Option<&Expr>) -> Result<()> {
        if let Some(message) = message {
            let value = value.unwrap();
            let named = if let Node::Var(name) = &value.node {
                if name.chars().next().is_some_and(syntax::unicode::upper)
                    && crate::ErrorClass::from_name(name).is_some()
                {
                    Some((
                        self.call_site(name, false).name,
                        self.locals.get(name).copied(),
                    ))
                } else {
                    None
                }
            } else {
                None
            };
            let start = self.emit(Op::RaiseStart(named, 0));
            self.expr(value)?;
            self.emit(Op::RaiseValue);
            self.patch(start, self.code.len());
            self.expr(message)?;
            self.emit(Op::Raise(2));
        } else if let Some(value) = value {
            self.expr(value)?;
            self.emit(Op::Raise(1));
        } else {
            self.emit(Op::Raise(0));
        }
        Ok(())
    }
}
