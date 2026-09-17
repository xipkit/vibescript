use super::*;

const LIMIT: u8 = 1 << ErrorClass::Limit as u8;

fn unit(name: &str) -> bool {
    matches!(
        name,
        "second"
            | "seconds"
            | "minute"
            | "minutes"
            | "hour"
            | "hours"
            | "day"
            | "days"
            | "week"
            | "weeks"
    )
}

pub(super) fn supported(facts: &Facts, receiver: Fact, name: &str) -> bool {
    matches!(
        facts.atom(receiver),
        Some(Atom::Time | Atom::Duration | Atom::Money | Atom::Regex)
    ) || (unit(name)
        && matches!(
            facts.atom(receiver),
            Some(
                Atom::Int
                    | Atom::Float
                    | Atom::Bool
                    | Atom::Nil
                    | Atom::String
                    | Atom::Symbol
                    | Atom::Range
            )
        ))
}

fn reject(ctx: &mut CallContext, failure: Failure) -> Result<Outcome> {
    let mut result = outcome(Atom::Never.fact());
    result.failures.push(ctx, failure)?;
    result.throws = RUNTIME;
    Ok(result)
}

fn shape(
    ctx: &mut CallContext,
    facts: &mut Facts,
    names: &[&str],
    values: &[Fact],
) -> Result<Fact> {
    let mut fields = Buffer::empty();
    for (name, &value) in names.iter().zip(values) {
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

pub(super) fn member(
    ctx: &mut CallContext,
    facts: &mut Facts,
    receiver: Fact,
    site: CallSite,
    name: &str,
    args: &Arguments,
) -> Result<Outcome> {
    if site.scope {
        return reject(ctx, Failure::Undefined);
    }
    let kind = facts.atom(receiver).unwrap();
    let count = args.positional.data.len();
    if matches!(
        name,
        "nil?" | "itself" | "dup" | "clone" | "freeze" | "frozen?" | "eql?" | "equal?"
    ) {
        if count != usize::from(matches!(name, "eql?" | "equal?")) {
            return reject(ctx, Failure::BuiltinArity);
        }
        if !args.keywords.data.is_empty() {
            return reject(ctx, Failure::BuiltinKeywords);
        }
        let value = match name {
            "nil?" => facts.boolean(ctx, false)?,
            "frozen?" => facts.boolean(ctx, true)?,
            "eql?" | "equal?" => Atom::Bool.fact(),
            _ => receiver,
        };
        return Ok(outcome(value));
    }
    if crate::members::names::universal(name) {
        let mut result = outcome(Atom::Never.fact());
        result.incomplete = true;
        return Ok(result);
    }
    if kind == Atom::Money && name == "format" {
        return Ok(outcome(Atom::String.fact()));
    }
    if !args.keywords.data.is_empty()
        && !(kind == Atom::Regex && matches!(name, "source" | "flags"))
    {
        return reject(ctx, Failure::BuiltinKeywords);
    }
    if kind == Atom::Int {
        if !site.auto {
            return reject(ctx, Failure::NonCallable);
        }
        let mut result = outcome(Atom::Duration.fact());
        if !matches!(facts.node(receiver), Node::Integer(_)) {
            result.throws = RUNTIME;
        }
        return Ok(result);
    }
    match kind {
        Atom::Time => time(ctx, facts, receiver, site, name, args),
        Atom::Duration => duration(ctx, facts, receiver, site, name, args),
        Atom::Money => money(ctx, facts, receiver, site, name, args),
        Atom::Regex => regex(ctx, facts, receiver, name, args),
        _ => reject(ctx, Failure::Undefined),
    }
}

fn between(
    ctx: &mut CallContext,
    facts: &mut Facts,
    receiver: Fact,
    args: &Arguments,
) -> Result<Outcome> {
    if args.positional.data.len() != 2 {
        return reject(ctx, Failure::BuiltinArity);
    }
    let kind = facts.atom(receiver).unwrap();
    let mut result = outcome(Atom::Never.fact());
    let lower = parameter(
        ctx,
        facts,
        &mut result,
        0,
        args.positional.data[0],
        kind.fact(),
    )?;
    parameter(
        ctx,
        facts,
        &mut result,
        1,
        args.positional.data[1],
        kind.fact(),
    )?;
    // A failed lower comparison skips the upper comparison, even when its
    // value is incompatible. Keep that possible normal result.
    if lower {
        result.value = Atom::Bool.fact();
    }
    if kind == Atom::Money {
        result.throws |= RUNTIME;
    }
    Ok(result)
}

fn duration(
    ctx: &mut CallContext,
    facts: &mut Facts,
    receiver: Fact,
    site: CallSite,
    name: &str,
    args: &Arguments,
) -> Result<Outcome> {
    let count = args.positional.data.len();
    if name == "between?" {
        return between(ctx, facts, receiver, args);
    }
    if matches!(
        name,
        "after" | "since" | "from_now" | "ago" | "before" | "until"
    ) {
        if site.auto {
            return reject(ctx, Failure::BuiltinValue);
        }
        if count > 1 {
            return reject(ctx, Failure::BuiltinArity);
        }
        let mut result = outcome(Atom::Time.fact());
        if let Some(&actual) = args.positional.data.first() {
            let expected = facts.union(ctx, &[Atom::Time.fact(), Atom::String.fact()])?;
            if !parameter(ctx, facts, &mut result, 0, actual, expected)? {
                result.value = Atom::Never.fact();
            }
            for i in 0..facts.arm_count(actual) {
                ctx.charge(1)?;
                if facts.atom(facts.arm(actual, i)) == Some(Atom::String) {
                    result.throws |= RUNTIME;
                }
            }
        }
        return Ok(result);
    }
    let render = matches!(name, "to_s" | "string" | "inspect");
    if !render && !site.auto {
        return reject(ctx, Failure::NonCallable);
    }
    if count != 0 {
        return reject(ctx, Failure::BuiltinArity);
    }
    let value = if unit(name) {
        Atom::Int.fact()
    } else {
        match name {
            "to_i" => Atom::Int.fact(),
            "in_seconds" | "in_minutes" | "in_hours" | "in_days" | "in_weeks" | "in_months"
            | "in_years" => Atom::Float.fact(),
            "to_s" | "string" | "inspect" | "iso8601" | "format" => Atom::String.fact(),
            "parts" => shape(
                ctx,
                facts,
                &["days", "hours", "minutes", "seconds"],
                &[Atom::Int.fact(); 4],
            )?,
            _ => return reject(ctx, Failure::Undefined),
        }
    };
    Ok(outcome(value))
}

fn money(
    ctx: &mut CallContext,
    facts: &mut Facts,
    receiver: Fact,
    site: CallSite,
    name: &str,
    args: &Arguments,
) -> Result<Outcome> {
    if name == "between?" {
        return between(ctx, facts, receiver, args);
    }
    if matches!(name, "currency" | "cents" | "amount") && !site.auto {
        return reject(ctx, Failure::NonCallable);
    }
    if !args.positional.data.is_empty() {
        return reject(ctx, Failure::BuiltinArity);
    }
    let value = match name {
        "currency" | "amount" | "to_s" | "string" | "inspect" => Atom::String.fact(),
        "cents" => Atom::Int.fact(),
        _ => return reject(ctx, Failure::Undefined),
    };
    Ok(outcome(value))
}

fn precision(
    ctx: &mut CallContext,
    facts: &mut Facts,
    result: &mut Outcome,
    actual: Fact,
    limited: bool,
) -> Result<bool> {
    parameter(ctx, facts, result, 0, actual, Atom::Int.fact())?;
    let mut possible = false;
    for i in 0..facts.arm_count(actual) {
        ctx.charge(1)?;
        let arm = facts.arm(actual, i);
        if arm == Atom::Never.fact()
            || facts.relation(ctx, arm, Atom::Int.fact())? == Relation::Rejected
        {
            continue;
        }
        match facts.node(arm) {
            Node::Integer(n) if *n < 0 => {
                result.failures.push(ctx, Failure::BuiltinDomain(arm))?;
                result.throws |= RUNTIME;
            }
            Node::Integer(n) if limited && *n > 100 => result.throws |= LIMIT,
            Node::Integer(_) => possible = true,
            _ => {
                possible = true;
                result.throws |= RUNTIME | if limited { LIMIT } else { 0 };
            }
        }
    }
    Ok(possible)
}

fn time(
    ctx: &mut CallContext,
    facts: &mut Facts,
    receiver: Fact,
    site: CallSite,
    name: &str,
    args: &Arguments,
) -> Result<Outcome> {
    let count = args.positional.data.len();
    if name == "between?" {
        return between(ctx, facts, receiver, args);
    }
    let arity = match name {
        "<=>" | "format" | "strftime" => count == 1,
        "iso8601" | "xmlschema" | "rfc3339" | "getlocal" | "localtime" | "round" => count <= 1,
        _ => count == 0,
    };
    if !arity {
        return reject(ctx, Failure::BuiltinArity);
    }
    let mut result = outcome(Atom::Never.fact());
    let mut possible = true;
    let value = match name {
        "<=>" => {
            let actual = args.positional.data[0];
            let mut values = Buffer::empty();
            for i in 0..facts.arm_count(actual) {
                ctx.charge(1)?;
                let arm = facts.arm(actual, i);
                if arm == Atom::Never.fact() {
                    continue;
                }
                if facts.relation(ctx, arm, Atom::Time.fact())? != Relation::Rejected {
                    values.push(ctx, Atom::Int.fact())?;
                }
                if facts.relation(ctx, arm, Atom::Time.fact())? != Relation::Accepted {
                    values.push(ctx, Atom::Nil.fact())?;
                }
            }
            facts.union(ctx, &values.data)?
        }
        "to_s" | "string" | "inspect" | "httpdate" | "rfc2822" | "rfc822" => Atom::String.fact(),
        "format" | "strftime" => {
            if site.auto {
                return reject(ctx, Failure::BuiltinValue);
            }
            possible = parameter(
                ctx,
                facts,
                &mut result,
                0,
                args.positional.data[0],
                Atom::String.fact(),
            )?;
            result.throws |= RUNTIME | LIMIT;
            Atom::String.fact()
        }
        "iso8601" | "xmlschema" | "rfc3339" => {
            if let Some(&actual) = args.positional.data.first() {
                possible = precision(ctx, facts, &mut result, actual, true)?;
            }
            Atom::String.fact()
        }
        "getlocal" | "localtime" => {
            if let Some(&actual) = args.positional.data.first() {
                let expected = facts.union(ctx, &[Atom::String.fact(), Atom::Nil.fact()])?;
                possible = parameter(ctx, facts, &mut result, 0, actual, expected)?;
            }
            result.throws |= RUNTIME;
            Atom::Time.fact()
        }
        "round" | "ceil" | "floor" => {
            if let Some(&actual) = args.positional.data.first() {
                possible = precision(ctx, facts, &mut result, actual, false)?;
            }
            Atom::Time.fact()
        }
        _ => {
            if !site.auto {
                return reject(ctx, Failure::NonCallable);
            }
            match name {
                "getutc" | "getgm" | "utc" | "gmtime" => Atom::Time.fact(),
                "year" | "month" | "mon" | "day" | "mday" | "hour" | "min" | "sec" | "wday"
                | "yday" | "nsec" | "tv_nsec" | "usec" | "tv_usec" | "hash" | "to_i" | "tv_sec"
                | "utc_offset" | "gmt_offset" | "gmtoff" => Atom::Int.fact(),
                "subsec" | "to_f" | "to_r" => Atom::Float.fact(),
                "zone" => Atom::String.fact(),
                "utc?" | "gmt?" | "dst?" | "isdst" | "sunday?" | "monday?" | "tuesday?"
                | "wednesday?" | "thursday?" | "friday?" | "saturday?" => Atom::Bool.fact(),
                "to_a" => facts.tuple(
                    ctx,
                    &[
                        Atom::Int.fact(),
                        Atom::Int.fact(),
                        Atom::Int.fact(),
                        Atom::Int.fact(),
                        Atom::Int.fact(),
                        Atom::Int.fact(),
                        Atom::Int.fact(),
                        Atom::Int.fact(),
                        Atom::Bool.fact(),
                        Atom::String.fact(),
                    ],
                )?,
                _ => return reject(ctx, Failure::Undefined),
            }
        }
    };
    if possible {
        result.value = value;
    }
    Ok(result)
}

fn regex(
    ctx: &mut CallContext,
    facts: &mut Facts,
    receiver: Fact,
    name: &str,
    args: &Arguments,
) -> Result<Outcome> {
    let count = args.positional.data.len();
    if count != usize::from(matches!(name, "match" | "match?")) {
        return reject(ctx, Failure::BuiltinArity);
    }
    let mut result = outcome(Atom::Never.fact());
    match name {
        "source" | "flags" => {
            let literal = if let Node::Regex(value) = facts.node(receiver) {
                Some(value.clone())
            } else {
                None
            };
            result.value = if let Some(value) = literal {
                let Kind::Regex(regex) = &value.0 else {
                    unreachable!()
                };
                let bytes = if name == "source" {
                    regex.source.as_bytes().unwrap()
                } else {
                    regex.flags().as_bytes()
                };
                facts.string(ctx, bytes)?
            } else {
                Atom::String.fact()
            };
        }
        "string" | "inspect" => result.value = Atom::String.fact(),
        "match" | "match?" => {
            if parameter(
                ctx,
                facts,
                &mut result,
                0,
                args.positional.data[0],
                Atom::String.fact(),
            )? {
                if name == "match?" {
                    result.value = Atom::Bool.fact();
                } else {
                    result.incomplete = true;
                }
                result.throws |= LIMIT;
            }
        }
        _ => return reject(ctx, Failure::Undefined),
    }
    Ok(result)
}
