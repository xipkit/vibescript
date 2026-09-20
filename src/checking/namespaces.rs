use super::facts::{Atom, Fact, Facts, Field, HashKind, Node};
use crate::{CallContext, Result, budget::Buffer, bytecode::Program};

pub(super) const WIDTH: usize = 4;

pub(super) fn value(
    ctx: &mut CallContext,
    facts: &mut Facts,
    program: &Program,
    owner: usize,
    module: usize,
) -> Result<Fact> {
    let definition = &program.namespaces[module];
    ctx.work_bytes(definition.name.len())?;
    let declaration = program.declaration_names[&definition.name];
    let ty = facts.nominal(ctx, owner, declaration, definition.name.as_bytes(), None)?;
    facts.type_value(ctx, ty)
}

pub(super) fn initial(
    ctx: &mut CallContext,
    facts: &mut Facts,
    program: &Program,
    owner: usize,
    module: usize,
) -> Result<Fact> {
    let mut fields = Buffer::empty();
    for (name, nested) in &program.namespaces[module].nested {
        ctx.charge(1)?;
        let value = value(ctx, facts, program, owner, *nested)?;
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
    facts.shape_fields(ctx, fields, false, Atom::String.fact(), HashKind::PLAIN)
}

pub(super) struct Selected {
    pub value: Fact,
    pub missing: bool,
    pub incomplete: bool,
}

pub(super) fn refine(
    ctx: &mut CallContext,
    facts: &mut Facts,
    fields: Fact,
    name: &str,
    present: bool,
) -> Result<Fact> {
    let mut kept = Buffer::empty();
    for i in 0..facts.arm_count(fields) {
        ctx.charge(1)?;
        let arm = facts.arm(fields, i);
        let Node::Shape(fields, open, ..) = facts.node(arm) else {
            kept.push(ctx, arm)?;
            continue;
        };
        let mut selected = None;
        for (index, field) in fields.data.iter().enumerate() {
            ctx.work_bytes(name.len().max(field.name.as_bytes().unwrap().len()))?;
            if field.name.as_bytes() == Some(name.as_bytes()) {
                selected = Some((index, field.value, field.optional));
                break;
            }
        }
        let next = match selected {
            Some((index, value, _)) if present => facts.replace(ctx, arm, index, Some(value))?,
            Some((index, _, true)) => facts.replace(ctx, arm, index, None)?,
            Some(_) => continue,
            None if !present || *open => arm,
            None => continue,
        };
        kept.push(ctx, next)?;
    }
    facts.union(ctx, &kept.data)
}

pub(super) fn field(
    ctx: &mut CallContext,
    facts: &mut Facts,
    fields: Fact,
    name: &str,
) -> Result<Selected> {
    let mut selected = Selected {
        value: Atom::Never.fact(),
        missing: false,
        incomplete: false,
    };
    for i in 0..facts.arm_count(fields) {
        ctx.charge(1)?;
        let arm = facts.arm(fields, i);
        if let Node::Shape(_, open, ..) = facts.node(arm) {
            let open = *open;
            if let Some((value, optional)) = facts.selected_field(ctx, arm, name.as_bytes())? {
                selected.value = facts.union(ctx, &[selected.value, value])?;
                selected.missing |= optional;
            } else if open {
                selected.incomplete = true;
            } else {
                selected.missing = true;
            }
        } else {
            selected.incomplete = true;
        }
    }
    Ok(selected)
}

/// Resolves the effective accessor contract after user method overrides.
pub(super) fn property_type(
    ctx: &mut CallContext,
    program: &Program,
    module: usize,
    name: &str,
) -> Result<Option<usize>> {
    let mut getter = None;
    let mut setter = None;
    for method in &program.namespaces[module].instance_methods {
        ctx.work_bytes(name.len().max(method.name.len()))?;
        if method.name.strip_suffix('=') == Some(name) {
            setter = Some(method.function)
        }
        if method.name == name {
            getter = Some(method.function)
        }
    }
    Ok(if let Some(setter) = setter {
        let function = &program.functions[setter];
        if function
            .accessor
            .as_ref()
            .is_some_and(|(field, setter)| field == name && *setter)
        {
            function.params.first().and_then(|param| param.ty)
        } else {
            None
        }
    } else {
        getter.and_then(|getter| {
            let function = &program.functions[getter];
            function
                .accessor
                .as_ref()
                .filter(|(field, setter)| field == name && !setter)
                .and(function.return_type)
        })
    })
}
