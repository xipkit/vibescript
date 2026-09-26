use crate::{
    CallContext, Error, ErrorKind, Result, Value,
    budget::{Buffer, CHUNK, MAX_VALUE_DEPTH},
    bytecode::Method,
    hash::Hash,
    json,
    scan::{self, Class},
    value::Kind,
};
use std::{cmp::Ordering, fmt::Write, sync::Arc};

fn type_error() -> Error {
    Error::new(ErrorKind::Type, "unsupported operand types")
}

/// Refuses operands that `op` cannot combine, naming the operation as Go does.
pub(crate) fn unsupported(op: &str) -> Error {
    Error::new(
        ErrorKind::Type,
        match op {
            "+" => "unsupported addition operands",
            "-" => "unsupported subtraction operands",
            "*" => "unsupported multiplication operands",
            "/" => "unsupported division operands",
            "//" => "unsupported floor division operands",
            "%" => "unsupported modulo operands",
            "**" => "unsupported exponentiation operands",
            "<<" => "unsupported shovel operands",
            "&" => "unsupported intersection operands",
            "<" | "<=" | ">" | ">=" | "<=>" => "unsupported comparison operands",
            _ => "unsupported operator",
        },
    )
}

/// The reference's message when `array.sum` meets values `+` cannot add.
pub(crate) const SUM_INCOMPATIBLE: &str = "array.sum cannot add incompatible values";

/// Relabels an addition that failed inside `array.sum` as the reference does.
/// Limit and interruption errors keep their own message.
pub(crate) fn sum_incompatible(error: Error) -> Error {
    if error.class() == Some(crate::ErrorClass::Runtime) {
        error.with_message(SUM_INCOMPATIBLE.to_owned())
    } else {
        error
    }
}

/// Rejects arguments to an array, string or hash member that takes none,
/// naming the receiver kind; other receivers keep the plain count.
fn member_arity(value: &Value, name: &str, args: &[Value]) -> Result<()> {
    if args.is_empty() {
        return Ok(());
    }
    let kind = match value.0 {
        Kind::Array(_) => "array",
        Kind::Bytes(_) => "string",
        Kind::Hash(_) => "hash",
        _ => return arity(args, 0),
    };
    Err(Error::new(
        ErrorKind::Argument,
        format!("{kind}.{name} does not take arguments"),
    ))
}

pub(crate) fn unary(ctx: &mut CallContext, op: &str, value: Value) -> Result<Value> {
    match (op, &value.0) {
        ("!", _) => Ok(Value::boolean(!value.truthy())),
        ("+", Kind::Int(_) | Kind::Big(_) | Kind::Float(_) | Kind::Bytes(_)) => Ok(value),
        ("-", Kind::Int(n)) if *n != i64::MIN => Ok(Value::int(-n)),
        ("-", Kind::Int(_) | Kind::Big(_)) => crate::integer::negate(ctx, &value, false),
        ("-", Kind::Float(n)) => Ok(Value::float(-n)),
        ("-", _) => Err(Error::new(ErrorKind::Type, "unsupported unary - operand")),
        ("+", _) => Err(Error::new(ErrorKind::Type, "unsupported unary + operand")),
        _ => Err(Error::new(ErrorKind::Type, "unsupported unary operator")),
    }
}

/// Applies `op` to two compact integers or two floats when the result is
/// another immediate, charging exactly what [`binary`] charges for them.
/// Returns `None` for other operands and operators, or when an integer result
/// overflows, leaving those to [`binary`].
#[inline(always)]
pub(crate) fn immediate(
    ctx: &mut CallContext,
    op: &str,
    a: &Value,
    b: &Value,
) -> Result<Option<Value>> {
    let value = match (&a.0, &b.0) {
        (Kind::Int(a), Kind::Int(b)) => match op {
            "+" => a.checked_add(*b).map(Value::int),
            "-" => a.checked_sub(*b).map(Value::int),
            "*" => a.checked_mul(*b).map(Value::int),
            "/" | "%" if *b != 0 => floor_divide(op, *a, *b).map(Value::int),
            "//" if *b != 0 => floor_divide("/", *a, *b).map(Value::int),
            "<" => Some(Value::boolean(a < b)),
            "<=" => Some(Value::boolean(a <= b)),
            ">" => Some(Value::boolean(a > b)),
            ">=" => Some(Value::boolean(a >= b)),
            "==" | "!=" => {
                // Equality charges one step per compared pair.
                ctx.charge(1)?;
                Some(Value::boolean((a == b) == (op == "==")))
            }
            _ => None,
        },
        (Kind::Float(a), Kind::Float(b)) => match op {
            "+" => Some(Value::float(a + b)),
            "-" => Some(Value::float(a - b)),
            "*" => Some(Value::float(a * b)),
            "/" => Some(Value::float(a / b)),
            "//" => Some(Value::float((a / b).floor())),
            "%" => Some(Value::float(float_modulo(*a, *b))),
            "<" => Some(Value::boolean(a < b)),
            "<=" => Some(Value::boolean(a <= b)),
            ">" => Some(Value::boolean(a > b)),
            ">=" => Some(Value::boolean(a >= b)),
            "==" | "!=" => {
                ctx.charge(1)?;
                Some(Value::boolean((a == b) == (op == "==")))
            }
            _ => None,
        },
        _ => None,
    };
    Ok(value)
}

/// Divides or takes the modulo of compact integers, rounding toward negative
/// infinity, or returns `None` when the quotient overflows. `b` is nonzero.
fn floor_divide(op: &str, a: i64, b: i64) -> Option<i64> {
    let q = a.checked_div(b)?;
    let r = a % b;
    let adjust = r != 0 && (r < 0) != (b < 0);
    Some(if op == "/" {
        if adjust { q - 1 } else { q }
    } else if adjust {
        r + b
    } else {
        r
    })
}

fn float_modulo(a: f64, b: f64) -> f64 {
    let remainder = a % b;
    if remainder != 0.0 && (remainder < 0.0) != (b < 0.0) {
        remainder + b
    } else {
        remainder
    }
}

pub(crate) fn binary(ctx: &mut CallContext, op: &str, a: Value, b: Value) -> Result<Value> {
    if op == "//" {
        return floor_division(ctx, a, b);
    }
    if op == "%" {
        if let Kind::Bytes(pattern) = &a.0 {
            let values = b.as_array().unwrap_or_else(|| std::slice::from_ref(&b));
            return crate::format::format(ctx, &pattern.data, values);
        }
    }
    if matches!(op, "=~" | "!~") {
        return crate::regex::value::binary(ctx, op, &a, &b);
    }
    if op == "<=>" {
        return crate::ordering::spaceship(ctx, &a, &b);
    }
    if op == "===" {
        return Ok(Value::boolean(case_matches(ctx, Some(&b), &a, false)?));
    }
    if matches!(op, "==" | "!=") {
        let same = equal(ctx, &a, &b, 0)?;
        return Ok(Value::boolean(if op == "==" { same } else { !same }));
    }
    if matches!(op, "<" | "<=" | ">" | ">=") {
        let cmp = compare(ctx, &a, &b).map_err(|error| {
            if error.kind == ErrorKind::Type {
                error.with_class(crate::ErrorClass::Argument)
            } else {
                error
            }
        })?;
        return Ok(Value::boolean(match op {
            "<" => cmp == Some(Ordering::Less),
            "<=" => matches!(cmp, Some(Ordering::Less | Ordering::Equal)),
            ">" => cmp == Some(Ordering::Greater),
            _ => matches!(cmp, Some(Ordering::Greater | Ordering::Equal)),
        }));
    }
    if op == "<<" {
        if !matches!(a.0, Kind::Array(_)) {
            return Err(unsupported(op));
        }
        return a.push(ctx, &[b]);
    }
    if op == "&" || (op == "-" && matches!(a.0, Kind::Array(_))) {
        if !matches!(a.0, Kind::Array(_)) || !matches!(b.0, Kind::Array(_)) {
            ctx.checkpoint()?;
            return Err(unsupported(op));
        }
        return crate::sets::binary(ctx, op, &a, &b);
    }
    if op == "+" && matches!(a.0, Kind::Array(_)) {
        if let Some(values) = b.as_array() {
            return a.push(ctx, values);
        }
    }
    if op == "+" && (matches!(a.0, Kind::Bytes(_)) || matches!(b.0, Kind::Bytes(_))) {
        let scalar = |v: &Value| {
            matches!(
                v.0,
                Kind::Bytes(_)
                    | Kind::Symbol(_)
                    | Kind::Int(_)
                    | Kind::Big(_)
                    | Kind::Float(_)
                    | Kind::Bool(_)
                    | Kind::Money(_)
                    | Kind::Duration(_)
                    | Kind::Time(_)
                    | Kind::Zoned(_)
                    | Kind::EnumMember(_)
                    | Kind::Regex(_)
                    | Kind::Range(_)
            )
        };
        if !scalar(&a) || !scalar(&b) {
            return Err(unsupported(op));
        }
        let a = to_string(ctx, &a)?;
        let b = to_string(ctx, &b)?;
        let mut out = Buffer::empty();
        out.extend(ctx, a.require_bytes()?)?;
        out.extend(ctx, b.require_bytes()?)?;
        return Value::from_bytes(ctx, out);
    }
    if crate::time::stamp(&a).is_some() || crate::time::stamp(&b).is_some() {
        return crate::time::binary(ctx, op, &a, &b);
    }
    if matches!(a.0, Kind::Money(_)) || matches!(b.0, Kind::Money(_)) {
        return crate::money::binary(op, &a, &b);
    }
    if matches!(a.0, Kind::Duration(_)) || matches!(b.0, Kind::Duration(_)) {
        return crate::duration::binary(op, &a, &b);
    }
    match (&a.0, &b.0) {
        (Kind::Int(a), Kind::Int(b)) => {
            let n = match op {
                "+" => a.checked_add(*b),
                "-" => a.checked_sub(*b),
                "*" => a.checked_mul(*b),
                "/" | "%" => {
                    if *b == 0 {
                        return Err(zero_division(op));
                    }
                    floor_divide(op, *a, *b)
                }
                "**" => {
                    if *b < 0 {
                        return float_power(*a as f64, *b as f64);
                    }
                    u32::try_from(*b)
                        .ok()
                        .and_then(|power| a.checked_pow(power))
                }
                _ => return Err(unsupported(op)),
            };
            match n {
                Some(n) => Ok(Value::int(n)),
                None => crate::integer::binary(ctx, op, &Value::int(*a), &Value::int(*b)),
            }
        }
        (Kind::Big(_), Kind::Int(0)) if op == "%" => Err(zero_division(op)),
        (Kind::Int(_) | Kind::Big(_), Kind::Int(_) | Kind::Big(_)) => {
            crate::integer::binary(ctx, op, &a, &b)
        }
        (
            Kind::Int(_) | Kind::Big(_) | Kind::Float(_),
            Kind::Int(_) | Kind::Big(_) | Kind::Float(_),
        ) => {
            let a = a.as_float().unwrap();
            let b = b.as_float().unwrap();
            if op == "**" {
                return float_power(a, b);
            }
            Ok(Value::float(match op {
                "+" => a + b,
                "-" => a - b,
                "*" => a * b,
                "/" => a / b,
                "%" => float_modulo(a, b),
                _ => return Err(unsupported(op)),
            }))
        }
        (Kind::Bytes(_), Kind::Big(count)) if op == "*" && count.negative => {
            Err(negative_repetition())
        }
        (Kind::Bytes(s), Kind::Int(_) | Kind::Float(_)) if op == "*" => {
            // A float count truncates toward zero, so a fraction above -1 repeats zero times.
            let n = crate::sequence::integer(&b).map_err(|_| unsupported(op))?;
            let n = usize::try_from(n).map_err(|_| negative_repetition())?;
            let len = s
                .data
                .len()
                .checked_mul(n)
                .ok_or_else(|| Error::new(ErrorKind::Memory, "string size overflow"))?;
            let mut out = Buffer::with_capacity(ctx, len)?;
            if !s.data.is_empty() && n != 0 {
                out.extend(ctx, &s.data)?;
                // Each pass copies everything written so far, doubling it.
                while out.data.len() < len {
                    ctx.charge(1)?;
                    let count = out.data.len().min(len - out.data.len());
                    for start in (0..count).step_by(CHUNK) {
                        let end = count.min(start + CHUNK);
                        ctx.work_bytes(end - start)?;
                        out.data.extend_from_within(start..end);
                    }
                }
            }
            Value::from_bytes(ctx, out)
        }
        _ => Err(unsupported(op)),
    }
}

/// Floor division: integers of any size divide as `/` does, and a float
/// operand gives the floored float quotient, which like float `/` is infinite
/// or NaN for a zero divisor. Money and durations keep their own division.
fn floor_division(ctx: &mut CallContext, a: Value, b: Value) -> Result<Value> {
    match (&a.0, &b.0) {
        (Kind::Int(_) | Kind::Big(_), Kind::Int(_) | Kind::Big(_)) => binary(ctx, "/", a, b),
        (
            Kind::Int(_) | Kind::Big(_) | Kind::Float(_),
            Kind::Int(_) | Kind::Big(_) | Kind::Float(_),
        ) => Ok(Value::float(
            (a.as_float().unwrap() / b.as_float().unwrap()).floor(),
        )),
        _ => Err(unsupported("//")),
    }
}

/// Refuses a zero divisor, naming modulo apart from division as Go does.
pub(crate) fn zero_division(op: &str) -> Error {
    Error::new(
        ErrorKind::Arithmetic,
        if op == "%" {
            "modulo by zero"
        } else {
            "division by zero"
        },
    )
    .with_class(crate::ErrorClass::ZeroDivision)
}

fn negative_repetition() -> Error {
    Error::new(
        ErrorKind::Argument,
        "negative argument for string repetition",
    )
}

pub(crate) fn float_power(base: f64, exponent: f64) -> Result<Value> {
    let value = base.powf(exponent);
    if !value.is_finite() {
        return Err(Error::new(
            ErrorKind::Arithmetic,
            "float exponentiation result is not finite",
        ));
    }
    Ok(Value::float(value))
}

pub(crate) fn case_matches(
    ctx: &mut CallContext,
    target: Option<&Value>,
    candidate: &Value,
    splat: bool,
) -> Result<bool> {
    let candidates = if splat {
        candidate
            .as_array()
            .ok_or_else(|| Error::new(ErrorKind::Type, "case when splat value must be an array"))?
    } else {
        std::slice::from_ref(candidate)
    };
    for candidate in candidates {
        ctx.charge(1)?;
        let matched = if let Some(target) = target {
            if let Kind::Regex(regex) = &candidate.0 {
                regex.matches(ctx, target, "regex match")?
            } else if let (Kind::Range(range), Kind::Int(_) | Kind::Big(_) | Kind::Float(_)) =
                (&candidate.0, &target.0)
            {
                range.contains(target)
            } else {
                // A range tests membership only for numbers; other targets, including
                // ranges, compare by equality.
                equal(ctx, candidate, target, 0)?
            }
        } else {
            candidate.truthy()
        };
        if matched {
            return Ok(true);
        }
    }
    Ok(false)
}

pub(crate) fn compare(ctx: &mut CallContext, a: &Value, b: &Value) -> Result<Option<Ordering>> {
    match (&a.0, &b.0) {
        (Kind::Money(a), Kind::Money(b)) => a
            .order(*b)
            .map(Some)
            .ok_or_else(|| Error::new(ErrorKind::Type, "money currency mismatch for comparison")),
        (Kind::Duration(a), Kind::Duration(b)) => Ok(Some(crate::duration::order(*a, *b))),
        (Kind::Time(_) | Kind::Zoned(_), Kind::Time(_) | Kind::Zoned(_)) => Ok(Some(
            crate::time::stamp(a)
                .unwrap()
                .order(crate::time::stamp(b).unwrap()),
        )),
        (Kind::Int(a), Kind::Int(b)) => Ok(Some(a.cmp(b))),
        (Kind::Int(_) | Kind::Big(_), Kind::Int(_) | Kind::Big(_)) => {
            crate::integer::compare(ctx, a, b).map(Some)
        }
        (Kind::Big(_), Kind::Float(f)) => crate::integer::compare_float(ctx, a, *f),
        (Kind::Float(f), Kind::Big(_)) => {
            crate::integer::compare_float(ctx, b, *f).map(|order| order.map(Ordering::reverse))
        }
        (Kind::Int(_) | Kind::Float(_), Kind::Int(_) | Kind::Float(_)) => {
            Ok(a.as_float().unwrap().partial_cmp(&b.as_float().unwrap()))
        }
        (Kind::Bytes(a), Kind::Bytes(b)) | (Kind::Symbol(a), Kind::Symbol(b)) => {
            for (a, b) in a.data.chunks(CHUNK).zip(b.data.chunks(CHUNK)) {
                ctx.work_bytes(a.len().min(b.len()))?;
                let cmp = a.cmp(b);
                if cmp != Ordering::Equal {
                    return Ok(Some(cmp));
                }
            }
            Ok(Some(a.data.len().cmp(&b.data.len())))
        }
        _ => Err(unsupported("<")),
    }
}

pub(crate) fn equal(ctx: &mut CallContext, a: &Value, b: &Value, depth: usize) -> Result<bool> {
    equal_kinds(ctx, a, b, depth, false)
}

pub(crate) fn eql(ctx: &mut CallContext, a: &Value, b: &Value, depth: usize) -> Result<bool> {
    equal_kinds(ctx, a, b, depth, true)
}

/// One suspended container comparison. Frames only borrow the operands, so
/// unwinding the walk never runs recursive drop glue. `shared` holds the
/// storage addresses of a pair that other paths may reach again.
enum EqualFrame<'a> {
    Array {
        a: &'a [Value],
        b: &'a [Value],
        index: usize,
        shared: Option<(usize, usize)>,
    },
    Hash {
        a: &'a Hash,
        b: &'a Hash,
        index: usize,
        shared: Option<(usize, usize)>,
    },
}

enum EqualStep<'a> {
    Same,
    Different,
    Enter(EqualFrame<'a>),
}

/// Returns the addresses of two containers when either is referenced more
/// than once. A pair reached along two paths must have a shared side, or its
/// parents would be the repeated pair, so only these need remembering.
pub(crate) fn shared_pair<T>(a: &Arc<T>, b: &Arc<T>) -> Option<(usize, usize)> {
    (Arc::strong_count(a) > 1 || Arc::strong_count(b) > 1)
        .then_some((Arc::as_ptr(a) as usize, Arc::as_ptr(b) as usize))
}

fn equal_kinds<'a>(
    ctx: &mut CallContext,
    a: &'a Value,
    b: &'a Value,
    depth: usize,
    strict: bool,
) -> Result<bool> {
    // Shared container pairs already found equal in this walk. A pair that
    // differed ends the walk, so only equal pairs are recorded; the operands
    // stay borrowed until it returns, which keeps every address valid.
    let mut equal = crate::pairs::Pairs::new();
    let mut current = match equal_step(ctx, a, b, depth, strict, &equal)? {
        EqualStep::Same => return Ok(true),
        EqualStep::Different => return Ok(false),
        EqualStep::Enter(mut frame) => {
            // The operands themselves cannot be reached again.
            match &mut frame {
                EqualFrame::Array { shared, .. } | EqualFrame::Hash { shared, .. } => {
                    *shared = None
                }
            }
            frame
        }
    };
    // Suspended ancestors of `current`; charged per push, released on any exit.
    let mut parents: Buffer<EqualFrame<'a>> = Buffer::empty();
    loop {
        let level = depth + parents.data.len() + 1;
        let pair = match &mut current {
            EqualFrame::Array { a, b, index, .. } => {
                let (a, b): (&'a [Value], &'a [Value]) = (*a, *b);
                if *index == a.len() {
                    None
                } else {
                    let i = *index;
                    *index += 1;
                    Some((&a[i], &b[i]))
                }
            }
            EqualFrame::Hash { a, b, index, .. } => {
                let (a, b): (&'a Hash, &'a Hash) = (*a, *b);
                if *index == a.buffer.data.len() {
                    None
                } else {
                    let (key, value) = &a.buffer.data[*index];
                    *index += 1;
                    let Some(i) = b.find(ctx, key.require_bytes()?)? else {
                        return Ok(false);
                    };
                    Some((value, &b.buffer.data[i].1))
                }
            }
        };
        let Some((x, y)) = pair else {
            if let EqualFrame::Array {
                shared: Some((left, right)),
                ..
            }
            | EqualFrame::Hash {
                shared: Some((left, right)),
                ..
            } = current
            {
                equal.insert(ctx, left, right, ())?;
            }
            match parents.data.pop() {
                Some(parent) => current = parent,
                None => return Ok(true),
            }
            continue;
        };
        match equal_step(ctx, x, y, level, strict, &equal)? {
            EqualStep::Same => {}
            EqualStep::Different => return Ok(false),
            EqualStep::Enter(frame) => {
                let suspended = std::mem::replace(&mut current, frame);
                parents.push(ctx, suspended)?;
            }
        }
    }
}

fn equal_step<'a>(
    ctx: &mut CallContext,
    a: &'a Value,
    b: &'a Value,
    depth: usize,
    strict: bool,
    equal: &crate::pairs::Pairs<()>,
) -> Result<EqualStep<'a>> {
    ctx.charge(1)?;
    if depth > MAX_VALUE_DEPTH {
        return ctx.guard(ErrorKind::Recursion, "value nesting too deep");
    }
    if strict && a.type_name() != b.type_name() {
        return Ok(EqualStep::Different);
    }
    // A remembered pair stands in for its walk only where that walk could not
    // reach the nesting limit from this depth.
    let remembered = |shared: Option<(usize, usize)>| {
        depth + a.depth().max(b.depth()) <= MAX_VALUE_DEPTH
            && shared.is_some_and(|(left, right)| equal.get(left, right).is_some())
    };
    let same = match (&a.0, &b.0) {
        (Kind::Nil, Kind::Nil) => true,
        (Kind::Regex(a), Kind::Regex(b)) => a.equal(ctx, b)?,
        (Kind::Shape(a), Kind::Shape(b)) => {
            json::bytes_equal(ctx, &a.definition.text, &b.definition.text)?
        }
        (Kind::Instance(a), Kind::Instance(b)) => a.same(b),
        (Kind::Namespace(a), Kind::Namespace(b)) => a.same_binding(b),
        (Kind::Function(a), Kind::Function(b)) => a.same(b),
        (Kind::Host(a), Kind::Host(b)) => a.same(b),
        (Kind::Enum(a), Kind::Enum(b)) => std::sync::Arc::ptr_eq(&a.definition, &b.definition),
        (Kind::EnumMember(a), Kind::EnumMember(b)) => {
            a.index == b.index
                && std::sync::Arc::ptr_eq(&a.enumeration.definition, &b.enumeration.definition)
        }
        (Kind::Money(a), Kind::Money(b)) => a == b,
        (Kind::Duration(a), Kind::Duration(b)) => a == b,
        (Kind::Time(_) | Kind::Zoned(_), Kind::Time(_) | Kind::Zoned(_)) => {
            crate::time::stamp(a) == crate::time::stamp(b)
        }
        (Kind::Builtin(a), Kind::Builtin(b)) => a == b,
        (Kind::Offset(a), Kind::Offset(b)) => std::sync::Arc::ptr_eq(a, b),
        (Kind::Bool(a), Kind::Bool(b)) => a == b,
        (Kind::Int(a), Kind::Int(b)) => a == b,
        (Kind::Big(_), Kind::Big(_)) => crate::integer::compare(ctx, a, b)? == Ordering::Equal,
        (Kind::Big(_), Kind::Float(f)) => {
            crate::integer::compare_float(ctx, a, *f)? == Some(Ordering::Equal)
        }
        (Kind::Float(f), Kind::Big(_)) => {
            crate::integer::compare_float(ctx, b, *f)? == Some(Ordering::Equal)
        }
        (Kind::Range(a), Kind::Range(b)) => {
            a.start == b.start && a.end == b.end && a.exclusive == b.exclusive
        }
        (Kind::Int(integer), Kind::Float(float)) | (Kind::Float(float), Kind::Int(integer)) => {
            // Casting the integer to float can round a neighboring integer to the
            // same value. Range-check the integral float before converting it.
            float.is_finite()
                && float.fract() == 0.0
                && *float >= i64::MIN as f64
                && *float < -(i64::MIN as f64)
                && *integer == *float as i64
        }
        (Kind::Float(a), Kind::Float(b)) => a == b,
        (Kind::Bytes(a), Kind::Bytes(b)) | (Kind::Symbol(a), Kind::Symbol(b)) => {
            json::bytes_equal(ctx, &a.data, &b.data)?
        }
        (Kind::Array(a), Kind::Array(b)) => {
            if a.buffer.data.len() != b.buffer.data.len() {
                false
            } else if a.buffer.data.is_empty() {
                true
            } else {
                let shared = shared_pair(a, b);
                if remembered(shared) {
                    return Ok(EqualStep::Same);
                }
                return Ok(EqualStep::Enter(EqualFrame::Array {
                    a: &a.buffer.data,
                    b: &b.buffer.data,
                    index: 0,
                    shared,
                }));
            }
        }
        (Kind::Hash(a), Kind::Hash(b)) => {
            if a.object != b.object || a.buffer.data.len() != b.buffer.data.len() {
                false
            } else if a.buffer.data.is_empty() {
                true
            } else {
                let shared = shared_pair(a, b);
                if remembered(shared) {
                    return Ok(EqualStep::Same);
                }
                return Ok(EqualStep::Enter(EqualFrame::Hash {
                    a: a.as_ref(),
                    b: b.as_ref(),
                    index: 0,
                    shared,
                }));
            }
        }
        _ => false,
    };
    Ok(if same {
        EqualStep::Same
    } else {
        EqualStep::Different
    })
}

/// Converts one `value[...]` selector to an integer index as the reference
/// does: a whole or fractional float truncates, and anything else is rejected.
pub(crate) fn index_selector(value: &Value) -> Result<i64> {
    match value.0 {
        Kind::Big(_) => Err(Error::new(
            ErrorKind::Type,
            "index must fit in a 64-bit integer",
        )),
        _ => crate::sequence::integer(value)
            .map_err(|_| Error::new(ErrorKind::Type, "index must be integer")),
    }
}

/// Rejects `value[...]` on a kind that has no index operator.
pub(crate) fn cannot_index(value: &Value) -> Error {
    Error::new(
        ErrorKind::Type,
        format!("cannot index {}", value.type_name()),
    )
}

/// Reads `value[selector]`.
pub(crate) fn index(ctx: &mut CallContext, value: &Value, index: &Value) -> Result<Value> {
    if matches!(index.0, Kind::Range(_)) && matches!(value.0, Kind::Array(_) | Kind::Bytes(_)) {
        return crate::sequence::slice(ctx, value, std::slice::from_ref(index), false, None);
    }
    match &value.0 {
        Kind::Array(h) => {
            let n = normalized(index_selector(index)?, h.buffer.data.len());
            Ok(n.and_then(|n| h.buffer.data.get(n))
                .cloned()
                .unwrap_or_default())
        }
        Kind::Hash(h) => {
            if let Some(value) = crate::regex::matches::index(ctx, h, index)? {
                return Ok(value);
            }
            let key = index.hash_key()?;
            Ok(h.find(ctx, key)?
                .map(|i| h.buffer.data[i].1.clone())
                .unwrap_or_default())
        }
        Kind::Bytes(h) => {
            let bytes = &h.data;
            let n = index_selector(index)?;
            let n = if n < 0 {
                let (count, _) = runes(ctx, bytes)?;
                normalized(n, count)
            } else {
                usize::try_from(n).ok()
            };
            let Some(n) = n else {
                return Ok(Value::nil());
            };
            let mut pos = 0;
            let mut count = 0;
            while pos < bytes.len() {
                ctx.charge(1)?;
                let (ch, len, valid) = scan::rune(&bytes[pos..]);
                if count == n {
                    if !valid {
                        let mut encoded = [0; 4];
                        return ctx.bytes(ch.encode_utf8(&mut encoded).as_bytes());
                    }
                    return ctx.bytes(&bytes[pos..pos + len]);
                }
                count += 1;
                pos += len;
            }
            Ok(Value::nil())
        }
        _ => Err(cannot_index(value)),
    }
}

/// Reads `value[start, length]`, the only form with more than one selector.
pub(crate) fn index_many(ctx: &mut CallContext, value: &Value, args: &[Value]) -> Result<Value> {
    match &value.0 {
        Kind::Array(_) | Kind::Bytes(_) => {
            let [start, length] = args else {
                return Err(Error::new(
                    ErrorKind::Argument,
                    format!(
                        "{} index expects one index, a start and length, or a range",
                        value.type_name()
                    ),
                ));
            };
            if matches!(start.0, Kind::Range(_)) {
                return Err(Error::new(ErrorKind::Argument, "index must be integer"));
            }
            index_selector(start)?;
            index_selector(length)?;
            crate::sequence::slice(ctx, value, args, false, None)
        }
        Kind::Hash(_) => Err(Error::new(
            ErrorKind::Type,
            format!("{} index expects a single key", value.type_name()),
        )),
        _ => Err(cannot_index(value)),
    }
}

pub(crate) fn normalized(index: i64, len: usize) -> Option<usize> {
    if index < 0 {
        len.checked_sub(usize::try_from(index.unsigned_abs()).ok()?)
    } else {
        usize::try_from(index).ok()
    }
}

pub(crate) fn set_index(
    ctx: &mut CallContext,
    root: Value,
    key: Value,
    value: Value,
) -> Result<Value> {
    match &root.0 {
        Kind::Array(h) => {
            let n = normalized(index_selector(&key)?, h.buffer.data.len())
                .ok_or_else(|| Error::new(ErrorKind::Argument, "array index out of bounds"))?;
            let len = h.buffer.data.len();
            if n >= len {
                return Err(Error::new(ErrorKind::Argument, "array index out of bounds"));
            }
            root.set_array_index(ctx, n, value)
        }
        Kind::Hash(_) => {
            let key = if matches!(key.0, Kind::Symbol(_)) {
                ctx.bytes(key.hash_key()?)?
            } else {
                key
            };
            key.hash_key()?;
            root.set_hash_index(ctx, key, value)
        }
        _ => Err(cannot_index(&root)),
    }
}

pub(crate) fn arity(args: &[Value], n: usize) -> Result<()> {
    if args.len() == n {
        Ok(())
    } else {
        Err(Error::new(
            ErrorKind::Argument,
            format!("expected {n} arguments, got {}", args.len()),
        ))
    }
}

fn index_offset(name: &str, value: &Value) -> Result<usize> {
    match crate::sequence::integer(value) {
        Ok(n) if n >= 0 => Ok(usize::try_from(n).unwrap_or(usize::MAX)),
        _ => Err(Error::new(
            ErrorKind::Argument,
            format!("array.{name} offset must be non-negative integer"),
        )),
    }
}

fn array_index(
    ctx: &mut CallContext,
    array: &[Value],
    needle: &Value,
    offset: Option<usize>,
    reverse: bool,
) -> Result<Option<usize>> {
    if array.is_empty() {
        return Ok(None);
    }
    if reverse {
        let start = offset.unwrap_or(array.len() - 1).min(array.len() - 1);
        for i in (0..=start).rev() {
            if equal(ctx, &array[i], needle, 0)? {
                return Ok(Some(i));
            }
        }
    } else {
        let start = offset.unwrap_or(0);
        for (i, item) in array.iter().enumerate().skip(start) {
            if equal(ctx, item, needle, 0)? {
                return Ok(Some(i));
            }
        }
    }
    Ok(None)
}

pub(crate) fn method(
    ctx: &mut CallContext,
    method: Method,
    name: &str,
    value: Value,
    args: &[Value],
) -> Result<Value> {
    use Method::*;
    match method {
        IsNil => {
            crate::members::universal_shape(name, &value, args.len(), false, false)?;
            return Ok(Value::boolean(matches!(value.0, Kind::Nil)));
        }
        Itself | Dup => {
            crate::members::universal_shape(name, &value, args.len(), false, false)?;
            return Ok(value);
        }
        ToString => {
            if matches!(value.0, Kind::Array(_)) {
                member_arity(&value, name, args)?;
            }
            if matches!(value.0, Kind::Int(_) | Kind::Big(_) | Kind::Float(_)) {
                let kind = value.type_name();
                crate::members::nullary(format_args!("{kind}.{name}"), args.len(), false, false)?;
            }
            arity(args, 0)?;
            if matches!(value.0, Kind::Hash(_)) {
                return Err(type_error());
            }
            return to_string(ctx, &value);
        }
        _ => (),
    }
    if let Kind::Range(range) = &value.0 {
        return crate::range::method(ctx, method, name, range, args);
    }
    match method {
        IsNil | Itself | Dup | ToString => unreachable!(),
        Prepend | Pop | Shift | Delete | Insert | Clear | Fill | Store | Replace => {
            crate::mutate::call(ctx, method, name, value, args).map(|(_, result)| result)
        }
        Empty => {
            member_arity(&value, name, args)?;
            Ok(Value::boolean(match &value.0 {
                Kind::Bytes(h) => h.data.is_empty(),
                Kind::Array(h) => h.buffer.data.is_empty(),
                Kind::Hash(h) => h.buffer.data.is_empty(),
                _ => return Err(type_error()),
            }))
        }
        Abs => {
            arity(args, 0)?;
            match value.0 {
                Kind::Int(n) if n != i64::MIN => Ok(Value::int(n.abs())),
                Kind::Int(_) | Kind::Big(_) => crate::integer::negate(ctx, &value, true),
                Kind::Float(n) => Ok(Value::float(n.abs())),
                _ => Err(type_error()),
            }
        }
        Even | Odd => {
            arity(args, 0)?;
            Ok(Value::boolean(if value.is_integer() {
                crate::integer::odd(&value) == matches!(method, Odd)
            } else {
                return Err(type_error());
            }))
        }
        Reverse if matches!(value.0, Kind::Bytes(_)) => {
            crate::text::method(ctx, method, name, value, args)
        }
        Ord | Chr | Bytes | Chars | Lines | Codepoints | StartWith | EndWith => {
            crate::text::method(ctx, method, name, value, args)
        }
        Reverse | Take | Drop | Compact | Uniq | Flatten | Chunk | Window | Zip | Transpose
        | ToHash | Fetch | ValuesAt | Dig | Key | HasValue | Member | RemapKeys | Except => {
            crate::collections::method(ctx, method, name, value, args)
        }
        Slice if matches!(value.0, Kind::Hash(_)) => {
            crate::collections::method(ctx, method, name, value, args)
        }
        At | Slice | ByteSlice | GetByte | First | Last | ToArray => {
            crate::sequence::method(ctx, method, value, args)
        }
        Cover | ExcludeEnd => Err(type_error()),
        Length | Size => {
            member_arity(&value, name, args)?;
            let n = match &value.0 {
                Kind::Bytes(h) => runes(ctx, &h.data)?.0,
                Kind::Array(h) => h.buffer.data.len(),
                Kind::Hash(h) => h.buffer.data.len(),
                _ => return Err(type_error()),
            };
            Ok(Value::int(n as i64))
        }
        ByteSize => {
            member_arity(&value, name, args)?;
            Ok(Value::int(value.require_bytes()?.len() as i64))
        }
        Include | Index | Rindex => {
            if matches!(method, Include) && matches!(value.0, Kind::Hash(_)) {
                return crate::collections::method(ctx, Key, name, value, args);
            }
            let found = if let Some(array) = value.as_array() {
                if matches!(method, Include) {
                    if args.len() != 1 {
                        return Err(Error::new(
                            ErrorKind::Argument,
                            "array.include? expects exactly one value",
                        ));
                    }
                    array_index(ctx, array, &args[0], None, false)?
                } else {
                    if args.is_empty() || args.len() > 2 {
                        return Err(Error::new(
                            ErrorKind::Argument,
                            format!(
                                "array.{name} expects a value (with optional offset) or a block"
                            ),
                        ));
                    }
                    let offset = match args.get(1) {
                        Some(offset) => Some(index_offset(name, offset)?),
                        None => None,
                    };
                    array_index(ctx, array, &args[0], offset, matches!(method, Rindex))?
                }
            } else {
                let include = matches!((method, &value.0), (Include, Kind::Bytes(_)));
                if include && args.len() != 1 {
                    return Err(Error::new(
                        ErrorKind::Argument,
                        "string.include? expects exactly one substring",
                    ));
                }
                arity(args, 1)?;
                let bytes = value.require_bytes()?;
                let needle = args[0].require_bytes().map_err(|mut error| {
                    if include {
                        error.message = "string.include? substring must be string".into();
                    }
                    error
                })?;
                let found = find(ctx, bytes, needle, matches!(method, Rindex))?;
                if let Some(pos) = found {
                    Some(runes(ctx, &bytes[..pos])?.0)
                } else {
                    None
                }
            };
            if matches!(method, Include) {
                Ok(Value::boolean(found.is_some()))
            } else {
                Ok(found.map(|n| Value::int(n as i64)).unwrap_or_default())
            }
        }
        Split => crate::text::split::call(ctx, &value, args),
        Join => join(ctx, &value, args),
        Push => value.push(ctx, args),
        Sum => {
            if args.len() > 1 {
                return Err(Error::new(
                    ErrorKind::Argument,
                    "array.sum accepts at most an initial value",
                ));
            }
            let mut rest = value.as_array().ok_or_else(type_error)?;
            let mut sum = args.first().cloned().unwrap_or_else(|| Value::int(0));
            while let Some(item) = rest.first() {
                // Integers add in place until one would overflow, each still
                // charged its step; `binary` handles every other addition.
                if let Kind::Int(mut total) = sum.0 {
                    let run = rest
                        .iter()
                        .take(CHUNK)
                        .take_while(|item| match item.0 {
                            Kind::Int(n) => total.checked_add(n).map(|n| total = n).is_some(),
                            _ => false,
                        })
                        .count();
                    if run > 0 {
                        ctx.charge_each(run as u64)?;
                        sum = Value::int(total);
                        rest = &rest[run..];
                        continue;
                    }
                }
                ctx.charge(1)?;
                if matches!(sum.0, Kind::Bytes(_)) != matches!(item.0, Kind::Bytes(_)) {
                    return Err(Error::new(ErrorKind::Type, SUM_INCOMPATIBLE));
                }
                sum = binary(ctx, "+", sum, item.clone()).map_err(sum_incompatible)?;
                rest = &rest[1..];
            }
            Ok(sum)
        }
        Keys | Values => {
            member_arity(&value, name, args)?;
            let entries = value.as_hash().ok_or_else(type_error)?;
            let mut out = Buffer::with_capacity(ctx, entries.len())?;
            for (k, v) in entries {
                ctx.charge(1)?;
                out.data.push(if matches!(method, Keys) {
                    k.clone()
                } else {
                    v.clone()
                });
            }
            Value::from_array(ctx, out)
        }
        ToFloat => {
            if matches!(value.0, Kind::Int(_) | Kind::Big(_) | Kind::Float(_)) {
                let kind = value.type_name();
                crate::members::nullary(format_args!("{kind}.{name}"), args.len(), false, false)?;
            }
            arity(args, 0)?;
            value.as_float().map(Value::float).ok_or_else(type_error)
        }
        ToInt => {
            if matches!(value.0, Kind::Int(_) | Kind::Big(_) | Kind::Float(_)) {
                let kind = value.type_name();
                crate::members::nullary(format_args!("{kind}.{name}"), args.len(), false, false)?;
            }
            arity(args, 0)?;
            match &value.0 {
                Kind::Int(_) | Kind::Big(_) => Ok(value),
                Kind::Float(n) => crate::integer::from_float(ctx, *n, "float.to_i"),
                _ => crate::conversion::integer(ctx, value.require_bytes()?, "string.to_i"),
            }
        }
    }
}

pub(crate) fn runes(ctx: &mut CallContext, bytes: &[u8]) -> Result<(usize, bool)> {
    let mut i = 0;
    let mut count = 0;
    let mut valid = true;
    while i < bytes.len() {
        let end = bytes.len().min(i + CHUNK);
        let chunk = &bytes[i..end];
        let ascii = scan::prefix(chunk, Class::Ascii);
        if ascii > 0 {
            ctx.work_bytes(ascii)?;
            i += ascii;
            count += ascii;
            continue;
        }
        let span = scan::unicode_span(chunk);
        if span.len > 0 {
            ctx.charge(span.steps)?;
            ctx.checkpoint()?;
            i += span.len;
            count += span.runes;
        } else {
            ctx.charge(1)?;
            let (_, n, ok) = scan::rune(&bytes[i..]);
            i += n;
            count += 1;
            valid &= ok;
        }
    }
    Ok((count, valid))
}

pub(crate) fn trim(ctx: &mut CallContext, bytes: &[u8]) -> Result<(usize, usize)> {
    let mut start = 0;
    while start < bytes.len() {
        ctx.charge(1)?;
        let (ch, n, _) = scan::rune(&bytes[start..]);
        if !ch.is_whitespace() {
            break;
        }
        start += n;
    }
    let mut end = bytes.len();
    while end > start {
        ctx.charge(1)?;
        let mut pos = end - 1;
        for _ in 0..3 {
            if pos == start || bytes[pos] & 0xc0 != 0x80 {
                break;
            }
            pos -= 1;
        }
        let (ch, n, valid) = scan::rune(&bytes[pos..end]);
        if !valid || pos + n != end || !ch.is_whitespace() {
            break;
        }
        end = pos;
    }
    Ok((start, end))
}

/// Finds the first, or with `last` the final, occurrence of `needle` with a
/// Knuth-Morris-Pratt scan. Each byte read and each table transition is one
/// unit of byte work, charged in chunks.
pub(crate) fn find(
    ctx: &mut CallContext,
    bytes: &[u8],
    needle: &[u8],
    last: bool,
) -> Result<Option<usize>> {
    if needle.is_empty() {
        return Ok(Some(if last { bytes.len() } else { 0 }));
    }
    if needle.len() > bytes.len() {
        return Ok(None);
    }
    let mut pending = 0;
    let mut table = Buffer::with_capacity(ctx, needle.len())?;
    table.data.push(0usize);
    let mut matched = 0;
    for i in 1..needle.len() {
        ctx.scan_bytes(&mut pending, 1)?;
        while matched > 0 && needle[i] != needle[matched] {
            ctx.scan_bytes(&mut pending, 1)?;
            matched = table.data[matched - 1];
        }
        if needle[i] == needle[matched] {
            matched += 1;
        }
        table.data.push(matched);
    }
    let mut matched = 0;
    let mut found = None;
    for (i, &b) in bytes.iter().enumerate() {
        ctx.scan_bytes(&mut pending, 1)?;
        while matched > 0 && b != needle[matched] {
            ctx.scan_bytes(&mut pending, 1)?;
            matched = table.data[matched - 1];
        }
        if b == needle[matched] {
            matched += 1;
        }
        if matched == needle.len() {
            found = Some(i + 1 - needle.len());
            if !last {
                break;
            }
            matched = table.data[matched - 1];
        }
    }
    ctx.settle_bytes(&mut pending)?;
    Ok(found)
}

fn join(ctx: &mut CallContext, value: &Value, args: &[Value]) -> Result<Value> {
    if args.len() > 1 {
        return Err(Error::new(
            ErrorKind::Argument,
            "array.join accepts at most one separator",
        ));
    }
    // Unlike the reference, a symbol separator is accepted.
    let sep = match args.first() {
        None => b"".as_slice(),
        Some(sep) => sep
            .as_bytes()
            .ok_or_else(|| Error::new(ErrorKind::Type, "array.join separator must be string"))?,
    };
    let array = value.as_array().ok_or_else(type_error)?;
    let mut out = Buffer::empty();
    join_into(ctx, array, sep, &mut out)?;
    Value::from_bytes(ctx, out)
}

/// A suspended array level of a join or flatten walk; borrows the input only.
struct Level<'a> {
    values: &'a [Value],
    index: usize,
}

fn join_into<'a>(
    ctx: &mut CallContext,
    array: &'a [Value],
    sep: &[u8],
    out: &mut Buffer<u8>,
) -> Result<()> {
    let mut current = Level {
        values: array,
        index: 0,
    };
    let mut parents: Buffer<Level<'a>> = Buffer::empty();
    loop {
        let i = current.index;
        if i == current.values.len() {
            match parents.data.pop() {
                Some(parent) => current = parent,
                None => return Ok(()),
            }
            continue;
        }
        current.index += 1;
        ctx.charge(1)?;
        if i > 0 {
            out.extend(ctx, sep)?;
        }
        let values: &'a [Value] = current.values;
        let v = &values[i];
        if let Some(nested) = v.as_array() {
            // A nested level sits one below its parent; the root is level 0.
            if parents.data.len() + 1 > MAX_VALUE_DEPTH {
                return ctx.guard(ErrorKind::Recursion, "join nesting too deep");
            }
            let suspended = std::mem::replace(
                &mut current,
                Level {
                    values: nested,
                    index: 0,
                },
            );
            parents.push(ctx, suspended)?;
        } else {
            let v = to_string(ctx, v)?;
            out.extend(ctx, v.require_bytes()?)?;
        }
    }
}

pub(crate) fn to_string(ctx: &mut CallContext, value: &Value) -> Result<Value> {
    let mut text = json::Number::new();
    match &value.0 {
        Kind::Regex(regex) => return regex.text(ctx),
        Kind::Offset(offset) => return Err(offset.value_error()),
        Kind::Host(method) => return Err(method.value_error()),
        Kind::Function(function) => return Err(function.value_error()),
        Kind::Builtin(builtin) => return Err(builtin.value_error()),
        Kind::Enum(_) | Kind::EnumMember(_) => return crate::enums::text(ctx, value),
        Kind::Money(money) => return money.text(ctx),
        Kind::Duration(seconds) => return crate::duration::text(ctx, *seconds),
        Kind::Time(_) | Kind::Zoned(_) => return crate::time::text(ctx, value, None),
        Kind::Big(_) => {
            let text = crate::integer::format(ctx, value, 10)?;
            return Value::from_bytes(ctx, text);
        }
        Kind::Bytes(_) => return Ok(value.clone()),
        Kind::Symbol(h) => return ctx.bytes(&h.data),
        Kind::Nil => return ctx.bytes(b""),
        Kind::Bool(v) => return ctx.bytes(if *v { b"true" } else { b"false" }),
        Kind::Int(n) => write!(text, "{n}").unwrap(),
        Kind::Float(n) => format_float(&mut text, *n),
        _ => return crate::text::display(ctx, value),
    }
    ctx.bytes(text.bytes())
}

pub(crate) fn format_float(out: &mut json::Number, value: f64) {
    if value.is_nan() {
        out.write_str("NaN").unwrap();
    } else if value.is_infinite() {
        out.write_str(if value.is_sign_negative() {
            "-Infinity"
        } else {
            "Infinity"
        })
        .unwrap();
    } else if value != 0.0 && !(1e-4..1e6).contains(&value.abs()) {
        let mut scientific = json::Number::new();
        write!(scientific, "{value:e}").unwrap();
        let text = std::str::from_utf8(scientific.bytes()).unwrap();
        let (mantissa, exponent) = text.split_once('e').unwrap();
        let exponent: i32 = exponent.parse().unwrap();
        write!(out, "{mantissa}e{exponent:+03}").unwrap();
    } else {
        write!(out, "{value}").unwrap();
    }
}

/// Helpers for deep-value tests in the collection walkers. Host values are
/// built untracked, so heights beyond the construction cap are reachable.
#[cfg(test)]
pub(crate) mod testing {
    use crate::{CallContext, CallOptions, Limits, Value};

    /// Wraps `leaf` in `height` single-element arrays.
    pub(crate) fn nested(height: usize, leaf: Value) -> Value {
        let mut value = leaf;
        for _ in 0..height {
            value = Value::array(vec![value]);
        }
        value
    }

    /// Wraps `leaf` in `height` single-entry hashes keyed `k`.
    pub(crate) fn nested_hash(height: usize, leaf: Value) -> Value {
        let mut value = leaf;
        for _ in 0..height {
            value = Value::hash(vec![(b"k".to_vec(), value)]);
        }
        value
    }

    /// Builds `levels` layers of `[child, child]` over `leaf`, sharing each child.
    pub(crate) fn shared(levels: usize, leaf: Value) -> Value {
        let mut value = leaf;
        for _ in 0..levels {
            value = Value::array(vec![value.clone(), value]);
        }
        value
    }

    /// Runs `work` on a thread whose native stack is far too small for a
    /// depth-proportional recursion over the tested values.
    pub(crate) fn on_small_stack<T: Send + 'static>(
        work: impl FnOnce() -> T + Send + 'static,
    ) -> T {
        // WASI has no threads, so this runs within the default wasm stack.
        if cfg!(target_os = "wasi") {
            return work();
        }
        std::thread::Builder::new()
            .stack_size(256 << 10)
            .spawn(work)
            .unwrap()
            .join()
            .unwrap()
    }

    pub(crate) fn context(steps: Option<u64>, memory_bytes: Option<usize>) -> CallContext {
        CallContext::new(CallOptions {
            limits: Limits {
                steps,
                memory_bytes,
                ..Limits::default()
            },
            ..CallOptions::default()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::testing::{context, nested, nested_hash, on_small_stack, shared};
    use super::*;
    use crate::{CallOptions, ErrorClass};

    fn ints(values: &[i64]) -> Value {
        Value::array(values.iter().copied().map(Value::int).collect())
    }

    #[test]
    fn integer_runs_sum_like_elementwise_addition_under_every_quota() {
        // The element-at-a-time sum the integer runs must reproduce.
        fn reference(ctx: &mut CallContext, array: &[Value], initial: Value) -> Result<Value> {
            let mut sum = initial;
            for item in array {
                ctx.charge(1)?;
                if matches!(sum.0, Kind::Bytes(_)) != matches!(item.0, Kind::Bytes(_)) {
                    return Err(Error::new(ErrorKind::Type, SUM_INCOMPATIBLE));
                }
                sum = binary(ctx, "+", sum, item.clone()).map_err(sum_incompatible)?;
            }
            Ok(sum)
        }
        let near = i64::MAX - 5000;
        let cases = [
            (0..6000).map(Value::int).collect::<Vec<_>>(),
            (0..3000)
                .map(|i| Value::int(if i % 1000 == 999 { near } else { i }))
                .collect(),
            (0..3000)
                .map(|i| match i % 7 {
                    0 => Value::float(0.5),
                    1 => Value::int(-near),
                    _ => Value::int(i),
                })
                .collect(),
            (0..100)
                .map(|i| if i == 60 { Value::nil() } else { Value::int(i) })
                .collect(),
            vec![Value::int(1), Value::bytes("a")],
        ];
        for items in cases {
            for initial in [None, Some(Value::int(near)), Some(Value::float(1.5))] {
                let args: Vec<Value> = initial.iter().cloned().collect();
                let start = initial.clone().unwrap_or_else(|| Value::int(0));
                let mut full = context(None, None);
                let expected = reference(&mut full, &items, start.clone());
                let needed = full.stats().steps;
                for limit in (0..=needed + 1)
                    .step_by(37)
                    .chain([needed.saturating_sub(1), needed])
                {
                    let mut want_ctx = context(Some(limit), None);
                    let want = reference(&mut want_ctx, &items, start.clone());
                    let mut got_ctx = context(Some(limit), None);
                    let got = method(
                        &mut got_ctx,
                        Method::Sum,
                        "sum",
                        Value::array(items.clone()),
                        &args,
                    );
                    match (&want, &got) {
                        (Ok(a), Ok(b)) => assert_eq!(a.to_string(), b.to_string()),
                        (Err(a), Err(b)) => assert_eq!((a.kind, &a.message), (b.kind, &b.message)),
                        _ => panic!("{limit}: {want:?} vs {got:?}"),
                    }
                    assert_eq!(want_ctx.stats().steps, got_ctx.stats().steps, "{limit}");
                }
                if let Ok(expected) = expected {
                    let mut ctx = context(None, None);
                    let actual = method(
                        &mut ctx,
                        Method::Sum,
                        "sum",
                        Value::array(items.clone()),
                        &args,
                    );
                    assert_eq!(actual.unwrap().to_string(), expected.to_string());
                }
            }
        }
    }

    fn pairs(entries: &[(&str, Value)]) -> Value {
        Value::hash(
            entries
                .iter()
                .map(|(key, value)| (key.as_bytes().to_vec(), value.clone()))
                .collect(),
        )
    }

    #[test]
    fn nested_equality_visits_each_pair_once_and_stops_at_the_first_difference() {
        let value = Value::array(vec![ints(&[1, 2]), ints(&[3])]);
        for (other, expected, steps) in [
            (Value::array(vec![ints(&[1, 2]), ints(&[3])]), true, 6),
            (Value::array(vec![ints(&[1, 9]), ints(&[3])]), false, 4),
            (Value::array(vec![ints(&[1, 2]), ints(&[3, 4])]), false, 5),
            (Value::array(vec![ints(&[1, 2])]), false, 1),
            (Value::array(vec![ints(&[1, 2]), Value::int(3)]), false, 5),
        ] {
            let mut ctx = CallContext::new(CallOptions::default());
            assert_eq!(equal(&mut ctx, &value, &other, 0).unwrap(), expected);
            assert_eq!(ctx.stats().steps, steps, "{other}");
            assert_eq!(ctx.stats().retained_memory_bytes, 0);
        }
    }

    #[test]
    fn equality_policies_are_preserved_through_containers() {
        let nan = Value::float(f64::NAN);
        let mut ctx = CallContext::new(CallOptions::default());
        let loose = Value::array(vec![ints(&[1])]);
        let float = Value::array(vec![Value::array(vec![Value::float(1.0)])]);
        assert!(equal(&mut ctx, &loose, &float, 0).unwrap());
        assert!(!eql(&mut ctx, &loose, &float, 0).unwrap());
        let symbol = Value::array(vec![Value::symbol("x")]);
        let text = Value::array(vec![Value::bytes("x")]);
        assert!(!equal(&mut ctx, &symbol, &text, 0).unwrap());
        let hash = pairs(&[("a", ints(&[1])), ("b", Value::int(2))]);
        let reordered = pairs(&[("b", Value::int(2)), ("a", ints(&[1]))]);
        assert!(equal(&mut ctx, &hash, &reordered, 0).unwrap());
        assert!(eql(&mut ctx, &hash, &reordered, 0).unwrap());
        let object = Value::object(vec![
            (b"a".to_vec(), ints(&[1])),
            (b"b".to_vec(), Value::int(2)),
        ]);
        assert!(!equal(&mut ctx, &hash, &object, 0).unwrap());
        let renamed = pairs(&[("a", ints(&[1])), ("c", Value::int(2))]);
        assert!(!equal(&mut ctx, &hash, &renamed, 0).unwrap());
        let coerced = pairs(&[
            ("a", Value::array(vec![Value::float(1.0)])),
            ("b", Value::int(2)),
        ]);
        assert!(equal(&mut ctx, &hash, &coerced, 0).unwrap());
        assert!(!eql(&mut ctx, &hash, &coerced, 0).unwrap());
        // Nested NaN stays unequal, even against the same storage.
        let wrapped = Value::array(vec![Value::array(vec![nan.clone()])]);
        assert!(!equal(&mut ctx, &wrapped, &wrapped, 0).unwrap());
        assert!(!eql(&mut ctx, &wrapped, &wrapped.clone(), 0).unwrap());
        let keyed = pairs(&[("n", nan)]);
        assert!(!equal(&mut ctx, &keyed, &keyed, 0).unwrap());
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }

    #[test]
    fn nested_equality_has_exact_step_and_frame_memory_boundaries() {
        let value = Value::array(vec![ints(&[1, 2]), Value::array(vec![ints(&[3])])]);
        let other = Value::array(vec![ints(&[1, 2]), Value::array(vec![ints(&[3])])]);
        let mut ctx = CallContext::new(CallOptions::default());
        assert!(equal(&mut ctx, &value, &other, 0).unwrap());
        let steps = ctx.stats().steps;
        let mut ctx = context(Some(steps), None);
        assert!(equal(&mut ctx, &value, &other, 0).unwrap());
        let mut ctx = context(Some(steps - 1), None);
        let error = equal(&mut ctx, &value, &other, 0).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Steps);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
        assert_eq!(ctx.charge(0).unwrap_err(), error);

        // The first suspended frame reserves one initial buffer growth.
        let frames = 8 * size_of::<EqualFrame>();
        let mut ctx = context(None, Some(frames));
        assert!(equal(&mut ctx, &value, &other, 0).unwrap());
        assert_eq!(ctx.stats().peak_memory_bytes, frames);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
        let mut ctx = context(None, Some(frames - 1));
        let error = equal(&mut ctx, &value, &other, 0).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Memory);
        assert_eq!(ctx.stats().peak_memory_bytes, 0);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
        assert_eq!(ctx.charge(0).unwrap_err(), error);
        // Flat operands never suspend a frame.
        let mut ctx = context(None, Some(0));
        assert!(equal(&mut ctx, &ints(&[1, 2, 3]), &ints(&[1, 2, 3]), 0).unwrap());
        assert_eq!(ctx.stats().peak_memory_bytes, 0);
    }

    #[test]
    fn nested_equality_observes_cancellation_and_deadlines_mid_walk() {
        let value = Value::array(vec![ints(&[1, 2, 3, 4]), ints(&[5, 6, 7, 8])]);
        let other = Value::array(vec![ints(&[1, 2, 3, 4]), ints(&[5, 6, 7, 8])]);
        for kind in [ErrorKind::Cancelled, ErrorKind::Deadline] {
            let mut ctx = CallContext::new(CallOptions::default());
            ctx.charge(14).unwrap();
            if kind == ErrorKind::Cancelled {
                ctx.cancellation().cancel();
            } else {
                ctx.options.deadline = Some(std::time::Instant::now());
            }
            let error = equal(&mut ctx, &value, &other, 0).unwrap_err();
            assert_eq!(error.kind, kind);
            assert_eq!(ctx.stats().steps, 16);
            assert_eq!(ctx.stats().retained_memory_bytes, 0);
            assert_eq!(ctx.checkpoint().unwrap_err(), error);
        }
    }

    #[test]
    fn shared_graph_equality_compares_each_pair_once() {
        // 2^40 paths, but 40 distinct pairs of shared arrays.
        let a = shared(40, Value::int(0));
        let b = shared(40, Value::int(0));
        let c = shared(40, Value::int(1));
        for strict in [false, true] {
            let mut ctx = context(Some(1_000), None);
            assert!(equal_kinds(&mut ctx, &a, &b, 0, strict).unwrap());
            assert!(!equal_kinds(&mut ctx, &a, &c, 0, strict).unwrap());
            assert_eq!(ctx.stats().retained_memory_bytes, 0);
        }
        // Each level costs its two element steps and one recorded pair.
        let cost = |levels| {
            let mut ctx = context(None, Some(usize::MAX));
            let (a, b) = (shared(levels, Value::int(0)), shared(levels, Value::int(0)));
            assert!(equal(&mut ctx, &a, &b, 0).unwrap());
            assert_eq!(ctx.stats().retained_memory_bytes, 0);
            ctx.stats()
        };
        let (small, large) = (cost(40), cost(80));
        assert!(large.steps < 2 * small.steps + 100, "{small:?} {large:?}");
        // The memo's 128 slots are reserved, and the quota bounds them.
        let peak = small.peak_memory_bytes;
        assert!(peak >= 128 * 2 * size_of::<usize>());
        let mut ctx = context(None, Some(peak - 1));
        let error = equal(&mut ctx, &a, &b, 0).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Memory);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
        assert_eq!(ctx.charge(0).unwrap_err(), error);
        // A shared NaN-bearing pair still compares unequal on every path.
        let nan = Value::array(vec![Value::float(f64::NAN)]);
        let twice = Value::array(vec![nan.clone(), nan.clone()]);
        let mut ctx = context(None, None);
        assert!(!equal(&mut ctx, &twice, &twice, 0).unwrap());
        drop(a);
        drop(b);
        drop(c);
    }

    #[test]
    fn remembered_pairs_do_not_hide_the_nesting_limit() {
        let outcome = on_small_stack(|| {
            let mut ctx = CallContext::new(CallOptions::default());
            // `inner` is shared and first compared at depth 1; the second path
            // reaches it deeper, where its walk exceeds the limit.
            let inner = nested(MAX_VALUE_DEPTH - 2, Value::int(1));
            let other = nested(MAX_VALUE_DEPTH - 2, Value::int(1));
            let deep = |value: &Value| Value::array(vec![value.clone(), nested(3, value.clone())]);
            let result = equal(&mut ctx, &deep(&inner), &deep(&other), 0);
            let stats = ctx.stats();
            drop(inner);
            drop(other);
            (result, stats)
        });
        let (result, stats) = outcome;
        let error = result.unwrap_err();
        assert_eq!(error.kind, ErrorKind::Recursion);
        assert_eq!(stats.retained_memory_bytes, 0);
    }

    #[test]
    fn deep_equality_walks_to_the_value_limit_on_a_small_stack() {
        let outcome = on_small_stack(|| {
            let mut ctx = CallContext::new(CallOptions::default());
            let arrays = (
                nested(MAX_VALUE_DEPTH, Value::int(1)),
                nested(MAX_VALUE_DEPTH, Value::int(1)),
                nested(MAX_VALUE_DEPTH, Value::float(1.0)),
                nested(MAX_VALUE_DEPTH, Value::int(2)),
            );
            let hashes = (
                nested_hash(MAX_VALUE_DEPTH, Value::int(1)),
                nested_hash(MAX_VALUE_DEPTH, Value::int(1)),
            );
            let within = (
                equal(&mut ctx, &arrays.0, &arrays.1, 0),
                eql(&mut ctx, &arrays.0, &arrays.1, 0),
                equal(&mut ctx, &arrays.0, &arrays.2, 0),
                eql(&mut ctx, &arrays.0, &arrays.2, 0),
                equal(&mut ctx, &arrays.0, &arrays.3, 0),
                equal(&mut ctx, &hashes.0, &hashes.1, 0),
                eql(&mut ctx, &hashes.0, &hashes.1, 0),
            );
            let steps = ctx.stats().steps;
            let beyond = (
                nested(MAX_VALUE_DEPTH + 1, Value::int(1)),
                nested(MAX_VALUE_DEPTH + 1, Value::int(1)),
                nested_hash(MAX_VALUE_DEPTH + 1, Value::int(1)),
                nested_hash(MAX_VALUE_DEPTH + 1, Value::int(1)),
            );
            let errors = (
                equal(&mut ctx, &beyond.0, &beyond.1, 0),
                eql(&mut ctx, &beyond.2, &beyond.3, 0),
            );
            let stats = ctx.stats();
            for value in [
                arrays.0, arrays.1, arrays.2, arrays.3, hashes.0, hashes.1, beyond.0, beyond.1,
                beyond.2, beyond.3,
            ] {
                drop(value);
            }
            (within, steps, errors, stats)
        });
        let (within, steps, errors, stats) = outcome;
        assert!(within.0.unwrap());
        assert!(within.1.unwrap());
        assert!(within.2.unwrap());
        assert!(!within.3.unwrap());
        assert!(!within.4.unwrap());
        assert!(within.5.unwrap());
        assert!(within.6.unwrap());
        // Every array walk charges one step per level plus the leaf pair.
        assert!(steps >= 5 * (MAX_VALUE_DEPTH as u64 + 1));
        for error in [errors.0.unwrap_err(), errors.1.unwrap_err()] {
            assert_eq!(error.kind, ErrorKind::Recursion);
            assert_eq!(error.class(), Some(ErrorClass::Limit));
            assert_eq!(error.message, "value nesting too deep");
        }
        assert_eq!(stats.retained_memory_bytes, 0);
    }

    fn joined(ctx: &mut CallContext, value: &Value, sep: &[u8]) -> Result<String> {
        let mut out = Buffer::empty();
        join_into(ctx, value.as_array().unwrap(), sep, &mut out)?;
        Ok(String::from_utf8(out.data).unwrap())
    }

    #[test]
    fn join_flattens_nested_arrays_with_separators_between_leaves() {
        let mut ctx = CallContext::new(CallOptions::default());
        let nested_values = Value::array(vec![
            Value::array(vec![Value::int(1), ints(&[2])]),
            Value::int(3),
        ]);
        assert_eq!(joined(&mut ctx, &nested_values, b"-").unwrap(), "1-2-3");
        let leading = Value::array(vec![Value::array(vec![]), Value::int(1)]);
        assert_eq!(joined(&mut ctx, &leading, b",").unwrap(), ",1");
        let trailing = Value::array(vec![ints(&[1]), Value::array(vec![])]);
        assert_eq!(joined(&mut ctx, &trailing, b",").unwrap(), "1,");
        let mixed = Value::array(vec![
            Value::bytes("a"),
            Value::array(vec![Value::nil(), Value::boolean(true)]),
            Value::float(1.5),
        ]);
        assert_eq!(joined(&mut ctx, &mixed, b"/").unwrap(), "a//true/1.5");
        assert_eq!(joined(&mut ctx, &Value::array(vec![]), b"/").unwrap(), "");
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }

    #[test]
    fn join_has_exact_step_and_frame_memory_boundaries() {
        let value = Value::array(vec![
            Value::array(vec![Value::int(1), ints(&[2])]),
            Value::int(3),
        ]);
        let mut ctx = CallContext::new(CallOptions::default());
        assert_eq!(joined(&mut ctx, &value, b"-").unwrap(), "1-2-3");
        let steps = ctx.stats().steps;
        let mut ctx = context(Some(steps), None);
        assert_eq!(joined(&mut ctx, &value, b"-").unwrap(), "1-2-3");
        let mut ctx = context(Some(steps - 1), None);
        let error = joined(&mut ctx, &value, b"-").unwrap_err();
        assert_eq!(error.kind, ErrorKind::Steps);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
        assert_eq!(ctx.charge(0).unwrap_err(), error);

        let empty_levels = Value::array(vec![Value::array(vec![Value::array(vec![])])]);
        let frames = 8 * size_of::<Level>();
        let mut ctx = context(None, Some(frames));
        assert_eq!(joined(&mut ctx, &empty_levels, b"").unwrap(), "");
        assert_eq!(ctx.stats().peak_memory_bytes, frames);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
        let mut ctx = context(None, Some(frames - 1));
        let error = joined(&mut ctx, &empty_levels, b"").unwrap_err();
        assert_eq!(error.kind, ErrorKind::Memory);
        assert_eq!(ctx.stats().peak_memory_bytes, 0);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
        assert_eq!(ctx.charge(0).unwrap_err(), error);
    }

    #[test]
    fn deep_join_walks_one_level_past_the_value_limit_on_a_small_stack() {
        let outcome = on_small_stack(|| {
            let mut ctx = CallContext::new(CallOptions::default());
            let within = nested(MAX_VALUE_DEPTH + 1, Value::int(1));
            let beyond = nested(MAX_VALUE_DEPTH + 2, Value::int(1));
            let joined_within = joined(&mut ctx, &within, b",");
            let steps = ctx.stats().steps;
            let joined_beyond = joined(&mut ctx, &beyond, b",");
            let stats = ctx.stats();
            drop(within);
            drop(beyond);
            (joined_within, steps, joined_beyond, stats)
        });
        let (within, steps, beyond, stats) = outcome;
        assert_eq!(within.unwrap(), "1");
        // One step per array level, plus the leaf's string conversion.
        assert!(steps > MAX_VALUE_DEPTH as u64);
        let error = beyond.unwrap_err();
        assert_eq!(error.kind, ErrorKind::Recursion);
        assert_eq!(error.class(), Some(ErrorClass::Limit));
        assert_eq!(error.message, "join nesting too deep");
        assert_eq!(stats.retained_memory_bytes, 0);
    }
}
