//! Locals: fixed at their first assignment, typed declarations, definite
//! assignment, no conversions, and literals that need a declared type.

use super::support::{clean, codes, error, spanned};

#[test]
fn a_local_keeps_the_type_of_its_first_assignment() {
    clean("count = 1\ncount = 2\ncount += 3\n");
    let source = "count = 1\ncount = \"one\"\n";
    let diagnostic = error(source, "V0102", "`count` is int");
    assert_eq!(spanned(source, &diagnostic), "\"one\"");
    assert_eq!(diagnostic.expected.as_deref(), Some("int"));
    assert_eq!(diagnostic.found.as_deref(), Some("string"));
    assert_eq!(diagnostic.labels[0].message, "declared here");
}

#[test]
fn a_second_assignment_in_a_branch_loop_or_block_keeps_the_type() {
    codes("x = 1\nif 1 > 2\n  x = \"a\"\nend\n", &["V0102"]);
    codes("x = 1\nwhile x < 3\n  x = 2.5\nend\n", &["V0102"]);
    codes("x = 1\n[1].each { |v| x = \"a\" }\n", &["V0102"]);
}

#[test]
fn assignment_never_converts() {
    codes("total = 1\ntotal = 1.5\n", &["V0102"]);
    clean("total: number = 1\ntotal = 1.5\n");
}

#[test]
fn a_typed_declaration_fixes_a_wider_type() {
    clean("label: string? = nil\nlabel = \"ready\"\n");
    clean("names: array<string> = []\nnames << \"Ada\"\n");
    clean("counts: hash<string, int> = {}\ncounts[\"a\"] = 1\n");
    codes("label: string = nil\n", &["V0101"]);
}

#[test]
fn nil_and_empty_literals_need_a_declared_type() {
    let source = "names = []\n";
    let diagnostic = error(source, "V0103", "does not say what `names` holds");
    assert_eq!(spanned(source, &diagnostic), "names");
    codes("value = nil\n", &["V0103"]);
    codes("counts = {}\n", &["V0103"]);
    codes("rows = [[]]\n", &["V0103"]);
    // Only the literal lacks a type: a value of type nil is fine.
    clean("def nothing\nend\nvalue = nothing\n");
}

#[test]
fn a_local_must_be_assigned_on_every_path_before_it_is_read() {
    let source = "def f(flag: bool) -> int\n  if flag\n    x = 1\n  end\n  x\nend\n";
    let diagnostic = error(source, "V0202", "`x` is not assigned on every path");
    assert_eq!(spanned(source, &diagnostic), "x");
    clean("def f(flag: bool) -> int\n  if flag\n    x = 1\n  else\n    x = 2\n  end\n  x\nend\n");
    clean(
        "def f(flag: bool) -> int\n  if flag\n    x = 1\n  else\n    return 0\n  end\n  x\nend\n",
    );
}

#[test]
fn a_local_first_assigned_in_a_block_is_local_to_it() {
    // Outside the block the name is free again, so it may hold another type.
    clean("[1].each { |v| inner = v }\ninner = \"text\"\n");
    clean("total = 0\n[1, 2].each { |v| total += v }\ntotal\n");
}

#[test]
fn a_loop_may_not_run_so_it_does_not_assign() {
    codes(
        "def f(n: int) -> int\n  while n > 0\n    y = n\n    n -= 1\n  end\n  y\nend\n",
        &["V0202"],
    );
    clean("def f -> int\n  while true\n    y = 1\n    break\n  end\n  y\nend\n");
}

#[test]
fn destructuring_binds_tuple_elements() {
    clean("q, r = 7.divmod(2)\ntotal: int = q + r\n");
    clean("a, b = 1, \"x\"\nc: string = b\nd: int = a\n");
    codes("a, b = 1, \"x\"\nc: int = b\n", &["V0101"]);
}
