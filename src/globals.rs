use crate::{
    CallContext, Error, ErrorKind, Result, Value,
    budget::{Buffer, MAX_VALUE_DEPTH},
    value::Kind,
};
use std::{collections::BTreeMap, sync::Arc};

/// Validates strict-effects inputs without importing values or executing host code.
pub(crate) fn validate(ctx: &mut CallContext, globals: &BTreeMap<String, Value>) -> Result<()> {
    let mut seen = Visited::empty();
    let mut frames = Buffer::empty();
    for (name, value) in globals {
        ctx.work_bytes(name.len())?;
        if !data(ctx, value, &mut seen, &mut frames)? {
            return Err(Error::new(
                ErrorKind::Runtime,
                format!(
                    "strict effects: global {name} must be data-only; register host capabilities separately"
                ),
            ));
        }
    }
    Ok(())
}

/// One container whose elements are still being validated. Hash positions
/// interleave keys and values so the visit order matches key-then-value.
struct Frame<'a> {
    value: &'a Value,
    position: usize,
}

enum Visit {
    Reject,
    Done,
    Enter,
}

fn data<'a>(
    ctx: &mut CallContext,
    value: &'a Value,
    seen: &mut Visited,
    frames: &mut Buffer<Frame<'a>>,
) -> Result<bool> {
    debug_assert!(frames.data.is_empty());
    let result = walk(ctx, value, seen, frames);
    frames.data.clear();
    result
}

fn walk<'a>(
    ctx: &mut CallContext,
    value: &'a Value,
    seen: &mut Visited,
    frames: &mut Buffer<Frame<'a>>,
) -> Result<bool> {
    match visit(ctx, value, seen, 0)? {
        Visit::Reject => return Ok(false),
        Visit::Done => return Ok(true),
        Visit::Enter => {
            frames.ensure(ctx, value.depth())?;
            frames.push(ctx, Frame { value, position: 0 })?;
        }
    }
    while let Some(frame) = frames.data.last_mut() {
        let parent: &'a Value = frame.value;
        let position = frame.position;
        let child = match &parent.0 {
            Kind::Array(array) => array.buffer.data.get(position),
            Kind::Hash(hash) => hash
                .buffer
                .data
                .get(position / 2)
                .map(|(key, value)| if position % 2 == 0 { key } else { value }),
            _ => unreachable!(),
        };
        let Some(child) = child else {
            let finished = frames.data.pop().unwrap();
            seen.insert(ctx, finished.value)?;
            continue;
        };
        frame.position += 1;
        let depth = frames.data.len();
        match visit(ctx, child, seen, depth)? {
            Visit::Reject => return Ok(false),
            Visit::Done => (),
            Visit::Enter => frames.push(
                ctx,
                Frame {
                    value: child,
                    position: 0,
                },
            )?,
        }
    }
    Ok(true)
}

fn visit(ctx: &mut CallContext, value: &Value, seen: &mut Visited, depth: usize) -> Result<Visit> {
    ctx.charge(1)?;
    if depth > MAX_VALUE_DEPTH || value.depth() > MAX_VALUE_DEPTH - depth {
        return ctx.guard(ErrorKind::Recursion, "value nesting too deep");
    }
    match &value.0 {
        Kind::Function(_)
        | Kind::Host(_)
        | Kind::Instance(_)
        | Kind::Namespace(_)
        | Kind::Builtin(_)
        | Kind::Offset(_)
        | Kind::Shape(_) => Ok(Visit::Reject),
        Kind::Array(_) | Kind::Hash(_) => {
            if seen.contains(ctx, value)? {
                Ok(Visit::Done)
            } else {
                Ok(Visit::Enter)
            }
        }
        _ => Ok(Visit::Done),
    }
}

/// Container identities that a boundary walk has already completed.
///
/// Only containers with more than one strong reference are recorded: a uniquely
/// owned container is reachable through exactly one path, so remembering it
/// would cost memory without saving work. Immutable container graphs cannot
/// contain cycles, so pre-order and post-order insertion are equivalent; walks
/// insert after finishing a container so a failed walk never memoizes anything.
/// Entries are charged to the invocation and released when the memo is dropped.
pub(crate) struct Visited {
    entries: Buffer<usize>,
}

impl Visited {
    pub fn empty() -> Self {
        Self {
            entries: Buffer::empty(),
        }
    }

    fn key(value: &Value) -> Option<usize> {
        match &value.0 {
            Kind::Array(array) if Arc::strong_count(array) > 1 => Some(Arc::as_ptr(array) as usize),
            Kind::Hash(hash) if Arc::strong_count(hash) > 1 => Some(Arc::as_ptr(hash) as usize),
            _ => None,
        }
    }

    /// Reports whether a shared container was already completed by this walk.
    pub fn contains(&self, ctx: &mut CallContext, value: &Value) -> Result<bool> {
        let Some(key) = Self::key(value) else {
            return Ok(false);
        };
        // Completion order makes repeated siblings cheap and keeps work charges
        // independent of allocator addresses and hash-table collisions.
        for &entry in self.entries.data.iter().rev() {
            ctx.charge(1)?;
            if entry == key {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Records a completed shared container; unshared values are ignored.
    pub fn insert(&mut self, ctx: &mut CallContext, value: &Value) -> Result<()> {
        let Some(key) = Self::key(value) else {
            return Ok(());
        };
        ctx.charge(1)?;
        self.entries.push(ctx, key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CallOptions, CancellationToken, ErrorClass, HostMethod, Limits};
    use std::time::Instant;

    fn nested(depth: usize, leaf: Value) -> Value {
        let mut value = leaf;
        for _ in 0..depth {
            value = Value::array(vec![value]);
        }
        value
    }

    fn nested_hash(depth: usize, leaf: Value) -> Value {
        let mut value = leaf;
        for _ in 0..depth {
            value = Value::hash(vec![(b"k".to_vec(), value)]);
        }
        value
    }

    fn globals(entries: &[(&str, Value)]) -> BTreeMap<String, Value> {
        entries
            .iter()
            .map(|(name, value)| ((*name).to_owned(), value.clone()))
            .collect()
    }

    fn host() -> Value {
        HostMethod::new("host.effect", |_, _, _| panic!("host ran")).value()
    }

    #[test]
    fn deepest_data_graphs_validate_and_one_more_level_is_a_recoverable_limit() {
        for build in [nested as fn(usize, Value) -> Value, nested_hash] {
            let mut ctx = CallContext::new(CallOptions::default());
            let accepted = globals(&[("deep", build(MAX_VALUE_DEPTH, Value::int(1)))]);
            validate(&mut ctx, &accepted).unwrap();
            assert!(ctx.stats().steps as usize >= MAX_VALUE_DEPTH);
            assert_eq!(ctx.stats().retained_memory_bytes, 0);

            let rejected = globals(&[("deep", build(MAX_VALUE_DEPTH + 1, Value::int(1)))]);
            let error = validate(&mut ctx, &rejected).unwrap_err();
            assert_eq!(error.kind, ErrorKind::Recursion);
            assert_eq!(error.class(), Some(ErrorClass::Limit));
            assert_eq!(error.message, "value nesting too deep");
            assert!(!ctx.exhausted());
            ctx.charge(1).unwrap();
            assert_eq!(ctx.stats().retained_memory_bytes, 0);
        }
    }

    #[test]
    fn callables_are_found_at_the_bottom_of_the_deepest_graphs() {
        let mut ctx = CallContext::new(CallOptions::default());
        let poison = nested(
            MAX_VALUE_DEPTH - 1,
            Value::hash(vec![(b"m".to_vec(), host())]),
        );
        let error = validate(&mut ctx, &globals(&[("unused", poison)])).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Runtime);
        assert_eq!(
            error.message,
            "strict effects: global unused must be data-only; register host capabilities separately"
        );
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }

    #[test]
    fn error_precedence_follows_global_order_and_element_order() {
        let too_deep = nested(MAX_VALUE_DEPTH + 1, Value::int(1));
        let mut ctx = CallContext::new(CallOptions::default());
        let error = validate(
            &mut ctx,
            &globals(&[("a", Value::array(vec![host()])), ("b", too_deep.clone())]),
        )
        .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Runtime);
        assert!(error.message.contains("global a "), "{error}");

        let mut ctx = CallContext::new(CallOptions::default());
        let error = validate(
            &mut ctx,
            &globals(&[("a", too_deep), ("b", Value::array(vec![host()]))]),
        )
        .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Recursion);

        let mut ctx = CallContext::new(CallOptions::default());
        let ok = nested(MAX_VALUE_DEPTH - 1, Value::int(1));
        let mixed = Value::array(vec![ok.clone(), host(), ok]);
        let error = validate(&mut ctx, &globals(&[("m", mixed)])).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Runtime);
    }

    #[test]
    fn shared_subgraphs_are_validated_once_across_levels_and_globals() {
        let mut data = Value::int(1);
        for _ in 0..MAX_VALUE_DEPTH {
            data = Value::array(vec![data.clone(), data]);
        }
        let mut ctx = CallContext::new(CallOptions {
            limits: Limits {
                steps: Some(16 * MAX_VALUE_DEPTH as u64 + 64),
                ..Default::default()
            },
            ..Default::default()
        });
        validate(&mut ctx, &globals(&[("a", data.clone()), ("b", data)])).unwrap();
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }

    #[test]
    fn validation_work_and_scratch_are_metered_exactly_and_released_on_failure() {
        let mut wide = Vec::new();
        for i in 0..8 {
            wide.push(nested_hash(MAX_VALUE_DEPTH / 2, Value::int(i)));
            wide.push(Value::bytes(vec![b'x'; 64]));
        }
        let input = globals(&[
            (
                "deep",
                nested(
                    MAX_VALUE_DEPTH - MAX_VALUE_DEPTH / 2 - 1,
                    Value::array(wide),
                ),
            ),
            ("flat", Value::array((0..512).map(Value::int).collect())),
        ]);
        let measure = || {
            let mut ctx = CallContext::new(CallOptions::default());
            validate(&mut ctx, &input).unwrap();
            let stats = ctx.stats();
            assert_eq!(stats.retained_memory_bytes, 0);
            stats
        };
        let measured = measure();
        assert_eq!(measured.steps, measure().steps);
        assert_eq!(measured.peak_memory_bytes, measure().peak_memory_bytes);
        assert!(measured.peak_memory_bytes > 0);

        let mut exact = CallContext::new(CallOptions {
            limits: Limits {
                steps: Some(measured.steps),
                memory_bytes: Some(measured.peak_memory_bytes),
                ..Default::default()
            },
            ..Default::default()
        });
        validate(&mut exact, &input).unwrap();
        assert_eq!(exact.stats().retained_memory_bytes, 0);

        let cancelled = CancellationToken::new();
        cancelled.cancel();
        let failures = [
            (
                CallOptions {
                    limits: Limits {
                        steps: Some(measured.steps - 1),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                ErrorKind::Steps,
            ),
            (
                CallOptions {
                    limits: Limits {
                        memory_bytes: Some(measured.peak_memory_bytes - 1),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                ErrorKind::Memory,
            ),
            (
                CallOptions {
                    cancellation: cancelled,
                    ..Default::default()
                },
                ErrorKind::Cancelled,
            ),
            (
                CallOptions {
                    deadline: Some(Instant::now()),
                    ..Default::default()
                },
                ErrorKind::Deadline,
            ),
        ];
        for (options, kind) in failures {
            let mut ctx = CallContext::new(options);
            assert_eq!(validate(&mut ctx, &input).unwrap_err().kind, kind);
            assert_eq!(ctx.stats().retained_memory_bytes, 0);
            assert_eq!(ctx.checkpoint().unwrap_err().kind, kind);
        }
    }

    #[test]
    fn validation_does_not_consume_native_stack_per_level() {
        let input = globals(&[
            ("arrays", nested(MAX_VALUE_DEPTH, Value::int(1))),
            ("hashes", nested_hash(MAX_VALUE_DEPTH, Value::int(2))),
        ]);
        let deep = || {
            let mut ctx = CallContext::new(CallOptions::default());
            validate(&mut ctx, &input).unwrap();
            assert_eq!(ctx.stats().retained_memory_bytes, 0);
        };
        // WASI has no threads, so this runs within the default wasm stack.
        if cfg!(target_os = "wasi") {
            return deep();
        }
        std::thread::scope(|scope| {
            std::thread::Builder::new()
                .stack_size(96 << 10)
                .spawn_scoped(scope, deep)
                .unwrap()
                .join()
                .unwrap();
        });
    }
}
