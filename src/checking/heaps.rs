use super::{
    facts::{Atom, Fact, Facts, Node},
    scalar::Operation,
};
use crate::{CallContext, Result, budget::Buffer};

/// Coalesces equally sized alternatives without losing their object positions.
pub(super) fn entries(
    ctx: &mut CallContext,
    facts: &mut Facts,
    heap: Fact,
) -> Result<Option<Buffer<Fact>>> {
    let mut entries: Option<Buffer<Fact>> = None;
    for i in 0..facts.arm_count(heap) {
        ctx.charge(1)?;
        let Node::Tuple(values) = facts.node(facts.arm(heap, i)) else {
            return Ok(None);
        };
        let mut copied = Buffer::empty();
        copied.extend(ctx, &values.data)?;
        if let Some(entries) = &mut entries {
            if entries.data.len() != copied.data.len() {
                return Ok(None);
            }
            for (a, b) in entries.data.iter_mut().zip(copied.data) {
                ctx.charge(1)?;
                *a = facts.union(ctx, &[*a, b])?;
            }
        } else {
            entries = Some(copied);
        }
    }
    Ok(entries)
}

/// An instance reference proves the selected object exists even after heap widening.
pub(super) fn read(
    ctx: &mut CallContext,
    facts: &mut Facts,
    heap: Fact,
    index: Fact,
) -> Result<Operation> {
    let mut values = Buffer::empty();
    let mut unsupported = false;
    for i in 0..facts.arm_count(heap) {
        ctx.charge(1)?;
        match facts.node(facts.arm(heap, i)) {
            Node::Tuple(entries) => {
                if let Node::Integer(index) = facts.node(index) {
                    if let Ok(index) = usize::try_from(*index) {
                        if let Some(&value) = entries.data.get(index) {
                            values.push(ctx, value)?;
                        }
                    }
                } else {
                    values.extend(ctx, &entries.data)?;
                }
            }
            Node::Array(element) => values.push(ctx, *element)?,
            Node::Atom(Atom::Never) => (),
            _ => unsupported = true,
        }
    }
    Ok(Operation {
        value: facts.union(ctx, &values.data)?,
        rejected: false,
        unsupported,
    })
}
