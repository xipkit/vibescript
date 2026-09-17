use super::*;
use crate::{
    output::Kind as Output, random::Method as Random, regex::value::Constructor as Regexp,
};

const LIMIT: u8 = 1 << ErrorClass::Limit as u8;

pub(super) fn invoke(
    ctx: &mut CallContext,
    facts: &mut Facts,
    builtin: Builtin,
    args: &Arguments,
) -> Result<Outcome> {
    if matches!(builtin, Builtin::Time(_) | Builtin::DurationBuild) {
        return super::temporal::invoke(ctx, facts, builtin, args);
    }
    let mut result = outcome(Atom::Never.fact());
    let count = args.positional.data.len();
    let arity = match builtin {
        Builtin::Regexp(Regexp::LastMatch) | Builtin::Now | Builtin::Random(Random::Uuid) => {
            count == 0
        }
        Builtin::Regexp(Regexp::Union) | Builtin::Output(_) => true,
        Builtin::Regexp(_) | Builtin::DurationParse | Builtin::Money => count == 1,
        Builtin::Regex(crate::regex::Utility::Match) | Builtin::MoneyCents => count == 2,
        Builtin::Regex(_) => count == 3,
        Builtin::Random(_) => count <= 1,
        Builtin::Format(_) => count >= 1,
        _ => {
            result.incomplete = true;
            return Ok(result);
        }
    };
    if !arity {
        result.failures.push(ctx, Failure::BuiltinArity)?;
    }
    if !args.keywords.data.is_empty()
        && !matches!(
            builtin,
            Builtin::Now | Builtin::DurationParse | Builtin::Money | Builtin::MoneyCents
        )
    {
        result.failures.push(ctx, Failure::BuiltinKeywords)?;
    }
    if !result.failures.data.is_empty() {
        result.throws = RUNTIME;
        return Ok(result);
    }
    let mut possible = true;
    let value = match builtin {
        Builtin::Regexp(constructor) => {
            for (index, &actual) in args.positional.data.iter().enumerate() {
                possible &= parameter(ctx, facts, &mut result, index, actual, Atom::String.fact())?;
            }
            match constructor {
                Regexp::New | Regexp::Union => {
                    result.throws |= RUNTIME | LIMIT;
                    Atom::Regex.fact()
                }
                Regexp::Escape | Regexp::Quote => Atom::String.fact(),
                Regexp::LastMatch => Atom::Nil.fact(),
            }
        }
        Builtin::Regex(utility) => {
            for (index, &actual) in args.positional.data.iter().enumerate() {
                possible &= parameter(ctx, facts, &mut result, index, actual, Atom::String.fact())?;
            }
            result.throws |= RUNTIME | LIMIT;
            if utility == crate::regex::Utility::Match {
                facts.union(ctx, &[Atom::String.fact(), Atom::Nil.fact()])?
            } else {
                Atom::String.fact()
            }
        }
        Builtin::Now => Atom::String.fact(),
        Builtin::DurationParse | Builtin::Money => {
            possible = parameter(
                ctx,
                facts,
                &mut result,
                0,
                args.positional.data[0],
                Atom::String.fact(),
            )?;
            result.throws |= RUNTIME;
            if builtin == Builtin::Money {
                Atom::Money.fact()
            } else {
                Atom::Duration.fact()
            }
        }
        Builtin::MoneyCents => {
            let expected = facts.union(ctx, &[Atom::Int.fact(), Atom::Float.fact()])?;
            let actual = args.positional.data[0];
            possible = parameter(ctx, facts, &mut result, 0, actual, expected)?;
            possible &= domain(ctx, facts, &mut result, actual, expected, integer)?;
            possible &= parameter(
                ctx,
                facts,
                &mut result,
                1,
                args.positional.data[1],
                Atom::String.fact(),
            )?;
            result.throws |= RUNTIME;
            Atom::Money.fact()
        }
        Builtin::Random(method) => return random(ctx, facts, method, args),
        Builtin::Format(_) => {
            possible = parameter(
                ctx,
                facts,
                &mut result,
                0,
                args.positional.data[0],
                Atom::String.fact(),
            )?;
            if possible {
                for &actual in &args.positional.data[1..] {
                    result.incomplete |= conversion(ctx, facts, actual)?;
                }
            }
            result.throws |= RUNTIME | LIMIT;
            Atom::String.fact()
        }
        Builtin::Output(kind) => {
            // Writers are execution configuration, not checker inputs. A configured
            // writer may return any ordinary exception class.
            result.throws = if count == 0 && kind != Output::Puts {
                RUNTIME
            } else {
                u8::MAX
            };
            if kind == Output::Inspect {
                match args.positional.data.as_slice() {
                    [] => Atom::Nil.fact(),
                    [value] => *value,
                    values => facts.tuple(ctx, values)?,
                }
            } else {
                for &actual in &args.positional.data {
                    result.incomplete |= conversion(ctx, facts, actual)?;
                }
                Atom::Nil.fact()
            }
        }
        _ => unreachable!(),
    };
    if possible {
        result.value = value;
    }
    Ok(result)
}

fn conversion(ctx: &mut CallContext, facts: &Facts, value: Fact) -> Result<bool> {
    let mut incomplete = false;
    for i in 0..facts.arm_count(value) {
        ctx.charge(1)?;
        incomplete |= matches!(
            facts.node(facts.arm(value, i)),
            Node::Atom(Atom::Unknown | Atom::Any)
                | Node::Named(_)
                | Node::Nominal { symbols: None, .. }
                | Node::Hash(_, _, false)
                | Node::Shape(_, _, _, false)
        );
    }
    Ok(incomplete)
}

pub(super) fn integer(node: &Node) -> Option<bool> {
    match node {
        Node::Integer(_) => Some(true),
        Node::Float(bits) => {
            let n = f64::from_bits(*bits);
            Some(n.is_finite() && n >= i64::MIN as f64 && n < 9_223_372_036_854_775_808.0)
        }
        _ => None,
    }
}

pub(super) fn domain(
    ctx: &mut CallContext,
    facts: &mut Facts,
    result: &mut Outcome,
    actual: Fact,
    expected: Fact,
    valid: impl Fn(&Node) -> Option<bool>,
) -> Result<bool> {
    let mut possible = false;
    for i in 0..facts.arm_count(actual) {
        ctx.charge(1)?;
        let arm = facts.arm(actual, i);
        if arm == Atom::Never.fact() || facts.relation(ctx, arm, expected)? == Relation::Rejected {
            continue;
        }
        match valid(facts.node(arm)) {
            Some(false) => {
                result.failures.push(ctx, Failure::BuiltinDomain(arm))?;
                result.throws |= RUNTIME;
            }
            valid => {
                possible = true;
                if valid.is_none() {
                    result.throws |= RUNTIME;
                }
            }
        }
    }
    Ok(possible)
}

pub(super) fn keyword(
    ctx: &mut CallContext,
    facts: &mut Facts,
    result: &mut Outcome,
    argument: super::super::arguments::Keyword,
    expected: Fact,
) -> Result<bool> {
    if facts.relation(ctx, argument.value, expected)? == Relation::Rejected {
        result.failures.push(
            ctx,
            Failure::BuiltinKeywordType {
                name: argument.name,
                actual: argument.value,
                expected,
            },
        )?;
    }
    let mut possible = false;
    for i in 0..facts.arm_count(argument.value) {
        ctx.charge(1)?;
        let arm = facts.arm(argument.value, i);
        let relation = facts.relation(ctx, arm, expected)?;
        possible |= arm != Atom::Never.fact() && relation != Relation::Rejected;
        if relation != Relation::Accepted {
            result.throws |= RUNTIME;
        }
    }
    Ok(possible)
}

pub(super) fn name(facts: &Facts, value: Fact) -> Option<&[u8]> {
    match facts.node(value) {
        Node::String(value) | Node::Symbol(value) => value.as_bytes(),
        _ => None,
    }
}

fn random(
    ctx: &mut CallContext,
    facts: &mut Facts,
    method: Random,
    args: &Arguments,
) -> Result<Outcome> {
    let mut result = outcome(Atom::Never.fact());
    if method == Random::Uuid {
        result.value = Atom::String.fact();
        result.throws = u8::MAX;
        return Ok(result);
    }
    let expected = match method {
        Random::Rand => facts.union(
            ctx,
            &[Atom::Nil.fact(), Atom::Int.fact(), Atom::Range.fact()],
        )?,
        Random::Seed => facts.union(ctx, &[Atom::Nil.fact(), Atom::Int.fact()])?,
        _ => Atom::Int.fact(),
    };
    let actual = if let Some(&actual) = args.positional.data.first() {
        parameter(ctx, facts, &mut result, 0, actual, expected)?;
        actual
    } else if method == Random::Id {
        facts.integer(ctx, 16)?
    } else {
        Atom::Nil.fact()
    };
    let mut returns = Buffer::empty();
    for i in 0..facts.arm_count(actual) {
        ctx.charge(1)?;
        let arm = facts.arm(actual, i);
        if arm == Atom::Never.fact() || facts.relation(ctx, arm, expected)? == Relation::Rejected {
            continue;
        }
        let mut entropy = true;
        if method == Random::Seed && facts.atom(arm) == Some(Atom::Int) {
            entropy = false;
        }
        let valid = match (method, facts.node(arm)) {
            (Random::Seed, Node::Integer(_)) => {
                entropy = false;
                Some(true)
            }
            (Random::Rand | Random::Seed, Node::Atom(Atom::Nil)) => Some(true),
            (Random::Rand | Random::Id, Node::Integer(n)) => {
                if method == Random::Id && *n > 1024 {
                    result.throws |= LIMIT;
                    continue;
                }
                Some(*n > 0)
            }
            (Random::Rand, Node::Range(start, end, exclusive)) => {
                Some(matches!((start, end), (Some(a), Some(b)) if a != b || !exclusive))
            }
            _ => None,
        };
        if valid == Some(false) {
            result.failures.push(ctx, Failure::BuiltinDomain(arm))?;
            result.throws |= RUNTIME;
            continue;
        }
        let value = match method {
            Random::Rand => match facts.atom(arm) {
                Some(Atom::Nil) => Atom::Float.fact(),
                Some(Atom::Int | Atom::Range) => Atom::Int.fact(),
                _ => facts.union(ctx, &[Atom::Int.fact(), Atom::Float.fact()])?,
            },
            Random::Seed => facts.union(ctx, &[Atom::Nil.fact(), Atom::Int.fact()])?,
            Random::Id => Atom::String.fact(),
            Random::Uuid => unreachable!(),
        };
        returns.push(ctx, value)?;
        if entropy {
            result.throws = u8::MAX;
        } else if valid.is_none() {
            result.throws |= RUNTIME;
        }
    }
    result.value = facts.union(ctx, &returns.data)?;
    Ok(result)
}
