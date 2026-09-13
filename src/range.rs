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
            Kind::Float(n) => {
                if n.is_nan() {
                    return false;
                }
                if descending {
                    self.start.is_none_or(|a| n <= a as f64)
                        && self.end.is_none_or(|b| {
                            if self.exclusive {
                                n > b as f64
                            } else {
                                n >= b as f64
                            }
                        })
                } else {
                    self.start.is_none_or(|a| n >= a as f64)
                        && self.end.is_none_or(|b| {
                            if self.exclusive {
                                n < b as f64
                            } else {
                                n <= b as f64
                            }
                        })
                }
            }
            _ => false,
        }
    }

    fn materialize(&self, ctx: &mut CallContext, count: i64, last: bool) -> Result<Value> {
        if count < 0 {
            return Err(Error::new(
                ErrorKind::Argument,
                "range count must be non-negative",
            ));
        }
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
            (i128::from(count), 0, 1)
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

fn open_error(kind: &str) -> Error {
    Error::new(
        ErrorKind::Argument,
        format!("cannot iterate a {kind} range"),
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

pub(crate) fn method(
    ctx: &mut CallContext,
    method: Method,
    range: &Range,
    args: &[Value],
) -> Result<Value> {
    use Method::*;
    match method {
        Include | Cover | Member => {
            crate::ops::arity(args, 1)?;
            Ok(Value::boolean(range.contains(&args[0])))
        }
        Size | ToArray => {
            crate::ops::arity(args, 0)?;
            let n = i64::try_from(range.length()?)
                .map_err(|_| Error::new(ErrorKind::Arithmetic, "range size overflow"))?;
            if matches!(method, Size) {
                Ok(Value::int(n))
            } else {
                range.materialize(ctx, n, false)
            }
        }
        First | Last => {
            let last = matches!(method, Last);
            let endpoint = if last { range.end } else { range.start }
                .ok_or_else(|| open_error(if last { "endless" } else { "beginless" }))?;
            if args.is_empty() {
                return Ok(Value::int(endpoint));
            }
            crate::ops::arity(args, 1)?;
            range.materialize(ctx, args[0].require_int()?, last)
        }
        ExcludeEnd => {
            crate::ops::arity(args, 0)?;
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
