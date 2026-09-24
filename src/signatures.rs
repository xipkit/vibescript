//! Signature declarations: typed, possibly generic signatures written as
//! Vibescript declarations.
//!
//! ADR-007 gives every builtin function, namespace member and member of every
//! value type a typed signature, and ADR-008 prints them as a prelude. Both
//! use this format: `def` for functions and members, `getter` for properties,
//! `module` for namespaces, `class` for the members of every value of a type
//! pattern such as `array<T>`, and `type` for aliases. [`Table::parse`] reads
//! it and [`Table`]'s `Display` prints it back in a canonical form.
//!
//! ```
//! use vibescript::signatures::Table;
//! let source = "class array<T>\n  def map<U>(&block: T -> U) -> array<U>\nend\n";
//! let table = Table::parse(source)?;
//! assert_eq!(table.to_string(), source);
//! # Ok::<(), vibescript::signatures::ParseError>(())
//! ```

mod parse;
mod render;
#[cfg(test)]
mod tests;

pub use parse::ParseError;

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
    /// A property, read without parentheses.
    Getter(Constant),
    /// A namespace constant such as `Math::PI`.
    Constant(Constant),
}

impl Member {
    /// The member's name.
    pub fn name(&self) -> &str {
        match self {
            Self::Function(function) => &function.name,
            Self::Getter(value) | Self::Constant(value) => &value.name,
        }
    }
}

/// A named value of one type: a constant, getter or host global.
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
    /// `name: T:`, passed as `name: value`.
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

    /// The global function named `name`.
    pub fn function(&self, name: &str) -> Option<&Function> {
        self.items.iter().find_map(|item| match item {
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

    /// The member named `name`.
    pub fn member(&self, name: &str) -> Option<&Member> {
        self.members.iter().find(|member| member.name() == name)
    }
}

impl Module {
    /// The member named `name`.
    pub fn member(&self, name: &str) -> Option<&Member> {
        self.members.iter().find(|member| member.name() == name)
    }
}
