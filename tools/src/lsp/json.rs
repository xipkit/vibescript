//! JSON with the reference server's wire conventions.
//!
//! Output follows Go's `encoding/json`: objects list their keys in the order
//! given here (struct order, or sorted where the reference encodes a map), and
//! strings escape `<`, `>`, `&`, U+2028 and U+2029. Input decoding follows
//! Go's rules for the typed parameter structs: absent and `null` fields take
//! zero values, keys match case-insensitively when no exact key exists, and a
//! value of the wrong type fails the whole decode.

use serde_json::{Map, Value};
use std::fmt::Write;

/// An encodable JSON value.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Json {
    Null,
    Bool(bool),
    Int(i64),
    Str(String),
    /// Already encoded JSON, such as a request id echoed verbatim.
    Raw(Box<str>),
    Array(Vec<Json>),
    Object(Vec<(&'static str, Json)>),
}

impl Json {
    pub(crate) fn str(text: impl Into<String>) -> Self {
        Self::Str(text.into())
    }

    /// Encodes the value as compact JSON.
    pub(crate) fn encode(&self) -> String {
        let mut out = String::new();
        let mut pending = vec![Step::Value(self)];
        while let Some(step) = pending.pop() {
            match step {
                Step::Text(text) => out.push_str(text),
                Step::Value(value) => match value {
                    Self::Null => out.push_str("null"),
                    Self::Bool(value) => out.push_str(if *value { "true" } else { "false" }),
                    Self::Int(value) => {
                        let _ = write!(out, "{value}");
                    }
                    Self::Str(text) => string(&mut out, text),
                    Self::Raw(raw) => out.push_str(raw),
                    Self::Array(items) => {
                        out.push('[');
                        pending.push(Step::Text("]"));
                        for (index, item) in items.iter().enumerate().rev() {
                            pending.push(Step::Value(item));
                            if index > 0 {
                                pending.push(Step::Text(","));
                            }
                        }
                    }
                    Self::Object(fields) => {
                        out.push('{');
                        pending.push(Step::Text("}"));
                        for (index, (key, item)) in fields.iter().enumerate().rev() {
                            pending.push(Step::Value(item));
                            pending.push(Step::Key(key));
                            if index > 0 {
                                pending.push(Step::Text(","));
                            }
                        }
                    }
                },
                Step::Key(key) => {
                    string(&mut out, key);
                    out.push(':');
                }
            }
        }
        out
    }
}

enum Step<'a> {
    Value(&'a Json),
    Key(&'a str),
    Text(&'static str),
}

/// Writes a quoted string the way Go's HTML-safe encoder does.
fn string(out: &mut String, text: &str) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            '<' | '>' | '&' | '\u{0}'..='\u{1f}' => {
                let byte = c as u8;
                out.push_str("\\u00");
                out.push(HEX[usize::from(byte >> 4)] as char);
                out.push(HEX[usize::from(byte & 15)] as char);
            }
            '\u{2028}' => out.push_str("\\u2028"),
            '\u{2029}' => out.push_str("\\u2029"),
            c => out.push(c),
        }
    }
    out.push('"');
}

/// A parameter decode failure: the reference rejects the whole message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Invalid;

/// Looks up a field the way Go matches struct fields: an exact key first,
/// otherwise any key equal to the name ignoring case.
pub(crate) fn field<'a>(object: &'a Map<String, Value>, name: &str) -> Option<&'a Value> {
    object.get(name).or_else(|| {
        object
            .iter()
            .find(|(key, _)| key.to_lowercase() == name.to_lowercase())
            .map(|(_, value)| value)
    })
}

/// Decodes a struct-typed value: `null` leaves every field zero.
pub(crate) fn object(value: Option<&Value>) -> Result<Option<&Map<String, Value>>, Invalid> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Object(object)) => Ok(Some(object)),
        Some(_) => Err(Invalid),
    }
}

/// Decodes a string field.
pub(crate) fn text(object: Option<&Map<String, Value>>, name: &str) -> Result<String, Invalid> {
    match object.and_then(|object| field(object, name)) {
        None | Some(Value::Null) => Ok(String::new()),
        Some(Value::String(text)) => Ok(text.clone()),
        Some(_) => Err(Invalid),
    }
}

/// Decodes an `int` field: an integral JSON number within 64 bits.
pub(crate) fn int(object: Option<&Map<String, Value>>, name: &str) -> Result<i64, Invalid> {
    match object.and_then(|object| field(object, name)) {
        None | Some(Value::Null) => Ok(0),
        Some(Value::Number(number)) => {
            let literal = number.to_string();
            // Go parses the literal itself, so `1.0` and `1e2` are not integers.
            if literal.contains(['.', 'e', 'E']) {
                return Err(Invalid);
            }
            literal.parse().map_err(|_| Invalid)
        }
        Some(_) => Err(Invalid),
    }
}

/// Decodes a nested struct field.
pub(crate) fn nested<'a>(
    object: Option<&'a Map<String, Value>>,
    name: &str,
) -> Result<Option<&'a Map<String, Value>>, Invalid> {
    self::object(object.and_then(|object| field(object, name)))
}

/// Decodes a slice field of structs; `null` elements decode as zero structs.
pub(crate) fn structs<'a>(
    object: Option<&'a Map<String, Value>>,
    name: &str,
) -> Result<Vec<Option<&'a Map<String, Value>>>, Invalid> {
    match object.and_then(|object| field(object, name)) {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(items)) => items.iter().map(|item| self::object(Some(item))).collect(),
        Some(_) => Err(Invalid),
    }
}

/// Decodes raw parameters the way `json.Unmarshal` does: absent parameters
/// are an error, while `null` decodes to a zero struct.
pub(crate) fn params(raw: Option<&str>) -> Result<Option<Value>, Invalid> {
    let raw = raw.ok_or(Invalid)?;
    let value: Value = serde_json::from_str(raw).map_err(|_| Invalid)?;
    match value {
        Value::Null => Ok(None),
        Value::Object(_) => Ok(Some(value)),
        _ => Err(Invalid),
    }
}

/// The object inside decoded parameters.
pub(crate) fn root(params: &Option<Value>) -> Option<&Map<String, Value>> {
    params.as_ref().and_then(Value::as_object)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_like_go() {
        let value = Json::Object(vec![
            (
                "a",
                Json::str("<b>&\"\\\n\t\r\u{8}\u{c}\u{1}\u{7f}é\u{2028}😀"),
            ),
            (
                "b",
                Json::Array(vec![Json::Int(-3), Json::Null, Json::Bool(true)]),
            ),
            ("c", Json::Object(Vec::new())),
            ("d", Json::Array(Vec::new())),
            ("e", Json::Raw("1e3".into())),
        ]);
        assert_eq!(
            value.encode(),
            concat!(
                r#"{"a":"\u003cb\u003e\u0026\"\\\n\t\r\b\f\u0001"#,
                "\u{7f}é",
                r#"\u2028😀","b":[-3,null,true],"c":{},"d":[],"e":1e3}"#
            )
        );
        // Outlines nest as deeply as modules do.
        let mut deep = Json::Null;
        for _ in 0..2_000 {
            deep = Json::Array(vec![deep]);
        }
        assert_eq!(deep.encode().len(), 4_004);
    }

    #[test]
    fn decodes_like_go() {
        let parsed = params(Some(r#"{"Position":{"line":2,"character":null},"x":1}"#)).unwrap();
        let root = root(&parsed);
        let position = nested(root, "position").unwrap();
        assert_eq!(int(position, "line"), Ok(2));
        assert_eq!(int(position, "character"), Ok(0));
        assert_eq!(text(root, "uri"), Ok(String::new()));
        for bad in [
            r#"{"line":1.5}"#,
            r#"{"line":1e2}"#,
            r#"{"line":"1"}"#,
            r#"{"line":1e30}"#,
        ] {
            let parsed = params(Some(bad)).unwrap();
            assert_eq!(int(super::root(&parsed), "line"), Err(Invalid), "{bad}");
        }
        assert_eq!(params(None), Err(Invalid));
        assert_eq!(params(Some("null")), Ok(None));
        assert_eq!(params(Some("[1]")), Err(Invalid));
        let parsed = params(Some(r#"{"changes":[null,{"text":"a"}]}"#)).unwrap();
        let changes = structs(super::root(&parsed), "changes").unwrap();
        assert_eq!(changes.len(), 2);
        assert_eq!(text(changes[0], "text"), Ok(String::new()));
        assert_eq!(text(changes[1], "text"), Ok("a".to_owned()));
    }
}
