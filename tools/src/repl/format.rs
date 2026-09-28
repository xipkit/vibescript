//! Renders values the way the Go REPL's `Value.String` does: strings and
//! symbols without quotes, nil as an empty string, floats in Go's shortest `%g`
//! form, and arrays and hashes as `[a, b]` and `{key: value}`. Kinds that the
//! Go REPL cannot produce keep the library's `Display` rendering.

use std::fmt::Write;
use vibescript::Value;

/// Renders a value as the Go REPL displays it: strings and symbols without
/// quotes, nil as an empty string inside containers, floats in Go's shortest
/// `%g` form and arrays and hashes as `[a, b]` and `{key: value}`. Other kinds
/// use the library's `Display`. Rendering is iterative, so deeply nested
/// containers cannot exhaust the native stack.
///
/// ```
/// use vibescript::Value;
/// use vibescript_tools::repl::render_value;
///
/// let value = Value::array(vec![Value::bytes("a"), Value::nil(), Value::float(1e21)]);
/// assert_eq!(render_value(&value), "[a, , 1e+21]");
/// ```
pub fn render_value(root: &Value) -> String {
    enum Item<'a> {
        Value(&'a Value),
        Text(&'static str),
        Key(&'a Value),
    }
    let mut out = String::new();
    let mut stack = vec![Item::Value(root)];
    while let Some(item) = stack.pop() {
        match item {
            Item::Text(text) => out.push_str(text),
            Item::Key(key) => {
                leaf(&mut out, key);
                out.push_str(": ");
            }
            Item::Value(value) => {
                if let Some(elements) = value.as_array() {
                    out.push('[');
                    stack.push(Item::Text("]"));
                    for (index, element) in elements.iter().enumerate().rev() {
                        stack.push(Item::Value(element));
                        if index > 0 {
                            stack.push(Item::Text(", "));
                        }
                    }
                } else if let Some(entries) =
                    value.as_hash().filter(|_| value.type_name() == "hash")
                {
                    out.push('{');
                    stack.push(Item::Text("}"));
                    for (index, (key, element)) in entries.iter().enumerate().rev() {
                        stack.push(Item::Value(element));
                        stack.push(Item::Key(key));
                        if index > 0 {
                            stack.push(Item::Text(", "));
                        }
                    }
                } else {
                    leaf(&mut out, value);
                }
            }
        }
    }
    out
}

fn leaf(out: &mut String, value: &Value) {
    match value.type_name() {
        "nil" => {}
        "string" | "symbol" => {
            out.push_str(&String::from_utf8_lossy(
                value.as_bytes().unwrap_or_default(),
            ));
        }
        "float" => float(out, value.as_float().unwrap_or_default()),
        _ => {
            let _ = write!(out, "{value}");
        }
    }
}

/// Formats a float like Go's `strconv.FormatFloat(f, 'g', -1, 64)`, spelling
/// the special values as Vibescript does.
fn float(out: &mut String, f: f64) {
    if f.is_nan() {
        out.push_str("NaN");
        return;
    }
    if f.is_infinite() {
        out.push_str(if f > 0.0 { "Infinity" } else { "-Infinity" });
        return;
    }
    // Rust's shortest round-trip digits, split into mantissa and exponent.
    let scientific = format!("{f:e}");
    let (mantissa, exponent) = scientific.split_once('e').unwrap_or((&scientific, "0"));
    let exponent: i32 = exponent.parse().unwrap_or(0);
    if f != 0.0 && !(-4..6).contains(&exponent) {
        let sign = if exponent < 0 { '-' } else { '+' };
        let _ = write!(out, "{mantissa}e{sign}{:02}", exponent.unsigned_abs());
    } else {
        let _ = write!(out, "{f}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn floats(f: f64) -> String {
        let mut out = String::new();
        float(&mut out, f);
        out
    }

    #[test]
    fn floats_match_go_shortest_general_format() {
        for (value, expected) in [
            (1.5, "1.5"),
            (2.0, "2"),
            (100.0, "100"),
            (100000.0, "100000"),
            (1000000.0, "1e+06"),
            (123456789.0, "1.23456789e+08"),
            (1e20, "1e+20"),
            (1e21, "1e+21"),
            (0.0001, "0.0001"),
            (0.00001, "1e-05"),
            (1e-7, "1e-07"),
            (1.5e-300, "1.5e-300"),
            (10.0 / 3.0, "3.3333333333333335"),
            (0.0, "0"),
            (-0.0, "-0"),
            (-2.5, "-2.5"),
            (f64::INFINITY, "Infinity"),
            (f64::NEG_INFINITY, "-Infinity"),
            (f64::NAN, "NaN"),
        ] {
            assert_eq!(floats(value), expected, "{value}");
        }
    }

    #[test]
    fn containers_render_like_go() {
        let value = Value::array(vec![
            Value::int(1),
            Value::nil(),
            Value::bytes("a"),
            Value::symbol("b"),
            Value::float(2.5),
            Value::hash(vec![
                (b"k".to_vec(), Value::nil()),
                (b"n".to_vec(), Value::float(1e21)),
            ]),
            Value::array(vec![]),
            Value::hash(vec![]),
        ]);
        assert_eq!(
            super::render_value(&value),
            "[1, , a, b, 2.5, {k: , n: 1e+21}, [], {}]"
        );
        assert_eq!(super::render_value(&Value::nil()), "");
        assert_eq!(super::render_value(&Value::object(vec![])), "<object>");
    }

    #[test]
    fn deep_nesting_renders_without_recursion() {
        let mut value = Value::int(7);
        for _ in 0..10_000 {
            value = Value::array(vec![value]);
        }
        let text = super::render_value(&value);
        assert_eq!(
            text,
            format!("{}7{}", "[".repeat(10_000), "]".repeat(10_000))
        );
    }
}
