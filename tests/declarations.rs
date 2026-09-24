use vibescript::{CallOptions, DeclarationKind, Engine};

fn outline(source: &str) -> Vec<(DeclarationKind, String, String)> {
    Engine::new()
        .compile(source)
        .unwrap()
        .declarations()
        .iter()
        .map(|declaration| {
            (
                declaration.kind,
                declaration.name.clone(),
                source[declaration.span.clone()].to_owned(),
            )
        })
        .collect()
}

#[test]
fn declarations_cover_each_top_level_definition_in_source_order() {
    let source = "\
x = 1
def double(n)
  n * 2
end
private def helper; 1; end; y = 2
export def shared
  helper
end
alias twice double
class Invoice
  def total
    7
  end
end # trailing comment
module Rates
  BONUS = 10
end
enum Status
  Open
  Closed
end
if x > 0
  z = double(x)
end
";
    use DeclarationKind::*;
    let expected = [
        (Function, "double", "def double(n)\n  n * 2\nend"),
        (Function, "helper", "private def helper; 1; end"),
        (Function, "shared", "export def shared\n  helper\nend"),
        (Function, "twice", "alias twice double"),
        (
            Class,
            "Invoice",
            "class Invoice\n  def total\n    7\n  end\nend",
        ),
        (Module, "Rates", "module Rates\n  BONUS = 10\nend"),
        (Enum, "Status", "enum Status\n  Open\n  Closed\nend"),
    ];
    let expected: Vec<_> = expected
        .iter()
        .map(|(kind, name, text)| (*kind, name.to_string(), text.to_string()))
        .collect();
    assert_eq!(outline(source), expected);
}

#[test]
fn sources_without_declarations_have_an_empty_outline() {
    assert!(outline("").is_empty());
    assert!(outline("1 + 2\n[1].each do |n|\n  n\nend").is_empty());
}

#[test]
fn declaration_spans_carry_definitions_into_later_scripts() {
    let engine = Engine::new();
    let first = "count = 3\ndef scale(n)\n  n * Tuning::FACTOR\nend\nmodule Tuning\n  FACTOR = 2\nend\n\
                 enum Level\n  Low\n  High\nend";
    let script = engine.compile(first).unwrap();
    let prelude: String = script
        .declarations()
        .iter()
        .map(|declaration| format!("{}\n", &first[declaration.span.clone()]))
        .collect();
    assert!(!prelude.contains("count"));
    let later = engine
        .compile(&format!("{prelude}[scale(5), Level::High]"))
        .unwrap();
    let names: Vec<_> = later
        .declarations()
        .iter()
        .map(|declaration| declaration.name.as_str())
        .collect();
    assert_eq!(names, ["scale", "Tuning", "Level"]);
    let result = later.run(CallOptions::default()).unwrap().value;
    assert_eq!(result.to_string(), "[10, Level::High]");
}
