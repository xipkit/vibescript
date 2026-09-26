//! `require` takes literal names; the checker resolves each required file,
//! checks it, and types its exports by their declarations.

use super::support::errors_with;
use vibescript::{Engine, ModuleConfig, diagnostic::Code};

/// An engine whose module path holds `files`, and the directory to remove.
pub(super) fn engine(files: &[(&str, &str)]) -> (Engine, std::path::PathBuf) {
    // WASI has no temporary directory, so fixtures live under the repository.
    let directory = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join(".cache/tmp")
        .join(format!(
            "static-modules-{}-{}",
            super::common::process_id(),
            files
                .iter()
                .map(|(name, _)| *name)
                .collect::<Vec<_>>()
                .join("-")
        ));
    std::fs::create_dir_all(&directory).unwrap();
    for (name, source) in files {
        std::fs::write(directory.join(name), source).unwrap();
    }
    let mut engine = Engine::new();
    engine
        .set_module_config(ModuleConfig {
            paths: vec![directory.clone()],
            ..ModuleConfig::default()
        })
        .unwrap();
    (engine, directory)
}

const HELPERS: &str =
    "def double(n: int) -> int\n  n * 2\nend\nprivate def hidden -> int\n  1\nend\n";

#[test]
fn required_user_methods_do_not_trigger_builtin_dispatch_rules() {
    let (mut engine, directory) = engine(&[(
        "dispatch.vibe",
        "class C; def send(n: int) -> int; n; end; end; def value -> C; C.new; end; def send(n: int) -> int; n; end",
    )]);
    let source = "m=require(\"dispatch\"); m.value.send(1) + m.send(2)";
    assert!(
        errors_with(&engine, source).is_empty(),
        "{:?}",
        errors_with(&engine, source)
    );
    engine.set_static_types(true);
    assert_eq!(
        engine
            .compile(source)
            .unwrap()
            .run(Default::default())
            .unwrap()
            .value
            .as_int(),
        Some(3)
    );
    let found = errors_with(
        &engine,
        "m=require(\"dispatch\"); m.value.respond_to?(:send)",
    );
    assert!(
        found.iter().any(|d| d.code == Code::DISPATCH_BY_NAME),
        "{found:?}"
    );
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn exports_are_typed_by_their_declarations() {
    let (engine, directory) = engine(&[("helpers.vibe", HELPERS)]);
    let clean = "def run -> int\n  h = require(\"helpers\")\n  h.double(2) + double(3)\nend\n";
    assert!(
        errors_with(&engine, clean).is_empty(),
        "{:?}",
        errors_with(&engine, clean)
    );
    let found = errors_with(
        &engine,
        "def run -> int\n  h = require(\"helpers\")\n  h.double(\"x\")\nend\n",
    );
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].code, Code::TYPE_MISMATCH);
    let found = errors_with(
        &engine,
        "def run -> int\n  h = require(\"helpers\")\n  h.hidden\nend\n",
    );
    assert_eq!(found[0].code, Code::UNKNOWN_MEMBER);
    let aliased = "def run -> int\n  require(\"helpers\", as: \"H\")\n  H.double(1)\nend\n";
    assert!(
        errors_with(&engine, aliased).is_empty(),
        "{:?}",
        errors_with(&engine, aliased)
    );
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn a_required_files_type_errors_are_reported_in_that_file() {
    let broken = "def double(n: int) -> int\n  n + \"x\"\nend\n";
    let (engine, directory) = engine(&[("broken.vibe", broken)]);
    let found = errors_with(&engine, "require(\"broken\")\n");
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].code, Code::NO_OPERATOR);
    assert_eq!(found[0].file.as_deref(), Some(b"broken.vibe".as_slice()));
    let mut engine = engine;
    engine.set_static_types(true);
    let error = engine
        .compile("require(\"broken\")\n")
        .err()
        .expect("a type error");
    assert_eq!(
        error.diagnostics()[0].file.as_deref(),
        Some(b"broken.vibe".as_slice())
    );
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn required_functions_read_their_files_top_level_locals() {
    let (engine, directory) = engine(&[(
        "locals.vibe",
        "x = 4\ndef get -> int\n  x\nend\ndef bad\n  x = \"s\"\nend\n",
    )]);
    let found = errors_with(&engine, "m = require(\"locals\")\nx: int = m.get\n");
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].code, Code::LOCAL_TYPE_CHANGED);
    std::fs::write(
        directory.join("locals.vibe"),
        "x = 4\ndef get -> int\n  x\nend\n",
    )
    .unwrap();
    assert!(errors_with(&engine, "m = require(\"locals\")\nx: int = m.get\n").is_empty());
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn relative_requires_use_the_requiring_files_origin() {
    let source = "other = require(\"./relative_target\")\ndef run -> int\n  other.double(3)\nend\n";
    let (mut engine, directory) = engine(&[
        ("relative_caller.vibe", source),
        ("relative_target.vibe", HELPERS),
    ]);
    let script = "m = require(\"relative_caller\")\nm.run\n";
    assert!(
        errors_with(&engine, script).is_empty(),
        "{:?}",
        errors_with(&engine, script)
    );
    engine.set_static_types(true);
    let value = engine
        .compile(script)
        .unwrap()
        .run(vibescript::CallOptions::default())
        .unwrap()
        .value;
    assert_eq!(value.as_int(), Some(6));
    std::fs::write(
        directory.join("relative_caller.vibe"),
        source.replace("double(3)", "double(\"bad\")"),
    )
    .unwrap();
    let found = errors_with(&engine, script);
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].code, Code::TYPE_MISMATCH);
    std::fs::remove_dir_all(directory).unwrap();
}
