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

mod visibility {
    use super::*;

    const CLASS: &str = "class Account
  property balance: int

  def initialize(@balance: int)
  end

  def total -> int
    fee + self.rate + bonus(self)
  end

  def bonus(other: Account) -> int
    other.rate
  end

  def self.make -> Account
    Account.new(audit)
  end

  private def fee -> int
    1
  end

  protected def rate -> int
    2
  end

  private

  def self.audit -> int
    3
  end
end
";

    /// Whether `body`, run after [`CLASS`] without static types, raises
    /// the runtime's visibility error.
    fn hidden_at_runtime(body: &str) -> bool {
        let source = format!("{CLASS}def run -> any\n  {body}\nend\n");
        match Engine::new()
            .compile(&source)
            .unwrap()
            .call("run", &[], CallOptions::default())
        {
            Ok(_) => false,
            Err(error) => {
                assert!(
                    error.message.starts_with("private method")
                        || error.message.starts_with("protected method"),
                    "{body}: {error}"
                );
                true
            }
        }
    }

    #[test]
    fn calls_the_runtime_allows_check_clean() {
        let source = format!("{CLASS}def run -> int\n  Account.make.total\nend\n");
        clean(&source);
        assert_eq!(run(&source).unwrap().to_string(), "5");
        assert!(!hidden_at_runtime("Account.make.total"));
    }

    #[test]
    fn calls_the_runtime_refuses_are_compile_errors() {
        for (body, message) in [
            (
                "Account.new(1).fee",
                "`fee` is private in `Account`: only `Account`'s own methods can call it, without a receiver",
            ),
            (
                "Account.new(1).rate",
                "`rate` is protected in `Account`: only `Account`'s instance methods can call it, on an instance of `Account`",
            ),
            (
                "Account.audit",
                "`audit` is private in `Account`: only `Account`'s own methods can call it, without a receiver",
            ),
        ] {
            let source = format!("{CLASS}def run -> int\n  {body}\nend\n");
            let found = codes(&source, &[Code::VISIBILITY]);
            assert_eq!(found[0].message, message);
            let label = found[0].labels[0].span;
            assert!(source[label.start..].starts_with("def "), "{body}");
            assert!(hidden_at_runtime(body), "{body}");
        }
    }

    #[test]
    fn a_receiver_makes_even_self_explicit() {
        let source = "class Box\n  def run -> int\n    self.secret\n  end\n\n  private def secret -> int\n    1\n  end\nend\n";
        codes(source, &[Code::VISIBILITY]);
        let error = Engine::new()
            .compile(&format!("{source}def run -> int\n  Box.new.run\nend\n"))
            .unwrap()
            .call("run", &[], CallOptions::default())
            .unwrap_err();
        assert_eq!(error.message, "private method secret");
    }

    #[test]
    fn protected_methods_follow_the_callers_kind() {
        // A class method may not call an instance's protected method.
        let source = "class Box\n  def self.peek(other: Box) -> int\n    other.secret\n  end\n\n  protected def secret -> int\n    1\n  end\nend\n";
        codes(source, &[Code::VISIBILITY]);
        // A protected class method answers the class's own class methods.
        clean(
            "class Box\n  def self.run -> int\n    Box.secret\n  end\n\n  protected\n\n  def self.secret -> int\n    1\n  end\nend\n",
        );
        codes(
            "class Box\n  protected\n\n  def self.secret -> int\n    1\n  end\nend\nn = Box.secret\n",
            &[Code::VISIBILITY],
        );
    }

    #[test]
    fn operators_and_setters_follow_visibility() {
        let source = "class Coin\n  getter cents: int\n\n  def initialize(@cents: int)\n  end\n\n  private def +(other: Coin) -> Coin\n    Coin.new(@cents + other.cents)\n  end\n\n  private def cents=(value: int)\n    @cents = value\n  end\nend\na = Coin.new(1)\nb = a + a\na.cents = 3\n";
        let found = codes(source, &[Code::VISIBILITY, Code::VISIBILITY]);
        assert!(found[0].message.starts_with("`+` is private"));
        assert!(found[1].message.starts_with("`cents=` is private"));
    }
}
