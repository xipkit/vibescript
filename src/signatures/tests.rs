use super::{Item, Member, ParamKind, Table, Type};

/// Every construct of the signature format, in printed form.
const SAMPLE: &str = "# A sample table.

type ordered = int | string

# Prints.
def show(*values: array<any>)

def pick(from: int = 1, to?: int, strict: bool: = false, name: string:) -> int?

module Ns
  LIMIT: int
  def run<U: ordered>(input: { id: string, \"odd key\"?: U?, ... }, &block?: (U, *any) -> U | nil) -> array<U>
end

class hash<string, V?>
  getter size?: bool
  def compact -> hash<string, V>
  def each(&block: ()) -> :done
end

class T
  def end(group: (int | string)?) -> T
end
";

#[test]
fn signature_files_round_trip() {
    let table = Table::parse(SAMPLE).unwrap();
    assert_eq!(table.to_string(), SAMPLE);
    assert_eq!(table.header, ["A sample table."]);
    let pick = table.function("pick").unwrap();
    let kinds: Vec<_> = pick
        .params
        .iter()
        .map(|param| (param.kind, param.optional))
        .collect();
    assert_eq!(
        kinds,
        [
            (ParamKind::Positional, true),
            (ParamKind::Positional, true),
            (ParamKind::Keyword, true),
            (ParamKind::Keyword, false),
        ]
    );
    assert_eq!(pick.params[0].default.as_deref(), Some("1"));
    let Some(Member::Function(run)) = table.module("Ns").unwrap().member("run") else {
        panic!("run is a function");
    };
    let block = run.block.as_ref().unwrap();
    assert!(block.optional);
    assert_eq!(block.params, [Type::Var("U".into())]);
    assert_eq!(block.rest, Some(Type::name("any")));
    let Some(Item::Class(class)) = table.items.get(4) else {
        panic!("a class");
    };
    assert_eq!(class.base(), "hash");
    assert_eq!(
        class.receiver,
        Type::Name(
            "hash".into(),
            vec![
                Type::name("string"),
                Type::Optional(Box::new(Type::Var("V".into())))
            ]
        )
    );
}

#[test]
fn signature_files_report_malformed_declarations() {
    for (source, line, message) in [
        (
            "def f(a?: int, b: int)",
            1,
            "required parameter b follows an optional one",
        ),
        ("def f(k: int:, a: int)", 1, "parameter a is out of order"),
        (
            "def f(*a: array<int>, *b: array<int>)",
            1,
            "parameter b is out of order",
        ),
        ("def f(a: int, a: int)", 1, "duplicate parameter a"),
        (
            "def f(a?: int = 1)",
            1,
            "a parameter with a default is written `name: T = value`",
        ),
        (
            "def f<t>(a: t)",
            1,
            "a type variable is a single capital letter",
        ),
        (
            "class array<T>\n  def f\n  def f\nend",
            3,
            "duplicate member f",
        ),
        (
            "def f\n# floating\n\ndef g",
            2,
            "a comment must directly precede a declaration",
        ),
        (
            "def f -> int # trailing",
            1,
            "comments go on their own line",
        ),
        ("module M\n  getter x: int\nend", 2, "expected `:`"),
        ("def f(x: \"a)", 1, "unterminated string"),
    ] {
        let error = Table::parse(source).unwrap_err();
        assert_eq!(
            (error.line, error.message.as_str()),
            (line, message),
            "{source}"
        );
    }
}
