//! One test per rewrite rule, and end-to-end fixtures.

use super::{Code, Invocation, Observations, Options, migrate, observe, repair};
use serde_json::json;

/// Migrates `source` after running `calls`, each `(function, args)`.
fn run(
    source: &str,
    calls: &[(&str, serde_json::Value)],
    options: &Options,
) -> (String, Vec<Code>) {
    let invocations: Vec<Invocation> = calls
        .iter()
        .map(|(function, args)| {
            Invocation::from_json(json!({"function": function, "args": args}), ".".as_ref())
                .unwrap()
        })
        .collect();
    let observations = if invocations.is_empty() {
        Observations::default()
    } else {
        observe(source, &invocations)
    };
    let migration = migrate(source, &observations, options);
    let codes = migration.diagnostics.iter().map(|d| d.code).collect();
    (migration.source, codes)
}

fn full(source: &str, calls: &[(&str, serde_json::Value)]) -> (String, Vec<Code>) {
    run(source, calls, &Options::default())
}

fn compatible(source: &str, calls: &[(&str, serde_json::Value)]) -> (String, Vec<Code>) {
    run(
        source,
        calls,
        &Options {
            new_syntax: false,
            ..Options::default()
        },
    )
}

#[test]
fn renames_members_for_the_observed_receiver_type() {
    let source = "def run(items, text)\n  [items.size, text.size, items.count]\nend\n";
    let (out, codes) = full(source, &[("run", json!([[1, 2], "ab"]))]);
    assert_eq!(
        out,
        "def run(items: array<int>, text: string) -> array<int>\n  [items.length, text.length, items.length]\nend\n"
    );
    assert!(codes.is_empty(), "{codes:?}");
}

#[test]
fn leaves_a_user_class_method_of_a_removed_name() {
    let source = "class Box\n  def size\n    3\n  end\nend\ndef run\n  Box.new.size\nend\n";
    let (out, _) = compatible(source, &[("run", json!([]))]);
    assert!(out.contains("Box.new.size"), "{out}");
}

#[test]
fn reports_manual_renames_and_mixed_receivers() {
    let source = "def run(x)\n  x.tap { |v| v }\n  x.size\nend\n";
    let (_, codes) = compatible(source, &[("run", json!([[1]])), ("run", json!(["s"]))]);
    assert!(codes.contains(&Code::Rename), "{codes:?}");
    let source = "def run(x)\n  x.seconds\nend\n";
    let (out, codes) = compatible(source, &[("run", json!([1])), ("run", json!(null))]);
    assert!(out.contains("x.seconds"), "{out}");
    let _ = codes;
}

#[test]
fn rewrites_rename_templates_with_arguments_and_blocks() {
    let source = "def run(ids)\n  a = ids.sub(\"x\", \"y\", regex: false)\n  b = [3, 4].find_index { |v| v > 3 }\n  c = 7.modulo(2) * 3\n  [a, b, c]\nend\n";
    let (out, _) = full(source, &[("run", json!(["axb"]))]);
    assert!(out.contains("a = ids.sub(\"x\", \"y\")\n"), "{out}");
    assert!(out.contains("b = [3, 4].index { |v| v > 3 }\n"), "{out}");
    assert!(out.contains("c = (7 % 2) * 3\n"), "{out}");
}

#[test]
fn converts_do_blocks_to_braces_keeping_their_call() {
    let source = "def run\n  a = [1, 2].map do |x|\n    x * 2\n  end\n  puts [1].map do |x| x end\n  a\nend\n";
    let (out, _) = compatible(source, &[("run", json!([]))]);
    assert!(
        out.contains("a = [1, 2].map { |x|\n    x * 2\n  }\n"),
        "{out}"
    );
    assert!(out.contains("puts([1].map) { |x| x }\n"), "{out}");
}

#[test]
fn moves_a_do_on_the_next_line_up_to_its_call() {
    let source = "def run\n  [1].each_with_index(\n  )\n  do |x|\n    x\n  end\nend\n";
    let (out, _) = compatible(source, &[]);
    assert!(
        out.contains("[1].each_with_index { |x|\n    x\n  }\n"),
        "{out}"
    );
}

#[test]
fn rewrites_unless_and_until_as_negations() {
    let source = "def run(n)\n  unless n == 0\n    n = n - 1 until n < 3\n  end\n  puts n unless n > 5\n  n\nend\n";
    let (out, codes) = compatible(source, &[("run", json!([9]))]);
    assert_eq!(
        out,
        "def run(n: int) -> int\n  if n != 0\n    n = n - 1 while !(n < 3)\n  end\n  puts n if !(n > 5)\n  n\nend\n"
    );
    assert!(codes.is_empty(), "{codes:?}");
}

#[test]
fn spells_symbol_hash_keys_as_strings() {
    let source = "def run\n  h = {}\n  h[:a] = 1\n  h[:\"b c\"] = 2\n  [h[:a], h]\nend\n";
    let (out, _) = compatible(source, &[("run", json!([]))]);
    assert!(
        out.contains("h[\"a\"] = 1\n  h[\"b c\"] = 2\n  [h[\"a\"], h]"),
        "{out}"
    );
}

#[test]
fn indexes_hash_fields_read_with_a_dot() {
    let source = "def run(user)\n  user.count = user.count + 1\n  [user.name, user.length, user.count]\nend\n";
    let (out, _) = compatible(source, &[("run", json!([{"name": "a", "count": 1}]))]);
    assert!(
        out.contains(
            "user[\"count\"] = user[\"count\"] + 1\n  [user[\"name\"], user.length, user[\"count\"]]"
        ),
        "{out}"
    );
    // A read that raised in a recorded run, or on a receiver that was not
    // always a hash, is left for a person.
    let source = "def run(user)\n  user.name\nend\n";
    let (out, codes) = compatible(source, &[("run", json!([{"id": 1}]))]);
    assert!(out.contains("user.name"), "{out}");
    assert!(codes.contains(&Code::Receiver), "{codes:?}");
    let (out, codes) = compatible(
        source,
        &[("run", json!([{"name": "a"}])), ("run", json!(["x"]))],
    );
    assert!(out.contains("user.name"), "{out}");
    assert!(codes.contains(&Code::Receiver), "{codes:?}");
}

#[test]
fn keeps_symbol_indexes_on_other_receivers() {
    let source = "def run(x)\n  x[:a]\nend\n";
    let (out, _) = compatible(source, &[("run", json!([[1]]))]);
    assert!(out.contains("x[:a]"), "{out}");
}

#[test]
fn writes_percent_literals_as_arrays() {
    let source = "def run\n  [%w[a b\"c], %i[x y?]]\nend\n";
    let (out, _) = compatible(source, &[]);
    assert!(out.contains("[[\"a\", \"b\\\"c\"], [:x, :y?]]"), "{out}");
}

#[test]
fn replaces_hash_new_where_it_is_called() {
    let source = "def run\n  a = Hash.new\n  b = Hash.new(0)\n  [a, b]\nend\n";
    let (out, codes) = compatible(source, &[("run", json!([]))]);
    assert!(out.contains("a = {}\n"), "{out}");
    assert!(out.contains("Hash.new(0)"), "{out}");
    assert!(codes.contains(&Code::HashNew), "{codes:?}");
}

#[test]
fn rewrites_nil_tests_as_comparisons() {
    let source = "def run(x)\n  a = x.nil?\n  b = !x.nil?\n  c = x.nil?.to_s\n  [a, b, c]\nend\n";
    let (out, _) = compatible(source, &[("run", json!([1]))]);
    assert!(out.contains("a = x == nil\n"), "{out}");
    assert!(out.contains("b = !(x == nil)\n"), "{out}");
    assert!(out.contains("c = (x == nil).to_s\n"), "{out}");
}

#[test]
fn drops_empty_argument_parentheses_where_safe() {
    let source = "def helper\n  1\nend\ndef run(x)\n  [helper(), x.length(), Time.now().year, uuid().length, puts()]\nend\n";
    let (out, _) = compatible(source, &[("run", json!([[1]]))]);
    assert!(
        out.contains("[helper, x.length, Time.now.year, uuid.length, puts()]"),
        "{out}"
    );
    let (out, _) = full(source, &[("run", json!([[1]]))]);
    assert!(out.contains("puts]"), "{out}");
}

#[test]
fn keeps_parentheses_that_call_a_function_value() {
    let source = "def run(h)\n  h.run()\nend\n";
    let (out, _) = compatible(source, &[("run", json!([{"run": 1}]))]);
    assert!(out.contains("h.run()"), "{out}");
}

#[test]
fn replaces_dispatch_by_a_literal_name() {
    let source =
        "def run(x)\n  [x.send(:length), x.public_send(:first, 1), x.send(x.first.to_sym)]\nend\n";
    let (out, codes) = compatible(source, &[("run", json!([["length"]]))]);
    assert!(
        out.contains("[x.length, x.first(1), x.send(x.first.to_sym)]"),
        "{out}"
    );
    assert!(codes.contains(&Code::Dispatch), "{codes:?}");
}

#[test]
fn reports_dynamic_require_and_quotes_symbols() {
    let source = "def run(name)\n  require(:util)\n  require(name)\nend\n";
    let (out, codes) = compatible(source, &[]);
    assert!(out.contains("require(\"util\")"), "{out}");
    assert!(codes.contains(&Code::Require), "{codes:?}");
}

#[test]
fn annotates_parameters_and_results() {
    let source = "def total(items, scale = 2, *rest, **opts)\n  items.sum * scale\nend\n";
    let (out, codes) = compatible(source, &[("total", json!([[1, 2], 3, "x"]))]);
    assert_eq!(
        out,
        "def total(items: array<int>, scale: int = 2, *rest: array<string>, **opts: hash<string, any>) -> int\n  items.sum * scale\nend\n"
    );
    assert_eq!(codes, [Code::Any]);
}

#[test]
fn annotates_keyword_parameters() {
    let source = "def tagged(tag:, limit: 3)\n  tag * limit\nend\n";
    let call = json!({"function": "tagged", "typed_kwargs": [["tag", ["string", "6162"]]]});
    let observations = observe(
        source,
        &[Invocation::from_json(call, ".".as_ref()).unwrap()],
    );
    let migration = migrate(source, &observations, &Options::default());
    // Keyword parameters move after a bare `*`, declared with their types.
    assert_eq!(
        migration.source,
        "def tagged(*, tag: string, limit: int = 3) -> string\n  tag * limit\nend\n"
    );
    // Without the new syntax they are left in their removed form, untyped.
    let migration = migrate(
        source,
        &observations,
        &Options {
            new_syntax: false,
            ..Options::default()
        },
    );
    assert!(
        migration
            .source
            .starts_with("def tagged(tag:, limit: 3) -> string"),
        "{}",
        migration.source
    );
    // After a rest parameter the keywords need no `*`, and a typed one
    // loses the colon after its type.
    let source = "def joined(*items, sep: \",\", width: int:)\n  items.join(sep) * width\nend\n";
    let call = json!({"function": "joined", "args": ["a", "b"], "typed_kwargs": [["width", ["int", "2"]]]});
    let observations = observe(
        source,
        &[Invocation::from_json(call, ".".as_ref()).unwrap()],
    );
    let migration = migrate(source, &observations, &Options::default());
    assert_eq!(
        migration.source,
        "def joined(*items: array<string>, sep: string = \",\", width: int) -> string\n  items.join(sep) * width\nend\n"
    );
}

#[test]
fn keeps_existing_annotations_and_lowercases_type_names() {
    let source = "def run(x: Int, y: object) -> String\n  x.to_s\nend\n";
    let (out, _) = compatible(source, &[]);
    assert_eq!(out, "def run(x: int, y: hash) -> string\n  x.to_s\nend\n");
}

#[test]
fn falls_back_to_any_and_reports_it() {
    let source = "def unused(a)\n  a\nend\n";
    let (out, codes) = compatible(source, &[]);
    assert_eq!(out, "def unused(a: any) -> any\n  a\nend\n");
    assert!(codes.iter().all(|code| *code == Code::Any), "{codes:?}");
}

#[test]
fn declares_the_block_of_a_function_that_yields() {
    let source = "def each_pair(h)\n  h.each { |k, v| yield k, v }\n  nil\nend\ndef twice(n)\n  yield(n) + yield(n)\nend\ndef run\n  each_pair({ \"a\" => 1 }) { |k, v| k }\n  twice(2) { |x| x * 3 }\nend\n";
    let source = source.replace("{ \"a\" => 1 }", "{ a: 1 }");
    let (out, _) = full(&source, &[("run", json!([]))]);
    assert!(
        out.contains("def each_pair(h: { a: int }, &block: (string, int) -> string)"),
        "{out}"
    );
    assert!(
        out.contains("def twice(n: int, &block: int -> int) -> int"),
        "{out}"
    );
}

#[test]
fn declares_locals_whose_first_value_does_not_fix_their_type() {
    let source = "def run(n)\n  names = []\n  label = nil\n  count = 1\n  count = 1.5 if n > 1\n  names << \"a\"\n  label = \"x\"\n  [names, label, count]\nend\n";
    let (out, _) = full(source, &[("run", json!([2]))]);
    assert!(out.contains("names: array<string> = []"), "{out}");
    assert!(out.contains("label: string? = nil"), "{out}");
    assert!(out.contains("count: number = 1"), "{out}");
    let (out, _) = compatible(source, &[("run", json!([2]))]);
    assert!(out.contains("names = []"), "{out}");
}

#[test]
fn declares_instance_variables_and_types_properties() {
    let source = "class Counter\n  property step\n  def initialize(start)\n    @count = start\n    @step = 1\n  end\n  def bump\n    @last = @count\n    @count += @step\n  end\nend\ndef run\n  c = Counter.new(5)\n  c.bump\n  c.step\nend\n";
    let (out, _) = full(source, &[("run", json!([]))]);
    assert!(
        out.contains("class Counter\n  @count: int\n  @last: int?\n"),
        "{out}"
    );
    assert!(out.contains("property step: int\n"), "{out}");
}

#[test]
fn floors_integer_division_and_reports_mixed_operands() {
    let source = "def run(a, b)\n  [a / 2, b / 2.0, a / b]\nend\n";
    let (out, codes) = full(source, &[("run", json!([7, 2])), ("run", json!([7, 2.5]))]);
    assert!(out.contains("[a // 2, b / 2.0, a / b]"), "{out}");
    assert!(codes.contains(&Code::Division), "{codes:?}");
    let (out, codes) = compatible(source, &[("run", json!([7, 2]))]);
    assert!(out.contains("a / 2"), "{out}");
    assert!(codes.contains(&Code::Syntax), "{codes:?}");
}

#[test]
fn tests_optional_values_against_nil() {
    let source = "def run(name, flag)\n  label = name || \"anon\"\n  puts label if name\n  unless flag\n    puts \"off\"\n  end\n  x = nil\n  x ||= 3\n  label\nend\n";
    let (out, codes) = compatible(
        source,
        &[("run", json!(["ada", true])), ("run", json!([null, null]))],
    );
    assert!(
        out.contains("label = name != nil ? name : \"anon\""),
        "{out}"
    );
    assert!(out.contains("puts label if name != nil"), "{out}");
    assert!(out.contains("if flag != true\n"), "{out}");
    assert!(out.contains("x = 3 if x == nil"), "{out}");
    assert!(
        codes.iter().all(|code| *code != Code::Condition),
        "{codes:?}"
    );
}

#[test]
fn reports_conditions_it_cannot_rewrite() {
    let source = "def run(v)\n  if v\n    1\n  end\nend\n";
    let (out, codes) = compatible(source, &[("run", json!([false])), ("run", json!(["x"]))]);
    assert!(out.contains("if v\n"), "{out}");
    assert!(codes.contains(&Code::Condition), "{codes:?}");
}

#[test]
fn reports_a_case_over_an_enum_that_misses_members() {
    let source = "enum Status\n  Draft\n  Done\n  Archived\nend\ndef label(s)\n  case s\n  when :draft then \"d\"\n  when Status::Done then \"x\"\n  end\nend\ndef run\n  label(Status::Draft)\nend\n";
    let (_, codes) = compatible(source, &[("run", json!([]))]);
    assert!(codes.contains(&Code::Case), "{codes:?}");
}

#[test]
fn preserves_comments_and_layout() {
    let source = "# Totals.\ndef run(items) # entry\n  # Sum them.\n  items.size   # count\nend\n";
    let (out, _) = compatible(source, &[("run", json!([[1]]))]);
    assert_eq!(
        out,
        "# Totals.\ndef run(items: array<int>) -> int # entry\n  # Sum them.\n  items.length   # count\nend\n"
    );
}

#[test]
fn leaves_sources_that_do_not_compile() {
    let (out, codes) = compatible("def run(\n", &[]);
    assert_eq!(out, "def run(\n");
    assert_eq!(codes, [Code::Unparsed]);
}

#[test]
fn output_is_formatted_and_migrating_again_changes_nothing() {
    let source = "def run(x)  \n  x.size()\r\nend";
    let (out, _) = compatible(source, &[("run", json!([[1]]))]);
    assert!(crate::format::is_formatted(&out), "{out:?}");
    let (again, _) = compatible(&out, &[("run", json!([[1]]))]);
    assert_eq!(again, out);
}

#[test]
fn distrusts_runs_it_cannot_reproduce() {
    let source = "def run(x)\n  x\nend\n";
    let probe = Invocation::from_json(
        json!({"function": "run", "args": [1], "capability_probe": true}),
        ".".as_ref(),
    )
    .unwrap();
    let plain =
        Invocation::from_json(json!({"function": "run", "args": [1]}), ".".as_ref()).unwrap();
    let observations = observe(source, &[plain, probe]);
    let migration = migrate(
        source,
        &observations,
        &Options {
            new_syntax: false,
            ..Options::default()
        },
    );
    assert!(
        migration.source.starts_with("def run(x: any) -> any"),
        "{}",
        migration.source
    );
}

#[test]
fn infers_results_of_functions_no_run_reached() {
    // The checker's inference from declared parameters, then literal exits.
    let source = "def next_id(n: int)\n  n + 1\nend\ndef label(flag: bool)\n  return \"on\" if flag\n  nil\nend\n";
    let (out, codes) = compatible(source, &[]);
    assert!(out.contains("def next_id(n: int) -> int\n"), "{out}");
    assert!(out.contains("def label(flag: bool) -> string?\n"), "{out}");
    assert!(codes.is_empty(), "{codes:?}");
}

/// Migrates `source` after running `calls`, then repairs the migration with
/// the static checker, returning the result and its static errors.
fn repaired(source: &str, calls: &[(&str, serde_json::Value)]) -> (String, Vec<String>) {
    let invocations: Vec<Invocation> = calls
        .iter()
        .map(|(function, args)| {
            Invocation::from_json(json!({"function": function, "args": args}), ".".as_ref())
                .unwrap()
        })
        .collect();
    let observations = observe(source, &invocations);
    let migration = migrate(source, &observations, &Options::default());
    let repaired = repair(source, &migration, &invocations, &observations);
    let errors = vibescript::Engine::new()
        .type_check(&repaired.source)
        .unwrap()
        .diagnostics
        .iter()
        .filter(|d| d.is_error())
        .map(|d| d.message.clone())
        .collect();
    (repaired.source, errors)
}

#[test]
fn widens_a_result_to_what_an_unrun_branch_returns() {
    let source = "def run(n)\n  begin\n    raise \"no\" if n > 0\n    [n, \"ok\"]\n  rescue => e\n    [e.message]\n  end\nend\n";
    let (out, errors) = repaired(source, &[("run", json!([1]))]);
    assert!(
        out.starts_with("def run(n: int) -> array<int | string>\n"),
        "{out}"
    );
    assert!(errors.is_empty(), "{errors:?}");
}

#[test]
fn reads_an_index_the_runs_never_missed_with_fetch() {
    let source = "def run(items)\n  items[0] + 1\nend\n";
    let (out, errors) = repaired(source, &[("run", json!([[1, 2]]))]);
    assert!(out.contains("items.fetch(0) + 1"), "{out}");
    assert!(errors.is_empty(), "{errors:?}");
}

#[test]
fn keeps_a_write_through_an_index_that_fetch_would_copy() {
    let source = "def run\n  a = [[1]]\n  a[0].push(2)\n  a\nend\n";
    let (out, _) = repaired(source, &[("run", json!([]))]);
    assert!(out.contains("a[0].push(2)"), "{out}");
}

#[test]
fn narrows_a_parsed_value_where_it_is_assigned() {
    let source = "def run(raw)\n  data = JSON.parse(raw)\n  data[\"n\"] + 1\nend\n";
    let (out, errors) = repaired(source, &[("run", json!(["{\"n\": 1}"]))]);
    assert!(
        out.contains("data = JSON.parse(raw).as({ n: int })"),
        "{out}"
    );
    assert!(errors.is_empty(), "{errors:?}");
}

#[test]
fn converts_what_plus_joins_to_a_string() {
    let source = "def run(count)\n  count + \" items\"\nend\n";
    let (out, errors) = repaired(source, &[("run", json!([3]))]);
    assert!(out.contains("count.to_s + \" items\""), "{out}");
    assert!(errors.is_empty(), "{errors:?}");
}

#[test]
fn types_a_function_no_run_reached_from_its_callers() {
    let source = "def twice(x)\n  x * 2\nend\ndef run(flag)\n  flag ? twice(3) : 0\nend\n";
    let (out, errors) = repaired(source, &[("run", json!([false]))]);
    assert!(out.starts_with("def twice(x: int) -> int\n"), "{out}");
    assert!(errors.is_empty(), "{errors:?}");
}

#[test]
fn spells_out_an_authors_bare_collections() {
    let source =
        "def keys(h: hash) -> array\n  h.keys\nend\ndef run(input)\n  keys({ a: 1 })\nend\n";
    let (out, errors) = repaired(source, &[("run", json!([null]))]);
    assert!(
        out.starts_with("def keys(h: hash<string, int>) -> array<string>\n"),
        "{out}"
    );
    assert!(errors.is_empty(), "{errors:?}");
}

#[test]
fn types_a_parameter_the_runs_only_passed_nil_from_its_callers() {
    let source = "def show(m)\n  return \"none\" if m == nil\n  m.captures.length.to_s\nend\ndef run(text)\n  show(text.match(/x(y)/))\nend\n";
    let (out, errors) = repaired(source, &[("run", json!(["z"]))]);
    assert!(out.contains("def show(m: match_data?)"), "{out}");
    assert!(errors.is_empty(), "{errors:?}");
}

#[test]
fn declares_a_record_read_with_computed_keys_a_dictionary() {
    let source = "def run(n)\n  h = {}\n  h[\"k\" + n.to_s] = n\n  h[\"k\" + n.to_s]\nend\n";
    let (out, errors) = repaired(source, &[("run", json!([1]))]);
    assert!(out.contains("h: hash<string, int> = {}"), "{out}");
    assert!(errors.is_empty(), "{errors:?}");
}

#[test]
fn widens_a_local_an_unrun_branch_assigns() {
    let source = "def run(flag)\n  x = 1\n  x = \"s\" if flag\n  x\nend\n";
    let (out, errors) = repaired(source, &[("run", json!([false]))]);
    assert!(out.contains("x: int | string = 1"), "{out}");
    assert!(errors.is_empty(), "{errors:?}");
}

#[test]
fn reports_a_removed_spelling_the_rules_leave_in_place() {
    let source = "class C\nend\ndef run(input)\n  C.respond_to?(:x)\nend\n";
    let call = json!({"function": "run", "args": [null]});
    let invocations = [Invocation::from_json(call, ".".as_ref()).unwrap()];
    let observations = observe(source, &invocations);
    let migration = migrate(source, &observations, &Options::default());
    let repaired = repair(source, &migration, &invocations, &observations);
    let notes: Vec<(Code, usize)> = repaired
        .diagnostics
        .iter()
        .map(|d| (d.code, d.line))
        .collect();
    assert_eq!(notes, [(Code::Dispatch, 4)], "{:?}", repaired.diagnostics);
}
