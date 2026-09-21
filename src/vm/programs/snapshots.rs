use super::*;
use crate::{namespace::Namespace, objects::Instance};

type Bindings = Buffer<(Arc<Code>, Arc<Instance>)>;

struct Source {
    code: Arc<Code>,
    environment: Option<Arc<Instance>>,
}

impl Source {
    fn namespace(namespace: &Namespace) -> Option<Self> {
        let code = namespace.owner.clone().or_else(|| {
            namespace
                .definition
                .owner
                .get()
                .and_then(|owner| owner.upgrade())
        })?;
        Some(Self {
            code,
            environment: namespace.environment.clone(),
        })
    }
}

struct Walk {
    values: Buffer<Value>,
    instances: Buffer<Arc<Instance>>,
}

impl Walk {
    fn new(ctx: &mut CallContext, roots: &[Value]) -> Result<Self> {
        let mut values = Buffer::empty();
        values.extend(ctx, roots)?;
        Ok(Self {
            values,
            instances: Buffer::empty(),
        })
    }

    fn next(&mut self, ctx: &mut CallContext) -> Result<Option<Source>> {
        while let Some(value) = self.values.data.pop() {
            ctx.charge(1)?;
            match value.0 {
                Kind::Array(array) => self.values.extend(ctx, &array.buffer.data)?,
                Kind::Hash(hash) => {
                    for (key, value) in &hash.buffer.data {
                        self.values.push(ctx, key.clone())?;
                        self.values.push(ctx, value.clone())?;
                    }
                }
                Kind::Instance(instance) => {
                    let mut seen = false;
                    for previous in &self.instances.data {
                        ctx.charge(1)?;
                        if instance.same(previous) {
                            seen = true;
                            break;
                        }
                    }
                    if !seen {
                        self.instances.push(ctx, instance.clone())?;
                        crate::objects::children(ctx, &instance, &mut self.values)?;
                        if let Some(source) = Source::namespace(instance.class()) {
                            return Ok(Some(source));
                        }
                    }
                }
                Kind::Namespace(namespace) => {
                    if let Some(environment) = &namespace.environment {
                        self.values
                            .push(ctx, Value(Kind::Instance(environment.clone())))?;
                    }
                    if let Some(source) = Source::namespace(&namespace) {
                        return Ok(Some(source));
                    }
                }
                Kind::Function(function) => {
                    self.values
                        .push(ctx, Value(Kind::Instance(function.environment.clone())))?;
                    return Ok(Some(Source {
                        code: function.code.clone(),
                        environment: Some(function.environment.clone()),
                    }));
                }
                _ => (),
            }
        }
        Ok(None)
    }
}

fn materialize(ctx: &mut CallContext, storage: &Storage, values: &[Value]) -> Result<Bindings> {
    let mut bindings = Buffer::empty();
    let mut walk = Walk::new(ctx, values)?;
    while let Some(source) = walk.next(ctx)? {
        if source.environment.is_some() {
            continue;
        }
        let code = &source.code;
        let mut found = false;
        for (previous, _) in &bindings.data {
            ctx.charge(1)?;
            if Arc::ptr_eq(code, previous) {
                found = true;
                break;
            }
        }
        if found {
            continue;
        }
        let environment = crate::objects::snapshot_environment(ctx)?;
        bindings.push(ctx, (code.clone(), environment.clone()))?;
        if let Some(program) = registered(ctx, storage, code, None)? {
            for state in &storage.namespaces.data {
                ctx.charge(1)?;
                if state.program != program {
                    continue;
                }
                let captured =
                    scopes::namespace(ctx, &environment, state.namespace.definition.index)?;
                for (name, value) in &state.fields.buffer.data {
                    ctx.charge(1)?;
                    let name = std::str::from_utf8(name.as_bytes().unwrap()).map_err(|_| {
                        Error::new(ErrorKind::Type, "namespace field name is not UTF-8")
                    })?;
                    crate::objects::set(ctx, &captured.fields, name, value)?;
                    walk.values.push(ctx, value.clone())?;
                }
            }
        }
    }
    Ok(bindings)
}

fn seal(ctx: &mut CallContext, values: &[Value]) -> Result<()> {
    let mut environments: Bindings = Buffer::empty();
    let mut walk = Walk::new(ctx, values)?;
    while let Some(source) = walk.next(ctx)? {
        let Some(environment) = &source.environment else {
            continue;
        };
        let code = &source.code;
        let mut seen = false;
        for (previous_code, previous) in &environments.data {
            ctx.charge(1)?;
            if Arc::ptr_eq(previous_code, code) && previous.same(environment) {
                seen = true;
                break;
            }
        }
        if seen {
            continue;
        }
        environments.push(ctx, (code.clone(), environment.clone()))?;
        // A snapshot records the current state, including a partially executed
        // initializer. Admitting it must never replay the source's initialization.
        for index in 0..code.program.namespaces.len() {
            scopes::initialized(ctx, environment, index)?;
        }
    }
    Ok(())
}

pub(super) fn snapshot(ctx: &mut CallContext, storage: &Storage, value: &Value) -> Result<Value> {
    let mut values = [value.clone()];
    snapshot_values(ctx, storage, &mut values)?;
    Ok(std::mem::take(&mut values[0]))
}

pub(super) fn snapshot_values(
    ctx: &mut CallContext,
    storage: &Storage,
    values: &mut [Value],
) -> Result<()> {
    let bindings = materialize(ctx, storage, values)?;
    let previous = ctx.snapshot_namespaces.replace(bindings);
    let result = ctx.snapshot_values(values);
    ctx.snapshot_namespaces = previous;
    result?;
    seal(ctx, values)
}

#[cfg(test)]
mod tests;
