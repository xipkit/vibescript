//! Observation of the values that flow through a running script, for tools
//! that infer types from example runs.
//!
//! An [`Observer`] set with [`Engine::set_observer`](crate::Engine::set_observer)
//! sees parameters as functions start, results as they return, values
//! passed to and returned from `yield`, local and instance-variable writes
//! and reads, the receivers of member calls and indexing, the operands of
//! binary and unary operators, and the values that conditions test. Each
//! event names the source offset the compiler attributes the operation to,
//! as its runtime errors do.
//!
//! Observation is uncharged: it adds no steps or memory to the call, and it
//! does not change what the script does. Scripts compiled without an
//! observer take none of these paths.

use crate::{
    CallContext, Value,
    bytecode::{Function, Invocation, Op},
    value::Kind,
};
use std::sync::Arc;

/// Receives a script's observation events. Events arrive in execution order
/// on the thread running the call.
pub trait Observer: Send + Sync {
    fn observe(&self, event: &Event<'_>);
}

/// One observed operation.
#[derive(Debug)]
pub struct Event<'a> {
    /// The source text of the program that ran the operation, which is a
    /// required file's for code in that file.
    pub source: &'a str,
    /// The byte offset the compiler attributes the operation to: a
    /// function's `def` for [`Site::Parameter`] and [`Site::Return`], a
    /// block's `do` or `{` for [`Site::BlockResult`], an operator for
    /// [`Site::Binary`], and otherwise the start of the expression.
    pub offset: usize,
    pub site: Site<'a>,
    /// The observed values, as each site describes.
    pub values: &'a [Value],
}

/// What an [`Event`] observed.
#[derive(Debug)]
#[non_exhaustive]
pub enum Site<'a> {
    /// A function starting, before its arguments bind; no values.
    Enter { function: &'a str },
    /// A parameter bound as its function starts: its value.
    Parameter {
        /// The function's qualified name, such as `Invoice#total`.
        function: &'a str,
        name: &'a str,
        index: usize,
    },
    /// A function returning, including through a `return` in a block: its result.
    Return { function: &'a str },
    /// A block finishing, and the value it passes back.
    BlockResult,
    /// A `yield`: the values it passes.
    Yield,
    /// A `yield` completing: the block's result.
    YieldResult,
    /// A local assigned: its new value.
    Store { name: &'a str },
    /// A local read: its value.
    Load { name: &'a str },
    /// An instance variable assigned: the instance and the value.
    InstanceStore { name: &'a str },
    /// An instance variable read: the instance and the value.
    InstanceLoad { name: &'a str },
    /// A member call: its receiver.
    Receiver { member: &'a str },
    /// A member call returning to its caller: its result.
    Result { member: &'a str },
    /// An index read: the receiver and the selectors.
    Index,
    /// A binary operator: both operands.
    Binary { operator: &'a str },
    /// A unary operator: its operand.
    Unary { operator: &'a str },
    /// A branch taken on a condition: the value tested. `origin` is the
    /// offset of the operation that computed it, which tells apart the
    /// conditions of one `if` and its `elsif` branches.
    Condition { origin: usize },
    /// A `case` comparing its subject with a `when` value: both of them.
    Case,
}

/// The name of an instance's class, such as `Invoice` or `Outer::Inner`.
pub fn class_name(value: &Value) -> Option<&str> {
    match &value.0 {
        Kind::Instance(instance) => Some(instance.class().definition.name.as_str()),
        _ => None,
    }
}

/// The observer and the reads waiting for their value.
pub(crate) struct State {
    observer: Arc<dyn Observer>,
    /// Reads whose value is on the stack once they complete.
    pending: Vec<Read>,
    /// The function and location of the last instruction each frame ran.
    last: Vec<(usize, usize)>,
}

/// A read waiting for the instruction after it.
#[derive(Clone, Copy)]
struct Read {
    depth: usize,
    /// The address of the function running it, which identifies the
    /// function while its program runs.
    function: usize,
    /// The read's instruction.
    issued: usize,
    /// The instruction that runs once it completes.
    next: usize,
    kind: Pending,
}

#[derive(Clone, Copy)]
enum Pending {
    Load(usize),
    Instance(usize),
    Yield,
    Result(usize),
}

impl State {
    pub(crate) fn new(observer: Arc<dyn Observer>) -> Self {
        Self {
            observer,
            pending: Vec::new(),
            last: Vec::new(),
        }
    }
}

/// The per-frame facts the hook needs.
pub(crate) struct Frame<'a> {
    pub depth: usize,
    pub ip: usize,
    pub local_base: usize,
    pub block: bool,
    pub receiver: Option<&'a Value>,
    /// The function a `return` in this frame leaves.
    pub home: Option<&'a Function>,
}

fn emit(ctx: &CallContext, source: &str, offset: usize, site: Site<'_>, values: &[Value]) {
    if let Some(state) = &ctx.observation {
        state.observer.observe(&Event {
            source,
            offset,
            site,
            values,
        });
    }
}

/// Reports a function starting, before its arguments bind.
pub(crate) fn enter(ctx: &CallContext, source: &str, function: &Function) {
    let site = Site::Enter {
        function: &function.name,
    };
    emit(ctx, source, function.offset as usize, site, &[]);
}

/// Reports a function's parameters once they are bound.
pub(crate) fn parameters(
    ctx: &CallContext,
    source: &str,
    function: &Function,
    locals: &[Option<Value>],
    local_base: usize,
) {
    if ctx.observation.is_none() {
        return;
    }
    for (index, param) in function.params.iter().enumerate() {
        if let Some(Some(value)) = locals.get(local_base + param.slot) {
            let site = Site::Parameter {
                function: &function.name,
                name: &param.name,
                index,
            };
            emit(
                ctx,
                source,
                function.offset as usize,
                site,
                std::slice::from_ref(value),
            );
        }
    }
}

/// Reports what the instruction at `frame.ip` is about to do, and the value
/// an earlier read left for this frame.
#[allow(clippy::too_many_arguments)]
pub(crate) fn before(
    ctx: &mut CallContext,
    program: &crate::bytecode::Program,
    function: &Function,
    frame: Frame<'_>,
    op: &Op,
    stack: &[Value],
    locals: &[Option<Value>],
    addresses: &[crate::address::Address],
) {
    let Some(state) = &mut ctx.observation else {
        return;
    };
    let source = program.source.text();
    let offset = function.locations[frame.ip] as usize;
    let function_ptr = function as *const Function as usize;
    if state.last.len() <= frame.depth {
        state.last.resize(frame.depth + 1, (0, 0));
    }
    let (last_function, last_offset) =
        std::mem::replace(&mut state.last[frame.depth], (function_ptr, offset));
    let origin = if last_function == function_ptr {
        last_offset
    } else {
        offset
    };
    while state
        .pending
        .last()
        .is_some_and(|read| read.depth > frame.depth)
    {
        state.pending.pop();
    }
    if let Some(read) = state.pending.last().copied()
        && read.depth == frame.depth
    {
        state.pending.pop();
        let pending = read.kind;
        if read.function == function_ptr
            && read.next == frame.ip
            && let Some(value) = stack.last()
        {
            let site_offset = function.locations[read.issued] as usize;
            let site = match pending {
                Pending::Load(slot) => Site::Load {
                    name: &function.local_names[slot],
                },
                Pending::Instance(name) => Site::InstanceLoad {
                    name: &program.members[name][1..],
                },
                Pending::Yield => Site::YieldResult,
                Pending::Result(name) => Site::Result {
                    member: &program.members[name],
                },
            };
            let values = [frame.receiver.cloned().unwrap_or_default(), value.clone()];
            let values = match pending {
                Pending::Instance(_) => &values[..],
                _ => &values[1..],
            };
            emit(ctx, source, site_offset, site, values);
        }
    }
    let last = |n: usize| &stack[stack.len() - n..];
    let pend = |ctx: &mut CallContext, next: usize, kind| {
        if let Some(state) = &mut ctx.observation {
            state.pending.push(Read {
                depth: frame.depth,
                function: function_ptr,
                issued: frame.ip,
                next,
                kind,
            });
        }
    };
    match *op {
        Op::Store(slot) => emit(
            ctx,
            source,
            offset,
            Site::Store {
                name: &function.local_names[slot],
            },
            last(1),
        ),
        Op::Load(slot) => pend(ctx, frame.ip + 1, Pending::Load(slot)),
        Op::LoadOptional(slot, ..) => {
            if locals
                .get(frame.local_base + slot)
                .is_some_and(Option::is_some)
            {
                pend(ctx, frame.ip + 1, Pending::Load(slot));
            }
        }
        Op::ReceiverBound(slot, next) => pend(ctx, next, Pending::Load(slot)),
        Op::NamespaceStore(name) | Op::NamespaceVariable(name, _) => {
            let raw = &program.members[name];
            if raw.starts_with('@') && !raw.starts_with("@@") {
                if matches!(op, Op::NamespaceVariable(..)) {
                    pend(ctx, frame.ip + 1, Pending::Instance(name));
                } else if let Some(receiver) = frame.receiver {
                    let values = [receiver.clone(), stack.last().cloned().unwrap_or_default()];
                    let site = Site::InstanceStore { name: &raw[1..] };
                    emit(ctx, source, offset, site, &values);
                }
            }
        }
        Op::Method(call, n) => {
            let site = Site::Receiver {
                member: &program.members[call.name],
            };
            emit(
                ctx,
                source,
                offset,
                site,
                &stack[stack.len() - n - 1..][..1],
            );
            pend(ctx, frame.ip + 1, Pending::Result(call.name));
        }
        Op::Mutate(call, _) | Op::Invoke(Invocation::Member(call, true)) => {
            if let Some(address) = addresses.last() {
                let site = Site::Receiver {
                    member: &program.members[call.name],
                };
                emit(
                    ctx,
                    source,
                    offset,
                    site,
                    std::slice::from_ref(&address.value),
                );
            }
            pend(ctx, frame.ip + 1, Pending::Result(call.name));
        }
        // A member read on the way to a mutation, such as `h.items` in
        // `h.items.push(1)`, reads through an address.
        Op::AddressMember(call) | Op::AddressMemberTarget(call, _) => {
            if let Some(address) = addresses.last() {
                let site = Site::Receiver {
                    member: &program.members[call.name],
                };
                emit(
                    ctx,
                    source,
                    offset,
                    site,
                    std::slice::from_ref(&address.value),
                );
            }
        }
        Op::Invoke(Invocation::Member(call, false)) => {
            let site = Site::Receiver {
                member: &program.members[call.name],
            };
            emit(ctx, source, offset, site, last(1));
            pend(ctx, frame.ip + 1, Pending::Result(call.name));
        }
        Op::Index(n) => emit(ctx, source, offset, Site::Index, last(n + 1)),
        Op::Binary(operator) => emit(ctx, source, offset, Site::Binary { operator }, last(2)),
        Op::Unary(operator) => emit(ctx, source, offset, Site::Unary { operator }, last(1)),
        Op::JumpFalse(_) | Op::JumpTrue(_) | Op::LoopTest => {
            emit(ctx, source, offset, Site::Condition { origin }, last(1));
        }
        Op::CaseCompare(true, _) => emit(ctx, source, offset, Site::Case, last(2)),
        Op::Yield(n) => {
            emit(ctx, source, offset, Site::Yield, last(n));
            pend(ctx, frame.ip + 1, Pending::Yield);
        }
        Op::BindEnd => parameters(ctx, source, function, locals, frame.local_base),
        Op::Return | Op::Finish => {
            let nonlocal = matches!(op, Op::Return) && frame.block && !function.initializer;
            if frame.block && !nonlocal {
                emit(
                    ctx,
                    source,
                    function.offset as usize,
                    Site::BlockResult,
                    last(1),
                );
            } else if let Some(home) = if nonlocal { frame.home } else { Some(function) } {
                let site = Site::Return {
                    function: &home.name,
                };
                emit(ctx, source, home.offset as usize, site, last(1));
            }
        }
        _ => (),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CallOptions, Engine};
    use std::sync::Mutex;

    #[derive(Default)]
    struct Log(Mutex<Vec<String>>);

    impl Observer for Log {
        fn observe(&self, event: &Event<'_>) {
            let values: Vec<String> = event.values.iter().map(|v| v.type_name().into()).collect();
            let site = match &event.site {
                Site::Parameter { name, .. } => format!("param {name}"),
                Site::Return { function } => format!("return {function}"),
                Site::Store { name } => format!("store {name}"),
                Site::Binary { operator } => format!("binary {operator}"),
                Site::Condition { origin } => format!("condition from {origin}"),
                Site::Receiver { member } => format!("receiver {member}"),
                Site::Result { member } => format!("result {member}"),
                _ => return,
            };
            self.0
                .lock()
                .unwrap()
                .push(format!("{} {site} {}", event.offset, values.join(" ")));
        }
    }

    #[test]
    fn reports_values_at_their_compiler_offsets() {
        let log = Arc::new(Log::default());
        let mut engine = Engine::new();
        engine.set_observer(log.clone());
        let source = "def half(n)\n  x = n / 2\n  x if n\nend\n";
        let script = engine.compile(source).unwrap();
        let outcome = script
            .call("half", &[Value::int(7)], CallOptions::default())
            .unwrap();
        assert_eq!(outcome.value.as_int(), Some(3));
        assert_eq!(
            *log.0.lock().unwrap(),
            [
                "0 param n int",
                "20 binary / int int",
                "14 store x int",
                "26 condition from 31 int",
                "0 return half int",
            ]
        );
    }

    #[test]
    fn leaves_results_and_accounting_unchanged() {
        let source = "def run(items)\n  items.map { |x| x * 2 }.sum\nend\n";
        let args = [Value::array(vec![Value::int(1), Value::int(2)])];
        let plain = Engine::new().compile(source).unwrap();
        let expected = plain.call("run", &args, CallOptions::default()).unwrap();
        let log = Arc::new(Log::default());
        let mut engine = Engine::new();
        engine.set_observer(log.clone());
        let observed = engine.compile(source).unwrap();
        let outcome = observed.call("run", &args, CallOptions::default()).unwrap();
        assert_eq!(outcome.value.as_int(), expected.value.as_int());
        assert_eq!(outcome.stats.steps, expected.stats.steps);
        let log = log.0.lock().unwrap();
        assert!(log.contains(&"17 receiver map array".to_owned()), "{log:?}");
        assert!(log.contains(&"17 result sum int".to_owned()), "{log:?}");
    }
}
