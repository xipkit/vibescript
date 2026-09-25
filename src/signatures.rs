//! The builtin signature table and the prelude printed from it.
//!
//! Every builtin function, namespace member and member of every value type has
//! one typed signature under its one canonical name (ADR-007, static types, and
//! ADR-008, a canonical surface). The table is written as Vibescript
//! declarations in `src/signatures/builtins.vibe`, whose header documents the
//! notation. [`table`] parses it, [`prelude`] prints it, and
//! [`crate::Engine::prelude`] extends the text with a host's functions,
//! capabilities and globals. [`renames`] lists the removed spellings and what
//! replaces each one.
//!
//! ```
//! let prelude = vibescript::signatures::prelude();
//! assert!(prelude.contains("  def map<U>(&block: T -> U) -> array<U>\n"));
//! assert_eq!(vibescript::signatures::Table::parse(&prelude)?.to_string(), prelude);
//! # Ok::<(), vibescript::signatures::ParseError>(())
//! ```

use std::sync::OnceLock;

pub(crate) mod host;
mod parse;
mod renames;
mod render;
#[cfg(test)]
mod tests;

pub use parse::ParseError;
pub use renames::{Rename, Replacement, renames};

/// A parsed signature file: the builtin table or a host's additions.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Table {
    /// The leading comment block, one entry per line without its `#`.
    pub header: Vec<String>,
    pub items: Vec<Item>,
}

/// A top-level declaration.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Item {
    /// A global function such as `puts`.
    Function(Function),
    /// A global value such as a host global.
    Constant(Constant),
    /// A namespace such as `Math`.
    Module(Module),
    /// The members of every value whose type matches a receiver pattern.
    Class(Class),
    /// A type name such as `comparable`.
    Alias(Alias),
}

/// A namespace and its members.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Module {
    pub doc: Vec<String>,
    pub name: String,
    pub members: Vec<Member>,
}

/// Members shared by every value matching `receiver`.
///
/// The receiver is a type pattern whose capitalized names are type variables,
/// such as `array<T>`, `array<T: comparable>` or `T` for every type. `vars`
/// lists them in order of appearance with their bounds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Class {
    pub doc: Vec<String>,
    pub receiver: Type,
    pub vars: Vec<TypeParam>,
    pub members: Vec<Member>,
}

/// A declaration inside a module or class body.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Member {
    Function(Function),
    /// A namespace constant such as `Math::PI`.
    Constant(Constant),
}

impl Member {
    /// The member's name.
    pub fn name(&self) -> &str {
        match self {
            Self::Function(function) => &function.name,
            Self::Constant(value) => &value.name,
        }
    }
}

/// A named value of one type: a namespace constant or a host global.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Constant {
    pub doc: Vec<String>,
    pub name: String,
    pub ty: Type,
}

/// A type alias, `type name = T`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Alias {
    pub doc: Vec<String>,
    pub name: String,
    pub ty: Type,
}

/// A function or method signature.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Function {
    pub doc: Vec<String>,
    pub name: String,
    /// Type variables the signature introduces, such as `U` in `map<U>`.
    pub type_params: Vec<TypeParam>,
    pub params: Vec<Param>,
    pub block: Option<Block>,
    /// The result type; `None` means the function returns `nil`.
    pub result: Option<Type>,
}

/// A type variable and the type it must be assignable to, if any.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TypeParam {
    pub name: String,
    pub bound: Option<Type>,
}

/// One parameter of a [`Function`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Param {
    pub name: String,
    pub kind: ParamKind,
    /// The parameter's type; for rest parameters, the collected array or hash.
    pub ty: Type,
    /// Whether a call may omit the argument.
    pub optional: bool,
    /// The literal the builtin uses when the argument is omitted, as written.
    pub default: Option<String>,
}

/// How a [`Param`] receives its argument.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ParamKind {
    Positional,
    /// `*name`, collecting remaining positional arguments.
    Rest,
    /// A parameter after a bare `*` or a rest parameter, passed as
    /// `name: value`.
    Keyword,
    /// `**name`, collecting remaining keyword arguments.
    KeywordRest,
}

/// The block a function yields to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Block {
    pub name: String,
    /// Whether a call may omit the block (`&block?:`).
    pub optional: bool,
    pub params: Vec<Type>,
    /// A trailing `*T`: any number of further arguments of type `T`.
    pub rest: Option<Type>,
    /// The type the block must return; `None` discards the block's value.
    pub result: Option<Type>,
}

/// A type in a signature.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum Type {
    /// A named type and its type arguments, such as `int`, `array<T>` or a
    /// host's `Status`.
    Name(String, Vec<Type>),
    /// A type variable in scope, such as `T`.
    Var(String),
    /// `T?`, which also admits `nil`.
    Optional(Box<Type>),
    /// `A | B`, with at least two arms.
    Union(Vec<Type>),
    /// A hash with string keys: its fields and whether other keys may appear.
    Shape(Vec<Field>, bool),
    /// Exactly one symbol, such as `:ascii`.
    Symbol(String),
    /// `[A, B]`: an array of exactly these elements, checked at compile time.
    Tuple(Vec<Type>),
}

impl Type {
    /// A named type without type arguments.
    pub fn name(name: &str) -> Self {
        Self::Name(name.to_owned(), Vec::new())
    }
}

/// One field of a [`Type::Shape`].
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Field {
    pub name: String,
    pub ty: Type,
    /// Whether the key may be absent.
    pub optional: bool,
}

impl Table {
    /// Parses a signature file.
    pub fn parse(source: &str) -> Result<Self, ParseError> {
        parse::table(source)
    }

    /// The global function's overloads named `name`, in declaration order.
    pub fn functions<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Function> + 'a {
        self.items.iter().filter_map(move |item| match item {
            Item::Function(function) if function.name == name => Some(function),
            _ => None,
        })
    }

    /// The namespace named `name`.
    pub fn module(&self, name: &str) -> Option<&Module> {
        self.items.iter().find_map(|item| match item {
            Item::Module(module) if module.name == name => Some(module),
            _ => None,
        })
    }
}

impl Class {
    /// The name the receiver pattern is built on: `array` for `array<T?>`,
    /// or the variable's name for a lone type variable.
    pub fn base(&self) -> &str {
        match &self.receiver {
            Type::Name(name, _) | Type::Var(name) => name,
            _ => "",
        }
    }

    /// The members named `name`: one, or each overload in declaration order.
    pub fn named<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Member> + 'a {
        self.members
            .iter()
            .filter(move |member| member.name() == name)
    }
}

impl Module {
    /// The members named `name`: one, or each overload in declaration order.
    pub fn named<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Member> + 'a {
        self.members
            .iter()
            .filter(move |member| member.name() == name)
    }
}

const BUILTINS: &str = include_str!("signatures/builtins.vibe");

/// The builtin signature table.
///
/// It lists the builtin globals, the namespaces and the members of every
/// value type, each under its canonical name, in the order `vibes prelude`
/// prints them.
pub fn table() -> &'static Table {
    static TABLE: OnceLock<Table> = OnceLock::new();
    TABLE.get_or_init(|| {
        Table::parse(BUILTINS).unwrap_or_else(|error| panic!("builtins.vibe: {error}"))
    })
}

/// The runtime type an alias of the table, such as `comparable`, names, for
/// annotations that name it like a builtin type.
pub(crate) fn alias_type(name: &str) -> Option<&'static crate::types::Type> {
    static ALIASES: OnceLock<Vec<(String, crate::types::Type)>> = OnceLock::new();
    ALIASES
        .get_or_init(|| {
            table()
                .items
                .iter()
                .filter_map(|item| match item {
                    Item::Alias(alias) => {
                        let ty = crate::syntax::parse_type(&alias.ty.to_string())
                            .unwrap_or_else(|error| panic!("alias {}: {error}", alias.name));
                        Some((alias.name.clone(), ty))
                    }
                    _ => None,
                })
                .collect()
        })
        .iter()
        .find_map(|(alias, ty)| (alias == name).then_some(ty))
}

/// The builtin prelude: [`table`] printed as Vibescript declarations.
///
/// The text is stable across calls and parses back to the same table.
pub fn prelude() -> String {
    table().to_string()
}
