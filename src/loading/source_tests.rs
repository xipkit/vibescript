use crate::checked::Faults;
use crate::{CallContext, Engine, ErrorKind, Result};

const MARKER: &str = "lib/resolver_fault_marker";
const INJECTED: &str = "injected module resolver invariant";

/// Fails the next resolution of [`MARKER`] as a test armed `faults` to,
/// once: the resolver breaks an invariant once it has found the file, or
/// runs out of its budget first.
pub(super) fn injected(faults: &Faults, ctx: &mut CallContext, request: &str) -> Result<()> {
    if request != MARKER {
        return Ok(());
    }
    if faults.exhaustion.strikes() {
        let exhausted = ctx.fail::<()>(ErrorKind::Memory, "injected resolver exhaustion");
        assert!(exhausted.is_err() && ctx.exhausted());
        return Err(crate::checked::internal(INJECTED));
    }
    if faults.resolution.strikes() {
        return Err(crate::checked::internal(INJECTED));
    }
    Ok(())
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
        let fault = &engine.loader.faults.resolution;
        fault.arm(0);
        let error = engine
            .compile(source)
            .err()
            .expect("the fault must fail compilation");
        assert!(fault.fired(), "nested={nested}: fault did not fire");
        assert_eq!(error.kind, ErrorKind::Internal, "nested={nested}: {error}");
        // `file!()` spells the path as the fault's caller location does,
        // with the platform's separators.
        let site = format!("internal error at {}:", file!());
        assert!(error.message.starts_with(&site), "{error}");
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
        let fault = &engine.loader.faults.exhaustion;
        fault.arm(0);
        let error = engine
            .compile(source)
            .err()
            .expect("the fault must fail compilation");
        assert!(fault.fired(), "nested={nested}: fault did not fire");
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
