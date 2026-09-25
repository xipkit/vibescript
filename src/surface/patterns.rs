//! The rename table as call patterns and replacement templates.

use crate::signatures::{Replacement, renames};
use std::sync::OnceLock;

/// What a pattern calls a member on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Callee {
    /// A member of the receiver, `$x.name`.
    Member,
    /// A namespace member, such as `Time.gm`.
    Namespace(String),
    /// A global function.
    Global,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ArgPattern {
    /// `$name`: one positional argument.
    Capture(String),
    /// `...`: every other argument and the block.
    Rest,
    /// `:name`: a positional symbol literal.
    Symbol(String),
    /// `name: $value` or `name: literal`.
    Keyword(String, KeywordValue),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeywordValue {
    Capture(String),
    Literal(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TemplatePiece {
    Text(String),
    /// `$x`, the receiver.
    Receiver,
    /// `$name`, a captured argument.
    Capture(String),
    /// `...`, the remaining arguments.
    Rest,
}

/// What a matched call becomes.
#[derive(Clone, Debug)]
pub enum Change {
    /// A replacement built from the template's pieces.
    Template(Vec<TemplatePiece>),
    /// No mechanical replacement; the hint says what to write instead.
    Manual(String),
}

/// One rename entry, ready to match calls.
#[derive(Clone, Debug)]
pub struct Pattern {
    /// The table's receiver: a type such as `array`, `T` for every type,
    /// `error`, `global` or a namespace.
    pub receiver: String,
    pub name: String,
    pub callee: Callee,
    /// The argument patterns; `None` matches a call without arguments or a block.
    pub args: Option<Vec<ArgPattern>>,
    pub rewrite: Change,
    /// The canonical name of a plain rename, such as `length` for `size`
    /// or `Time.utc` for `Time.gm`.
    pub canonical: Option<String>,
}

impl Pattern {
    /// The member a plain rename calls instead, such as `length` for `size`.
    pub fn target_member(&self) -> Option<&str> {
        let Change::Template(pieces) = &self.rewrite else {
            return None;
        };
        match pieces.as_slice() {
            [TemplatePiece::Receiver, TemplatePiece::Text(text), ..] => {
                let name = text.strip_prefix('.')?;
                let end = name
                    .find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '_' | '?' | '!')))
                    .unwrap_or(name.len());
                Some(&name[..end])
            }
            _ => None,
        }
    }

    /// Whether the replacement is an operator expression, which may need
    /// parentheses where the call stood.
    pub fn operator(&self) -> bool {
        let Change::Template(pieces) = &self.rewrite else {
            return false;
        };
        pieces.iter().any(|piece| {
            matches!(piece, TemplatePiece::Text(text)
                if [" == ", " % ", " <=> ", " * ", " + ", "] = "].iter().any(|op| text.contains(op)))
        })
    }

    /// What to write instead, for a diagnostic: the replacement with its
    /// placeholders named, such as "use `x == nil`", or the manual hint.
    pub fn advice(&self) -> String {
        let pieces = match &self.rewrite {
            Change::Manual(hint) => return hint.clone(),
            Change::Template(pieces) => pieces,
        };
        if let Some(canonical) = &self.canonical {
            return format!("use `{canonical}`");
        }
        let text: String = pieces
            .iter()
            .map(|piece| match piece {
                TemplatePiece::Text(text) => text.as_str(),
                TemplatePiece::Receiver => "x",
                TemplatePiece::Capture(name) => name.as_str(),
                TemplatePiece::Rest => "...",
            })
            .collect();
        format!("use `{text}`")
    }

    /// How often the template repeats the receiver.
    pub fn receiver_uses(&self) -> usize {
        match &self.rewrite {
            Change::Template(pieces) => pieces
                .iter()
                .filter(|piece| **piece == TemplatePiece::Receiver)
                .count(),
            Change::Manual(_) => 0,
        }
    }
}

/// Every rename entry except the `*` rules for empty parentheses and type
/// names, which have rules of their own.
pub fn patterns() -> &'static [Pattern] {
    static PATTERNS: OnceLock<Vec<Pattern>> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        renames()
            .iter()
            .filter(|rename| !matches!(rename.receiver.as_str(), "*" | "type"))
            .map(|rename| {
                let rewrite = match &rename.replacement {
                    Replacement::Rewrite(template) => Change::Template(template_pieces(template)),
                    Replacement::Manual(hint) => Change::Manual(hint.clone()),
                };
                let (callee, rest) = if let Some(rest) = rename.pattern.strip_prefix("$x.") {
                    (Callee::Member, rest)
                } else if rename.receiver == "global" {
                    (Callee::Global, rename.pattern.as_str())
                } else {
                    let (namespace, rest) = rename.pattern.split_once('.').unwrap_or_default();
                    (Callee::Namespace(namespace.to_owned()), rest)
                };
                let args = rest
                    .find('(')
                    .map(|open| arg_patterns(&rest[open + 1..rest.len() - 1]));
                let canonical = rename.canonical().map(|(namespace, name)| match namespace {
                    Some(namespace) => format!("{namespace}.{name}"),
                    None => name.to_owned(),
                });
                Pattern {
                    receiver: rename.receiver.clone(),
                    name: rename.name.clone(),
                    callee,
                    args,
                    rewrite,
                    canonical,
                }
            })
            .collect()
    })
}

fn arg_patterns(text: &str) -> Vec<ArgPattern> {
    text.split(", ")
        .filter(|arg| !arg.is_empty())
        .map(|arg| {
            if arg == "..." {
                ArgPattern::Rest
            } else if let Some(name) = arg.strip_prefix('$') {
                ArgPattern::Capture(name.to_owned())
            } else if let Some(name) = arg.strip_prefix(':') {
                ArgPattern::Symbol(name.to_owned())
            } else if let Some((name, value)) = arg.split_once(": ") {
                let value = match value.strip_prefix('$') {
                    Some(capture) => KeywordValue::Capture(capture.to_owned()),
                    None => KeywordValue::Literal(value.to_owned()),
                };
                ArgPattern::Keyword(name.to_owned(), value)
            } else {
                ArgPattern::Capture(arg.to_owned())
            }
        })
        .collect()
}

fn template_pieces(template: &str) -> Vec<TemplatePiece> {
    let mut pieces = Vec::new();
    let mut text = String::new();
    let mut rest = template;
    while !rest.is_empty() {
        if let Some(tail) = rest.strip_prefix("...") {
            flush(&mut pieces, &mut text);
            pieces.push(TemplatePiece::Rest);
            rest = tail;
        } else if let Some(tail) = rest.strip_prefix('$') {
            let end = tail
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .unwrap_or(tail.len());
            flush(&mut pieces, &mut text);
            pieces.push(if &tail[..end] == "x" {
                TemplatePiece::Receiver
            } else {
                TemplatePiece::Capture(tail[..end].to_owned())
            });
            rest = &tail[end..];
        } else {
            let c = rest.chars().next().unwrap();
            text.push(c);
            rest = &rest[c.len_utf8()..];
        }
    }
    flush(&mut pieces, &mut text);
    pieces
}

fn flush(pieces: &mut Vec<TemplatePiece>, text: &mut String) {
    if !text.is_empty() {
        pieces.push(TemplatePiece::Text(std::mem::take(text)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_every_entry() {
        let patterns = patterns();
        let size = patterns
            .iter()
            .find(|p| p.receiver == "array" && p.name == "size")
            .unwrap();
        assert_eq!(size.callee, Callee::Member);
        assert_eq!(size.args, None);
        assert_eq!(size.target_member(), Some("length"));
        let sub = patterns
            .iter()
            .find(|p| p.receiver == "string" && p.name == "sub")
            .unwrap();
        assert_eq!(
            sub.args.as_deref(),
            Some(
                &[
                    ArgPattern::Capture("pattern".into()),
                    ArgPattern::Rest,
                    ArgPattern::Keyword("regex".into(), KeywordValue::Literal("true".into())),
                ][..]
            )
        );
        let gm = patterns.iter().find(|p| p.name == "gm").unwrap();
        assert_eq!(gm.callee, Callee::Namespace("Time".into()));
        let store = patterns.iter().find(|p| p.name == "store").unwrap();
        assert!(store.operator());
        let nil = patterns.iter().find(|p| p.name == "nil?").unwrap();
        assert!(nil.operator());
        assert!(matches!(
            patterns.iter().find(|p| p.name == "tap").unwrap().rewrite,
            Change::Manual(_)
        ));
    }
}
