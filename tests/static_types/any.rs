//! `any`: values the program cannot know statically. They may be compared,
//! tested, passed where `any` is accepted and narrowed; nothing else.

use super::support::{clean, codes, error};

#[test]
fn any_may_be_compared_tested_and_passed_on() {
    clean("def f(v: any) -> bool\n  v == 1\nend\n");
    clean("def f(v: any) -> bool\n  v == nil\nend\n");
    clean("def f(v: any) -> bool\n  v.is_type?(:int)\nend\n");
    clean("def keep(v: any) -> any\n  v\nend\ndef f(v: any) -> any\n  keep(v)\nend\n");
    clean("def f(v: any)\n  puts v\nend\n");
}

#[test]
fn every_other_use_of_any_is_an_error() {
    let source = "body = JSON.parse(\"{}\")\nbody[\"name\"]\n";
    error(source, "V0106", "type any");
    codes("def f(v: any) -> string\n  v.upcase\nend\n", &["V0106"]);
    codes("def f(v: any) -> int\n  v + 1\nend\n", &["V0106"]);
    codes(
        "def g(n: int) -> int\n  n\nend\ndef f(v: any) -> int\n  g(v)\nend\n",
        &["V0106"],
    );
}

#[test]
fn json_parse_as_gives_the_type_it_names() {
    clean(
        "def name(raw: string) -> string\n  user = JSON.parse_as(raw, { name: string, age?: int })\n  user[\"name\"].upcase\nend\n",
    );
    clean("def count(raw: string) -> int\n  JSON.parse_as(raw, array<int>).length\nend\n");
}

#[test]
fn a_checked_cast_gives_its_type() {
    clean("def f(v: any) -> int\n  v.as(int) + 1\nend\n");
    clean("def f(v: int | string) -> string\n  v.as(string).upcase\nend\n");
    clean("def f(v: any) -> string\n  v.as({ name: string })[\"name\"]\nend\n");
    error(
        "def f(v: int) -> string\n  v.as(string)\nend\n",
        "V0120",
        "can never be string",
    );
}
