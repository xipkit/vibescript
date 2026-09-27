//! Advice for familiar foreign names, only after ordinary resolution fails.

use super::{Checker, calls::Call, ty::Kind};
use crate::{
    diagnostic::{Code, Diagnostic, Edit, Fix, Label, Span},
    syntax::{ArgumentKind, CallForm, Expr, Node},
};

const NAMES: &[(&str, &str, &str)] = &[
    (
        "",
        "len",
        "`len(x)` is `x.length` in Vibescript; use `text.bytesize` for a byte count",
    ),
    ("", "str", "`str(x)` is `x.to_s` in Vibescript"),
    (
        "",
        "int",
        "`int(x)` is `x.to_i` in Vibescript; use `text.to_i(base)` for an explicit radix",
    ),
    ("", "float", "`float(x)` is `x.to_f` in Vibescript"),
    (
        "",
        "Integer",
        "`Integer(x)` uses `x.to_i` in Vibescript; check conversion and invalid-input behavior",
    ),
    (
        "",
        "Float",
        "`Float(x)` uses `x.to_f` in Vibescript; check invalid-input behavior",
    ),
    ("", "String", "`String(x)` is `x.to_s` in Vibescript"),
    (
        "",
        "sorted",
        "`sorted(items)` is `items.sort` in Vibescript; use `items.sort_by { |item| key }` for a key",
    ),
    (
        "",
        "reversed",
        "`reversed(items)` uses `items.reverse` in Vibescript",
    ),
    (
        "",
        "enumerate",
        "`enumerate(items)` uses `items.each_with_index { |item, index| ... }` in Vibescript",
    ),
    (
        "",
        "zip",
        "`zip(a, b)` uses `a.zip(b)` in Vibescript; check unequal-length behavior",
    ),
    ("", "sum", "`sum(items)` is `items.sum` in Vibescript"),
    (
        "",
        "min",
        "`min(items)` is `items.min` in Vibescript; the result may be nil",
    ),
    (
        "",
        "max",
        "`max(items)` is `items.max` in Vibescript; the result may be nil",
    ),
    ("", "abs", "`abs(x)` is `x.abs` in Vibescript"),
    (
        "",
        "round",
        "`round(x)` uses `x.round` in Vibescript; check rounding and precision behavior",
    ),
    (
        "",
        "range",
        "`range(n)` uses `0...n` in Vibescript; use explicit bounds and `step` for other forms",
    ),
    (
        "",
        "isinstance",
        "`isinstance(x, T)` uses `x.is_type?(:type_name)` in Vibescript, for example `x.is_type?(:string)`",
    ),
    (
        "",
        "parseInt",
        "`parseInt(text)` uses `text.to_i` in Vibescript; use `text.to_i(base)` for an explicit radix",
    ),
    (
        "",
        "parseFloat",
        "`parseFloat(text)` uses `text.to_f` in Vibescript; check invalid-input behavior",
    ),
    (
        "fmt",
        "Sprintf",
        "`fmt.Sprintf(pattern, ...)` is `format(pattern, ...)` in Vibescript; check supported format verbs",
    ),
    (
        "fmt",
        "Println",
        "`fmt.Println(...)` uses `puts ...` in Vibescript; each argument gets its own line",
    ),
    (
        "fmt",
        "Print",
        "`fmt.Print(...)` uses `print ...` in Vibescript; check argument spacing",
    ),
    (
        "strings",
        "ToLower",
        "`strings.ToLower(text)` is `text.downcase` in Vibescript",
    ),
    (
        "strings",
        "ToUpper",
        "`strings.ToUpper(text)` is `text.upcase` in Vibescript",
    ),
    (
        "strings",
        "TrimSpace",
        "`strings.TrimSpace(text)` uses `text.strip` in Vibescript; check the whitespace set",
    ),
    (
        "strings",
        "Contains",
        "`strings.Contains(text, part)` is `text.include?(part)` in Vibescript",
    ),
    (
        "strings",
        "HasPrefix",
        "`strings.HasPrefix(text, prefix)` is `text.start_with?(prefix)` in Vibescript",
    ),
    (
        "strings",
        "HasSuffix",
        "`strings.HasSuffix(text, suffix)` is `text.end_with?(suffix)` in Vibescript",
    ),
    (
        "strings",
        "Split",
        "`strings.Split(text, separator)` uses `text.split(separator)` in Vibescript; check empty-field behavior",
    ),
    (
        "strings",
        "Join",
        "`strings.Join(items, separator)` is `items.join(separator)` in Vibescript",
    ),
    (
        "strings",
        "ReplaceAll",
        "`strings.ReplaceAll(text, old, new)` is `text.gsub(old, new)` in Vibescript",
    ),
    (
        "strconv",
        "Atoi",
        "`strconv.Atoi(text)` uses `text.to_i` in Vibescript; check invalid-input behavior",
    ),
    (
        "strconv",
        "Itoa",
        "`strconv.Itoa(n)` is `n.to_s` in Vibescript",
    ),
    (
        "json",
        "loads",
        "`json.loads(text)` is `JSON.parse(text)` in Vibescript; prefer `JSON.parse_as(text, T)` for known shapes",
    ),
    (
        "json",
        "dumps",
        "`json.dumps(value)` is `JSON.stringify(value)` in Vibescript",
    ),
    (
        "console",
        "log",
        "`console.log(...)` uses `puts ...` in Vibescript; each argument gets its own line",
    ),
    (
        "Object",
        "keys",
        "`Object.keys(value)` is `value.keys` in Vibescript for hashes and records",
    ),
    (
        "Object",
        "values",
        "`Object.values(value)` is `value.values` in Vibescript for hashes and records",
    ),
];

fn note(scope: &str, name: &str) -> Option<&'static str> {
    NAMES.iter().find_map(|&(namespace, foreign, note)| {
        (namespace == scope && foreign == name).then_some(note)
    })
}

impl<'a> Checker<'a> {
    /// Reports an unresolved call with foreign-name advice and a safe length edit.
    pub(super) fn foreign_call(
        &mut self,
        expr: &'a Expr,
        call: &Call<'a, '_>,
        mut diagnostic: Diagnostic,
    ) {
        if let Some(note) = note("", call.name) {
            diagnostic = diagnostic.with_label(call.name_span, note);
        }
        if let ("len", [arg], None) = (call.name, call.args, call.block) {
            let ty = self.expr(&arg.value, None);
            if matches!(arg.kind, ArgumentKind::Positional)
                && matches!(
                    self.types.kind(ty),
                    Kind::Array(_) | Kind::Tuple(_) | Kind::Hash(_) | Kind::Shape(..)
                )
            {
                if let Node::Call(_, _, form) = &expr.node {
                    // Keep the member inside an outer parenless call's argument.
                    let (prefix, suffix) = if *form == CallForm::Parenthesized {
                        ("(", ".length)")
                    } else {
                        ("((", ").length)")
                    };
                    diagnostic = diagnostic.with_fix(Fix::edits(
                        "read the collection's `length`",
                        vec![
                            Edit {
                                span: call.name_span,
                                replacement: prefix.into(),
                            },
                            Edit {
                                span: Span::at(self.spans.expr(expr).end),
                                replacement: suffix.into(),
                            },
                        ],
                    ));
                }
            }
        } else {
            self.loose_args(call);
        }
        self.report(diagnostic);
    }

    /// Adds advice to the unresolved root of a familiar namespace operation.
    pub(super) fn foreign_namespace(&mut self, receiver: &'a Expr, member: &str) {
        let Node::Var(namespace) = &receiver.node else {
            return;
        };
        let Some(note) = note(namespace, member) else {
            return;
        };
        let span = self.spans.token(receiver.offset as usize);
        if let Some(diagnostic) =
            self.diagnostics.iter_mut().rev().find(|diagnostic| {
                diagnostic.code == Code::UNDEFINED_NAME && diagnostic.span == span
            })
        {
            diagnostic.labels.push(Label {
                span,
                message: note.into(),
                file: None,
            });
        }
    }
}
