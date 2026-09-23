//! Native mirror of the Go v0.70.0 integration driver over the pinned `tests/` programs.
//!
//! The reference driver is `internal/runtime/integration_test.go` at commit
//! `5cba216c33bea8890787d64efb2ab926a761fb1b`. Every driver invocation has a stable
//! identifier listed in `tests/upstream/driver.json`; each group test below asserts
//! that it exercised exactly its declared identifiers, and
//! `driver_manifest_is_fully_covered` checks that the groups together cover every
//! manifest identifier and that every pinned `tests/` program is walked.
//!
//! Expected values are the Go driver's literals, written in a small JSON notation:
//! plain JSON maps to nil/bool/int/float/string/array/hash, and one-key objects
//! `{"$symbol": name}`, `{"$money": [currency, cents]}`, `{"$time": [seconds, nanos]}`
//! and `{"$enum": [enum, member, symbol]}` name typed values. Hash comparison ignores
//! insertion order but enforces the key set, as Go's `assertValueEqual` does.
//! Rejections assert a runtime `ErrorKind` derived from the Rust source; Go's error
//! wording is not asserted.

use serde_json::{Value as Json, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    f64::consts::PI,
    fs,
    path::Path,
};
use vibescript::{CallOptions, Engine, ErrorKind, Limits, Script, Value};

/// `Config{StepQuota: 5_000_000}` in `TestComplexExamplesStress` and
/// `TestAllVibeFilesCompileAndRun`; memory and recursion keep their defaults.
const HIGH_QUOTA: u64 = 5_000_000;
/// Driver invocations recorded in `tests/upstream/driver.json`.
const MANIFEST_TOTAL: usize = 105;
/// Reference programs under Go's `tests/` tree.
const PROGRAM_TOTAL: usize = 39;

// ---------------------------------------------------------------------------
// Case tables. Line numbers refer to internal/runtime/integration_test.go.
// ---------------------------------------------------------------------------

/// `TestComplexExamplesCompile`, lines 29-49.
const COMPILE_CASES: &[(&str, &str)] = &[
    ("compile/complex/analytics", "tests/complex/analytics.vibe"),
    ("compile/complex/durations", "tests/complex/durations.vibe"),
    ("compile/complex/finance", "tests/complex/finance.vibe"),
    ("compile/complex/strings", "tests/complex/strings.vibe"),
    ("compile/complex/loops", "tests/complex/loops.vibe"),
    ("compile/complex/typed", "tests/complex/typed.vibe"),
    ("compile/complex/pipeline", "tests/complex/pipeline.vibe"),
    ("compile/complex/massive", "tests/complex/massive.vibe"),
    (
        "compile/complex/chudnovsky",
        "tests/complex/chudnovsky.vibe",
    ),
];

/// `TestComplexExamplesRun` exact-value cases at default limits (lines 62-77,
/// 104-140, 161-180, 203-238). The two predicate cases are coded in
/// `complex_examples_run`.
fn complex_run_cases() -> Vec<(&'static str, &'static str, Json)> {
    vec![
        (
            "run/complex/analytics",
            "tests/complex/analytics.vibe",
            json!({
                "total": 21,
                "top": 9,
                "names": ["alex", "bea", "cam"],
                "average": 7,
                "active": [
                    {"name": "alex", "score": 5, "last_seen": 100},
                    {"name": "cam", "score": 7, "last_seen": 120}
                ],
                "leaders": []
            }),
        ),
        (
            "run/complex/finance",
            "tests/complex/finance.vibe",
            json!({
                "fee": {"$money": ["USD", 1100]},
                "applied": [{"$money": ["USD", 300]}, {"$money": ["USD", 450]}],
                "average": {"total": {"$money": ["USD", 550]}, "count": 2}
            }),
        ),
        (
            "run/complex/strings",
            "tests/complex/strings.vibe",
            json!({
                "slug": "hello-world",
                "initials": "AL",
                "title": "vibes",
                "wrapped": ["one two", "three", "four"]
            }),
        ),
        (
            "run/complex/loops",
            "tests/complex/loops.vibe",
            json!({"sum": 10, "flat": [1, 2, 3, 4], "countdown": [3, 2, 1, 0]}),
        ),
        (
            "run/complex/typed",
            "tests/complex/typed.vibe",
            json!({
                "announce": "score: alex => 9",
                "adjusted": {"$time": [90, 0]},
                "maybe_nil": null,
                "maybe_val": 7,
                "user": {"name": "alex", "active": true}
            }),
        ),
        (
            "run/complex/pipeline",
            "tests/complex/pipeline.vibe",
            json!({
                "normalized": [
                    {"name": "a", "score": 100},
                    {"name": "b", "score": 80},
                    {"name": "c", "score": 40}
                ],
                "filtered": [
                    {"name": "a", "score": 120},
                    {"name": "b", "score": 80}
                ],
                "top": [
                    {"name": "a", "score": 100},
                    {"name": "b", "score": 80}
                ]
            }),
        ),
        // Default-limit witness: Go succeeds under Config{} (1,000,000 steps).
        (
            "run/complex/massive",
            "tests/complex/massive.vibe",
            json!(31_375),
        ),
        (
            "run/complex/yield_basics",
            "tests/complex/yield_basics.vibe",
            json!({"count": 2, "doubled": 42, "sum": 70}),
        ),
        (
            "run/complex/with_blocks",
            "tests/complex/with_blocks.vibe",
            json!({"sum": 15, "doubled": [2, 4, 6, 8, 10], "first_even": 2}),
        ),
        (
            "run/complex/advanced_blocks",
            "tests/complex/advanced_blocks.vibe",
            json!({"captured": [1, 2], "nested": 36, "defaulted": 7}),
        ),
    ]
}

/// `TestComplexExamplesRun` predicate cases (lines 78-103 and 181-202).
const COMPLEX_RUN_PREDICATE_IDS: &[&str] = &["run/complex/durations", "run/complex/chudnovsky"];

/// `TestProgramFixtures`, lines 261-439. `classes/counter` is coded separately so it
/// can be called twice on one script.
fn program_fixture_cases() -> Vec<(&'static str, &'static str, Json)> {
    vec![
        (
            "fixture/runtime_stress/run",
            "tests/runtime_stress.vibe",
            json!({
                "total": 500_500,
                "square_sum": 55,
                "iso": "PT3M30S",
                "shifted": "1970-01-01T00:03:30Z",
                "minutes": 3,
                "seconds": 30
            }),
        ),
        (
            "fixture/typing_fixture/run",
            "tests/typing_fixture.vibe",
            json!({"add_nil": null, "add_val": 7, "kw": "alex-7"}),
        ),
        (
            "fixture/enums_fixture/run",
            "tests/enums_fixture.vibe",
            json!({
                "member_name": "Draft",
                "published_name": "Published",
                "names": ["Draft", "Published"],
                "facts": enum_facts()
            }),
        ),
        (
            "fixture/collections_fixture/run",
            "tests/collections_fixture.vibe",
            json!({"initial": "A", "sizes": {"arr": 2, "str": 5}}),
        ),
        (
            "fixture/blocks/block_arity/run",
            "tests/blocks/block_arity.vibe",
            json!({"extra": [1], "missing": [9, null, null]}),
        ),
        (
            "fixture/blocks/block_closure/run",
            "tests/blocks/block_closure.vibe",
            json!({"total": 6, "shadow": 5, "mapped": 16}),
        ),
        (
            "fixture/blocks/reduce_single/run",
            "tests/blocks/reduce_single.vibe",
            json!({"single": 7, "hash": {"one": 2, "two": 4}}),
        ),
        (
            "fixture/blocks/instance_block_context/run",
            "tests/blocks/instance_block_context.vibe",
            json!({"total": 10, "count": 10}),
        ),
        (
            "fixture/classes/people/run",
            "tests/classes/people.vibe",
            json!({"name": "John", "age": 1, "reveal": "shh"}),
        ),
        (
            "fixture/classes/point/run",
            "tests/classes/point.vibe",
            json!({"before": {"x": 2, "y": 3}, "after": {"x": 9, "y": 3}}),
        ),
        (
            "fixture/classes/privacy/run",
            "tests/classes/privacy.vibe",
            json!({"internal": 42}),
        ),
        (
            "fixture/classes/self_calls/run",
            "tests/classes/self_calls.vibe",
            json!({"class_call": 10, "instance_call": 14}),
        ),
        (
            "fixture/classes/setter/run",
            "tests/classes/setter.vibe",
            json!({"before": 0, "after": 5}),
        ),
        (
            "fixture/classes/typed_property/run",
            "tests/classes/typed_property.vibe",
            json!({"owner": "Ada Lovelace", "balance": 100, "note": "primary"}),
        ),
    ]
}

/// `TestProgramFixtures` `classes/counter` (lines 373-381), asserted on two calls.
const COUNTER_FIXTURE_ID: &str = "fixture/classes/counter/run";

/// The `facts` hash asserted at lines 300-308 and 462-470.
fn enum_facts() -> Json {
    json!({
        "same": true,
        "symbol_same": false,
        "cross_enum_same": false,
        "name": "Draft",
        "symbol": {"$symbol": "draft"},
        "render": "status=draft",
        "payload": "{\"status\":\"draft\"}"
    })
}

/// `TestEnumFixtureTypedCalls`, lines 441-471.
const ENUM_IDS: &[&str] = &[
    "enum/enums_fixture/member",
    "enum/enums_fixture/publish",
    "enum/enums_fixture/block_names",
    "enum/enums_fixture/facts",
];

/// `TestBlockErrorCases`, lines 473-495.
const BLOCK_ERROR_IDS: &[&str] = &[
    "reject/blocks/error_cases/each_without_block",
    "reject/blocks/error_cases/map_without_block",
    "success/blocks/error_cases/reduce_empty_without_init",
    "success/blocks/error_cases/reduce_empty_with_init",
];

/// `TestBlockErrorPropagation`, lines 497-501.
const BLOCK_PROPAGATION_IDS: &[&str] = &["reject/blocks/block_error_propagation/explode"];

/// `TestComplexExamplesStress`, lines 503-528.
const STRESS_IDS: &[&str] = &["stress/complex/massive", "stress/complex/chudnovsky"];

/// `TestRuntimeErrorCases`, lines 570-581. Kinds come from the Rust source:
/// `src/ops.rs:117` (Arithmetic for `/` and `%` by zero), `src/sequence.rs:12`
/// (Type for a non-integer index), `src/ops.rs:414` (Type for indexing an int) and
/// `src/vm/call_targets.rs:205` (Name for an unknown member).
const RUNTIME_ERROR_CASES: &[(&str, &str, ErrorKind)] = &[
    (
        "reject/errors/runtime/div_by_zero",
        "div_by_zero",
        ErrorKind::Arithmetic,
    ),
    (
        "reject/errors/runtime/mod_by_zero",
        "mod_by_zero",
        ErrorKind::Arithmetic,
    ),
    (
        "reject/errors/runtime/array_non_integer_index",
        "array_non_integer_index",
        ErrorKind::Type,
    ),
    (
        "reject/errors/runtime/string_non_integer_index",
        "string_non_integer_index",
        ErrorKind::Type,
    ),
    (
        "reject/errors/runtime/index_unsupported_type",
        "index_unsupported_type",
        ErrorKind::Type,
    ),
    (
        "reject/errors/runtime/method_missing",
        "method_missing",
        ErrorKind::Name,
    ),
    (
        "reject/errors/runtime/nil_method",
        "nil_method",
        ErrorKind::Name,
    ),
];

/// `TestTypeErrorCases`, lines 583-593. Kinds: `src/ops.rs:12` (operand mismatch)
/// and `src/types/diagnostics.rs:128` (argument and return annotations).
const TYPE_ERROR_CASES: &[(&str, &str, ErrorKind)] = &[
    (
        "reject/errors/types/sub_mismatch",
        "sub_mismatch",
        ErrorKind::Type,
    ),
    (
        "reject/errors/types/mul_mismatch",
        "mul_mismatch",
        ErrorKind::Type,
    ),
    (
        "reject/errors/types/div_mismatch",
        "div_mismatch",
        ErrorKind::Type,
    ),
    (
        "reject/errors/types/unary_mismatch",
        "unary_mismatch",
        ErrorKind::Type,
    ),
    (
        "reject/errors/types/arg_type_mismatch",
        "arg_type_mismatch",
        ErrorKind::Type,
    ),
    (
        "reject/errors/types/return_type_mismatch",
        "return_type_mismatch",
        ErrorKind::Type,
    ),
];

/// `TestAttributeErrorCases`, lines 595-600. Kind: `src/vm/namespaces.rs:526`.
const ATTRIBUTE_ERROR_CASES: &[(&str, &str, ErrorKind)] = &[(
    "reject/errors/attributes/set_readonly",
    "set_readonly",
    ErrorKind::Argument,
)];

/// `TestYieldErrorCases`, lines 602-613. Kind: `src/vm.rs:1344` (`Error::local_jump`).
const YIELD_IDS: &[&str] = &[
    "reject/errors/yield/yield_without_block",
    "success/errors/yield/run",
];

/// `TestArgumentErrorCases`, lines 615-634. Kind: `src/vm.rs:3526` (`Error::argument`).
const ARGUMENT_IDS: &[&str] = &[
    "reject/errors/arguments/too_few_args",
    "reject/errors/arguments/too_many_args",
    "success/errors/arguments/run",
];

// ---------------------------------------------------------------------------
// Group tests, one per Go test function.
// ---------------------------------------------------------------------------

#[test]
fn complex_examples_compile() {
    let engine = Engine::new();
    let mut covered = Coverage::default();
    for (id, path) in COMPILE_CASES {
        compile(&engine, path);
        covered.record(id);
    }
    covered.finish(COMPILE_CASES.iter().map(|(id, _)| *id));
}

#[test]
fn complex_examples_run() {
    let engine = Engine::new();
    let mut covered = Coverage::default();
    for (id, path, expected) in complex_run_cases() {
        let script = compile(&engine, path);
        let value = call(id, &script, "run", &[], CallOptions::default());
        assert_value(id, &value, &expected);
        covered.record(id);
    }

    // durations/run, lines 78-103. `readable` depends on the real clock.
    {
        let id = "run/complex/durations";
        let script = compile(&engine, "tests/complex/durations.vibe");
        let value = call(id, &script, "run", &[], CallOptions::default());
        let fields = hash_fields(id, &value);
        assert_eq!(
            field(id, &fields, "span").as_int(),
            Some(5400),
            "{id}: span mismatch: {}",
            field(id, &fields, "span")
        );
        assert_eq!(
            string_of(id, field(id, &fields, "iso")),
            "PT1H30M",
            "{id}: iso mismatch"
        );
        let shifted = field(id, &fields, "shifted");
        assert_eq!(
            shifted.as_array().map(<[Value]>::len),
            Some(2),
            "{id}: shifted mismatch: {shifted}"
        );
        let readable = string_of(id, field(id, &fields, "readable"));
        assert!(
            readable.contains(" -> "),
            "{id}: readable mismatch: {readable}"
        );
        covered.record(id);
    }

    // chudnovsky/run, lines 181-202.
    {
        let id = "run/complex/chudnovsky";
        let script = compile(&engine, "tests/complex/chudnovsky.vibe");
        let value = call(id, &script, "run", &[], CallOptions::default());
        let fields = hash_fields(id, &value);
        let _coarse = float_of(id, field(id, &fields, "coarse"));
        let precise = float_of(id, field(id, &fields, "precise"));
        assert!(
            (precise - PI).abs() <= 1e-4,
            "{id}: precise pi off: {precise}"
        );
        covered.record(id);
    }

    covered.finish(
        complex_run_cases()
            .iter()
            .map(|(id, _, _)| *id)
            .chain(COMPLEX_RUN_PREDICATE_IDS.iter().copied()),
    );
}

#[test]
fn program_fixtures() {
    let engine = Engine::new();
    let mut covered = Coverage::default();
    for (id, path, expected) in program_fixture_cases() {
        let script = compile(&engine, path);
        let value = call(id, &script, "run", &[], CallOptions::default());
        assert_value(id, &value, &expected);
        covered.record(id);
    }

    // classes/counter, lines 373-381. The class variable must start at 0 on every
    // call of the same compiled script (docs/compatibility.md, "State isolation
    // across host calls"), so the same expectation is asserted twice.
    {
        let id = COUNTER_FIXTURE_ID;
        let script = compile(&engine, "tests/classes/counter.vibe");
        let expected = json!({"before": 0, "after": 3});
        for attempt in 1..=2 {
            let value = call(id, &script, "run", &[], CallOptions::default());
            assert_value(&format!("{id} (call {attempt})"), &value, &expected);
        }
        covered.record(id);
    }

    covered.finish(
        program_fixture_cases()
            .iter()
            .map(|(id, _, _)| *id)
            .chain([COUNTER_FIXTURE_ID]),
    );
}

#[test]
fn enum_fixture_typed_calls() {
    let mut covered = Coverage::default();
    let script = compile(&Engine::new(), "tests/enums_fixture.vibe");

    // Go compares against members read from the same compiled script with
    // Value.Equal; the host-visible equivalent is the (enum, member, symbol) triple.
    let id = "enum/enums_fixture/member";
    let draft = call(id, &script, "member", &[], CallOptions::default());
    assert_value(id, &draft, &json!({"$enum": ["Status", "Draft", "draft"]}));
    covered.record(id);

    let id = "enum/enums_fixture/publish";
    let published = call(
        id,
        &script,
        "publish",
        &[Value::symbol("published")],
        CallOptions::default(),
    );
    assert_value(
        id,
        &published,
        &json!({"$enum": ["Status", "Published", "published"]}),
    );
    covered.record(id);

    // The second element is the live member returned by `publish` above.
    let id = "enum/enums_fixture/block_names";
    let names = call(
        id,
        &script,
        "block_names",
        &[Value::array(vec![
            Value::symbol("draft"),
            published.clone(),
        ])],
        CallOptions::default(),
    );
    assert_value(id, &names, &json!(["Draft", "Published"]));
    covered.record(id);

    let id = "enum/enums_fixture/facts";
    let facts = call(id, &script, "facts", &[], CallOptions::default());
    assert_value(id, &facts, &enum_facts());
    covered.record(id);

    covered.finish(ENUM_IDS.iter().copied());
}

#[test]
fn block_error_cases() {
    let mut covered = Coverage::default();
    let script = compile(&Engine::new(), "tests/blocks/error_cases.vibe");

    // Kind: src/iteration.rs:524 (`argument("<name> requires a block")`).
    for (id, function) in [
        (
            "reject/blocks/error_cases/each_without_block",
            "each_without_block",
        ),
        (
            "reject/blocks/error_cases/map_without_block",
            "map_without_block",
        ),
    ] {
        reject(id, &script, function, &[], ErrorKind::Argument);
        covered.record(id);
    }

    // An empty array with no initial value folds to nil rather than raising
    // (driver comment, lines 480-481).
    let id = "success/blocks/error_cases/reduce_empty_without_init";
    let value = call(
        id,
        &script,
        "reduce_empty_without_init",
        &[],
        CallOptions::default(),
    );
    assert_value(id, &value, &json!(null));
    covered.record(id);

    let id = "success/blocks/error_cases/reduce_empty_with_init";
    let value = call(
        id,
        &script,
        "reduce_empty_with_init",
        &[],
        CallOptions::default(),
    );
    assert_value(id, &value, &json!(10));
    covered.record(id);

    covered.finish(BLOCK_ERROR_IDS.iter().copied());
}

#[test]
fn block_error_propagation() {
    let mut covered = Coverage::default();
    let script = compile(&Engine::new(), "tests/blocks/block_error_propagation.vibe");
    // An unknown int member raised inside a map block (src/vm/call_targets.rs:205).
    let id = "reject/blocks/block_error_propagation/explode";
    reject(id, &script, "explode", &[], ErrorKind::Name);
    covered.record(id);
    covered.finish(BLOCK_PROPAGATION_IDS.iter().copied());
}

#[test]
fn complex_examples_stress() {
    let engine = Engine::new();
    let mut covered = Coverage::default();

    let id = "stress/complex/massive";
    let massive = compile(&engine, "tests/complex/massive.vibe");
    let value = call(id, &massive, "run", &[], high_quota());
    assert_eq!(
        value.as_int(),
        Some(31_375),
        "{id}: unexpected massive sum: {value}"
    );
    covered.record(id);

    // Fifty sequential calls on one compiled script, each with a fresh quota.
    let id = "stress/complex/chudnovsky";
    let pi = compile(&engine, "tests/complex/chudnovsky.vibe");
    for i in 0..50 {
        let value = pi
            .call("pi_approx_precise", &[Value::int(5_000)], high_quota())
            .unwrap_or_else(|e| panic!("{id}: pi_approx_precise run {i} failed: {e}"))
            .value;
        let approximation = float_of(&format!("{id} (run {i})"), &value);
        assert!(
            (approximation - PI).abs() <= 1e-6,
            "{id}: run {i}: pi approximation off: {approximation}"
        );
    }
    covered.record(id);

    covered.finish(STRESS_IDS.iter().copied());
}

#[test]
fn runtime_error_cases() {
    rejection_group("tests/errors/runtime.vibe", RUNTIME_ERROR_CASES);
}

#[test]
fn type_error_cases() {
    let mut covered = Coverage::default();
    let script = compile(&Engine::new(), "tests/errors/types.vibe");
    for (id, function, kind) in TYPE_ERROR_CASES {
        // Line 591 passes the host string "wrong" to `arg_type_mismatch(n: int)`.
        let args = if *function == "arg_type_mismatch" {
            vec![Value::bytes("wrong")]
        } else {
            Vec::new()
        };
        reject(id, &script, function, &args, *kind);
        covered.record(id);
    }
    covered.finish(TYPE_ERROR_CASES.iter().map(|(id, _, _)| *id));
}

#[test]
fn attribute_error_cases() {
    rejection_group("tests/errors/attributes.vibe", ATTRIBUTE_ERROR_CASES);
}

#[test]
fn yield_error_cases() {
    let mut covered = Coverage::default();
    let script = compile(&Engine::new(), "tests/errors/yield.vibe");

    let id = "reject/errors/yield/yield_without_block";
    reject(id, &script, "yield_without_block", &[], ErrorKind::Argument);
    covered.record(id);

    let id = "success/errors/yield/run";
    let value = call(id, &script, "run", &[], CallOptions::default());
    assert_value(id, &value, &json!({"count": 3}));
    covered.record(id);

    covered.finish(YIELD_IDS.iter().copied());
}

#[test]
fn argument_error_cases() {
    let mut covered = Coverage::default();
    let script = compile(&Engine::new(), "tests/errors/arguments.vibe");

    for (id, function) in [
        ("reject/errors/arguments/too_few_args", "too_few_args"),
        ("reject/errors/arguments/too_many_args", "too_many_args"),
    ] {
        reject(id, &script, function, &[], ErrorKind::Argument);
        covered.record(id);
    }

    // Lines 623-633 assert only `a` and `b`; `c` and `d` are not pinned by the driver.
    let id = "success/errors/arguments/run";
    let value = call(id, &script, "run", &[], CallOptions::default());
    let fields = hash_fields(id, &value);
    assert_eq!(
        field(id, &fields, "a").as_int(),
        Some(15),
        "{id}: a mismatch"
    );
    assert_eq!(
        field(id, &fields, "b").as_int(),
        Some(25),
        "{id}: b mismatch"
    );
    covered.record(id);

    covered.finish(ARGUMENT_IDS.iter().copied());
}

/// `TestAllVibeFilesCompileAndRun`, lines 530-568: every pinned `tests/` program
/// compiles, and those declaring a top-level `run` execute it at the high quota.
/// Compile-only programs are listed explicitly in the manifest, cross-checked
/// against the source text and against the runtime's missing-function error.
#[test]
fn all_vibe_files_compile_and_run() {
    let programs = manifest();
    let pinned = pinned_test_paths();
    let listed: BTreeSet<String> = programs.iter().map(|p| p.path.clone()).collect();
    assert_eq!(
        pinned, listed,
        "tests/ entries in sources.json must equal the programs in driver.json"
    );

    let engine = Engine::new();
    let mut covered = Coverage::default();
    for program in &programs {
        let id = format!("all/{}", program.stem);
        let text = source(&program.path);
        let declares_run = text
            .lines()
            .any(|line| line == "def run" || line.starts_with("def run("));
        assert_eq!(
            declares_run, program.defines_run,
            "{}: manifest defines_run disagrees with the source text",
            program.path
        );
        let script = compile(&engine, &program.path);
        if program.defines_run {
            call(&id, &script, "run", &[], high_quota());
        } else {
            let error = match script.call("run", &[], high_quota()) {
                Ok(outcome) => panic!(
                    "{id}: compile-only program unexpectedly ran: {}",
                    outcome.value
                ),
                Err(error) => error,
            };
            assert_eq!(error.kind, ErrorKind::Name, "{id}: {error}");
            assert_eq!(error.message, "function run not found", "{id}: {error}");
        }
        covered.record(&id);
    }
    covered.finish(programs.iter().map(|p| format!("all/{}", p.stem)));
}

/// Mechanical audit: the manifest lists 39 programs and 105 invocations, every
/// program has exactly one walk invocation, and the identifiers declared by the
/// group tests above are exactly the manifest identifiers.
#[test]
fn driver_manifest_is_fully_covered() {
    let programs = manifest();
    assert_eq!(programs.len(), PROGRAM_TOTAL);

    let mut manifest_ids = BTreeSet::new();
    for program in &programs {
        assert_eq!(program.stem, stem_of(&program.path), "{}", program.path);
        let walk: Vec<&String> = program
            .invocations
            .keys()
            .filter(|id| id.starts_with("all/"))
            .collect();
        assert_eq!(
            walk,
            vec![&format!("all/{}", program.stem)],
            "{}",
            program.path
        );
        for (id, evidence) in &program.invocations {
            let (class, rest) = id
                .split_once('/')
                .unwrap_or_else(|| panic!("malformed id {id}"));
            assert!(
                matches!(
                    class,
                    "compile"
                        | "run"
                        | "fixture"
                        | "enum"
                        | "reject"
                        | "success"
                        | "stress"
                        | "all"
                ),
                "{id}: unknown class"
            );
            assert!(
                rest == program.stem || rest.starts_with(&format!("{}/", program.stem)),
                "{id}: does not belong to {}",
                program.path
            );
            assert!(
                evidence.starts_with("integration_test.go:"),
                "{id}: evidence must cite the Go driver"
            );
            assert!(
                manifest_ids.insert(id.clone()),
                "duplicate manifest id {id}"
            );
        }
    }
    assert_eq!(manifest_ids.len(), MANIFEST_TOTAL);

    let declared = declared_ids(&programs);
    let missing: Vec<_> = manifest_ids.difference(&declared).collect();
    let extra: Vec<_> = declared.difference(&manifest_ids).collect();
    assert!(
        missing.is_empty() && extra.is_empty(),
        "manifest ids without a native assertion: {missing:?}; assertions without a manifest id: {extra:?}"
    );
}

/// Every identifier asserted by a group test above.
fn declared_ids(programs: &[Program]) -> BTreeSet<String> {
    let mut ids = BTreeSet::new();
    let mut declare = |id: &str| assert!(ids.insert(id.to_string()), "id declared twice: {id}");
    for (id, _) in COMPILE_CASES {
        declare(id);
    }
    for (id, _, _) in complex_run_cases() {
        declare(id);
    }
    for id in COMPLEX_RUN_PREDICATE_IDS {
        declare(id);
    }
    for (id, _, _) in program_fixture_cases() {
        declare(id);
    }
    declare(COUNTER_FIXTURE_ID);
    for id in ENUM_IDS
        .iter()
        .chain(BLOCK_ERROR_IDS)
        .chain(BLOCK_PROPAGATION_IDS)
        .chain(STRESS_IDS)
        .chain(YIELD_IDS)
        .chain(ARGUMENT_IDS)
    {
        declare(id);
    }
    for (id, _, _) in RUNTIME_ERROR_CASES
        .iter()
        .chain(TYPE_ERROR_CASES)
        .chain(ATTRIBUTE_ERROR_CASES)
    {
        declare(id);
    }
    for program in programs {
        declare(&format!("all/{}", program.stem));
    }
    ids
}

// ---------------------------------------------------------------------------
// Helpers.
// ---------------------------------------------------------------------------

/// Records which driver invocations a group test asserted.
#[derive(Default)]
struct Coverage(BTreeSet<String>);

impl Coverage {
    fn record(&mut self, id: &str) {
        self.0.insert(id.to_string());
    }

    fn finish<I>(self, expected: I)
    where
        I: IntoIterator,
        I::Item: Into<String>,
    {
        let expected: BTreeSet<String> = expected.into_iter().map(Into::into).collect();
        assert_eq!(
            self.0, expected,
            "asserted invocations differ from the group's declared invocations"
        );
    }
}

struct Program {
    path: String,
    stem: String,
    defines_run: bool,
    invocations: BTreeMap<String, String>,
}

fn upstream_root() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/upstream")
}

fn source(path: &str) -> String {
    let full = upstream_root().join(path);
    fs::read_to_string(&full).unwrap_or_else(|e| panic!("{}: {e}", full.display()))
}

fn compile(engine: &Engine, path: &str) -> Script {
    engine
        .compile(&source(path))
        .unwrap_or_else(|e| panic!("{path}: compile failed: {e}"))
}

fn high_quota() -> CallOptions {
    CallOptions {
        limits: Limits {
            steps: Some(HIGH_QUOTA),
            ..Limits::default()
        },
        ..CallOptions::default()
    }
}

fn call(id: &str, script: &Script, function: &str, args: &[Value], options: CallOptions) -> Value {
    script
        .call(function, args, options)
        .unwrap_or_else(|e| panic!("{id}: unexpected error: {e}"))
        .value
}

/// Requires a call-time rejection of the given runtime kind. Quota, cancellation,
/// syntax, host and control-flow failures never satisfy a rejection.
fn reject(id: &str, script: &Script, function: &str, args: &[Value], kind: ErrorKind) {
    let error = match script.call(function, args, CallOptions::default()) {
        Ok(outcome) => panic!("{id}: expected a rejection, got {}", outcome.value),
        Err(error) => error,
    };
    assert!(
        !matches!(
            error.kind,
            ErrorKind::Steps
                | ErrorKind::Memory
                | ErrorKind::Recursion
                | ErrorKind::OutputLimit
                | ErrorKind::Cancelled
                | ErrorKind::Deadline
                | ErrorKind::Syntax
                | ErrorKind::Host
                | ErrorKind::ControlFlow
        ) && error.class().is_some(),
        "{id}: expected a script runtime error, got {:?}: {error}",
        error.kind
    );
    assert_eq!(error.kind, kind, "{id}: {error}");
}

fn rejection_group(path: &str, cases: &[(&str, &str, ErrorKind)]) {
    let mut covered = Coverage::default();
    let script = compile(&Engine::new(), path);
    for (id, function, kind) in cases {
        reject(id, &script, function, &[], *kind);
        covered.record(id);
    }
    covered.finish(cases.iter().map(|(id, _, _)| *id));
}

fn hash_fields(id: &str, value: &Value) -> BTreeMap<String, Value> {
    assert_eq!(
        value.type_name(),
        "hash",
        "{id}: expected hash, got {} {value}",
        value.type_name()
    );
    let mut fields = BTreeMap::new();
    for (key, entry) in value.as_hash().unwrap() {
        let key = String::from_utf8_lossy(
            key.as_bytes()
                .unwrap_or_else(|| panic!("{id}: non-string hash key {key}")),
        )
        .into_owned();
        assert!(
            fields.insert(key.clone(), entry.clone()).is_none(),
            "{id}: duplicate hash key {key}"
        );
    }
    fields
}

fn field<'a>(id: &str, fields: &'a BTreeMap<String, Value>, key: &str) -> &'a Value {
    fields
        .get(key)
        .unwrap_or_else(|| panic!("{id}: missing key {key}"))
}

fn string_of(id: &str, value: &Value) -> String {
    assert_eq!(
        value.type_name(),
        "string",
        "{id}: expected string, got {} {value}",
        value.type_name()
    );
    String::from_utf8_lossy(value.as_bytes().unwrap()).into_owned()
}

fn float_of(id: &str, value: &Value) -> f64 {
    assert_eq!(
        value.type_name(),
        "float",
        "{id}: expected float, got {} {value}",
        value.type_name()
    );
    value.as_float().unwrap()
}

fn assert_value(id: &str, actual: &Value, expected: &Json) {
    if let Some(problem) = difference(actual, expected, "$") {
        panic!("{id}: {problem}\n  actual:   {actual}\n  expected: {expected}");
    }
}

/// Returns the first difference between a value and its JSON-notation expectation.
fn difference(actual: &Value, expected: &Json, path: &str) -> Option<String> {
    let kind = actual.type_name();
    let mismatch = |want: String| Some(format!("{path}: expected {want}, got {kind} {actual}"));
    match expected {
        Json::Null => (kind != "nil").then(|| mismatch("nil".into())).flatten(),
        Json::Bool(b) => (kind != "bool" || actual.truthy() != *b)
            .then(|| mismatch(format!("bool {b}")))
            .flatten(),
        Json::Number(n) => {
            if let Some(i) = n.as_i64() {
                (actual.as_int() != Some(i))
                    .then(|| mismatch(format!("int {i}")))
                    .flatten()
            } else if let Some(f) = n.as_f64() {
                // NaN never compares equal, so a NaN expectation or result fails.
                (kind != "float" || actual.as_float() != Some(f))
                    .then(|| mismatch(format!("float {f}")))
                    .flatten()
            } else {
                panic!("{path}: unsupported numeric expectation {n}")
            }
        }
        Json::String(s) => (kind != "string" || actual.as_bytes() != Some(s.as_bytes()))
            .then(|| mismatch(format!("string {s:?}")))
            .flatten(),
        Json::Array(items) => {
            let Some(values) = actual.as_array() else {
                return mismatch("array".into());
            };
            if values.len() != items.len() {
                return mismatch(format!("array of length {}", items.len()));
            }
            values
                .iter()
                .zip(items)
                .enumerate()
                .find_map(|(i, (value, item))| difference(value, item, &format!("{path}[{i}]")))
        }
        Json::Object(map) => {
            if let Some((tag, spec)) = typed_tag(map) {
                return typed_difference(actual, tag, spec, path);
            }
            if kind != "hash" {
                return mismatch("hash".into());
            }
            let mut entries: BTreeMap<Vec<u8>, &Value> = BTreeMap::new();
            for (key, value) in actual.as_hash().unwrap() {
                let Some(bytes) = key.as_bytes() else {
                    return Some(format!("{path}: non-string hash key {key}"));
                };
                if entries.insert(bytes.to_vec(), value).is_some() {
                    return Some(format!("{path}: duplicate hash key {key}"));
                }
            }
            let actual_keys: BTreeSet<&[u8]> = entries.keys().map(Vec::as_slice).collect();
            let expected_keys: BTreeSet<&[u8]> = map.keys().map(String::as_bytes).collect();
            if actual_keys != expected_keys {
                let show = |keys: &BTreeSet<&[u8]>| {
                    keys.iter()
                        .map(|k| String::from_utf8_lossy(k).into_owned())
                        .collect::<Vec<_>>()
                };
                return Some(format!(
                    "{path}: hash keys {:?} differ from expected {:?}",
                    show(&actual_keys),
                    show(&expected_keys)
                ));
            }
            map.iter().find_map(|(key, item)| {
                difference(entries[key.as_bytes()], item, &format!("{path}.{key}"))
            })
        }
    }
}

/// Recognizes one-key `$tag` objects in the expectation notation.
fn typed_tag(map: &serde_json::Map<String, Json>) -> Option<(&str, &Json)> {
    if map.len() != 1 {
        return None;
    }
    let (key, spec) = map.iter().next().unwrap();
    key.strip_prefix('$').map(|tag| (tag, spec))
}

fn typed_difference(actual: &Value, tag: &str, spec: &Json, path: &str) -> Option<String> {
    let kind = actual.type_name();
    match tag {
        "symbol" => {
            let name = spec_text(spec, path);
            (kind != "symbol" || actual.as_bytes() != Some(name.as_bytes()))
                .then(|| format!("{path}: expected symbol :{name}, got {kind} {actual}"))
        }
        "money" => {
            let parts = spec_parts(spec, 2, path);
            let (currency, cents) = (spec_text(&parts[0], path), spec_integer(&parts[1], path));
            (actual.as_money() != Some((cents, currency))).then(|| {
                format!("{path}: expected money {cents} cents {currency}, got {kind} {actual}")
            })
        }
        "time" => {
            let parts = spec_parts(spec, 2, path);
            let (seconds, nanos) = (spec_integer(&parts[0], path), spec_integer(&parts[1], path));
            let nanos = u32::try_from(nanos).unwrap_or_else(|_| panic!("{path}: bad nanos"));
            (kind != "time" || actual.as_time() != Some((seconds, nanos)))
                .then(|| format!("{path}: expected time {seconds}s {nanos}ns, got {kind} {actual}"))
        }
        "enum" => {
            let parts = spec_parts(spec, 3, path);
            let (enumeration, member, symbol) = (
                spec_text(&parts[0], path),
                spec_text(&parts[1], path),
                spec_text(&parts[2], path),
            );
            (actual.as_enum_member() != Some((enumeration, member, symbol))).then(|| {
                format!(
                    "{path}: expected enum member {enumeration}::{member} (:{symbol}), got {kind} {actual}"
                )
            })
        }
        other => panic!("{path}: unsupported expectation tag ${other}"),
    }
}

fn spec_parts<'a>(spec: &'a Json, count: usize, path: &str) -> &'a [Json] {
    let items = spec
        .as_array()
        .unwrap_or_else(|| panic!("{path}: typed expectation expects an array"));
    assert_eq!(
        items.len(),
        count,
        "{path}: typed expectation expects {count} parts"
    );
    items
}

fn spec_text<'a>(value: &'a Json, path: &str) -> &'a str {
    value
        .as_str()
        .unwrap_or_else(|| panic!("{path}: typed expectation expects a string part"))
}

fn spec_integer(value: &Json, path: &str) -> i64 {
    value
        .as_i64()
        .unwrap_or_else(|| panic!("{path}: typed expectation expects an integer part"))
}

fn stem_of(path: &str) -> String {
    path.strip_prefix("tests/")
        .and_then(|rest| rest.strip_suffix(".vibe"))
        .unwrap_or_else(|| panic!("{path}: not a tests/ program"))
        .to_string()
}

/// Reads `tests/upstream/driver.json`.
fn manifest() -> Vec<Program> {
    let text = fs::read(upstream_root().join("driver.json")).unwrap();
    let json: Json = serde_json::from_slice(&text).unwrap();
    json["programs"]
        .as_array()
        .expect("driver.json programs")
        .iter()
        .map(|entry| Program {
            path: entry["path"].as_str().expect("program path").to_string(),
            stem: entry["stem"].as_str().expect("program stem").to_string(),
            defines_run: entry["defines_run"].as_bool().expect("program defines_run"),
            invocations: entry["invocations"]
                .as_object()
                .expect("program invocations")
                .iter()
                .map(|(id, evidence)| {
                    (
                        id.clone(),
                        evidence.as_str().expect("invocation evidence").to_string(),
                    )
                })
                .collect(),
        })
        .collect()
}

/// The `tests/` entries pinned in `tests/upstream/sources.json`.
fn pinned_test_paths() -> BTreeSet<String> {
    let text = fs::read(upstream_root().join("sources.json")).unwrap();
    let json: Json = serde_json::from_slice(&text).unwrap();
    json["files"]
        .as_array()
        .expect("sources.json files")
        .iter()
        .map(|entry| entry["path"].as_str().expect("file path").to_string())
        .filter(|path| path.starts_with("tests/"))
        .collect()
}
