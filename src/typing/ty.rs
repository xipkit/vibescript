//! Compile-time types: interned, so a type is a small copyable id and
//! comparing two types compares ids.

use super::{
    counted::{CountedMap, CountedVec, ScratchVec},
    meter::{self, Heap, Meter},
};
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

/// The head an alternative of `kind` files under, a closed shape's by
/// the number its keys hash to, `exact`.
fn head(kind: &Kind, exact: u64) -> Head {
    match kind {
        Kind::Array(_) => Head::Array,
        Kind::Hash(_) => Head::Hash,
        Kind::Tuple(items) => Head::Tuple(items.len()),
        Kind::Shape(fields, open) => {
            if *open || fields.iter().any(|field| field.optional) {
                Head::Loose
            } else {
                Head::Exact(exact)
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
/// [`Types::assignable`]'s rules relate only these. A closed shape's keys
/// hash to `exact`, as `{}`'s none do.
fn targets(kind: &Kind, exact: u64) -> Vec<Head> {
    let plain = |kind: Kind| Head::Plain(std::mem::discriminant(&kind));
    match kind {
        Kind::Tuple(items) => vec![Head::Tuple(items.len()), Head::Array],
        Kind::Shape(_, open) => {
            if *open {
                vec![Head::Loose, Head::Hash]
            } else {
                vec![Head::Exact(exact), Head::Loose, Head::Hash]
            }
        }
        Kind::EmptyHash => vec![Head::Hash, Head::Loose, Head::Exact(exact)],
        Kind::Hash(_) => vec![Head::Hash, Head::Loose],
        Kind::SymbolLit(_) => vec![head(kind, exact), plain(Kind::Symbol)],
        Kind::EnumValue(_) => vec![head(kind, exact), plain(Kind::AnyEnum)],
        Kind::EnumType(_) => vec![head(kind, exact), plain(Kind::AnyEnumType)],
        _ => vec![head(kind, exact)],
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

/// What a field owns: its name.
impl super::counted::Owned for Field {
    fn owned(&self) -> usize {
        self.name.len()
    }
}

/// A type's alternatives, as [`Types::members`] gives them: a union's,
/// shared with its kind, or the type alone.
pub(crate) struct Members {
    kind: Arc<Kind>,
    single: Ty,
}

impl std::ops::Deref for Members {
    type Target = [Ty];

    fn deref(&self) -> &[Ty] {
        match &*self.kind {
            Kind::Union(members) => members,
            _ => std::slice::from_ref(&self.single),
        }
    }
}

impl IntoIterator for Members {
    type Item = Ty;
    type IntoIter = MembersIter;

    fn into_iter(self) -> MembersIter {
        MembersIter {
            members: self,
            at: 0,
        }
    }
}

impl<'m> IntoIterator for &'m Members {
    type Item = &'m Ty;
    type IntoIter = std::slice::Iter<'m, Ty>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

/// The alternatives of a [`Members`], in order.
pub(crate) struct MembersIter {
    members: Members,
    at: usize,
}

impl Iterator for MembersIter {
    type Item = Ty;

    fn next(&mut self) -> Option<Ty> {
        let member = self.members.get(self.at).copied();
        self.at += 1;
        member
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let left = self.members.len().saturating_sub(self.at);
        (left, Some(left))
    }
}

impl ExactSizeIterator for MembersIter {}

/// The names of script classes and enums, for rendering types.
#[derive(Default)]
pub(crate) struct Names {
    pub namespaces: CountedVec<String>,
    pub enums: CountedVec<String>,
    pub builtins: CountedVec<String>,
    /// Declared host capabilities, by the index a `Kind::Host` holds.
    pub hosts: CountedVec<String>,
}

/// The type interner of one check.
pub(crate) struct Types {
    /// Each type's kind, which the interner's map shares rather than
    /// copies.
    kinds: CountedVec<Arc<Kind>>,
    ids: CountedMap<Arc<Kind>, Ty>,
    /// What the interned kinds hold on the heap: their allocations and
    /// what those own, such as a shape's fields and their names.
    payload: usize,
    /// Pairs already decided, up to [`MEMO`] of them.
    assignable: CountedMap<(Ty, Ty), bool>,
    /// Each indexed union's alternatives by [`Head`], up to [`INDEXED`]
    /// alternatives in all.
    index: CountedMap<Ty, Arc<HashMap<Head, Vec<Ty>>>>,
    indexed: usize,
    /// What the indexes hold.
    index_bytes: usize,
    /// [`Self::plain`] of each type asked about.
    plain: CountedMap<Ty, bool>,
    /// The number each closed shape's keys hash to, for the unions'
    /// indexes: hashed once, as it is first asked for.
    exact: CountedMap<Ty, u64>,
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
            meter::map(&self.exact),
        ];
        meter::vec(self.kinds.as_vec())
            + tables.iter().sum::<usize>()
            + self.payload
            + self.index_bytes
            + self.scratch
    }

    /// A type table with an account of its own.
    pub fn new() -> Self {
        Self::metered(Meter::new(Default::default(), None))
    }

    /// A type table charging `meter`.
    pub fn metered(meter: Arc<Meter>) -> Self {
        let mut types = Self {
            kinds: CountedVec::new(),
            ids: CountedMap::new(),
            payload: 0,
            assignable: CountedMap::new(),
            index: CountedMap::new(),
            indexed: 0,
            index_bytes: 0,
            plain: CountedMap::new(),
            exact: CountedMap::new(),
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
            types.add(kind);
        }
        let number = types.add(Kind::Union(Box::new([Ty::INT, Ty::FLOAT])));
        debug_assert_eq!(number, Ty::NUMBER);
        types
    }

    /// Adds `steps` to the check's work. Returns whether the check has
    /// stopped.
    #[must_use = "the budget may have stopped the check, which must then do no more work"]
    pub fn charge(&self, steps: u64) -> bool {
        self.meter.charge(steps)
    }

    /// Whether the check passed its budget; operations then return at once.
    pub fn stopped(&self) -> bool {
        self.meter.stopped()
    }

    /// Checks the account against the budget, the whole check's work and
    /// memory, so one operation on large types cannot run past it. Returns
    /// whether the check has stopped.
    #[must_use = "the budget may have stopped the check, which must then do no more work"]
    fn poll(&self) -> bool {
        self.meter.poll(|| self.meter.held(self.bytes()))
    }

    /// Records `bytes` an operation holds beside the table while it runs,
    /// such as a large type it is building, which the budget bounds with
    /// the rest; smaller ones stay within the account's margin. Returns
    /// whether the check has stopped.
    #[must_use = "the budget may have stopped the check, which must then do no more work"]
    fn transient(&self, bytes: usize) -> bool {
        if bytes >= 4096 {
            return self.meter.transient(self.bytes(), bytes);
        }
        self.stopped()
    }

    /// Counts `bytes` an operation keeps beside the table while it polls,
    /// until [`Self::release`] takes them back; returns them. A check that
    /// has stopped, or that they stop, holds nothing and gets `None`.
    #[must_use = "the budget may have stopped the check, which must then do no more work"]
    fn hold(&mut self, bytes: usize) -> Option<usize> {
        if self.stopped() {
            return None;
        }
        self.scratch += bytes;
        if bytes >= 4096 && self.meter.transient(self.bytes(), 0) {
            self.scratch -= bytes;
            return None;
        }
        Some(bytes)
    }

    /// Takes back what [`Self::hold`] counted.
    fn release(&mut self, bytes: usize) {
        self.scratch -= bytes;
    }

    pub fn intern(&mut self, kind: Kind) -> Ty {
        // A stopped check adds no types.
        if self.stopped() {
            return Ty::ERROR;
        }
        // Hashing the kind walks it as far as measuring it does, and a
        // check that measure stops adds nothing.
        let heap = kind.heap();
        if self.transient(heap) {
            return Ty::ERROR;
        }
        let Some(ty) = self.insert(kind, heap, false) else {
            return Ty::ERROR;
        };
        if self.poll() {
            return Ty::ERROR;
        }
        ty
    }

    /// Adds one of the table's own types, which it holds however the check
    /// stands, since each has its fixed place; the check's first poll reads
    /// a stop.
    fn add(&mut self, kind: Kind) -> Ty {
        let heap = kind.heap();
        let ty = self.insert(kind, heap, true).unwrap_or(Ty::ERROR);
        let _ = self.poll();
        ty
    }

    /// Interns `kind`, whose kinds hold `heap` bytes, unless the table has
    /// it already. The kind and room for it in both tables are counted
    /// before either changes; `None` when the budget refuses them, which
    /// stops the check and interns nothing, unless the table holds it
    /// `regardless`.
    fn insert(&mut self, kind: Kind, heap: usize, regardless: bool) -> Option<Ty> {
        if let Some(&ty) = self.ids.get(&kind) {
            return Some(ty);
        }
        let ledger = if regardless {
            self.meter.types().regardless()
        } else {
            self.meter.types()
        };
        let payload = 2 * std::mem::size_of::<usize>() + std::mem::size_of::<Kind>() + heap;
        ledger.keep(payload).ok()?;
        self.kinds.reserve(ledger, 1).ok()?;
        self.ids.reserve(ledger, 1).ok()?;
        let ty = Ty(self.kinds.len() as u32);
        self.payload += payload;
        let kind = Arc::new(kind);
        self.kinds.push_within(Arc::clone(&kind));
        self.ids.insert_within(kind, ty);
        Some(ty)
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
        if self.charge(1) {
            return Ty::ERROR;
        }
        match &*self.shared(ty) {
            Kind::Shape(fields, open) => {
                let types = fields.iter().map(|field| field.ty);
                let Some(mut values) = self.mapped(types, fields.len() + 1, Self::rekeyed) else {
                    return Ty::ERROR;
                };
                if *open {
                    values.add(Ty::ANY);
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
                match self.mapped(items.iter().copied(), items.len(), Self::rekeyed) {
                    Some(items) => self.tuple(items.into_vec()),
                    None => Ty::ERROR,
                }
            }
            Kind::Union(members) => {
                match self.mapped(members.iter().copied(), members.len(), Self::rekeyed) {
                    Some(members) => self.union(&members),
                    None => Ty::ERROR,
                }
            }
            _ => ty,
        }
    }

    /// `map` of each of `types`, in a list made at `room` places and
    /// counted while it lives, since mapping one maps the types nested in
    /// it in turn, beside the list; `None` once the check stops.
    fn mapped(
        &mut self,
        types: impl Iterator<Item = Ty>,
        room: usize,
        mut map: impl FnMut(&mut Self, Ty) -> Ty,
    ) -> Option<ScratchVec<Ty>> {
        let mut mapped = ScratchVec::new(&self.meter);
        mapped.reserve(room).ok()?;
        for ty in types {
            let ty = map(self, ty);
            mapped.add(ty);
        }
        Some(mapped)
    }

    /// A shape from fields in any order; a later field of the same name wins.
    pub fn shape(&mut self, mut fields: Vec<Field>, open: bool) -> Ty {
        // The fields are held beside the table until they are interned.
        let Some(held) = self.hold(fields.heap()) else {
            return Ty::ERROR;
        };
        // Sorted, once the sort's steps and the copy a stable sort keeps
        // are counted; the limit counts the distinct names, which a later
        // field of a name already given does not add to.
        fields.reverse();
        let sorted = super::counted::sort_by(&self.meter, &mut fields, |a, b| a.name.cmp(&b.name));
        if sorted.is_err() {
            self.release(held);
            return Ty::ERROR;
        }
        fields.dedup_by(|a, b| a.name == b.name);
        if self.work(fields.len()) {
            self.release(held);
            return Ty::ERROR;
        }
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
        // The caller's types, and a copy of them in order, are held beside
        // the table while this runs.
        if self.charge(types.len() as u64) || self.poll() {
            return Ty::ERROR;
        }
        let Some(held) = self.hold(2 * std::mem::size_of_val(types)) else {
            return Ty::ERROR;
        };
        let ty = self.union_held(types);
        self.release(held);
        ty
    }

    /// [`Self::union`], once `types` and a copy of them are held.
    fn union_held(&mut self, types: &[Ty]) -> Ty {
        // Each distinct type once, so that many of one wide union flatten
        // it once rather than once each.
        let mut distinct = types.to_vec();
        if super::counted::sort_unstable_by(&self.meter, &mut distinct, Ord::cmp).is_err() {
            return Ty::ERROR;
        }
        distinct.dedup();
        let mut absorbed = false;
        for &ty in &distinct {
            match self.kind(ty) {
                Kind::Error => return Ty::ERROR,
                _ if ty == Ty::ANY => absorbed = true,
                _ => (),
            }
        }
        if absorbed {
            return Ty::ANY;
        }
        // The members gathered so far are put in order and made distinct
        // whenever they pass twice as many as the last time, so they never
        // hold many more than the distinct members, which the table's own
        // unions hold already. The list is counted before it grows, while
        // its old and new storage are both held, and while it lives.
        let mut next = 2 * (MAX_ALTERNATIVES + 1);
        let mut members = ScratchVec::new(&self.meter);
        if members.reserve(distinct.len().min(next)).is_err() {
            return Ty::ERROR;
        }
        for &ty in &distinct {
            let count = match self.kind(ty) {
                Kind::Never => continue,
                Kind::Union(inner) => inner.len(),
                _ => {
                    if members.push(ty).is_err() {
                        return Ty::ERROR;
                    }
                    continue;
                }
            };
            // Each member a union adds is work, which the budget bounds.
            if self.work(count) {
                return Ty::ERROR;
            }
            let Kind::Union(inner) = self.kind(ty) else {
                unreachable!("a union stays one");
            };
            if members.extend_from_slice(inner).is_err() {
                return Ty::ERROR;
            }
            if members.len() > next {
                if super::counted::sort_unstable_by(&self.meter, &mut members, Ord::cmp).is_err() {
                    return Ty::ERROR;
                }
                members.dedup();
                next = next.max(2 * members.len());
            }
        }
        if super::counted::sort_unstable_by(&self.meter, &mut members, Ord::cmp).is_err() {
            return Ty::ERROR;
        }
        members.dedup();
        if members.len() > MAX_ALTERNATIVES {
            self.too_large.get_or_insert(("union", members.len()));
            return Ty::ERROR;
        }
        match members.len() {
            0 => Ty::NEVER,
            1 => members[0],
            _ => self.intern(Kind::Union(members.into_vec().into())),
        }
    }

    /// The alternatives of a union, or the type itself.
    /// The alternatives of `ty`, a union's or `ty` alone, shared with the
    /// table's kind rather than copied, so that one held while others are
    /// read holds nothing more.
    pub fn members(&self, ty: Ty) -> Members {
        Members {
            kind: self.shared(ty),
            single: ty,
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
        // A pair the budget refuses room for is decided again if asked.
        if self
            .assignable
            .insert(self.meter.types(), (from, to), result)
            .is_err()
        {
            return true;
        }
        result
    }

    /// A stopped check relates no more types, and takes every one as
    /// assignable, so it reports nothing more.
    fn assignable_uncached(&mut self, from: Ty, to: Ty) -> bool {
        if self.charge(1) || self.poll() {
            return true;
        }
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
                if self.work(count) {
                    return true;
                }
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
                if self.work(count) {
                    return true;
                }
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
                if self.work(count) {
                    return true;
                }
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
            .iter()
            .any(|&candidate| self.assignable(value, candidate))
    }

    /// The alternatives of the union `union` that a value of `value`'s kind
    /// may fit, by the union's index, in a list counted while it lives,
    /// since comparing a value with each compares nested types in turn.
    fn candidates(&mut self, union: Ty, value: Ty) -> ScratchVec<Ty> {
        let index = match self.index.get(&union) {
            Some(index) => Arc::clone(index),
            None => {
                let Kind::Union(alternatives) = self.kind(union) else {
                    return ScratchVec::new(&self.meter);
                };
                // Charged before it is built.
                let count = alternatives.len();
                if self.work(count) {
                    return ScratchVec::new(&self.meter);
                }
                if self.indexed + count > INDEXED {
                    self.index.clear();
                    self.indexed = 0;
                    self.index_bytes = 0;
                }
                // The most the index can take is counted before it is
                // built, with room for it in the table of indexes: a
                // table with a head for each alternative at most, and
                // lists of each head's alternatives, each at most twice
                // as long as it is or four long.
                let ledger = self.meter.types();
                let most = meter::table::<(Head, Vec<Ty>)>(2 * count + 1)
                    + 6 * count * std::mem::size_of::<Ty>()
                    + 2 * std::mem::size_of::<usize>();
                if ledger.keep(most).is_err() || self.index.reserve(ledger, 1).is_err() {
                    return ScratchVec::new(&self.meter);
                }
                let kind = self.shared(union);
                let Kind::Union(alternatives) = &*kind else {
                    return ScratchVec::new(&self.meter);
                };
                let mut index: HashMap<Head, Vec<Ty>> = HashMap::new();
                for &alternative in alternatives.iter() {
                    let exact = self.exact_keys(alternative);
                    index
                        .entry(head(self.kind(alternative), exact))
                        .or_default()
                        .push(alternative);
                }
                self.indexed += count;
                self.index_bytes += index.heap() + 2 * std::mem::size_of::<usize>();
                let index = Arc::new(index);
                self.index.insert_within(union, Arc::clone(&index));
                index
            }
        };
        // Charged, and counted, before it is listed.
        let exact = self.exact_keys(value);
        let targets = targets(self.kind(value), exact);
        let count: usize = targets
            .iter()
            .filter_map(|target| index.get(target).map(Vec::len))
            .sum();
        let mut found = ScratchVec::new(&self.meter);
        if self.work(count) || found.reserve(count).is_err() {
            return ScratchVec::new(&self.meter);
        }
        for target in targets {
            if let Some(alternatives) = index.get(&target) {
                for &alternative in alternatives {
                    found.add(alternative);
                }
            }
        }
        found
    }

    /// The number `ty`'s keys hash to, when it is a closed shape or `{}`,
    /// for the unions' indexes: a shape's hashed once, as it is first asked
    /// for, and kept, the bytes hashed charged as a step for each 64 of
    /// them; `0` for any other type, or once the check stops.
    fn exact_keys(&mut self, ty: Ty) -> u64 {
        if let Some(&exact) = self.exact.get(&ty) {
            return exact;
        }
        let kind = self.shared(ty);
        let fields = match &*kind {
            Kind::Shape(fields, false) => fields,
            Kind::EmptyHash => return keys(&[]),
            _ => return 0,
        };
        let bytes: usize = fields.iter().map(|field| field.name.len()).sum();
        if self.work(bytes.saturating_add(fields.len())) {
            return 0;
        }
        let exact = keys(fields);
        if self.exact.insert(self.meter.types(), ty, exact).is_err() {
            return 0;
        }
        exact
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
        if self.work(count) {
            return true;
        }
        let (Kind::Shape(a, _), Kind::Shape(b, _)) = (self.kind(from), self.kind(to)) else {
            return false;
        };
        // The fields both declare, whose types are compared once the names
        // are, in a list counted while it lives: comparing them compares
        // nested shapes' fields in turn, beside it. A check that stops
        // relates no more types, and takes every one as assignable.
        let mut pairs = ScratchVec::new(&self.meter);
        if pairs.reserve(a.len().min(b.len())).is_err() {
            return true;
        }
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
                    pairs.add((a[i].ty, b[j].ty));
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
        pairs.iter().all(|&(x, y)| self.assignable(x, y))
    }

    /// Charges `units` of work that grows with a type's size, a step for
    /// every 64, so small types cost nothing more. Returns whether the
    /// check has stopped.
    #[must_use = "the budget may have stopped the check, which must then do no more work"]
    pub fn work(&mut self, units: usize) -> bool {
        if units >= 64 {
            return self.charge((units / 64) as u64) || self.poll();
        }
        self.stopped()
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
    /// Each pair of an alternative of `ty` and a candidate of `declared` is
    /// charged, as [`Self::candidates`] charges them and as each relation
    /// not decided before is, and the budget is asked for each alternative
    /// of `ty`.
    pub fn meet(&mut self, declared: Ty, ty: Ty) -> Vec<Ty> {
        let values = self.members(ty);
        // The alternatives kept, in a list counted while it lives, put in
        // order and made distinct whenever they pass twice as many as the
        // last time, so they never hold many more than `declared`'s
        // alternatives, which are at most 1,024, however many pairs fit.
        let mut kept = ScratchVec::new(&self.meter);
        let mut next = 2 * (MAX_ALTERNATIVES + 1);
        for value in values {
            if self.poll() {
                return Vec::new();
            }
            if !matches!(self.kind(declared), Kind::Union(_)) {
                if self.assignable(value, declared) {
                    kept.add(declared);
                }
                continue;
            }
            if matches!(self.kind(declared), Kind::Union(alternatives) if alternatives.binary_search(&value).is_ok())
            {
                kept.add(value);
            }
            for &candidate in self.candidates(declared, value).iter() {
                if candidate != value && self.assignable(value, candidate) {
                    kept.add(candidate);
                }
            }
            if self.stopped() {
                return Vec::new();
            }
            if kept.len() > next {
                if super::counted::sort_unstable_by(&self.meter, &mut kept, Ord::cmp).is_err() {
                    return Vec::new();
                }
                kept.dedup();
                next = next.max(2 * kept.len());
            }
        }
        // A check the sort stops meets nothing.
        if super::counted::sort_unstable_by(&self.meter, &mut kept, Ord::cmp).is_err() {
            return Vec::new();
        }
        kept.dedup();
        kept.into_vec()
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
                // The fields, with copies of their names, are listed at
                // their number, in a list counted while it lives, the
                // copies counted before they are made.
                let mut copied = ScratchVec::new(&self.meter);
                let names = fields.iter().map(|field| field.name.len()).sum();
                if copied.reserve_with(fields.len(), names).is_err() {
                    return Ty::ERROR;
                }
                for field in fields.iter() {
                    let ty = self.subst(field.ty, bindings);
                    copied.push_within(Field {
                        name: field.name.clone(),
                        ty,
                        optional: field.optional,
                    });
                }
                self.shape(copied.into_vec(), *open)
            }
            Kind::Tuple(items) => {
                let subst = |types: &mut Self, t| types.subst(t, bindings);
                match self.mapped(items.iter().copied(), items.len(), subst) {
                    Some(items) => self.tuple(items.into_vec()),
                    None => Ty::ERROR,
                }
            }
            Kind::Union(items) => {
                let subst = |types: &mut Self, t| types.subst(t, bindings);
                match self.mapped(items.iter().copied(), items.len(), subst) {
                    Some(items) => self.union(&items),
                    None => Ty::ERROR,
                }
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

    /// [`Self::display`], and whether it was cut short. A stopped check,
    /// whose findings are dropped, spells nothing.
    fn spell(&self, ty: Ty) -> (String, bool) {
        if self.stopped() {
            return (String::new(), false);
        }
        // One byte past what the display spells, so a display that uses
        // it all was cut.
        let mut room = SPELLED + 1;
        let mut out = String::new();
        self.write(ty, &mut out, &mut room);
        if room > 0 {
            return (out, false);
        }
        let mut end = SPELLED.min(out.len());
        while !out.is_char_boundary(end) {
            end -= 1;
        }
        out.truncate(end);
        out.push_str("...");
        (out, true)
    }

    /// Writes `ty` to `out`, taking each byte it writes from `room`, which
    /// every part of the display shares, however deep it nests, and
    /// stopping once the room is used up. The alternatives of a union are
    /// each written apart, to be put in order, and then moved into `out`,
    /// which takes no more room.
    fn write(&self, ty: Ty, out: &mut String, room: &mut usize) {
        if *room == 0 {
            return;
        }
        match self.kind(ty) {
            Kind::Error => put(out, "unknown", room),
            Kind::Never => put(out, "never", room),
            Kind::Any => put(out, "any", room),
            Kind::Nil => put(out, "nil", room),
            Kind::Bool => put(out, "bool", room),
            Kind::Int => put(out, "int", room),
            Kind::Float => put(out, "float", room),
            Kind::String => put(out, "string", room),
            Kind::Symbol => put(out, "symbol", room),
            Kind::Duration => put(out, "duration", room),
            Kind::Time => put(out, "time", room),
            Kind::Money => put(out, "money", room),
            Kind::Range => put(out, "range", room),
            Kind::Regex => put(out, "regex", room),
            Kind::MatchData => put(out, "match_data", room),
            Kind::ErrorValue => put(out, "error", room),
            Kind::EmptyHash => put(out, "{}", room),
            Kind::AnyEnum => put(out, "enum_value", room),
            Kind::AnyEnumType => put(out, "enum_type", room),
            Kind::Array(element) => {
                put(out, "array<", room);
                self.write(*element, out, room);
                put(out, ">", room);
            }
            Kind::Hash(value) => {
                put(out, "hash<string, ", room);
                self.write(*value, out, room);
                put(out, ">", room);
            }
            Kind::Shape(fields, open) => {
                if fields.is_empty() && !open {
                    put(out, "{}", room);
                    return;
                }
                put(out, "{ ", room);
                for (index, field) in fields.iter().enumerate() {
                    if *room == 0 {
                        return;
                    }
                    if index > 0 {
                        put(out, ", ", room);
                    }
                    write_field_name(&field.name, out, room);
                    if field.optional {
                        put(out, "?", room);
                    }
                    put(out, ": ", room);
                    self.write(field.ty, out, room);
                }
                if *open {
                    if !fields.is_empty() {
                        put(out, ", ", room);
                    }
                    put(out, "...", room);
                }
                put(out, " }", room);
            }
            Kind::Tuple(items) => {
                put(out, "[", room);
                for (index, &item) in items.iter().enumerate() {
                    if *room == 0 {
                        return;
                    }
                    if index > 0 {
                        put(out, ", ", room);
                    }
                    self.write(item, out, room);
                }
                put(out, "]", room);
            }
            Kind::Union(members) => {
                let nil = members.contains(&Ty::NIL);
                let others = || members.iter().copied().filter(|&m| m != Ty::NIL);
                let number = members.contains(&Ty::INT) && members.contains(&Ty::FLOAT);
                // Each alternative is written apart, while the room lasts;
                // past it the display is cut, so the alternatives after
                // are not written.
                let mut parts: Vec<String> = Vec::new();
                for m in others() {
                    if number && (m == Ty::INT || m == Ty::FLOAT) {
                        continue;
                    }
                    if *room == 0 {
                        break;
                    }
                    let mut text = String::new();
                    self.write(m, &mut text, room);
                    parts.push(text);
                }
                if number && *room > 0 {
                    let mut text = String::new();
                    put(&mut text, "number", room);
                    parts.push(text);
                }
                parts.sort_unstable();
                if nil && parts.len() == 1 {
                    let single = others().nth(1).is_none() || number;
                    if single {
                        out.push_str(&parts[0]);
                        put(out, "?", room);
                        return;
                    }
                }
                if nil && *room > 0 {
                    let mut text = String::new();
                    put(&mut text, "nil", room);
                    parts.push(text);
                }
                // Once the room is used up, the display is cut where it
                // was, so the parts after are not written, which keeps it
                // a prefix of the display in full.
                for (index, part) in parts.iter().enumerate() {
                    if index > 0 {
                        if *room == 0 {
                            break;
                        }
                        put(out, " | ", room);
                    }
                    out.push_str(part);
                }
            }
            Kind::Instance(id) | Kind::Namespace(id) => {
                put(out, name_of(&self.names.namespaces, *id), room)
            }
            Kind::EnumValue(id) | Kind::EnumType(id) => {
                put(out, name_of(&self.names.enums, *id), room)
            }
            Kind::Builtin(id) => put(out, name_of(&self.names.builtins, *id), room),
            Kind::TypeLit(described) => {
                put(out, "type<", room);
                self.write(*described, out, room);
                put(out, ">", room);
            }
            Kind::Var(index) => put(out, &format!("T{index}"), room),
            Kind::SymbolLit(name) => {
                put(out, ":", room);
                put(out, name, room);
            }
            Kind::Exports(_) => put(out, "module", room),
            Kind::Host(id) => put(out, name_of(&self.names.hosts, *id), room),
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
        // A type the budget refuses room for is looked at again if asked.
        if self.plain.insert(self.meter.types(), ty, plain).is_err() {
            return true;
        }
        plain
    }

    /// The base type name of each alternative of `ty`, for
    /// [`super::ReceiverType`], sorted and without repeats, its pass over
    /// the alternatives a step for each 64 of them. The names are put in a
    /// list counted while it is built, each name a display spells counted
    /// before it is spelled, and in order through the meter; `None` once
    /// the budget refuses them, which stops the check.
    pub fn bases(&self, ty: Ty) -> Option<Vec<String>> {
        let members = self.members(ty);
        if members.len() >= 64 && (self.charge((members.len() / 64) as u64) || self.poll()) {
            return None;
        }
        let mut bases = ScratchVec::new(&self.meter);
        if bases.reserve(members.len()).is_err() {
            return None;
        }
        for &member in members.iter() {
            let base = match base_word(self.kind(member)) {
                Some(word) => word.to_owned(),
                None => {
                    self.meter.admit(self.spelled(member))?;
                    self.display(member)
                }
            };
            if bases.push(base).is_err() {
                return None;
            }
        }
        if super::counted::sort_unstable_by(&self.meter, &mut bases, Ord::cmp).is_err() {
            return None;
        }
        bases.dedup();
        Some(bases.into_vec())
    }

    /// The one base of all `ty`'s alternatives when it is one the runtime
    /// binds builtins to, the only one their [`Self::bases`] name, found
    /// without spelling any, its pass over them a step for each 64; `None`
    /// when there is none, or once the check stops.
    pub fn direct_base(&self, ty: Ty) -> Option<crate::members::direct::Base> {
        use crate::members::direct::Base;
        let members = self.members(ty);
        if members.len() >= 64 && (self.charge((members.len() / 64) as u64) || self.poll()) {
            return None;
        }
        let mut found = None;
        for &member in members.iter() {
            let base = match self.kind(member) {
                Kind::Array(_) | Kind::Tuple(_) => Base::Array,
                Kind::Hash(_) | Kind::Shape(..) | Kind::EmptyHash => Base::Hash,
                Kind::String => Base::String,
                Kind::Int => Base::Int,
                Kind::Float => Base::Float,
                _ => return None,
            };
            if found.is_some_and(|other| other != base) {
                return None;
            }
            found = Some(base);
        }
        found
    }

    /// The bytes [`Self::display`] spells for `member`, an alternative
    /// [`Self::bases`] names by its display, at most what a display cut
    /// short takes.
    fn spelled(&self, member: Ty) -> usize {
        let length = match self.kind(member) {
            Kind::Instance(id) => name_of(&self.names.namespaces, *id).len(),
            Kind::EnumValue(id) => name_of(&self.names.enums, *id).len(),
            Kind::SymbolLit(name) => 1 + name.len(),
            Kind::Var(index) => 2 + index.checked_ilog10().unwrap_or(0) as usize,
            _ => "match_data".len(),
        };
        length.min(SPELLED + "...".len())
    }
}

fn write_field_name(name: &str, out: &mut String, room: &mut usize) {
    let plain = !name.is_empty()
        && name
            .chars()
            .next()
            .is_some_and(|c| c == '_' || c.is_alphabetic())
        && name.chars().all(|c| c == '_' || c.is_alphanumeric());
    if plain {
        put(out, name, room);
    } else {
        put(out, "\"", room);
        for c in name.chars() {
            if *room == 0 {
                return;
            }
            if c == '"' || c == '\\' {
                put(out, "\\", room);
            }
            put(out, c.encode_utf8(&mut [0; 4]), room);
        }
        put(out, "\"", room);
    }
}

/// The name of `id` in `list`, or `?` for one it lacks.
/// The base [`Types::bases`] names an alternative of `kind` by, when it is
/// a word rather than the alternative's display: a class's or an enum's
/// name, a symbol literal or a type variable.
fn base_word(kind: &Kind) -> Option<&'static str> {
    Some(match kind {
        Kind::Array(_) | Kind::Tuple(_) => "array",
        Kind::Hash(_) | Kind::Shape(..) | Kind::EmptyHash => "hash",
        Kind::TypeLit(_) => "type",
        Kind::Namespace(_)
        | Kind::EnumType(_)
        | Kind::Builtin(_)
        | Kind::AnyEnumType
        | Kind::Exports(_) => "namespace",
        Kind::Host(_) => "host",
        Kind::AnyEnum => "enum_value",
        Kind::Error => "unknown",
        Kind::Never => "never",
        Kind::Any => "any",
        Kind::Nil => "nil",
        Kind::Bool => "bool",
        Kind::Int => "int",
        Kind::Float => "float",
        Kind::String => "string",
        Kind::Symbol => "symbol",
        Kind::Duration => "duration",
        Kind::Time => "time",
        Kind::Money => "money",
        Kind::Range => "range",
        Kind::Regex => "regex",
        Kind::MatchData => "match_data",
        Kind::ErrorValue => "error",
        _ => return None,
    })
}

fn name_of(list: &[String], id: u32) -> &str {
    list.get(id as usize).map_or("?", String::as_str)
}

/// Writes as much of `text` to `out` as `room` has left, on a character
/// boundary, and takes it from the room, which a text it cuts uses up.
fn put(out: &mut String, text: &str, room: &mut usize) {
    let mut end = text.len().min(*room);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    out.push_str(&text[..end]);
    *room = if end < text.len() { 0 } else { *room - end };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_receivers_bases_and_its_direct_base_agree() {
        use crate::members::direct::Base;
        let mut types = Types::new();
        let array = types.array(Ty::INT);
        let tuple = types.tuple(vec![Ty::INT, Ty::STRING]);
        let hash = types.hash(Ty::INT);
        let arrays = types.union(&[array, tuple]);
        let optional = types.optional(Ty::STRING);
        let mixed = types.union(&[hash, Ty::INT, Ty::NIL]);
        let symbol = types.intern(Kind::SymbolLit("name".into()));
        let cases = [
            (arrays, vec!["array"], Some(Base::Array)),
            (hash, vec!["hash"], Some(Base::Hash)),
            (Ty::FLOAT, vec!["float"], Some(Base::Float)),
            (optional, vec!["nil", "string"], None),
            (mixed, vec!["hash", "int", "nil"], None),
            (Ty::ANY, vec!["any"], None),
            (symbol, vec![":name"], None),
        ];
        for (ty, bases, base) in cases {
            assert_eq!(types.bases(ty).unwrap(), bases, "{}", types.display(ty));
            assert_eq!(types.direct_base(ty), base, "{}", types.display(ty));
        }
    }

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
