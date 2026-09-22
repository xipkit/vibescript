use super::{
    Argument, ArgumentKind, Block, Definition, Expr, Node, Parameter, Rescue, Statement, Stmt,
    Target, Try, When,
};
use crate::{
    Result,
    compilation::{Boxed, Buffer, Name, Task, Tasks, Work},
};

// Alias declarations own separate syntax containers while sharing immutable literals.
pub(super) fn definition(work: &dyn Work, value: &Definition) -> Result<Definition> {
    let copying = Copying {
        work,
        tasks: Tasks::new(),
    };
    match copying
        .tasks
        .run(Call::Definition(value), |call| copying.start(call))?
    {
        Copied::Definition(definition) => Ok(definition),
        _ => unreachable!(),
    }
}

// Copies nest as deeply as the syntax they copy, so they run as tasks.
#[derive(Clone, Copy)]
enum Call<'x> {
    Definition(&'x Definition),
    Expr(&'x Expr),
    Stmt(&'x Stmt),
    Target(&'x Target),
}

enum Copied {
    Definition(Definition),
    Expr(Expr),
    Stmt(Stmt),
    Target(Target),
}

struct Copying<'w, 'x> {
    work: &'w dyn Work,
    tasks: Tasks<Call<'x>, Copied>,
}

impl<'x> Copying<'_, 'x> {
    fn start(&self, call: Call<'x>) -> Task<'_, Copied> {
        match call {
            Call::Definition(value) => {
                Box::pin(async move { Ok(Copied::Definition(self.definition(value).await?)) })
            }
            Call::Expr(value) => {
                Box::pin(async move { Ok(Copied::Expr(self.expression(value).await?)) })
            }
            Call::Stmt(value) => {
                Box::pin(async move { Ok(Copied::Stmt(self.statement(value).await?)) })
            }
            Call::Target(value) => {
                Box::pin(async move { Ok(Copied::Target(self.target(value).await?)) })
            }
        }
    }

    async fn expr(&self, value: &'x Expr) -> Result<Expr> {
        match self.tasks.call(Call::Expr(value)).await? {
            Copied::Expr(value) => Ok(value),
            _ => unreachable!(),
        }
    }

    async fn stmt(&self, value: &'x Stmt) -> Result<Stmt> {
        match self.tasks.call(Call::Stmt(value)).await? {
            Copied::Stmt(value) => Ok(value),
            _ => unreachable!(),
        }
    }

    async fn place(&self, value: &'x Target) -> Result<Target> {
        match self.tasks.call(Call::Target(value)).await? {
            Copied::Target(value) => Ok(value),
            _ => unreachable!(),
        }
    }

    fn name(&self, name: &Name) -> Result<Name> {
        self.work.bytes(name.len())?;
        Ok(name.clone())
    }

    async fn definition(&self, value: &'x Definition) -> Result<Definition> {
        let work = self.work;
        let mut params = Buffer::with_capacity(work, value.params.len())?;
        for p in &value.params {
            work.charge(1)?;
            let default = match &p.default {
                Some(e) => Some(self.expr(e).await?),
                None => None,
            };
            params.push(
                work,
                Parameter {
                    ivar: p.ivar.as_ref().map(|n| self.name(n)).transpose()?,
                    name: self.name(&p.name)?,
                    kind: p.kind,
                    default,
                    ty: p.ty.as_ref().map(|ty| ty.copy(work)).transpose()?,
                },
            )?;
        }
        Ok(Definition {
            offset: value.offset,
            private: value.private,
            accessor: value
                .accessor
                .as_ref()
                .map(|(name, setter)| Ok((self.name(name)?, *setter)))
                .transpose()?,
            name: self.name(&value.name)?,
            params,
            body: self.body(&value.body).await?,
            return_type: value
                .return_type
                .as_ref()
                .map(|ty| ty.copy(work))
                .transpose()?,
        })
    }

    async fn body(&self, values: &'x Buffer<Stmt>) -> Result<Buffer<Stmt>> {
        let mut body = Buffer::with_capacity(self.work, values.len())?;
        for value in values {
            self.work.charge(1)?;
            body.push(self.work, self.stmt(value).await?)?;
        }
        Ok(body)
    }

    async fn exprs(&self, values: &'x Buffer<Expr>) -> Result<Buffer<Expr>> {
        let mut exprs = Buffer::with_capacity(self.work, values.len())?;
        for value in values {
            self.work.charge(1)?;
            exprs.push(self.work, self.expr(value).await?)?;
        }
        Ok(exprs)
    }

    async fn optional(&self, value: &'x Option<Expr>) -> Result<Option<Expr>> {
        Ok(match value {
            Some(e) => Some(self.expr(e).await?),
            None => None,
        })
    }

    async fn boxed(&self, value: &'x Expr) -> Result<Boxed<Expr>> {
        Boxed::new(self.work, self.expr(value).await?)
    }

    async fn optional_box(&self, value: &'x Option<Boxed<Expr>>) -> Result<Option<Boxed<Expr>>> {
        Ok(match value {
            Some(e) => Some(self.boxed(e).await?),
            None => None,
        })
    }

    async fn statement(&self, value: &'x Stmt) -> Result<Stmt> {
        let work = self.work;
        work.charge(1)?;
        let node = match &value.node {
            Statement::Retry => Statement::Retry,
            Statement::Module(name) => Statement::Module(self.name(name)?),
            Statement::UnboundClass(name) => Statement::UnboundClass(self.name(name)?),
            Statement::Raise(value, message) => Statement::Raise(
                self.optional_box(value).await?,
                self.optional_box(message).await?,
            ),
            Statement::Expr(value) => Statement::Expr(self.expr(value).await?),
            Statement::Assign(place, op, value) => {
                Statement::Assign(self.place(place).await?, op, self.expr(value).await?)
            }
            Statement::If(branches, alternate) => {
                let mut copies = Buffer::with_capacity(work, branches.len())?;
                for (condition, statements) in branches {
                    work.charge(1)?;
                    let condition = self.expr(condition).await?;
                    copies.push(work, (condition, self.body(statements).await?))?;
                }
                Statement::If(copies, self.body(alternate).await?)
            }
            Statement::While(condition, statements) => {
                Statement::While(self.expr(condition).await?, self.body(statements).await?)
            }
            Statement::For(place, source, statements) => Statement::For(
                self.place(place).await?,
                self.expr(source).await?,
                self.body(statements).await?,
            ),
            Statement::Return(value) => Statement::Return(self.optional(value).await?),
            Statement::Break(value) => Statement::Break(self.optional(value).await?),
            Statement::Next(value) => Statement::Next(self.optional(value).await?),
        };
        Ok(Stmt {
            node,
            depth: value.depth,
            offset: value.offset,
        })
    }

    async fn target(&self, place: &'x Target) -> Result<Target> {
        let work = self.work;
        work.charge(1)?;
        Ok(match place {
            Target::Value(value) => Target::Value(self.expr(value).await?),
            Target::Typed(place, ty) => {
                Target::Typed(Boxed::new(work, self.place(place).await?)?, ty.copy(work)?)
            }
            Target::Tuple(parts) => {
                let mut copies = Buffer::with_capacity(work, parts.len())?;
                for (part, rest) in parts {
                    work.charge(1)?;
                    let part = match part {
                        Some(part) => Some(self.place(part).await?),
                        None => None,
                    };
                    copies.push(work, (part, *rest))?;
                }
                Target::Tuple(copies)
            }
        })
    }

    async fn arguments(&self, args: &'x Buffer<Argument>) -> Result<Buffer<Argument>> {
        let mut copies = Buffer::with_capacity(self.work, args.len())?;
        for arg in args {
            self.work.charge(1)?;
            let kind = match &arg.kind {
                ArgumentKind::Positional => ArgumentKind::Positional,
                ArgumentKind::Splat => ArgumentKind::Splat,
                ArgumentKind::Keyword(name) => ArgumentKind::Keyword(self.name(name)?),
                ArgumentKind::KeywordSplat => ArgumentKind::KeywordSplat,
            };
            let value = self.expr(&arg.value).await?;
            copies.push(self.work, Argument { kind, value })?;
        }
        Ok(copies)
    }

    async fn block(&self, value: &'x Block) -> Result<Block> {
        let mut params = Buffer::with_capacity(self.work, value.params.len())?;
        for param in &value.params {
            self.work.charge(1)?;
            params.push(self.work, self.place(param).await?)?;
        }
        Ok(Block {
            params,
            body: self.body(&value.body).await?,
            implicit: value.implicit,
            infer_it: value.infer_it,
        })
    }

    async fn attempt(&self, attempt: &'x Try) -> Result<Try> {
        let work = self.work;
        let body = self.body(&attempt.body).await?;
        let mut rescues = Buffer::with_capacity(work, attempt.rescues.len())?;
        for r in &attempt.rescues {
            work.charge(1)?;
            rescues.push(
                work,
                Rescue {
                    classes: r.classes.copy_with(work, |c| Ok(*c))?,
                    binding: r.binding.as_ref().map(|n| self.name(n)).transpose()?,
                    body: self.body(&r.body).await?,
                    offset: r.offset,
                },
            )?;
        }
        Ok(Try {
            modifier: attempt.modifier,
            body,
            rescues,
            alternate: self.body(&attempt.alternate).await?,
            ensure: self.body(&attempt.ensure).await?,
        })
    }

    async fn expression(&self, value: &'x Expr) -> Result<Expr> {
        let work = self.work;
        work.charge(1)?;
        let node = match &value.node {
            Node::Integer(n) => Node::Integer(*n),
            Node::Literal(value) => Node::Literal(value.clone()),
            Node::Regex(bytes, flags) => {
                work.bytes(bytes.len())?;
                Node::Regex(bytes.clone(), *flags)
            }
            Node::BigInteger(text, radix) => {
                work.bytes(text.len())?;
                Node::BigInteger(text.clone(), *radix)
            }
            Node::Var(name) => Node::Var(self.name(name)?),
            Node::Shape(ty, fallback, names) => Node::Shape(
                Boxed::new(work, ty.copy(work)?)?,
                self.optional_box(fallback).await?,
                names.copy_with(work, |n| self.name(n))?,
            ),
            Node::Try(attempt) => {
                Node::Try(Boxed::new(work, Box::pin(self.attempt(attempt)).await?)?)
            }
            Node::Template(values, symbol) => Node::Template(self.exprs(values).await?, *symbol),
            Node::Array(values) => Node::Array(self.exprs(values).await?),
            Node::Yield(values) => Node::Yield(self.exprs(values).await?),
            Node::Hash(entries) => {
                let mut copies = Buffer::with_capacity(work, entries.len())?;
                for (key, value) in entries {
                    work.charge(1)?;
                    work.bytes(key.len())?;
                    copies.push(work, (key.clone(), self.expr(value).await?))?;
                }
                Node::Hash(copies)
            }
            Node::Unary(op, value) => Node::Unary(op, self.boxed(value).await?),
            Node::Binary(op, left, right) => {
                Node::Binary(op, self.boxed(left).await?, self.boxed(right).await?)
            }
            Node::Range(first, last, exclusive) => Node::Range(
                self.optional_box(first).await?,
                self.optional_box(last).await?,
                *exclusive,
            ),
            Node::Conditional(branches, alternate) => {
                let mut copies = Buffer::with_capacity(work, branches.len())?;
                for (condition, result) in branches {
                    work.charge(1)?;
                    let condition = self.expr(condition).await?;
                    copies.push(work, (condition, self.expr(result).await?))?;
                }
                Node::Conditional(copies, self.boxed(alternate).await?)
            }
            Node::Case(value, clauses, alternate) => {
                let value = self.optional_box(value).await?;
                let mut copies = Buffer::with_capacity(work, clauses.len())?;
                for clause in clauses {
                    work.charge(1)?;
                    let mut values = Buffer::with_capacity(work, clause.values.len())?;
                    for (value, splat) in &clause.values {
                        work.charge(1)?;
                        values.push(work, (self.expr(value).await?, *splat))?;
                    }
                    let result = self.expr(&clause.result).await?;
                    copies.push(work, When { values, result })?;
                }
                Node::Case(value, copies, self.optional_box(alternate).await?)
            }
            Node::Compound(stmt) => Node::Compound(Boxed::new(work, self.stmt(stmt).await?)?),
            Node::Call(name, args, form) => {
                Node::Call(self.name(name)?, self.arguments(args).await?, *form)
            }
            Node::ComputedCall(receiver, args) => {
                Node::ComputedCall(self.boxed(receiver).await?, self.arguments(args).await?)
            }
            Node::BlockCall(receiver, value) => Node::BlockCall(
                self.boxed(receiver).await?,
                Box::pin(self.block(value)).await?,
            ),
            Node::Member(receiver, name) => {
                Node::Member(self.boxed(receiver).await?, self.name(name)?)
            }
            Node::SafeMember(receiver, name) => {
                Node::SafeMember(self.boxed(receiver).await?, self.name(name)?)
            }
            Node::Scope(receiver, name, args) => {
                let receiver = self.boxed(receiver).await?;
                let name = self.name(name)?;
                let args = match args {
                    Some(args) => Some(self.arguments(args).await?),
                    None => None,
                };
                Node::Scope(receiver, name, args)
            }
            Node::Method(receiver, name, args, form) => Node::Method(
                self.boxed(receiver).await?,
                self.name(name)?,
                self.arguments(args).await?,
                *form,
            ),
            Node::SafeMethod(receiver, name, args, form) => Node::SafeMethod(
                self.boxed(receiver).await?,
                self.name(name)?,
                self.arguments(args).await?,
                *form,
            ),
            Node::Index(receiver, indices) => {
                Node::Index(self.boxed(receiver).await?, self.exprs(indices).await?)
            }
        };
        Ok(Expr {
            node,
            depth: value.depth,
            offset: value.offset,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CallContext, CallOptions, ErrorKind, compilation::Meter};
    use std::cell::RefCell;

    fn original(context: &mut CallContext) -> Definition {
        let source = "class Box;def original(n:int=1);begin;a=[1,2,3].map{|v|[v+n,v*2]};if n>0;a[0];else;0;end;rescue ArgumentError|TypeError=>e;raise e;ensure;nil;end;end;end";
        let mut parsed = crate::syntax::parse(source, &Meter(RefCell::new(context))).unwrap();
        parsed.modules.remove(0).instance_methods.remove(0).0
    }

    #[test]
    fn alias_copies_obey_compilation_memory_limits_and_release_partial_work() {
        for fraction in [0, 1, 2, 3] {
            let mut context = CallContext::new(CallOptions::default());
            let original = original(&mut context);
            let before = context.stats().retained_memory_bytes;
            let copy = definition(&Meter(RefCell::new(&mut context)), &original).unwrap();
            let additional = context.stats().retained_memory_bytes - before;
            assert!(additional > 0);
            drop(copy);
            assert_eq!(context.stats().retained_memory_bytes, before);
            let budget = match fraction {
                0 => 1,
                1 => additional / 2,
                2 => additional - 1,
                _ => additional,
            };
            context.options.limits.memory_bytes = Some(before + budget);
            let result = definition(&Meter(RefCell::new(&mut context)), &original);
            if fraction == 3 {
                let copy = result.unwrap();
                assert_eq!(copy.name, original.name);
                assert_eq!(copy.body.len(), original.body.len());
                drop(copy);
            } else {
                assert_eq!(result.unwrap_err().kind, ErrorKind::Memory);
                assert_eq!(context.checkpoint().unwrap_err().kind, ErrorKind::Memory);
            }
            assert_eq!(original.name, "original");
            assert!(!original.body.is_empty());
            assert_eq!(context.stats().retained_memory_bytes, before);
            drop(original);
            assert_eq!(context.stats().retained_memory_bytes, 0);
        }
    }
}
