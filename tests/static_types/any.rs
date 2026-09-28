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

/// Every type an annotation can name, as the argument of `as` and of
/// `JSON.parse_as`, with the type a declaration of it has.
const TYPES: [(&str, &str); 32] = [
    ("any", "any"),
    ("int", "int"),
    ("float", "float"),
    ("number", "number"),
    ("string", "string"),
    ("symbol", "symbol"),
    ("bool", "bool"),
    ("nil", "nil"),
    ("duration", "duration"),
    ("time", "time"),
    ("money", "money"),
    ("range", "range"),
    ("regex", "regex"),
    ("match_data", "match_data"),
    ("error", "error"),
    ("type<int>", "type<int>"),
    ("array<int>", "array<int>"),
    ("hash<string, int>", "hash<string, int>"),
    ("comparable", "comparable"),
    ("int?", "int?"),
    ("int | string", "int | string"),
    ("{ a: int }", "{ a: int }"),
    ("[int, string]", "[int, string]"),
    ("Status", "Status"),
    ("Box", "Box"),
    ("Pair", "Pair"),
    // A type literal names the source's classes and enums inside a type.
    ("array<Box>", "array<Box>"),
    ("array<Box?>", "array<Box?>"),
    (
        "{ box: Box, status: Status? }",
        "{ box: Box, status: Status? }",
    ),
    ("hash<string, Status>", "hash<string, Status>"),
    ("[Box, Status]", "[Box, Status]"),
    ("Box | Status", "Box | Status"),
];

const DECLARATIONS: &str = "enum Status\n  Draft\nend\nclass Box\nend\ntype Pair = [int, string]\n";

#[test]
fn every_annotation_type_casts_and_parses_as_itself() {
    for (written, declared) in TYPES {
        clean(&format!(
            "{DECLARATIONS}def cast(v: any) -> {declared}\n  v.as({written})\nend\n"
        ));
        // Only a cast reads `nil` as the type; elsewhere it is the value.
        if written != "nil" {
            clean(&format!(
                "{DECLARATIONS}def parsed(raw: string) -> {declared}\n  JSON.parse_as(raw, {written})\nend\n"
            ));
        }
    }
    // A match stays one after a cast, and indexes as one.
    clean("def first(v: any) -> string?\n  v.as(match_data)[0]\nend\n");
    // A local named like a type is the local.
    clean(
        "def f(error: string) -> string\n  identity(error)\nend\ndef identity(s: string) -> string\n  s\nend\n",
    );
}

#[test]
fn an_array_accepts_values_its_element_type_accepts() {
    clean(
        "def f(v: any, n: int?) -> array<any>\n  a: array<any> = []\n  a << v\n  a << nil\n  a << n\n  a\nend\n",
    );
    codes(
        "def f(xs: array<int>) -> array<int>\n  xs << xs[0]\nend\n",
        &["V0107"],
    );
}
