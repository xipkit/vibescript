use super::facts::{Atom, Fact, Facts, HashKind, Node};
use crate::{CallContext, Result, budget::Buffer, bytecode::CallSite, members::names};

pub(super) fn contains(ctx: &mut CallContext, facts: &Facts, value: Fact) -> Result<bool> {
    for i in 0..facts.arm_count(value) {
        ctx.charge(1)?;
        if matches!(
            facts.node(facts.arm(value, i)),
            Node::Shape(_, _, _, HashKind::OBJECT) | Node::Hash(_, _, HashKind::OBJECT)
        ) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Whether plain-hash and host-object dispatch can differ for this member.
///
/// Builtin names on plain hashes never read stored fields, while object fields
/// override them; scoped access requires an object namespace. Other names read
/// fields on both provenances.
pub(super) fn provenance_sensitive(name: &str, scope: bool) -> bool {
    scope || crate::members::hash_builtin(name)
}

/// Whether a union needs per-arm dispatch: object arms, uncertain provenance
/// for a sensitive name, or stored-field lookups on any hash arm.
fn splits(
    ctx: &mut CallContext,
    facts: &Facts,
    value: Fact,
    name: &str,
    scope: bool,
) -> Result<bool> {
    for i in 0..facts.arm_count(value) {
        ctx.charge(1)?;
        let kind = match facts.node(facts.arm(value, i)) {
            Node::Shape(_, _, _, kind) | Node::Hash(_, _, kind) => *kind,
            _ => continue,
        };
        if kind.object()
            || (!kind.single() && provenance_sensitive(name, scope))
            || scope
            || !crate::members::hash_builtin(name)
        {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Splits a receiver into alternatives whose member dispatch is uniform.
///
/// Uncertain hash provenance becomes separate plain and object copies plus one
/// protected alternative per admitted tag, and an object field with optional
/// presence or several possible values becomes one variant per alternative.
/// Returns `None` when no split is needed.
///
/// Reads of declared fields agree across provenances and stay unsplit, except
/// the match offset fields: a bare `begin` or `end` read fails on a match
/// object where a plain hash returns its stored field.
pub(super) fn variants(
    ctx: &mut CallContext,
    facts: &mut Facts,
    receiver: Fact,
    name: &str,
    scope: bool,
) -> Result<Option<Buffer<Fact>>> {
    ctx.charge(1)?;
    if let Node::Union(arms) = facts.node(receiver) {
        if splits(ctx, facts, receiver, name, scope)? {
            let mut variants = Buffer::empty();
            variants.extend(ctx, &arms.data)?;
            return Ok(Some(variants));
        }
        return Ok(None);
    }
    if let Node::Shape(_, _, _, kind) | Node::Hash(_, _, kind) = facts.node(receiver) {
        let kind = *kind;
        if !kind.single() {
            let offset = kind.has(HashKind::MATCH)
                && matches!(name, "begin" | "end")
                && matches!(facts.node(receiver), Node::Shape(..))
                && facts
                    .selected_field(ctx, receiver, name.as_bytes())?
                    .is_some();
            if !provenance_sensitive(name, scope) && !offset {
                return Ok(None);
            }
            let mut variants = Buffer::empty();
            if kind.has(HashKind::PLAIN) {
                let plain = facts.hash_as(ctx, receiver, HashKind::PLAIN)?;
                variants.push(ctx, plain)?;
            }
            if kind.has(HashKind::OBJECT) {
                let object = facts.hash_as(ctx, receiver, HashKind::OBJECT)?;
                variants.push(ctx, object)?;
            }
            for (tag, bit) in [
                (crate::hash::Tag::Match, HashKind::MATCH),
                (crate::hash::Tag::Error, HashKind::ERROR),
            ] {
                ctx.charge(1)?;
                if !kind.has(bit) {
                    continue;
                }
                let protected = facts.protected_variant(ctx, receiver, tag)?;
                if protected != Atom::Never.fact() {
                    variants.push(ctx, protected)?;
                }
            }
            return Ok(Some(variants));
        }
    }
    let Node::Shape(fields, _, _, HashKind::OBJECT) = facts.node(receiver) else {
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
    /// The field may be present with this value, or absent. Absence dispatches
    /// natively for builtin names and fails for other or scoped lookups; see
    /// [`absent_is_native`].
    Uncertain(Fact),
    Missing,
}

/// Whether a member absent from an object dispatches natively instead of failing.
pub(super) fn absent_is_native(site: CallSite, name: &str) -> bool {
    !site.scope && (names::universal(name) || names::Receiver::Hash.available(name))
}

fn callable(facts: &Facts, value: Fact) -> bool {
    matches!(
        facts.node(value),
        Node::Builtin(_) | Node::Offset(_) | Node::Callable { .. }
    )
}

/// Resolves a hash's stored field before native method dispatch.
///
/// Plain hashes only read fields for non-builtin names and reject scoped
/// access; object fields override builtins except non-callable universal
/// helpers. Uncertain provenance, including possibly protected contracts,
/// must be split by [`variants`] first when the name is provenance-sensitive;
/// otherwise both provenances agree. Protected alternatives are not selected
/// here: their members dispatch through the protected builtin summaries.
pub(super) fn select(
    ctx: &mut CallContext,
    facts: &Facts,
    receiver: Fact,
    site: CallSite,
    name: &str,
) -> Result<Option<Selection>> {
    ctx.charge(1)?;
    let (field, open, kind) = match facts.node(receiver) {
        Node::Hash(_, value, kind) => (Some((*value, true)), true, *kind),
        Node::Shape(_, open, _, kind) => (
            facts.selected_field(ctx, receiver, name.as_bytes())?,
            *open,
            *kind,
        ),
        _ => return Ok(None),
    };
    if !kind.object() {
        if site.scope {
            return Ok(Some(Selection::Missing));
        }
        if crate::members::hash_builtin(name) {
            return Ok(Some(Selection::Native));
        }
    }
    // Non-callable data cannot override a universal helper, except the
    // block-taking helpers that always prefer a stored field.
    let data_safe = !site.scope && names::universal(name) && !matches!(name, "tap" | "yield_self");
    let selected = match field {
        Some((field, optional)) => {
            if data_safe && facts.known_non_callable(ctx, field)? {
                Selection::Native
            } else if optional || (data_safe && !callable(facts, field)) {
                Selection::Uncertain(field)
            } else {
                Selection::Field(field)
            }
        }
        None if open => Selection::Uncertain(Atom::Unknown.fact()),
        None if absent_is_native(site, name) => Selection::Native,
        None => Selection::Missing,
    };
    Ok(Some(selected))
}
