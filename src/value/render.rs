//! Iterative `Display` and `Debug` rendering for nested values.
//!
//! Both traits render containers with an explicit frame stack instead of recursing once
//! per level, so formatting a value near the container limit cannot exhaust the native
//! stack. Neither trait can reach a `CallContext`, so the frame stack is an ordinary
//! `Vec` of one small frame per open container; it is the only unaccounted storage and
//! is bounded by the value's height, never by its width.
//!
//! `Display` output is unchanged: arrays print as `[a, b]`, hashes as `{k: v}`, host
//! objects as `<object>`, and protected hashes print their `to_s` entry. `Debug` keeps
//! the derived `Value(Kind)` shape for leaves and renders containers as
//! `Value(Array([..]))`, `Value(Hash({..}))`, `Value(Hash(Match, {..}))`, or
//! `Value(Object({..}))` without the heap bookkeeping the derived form exposed.

use super::{Kind, Value};
use crate::hash::Tag;
use std::{fmt, slice};

#[derive(Clone, Copy)]
enum Mode {
    Display,
    Debug,
}

enum Items<'a> {
    Array(slice::Iter<'a, Value>),
    Hash(slice::Iter<'a, (Value, Value)>),
}

struct Frame<'a> {
    items: Items<'a>,
    started: bool,
    /// A hash value whose key has been written; rendered after the separator.
    pending: Option<&'a Value>,
    close: &'static str,
}

impl<'a> Frame<'a> {
    fn new(items: Items<'a>, close: &'static str) -> Self {
        Self {
            items,
            started: false,
            pending: None,
            close,
        }
    }
}

pub(super) fn display(value: &Value, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    render(value, f, Mode::Display)
}

pub(super) fn debug(value: &Value, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    render(value, f, Mode::Debug)
}

fn render(root: &Value, f: &mut fmt::Formatter<'_>, mode: Mode) -> fmt::Result {
    let mut frames: Vec<Frame<'_>> = Vec::new();
    let mut next = Some(root);
    loop {
        while let Some(value) = next.take() {
            next = open(value, f, mode, &mut frames)?;
        }
        let Some(frame) = frames.last_mut() else {
            return Ok(());
        };
        if let Some(value) = frame.pending.take() {
            f.write_str(": ")?;
            next = Some(value);
            continue;
        }
        let item = match &mut frame.items {
            Items::Array(items) => items.next().map(|value| (value, None)),
            Items::Hash(items) => items.next().map(|(key, value)| (key, Some(value))),
        };
        match item {
            Some((value, pending)) => {
                if frame.started {
                    f.write_str(", ")?;
                }
                frame.started = true;
                frame.pending = pending;
                next = Some(value);
            }
            None => {
                f.write_str(frame.close)?;
                frames.pop();
            }
        }
    }
}

/// Writes a leaf or a container's opening text; returns a value to render in its place.
fn open<'a>(
    value: &'a Value,
    f: &mut fmt::Formatter<'_>,
    mode: Mode,
    frames: &mut Vec<Frame<'a>>,
) -> Result<Option<&'a Value>, fmt::Error> {
    match (&value.0, mode) {
        (Kind::Array(heap), _) => {
            let (prefix, close) = match mode {
                Mode::Display => ("[", "]"),
                Mode::Debug => ("Value(Array([", "]))"),
            };
            f.write_str(prefix)?;
            frames.push(Frame::new(Items::Array(heap.buffer.data.iter()), close));
            Ok(())
        }
        (Kind::Hash(hash), _) => {
            if matches!(mode, Mode::Display) && hash.tag.protected() {
                if let Some((_, value)) = hash
                    .buffer
                    .data
                    .iter()
                    .find(|(key, _)| key.as_bytes() == Some(b"to_s"))
                {
                    return Ok(Some(value));
                }
            }
            let (prefix, close) = match mode {
                Mode::Display if hash.object => return f.write_str("<object>").map(|()| None),
                Mode::Display => ("{", "}"),
                Mode::Debug if hash.object => ("Value(Object({", "}))"),
                Mode::Debug => (
                    match hash.tag {
                        Tag::None => "Value(Hash({",
                        Tag::Match => "Value(Hash(Match, {",
                        Tag::Error => "Value(Hash(Error, {",
                    },
                    "}))",
                ),
            };
            f.write_str(prefix)?;
            frames.push(Frame::new(Items::Hash(hash.buffer.data.iter()), close));
            Ok(())
        }
        (kind, Mode::Debug) => write!(f, "Value({kind:?})"),
        (Kind::Host(method), Mode::Display) => write!(f, "<builtin {}>", method.name()),
        (Kind::Regex(regex), Mode::Display) => {
            let text = regex
                .text(&mut crate::integer::unlimited_context())
                .map_err(|_| fmt::Error)?;
            f.write_str(
                std::str::from_utf8(text.as_bytes().unwrap_or_default()).map_err(|_| fmt::Error)?,
            )
        }
        (Kind::Shape(shape), Mode::Display) => write!(
            f,
            "<Shape {}>",
            String::from_utf8_lossy(&shape.definition.text)
        ),
        (Kind::Nil, Mode::Display) => f.write_str("nil"),
        (Kind::Money(money), Mode::Display) => write!(f, "{money}"),
        (Kind::Duration(seconds), Mode::Display) => write!(f, "{seconds}s"),
        (Kind::Time(_) | Kind::Zoned(_), Mode::Display) => {
            let mut ctx = crate::integer::unlimited_context();
            let text = crate::time::text(&mut ctx, value, None).map_err(|_| fmt::Error)?;
            f.write_str(
                std::str::from_utf8(text.as_bytes().unwrap_or_default()).map_err(|_| fmt::Error)?,
            )
        }
        (Kind::Instance(instance), Mode::Display) => {
            write!(f, "<{} instance>", instance.class().definition.name)
        }
        (Kind::Namespace(namespace), Mode::Display) => {
            write!(f, "<Class {}>", namespace.definition.name)
        }
        (Kind::Function(function), Mode::Display) => write!(
            f,
            "<function {}>",
            function
                .code
                .program
                .functions
                .get(function.index)
                .map_or("", |function| function.name.as_str())
        ),
        (Kind::Builtin(builtin), Mode::Display) => write!(f, "<builtin {}>", builtin.name()),
        (Kind::Offset(offset), Mode::Display) => write!(f, "<builtin {}>", offset.name()),
        (Kind::Enum(e), Mode::Display) => write!(f, "<Enum {}>", e.definition.name),
        (Kind::EnumMember(m), Mode::Display) => write!(
            f,
            "{}::{}",
            m.enumeration.definition.name,
            m.definition()
                .ok()
                .map_or("", |definition| definition.name.as_str())
        ),
        (Kind::Bool(b), Mode::Display) => write!(f, "{b}"),
        (Kind::Int(n), Mode::Display) => write!(f, "{n}"),
        (Kind::Big(_), Mode::Display) => {
            let mut ctx = crate::integer::unlimited_context();
            let text = crate::integer::format(&mut ctx, value, 10).map_err(|_| fmt::Error)?;
            f.write_str(std::str::from_utf8(&text.data).map_err(|_| fmt::Error)?)
        }
        (Kind::Float(n), Mode::Display) => write!(f, "{n}"),
        (Kind::Range(r), Mode::Display) => write!(f, "{r}"),
        (Kind::Bytes(h) | Kind::Symbol(h), Mode::Display) => {
            write!(f, "{}", String::from_utf8_lossy(h.data()))
        }
    }?;
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn a_protected_record_without_display_text_still_renders() {
        let mut hash = crate::hash::Hash::untracked(Vec::new(), 1);
        hash.tag = Tag::Error;
        let value = Value(Kind::Hash(Arc::new(hash)));
        assert_eq!(value.to_string(), "{}");
        assert_eq!(format!("{value:?}"), "Value(Hash(Error, {}))");
    }

    #[test]
    fn every_leaf_kind_has_its_own_display_text() {
        let script = crate::Engine::new()
            .compile(
                r#"
class Point
end
enum Status
  Draft
end
def helper -> int
  1
end
def run -> any
  [/a.b/i, {x: int}, Point.new, Point, Status, Status::Draft, 123456789012345678901234, 5.seconds]
end
def matched -> any
  "ab".match(/b/)
end
"#,
            )
            .unwrap();
        let value = script
            .call("run", &[], crate::CallOptions::default())
            .unwrap()
            .value;
        assert_eq!(
            value.to_string(),
            "[/a.b/i, <Shape { x: int }>, <Point instance>, <Class Point>, <Enum Status>, \
             Status::Draft, 123456789012345678901234, 5s]"
        );

        let matched = script
            .call("matched", &[], crate::CallOptions::default())
            .unwrap()
            .value;
        let Kind::Hash(matched) = &matched.0 else {
            panic!("match data is a hash");
        };
        let offsets: Vec<String> = matched
            .buffer
            .data
            .iter()
            .filter(|(_, value)| matches!(value.0, Kind::Offset(_)))
            .map(|(_, value)| value.to_string())
            .collect();
        assert_eq!(
            offsets,
            ["<builtin match_data.begin>", "<builtin match_data.end>"]
        );

        let host = crate::HostMethod::new("sms.deliver", |_, _, _| Ok(Value::nil()));
        assert_eq!(host.value().to_string(), "<builtin sms.deliver>");

        let mut ctx = crate::CallContext::new(crate::CallOptions::default());
        let environment = crate::objects::environment(&mut ctx).unwrap();
        let code = script.inner.code.clone();
        let helper = code
            .program
            .functions
            .iter()
            .position(|function| function.name == "helper")
            .unwrap();
        let function = crate::exports::Function::new(&mut ctx, code, environment, helper).unwrap();
        assert_eq!(
            Value(Kind::Function(function)).to_string(),
            "<function helper>"
        );
    }

    #[test]
    fn leaves_render_in_both_modes_inside_containers() {
        let value = Value::array(vec![
            Value::nil(),
            Value::boolean(true),
            Value::int(-3),
            Value::float(1.5),
            Value::bytes("hi"),
            Value::symbol("k"),
            Value::range(Some(1), Some(4), true),
            Value::hash(vec![(b"a".to_vec(), Value::array(Vec::new()))]),
            Value::object(Vec::new()),
        ]);
        assert_eq!(
            value.to_string(),
            "[nil, true, -3, 1.5, hi, k, 1...4, {a: []}, <object>]"
        );
        assert_eq!(
            format!("{value:?}"),
            "Value(Array([Value(Nil), Value(Bool(true)), Value(Int(-3)), Value(Float(1.5)), \
             Value(Bytes([104, 105])), Value(Symbol([107])), \
             Value(Range(Range { start: Some(1), end: Some(4), exclusive: true, charge: None })), \
             Value(Hash({Value(Bytes([97])): Value(Array([]))})), Value(Object({}))]))"
        );
    }
}
