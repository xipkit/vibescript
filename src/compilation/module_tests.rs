use crate::checked::Faults;
use crate::{Engine, ErrorKind, Result};

const MARKER: &str = "module_parse_fault_marker";
const INJECTED: &str = "injected module parse invariant";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Parse,
    Report,
}

/// Fails the account of a name as long as [`MARKER`], when it is the one
/// a test armed `faults` to fail.
pub(super) fn injected(faults: Option<&Faults>, bytes: usize) -> Result<()> {
    if bytes == MARKER.len() && faults.is_some_and(|faults| faults.parse.strikes()) {
        return Err(crate::checked::internal(INJECTED));
    }
    Ok(())
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
        let fault = &engine.loader.faults.parse;
        fault.arm(match phase {
            Phase::Parse => 0,
            Phase::Report => 2,
        });
        let error = engine
            .compile(source)
            .err()
            .expect("the fault must fail compilation");
        assert!(fault.fired(), "fault did not fire");
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
    let faults = std::sync::Arc::new(Faults::default());
    let mut context = crate::CallContext::new(crate::CallOptions {
        faults: Some(faults.clone()),
        ..Default::default()
    });
    let work = Meter::new(&mut context);
    // Each fail-fast parse lexes the name once, without parsing it.
    faults.parse.arm(1);
    let first = crate::syntax::parse_with_tokens(&source, &work)
        .err()
        .unwrap();
    assert_eq!(first.kind, ErrorKind::Syntax);
    assert_eq!(faults.parse.left(), Some(0));
    faults.parse.arm(1);
    let canonical = crate::syntax::canonical_error(&source, &work).unwrap();
    assert_eq!(canonical.kind, ErrorKind::Syntax);
    assert_eq!(faults.parse.left(), Some(0));
    // Skip host_syntax's canonical lex; fail when recovery parses the name.
    faults.parse.arm(1);
    let error = crate::syntax::host_syntax(&source, &work, first);
    assert_eq!(error.kind, ErrorKind::Internal);
    assert!(error.message.ends_with(INJECTED), "{error}");
    assert_eq!(faults.parse.left(), None);
    work.checkpoint().unwrap();
}
