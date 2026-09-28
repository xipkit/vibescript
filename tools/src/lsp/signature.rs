//! Signature help for the call around the cursor.

use super::catalog::Catalog;
use super::docs::builtin_docs;
use super::document::SignatureHelp;
use super::hover::{param_labels, params_text};
use super::text::{character_index, mask_code, word_char};
use vibescript::tooling::{ItemKind, Outline};

/// Signature help for the innermost call around the cursor on its line, or
/// for a paren-less `assert`. User functions from the last compiled version
/// of the document come first, then documented global builtins.
pub(crate) fn help<S: AsRef<str>>(
    catalog: &Catalog,
    compiled: Option<&Outline>,
    lines: &[S],
    line: i64,
    character: i64,
) -> Option<SignatureHelp> {
    let (callee, active) = enclosing_call(catalog, lines, line, character)
        .or_else(|| parenless_call(lines, line, character))?;
    if let Some(compiled) = compiled {
        let function = compiled.items.iter().find(|item| {
            matches!(item.kind, ItemKind::Function | ItemKind::Alias) && item.name == callee
        });
        if let Some(item) = function {
            let labels = param_labels(item.function.as_ref());
            let mut label = format!("{}({})", item.name, params_text(item.function.as_ref()));
            if let Some(result) = item.function.as_ref().and_then(|f| f.return_type.as_ref()) {
                label.push_str(" -> ");
                label.push_str(result);
            }
            return Some(response(&label, &labels, active));
        }
    }
    let entry = builtin_docs().get(&callee)?;
    if !catalog.is_function(&callee) {
        return None;
    }
    let label = entry.signature.replace('`', "");
    Some(response(&label, &labels_from_signature(&label), active))
}

fn response(label: &str, params: &[String], active: usize) -> SignatureHelp {
    let mut active = active;
    if active >= params.len() && !params.is_empty() {
        active = params.len() - 1;
    }
    SignatureHelp {
        label: label.to_owned(),
        parameters: params.to_vec(),
        active_parameter: u32::try_from(active).unwrap_or(u32::MAX),
    }
}

/// The innermost unclosed call before the cursor on its line and the
/// zero-based argument index at the cursor. Commas inside nested calls,
/// arrays, hashes and strings do not count, and an instance method has no
/// signature to offer.
pub(crate) fn enclosing_call<S: AsRef<str>>(
    catalog: &Catalog,
    lines: &[S],
    line: i64,
    character: i64,
) -> Option<(String, usize)> {
    let text = usize::try_from(line)
        .ok()
        .and_then(|line| lines.get(line))?
        .as_ref();
    let chars: Vec<char> = text.chars().collect();
    let cursor = character_index(text, character).min(chars.len());
    let masked = mask_code(&chars[..cursor]);
    let (mut parens, mut squares, mut braces) = (0, 0, 0);
    let mut active = 0;
    for index in (0..cursor).rev() {
        match masked[index] {
            ')' => parens += 1,
            ']' => squares += 1,
            '}' => braces += 1,
            '[' => {
                if squares > 0 {
                    squares -= 1;
                    continue;
                }
                // Commas so far belong to an unclosed array argument.
                active = 0;
            }
            '{' => {
                if braces > 0 {
                    braces -= 1;
                    continue;
                }
                active = 0;
            }
            '(' => {
                if parens > 0 {
                    parens -= 1;
                    continue;
                }
                let mut end = index;
                while end > 0 && matches!(chars[end - 1], ' ' | '\t') {
                    end -= 1;
                }
                let mut start = end;
                while start > 0 && word_char(chars[start - 1]) {
                    start -= 1;
                }
                if start == end {
                    return None;
                }
                let name: String = chars[start..end].iter().collect();
                if start > 0 && chars[start - 1] == '.' {
                    let receiver_end = start - 1;
                    let mut receiver_start = receiver_end;
                    while receiver_start > 0 && word_char(chars[receiver_start - 1]) {
                        receiver_start -= 1;
                    }
                    let receiver: String = chars[receiver_start..receiver_end].iter().collect();
                    if receiver.is_empty()
                        || !catalog.is_namespace(&receiver)
                        || (receiver_start > 0 && chars[receiver_start - 1] == '.')
                    {
                        return None;
                    }
                    return Some((format!("{receiver}.{name}"), active));
                }
                return Some((name, active));
            }
            ',' if parens == 0 && squares == 0 && braces == 0 => active += 1,
            _ => (),
        }
    }
    None
}

/// The paren-less statement form of the builtins the parser accepts without
/// parentheses (`assert cond, msg`), counting top-level commas.
pub(crate) fn parenless_call<S: AsRef<str>>(
    lines: &[S],
    line: i64,
    character: i64,
) -> Option<(String, usize)> {
    let text = usize::try_from(line)
        .ok()
        .and_then(|line| lines.get(line))?
        .as_ref();
    let chars: Vec<char> = text.chars().collect();
    let cursor = character_index(text, character).min(chars.len());
    let masked = mask_code(&chars[..cursor]);
    let mut start = 0;
    while start < cursor && matches!(masked[start], ' ' | '\t') {
        start += 1;
    }
    let mut end = start;
    while end < cursor && word_char(masked[end]) {
        end += 1;
    }
    if end == start || end >= cursor {
        return None;
    }
    let callee: String = masked[start..end].iter().collect();
    if callee != "assert" || !matches!(masked[end], ' ' | '\t') {
        return None;
    }
    let (mut parens, mut squares, mut braces) = (0i64, 0i64, 0i64);
    let mut active = 0;
    for c in &masked[end..cursor] {
        match c {
            '(' => parens += 1,
            ')' => parens -= 1,
            '[' => squares += 1,
            ']' => squares -= 1,
            '{' => braces += 1,
            '}' => braces -= 1,
            ',' if parens == 0 && squares == 0 && braces == 0 => active += 1,
            _ => (),
        }
    }
    Some((callee, active))
}

/// Parameter labels between the outermost parentheses of a signature.
fn labels_from_signature(label: &str) -> Vec<String> {
    let (Some(open), Some(close)) = (label.find('('), label.rfind(')')) else {
        return Vec::new();
    };
    if close <= open + 1 {
        return Vec::new();
    }
    label[open + 1..close]
        .split(',')
        .map(|part| part.trim().to_owned())
        .collect()
}
