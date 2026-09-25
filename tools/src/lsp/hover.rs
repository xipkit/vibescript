//! Hover documentation.
//!
//! Lookup order: a qualified builtin (`JSON.parse_as`); for a word reached
//! through `.` on a value, the member documentation, since `money.format` is
//! never the global `format`; otherwise builtin, namespace and keyword docs;
//! then a declaration in the current document; and finally a one-line
//! classification, so hover never comes back empty for a word.

use super::catalog::Catalog;
use super::docs::{builtin_docs, keyword_doc, member_doc_markdown, namespace_doc};
use super::navigation::{anchor_line, children};
use super::text::{line_at, word_char, word_span};
use vibescript::tooling::{Function, Item, ItemKind, Outline, Parameter, ParameterKind};

/// The hover markdown for `word` at a position.
pub(crate) fn markdown<S: AsRef<str>>(
    catalog: &Catalog,
    program: Option<&Outline>,
    lines: &[S],
    line: i64,
    character: i64,
    word: &str,
) -> String {
    let docs = builtin_docs();
    let qualified = qualified_word(lines, line, character);
    if let Some(entry) = docs.get(&qualified).filter(|_| !qualified.is_empty()) {
        return entry.markdown.clone();
    }
    if value_member_access(catalog, lines, line, character) {
        let markdown = member_doc_markdown(word);
        if !markdown.is_empty() {
            return markdown;
        }
    } else {
        if let Some(entry) = docs.get(word) {
            return entry.markdown.clone();
        }
        let markdown = namespace_doc(catalog, word);
        if !markdown.is_empty() {
            return markdown;
        }
        if let Some(doc) = keyword_doc(word) {
            return format!("`{word}`\n\n{doc}");
        }
    }
    if let Some(markdown) = user_symbol(program, lines, word, line, character) {
        return markdown;
    }
    format!("`{word}`\n\nVibescript {}", classify(catalog, word))
}

pub(crate) fn classify(catalog: &Catalog, word: &str) -> &'static str {
    if vibescript::tooling::keywords().contains(&word) {
        "keyword"
    } else if catalog.is_builtin(word) {
        "builtin"
    } else {
        "symbol"
    }
}

/// The receiver word and the character range of the word before it, when the
/// word directly follows `Receiver.` or `Receiver::`.
fn receiver_before(chars: &[char], start: usize) -> Option<(usize, usize)> {
    let receiver_end = if start > 0 && chars[start - 1] == '.' {
        start - 1
    } else if start >= 2 && chars[start - 1] == ':' && chars[start - 2] == ':' {
        start - 2
    } else {
        return None;
    };
    let mut receiver_start = receiver_end;
    while receiver_start > 0 && word_char(chars[receiver_start - 1]) {
        receiver_start -= 1;
    }
    Some((receiver_start, receiver_end))
}

/// "Receiver.word" when the word directly follows a standalone receiver;
/// `Math::PI` qualifies as `Math.PI`, while `payload.JSON.parse` does not.
pub(crate) fn qualified_word<S: AsRef<str>>(lines: &[S], line: i64, character: i64) -> String {
    let Some((chars, start, end)) = word_span(lines, line, character) else {
        return String::new();
    };
    let Some((receiver_start, receiver_end)) = receiver_before(&chars, start) else {
        return String::new();
    };
    if receiver_start == receiver_end
        || (receiver_start > 0 && matches!(chars[receiver_start - 1], '.' | ':'))
    {
        return String::new();
    }
    let receiver: String = chars[receiver_start..receiver_end].iter().collect();
    let word: String = chars[start..end].iter().collect();
    format!("{receiver}.{word}")
}

/// Whether the word is reached through `.` on a value rather than a
/// namespace: `..` is a range and `::` a scope accessor. A leading dot
/// continuing the previous line's call counts.
pub(crate) fn value_member_access<S: AsRef<str>>(
    catalog: &Catalog,
    lines: &[S],
    line: i64,
    character: i64,
) -> bool {
    let Some((chars, start, _)) = word_span(lines, line, character) else {
        return false;
    };
    if start == 0 || chars[start - 1] != '.' || (start >= 2 && chars[start - 2] == '.') {
        return false;
    }
    let receiver_end = start - 1;
    let mut receiver_start = receiver_end;
    while receiver_start > 0 && word_char(chars[receiver_start - 1]) {
        receiver_start -= 1;
    }
    let receiver: String = chars[receiver_start..receiver_end].iter().collect();
    !catalog.is_namespace(&receiver)
}

/// Hover for a declaration in the document. At a member write site the setter
/// `name=` is tried first; elsewhere the plain name wins.
fn user_symbol<S: AsRef<str>>(
    program: Option<&Outline>,
    lines: &[S],
    word: &str,
    line: i64,
    character: i64,
) -> Option<String> {
    let program = program?;
    if word.is_empty() {
        return None;
    }
    let setter = format!("{word}=");
    let mut candidates = [word, setter.as_str()];
    if assignment_follows(lines, line, character)
        && (dot_precedes(lines, line, character) || def_precedes(lines, line, character))
    {
        candidates.reverse();
    }
    let qualifier = receiver_word(lines, line, character);
    let member_shaped = dot_precedes(lines, line, character);
    candidates.into_iter().find_map(|candidate| {
        symbol_doc(
            program,
            lines,
            candidate,
            line + 1,
            &qualifier,
            member_shaped,
        )
    })
}

/// The receiver word before `Receiver.` or `Receiver::`, except `self`.
fn receiver_word<S: AsRef<str>>(lines: &[S], line: i64, character: i64) -> String {
    let Some((chars, start, _)) = word_span(lines, line, character) else {
        return String::new();
    };
    let Some((receiver_start, receiver_end)) = receiver_before(&chars, start) else {
        return String::new();
    };
    let receiver: String = chars[receiver_start..receiver_end].iter().collect();
    if receiver == "self" {
        return String::new();
    }
    receiver
}

fn dot_precedes<S: AsRef<str>>(lines: &[S], line: i64, character: i64) -> bool {
    word_span(lines, line, character)
        .is_some_and(|(chars, start, _)| start > 0 && chars[start - 1] == '.')
}

/// Whether the word is the name in a `def` declaration, optionally after `self.`.
fn def_precedes<S: AsRef<str>>(lines: &[S], line: i64, character: i64) -> bool {
    let Some((chars, start, _)) = word_span(lines, line, character) else {
        return false;
    };
    let mut index = start;
    if index >= 5 && chars[index - 5..index] == ['s', 'e', 'l', 'f', '.'] {
        index -= 5;
    }
    while index > 0 && matches!(chars[index - 1], ' ' | '\t') {
        index -= 1;
    }
    index >= 3
        && chars[index - 3..index] == ['d', 'e', 'f']
        && (index == 3 || !word_char(chars[index - 4]))
}

/// Whether a bare `=` follows the word, the shape of a setter call; `==` and
/// `=~` do not count.
fn assignment_follows<S: AsRef<str>>(lines: &[S], line: i64, character: i64) -> bool {
    let Some((chars, _, end)) = word_span(lines, line, character) else {
        return false;
    };
    let mut index = end;
    while index < chars.len() && matches!(chars[index], ' ' | '\t') {
        index += 1;
    }
    if chars.get(index) != Some(&'=') {
        return false;
    }
    !matches!(chars.get(index + 1), Some('=' | '~'))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Declaration,
    InstanceMethod,
    ClassMethod,
    EnumMember,
}

/// One declaration matching the hovered word. Scoped candidates live in a
/// class, module or enum body spanning `container` (one-based, inclusive).
struct Candidate {
    markdown: String,
    line: usize,
    container: (usize, usize),
    scoped: bool,
    owner: String,
    kind: Kind,
}

/// Resolves `word` among every matching declaration, preferring the one on
/// the hovered line, then one whose container encloses it (the innermost when
/// they nest), then the nearest declaration above it. `hover_line` is one-based.
fn symbol_doc<S: AsRef<str>>(
    program: &Outline,
    lines: &[S],
    word: &str,
    hover_line: i64,
    qualifier: &str,
    member_shaped: bool,
) -> Option<String> {
    let mut candidates = collect(program, lines, word);
    if !qualifier.is_empty() {
        let owned = candidates
            .iter()
            .any(|candidate| candidate.owner == qualifier);
        if owned {
            // A receiver naming the owner dispatches class methods and enum
            // members, never the owner's instance methods.
            candidates.retain(|candidate| {
                candidate.owner == qualifier && candidate.kind != Kind::InstanceMethod
            });
        } else {
            // An instance receiver reaches only scoped instance methods.
            candidates
                .retain(|candidate| candidate.scoped && candidate.kind == Kind::InstanceMethod);
        }
    }
    if candidates.is_empty() {
        return None;
    }
    if let Some(candidate) = candidates
        .iter()
        .find(|candidate| candidate.line as i64 == hover_line)
    {
        return Some(candidate.markdown.clone());
    }
    let mut best: Option<&Candidate> = None;
    for candidate in &candidates {
        let (start, end) = candidate.container;
        if !candidate.scoped || hover_line < start as i64 || hover_line > end as i64 {
            continue;
        }
        if best.is_none_or(|best| start > best.container.0) {
            best = Some(candidate);
        }
    }
    if let Some(best) = best {
        return Some(best.markdown.clone());
    }
    // Outside every container, top-level declarations outrank methods of a
    // class the position is not in, and a bare word cannot reach them at all.
    let unscoped: Vec<&Candidate> = candidates.iter().filter(|c| !c.scoped).collect();
    let pool: Vec<&Candidate> = if unscoped.is_empty() {
        if qualifier.is_empty() && !member_shaped {
            return None;
        }
        candidates.iter().collect()
    } else {
        unscoped
    };
    let chosen = pool
        .iter()
        .rev()
        .find(|candidate| candidate.line as i64 <= hover_line)
        .unwrap_or(&pool[0]);
    Some(chosen.markdown.clone())
}

fn collect<S: AsRef<str>>(program: &Outline, lines: &[S], word: &str) -> Vec<Candidate> {
    let mut out = Vec::new();
    let file_end = lines.len() + 1;
    for (index, item) in program.items.iter().enumerate() {
        let next_start = program
            .items
            .get(index + 1)
            .map(|next| next.position.line)
            .filter(|line| *line > 0)
            .unwrap_or(file_end);
        match item.kind {
            ItemKind::Function if item.name == word => out.push(Candidate {
                markdown: user_markdown(lines, &signature(item, false), item),
                line: item.position.line,
                container: (0, 0),
                scoped: false,
                owner: String::new(),
                kind: Kind::Declaration,
            }),
            ItemKind::Class | ItemKind::Module => {
                collect_class(
                    &mut out,
                    item,
                    lines,
                    word,
                    (item.position.line, next_start - 1),
                    "",
                );
            }
            ItemKind::Enum => {
                if item.name == word {
                    out.push(Candidate {
                        markdown: user_markdown(lines, &format!("enum {}", item.name), item),
                        line: item.position.line,
                        container: (0, 0),
                        scoped: false,
                        owner: String::new(),
                        kind: Kind::Declaration,
                    });
                }
                for member in item.children.iter().filter(|member| member.name == word) {
                    out.push(Candidate {
                        markdown: user_markdown(
                            lines,
                            &format!("{}::{}", item.name, member.name),
                            member,
                        ),
                        line: member.position.line,
                        container: (item.position.line, next_start - 1),
                        scoped: true,
                        owner: item.name.clone(),
                        kind: Kind::EnumMember,
                    });
                }
            }
            _ => (),
        }
    }
    out
}

/// Matches within one class or module: the declaration itself, its methods,
/// and its nested modules, each bounded by the sibling that follows it.
fn collect_class<S: AsRef<str>>(
    out: &mut Vec<Candidate>,
    item: &Item,
    lines: &[S],
    word: &str,
    container: (usize, usize),
    parent: &str,
) {
    if item.name == word {
        let keyword = if item.kind == ItemKind::Module {
            "module"
        } else {
            "class"
        };
        out.push(Candidate {
            markdown: user_markdown(lines, &format!("{keyword} {}", item.name), item),
            line: item.position.line,
            container: (0, 0),
            scoped: false,
            owner: parent.to_owned(),
            kind: Kind::Declaration,
        });
    }
    for (kind, class_method) in [(ItemKind::Method, false), (ItemKind::ClassMethod, true)] {
        for method in children(item, kind).filter(|method| method.name == word) {
            out.push(Candidate {
                markdown: user_markdown(lines, &signature(method, class_method), method),
                line: method.position.line,
                container,
                scoped: true,
                owner: item.name.clone(),
                kind: if class_method {
                    Kind::ClassMethod
                } else {
                    Kind::InstanceMethod
                },
            });
        }
    }
    let sibling_starts: Vec<usize> = item
        .children
        .iter()
        .filter(|child| child.kind != ItemKind::Property)
        .map(|child| child.position.line)
        .filter(|line| *line > 0)
        .collect();
    for nested in children(item, ItemKind::Module) {
        let mut end = container.1;
        for sibling in &sibling_starts {
            if *sibling > nested.position.line && sibling - 1 < end {
                end = sibling - 1;
            }
        }
        collect_class(
            out,
            nested,
            lines,
            word,
            (nested.position.line, end),
            &item.name,
        );
    }
}

/// A declaration line rebuilt from its signature.
pub(crate) fn signature(item: &Item, class_method: bool) -> String {
    let mut signature = String::from("def ");
    if class_method {
        signature.push_str("self.");
    }
    signature.push_str(&item.name);
    let function = item.function.as_ref();
    let params = function.map_or(&[][..], |function| function.params.as_slice());
    if !params.is_empty() {
        signature.push('(');
        signature.push_str(&params_text(function));
        signature.push(')');
    }
    if let Some(result) = function.and_then(|function| function.return_type.as_ref()) {
        signature.push_str(" -> ");
        signature.push_str(result);
    }
    signature
}

/// One parameter in declaration form: its name, annotation and a default
/// marker.
pub(crate) fn param_label(param: &Parameter) -> String {
    let target = match param.kind {
        ParameterKind::Rest => format!("*{}", param.name),
        ParameterKind::KeywordRest => format!("**{}", param.name),
        _ => param.name.clone(),
    };
    let mut label = target;
    if let Some(ty) = &param.type_annotation {
        label.push_str(": ");
        label.push_str(ty);
    }
    if param.default {
        label.push_str(" = …");
    }
    label
}

/// A function's parameters in declaration form, with the bare `*` that
/// starts its keyword parameters unless a rest parameter already does.
pub(crate) fn params_text(function: Option<&Function>) -> String {
    let params = function.map_or(&[][..], |function| function.params.as_slice());
    let first = params
        .iter()
        .position(|param| param.kind == ParameterKind::Keyword);
    let mut labels = Vec::with_capacity(params.len() + 1);
    for (index, param) in params.iter().enumerate() {
        let rest = params[..index]
            .iter()
            .any(|param| param.kind == ParameterKind::Rest);
        if first == Some(index) && !rest {
            labels.push("*".to_owned());
        }
        labels.push(param_label(param));
    }
    labels.join(", ")
}

/// The parameter labels of a function's signature.
pub(crate) fn param_labels(function: Option<&Function>) -> Vec<String> {
    function
        .map(|function| function.params.iter().map(param_label).collect())
        .unwrap_or_default()
}

/// A declaration hover: the signature in a code block and the comment block
/// directly above the declaration in the live buffer.
fn user_markdown<S: AsRef<str>>(lines: &[S], signature: &str, item: &Item) -> String {
    let mut markdown = format!("```vibe\n{signature}\n```");
    if let Some(line) = anchor_line(lines, item.position.line, &item.name) {
        let comment = comment_above(lines, line);
        if !comment.is_empty() {
            markdown.push_str("\n\n");
            markdown.push_str(&comment);
        }
    }
    markdown
}

/// The contiguous `#` comment block above a line, without its markers or the
/// machine-facing `vibe:` and `uses:` directives.
fn comment_above<S: AsRef<str>>(lines: &[S], line: usize) -> String {
    let mut block = Vec::new();
    for index in (0..line).rev() {
        let Some(rest) = line_at(lines, index as i64).trim().strip_prefix('#') else {
            break;
        };
        block.push(rest.strip_prefix(' ').unwrap_or(rest));
    }
    block.reverse();
    let mut parts: Vec<&str> = block
        .into_iter()
        .filter(|text| !text.starts_with("vibe:") && !text.starts_with("uses:"))
        .collect();
    while parts.first().is_some_and(|text| text.trim().is_empty()) {
        parts.remove(0);
    }
    while parts.last().is_some_and(|text| text.trim().is_empty()) {
        parts.pop();
    }
    parts.join("\n")
}
