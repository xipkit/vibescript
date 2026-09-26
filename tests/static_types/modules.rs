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
    let (engine, directory) = engine(&[(
        "dispatch.vibe",
        "class C; def send(n: int) -> int; n; end; end; def value -> C; C.new; end; def send(n: int) -> int; n; end",
    )]);
    let source = "m=require(\"dispatch\"); m.value.send(1) + m.send(2)";
    assert!(
        errors_with(&engine, source).is_empty(),
        "{:?}",
        errors_with(&engine, source)
    );
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
    let (engine, directory) = engine(&[
        ("relative_caller.vibe", source),
        ("relative_target.vibe", HELPERS),
    ]);
    let script = "m = require(\"relative_caller\")\nm.run\n";
    assert!(
        errors_with(&engine, script).is_empty(),
        "{:?}",
        errors_with(&engine, script)
    );
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

#[test]
fn modules_are_namespaces_and_cannot_be_used_as_instance_types() {
    super::support::clean("class C; end; x: C = C.new; module M; N = 1; end; M::N");
    for source in [
        "module M; end; x: M = M",
        "module Outer; module M; end; end; x: Outer::M = Outer::M",
        "module M; end; def f(x: M); end",
    ] {
        super::support::codes(source, &["V0116"]);
        assert!(Engine::new().compile(source).is_err());
    }
}

#[test]
fn capitalized_methods_use_dot_dispatch() {
    let source = "module M; def self.F -> int; 7; end; end; M :: F";
    let diagnostic = super::support::codes(source, &["V0416"]);
    let fixed = super::support::fixed(source, &diagnostic[0]);
    super::support::clean(&fixed);
    assert_eq!(
        Engine::new()
            .compile(&fixed)
            .unwrap()
            .run(Default::default())
            .unwrap()
            .value
            .as_int(),
        Some(7),
    );
    super::support::clean(
        "module M; F = 3; def self.F -> string; 'method'; end; end; x: int = M::F; y: string = M.F",
    );
}

#[test]
fn require_validates_its_call_shape_and_alias() {
    let (engine, directory) = engine(&[("required.vibe", HELPERS), ("other.vibe", HELPERS)]);
    let source =
        "require('required', as: 'helpers'); require('required', as: 'helpers'); helpers.double(3)";
    assert!(errors_with(&engine, source).is_empty());
    assert_eq!(
        engine
            .compile(source)
            .unwrap()
            .run(Default::default())
            .unwrap()
            .value
            .as_int(),
        Some(6)
    );
    for (source, code) in [
        ("require", Code::NO_OVERLOAD),
        ("require('required', 'other')", Code::NO_OVERLOAD),
        ("require('required', bad: 'helpers')", Code::UNKNOWN_KEYWORD),
        ("require('required') { 1 }", Code::UNEXPECTED_BLOCK),
        ("require('required', as: :helpers)", Code::DYNAMIC_REQUIRE),
        ("require(*['required'])", Code::DYNAMIC_REQUIRE),
        (
            "require('required', as: 'not a name')",
            Code::INVALID_REQUIRE_ALIAS,
        ),
        ("require('required', as: 'if')", Code::INVALID_REQUIRE_ALIAS),
        ("h = 1; require('required', as: 'h')", Code::DUPLICATE_NAME),
        (
            "def h; end; require('required', as: 'h')",
            Code::DUPLICATE_NAME,
        ),
        ("require('required', as: 'Math')", Code::DUPLICATE_NAME),
        (
            "require('required', as: 'h'); require('other', as: 'h')",
            Code::DUPLICATE_NAME,
        ),
        (
            "module M; H = 1; def self.load; require('required', as: 'H'); end; end",
            Code::DUPLICATE_NAME,
        ),
    ] {
        let diagnostics = errors_with(&engine, source);
        assert!(
            diagnostics.iter().any(|d| d.code == code),
            "{source}: {diagnostics:?}"
        );
        assert!(engine.compile(source).is_err(), "{source}");
    }
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn unresolved_modules_explain_the_failure_and_keep_foreign_positions() {
    let (engine, directory) = engine(&[
        ("bad-syntax.vibe", "def broken("),
        ("bad-type.vibe", "\n\ndef f -> int\n  'wrong'\nend"),
    ]);
    for (source, reason) in [
        ("require('missing')", "not found"),
        ("require('bad-syntax')", "parse error"),
    ] {
        let errors = errors_with(&engine, source);
        assert_eq!(errors[0].code, Code::UNDEFINED_NAME);
        assert!(errors[0].message.contains(reason), "{errors:?}");
    }
    let errors = errors_with(&engine, "require('bad-type')");
    assert!(errors[0].render("").contains("bad-type.vibe:4:3"));
    assert!(errors[0].to_json("").contains("\"line\":4,\"column\":3"));
    std::fs::remove_dir_all(directory).unwrap();
}
