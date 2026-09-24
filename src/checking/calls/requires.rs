use super::*;
use crate::checking::{
    environment::Environment,
    facts::{Field, HashKind, Node},
    pending::Pending,
    slots::Slots,
};
use crate::{
    Error, ErrorClass, ErrorKind,
    code::{Code, Export},
    loading::Pin,
};

pub(in crate::checking) struct Request {
    pub name: Value,
    pub alias: Option<Value>,
    pub bindings: Buffer<Fact>,
}

pub(super) struct Imports {
    groups: Buffer<Group>,
    modules: Buffer<Module>,
}

struct Group {
    receiving: SourceId,
    pins: Buffer<Pin>,
}

struct Resolved {
    code: Arc<Code>,
    receiving: SourceId,
    loader: Arc<crate::loading::Loader>,
}

struct Module {
    code: Arc<Code>,
    receiving: SourceId,
    source: SourceId,
    exports: Fact,
}

impl Imports {
    pub fn new() -> Self {
        Self {
            groups: Buffer::empty(),
            modules: Buffer::empty(),
        }
    }

    fn group(&mut self, ctx: &mut CallContext, receiving: SourceId) -> Result<usize> {
        for (index, group) in self.groups.data.iter().enumerate() {
            ctx.charge(1)?;
            if group.receiving == receiving {
                return Ok(index);
            }
        }
        let index = self.groups.data.len();
        self.groups.push(
            ctx,
            Group {
                receiving,
                pins: Buffer::empty(),
            },
        )?;
        Ok(index)
    }
}

pub(in crate::checking) fn failure(
    ctx: &mut CallContext,
    facts: &mut Facts,
    error: Error,
) -> Result<Outcome> {
    ctx.checkpoint()?;
    let Some(class) = error.class().filter(|class| *class != ErrorClass::Limit) else {
        return Err(error);
    };
    let message = facts.string(ctx, error.message.as_bytes())?;
    let mut result = Outcome::empty();
    result
        .failures
        .push(ctx, Failure::Require { message, class })?;
    Ok(result)
}

fn success(ctx: &mut CallContext, globals: Globals, value: Fact) -> Result<Outcome> {
    let mut result = Outcome::empty();
    result.value = value;
    result.exits.push(
        ctx,
        blocks::Exit {
            pc: 0,
            completion: blocks::Completion::Value,
            value,
            captures: Slots::new(0, Atom::Never.fact()),
            written: Slots::new(0, false),
            refined: Slots::new(0, false),
            pending: Pending::new(),
            globals,
        },
    )?;
    Ok(result)
}

fn merge(
    ctx: &mut CallContext,
    facts: &mut Facts,
    result: &mut Outcome,
    next: Outcome,
) -> Result<()> {
    result.value = facts.union(ctx, &[result.value, next.value])?;
    result.throws |= next.throws;
    result.incomplete |= next.incomplete;
    result.failures.extend(ctx, &next.failures.data)?;
    for exit in next.exits.data {
        result.exits.push(ctx, exit)?;
    }
    Ok(())
}

impl Solver<'_, '_> {
    pub(super) fn require_file(
        &mut self,
        ctx: &mut CallContext,
        facts: &mut Facts,
        request: &Request,
        current_error: u16,
        globals: &Globals,
    ) -> Result<Outcome> {
        ctx.checkpoint()?;
        let layout = self.prepare(ctx, facts)?;
        let receiving = layout.source(ctx, self.source)?.receiving;
        let root = self.root_handle(ctx)?;
        let Some(loader) = root.view().world.loader else {
            return Ok(Outcome {
                incomplete: true,
                ..Outcome::empty()
            });
        };
        let Some(receiving_code) = facts.source_code(ctx, receiving)? else {
            return Ok(Outcome {
                incomplete: true,
                ..Outcome::empty()
            });
        };
        let caller = facts.source_code(ctx, self.source)?;
        let group = self.state.imports.group(ctx, receiving)?;
        let loaded = loader.load(
            ctx,
            &mut self.state.imports.groups.data[group].pins,
            request.name.as_bytes().unwrap(),
            caller.as_ref().and_then(|code| code.origin.as_ref()),
            &receiving_code,
        );
        let code = match loaded {
            Ok(code) => code,
            Err(error) => return failure(ctx, facts, error.in_required_file()),
        };
        let resolved = Resolved {
            code,
            receiving,
            loader: loader.clone(),
        };
        let mut pending = Buffer::empty();
        let globals = globals.snapshot(ctx)?;
        pending.push(ctx, globals)?;
        let mut result = Outcome::empty();
        while let Some(globals) = pending.data.pop() {
            ctx.charge(1)?;
            let next = self.require_variant(
                ctx,
                facts,
                request,
                current_error,
                globals,
                &resolved,
                &mut pending,
            )?;
            merge(ctx, facts, &mut result, next)?;
        }
        Ok(result)
    }

    #[allow(clippy::too_many_arguments)]
    fn require_variant(
        &mut self,
        ctx: &mut CallContext,
        facts: &mut Facts,
        request: &Request,
        current_error: u16,
        mut globals: Globals,
        resolved: &Resolved,
        pending: &mut Buffer<Globals>,
    ) -> Result<Outcome> {
        let Resolved {
            code,
            receiving,
            loader,
        } = resolved;
        let receiving = *receiving;
        globals.expand(ctx, &self.state.storage.layout)?;
        let mut fresh = None;
        let mut attempt = 0;
        for index in 0..self.state.imports.modules.data.len() {
            ctx.charge(1)?;
            let module = &self.state.imports.modules.data[index];
            if module.receiving != receiving || !Arc::ptr_eq(&module.code, code) {
                continue;
            }
            attempt += 1;
            let source = module.source;
            let slot = globals.layout.source(ctx, source)?.import.unwrap();
            let status = globals.value(ctx, slot)?;
            if facts.arm_count(status) > 1 {
                for arm in 0..facts.arm_count(status) {
                    let arm = facts.arm(status, arm);
                    let mut variant = globals.snapshot(ctx)?;
                    variant.store(ctx, facts, slot, arm)?;
                    pending.push(ctx, variant)?;
                }
                return Ok(Outcome::empty());
            }
            match facts.node(status) {
                Node::Symbol(value) if value.as_bytes() == Some(b"loading") => {
                    return failure(
                        ctx,
                        facts,
                        Error::new(ErrorKind::Runtime, "require: circular dependency detected"),
                    );
                }
                Node::Symbol(value) if value.as_bytes() == Some(b"loaded") => {
                    return self.loaded_module(ctx, facts, index, request, globals);
                }
                Node::Atom(Atom::Nil) => {
                    fresh.get_or_insert(index);
                }
                Node::Symbol(value) if value.as_bytes() == Some(b"failed") => (),
                _ => {
                    return Ok(Outcome {
                        incomplete: true,
                        ..Outcome::empty()
                    });
                }
            }
        }
        let index = if let Some(index) = fresh {
            index
        } else {
            let owner = facts.import_owner(ctx, code, receiving, attempt)?;
            let environment = Environment::module(ctx, facts, code, owner, loader)?;
            let handle = Handle::owned(ctx, facts, environment)?;
            let source = handle.view().source;
            let world = self.state.worlds.insert(ctx, handle.clone())?;
            self.state
                .adapter(world, &handle)
                .prepare_receiving(ctx, facts, Some(receiving))?;
            let mut fields = Buffer::empty();
            for (name, target) in &code.exports {
                ctx.charge(1)?;
                let value = match *target {
                    Export::Function(index) => {
                        facts.callable(ctx, owner, Callable::Function(index))?
                    }
                    Export::Enum(index) => {
                        facts.enumeration(ctx, &code.program.declarations[index])?
                    }
                };
                let name = ctx.bytes(name.as_bytes())?;
                fields.push(
                    ctx,
                    Field {
                        name,
                        value,
                        optional: false,
                    },
                )?;
            }
            let exports =
                facts.shape_fields(ctx, fields, false, Atom::String.fact(), HashKind::OBJECT)?;
            let index = self.state.imports.modules.data.len();
            self.state.imports.modules.push(
                ctx,
                Module {
                    code: code.clone(),
                    receiving,
                    source,
                    exports,
                },
            )?;
            index
        };
        globals.expand(ctx, &self.state.storage.layout)?;
        if let Some(rejected) = self.check_alias(ctx, facts, index, request, &globals)? {
            return Ok(rejected);
        }
        let source = self.state.imports.modules.data[index].source;
        let slot = globals.layout.source(ctx, source)?.import.unwrap();
        let loading = facts.symbol(ctx, b"loading")?;
        globals.store(ctx, facts, slot, loading)?;
        let (world, handle) = self.state.worlds.get(ctx, source)?;
        let mut context = Context::plain();
        context.kind = Kind::Entry {
            general: false,
            admit: false,
        };
        context.globals = globals;
        let mut adapter = self.state.adapter(world, &handle);
        let job = adapter.request(ctx, facts, 0, &[], current_error, &context)?;
        adapter.depend(ctx, job)?;
        let mut result = Outcome::empty();
        if let Some(report) = &adapter.state.jobs.data[job].report {
            for exit in &report.block_exits.data {
                let exit = exit.snapshot(ctx)?;
                result.exits.push(ctx, exit)?;
            }
        }
        result.value = Atom::Never.fact();
        for exit in &mut result.exits.data {
            ctx.charge(1)?;
            let completed = exit.completion == blocks::Completion::Value;
            let status = facts.symbol(ctx, if completed { b"loaded" } else { b"failed" })?;
            exit.globals.store(ctx, facts, slot, status)?;
            if completed {
                self.publish(
                    ctx,
                    facts,
                    index,
                    request.alias.as_ref(),
                    &mut exit.globals,
                    true,
                )?;
                exit.value = self.state.imports.modules.data[index].exports;
                result.value = facts.union(ctx, &[result.value, exit.value])?;
                // A failed initialization publishes nothing, so its partial private state
                // never reaches the file's declarations.
                self.state.adapter(world, &handle).required_declarations(
                    ctx,
                    facts,
                    &exit.globals,
                )?;
            }
        }
        Ok(result)
    }

    fn check_alias(
        &mut self,
        ctx: &mut CallContext,
        facts: &mut Facts,
        index: usize,
        request: &Request,
        globals: &Globals,
    ) -> Result<Option<Outcome>> {
        let Some(alias) = &request.alias else {
            return Ok(None);
        };
        let name = std::str::from_utf8(alias.as_bytes().unwrap()).unwrap();
        let module = &self.state.imports.modules.data[index];
        let exports = module.exports;
        let receiving = module.receiving;
        let mut bindings = Buffer::empty();
        bindings.extend(ctx, &request.bindings.data)?;
        let root = self.root_handle(ctx)?;
        let mut shared = false;
        if let Some(slot) = globals.layout.root(ctx, receiving, name)? {
            if globals.value(ctx, slot)? != Atom::Never.fact() {
                if globals.missing(ctx, slot)? {
                    return Ok(Some(Outcome {
                        incomplete: true,
                        ..Outcome::empty()
                    }));
                }
                shared = true;
                let value = globals.value(ctx, slot)?;
                bindings.push(ctx, value)?;
            }
        }
        if !shared
            && (root.view().world.declared_target(ctx, receiving, name)? != Target::Undefined
                || crate::builtin::Global::parse(name).is_some())
        {
            return failure(
                ctx,
                facts,
                Error::new(ErrorKind::Argument, "require: alias already defined"),
            )
            .map(Some);
        }
        ctx.charge(module.code.exports.len() as u64 + 1)?;
        let functions = module
            .code
            .exports
            .iter()
            .any(|(_, target)| matches!(target, Export::Function(_)));
        for value in bindings.data {
            ctx.charge(1)?;
            // Structural equality proves the runtime's function-bearing export comparison.
            // Other objects require allocation identity, which the checker does not yet track.
            if value == exports && functions {
                continue;
            }
            let unknown = facts.relation(ctx, value, exports)? == Relation::Gradual;
            if unknown || value == exports {
                return Ok(Some(Outcome {
                    incomplete: true,
                    ..Outcome::empty()
                }));
            }
            return failure(
                ctx,
                facts,
                Error::new(ErrorKind::Argument, "require: alias already defined"),
            )
            .map(Some);
        }
        Ok(None)
    }

    fn loaded_module(
        &mut self,
        ctx: &mut CallContext,
        facts: &mut Facts,
        index: usize,
        request: &Request,
        mut globals: Globals,
    ) -> Result<Outcome> {
        if let Some(rejected) = self.check_alias(ctx, facts, index, request, &globals)? {
            return Ok(rejected);
        }
        self.publish(
            ctx,
            facts,
            index,
            request.alias.as_ref(),
            &mut globals,
            false,
        )?;
        success(ctx, globals, self.state.imports.modules.data[index].exports)
    }

    fn publish(
        &mut self,
        ctx: &mut CallContext,
        facts: &mut Facts,
        index: usize,
        alias: Option<&Value>,
        globals: &mut Globals,
        initial: bool,
    ) -> Result<()> {
        let root = self.root_handle(ctx)?;
        let module = &self.state.imports.modules.data[index];
        let receiving = module.receiving;
        let exports = module.exports;
        if initial {
            let Node::Shape(fields, ..) = facts.node(exports) else {
                unreachable!()
            };
            let mut entries = Buffer::empty();
            for field in &fields.data {
                entries.push(ctx, (field.name.clone(), field.value))?;
            }
            for (name, value) in entries.data {
                let name = std::str::from_utf8(name.as_bytes().unwrap()).unwrap();
                if root.view().world.declared_target(ctx, receiving, name)? != Target::Undefined
                    || crate::builtin::Global::parse(name).is_some()
                {
                    continue;
                }
                let slot = self.state.storage.root(ctx, receiving, name)?;
                globals.expand(ctx, &self.state.storage.layout)?;
                if globals.value(ctx, slot)? == Atom::Never.fact() {
                    // Supplied roots exist even when their lazy value has not been admitted.
                    if globals
                        .layout
                        .source(ctx, receiving)?
                        .root(ctx, slot)?
                        .is_some()
                    {
                        continue;
                    }
                    globals.store(ctx, facts, slot, value)?;
                } else if globals.missing(ctx, slot)? {
                    let current = globals.value(ctx, slot)?;
                    let value = facts.union(ctx, &[current, value])?;
                    globals.store(ctx, facts, slot, value)?;
                }
            }
        }
        if let Some(alias) = alias {
            let name = std::str::from_utf8(alias.as_bytes().unwrap()).unwrap();
            let slot = self.state.storage.root(ctx, receiving, name)?;
            globals.expand(ctx, &self.state.storage.layout)?;
            globals.store(ctx, facts, slot, exports)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
