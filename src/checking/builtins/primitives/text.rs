use super::*;

pub(super) fn supported(name: &str) -> bool {
    matches!(
        name,
        "ord"
            | "chr"
            | "start_with?"
            | "end_with?"
            | "split"
            | "index"
            | "rindex"
            | "hex"
            | "oct"
            | "clamp"
            | "between?"
            | "to_sym"
            | "intern"
            | "to_s"
            | "string"
            | "to_i"
            | "to_f"
            | "inspect"
            | "casecmp"
            | "casecmp?"
            | "upcase"
            | "upcase!"
            | "downcase"
            | "downcase!"
            | "capitalize"
            | "capitalize!"
            | "swapcase"
            | "swapcase!"
            | "center"
            | "ljust"
            | "rjust"
            | "partition"
            | "rpartition"
            | "strip"
            | "strip!"
            | "lstrip"
            | "lstrip!"
            | "rstrip"
            | "rstrip!"
            | "squish"
            | "squish!"
            | "chomp"
            | "chomp!"
            | "chop"
            | "chop!"
            | "delete_prefix"
            | "delete_prefix!"
            | "delete_suffix"
            | "delete_suffix!"
            | "reverse!"
            | "count"
            | "delete"
            | "delete!"
            | "tr"
            | "tr!"
            | "squeeze"
            | "squeeze!"
    )
}

pub(super) fn arity(name: &str, count: usize) -> bool {
    match name.trim_end_matches('!') {
        "start_with?" | "end_with?" | "count" | "delete" => count >= 1,
        "squeeze" => true,
        "split" => count <= 2,
        "center" | "ljust" | "rjust" | "index" | "rindex" => (1..=2).contains(&count),
        "upcase" | "downcase" | "capitalize" | "swapcase" | "chomp" => count <= 1,
        "clamp" | "between?" | "tr" => count == 2,
        "casecmp" | "casecmp?" | "partition" | "rpartition" | "delete_prefix" | "delete_suffix" => {
            count == 1
        }
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
    let mut result = outcome(Atom::Never.fact());
    let positional = &args.positional.data;
    let string = Atom::String.fact();
    let mut possible = true;
    let base = name.trim_end_matches('!');
    let value = match base {
        "start_with?" | "end_with?" => return affixes(ctx, facts, receiver, name, positional),
        "casecmp" | "casecmp?" => {
            let mut returns = Buffer::empty();
            for i in 0..facts.arm_count(positional[0]) {
                ctx.charge(1)?;
                let arm = facts.arm(positional[0], i);
                let relation = facts.relation(ctx, arm, string)?;
                if relation != Relation::Rejected {
                    returns.push(
                        ctx,
                        if name == "casecmp" {
                            Atom::Int.fact()
                        } else {
                            Atom::Bool.fact()
                        },
                    )?;
                }
                if relation != Relation::Accepted {
                    returns.push(ctx, Atom::Nil.fact())?;
                }
            }
            facts.union(ctx, &returns.data)?
        }
        "upcase" | "downcase" | "capitalize" | "swapcase" => {
            if let Some(&mode) = positional.first() {
                possible &= parameter(ctx, facts, &mut result, 0, mode, Atom::Symbol.fact())?;
                possible &= domain(ctx, facts, &mut result, mode, |ctx, facts, value| {
                    Ok(if let Node::Symbol(value) = facts.node(value) {
                        let bytes = value.as_bytes().unwrap();
                        ctx.work_bytes(bytes.len())?;
                        Some(bytes == b"ascii" || base == "downcase" && bytes == b"fold")
                    } else {
                        None
                    })
                })?;
            }
            string
        }
        "index" | "rindex" => {
            let mut may_skip = false;
            if let Some(&offset) = positional.get(1) {
                let numeric = facts.union(ctx, &[Atom::Int.fact(), Atom::Float.fact()])?;
                possible &= parameter(ctx, facts, &mut result, 1, offset, numeric)?;
                possible &= domain(ctx, facts, &mut result, offset, machine_integer)?;
                if base == "rindex" && possible {
                    let number = match facts.node(offset) {
                        Node::Integer(value) => Some(*value),
                        Node::Float(bits) => {
                            crate::sequence::integer(&Value::float(f64::from_bits(*bits))).ok()
                        }
                        _ => None,
                    };
                    may_skip = number.is_none_or(|n| n < 0);
                    if let (Some(n), Node::String(text)) = (number, facts.node(receiver)) {
                        if n < 0
                            && i128::from(n)
                                + (crate::ops::runes(ctx, text.as_bytes().unwrap())?.0 as i128)
                                < 0
                        {
                            result.value = Atom::Nil.fact();
                            return Ok(result);
                        }
                    }
                }
            }
            let needle = parameter(ctx, facts, &mut result, 0, positional[0], string)?;
            possible &= needle || may_skip;
            if needle {
                facts.nullable(ctx, Atom::Int.fact())?
            } else {
                Atom::Nil.fact()
            }
        }
        "center" | "ljust" | "rjust" => {
            let numeric = facts.union(ctx, &[Atom::Int.fact(), Atom::Float.fact()])?;
            possible &= parameter(ctx, facts, &mut result, 0, positional[0], numeric)?;
            possible &= domain(ctx, facts, &mut result, positional[0], machine_integer)?;
            if let Some(&pad) = positional.get(1) {
                possible &= parameter(ctx, facts, &mut result, 1, pad, string)?;
                possible &= domain(ctx, facts, &mut result, pad, |_, facts, value| {
                    Ok(if let Node::String(value) = facts.node(value) {
                        Some(!value.as_bytes().unwrap().is_empty())
                    } else {
                        None
                    })
                })?;
            }
            string
        }
        "clamp" => {
            let bound = facts.nullable(ctx, string)?;
            for (index, &value) in positional.iter().enumerate() {
                possible &= parameter(ctx, facts, &mut result, index, value, bound)?;
            }
            result.throws |= RUNTIME;
            string
        }
        "between?" => return between(ctx, facts, receiver, positional),
        "split" => {
            if let Some(&separator) = positional.first() {
                let expected = facts.nullable(ctx, string)?;
                possible &= parameter(ctx, facts, &mut result, 0, separator, expected)?;
            }
            if let Some(&limit) = positional.get(1) {
                possible &= parameter(ctx, facts, &mut result, 1, limit, Atom::Int.fact())?;
                possible &= domain(ctx, facts, &mut result, limit, machine_integer)?;
            }
            facts.array(ctx, string)?
        }
        "chomp" => {
            if let Some(&separator) = positional.first() {
                let expected = facts.nullable(ctx, string)?;
                possible &= parameter(ctx, facts, &mut result, 0, separator, expected)?;
            }
            string
        }
        "partition" | "rpartition" | "delete_prefix" | "delete_suffix" | "count" | "delete"
        | "tr" | "squeeze" => {
            for (index, &value) in positional.iter().enumerate() {
                possible &= parameter(ctx, facts, &mut result, index, value, string)?;
            }
            if matches!(base, "count" | "delete" | "tr" | "squeeze") {
                result.throws |= RUNTIME;
            }
            match base {
                "count" => Atom::Int.fact(),
                "partition" | "rpartition" => facts.tuple(ctx, &[string; 3])?,
                _ => string,
            }
        }
        "ord" | "to_i" | "hex" | "oct" => {
            result.throws |= RUNTIME | LIMIT;
            Atom::Int.fact()
        }
        "to_f" => {
            result.throws |= RUNTIME;
            Atom::Float.fact()
        }
        "to_sym" | "intern" => Atom::Symbol.fact(),
        _ => string,
    };
    if possible {
        result.value = if name.ends_with('!') {
            facts.nullable(ctx, value)?
        } else {
            value
        };
    }
    Ok(result)
}

fn affixes(
    ctx: &mut CallContext,
    facts: &mut Facts,
    receiver: Fact,
    name: &str,
    args: &[Fact],
) -> Result<Outcome> {
    let mut result = outcome(Atom::Never.fact());
    let mut positive = false;
    let mut negative = true;
    for (index, &argument) in args.iter().enumerate() {
        if !negative {
            break;
        }
        let possible = parameter(
            ctx,
            facts,
            &mut result,
            index,
            argument,
            Atom::String.fact(),
        )?;
        let mut next = false;
        for i in 0..facts.arm_count(argument) {
            ctx.charge(1)?;
            let arm = facts.arm(argument, i);
            if facts.relation(ctx, arm, Atom::String.fact())? == Relation::Rejected {
                continue;
            }
            let known = match (facts.node(receiver), facts.node(arm)) {
                (Node::String(text), Node::String(part)) => {
                    let method = if name == "start_with?" {
                        crate::bytecode::Method::StartWith
                    } else {
                        crate::bytecode::Method::EndWith
                    };
                    let value =
                        crate::text::method(ctx, method, text.clone(), std::slice::from_ref(part))?;
                    Some(value.truthy())
                }
                (_, Node::String(part)) if part.as_bytes().unwrap().is_empty() => Some(true),
                _ => None,
            };
            positive |= known != Some(false);
            next |= known != Some(true);
        }
        negative = possible && next;
    }
    result.value = match (positive, negative) {
        (true, true) => Atom::Bool.fact(),
        (true, false) => facts.boolean(ctx, true)?,
        (false, true) => facts.boolean(ctx, false)?,
        (false, false) => Atom::Never.fact(),
    };
    Ok(result)
}
