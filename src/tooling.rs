//! Read-only views of source declarations and member tables for editor tooling.
//!
//! These views never compile or execute code. [`outline`] and [`member_receiver`]
//! parse with the same source-size and syntax-depth guards as
//! [`Engine::compile`](crate::Engine::compile); the catalogs describe the
//! runtime's reserved words and member tables. [`crate::builtins`] lists the
//! global builtins.
#![doc = include_str!("../docs/tooling.md")]

use crate::{Position, Result};

mod walk;

/// A declaration outline of one source, in source order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Outline {
    /// Top-level declarations and statements.
    pub items: Vec<Item>,
}

/// One declaration or statement in an [`Outline`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    pub kind: ItemKind,
    /// The declared name; empty for a plain statement. Setter methods keep
    /// their trailing `=`.
    pub name: String,
    /// Where the declaration or statement starts: its first keyword, modifier,
    /// member name or expression.
    pub position: Position,
    /// The signature and body facts of a function, method or alias. An alias
    /// reports the facts of the declaration it copies.
    pub function: Option<Function>,
    /// The aliased function or method name, for an alias.
    pub target: Option<String>,
    /// Class and module members, nested modules and body statements, or enum
    /// members, in source order.
    pub children: Vec<Item>,
}

/// What an [`Item`] declares.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ItemKind {
    /// A top-level function.
    Function,
    /// A top-level function alias or a class method alias.
    Alias,
    Class,
    Module,
    Enum,
    EnumMember,
    /// An instance method declared with `def` in a class.
    Method,
    /// A `def self.name` method of a class or module.
    ClassMethod,
    /// A `property`, `getter` or `setter` declaration, once per name.
    Property,
    /// A module constant: a plain assignment to a capitalized name in a module body.
    Constant,
    /// Any other top-level, class-body or module-body statement.
    Statement,
}

/// Signature and body facts of a function or method.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Function {
    pub params: Vec<Parameter>,
    /// The declared result type in canonical form, such as `array<int>?`.
    pub return_type: Option<String>,
    /// Local names the body assigns, including loop variables and names bound
    /// inside nested control flow, in first-assignment order. Block bodies and
    /// rescue bindings are not included.
    pub locals: Vec<String>,
    /// Named rescue clauses in the body, outside blocks, in nesting order: a
    /// `begin` lists its own clauses before those nested in its body.
    pub rescues: Vec<Rescue>,
    /// The start of the body's last statement, counting statements nested in
    /// control flow and `begin` clauses but not in blocks or expressions.
    pub last_statement: Option<Position>,
}

/// One parameter of a [`Function`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Parameter {
    /// The bound name, without the `@` of an instance-variable parameter.
    pub name: String,
    pub kind: ParameterKind,
    /// The declared type in canonical form.
    pub type_annotation: Option<String>,
    /// Whether the parameter has a default expression.
    pub default: bool,
    /// Whether the parameter assigns an instance variable (`def initialize(@name)`).
    pub instance: bool,
}

/// How a [`Parameter`] receives its argument.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ParameterKind {
    Positional,
    Keyword,
    /// A `*rest` capture of remaining positional arguments.
    Rest,
    /// A `**rest` capture of remaining keyword arguments.
    KeywordRest,
}

/// A rescue clause that binds its error to a name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rescue {
    pub binding: String,
    /// The position of the `rescue` keyword.
    pub position: Position,
    /// The start of the clause body's last statement, counted as for
    /// [`Function::last_statement`].
    pub last_statement: Option<Position>,
}

/// The reserved words of the language, sorted.
///
/// ```
/// assert!(vibescript::tooling::keywords().contains(&"def"));
/// ```
pub fn keywords() -> &'static [&'static str] {
    &crate::syntax::KEYWORDS
}

/// Reports whether `c` can continue an identifier: `_`, or a letter or
/// decimal digit by the reference's Unicode tables.
///
/// ```
/// use vibescript::tooling::identifier_char;
/// assert!(identifier_char('é') && identifier_char('_') && identifier_char('٣'));
/// assert!(!identifier_char('?') && !identifier_char('Ⅻ'));
/// ```
pub fn identifier_char(c: char) -> bool {
    c == '_' || crate::syntax::unicode::letter_or_digit(c)
}

/// Reports whether `c` is an uppercase letter by the reference's Unicode
/// tables. A capitalized name, such as a module constant's, starts with one.
///
/// ```
/// assert!(vibescript::tooling::uppercase('Ä') && !vibescript::tooling::uppercase('Ⓐ'));
/// ```
pub fn uppercase(c: char) -> bool {
    crate::syntax::unicode::upper(c)
}

/// Builtin member names per receiver kind, in reference order.
///
/// Receivers are named `string`, `symbol`, `array`, `hash`, `int`, `float`,
/// `money`, `duration`, `time`, `range`, `nil`, `bool` and `regex`. Each list
/// ends with the universal helpers, such as `tap` and `respond_to?`, that the
/// kind does not dispatch itself.
///
/// ```
/// let members = vibescript::tooling::member_names();
/// let (_, string) = members.iter().find(|(kind, _)| *kind == "string").unwrap();
/// assert!(string.contains(&"upcase") && string.contains(&"tap"));
/// ```
pub fn member_names() -> Vec<(&'static str, Vec<&'static str>)> {
    use crate::members::names::candidates::*;
    [
        ("string", STRING),
        ("symbol", SYMBOL),
        ("array", ARRAY),
        ("hash", HASH),
        ("int", INT),
        ("float", FLOAT),
        ("money", MONEY),
        ("duration", DURATION),
        ("time", TIME),
        ("range", RANGE),
        ("nil", NIL),
        ("bool", BOOL),
        ("regex", REGEX),
    ]
    .into_iter()
    .map(|(kind, own)| {
        let mut names = own.to_vec();
        for name in UNIVERSAL {
            if !names.contains(name) {
                names.push(name);
            }
        }
        (kind, names)
    })
    .collect()
}

/// Parses source into its declaration outline without compiling it.
///
/// Parse failures return the same error as [`Engine::compile`](crate::Engine::compile).
///
/// ```
/// use vibescript::tooling::{ItemKind, outline};
/// let outline = outline("# Adds one.\ndef inc(n: int) -> int\n  n + 1\nend\n")?;
/// let inc = &outline.items[0];
/// assert_eq!((inc.kind, inc.name.as_str(), inc.position.line), (ItemKind::Function, "inc", 2));
/// let function = inc.function.as_ref().unwrap();
/// assert_eq!(function.params[0].type_annotation.as_deref(), Some("int"));
/// assert_eq!(function.return_type.as_deref(), Some("int"));
/// # Ok::<(), vibescript::Error>(())
/// ```
pub fn outline(source: &str) -> Result<Outline> {
    let (declarations, record) = crate::syntax::record::parse(source, None);
    let declarations =
        declarations.map_err(|error| crate::source::parse_error(source, None, error, &()))?;
    Ok(walk::outline(source, &declarations, &record))
}

/// Reports the receiver kind of the first member access named `name`, when the
/// syntax alone decides it.
///
/// A literal receiver decides its kind, as does a parameter of the enclosing
/// function annotated with one non-nullable builtin type. Kinds are named as by
/// [`member_names`]. Any other receiver reports `None`, as does a source whose
/// syntax fails before the access; an error after it does not matter. Accesses
/// inside string interpolation are not considered.
///
/// ```
/// use vibescript::tooling::member_receiver;
/// assert_eq!(member_receiver("def f(s: string)\n  s.probe\nend", "probe"), Some("string"));
/// assert_eq!(member_receiver("x = [1].probe\ndef broken(", "probe"), Some("array"));
/// assert_eq!(member_receiver("def f(s: string?)\n  s.probe\nend", "probe"), None);
/// ```
pub fn member_receiver(source: &str, name: &str) -> Option<&'static str> {
    let (parsed, record) = crate::syntax::record::parse(source, Some(name));
    // Like the reference, a nesting failure anywhere discards the capture.
    if parsed.is_err_and(|error| error.message == crate::syntax::TOO_DEEP) {
        return None;
    }
    record.probe?.receiver?
}

#[cfg(test)]
mod tests;
