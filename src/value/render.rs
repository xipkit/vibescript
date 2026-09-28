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
    match &value.0 {
        Kind::Array(heap) => {
            let (prefix, close) = match mode {
                Mode::Display => ("[", "]"),
                Mode::Debug => ("Value(Array([", "]))"),
            };
            f.write_str(prefix)?;
            frames.push(Frame::new(Items::Array(heap.buffer.data.iter()), close));
            Ok(None)
        }
        Kind::Hash(hash) => {
            let (prefix, close) = match mode {
                Mode::Display if hash.tag.protected() => {
                    let entry = hash
                        .buffer
                        .data
                        .iter()
                        .find(|(key, _)| key.as_bytes() == Some(b"to_s"))
                        .unwrap();
                    return Ok(Some(&entry.1));
                }
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
            Ok(None)
        }
        _ => {
            match mode {
                Mode::Display => scalar(value, f)?,
                Mode::Debug => write!(f, "Value({:?})", value.0)?,
            }
            Ok(None)
        }
    }
}

fn scalar(value: &Value, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match &value.0 {
        Kind::Host(method) => write!(f, "<builtin {}>", method.name()),
        Kind::Regex(regex) => {
            let text = regex
                .text(&mut crate::integer::unlimited_context())
                .map_err(|_| fmt::Error)?;
            f.write_str(std::str::from_utf8(text.as_bytes().unwrap()).map_err(|_| fmt::Error)?)
        }
        Kind::Shape(shape) => write!(
            f,
            "<Shape {}>",
            String::from_utf8_lossy(&shape.definition.text)
        ),
        Kind::Nil => f.write_str("nil"),
        Kind::Money(money) => write!(f, "{money}"),
        Kind::Duration(seconds) => write!(f, "{seconds}s"),
        Kind::Time(_) | Kind::Zoned(_) => {
            let mut ctx = crate::integer::unlimited_context();
            let text = crate::time::text(&mut ctx, value, None).map_err(|_| fmt::Error)?;
            f.write_str(std::str::from_utf8(text.as_bytes().unwrap()).map_err(|_| fmt::Error)?)
        }
        Kind::Instance(instance) => {
            write!(f, "<{} instance>", instance.class().definition.name)
        }
        Kind::Namespace(namespace) => write!(f, "<Class {}>", namespace.definition.name),
        Kind::Function(function) => write!(
            f,
            "<function {}>",
            function.code.program.functions[function.index].name
        ),
        Kind::Builtin(builtin) => write!(f, "<builtin {}>", builtin.name()),
        Kind::Offset(offset) => write!(f, "<builtin {}>", offset.name()),
        Kind::Enum(e) => write!(f, "<Enum {}>", e.definition.name),
        Kind::EnumMember(m) => write!(
            f,
            "{}::{}",
            m.enumeration.definition.name,
            m.definition().name
        ),
        Kind::Bool(b) => write!(f, "{b}"),
        Kind::Int(n) => write!(f, "{n}"),
        Kind::Big(_) => {
            let mut ctx = crate::integer::unlimited_context();
            let text = crate::integer::format(&mut ctx, value, 10).map_err(|_| fmt::Error)?;
            f.write_str(std::str::from_utf8(&text.data).map_err(|_| fmt::Error)?)
        }
        Kind::Float(n) => write!(f, "{n}"),
        Kind::Range(r) => write!(f, "{r}"),
        Kind::Bytes(h) | Kind::Symbol(h) => {
            write!(f, "{}", String::from_utf8_lossy(&h.data))
        }
        Kind::Array(_) | Kind::Hash(_) => unreachable!("containers open render frames"),
    }
}
