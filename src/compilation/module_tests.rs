use crate::{Engine, ErrorKind, Result};
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

const MARKER: &str = "module_parse_fault_marker";
const INJECTED: &str = "injected module parse invariant";
const OFF: usize = usize::MAX;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Parse,
    Report,
}

thread_local! {
    static LEFT: AtomicUsize = const { AtomicUsize::new(OFF) };
}

pub(super) fn injected(bytes: usize) -> Result<()> {
    if bytes == MARKER.len() {
        let fired = LEFT.with(|left| match left.load(Relaxed) {
            OFF => false,
            0 => {
                left.store(OFF, Relaxed);
                true
            }
            more => {
                left.store(more - 1, Relaxed);
                false
            }
        });
        if fired {
            return Err(crate::checked::internal(INJECTED));
        }
    }
    Ok(())
}

struct Armed;

impl Drop for Armed {
    fn drop(&mut self) {
        LEFT.with(|left| left.store(OFF, Relaxed));
    }
}

fn compile_fault(phase: Phase) {
    for nested in [false, true] {
        let mut engine = Engine::new();
        let helper = match phase {
            Phase::Parse => format!("export def value -> string\n  \"{MARKER}\"\nend\n"),
            // Lexing reads the name once in each fail-fast parse. Only
            // diagnostic replay parses its assignment, after the `)`.
            Phase::Report => format!("x = )\n{MARKER} = 1\n"),
        };
        let mut sources = std::collections::BTreeMap::from([("lib/helper.vibe".into(), helper)]);
        let source = if nested {
            sources.insert("outer.vibe".into(), "require(\"lib/helper\")".into());
            "require(\"outer\")"
        } else {
            "require(\"lib/helper\")"
        };
        engine.set_module_sources(sources).unwrap();
        LEFT.with(|left| {
            left.store(
                match phase {
                    Phase::Parse => 0,
                    Phase::Report => 2,
                },
                Relaxed,
            )
        });
        let guard = Armed;
        let error = engine
            .compile(source)
            .err()
            .expect("the fault must fail compilation");
        assert!(
            LEFT.with(|left| left.load(Relaxed) == OFF),
            "fault did not fire"
        );
        drop(guard);
        assert_eq!(error.kind, ErrorKind::Internal, "nested={nested}: {error}");
        assert!(
            error
                .message
                .starts_with("internal error at src/compilation/module_tests.rs:"),
            "{error}"
        );
        assert!(error.message.ends_with(INJECTED), "{error}");
        assert!(error.diagnostics().is_empty(), "{error}");
        // The one-shot fault leaves no quota latch or cached failure behind.
        match phase {
            Phase::Parse => assert!(engine.compile(source).is_ok()),
            Phase::Report => {
                let error = engine.compile(source).err().unwrap();
                assert_eq!(error.kind, ErrorKind::Type, "{error}");
                assert!(error.message.contains("V0201"), "{error}");
            }
        }
    }
}

#[test]
fn initial_module_parse_internal_reaches_compile() {
    compile_fault(Phase::Parse);
}

#[test]
fn module_diagnostic_replay_internal_reaches_compile() {
    compile_fault(Phase::Report);
}

#[test]
fn replay_fault_fires_only_after_the_fail_fast_parses() {
    use crate::compilation::{Meter, Work};

    let source = format!("x = )\n{MARKER} = 1\n");
    let mut context = crate::CallContext::new(Default::default());
    let work = Meter::new(&mut context);
    let guard = Armed;
    // Each fail-fast parse lexes the name once, without parsing it.
    LEFT.with(|left| left.store(1, Relaxed));
    let first = crate::syntax::parse_with_tokens(&source, &work)
        .err()
        .unwrap();
    assert_eq!(first.kind, ErrorKind::Syntax);
    assert_eq!(LEFT.with(|left| left.load(Relaxed)), 0);
    LEFT.with(|left| left.store(1, Relaxed));
    let canonical = crate::syntax::canonical_error(&source, &work).unwrap();
    assert_eq!(canonical.kind, ErrorKind::Syntax);
    assert_eq!(LEFT.with(|left| left.load(Relaxed)), 0);
    // Skip host_syntax's canonical lex; fail when recovery parses the name.
    LEFT.with(|left| left.store(1, Relaxed));
    let error = crate::syntax::host_syntax(&source, &work, first);
    assert_eq!(error.kind, ErrorKind::Internal);
    assert!(error.message.ends_with(INJECTED), "{error}");
    assert_eq!(LEFT.with(|left| left.load(Relaxed)), OFF);
    work.checkpoint().unwrap();
    drop(guard);
}
