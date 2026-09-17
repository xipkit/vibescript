use super::{
    native::{domain, integer, keyword, name},
    *,
};
use crate::time::Constructor as Time;

pub(super) fn invoke(
    ctx: &mut CallContext,
    facts: &mut Facts,
    builtin: Builtin,
    args: &Arguments,
) -> Result<Outcome> {
    let mut result = outcome(Atom::Never.fact());
    let count = args.positional.data.len();
    let arity = match builtin {
        Builtin::DurationBuild => count == usize::from(args.keywords.data.is_empty()),
        Builtin::Time(Time::Now) => count == 0,
        Builtin::Time(Time::Parse) => (1..=2).contains(&count),
        Builtin::Time(Time::At) => (1..=3).contains(&count),
        Builtin::Time(Time::New) => count >= 1,
        Builtin::Time(_) => (1..=7).contains(&count),
        _ => unreachable!(),
    };
    if !arity {
        result.failures.push(ctx, Failure::BuiltinArity)?;
        result.throws = RUNTIME;
        return Ok(result);
    }
    let numeric = facts.union(ctx, &[Atom::Int.fact(), Atom::Float.fact()])?;
    let nullable_string = facts.union(ctx, &[Atom::String.fact(), Atom::Nil.fact()])?;
    let mut possible = true;
    for &arg in &args.keywords.data {
        ctx.charge(1)?;
        if builtin == Builtin::DurationBuild {
            if matches!(
                name(facts, arg.name),
                Some(b"weeks" | b"days" | b"hours" | b"minutes" | b"seconds")
            ) {
                possible &= keyword(ctx, facts, &mut result, arg, numeric)?;
                possible &= domain(ctx, facts, &mut result, arg.value, numeric, integer)?;
            } else {
                result
                    .failures
                    .push(ctx, Failure::BuiltinKeyword(arg.name))?;
                result.throws |= RUNTIME;
                possible = false;
            }
        } else if let Builtin::Time(constructor) = builtin {
            if matches!(constructor, Time::New | Time::At | Time::Now | Time::Parse)
                && name(facts, arg.name) == Some(b"in")
            {
                possible &= keyword(ctx, facts, &mut result, arg, nullable_string)?;
                result.throws |= RUNTIME;
            } else if matches!(constructor, Time::At | Time::Parse) {
                result
                    .failures
                    .push(ctx, Failure::BuiltinKeyword(arg.name))?;
                result.throws |= RUNTIME;
                possible = false;
            }
        }
    }
    if builtin == Builtin::DurationBuild {
        if let Some(&actual) = args.positional.data.first() {
            possible &= parameter(ctx, facts, &mut result, 0, actual, numeric)?;
            possible &= domain(ctx, facts, &mut result, actual, numeric, integer)?;
        }
        if possible {
            result.value = Atom::Duration.fact();
        }
        return Ok(result);
    }
    let Builtin::Time(constructor) = builtin else {
        unreachable!()
    };
    if constructor != Time::Now {
        result.throws |= RUNTIME;
    }
    for (index, &actual) in args.positional.data.iter().enumerate() {
        ctx.charge(1)?;
        let expected = match (constructor, index) {
            (Time::Parse, 0) => Atom::String.fact(),
            (Time::Parse, 1) | (Time::New, 6) => nullable_string,
            (Time::At, 2) => Atom::Symbol.fact(),
            (_, 0) | (Time::At, 1) => numeric,
            (_, 6) => facts.union(ctx, &[numeric, Atom::Nil.fact()])?,
            _ => continue,
        };
        possible &= parameter(ctx, facts, &mut result, index, actual, expected)?;
        if constructor == Time::Parse || (constructor == Time::New && index == 6) {
            continue;
        }
        possible &= domain(
            ctx,
            facts,
            &mut result,
            actual,
            expected,
            |node| match node {
                Node::Symbol(value) if constructor == Time::At && index == 2 => Some(matches!(
                    value.as_bytes(),
                    Some(b"microsecond" | b"usec" | b"millisecond" | b"nanosecond" | b"nsec")
                )),
                Node::Integer(n) if index == 6 => Some((0..1_000_000).contains(n)),
                Node::Integer(_) => Some(true),
                Node::Float(bits) => {
                    let n = f64::from_bits(*bits);
                    Some(
                        n.is_finite()
                            && if constructor == Time::At {
                                true
                            } else if index == 6 {
                                (0.0..1_000_000.0).contains(&n)
                            } else {
                                n > i64::MIN as f64 && n < i64::MAX as f64
                            },
                    )
                }
                Node::Atom(Atom::Nil) if index == 6 => Some(true),
                _ => None,
            },
        )?;
    }
    if possible {
        result.value = Atom::Time.fact();
    }
    Ok(result)
}
