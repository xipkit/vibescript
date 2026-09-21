use super::*;
use crate::members::{
    introspection::{Predicate, Query, method_name},
    names::{self, Receiver},
};

pub(super) fn member(
    ctx: &mut CallContext,
    facts: &mut Facts,
    receiver: Fact,
    predicate: Predicate,
    args: &Arguments,
) -> Result<Outcome> {
    let actual = args.positional.data[0];
    let mut result = outcome(Atom::Never.fact());
    if !matches!(predicate, Predicate::Respond | Predicate::IsType) {
        let mut possible = false;
        let mut invalid = false;
        for i in 0..facts.arm_count(actual) {
            ctx.charge(1)?;
            let value = facts.arm(actual, i);
            if value == Atom::Never.fact() {
                continue;
            }
            if let Node::TypeValue(class) = *facts.node(value) {
                if matches!(facts.node(class), Node::Nominal { symbols: None, .. }) {
                    let belongs = match facts.node(receiver) {
                        Node::Instance { class: actual, .. } => {
                            facts.same_nominal(ctx, *actual, class)?
                        }
                        _ => false,
                    };
                    let value = facts.boolean(ctx, belongs)?;
                    result.value = facts.union(ctx, &[result.value, value])?;
                    continue;
                }
            }
            let unknown = matches!(
                facts.node(value),
                Node::Atom(Atom::Unknown | Atom::Any) | Node::Named(_) | Node::Nominal { .. }
            ) || matches!(
                facts.node(value),
                Node::Hash(_, _, kind) | Node::Shape(_, _, _, kind) if !kind.plain()
            ) && !namespace(ctx, facts, value)?;
            possible |= unknown;
            invalid |= !unknown;
            result.throws |= RUNTIME;
        }
        if possible {
            let value = if matches!(facts.node(receiver), Node::Instance { .. }) {
                Atom::Bool.fact()
            } else {
                facts.boolean(ctx, false)?
            };
            result.value = facts.union(ctx, &[result.value, value])?;
        }
        if invalid {
            result.failures.push(ctx, Failure::BuiltinDomain(actual))?;
        }
        return Ok(result);
    }
    let expected = facts.union(ctx, &[Atom::String.fact(), Atom::Symbol.fact()])?;
    let mut possible = parameter(ctx, facts, &mut result, 0, actual, expected)?;
    if let Some(&private) = args.positional.data.get(1) {
        possible &= parameter(ctx, facts, &mut result, 1, private, Atom::Bool.fact())?;
    }
    if !possible {
        return Ok(result);
    }
    for i in 0..facts.arm_count(actual) {
        ctx.charge(1)?;
        let arm = facts.arm(actual, i);
        if facts.relation(ctx, arm, expected)? == Relation::Rejected {
            continue;
        }
        let known = match facts.node(arm) {
            Node::String(value) | Node::Symbol(value) => Some(value.clone()),
            _ => None,
        };
        let value = if let Some(value) = known {
            if predicate == Predicate::Respond {
                responds(ctx, facts, receiver, value.as_bytes().unwrap())?
            } else {
                match Predicate::IsType.validate(ctx, std::slice::from_ref(&value), false, false) {
                    Ok(Query::Type(atom)) => {
                        if let Some(matches) =
                            atom.native_match(super::receiver(facts, receiver).unwrap())
                        {
                            facts.boolean(ctx, matches)?
                        } else {
                            result.incomplete = true;
                            Atom::Never.fact()
                        }
                    }
                    Ok(_) => unreachable!(),
                    Err(error) => {
                        ctx.checkpoint()?;
                        let Some(class) = error.class() else {
                            return Err(error);
                        };
                        result.throws |= 1 << class as u8;
                        result.failures.push(ctx, Failure::BuiltinDomain(arm))?;
                        Atom::Never.fact()
                    }
                }
            }
        } else if predicate == Predicate::Respond {
            Atom::Bool.fact()
        } else {
            // A nonliteral atom can require lexical class/module resolution.
            result.incomplete = true;
            result.throws |= RUNTIME;
            Atom::Never.fact()
        };
        result.value = facts.union(ctx, &[result.value, value])?;
    }
    Ok(result)
}

fn callable(ctx: &mut CallContext, facts: &mut Facts, value: Fact) -> Result<Fact> {
    let mut result = Atom::Never.fact();
    for i in 0..facts.arm_count(value) {
        ctx.charge(1)?;
        let arm = facts.arm(value, i);
        let next = match facts.node(arm) {
            Node::Atom(Atom::Never) => continue,
            Node::Builtin(_) | Node::Offset(_) => facts.boolean(ctx, true)?,
            Node::Atom(Atom::Unknown | Atom::Any)
            | Node::Named(_)
            | Node::Nominal { .. }
            | Node::Instance { .. } => Atom::Bool.fact(),
            _ => facts.boolean(ctx, false)?,
        };
        result = facts.union(ctx, &[result, next])?;
    }
    Ok(result)
}

fn responds(
    ctx: &mut CallContext,
    facts: &mut Facts,
    receiver: Fact,
    bytes: &[u8],
) -> Result<Fact> {
    let name = method_name(ctx, bytes)?;
    let universal = name.is_some_and(names::universal);
    if universal && !matches!(name, Some("tap" | "yield_self")) {
        return facts.boolean(ctx, true);
    }
    let kind = super::receiver(facts, receiver).unwrap();
    let view = if let Node::Protected(shape, ..) = facts.node(receiver) {
        *shape
    } else {
        receiver
    };
    let plain = receiver != view || facts.plain_hash(view);
    if kind == Receiver::Hash && plain && name.is_some_and(|n| kind.typed(n).is_some()) {
        return facts.boolean(ctx, true);
    }
    let available = universal || name.is_some_and(|n| kind.available(n));
    let mut result = Atom::Never.fact();
    let mut absent = true;
    match facts.node(view) {
        Node::Shape(_, open, _, _) => {
            let open = *open;
            if let Some((value, optional)) = facts.selected_field(ctx, view, bytes)? {
                absent = optional;
                result = callable(ctx, facts, value)?;
            } else if open {
                result = Atom::Bool.fact();
            }
        }
        Node::Hash(_, values, _) => {
            result = callable(ctx, facts, *values)?;
        }
        _ => (),
    }
    if absent {
        let fallback = if matches!(facts.node(receiver), Node::Atom(Atom::Int))
            && available != (universal || name.is_some_and(|n| Receiver::Big.available(n)))
        {
            Atom::Bool.fact()
        } else {
            facts.boolean(ctx, available)?
        };
        result = facts.union(ctx, &[result, fallback])?;
    }
    Ok(result)
}
