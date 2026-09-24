//! The removed spellings and their replacements, read from `renames.txt`.

use std::sync::OnceLock;

/// A removed spelling and what replaces it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rename {
    /// Whose spelling it is: a class from the signature table (`T` for every
    /// type), `global`, a namespace such as `Time`, `type` for type names, or
    /// `*` for a rule that applies to every name.
    pub receiver: String,
    /// The removed member, function or type name.
    pub name: String,
    /// The call the entry matches, with `$x` for the receiver, `$name` for an
    /// argument and `...` for the other arguments and the block.
    pub pattern: String,
    pub replacement: Replacement,
    /// A further condition on the entry, from its trailing comment.
    pub note: Option<String>,
}

/// What a removed spelling becomes.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Replacement {
    /// A rewrite template using the pattern's placeholders.
    Rewrite(String),
    /// No mechanical rewrite applies; the hint says what to write instead.
    Manual(String),
}

impl Rename {
    /// The canonical member or function the replacement calls with the same
    /// receiver and arguments, when it is a plain rename such as `size` to
    /// `length`: `(namespace, name)`, where the namespace is `None` for a
    /// member of the receiver or a global function.
    pub fn canonical(&self) -> Option<(Option<&str>, &str)> {
        let Replacement::Rewrite(template) = &self.replacement else {
            return None;
        };
        let arguments = &self.pattern[self.pattern.find('(').unwrap_or(self.pattern.len())..];
        let (namespace, rest) = match template.strip_prefix("$x.") {
            Some(rest) => (None, rest),
            None if self.receiver == "global" && !template.contains('.') => {
                (None, template.as_str())
            }
            None => {
                let (namespace, rest) = template.split_once('.')?;
                if !namespace.starts_with(|c: char| c.is_ascii_uppercase()) {
                    return None;
                }
                (Some(namespace), rest)
            }
        };
        let end = rest.find('(').unwrap_or(rest.len());
        let name = &rest[..end];
        let plain = !name.is_empty()
            && name
                .trim_end_matches(['?', '!'])
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_');
        (plain && rest[end..] == *arguments).then_some((namespace, name))
    }
}

const RENAMES: &str = include_str!("renames.txt");

/// Every removed spelling the runtime still accepts, in file order.
///
/// Each entry names the receiver, the removed name, the call pattern it
/// applies to and the replacement, so the migration and the compiler's fixes
/// rewrite the same spellings the signature table leaves out.
///
/// ```
/// let size = vibescript::signatures::renames()
///     .iter()
///     .find(|rename| rename.receiver == "array" && rename.name == "size")
///     .unwrap();
/// assert_eq!(size.canonical(), Some((None, "length")));
/// ```
pub fn renames() -> &'static [Rename] {
    static RENAMES_TABLE: OnceLock<Vec<Rename>> = OnceLock::new();
    RENAMES_TABLE
        .get_or_init(|| parse(RENAMES).unwrap_or_else(|error| panic!("renames.txt: {error}")))
}

pub(super) fn parse(source: &str) -> Result<Vec<Rename>, String> {
    let mut renames = Vec::new();
    for (index, line) in source.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let fail = |message: &str| format!("line {}: {message}", index + 1);
        let (line, note) = match line.split_once("  # ") {
            Some((line, note)) => (line.trim_end(), Some(note.trim().to_owned())),
            None => (line, None),
        };
        let (receiver, rest) = line
            .split_once(": ")
            .ok_or_else(|| fail("expected `receiver: pattern => replacement`"))?;
        let (pattern, replacement) = rest
            .split_once(" => ")
            .ok_or_else(|| fail("expected `=>`"))?;
        let name = removed_name(receiver, pattern).ok_or_else(|| fail("unrecognized pattern"))?;
        let replacement = match replacement.strip_prefix("manual: ") {
            Some(hint) => Replacement::Manual(hint.to_owned()),
            None => Replacement::Rewrite(replacement.to_owned()),
        };
        renames.push(Rename {
            receiver: receiver.to_owned(),
            name,
            pattern: pattern.to_owned(),
            replacement,
            note,
        });
    }
    Ok(renames)
}

/// The removed name a pattern calls: the member after `$x.`, the namespace
/// member after `Name.`, or the global function or type name itself.
fn removed_name(receiver: &str, pattern: &str) -> Option<String> {
    let rest = match pattern.strip_prefix("$x.") {
        Some(rest) => rest,
        None if matches!(receiver, "global" | "type" | "*") => pattern,
        None => pattern.strip_prefix(receiver)?.strip_prefix('.')?,
    };
    let end = rest.find('(').unwrap_or(rest.len());
    let name = &rest[..end];
    (!name.is_empty() && !name.contains(' ')).then(|| name.to_owned())
}
