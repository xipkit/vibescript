//! Operators, typed by the operator table.

use super::support::{clean, codes};

#[test]
fn arithmetic_on_numbers_strings_arrays_money_durations_and_times() {
    clean(
        "a: int = 1 + 2\nb: float = 1 + 2.5\nc: string = \"a\" + \"b\"\nd: array<int | string> = [1] + [\"a\"]\n",
    );
    clean(
        "e: money = money(\"1.00 USD\") + money(\"2.00 USD\")\nf: duration = 1.hours + 2.minutes\ng: time = Time.now + 1.hours\nh: float = Time.now - Time.now\n",
    );
    clean("i: string = \"ab\" * 3\nj: int = 7 % 2\nk: string = \"%d\" % 3\nl: int = 2 ** 3\n");
    codes("x = \"a\" + 1\n", &["V0108"]);
    codes("x = [1] + 1\n", &["V0108"]);
    codes("x = 1.hours + \"a\"\n", &["V0108"]);
}

#[test]
fn slash_divides_to_a_float_and_floor_division_keeps_integers() {
    clean("half: float = 7 / 2\nratio: float = 7 / 2.0\nn: number = 1\nq: float = n / 2\n");
    clean("total: int = 7 // 2\n");
    codes("half: int = 7 / 2\n", &["V0101"]);
    codes("n = 12\nn /= 5\n", &["V0102"]);
}

#[test]
fn comparisons_take_values_of_one_ordered_kind() {
    clean(
        "a: bool = 1 < 2.5\nb: bool = \"a\" < \"b\"\nc: bool = 1.hours > 2.minutes\nd: int = 1 <=> 2\n",
    );
    codes("x = 1 < \"a\"\n", &["V0108"]);
    clean("x: bool = 1 == \"a\"\n");
}

#[test]
fn shovel_appends_elements_of_the_array_type() {
    clean("xs: array<int> = []\nxs << 1\n");
    codes("xs: array<int> = []\nxs << \"a\"\n", &["V0101"]);
}

#[test]
fn operators_refuse_optional_and_any_operands() {
    codes("def f(n: int?) -> int\n  n * 2\nend\n", &["V0107"]);
    codes("def f(n: any) -> int\n  n * 2\nend\n", &["V0106"]);
}

#[test]
fn a_class_operator_with_a_rest_parameter_takes_its_elements() {
    let class = "class C\n  def +(*values: array<int>) -> int\n    values.length\n  end\nend\n";
    clean(&format!("{class}x: int = C.new + 3\n"));
    codes(&format!("{class}x = C.new + \"a\"\n"), &["V0101"]);
}
