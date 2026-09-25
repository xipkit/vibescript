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
fn class_variable_declarations_are_walked() {
    round_trip(
        "class C\n  @@n: int = [1].size\nend\n",
        Code::REMOVED_NAME,
        "size",
        "class C\n  @@n: int = [1].length\nend\n",
    );
}

#[test]
fn the_static_checkers_receiver_types_decide_typed_renames() {
    let source = "h = { a: 1 }\nok = h.include?(\"a\")\nt = Time.now\nd = t.day\ne = 5.day\n";
    let checked = Engine::new().type_check(source).unwrap();
    let found: Vec<(&str, Option<String>)> = checked
        .diagnostics
        .iter()
        .map(|d| {
            let fix = d.applicable_fix().and_then(|fix| fix.apply(source));
            (&source[d.span.start..d.span.end], fix)
        })
        .collect();
    assert_eq!(found.len(), 2, "{:?}", checked.diagnostics);
    assert_eq!(found[0].0, "include?");
    assert!(
        found[0]
            .1
            .as_deref()
            .unwrap()
            .contains("ok = h.key?(\"a\")\n")
    );
    assert_eq!(found[1].0, "day");
    assert!(found[1].1.as_deref().unwrap().ends_with("e = 5.days\n"));
    // Without them, `include?` could be an array's, and is left alone.
    assert!(with_code(source, Code::REMOVED_NAME).len() == 1);
    // An optional receiver renames as what it narrows to.
    let optional = "def f(items: array<int>?) -> int\n  items.size\nend\n";
    let checked = Engine::new().type_check(optional).unwrap();
    let size = checked
        .diagnostics
        .iter()
        .find(|d| d.code == Code::REMOVED_NAME)
        .expect("a removed name");
    assert!(size.applicable_fix().is_some(), "{size:?}");
    // An `any` receiver is unknown, and `size` is removed on every type.
    let untyped = "def f(items) -> int\n  items.size\nend\n";
    let checked = Engine::new().type_check(untyped).unwrap();
    assert!(
        checked
            .diagnostics
            .iter()
            .any(|d| d.code == Code::REMOVED_NAME && &untyped[d.span.start..d.span.end] == "size"),
        "{:?}",
        checked.diagnostics
    );
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
fn keyword_parameters_move_after_a_bare_star() {
    round_trip(
        "def send(to: string, retries: 3) -> string\n  to\nend\n",
        Code::KEYWORD_PARAMETER,
        "retries: 3",
        "def send(to: string, *, retries: int = 3) -> string\n  to\nend\n",
    );
    round_trip(
        "def send(to: string, name: string:, cc:\"x\") -> string\n  to\nend\n",
        Code::KEYWORD_PARAMETER,
        "name: string:, cc:\"x\"",
        "def send(to: string, *, name: string, cc: string = \"x\") -> string\n  to\nend\n",
    );
    round_trip(
        "def join(*items: array<int>, sep: \",\", width: -1) -> string\n  sep\nend\n",
        Code::KEYWORD_PARAMETER,
        "sep: \",\", width: -1",
        "def join(*items: array<int>, sep: string = \",\", width: int = -1) -> string\n  sep\nend\n",
    );
    let message = &with_code("def f(a: int, n: 2)\nend\n", Code::KEYWORD_PARAMETER)[0].message;
    assert_eq!(
        message,
        "`n: 2` was removed; keyword parameters follow a bare `*`: `*, n: int = 2`"
    );
    // Without a literal default the type is left for a person to declare.
    let fixed = fixed(
        "def f(a: int, name:, label: nil)\nend\n",
        Code::KEYWORD_PARAMETER,
    );
    assert_eq!(fixed, "def f(a: int, *, name, label = nil)\nend\n");
    let mut engine = Engine::new();
    engine.set_static_types(true);
    let codes: Vec<Code> = engine
        .compile(&fixed)
        .err()
        .expect("untyped parameters")
        .diagnostics()
        .iter()
        .map(|d| d.code)
        .collect();
    assert_eq!(codes, [Code::MISSING_PARAMETER_TYPE; 2]);
    // The canonical forms are left alone.
    for source in [
        "def f(a: int, *, b: int, c: string? = nil)\nend\n",
        "def f(*items: array<int>, sep: string = \",\")\nend\n",
        "def f(a: int = 1, b: string = \"x\")\nend\n",
    ] {
        assert!(
            with_code(source, Code::KEYWORD_PARAMETER).is_empty(),
            "{source}"
        );
        assert_clean(source, Code::KEYWORD_PARAMETER);
    }
}

/// The static check's diagnostics of `code` in `source`, which uses the
/// checker's receiver types.
fn checked(source: &str, code: Code) -> Vec<Diagnostic> {
    Engine::new()
        .type_check(source)
        .unwrap()
        .diagnostics
        .into_iter()
        .filter(|diagnostic| diagnostic.code == code)
        .collect()
}

/// Applies `code`'s fixes until none applies.
fn fixed_statically(source: &str, code: Code) -> String {
    let mut source = source.to_owned();
    while let Some(fix) = checked(&source, code)
        .first()
        .and_then(|diagnostic| diagnostic.applicable_fix().cloned())
    {
        source = fix.apply(&source).expect("the fix applies");
    }
    source
}

#[test]
fn hash_fields_are_indexed_not_dotted() {
    let source = "def label(user: { name: string }) -> string\n  user.name\nend\n";
    let found = checked(source, Code::FIELD_ACCESS);
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(&source[found[0].span.start..found[0].span.end], ".name");
    assert_eq!(
        found[0].message,
        "`.name` was removed; hash fields are indexed: `user[\"name\"]`"
    );
    // The unknown member the checker found inside it is left to it.
    let all = Engine::new().type_check(source).unwrap().diagnostics;
    assert_eq!(all.len(), 1, "{all:?}");
    let expected = "def label(user: { name: string }) -> string\n  user[\"name\"]\nend\n";
    assert_eq!(fixed_statically(source, Code::FIELD_ACCESS), expected);
    assert_clean(expected, Code::FIELD_ACCESS);
    assert!(
        Engine::new()
            .type_check(expected)
            .unwrap()
            .diagnostics
            .is_empty()
    );
    // Dictionaries, nested shapes and writes, fixed one level at a time.
    let source = "def f(doc: { meta: { id: string } }, counts: hash<string, int>) -> string\n  counts.seen = 1\n  doc.meta.id\nend\n";
    assert_eq!(
        fixed_statically(source, Code::FIELD_ACCESS),
        "def f(doc: { meta: { id: string } }, counts: hash<string, int>) -> string\n  counts[\"seen\"] = 1\n  doc[\"meta\"][\"id\"]\nend\n"
    );
    // A hash literal is a hash without the checker's types.
    round_trip(
        "n = { \"a\" => 1 }.a\n"
            .replace("{ \"a\" => 1 }", "{ a: 1 }")
            .as_str(),
        Code::FIELD_ACCESS,
        ".a",
        "n = { a: 1 }[\"a\"]\n",
    );
    // Safe navigation is reported without a fix.
    let source = "def f(user: { name: string }?) -> string?\n  user&.name\nend\n";
    let found = checked(source, Code::FIELD_ACCESS);
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].fixes.is_empty());
    // Methods, casts, other receivers and untyped values are left alone.
    for source in [
        "def f(h: hash<string, int>) -> int\n  h.length + h.keys.length\nend\n",
        "def f(value: any) -> hash<string, int>\n  value.as(hash<string, int>)\nend\n",
        "def f(h: { as: int }) -> { as: int }\n  h.as({ as: int })\nend\n",
        "def f(value: any) -> any\n  value.name\nend\n",
        "def f(t: time) -> int\n  t.year\nend\n",
    ] {
        assert!(checked(source, Code::FIELD_ACCESS).is_empty(), "{source}");
    }
    // Without static types, dot reads still run.
    let script = Engine::new()
        .compile("def run -> int\n  h = { a: 1 }\n  h.a\nend\n")
        .unwrap();
    let result = script
        .call("run", &[], crate::CallOptions::default())
        .unwrap();
    assert_eq!(result.value.as_int(), Some(1));
}

#[test]
fn required_symbols_become_strings() {
    round_trip(
        "helpers = require(:helpers)\n",
        Code::DYNAMIC_REQUIRE,
        ":helpers",
        "helpers = require(\"helpers\")\n",
    );
    round_trip(
        "helpers = require(\"reports/format\", as: :fmt)\n",
        Code::DYNAMIC_REQUIRE,
        ":fmt",
        "helpers = require(\"reports/format\", as: \"fmt\")\n",
    );
    let found = with_code("helpers = require(:helpers)\n", Code::DYNAMIC_REQUIRE);
    assert_eq!(
        found[0].message,
        "`require` names modules and aliases with string literals, not `:helpers`; name the module with the string `\"helpers\"`"
    );
    let checked = Engine::new()
        .type_check("helpers = require(:helpers)\n")
        .unwrap();
    let required: Vec<_> = checked
        .diagnostics
        .iter()
        .filter(|d| d.code == Code::DYNAMIC_REQUIRE)
        .collect();
    assert_eq!(required.len(), 1, "{:?}", checked.diagnostics);
    assert!(required[0].applicable_fix().is_some());
    // A name known only at runtime has no rewrite; the checker reports it.
    assert!(with_code("name = \"a\"\nrequire(name)\n", Code::DYNAMIC_REQUIRE).is_empty());
    assert!(with_code("require(\"helpers\")\n", Code::DYNAMIC_REQUIRE).is_empty());
}

#[test]
fn every_surface_code_is_registered_with_a_test() {
    let surface: Vec<Code> = crate::diagnostic::codes()
        .iter()
        .map(|info| info.code)
        .filter(|code| code.area() == Some(crate::diagnostic::Area::Surface))
        .collect();
    assert_eq!(surface.len(), 15);
    assert_eq!(surface.first(), Some(&Code::REMOVED_NAME));
    assert_eq!(surface.last(), Some(&Code::FIELD_ACCESS));
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

// WASI preview 1 has no temporary directory to hold the required file.
#[cfg(not(target_os = "wasi"))]
#[test]
fn removed_spellings_in_required_files_are_reported_at_compile_time() {
    let directory = std::env::temp_dir().join(format!("surface-modules-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let helpers = "def count_of(items: array<int>) -> int\n  items.size\nend\n";
    std::fs::write(directory.join("helpers.vibe"), helpers).unwrap();
    let mut engine = Engine::new();
    engine
        .set_module_config(crate::ModuleConfig {
            paths: vec![directory.clone()],
            ..crate::ModuleConfig::default()
        })
        .unwrap();
    engine.set_static_types(true);
    let source = "def run -> int\n  h = require(\"helpers\")\n  h.count_of([1])\nend\n";
    let error = engine.compile(source).err().expect("a compile error");
    let found: Vec<(String, Option<String>)> = error
        .diagnostics()
        .iter()
        .map(|d| {
            let file = d
                .file
                .as_deref()
                .map(|f| String::from_utf8_lossy(f).into_owned());
            (d.code.to_string(), file)
        })
        .collect();
    assert_eq!(
        found,
        [("V0401".to_owned(), Some("helpers.vibe".to_owned()))]
    );
    let _ = std::fs::remove_dir_all(&directory);
}

// WASI preview 1 cannot start the larger-stack thread, so it skips such
// sources.
#[cfg(not(target_os = "wasi"))]
#[test]
fn nesting_deeper_than_the_limit_is_still_checked() {
    let depth = 400;
    let source = format!("x = {}[1].size{}\n", "[".repeat(depth), "]".repeat(depth));
    let found = with_code(&source, Code::REMOVED_NAME);
    assert_eq!(found.len(), 1, "{found:?}");
}

/// A call matching a rename entry's pattern, with `1` for each argument it
/// captures.
fn call_for(pattern: &str) -> String {
    let mut out = String::new();
    let mut rest = pattern;
    while let Some(start) = rest.find('$') {
        out.push_str(&rest[..start]);
        let tail = &rest[start + 1..];
        let end = tail
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .unwrap_or(tail.len());
        out.push_str(if &tail[..end] == "x" { "x" } else { "1" });
        rest = &tail[end..];
    }
    out.push_str(rest);
    out.replace(", ..., ", ", ")
        .replace("(..., ", "(")
        .replace(", ...)", ")")
        .replace("(...)", "(1)")
}

#[test]
fn every_rename_table_entry_is_reported() {
    use crate::signatures::renames;
    let mut missing = Vec::new();
    for rename in renames() {
        let call = call_for(&rename.pattern);
        let source = match rename.receiver.as_str() {
            "*" if rename.pattern.starts_with("$x.") => "y = [1].length()\n".to_owned(),
            "*" => "y = uuid()\n".to_owned(),
            "global" => format!("y = {call}\n"),
            "type" => format!("def f(x: {}) -> int\n  1\nend\n", rename.name),
            "error" => format!("begin\n  raise \"no\"\nrescue => x\n  y = {call}\nend\n"),
            "T" => format!("def f(x: int) -> int\n  y = {call}\n  1\nend\n"),
            receiver if receiver.starts_with(|c: char| c.is_ascii_uppercase()) => {
                format!("y = {call}\n")
            }
            receiver => format!("def f(x: {receiver}) -> int\n  y = {call}\n  1\nend\n"),
        };
        let Ok(found) = check(&source) else {
            missing.push(format!("{source:?} does not parse"));
            continue;
        };
        let name = if rename.receiver == "*" {
            "()"
        } else {
            rename.name.as_str()
        };
        let reported = found
            .iter()
            .any(|d| source[d.span.start..d.span.end].contains(name));
        if !reported {
            missing.push(format!("{source:?}: {found:?}"));
        }
    }
    assert!(missing.is_empty(), "{}", missing.join("\n"));
}
