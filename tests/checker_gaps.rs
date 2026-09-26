//! Gaps between the static checker, its fixes and the runtime that migrating
//! the documentation found, each with programs the checker accepts, programs
//! it rejects, and what the runtime does with them.

mod common;

use vibescript::{
    CallOptions, Engine, ModuleConfig, Value,
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

/// An engine without static types, for what the runtime does with a
/// program the checker rejects.
fn unchecked() -> Engine {
    let mut engine = Engine::new();
    engine.set_static_types(false);
    engine
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
        // An enum member may be lowercase, so only a namespace's name
        // without arguments is a call.
        clean("enum Kind\n  enum\n  Other\nend\nkind = Kind::enum\n");
        clean("enum _state\n  aBC\nend\nname = _state::aBC.name\n");
        codes(
            "enum Kind\n  Other\nend\nkind = Kind::missing\n",
            &[Code::UNKNOWN_ENUM_MEMBER],
        );
        let source = "module Box\n  def self.size -> int\n    1\n  end\nend\nn = Box::size\n";
        let found = codes(source, &[Code::SCOPED_CALL]);
        assert!(fixed(source, &found[0]).ends_with("n = Box.size\n"));
    }

    #[test]
    fn a_scoped_call_still_runs_until_the_switchover() {
        let value = unchecked()
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
        match unchecked()
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
        let error = unchecked()
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
        // Without a `!=` of its own, `!=` calls `==`.
        let source = "class Coin\n  private def ==(other: any) -> bool\n    true\n  end\nend\nsame = Coin.new != 1\n";
        let found = codes(source, &[Code::VISIBILITY]);
        assert!(found[0].message.starts_with("`==` is private"));
        let span = found[0].span;
        assert_eq!(&source[span.start..span.end], "!=");
        let error = unchecked()
            .compile(source)
            .unwrap()
            .run(CallOptions::default())
            .unwrap_err();
        assert_eq!(error.message, "private method ==");
    }
}

mod break_values {
    use super::*;

    /// The type the checker reports for `value` when it is assigned where
    /// `array<bool>` is expected, which no tested value is.
    #[track_caller]
    fn type_of(prelude: &str, value: &str) -> String {
        let source = format!("{prelude}probe: array<bool> = {value}\n");
        let found = codes(&source, &[Code::TYPE_MISMATCH]);
        found[0].found.clone().unwrap()
    }

    #[test]
    fn a_builtin_iterators_type_includes_its_break_values() {
        assert_eq!(
            type_of("", "[1, 2].each { |n| break \"s\" }"),
            "array<int> | string"
        );
        assert_eq!(
            type_of("", "[1, 2].map { |n|\n  break if n > 1\n  n\n}"),
            "array<int>?"
        );
        assert_eq!(
            type_of(
                "",
                "{ a: 1 }.each { |key, value| break value if value > 0 }"
            ),
            "int | { a: int }"
        );
        // A break inside a loop in the block leaves only the loop.
        assert_eq!(
            type_of("", "[1].each { |n| while true\n  break \"s\"\nend }"),
            "array<int>"
        );
    }

    #[test]
    fn a_constructors_type_includes_its_break_values() {
        let class = "class C\n  def initialize(&block: int)\n    yield 1\n  end\nend\n";
        assert_eq!(type_of(class, "C.new { |n| break 7 }"), "C | int");
        assert_eq!(type_of(class, "C.new { |n| n }"), "C");
        let source = format!("{class}def run -> C | int\n  C.new {{ |n| break 7 }}\nend\n");
        clean(&source);
        assert_eq!(run(&source).unwrap().to_string(), "7");
    }

    #[test]
    fn loop_has_the_type_of_its_break_values() {
        assert_eq!(type_of("", "loop { break }"), "nil");
        assert_eq!(
            type_of("i = 0\n", "loop {\n  i += 1\n  break i if i > 3\n}"),
            "int"
        );
        assert_eq!(
            type_of(
                "i = 0\n",
                "loop {\n  i += 1\n  break \"s\" if i > 3\n  break if i > 9\n}"
            ),
            "string?"
        );
        // A loop that only returns has no value.
        let source = "def run -> int\n  loop { return 5 }\nend\n";
        clean(source);
        assert_eq!(run(source).unwrap().to_string(), "5");
    }

    #[test]
    fn a_yielding_functions_break_values_join_its_result() {
        // Without a declared result, a break value is the call's value.
        let each = "def each_one(&block: int)\n  yield 1\nend\n";
        assert_eq!(type_of(each, "each_one { |n| break \"x\" }"), "string?");
        let source = format!("{each}def run -> string?\n  each_one {{ |n| break \"x\" }}\nend\n");
        clean(&source);
        assert_eq!(run(&source).unwrap().to_string(), "x");
    }

    #[test]
    fn a_break_out_of_a_function_with_a_result_must_fit_it() {
        // The runtime checks a break value against the function's result.
        let twice = "def twice(&block: int -> int) -> int\n  yield(1) + yield(2)\nend\n";
        let found = codes(
            &format!("{twice}z = twice {{ |n| break \"early\" }}\n"),
            &[Code::TYPE_MISMATCH],
        );
        assert_eq!(
            found[0].message,
            "a `break` out of the block returns from `twice`, which returns int, found string"
        );
        codes(
            &format!("{twice}z = twice {{ |n| break }}\n"),
            &[Code::TYPE_MISMATCH],
        );
        let error = unchecked()
            .compile(&format!(
                "{twice}def run -> int\n  twice {{ |n| break \"early\" }}\nend\n"
            ))
            .unwrap()
            .call("run", &[], CallOptions::default())
            .unwrap_err();
        assert_eq!(
            error.message,
            "return value for twice expected int, got string"
        );
        let source = format!("{twice}def run -> int\n  twice {{ |n| break 9 }}\nend\n");
        clean(&source);
        assert_eq!(run(&source).unwrap().to_string(), "9");
        assert_eq!(type_of(twice, "twice { |n| break 9 }"), "int");
    }

    #[test]
    fn a_break_out_of_a_nested_yield_ends_the_inner_loop_or_call() {
        // A function that yields inside a loop or a block keeps its own
        // result: the break ends that loop or call.
        let source = "def zero(&block: () -> any) -> any
  yield
end

def pairs(&block: () -> any) -> array<any>
  first = zero { yield }
  [first, 99]
end

def run -> array<any>
  pairs { break 7 }
end
";
        clean(source);
        assert_eq!(run(source).unwrap().to_string(), "[7, 99]");
        let looping = "def upto(&block: int -> any) -> int
  i = 0
  while i < 3
    yield i
    i += 1
  end
  i
end
";
        assert_eq!(type_of(looping, "upto { |n| break \"s\" }"), "int");
        let source = format!("{looping}def run -> int\n  upto {{ |n| break \"s\" }}\nend\n");
        clean(&source);
        assert_eq!(run(&source).unwrap().to_string(), "0");
        // A function that never yields never sees a break.
        let source = "def given(&block?: int) -> bool\n  block_given?\nend\ndef run -> bool\n  given { |v| break 7 }\nend\n";
        clean(source);
        assert_eq!(run(source).unwrap().to_string(), "true");
    }

    #[test]
    fn a_builtin_iterators_break_value_is_what_runs() {
        let source = "def run -> array<int> | string\n  [1, 2].each { |n| break \"s\" }\nend\n";
        clean(source);
        assert_eq!(run(source).unwrap().to_string(), "s");
    }
}

mod nested_writes {
    use super::*;
    use vibescript::{ErrorClass, ErrorKind};

    #[test]
    fn a_write_through_an_element_reads_it_as_present() {
        clean(
            "grid = [[1, 2], [3, 4]]
grid[0][1] = 9
grid[1] << 5
grid[1].push(6)
last = grid[0].pop
deep = [[[1]]]
deep[0][0][0] = 7
h: hash<string, hash<string, int>> = { a: { x: 1 } }
h[\"a\"][\"y\"] = 2
h[\"a\"].delete(\"x\")
lists: hash<string, array<int>> = { a: [1] }
lists[\"a\"] << 2
lists[\"b\"]&.push(3)
",
        );
    }

    #[test]
    fn a_read_stays_optional_and_keeps_its_fetch_fix() {
        let source = "grid = [[1, 2]]\nn = grid[0][1]\n";
        let found = codes(source, &[Code::OPTIONAL_USE]);
        assert_eq!(
            fixed(source, &found[0]),
            "grid = [[1, 2]]\nn = grid.fetch(0)[1]\n"
        );
        // The element a compound assignment reads may still be missing.
        let source = "grid = [[1, 2]]\ngrid[0][1] += 1\n";
        let found = codes(source, &[Code::OPTIONAL_USE]);
        assert_eq!(
            fixed(source, &found[0]),
            "grid = [[1, 2]]\ngrid[0][1] = grid[0].fetch(1) + 1\n"
        );
    }

    #[test]
    fn no_fix_rewrites_a_read_a_write_goes_through() {
        for source in [
            // An element that may hold nil must be tested first.
            "rows: array<array<int>?> = [[1]]\nrows[0][0] = 2\n",
            // A range reads a copy, which the write would change.
            "grid = [[1, 2]]\ngrid[0..1][0] = [3]\n",
        ] {
            let found = codes(source, &[Code::OPTIONAL_USE]);
            assert!(found[0].fixes.is_empty(), "{source}: {:?}", found[0].fixes);
        }
        let source = "def rows -> array<array<int>>?\n  nil\nend\nrows[0] << 1\n";
        let found = codes(source, &[Code::OPTIONAL_USE]);
        assert!(found[0].fixes.is_empty());
    }

    #[test]
    fn a_write_through_a_present_element_updates_it_in_place() {
        let source = "def run -> array<array<int>>
  grid = [[1, 2], [3]]
  grid[0][1] = 9
  grid[1] << 5
  grid[1].push(6)
  grid
end
";
        clean(source);
        assert_eq!(run(source).unwrap().to_string(), "[[1, 9], [3, 5, 6]]");
    }

    /// The error of running `body` in a function with static types.
    fn failure(body: &str) -> vibescript::Error {
        run(&format!("def run -> any\n{body}\n  nil\nend\n")).unwrap_err()
    }

    #[test]
    fn a_write_through_a_missing_element_raises() {
        for (body, message, column) in [
            (
                "  grid = [[1, 2]]\n  grid[5][1] = 9",
                "cannot write through a missing element: array index 5 outside of array bounds: -1...1",
                10,
            ),
            (
                "  grid = [[1, 2]]\n  i = -2\n  grid[i][0] = 1",
                "cannot write through a missing element: array index -2 outside of array bounds: -1...1",
                10,
            ),
            (
                "  h: hash<string, hash<string, int>> = { a: { x: 1 } }\n  h[\"b\"][\"x\"] = 9",
                "cannot write through a missing element: hash key not found: \"b\"",
                9,
            ),
            (
                "  grid = [[1, 2]]\n  grid[3] << 9",
                "cannot write through a missing element: array index 3 outside of array bounds: -1...1",
                12,
            ),
            (
                "  lists: hash<string, array<int>> = { a: [1] }\n  lists[\"b\"].push(9)",
                "cannot write through a missing element: hash key not found: \"b\"",
                8,
            ),
            (
                "  deep = [[[1]]]\n  deep[0][4][0] = 7",
                "cannot write through a missing element: array index 4 outside of array bounds: -1...1",
                13,
            ),
        ] {
            let error = failure(body);
            assert_eq!(error.message, message, "{body}");
            assert_eq!(error.kind, ErrorKind::Argument, "{body}");
            // The class and kind `fetch` raises with.
            assert_eq!(error.class(), Some(ErrorClass::Runtime), "{body}");
            // Where the write fails: at its target's index, its receiver's
            // or its operator.
            let position = &error.diagnostic.as_ref().unwrap().position;
            let line = body.lines().count() + 1;
            assert_eq!((position.line, position.column), (line, column), "{body}");
        }
    }

    #[test]
    fn a_missing_element_is_rescued_like_fetch() {
        let source = "def run -> string
  grid = [[1]]
  begin
    grid[2][0] = 1
    \"written\"
  rescue RuntimeError => error
    error.message
  end
end
";
        clean(source);
        assert_eq!(
            run(source).unwrap().to_string(),
            "cannot write through a missing element: array index 2 outside of array bounds: -1...1"
        );
    }
}

mod parse_as_enums {
    use super::*;

    const STATUS: &str =
        "enum Status\n  Draft\n  InReview\nend\n\ntype Review = { status: Status, score: int }\n";

    /// The value of `body`, run after [`STATUS`] with static types.
    fn parse(body: &str) -> Result<Value, vibescript::Error> {
        run(&format!("{STATUS}def run -> any\n  {body}\nend\n"))
    }

    #[test]
    fn a_json_string_names_a_member_by_its_symbol() {
        assert_eq!(
            parse("JSON.parse_as(\"\\\"in_review\\\"\", Status)")
                .unwrap()
                .to_string(),
            "Status::InReview"
        );
        assert_eq!(
            parse("JSON.parse_as(\"{\\\"status\\\":\\\"draft\\\",\\\"score\\\":3}\", Review)")
                .unwrap()
                .to_string(),
            "{status: Status::Draft, score: 3}"
        );
        // What JSON.stringify writes reads back as the member.
        assert_eq!(
            parse("JSON.parse_as(JSON.stringify(Status::InReview), Status)")
                .unwrap()
                .to_string(),
            "Status::InReview"
        );
    }

    #[test]
    fn anything_else_is_the_typed_boundary_error() {
        for (body, message) in [
            (
                "JSON.parse_as(\"\\\"InReview\\\"\", Status)",
                "JSON.parse_as value expected Status, got string",
            ),
            (
                "JSON.parse_as(\"1\", Status)",
                "JSON.parse_as value expected Status, got int",
            ),
            (
                "JSON.parse_as(\"{\\\"status\\\":\\\"gone\\\",\\\"score\\\":3}\", Review)",
                "JSON.parse_as value expected { score: int, status: Status }, got { score: int, status: string }",
            ),
        ] {
            let error = parse(body).unwrap_err();
            assert_eq!(error.message, message, "{body}");
            assert_eq!(error.kind, vibescript::ErrorKind::Type, "{body}");
        }
    }

    #[test]
    fn the_result_has_the_enums_type() {
        let source = format!(
            "{STATUS}def label(raw: string) -> string\n  case JSON.parse_as(raw, Status)\n  when Status::Draft then \"draft\"\n  when Status::InReview then \"in review\"\n  end\nend\n\ndef score(raw: string) -> int\n  JSON.parse_as(raw, Review)[\"score\"]\nend\n"
        );
        clean(&source);
        let found = codes(
            &format!("{STATUS}n: int = JSON.parse_as(\"1\", Status)\n"),
            &[Code::TYPE_MISMATCH],
        );
        assert_eq!(found[0].found.as_deref(), Some("Status"));
    }

    #[test]
    fn a_value_where_a_type_is_expected_is_an_error() {
        // Braces in the call that name an enum are a shape type.
        clean(&format!(
            "{STATUS}review = JSON.parse_as(\"{{}}\", {{ status: Status? }})\n"
        ));
        // Braces holding a member are a hash, which the runtime refuses.
        let source =
            format!("{STATUS}review = JSON.parse_as(\"{{}}\", {{ status: Status::Draft }})\n");
        let found = codes(&source, &[Code::TYPE_MISMATCH]);
        assert!(
            found[0].message.starts_with(
                "argument 2 (`schema`) of `parse_as` is a type, found { status: Status }; braces make a type only where every field names one"
            ),
            "{}",
            found[0].message
        );
        let error = unchecked()
            .compile(&format!(
                "{STATUS}def run -> any\n  JSON.parse_as(\"{{}}\", {{ status: Status::Draft }})\nend\n"
            ))
            .unwrap()
            .call("run", &[], CallOptions::default())
            .unwrap_err();
        assert_eq!(
            error.message,
            "JSON.parse_as expects a type literal as its second argument"
        );
        codes(
            &format!("{STATUS}n = JSON.parse_as(\"1\", 5)\n"),
            &[Code::TYPE_MISMATCH],
        );
        // Empty braces are an empty hash, and name nothing.
        let found = codes("h = JSON.parse_as(\"{}\", {})\n", &[Code::TYPE_MISMATCH]);
        assert_eq!(
            found[0].message,
            "argument 2 (`schema`) of `parse_as` is a type, found {}"
        );
    }

    #[test]
    fn a_class_names_its_type_but_json_never_holds_one() {
        let source = "class Box\nend\ndef run -> Box\n  JSON.parse_as(\"{}\", Box)\nend\n";
        clean(source);
        assert_eq!(
            run(source).unwrap_err().message,
            "JSON.parse_as value expected Box, got {}"
        );
    }
}

mod required_types {
    use super::*;

    const STATES: &str = "enum State
  Open
  Closed
end

class Door
  getter state: State

  def initialize(@state: State)
  end

  def open? -> bool
    @state == State::Open
  end

  private def hinge -> int
    1
  end
end

def closed -> State
  State::Closed
end

def door -> Door
  Door.new(:open)
end
";

    /// An engine whose module path, a directory for `test`, holds
    /// `states.vibe`, with static types on, and the directory to remove.
    fn engine(test: &str) -> (Engine, std::path::PathBuf) {
        // WASI has no temporary directory, so fixtures live under the repository.
        let directory = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join(".cache/tmp")
            .join(format!("checker-gaps-{test}-{}", common::process_id()));
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join("states.vibe"), STATES).unwrap();
        let mut engine = Engine::new();
        engine
            .set_module_config(ModuleConfig {
                paths: vec![directory.clone()],
                ..ModuleConfig::default()
            })
            .unwrap();
        engine.set_static_types(true);
        (engine, directory)
    }

    #[test]
    fn a_required_files_enums_and_instances_are_typed() {
        let (engine, directory) = engine("typed");
        let source = "def run -> array<any>
  states = require(\"states\")
  require(\"states\", as: \"k\")
  shut: State = closed
  same = shut == State::Closed
  from_exports = states.State::Open == k.State::Open
  entry = door
  label = case entry.state
          when State::Open then \"open\"
          when State::Closed then \"closed\"
          end
  [same, from_exports, entry.open?, label, pick(:closed)]
end

def pick(state: State) -> bool
  state == State::Closed
end
";
        codes_with(&engine, source, &[]);
        let value = engine
            .compile(source)
            .unwrap()
            .call("run", &[], CallOptions::default())
            .unwrap()
            .value;
        assert_eq!(value.to_string(), "[true, true, true, open, true]");
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn their_types_are_checked() {
        let (engine, directory) = engine("checked");
        let prelude = "require(\"states\")\n";
        for (line, code) in [
            ("n: int = closed", Code::TYPE_MISMATCH),
            ("x = door.state.length", Code::UNKNOWN_MEMBER),
            ("x = State::Ajar", Code::UNKNOWN_ENUM_MEMBER),
            ("x = door.hinge", Code::VISIBILITY),
            // A class stays private to its file, as it does at runtime.
            ("x = Door.new(:open)", Code::UNDEFINED_NAME),
        ] {
            let found = codes_with(&engine, &format!("{prelude}{line}\n"), &[code]);
            if code == Code::TYPE_MISMATCH {
                assert_eq!(found[0].found.as_deref(), Some("State"));
            }
        }
        codes_with(
            &engine,
            &format!("{prelude}def open(d: Door) -> bool\n  d.open?\nend\n"),
            &[Code::UNKNOWN_TYPE],
        );
        let mut plain = unchecked();
        plain
            .set_module_config(ModuleConfig {
                paths: vec![directory.clone()],
                ..ModuleConfig::default()
            })
            .unwrap();
        let error = plain
            .compile("def run -> any\n  require(\"states\")\n  Door.new(:open)\nend\n")
            .unwrap()
            .call("run", &[], CallOptions::default())
            .unwrap_err();
        assert_eq!(error.message, "undefined variable Door");
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn a_local_declaration_keeps_its_name() {
        let (engine, directory) = engine("local");
        // The requiring script's own enum wins, as the runtime binds it.
        let source = "enum State\n  Draft\nend\nrequire(\"states\")\nmine = State::Draft\nshut: State = closed\n";
        let found = codes_with(&engine, source, &[Code::TYPE_MISMATCH]);
        assert_eq!(found[0].expected.as_deref(), Some("State"));
        std::fs::remove_dir_all(directory).unwrap();
    }
}
