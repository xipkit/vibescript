use super::{
    arguments::{Arguments, Failure},
    calls::Outcome,
    facts::{Atom, Fact, Facts, Field, Node},
    relation::Relation,
    scalar::Test,
};
use crate::{
    CallContext, ErrorClass, Result, Value,
    budget::Buffer,
    builtin::{Builtin, Math},
    bytecode::CallSite,
    value::Kind,
};

const RUNTIME: u8 = 1 << ErrorClass::Runtime as u8;

mod enums;
mod native;
mod primitives;
pub(super) mod protected;
mod temporal;
mod values;

pub(super) fn native_receiver(
    facts: &Facts,
    value: Fact,
) -> Option<crate::members::names::Receiver> {
    primitives::receiver(facts, value)
}

pub(super) fn primitive_member(
    ctx: &mut CallContext,
    facts: &Facts,
    receiver: Fact,
    name: &str,
) -> Result<bool> {
    primitives::supported(ctx, facts, receiver, name)
}

pub(super) fn value_member(
    ctx: &mut CallContext,
    facts: &Facts,
    receiver: Fact,
    name: &str,
) -> Result<bool> {
    for i in 0..facts.arm_count(receiver) {
        ctx.charge(1)?;
        let arm = facts.arm(receiver, i);
        if !matches!(facts.node(arm), Node::Protected(..))
            && !enums::supported(facts, arm)
            && !values::supported(facts, arm, name)
            && !primitives::supported(ctx, facts, arm, name)?
        {
            return Ok(false);
        }
    }
    Ok(true)
}

fn outcome(value: Fact) -> Outcome {
    Outcome {
        exits: Buffer::empty(),
        value,
        throws: 0,
        failures: Buffer::empty(),
        incomplete: false,
    }
}

pub(super) fn global(ctx: &mut CallContext, facts: &mut Facts, value: &Value) -> Result<Fact> {
    ctx.checkpoint()?;
    if let Kind::Builtin(builtin) = value.0 {
        return facts.builtin(ctx, builtin);
    }
    let Kind::Hash(hash) = &value.0 else {
        unreachable!()
    };
    let mut fields = Buffer::empty();
    for (name, value) in &hash.buffer.data {
        ctx.charge(1)?;
        let name = ctx.bytes(name.as_bytes().unwrap())?;
        let value = match value.0 {
            Kind::Builtin(builtin) => facts.builtin(ctx, builtin)?,
            Kind::Float(number) => facts.float(ctx, number)?,
            _ => unreachable!(),
        };
        fields.push(
            ctx,
            Field {
                name,
                value,
                optional: false,
            },
        )?;
    }
    facts.shape_fields(ctx, fields, false, Atom::String.fact(), false)
}

fn parameter(
    ctx: &mut CallContext,
    facts: &mut Facts,
    result: &mut Outcome,
    index: usize,
    actual: Fact,
    expected: Fact,
) -> Result<bool> {
    if facts.relation(ctx, actual, expected)? == Relation::Rejected {
        result.failures.push(
            ctx,
            Failure::Type {
                parameter: index,
                actual,
                expected,
            },
        )?;
    }
    let mut possible = false;
    for i in 0..facts.arm_count(actual) {
        ctx.charge(1)?;
        let arm = facts.arm(actual, i);
        let relation = facts.relation(ctx, arm, expected)?;
        possible |= relation != Relation::Rejected && arm != Atom::Never.fact();
        if relation != Relation::Accepted {
            result.throws |= RUNTIME;
        }
    }
    Ok(possible)
}

pub(super) fn invoke(
    ctx: &mut CallContext,
    facts: &mut Facts,
    builtin: Builtin,
    args: &Arguments,
) -> Result<Outcome> {
    ctx.checkpoint()?;
    let mut result = outcome(Atom::Never.fact());
    let count = args.positional.data.len();
    let supported = matches!(
        builtin,
        Builtin::JsonParse
            | Builtin::JsonParseAs
            | Builtin::JsonStringify
            | Builtin::ToInt
            | Builtin::ToFloat
            | Builtin::Math(_)
            | Builtin::HashNew
            | Builtin::Assert
    );
    if !supported {
        return native::invoke(ctx, facts, builtin, args);
    }
    if builtin != Builtin::Assert && !args.keywords.data.is_empty() {
        result.failures.push(ctx, Failure::BuiltinKeywords)?;
    }
    let arity = match builtin {
        Builtin::Assert => count >= 1,
        Builtin::HashNew => count == 0,
        Builtin::Math(Math::Log) => (1..=2).contains(&count),
        Builtin::Math(Math::Atan2 | Math::Hypot) | Builtin::JsonParseAs => count == 2,
        _ => count == 1,
    };
    if !arity {
        result.failures.push(ctx, Failure::BuiltinArity)?;
    }
    if !result.failures.data.is_empty() {
        result.throws = RUNTIME;
        return Ok(result);
    }
    if builtin == Builtin::HashNew {
        result.value =
            facts.shape_fields(ctx, Buffer::empty(), false, Atom::String.fact(), true)?;
        return Ok(result);
    }
    let first = args.positional.data[0];
    match builtin {
        Builtin::Assert => {
            if facts.filter(ctx, first, Test::Truth, true)? != Atom::Never.fact() {
                result.value = Atom::Nil.fact();
            }
            if facts.filter(ctx, first, Test::Truth, false)? != Atom::Never.fact() {
                result.throws |= 1 << ErrorClass::Assertion as u8;
                // Message rendering can fail before the assertion is raised.
                if count > 1 || !args.keywords.data.is_empty() {
                    result.throws |= RUNTIME;
                }
            }
        }
        Builtin::JsonParse | Builtin::JsonParseAs => {
            let expected = Atom::String.fact();
            let possible = parameter(ctx, facts, &mut result, 0, first, expected)?;
            result.throws |= RUNTIME;
            if builtin == Builtin::JsonParse {
                if possible {
                    result.value = Atom::Unknown.fact();
                }
            } else {
                let ty = args.positional.data[1];
                let mut invalid = false;
                let mut returns = Buffer::empty();
                for i in 0..facts.arm_count(ty) {
                    ctx.charge(1)?;
                    match facts.node(facts.arm(ty, i)) {
                        Node::TypeValue(contract) => {
                            result.incomplete |= facts.unresolved(*contract);
                            let value = facts.value_domain(ctx, *contract)?;
                            returns.push(ctx, value)?;
                        }
                        Node::Atom(Atom::Unknown | Atom::Any) => {
                            returns.push(ctx, Atom::Unknown.fact())?
                        }
                        Node::Atom(Atom::Never) => (),
                        Node::Named(_) | Node::Nominal { .. } => result.incomplete = true,
                        _ => invalid = true,
                    }
                }
                if invalid {
                    result.failures.push(ctx, Failure::TypeLiteral(ty))?;
                }
                if possible {
                    result.value = facts.union(ctx, &returns.data)?;
                }
            }
        }
        Builtin::JsonStringify => {
            result.value = Atom::String.fact();
            let encoding = json_value(ctx, facts, first)?;
            result.throws = if encoding.invalid || encoding.fallible {
                RUNTIME
            } else {
                0
            };
            result.incomplete = encoding.incomplete;
            if encoding.invalid {
                result.failures.push(ctx, Failure::JsonValue(first))?;
            }
        }
        Builtin::ToInt | Builtin::ToFloat => {
            let expected = facts.union(
                ctx,
                &[Atom::Int.fact(), Atom::Float.fact(), Atom::String.fact()],
            )?;
            if parameter(ctx, facts, &mut result, 0, first, expected)? {
                result.value = if builtin == Builtin::ToInt {
                    Atom::Int.fact()
                } else {
                    Atom::Float.fact()
                };
            }
            for i in 0..facts.arm_count(first) {
                ctx.charge(1)?;
                let arm = facts.arm(first, i);
                if facts.atom(arm) == Some(Atom::String) {
                    result.throws |= RUNTIME;
                }
                if builtin == Builtin::ToInt && facts.atom(arm) == Some(Atom::Float) {
                    match facts.node(arm) {
                        Node::Float(bits)
                            if f64::from_bits(*bits).is_finite()
                                && f64::from_bits(*bits).fract() == 0.0 => {}
                        Node::Float(_) => {
                            result.failures.push(ctx, Failure::BuiltinDomain(arm))?;
                            result.throws |= RUNTIME;
                            if facts.arm_count(first) == 1 {
                                result.value = Atom::Never.fact();
                            }
                        }
                        _ => result.throws |= RUNTIME,
                    }
                }
            }
        }
        Builtin::Math(method) => {
            let expected = facts.union(ctx, &[Atom::Int.fact(), Atom::Float.fact()])?;
            let mut possible = true;
            for (i, &actual) in args.positional.data.iter().enumerate() {
                ctx.charge(1)?;
                possible &= parameter(ctx, facts, &mut result, i, actual, expected)?;
                if matches!(
                    method,
                    Math::Sqrt | Math::Log | Math::Log2 | Math::Log10 | Math::Asin | Math::Acos
                ) {
                    for j in 0..facts.arm_count(actual) {
                        ctx.charge(1)?;
                        let arm = facts.arm(actual, j);
                        let number = match facts.node(arm) {
                            Node::Integer(n) => Some(*n as f64),
                            Node::Float(bits) => Some(f64::from_bits(*bits)),
                            _ => None,
                        };
                        if let Some(n) = number {
                            let invalid = if matches!(method, Math::Asin | Math::Acos) {
                                n.abs() > 1.0
                            } else {
                                n < 0.0
                            };
                            if invalid {
                                result.failures.push(ctx, Failure::BuiltinDomain(arm))?;
                                result.throws |= RUNTIME;
                                if facts.arm_count(actual) == 1 {
                                    possible = false;
                                }
                            }
                        } else {
                            result.throws |= RUNTIME;
                        }
                    }
                }
            }
            if possible {
                result.value = Atom::Float.fact();
            }
        }
        _ => unreachable!(),
    }
    Ok(result)
}

#[derive(Default)]
struct Encoding {
    invalid: bool,
    incomplete: bool,
    fallible: bool,
}

fn json_value(ctx: &mut CallContext, facts: &Facts, value: Fact) -> Result<Encoding> {
    let mut pending = Buffer::empty();
    let mut seen = Buffer::with_capacity(ctx, facts.len())?;
    ctx.charge(facts.len() as u64)?;
    seen.data.resize(facts.len(), false);
    pending.push(ctx, value)?;
    let mut result = Encoding::default();
    while let Some(value) = pending.data.pop() {
        ctx.charge(1)?;
        if std::mem::replace(&mut seen.data[value.0], true) {
            continue;
        }
        match facts.node(value) {
            Node::Protected(shape, _) => pending.push(ctx, *shape)?,
            Node::Array(element) => pending.push(ctx, *element)?,
            Node::Tuple(values) | Node::Union(values) => pending.extend(ctx, &values.data)?,
            Node::Hash(_, value, _) => pending.push(ctx, *value)?,
            Node::Shape(fields, ..) => {
                for field in &fields.data {
                    pending.push(ctx, field.value)?;
                }
            }
            Node::Atom(Atom::Duration | Atom::Money | Atom::Time | Atom::Range | Atom::Regex)
            | Node::Range(..)
            | Node::Regex(_)
            | Node::Builtin(_)
            | Node::Offset(_)
            | Node::TypeValue(_)
            | Node::Enumeration { .. } => result.invalid = true,
            Node::Nominal {
                symbols: Some(_), ..
            } => (),
            Node::Named(_) | Node::Nominal { .. } | Node::Choice(_) => result.incomplete = true,
            Node::Float(bits) if !f64::from_bits(*bits).is_finite() => result.invalid = true,
            Node::Atom(Atom::Unknown | Atom::Any | Atom::Float) => result.fallible = true,
            _ => (),
        }
    }
    Ok(result)
}

pub(super) fn namespace(ctx: &mut CallContext, facts: &Facts, value: Fact) -> Result<bool> {
    let Node::Shape(fields, false, _, false) = facts.node(value) else {
        return Ok(false);
    };
    for field in &fields.data {
        ctx.charge(1)?;
        if matches!(facts.node(field.value), Node::Builtin(_)) {
            return Ok(true);
        }
    }
    Ok(false)
}

pub(super) fn namespace_call(
    ctx: &mut CallContext,
    facts: &Facts,
    receiver: Fact,
    name: &str,
) -> Result<bool> {
    if !namespace(ctx, facts, receiver)? {
        return Ok(false);
    }
    Ok(facts
        .selected_field(ctx, receiver, name.as_bytes())?
        .is_some_and(|(value, optional)| {
            !optional && matches!(facts.node(value), Node::Builtin(_))
        }))
}

pub(super) fn member(
    ctx: &mut CallContext,
    facts: &mut Facts,
    receiver: Fact,
    site: CallSite,
    name: &str,
    args: &Arguments,
) -> Result<Option<Outcome>> {
    let mut special = false;
    for i in 0..facts.arm_count(receiver) {
        ctx.charge(1)?;
        let arm = facts.arm(receiver, i);
        special |= matches!(facts.node(arm), Node::TypeValue(_) | Node::Protected(..))
            || enums::supported(facts, arm)
            || values::supported(facts, arm, name)
            || primitives::supported(ctx, facts, arm, name)?
            || namespace(ctx, facts, arm)?;
    }
    if !special {
        return Ok(None);
    }
    let mut result = outcome(Atom::Never.fact());
    for i in 0..facts.arm_count(receiver) {
        ctx.charge(1)?;
        let arm = facts.arm(receiver, i);
        let next = if let Some(next) = member_arm(ctx, facts, arm, site, name, args)? {
            next
        } else if !args.keywords.data.is_empty() || args.block.is_some() {
            let mut next = outcome(Atom::Never.fact());
            next.incomplete = true;
            next
        } else {
            let operation = facts.collection_member(ctx, arm, site, name, &args.positional.data)?;
            let missing = operation.unsupported
                && matches!(
                    name,
                    "captures"
                        | "named_captures"
                        | "pre_match"
                        | "post_match"
                        | "begin"
                        | "end"
                        | "message"
                        | "backtrace"
                        | "code_frame"
                        | "name"
                        | "symbol"
                        | "enum"
                )
                && facts.known_primitive(ctx, arm)?;
            let mut next = outcome(operation.value);
            next.incomplete = operation.unsupported && !missing;
            if missing {
                next.value = Atom::Never.fact();
            }
            if operation.rejected || missing {
                next.failures.push(ctx, Failure::NonCallable)?;
            }
            next
        };
        result.value = facts.union(ctx, &[result.value, next.value])?;
        result.throws |= next.throws;
        result.incomplete |= next.incomplete;
        result.failures.extend(ctx, &next.failures.data)?;
    }
    Ok(Some(result))
}

fn member_arm(
    ctx: &mut CallContext,
    facts: &mut Facts,
    receiver: Fact,
    site: CallSite,
    name: &str,
    args: &Arguments,
) -> Result<Option<Outcome>> {
    if site.scope && enums::supported(facts, receiver) {
        return enums::member(ctx, facts, receiver, site, name, args).map(Some);
    }
    if primitives::supported(ctx, facts, receiver, name)? {
        return primitives::member(ctx, facts, receiver, site, name, args).map(Some);
    }
    if enums::supported(facts, receiver) {
        return enums::member(ctx, facts, receiver, site, name, args).map(Some);
    }
    if args.block.is_some() {
        let mut result = outcome(Atom::Never.fact());
        result.incomplete = true;
        return Ok(Some(result));
    }
    if matches!(facts.node(receiver), Node::Protected(..)) {
        return protected::member(ctx, facts, receiver, site, name, args).map(Some);
    }
    if values::supported(facts, receiver, name) {
        return values::member(ctx, facts, receiver, site, name, args).map(Some);
    }
    if let Node::TypeValue(_) = facts.node(receiver) {
        let mut result = outcome(Atom::Never.fact());
        if site.scope || !matches!(name, "nil?" | "itself" | "dup") {
            result.failures.push(ctx, Failure::Undefined)?;
        } else if !args.positional.data.is_empty() || !args.keywords.data.is_empty() {
            result.failures.push(ctx, Failure::BuiltinArity)?;
        } else {
            result.value = if name == "nil?" {
                facts.boolean(ctx, false)?
            } else {
                receiver
            };
        }
        return Ok(Some(result));
    }
    if !namespace(ctx, facts, receiver)? {
        return Ok(None);
    }
    let Some((field, false)) = facts.selected_field(ctx, receiver, name.as_bytes())? else {
        if !site.scope
            && matches!(
                name,
                "keys" | "values" | "length" | "size" | "empty?" | "nil?" | "itself" | "dup"
            )
        {
            let mut result = outcome(Atom::Never.fact());
            if !args.positional.data.is_empty() || !args.keywords.data.is_empty() {
                result.failures.push(ctx, Failure::BuiltinArity)?;
                return Ok(Some(result));
            }
            result.value = match name {
                "keys" => facts.array(ctx, Atom::String.fact())?,
                "values" => {
                    let value = facts.shape_values(ctx, receiver, false)?;
                    facts.array(ctx, value)?
                }
                "nil?" | "empty?" => facts.boolean(ctx, false)?,
                "itself" | "dup" => receiver,
                _ => {
                    let Node::Shape(fields, ..) = facts.node(receiver) else {
                        unreachable!()
                    };
                    facts.integer(ctx, fields.data.len() as i64)?
                }
            };
            return Ok(Some(result));
        }
        if !site.scope
            && (crate::members::hash_builtin(name) || crate::members::names::universal(name))
        {
            return Ok(None);
        }
        let mut result = outcome(Atom::Never.fact());
        result.failures.push(ctx, Failure::Undefined)?;
        return Ok(Some(result));
    };
    let Node::Builtin(builtin) = facts.node(field) else {
        if !site.scope {
            return Ok(None);
        }
        let mut result = outcome(field);
        if !site.auto {
            result.value = Atom::Never.fact();
            result.failures.push(ctx, Failure::NonCallable)?;
        }
        return Ok(Some(result));
    };
    let builtin = *builtin;
    if site.auto {
        if site.scope {
            return Ok(Some(outcome(field)));
        }
        if !builtin.auto() {
            let mut result = outcome(Atom::Never.fact());
            result.failures.push(ctx, Failure::BuiltinValue)?;
            return Ok(Some(result));
        }
    }
    invoke(ctx, facts, builtin, args).map(Some)
}
