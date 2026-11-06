use crate::checked::{Access, Checked, violation};
use crate::{
    CallContext, Error, ErrorKind, Result, Value, bytecode::CallSite, bytes, ops, scan, value::Kind,
};
use std::{cmp::Ordering, fmt};

// Byte alignment leaves room for the enum tag within the existing 16-byte Value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Money([u8; 11]);

/// Names a rejected money literal, quoting the input as Go does.
fn rejected(reason: &str, input: &[u8]) -> Error {
    let mut message = reason.as_bytes().to_vec();
    crate::shapes::quote(input, &mut message);
    Error::new(
        ErrorKind::Argument,
        String::from_utf8_lossy(&message).into_owned(),
    )
}
fn overflow() -> Error {
    Error::new(ErrorKind::Arithmetic, "money arithmetic overflow")
}

impl Money {
    pub fn new(cents: i64, currency: &[u8]) -> Result<Self> {
        if currency.len() != 3 || !currency.iter().all(u8::is_ascii_alphabetic) {
            return Err(rejected("currency must be 3 letters, got ", currency));
        }
        let mut bytes = [0; 11];
        bytes[..8].copy_from_slice(&cents.to_le_bytes());
        for (to, from) in bytes[8..].iter_mut().zip(currency) {
            *to = from.to_ascii_uppercase();
        }
        Ok(Self(bytes))
    }
    pub fn cents(self) -> i64 {
        let [a, b, c, d, e, f, g, h, ..] = self.0;
        i64::from_le_bytes([a, b, c, d, e, f, g, h])
    }
    /// Returns the currency code, reporting corrupt inline text as an invariant failure.
    pub fn currency(&self) -> Access<&str> {
        std::str::from_utf8(&self.0[8..]).map_err(|_| violation("money currency must be UTF-8"))
    }
    fn amount(self, cents: i64) -> Self {
        let mut value = self;
        value.0[..8].copy_from_slice(&cents.to_le_bytes());
        value
    }
    /// Orders amounts in the same currency; different currencies have no ordering.
    pub fn order(self, other: Self) -> Access<Option<Ordering>> {
        Ok((self.currency()? == other.currency()?).then(|| self.cents().cmp(&other.cents())))
    }
    pub fn text(self, ctx: &mut CallContext) -> Result<Value> {
        let mut buffer = bytes::Number::new();
        self.currency()?;
        buffer.format(format_args!("{self}"))?;
        ctx.bytes(buffer.bytes()?)
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
            String::from_utf8_lossy(&self.0[8..])
        )
    }
}

pub(crate) fn parse(ctx: &mut CallContext, input: &[u8]) -> Result<Money> {
    let literal = || rejected("invalid money literal ", input);
    let amount_error = || rejected("invalid money amount ", input);
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
        let (ch, width, _) = scan::rune(input.tail(at)?);
        if ch.is_whitespace() {
            if let Some(start) = start.take() {
                *fields.at_mut(count)? = (start, at);
                count += 1;
            }
        } else if start.is_none() {
            if count == 2 {
                return Err(literal());
            }
            start = Some(at);
        }
        at += width;
    }
    if let Some(start) = start {
        *fields.at_mut(count)? = (start, at);
        count += 1;
    }
    if count != 2 {
        return Err(literal());
    }
    let money = Money::new(0, input.span(fields[1].0, fields[1].1)?)?;
    let amount = input.span(fields[0].0, fields[0].1)?;
    let negative = amount.first() == Some(&b'-');
    let sign = usize::from(matches!(amount.first(), Some(b'-' | b'+')));
    let amount = amount.tail(sign)?;
    let mut dot = None;
    for (index, chunk) in amount.chunks(1024).enumerate() {
        ctx.work_bytes(chunk.len())?;
        if let Some(position) = chunk.iter().position(|&byte| byte == b'.') {
            dot = Some(index * 1024 + position);
            break;
        }
    }
    let whole = amount.span(0, dot.unwrap_or(amount.len()))?;
    let fraction = dot
        .and_then(|dot| amount.get(dot + 1..))
        .unwrap_or_default();
    if whole.is_empty() && fraction.is_empty() {
        return Err(amount_error());
    }
    if fraction.len() > 2 {
        // Go rejects malformed digits before an extra decimal place; the field
        // scan above has already charged for reading them.
        let digits = |part: &[u8]| part.iter().all(u8::is_ascii_digit);
        return Err(if digits(whole) && digits(fraction) {
            rejected("money literal supports at most 2 decimal places: ", input)
        } else {
            amount_error()
        });
    }
    let mut dollars = 0u64;
    for (at, &digit) in whole.iter().enumerate() {
        if at % 1024 == 0 {
            ctx.work_bytes((whole.len() - at).min(1024))?;
        }
        if !digit.is_ascii_digit() {
            return Err(amount_error());
        }
        dollars = dollars
            .checked_mul(10)
            .and_then(|v| v.checked_add(u64::from(digit - b'0')))
            .ok_or_else(amount_error)?;
        if dollars > i64::MAX as u64 {
            return Err(amount_error());
        }
    }
    let mut cents = 0u64;
    for &digit in fraction {
        if !digit.is_ascii_digit() {
            return Err(amount_error());
        }
        cents = cents * 10 + u64::from(digit - b'0');
    }
    if fraction.len() == 1 {
        cents *= 10;
    }
    let magnitude = i128::from(dollars) * 100 + i128::from(cents);
    let cents =
        i64::try_from(if negative { -magnitude } else { magnitude }).map_err(|_| amount_error())?;
    Ok(money.amount(cents))
}

pub(crate) fn binary(op: &str, left: &Value, right: &Value) -> Result<Value> {
    let value = match (&left.0, &right.0, op) {
        (Kind::Money(a), Kind::Money(b), "+" | "-") => {
            if a.currency()? != b.currency()? {
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
        _ => return Err(ops::unsupported(op)),
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
        "dup" => {
            crate::members::dup_shape(args.len(), keywords, block)?;
            return Ok(Some(receiver.clone()));
        }
        "currency" | "cents" => {
            if !site.auto || keywords || block {
                return Err(Error::new(
                    ErrorKind::Type,
                    "attempted to call non-callable value",
                ));
            }
            if name == "currency" {
                ctx.bytes(money.currency()?.as_bytes())?
            } else {
                Value::int(money.cents())
            }
        }
        "to_s" | "inspect" => {
            crate::arguments::nullary(&format!("money.{name}"), args, keywords, block)?;
            money.text(ctx)?
        }
        "between?" => {
            crate::arguments::between("money.between?", args, keywords, block)?;
            let low = ops::compare(ctx, receiver, args.at(0)?)?;
            let result = if matches!(low, Some(Ordering::Equal | Ordering::Greater)) {
                matches!(
                    ops::compare(ctx, receiver, args.at(1)?)?,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corrupt_currency_fails_operations_but_remains_printable_for_diagnostics() {
        let mut money = Money::new(123, b"USD").unwrap();
        money.0[8] = 0xff;
        let value = Value(Kind::Money(money));
        assert!(value.as_money().is_none());
        assert_eq!(value.to_string(), "1.23 �SD");
        let mut ctx = CallContext::new(crate::CallOptions::default());
        assert_eq!(money.text(&mut ctx).unwrap_err().kind, ErrorKind::Internal);
        assert_eq!(
            crate::Error::from(money.order(money).unwrap_err()).kind,
            ErrorKind::Internal
        );
        assert_eq!(
            crate::text::inspect::output(&mut ctx, &value, usize::MAX)
                .unwrap_err()
                .kind,
            ErrorKind::Internal
        );
    }
}
