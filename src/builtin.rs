use crate::{
    CallContext, Error, ErrorKind, Result, Value, bytecode::Method, json, math, ops, value::Kind,
};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Builtin {
    Output(crate::output::Kind),
    Format(crate::format::Function),
    Assert,
    Regexp(crate::regex::value::Constructor),
    Regex(crate::regex::Utility),
    Time(crate::time::Constructor),
    Now,
    Random(crate::random::Method),
    DurationBuild,
    DurationParse,
    Money,
    MoneyCents,
    ToInt,
    ToFloat,
    JsonParse,
    JsonParseAs,
    JsonStringify,
    Math(Math),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Math {
    Sqrt,
    Cbrt,
    Sin,
    Cos,
    Tan,
    Asin,
    Acos,
    Atan,
    Exp,
    Log2,
    Log10,
    Atan2,
    Hypot,
    Log,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Global {
    Output(crate::output::Kind),
    Format(crate::format::Function),
    Assert,
    Regexp,
    Regex,
    Time,
    Now,
    Random(crate::random::Method),
    Duration,
    Money,
    MoneyCents,
    ToInt,
    ToFloat,
    Json,
    Math,
}

impl Global {
    pub fn name(self) -> &'static str {
        match self {
            Self::Output(kind) => kind.name(),
            Self::Format(function) => function.name(),
            Self::Assert => "assert",
            Self::Regexp => "Regexp",
            Self::Regex => "Regex",
            Self::Time => "Time",
            Self::Now => "now",
            Self::Random(method) => method.name(),
            Self::Duration => "Duration",
            Self::Money => "money",
            Self::MoneyCents => "money_cents",
            Self::ToInt => "to_int",
            Self::ToFloat => "to_float",
            Self::Json => "JSON",
            Self::Math => "Math",
        }
    }
    pub fn parse(name: &str) -> Option<Self> {
        if let Some(kind) = crate::output::Kind::parse(name) {
            return Some(Self::Output(kind));
        }
        match name {
            "format" => Some(Self::Format(crate::format::Function::Format)),
            "sprintf" => Some(Self::Format(crate::format::Function::Sprintf)),
            "assert" => Some(Self::Assert),
            "Regexp" => Some(Self::Regexp),
            "Regex" => Some(Self::Regex),
            "Time" => Some(Self::Time),
            "now" => Some(Self::Now),
            "rand" => Some(Self::Random(crate::random::Method::Rand)),
            "srand" => Some(Self::Random(crate::random::Method::Seed)),
            "uuid" => Some(Self::Random(crate::random::Method::Uuid)),
            "random_id" => Some(Self::Random(crate::random::Method::Id)),
            "Duration" => Some(Self::Duration),
            "money" => Some(Self::Money),
            "money_cents" => Some(Self::MoneyCents),
            "to_int" => Some(Self::ToInt),
            "to_float" => Some(Self::ToFloat),
            "JSON" => Some(Self::Json),
            "Math" => Some(Self::Math),
            _ => None,
        }
    }

    pub fn value(self) -> Value {
        use Builtin::*;
        let mut entries = match self {
            Self::Output(kind) => return Value(Kind::Builtin(Output(kind))),
            Self::Format(function) => return Value(Kind::Builtin(Format(function))),
            Self::Assert => return Value(Kind::Builtin(Assert)),
            Self::Regexp => [
                crate::regex::value::Constructor::New,
                crate::regex::value::Constructor::Union,
                crate::regex::value::Constructor::Escape,
                crate::regex::value::Constructor::Quote,
                crate::regex::value::Constructor::LastMatch,
            ]
            .into_iter()
            .map(|constructor| {
                (
                    constructor.member(),
                    Value(Kind::Builtin(Regexp(constructor))),
                )
            })
            .collect(),
            Self::Regex => vec![
                (
                    "match",
                    Value(Kind::Builtin(Regex(crate::regex::Utility::Match))),
                ),
                (
                    "replace",
                    Value(Kind::Builtin(Regex(crate::regex::Utility::Replace))),
                ),
                (
                    "replace_all",
                    Value(Kind::Builtin(Regex(crate::regex::Utility::ReplaceAll))),
                ),
            ],
            Self::Time => [
                crate::time::Constructor::New,
                crate::time::Constructor::Local,
                crate::time::Constructor::Mktime,
                crate::time::Constructor::Utc,
                crate::time::Constructor::Gm,
                crate::time::Constructor::At,
                crate::time::Constructor::Now,
                crate::time::Constructor::Parse,
            ]
            .into_iter()
            .map(|constructor| {
                (
                    constructor.name().strip_prefix("Time.").unwrap(),
                    Value(Kind::Builtin(Time(constructor))),
                )
            })
            .collect(),
            Self::Now => return Value(Kind::Builtin(Now)),
            Self::Random(method) => return Value(Kind::Builtin(Random(method))),
            Self::Duration => vec![
                ("build", Value(Kind::Builtin(DurationBuild))),
                ("parse", Value(Kind::Builtin(DurationParse))),
            ],
            Self::Money => return Value(Kind::Builtin(Builtin::Money)),
            Self::MoneyCents => return Value(Kind::Builtin(Builtin::MoneyCents)),
            Self::ToInt => return Value(Kind::Builtin(Builtin::ToInt)),
            Self::ToFloat => return Value(Kind::Builtin(Builtin::ToFloat)),
            Self::Json => vec![
                ("parse", Value(Kind::Builtin(JsonParse))),
                ("parse_as", Value(Kind::Builtin(JsonParseAs))),
                ("stringify", Value(Kind::Builtin(JsonStringify))),
            ],
            Self::Math => {
                let mut entries = vec![
                    ("PI", Value::float(std::f64::consts::PI)),
                    ("E", Value::float(std::f64::consts::E)),
                ];
                for method in [
                    crate::builtin::Math::Sqrt,
                    crate::builtin::Math::Cbrt,
                    crate::builtin::Math::Sin,
                    crate::builtin::Math::Cos,
                    crate::builtin::Math::Tan,
                    crate::builtin::Math::Asin,
                    crate::builtin::Math::Acos,
                    crate::builtin::Math::Atan,
                    crate::builtin::Math::Exp,
                    crate::builtin::Math::Log2,
                    crate::builtin::Math::Log10,
                    crate::builtin::Math::Atan2,
                    crate::builtin::Math::Hypot,
                    crate::builtin::Math::Log,
                ] {
                    let builtin = Math(method);
                    entries.push((
                        builtin.name().strip_prefix("Math.").unwrap(),
                        Value(Kind::Builtin(builtin)),
                    ));
                }
                entries
            }
        };
        entries.sort_by_key(|(key, _)| *key);
        let mut value = Value::hash(
            entries
                .into_iter()
                .map(|(key, value)| (key.as_bytes().to_vec(), value))
                .collect(),
        );
        let Kind::Hash(hash) = &mut value.0 else {
            unreachable!()
        };
        Arc::get_mut(hash).unwrap().object = true;
        value
    }
}

impl Builtin {
    pub fn auto(self) -> bool {
        self == Self::Now
            || matches!(
                self,
                Self::Random(crate::random::Method::Rand | crate::random::Method::Uuid)
            )
            || self == Self::Regexp(crate::regex::value::Constructor::LastMatch)
            || matches!(self, Self::Time(constructor) if constructor.auto())
    }

    pub fn read(self, ctx: &mut CallContext) -> Result<Value> {
        if self.auto() {
            self.call(ctx, &[], &[], false)
        } else {
            Err(self.value_error())
        }
    }

    pub fn name(self) -> &'static str {
        use Math::*;
        match self {
            Self::Output(kind) => kind.name(),
            Self::Format(function) => function.name(),
            Self::Assert => "assert",
            Self::Regexp(constructor) => constructor.name(),
            Self::Regex(utility) => utility.name(),
            Self::Time(constructor) => constructor.name(),
            Self::Now => "now",
            Self::Random(method) => method.name(),
            Self::DurationBuild => "Duration.build",
            Self::DurationParse => "Duration.parse",
            Self::Money => "money",
            Self::MoneyCents => "money_cents",
            Self::ToInt => "to_int",
            Self::ToFloat => "to_float",
            Self::JsonParse => "JSON.parse",
            Self::JsonParseAs => "JSON.parse_as",
            Self::JsonStringify => "JSON.stringify",
            Self::Math(method) => match method {
                Sqrt => "Math.sqrt",
                Cbrt => "Math.cbrt",
                Sin => "Math.sin",
                Cos => "Math.cos",
                Tan => "Math.tan",
                Asin => "Math.asin",
                Acos => "Math.acos",
                Atan => "Math.atan",
                Exp => "Math.exp",
                Log2 => "Math.log2",
                Log10 => "Math.log10",
                Atan2 => "Math.atan2",
                Hypot => "Math.hypot",
                Log => "Math.log",
            },
        }
    }

    pub fn value_error(self) -> Error {
        if matches!(self, Self::Output(_) | Self::Format(_)) {
            let name = self.name();
            return Error::new(
                ErrorKind::Type,
                format!(
                    "{name} is a method and cannot be used as a value; call it with {name}(...)"
                ),
            );
        }
        Error::new(
            ErrorKind::Type,
            format!(
                "{} is a method and cannot be used as a value; call it directly",
                self.name()
            ),
        )
    }

    pub fn call(
        self,
        ctx: &mut CallContext,
        args: &[Value],
        keywords: &[(Value, Value)],
        block: bool,
    ) -> Result<Value> {
        ctx.checkpoint()?;
        // Calls with script conversions run through the VM.
        if matches!(self, Self::Output(_) | Self::Format(_)) {
            return Err(self.value_error());
        }
        if self == Self::Assert {
            let Some(condition) = args.first() else {
                return Err(Error::new(
                    ErrorKind::Argument,
                    "assert requires a condition argument",
                ));
            };
            if condition.truthy() {
                return Ok(Value::nil());
            }
            let keyword = keywords
                .iter()
                .find(|(key, _)| key.as_bytes() == Some(b"message"))
                .map(|(_, value)| value);
            let message = match args.get(1).or(keyword) {
                Some(value) => ops::to_string(ctx, value)?,
                None => ctx.bytes(b"assertion failed")?,
            };
            let bytes = message.require_bytes()?;
            return Err(Error::from_bytes(ctx, bytes)?.with_class(crate::ErrorClass::Assertion));
        }
        if let Self::Random(method) = self {
            return method.call(ctx, args, keywords, block);
        }
        if let Self::Regexp(constructor) = self {
            return constructor.call(ctx, args, keywords, block);
        }
        if let Self::Regex(utility) = self {
            return utility.call(ctx, args, keywords, block);
        }
        if let Self::Time(constructor) = self {
            return constructor.call(ctx, args, keywords);
        }
        if self == Self::Now {
            return crate::time::now(ctx, args);
        }
        if self == Self::DurationBuild {
            return crate::duration::build(ctx, args, keywords);
        }
        if self == Self::DurationParse {
            return crate::duration::parse(ctx, args);
        }
        if self == Self::Money {
            ops::arity(args, 1)?;
            let Kind::Bytes(bytes) = &args[0].0 else {
                return Err(Error::new(
                    ErrorKind::Type,
                    "money expects a string literal",
                ));
            };
            return crate::money::parse(ctx, &bytes.data).map(|value| Value(Kind::Money(value)));
        }
        if self == Self::MoneyCents {
            ops::arity(args, 2)?;
            let cents = crate::sequence::integer(&args[0]).map_err(|_| {
                Error::new(
                    ErrorKind::Type,
                    "money_cents expects finite cents within the signed 64-bit range",
                )
            })?;
            let Kind::Bytes(bytes) = &args[1].0 else {
                return Err(Error::new(
                    ErrorKind::Type,
                    "money_cents expects a currency string",
                ));
            };
            return crate::money::Money::new(cents, &bytes.data)
                .map(|value| Value(Kind::Money(value)));
        }
        if !keywords.is_empty() || block {
            return Err(Error::new(
                ErrorKind::Argument,
                format!(
                    "{} does not accept keyword arguments or blocks",
                    self.name()
                ),
            ));
        }
        if let Self::Math(method) = self {
            return call_math(method, args);
        }
        ops::arity(args, if self == Self::JsonParseAs { 2 } else { 1 })?;
        let value = &args[0];
        match self {
            Self::ToInt => {
                if let Kind::Float(number) = value.0 {
                    if number.trunc() != number {
                        return Err(Error::new(
                            ErrorKind::Argument,
                            "to_int cannot convert a fractional float",
                        ));
                    }
                }
                if !matches!(
                    value.0,
                    Kind::Int(_) | Kind::Big(_) | Kind::Float(_) | Kind::Bytes(_)
                ) {
                    return Err(Error::new(
                        ErrorKind::Type,
                        "to_int expects an int, float or string",
                    ));
                }
                ops::method(ctx, Method::ToInt, value.clone(), &[])
            }
            Self::ToFloat => match &value.0 {
                Kind::Int(_) | Kind::Big(_) | Kind::Float(_) => {
                    Ok(Value::float(value.as_float().unwrap()))
                }
                Kind::Bytes(bytes) => crate::conversion::float(ctx, &bytes.data).map(Value::float),
                _ => Err(Error::new(
                    ErrorKind::Type,
                    "to_float expects an int, float or string",
                )),
            },
            Self::JsonParse => json::parse_builtin(ctx, value.require_bytes()?),
            Self::JsonStringify => json::stringify_builtin(ctx, value),
            Self::JsonParseAs => {
                let Kind::Bytes(bytes) = &value.0 else {
                    return Err(Error::new(
                        ErrorKind::Type,
                        "JSON.parse_as expects a JSON string",
                    ));
                };
                let Kind::Shape(shape) = &args[1].0 else {
                    return Err(Error::new(
                        ErrorKind::Type,
                        "JSON.parse_as expects a type literal",
                    ));
                };
                let parsed = json::parse_builtin(ctx, &bytes.data)?;
                crate::types::prepare(ctx, &shape.definition.ty, |_, _| {
                    Err(Error::new(ErrorKind::Type, "unknown named type"))
                })?
                .normalize_with(ctx, parsed, crate::types::Context::Json)
            }
            Self::Math(_)
            | Self::Output(_)
            | Self::Format(_)
            | Self::Money
            | Self::MoneyCents
            | Self::DurationBuild
            | Self::DurationParse
            | Self::Time(_)
            | Self::Now
            | Self::Random(_)
            | Self::Regex(_)
            | Self::Regexp(_)
            | Self::Assert => unreachable!(),
        }
    }
}

fn call_math(method: Math, args: &[Value]) -> Result<Value> {
    use Math::*;
    let arity = if matches!(method, Atan2 | Hypot) {
        2
    } else {
        1
    };
    if !(method == Log && args.len() == 2) {
        ops::arity(args, arity)?;
    }
    let number = |value: &Value| {
        value
            .as_float()
            .ok_or_else(|| Error::new(ErrorKind::Type, "Math expects numeric arguments"))
    };
    let x = number(&args[0])?;
    if matches!(method, Sqrt | Log | Log2 | Log10) && x < 0.0
        || matches!(method, Asin | Acos) && (x < -1.0 || x > 1.0)
    {
        return Err(Error::new(
            ErrorKind::Arithmetic,
            "Math argument is outside the function's domain",
        ));
    }
    let result = match method {
        Sqrt => x.sqrt(),
        Cbrt => math::cbrt(x),
        Sin => math::sin(x),
        Cos => math::cos(x),
        Tan => math::tan(x),
        Asin => math::asin(x),
        Acos => math::acos(x),
        Atan => math::atan(x),
        Exp => math::exp(x),
        Log2 => math::log2(x),
        Log10 => math::log10(x),
        Atan2 => math::atan2(x, number(&args[1])?),
        Hypot => math::hypot(x, number(&args[1])?),
        Log if args.len() == 1 => math::log(x),
        Log => {
            let base = number(&args[1])?;
            if base < 0.0 {
                return Err(Error::new(
                    ErrorKind::Arithmetic,
                    "Math.log base is outside its domain",
                ));
            }
            math::log(x) / math::log(base)
        }
    };
    Ok(Value::float(result))
}
