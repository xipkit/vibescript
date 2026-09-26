use crate::{CallContext, Error, ErrorKind, Result, Value, json, math, ops, value::Kind};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Builtin {
    Require,
    Output(crate::output::Kind),
    Format,
    Assert,
    Loop,
    /// `Regex.new`, `Regex.union` and `Regex.escape`, which name themselves
    /// `Regexp.*` in their messages, as Go does.
    Regexp(crate::regex::value::Constructor),
    Regex(crate::regex::Utility),
    Time(crate::time::Constructor),
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
    Require,
    Output(crate::output::Kind),
    Format,
    Assert,
    Loop,
    Regex,
    Time,
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
    /// Every global a script can reach by name, in name order.
    pub const ALL: [Self; 21] = [
        Self::Duration,
        Self::Json,
        Self::Math,
        Self::Regex,
        Self::Time,
        Self::Assert,
        Self::Format,
        Self::Loop,
        Self::Money,
        Self::MoneyCents,
        Self::Output(crate::output::Kind::Inspect),
        Self::Output(crate::output::Kind::Print),
        Self::Output(crate::output::Kind::Puts),
        Self::Random(crate::random::Method::Rand),
        Self::Random(crate::random::Method::Id),
        Self::Require,
        Self::Random(crate::random::Method::Seed),
        Self::ToFloat,
        Self::ToInt,
        Self::Random(crate::random::Method::Uuid),
        Self::Output(crate::output::Kind::Warn),
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::Require => "require",
            Self::Output(kind) => kind.name(),
            Self::Format => "format",
            Self::Assert => "assert",
            Self::Loop => "loop",
            Self::Regex => "Regex",
            Self::Time => "Time",
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
            "require" => Some(Self::Require),
            "format" => Some(Self::Format),
            "assert" => Some(Self::Assert),
            "loop" => Some(Self::Loop),
            "Regex" => Some(Self::Regex),
            "Time" => Some(Self::Time),
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
        let field = |name: &str, value| (Value::bytes(name.as_bytes()), value);
        let mut entries = match self {
            Self::Require => return Value(Kind::Builtin(Require)),
            Self::Output(kind) => return Value(Kind::Builtin(Output(kind))),
            Self::Format => return Value(Kind::Builtin(Format)),
            Self::Assert => return Value(Kind::Builtin(Assert)),
            Self::Loop => return Value(Kind::Builtin(Loop)),
            Self::Regex => vec![
                field(
                    "escape",
                    Value(Kind::Builtin(Regexp(
                        crate::regex::value::Constructor::Escape,
                    ))),
                ),
                field(
                    "match",
                    Value(Kind::Builtin(Regex(crate::regex::Utility::Match))),
                ),
                field(
                    "new",
                    Value(Kind::Builtin(Regexp(crate::regex::value::Constructor::New))),
                ),
                field(
                    "replace",
                    Value(Kind::Builtin(Regex(crate::regex::Utility::Replace))),
                ),
                field(
                    "replace_all",
                    Value(Kind::Builtin(Regex(crate::regex::Utility::ReplaceAll))),
                ),
                field(
                    "union",
                    Value(Kind::Builtin(Regexp(
                        crate::regex::value::Constructor::Union,
                    ))),
                ),
            ],
            Self::Time => [
                crate::time::Constructor::Local,
                crate::time::Constructor::Utc,
                crate::time::Constructor::At,
                crate::time::Constructor::Now,
                crate::time::Constructor::Parse,
            ]
            .into_iter()
            .map(|constructor| {
                field(
                    constructor.name().strip_prefix("Time.").unwrap(),
                    Value(Kind::Builtin(Time(constructor))),
                )
            })
            .collect(),
            Self::Random(method) => return Value(Kind::Builtin(Random(method))),
            Self::Duration => vec![
                field("build", Value(Kind::Builtin(DurationBuild))),
                field("parse", Value(Kind::Builtin(DurationParse))),
            ],
            Self::Money => return Value(Kind::Builtin(Builtin::Money)),
            Self::MoneyCents => return Value(Kind::Builtin(Builtin::MoneyCents)),
            Self::ToInt => return Value(Kind::Builtin(Builtin::ToInt)),
            Self::ToFloat => return Value(Kind::Builtin(Builtin::ToFloat)),
            Self::Json => vec![
                field("parse", Value(Kind::Builtin(JsonParse))),
                field("parse_as", Value(Kind::Builtin(JsonParseAs))),
                field("stringify", Value(Kind::Builtin(JsonStringify))),
            ],
            Self::Math => {
                let mut entries = vec![
                    field("PI", Value::float(std::f64::consts::PI)),
                    field("E", Value::float(std::f64::consts::E)),
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
                    entries.push(field(
                        builtin.name().strip_prefix("Math.").unwrap(),
                        Value(Kind::Builtin(builtin)),
                    ));
                }
                entries
            }
        };
        entries.sort_unstable_by(|(left, _), (right, _)| {
            left.as_bytes().unwrap().cmp(right.as_bytes().unwrap())
        });
        let depth = 1 + entries
            .iter()
            .map(|(_, value)| value.depth())
            .max()
            .unwrap_or(0);
        let mut hash = crate::hash::Hash::untracked(entries, depth);
        Arc::get_mut(&mut hash).unwrap().object = true;
        Value(Kind::Hash(hash))
    }
}

impl Builtin {
    /// Reports whether a read of the builtin without arguments calls it: the
    /// language writes a call without arguments without parentheses.
    pub fn auto(self) -> bool {
        matches!(
            self,
            Self::DurationBuild
                | Self::Output(_)
                | Self::Random(_)
                | Self::Regexp(crate::regex::value::Constructor::Union)
                | Self::Time(crate::time::Constructor::Now)
        )
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
            Self::Require => "require",
            Self::Output(kind) => kind.name(),
            Self::Format => "format",
            Self::Assert => "assert",
            Self::Loop => "loop",
            Self::Regexp(constructor) => constructor.name(),
            Self::Regex(utility) => utility.name(),
            Self::Time(constructor) => constructor.name(),
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

    /// Refuses the builtin as a value, naming it as the script wrote it, as Go does:
    /// a namespaced builtin such as `JSON.parse` is referred to by its member name.
    pub fn value_error(self) -> Error {
        let name = self.name();
        let name = name.rsplit('.').next().unwrap_or(name);
        Error::new(
            ErrorKind::Type,
            format!("{name} is a method and cannot be used as a value; call it with {name}(...)"),
        )
    }

    /// Calls a `JSON` member, checking its call shape in the reference's order
    /// and wording.
    fn json(
        self,
        ctx: &mut CallContext,
        args: &[Value],
        keywords: bool,
        block: bool,
    ) -> Result<Value> {
        let name = self.name();
        let refuse = |kind, problem: &str| Error::new(kind, format!("{name} {problem}"));
        let modifiers = || {
            if keywords {
                Err(refuse(
                    ErrorKind::Argument,
                    "does not accept keyword arguments",
                ))
            } else if block {
                Err(refuse(ErrorKind::Argument, "does not accept blocks"))
            } else {
                Ok(())
            }
        };
        match self {
            Self::JsonParse => {
                let [Value(Kind::Bytes(bytes))] = args else {
                    let kind = if args.len() == 1 {
                        ErrorKind::Type
                    } else {
                        ErrorKind::Argument
                    };
                    return Err(refuse(kind, "expects a single JSON string argument"));
                };
                modifiers()?;
                json::parse_builtin(ctx, &bytes.data, name)
            }
            Self::JsonParseAs => {
                modifiers()?;
                parse_as(ctx, args, |_, _| {
                    Err(Error::new(ErrorKind::Type, "unknown named type"))
                })
            }
            _ => {
                let [value] = args else {
                    return Err(refuse(
                        ErrorKind::Argument,
                        "expects a single value argument",
                    ));
                };
                modifiers()?;
                json::stringify_builtin(ctx, value)
            }
        }
    }

    pub fn call(
        self,
        ctx: &mut CallContext,
        args: &[Value],
        keywords: &[(Value, Value)],
        block: bool,
    ) -> Result<Value> {
        ctx.checkpoint()?;
        // Without values, output needs no script conversion, as a parenless call.
        if let (Self::Output(kind), []) = (self, args) {
            kind.validate(ctx, !keywords.is_empty(), block)?;
            kind.write_empty(ctx)?;
            return Ok(Value::nil());
        }
        // Calls with script conversions or repeated blocks run through the VM.
        if matches!(
            self,
            Self::Output(_) | Self::Format | Self::Loop | Self::Require
        ) {
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
            let message = match args.get(1) {
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
        if self == Self::DurationBuild {
            return crate::duration::build(ctx, args, keywords);
        }
        if self == Self::DurationParse {
            return crate::duration::parse(ctx, args);
        }
        if self == Self::Money {
            if args.len() != 1 {
                return Err(Error::new(
                    ErrorKind::Argument,
                    "money expects a single string literal",
                ));
            }
            let Kind::Bytes(bytes) = &args[0].0 else {
                return Err(Error::new(
                    ErrorKind::Type,
                    "money expects a string literal",
                ));
            };
            return crate::money::parse(ctx, &bytes.data).map(|value| Value(Kind::Money(value)));
        }
        if self == Self::MoneyCents {
            if args.len() != 2 {
                return Err(Error::new(
                    ErrorKind::Argument,
                    "money_cents expects cents and currency",
                ));
            }
            if !matches!(args[0].0, Kind::Int(_) | Kind::Big(_) | Kind::Float(_)) {
                return Err(Error::new(
                    ErrorKind::Type,
                    "money_cents expects integer cents",
                ));
            }
            let cents = crate::conversion::int64(&args[0]).map_err(|error| {
                Error::new(
                    ErrorKind::Type,
                    format!("money_cents expects integer cents: {}", error.message),
                )
            })?;
            let Kind::Bytes(bytes) = &args[1].0 else {
                return Err(Error::new(
                    ErrorKind::Type,
                    "money_cents expects currency string",
                ));
            };
            return crate::money::Money::new(cents, &bytes.data)
                .map(|value| Value(Kind::Money(value)));
        }
        if let Self::Math(method) = self {
            return call_math(self.name(), method, args, !keywords.is_empty(), block);
        }
        if matches!(self, Self::ToInt | Self::ToFloat) {
            // Go checks the argument count before keywords and blocks here.
            let refused = if args.len() != 1 {
                "expects a single value argument"
            } else if !keywords.is_empty() {
                "does not accept keyword arguments"
            } else if block {
                "does not accept blocks"
            } else {
                ""
            };
            if !refused.is_empty() {
                return Err(Error::new(
                    ErrorKind::Argument,
                    format!("{} {refused}", self.name()),
                ));
            }
        }
        if matches!(
            self,
            Self::JsonParse | Self::JsonParseAs | Self::JsonStringify
        ) {
            return self.json(ctx, args, !keywords.is_empty(), block);
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
        ops::arity(args, 1)?;
        let value = &args[0];
        match self {
            Self::ToInt => match &value.0 {
                Kind::Int(_) | Kind::Big(_) => Ok(value.clone()),
                Kind::Float(number) if number.trunc() != *number => Err(Error::new(
                    ErrorKind::Argument,
                    "to_int cannot convert non-integer float",
                )),
                Kind::Float(number) => crate::integer::from_float(ctx, *number, "to_int"),
                Kind::Bytes(bytes) => crate::conversion::integer(ctx, &bytes.data, "to_int"),
                _ => Err(Error::new(
                    ErrorKind::Type,
                    "to_int expects int, float, or string",
                )),
            },
            Self::ToFloat => match &value.0 {
                Kind::Int(_) | Kind::Big(_) | Kind::Float(_) => {
                    Ok(Value::float(value.as_float().unwrap()))
                }
                Kind::Bytes(bytes) => {
                    crate::conversion::float(ctx, &bytes.data, "to_float").map(Value::float)
                }
                _ => Err(Error::new(
                    ErrorKind::Type,
                    "to_float expects int, float, or string",
                )),
            },
            Self::JsonParse
            | Self::JsonStringify
            | Self::JsonParseAs
            | Self::Math(_)
            | Self::Output(_)
            | Self::Format
            | Self::Money
            | Self::MoneyCents
            | Self::DurationBuild
            | Self::DurationParse
            | Self::Time(_)
            | Self::Random(_)
            | Self::Regex(_)
            | Self::Regexp(_)
            | Self::Assert
            | Self::Loop
            | Self::Require => unreachable!(),
        }
    }
}

/// `JSON.parse_as(text, T)`: parses `text` and checks the value against
/// `T`, a type literal, class or enum, as a typed boundary does. `resolve`
/// finds the classes and enums a type literal names.
pub(crate) fn parse_as(
    ctx: &mut CallContext,
    args: &[Value],
    resolve: impl FnMut(&mut CallContext, &str) -> Result<Value>,
) -> Result<Value> {
    let name = Builtin::JsonParseAs.name();
    let refuse = |kind, problem: &str| Error::new(kind, format!("{name} {problem}"));
    let [Value(Kind::Bytes(bytes)), literal] = args else {
        let kind = if args.len() == 2 {
            ErrorKind::Type
        } else {
            ErrorKind::Argument
        };
        return Err(refuse(kind, "expects a JSON string and a type literal"));
    };
    // A class or enum names its own type, as in `as`.
    let nominal = match &literal.0 {
        Kind::Enum(enumeration) => Some(enumeration.definition.name.to_string()),
        Kind::Namespace(class) => Some(class.definition.name.to_string()),
        _ => None,
    };
    if let Some(nominal) = nominal {
        let parsed = json::parse_builtin(ctx, &bytes.data, name)?;
        let ty = crate::types::Type {
            name: nominal,
            kind: crate::types::TypeKind::Named,
            nullable: false,
        };
        return crate::types::prepare(ctx, &ty, |_, _| Ok(literal.clone()))?.normalize_with(
            ctx,
            parsed,
            crate::types::Context::Json,
        );
    }
    let Kind::Shape(shape) = &literal.0 else {
        return Err(refuse(
            ErrorKind::Type,
            "expects a type literal as its second argument",
        ));
    };
    let (parsed, checked) = json::parse_typed(ctx, &bytes.data, name, Some(&shape.definition.ty))?;
    let prepared = crate::types::prepare(ctx, &shape.definition.ty, resolve)?;
    if let Some(steps) = checked {
        ctx.charge_each(steps)?;
        return Ok(parsed);
    }
    prepared.normalize_with(ctx, parsed, crate::types::Context::Json)
}

fn call_math(
    name: &str,
    method: Math,
    args: &[Value],
    keywords: bool,
    block: bool,
) -> Result<Value> {
    use Math::*;
    if keywords || block {
        return Err(Error::new(
            ErrorKind::Argument,
            if keywords {
                format!("{name} does not accept keyword arguments")
            } else {
                format!("{name} does not accept a block")
            },
        ));
    }
    let expected = if method == Log {
        (1..=2).contains(&args.len()).then_some(args.len())
    } else if matches!(method, Atan2 | Hypot) {
        Some(2)
    } else {
        Some(1)
    };
    if expected != Some(args.len()) {
        return Err(Error::new(
            ErrorKind::Argument,
            match method {
                Log => format!("Math.log expects 1 or 2 arguments, got {}", args.len()),
                Atan2 | Hypot => format!("{name} expects 2 arguments, got {}", args.len()),
                _ => format!("{name} expects 1 argument, got {}", args.len()),
            },
        ));
    }
    let number = |value: &Value| {
        value.as_float().ok_or_else(|| {
            Error::new(
                ErrorKind::Type,
                format!(
                    "{name} expects a numeric argument, got {}",
                    value.type_name()
                ),
            )
        })
    };
    let domain = || Error::new(ErrorKind::Arithmetic, format!("{name} out of domain"));
    let x = number(&args[0])?;
    if matches!(method, Sqrt | Log | Log2 | Log10) && x < 0.0
        || matches!(method, Asin | Acos) && (x < -1.0 || x > 1.0)
    {
        return Err(domain());
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
                return Err(domain());
            }
            math::log(x) / math::log(base)
        }
    };
    Ok(Value::float(result))
}

#[cfg(test)]
mod tests {
    use super::Global;

    #[test]
    fn every_global_is_listed_once_in_name_order() {
        let names: Vec<_> = Global::ALL.iter().map(|global| global.name()).collect();
        assert!(names.is_sorted(), "{names:?}");
        assert!(names.windows(2).all(|pair| pair[0] != pair[1]), "{names:?}");
        for global in Global::ALL {
            assert_eq!(Global::parse(global.name()), Some(global));
        }
        for removed in ["proc", "lambda", "Proc", "Kernel", "__main__"] {
            assert_eq!(Global::parse(removed), None);
        }
    }
}
