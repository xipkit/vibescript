use super::*;

pub(super) fn supported(facts: &Facts, value: Fact) -> bool {
    matches!(
        facts.node(value),
        Node::Enumeration { .. } | Node::EnumMember { .. }
    )
}

fn reject(ctx: &mut CallContext, failure: Failure) -> Result<Outcome> {
    let mut result = outcome(Atom::Never.fact());
    result.failures.push(ctx, failure)?;
    result.throws = RUNTIME;
    Ok(result)
}

pub(super) fn member(
    ctx: &mut CallContext,
    facts: &mut Facts,
    receiver: Fact,
    site: CallSite,
    name: &str,
    args: &Arguments,
) -> Result<Outcome> {
    let (parent, index) = match facts.node(receiver) {
        Node::Enumeration { .. } => (receiver, None),
        Node::EnumMember { enumeration, index } => (*enumeration, Some(*index)),
        _ => unreachable!(),
    };
    let Node::Enumeration { value, .. } = facts.node(parent) else {
        unreachable!()
    };
    let Kind::Enum(value) = &value.0 else {
        unreachable!()
    };
    let value = value.clone();
    if site.scope {
        if index.is_some() {
            return reject(ctx, Failure::BuiltinDomain(receiver));
        }
        let Some(index) = value.lookup(ctx, name.as_bytes())? else {
            return reject(ctx, Failure::Undefined);
        };
        if !site.auto {
            return reject(ctx, Failure::NonCallable);
        }
        return Ok(outcome(facts.enum_member(ctx, parent, index)?));
    }
    if matches!(name, "to_s" | "string" | "inspect") {
        if !args.positional.data.is_empty() {
            return reject(ctx, Failure::BuiltinArity);
        }
        if !args.keywords.data.is_empty() {
            return reject(ctx, Failure::BuiltinKeywords);
        }
        if args.block.is_some() {
            return reject(ctx, Failure::BuiltinBlock);
        }
        let mut bytes = Buffer::empty();
        if index.is_none() {
            bytes.extend(ctx, b"<Enum ")?;
        }
        bytes.extend(ctx, value.definition.name.as_bytes())?;
        if let Some(index) = index {
            bytes.extend(ctx, b"::")?;
            bytes.extend(ctx, value.definition.members[index].name.as_bytes())?;
        } else {
            bytes.extend(ctx, b">")?;
        }
        return Ok(outcome(facts.string(ctx, &bytes.data)?));
    }
    let property = match (name, index) {
        ("name", None) => facts.string(ctx, value.definition.name.as_bytes())?,
        ("name", Some(index)) => {
            facts.string(ctx, value.definition.members[index].name.as_bytes())?
        }
        ("symbol", Some(index)) => {
            facts.symbol(ctx, value.definition.members[index].symbol.as_bytes())?
        }
        ("enum", Some(_)) => parent,
        _ => return reject(ctx, Failure::Undefined),
    };
    if !site.auto {
        return reject(ctx, Failure::NonCallable);
    }
    Ok(outcome(property))
}
