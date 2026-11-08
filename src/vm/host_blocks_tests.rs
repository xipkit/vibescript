use super::*;
use crate::{CallOptions, Capability, Engine, HostMethod, Limits, Outcome, Script, actor::Actor};
use std::panic::{AssertUnwindSafe, catch_unwind};

/// A block argument that makes entering the block panic, after its frame is
/// pushed but before its evaluation begins.
const PANIC_ON_ENTRY: &[u8] = b"panic while entering this block";

pub(super) fn entered(args: &[Value]) {
    if args.first().and_then(Value::as_bytes) == Some(PANIC_ON_ENTRY) {
        panic!("injected panic while entering a block");
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Leaf {
    Value,
    Panic,
    QuotaThenPanic,
    CancelThenPanic,
}

/// What `inspect` passes its block.
#[derive(Clone, Copy, Debug)]
enum Entry {
    Normal,
    Panic,
}

type Trace = Arc<Actor<Vec<String>>>;

/// `inspect` catches a panic from its block and keeps its context; `relay`
/// passes its block through without catching; `leaf` acts as `mode` says.
struct Fixture {
    script: Script,
    options: CallOptions,
    trace: Trace,
}

impl Fixture {
    fn new(body: &str, mode: Leaf, entry: Entry) -> Self {
        let trace = Trace::default();
        let cancellation = crate::CancellationToken::new();
        let observed = trace.clone();
        let args = match entry {
            Entry::Normal => vec![],
            Entry::Panic => vec![Value::bytes(PANIC_ON_ENTRY)],
        };
        let inspect = HostMethod::new_with_block("inspect", move |call, _, _| {
            let before = std::ptr::from_mut(call.context()).addr();
            let caught = catch_unwind(AssertUnwindSafe(|| call.call_block(&args)));
            assert_eq!(std::ptr::from_mut(call.context()).addr(), before);
            let mut seen = vec![match caught {
                Ok(result) => format!(
                    "block {:?}",
                    result.map(|v| v.to_string()).map_err(|error| error.kind)
                ),
                Err(_) => "caught".into(),
            }];
            seen.push(format!(
                "checkpoint {:?}",
                call.context().checkpoint().map_err(|error| error.kind)
            ));
            let again = catch_unwind(AssertUnwindSafe(|| call.call_block(&args)));
            seen.push(match again {
                Ok(result) => format!(
                    "again {:?}",
                    result.map(|v| v.to_string()).map_err(|error| error.kind)
                ),
                Err(_) => "again panicked".into(),
            });
            observed.with(|trace| trace.extend(seen));
            Ok(Value::int(999))
        });
        let relay = HostMethod::new_with_block("relay", |call, _, _| call.call_block(&[]));
        let mut engine = Engine::new();
        let options = CallOptions {
            capabilities: vec![
                Capability::from_value("inspect", inspect.value()),
                Capability::from_value("relay", relay.value()),
            ],
            limits: Limits {
                memory_bytes: Some(32_768),
                ..Limits::default()
            },
            cancellation: cancellation.clone(),
            ..CallOptions::default()
        };
        for capability in &options.capabilities {
            engine.declare_capability(capability).unwrap();
        }
        engine.register("leaf", move |ctx, _| match mode {
            Leaf::Value => Ok(Value::int(7)),
            Leaf::Panic => panic!("nested host panic"),
            Leaf::QuotaThenPanic => {
                let _ignored = ctx.bytes(&[0; 65_536]);
                panic!("nested host panic after quota");
            }
            Leaf::CancelThenPanic => {
                cancellation.cancel();
                let _ignored = ctx.checkpoint();
                panic!("nested host panic after cancellation");
            }
        });
        let marked = trace.clone();
        engine.register("mark", move |_, args| {
            marked.with(|trace| trace.push(args[0].to_string()));
            Ok(Value::nil())
        });
        let script = engine
            .compile(&format!(
                "def run -> any
                   begin
                     value = inspect {{
                       begin
                         {body}
                       rescue
                         mark(\"inner rescue\")
                       ensure
                         mark(\"inner ensure\")
                       end
                     }}
                     mark(\"after\")
                     value
                   rescue
                     mark(\"outer rescue\")
                   ensure
                     mark(\"outer ensure\")
                   end
                 end"
            ))
            .unwrap();
        Self {
            script,
            options,
            trace,
        }
    }

    fn call(&self) -> Result<Outcome> {
        self.script.call("run", &[], self.options.clone())
    }

    fn trace(&self) -> Vec<String> {
        self.trace.read(Vec::clone)
    }
}

fn assert_terminal(fixture: &Fixture, result: Result<Outcome>, kind: ErrorKind, label: &str) {
    let error = result
        .err()
        .unwrap_or_else(|| panic!("{label}: the call must fail"));
    assert_eq!(error.kind, kind, "{label}: {}", error.message);
    if kind == ErrorKind::Internal {
        assert!(
            error
                .message
                .ends_with("block was abandoned before completion"),
            "{label}: {}",
            error.message
        );
    }
    // No rescue or ensure runs, and the host can neither continue the run
    // nor start the block again.
    assert_eq!(
        fixture.trace(),
        [
            "caught".to_string(),
            format!("checkpoint Err({kind:?})"),
            format!("again Err({kind:?})"),
        ],
        "{label}"
    );
}

#[test]
fn blocks_without_panics_keep_working() {
    let fixture = Fixture::new("relay { leaf() }", Leaf::Value, Entry::Normal);
    let outcome = fixture.call().unwrap();
    assert_eq!(outcome.value.as_int(), Some(999));
    assert_eq!(outcome.stats.retained_memory_bytes, 0);
    assert_eq!(
        fixture.trace(),
        [
            "inner ensure",
            "inner ensure",
            "block Ok(\"7\")",
            "checkpoint Ok(())",
            "again Ok(\"7\")",
            "after",
            "outer ensure"
        ]
    );
}

#[cfg_attr(not(panic = "unwind"), ignore = "catching a panic requires unwinding")]
#[test]
fn a_caught_nested_host_panic_makes_the_invocation_terminal() {
    for (body, mode, kind) in [
        ("leaf()", Leaf::Panic, ErrorKind::Internal),
        ("relay { leaf() }", Leaf::Panic, ErrorKind::Internal),
        ("leaf()", Leaf::QuotaThenPanic, ErrorKind::Memory),
        ("relay { leaf() }", Leaf::QuotaThenPanic, ErrorKind::Memory),
        ("leaf()", Leaf::CancelThenPanic, ErrorKind::Cancelled),
    ] {
        let fixture = Fixture::new(body, mode, Entry::Normal);
        let result = fixture.call();
        assert_terminal(&fixture, result, kind, &format!("{body}/{mode:?}"));
    }
}

#[cfg_attr(not(panic = "unwind"), ignore = "catching a panic requires unwinding")]
#[test]
fn a_panic_while_entering_a_block_makes_the_invocation_terminal() {
    let fixture = Fixture::new("leaf()", Leaf::Value, Entry::Panic);
    let result = fixture.call();
    assert_terminal(&fixture, result, ErrorKind::Internal, "entry");
}

/// Under the Tokio runner a synchronous host's block is prepared before the
/// bridge lends the execution to its inline driver.
#[cfg(feature = "tokio")]
#[cfg_attr(not(panic = "unwind"), ignore = "catching a panic requires unwinding")]
#[test]
fn a_bridged_block_is_terminal_when_it_unwinds_before_or_after_it_is_lent() {
    for (body, mode, entry) in [
        ("leaf()", Leaf::Value, Entry::Panic),
        ("leaf()", Leaf::Panic, Entry::Normal),
        ("relay { leaf() }", Leaf::Panic, Entry::Normal),
    ] {
        let fixture = Fixture::new(body, mode, entry);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        let runner = crate::asynchronous::Runner::new(1).unwrap();
        let result = runtime.block_on(runner.call(
            fixture.script.clone(),
            "run".into(),
            vec![],
            fixture.options.clone(),
        ));
        let label = format!("{body}/{mode:?}/{entry:?}");
        assert_terminal(&fixture, result, ErrorKind::Internal, &label);
        assert_eq!(runner.available_slots(), 1, "{label}");
    }
}
