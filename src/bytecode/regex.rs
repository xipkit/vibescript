//! Regex constants share code without retaining an invocation's memory budget.

use crate::{CallContext, Result, Value, regex::value::Regex};
use std::sync::{Mutex, OnceLock};

#[derive(Debug)]
pub(crate) struct Literal {
    source: Value,
    flags: u8,
    compiled: OnceLock<Result<Value>>,
    compiling: Mutex<()>,
}

impl Literal {
    /// Compiles a host constant now, or defers compilation inside a budgeted
    /// invocation until evaluation, where its work has always been charged.
    pub fn new(source: Value, flags: u8, unmetered: bool) -> Self {
        let compiled = OnceLock::new();
        if unmetered {
            let mut ctx = crate::integer::unlimited_context();
            let value = Regex::compile(&mut ctx, source.clone(), flags, "regex literal");
            if !ctx.exhausted() {
                compiled.set(value).unwrap();
            }
        }
        Self {
            source,
            flags,
            compiled,
            compiling: Mutex::new(()),
        }
    }

    /// Imports a compiled literal into this call, reporting deferred pattern
    /// errors at evaluation and leaving interruption or quota failures uncached.
    pub fn value(&self, ctx: &mut CallContext) -> Result<Value> {
        ctx.checkpoint()?;
        if let Some(value) = self.compiled.get() {
            return ctx.import(value.as_ref().map_err(Clone::clone)?);
        }
        let _compiling = self.compiling.lock().unwrap();
        ctx.checkpoint()?;
        if let Some(value) = self.compiled.get() {
            return ctx.import(value.as_ref().map_err(Clone::clone)?);
        }
        let value = Regex::compile(ctx, self.source.clone(), self.flags, "regex literal");
        if !ctx.exhausted() {
            let cached = match &value {
                Ok(value) => crate::integer::unlimited_context().import(value),
                Err(error) => Err(error.clone()),
            };
            self.compiled.set(cached).unwrap();
        }
        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CallOptions, ErrorKind, Limits};
    use std::sync::Arc;

    #[test]
    fn imports_keep_full_capacity_charges_and_release_each_call() {
        for flags in 0..4 {
            let source = Value::bytes(br"(?<id>ID-[0-9]{8})");
            let literal = Literal::new(source.clone(), flags, true);
            let mut reference = CallContext::new(CallOptions::default());
            let compiled = Regex::compile(&mut reference, source, flags, "regex literal").unwrap();
            let retained = reference.stats().retained_memory_bytes;
            for _ in 0..3 {
                let mut ctx = CallContext::new(CallOptions::default());
                let memory = Arc::downgrade(&ctx.identity());
                let imported = literal.value(&mut ctx).unwrap();
                assert_eq!(imported.as_regex(), compiled.as_regex());
                assert_eq!(ctx.stats().retained_memory_bytes, retained);
                assert!(ctx.stats().steps < reference.stats().steps);
                drop(ctx);
                assert!(memory.upgrade().is_some());
                drop(imported);
                assert!(memory.upgrade().is_none());
            }
        }
    }

    #[test]
    fn cold_compilation_keeps_quotas_and_detaches_the_cached_value() {
        let source = Value::bytes(br"(a?){1000}");
        let literal = Literal::new(source.clone(), 0, false);
        let mut small = CallContext::new(CallOptions {
            limits: Limits {
                steps: Some(10),
                ..Limits::default()
            },
            ..CallOptions::default()
        });
        assert_eq!(
            literal.value(&mut small).unwrap_err().kind,
            ErrorKind::Steps
        );
        assert!(literal.compiled.get().is_none());
        let mut small = CallContext::new(CallOptions {
            limits: Limits {
                memory_bytes: Some(512),
                ..Limits::default()
            },
            ..CallOptions::default()
        });
        assert_eq!(
            literal.value(&mut small).unwrap_err().kind,
            ErrorKind::Memory
        );
        assert!(literal.compiled.get().is_none());
        let mut first = CallContext::new(CallOptions::default());
        let memory = Arc::downgrade(&first.identity());
        let value = literal.value(&mut first).unwrap();
        let mut reference = CallContext::new(CallOptions::default());
        let expected = Regex::compile(&mut reference, source, 0, "regex literal").unwrap();
        assert_eq!(first.stats().steps, reference.stats().steps);
        assert_eq!(
            first.stats().peak_memory_bytes,
            reference.stats().peak_memory_bytes
        );
        assert_eq!(value.as_regex(), expected.as_regex());
        drop(value);
        drop(first);
        assert!(memory.upgrade().is_none());
        let mut next = CallContext::new(CallOptions::default());
        let value = literal.value(&mut next).unwrap();
        assert_eq!(value.as_regex(), expected.as_regex());
        assert_eq!(
            next.stats().retained_memory_bytes,
            reference.stats().retained_memory_bytes
        );
        assert!(next.stats().steps < reference.stats().steps);
    }

    #[test]
    fn pattern_errors_are_deferred_and_cancellation_wins_on_cache_hits() {
        for pattern in [b"[".as_slice(), b"(a{1000}){1000}"] {
            for unmetered in [false, true] {
                let literal = Literal::new(Value::bytes(pattern), 0, unmetered);
                let mut reference = CallContext::new(CallOptions::default());
                let expected =
                    Regex::compile(&mut reference, Value::bytes(pattern), 0, "regex literal")
                        .unwrap_err();
                for _ in 0..2 {
                    let mut ctx = CallContext::new(CallOptions::default());
                    assert_eq!(literal.value(&mut ctx).unwrap_err(), expected);
                }
                let options = CallOptions::default();
                options.cancellation.cancel();
                assert_eq!(
                    literal
                        .value(&mut CallContext::new(options))
                        .unwrap_err()
                        .kind,
                    ErrorKind::Cancelled
                );
                let mut expired = CallContext::new(CallOptions {
                    deadline: Some(std::time::Instant::now()),
                    ..CallOptions::default()
                });
                assert_eq!(
                    literal.value(&mut expired).unwrap_err().kind,
                    ErrorKind::Deadline
                );
            }
        }
        let script = crate::Engine::new()
            .compile("def run(evaluate: bool) -> regex?\nif evaluate\n /(/\nend\nend")
            .unwrap();
        assert_eq!(
            script
                .call("run", &[Value::boolean(false)], CallOptions::default())
                .unwrap()
                .value
                .type_name(),
            "nil"
        );
        let error = script
            .call("run", &[Value::boolean(true)], CallOptions::default())
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Argument);
        assert_eq!(
            error.diagnostic.unwrap().position,
            crate::Position { line: 3, column: 2 }
        );
    }

    #[cfg(not(target_os = "wasi"))]
    #[test]
    fn concurrent_cold_calls_compile_once_without_sharing_budgets() {
        let literal = Literal::new(Value::bytes(br"(a?){1000}"), 0, false);
        let start = std::sync::Barrier::new(8);
        let calls = std::thread::scope(|scope| {
            let calls: Vec<_> = (0..8)
                .map(|_| {
                    scope.spawn(|| {
                        let mut ctx = CallContext::new(CallOptions::default());
                        let memory = Arc::downgrade(&ctx.identity());
                        start.wait();
                        let value = literal.value(&mut ctx).unwrap();
                        let stats = ctx.stats();
                        drop(value);
                        drop(ctx);
                        assert!(memory.upgrade().is_none());
                        stats
                    })
                })
                .collect();
            calls
                .into_iter()
                .map(|call| call.join().unwrap())
                .collect::<Vec<_>>()
        });
        assert_eq!(calls.iter().filter(|stats| stats.steps > 2).count(), 1);
        assert!(
            calls
                .iter()
                .all(|stats| stats.retained_memory_bytes == calls[0].retained_memory_bytes)
        );
    }
}
