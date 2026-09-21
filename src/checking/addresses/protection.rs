use super::*;
use crate::checking::facts::{Field, HashKind};

impl Address {
    /// Refines an attached root to the alternatives that protect this selection.
    /// This observes existing storage; it never models a successful write.
    pub fn protected_root(&self, ctx: &mut CallContext, facts: &mut Facts) -> Result<Option<Fact>> {
        if self.root.is_none() || self.attached != Attached::Yes {
            return Ok(None);
        }
        let mut value = protected_alternatives(ctx, facts, self.value)?;
        for hop in self.path.data.iter().rev() {
            ctx.charge(1)?;
            let ancestor = protected_alternatives(ctx, facts, hop.container)?;
            if value != Atom::Never.fact() {
                value = if hop.instance {
                    // Instance fields have shared heap identity, not value storage.
                    hop.container
                } else {
                    selected_parent(ctx, facts, hop.container, hop.key, value)?
                };
            }
            value = facts.union(ctx, &[ancestor, value])?;
        }
        Ok((value != Atom::Never.fact()).then_some(value))
    }
}

fn protected_alternatives(
    ctx: &mut CallContext,
    facts: &mut Facts,
    receiver: Fact,
) -> Result<Fact> {
    let mut alternatives = Buffer::empty();
    for i in 0..facts.arm_count(receiver) {
        ctx.charge(1)?;
        let arm = facts.arm(receiver, i);
        let kind = match facts.node(arm) {
            Node::Protected(..) => {
                alternatives.push(ctx, arm)?;
                continue;
            }
            Node::Hash(_, _, kind) | Node::Shape(_, _, _, kind) => *kind,
            _ => continue,
        };
        for (tag, bit) in [
            (crate::hash::Tag::Match, HashKind::MATCH),
            (crate::hash::Tag::Error, HashKind::ERROR),
        ] {
            ctx.charge(1)?;
            if kind.has(bit) {
                let variant = facts.protected_variant(ctx, arm, tag)?;
                alternatives.push(ctx, variant)?;
            }
        }
    }
    facts.union(ctx, &alternatives.data)
}

fn selected_parent(
    ctx: &mut CallContext,
    facts: &mut Facts,
    parent: Fact,
    key: Fact,
    value: Fact,
) -> Result<Fact> {
    let mut alternatives = Buffer::empty();
    for i in 0..facts.arm_count(parent) {
        ctx.charge(1)?;
        let original = facts.arm(parent, i);
        let (arm, protection) = match facts.node(original) {
            Node::Protected(shape, tag, certainty) => (*shape, Some((*tag, *certainty))),
            _ => (original, None),
        };
        let selected = match (facts.node(arm), facts.node(key)) {
            (Node::Shape(fields, open, keys, kind), Node::String(name) | Node::Symbol(name)) => {
                let (open, keys, kind, name) = (*open, *keys, *kind, name.clone());
                let mut copied = Buffer::empty();
                for field in &fields.data {
                    ctx.charge(1)?;
                    copied.push(
                        ctx,
                        Field {
                            name: field.name.clone(),
                            value: field.value,
                            optional: field.optional,
                        },
                    )?;
                }
                let mut selected = false;
                let mut possible = true;
                for field in &mut copied.data {
                    ctx.charge(1)?;
                    if same_bytes(ctx, &field.name, &name)? {
                        possible = facts.overlaps(ctx, field.value, value)?;
                        field.value = value;
                        field.optional = false;
                        selected = true;
                        break;
                    }
                }
                if !possible {
                    Atom::Never.fact()
                } else if selected {
                    facts.shape_fields(ctx, copied, open, keys, kind)?
                } else {
                    arm
                }
            }
            (Node::Tuple(elements), Node::Integer(index)) => {
                let at = if *index < 0 {
                    *index as i128 + elements.data.len() as i128
                } else {
                    *index as i128
                };
                if at < 0 || at >= elements.data.len() as i128 {
                    Atom::Never.fact()
                } else {
                    let at = at as usize;
                    let mut copied = Buffer::empty();
                    copied.extend(ctx, &elements.data)?;
                    if facts.overlaps(ctx, copied.data[at], value)? {
                        copied.data[at] = value;
                        facts.tuple(ctx, &copied.data)?
                    } else {
                        Atom::Never.fact()
                    }
                }
            }
            _ => arm,
        };
        let selected = if let Some((tag, certainty)) = protection {
            if selected == Atom::Never.fact() {
                selected
            } else {
                facts.protected_as(ctx, selected, tag, certainty)?
            }
        } else {
            selected
        };
        alternatives.push(ctx, selected)?;
    }
    facts.union(ctx, &alternatives.data)
}
