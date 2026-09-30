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

impl<'x> Compiling<'_, 'x> {
    /// Compiles a `begin` expression. `target` is the argument count of a call
    /// that uses each branch's result as its target.
    pub(super) async fn attempt(
        &self,
        attempt: &'x syntax::Try,
        target: Option<usize>,
    ) -> Result<()> {
        let (index, mut spec) = {
            let mut c = self.c();
            c.work.charge(1)?;
            let index = c.program.handlers.len();
            c.program.handlers.push(TrySpec::default());
            c.emit(Op::TryBegin(narrow(index)));
            let (body_locals, held) = c.statement_bindings(&attempt.body)?.into_parts();
            crate::budget::Charge::merge(&mut c.handler_locals, held);
            let spec = TrySpec {
                body: c.code.len(),
                body_locals,
                ..TrySpec::default()
            };
            (index, spec)
        };
        self.attempt_block(&attempt.body, target).await?;
        self.c().emit(Op::TryBody);
        for clause in &attempt.rescues {
            let (saved_offset, previous, parameter, binding, locals, start) = {
                let mut c = self.c();
                let work = c.work;
                let saved_offset = std::mem::replace(&mut c.offset, clause.offset);
                let (previous, parameter, binding) = if let Some(name) = &clause.binding {
                    let previous = c.locals.get(work, name)?.copied();
                    let parameter = c.parameters.contains(work, name)?;
                    let slot = c.rescue_slot(name)?;
                    c.locals.insert(work, name.clone(), slot)?;
                    c.parameters.insert(work, name.clone(), ())?;
                    (previous, parameter, Some(slot))
                } else {
                    (None, false, None)
                };
                let (bindings, held) = c.statement_bindings(&clause.body)?.into_parts();
                crate::budget::Charge::merge(&mut c.handler_locals, held);
                let locals = bindings
                    .into_iter()
                    .filter(|slot| Some(*slot) != binding)
                    .collect();
                let start = c.code.len();
                (saved_offset, previous, parameter, binding, locals, start)
            };
            self.attempt_block(&clause.body, target).await?;
            let mut c = self.c();
            let work = c.work;
            c.emit(Op::TryEnd);
            if let Some(name) = &clause.binding {
                if let Some(slot) = previous {
                    c.locals.insert(work, name.clone(), slot)?;
                } else {
                    c.locals.remove(work, name.as_str())?;
                }
                if !parameter {
                    c.parameters.remove(work, name.as_str())?;
                }
            }
            c.offset = saved_offset;
            spec.rescues.push(RescueSpec {
                classes: clause.classes.to_vec(),
                binding,
                locals,
                start,
                empty: clause.body.is_empty(),
            });
        }
        {
            let mut c = self.c();
            let (alternate_locals, held) = c.statement_bindings(&attempt.alternate)?.into_parts();
            crate::budget::Charge::merge(&mut c.handler_locals, held);
            spec.alternate_locals = alternate_locals;
            if !attempt.alternate.is_empty() {
                spec.alternate = Some(c.code.len());
            }
        }
        if !attempt.alternate.is_empty() {
            self.block(&attempt.alternate).await?;
            self.c().emit(Op::TryEnd);
        }
        if !attempt.ensure.is_empty() {
            spec.ensure = Some(self.c().code.len());
            self.block(&attempt.ensure).await?;
            let mut c = self.c();
            c.emit(Op::Pop);
            c.emit(Op::EnsureEnd);
        }
        let mut c = self.c();
        spec.end = c.code.len();
        c.program.handlers[index] = spec;
        Ok(())
    }

    async fn attempt_block(&self, body: &'x [Stmt], target: Option<usize>) -> Result<()> {
        self.c().work.charge(1)?;
        if let Some(arguments) = target {
            let [
                Stmt {
                    node: Statement::Expr(expr),
                    ..
                },
            ] = body
            else {
                unreachable!()
            };
            self.call_target(expr, arguments).await?;
            self.c().emit(Op::Nil);
            Ok(())
        } else {
            self.block(body).await
        }
    }

    pub(super) async fn raise(
        &self,
        value: Option<&'x Expr>,
        message: Option<&'x Expr>,
    ) -> Result<()> {
        self.c().work.charge(1)?;
        if let Some(message) = message {
            let value = value.unwrap();
            let start = {
                let mut c = self.c();
                let named = if let Node::Var(name) = &value.node {
                    if name.chars().next().is_some_and(syntax::unicode::upper)
                        && crate::ErrorClass::from_name(name).is_some()
                    {
                        Some((
                            c.call_site(name, false).name as usize,
                            c.locals.get(c.work, name.as_str())?.copied(),
                        ))
                    } else {
                        None
                    }
                } else {
                    None
                };
                let named = named.map(|named| {
                    c.program.raises.push(named);
                    narrow(c.program.raises.len() - 1)
                });
                c.emit(Op::RaiseStart(named, 0))
            };
            self.expr(value).await?;
            {
                let mut c = self.c();
                c.emit(Op::RaiseValue);
                let end = c.code.len();
                c.patch(start, end);
            }
            self.expr(message).await?;
            self.c().emit(Op::Raise(2));
        } else if let Some(value) = value {
            self.expr(value).await?;
            self.c().emit(Op::Raise(1));
        } else {
            self.c().emit(Op::Raise(0));
        }
        Ok(())
    }
}

impl Compiler<'_> {
    fn rescue_slot(&mut self, name: &Name) -> Result<usize> {
        let mut storage = [0; 20];
        let digits = decimal_digits(self.slots as u64, &mut storage);
        let name = Name::join(self.work, &["\0rescue", digits, ":", name])?;
        self.slot(&name)
    }
}
