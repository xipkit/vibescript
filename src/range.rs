use crate::{
    CallContext, Error, ErrorKind, Result, Value,
    budget::{Buffer, Charge},
    bytecode::Method,
    value::Kind,
};
use std::{fmt, mem::size_of, sync::Arc};

#[derive(Debug)]
pub(crate) struct Range {
    pub start: Option<i64>,
    pub end: Option<i64>,
    pub exclusive: bool,
    charge: Option<Charge>,
}

impl Range {
    pub fn untracked(start: Option<i64>, end: Option<i64>, exclusive: bool) -> Arc<Self> {
        Arc::new(Self {
            start,
            end,
            exclusive,
            charge: None,
        })
    }

    pub fn new(
        ctx: &mut CallContext,
        start: Option<i64>,
        end: Option<i64>,
        exclusive: bool,
    ) -> Result<Arc<Self>> {
        let charge = ctx.reserve(size_of::<Self>() + 2 * size_of::<usize>())?;
        Ok(Arc::new(Self {
            start,
            end,
            exclusive,
            charge,
        }))
    }

    pub fn import(ctx: &mut CallContext, range: &Arc<Self>) -> Result<Arc<Self>> {
        if ctx.owns(&range.charge) || ctx.options.limits.memory_bytes.is_none() {
            Ok(range.clone())
        } else {
            Self::new(ctx, range.start, range.end, range.exclusive)
        }
    }

    pub fn length(&self) -> Result<i128> {
        let start = self.start.ok_or_else(|| open_error("beginless"))?;
        let end = self.end.ok_or_else(|| open_error("endless"))?;
        Ok((i128::from(end) - i128::from(start)).abs() + i128::from(!self.exclusive))
    }

    pub fn contains(&self, value: &Value) -> bool {
        let descending = matches!((self.start, self.end), (Some(a), Some(b)) if a > b);
        match value.0 {
            Kind::Big(ref n) => {
                if n.negative {
                    self.start.is_none()
                } else {
                    self.end.is_none()
                }
            }
            Kind::Int(n) => {
                if descending {
                    self.start.is_none_or(|a| n <= a)
                        && self
                            .end
                            .is_none_or(|b| if self.exclusive { n > b } else { n >= b })
                } else {
                    self.start.is_none_or(|a| n >= a)
                        && self
                            .end
                            .is_none_or(|b| if self.exclusive { n < b } else { n <= b })
                }
            }
            Kind::Float(n) => contains_float(self.start, self.end, self.exclusive, n),
            _ => false,
        }
    }

    /// Builds up to `count` leading or trailing elements; `count` is not
    /// negative.
    fn materialize(&self, ctx: &mut CallContext, count: i64, last: bool) -> Result<Value> {
        let start = self.start.ok_or_else(|| open_error("beginless"))?;
        let (count, skip, direction) = if let Some(end) = self.end {
            let length = self.length()?;
            let count = i128::from(count).min(length);
            (
                count,
                if last { length - count } else { 0 },
                if start > end { -1 } else { 1 },
            )
        } else {
            if last {
                return Err(open_error("endless"));
            }
            let available = i128::from(i64::MAX) - i128::from(start) + 1;
            (i128::from(count).min(available), 0, 1)
        };
        let mut current = i128::from(start) + skip * direction;
        let mut out = Buffer::empty();
        for _ in 0..count {
            ctx.charge(1)?;
            let n = i64::try_from(current).map_err(|_| {
                Error::new(
                    ErrorKind::Arithmetic,
                    "range element exceeds 64-bit integer",
                )
            })?;
            out.push(ctx, Value::int(n))?;
            current += direction;
        }
        Value::from_array(ctx, out)
    }
}

const INTEGER_FLOOR: f64 = -9223372036854775808.0;
const INTEGER_CEILING: f64 = 9223372036854775808.0;

/// Truncates a float toward zero when the result fits a 64-bit integer.
pub(crate) fn truncate(value: f64) -> Option<i64> {
    (INTEGER_FLOOR..INTEGER_CEILING)
        .contains(&value)
        .then_some(value as i64)
}

/// Converts a range literal endpoint. Finite floats truncate toward zero, as at
/// other integer sites; big integers, non-finite floats and other values fail.
pub(crate) fn endpoint(value: &Value) -> Result<i64> {
    let message = match value.0 {
        Kind::Int(n) => return Ok(n),
        Kind::Float(n) => {
            if let Some(n) = truncate(n) {
                return Ok(n);
            }
            let mut text = crate::json::Number::new();
            crate::ops::format_float(&mut text, n);
            let text = std::str::from_utf8(text.bytes()).unwrap();
            if n.is_finite() {
                format!("float {text} is out of integer range")
            } else {
                format!("cannot convert {text} to integer")
            }
        }
        Kind::Big(_) => "range endpoints must fit in a 64-bit integer".to_owned(),
        _ => "expected integer".to_owned(),
    };
    Err(Error::new(ErrorKind::Type, message))
}

/// Reports whether a float lies within integer bounds. The comparison uses the
/// float's floor and ceiling as integers, so it stays exact beyond 2^53.
pub(crate) fn contains_float(
    start: Option<i64>,
    end: Option<i64>,
    exclusive: bool,
    value: f64,
) -> bool {
    if value.is_nan() {
        return false;
    }
    if value < INTEGER_FLOOR {
        return start.is_none();
    }
    if value >= INTEGER_CEILING {
        return end.is_none();
    }
    let (floor, ceil) = (value.floor() as i64, value.ceil() as i64);
    match (start, end) {
        (Some(a), Some(b)) if a > b => ceil <= a && if exclusive { ceil > b } else { floor >= b },
        _ => {
            start.is_none_or(|a| floor >= a)
                && end.is_none_or(|b| if exclusive { floor < b } else { ceil <= b })
        }
    }
}

fn open_error(kind: &str) -> Error {
    let article = if kind == "endless" { "an" } else { "a" };
    Error::new(
        ErrorKind::Argument,
        format!("cannot iterate {article} {kind} range"),
    )
}

impl fmt::Display for Range {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(n) = self.start {
            write!(f, "{n}")?;
        }
        f.write_str(if self.exclusive { "..." } else { ".." })?;
        if let Some(n) = self.end {
            write!(f, "{n}")?;
        }
        Ok(())
    }
}

/// Runs the range member `name`, which `method` identifies.
pub(crate) fn method(
    ctx: &mut CallContext,
    method: Method,
    name: &str,
    range: &Range,
    args: &[Value],
) -> Result<Value> {
    use Method::*;
    let argument =
        |problem: &str| Error::new(ErrorKind::Argument, format!("range.{name} {problem}"));
    match method {
        Include | Cover | Member => {
            if args.len() != 1 {
                return Err(argument("expects one argument"));
            }
            Ok(Value::boolean(range.contains(&args[0])))
        }
        Size | ToArray => {
            if !args.is_empty() {
                return Err(argument("does not take arguments"));
            }
            let n = match i64::try_from(range.length()?) {
                Ok(n) => n,
                Err(_) if matches!(method, ToArray) => {
                    return ctx.guard(ErrorKind::Arithmetic, "range.to_a result too large");
                }
                Err(_) => {
                    return Err(Error::new(ErrorKind::Arithmetic, "range.size overflow"));
                }
            };
            if matches!(method, Size) {
                Ok(Value::int(n))
            } else {
                range.materialize(ctx, n, false)
            }
        }
        First | Last => {
            let last = matches!(method, Last);
            let endpoint = if last { range.end } else { range.start }.ok_or_else(|| {
                Error::new(
                    ErrorKind::Argument,
                    if last {
                        "cannot get the last element of an endless range"
                    } else {
                        "cannot get the first element of a beginless range"
                    },
                )
            })?;
            if args.is_empty() {
                return Ok(Value::int(endpoint));
            }
            if args.len() != 1 {
                return Err(argument("expects at most one argument"));
            }
            if range.start.is_none() {
                return Err(open_error("beginless"));
            }
            let count = match args[0].0 {
                Kind::Int(count) if count < 0 => {
                    return Err(argument("count must be non-negative"));
                }
                Kind::Int(count) => count,
                Kind::Big(_) => {
                    return Err(Error::new(
                        ErrorKind::Type,
                        format!("range.{name} count must fit in a 64-bit integer"),
                    ));
                }
                _ => {
                    return Err(Error::new(
                        ErrorKind::Type,
                        format!("range.{name} expects an integer count"),
                    ));
                }
            };
            range.materialize(ctx, count, last)
        }
        ExcludeEnd => {
            if !args.is_empty() {
                return Err(argument("does not take arguments"));
            }
            Ok(Value::boolean(range.exclusive))
        }
        _ => Err(Error::new(ErrorKind::Type, "unsupported range method")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CallOptions;

    #[test]
    fn imported_ranges_charge_each_call_and_release_with_the_value() {
        let input = Value::range(Some(1), None, true);
        let mut first = CallContext::new(CallOptions::default());
        let imported = first.import(&input).unwrap();
        let retained = first.stats().retained_memory_bytes;
        assert!(retained > 0);
        let alias = first.import(&imported).unwrap();
        assert_eq!(first.stats().retained_memory_bytes, retained);
        let mut second = CallContext::new(CallOptions::default());
        let other = second.import(&imported).unwrap();
        assert_eq!(second.stats().retained_memory_bytes, retained);
        drop(imported);
        drop(alias);
        assert_eq!(first.stats().retained_memory_bytes, 0);
        assert_eq!(other.as_range(), Some((Some(1), None, true)));
        drop(other);
        assert_eq!(second.stats().retained_memory_bytes, 0);
    }
}
