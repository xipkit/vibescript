use super::facts::{Fact, Facts, HashKind, Node};
use crate::{CallContext, Result, budget::Buffer, bytecode::CallSite, members::names};

pub(super) fn contains(ctx: &mut CallContext, facts: &Facts, value: Fact) -> Result<bool> {
    for i in 0..facts.arm_count(value) {
        ctx.charge(1)?;
        if matches!(
            facts.node(facts.arm(value, i)),
            Node::Shape(_, _, _, HashKind::Object) | Node::Hash(_, _, HashKind::Object)
        ) {
            return Ok(true);
        }
    }
    Ok(false)
}

pub(super) fn variants(
    ctx: &mut CallContext,
    facts: &mut Facts,
    receiver: Fact,
    name: &str,
) -> Result<Option<Buffer<Fact>>> {
    ctx.charge(1)?;
    if let Node::Union(arms) = facts.node(receiver) {
        if contains(ctx, facts, receiver)? {
            let mut variants = Buffer::empty();
            variants.extend(ctx, &arms.data)?;
            return Ok(Some(variants));
        }
    }
    let Node::Shape(fields, _, _, HashKind::Object) = facts.node(receiver) else {
        return Ok(None);
    };
    let Some((value, optional)) = facts.selected_field(ctx, receiver, name.as_bytes())? else {
        return Ok(None);
    };
    if !optional && facts.arm_count(value) == 1 {
        return Ok(None);
    }
    let mut selected = None;
    for (index, field) in fields.data.iter().enumerate() {
        let key = field.name.as_bytes().unwrap();
        ctx.work_bytes(key.len().min(name.len()).saturating_add(1))?;
        if key == name.as_bytes() {
            selected = Some(index);
            break;
        }
    }
    let index = selected.unwrap();
    let mut variants = Buffer::empty();
    if optional {
        let absent = facts.replace(ctx, receiver, index, None)?;
        variants.push(ctx, absent)?;
    }
    for i in 0..facts.arm_count(value) {
        ctx.charge(1)?;
        let arm = facts.arm(value, i);
        let present = facts.replace(ctx, receiver, index, Some(arm))?;
        variants.push(ctx, present)?;
    }
    Ok(Some(variants))
}

#[derive(Clone, Copy, Debug)]
pub(super) enum Selection {
    Native,
    Field(Fact),
    Missing,
    Incomplete,
}

/// Resolves an object's field override before native method dispatch.
pub(super) fn select(
    ctx: &mut CallContext,
    facts: &Facts,
    receiver: Fact,
    site: CallSite,
    name: &str,
) -> Result<Option<Selection>> {
    ctx.charge(1)?;
    if matches!(facts.node(receiver), Node::Hash(_, _, HashKind::Object)) {
        return Ok(Some(Selection::Incomplete));
    }
    let Node::Shape(_, open, _, HashKind::Object) = facts.node(receiver) else {
        return Ok(None);
    };
    let selected = if let Some((field, optional)) =
        facts.selected_field(ctx, receiver, name.as_bytes())?
    {
        if optional {
            Selection::Incomplete
        } else if !site.scope && names::universal(name) && !matches!(name, "tap" | "yield_self") {
            if facts.known_non_callable(ctx, field)? {
                Selection::Native
            } else if matches!(facts.node(field), Node::Builtin(_) | Node::Offset(_)) {
                Selection::Field(field)
            } else {
                Selection::Incomplete
            }
        } else {
            Selection::Field(field)
        }
    } else if *open {
        Selection::Incomplete
    } else if !site.scope && (names::universal(name) || names::Receiver::Hash.available(name)) {
        Selection::Native
    } else {
        Selection::Missing
    };
    Ok(Some(selected))
}
