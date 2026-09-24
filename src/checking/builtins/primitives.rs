use super::*;

mod introspection;
mod number;
mod range;
mod template;
mod text;

pub(super) fn receiver(facts: &Facts, value: Fact) -> Option<crate::members::names::Receiver> {
    use crate::members::names::Receiver;
    Some(match facts.node(value) {
        Node::Tuple(_) | Node::Array(_) => Receiver::Array,
        Node::Hash(..) | Node::Shape(..) | Node::Protected(..) => Receiver::Hash,
        Node::Builtin(_) | Node::Offset(_) | Node::TypeValue(_) | Node::Instance { .. } => {
            Receiver::Other
        }
        Node::Enumeration { .. } => Receiver::Enum,
        Node::EnumMember { .. } => Receiver::EnumMember,
        _ => match facts.atom(value)? {
            Atom::Never | Atom::Unknown | Atom::Any => return None,
            Atom::Nil => Receiver::Nil,
            Atom::Bool => Receiver::Bool,
            Atom::Int => Receiver::Int,
            Atom::Float => Receiver::Float,
            Atom::String => Receiver::Bytes,
            Atom::Symbol => Receiver::Symbol,
            Atom::Duration => Receiver::Duration,
            Atom::Time => Receiver::Time,
            Atom::Money => Receiver::Money,
            Atom::Range => Receiver::Range,
            Atom::Regex => Receiver::Regex,
        },
    })
}

const LIMIT: u8 = 1 << ErrorClass::Limit as u8;
const ZERO: u8 = 1 << ErrorClass::ZeroDivision as u8;

fn universal(name: &str) -> bool {
    crate::members::introspection::supported(name)
        || matches!(
            name,
            "nil?" | "itself" | "dup" | "clone" | "freeze" | "frozen?" | "eql?" | "equal?"
        )
}

/// Symbol members served by the runtime's byte-oriented fallbacks, which
/// introspection does not advertise.
fn symbol_bytes(name: &str) -> bool {
    matches!(name, "bytesize" | "getbyte" | "to_i") || symbol_search(name)
}

/// Symbol searches that read the symbol's bytes like a string's.
fn symbol_search(name: &str) -> bool {
    matches!(name, "index" | "rindex" | "find_index" | "include?")
}

fn missing(facts: &Facts, value: Fact, name: &str) -> bool {
    use crate::members::names::{self, Receiver};

    let Some(kind) = receiver(facts, value) else {
        return false;
    };
    matches!(
        kind,
        Receiver::Nil
            | Receiver::Bool
            | Receiver::Int
            | Receiver::Float
            | Receiver::Bytes
            | Receiver::Range
            | Receiver::Array
            | Receiver::Symbol
    ) && !names::universal(name)
        && !kind.available(name)
        && !(kind == Receiver::Symbol && symbol_bytes(name))
}

pub(super) fn supported(
    ctx: &mut CallContext,
    facts: &Facts,
    receiver: Fact,
    name: &str,
) -> Result<bool> {
    if missing(facts, receiver, name) {
        return Ok(true);
    }
    if universal(name) {
        return Ok(match facts.node(receiver) {
            Node::Named(_) | Node::Nominal { .. } => false,
            Node::Hash(_, _, kind) | Node::Shape(_, _, _, kind) if !kind.plain() => {
                namespace(ctx, facts, receiver)? && !namespace_call(ctx, facts, receiver, name)?
            }
            Node::Atom(Atom::Unknown | Atom::Any | Atom::Never) => false,
            _ => true,
        });
    }
    Ok(match facts.atom(receiver) {
        Some(Atom::Int | Atom::Float) => number::supported(name),
        Some(Atom::String) => text::supported(name),
        Some(Atom::Nil | Atom::Bool) => matches!(name, "inspect" | "to_s" | "string"),
        Some(Atom::Range) => {
            matches!(name, "inspect" | "to_s" | "string") || range::supported(name)
        }
        Some(Atom::Symbol) => {
            matches!(
                name,
                "inspect" | "to_s" | "string" | "id2name" | "to_sym" | "to_i"
            ) || symbol_search(name)
        }
        _ => false,
    })
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
    if site.scope || missing(facts, receiver, name) {
        return reject(ctx, Failure::Undefined);
    }
    let kind = facts.atom(receiver);
    let common = universal(name);
    let strict = common
        || matches!(name, "inspect" | "to_s" | "string" | "id2name" | "to_sym")
        || matches!(kind, Some(Atom::Int | Atom::Float | Atom::String))
            && matches!(
                name,
                "clamp" | "between?" | "to_s" | "string" | "to_i" | "to_f"
            )
        || kind == Some(Atom::String)
            && matches!(
                name,
                "to_sym"
                    | "intern"
                    | "count"
                    | "delete"
                    | "delete!"
                    | "tr"
                    | "tr!"
                    | "squeeze"
                    | "squeeze!"
            );
    let ranged = kind == Some(Atom::Range) && range::supported(name);
    let keywords = strict
        || ranged
        || matches!(name, "to_s" | "string")
        || kind == Some(Atom::String)
            && matches!(
                name,
                "center" | "ljust" | "rjust" | "partition" | "rpartition"
            )
        || kind == Some(Atom::Symbol) && name == "to_i";
    if keywords && !args.keywords.data.is_empty() {
        return reject(ctx, Failure::BuiltinKeywords);
    }
    let temporal_equality = name == "eql?" && matches!(kind, Some(Atom::Time | Atom::Duration));
    if strict && args.block.is_some() && !temporal_equality {
        return reject(ctx, Failure::BuiltinBlock);
    }
    let count = args.positional.data.len();
    let predicate = crate::members::introspection::Predicate::parse(name);
    let arity = if let Some(predicate) = predicate {
        count == 1 || predicate == crate::members::introspection::Predicate::Respond && count == 2
    } else if common {
        count == usize::from(matches!(name, "eql?" | "equal?"))
    } else {
        match kind {
            Some(Atom::Int | Atom::Float) => number::arity(name, count),
            Some(Atom::String) => text::arity(name, count),
            Some(Atom::Range) => range::arity(name, count),
            Some(Atom::Symbol) if symbol_search(name) => count == 1,
            _ => count == 0,
        }
    };
    if !arity {
        return reject(ctx, Failure::BuiltinArity);
    }
    if let Some(predicate) = predicate {
        return introspection::member(ctx, facts, receiver, predicate, args);
    }
    if kind == Some(Atom::Symbol) && name == "to_sym"
        || kind == Some(Atom::String) && matches!(name, "to_s" | "string")
    {
        return Ok(outcome(receiver));
    }
    let converted = match facts.node(receiver) {
        Node::String(value) if matches!(name, "to_sym" | "intern") => Some((value.clone(), true)),
        Node::Symbol(value) if matches!(name, "id2name" | "to_s" | "string") => {
            Some((value.clone(), false))
        }
        _ => None,
    };
    if let Some((value, symbol)) = converted {
        let value = if symbol {
            facts.symbol(ctx, value.as_bytes().unwrap())?
        } else {
            facts.string(ctx, value.as_bytes().unwrap())?
        };
        return Ok(outcome(value));
    }
    if common {
        let value = match name {
            "nil?" => facts.boolean(ctx, receiver == Atom::Nil.fact())?,
            "frozen?" => facts.boolean(ctx, true)?,
            "eql?" | "equal?" => {
                if let Some(result) = literal_call(ctx, facts, receiver, site, name, args)? {
                    return Ok(result);
                }
                return equality(
                    ctx,
                    facts,
                    receiver,
                    args.positional.data[0],
                    name == "eql?",
                );
            }
            _ => receiver,
        };
        return Ok(outcome(value));
    }
    // Materializing members stay symbolic rather than running on literals.
    if ranged {
        return range::member(ctx, facts, receiver, name, args);
    }
    if let Some(result) = literal_call(ctx, facts, receiver, site, name, args)? {
        return Ok(result);
    }
    match kind {
        Some(Atom::Int | Atom::Float) => number::member(ctx, facts, receiver, name, args),
        Some(Atom::String) => text::member(ctx, facts, receiver, name, args),
        Some(Atom::Symbol) if symbol_search(name) => {
            // The needle is read as bytes, so it may be a string or a symbol.
            let mut result = outcome(Atom::Never.fact());
            let needle = facts.union(ctx, &[Atom::String.fact(), Atom::Symbol.fact()])?;
            if parameter(ctx, facts, &mut result, 0, args.positional.data[0], needle)? {
                result.value = if name == "include?" {
                    Atom::Bool.fact()
                } else {
                    facts.nullable(ctx, Atom::Int.fact())?
                };
            }
            Ok(result)
        }
        Some(Atom::Symbol) if name == "to_i" => {
            // Symbols parse their bytes like `string.to_i`.
            let mut result = outcome(Atom::Int.fact());
            result.throws = RUNTIME | LIMIT;
            Ok(result)
        }
        _ => Ok(outcome(if name == "to_sym" {
            receiver
        } else {
            Atom::String.fact()
        })),
    }
}

/// Models the native `eql?` (strict) or `equal?` (identity) helper for any
/// admitted receiver without invoking user methods.
///
/// The Limit class is reported only when some comparison path can actually reach
/// the native value-depth guard; scalar and shallow operands throw nothing.
pub(super) fn equality(
    ctx: &mut CallContext,
    facts: &mut Facts,
    receiver: Fact,
    argument: Fact,
    strict: bool,
) -> Result<Outcome> {
    let (value, guarded) = facts.helper_equal(ctx, receiver, argument, strict)?;
    let mut result = outcome(value);
    if guarded {
        result.throws = LIMIT;
    }
    Ok(result)
}

fn literal(ctx: &mut CallContext, facts: &Facts, fact: Fact) -> Result<Option<Value>> {
    ctx.charge(1)?;
    Ok(Some(match facts.node(fact) {
        Node::Atom(Atom::Nil) => Value::nil(),
        Node::Boolean(value) => Value::boolean(*value),
        Node::Integer(value) => Value::int(*value),
        Node::Float(bits) => Value::float(f64::from_bits(*bits)),
        Node::String(value) | Node::Regex(value) => value.clone(),
        Node::Symbol(value) => {
            let Kind::Bytes(bytes) = &value.0 else {
                unreachable!()
            };
            Value(Kind::Symbol(bytes.clone()))
        }
        Node::Range(start, end, exclusive) => Value(Kind::Range(crate::range::Range::new(
            ctx, *start, *end, *exclusive,
        )?)),
        _ => return Ok(None),
    }))
}

fn literal_call(
    ctx: &mut CallContext,
    facts: &mut Facts,
    receiver: Fact,
    site: CallSite,
    name: &str,
    args: &Arguments,
) -> Result<Option<Outcome>> {
    let Some(value) = literal(ctx, facts, receiver)? else {
        return Ok(None);
    };
    let mut positional = Buffer::with_capacity(ctx, args.positional.data.len())?;
    for &argument in &args.positional.data {
        let Some(value) = literal(ctx, facts, argument)? else {
            return Ok(None);
        };
        positional.push(ctx, value)?;
    }
    // Only admitted pure native methods reach this path. Flags have already
    // been rejected or are deliberately ignored; no callback is constructed.
    let result = crate::members::call(ctx, site, name, value, &positional.data);
    let mut output = outcome(Atom::Never.fact());
    match result {
        Ok((_, value)) => output.value = computed(ctx, facts, &value)?,
        Err(error) => {
            ctx.checkpoint()?;
            let Some(class) = error.class() else {
                return Err(error);
            };
            output.throws = 1 << class as u8;
            output
                .failures
                .push(ctx, Failure::BuiltinDomain(receiver))?;
        }
    }
    Ok(Some(output))
}

fn computed(ctx: &mut CallContext, facts: &mut Facts, value: &Value) -> Result<Fact> {
    // Computed scalar constants would otherwise grow an unbounded union in
    // loops such as n = n.succ or s = s.center(n).
    Ok(match &value.0 {
        Kind::Nil => Atom::Nil.fact(),
        Kind::Bool(value) => facts.boolean(ctx, *value)?,
        Kind::Int(_) | Kind::Big(_) => Atom::Int.fact(),
        Kind::Float(_) => Atom::Float.fact(),
        Kind::Bytes(_) => Atom::String.fact(),
        Kind::Symbol(_) => Atom::Symbol.fact(),
        Kind::Array(values) => {
            let mut elements = Buffer::with_capacity(ctx, values.buffer.data.len())?;
            for value in &values.buffer.data {
                ctx.charge(1)?;
                // Native primitive methods only produce flat arrays.
                let element = match value.0 {
                    Kind::Int(_) | Kind::Big(_) => Atom::Int.fact(),
                    Kind::Float(_) => Atom::Float.fact(),
                    Kind::Bytes(_) => Atom::String.fact(),
                    _ => unreachable!(),
                };
                elements.push(ctx, element)?;
            }
            facts.tuple(ctx, &elements.data)?
        }
        _ => unreachable!(),
    })
}

fn domain(
    ctx: &mut CallContext,
    facts: &Facts,
    result: &mut Outcome,
    value: Fact,
    mut valid: impl FnMut(&mut CallContext, &Facts, Fact) -> Result<Option<bool>>,
) -> Result<bool> {
    let mut possible = false;
    for i in 0..facts.arm_count(value) {
        ctx.charge(1)?;
        let arm = facts.arm(value, i);
        if arm == Atom::Never.fact() {
            continue;
        }
        let allowed = valid(ctx, facts, arm)?;
        possible |= allowed != Some(false);
        if allowed == Some(false) {
            result.failures.push(ctx, Failure::BuiltinDomain(arm))?;
        }
        if allowed != Some(true) {
            result.throws |= RUNTIME;
        }
    }
    Ok(possible)
}

fn machine_integer(_: &mut CallContext, facts: &Facts, value: Fact) -> Result<Option<bool>> {
    Ok(match facts.node(value) {
        Node::Integer(_) => Some(true),
        Node::Float(bits) => {
            Some(crate::sequence::integer(&Value::float(f64::from_bits(*bits))).is_ok())
        }
        _ => None,
    })
}

fn between(
    ctx: &mut CallContext,
    facts: &mut Facts,
    receiver: Fact,
    args: &[Fact],
) -> Result<Outcome> {
    use crate::checking::ordering::{EQUAL, GREATER, LESS, UNORDERED};
    let expected = if facts.atom(receiver) == Some(Atom::String) {
        Atom::String.fact()
    } else {
        facts.union(ctx, &[Atom::Int.fact(), Atom::Float.fact()])?
    };
    let mut result = outcome(Atom::Never.fact());
    let mut positive = true;
    let mut negative = false;
    for (index, &bound) in args.iter().enumerate() {
        if !positive {
            break;
        }
        parameter(ctx, facts, &mut result, index, bound, expected)?;
        let mut ordering = 0;
        for i in 0..facts.arm_count(bound) {
            ctx.charge(1)?;
            let arm = facts.arm(bound, i);
            if facts.relation(ctx, arm, expected)? != Relation::Rejected {
                ordering |= if index == 0 {
                    facts.order_result(ctx, arm, receiver)?
                } else {
                    facts.order_result(ctx, receiver, arm)?
                };
            }
        }
        negative |= ordering & (GREATER | UNORDERED) != 0;
        positive = ordering & (LESS | EQUAL) != 0;
    }
    result.value = match (positive, negative) {
        (true, true) => Atom::Bool.fact(),
        (true, false) => facts.boolean(ctx, true)?,
        (false, true) => facts.boolean(ctx, false)?,
        (false, false) => Atom::Never.fact(),
    };
    Ok(result)
}
