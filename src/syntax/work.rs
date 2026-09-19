use super::{
    Argument, ArgumentKind, Block, Definition, Expr, Node, Parameter, Rescue, Statement, Stmt,
    Target, Try, When,
};
use crate::{
    Result,
    compilation::{Boxed, Buffer, Work},
};

// Alias declarations own separate syntax containers while sharing immutable literals.
pub(super) fn definition(work: &dyn Work, value: &Definition) -> Result<Definition> {
    Ok(Definition {
        offset: value.offset,
        private: value.private,
        accessor: value
            .accessor
            .as_ref()
            .map(|(name, setter)| Ok((name_copy(work, name)?, *setter)))
            .transpose()?,
        name: name_copy(work, &value.name)?,
        params: value.params.copy_with(work, |p| {
            Ok(Parameter {
                ivar: p.ivar.as_ref().map(|n| name_copy(work, n)).transpose()?,
                name: name_copy(work, &p.name)?,
                kind: p.kind,
                default: p
                    .default
                    .as_ref()
                    .map(|e| expression(work, e))
                    .transpose()?,
                ty: p.ty.as_ref().map(|ty| type_copy(work, ty)).transpose()?,
            })
        })?,
        body: body(work, &value.body)?,
        return_type: value
            .return_type
            .as_ref()
            .map(|ty| type_copy(work, ty))
            .transpose()?,
    })
}

fn name_copy(work: &dyn Work, name: &str) -> Result<String> {
    work.bytes(name.len())?;
    Ok(name.to_owned())
}

fn type_copy(work: &dyn Work, ty: &crate::types::Type) -> Result<crate::types::Type> {
    work.ty(ty)?;
    Ok(ty.clone())
}

fn body(work: &dyn Work, values: &Buffer<Stmt>) -> Result<Buffer<Stmt>> {
    values.copy_with(work, |value| statement(work, value))
}

fn statement(work: &dyn Work, value: &Stmt) -> Result<Stmt> {
    work.charge(1)?;
    let node = match &value.node {
        Statement::Retry => Statement::Retry,
        Statement::Module(name) => Statement::Module(name_copy(work, name)?),
        Statement::UnboundClass(name) => Statement::UnboundClass(name_copy(work, name)?),
        Statement::Raise(value, message) => {
            Statement::Raise(optional_box(work, value)?, optional_box(work, message)?)
        }
        Statement::Expr(value) => Statement::Expr(expression(work, value)?),
        Statement::Assign(place, op, value) => {
            Statement::Assign(target(work, place)?, op, expression(work, value)?)
        }
        Statement::If(condition, yes, no) => Statement::If(
            expression(work, condition)?,
            body(work, yes)?,
            body(work, no)?,
        ),
        Statement::While(condition, statements) => {
            Statement::While(expression(work, condition)?, body(work, statements)?)
        }
        Statement::For(place, source, statements) => Statement::For(
            target(work, place)?,
            expression(work, source)?,
            body(work, statements)?,
        ),
        Statement::Return(value) => {
            Statement::Return(value.as_ref().map(|e| expression(work, e)).transpose()?)
        }
        Statement::Break(value) => {
            Statement::Break(value.as_ref().map(|e| expression(work, e)).transpose()?)
        }
        Statement::Next(value) => {
            Statement::Next(value.as_ref().map(|e| expression(work, e)).transpose()?)
        }
    };
    Ok(Stmt {
        node,
        offset: value.offset,
    })
}

fn target(work: &dyn Work, place: &Target) -> Result<Target> {
    work.charge(1)?;
    Ok(match place {
        Target::Value(value) => Target::Value(expression(work, value)?),
        Target::Typed(place, ty) => Target::Typed(
            Boxed::new(work, target(work, place)?)?,
            type_copy(work, ty)?,
        ),
        Target::Tuple(parts) => Target::Tuple(parts.copy_with(work, |(part, rest)| {
            Ok((part.as_ref().map(|p| target(work, p)).transpose()?, *rest))
        })?),
    })
}

fn arguments(work: &dyn Work, args: &Buffer<Argument>) -> Result<Buffer<Argument>> {
    args.copy_with(work, |arg| {
        let kind = match &arg.kind {
            ArgumentKind::Positional => ArgumentKind::Positional,
            ArgumentKind::Splat => ArgumentKind::Splat,
            ArgumentKind::Keyword(name) => ArgumentKind::Keyword(name_copy(work, name)?),
            ArgumentKind::KeywordSplat => ArgumentKind::KeywordSplat,
        };
        Ok(Argument {
            kind,
            value: expression(work, &arg.value)?,
        })
    })
}

fn block(work: &dyn Work, value: &Block) -> Result<Block> {
    Ok(Block {
        params: value.params.copy_with(work, |p| target(work, p))?,
        body: body(work, &value.body)?,
        implicit: value.implicit,
        infer_it: value.infer_it,
    })
}

fn boxed(work: &dyn Work, value: &Expr) -> Result<Boxed<Expr>> {
    Boxed::new(work, expression(work, value)?)
}

fn optional_box(work: &dyn Work, value: &Option<Boxed<Expr>>) -> Result<Option<Boxed<Expr>>> {
    value.as_ref().map(|e| boxed(work, e)).transpose()
}

fn expression(work: &dyn Work, value: &Expr) -> Result<Expr> {
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
        Node::Var(name) => Node::Var(name_copy(work, name)?),
        Node::Shape(ty, fallback, names) => Node::Shape(
            Boxed::new(work, type_copy(work, ty)?)?,
            optional_box(work, fallback)?,
            names.copy_with(work, |n| name_copy(work, n))?,
        ),
        Node::Try(attempt) => Node::Try(Boxed::new(
            work,
            Try {
                modifier: attempt.modifier,
                body: body(work, &attempt.body)?,
                rescues: attempt.rescues.copy_with(work, |r| {
                    Ok(Rescue {
                        classes: r.classes.copy_with(work, |c| Ok(*c))?,
                        binding: r.binding.as_ref().map(|n| name_copy(work, n)).transpose()?,
                        body: body(work, &r.body)?,
                        offset: r.offset,
                    })
                })?,
                alternate: body(work, &attempt.alternate)?,
                ensure: body(work, &attempt.ensure)?,
            },
        )?),
        Node::Template(values, symbol) => {
            Node::Template(values.copy_with(work, |e| expression(work, e))?, *symbol)
        }
        Node::Array(values) => Node::Array(values.copy_with(work, |e| expression(work, e))?),
        Node::Yield(values) => Node::Yield(values.copy_with(work, |e| expression(work, e))?),
        Node::Hash(entries) => Node::Hash(entries.copy_with(work, |(key, value)| {
            work.bytes(key.len())?;
            Ok((key.clone(), expression(work, value)?))
        })?),
        Node::Unary(op, value) => Node::Unary(op, boxed(work, value)?),
        Node::Binary(op, left, right) => Node::Binary(op, boxed(work, left)?, boxed(work, right)?),
        Node::Range(first, last, exclusive) => Node::Range(
            optional_box(work, first)?,
            optional_box(work, last)?,
            *exclusive,
        ),
        Node::Conditional(condition, yes, no) => {
            Node::Conditional(boxed(work, condition)?, boxed(work, yes)?, boxed(work, no)?)
        }
        Node::Case(value, clauses, alternate) => Node::Case(
            optional_box(work, value)?,
            clauses.copy_with(work, |clause| {
                Ok(When {
                    values: clause.values.copy_with(work, |(value, splat)| {
                        Ok((expression(work, value)?, *splat))
                    })?,
                    result: expression(work, &clause.result)?,
                })
            })?,
            optional_box(work, alternate)?,
        ),
        Node::Loop(stmt) => Node::Loop(Boxed::new(work, statement(work, stmt)?)?),
        Node::Call(name, args, form) => {
            Node::Call(name_copy(work, name)?, arguments(work, args)?, *form)
        }
        Node::ComputedCall(receiver, args) => {
            Node::ComputedCall(boxed(work, receiver)?, arguments(work, args)?)
        }
        Node::BlockCall(receiver, value) => {
            Node::BlockCall(boxed(work, receiver)?, block(work, value)?)
        }
        Node::Member(receiver, name) => {
            Node::Member(boxed(work, receiver)?, name_copy(work, name)?)
        }
        Node::SafeMember(receiver, name) => {
            Node::SafeMember(boxed(work, receiver)?, name_copy(work, name)?)
        }
        Node::Scope(receiver, name, args) => Node::Scope(
            boxed(work, receiver)?,
            name_copy(work, name)?,
            args.as_ref()
                .map(|args| arguments(work, args))
                .transpose()?,
        ),
        Node::Method(receiver, name, args, form) => Node::Method(
            boxed(work, receiver)?,
            name_copy(work, name)?,
            arguments(work, args)?,
            *form,
        ),
        Node::SafeMethod(receiver, name, args, form) => Node::SafeMethod(
            boxed(work, receiver)?,
            name_copy(work, name)?,
            arguments(work, args)?,
            *form,
        ),
        Node::Index(receiver, indices) => Node::Index(
            boxed(work, receiver)?,
            indices.copy_with(work, |e| expression(work, e))?,
        ),
    };
    Ok(Expr {
        node,
        depth: value.depth,
        offset: value.offset,
    })
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
