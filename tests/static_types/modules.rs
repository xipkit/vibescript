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
