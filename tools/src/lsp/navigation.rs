//! Go-to-definition and the document outline.
//!
//! Declarations come from the most recent parse of the document, which can
//! predate edits that shifted its lines, so every declaration is re-anchored
//! to the line that declares it in the live buffer. A declaration the buffer
//! no longer contains is dropped rather than pointing into unrelated text.

use super::document::{Position, Range, Symbol, SymbolKind};
use super::text::{utf16_character, word_char};
use vibescript::tooling::{Item, ItemKind, Outline};

/// The location of the top-level declaration named `word`: a function, class
/// or module and their members, or an enum and its members. A setter's `name=`
/// declaration matches the bare word when nothing else does.
pub(crate) fn definition<S: AsRef<str>>(
    program: Option<&Outline>,
    lines: &[S],
    word: &str,
) -> Option<Range> {
    let program = program?;
    if word.is_empty() {
        return None;
    }
    [word.to_owned(), format!("{word}=")]
        .iter()
        .find_map(|candidate| exact_definition(program, lines, candidate))
}

fn exact_definition<S: AsRef<str>>(program: &Outline, lines: &[S], word: &str) -> Option<Range> {
    for item in &program.items {
        match item.kind {
            ItemKind::Function if item.name == word => {
                return anchored_location(lines, item, &item.name);
            }
            ItemKind::Class | ItemKind::Module => {
                if let Some(location) = class_definition(item, lines, word) {
                    return Some(location);
                }
            }
            ItemKind::Enum => {
                if item.name == word {
                    return anchored_location(lines, item, &item.name);
                }
                if let Some(member) = item.children.iter().find(|member| member.name == word) {
                    return anchored_location(lines, member, &member.name);
                }
            }
            _ => (),
        }
    }
    None
}

/// Resolves a word within one class or module: its name, its methods, its
/// module constants and its nested modules.
fn class_definition<S: AsRef<str>>(item: &Item, lines: &[S], word: &str) -> Option<Range> {
    if item.name == word {
        return anchored_location(lines, item, &item.name);
    }
    for kind in [ItemKind::Method, ItemKind::ClassMethod, ItemKind::Constant] {
        if let Some(member) = children(item, kind).find(|member| member.name == word) {
            return anchored_location(lines, member, &member.name);
        }
    }
    children(item, ItemKind::Module).find_map(|nested| class_definition(nested, lines, word))
}

/// A class or module's children of one kind, in declaration order.
pub(crate) fn children(item: &Item, kind: ItemKind) -> impl Iterator<Item = &Item> {
    let constants = kind != ItemKind::Constant || item.kind == ItemKind::Module;
    item.children
        .iter()
        .filter(move |child| constants && child.kind == kind)
}

fn anchored_location<S: AsRef<str>>(lines: &[S], item: &Item, name: &str) -> Option<Range> {
    let line = anchor_line(lines, item.position.line, name)?;
    Some(location(lines, line, name))
}

/// The zero-based line currently declaring `name`: the recorded line when it
/// still matches, or else the first matching line in the buffer.
pub(crate) fn anchor_line<S: AsRef<str>>(
    lines: &[S],
    recorded: usize,
    name: &str,
) -> Option<usize> {
    let recorded = recorded.checked_sub(1);
    if let Some(line) = recorded.filter(|line| declares(lines, *line, name)) {
        return Some(line);
    }
    (0..lines.len()).find(|line| declares(lines, *line, name))
}

/// Whether a line declares `name`: a `def`, `class`, `module` or `enum`
/// declaration after optional modifiers and a `self.` receiver, a bare enum
/// member, or a constant assignment.
fn declares<S: AsRef<str>>(lines: &[S], line: usize, name: &str) -> bool {
    let Some(text) = lines.get(line) else {
        return false;
    };
    let mut text = text.as_ref().trim();
    for modifier in ["export ", "private ", "protected ", "public "] {
        text = text.strip_prefix(modifier).unwrap_or(text);
    }
    for keyword in ["def ", "class ", "module ", "enum "] {
        let Some(rest) = text.strip_prefix(keyword) else {
            continue;
        };
        let rest = rest.strip_prefix("self.").unwrap_or(rest);
        let Some(tail) = rest.strip_prefix(name) else {
            return false;
        };
        return tail.chars().next().is_none_or(|c| !word_char(c));
    }
    if text == name {
        return true;
    }
    constant_name(name) && constant_assignment(text, name)
}

/// Whether a name is spelled like a constant, with a leading uppercase letter.
pub(crate) fn constant_name(name: &str) -> bool {
    name.chars()
        .next()
        .is_some_and(vibescript::tooling::uppercase)
}

/// Whether `text` assigns `name` with a plain `=`, not `==`.
fn constant_assignment(text: &str, name: &str) -> bool {
    let Some(rest) = text.strip_prefix(name) else {
        return false;
    };
    let rest = rest.trim_start_matches([' ', '\t']);
    rest.starts_with('=') && !rest.starts_with("==")
}

/// The range of the declared name on a line. Setter names match their bare
/// spelling, since the name follows the declaration keyword.
fn location<S: AsRef<str>>(lines: &[S], line: usize, name: &str) -> Range {
    let text = lines.get(line).map_or("", AsRef::as_ref);
    let bare = name.strip_suffix('=').unwrap_or(name);
    let start = find_word(text, bare, 0).unwrap_or(0);
    let start_character = utf16_character(text, start);
    let mut end_character = utf16_character(text, start + bare.chars().count());
    if end_character <= start_character {
        end_character = start_character + 1;
    }
    let line = clamp(line);
    Range {
        start: Position::new(line, clamp(start_character)),
        end: Position::new(line, clamp(end_character)),
    }
}

fn clamp(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

/// The character column of the first whole-word occurrence of `name` at or
/// after `from`.
pub(crate) fn find_word(text: &str, name: &str, from: usize) -> Option<usize> {
    if name.is_empty() {
        return None;
    }
    let chars: Vec<char> = text.chars().collect();
    let needle: Vec<char> = name.chars().collect();
    let mut index = from;
    while index + needle.len() <= chars.len() {
        let end = index + needle.len();
        if chars[index..end] == needle[..]
            && (index == 0 || !word_char(chars[index - 1]))
            && chars.get(end).is_none_or(|c| !word_char(*c))
        {
            return Some(index);
        }
        index += 1;
    }
    None
}

/// The outline: functions, classes and modules with their methods, constants
/// and nested modules, and enums with their members.
pub(crate) fn symbols<S: AsRef<str>>(program: Option<&Outline>, lines: &[S]) -> Vec<Symbol> {
    let Some(program) = program else {
        return Vec::new();
    };
    let mut symbols = Vec::new();
    for item in &program.items {
        match item.kind {
            ItemKind::Function => {
                push_symbol(
                    &mut symbols,
                    lines,
                    item,
                    &item.name,
                    SymbolKind::Function,
                    Vec::new(),
                );
            }
            ItemKind::Class | ItemKind::Module => push_class(&mut symbols, lines, item),
            ItemKind::Enum => {
                let mut members = Vec::new();
                for member in &item.children {
                    let kind = SymbolKind::EnumMember;
                    push_symbol(&mut members, lines, member, &member.name, kind, Vec::new());
                }
                push_symbol(
                    &mut symbols,
                    lines,
                    item,
                    &item.name,
                    SymbolKind::Enum,
                    members,
                );
            }
            _ => (),
        }
    }
    symbols
}

fn push_class<S: AsRef<str>>(symbols: &mut Vec<Symbol>, lines: &[S], item: &Item) {
    let mut members = Vec::new();
    for method in children(item, ItemKind::Method) {
        push_symbol(
            &mut members,
            lines,
            method,
            &method.name,
            SymbolKind::Method,
            Vec::new(),
        );
    }
    for method in children(item, ItemKind::ClassMethod) {
        let name = format!("self.{}", method.name);
        push_symbol(
            &mut members,
            lines,
            method,
            &name,
            SymbolKind::Method,
            Vec::new(),
        );
    }
    for constant in children(item, ItemKind::Constant) {
        let kind = SymbolKind::Constant;
        push_symbol(
            &mut members,
            lines,
            constant,
            &constant.name,
            kind,
            Vec::new(),
        );
    }
    for nested in children(item, ItemKind::Module) {
        push_class(&mut members, lines, nested);
    }
    let kind = if item.kind == ItemKind::Module {
        SymbolKind::Module
    } else {
        SymbolKind::Class
    };
    push_symbol(symbols, lines, item, &item.name, kind, members);
}

/// Appends a symbol re-anchored in the live buffer, or drops it with its
/// children when the buffer no longer declares it.
fn push_symbol<S: AsRef<str>>(
    symbols: &mut Vec<Symbol>,
    lines: &[S],
    item: &Item,
    name: &str,
    kind: SymbolKind,
    children: Vec<Symbol>,
) {
    if let Some(line) = anchor_line(lines, item.position.line, &item.name) {
        symbols.push(symbol(name, kind, line, lines, children));
    }
}

/// One outline entry: the selection covers the declaration line, and the full
/// range extends to the last child so clients can nest breadcrumbs.
pub(crate) fn symbol<S: AsRef<str>>(
    name: &str,
    kind: SymbolKind,
    line: usize,
    lines: &[S],
    children: Vec<Symbol>,
) -> Symbol {
    let text = lines.get(line).map_or("", AsRef::as_ref);
    let mut end = clamp(utf16_character(text, text.chars().count()));
    let line = clamp(line);
    let selection_range = Range {
        start: Position::new(line, 0),
        end: Position::new(line, end),
    };
    let mut end_line = line;
    for child in &children {
        if child.range.end.line > end_line {
            end_line = child.range.end.line;
            end = child.range.end.character;
        }
    }
    Symbol {
        name: name.to_owned(),
        kind,
        range: Range {
            start: Position::new(line, 0),
            end: Position::new(end_line, end),
        },
        selection_range,
        children,
    }
}
