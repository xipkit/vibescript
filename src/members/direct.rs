//! Builtin members the compiler binds to a call whose receiver has one static
//! base type, so that the call skips dynamic dispatch's search by name.
//!
//! [`call`] runs such a member exactly as dynamic dispatch would, with the
//! same result and the same charges, when the receiver has the runtime kind
//! the member serves; otherwise it declines and the call dispatches
//! dynamically. A test compares it with dynamic dispatch for every member it
//! serves.

use crate::{CallContext, Result, Value, bytecode::Method, hash::Tag, ops, value::Kind};

/// A receiver's static base type, as the checker proves it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Base {
    Hash,
    Array,
    String,
    Int,
    Float,
}

impl Base {
    /// The one base type the checker names for a receiver, when the runtime
    /// binds builtins to it.
    pub(crate) fn of(bases: &[String]) -> Option<Self> {
        let [base] = bases else {
            return None;
        };
        Some(match base.as_str() {
            "hash" => Self::Hash,
            "array" => Self::Array,
            "string" => Self::String,
            "int" => Self::Int,
            "float" => Self::Float,
            _ => return None,
        })
    }
}

/// Whether a call of `method` with `count` arguments, and no block or
/// keywords, on a receiver of `base` is served directly.
pub(crate) fn serves(base: Base, method: Method, count: usize) -> bool {
    use Method::*;
    match base {
        Base::String => matches!(
            (method, count),
            (Length | Empty | ByteSize, 0) | (StartWith | EndWith | Include, 1)
        ),
        Base::Array => matches!(
            (method, count),
            (Length | Empty | First | Last | Sum, 0) | (Fetch | Include, 1) | (Join, 0 | 1)
        ),
        Base::Hash => matches!(
            (method, count),
            (Length | Empty | Keys | Values, 0) | (Fetch | Key | HasValue, 1)
        ),
        Base::Int => matches!((method, count), (Abs | Even | Odd, 0)),
        Base::Float => matches!((method, count), (Abs, 0)),
    }
}

/// Calls `method` on `receiver` as dynamic dispatch would when the receiver's
/// runtime kind is one the member serves, or returns `None` to leave the call
/// to dynamic dispatch: for an arbitrary-precision integer, a host object,
/// whose fields come before hash members, or a rescued error or match data.
pub(crate) fn call(
    ctx: &mut CallContext,
    method: Method,
    name: &str,
    receiver: &Value,
    args: &[Value],
) -> Result<Option<Value>> {
    let base = match &receiver.0 {
        Kind::Bytes(_) => Base::String,
        Kind::Array(_) => Base::Array,
        Kind::Hash(hash) if !hash.object && hash.tag == Tag::None => Base::Hash,
        Kind::Int(_) => Base::Int,
        Kind::Float(_) => Base::Float,
        _ => return Ok(None),
    };
    if !serves(base, method, args.len()) {
        return Ok(None);
    }
    ops::method(ctx, method, name, receiver.clone(), args).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CallOptions, bytecode::CallSite};

    fn context() -> CallContext {
        CallContext::new(CallOptions::default())
    }

    fn outcome(ctx: &CallContext, result: Result<Value>) -> String {
        let stats = ctx.stats();
        let result = match result {
            Ok(value) => format!("{value:?}"),
            Err(error) => format!("{:?} {}", error.kind, error.message),
        };
        format!(
            "{result} steps={} peak={}",
            stats.steps, stats.peak_memory_bytes
        )
    }

    /// Dispatches a call dynamically, as a member call without a block
    /// does: through iteration first for a member that can iterate.
    fn dynamic(
        ctx: &mut CallContext,
        method: Method,
        name: &str,
        receiver: &Value,
        args: &[Value],
    ) -> Result<Value> {
        let site = CallSite {
            name: 0,
            method: Some(method),
            auto: false,
            scope: false,
        };
        if !crate::iteration::method(name) {
            return crate::members::call(ctx, site, name, receiver.clone(), args)
                .map(|(_, value)| value);
        }
        assert!(crate::iteration::start(ctx, name, receiver, args, &[], None)?.is_none());
        // The arguments' own storage belongs to the calling instructions.
        let mut arguments = crate::arguments::Arguments::empty();
        arguments.positional = crate::budget::Buffer::untracked(args.to_vec());
        crate::members::call_keywords(ctx, site, name, receiver.clone(), &arguments)
            .map(|(_, value)| value)
    }

    #[test]
    fn direct_members_match_dynamic_dispatch() {
        let text = |text: &str| Value::bytes(text);
        let texts = [text(""), text("héllo wörld"), text("abc")];
        let arrays = [
            Value::array(vec![]),
            Value::array(vec![Value::int(3), Value::int(1), Value::int(2)]),
            Value::array(vec![Value::int(i64::MAX), Value::int(1)]),
            Value::array(vec![Value::float(0.5), Value::int(2)]),
            Value::array(vec![text("a"), text("bc")]),
        ];
        let hashes = [
            Value::hash(vec![]),
            Value::hash(vec![
                (b"a".to_vec(), Value::int(1)),
                (b"b".to_vec(), Value::int(2)),
            ]),
        ];
        let numbers = [
            Value::int(-7),
            Value::int(i64::MIN),
            Value::int(8),
            Value::float(-2.5),
            Value::float(f64::NAN),
        ];
        let strings = || {
            vec![
                vec![text("")],
                vec![text("hé")],
                vec![text("ld")],
                vec![text("a")],
            ]
        };
        let ints = || {
            [0, 1, -1, 2, 9, i64::MIN]
                .into_iter()
                .map(|n| vec![Value::int(n)])
                .collect::<Vec<_>>()
        };
        let none = || vec![vec![]];
        let mut cases: Vec<(&Value, &str, Method, Vec<Vec<Value>>)> = Vec::new();
        for receiver in &texts {
            cases.push((receiver, "length", Method::Length, none()));
            cases.push((receiver, "empty?", Method::Empty, none()));
            cases.push((receiver, "bytesize", Method::ByteSize, none()));
            cases.push((receiver, "start_with?", Method::StartWith, strings()));
            cases.push((receiver, "end_with?", Method::EndWith, strings()));
            cases.push((receiver, "include?", Method::Include, strings()));
        }
        for receiver in &arrays {
            cases.push((receiver, "length", Method::Length, none()));
            cases.push((receiver, "empty?", Method::Empty, none()));
            cases.push((receiver, "first", Method::First, none()));
            cases.push((receiver, "last", Method::Last, none()));
            cases.push((receiver, "fetch", Method::Fetch, ints()));
            let mut members = ints();
            members.extend(strings());
            cases.push((receiver, "include?", Method::Include, members));
            let mut separators = strings();
            separators.push(vec![]);
            cases.push((receiver, "join", Method::Join, separators));
            if receiver
                .as_array()
                .unwrap()
                .iter()
                .all(|item| item.as_float().is_some())
            {
                cases.push((receiver, "sum", Method::Sum, none()));
            }
        }
        for receiver in &hashes {
            cases.push((receiver, "length", Method::Length, none()));
            cases.push((receiver, "empty?", Method::Empty, none()));
            cases.push((receiver, "keys", Method::Keys, none()));
            cases.push((receiver, "values", Method::Values, none()));
            cases.push((receiver, "fetch", Method::Fetch, strings()));
            cases.push((receiver, "key?", Method::Key, strings()));
            cases.push((receiver, "value?", Method::HasValue, ints()));
        }
        for receiver in &numbers {
            cases.push((receiver, "abs", Method::Abs, none()));
            if receiver.is_integer() {
                cases.push((receiver, "even?", Method::Even, none()));
                cases.push((receiver, "odd?", Method::Odd, none()));
            }
        }
        let mut served = 0;
        for (receiver, name, method, arguments) in cases {
            for args in &arguments {
                let mut direct = context();
                let Some(result) = call(&mut direct, method, name, receiver, args).transpose()
                else {
                    continue;
                };
                served += 1;
                let mut dynamic_context = context();
                let expected = dynamic(&mut dynamic_context, method, name, receiver, args);
                assert_eq!(
                    outcome(&direct, result),
                    outcome(&dynamic_context, expected),
                    "{name} on {receiver:?} with {args:?}"
                );
            }
        }
        assert!(served > 100, "served {served}");
    }
}
