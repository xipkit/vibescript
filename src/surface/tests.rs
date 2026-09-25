//! One positive and one negative case per code, and a round trip through
//! each fix: the fixed source compiles without that diagnostic.

use super::check;
use crate::{
    Engine,
    diagnostic::{Applicability, Code, Diagnostic},
};

fn diagnostics(source: &str) -> Vec<Diagnostic> {
    check(source).unwrap_or_else(|error| panic!("{source:?}: {error}"))
}

fn with_code(source: &str, code: Code) -> Vec<Diagnostic> {
    diagnostics(source)
        .into_iter()
        .filter(|diagnostic| diagnostic.code == code)
        .collect()
}

/// The source after the first diagnostic of `code`'s fix, which must be
/// machine-applicable.
fn fixed(source: &str, code: Code) -> String {
    let found = with_code(source, code);
    let diagnostic = found
        .first()
        .unwrap_or_else(|| panic!("no {code} in {source:?}: {:?}", diagnostics(source)));
    let fix = diagnostic
        .applicable_fix()
        .unwrap_or_else(|| panic!("no fix for {diagnostic:?}"));
    fix.apply(source).expect("the fix applies")
}

/// Asserts that `source` has `code` at `at`, that its fix gives `expected`,
/// and that `expected` compiles with static types without that code.
fn round_trip(source: &str, code: Code, at: &str, expected: &str) {
    let found = with_code(source, code);
    assert_eq!(found.len(), 1, "{source:?}: {found:?}");
    let offset = source.find(at).expect("the diagnostic's text");
    assert_eq!(found[0].span.start, offset, "{found:?}");
    assert_eq!(found[0].span.end, offset + at.len(), "{found:?}");
    assert!(found[0].is_error());
    assert_eq!(fixed(source, code), expected);
    assert_clean(expected, code);
}

/// Asserts that `source` compiles with static types, and has no `code`.
fn assert_clean(source: &str, code: Code) {
    assert!(with_code(source, code).is_empty(), "{source:?}");
    let mut engine = Engine::new();
    engine.set_static_types(true);
    if let Err(error) = engine.compile(source) {
        assert!(
            error.diagnostics().iter().all(|d| d.code != code),
            "{source:?}: {error}"
        );
        assert!(!error.diagnostics().is_empty(), "{source:?}: {error}");
    }
}

#[test]
fn removed_names_are_renamed() {
    round_trip(
        "items = [1, 2]\nn = [1, 2].size\n",
        Code::REMOVED_NAME,
        "size",
        "items = [1, 2]\nn = [1, 2].length\n",
    );
    round_trip(
        "def label(name: string) -> int\n  name.size\nend\n",
        Code::REMOVED_NAME,
        "size",
        "def label(name: string) -> int\n  name.length\nend\n",
    );
    round_trip(
        "text = sprintf(\"%d\", 1)\n",
        Code::REMOVED_NAME,
        "sprintf",
        "text = format(\"%d\", 1)\n",
    );
    round_trip(
        "t = Time.gm(2024, 1, 2)\n",
        Code::REMOVED_NAME,
        "gm",
        "t = Time.utc(2024, 1, 2)\n",
    );
    let message = &with_code("n = [1].size\n", Code::REMOVED_NAME)[0].message;
    assert_eq!(message, "`size` was removed; use `length`");
}

#[test]
fn canonical_names_and_user_methods_are_not_removed_names() {
    assert!(with_code("n = [1, 2].length\n", Code::REMOVED_NAME).is_empty());
    let source = "class Box\n  def size\n    3\n  end\nend\nn = Box.new.size\n";
    assert!(with_code(source, Code::REMOVED_NAME).is_empty());
    // A receiver of unknown type could be a time, whose `day` is canonical.
    assert!(with_code("def f(t)\n  t.day\nend\n", Code::REMOVED_NAME).is_empty());
}

#[test]
fn removed_names_without_a_safe_rewrite_have_no_fix() {
    let found = with_code("x = [1].reduce(:+)\n", Code::REMOVED_NAME);
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].fixes.is_empty());
    assert!(
        found[0]
            .message
            .starts_with("`reduce` was removed; pass a block")
    );
    // `%` refuses the float operand `modulo` takes.
    let found = with_code("x = 7.5.modulo(2)\n", Code::REMOVED_NAME);
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].fixes.is_empty());
}

#[test]
fn nil_predicates_become_comparisons() {
    round_trip(
        "x = nil\ny = x.nil?\n",
        Code::NIL_PREDICATE,
        "nil?",
        "x = nil\ny = x == nil\n",
    );
    round_trip(
        "x = nil\ny = !x.nil?\n",
        Code::NIL_PREDICATE,
        "nil?",
        "x = nil\ny = !(x == nil)\n",
    );
    assert!(with_code("x = nil\ny = x == nil\n", Code::NIL_PREDICATE).is_empty());
}

#[test]
fn identity_equality_offers_a_suggestion_only() {
    let source = "a = 1\nb = 1\nsame = a.eql?(b)\n";
    let found = with_code(source, Code::IDENTITY_EQUALITY);
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].span.start, source.find("eql?").unwrap());
    assert!(found[0].applicable_fix().is_none());
    let fix = &found[0].fixes[0];
    assert_eq!(fix.applicability, Applicability::Suggestion);
    let suggested = fix.apply(source).unwrap();
    assert_eq!(suggested, "a = 1\nb = 1\nsame = a == b\n");
    assert_clean(&suggested, Code::IDENTITY_EQUALITY);
    assert!(with_code("a = 1\nsame = a == 1\n", Code::IDENTITY_EQUALITY).is_empty());
}

#[test]
fn identity_calls_become_the_expression() {
    round_trip(
        "x = 1\ny = x.itself\n",
        Code::IDENTITY_CALL,
        "itself",
        "x = 1\ny = x\n",
    );
    let found = with_code("x = 1\ny = x.tap { |v| v }\n", Code::IDENTITY_CALL);
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].fixes.is_empty());
    assert!(with_code("x = 1\ny = x\n", Code::IDENTITY_CALL).is_empty());
}

#[test]
fn dispatch_by_a_literal_name_becomes_a_direct_call() {
    round_trip(
        "items = [1]\nn = items.send(:first, 1)\n",
        Code::DISPATCH_BY_NAME,
        "send",
        "items = [1]\nn = items.first(1)\n",
    );
    let found = with_code(
        "items = [1]\nname = \"first\"\nn = items.send(name)\n",
        Code::DISPATCH_BY_NAME,
    );
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].fixes.is_empty());
    let found = with_code(
        "items = [1]\nok = items.respond_to?(:first)\n",
        Code::DISPATCH_BY_NAME,
    );
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(with_code("items = [1]\nn = items.first(1)\n", Code::DISPATCH_BY_NAME).is_empty());
    // A capability's own `send` is the host's.
    assert!(with_code("mailer.send(:welcome)\n", Code::DISPATCH_BY_NAME).is_empty());
}

#[test]
fn do_blocks_become_braces() {
    round_trip(
        "total = 0\n[1, 2].each do |x|\n  total += x\nend\n",
        Code::DO_BLOCK,
        "do",
        "total = 0\n[1, 2].each { |x|\n  total += x\n}\n",
    );
    round_trip(
        "puts [1].map do |x| x end\n",
        Code::DO_BLOCK,
        "do",
        "puts([1].map) { |x| x }\n",
    );
    // A `do` on the next line moves up to its call.
    round_trip(
        "def zero\n  yield\nend\nzero\ndo\n  7\nend\n",
        Code::DO_BLOCK,
        "do",
        "def zero\n  yield\nend\nzero {\n  7\n}\n",
    );
    assert!(with_code("[1, 2].each { |x| puts x }\n", Code::DO_BLOCK).is_empty());
}

#[test]
fn unless_becomes_a_negated_if() {
    round_trip(
        "x = 1\nputs x unless x == 2\n",
        Code::UNLESS,
        "unless",
        "x = 1\nputs x if x != 2\n",
    );
    round_trip(
        "x = true\nunless x\n  puts 1\nend\n",
        Code::UNLESS,
        "unless",
        "x = true\nif !x\n  puts 1\nend\n",
    );
    round_trip(
        "a = true\nb = false\nputs 1 unless a && b\n",
        Code::UNLESS,
        "unless",
        "a = true\nb = false\nputs 1 if !(a && b)\n",
    );
    assert!(with_code("x = 1\nputs x if x != 2\n", Code::UNLESS).is_empty());
}

#[test]
fn until_becomes_a_negated_while() {
    round_trip(
        "i = 0\nuntil i == 3\n  i += 1\nend\n",
        Code::UNTIL,
        "until",
        "i = 0\nwhile i != 3\n  i += 1\nend\n",
    );
    assert!(with_code("i = 0\nwhile i < 3\n  i += 1\nend\n", Code::UNTIL).is_empty());
}

#[test]
fn symbol_keys_become_strings() {
    round_trip(
        "h = { a: 1 }\nx = h[:a]\n",
        Code::SYMBOL_KEY,
        ":a",
        "h = { a: 1 }\nx = h[\"a\"]\n",
    );
    round_trip(
        "h = { a: 1 }\nh[:b] = 2\n",
        Code::SYMBOL_KEY,
        ":b",
        "h = { a: 1 }\nh[\"b\"] = 2\n",
    );
    assert!(with_code("h = { a: 1 }\nx = h[\"a\"]\n", Code::SYMBOL_KEY).is_empty());
}

#[test]
fn percent_literals_become_arrays() {
    round_trip(
        "names = %w[ada grace]\n",
        Code::PERCENT_LITERAL,
        "%w[ada grace]",
        "names = [\"ada\", \"grace\"]\n",
    );
    round_trip(
        "names = %i[ada grace]\n",
        Code::PERCENT_LITERAL,
        "%i[ada grace]",
        "names = [:ada, :grace]\n",
    );
    // After a local, a percent literal is a command argument, and so is
    // the array in parentheses.
    round_trip(
        "[1].map { it %w[a b] }\n",
        Code::PERCENT_LITERAL,
        "%w[a b]",
        "[1].map { it ([\"a\", \"b\"]) }\n",
    );
    assert!(with_code("names = [\"ada\"]\n", Code::PERCENT_LITERAL).is_empty());
}

#[test]
fn hash_new_becomes_a_literal() {
    round_trip(
        "counts = Hash.new\n",
        Code::HASH_NEW,
        "Hash.new",
        "counts = {}\n",
    );
    let found = with_code("counts = Hash.new(0)\n", Code::HASH_NEW);
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].fixes.is_empty());
    assert!(with_code("counts = {}\n", Code::HASH_NEW).is_empty());
}

#[test]
fn empty_parentheses_are_dropped() {
    round_trip(
        "id = uuid()\n",
        Code::EMPTY_PARENTHESES,
        "()",
        "id = uuid\n",
    );
    round_trip(
        "n = [1].length()\n",
        Code::EMPTY_PARENTHESES,
        "()",
        "n = [1].length\n",
    );
    round_trip(
        "def zero -> int\n  0\nend\nn = zero()\n",
        Code::EMPTY_PARENTHESES,
        "()",
        "def zero -> int\n  0\nend\nn = zero\n",
    );
    assert!(with_code("id = uuid\n", Code::EMPTY_PARENTHESES).is_empty());
    // A function that takes arguments is called with them.
    let source = "def f(a: int = 1) -> int\n  a\nend\nn = f()\n";
    assert!(with_code(source, Code::EMPTY_PARENTHESES).is_empty());
}

#[test]
fn type_names_are_lowercase() {
    round_trip(
        "def f(x: Int) -> int\n  x\nend\n",
        Code::TYPE_NAME,
        "Int",
        "def f(x: int) -> int\n  x\nend\n",
    );
    round_trip(
        "def f(x: object) -> int\n  1\nend\n",
        Code::TYPE_NAME,
        "object",
        "def f(x: hash) -> int\n  1\nend\n",
    );
    assert!(with_code("def f(x: int) -> int\n  x\nend\n", Code::TYPE_NAME).is_empty());
}

#[test]
fn every_surface_code_is_registered_with_a_test() {
    let surface: Vec<Code> = crate::diagnostic::codes()
        .iter()
        .map(|info| info.code)
        .filter(|code| code.area() == Some(crate::diagnostic::Area::Surface))
        .collect();
    assert_eq!(surface.len(), 13);
    assert_eq!(surface.first(), Some(&Code::REMOVED_NAME));
    assert_eq!(surface.last(), Some(&Code::TYPE_NAME));
}

#[test]
fn static_compilation_reports_removed_spellings() {
    let source = "names = %w[a b]\nn = names.size\n";
    let mut engine = Engine::new();
    assert!(engine.compile(source).is_ok());
    engine.set_static_types(true);
    let error = engine.compile(source).err().expect("a compile error");
    let codes: Vec<String> = error
        .diagnostics()
        .iter()
        .map(|d| d.code.to_string())
        .collect();
    assert_eq!(codes, ["V0410", "V0401"]);
    assert_eq!(error.kind, crate::ErrorKind::Type);
    let checked = engine.type_check(source).unwrap();
    assert_eq!(checked.diagnostics, error.diagnostics());
}

#[test]
fn nesting_deeper_than_the_limit_is_still_checked() {
    let depth = 400;
    let source = format!("x = {}[1].size{}\n", "[".repeat(depth), "]".repeat(depth));
    let found = with_code(&source, Code::REMOVED_NAME);
    assert_eq!(found.len(), 1, "{found:?}");
}
