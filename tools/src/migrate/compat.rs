//! What today's runtime accepts, found by asking it, for migrations that
//! must keep running on it.

use std::{
    collections::HashMap,
    sync::{Mutex, OnceLock},
};
use vibescript::{CallOptions, Engine, Value};

/// A value of each receiver kind the rename table names.
fn sample(kind: &str) -> Option<Value> {
    Some(match kind {
        "string" => Value::bytes("a"),
        "symbol" => Value::symbol("a"),
        "array" => Value::array(vec![Value::int(1)]),
        "hash" => Value::hash(vec![(b"a".to_vec(), Value::int(1))]),
        "int" => Value::int(1),
        "float" => Value::float(1.5),
        "money" => Value::money(100, "USD").ok()?,
        "duration" => Value::duration(60),
        "time" => Value::time(0, 0).ok()?,
        "range" => Value::range(Some(1), Some(2), false),
        "nil" => Value::nil(),
        "bool" => Value::boolean(true),
        _ => return None,
    })
}

fn cache() -> &'static Mutex<HashMap<String, bool>> {
    static CACHE: OnceLock<Mutex<HashMap<String, bool>>> = OnceLock::new();
    CACHE.get_or_init(Default::default)
}

/// Whether evaluating `expression` with `x` bound to `value` works without
/// a "cannot be used as a value" error.
fn works(key: String, expression: &str, value: Option<Value>) -> bool {
    if let Some(&known) = cache().lock().unwrap().get(&key) {
        return known;
    }
    let source = format!("def probe(x)\n  {expression}\nend\n");
    let mut engine = Engine::new();
    engine.set_output_writer(|_, _| Ok(()));
    engine.set_error_writer(|_, _| Ok(()));
    let result = engine.compile(&source).and_then(|script| {
        let args = [value.unwrap_or_default()];
        script.call("probe", &args, CallOptions::default())
    });
    let known =
        !matches!(&result, Err(error) if error.message.contains("cannot be used as a value"));
    cache().lock().unwrap().insert(key, known);
    known
}

/// Whether the linked compiler accepts `source`, for syntax that only the
/// ADR-007 compiler has.
pub(crate) fn compiles(source: &str) -> bool {
    let key = format!("compiles {source}");
    if let Some(&known) = cache().lock().unwrap().get(&key) {
        return known;
    }
    let known = Engine::new().compile(source).is_ok();
    cache().lock().unwrap().insert(key, known);
    known
}

/// Whether the linked compiler accepts a typed optional keyword parameter,
/// `name: T: = value`.
pub(crate) fn typed_keyword_defaults() -> bool {
    compiles("def f(a: int: = 1)\nend\n")
}

/// Whether the linked compiler names regexes in annotations.
pub(crate) fn regex_type() -> bool {
    compiles("def f(a: regex)\nend\n")
}

/// Whether `x.member` without parentheses calls the member on a value of `kind`.
pub(crate) fn member_without_parens(kind: &str, member: &str) -> bool {
    let Some(value) = sample(kind) else {
        return true;
    };
    works(
        format!("member {kind} {member}"),
        &format!("x.{member}"),
        Some(value),
    )
}

/// Whether `Namespace.member` without parentheses calls the member.
pub(crate) fn namespace_without_parens(namespace: &str, member: &str) -> bool {
    works(
        format!("namespace {namespace} {member}"),
        &format!("{namespace}.{member}"),
        None,
    )
}
