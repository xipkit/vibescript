use super::*;

pub(super) fn supported(name: &str) -> bool {
    matches!(
        name,
        "abs"
            | "even?"
            | "odd?"
            | "zero?"
            | "positive?"
            | "negative?"
            | "nonzero?"
            | "nan?"
            | "finite?"
            | "infinite?"
            | "next"
            | "succ"
            | "pred"
            | "round"
            | "floor"
            | "ceil"
            | "div"
            | "divmod"
            | "fdiv"
            | "remainder"
            | "modulo"
            | "clamp"
            | "between?"
            | "to_s"
            | "string"
            | "inspect"
            | "to_i"
            | "to_f"
    )
}

pub(super) fn arity(name: &str, count: usize) -> bool {
    match name {
        "round" | "floor" | "ceil" => count <= 1,
        "clamp" => (1..=2).contains(&count),
        "between?" => count == 2,
        "div" | "divmod" | "fdiv" | "remainder" | "modulo" => count == 1,
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
    let kind = facts.atom(receiver).unwrap();
    if (kind == Atom::Float && matches!(name, "even?" | "odd?" | "next" | "succ" | "pred"))
        || (kind == Atom::Int && matches!(name, "nan?" | "finite?" | "infinite?"))
    {
        return reject(ctx, Failure::Undefined);
    }
    let mut result = outcome(Atom::Never.fact());
    let numeric = facts.union(ctx, &[Atom::Int.fact(), Atom::Float.fact()])?;
    let positional = &args.positional.data;
    let mut possible = true;
    let value = match name {
        "zero?" | "positive?" | "negative?" | "nan?" | "finite?" | "even?" | "odd?" => {
            Atom::Bool.fact()
        }
        "between?" => return between(ctx, facts, receiver, positional),
        "nonzero?" => facts.nullable(ctx, receiver)?,
        "infinite?" => facts.nullable(ctx, Atom::Int.fact())?,
        "abs" | "next" | "succ" | "pred" => {
            if kind == Atom::Int {
                result.throws |= LIMIT;
            }
            kind.fact()
        }
        "round" | "floor" | "ceil" => {
            let precision = positional.first().copied();
            if let Some(precision) = precision {
                possible &= parameter(ctx, facts, &mut result, 0, precision, Atom::Int.fact())?;
                possible &= domain(ctx, facts, &mut result, precision, |_, facts, value| {
                    Ok(match facts.node(value) {
                        Node::Integer(value) => Some(i32::try_from(*value).is_ok()),
                        _ => None,
                    })
                })?;
            }
            result.throws |= LIMIT;
            if kind == Atom::Int {
                Atom::Int.fact()
            } else {
                let mut integer = precision.is_none();
                let mut float = false;
                if let Some(precision) = precision {
                    for i in 0..facts.arm_count(precision) {
                        ctx.charge(1)?;
                        match facts.node(facts.arm(precision, i)) {
                            Node::Integer(value) => {
                                integer |= *value <= 0;
                                float |= *value > 0;
                            }
                            _ => {
                                integer = true;
                                float = true;
                            }
                        }
                    }
                }
                if integer {
                    result.throws |= RUNTIME;
                }
                match (integer, float) {
                    (true, false) => Atom::Int.fact(),
                    (false, true) => Atom::Float.fact(),
                    _ => numeric,
                }
            }
        }
        "div" | "divmod" | "fdiv" | "remainder" | "modulo" => {
            let divisor = positional[0];
            possible &= parameter(ctx, facts, &mut result, 0, divisor, numeric)?;
            let mut integer = false;
            let mut float = false;
            let mut nonzero = false;
            for i in 0..facts.arm_count(divisor) {
                ctx.charge(1)?;
                let arm = facts.arm(divisor, i);
                if facts.relation(ctx, arm, numeric)? == Relation::Rejected {
                    continue;
                }
                let zero = match facts.node(arm) {
                    Node::Integer(value) => Some(*value == 0),
                    Node::Float(bits) => Some(f64::from_bits(*bits) == 0.0),
                    _ => None,
                };
                nonzero |= zero != Some(true);
                if name != "fdiv" && zero != Some(false) {
                    result.throws |= ZERO;
                }
                integer |= kind == Atom::Int && facts.atom(arm) != Some(Atom::Float);
                float |= kind == Atom::Float || facts.atom(arm) != Some(Atom::Int);
            }
            if name != "fdiv" && !nonzero && possible {
                result.failures.push(ctx, Failure::BuiltinDomain(divisor))?;
                possible = false;
            }
            if name == "fdiv" {
                Atom::Float.fact()
            } else {
                result.throws |= LIMIT;
                if float && matches!(name, "div" | "divmod") {
                    result.throws |= RUNTIME;
                }
                let remainder = match (integer, float) {
                    (true, false) => Atom::Int.fact(),
                    (false, true) => Atom::Float.fact(),
                    _ => numeric,
                };
                match name {
                    "div" => Atom::Int.fact(),
                    "divmod" => facts.tuple(ctx, &[Atom::Int.fact(), remainder])?,
                    _ => remainder,
                }
            }
        }
        "clamp" => {
            result.throws |= RUNTIME;
            if positional.len() == 1 {
                let range = positional[0];
                possible &= parameter(ctx, facts, &mut result, 0, range, Atom::Range.fact())?;
                possible &= domain(ctx, facts, &mut result, range, |_, facts, value| {
                    Ok(match facts.node(value) {
                        Node::Range(start, end, exclusive) => {
                            Some(!exclusive && !matches!((start, end), (Some(a), Some(b)) if a > b))
                        }
                        _ => None,
                    })
                })?;
                facts.union(ctx, &[receiver, Atom::Int.fact()])?
            } else {
                let bound = facts.nullable(ctx, numeric)?;
                for (index, &value) in positional.iter().enumerate() {
                    possible &= parameter(ctx, facts, &mut result, index, value, bound)?;
                }
                let mut returns = Buffer::empty();
                returns.push(ctx, receiver)?;
                for &value in positional {
                    for i in 0..facts.arm_count(value) {
                        ctx.charge(1)?;
                        let arm = facts.arm(value, i);
                        if facts.relation(ctx, arm, numeric)? != Relation::Rejected {
                            returns.push(ctx, arm)?;
                        }
                    }
                }
                facts.union(ctx, &returns.data)?
            }
        }
        "to_s" | "string" | "inspect" => Atom::String.fact(),
        "to_f" => Atom::Float.fact(),
        "to_i" => {
            if kind == Atom::Float {
                result.throws |= RUNTIME | LIMIT;
            }
            Atom::Int.fact()
        }
        _ => unreachable!(),
    };
    if possible {
        result.value = value;
    }
    Ok(result)
}
