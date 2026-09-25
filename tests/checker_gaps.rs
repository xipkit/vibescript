//! Gaps between the static checker, its fixes and the runtime that migrating
//! the documentation found, each with programs the checker accepts, programs
//! it rejects, and what the runtime does with them.

use vibescript::{
    CallOptions, Engine, Value,
    diagnostic::{Applicability, Code, Diagnostic},
};

/// The error diagnostics of `source`, checked by `engine`.
fn errors_with(engine: &Engine, source: &str) -> Vec<Diagnostic> {
    engine
        .type_check(source)
        .unwrap_or_else(|error| panic!("{source}\ndoes not parse: {error}"))
        .diagnostics
        .into_iter()
        .filter(Diagnostic::is_error)
        .collect()
}

fn describe(source: &str, diagnostics: &[Diagnostic]) -> String {
    diagnostics.iter().map(|d| d.render(source)).collect()
}

/// Asserts that `source` has exactly errors with these codes, in source
/// order, and returns them.
#[track_caller]
fn codes_with(engine: &Engine, source: &str, expected: &[Code]) -> Vec<Diagnostic> {
    let found = errors_with(engine, source);
    let actual: Vec<Code> = found.iter().map(|d| d.code).collect();
    assert_eq!(
        actual,
        expected,
        "in\n{source}\nfound:\n{}",
        describe(source, &found)
    );
    found
}

#[track_caller]
fn codes(source: &str, expected: &[Code]) -> Vec<Diagnostic> {
    codes_with(&Engine::new(), source, expected)
}

#[track_caller]
fn clean(source: &str) {
    codes(source, &[]);
}

/// Applies a diagnostic's machine-applicable fix.
#[track_caller]
fn fixed(source: &str, diagnostic: &Diagnostic) -> String {
    let fix = diagnostic
        .fixes
        .iter()
        .find(|fix| fix.applicability == Applicability::Always)
        .unwrap_or_else(|| panic!("no fix on {diagnostic:?}"));
    fix.apply(source).expect("the fix applies")
}

/// Runs `source` with static types on and returns the value of `run`.
#[track_caller]
fn run(source: &str) -> Result<Value, vibescript::Error> {
    let mut engine = Engine::new();
    engine.set_static_types(true);
    let script = engine
        .compile(source)
        .unwrap_or_else(|error| panic!("{source}\ndoes not compile: {error}"));
    script
        .call("run", &[], CallOptions::default())
        .map(|result| result.value)
}

mod scoped_calls {
    use super::*;

    #[test]
    fn calls_written_with_two_colons_are_removed_with_a_dot_fix() {
        let source = "x = JSON::parse(\"1\")\n";
        let found = codes(source, &[Code::SCOPED_CALL]);
        assert_eq!(
            found[0].message,
            "`JSON::parse` was removed; use `JSON.parse`; `::` names only constants, nested types and enum members"
        );
        assert_eq!(&source[found[0].span.start..found[0].span.end], "::");
        assert_eq!(fixed(source, &found[0]), "x = JSON.parse(\"1\")\n");

        let module =
            "module Pricing\n  def self.with_tax(cents: int) -> int\n    cents + 1\n  end\nend\n";
        for (call, canonical) in [
            ("Pricing::with_tax(1)", "Pricing.with_tax(1)"),
            ("Time::now", "Time.now"),
            ("Math::sqrt(4.0)", "Math.sqrt(4.0)"),
        ] {
            let source = format!("{module}x = {call}\n");
            let found = codes(&source, &[Code::SCOPED_CALL]);
            assert_eq!(
                fixed(&source, &found[0]),
                format!("{module}x = {canonical}\n")
            );
            clean(&format!("{module}x = {canonical}\n"));
        }
    }

    #[test]
    fn constants_nested_types_and_enum_members_keep_two_colons() {
        clean(
            "class Outer\n  LIMIT = 3\n  class Inner\n  end\nend\nenum Status\n  Draft\nend\nlimit = Outer::LIMIT\nstatus = Status::Draft\npi = Math::PI\ninner = Outer::Inner.new\n",
        );
    }

    #[test]
    fn a_scoped_call_still_runs_until_the_switchover() {
        let value = Engine::new()
            .compile("def run -> any\n  JSON::parse(\"[1]\")\nend\n")
            .unwrap()
            .call("run", &[], CallOptions::default())
            .unwrap()
            .value;
        assert_eq!(value.to_string(), "[1]");
    }
}
