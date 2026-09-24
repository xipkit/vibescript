use super::*;

/// Range members whose runtime entry is the native range method table. They
/// reject keywords, ignore an attached block and never invoke script code.
pub(super) fn supported(name: &str) -> bool {
    matches!(
        name,
        "first"
            | "last"
            | "length"
            | "size"
            | "to_a"
            | "include?"
            | "cover?"
            | "member?"
            | "exclude_end?"
    )
}

pub(super) fn arity(name: &str, count: usize) -> bool {
    match name {
        "first" | "last" => count <= 1,
        "include?" | "cover?" | "member?" => count == 1,
        _ => count == 0,
    }
}

pub(super) fn member(
    ctx: &mut CallContext,
    facts: &mut Facts,
    receiver: Fact,
    name: &str,
    args: &Arguments,
) -> Result<Outcome> {
    let operation = facts.range_member(ctx, receiver, name, &args.positional.data)?;
    let mut result = outcome(operation.value);
    result.incomplete = operation.unsupported;
    if operation.rejected {
        result
            .failures
            .push(ctx, Failure::BuiltinDomain(receiver))?;
    }
    if operation.rejected || operation.throws {
        result.throws |= RUNTIME;
    }
    if name == "to_a" && facts.range_array_limit(receiver) {
        result.throws |= LIMIT;
    }
    Ok(result)
}
