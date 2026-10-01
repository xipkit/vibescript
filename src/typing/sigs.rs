//! Function signatures as the checker reads them: script declarations,
//! builtins from the signature table and host functions share one form.

use super::ty::{Field, Kind, Ty, Types};
use crate::signatures::{self, Class, Function, Item, Member};
use std::{collections::HashMap, rc::Rc, sync::OnceLock};

/// How a parameter receives its argument.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ParamKind {
    Positional,
    Rest,
    Keyword,
    KeywordRest,
}

/// One parameter. A rest parameter's type is the collected array or hash.
#[derive(Clone, Debug)]
pub(crate) struct Param {
    pub name: String,
    pub kind: ParamKind,
    pub ty: Ty,
    pub optional: bool,
}

/// The block a function yields to.
#[derive(Clone, Debug)]
pub(crate) struct BlockSig {
    pub params: Vec<Ty>,
    /// Any number of further arguments of this type.
    pub rest: Option<Ty>,
    /// The type the block returns; `None` discards its value.
    pub result: Option<Ty>,
    pub optional: bool,
}

/// A type variable of a builtin signature.
#[derive(Clone, Debug)]
pub(crate) struct Var {
    pub name: String,
    /// The type it must be assignable to, as a single type, not a union.
    pub bound: Option<Ty>,
}

/// A function signature.
#[derive(Clone, Debug)]
pub(crate) struct Sig {
    pub name: String,
    pub params: Vec<Param>,
    /// The result; `None` means the function returns `nil`.
    pub result: Option<Ty>,
    pub block: Option<BlockSig>,
    /// Type variables, class variables first; `Kind::Var(i)` is `vars[i]`.
    pub vars: Vec<Var>,
    /// Where a `break` out of the call's block goes.
    pub breaks: Breaks,
    /// Whether the callee checks its arguments and its block's results
    /// against their declared types at runtime, which makes a symbol
    /// literal an enum member: a script function or method. A builtin or
    /// host function receives a symbol as a symbol.
    pub converts: bool,
    /// The script function or method of this source, when it is one.
    pub id: Option<super::program::FnId>,
}

/// Where a `break` out of the block a call passes goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Breaks {
    /// It ends the call, and its value is the call's, as for a builtin.
    Call,
    /// It returns from the function through the function's declared
    /// result, which the runtime checks: a script function that yields
    /// only outside loops and blocks, or a host method whose signature
    /// declares its result.
    Result,
    /// It ends the loop or the call with a block around the function's
    /// `yield`, so the call's value is the function's result. The function
    /// sees the break's value there, which has the function's result type,
    /// as a value returned through its result would.
    Inside,
    /// The function never yields, so nothing breaks.
    Never,
}

impl Sig {
    /// A host method's signature: a `break` out of its block becomes its
    /// result, which the runtime validates against the declared result, so
    /// the break's value must have that type.
    pub fn host(mut self) -> Self {
        if self.block.is_some() && self.result.is_some_and(|result| result != Ty::ANY) {
            self.breaks = Breaks::Result;
        }
        self
    }

    /// The positional arguments the signature accepts: at least, and at most
    /// unless it has a rest parameter.
    pub fn positional(&self) -> (usize, Option<usize>) {
        let mut min = 0;
        let mut max = Some(0);
        for param in &self.params {
            match param.kind {
                ParamKind::Positional => {
                    max = max.map(|m| m + 1);
                    if !param.optional {
                        min += 1;
                    }
                }
                ParamKind::Rest => max = None,
                _ => (),
            }
        }
        (min, max)
    }

    pub fn keyword(&self, name: &str) -> Option<&Param> {
        self.params
            .iter()
            .find(|p| p.kind == ParamKind::Keyword && p.name == name)
    }

    pub fn keyword_rest(&self) -> Option<&Param> {
        self.params
            .iter()
            .find(|p| p.kind == ParamKind::KeywordRest)
    }

    pub fn rest(&self) -> Option<&Param> {
        self.params.iter().find(|p| p.kind == ParamKind::Rest)
    }

    /// The index of the first keyword parameter, which a declaration marks
    /// with a bare `*` unless a rest parameter precedes it.
    pub fn keyword_star(&self) -> Option<usize> {
        let first = self
            .params
            .iter()
            .position(|p| p.kind == ParamKind::Keyword)?;
        let rest = self.params[..first]
            .iter()
            .any(|p| p.kind == ParamKind::Rest);
        (!rest).then_some(first)
    }

    /// Renders the parameter list for messages into `out`.
    pub fn describe(&self, types: &Types, out: &mut super::counted::Text<'_>) {
        out.push_str(&self.name);
        out.push('(');
        let star = self.keyword_star();
        for (index, param) in self.params.iter().enumerate() {
            if index > 0 {
                out.push_str(", ");
            }
            if star == Some(index) {
                out.push_str("*, ");
            }
            match param.kind {
                ParamKind::Rest => out.push('*'),
                ParamKind::KeywordRest => out.push_str("**"),
                _ => (),
            }
            out.push_str(&param.name);
            if param.optional && param.kind == ParamKind::Positional {
                out.push('?');
            }
            out.push_str(": ");
            out.push_str(&types.display(param.ty));
        }
        if let Some(block) = &self.block {
            if !self.params.is_empty() {
                out.push_str(", ");
            }
            out.push_str(if block.optional { "&block?" } else { "&block" });
        }
        out.push(')');
    }
}

/// The signature table indexed for lookup.
pub(crate) struct Index {
    pub globals: HashMap<&'static str, Vec<&'static Function>>,
    /// Namespaces in table order; a `Kind::Builtin` names one by position.
    pub modules: Vec<(&'static str, &'static signatures::Module)>,
    /// Classes by the base their receiver pattern is built on: a type name,
    /// or `T` for every type.
    pub classes: HashMap<&'static str, Vec<&'static Class>>,
    pub aliases: HashMap<&'static str, &'static signatures::Type>,
    /// Removed spellings by receiver and name.
    pub renames: HashMap<(&'static str, &'static str), &'static signatures::Rename>,
}

pub(crate) fn index() -> &'static Index {
    static INDEX: OnceLock<Index> = OnceLock::new();
    INDEX.get_or_init(|| {
        let table = signatures::table();
        let mut index = Index {
            globals: HashMap::new(),
            modules: Vec::new(),
            classes: HashMap::new(),
            aliases: HashMap::new(),
            renames: HashMap::new(),
        };
        for item in &table.items {
            match item {
                Item::Function(function) => index
                    .globals
                    .entry(function.name.as_str())
                    .or_default()
                    .push(function),
                Item::Module(module) => index.modules.push((module.name.as_str(), module)),
                Item::Class(class) => index.classes.entry(class.base()).or_default().push(class),
                Item::Alias(alias) => {
                    index.aliases.insert(alias.name.as_str(), &alias.ty);
                }
                _ => (),
            }
        }
        for rename in signatures::renames() {
            index
                .renames
                .entry((rename.receiver.as_str(), rename.name.as_str()))
                .or_insert(rename);
        }
        index
    })
}

impl Index {
    pub fn module(&self, name: &str) -> Option<u32> {
        self.modules
            .iter()
            .position(|(module, _)| *module == name)
            .map(|index| index as u32)
    }
}

/// Converts table signatures into checker signatures, once per function.
#[derive(Default)]
pub(crate) struct Converter {
    cache: HashMap<(usize, usize), Rc<Sig>>,
    /// What the cached signatures hold.
    cached: usize,
}

impl Converter {
    /// What the cache of converted signatures holds.
    pub fn grown(&self) -> usize {
        super::meter::map(&self.cache) + self.cached
    }

    /// The signature of `function`, declared in `class` when it is a member
    /// of a value type, whose receiver pattern's variables come first.
    pub fn convert(
        &mut self,
        types: &mut Types,
        function: &'static Function,
        class: Option<&'static Class>,
    ) -> Rc<Sig> {
        let key = (
            std::ptr::from_ref(function) as usize,
            class.map_or(0, |c| std::ptr::from_ref(c) as usize),
        );
        if let Some(sig) = self.cache.get(&key) {
            return sig.clone();
        }
        let sig = Rc::new(self.convert_owned(types, function, class));
        self.cached += super::meter::Heap::heap(&sig);
        self.cache.insert(key, sig.clone());
        sig
    }

    /// Converts a signature without caching it, such as a host function's.
    pub fn convert_owned(
        &mut self,
        types: &mut Types,
        function: &Function,
        class: Option<&Class>,
    ) -> Sig {
        let mut vars: Vec<Var> = Vec::new();
        let mut names: Vec<String> = Vec::new();
        if let Some(class) = class {
            for param in &class.vars {
                names.push(param.name.clone());
                vars.push(Var {
                    name: param.name.clone(),
                    bound: None,
                });
            }
        }
        for param in &function.type_params {
            names.push(param.name.clone());
            vars.push(Var {
                name: param.name.clone(),
                bound: None,
            });
        }
        let bounds: Vec<(usize, Option<&signatures::Type>)> = class
            .map(|class| {
                class
                    .vars
                    .iter()
                    .map(|v| v.bound.as_ref())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
            .into_iter()
            .chain(function.type_params.iter().map(|v| v.bound.as_ref()))
            .enumerate()
            .collect();
        for (index, bound) in bounds {
            vars[index].bound = bound.map(|bound| table_type(types, bound, &names));
        }
        let params = function
            .params
            .iter()
            .map(|param| Param {
                name: param.name.clone(),
                kind: match param.kind {
                    signatures::ParamKind::Positional => ParamKind::Positional,
                    signatures::ParamKind::Rest => ParamKind::Rest,
                    signatures::ParamKind::Keyword => ParamKind::Keyword,
                    _ => ParamKind::KeywordRest,
                },
                ty: table_type(types, &param.ty, &names),
                optional: param.optional,
            })
            .collect();
        let block = function.block.as_ref().map(|block| BlockSig {
            params: block
                .params
                .iter()
                .map(|ty| table_type(types, ty, &names))
                .collect(),
            rest: block.rest.as_ref().map(|ty| table_type(types, ty, &names)),
            result: block
                .result
                .as_ref()
                .map(|ty| table_type(types, ty, &names)),
            optional: block.optional,
        });
        Sig {
            name: function.name.clone(),
            params,
            result: function
                .result
                .as_ref()
                .map(|ty| table_type(types, ty, &names)),
            block,
            vars,
            breaks: Breaks::Call,
            converts: false,
            id: None,
        }
    }
}

/// A class's receiver pattern, with its variables numbered as
/// [`Converter::convert`] numbers them.
pub(crate) fn receiver_pattern(types: &mut Types, class: &Class) -> Ty {
    let names: Vec<String> = class.vars.iter().map(|v| v.name.clone()).collect();
    table_type(types, &class.receiver, &names)
}

/// Converts a signature-table type. `vars` names the type variables in scope.
pub(crate) fn table_type(types: &mut Types, ty: &signatures::Type, vars: &[String]) -> Ty {
    use signatures::Type as T;
    match ty {
        T::Var(name) => match vars.iter().position(|v| v == name) {
            Some(index) => types.intern(Kind::Var(index as u32)),
            None => Ty::ANY,
        },
        T::Optional(inner) => {
            let inner = table_type(types, inner, vars);
            types.optional(inner)
        }
        T::Union(arms) => {
            let arms: Vec<Ty> = arms
                .iter()
                .map(|arm| table_type(types, arm, vars))
                .collect();
            types.union(&arms)
        }
        T::Shape(fields, open) => {
            let fields = fields
                .iter()
                .map(|field| Field {
                    name: field.name.as_str().into(),
                    ty: table_type(types, &field.ty, vars),
                    optional: field.optional,
                })
                .collect();
            types.shape(fields, *open)
        }
        T::Symbol(name) => types.intern(Kind::SymbolLit(name.as_str().into())),
        T::Tuple(items) => {
            let items = items
                .iter()
                .map(|item| table_type(types, item, vars))
                .collect();
            types.tuple(items)
        }
        T::Name(name, args) => {
            let arg = |types: &mut Types, index: usize| {
                args.get(index)
                    .map_or(Ty::ANY, |arg| table_type(types, arg, vars))
            };
            match name.as_str() {
                "array" => {
                    let element = arg(types, 0);
                    types.array(element)
                }
                "hash" => {
                    let value = if args.len() >= 2 {
                        arg(types, 1)
                    } else {
                        Ty::ANY
                    };
                    types.hash(value)
                }
                "type" => {
                    let described = arg(types, 0);
                    types.type_lit(described)
                }
                other => match scalar(other) {
                    Some(ty) => ty,
                    None => match index().aliases.get(other) {
                        Some(alias) => table_type(types, alias, vars),
                        None => Ty::ANY,
                    },
                },
            }
        }
    }
}

/// A builtin scalar type name.
pub(crate) fn scalar(name: &str) -> Option<Ty> {
    Some(match name {
        "any" => Ty::ANY,
        "nil" => Ty::NIL,
        "bool" => Ty::BOOL,
        "int" => Ty::INT,
        "float" => Ty::FLOAT,
        "number" => Ty::NUMBER,
        "string" => Ty::STRING,
        "symbol" => Ty::SYMBOL,
        "duration" => Ty::DURATION,
        "time" => Ty::TIME,
        "money" => Ty::MONEY,
        "range" => Ty::RANGE,
        "regex" => Ty::REGEX,
        "match_data" => Ty::MATCH_DATA,
        "error" => Ty::ERROR_VALUE,
        "enum_type" => Ty::ANY_ENUM_TYPE,
        "enum_value" => Ty::ANY_ENUM,
        _ => return None,
    })
}

/// The table class bases a receiver type may match, most specific first;
/// every type also matches `T`.
pub(crate) fn bases(types: &Types, ty: Ty) -> &'static [&'static str] {
    match types.kind(ty) {
        Kind::Array(_) | Kind::Tuple(_) => &["array", "T"],
        Kind::Hash(_) | Kind::Shape(..) | Kind::EmptyHash => &["hash", "T"],
        Kind::Int => &["int", "T"],
        Kind::Float => &["float", "T"],
        Kind::String => &["string", "T"],
        Kind::Symbol | Kind::SymbolLit(_) => &["symbol", "T"],
        Kind::Nil => &["nil", "T"],
        Kind::Bool => &["bool", "T"],
        Kind::Range => &["range", "T"],
        Kind::Money => &["money", "T"],
        Kind::Duration => &["duration", "T"],
        Kind::Time => &["time", "T"],
        Kind::Regex => &["regex", "T"],
        Kind::MatchData => &["match_data", "T"],
        Kind::ErrorValue => &["error", "T"],
        Kind::EnumValue(_) | Kind::AnyEnum => &["enum_value", "T"],
        Kind::EnumType(_) | Kind::AnyEnumType => &["enum_type", "T"],
        _ => &["T"],
    }
}

/// The members named `name` of the table classes whose receiver pattern
/// `receiver` matches, with the bindings of the pattern's variables. The
/// first matching class that declares the name wins, so a specific class
/// shadows the members of every type.
pub(crate) fn members(
    types: &mut Types,
    receiver: Ty,
    name: &str,
) -> Vec<(&'static Function, &'static Class, Vec<Option<Ty>>)> {
    let index = index();
    for base in bases(types, receiver) {
        let Some(classes) = index.classes.get(base) else {
            continue;
        };
        let mut found = Vec::new();
        for &class in classes {
            let functions: Vec<&'static Function> = class
                .members
                .iter()
                .filter(|member| member.name() == name)
                .filter_map(|member| match member {
                    Member::Function(function) => Some(function),
                    _ => None,
                })
                .collect();
            if functions.is_empty() {
                continue;
            }
            let pattern = receiver_pattern(types, class);
            let mut bindings = vec![None; class.vars.len()];
            if !bind_receiver(types, pattern, receiver, &mut bindings) {
                continue;
            }
            for function in functions {
                found.push((function, class, bindings.clone()));
            }
        }
        if !found.is_empty() {
            return found;
        }
    }
    Vec::new()
}

/// Whether any table class that `receiver` could match declares `name`,
/// whatever its bounds.
pub(crate) fn declares(types: &Types, receiver: Ty, name: &str) -> bool {
    let index = index();
    bases(types, receiver).iter().any(|base| {
        index.classes.get(base).is_some_and(|classes| {
            classes
                .iter()
                .any(|class| class.named(name).next().is_some())
        })
    })
}

/// Matches a receiver pattern such as `array<T?>` against a receiver type,
/// binding the pattern's variables.
fn bind_receiver(types: &mut Types, pattern: Ty, actual: Ty, bindings: &mut [Option<Ty>]) -> bool {
    match (&*types.shared(pattern), &*types.shared(actual)) {
        (Kind::Var(index), _) => {
            // An empty literal's elements are unknown, not impossible: the
            // arguments and the block may bind the variable instead.
            if actual == Ty::NEVER {
                return true;
            }
            let slot = &mut bindings[*index as usize];
            match slot {
                Some(bound) => *bound == actual,
                None => {
                    *slot = Some(actual);
                    true
                }
            }
        }
        (Kind::Array(p), Kind::Array(a)) => bind_receiver(types, *p, *a, bindings),
        (Kind::Array(p), Kind::Tuple(items)) => {
            let element = types.union(items);
            bind_receiver(types, *p, element, bindings)
        }
        (Kind::Hash(p), _) => match types.hash_value(actual) {
            Some(value) => bind_receiver(types, *p, value, bindings),
            None => false,
        },
        (Kind::Tuple(p), Kind::Tuple(a)) => {
            p.len() == a.len()
                && p.iter()
                    .zip(a.iter())
                    .all(|(&p, &a)| bind_receiver(types, p, a, bindings))
        }
        (Kind::Union(arms), _) => {
            // `T?`: bind T to the receiver without nil.
            let vars: Vec<Ty> = arms.iter().copied().filter(|&a| a != Ty::NIL).collect();
            if vars.len() == 1 && arms.contains(&Ty::NIL) {
                let rest = types.without_nil(actual);
                return bind_receiver(types, vars[0], rest, bindings);
            }
            pattern == actual
        }
        _ => pattern == actual || types.assignable(actual, pattern),
    }
}
