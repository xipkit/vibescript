use crate::{
    CallContext, Error, ErrorKind, Result, Value,
    budget::{Buffer, Charge, MAX_ENVIRONMENT_DEPTH, MAX_VALUE_DEPTH},
    code::Code,
    globals::Visited,
    objects::Instance,
    value::Kind,
};
use std::sync::Arc;

#[derive(Debug)]
pub(crate) struct Function {
    pub code: Arc<Code>,
    pub environment: Arc<Instance>,
    pub index: usize,
    _header: Option<Charge>,
}

impl Function {
    pub fn new(
        ctx: &mut CallContext,
        code: Arc<Code>,
        environment: Arc<Instance>,
        index: usize,
    ) -> Result<Arc<Self>> {
        ctx.has_exports = true;
        ctx.scoped_sources = true;
        Code::retain(ctx, &code)?;
        let header = ctx.reserve(size_of::<Self>() + 2 * size_of::<usize>())?;
        Ok(Arc::new(Self {
            code,
            environment,
            index,
            _header: header,
        }))
    }

    pub fn import(ctx: &mut CallContext, value: &Arc<Self>) -> Result<Arc<Self>> {
        ctx.has_exports = true;
        ctx.scoped_sources = true;
        if ctx.namespace_depth >= MAX_ENVIRONMENT_DEPTH {
            return ctx.guard(
                ErrorKind::Recursion,
                "function environment nesting too deep",
            );
        }
        ctx.namespace_depth += 1;
        let environment = crate::objects::import(ctx, &value.environment);
        ctx.namespace_depth -= 1;
        Self::with_environment(ctx, value, environment?)
    }

    pub fn with_environment(
        ctx: &mut CallContext,
        value: &Arc<Self>,
        environment: Arc<Instance>,
    ) -> Result<Arc<Self>> {
        Self::new(ctx, value.code.clone(), environment, value.index)
    }

    pub fn same(&self, other: &Self) -> bool {
        self.index == other.index
            && Arc::ptr_eq(&self.code, &other.code)
            && self.environment.same(&other.environment)
    }

    pub fn value_error(&self) -> Error {
        Error::new(
            ErrorKind::Type,
            format!(
                "{} is a function and cannot be used as a value; call it through its module",
                self.code.program.functions[self.index].name
            ),
        )
    }
}

pub(crate) fn check(ctx: &mut CallContext, value: &Value) -> Result<()> {
    if ctx.has_exports {
        check_depth(ctx, value)?;
    }
    Ok(())
}

pub(crate) fn member(
    ctx: &mut CallContext,
    site: crate::bytecode::CallSite,
    name: &str,
    receiver: &Value,
) -> Result<Option<Arc<Function>>> {
    if ctx.has_exports && matches!(receiver.0, Kind::Hash(_)) {
        if let Some(Value(Kind::Function(function))) =
            crate::members::prepare(ctx, site, name, receiver)?
        {
            return Ok(Some(function));
        }
    }
    Ok(None)
}

/// A plain container whose elements are still being checked.
struct Frame<'a> {
    value: &'a Value,
    position: usize,
}

/// Rejects detached callables anywhere inside plain containers. Objects and
/// namespaces keep their methods, so they are not entered. Frames live in a
/// metered buffer sized from the cached height, and shared subtrees are
/// checked once per call through [`Visited`].
fn check_depth(ctx: &mut CallContext, value: &Value) -> Result<()> {
    if !visit(ctx, value, 0)? {
        return Ok(());
    }
    let mut frames = Buffer::with_capacity(ctx, value.depth().min(MAX_VALUE_DEPTH + 1))?;
    let mut seen = Visited::empty();
    frames.push(ctx, Frame { value, position: 0 })?;
    while let Some(frame) = frames.data.last_mut() {
        let parent: &Value = frame.value;
        let position = frame.position;
        let child = match &parent.0 {
            Kind::Array(array) => array.buffer.data.get(position),
            Kind::Hash(hash) => hash.buffer.data.get(position).map(|(_, value)| value),
            _ => unreachable!(),
        };
        let Some(child) = child else {
            let finished = frames.data.pop().unwrap();
            if !frames.data.is_empty() {
                seen.insert(ctx, finished.value)?;
            }
            continue;
        };
        frame.position += 1;
        let depth = frames.data.len();
        if visit(ctx, child, depth)? && !seen.contains(ctx, child)? {
            frames.push(
                ctx,
                Frame {
                    value: child,
                    position: 0,
                },
            )?;
        }
    }
    Ok(())
}

/// Checks one value and reports whether its elements still need visiting.
fn visit(ctx: &mut CallContext, value: &Value, depth: usize) -> Result<bool> {
    ctx.charge(1)?;
    if depth > MAX_VALUE_DEPTH {
        return ctx.guard(ErrorKind::Recursion, "value nesting too deep");
    }
    match &value.0 {
        Kind::Host(method) => Err(method.value_error()),
        Kind::Function(function) => Err(function.value_error()),
        Kind::Array(_) => Ok(true),
        Kind::Hash(hash) => Ok(!hash.object),
        _ => Ok(false),
    }
}

#[cfg(test)]
mod check_tests {
    use super::*;
    use crate::{CallOptions, CancellationToken, ErrorClass, HostMethod, Limits};

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

    fn host() -> Value {
        HostMethod::new("host.effect", |_, _, _| panic!("host ran")).value()
    }

    fn exporting() -> CallContext {
        let mut ctx = CallContext::new(CallOptions::default());
        ctx.has_exports = true;
        ctx
    }

    #[test]
    fn checks_are_skipped_entirely_without_exports() {
        let mut ctx = CallContext::new(CallOptions::default());
        check(&mut ctx, &nested(4, host())).unwrap();
        assert_eq!(ctx.stats().steps, 0);
        assert_eq!(ctx.stats().peak_memory_bytes, 0);
    }

    #[test]
    fn deepest_returns_pass_and_one_more_level_is_a_recoverable_limit() {
        for build in [nested as fn(usize, Value) -> Value, nested_hash] {
            let mut ctx = exporting();
            check(&mut ctx, &build(MAX_VALUE_DEPTH, Value::int(1))).unwrap();
            assert_eq!(ctx.stats().retained_memory_bytes, 0);
            let error = check(&mut ctx, &build(MAX_VALUE_DEPTH + 1, Value::int(1))).unwrap_err();
            assert_eq!(error.kind, ErrorKind::Recursion);
            assert_eq!(error.class(), Some(ErrorClass::Limit));
            assert_eq!(error.message, "value nesting too deep");
            assert!(!ctx.exhausted());
            assert_eq!(ctx.stats().retained_memory_bytes, 0);
        }
    }

    #[test]
    fn callables_are_rejected_at_the_bottom_but_stay_attached_inside_objects() {
        let mut ctx = exporting();
        let detached = nested(
            MAX_VALUE_DEPTH - 1,
            Value::hash(vec![(b"m".to_vec(), host())]),
        );
        let error = check(&mut ctx, &detached).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Type);
        assert!(
            error.message.contains("cannot be used as a value"),
            "{error}"
        );
        assert_eq!(ctx.stats().retained_memory_bytes, 0);

        let attached = nested(
            MAX_VALUE_DEPTH - 1,
            Value::object(vec![(b"m".to_vec(), host())]),
        );
        check(&mut ctx, &attached).unwrap();
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }

    #[test]
    fn element_order_decides_between_callable_and_depth_errors() {
        let too_deep = nested(MAX_VALUE_DEPTH + 1, Value::int(1));
        let mut ctx = exporting();
        let error = check(&mut ctx, &Value::array(vec![host(), too_deep.clone()])).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Type);
        let mut ctx = exporting();
        let error = check(&mut ctx, &Value::array(vec![too_deep, host()])).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Recursion);
    }

    #[test]
    fn shared_subtrees_are_checked_once() {
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
        ctx.has_exports = true;
        check(&mut ctx, &data).unwrap();
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }

    #[test]
    fn check_work_and_scratch_are_metered_exactly_and_released_on_failure() {
        let mut wide = Vec::new();
        for i in 0..8 {
            wide.push(nested_hash(MAX_VALUE_DEPTH / 2, Value::int(i)));
            wide.push(Value::object(vec![(b"m".to_vec(), host())]));
        }
        let input = nested(
            MAX_VALUE_DEPTH - MAX_VALUE_DEPTH / 2 - 1,
            Value::array(wide),
        );
        let measure = || {
            let mut ctx = exporting();
            check(&mut ctx, &input).unwrap();
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
        exact.has_exports = true;
        check(&mut exact, &input).unwrap();
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
        ];
        for (options, kind) in failures {
            let mut ctx = CallContext::new(options);
            ctx.has_exports = true;
            assert_eq!(check(&mut ctx, &input).unwrap_err().kind, kind);
            assert_eq!(ctx.stats().retained_memory_bytes, 0);
            assert_eq!(ctx.checkpoint().unwrap_err().kind, kind);
        }
    }

    #[test]
    fn checks_do_not_consume_native_stack_per_level() {
        let inputs = [
            nested(MAX_VALUE_DEPTH, Value::int(1)),
            nested_hash(MAX_VALUE_DEPTH, Value::int(2)),
        ];
        std::thread::scope(|scope| {
            std::thread::Builder::new()
                .stack_size(96 << 10)
                .spawn_scoped(scope, || {
                    for input in &inputs {
                        let mut ctx = exporting();
                        check(&mut ctx, input).unwrap();
                        assert_eq!(ctx.stats().retained_memory_bytes, 0);
                    }
                })
                .unwrap()
                .join()
                .unwrap();
        });
    }
}
