use super::*;
use crate::{checking::scalar::Operation, hash::Tag};

fn reject(ctx: &mut CallContext, failure: Failure) -> Result<Outcome> {
    let mut result = outcome(Atom::Never.fact());
    result.failures.push(ctx, failure)?;
    result.throws = RUNTIME;
    Ok(result)
}

fn fields(ctx: &mut CallContext, facts: &mut Facts, entries: &[(&str, Fact)]) -> Result<Fact> {
    let mut fields = Buffer::with_capacity(ctx, entries.len())?;
    for &(name, value) in entries {
        let name = ctx.bytes(name.as_bytes())?;
        fields.push(
            ctx,
            Field {
                name,
                value,
                optional: false,
            },
        )?;
    }
    facts.shape_fields(ctx, fields, false, Atom::String.fact(), true)
}

pub(super) fn match_data(ctx: &mut CallContext, facts: &mut Facts, regex: Fact) -> Result<Fact> {
    let string = Atom::String.fact();
    let capture = facts.nullable(ctx, string)?;
    let offset = facts.nullable(ctx, Atom::Int.fact())?;
    let literal = if let Node::Regex(value) = facts.node(regex) {
        Some(value.clone())
    } else {
        None
    };
    let (captures, named, offsets) = if let Some(value) = literal {
        let Kind::Regex(regex) = &value.0 else {
            unreachable!()
        };
        let (source, names) = regex.capture_names();
        let mut captures = Buffer::with_capacity(ctx, names.len() - 1)?;
        let mut offsets = Buffer::with_capacity(ctx, names.len())?;
        let mut fields = Buffer::empty();
        offsets.push(ctx, Atom::Int.fact())?;
        for &(start, end) in &names[1..] {
            captures.push(ctx, capture)?;
            offsets.push(ctx, offset)?;
            if start != end {
                let name = ctx.bytes(&source[start..end])?;
                fields.push(
                    ctx,
                    Field {
                        name,
                        value: capture,
                        optional: false,
                    },
                )?;
            }
        }
        (
            facts.tuple(ctx, &captures.data)?,
            facts.shape_fields(ctx, fields, false, string, true)?,
            facts.tuple(ctx, &offsets.data)?,
        )
    } else {
        (
            facts.array(ctx, capture)?,
            facts.hash_kind(ctx, string, capture, true)?,
            facts.array(ctx, offset)?,
        )
    };
    let offset = facts.offset(ctx, offsets)?;
    let shape = fields(
        ctx,
        facts,
        &[
            ("begin", offset),
            ("captures", captures),
            ("end", offset),
            ("named_captures", named),
            ("post_match", string),
            ("pre_match", string),
            ("to_s", string),
        ],
    )?;
    facts.protected(ctx, shape, Tag::Match)
}

pub(in crate::checking) fn invoke(
    ctx: &mut CallContext,
    facts: &mut Facts,
    receiver: Fact,
    args: &Arguments,
) -> Result<Outcome> {
    let mut result = outcome(Atom::Never.fact());
    for i in 0..facts.arm_count(receiver) {
        ctx.charge(1)?;
        let arm = facts.arm(receiver, i);
        let next = match facts.node(arm) {
            Node::Offset(values) => invoke_offset(ctx, facts, *values, args)?,
            Node::Builtin(builtin) => super::invoke(ctx, facts, *builtin, args)?,
            Node::Atom(Atom::Never) => continue,
            Node::Atom(Atom::Unknown | Atom::Any) => {
                let mut next = outcome(Atom::Unknown.fact());
                next.throws = u8::MAX;
                next
            }
            _ if facts.known_non_callable(ctx, arm)? => reject(ctx, Failure::NonCallable)?,
            _ => {
                result.incomplete = true;
                continue;
            }
        };
        result.value = facts.union(ctx, &[result.value, next.value])?;
        result.throws |= next.throws;
        result.incomplete |= next.incomplete;
        result.failures.extend(ctx, &next.failures.data)?;
    }
    Ok(result)
}

fn invoke_offset(
    ctx: &mut CallContext,
    facts: &mut Facts,
    values: Fact,
    args: &Arguments,
) -> Result<Outcome> {
    if !args.keywords.data.is_empty() {
        return reject(ctx, Failure::BuiltinKeywords);
    }
    if args.positional.data.len() != 1 {
        return reject(ctx, Failure::BuiltinArity);
    }
    let mut result = outcome(Atom::Never.fact());
    let expected = facts.union(ctx, &[Atom::Int.fact(), Atom::Float.fact()])?;
    let actual = args.positional.data[0];
    parameter(ctx, facts, &mut result, 0, actual, expected)?;
    let length = if let Node::Tuple(items) = facts.node(values) {
        Some(items.data.len())
    } else {
        None
    };
    for i in 0..facts.arm_count(actual) {
        ctx.charge(1)?;
        let arm = facts.arm(actual, i);
        if arm == Atom::Never.fact() || facts.relation(ctx, arm, expected)? == Relation::Rejected {
            continue;
        }
        let selected = match facts.node(arm) {
            Node::Integer(n) => Some(*n),
            Node::Float(bits) => {
                let n = f64::from_bits(*bits);
                if !n.is_finite()
                    || !(-9_223_372_036_854_775_808.0..9_223_372_036_854_775_808.0).contains(&n)
                {
                    result.failures.push(ctx, Failure::BuiltinDomain(arm))?;
                    result.throws |= RUNTIME;
                    continue;
                }
                Some(n as i64)
            }
            _ => None,
        };
        let value = if let (Some(n), Some(length)) = (selected, length) {
            let at = if n < 0 {
                i128::from(n) + length as i128
            } else {
                i128::from(n)
            };
            if at < 0 || at >= length as i128 {
                result.failures.push(ctx, Failure::BuiltinDomain(arm))?;
                result.throws |= RUNTIME;
                continue;
            }
            let Node::Tuple(items) = facts.node(values) else {
                unreachable!()
            };
            items.data[at as usize]
        } else if selected == Some(0) {
            Atom::Int.fact()
        } else {
            result.throws |= RUNTIME;
            facts.elements(ctx, values)?
        };
        result.value = facts.union(ctx, &[result.value, value])?;
    }
    Ok(result)
}

pub(in crate::checking) fn member(
    ctx: &mut CallContext,
    facts: &mut Facts,
    receiver: Fact,
    site: CallSite,
    name: &str,
    args: &Arguments,
) -> Result<Outcome> {
    let Node::Protected(shape, _) = facts.node(receiver) else {
        unreachable!()
    };
    let shape = *shape;
    if let Some((field, false)) = facts.selected_field(ctx, shape, name.as_bytes())? {
        if let Node::Offset(values) = facts.node(field) {
            return if site.auto {
                if site.scope {
                    Ok(outcome(field))
                } else {
                    reject(ctx, Failure::BuiltinValue)
                }
            } else {
                invoke_offset(ctx, facts, *values, args)
            };
        }
        return if site.auto {
            Ok(outcome(field))
        } else {
            reject(ctx, Failure::NonCallable)
        };
    }
    if site.scope {
        return reject(ctx, Failure::Undefined);
    }
    if crate::bytecode::mutating_member(name) {
        return reject(ctx, Failure::BuiltinValue);
    }
    if !crate::members::hash_builtin(name) {
        return reject(ctx, Failure::Undefined);
    }
    if matches!(
        name,
        "itself" | "dup" | "clone" | "freeze" | "frozen?" | "nil?" | "eql?" | "equal?" | "inspect"
    ) {
        if !args.keywords.data.is_empty() {
            return reject(ctx, Failure::BuiltinKeywords);
        }
        if args.positional.data.len() != usize::from(matches!(name, "eql?" | "equal?")) {
            return reject(ctx, Failure::BuiltinArity);
        }
        return Ok(outcome(match name {
            "frozen?" => facts.boolean(ctx, true)?,
            "nil?" => facts.boolean(ctx, false)?,
            "eql?" | "equal?" => Atom::Bool.fact(),
            "inspect" => Atom::String.fact(),
            _ => receiver,
        }));
    }
    if !args.keywords.data.is_empty()
        && !matches!(name, "length" | "size" | "empty?" | "keys" | "values")
    {
        let mut result = outcome(Atom::Never.fact());
        result.incomplete = true;
        return Ok(result);
    }
    let operation = facts.collection_member(ctx, shape, site, name, &args.positional.data)?;
    let mut result = outcome(operation.value);
    result.incomplete = operation.unsupported;
    if operation.rejected {
        result.failures.push(ctx, Failure::BuiltinValue)?;
        result.throws |= RUNTIME;
    }
    Ok(result)
}

pub(in crate::checking) fn index(
    ctx: &mut CallContext,
    facts: &mut Facts,
    receiver: Fact,
    key: Fact,
    length: Option<Fact>,
) -> Result<Operation> {
    let Node::Protected(shape, tag) = facts.node(receiver) else {
        unreachable!()
    };
    let (shape, tag) = (*shape, *tag);
    if length.is_some() {
        return Ok(Operation {
            value: Atom::Never.fact(),
            rejected: true,
            unsupported: false,
        });
    }
    if tag == Tag::Match {
        if matches!(facts.atom(key), Some(Atom::Int | Atom::Float)) {
            let captures = facts.selected_field(ctx, shape, b"captures")?.unwrap().0;
            if let Node::Tuple(captures) = facts.node(captures) {
                let mut items = Buffer::with_capacity(ctx, captures.data.len() + 1)?;
                items.push(ctx, Atom::String.fact())?;
                items.extend(ctx, &captures.data)?;
                let all = facts.tuple(ctx, &items.data)?;
                return facts.collection_index(ctx, all, &[key]);
            }
            let value = if matches!(facts.node(key), Node::Integer(0)) {
                Atom::String.fact()
            } else {
                facts.nullable(ctx, Atom::String.fact())?
            };
            return Ok(Operation {
                value,
                rejected: false,
                unsupported: false,
            });
        }
        if let Node::String(name) | Node::Symbol(name) = facts.node(key) {
            if let Some((value, false)) =
                facts.selected_field(ctx, shape, name.as_bytes().unwrap())?
            {
                return Ok(Operation {
                    value,
                    rejected: false,
                    unsupported: false,
                });
            }
            let named = facts
                .selected_field(ctx, shape, b"named_captures")?
                .unwrap()
                .0;
            return facts.collection_index(ctx, named, &[key]);
        }
        if matches!(
            facts.atom(key),
            Some(Atom::String | Atom::Symbol | Atom::Unknown | Atom::Any)
        ) {
            let fields = facts.shape_values(ctx, shape, true)?;
            let value = facts.union(ctx, &[fields, Atom::String.fact(), Atom::Nil.fact()])?;
            return Ok(Operation {
                value,
                rejected: false,
                unsupported: false,
            });
        }
    }
    facts.collection_index(ctx, shape, &[key])
}

pub(super) fn string_match(
    ctx: &mut CallContext,
    facts: &mut Facts,
    _receiver: Fact,
    name: &str,
    args: &Arguments,
) -> Result<Outcome> {
    if !(1..=2).contains(&args.positional.data.len()) {
        return reject(ctx, Failure::BuiltinArity);
    }
    let expected = facts.union(ctx, &[Atom::String.fact(), Atom::Regex.fact()])?;
    let pattern = args.positional.data[0];
    let mut result = outcome(Atom::Never.fact());
    let mut possible = parameter(ctx, facts, &mut result, 0, pattern, expected)?;
    if let Some(&offset) = args.positional.data.get(1) {
        let number = facts.union(ctx, &[Atom::Int.fact(), Atom::Float.fact()])?;
        let mut valid = false;
        parameter(ctx, facts, &mut result, 1, offset, number)?;
        for i in 0..facts.arm_count(offset) {
            ctx.charge(1)?;
            let arm = facts.arm(offset, i);
            if arm == Atom::Never.fact() || facts.relation(ctx, arm, number)? == Relation::Rejected
            {
                continue;
            }
            let domain = match facts.node(arm) {
                Node::Integer(n) => Some(name != "match?" || *n >= 0),
                Node::Float(bits) => {
                    let n = f64::from_bits(*bits);
                    Some(
                        n.is_finite()
                            && (-9_223_372_036_854_775_808.0..9_223_372_036_854_775_808.0)
                                .contains(&n)
                            && (name != "match?" || n as i64 >= 0),
                    )
                }
                _ => None,
            };
            if domain == Some(false) {
                result.failures.push(ctx, Failure::BuiltinDomain(arm))?;
                result.throws |= RUNTIME;
            } else {
                valid = true;
                if domain.is_none() {
                    result.throws |= RUNTIME;
                }
            }
        }
        possible &= valid;
    }
    result.throws |= 1 << ErrorClass::Limit as u8;
    if possible {
        for i in 0..facts.arm_count(pattern) {
            ctx.charge(1)?;
            let arm = facts.arm(pattern, i);
            if arm == Atom::Never.fact()
                || facts.relation(ctx, arm, expected)? == Relation::Rejected
            {
                continue;
            }
            if facts.atom(arm) != Some(Atom::Regex) {
                result.throws |= RUNTIME;
            }
            let value = if name == "match?" {
                Atom::Bool.fact()
            } else {
                let matched = match_data(ctx, facts, arm)?;
                facts.nullable(ctx, matched)?
            };
            result.value = facts.union(ctx, &[result.value, value])?;
        }
    }
    Ok(result)
}
