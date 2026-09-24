//! `vibes test`, ported from the Go reference's test_command_test.go.

// These tests run the vibes binary as a subprocess, which WASI cannot spawn.
#![cfg(not(target_os = "wasi"))]

mod support;
use support::{Files, vibes, vibes_in};

#[test]
fn reports_passing_and_failing_tests() {
    let files = Files::new();
    files.write(
        "math_test.vibe",
        "def test_addition()\n  assert 1 + 2 == 3\nend\n\n\
         def test_subtraction()\n  assert 5 - 2 == 3, \"subtraction is broken\"\nend\n\n\
         def helper()\n  \"not a test\"\nend\n",
    );
    files.write(
        "broken_test.vibe",
        "def test_failure()\n  assert 1 == 2, \"one is not two\"\nend\n",
    );
    let dir = files.0.to_str().unwrap();
    let run = vibes(&["test", dir]);
    assert_eq!(run.status, Some(1));
    assert_eq!(
        run.stdout,
        format!(
            "--- FAIL: {dir}/broken_test.vibe :: test_failure\n    one is not two\n      \
             --> line 2, column 3\n     2 |   assert 1 == 2, \"one is not two\"\n       |   ^\n      \
             at test_failure (2:3)\n      at test_failure (1:1)\n\
             ok   {dir}/math_test.vibe (2 test(s))\n\
             3 test(s) across 2 file(s): 2 passed, 1 failed\n"
        )
    );
    assert_eq!(run.stderr, "vibes test: 1 test(s) failed\n");
    assert!(!run.stdout.contains("helper"));
}

#[test]
fn passing_suites_exit_cleanly_and_share_the_output_streams() {
    let files = Files::new();
    files.write(
        "prints_test.vibe",
        "def test_prints()\n  puts \"hello from a test\"\n  warn \"careful\"\n  assert true\nend\n",
    );
    vibes_in(Some(&files.0), &["test"]).expect(
        0,
        "hello from a test\nok   prints_test.vibe (1 test(s))\n\
         1 test(s) across 1 file(s): 1 passed, 0 failed\n",
        "careful\n",
    );
}

#[test]
fn failures_show_the_assertion_position() {
    let files = Files::new();
    files.write(
        "pos_test.vibe",
        "def test_position()\n  value = 1\n  assert value == 2\nend\n",
    );
    let run = vibes_in(Some(&files.0), &["test", "."]);
    assert_eq!(run.status, Some(1));
    assert!(run.stdout.contains("line 3"), "{}", run.stdout);
}

#[test]
fn filters_tests_by_regular_expression() {
    let files = Files::new();
    files.write(
        "filter_test.vibe",
        "def test_alpha()\n  assert true\nend\n\ndef test_beta()\n  assert false, \"beta always fails\"\nend\n",
    );
    let dir = Some(files.0.as_path());
    vibes_in(dir, &["test", "-run", "alpha", "."]).expect(
        0,
        "ok   filter_test.vibe (1 test(s))\n1 test(s) across 1 file(s): 1 passed, 0 failed\n",
        "",
    );
    vibes_in(dir, &["test", "-run", "zzz"]).expect(
        0,
        "ok   filter_test.vibe (no test functions)\n0 test(s) across 1 file(s): 0 passed, 0 failed\n",
        "",
    );
    vibes_in(dir, &["test", "-run", "[", "."])
        .fails("vibes test: invalid -run pattern: error parsing regexp: missing closing ]: `[`");
}

#[test]
fn discovers_nested_files_and_resolves_modules() {
    let files = Files::new();
    files.write("helper.vibe", "def double(n)\n  n * 2\nend\n");
    files.write(
        "nested/deep_test.vibe",
        "def test_double()\n  helper = require(\"helper\")\n  assert helper.double(2) == 4\nend\n",
    );
    files.write("nested/sibling.vibe", "def triple(n)\n  n * 3\nend\n");
    files.write(
        "nested/sibling_test.vibe",
        "def test_triple()\n  helper = require(\"sibling\")\n  assert helper.triple(3) == 9\nend\n",
    );
    let dir = files.0.to_str().unwrap();
    vibes(&["test", "-module-path", dir, dir]).expect(
        0,
        &format!(
            "ok   {dir}/nested/deep_test.vibe (1 test(s))\n\
             ok   {dir}/nested/sibling_test.vibe (1 test(s))\n\
             2 test(s) across 2 file(s): 2 passed, 0 failed\n"
        ),
        "",
    );
}

#[test]
fn reports_files_that_cannot_run() {
    let files = Files::new();
    files.write("bad_test.vibe", "def test_oops(\n");
    files.write(
        "top_test.vibe",
        "puts \"top\"\ndef test_a\n  assert true\nend\n",
    );
    files.write("assign_test.vibe", "x = 1\n");
    files.write(
        "params_test.vibe",
        "def test_needs_arg(value)\n  assert value\nend\n\ndef test_default_ok(value = 1)\n  assert value == 1\nend\n",
    );
    let run = vibes_in(Some(&files.0), &["test"]);
    assert_eq!(run.status, Some(1));
    assert_eq!(
        run.stdout,
        "--- FAIL: assign_test.vibe :: (compile)\n    unsupported top-level statement *ast.AssignStmt\n\
         --- FAIL: bad_test.vibe :: (compile)\n    parse error at 2:0: expected name\n      \
         --> line 2, column 1\n     2 | \n       | ^\n\
         --- FAIL: params_test.vibe :: test_needs_arg\n    test functions must not require parameters\n\
         --- FAIL: top_test.vibe :: (compile)\n    unsupported top-level statement *ast.ExprStmt\n\
         5 test(s) across 4 file(s): 1 passed, 4 failed\n"
    );
    assert_eq!(run.stderr, "vibes test: 4 test(s) failed\n");
}

#[test]
fn discovery_errors_use_the_reference_wording() {
    let files = Files::new();
    let dir = Some(files.0.as_path());
    vibes_in(dir, &["test"]).fails("vibes test: no *_test.vibe files found under .");
    files.write("script.vibe", "def run()\n  1\nend\n");
    vibes_in(dir, &["test", "script.vibe"])
        .fails("vibes test: \"script.vibe\" is not a *_test.vibe file");
    vibes_in(dir, &["test", "missing", "."])
        .fails("vibes test: access \"missing\": stat missing: no such file or directory");
    files.write("ok_test.vibe", "def test_ok\n  assert true\nend\n");
    std::fs::create_dir(files.0.join("sub")).unwrap();
    vibes_in(
        dir,
        &[
            "test",
            "ok_test.vibe",
            "./ok_test.vibe",
            "sub/../ok_test.vibe",
        ],
    )
    .expect(
        0,
        "ok   ok_test.vibe (1 test(s))\n1 test(s) across 1 file(s): 1 passed, 0 failed\n",
        "",
    );
    vibes_in(dir, &["test", "-profile", "bogus", "."]).fails(
        "vibes test: unknown quota profile \"bogus\" (choose one of: low, medium, high, xhigh)",
    );
}
