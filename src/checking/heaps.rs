use super::{
    facts::{Atom, Fact, Facts, Node},
    scalar::Operation,
};
use crate::{CallContext, Result, budget::Buffer};

/// Coalesces heap alternatives into one list of entries without losing object positions. A
/// shorter alternative has not allocated the later objects on its path, so no reference to
/// them exists there.
pub(super) fn entries(
    ctx: &mut CallContext,
    facts: &mut Facts,
    heap: Fact,
) -> Result<Option<Buffer<Fact>>> {
    let mut entries: Option<Buffer<Fact>> = None;
    for i in 0..facts.arm_count(heap) {
        ctx.charge(1)?;
        let arm = facts.arm(heap, i);
        if arm == Atom::Never.fact() {
            continue;
        }
        let Node::Tuple(values) = facts.node(arm) else {
            return Ok(None);
        };
        let mut copied = Buffer::empty();
        copied.extend(ctx, &values.data)?;
        let Some(entries) = &mut entries else {
            entries = Some(copied);
            continue;
        };
        for (index, value) in copied.data.into_iter().enumerate() {
            ctx.charge(1)?;
            if let Some(entry) = entries.data.get_mut(index) {
                *entry = facts.union(ctx, &[*entry, value])?;
            } else {
                entries.push(ctx, value)?;
            }
        }
    }
    Ok(Some(entries.unwrap_or_else(Buffer::empty)))
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
        throws: false,
    })
}
