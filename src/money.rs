use crate::{
    CallContext, Error, ErrorKind, Result, Value, bytecode::CallSite, json, ops, scan, value::Kind,
};
use std::{
    cmp::Ordering,
    fmt::{self, Write},
};

// Byte alignment leaves room for the enum tag within the existing 16-byte Value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Money([u8; 11]);

fn invalid() -> Error {
    Error::new(ErrorKind::Argument, "invalid money literal")
}
fn overflow() -> Error {
    Error::new(ErrorKind::Arithmetic, "money arithmetic overflow")
}

impl Money {
    pub fn new(cents: i64, currency: &[u8]) -> Result<Self> {
        if currency.len() != 3 || !currency.iter().all(u8::is_ascii_alphabetic) {
            return Err(Error::new(
                ErrorKind::Argument,
                "currency must be three ASCII letters",
            ));
        }
        let mut bytes = [0; 11];
        bytes[..8].copy_from_slice(&cents.to_le_bytes());
        for (to, from) in bytes[8..].iter_mut().zip(currency) {
            *to = from.to_ascii_uppercase();
        }
        Ok(Self(bytes))
    }
    pub fn cents(self) -> i64 {
        i64::from_le_bytes(self.0[..8].try_into().unwrap())
    }
    pub fn currency(&self) -> &str {
        std::str::from_utf8(&self.0[8..]).unwrap()
    }
    fn amount(self, cents: i64) -> Self {
        let mut value = self;
        value.0[..8].copy_from_slice(&cents.to_le_bytes());
        value
    }
    pub fn order(self, other: Self) -> Option<Ordering> {
        (self.currency() == other.currency()).then(|| self.cents().cmp(&other.cents()))
    }
    pub fn text(self, ctx: &mut CallContext) -> Result<Value> {
        let mut buffer = json::Number::new();
        write!(buffer, "{self}").unwrap();
        ctx.bytes(buffer.bytes())
    }
}

impl fmt::Display for Money {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let cents = self.cents();
        let sign = if cents < 0 { "-" } else { "" };
        let magnitude = cents.unsigned_abs();
        write!(
            f,
            "{sign}{}.{:02} {}",
            magnitude / 100,
            magnitude % 100,
            self.currency()
        )
    }
}

pub(crate) fn parse(ctx: &mut CallContext, input: &[u8]) -> Result<Money> {
    let mut fields = [(0, 0); 2];
    let mut count = 0;
    let mut start = None;
    let mut at = 0;
    let mut checkpoint = 0;
    while at < input.len() {
        if at >= checkpoint {
            ctx.work_bytes((input.len() - at).min(1024))?;
            checkpoint = at + 1024;
        }
        let (ch, width, _) = scan::rune(&input[at..]);
        if ch.is_whitespace() {
            if let Some(start) = start.take() {
                fields[count] = (start, at);
                count += 1;
            }
        } else if start.is_none() {
            if count == 2 {
                return Err(invalid());
            }
            start = Some(at);
        }
        at += width;
    }
    if let Some(start) = start {
        fields[count] = (start, at);
        count += 1;
    }
    if count != 2 {
        return Err(invalid());
    }
    let money = Money::new(0, &input[fields[1].0..fields[1].1])?;
    let amount = &input[fields[0].0..fields[0].1];
    let negative = amount.first() == Some(&b'-');
    let sign = usize::from(matches!(amount.first(), Some(b'-' | b'+')));
    let amount = &amount[sign..];
    let mut dot = None;
    for (index, chunk) in amount.chunks(1024).enumerate() {
        ctx.work_bytes(chunk.len())?;
        if let Some(position) = chunk.iter().position(|&byte| byte == b'.') {
            dot = Some(index * 1024 + position);
            break;
        }
    }
    let whole = &amount[..dot.unwrap_or(amount.len())];
    let fraction = dot.map_or(b"".as_slice(), |dot| &amount[dot + 1..]);
    if whole.is_empty() && fraction.is_empty() || fraction.len() > 2 {
        return Err(invalid());
    }
    let mut dollars = 0u64;
    for (at, &digit) in whole.iter().enumerate() {
        if at % 1024 == 0 {
            ctx.work_bytes((whole.len() - at).min(1024))?;
        }
        if !digit.is_ascii_digit() {
            return Err(invalid());
        }
        dollars = dollars
            .checked_mul(10)
            .and_then(|v| v.checked_add(u64::from(digit - b'0')))
            .ok_or_else(invalid)?;
        if dollars > i64::MAX as u64 {
            return Err(invalid());
        }
    }
    let mut cents = 0u64;
    for &digit in fraction {
        if !digit.is_ascii_digit() {
            return Err(invalid());
        }
        cents = cents * 10 + u64::from(digit - b'0');
    }
    if fraction.len() == 1 {
        cents *= 10;
    }
    let magnitude = u128::from(dollars) * 100 + u128::from(cents);
    let limit = (i64::MAX as u128) + u128::from(negative);
    if magnitude > limit {
        return Err(invalid());
    }
    let cents = if negative {
        -(magnitude as i128)
    } else {
        magnitude as i128
    } as i64;
    Ok(money.amount(cents))
}

pub(crate) fn binary(op: &str, left: &Value, right: &Value) -> Result<Value> {
    let value = match (&left.0, &right.0, op) {
        (Kind::Money(a), Kind::Money(b), "+" | "-") => {
            if a.currency() != b.currency() {
                return Err(Error::new(ErrorKind::Arithmetic, "money currency mismatch"));
            }
            let cents = if op == "+" {
                a.cents().checked_add(b.cents())
            } else {
                a.cents().checked_sub(b.cents())
            };
            a.amount(cents.ok_or_else(overflow)?)
        }
        (Kind::Money(money), Kind::Int(factor), "*")
        | (Kind::Int(factor), Kind::Money(money), "*") => {
            money.amount(money.cents().checked_mul(*factor).ok_or_else(overflow)?)
        }
        (Kind::Money(money), Kind::Int(divisor), "/") => {
            if *divisor == 0 {
                return Err(Error::new(ErrorKind::Arithmetic, "division by zero"));
            }
            money.amount(money.cents().checked_div(*divisor).ok_or_else(overflow)?)
        }
        (Kind::Money(_), Kind::Big(_), "*" | "/") | (Kind::Big(_), Kind::Money(_), "*") => {
            return Err(overflow());
        }
        _ => return Err(Error::new(ErrorKind::Type, "unsupported money operands")),
    };
    Ok(Value(Kind::Money(value)))
}

pub(crate) fn member(
    ctx: &mut CallContext,
    site: CallSite,
    name: &str,
    receiver: &Value,
    args: &[Value],
    keywords: bool,
    block: bool,
) -> Result<Option<Value>> {
    if site.scope {
        return Ok(None);
    }
    let Kind::Money(money) = receiver.0 else {
        return Ok(None);
    };
    let result = match name {
        "nil?" | "itself" | "dup" | "eql?" | "equal?" => {
            if keywords || block {
                return Err(Error::new(
                    ErrorKind::Argument,
                    "money predicates do not accept keywords or blocks",
                ));
            }
            let comparison = matches!(name, "eql?" | "equal?");
            ops::arity(args, usize::from(comparison))?;
            if comparison {
                Value::boolean(ops::equal(ctx, receiver, &args[0], 0)?)
            } else if name == "nil?" {
                Value::boolean(false)
            } else {
                receiver.clone()
            }
        }
        "currency" | "cents" | "amount" => {
            if !site.auto || keywords || block {
                return Err(Error::new(
                    ErrorKind::Type,
                    "attempted to call non-callable value",
                ));
            }
            match name {
                "currency" => ctx.bytes(money.currency().as_bytes())?,
                "cents" => Value::int(money.cents()),
                _ => money.text(ctx)?,
            }
        }
        "format" => money.text(ctx)?,
        "to_s" | "string" | "inspect" => {
            if keywords || block {
                return Err(Error::new(
                    ErrorKind::Argument,
                    "money rendering does not accept keywords or blocks",
                ));
            }
            ops::arity(args, 0)?;
            money.text(ctx)?
        }
        "between?" => {
            if keywords || block {
                return Err(Error::new(
                    ErrorKind::Argument,
                    "between? does not accept keywords or blocks",
                ));
            }
            ops::arity(args, 2)?;
            let low = ops::compare(ctx, receiver, &args[0])?;
            let result = if matches!(low, Some(Ordering::Equal | Ordering::Greater)) {
                matches!(
                    ops::compare(ctx, receiver, &args[1])?,
                    Some(Ordering::Equal | Ordering::Less)
                )
            } else {
                false
            };
            Value::boolean(result)
        }
        _ => return Ok(None),
    };
    Ok(Some(result))
}
