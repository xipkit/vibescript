//! An internal error reports a VM invariant that failed. The tests corrupt a
//! compiled program's bytecode so that an instruction indexes a table out of
//! range, and check that the host sees the internal error whatever handlers
//! the script wraps around the failing instruction.

use super::*;
use crate::{CallOptions, Engine, Script};

/// Compiles `source` and points the first constant load in `function` past
/// the constant table. The code is sealed in an `Arc` with a weak reference
/// to itself, so it is unwrapped, edited and sealed again.
fn corrupted(source: &str, function: &str) -> Script {
    let script = Engine::new().compile(source).unwrap();
    let crate::ScriptInner {
        code,
        loader,
        strict_effects,
        random_source,
        output_writer,
        error_writer,
    } = Arc::try_unwrap(script.inner).ok().unwrap();
    let mut code = Arc::try_unwrap(code).ok().unwrap();
    let function = code
        .program
        .functions
        .iter_mut()
        .find(|candidate| candidate.name == function)
        .unwrap();
    let constant = function
        .code
        .iter_mut()
        .find(|op| matches!(op, Op::Constant(_)))
        .unwrap();
    *constant = Op::Constant(u32::MAX);
    let code = Arc::new_cyclic(|owner| {
        code.program.owner = owner.clone();
        code
    });
    Script {
        inner: Arc::new(crate::ScriptInner {
            code,
            loader,
            strict_effects,
            random_source,
            output_writer,
            error_writer,
        }),
    }
}

fn internal_error(source: &str) -> Error {
    let script = corrupted(source, "run");
    let error = script
        .call("run", &[], CallOptions::default())
        .expect_err("a corrupted constant index must fail the call");
    assert_eq!(error.kind, ErrorKind::Internal, "{error}");
    assert!(error.class().is_none(), "{error}");
    assert!(
        error.message.contains("index 4294967295 is out of range"),
        "{}",
        error.message
    );
    error
}

#[test]
fn a_corrupted_constant_index_is_an_internal_error() {
    internal_error("def run -> int\n123456\nend");
}

#[test]
fn rescue_does_not_catch_an_internal_error() {
    internal_error("def run -> int\nbegin\n123456\nrescue\n42\nend\nend");
}

#[test]
fn ensure_cannot_return_past_an_internal_error() {
    internal_error("def run -> int\nbegin\n123456\nensure\nreturn 42\nend\nend");
}

#[test]
fn ensure_cannot_replace_an_internal_error() {
    internal_error(
        "def run -> int\nbegin\nbegin\n123456\nensure\nraise \"replacement\"\nend\nrescue\n42\nend\nend",
    );
}
