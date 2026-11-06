use crate::{CallContext, Engine, ErrorKind, Result};
use std::sync::atomic::{AtomicU8, Ordering::Relaxed};

const MARKER: &str = "lib/resolver_fault_marker";
const INJECTED: &str = "injected module resolver invariant";

const OFF: u8 = 0;

#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
enum Fault {
    /// The resolver breaks an invariant once it has found the file.
    Internal = 1,
    /// The resolver runs out of its budget, then breaks an invariant.
    Exhausted = 2,
}

thread_local! {
    static ARMED: AtomicU8 = const { AtomicU8::new(OFF) };
}

/// Fails the next resolution of [`MARKER`] as the armed fault says, once.
pub(super) fn injected(ctx: &mut CallContext, request: &str) -> Result<()> {
    if request != MARKER {
        return Ok(());
    }
    match ARMED.with(|armed| armed.swap(OFF, Relaxed)) {
        OFF => Ok(()),
        fault if fault == Fault::Internal as u8 => Err(crate::checked::internal(INJECTED)),
        _ => {
            let exhausted = ctx.fail::<()>(ErrorKind::Memory, "injected resolver exhaustion");
            assert!(exhausted.is_err() && ctx.exhausted());
            Err(crate::checked::internal(INJECTED))
        }
    }
}

struct Armed;

impl Armed {
    fn new(fault: Fault) -> Self {
        ARMED.with(|armed| armed.store(fault as u8, Relaxed));
        Self
    }

    fn fired(&self) -> bool {
        ARMED.with(|armed| armed.load(Relaxed) == OFF)
    }
}

impl Drop for Armed {
    fn drop(&mut self) {
        ARMED.with(|armed| armed.store(OFF, Relaxed));
    }
}

/// An engine whose script requires the marker, directly or through another
/// file, and the script.
fn engine(nested: bool) -> (Engine, &'static str) {
    let mut sources = std::collections::BTreeMap::from([(
        format!("{MARKER}.vibe"),
        "export def value -> int\n  1\nend\n".to_string(),
    )]);
    let source = if nested {
        sources.insert("outer.vibe".into(), format!("require(\"{MARKER}\")"));
        "require(\"outer\")"
    } else {
        "require(\"lib/resolver_fault_marker\")"
    };
    let mut engine = Engine::new();
    engine.set_module_sources(sources).unwrap();
    (engine, source)
}

#[test]
fn module_resolver_internal_reaches_compile() {
    for nested in [false, true] {
        let (engine, source) = engine(nested);
        let armed = Armed::new(Fault::Internal);
        let error = engine
            .compile(source)
            .err()
            .expect("the fault must fail compilation");
        assert!(armed.fired(), "nested={nested}: fault did not fire");
        drop(armed);
        assert_eq!(error.kind, ErrorKind::Internal, "nested={nested}: {error}");
        assert!(
            error
                .message
                .starts_with("internal error at src/loading/source_tests.rs:"),
            "{error}"
        );
        assert!(error.message.ends_with(INJECTED), "{error}");
        assert!(error.diagnostics().is_empty(), "{error}");
        // The one-shot fault leaves no quota latch or cached failure behind.
        assert!(engine.compile(source).is_ok(), "nested={nested}");
    }
}

#[test]
fn module_resolver_exhaustion_outranks_its_internal() {
    for nested in [false, true] {
        let (engine, source) = engine(nested);
        let armed = Armed::new(Fault::Exhausted);
        let error = engine
            .compile(source)
            .err()
            .expect("the fault must fail compilation");
        assert!(armed.fired(), "nested={nested}: fault did not fire");
        drop(armed);
        assert_eq!(error.kind, ErrorKind::Memory, "nested={nested}: {error}");
        assert!(!error.message.contains(INJECTED), "{error}");
        assert!(engine.compile(source).is_ok(), "nested={nested}");
    }
}

#[test]
fn unresolved_module_stays_a_type_diagnostic() {
    for nested in [false, true] {
        let (mut engine, _) = engine(nested);
        let source = if nested {
            engine
                .set_module_sources(std::collections::BTreeMap::from([(
                    "outer.vibe".into(),
                    "require(\"lib/missing\")".into(),
                )]))
                .unwrap();
            "require(\"outer\")"
        } else {
            "require(\"lib/missing\")"
        };
        let error = engine.compile(source).err().unwrap();
        assert_eq!(error.kind, ErrorKind::Type, "nested={nested}: {error}");
        assert!(error.message.contains("V0201"), "{error}");
        assert!(error.message.contains("module not found"), "{error}");
    }
}
