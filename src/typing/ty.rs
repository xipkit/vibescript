//! Compile-time types: interned, so a type is a small copyable id and
//! comparing two types compares ids.

use std::collections::HashMap;

/// An interned type. Equal types have equal ids.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct Ty(u32);

impl Ty {
    /// A type that an earlier error left unknown; it is compatible with
    /// everything, so one mistake reports once.
    pub const ERROR: Ty = Ty(0);
    /// The type of an expression that never produces a value, such as `raise`.
    pub const NEVER: Ty = Ty(1);
    pub const ANY: Ty = Ty(2);
    pub const NIL: Ty = Ty(3);
    pub const BOOL: Ty = Ty(4);
    pub const INT: Ty = Ty(5);
    pub const FLOAT: Ty = Ty(6);
    pub const STRING: Ty = Ty(7);
    pub const SYMBOL: Ty = Ty(8);
    pub const DURATION: Ty = Ty(9);
    pub const TIME: Ty = Ty(10);
    pub const MONEY: Ty = Ty(11);
    pub const RANGE: Ty = Ty(12);
    pub const REGEX: Ty = Ty(13);
    pub const MATCH_DATA: Ty = Ty(14);
    /// What `rescue => error` binds.
    pub const ERROR_VALUE: Ty = Ty(15);
    /// `{}` without a declared type.
    pub const EMPTY_HASH: Ty = Ty(16);
    /// A member of any enum, `enum_value` in the signature table.
    pub const ANY_ENUM: Ty = Ty(17);
    /// Any enum used as a value, `enum_type` in the signature table.
    pub const ANY_ENUM_TYPE: Ty = Ty(18);
    /// `int | float`.
    pub const NUMBER: Ty = Ty(19);
}

/// What a [`Ty`] is.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Kind {
    Error,
    Never,
    Any,
    Nil,
    Bool,
    Int,
    Float,
    String,
    Symbol,
    Duration,
    Time,
    Money,
    Range,
    Regex,
    MatchData,
    ErrorValue,
    EmptyHash,
    AnyEnum,
    AnyEnumType,
    Array(Ty),
    /// `hash<string, V>`, by its value type.
    Hash(Ty),
    /// Fields sorted by name, and whether other keys may appear.
    Shape(Box<[Field]>, bool),
    Tuple(Box<[Ty]>),
    /// At least two alternatives, flattened, sorted by id and distinct.
    Union(Box<[Ty]>),
    /// An instance of the script class with this id.
    Instance(u32),
    /// A member of the script enum with this id.
    EnumValue(u32),
    /// The script enum with this id, used as a value.
    EnumType(u32),
    /// A script class or module used as a value, such as `Invoice` in
    /// `Invoice.new`.
    Namespace(u32),
    /// A builtin namespace such as `Math`, by its index in the signature table.
    Builtin(u32),
    /// `type<T>`, a type literal.
    TypeLit(Ty),
    /// A signature's type variable, by its index in the signature.
    Var(u32),
    /// Exactly one symbol, as signatures spell `:ascii`.
    SymbolLit(Box<str>),
    /// The object `require` returns, by the index of the required module.
    Exports(u32),
    /// A capability the host declares with members, by its index among
    /// them, used as a value such as `SMS` in `SMS.send`.
    Host(u32),
}

/// A field of a shape.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Field {
    pub name: Box<str>,
    pub ty: Ty,
    pub optional: bool,
}

/// The names of script classes and enums, for rendering types.
#[derive(Default)]
pub(crate) struct Names {
    pub namespaces: Vec<String>,
    pub enums: Vec<String>,
    pub builtins: Vec<String>,
    /// Declared host capabilities, by the index a `Kind::Host` holds.
    pub hosts: Vec<String>,
}

/// The type interner of one check.
pub(crate) struct Types {
    kinds: Vec<Kind>,
    ids: HashMap<Kind, Ty>,
    assignable: HashMap<(Ty, Ty), bool>,
    pub names: Names,
    /// Work done, for [`super::Checked::steps`].
    pub steps: u64,
}

impl Types {
    pub fn new() -> Self {
        let mut types = Self {
            kinds: Vec::new(),
            ids: HashMap::new(),
            assignable: HashMap::new(),
            names: Names::default(),
            steps: 0,
        };
        for kind in [
            Kind::Error,
            Kind::Never,
            Kind::Any,
            Kind::Nil,
            Kind::Bool,
            Kind::Int,
            Kind::Float,
            Kind::String,
            Kind::Symbol,
            Kind::Duration,
            Kind::Time,
            Kind::Money,
            Kind::Range,
            Kind::Regex,
            Kind::MatchData,
            Kind::ErrorValue,
            Kind::EmptyHash,
            Kind::AnyEnum,
            Kind::AnyEnumType,
        ] {
            types.intern(kind);
        }
        let number = types.intern(Kind::Union(Box::new([Ty::INT, Ty::FLOAT])));
        debug_assert_eq!(number, Ty::NUMBER);
        types
    }

    pub fn intern(&mut self, kind: Kind) -> Ty {
        if let Some(&ty) = self.ids.get(&kind) {
            return ty;
        }
        let ty = Ty(self.kinds.len() as u32);
        self.kinds.push(kind.clone());
        self.ids.insert(kind, ty);
        ty
    }

    pub fn kind(&self, ty: Ty) -> &Kind {
        &self.kinds[ty.0 as usize]
    }

    pub fn array(&mut self, element: Ty) -> Ty {
        self.intern(Kind::Array(element))
    }

    pub fn hash(&mut self, value: Ty) -> Ty {
        self.intern(Kind::Hash(value))
    }

    pub fn tuple(&mut self, elements: Vec<Ty>) -> Ty {
        self.intern(Kind::Tuple(elements.into()))
    }

    pub fn type_lit(&mut self, ty: Ty) -> Ty {
        self.intern(Kind::TypeLit(ty))
    }

    /// A shape from fields in any order; a later field of the same name wins.
    pub fn shape(&mut self, mut fields: Vec<Field>, open: bool) -> Ty {
        fields.reverse();
        fields.sort_by(|a, b| a.name.cmp(&b.name));
        fields.dedup_by(|a, b| a.name == b.name);
        self.intern(Kind::Shape(fields.into(), open))
    }

    /// `ty?`.
    pub fn optional(&mut self, ty: Ty) -> Ty {
        self.union(&[ty, Ty::NIL])
    }

    /// The union of `types`: nested unions flatten, `never` drops out, and
    /// `any` or an unknown type absorbs the rest.
    pub fn union(&mut self, types: &[Ty]) -> Ty {
        self.steps += types.len() as u64;
        let mut members = Vec::with_capacity(types.len());
        for &ty in types {
            match self.kind(ty) {
                Kind::Error => return Ty::ERROR,
                Kind::Never => (),
                Kind::Union(inner) => members.extend_from_slice(inner),
                _ => members.push(ty),
            }
        }
        if members.contains(&Ty::ANY) {
            return Ty::ANY;
        }
        members.sort_unstable();
        members.dedup();
        match members.len() {
            0 => Ty::NEVER,
            1 => members[0],
            _ => self.intern(Kind::Union(members.into())),
        }
    }

    /// The alternatives of a union, or the type itself.
    pub fn members(&self, ty: Ty) -> Vec<Ty> {
        match self.kind(ty) {
            Kind::Union(members) => members.to_vec(),
            _ => vec![ty],
        }
    }

    pub fn has_nil(&self, ty: Ty) -> bool {
        ty == Ty::NIL || matches!(self.kind(ty), Kind::Union(members) if members.contains(&Ty::NIL))
    }

    /// The type without `nil`.
    pub fn without_nil(&mut self, ty: Ty) -> Ty {
        self.without(ty, Ty::NIL)
    }

    /// The type without the alternatives assignable to `removed`.
    pub fn without(&mut self, ty: Ty, removed: Ty) -> Ty {
        if ty == Ty::ERROR || ty == Ty::ANY {
            return ty;
        }
        let members = self.members(ty);
        let kept: Vec<Ty> = members
            .into_iter()
            .filter(|&member| member == Ty::ANY || !self.assignable(member, removed))
            .collect();
        self.union(&kept)
    }

    /// Whether a value of type `from` may be stored where `to` is expected.
    ///
    /// Collections are values, so they are covariant. A shape is exact
    /// unless it is open; a shape whose fields all fit `V` is a
    /// `hash<string, V>`; an array literal's tuple fits an array of its
    /// elements. Otherwise the only relations are unions, `nil`, `any` and
    /// `never`.
    pub fn assignable(&mut self, from: Ty, to: Ty) -> bool {
        if from == to || from == Ty::ERROR || to == Ty::ERROR || from == Ty::NEVER || to == Ty::ANY
        {
            return true;
        }
        if let Some(&known) = self.assignable.get(&(from, to)) {
            return known;
        }
        let result = self.assignable_uncached(from, to);
        self.assignable.insert((from, to), result);
        result
    }

    fn assignable_uncached(&mut self, from: Ty, to: Ty) -> bool {
        self.steps += 1;
        let from_kind = self.kind(from).clone();
        let to_kind = self.kind(to).clone();
        if let Kind::Union(members) = &from_kind {
            return members.iter().all(|&member| self.assignable(member, to));
        }
        if let Kind::Union(members) = &to_kind {
            if members.iter().any(|&member| self.assignable(from, member)) {
                return true;
            }
            // A collection of a union may be split across alternatives
            // only element-wise, which the alternatives above cover.
            return false;
        }
        match (&from_kind, &to_kind) {
            (Kind::Array(a), Kind::Array(b)) => self.assignable(*a, *b),
            (Kind::Tuple(items), Kind::Array(element)) => {
                items.iter().all(|&item| self.assignable(item, *element))
            }
            (Kind::Tuple(a), Kind::Tuple(b)) => {
                a.len() == b.len() && a.iter().zip(b.iter()).all(|(&x, &y)| self.assignable(x, y))
            }
            (Kind::Hash(a), Kind::Hash(b)) => self.assignable(*a, *b),
            (Kind::EmptyHash, Kind::Hash(_)) => true,
            (Kind::EmptyHash, Kind::Shape(fields, _)) => fields.iter().all(|field| field.optional),
            (Kind::Shape(fields, open), Kind::Hash(value)) => {
                (!open || *value == Ty::ANY)
                    && fields.iter().all(|field| self.assignable(field.ty, *value))
            }
            (Kind::Shape(from_fields, from_open), Kind::Shape(to_fields, to_open)) => {
                if *from_open && !to_open {
                    return false;
                }
                for field in to_fields.iter() {
                    match from_fields.iter().find(|f| f.name == field.name) {
                        Some(found) => {
                            if found.optional && !field.optional {
                                return false;
                            }
                            if !self.assignable(found.ty, field.ty) {
                                return false;
                            }
                        }
                        None if field.optional || *from_open => (),
                        None => return false,
                    }
                }
                *to_open
                    || from_fields
                        .iter()
                        .all(|f| to_fields.iter().any(|t| t.name == f.name))
            }
            (Kind::Hash(value), Kind::Shape(fields, true)) => {
                *value == Ty::ANY && fields.iter().all(|field| field.optional)
            }
            (Kind::EnumValue(_), Kind::AnyEnum) => true,
            (Kind::EnumType(_), Kind::AnyEnumType) => true,
            (Kind::TypeLit(a), Kind::TypeLit(b)) => *b == Ty::ANY || *a == *b,
            (Kind::SymbolLit(_), Kind::Symbol) => true,
            _ => false,
        }
    }

    /// Whether the type mentions a signature's type variable.
    pub fn has_var(&self, ty: Ty) -> bool {
        match self.kind(ty) {
            Kind::Var(_) => true,
            Kind::Array(t) | Kind::Hash(t) | Kind::TypeLit(t) => self.has_var(*t),
            Kind::Shape(fields, _) => fields.iter().any(|f| self.has_var(f.ty)),
            Kind::Tuple(items) | Kind::Union(items) => items.iter().any(|&t| self.has_var(t)),
            _ => false,
        }
    }

    /// Replaces bound type variables; unbound ones stay.
    pub fn subst(&mut self, ty: Ty, bindings: &[Option<Ty>]) -> Ty {
        if !self.has_var(ty) {
            return ty;
        }
        match self.kind(ty).clone() {
            Kind::Var(index) => bindings
                .get(index as usize)
                .copied()
                .flatten()
                .unwrap_or(ty),
            Kind::Array(t) => {
                let t = self.subst(t, bindings);
                self.array(t)
            }
            Kind::Hash(t) => {
                let t = self.subst(t, bindings);
                self.hash(t)
            }
            Kind::TypeLit(t) => {
                let t = self.subst(t, bindings);
                self.type_lit(t)
            }
            Kind::Shape(fields, open) => {
                let fields = fields
                    .iter()
                    .map(|field| Field {
                        name: field.name.clone(),
                        ty: self.subst(field.ty, bindings),
                        optional: field.optional,
                    })
                    .collect();
                self.shape(fields, open)
            }
            Kind::Tuple(items) => {
                let items = items.iter().map(|&t| self.subst(t, bindings)).collect();
                self.tuple(items)
            }
            Kind::Union(items) => {
                let items: Vec<Ty> = items.iter().map(|&t| self.subst(t, bindings)).collect();
                self.union(&items)
            }
            _ => ty,
        }
    }

    /// Replaces every type variable, bound or not: unbound ones become
    /// unknown, so they never cause a second error.
    pub fn close(&mut self, ty: Ty, bindings: &[Option<Ty>]) -> Ty {
        let ty = self.subst(ty, bindings);
        if !self.has_var(ty) {
            return ty;
        }
        let unknown: Vec<Option<Ty>> = (0..64).map(|_| Some(Ty::ERROR)).collect();
        self.subst(ty, &unknown)
    }

    /// The element type an array, tuple or range yields when iterated.
    pub fn element(&mut self, ty: Ty) -> Option<Ty> {
        match self.kind(ty).clone() {
            Kind::Array(element) => Some(element),
            Kind::Tuple(items) => Some(self.union(&items)),
            Kind::Range => Some(Ty::INT),
            Kind::Error | Kind::Any => Some(ty),
            _ => None,
        }
    }

    /// The value type of a hash or shape, as `hash<string, V>` would read it.
    pub fn hash_value(&mut self, ty: Ty) -> Option<Ty> {
        match self.kind(ty).clone() {
            Kind::Hash(value) => Some(value),
            Kind::EmptyHash => Some(Ty::NEVER),
            Kind::Shape(fields, open) => {
                if open {
                    return Some(Ty::ANY);
                }
                let types: Vec<Ty> = fields.iter().map(|f| f.ty).collect();
                Some(self.union(&types))
            }
            _ => None,
        }
    }

    /// The type as an annotation writes it.
    /// `ty` as an annotation writes it, or `any` when no annotation can
    /// name it, such as for a class used as a value or a required file.
    pub fn annotation(&self, ty: Ty) -> String {
        if self.nameable(ty) {
            self.display(ty)
        } else {
            "any".to_owned()
        }
    }

    fn nameable(&self, ty: Ty) -> bool {
        match self.kind(ty) {
            Kind::Any
            | Kind::Nil
            | Kind::Bool
            | Kind::Int
            | Kind::Float
            | Kind::String
            | Kind::Symbol
            | Kind::Duration
            | Kind::Time
            | Kind::Money
            | Kind::Range
            | Kind::Regex
            | Kind::MatchData
            | Kind::ErrorValue
            | Kind::AnyEnum
            | Kind::AnyEnumType
            | Kind::Instance(_)
            | Kind::EnumValue(_) => true,
            Kind::Array(inner) | Kind::Hash(inner) => self.nameable(*inner),
            Kind::Shape(fields, _) => fields.iter().all(|field| self.nameable(field.ty)),
            Kind::Tuple(items) | Kind::Union(items) => {
                items.iter().all(|&item| self.nameable(item))
            }
            _ => false,
        }
    }

    pub fn display(&self, ty: Ty) -> String {
        let mut out = String::new();
        self.write(ty, &mut out);
        out
    }

    fn write(&self, ty: Ty, out: &mut String) {
        match self.kind(ty) {
            Kind::Error => out.push_str("unknown"),
            Kind::Never => out.push_str("never"),
            Kind::Any => out.push_str("any"),
            Kind::Nil => out.push_str("nil"),
            Kind::Bool => out.push_str("bool"),
            Kind::Int => out.push_str("int"),
            Kind::Float => out.push_str("float"),
            Kind::String => out.push_str("string"),
            Kind::Symbol => out.push_str("symbol"),
            Kind::Duration => out.push_str("duration"),
            Kind::Time => out.push_str("time"),
            Kind::Money => out.push_str("money"),
            Kind::Range => out.push_str("range"),
            Kind::Regex => out.push_str("regex"),
            Kind::MatchData => out.push_str("match_data"),
            Kind::ErrorValue => out.push_str("error"),
            Kind::EmptyHash => out.push_str("{}"),
            Kind::AnyEnum => out.push_str("enum_value"),
            Kind::AnyEnumType => out.push_str("enum_type"),
            Kind::Array(element) => {
                out.push_str("array<");
                self.write(*element, out);
                out.push('>');
            }
            Kind::Hash(value) => {
                out.push_str("hash<string, ");
                self.write(*value, out);
                out.push('>');
            }
            Kind::Shape(fields, open) => {
                if fields.is_empty() && !open {
                    out.push_str("{}");
                    return;
                }
                out.push_str("{ ");
                for (index, field) in fields.iter().enumerate() {
                    if index > 0 {
                        out.push_str(", ");
                    }
                    write_field_name(&field.name, out);
                    if field.optional {
                        out.push('?');
                    }
                    out.push_str(": ");
                    self.write(field.ty, out);
                }
                if *open {
                    if !fields.is_empty() {
                        out.push_str(", ");
                    }
                    out.push_str("...");
                }
                out.push_str(" }");
            }
            Kind::Tuple(items) => {
                out.push('[');
                for (index, &item) in items.iter().enumerate() {
                    if index > 0 {
                        out.push_str(", ");
                    }
                    self.write(item, out);
                }
                out.push(']');
            }
            Kind::Union(members) => {
                let nil = members.contains(&Ty::NIL);
                let others: Vec<Ty> = members.iter().copied().filter(|&m| m != Ty::NIL).collect();
                let number = others.contains(&Ty::INT) && others.contains(&Ty::FLOAT);
                let mut parts: Vec<String> = others
                    .iter()
                    .filter(|&&m| !number || (m != Ty::INT && m != Ty::FLOAT))
                    .map(|&m| {
                        let mut text = String::new();
                        self.write(m, &mut text);
                        text
                    })
                    .collect();
                if number {
                    parts.push("number".to_owned());
                }
                parts.sort();
                if nil && parts.len() == 1 {
                    let single = others.len() == 1 || number;
                    if single {
                        out.push_str(&parts[0]);
                        out.push('?');
                        return;
                    }
                }
                if nil {
                    parts.push("nil".to_owned());
                }
                out.push_str(&parts.join(" | "));
            }
            Kind::Instance(id) | Kind::Namespace(id) => out.push_str(
                self.names
                    .namespaces
                    .get(*id as usize)
                    .map_or("?", String::as_str),
            ),
            Kind::EnumValue(id) | Kind::EnumType(id) => out.push_str(
                self.names
                    .enums
                    .get(*id as usize)
                    .map_or("?", String::as_str),
            ),
            Kind::Builtin(id) => out.push_str(
                self.names
                    .builtins
                    .get(*id as usize)
                    .map_or("?", String::as_str),
            ),
            Kind::TypeLit(described) => {
                out.push_str("type<");
                self.write(*described, out);
                out.push('>');
            }
            Kind::Var(index) => {
                out.push_str(&format!("T{index}"));
            }
            Kind::SymbolLit(name) => {
                out.push(':');
                out.push_str(name);
            }
            Kind::Exports(_) => out.push_str("module"),
            Kind::Host(id) => out.push_str(
                self.names
                    .hosts
                    .get(*id as usize)
                    .map_or("?", String::as_str),
            ),
        }
    }

    /// The base type name of each alternative, for [`super::ReceiverType`].
    pub fn bases(&self, ty: Ty) -> Vec<String> {
        let mut bases: Vec<String> = self
            .members(ty)
            .into_iter()
            .map(|member| match self.kind(member) {
                Kind::Array(_) | Kind::Tuple(_) => "array".to_owned(),
                Kind::Hash(_) | Kind::Shape(..) | Kind::EmptyHash => "hash".to_owned(),
                Kind::TypeLit(_) => "type".to_owned(),
                Kind::Namespace(_)
                | Kind::EnumType(_)
                | Kind::Builtin(_)
                | Kind::AnyEnumType
                | Kind::Exports(_) => "namespace".to_owned(),
                Kind::Instance(_) | Kind::EnumValue(_) => self.display(member),
                Kind::Host(_) => "host".to_owned(),
                Kind::AnyEnum => "enum_value".to_owned(),
                Kind::Error => "unknown".to_owned(),
                _ => self.display(member),
            })
            .collect();
        bases.sort();
        bases.dedup();
        bases
    }
}

fn write_field_name(name: &str, out: &mut String) {
    let plain = !name.is_empty()
        && name
            .chars()
            .next()
            .is_some_and(|c| c == '_' || c.is_alphabetic())
        && name.chars().all(|c| c == '_' || c.is_alphanumeric());
    if plain {
        out.push_str(name);
    } else {
        out.push('"');
        for c in name.chars() {
            if c == '"' || c == '\\' {
                out.push('\\');
            }
            out.push(c);
        }
        out.push('"');
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unions_flatten_sort_and_render_optionals() {
        let mut types = Types::new();
        let optional = types.optional(Ty::INT);
        assert_eq!(types.display(optional), "int?");
        let nested = types.union(&[optional, Ty::STRING, Ty::NEVER]);
        assert_eq!(types.display(nested), "int | string | nil");
        assert_eq!(types.union(&[Ty::INT, Ty::FLOAT]), Ty::NUMBER);
        let number = types.optional(Ty::NUMBER);
        assert_eq!(types.display(number), "number?");
        assert_eq!(types.union(&[Ty::INT, Ty::ANY]), Ty::ANY);
        assert_eq!(
            types.without_nil(nested),
            types.union(&[Ty::INT, Ty::STRING])
        );
    }

    #[test]
    fn shapes_are_exact_unless_open_and_fit_uniform_hashes() {
        let mut types = Types::new();
        let field = |name: &str, ty, optional| Field {
            name: name.into(),
            ty,
            optional,
        };
        let point = types.shape(
            vec![field("y", Ty::INT, false), field("x", Ty::INT, false)],
            false,
        );
        assert_eq!(types.display(point), "{ x: int, y: int }");
        let dictionary = types.hash(Ty::INT);
        assert!(types.assignable(point, dictionary));
        let wider = types.shape(
            vec![
                field("x", Ty::INT, false),
                field("y", Ty::INT, false),
                field("z", Ty::INT, true),
            ],
            false,
        );
        assert!(types.assignable(point, wider));
        assert!(!types.assignable(wider, point));
        let open = types.shape(vec![field("x", Ty::INT, false)], true);
        assert!(types.assignable(point, open));
        assert!(!types.assignable(open, point));
        let numbers = types.array(Ty::NUMBER);
        let ints = types.array(Ty::INT);
        assert!(types.assignable(ints, numbers));
        assert!(!types.assignable(numbers, ints));
        let pair = types.tuple(vec![Ty::INT, Ty::STRING]);
        let either = types.union(&[Ty::INT, Ty::STRING]);
        let mixed = types.array(either);
        assert!(types.assignable(pair, mixed));
        assert!(!types.assignable(mixed, pair));
    }
}
