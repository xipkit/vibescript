//! Hover and completion documentation parsed from the builtin reference's
//! markdown in `reference/`.
//!
//! The guides began as copies of the Go reference's and now document the
//! canonical names with the signature table's signatures; `tests/docs.rs`
//! compiles their examples.

use super::catalog::Catalog;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::OnceLock;

const BUILTINS: &str = include_str!("reference/builtins.md");
const STDLIB: &str = include_str!("reference/stdlib_core_utilities.md");
const STRINGS: &str = include_str!("reference/strings.md");
const ARRAYS: &str = include_str!("reference/arrays.md");
const HASHES: &str = include_str!("reference/hashes.md");
const TIME: &str = include_str!("reference/time.md");
const DURATIONS: &str = include_str!("reference/durations.md");

/// One builtin documentation entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct BuiltinDoc {
    /// The code-formatted usage line, such as "`assert(condition, message)`".
    pub signature: String,
    /// The signature line and the entry's description paragraphs.
    pub markdown: String,
}

/// Builtin documentation keyed by bare (`puts`) and qualified (`JSON.parse_as`) names.
pub(crate) fn builtin_docs() -> &'static HashMap<String, BuiltinDoc> {
    static DOCS: OnceLock<HashMap<String, BuiltinDoc>> = OnceLock::new();
    DOCS.get_or_init(|| parse_builtin_docs(BUILTINS))
}

/// Builds the name to documentation table from the builtins reference.
///
/// Two shapes register entries: "### `name(sig)`" headings, named by every
/// code span in the heading and described by the following paragraphs, and
/// "- `Namespace.member(sig)` – description" bullets for qualified names.
/// `::` accessors normalize to dotted names, and the first entry for a name wins.
pub(crate) fn parse_builtin_docs(markdown: &str) -> HashMap<String, BuiltinDoc> {
    let mut entries = HashMap::new();
    let lines: Vec<&str> = markdown.split('\n').collect();
    let mut fence = false;
    let mut index = 0;
    while index < lines.len() {
        let line = lines[index];
        let trimmed = line.trim();
        if trimmed.starts_with("```") {
            fence = !fence;
        } else if !fence {
            if let Some(heading) = line.strip_prefix("### ") {
                let refs = entry_refs(heading, false);
                if !refs.is_empty() {
                    add_entries(
                        &mut entries,
                        &refs,
                        heading.trim(),
                        &description(&lines, index + 1),
                    );
                }
            } else if line.starts_with("- `") {
                let (bullet, last) = joined_bullet(&lines, index);
                index = last;
                if let Some((signature, description)) = split_bullet(&bullet) {
                    let refs = entry_refs(&signature, true);
                    if !refs.is_empty() {
                        add_entries(&mut entries, &refs, &signature, &description);
                    }
                }
            }
        }
        index += 1;
    }
    entries
}

/// The text paragraphs after a heading, up to the next heading, skipping
/// fenced code and bullet lists.
fn description(lines: &[&str], start: usize) -> String {
    let mut paragraphs = Vec::new();
    let mut current: Vec<&str> = Vec::new();
    let flush = |current: &mut Vec<&str>, paragraphs: &mut Vec<String>| {
        if !current.is_empty() {
            paragraphs.push(current.join("\n"));
            current.clear();
        }
    };
    let mut fence = false;
    let mut index = start;
    while index < lines.len() {
        let trimmed = lines[index].trim();
        if trimmed.starts_with("```") {
            fence = !fence;
            flush(&mut current, &mut paragraphs);
        } else if !fence {
            if lines[index].starts_with('#') {
                break;
            }
            if trimmed.is_empty() {
                flush(&mut current, &mut paragraphs);
            } else if trimmed.starts_with("- ") || trimmed.starts_with("* ") {
                index = joined_bullet(lines, index).1;
            } else {
                current.push(trimmed);
            }
        }
        index += 1;
    }
    flush(&mut current, &mut paragraphs);
    paragraphs.join("\n\n")
}

/// Joins a bullet with its indented continuation lines, returning the text and
/// the index of the bullet's last line.
fn joined_bullet(lines: &[&str], mut index: usize) -> (String, usize) {
    let mut parts = vec![lines[index].trim()];
    while let Some(next) = lines.get(index + 1) {
        let trimmed = next.trim();
        if trimmed.is_empty() || !next.starts_with("  ") || trimmed.starts_with("- ") {
            break;
        }
        parts.push(trimmed);
        index += 1;
    }
    (parts.join(" "), index)
}

/// Splits "- `name(sig)` – description" around its en or em dash.
fn split_bullet(bullet: &str) -> Option<(String, String)> {
    let body = bullet.strip_prefix("- ").unwrap_or(bullet);
    for separator in [" – ", " — "] {
        if let Some(index) = body.find(separator).filter(|index| *index > 0) {
            return Some((
                body[..index].trim().to_owned(),
                body[index + separator.len()..].trim().to_owned(),
            ));
        }
    }
    None
}

/// The code spans of `text`: runs of non-backtick characters between backticks.
fn code_spans(text: &str) -> Vec<(&str, &str)> {
    let mut spans = Vec::new();
    let mut index = 0;
    while let Some(open) = text[index..].find('`').map(|found| index + found) {
        let Some(close) = text[open + 1..].find('`').map(|found| open + 1 + found) else {
            break;
        };
        if close == open + 1 {
            index = open + 1;
            continue;
        }
        spans.push((&text[open..=close], &text[open + 1..close]));
        index = close + 1;
    }
    spans
}

/// Whether `name` is an identifier with an optional `?` or `!` suffix.
fn member_name(name: &str) -> bool {
    let name = name
        .strip_suffix('?')
        .or_else(|| name.strip_suffix('!'))
        .unwrap_or(name);
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Entry names and their individual signatures in a heading or bullet.
fn entry_refs(text: &str, qualified: bool) -> Vec<(String, String)> {
    let mut refs: Vec<(String, String)> = Vec::new();
    for (span, inner) in code_spans(text) {
        let name = inner
            .find([' ', '(', '{', '<'])
            .map_or(inner, |cut| &inner[..cut])
            .replace("::", ".");
        if !name.split('.').all(member_name) {
            continue;
        }
        if qualified && !name.contains('.') {
            continue;
        }
        if refs.iter().any(|(seen, _)| *seen == name) {
            continue;
        }
        refs.push((name, span.to_owned()));
    }
    refs
}

fn add_entries(
    entries: &mut HashMap<String, BuiltinDoc>,
    refs: &[(String, String)],
    signature: &str,
    description: &str,
) {
    let mut markdown = signature.to_owned();
    if !description.is_empty() {
        markdown.push_str("\n\n");
        markdown.push_str(description);
    }
    for (name, signature) in refs {
        entries.entry(name.clone()).or_insert_with(|| BuiltinDoc {
            signature: signature.clone(),
            markdown: markdown.clone(),
        });
    }
}

/// Declaration words that are not reserved but still deserve keyword docs.
#[cfg(test)]
pub(crate) const CONTEXTUAL_WORDS: [&str; 5] =
    ["alias", "alias_method", "module", "protected", "public"];

/// One-line descriptions of every reserved keyword and contextual word.
pub(crate) fn keyword_doc(word: &str) -> Option<&'static str> {
    Some(match word {
        "begin" => {
            "Opens a block whose failures are handled by `rescue` clauses, with optional `else` and `ensure`."
        }
        "break" => "Exits the nearest loop; `break value` becomes the loop's value.",
        "case" => {
            "Opens a multi-branch match; over an enum or `bool` it must handle every value or have an `else`."
        }
        "class" => {
            "Declares a class grouping behavior and methods; instances are created with `Klass.new`."
        }
        "def" => {
            "Declares a function or method with typed parameters and an optional `-> T` result."
        }
        "do" => "Removed block opener: blocks are written with braces, `{ |args| ... }`.",
        "else" => "Fallback branch of an `if`, `case`, or `begin`/`rescue`.",
        "elsif" => "Adds another condition branch to an `if`.",
        "end" => "Closes a `def`, `class`, `module`, `if`, `while`, or other opener.",
        "ensure" => "Runs cleanup code whether the protected body raised or not.",
        "enum" => "Declares a nominal state set; members are accessed with `::` (`Status::Draft`).",
        "export" => "Marks a top-level function as exported from a module file.",
        "false" => "Boolean false literal.",
        "for" => "Iterates over a collection: `for item in items ... end`.",
        "getter" => {
            "Declares a read-only accessor backed by the instance variable of the same name."
        }
        "if" => {
            "Runs its body when the `bool` condition is true; also usable as a modifier and as an expression."
        }
        "in" => "Separates the loop variable from the collection in `for ... in`.",
        "next" => "Skips to the next iteration of the nearest loop.",
        "nil" => "The absence-of-value literal.",
        "private" => {
            "Marks subsequent (or one prefixed) method declarations as internal to the class or module."
        }
        "property" => {
            "Declares a read-write accessor (`x` and `x=`) backed by an instance variable."
        }
        "raise" => "Raises an error, unwinding to the nearest matching `rescue`.",
        "rescue" => {
            "Handles an error raised in the preceding `begin` or method body; also an expression modifier."
        }
        "retry" => "Re-runs the `begin` body from inside a `rescue` handler.",
        "return" => "Exits the enclosing function with an optional value.",
        "self" => "The current instance; `def self.name` declares a class method.",
        "setter" => "Declares a write-only accessor (`x=`) backed by an instance variable.",
        "then" => "Optional separator between a condition and its single-line body.",
        "true" => "Boolean true literal.",
        "unless" => "Removed: write `if !condition`.",
        "until" => "Removed: write `while !condition`.",
        "when" => "One branch of a `case`: a value, range or regex to match, then one expression.",
        "while" => {
            "Loops while its `bool` condition stays true; also usable as a statement modifier."
        }
        "yield" => {
            "Invokes the block the current function declares with a typed `&block` parameter."
        }
        "alias" => "Declares an alternate name for a method: `alias new_name old_name`.",
        "alias_method" => "Declares an alternate method name: `alias_method :new_name, :old_name`.",
        "module" => {
            "Declares a namespace of `def self.` functions, constants, and nested modules; contextual — it only opens a declaration before a constant name."
        }
        "protected" => "Marks subsequent methods callable only from instances of the same class.",
        "public" => "Restores public visibility for subsequent method declarations.",
        _ => return None,
    })
}

/// Every word with a keyword description, for coverage checks.
#[cfg(test)]
pub(crate) fn keyword_doc_words() -> Vec<&'static str> {
    let candidates = vibescript::tooling::keywords()
        .iter()
        .copied()
        .chain(CONTEXTUAL_WORDS);
    candidates
        .filter(|word| keyword_doc(word).is_some())
        .collect()
}

/// The pseudo-receiver of the helpers every value answers.
pub(crate) const UNIVERSAL: &str = "universal";

/// One member documentation entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MemberDoc {
    pub receiver: String,
    pub signature: String,
    pub markdown: String,
}

/// Member documentation: typed entries per name, sorted by receiver, and the
/// universal helpers documented once.
pub(crate) struct MemberDocs {
    pub entries: HashMap<String, Vec<MemberDoc>>,
    pub universal: HashMap<String, MemberDoc>,
}

/// Section titles of the stdlib reference and the receiver each documents.
fn stdlib_section(title: &str) -> &'static str {
    match title {
        "Strings" => "string",
        "Arrays" => "array",
        "Hashes" => "hash",
        "Integers" => "int",
        "Floats" => "float",
        "Money" => "money",
        "Durations" => "duration",
        "Times" => "time",
        "Symbols" => "symbol",
        "Ranges" => "range",
        "Universal Members"
        | "Universal Predicates"
        | "Debug Representation"
        | "Universal Methods"
        | "Object Helpers"
        | "Object Introspection" => UNIVERSAL,
        _ => "",
    }
}

/// Member names per receiver kind, as the runtime dispatches them.
pub(crate) fn runtime_members() -> &'static BTreeMap<&'static str, Vec<&'static str>> {
    static MEMBERS: OnceLock<BTreeMap<&'static str, Vec<&'static str>>> = OnceLock::new();
    MEMBERS.get_or_init(|| vibescript::tooling::member_names().into_iter().collect())
}

/// The receivers dispatching each member name.
fn runtime_index() -> &'static HashMap<&'static str, HashSet<&'static str>> {
    static INDEX: OnceLock<HashMap<&'static str, HashSet<&'static str>>> = OnceLock::new();
    INDEX.get_or_init(|| {
        let mut index: HashMap<&str, HashSet<&str>> = HashMap::new();
        for (receiver, names) in runtime_members() {
            for name in names {
                index.entry(name).or_default().insert(receiver);
            }
        }
        index
    })
}

/// The member documentation index, parsed once. The compact stdlib reference
/// comes first so its hover-sized entries win over the narrative guides.
pub(crate) fn member_docs() -> &'static MemberDocs {
    static DOCS: OnceLock<MemberDocs> = OnceLock::new();
    DOCS.get_or_init(|| {
        let mut docs = MemberDocs {
            entries: HashMap::new(),
            universal: HashMap::new(),
        };
        docs.parse(STDLIB, None);
        docs.parse(STRINGS, Some("string"));
        docs.parse(ARRAYS, Some("array"));
        docs.parse(HASHES, Some("hash"));
        docs.parse(TIME, Some("time"));
        docs.parse(DURATIONS, Some("duration"));
        docs.demote_partial_universals();
        for entries in docs.entries.values_mut() {
            entries.sort_by(|left, right| left.receiver.cmp(&right.receiver));
        }
        docs
    })
}

impl MemberDocs {
    /// Parses one documentation file. A fixed receiver documents the whole
    /// file; otherwise the receiver follows the current "## " section.
    fn parse(&mut self, markdown: &str, fixed: Option<&str>) {
        let lines: Vec<&str> = markdown.split('\n').collect();
        let mut receiver = fixed.unwrap_or("").to_owned();
        let mut fence = false;
        let mut index = 0;
        while index < lines.len() {
            let line = lines[index];
            if line.trim().starts_with("```") {
                fence = !fence;
            } else if !fence {
                if let Some(title) = line.strip_prefix("## ").filter(|_| fixed.is_none()) {
                    receiver = stdlib_section(title.trim()).to_owned();
                } else if let Some(heading) = line.strip_prefix("### ") {
                    let refs = leading_member_names(heading, &receiver).0;
                    if !refs.is_empty() {
                        self.add(&refs, heading.trim(), &description(&lines, index + 1));
                    }
                } else if line.starts_with("- `") {
                    let (bullet, last) = joined_bullet(&lines, index);
                    index = last;
                    let body = bullet.strip_prefix("- ").unwrap_or(&bullet);
                    match split_bullet(&bullet) {
                        Some((signature, description)) => {
                            let refs = leading_member_names(body, &receiver).0;
                            self.add(&refs, &signature, &description);
                        }
                        None => {
                            // A bullet that is only a run of code spans is an
                            // enumeration line, resolved by the bang fallback.
                            let (refs, rest) = leading_member_names(body, &receiver);
                            if !rest.trim().is_empty() {
                                self.add(&refs, body, "");
                            }
                        }
                    }
                }
            }
            index += 1;
        }
    }

    fn add(&mut self, refs: &[(String, String)], signature: &str, description: &str) {
        let mut markdown = signature.to_owned();
        if !description.is_empty() {
            markdown.push_str("\n\n");
            markdown.push_str(description);
        }
        for (receiver, name) in refs {
            let doc = MemberDoc {
                receiver: receiver.clone(),
                signature: signature.to_owned(),
                markdown: markdown.clone(),
            };
            if receiver == UNIVERSAL {
                self.universal.entry(name.clone()).or_insert(doc);
                continue;
            }
            let entries = self.entries.entry(name.clone()).or_default();
            if !entries.iter().any(|entry| entry.receiver == *receiver) {
                entries.push(doc);
            }
        }
    }

    /// Moves universal entries the runtime does not dispatch on every receiver
    /// to typed entries for the receivers that do.
    fn demote_partial_universals(&mut self) {
        let receivers = runtime_members();
        let names: Vec<String> = self.universal.keys().cloned().collect();
        for name in names {
            let dispatching = runtime_index().get(name.as_str());
            let universal = dispatching.is_some_and(|dispatching| {
                !dispatching.is_empty()
                    && receivers
                        .keys()
                        .all(|receiver| dispatching.contains(receiver))
            });
            if universal {
                continue;
            }
            let entry = self.universal.remove(&name).expect("the name was listed");
            for receiver in dispatching.into_iter().flatten() {
                if !receivers.contains_key(receiver) {
                    continue;
                }
                let entries = self.entries.entry(name.clone()).or_default();
                if !entries
                    .iter()
                    .any(|existing| existing.receiver == *receiver)
                {
                    entries.push(MemberDoc {
                        receiver: (*receiver).to_owned(),
                        signature: entry.signature.clone(),
                        markdown: entry.markdown.clone(),
                    });
                }
            }
        }
    }
}

/// The members named by the leading run of code spans in `text`, as
/// (receiver, name) pairs, and the text after the run.
fn leading_member_names(text: &str, receiver: &str) -> (Vec<(String, String)>, String) {
    let mut rest = text.trim().to_owned();
    let mut refs: Vec<(String, String)> = Vec::new();
    loop {
        rest = trim_separators(&rest);
        let Some(after) = rest.strip_prefix('`') else {
            return (refs, rest);
        };
        let Some(end) = after.find('`') else {
            return (refs, rest);
        };
        let span = &after[..end];
        let next = after[end + 1..].to_owned();
        let Some(reference) = member_ref(span, receiver) else {
            return (refs, rest);
        };
        rest = next;
        if !refs.contains(&reference) {
            refs.push(reference);
        }
    }
}

/// Removes the separators an alias list uses between its code spans.
fn trim_separators(text: &str) -> String {
    let mut text = text.to_owned();
    loop {
        let mut trimmed = text.trim_start_matches([' ', '\t', '/', ',']);
        for word in ["and ", "or "] {
            if let Some(rest) = trimmed.strip_prefix(word) {
                if rest.trim_start_matches(' ').starts_with('`') {
                    trimmed = rest;
                }
            }
        }
        if trimmed.starts_with('(') {
            if let Some(rest) = cut_balanced_parens(trimmed) {
                trimmed = rest;
            }
        }
        if trimmed == text {
            return text;
        }
        text = trimmed.to_owned();
    }
}

fn cut_balanced_parens(text: &str) -> Option<&str> {
    let mut depth = 0;
    for (index, c) in text.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&text[index + 1..]);
                }
            }
            _ => (),
        }
    }
    None
}

/// Resolves one code span to a member: a member name optionally followed by
/// type parameters, arguments, a block shape or a result type, as in
/// `map<U>(&block: T -> U) -> array<U>`. "type.name" names its own receiver,
/// which must be a runtime receiver kind.
fn member_ref(span: &str, receiver: &str) -> Option<(String, String)> {
    let (name, rest) = match span.find([' ', '(', '{', '<']) {
        Some(cut) => (&span[..cut], &span[cut..]),
        None => (span, ""),
    };
    let rest = skip_type_parameters(rest).trim_start_matches(' ');
    if !rest.is_empty()
        && !rest.starts_with('(')
        && !rest.starts_with('{')
        && !rest.starts_with("->")
    {
        return None;
    }
    if let Some((prefix, member)) = name.split_once('.') {
        if !runtime_members().contains_key(prefix) || !member_name(member) {
            return None;
        }
        return Some((prefix.to_owned(), member.to_owned()));
    }
    if receiver.is_empty() || !member_name(name) {
        return None;
    }
    Some((receiver.to_owned(), name.to_owned()))
}

/// The text after a leading `<...>` type parameter list, or all of `text`.
fn skip_type_parameters(text: &str) -> &str {
    if !text.starts_with('<') {
        return text;
    }
    let mut depth = 0;
    for (index, c) in text.char_indices() {
        match c {
            '<' => depth += 1,
            '>' => {
                depth -= 1;
                if depth == 0 {
                    return &text[index + 1..];
                }
            }
            _ => (),
        }
    }
    text
}

/// Hover markdown for a value member: the universal helper, the single typed
/// entry, or one section per receiver group separated by rules. Empty when
/// the member is undocumented.
pub(crate) fn member_doc_markdown(word: &str) -> String {
    let docs = member_docs();
    if let Some(entry) = docs.universal.get(word) {
        return entry.markdown.clone();
    }
    let entries = match docs.entries.get(word) {
        Some(entries) if !entries.is_empty() => entries.clone(),
        _ => bang_variant_entries(word),
    };
    match entries.len() {
        0 => return String::new(),
        1 => return entries[0].markdown.clone(),
        _ => (),
    }
    let mut groups: Vec<(String, Vec<String>)> = Vec::new();
    for entry in entries {
        match groups
            .iter_mut()
            .find(|(markdown, _)| *markdown == entry.markdown)
        {
            Some((_, receivers)) => receivers.push(entry.receiver),
            None => groups.push((entry.markdown, vec![entry.receiver])),
        }
    }
    if groups.len() == 1 {
        return groups.remove(0).0;
    }
    let sections: Vec<String> = groups
        .into_iter()
        .map(|(markdown, receivers)| {
            let labels: Vec<String> = receivers
                .iter()
                .map(|receiver| format!("`{receiver}.{word}`"))
                .collect();
            format!("{}\n\n{markdown}", labels.join(" / "))
        })
        .collect();
    sections.join("\n\n---\n\n")
}

/// Documentation for an in-place bang variant composed from its base member,
/// for the receivers that dispatch the bang name.
fn bang_variant_entries(word: &str) -> Vec<MemberDoc> {
    let Some(base) = word.strip_suffix('!').filter(|base| !base.is_empty()) else {
        return Vec::new();
    };
    let Some(receivers) = runtime_index().get(word).filter(|set| !set.is_empty()) else {
        return Vec::new();
    };
    let note = if word == "sub!" || word == "gsub!" {
        format!(
            "In-place variant of `{base}`: returns the rewritten string whenever the pattern matched, and `nil` only when it never matched."
        )
    } else {
        format!(
            "In-place variant of `{base}`: returns the transformed value, or `nil` when nothing changed."
        )
    };
    member_docs()
        .entries
        .get(base)
        .into_iter()
        .flatten()
        .filter(|entry| receivers.contains(entry.receiver.as_str()))
        .map(|entry| MemberDoc {
            receiver: entry.receiver.clone(),
            signature: format!("`{word}`"),
            markdown: format!("`{word}`\n\n{note}\n\n{}", entry.markdown),
        })
        .collect()
}

/// Member documentation for completion items, which have no receiver: only a
/// universal helper or a name with one documenting receiver qualifies.
pub(crate) fn unambiguous_member_doc(name: &str) -> String {
    let docs = member_docs();
    if let Some(entry) = docs.universal.get(name) {
        return entry.markdown.clone();
    }
    if let Some([entry]) = docs.entries.get(name).map(Vec::as_slice) {
        return entry.markdown.clone();
    }
    if let [entry] = bang_variant_entries(name).as_slice() {
        return entry.markdown.clone();
    }
    String::new()
}

/// The introduction of each "## " section of the builtins reference.
fn namespace_intros() -> &'static HashMap<String, String> {
    static INTROS: OnceLock<HashMap<String, String>> = OnceLock::new();
    INTROS.get_or_init(|| {
        let mut table = HashMap::new();
        let mut current = String::new();
        let mut intro: Vec<&str> = Vec::new();
        let flush = |current: &str, intro: &[&str], table: &mut HashMap<String, String>| {
            let text = intro.join("\n").trim().to_owned();
            if !current.is_empty() && !text.is_empty() {
                table.insert(current.to_owned(), text);
            }
        };
        for line in BUILTINS.split('\n') {
            if let Some(title) = line.strip_prefix("## ") {
                flush(&current, &intro, &mut table);
                current = title.trim().to_owned();
                intro.clear();
            } else if line.starts_with("### ") {
                flush(&current, &intro, &mut table);
                current.clear();
                intro.clear();
            } else if !current.is_empty() {
                intro.push(line);
            }
        }
        flush(&current, &intro, &mut table);
        // Proc and Regexp have no section of their own.
        table.entry("Proc".to_owned()).or_insert_with(|| {
            "Removed callable constructor: `Proc.new` fails with a teaching error, since executable code is not a value. Define a named function or attach a block instead.".to_owned()
        });
        table.entry("Regexp".to_owned()).or_insert_with(|| {
            "Ruby-style regular expression helpers for building and inspecting regex values."
                .to_owned()
        });
        table
    })
}

/// Hover markdown for a namespace: its introduction and documented members.
pub(crate) fn namespace_doc(catalog: &Catalog, word: &str) -> String {
    if !catalog.is_namespace(word) {
        return String::new();
    }
    let prefix = format!("{word}.");
    let mut members: Vec<&str> = builtin_docs()
        .keys()
        .filter_map(|name| name.strip_prefix(&prefix))
        .collect();
    members.sort_unstable();
    let mut markdown = format!("`{word}`");
    if let Some(intro) = namespace_intros().get(word) {
        markdown.push_str("\n\n");
        markdown.push_str(intro);
    }
    if !members.is_empty() {
        markdown.push_str("\n\nMembers: `");
        markdown.push_str(&members.join("`, `"));
        markdown.push('`');
    }
    markdown
}

/// The signature table's member signatures by member name, each a
/// receiver-qualified line such as
/// `` `array<T>.map<U>(&block: T -> U) -> array<U>` ``, one per overload, in
/// table order. Members of every type are written without a receiver.
fn member_signatures() -> &'static HashMap<&'static str, Vec<String>> {
    static INDEX: OnceLock<HashMap<&'static str, Vec<String>>> = OnceLock::new();
    INDEX.get_or_init(|| {
        let mut index: HashMap<&str, Vec<String>> = HashMap::new();
        for item in &vibescript::signatures::table().items {
            let vibescript::signatures::Item::Class(class) = item else {
                continue;
            };
            let every = matches!(class.receiver, vibescript::signatures::Type::Var(_));
            let receiver = class.pattern();
            for member in &class.members {
                let vibescript::signatures::Member::Function(function) = member else {
                    continue;
                };
                let declaration = function.to_string();
                let signature = declaration.strip_prefix("def ").unwrap_or(&declaration);
                let line = if every {
                    format!("`{signature}`")
                } else {
                    format!("`{receiver}.{signature}`")
                };
                index.entry(function.name.as_str()).or_default().push(line);
            }
        }
        index
    })
}

/// Renders one member's signatures from the signature table, one line per
/// receiver and overload, or nothing for a name the table does not list,
/// such as a removed spelling.
pub(crate) fn table_signatures(label: &str) -> String {
    member_signatures()
        .get(label)
        .map(|lines| lines.join("\n"))
        .unwrap_or_default()
}
