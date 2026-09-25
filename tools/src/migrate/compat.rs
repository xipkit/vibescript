//! What today's runtime accepts, found by asking it, for migrations that
//! must keep running on it.

use std::{
    collections::HashMap,
    sync::{Mutex, OnceLock},
};
use vibescript::Engine;

fn cache() -> &'static Mutex<HashMap<String, bool>> {
    static CACHE: OnceLock<Mutex<HashMap<String, bool>>> = OnceLock::new();
    CACHE.get_or_init(Default::default)
}

/// Whether the linked compiler accepts `source`, for syntax that only the
/// ADR-007 compiler has.
pub(crate) fn compiles(source: &str) -> bool {
    let key = format!("compiles {source}");
    if let Some(&known) = cache().lock().unwrap().get(&key) {
        return known;
    }
    let known = engine().compile(source).is_ok();
    cache().lock().unwrap().insert(key, known);
    known
}

/// Whether the linked compiler names regexes in annotations.
pub(crate) fn regex_type() -> bool {
    compiles("def f(a: regex)\nend\n")
}

/// An engine that compiles without static types, since a migration
/// compiles and runs sources written for the ADR-004 language and partial
/// migrations of them, which need not type check.
pub(crate) fn engine() -> Engine {
    let mut engine = Engine::new();
    engine.set_static_types(false);
    engine
}
