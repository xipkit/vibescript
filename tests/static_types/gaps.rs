//! Regressions for the remaining static checker soundness gaps.

use super::support::{clean, codes};

#[test]
fn defaults_match_parameter_types_in_declaration_order() {
    clean("def f(a: int = 1, b: int = a + 1, *, c: string = \"x\")\nend\n");
    codes("def f(a: int = \"x\")\nend\n", &["V0101"]);
    codes("def f(*, a: int = \"x\")\nend\n", &["V0101"]);
    clean("def f(a: array<int> = [])\nend\n");
}

#[test]
fn loop_and_rescue_bindings_keep_existing_local_types() {
    clean("x = 0\nfor x in [1, 2]\n  x + 1\nend\n");
    codes("x = \"s\"\nfor x in [1, 2]\nend\n", &["V0102"]);
    clean("begin\n  raise \"x\"\nrescue => e\n  e.message\nend\n");
    codes(
        "e = 1\nbegin\n  raise \"x\"\nrescue => e\n  0\nend\n",
        &["V0102"],
    );
}

#[test]
fn control_forms_require_their_runtime_context() {
    clean("while true\n  break\nend\n[1].each { next }\n");
    codes("break\n", &["V0001"]);
    codes("next\n", &["V0001"]);
    clean("class A\n  @@x: int = 1\nend\n");
    codes("@@x = 3\n", &["V0204"]);
    clean("block_given?\n");
    codes("block_given?(1)\n", &["V0301"]);
}
