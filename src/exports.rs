use crate::{
    CallContext, Error, ErrorKind, Result, Value,
    budget::{Charge, MAX_VALUE_DEPTH},
    code::Code,
    objects::Instance,
    value::Kind,
};
use std::sync::Arc;

#[derive(Debug)]
pub(crate) struct Function {
    pub code: Arc<Code>,
    pub environment: Arc<Instance>,
    pub index: usize,
    _header: Option<Charge>,
}

impl Function {
    pub fn new(
        ctx: &mut CallContext,
        code: Arc<Code>,
        environment: Arc<Instance>,
        index: usize,
    ) -> Result<Arc<Self>> {
        ctx.has_exports = true;
        ctx.scoped_sources = true;
        Code::retain(ctx, &code)?;
        let header = ctx.reserve(size_of::<Self>() + 2 * size_of::<usize>())?;
        Ok(Arc::new(Self {
            code,
            environment,
            index,
            _header: header,
        }))
    }

    pub fn import(ctx: &mut CallContext, value: &Arc<Self>) -> Result<Arc<Self>> {
        ctx.has_exports = true;
        ctx.scoped_sources = true;
        if ctx.namespace_depth >= MAX_VALUE_DEPTH {
            return ctx.guard(
                ErrorKind::Recursion,
                "function environment nesting too deep",
            );
        }
        ctx.namespace_depth += 1;
        let environment = crate::objects::import(ctx, &value.environment);
        ctx.namespace_depth -= 1;
        Self::with_environment(ctx, value, environment?)
    }

    pub fn with_environment(
        ctx: &mut CallContext,
        value: &Arc<Self>,
        environment: Arc<Instance>,
    ) -> Result<Arc<Self>> {
        Self::new(ctx, value.code.clone(), environment, value.index)
    }

    pub fn same(&self, other: &Self) -> bool {
        self.index == other.index
            && Arc::ptr_eq(&self.code, &other.code)
            && self.environment.same(&other.environment)
    }

    pub fn value_error(&self) -> Error {
        Error::new(
            ErrorKind::Type,
            format!(
                "{} is a function and cannot be used as a value; call it through its module",
                self.code.program.functions[self.index].name
            ),
        )
    }
}

pub(crate) fn check(ctx: &mut CallContext, value: &Value) -> Result<()> {
    if ctx.has_exports {
        check_depth(ctx, value, 0)?;
    }
    Ok(())
}

pub(crate) fn member(
    ctx: &mut CallContext,
    site: crate::bytecode::CallSite,
    name: &str,
    receiver: &Value,
) -> Result<Option<Arc<Function>>> {
    if ctx.has_exports && matches!(receiver.0, Kind::Hash(_)) {
        if let Some(Value(Kind::Function(function))) =
            crate::members::field(ctx, site, name, receiver)?
        {
            return Ok(Some(function));
        }
    }
    Ok(None)
}

fn check_depth(ctx: &mut CallContext, value: &Value, depth: usize) -> Result<()> {
    ctx.charge(1)?;
    if depth > MAX_VALUE_DEPTH {
        return ctx.guard(ErrorKind::Recursion, "value nesting too deep");
    }
    match &value.0 {
        Kind::Function(function) => return Err(function.value_error()),
        Kind::Array(array) => {
            for value in &array.buffer.data {
                check_depth(ctx, value, depth + 1)?;
            }
        }
        Kind::Hash(hash) if !hash.object => {
            for (_, value) in &hash.buffer.data {
                check_depth(ctx, value, depth + 1)?;
            }
        }
        _ => (),
    }
    Ok(())
}
