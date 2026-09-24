//! Context-aware completion: member methods after a `.` receiver, otherwise
//! keywords, builtins, the document's functions and the enclosing function's
//! parameters and locals.

use super::catalog::{Catalog, catalog};
use super::docs::{
    builtin_docs, contract_signatures, keyword_doc, namespace_doc, runtime_members,
    unambiguous_member_doc,
};
use super::document::{CompletionItem, CompletionKind};
use super::text::{character_index, word_char};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, OnceLock};
use vibescript::tooling::{self, Item, ItemKind, Outline};

pub(crate) type Entries = Vec<Arc<CompletionItem>>;

fn item(label: &str, kind: CompletionKind, detail: &str) -> CompletionItem {
    CompletionItem {
        label: label.to_owned(),
        kind,
        detail: detail.to_owned(),
        documentation: None,
    }
}

/// Keywords and builtins with their documentation, sorted by label.
pub(crate) fn builtins() -> &'static Entries {
    static ENTRIES: OnceLock<Entries> = OnceLock::new();
    ENTRIES.get_or_init(|| static_entries(catalog()))
}

/// Keywords and builtins with their documentation, sorted by label.
pub(crate) fn static_entries(catalog: &Catalog) -> Entries {
    let keywords = tooling::keywords();
    let mut labels: Vec<&str> = keywords.to_vec();
    labels.extend(catalog.top_level_names.iter().map(String::as_str));
    labels.sort_unstable();
    labels
        .into_iter()
        .map(|label| {
            let mut entry = item(label, CompletionKind::Function, "builtin");
            if keywords.contains(&label) {
                entry.kind = CompletionKind::Keyword;
                entry.detail = "keyword".to_owned();
                entry.documentation = keyword_doc(label).map(|doc| format!("`{label}`\n\n{doc}"));
            } else if catalog.is_namespace(label) {
                entry.kind = CompletionKind::Module;
                entry.detail = "namespace".to_owned();
                entry.documentation = Some(namespace_doc(catalog, label));
            } else if let Some(doc) = builtin_docs().get(label) {
                entry.documentation = Some(doc.markdown.clone());
                entry.detail = doc.signature.replace('`', "");
            } else if !catalog.is_function(label) {
                entry.kind = CompletionKind::Constant;
            }
            entry.documentation = entry.documentation.filter(|doc| !doc.is_empty());
            Arc::new(entry)
        })
        .collect()
}

/// Documentation for a member item: unambiguous prose, then the registered
/// receiver-qualified contract signatures.
fn member_documentation(label: &str) -> Option<String> {
    let mut documentation = unambiguous_member_doc(label);
    let signatures = contract_signatures(label);
    if !signatures.is_empty() {
        if !documentation.is_empty() {
            documentation.push_str("\n\n");
        }
        documentation.push_str(&signatures);
    }
    (!documentation.is_empty()).then_some(documentation)
}

/// The type-unaware union of every builtin member, labeled with the receiver
/// kinds that provide it.
pub(crate) fn members() -> &'static Entries {
    static ENTRIES: OnceLock<Entries> = OnceLock::new();
    ENTRIES.get_or_init(member_entries)
}

fn member_entries() -> Entries {
    let mut by_name: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for (receiver, names) in runtime_members() {
        for name in names {
            by_name.entry(name).or_default().push(receiver);
        }
    }
    by_name
        .into_iter()
        .map(|(label, mut receivers)| {
            receivers.sort_unstable();
            let mut entry = item(label, CompletionKind::Method, &receivers.join(", "));
            entry.documentation = member_documentation(label);
            Arc::new(entry)
        })
        .collect()
}

/// The members of one receiver kind, shaped like the union items.
pub(crate) fn receiver_entries(receiver: &str) -> Option<Entries> {
    let names = runtime_members()
        .get(receiver)
        .filter(|names| !names.is_empty())?;
    let mut sorted = names.clone();
    sorted.sort_unstable();
    Some(
        sorted
            .into_iter()
            .map(|label| {
                let mut entry = item(label, CompletionKind::Method, receiver);
                entry.documentation = member_documentation(label);
                Arc::new(entry)
            })
            .collect(),
    )
}

/// Whether the cursor directly follows a `.` member access, allowing a
/// partially typed name. The dot of a float literal (`1.5`, `1.5e2`) does not
/// count, while `1.` and `1.days` do.
pub(crate) fn member_context<S: AsRef<str>>(lines: &[S], line: i64, character: i64) -> bool {
    let Some(text) = usize::try_from(line).ok().and_then(|line| lines.get(line)) else {
        return false;
    };
    let text = text.as_ref();
    let chars: Vec<char> = text.chars().collect();
    let end = character_index(text, character).min(chars.len());
    let mut start = end;
    while start > 0 && word_char(chars[start - 1]) {
        start -= 1;
    }
    if start == 0 || chars[start - 1] != '.' {
        return false;
    }
    if start >= 2 && digit(chars[start - 2]) {
        if start < end {
            if fraction_suffix(&chars[start..end]) {
                return false;
            }
        } else if chars.get(start).is_some_and(|c| digit(*c)) {
            return false;
        }
    }
    true
}

/// A decimal digit by the reference's Unicode tables.
fn digit(c: char) -> bool {
    c.is_numeric() && tooling::identifier_char(c)
}

/// Whether a suffix after a digit's dot is a float's fraction and optional
/// exponent, with `_` only between digits; a trailing `e` still counts.
fn fraction_suffix(suffix: &[char]) -> bool {
    if !suffix.first().is_some_and(|c| digit(*c)) {
        return false;
    }
    let mut index = digits(suffix, 0);
    if index == suffix.len() {
        return true;
    }
    if !matches!(suffix[index], 'e' | 'E') {
        return false;
    }
    index += 1;
    if index == suffix.len() {
        return true;
    }
    if !digit(suffix[index]) {
        return false;
    }
    digits(suffix, index) == suffix.len()
}

fn digits(chars: &[char], mut index: usize) -> usize {
    while index < chars.len() {
        let separator = chars[index] == '_'
            && index > 0
            && digit(chars[index - 1])
            && chars.get(index + 1).is_some_and(|c| digit(*c));
        if !digit(chars[index]) && !separator {
            break;
        }
        index += 1;
    }
    index
}

/// A name no script writes, spliced over the partial member name at the
/// cursor so the receiver parses the same way wherever the cursor sits.
pub(crate) const PROBE: &str = "vibesCompletionProbe__";

/// The members of the receiver at the cursor, or `None` when its kind is not
/// decidable from syntax and every member must be offered.
pub(crate) fn narrowed_entries<S: AsRef<str>>(
    source: &str,
    lines: &[S],
    line: i64,
    character: i64,
) -> Option<Entries> {
    let probed = splice_probe(source, lines, line, character)?;
    let receiver = tooling::member_receiver(&probed, PROBE)?;
    receiver_entries(receiver)
}

fn splice_probe<S: AsRef<str>>(
    source: &str,
    lines: &[S],
    line: i64,
    character: i64,
) -> Option<String> {
    let index = usize::try_from(line).ok()?;
    let text = lines.get(index)?.as_ref();
    let chars: Vec<char> = text.chars().collect();
    let end = character_index(text, character).min(chars.len());
    let mut start = end;
    while start > 0 && word_char(chars[start - 1]) {
        start -= 1;
    }
    if start == 0 || chars[start - 1] != '.' {
        return None;
    }
    let mut source_lines: Vec<&str> = source.split('\n').collect();
    if index >= source_lines.len() {
        return None;
    }
    let spliced: String = chars[..start]
        .iter()
        .chain(PROBE.chars().collect::<Vec<_>>().iter())
        .chain(chars[end..].iter())
        .collect();
    source_lines[index] = &spliced;
    Some(source_lines.join("\n"))
}

/// The script-local completion index: the document's functions, and per
/// function its parameters and locals and its rescue bindings.
#[derive(Debug)]
pub(crate) struct Index {
    items: Entries,
    scopes: Vec<Scope>,
}

#[derive(Debug)]
struct Scope {
    start: i64,
    end: i64,
    items: Entries,
    blocks: Vec<Block>,
}

#[derive(Debug)]
struct Block {
    start: i64,
    end: i64,
    items: Entries,
}

/// The zero-based line after the last statement, or zero when every statement
/// sits on the first line, as the reference measures a body.
fn last_statement_line(last: Option<vibescript::Position>) -> i64 {
    match last {
        Some(position) if position.line > 1 => position.line as i64,
        _ => 0,
    }
}

/// The document's functions and aliases with the declarations they run.
fn functions(compiled: &Outline) -> Vec<(&str, &Item)> {
    let declared: HashMap<&str, &Item> = compiled
        .items
        .iter()
        .filter(|item| item.kind == ItemKind::Function)
        .map(|item| (item.name.as_str(), item))
        .collect();
    let mut functions: Vec<(&str, &Item)> = compiled
        .items
        .iter()
        .filter_map(|item| match item.kind {
            ItemKind::Function => Some((item.name.as_str(), item)),
            ItemKind::Alias => {
                let target = item.target.as_deref()?;
                Some((item.name.as_str(), *declared.get(target)?))
            }
            _ => None,
        })
        .collect();
    functions.sort_by(|left, right| left.0.cmp(right.0));
    functions
}

impl Index {
    /// Builds the index from the last compiled outline, anchoring each
    /// function to its `def` line in the current buffer when it still exists.
    pub(crate) fn new<S: AsRef<str>>(compiled: &Outline, lines: &[S], builtins: &Entries) -> Self {
        let functions = functions(compiled);
        let def_lines = def_lines(lines);
        let mut items = builtins.clone();
        let mut scopes = Vec::with_capacity(functions.len());
        for (name, declaration) in &functions {
            items.push(Arc::new(item(name, CompletionKind::Function, "function")));
            let compiled_start = declaration.position.line as i64 - 1;
            let start = def_lines
                .get(*name)
                .map_or(compiled_start, |line| *line as i64);
            let function = declaration.function.as_ref();
            let extent =
                last_statement_line(function.and_then(|f| f.last_statement)) - compiled_start;
            let mut scope = Scope {
                start,
                end: function_end(lines, start, extent),
                items: Vec::new(),
                blocks: Vec::new(),
            };
            let mut seen = HashSet::new();
            if let Some(function) = function {
                for param in &function.params {
                    add_local(&mut scope.items, &mut seen, &param.name, "parameter");
                }
                for local in &function.locals {
                    add_local(&mut scope.items, &mut seen, local, "local");
                }
                for rescue in &function.rescues {
                    let block_start = start + (rescue.position.line as i64 - 1 - compiled_start);
                    let rescue_end = last_statement_line(rescue.last_statement);
                    let block_end = if rescue_end > 0 {
                        start + (rescue_end - compiled_start)
                    } else {
                        block_start
                    };
                    let mut block_items = Vec::new();
                    add_local(
                        &mut block_items,
                        &mut HashSet::new(),
                        &rescue.binding,
                        "local",
                    );
                    scope.blocks.push(Block {
                        start: block_start,
                        end: block_end,
                        items: block_items,
                    });
                }
            }
            scope
                .items
                .sort_by(|left, right| left.label.cmp(&right.label));
            scopes.push(scope);
        }
        let function_items = items.split_off(builtins.len());
        let mut function_items = function_items;
        function_items.sort_by(|left, right| left.label.cmp(&right.label));
        items.extend(function_items);
        Self { items, scopes }
    }

    /// The items offered on a line: the innermost enclosing function's
    /// parameters and locals, and the bindings of every rescue clause around it.
    pub(crate) fn items_at(&self, line: i64) -> Entries {
        let mut enclosing: Option<&Scope> = None;
        for scope in &self.scopes {
            if scope.start <= line
                && line <= scope.end
                && enclosing.is_none_or(|current| scope.start > current.start)
            {
                enclosing = Some(scope);
            }
        }
        let Some(scope) = enclosing else {
            return self.items.clone();
        };
        let mut items = self.items.clone();
        items.extend(scope.items.iter().cloned());
        for block in &scope.blocks {
            if block.start <= line && line <= block.end {
                items.extend(block.items.iter().cloned());
            }
        }
        items
    }
}

fn add_local(items: &mut Entries, seen: &mut HashSet<String>, name: &str, detail: &str) {
    if name.is_empty() || !seen.insert(name.to_owned()) {
        return;
    }
    items.push(Arc::new(item(name, CompletionKind::Variable, detail)));
}

/// The first line declaring each top-level `def name`, after an optional
/// `export` or `private` modifier.
fn def_lines<S: AsRef<str>>(lines: &[S]) -> HashMap<String, usize> {
    let mut found = HashMap::new();
    for (index, line) in lines.iter().enumerate() {
        let mut decl = line.as_ref();
        for modifier in ["export ", "private "] {
            if let Some(rest) = decl.strip_prefix(modifier) {
                decl = rest;
                break;
            }
        }
        let Some(rest) = decl.strip_prefix("def ") else {
            continue;
        };
        let end = rest.find(['(', ' ', '\t']).unwrap_or(rest.len());
        let name = &rest[..end];
        if !name.is_empty() {
            found.entry(name.to_owned()).or_insert(index);
        }
    }
    found
}

/// Estimates the line of the `end` closing a top-level function: the later of
/// the first unindented `end` and the line after the body's last statement.
/// A buffer with neither signal leaves the function open to the end.
fn function_end<S: AsRef<str>>(lines: &[S], start: i64, extent: i64) -> i64 {
    let count = lines.len() as i64;
    let first = usize::try_from(start + 1).unwrap_or(0);
    let textual = lines
        .iter()
        .enumerate()
        .skip(first)
        .find(|(_, line)| line.as_ref().trim_end_matches([' ', '\t']) == "end")
        .map_or(count, |(index, _)| index as i64);
    if extent > 0 && textual < count {
        return textual.max(start + extent);
    }
    textual
}

/// The completion items at a position.
pub(crate) struct Request<'a, S> {
    pub source: &'a str,
    pub lines: &'a [S],
    pub line: i64,
    pub character: i64,
}

impl<S: AsRef<str>> Request<'_, S> {
    /// Member methods after a `.`, narrowed to the receiver's kind when the
    /// syntax decides it; otherwise the index for the line, or the static
    /// keywords and builtins when the document never compiled.
    pub(crate) fn entries(&self, index: Option<&Index>) -> Entries {
        if member_context(self.lines, self.line, self.character) {
            return narrowed_entries(self.source, self.lines, self.line, self.character)
                .unwrap_or_else(|| members().clone());
        }
        match index {
            Some(index) => index.items_at(self.line),
            None => builtins().clone(),
        }
    }
}
