//! Compile-time types: interned, so a type is a small copyable id and
//! comparing two types compares ids.

use super::meter::{self, Heap, Meter};
use std::{collections::HashMap, sync::Arc};

/// The most alternatives a union may have: a wider one is an error
/// (V0124), reported where it is written or inferred, before the checker
/// relates it to anything, like the 1,024 levels of syntax the parser
/// allows. No corpus program comes near: the widest union in them has 10.
pub const MAX_ALTERNATIVES: usize = 1024;

/// The most fields a shape may have (V0124). A hash literal of a test that
/// bounds shape writes has 6,003, the most in the corpora.
pub const MAX_FIELDS: usize = 16_384;

/// The most of a type a diagnostic spells out, in bytes: a shape whose
/// fields are shapes, through aliases, repeats them in full at each level.
const SPELLED: usize = 16 << 10;

/// Entries the assignability memo holds before it starts over, so the memo
/// stays small however many pairs a check compares.
const MEMO: usize = 1 << 16;

/// Alternatives the union index holds, across the unions it indexes,
/// before it starts over.
const INDEXED: usize = 1 << 16;

/// Where an alternative of a union files in its index: a value can fit only
/// the alternatives under the heads [`targets`] lists for it, so a value is
/// compared with those, never with the whole union.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Head {
    /// A kind without parts or ids, by its discriminant.
    Plain(std::mem::Discriminant<Kind>),
    /// A kind with an id, such as the instances of one class.
    Id(std::mem::Discriminant<Kind>, u32),
    Array,
    Hash,
    Tuple(usize),
    /// A closed shape whose fields are all required, by its keys: only a
    /// shape of the same keys fits it.
    Exact(u64),
    /// An open shape, or one with optional fields, which shapes of other
    /// keys may fit.
    Loose,
}

/// The head an alternative of `kind` files under.
fn head(kind: &Kind) -> Head {
    match kind {
        Kind::Array(_) => Head::Array,
        Kind::Hash(_) => Head::Hash,
        Kind::Tuple(items) => Head::Tuple(items.len()),
        Kind::Shape(fields, open) => {
            if *open || fields.iter().any(|field| field.optional) {
                Head::Loose
            } else {
                Head::Exact(keys(fields))
            }
        }
        Kind::Instance(id)
        | Kind::EnumValue(id)
        | Kind::EnumType(id)
        | Kind::Namespace(id)
        | Kind::Builtin(id)
        | Kind::Var(id)
        | Kind::Exports(id)
        | Kind::Host(id) => Head::Id(std::mem::discriminant(kind), *id),
        _ => Head::Plain(std::mem::discriminant(kind)),
    }
}

/// The heads of every alternative a value of `kind` may fit, besides itself:
/// [`Types::assignable`]'s rules relate only these.
fn targets(kind: &Kind) -> Vec<Head> {
    let plain = |kind: Kind| Head::Plain(std::mem::discriminant(&kind));
    match kind {
        Kind::Tuple(items) => vec![Head::Tuple(items.len()), Head::Array],
        Kind::Shape(fields, open) => {
            if *open {
                vec![Head::Loose, Head::Hash]
            } else {
                vec![Head::Exact(keys(fields)), Head::Loose, Head::Hash]
            }
        }
        Kind::EmptyHash => vec![Head::Hash, Head::Loose, Head::Exact(keys(&[]))],
        Kind::Hash(_) => vec![Head::Hash, Head::Loose],
        Kind::SymbolLit(_) => vec![head(kind), plain(Kind::Symbol)],
        Kind::EnumValue(_) => vec![head(kind), plain(Kind::AnyEnum)],
        Kind::EnumType(_) => vec![head(kind), plain(Kind::AnyEnumType)],
        _ => vec![head(kind)],
    }
}

/// A shape's keys, as one number.
fn keys(fields: &[Field]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for field in fields {
        field.name.hash(&mut hasher);
    }
    hasher.finish()
}

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

impl Heap for Kind {
    fn heap(&self) -> usize {
        match self {
            Kind::Shape(fields, _) => fields.heap(),
            Kind::Tuple(items) | Kind::Union(items) => items.heap(),
            Kind::SymbolLit(name) => name.heap(),
            _ => 0,
        }
    }
}

impl Heap for Field {
    fn heap(&self) -> usize {
        self.name.heap()
    }
}

impl Heap for Names {
    fn heap(&self) -> usize {
        self.namespaces.heap() + self.enums.heap() + self.builtins.heap() + self.hosts.heap()
    }
}

impl Heap for Head {
    fn heap(&self) -> usize {
        0
    }
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
    /// Each type's kind, which the interner's map shares rather than
    /// copies.
    kinds: Vec<Arc<Kind>>,
    ids: HashMap<Arc<Kind>, Ty>,
    /// What the interned kinds hold on the heap: their allocations and
    /// what those own, such as a shape's fields and their names.
    payload: usize,
    /// Pairs already decided, up to [`MEMO`] of them.
    assignable: HashMap<(Ty, Ty), bool>,
    /// Each indexed union's alternatives by [`Head`], up to [`INDEXED`]
    /// alternatives in all.
    index: HashMap<Ty, Arc<HashMap<Head, Vec<Ty>>>>,
    indexed: usize,
    /// What the indexes hold.
    index_bytes: usize,
    /// [`Self::plain`] of each type asked about.
    plain: HashMap<Ty, bool>,
    pub names: Names,
    /// The check's work and memory account, which type operations charge
    /// and poll while they run.
    meter: Arc<Meter>,
    /// A union or shape too large to build since the checker last looked:
    /// what it was and its size. It became unknown, and the checker
    /// reports it where it looks.
    pub too_large: Option<(&'static str, usize)>,
    /// What operations under way keep beside the table.
    scratch: usize,
}

impl Types {
    /// The bytes the interned types and the caches hold; the names of
    /// declarations are the checker's to count, since they change rarely.
    pub fn bytes(&self) -> usize {
        let tables = [
            meter::map(&self.ids),
            meter::map(&self.assignable),
            meter::map(&self.index),
            meter::map(&self.plain),
        ];
        let largest = tables.iter().copied().max().unwrap_or(0);
        meter::vec(&self.kinds)
            + tables.iter().sum::<usize>()
            + self.payload
            + self.index_bytes
            + self.scratch
            + meter::growth(largest)
    }

    /// A type table with an account of its own.
    pub fn new() -> Self {
        Self::metered(Meter::new(Default::default(), None))
    }

    /// A type table charging `meter`.
    pub fn metered(meter: Arc<Meter>) -> Self {
        let mut types = Self {
            kinds: Vec::new(),
            ids: HashMap::new(),
            payload: 0,
            assignable: HashMap::new(),
            index: HashMap::new(),
            indexed: 0,
            index_bytes: 0,
            plain: HashMap::new(),
            names: Names::default(),
            meter,
            too_large: None,
            scratch: 0,
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

    /// Adds `steps` to the check's work.
    pub fn charge(&self, steps: u64) {
        self.meter.charge(steps);
    }

    /// Whether the check passed its budget; operations then return at once.
    pub fn stopped(&self) -> bool {
        self.meter.stopped()
    }

    /// Checks the account against the budget, the whole check's work and
    /// memory, so one operation on large types cannot run past it.
    fn poll(&self) {
        self.meter.poll(|| self.meter.held(self.bytes()));
    }

    /// Records `bytes` an operation holds beside the table while it runs,
    /// such as a large type it is building, which the budget bounds with
    /// the rest; smaller ones stay within the account's margin.
    fn transient(&self, bytes: usize) {
        if bytes >= 4096 {
            self.meter.transient(self.bytes(), bytes);
        }
    }

    /// Counts `bytes` an operation keeps beside the table while it polls,
    /// until [`Self::release`] takes them back; returns them.
    fn hold(&mut self, bytes: usize) -> usize {
        self.scratch += bytes;
        if bytes >= 4096 {
            self.meter.transient(self.bytes(), 0);
        }
        bytes
    }

    /// Takes back what [`Self::hold`] counted.
    fn release(&mut self, bytes: usize) {
        self.scratch -= bytes;
    }

    pub fn intern(&mut self, kind: Kind) -> Ty {
        // Hashing the kind walks it as far as measuring it does.
        let heap = kind.heap();
        self.transient(heap);
        if let Some(&ty) = self.ids.get(&kind) {
            return ty;
        }
        let ty = Ty(self.kinds.len() as u32);
        self.payload += 2 * std::mem::size_of::<usize>() + std::mem::size_of::<Kind>() + heap;
        let kind = Arc::new(kind);
        self.kinds.push(Arc::clone(&kind));
        self.ids.insert(kind, ty);
        self.poll();
        ty
    }

    pub fn kind(&self, ty: Ty) -> &Kind {
        &self.kinds[ty.0 as usize]
    }

    /// `ty`'s kind, shared rather than copied, which the table's own
    /// changes then leave alone.
    pub fn shared(&self, ty: Ty) -> Arc<Kind> {
        Arc::clone(&self.kinds[ty.0 as usize])
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

    /// The type a value of `ty` has once every hash in it has its keys
    /// renamed, as `deep_transform_keys` renames them: each shape becomes a
    /// dictionary of its fields' types.
    pub fn rekeyed(&mut self, ty: Ty) -> Ty {
        self.charge(1);
        match &*self.shared(ty) {
            Kind::Shape(fields, open) => {
                let mut values: Vec<Ty> =
                    fields.iter().map(|field| self.rekeyed(field.ty)).collect();
                if *open {
                    values.push(Ty::ANY);
                }
                let value = self.union(&values);
                self.hash(value)
            }
            Kind::Hash(value) => {
                let value = self.rekeyed(*value);
                self.hash(value)
            }
            Kind::Array(element) => {
                let element = self.rekeyed(*element);
                self.array(element)
            }
            Kind::Tuple(items) => {
                let items = items.iter().map(|&item| self.rekeyed(item)).collect();
                self.tuple(items)
            }
            Kind::Union(members) => {
                let members: Vec<Ty> = members.iter().map(|&member| self.rekeyed(member)).collect();
                self.union(&members)
            }
            _ => ty,
        }
    }

    /// A shape from fields in any order; a later field of the same name wins.
    pub fn shape(&mut self, mut fields: Vec<Field>, open: bool) -> Ty {
        // The fields are held beside the table until they are interned.
        let held = self.hold(fields.heap());
        fields.reverse();
        fields.sort_by(|a, b| a.name.cmp(&b.name));
        // Sorting them in order kept a copy of them for a moment.
        self.transient(fields.capacity() * std::mem::size_of::<Field>());
        fields.dedup_by(|a, b| a.name == b.name);
        self.work(fields.len());
        let ty = if fields.len() > MAX_FIELDS {
            self.too_large.get_or_insert(("shape", fields.len()));
            Ty::ERROR
        } else {
            self.intern(Kind::Shape(fields.into(), open))
        };
        self.release(held);
        ty
    }

    /// `ty?`.
    pub fn optional(&mut self, ty: Ty) -> Ty {
        self.union(&[ty, Ty::NIL])
    }

    /// The union of `types`: nested unions flatten, `never` drops out, and
    /// `any` or an unknown type absorbs the rest.
    pub fn union(&mut self, types: &[Ty]) -> Ty {
        // The caller's types are held beside the table while this runs.
        self.transient(std::mem::size_of_val(types));
        self.charge(types.len() as u64);
        self.poll();
        if self.stopped() {
            return Ty::ERROR;
        }
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
        // The caller's types, and the members gathered from them.
        self.transient(
            std::mem::size_of_val(types) + members.capacity() * std::mem::size_of::<Ty>(),
        );
        members.sort_unstable();
        members.dedup();
        if members.len() > MAX_ALTERNATIVES {
            self.too_large.get_or_insert(("union", members.len()));
            return Ty::ERROR;
        }
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
        if from == to
            || from == Ty::ERROR
            || to == Ty::ERROR
            || from == Ty::NEVER
            || to == Ty::ANY
            || self.stopped()
        {
            return true;
        }
        if let Some(&known) = self.assignable.get(&(from, to)) {
            return known;
        }
        let result = self.assignable_uncached(from, to);
        if self.assignable.len() >= MEMO {
            self.assignable.clear();
        }
        self.assignable.insert((from, to), result);
        result
    }

    fn assignable_uncached(&mut self, from: Ty, to: Ty) -> bool {
        self.charge(1);
        self.poll();
        if let Kind::Union(members) = self.kind(from) {
            let count = members.len();
            for index in 0..count {
                let Kind::Union(members) = self.kind(from) else {
                    unreachable!("a union stays one");
                };
                let member = members[index];
                if !self.fits(member, to) {
                    return false;
                }
            }
            return true;
        }
        if matches!(self.kind(to), Kind::Union(_)) {
            // A collection of a union may be split across alternatives
            // only element-wise, which the alternatives cover.
            return self.fits(from, to);
        }
        match (self.kind(from), self.kind(to)) {
            (Kind::Array(a), Kind::Array(b)) | (Kind::Hash(a), Kind::Hash(b)) => {
                let (a, b) = (*a, *b);
                self.assignable(a, b)
            }
            (Kind::Tuple(items), Kind::Array(element)) => {
                let (count, element) = (items.len(), *element);
                self.work(count);
                (0..count).all(|index| {
                    let item = self.tuple_item(from, index);
                    self.assignable(item, element)
                })
            }
            (Kind::Tuple(a), Kind::Tuple(b)) => {
                let count = a.len();
                if count != b.len() {
                    return false;
                }
                self.work(count);
                (0..count).all(|index| {
                    let (x, y) = (self.tuple_item(from, index), self.tuple_item(to, index));
                    self.assignable(x, y)
                })
            }
            (Kind::EmptyHash, Kind::Hash(_)) => true,
            (Kind::EmptyHash, Kind::Shape(fields, _)) => fields.iter().all(|field| field.optional),
            (Kind::Shape(fields, open), Kind::Hash(value)) => {
                let (count, open, value) = (fields.len(), *open, *value);
                if open && value != Ty::ANY {
                    return false;
                }
                self.work(count);
                (0..count).all(|index| {
                    let field = self.field_type(from, index);
                    self.assignable(field, value)
                })
            }
            (Kind::Shape(..), Kind::Shape(..)) => self.shape_fits(from, to),
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

    /// Whether `value`, which is not a union, fits `to`: when `to` is a
    /// union, the alternative it is, or one of the alternatives its kind
    /// may fit, which the union's index lists, so wide unions compare a
    /// value with a few alternatives, not all of them.
    fn fits(&mut self, value: Ty, to: Ty) -> bool {
        let Kind::Union(alternatives) = self.kind(to) else {
            return self.assignable(value, to);
        };
        if value == Ty::ERROR || value == Ty::NEVER || alternatives.binary_search(&value).is_ok() {
            return true;
        }
        let candidates = self.candidates(to, value);
        candidates
            .into_iter()
            .any(|candidate| self.assignable(value, candidate))
    }

    /// The alternatives of the union `union` that a value of `value`'s kind
    /// may fit, by the union's index.
    fn candidates(&mut self, union: Ty, value: Ty) -> Vec<Ty> {
        let index = match self.index.get(&union) {
            Some(index) => Arc::clone(index),
            None => {
                let Kind::Union(alternatives) = self.kind(union) else {
                    return Vec::new();
                };
                let mut index: HashMap<Head, Vec<Ty>> = HashMap::new();
                for &alternative in alternatives.iter() {
                    index
                        .entry(head(self.kind(alternative)))
                        .or_default()
                        .push(alternative);
                }
                let count = alternatives.len();
                self.work(count);
                if self.indexed + count > INDEXED {
                    self.index.clear();
                    self.indexed = 0;
                    self.index_bytes = 0;
                }
                self.indexed += count;
                self.index_bytes += index.heap() + 2 * std::mem::size_of::<usize>();
                let index = Arc::new(index);
                self.index.insert(union, Arc::clone(&index));
                index
            }
        };
        let mut found = Vec::new();
        for target in targets(self.kind(value)) {
            if let Some(alternatives) = index.get(&target) {
                found.extend_from_slice(alternatives);
            }
        }
        self.work(found.len());
        found
    }

    /// Whether the shape `from` fits the shape `to`, comparing their fields,
    /// which both keep sorted by name, in one pass.
    fn shape_fits(&mut self, from: Ty, to: Ty) -> bool {
        let (count, from_open, to_open) = match (self.kind(from), self.kind(to)) {
            (Kind::Shape(a, from_open), Kind::Shape(b, to_open)) => {
                (a.len() + b.len(), *from_open, *to_open)
            }
            _ => return false,
        };
        if from_open && !to_open {
            return false;
        }
        self.work(count);
        let (Kind::Shape(a, _), Kind::Shape(b, _)) = (self.kind(from), self.kind(to)) else {
            return false;
        };
        let mut pairs = Vec::new();
        let (mut i, mut j) = (0, 0);
        while i < a.len() || j < b.len() {
            let order = match (a.get(i), b.get(j)) {
                (Some(x), Some(y)) => x.name.cmp(&y.name),
                (Some(_), None) => std::cmp::Ordering::Less,
                _ => std::cmp::Ordering::Greater,
            };
            match order {
                std::cmp::Ordering::Equal => {
                    if a[i].optional && !b[j].optional {
                        return false;
                    }
                    pairs.push((a[i].ty, b[j].ty));
                    i += 1;
                    j += 1;
                }
                // A field `to` does not declare.
                std::cmp::Ordering::Less => {
                    if !to_open {
                        return false;
                    }
                    i += 1;
                }
                // A field `from` lacks.
                std::cmp::Ordering::Greater => {
                    if !(b[j].optional || from_open) {
                        return false;
                    }
                    j += 1;
                }
            }
        }
        pairs.into_iter().all(|(x, y)| self.assignable(x, y))
    }

    /// Charges `units` of work that grows with a type's size, a step for
    /// every 64, so small types cost nothing more.
    pub fn work(&mut self, units: usize) {
        if units >= 64 {
            self.charge((units / 64) as u64);
            self.poll();
        }
    }

    fn tuple_item(&self, tuple: Ty, index: usize) -> Ty {
        match self.kind(tuple) {
            Kind::Tuple(items) => items[index],
            _ => Ty::ERROR,
        }
    }

    fn field_type(&self, shape: Ty, index: usize) -> Ty {
        match self.kind(shape) {
            Kind::Shape(fields, _) => fields[index].ty,
            _ => Ty::ERROR,
        }
    }

    /// The alternatives of `declared` that a value of `ty` may be: those
    /// some alternative of `ty` fits, found through `declared`'s index.
    pub fn meet(&mut self, declared: Ty, ty: Ty) -> Vec<Ty> {
        let values = self.members(ty);
        let mut kept = Vec::new();
        for value in values {
            if !matches!(self.kind(declared), Kind::Union(_)) {
                if self.assignable(value, declared) {
                    kept.push(declared);
                }
                continue;
            }
            if matches!(self.kind(declared), Kind::Union(alternatives) if alternatives.binary_search(&value).is_ok())
            {
                kept.push(value);
            }
            for candidate in self.candidates(declared, value) {
                if candidate != value && self.assignable(value, candidate) {
                    kept.push(candidate);
                }
            }
        }
        kept.sort_unstable();
        kept.dedup();
        kept
    }

    /// The field of a shape's `fields` named `name`, by binary search:
    /// shapes keep their fields sorted by name.
    pub fn field<'f>(fields: &'f [Field], name: &[u8]) -> Option<&'f Field> {
        fields
            .binary_search_by(|field| field.name.as_bytes().cmp(name))
            .ok()
            .map(|index| &fields[index])
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
        match &*self.shared(ty) {
            Kind::Var(index) => bindings
                .get(*index as usize)
                .copied()
                .flatten()
                .unwrap_or(ty),
            Kind::Array(t) => {
                let t = self.subst(*t, bindings);
                self.array(t)
            }
            Kind::Hash(t) => {
                let t = self.subst(*t, bindings);
                self.hash(t)
            }
            Kind::TypeLit(t) => {
                let t = self.subst(*t, bindings);
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
                self.shape(fields, *open)
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
        self.close_as(ty, bindings, Ty::ERROR)
    }

    /// Replaces every type variable of a call's result. Nothing constrains
    /// one left unbound, such as the element of an empty literal, so no
    /// value has its type and it becomes `never`; unknown would accept
    /// anything without a report.
    pub fn close_result(&mut self, ty: Ty, bindings: &[Option<Ty>]) -> Ty {
        self.close_as(ty, bindings, Ty::NEVER)
    }

    fn close_as(&mut self, ty: Ty, bindings: &[Option<Ty>], unbound: Ty) -> Ty {
        let ty = self.subst(ty, bindings);
        if !self.has_var(ty) {
            return ty;
        }
        let unknown: Vec<Option<Ty>> = (0..64).map(|_| Some(unbound)).collect();
        self.subst(ty, &unknown)
    }

    /// The element type an array, tuple or range yields when iterated.
    pub fn element(&mut self, ty: Ty) -> Option<Ty> {
        match &*self.shared(ty) {
            Kind::Array(element) => Some(*element),
            Kind::Tuple(items) => Some(self.union(items)),
            Kind::Range => Some(Ty::INT),
            Kind::Error | Kind::Any | Kind::Never => Some(ty),
            _ => None,
        }
    }

    /// The value type of a hash or shape, as `hash<string, V>` would read it.
    pub fn hash_value(&mut self, ty: Ty) -> Option<Ty> {
        match &*self.shared(ty) {
            Kind::Hash(value) => Some(*value),
            Kind::EmptyHash => Some(Ty::NEVER),
            Kind::Shape(fields, open) => {
                if *open {
                    return Some(Ty::ANY);
                }
                let types: Vec<Ty> = fields.iter().map(|f| f.ty).collect();
                Some(self.union(&types))
            }
            _ => None,
        }
    }

    /// `ty` as an annotation writes it, or `any` when no annotation can
    /// name it, such as for a class used as a value or a required file, or
    /// one too large to spell out.
    pub fn annotation(&self, ty: Ty) -> String {
        match self.spell(ty) {
            (text, false) if self.nameable(ty) => text,
            _ => "any".to_owned(),
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

    /// `ty` as a diagnostic writes it, cut short with `...` after
    /// [`SPELLED`] bytes.
    pub fn display(&self, ty: Ty) -> String {
        self.spell(ty).0
    }

    /// [`Self::display`], and whether it was cut short.
    fn spell(&self, ty: Ty) -> (String, bool) {
        let mut out = String::new();
        self.write(ty, &mut out);
        if out.len() <= SPELLED {
            return (out, false);
        }
        let mut end = SPELLED;
        while !out.is_char_boundary(end) {
            end -= 1;
        }
        out.truncate(end);
        out.push_str("...");
        (out, true)
    }

    /// Writes `ty` to `out`, stopping once `out` holds more than
    /// [`SPELLED`] bytes, which a shape nested through aliases reaches
    /// however few bytes spell its declarations.
    fn write(&self, ty: Ty, out: &mut String) {
        if out.len() > SPELLED {
            return;
        }
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
                    if out.len() > SPELLED {
                        return;
                    }
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
                    if out.len() > SPELLED {
                        return;
                    }
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
                // Past the bytes left to spell, the display is cut, so the
                // alternatives after them are not written.
                let room = SPELLED.saturating_sub(out.len());
                let mut written = 0;
                let mut parts: Vec<String> = Vec::new();
                for &m in &others {
                    if number && (m == Ty::INT || m == Ty::FLOAT) {
                        continue;
                    }
                    if written > room {
                        break;
                    }
                    let mut text = String::new();
                    self.write(m, &mut text);
                    written += text.len() + 3;
                    parts.push(text);
                }
                if number {
                    parts.push("number".to_owned());
                }
                parts.sort_unstable();
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
    /// Whether a value of type `ty` is plain: it can hold no host method or
    /// exported function. Only `any`, capabilities and required modules can,
    /// and so can a type the checker could not determine.
    pub fn plain(&mut self, ty: Ty) -> bool {
        if let Some(&plain) = self.plain.get(&ty) {
            return plain;
        }
        let plain = match &*self.shared(ty) {
            Kind::Any | Kind::Error | Kind::Var(_) | Kind::Exports(_) | Kind::Host(_) => false,
            Kind::Array(element) | Kind::Hash(element) => self.plain(*element),
            Kind::Shape(fields, _) => fields.iter().all(|field| self.plain(field.ty)),
            Kind::Tuple(items) | Kind::Union(items) => items.iter().all(|&item| self.plain(item)),
            _ => true,
        };
        self.plain.insert(ty, plain);
        plain
    }

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
    fn unions_compare_each_value_with_the_alternatives_it_can_fit() {
        let mut types = Types::new();
        let field = |name: &str, optional| Field {
            name: name.into(),
            ty: Ty::INT,
            optional,
        };
        let shapes: Vec<Ty> = (0..1_000)
            .map(|i| types.shape(vec![field(&format!("a{i}"), false)], false))
            .collect();
        let wide = types.union(&shapes);
        let optional = types.optional(wide);
        assert!(types.assignable(wide, optional));
        assert!(!types.assignable(optional, wide));
        assert_eq!(types.meet(optional, wide).len(), 1_000);
        // Deciding them compares no pair of different shapes.
        assert!(types.assignable.len() < 100, "{}", types.assignable.len());
        // Values of other kinds still fit the alternatives their rules name.
        let ints = types.array(Ty::INT);
        let dictionary = types.hash(Ty::INT);
        let loose = types.shape(vec![field("a0", false), field("z", true)], false);
        let open = types.shape(vec![], true);
        let members = types.union(&[ints, dictionary, loose, Ty::SYMBOL, Ty::ANY_ENUM]);
        let pair = types.tuple(vec![Ty::INT, Ty::INT]);
        let enum_value = types.intern(Kind::EnumValue(0));
        let symbol = types.intern(Kind::SymbolLit("a".into()));
        for value in [pair, shapes[0], Ty::EMPTY_HASH, enum_value, symbol] {
            assert!(types.assignable(value, members), "{}", types.display(value));
        }
        let with_open = types.union(&[open, Ty::INT]);
        assert!(types.assignable(shapes[1], with_open));
        let anything = types.hash(Ty::ANY);
        assert!(types.assignable(anything, with_open));
        assert!(!types.assignable(dictionary, with_open));
        assert!(!types.assignable(Ty::STRING, members));
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
