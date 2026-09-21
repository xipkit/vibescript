use crate::{
    CallContext, Result, Value,
    budget::Buffer,
    types::{Scalar, Type, TypeKind},
};
use std::hash::{DefaultHasher, Hash, Hasher};

const EMPTY: usize = usize::MAX;

mod attached;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum Callable {
    Host(usize),
    Function(usize),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(super) struct Fact(pub usize);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(usize)]
pub(super) enum Atom {
    Never,
    Unknown,
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
}

impl Atom {
    pub fn fact(self) -> Fact {
        Fact(self as usize)
    }
}

#[derive(Debug)]
pub(super) struct Field {
    pub name: Value,
    pub value: Fact,
    pub optional: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum NominalId {
    Binding(usize, usize),
    Enumeration(usize),
}

/// The set of runtime provenances a hash fact may have.
///
/// Bits are only added by joins and removed by evidence (plain or object
/// copies). A kind lacking a tag bit is never that protected object at
/// runtime, so a join of untagged hashes never admits protected values.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) struct HashKind(u8);

impl HashKind {
    pub const PLAIN: Self = Self(0b0001);
    pub const OBJECT: Self = Self(0b0010);
    pub const MATCH: Self = Self(0b0100);
    pub const ERROR: Self = Self(0b1000);
    /// Structural annotations without concrete values admit every provenance.
    pub const ANY: Self = Self(0b1111);

    /// Certainly a plain hash.
    pub fn plain(self) -> bool {
        self == Self::PLAIN
    }

    /// Certainly an untagged host object.
    pub fn object(self) -> bool {
        self == Self::OBJECT
    }

    /// Exactly one untagged dispatch provenance, so member dispatch is uniform.
    pub fn single(self) -> bool {
        self.plain() || self.object()
    }

    /// May be a protected match or error object.
    pub fn tagged(self) -> bool {
        self.0 & (Self::MATCH.0 | Self::ERROR.0) != 0
    }

    /// Whether the two sets share a possible runtime provenance.
    pub fn overlaps(self, other: Self) -> bool {
        self.0 & other.0 != 0
    }

    pub fn join(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

impl From<bool> for HashKind {
    fn from(plain: bool) -> Self {
        if plain { Self::PLAIN } else { Self::ANY }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum InstanceKind {
    // Created during this analysis; cannot alias an unknown incoming object.
    Concrete,
    Captured,
    Symbolic,
    Summary,
}

impl InstanceKind {
    pub fn concrete(self) -> bool {
        matches!(self, Self::Concrete | Self::Captured)
    }
}

#[derive(Debug)]
pub(super) enum Node {
    Atom(Atom),
    Boolean(bool),
    Integer(i64),
    IntegerBounds(super::integers::Bounds),
    Float(u64),
    String(Value),
    Symbol(Value),
    Range(Option<i64>, Option<i64>, bool),
    Regex(Value),
    Builtin(crate::builtin::Builtin),
    Callable {
        owner: usize,
        target: Callable,
    },
    Offset(Fact),
    Protected(Fact, crate::hash::Tag),
    TypeValue(Fact),
    Instance {
        class: Fact,
        slot: usize,
        kind: InstanceKind,
    },
    Enumeration {
        nominal: Fact,
        value: Value,
    },
    EnumMember {
        enumeration: Fact,
        index: Option<usize>,
    },
    Array(Fact),
    Tuple(Buffer<Fact>),
    Hash(Fact, Fact, HashKind),
    Shape(Buffer<Field>, bool, Fact, HashKind),
    Union(Buffer<Fact>),
    Choice(Buffer<Fact>),
    Named(Value),
    Nominal {
        identity: NominalId,
        name: Value,
        symbols: Option<Buffer<Value>>,
    },
}

#[derive(Debug)]
struct Entry {
    node: Node,
    hash: u64,
    next: usize,
    choices: bool,
    string_key: Option<bool>,
    normalizes: bool,
    unresolved: bool,
    singleton: bool,
    depth: usize,
    escapes: bool,
    exported: Fact,
}

#[derive(Debug)]
pub(super) struct Facts {
    entries: Buffer<Entry>,
    buckets: Buffer<usize>,
    enumerations: Buffer<Fact>,
    sources: super::sources::Sources,
    max_depth: usize,
}

impl Facts {
    pub fn new(ctx: &mut CallContext) -> Result<Self> {
        let mut facts = Self {
            entries: Buffer::empty(),
            buckets: Buffer::empty(),
            enumerations: Buffer::empty(),
            sources: super::sources::Sources::new(),
            max_depth: 0,
        };
        for atom in [
            Atom::Never,
            Atom::Unknown,
            Atom::Any,
            Atom::Nil,
            Atom::Bool,
            Atom::Int,
            Atom::Float,
            Atom::String,
            Atom::Symbol,
            Atom::Duration,
            Atom::Time,
            Atom::Money,
            Atom::Range,
            Atom::Regex,
        ] {
            let fact = facts.intern(ctx, Node::Atom(atom))?;
            debug_assert_eq!(fact, atom.fact());
        }
        Ok(facts)
    }

    pub fn node(&self, fact: Fact) -> &Node {
        &self.entries.data[fact.0].node
    }

    /// Compares declared types without merging the state of captured snapshots.
    pub fn same_nominal(&self, ctx: &mut CallContext, left: Fact, right: Fact) -> Result<bool> {
        if left == right {
            return Ok(true);
        }
        match (self.node(left), self.node(right)) {
            (
                Node::Nominal {
                    identity: NominalId::Binding(a, ai),
                    ..
                },
                Node::Nominal {
                    identity: NominalId::Binding(b, bi),
                    ..
                },
            ) if ai == bi => self.sources.same_type(ctx, *a, *b),
            _ => Ok(false),
        }
    }

    /// Identifies source code and captured scope without retaining the scope's mutable heap.
    pub fn source_owner(
        &mut self,
        ctx: &mut CallContext,
        code: &std::sync::Arc<crate::code::Code>,
        environment: Option<&crate::objects::Instance>,
    ) -> Result<usize> {
        self.sources.owner(ctx, code, environment)
    }

    /// Separates private environments for imports and retries within each invocation.
    pub fn import_owner(
        &mut self,
        ctx: &mut CallContext,
        code: &std::sync::Arc<crate::code::Code>,
        receiving: super::sources::SourceId,
        attempt: usize,
    ) -> Result<usize> {
        self.sources.import_owner(ctx, code, receiving, attempt)
    }

    /// Returns a deterministic source key for call summaries and diagnostics.
    pub fn source_id(
        &self,
        ctx: &mut CallContext,
        owner: usize,
    ) -> Result<super::sources::SourceId> {
        self.sources.id(ctx, owner)
    }

    /// Borrows an owned source through a shared handle without retaining its mutable environment.
    pub fn source_code(
        &self,
        ctx: &mut CallContext,
        source: super::sources::SourceId,
    ) -> Result<Option<std::sync::Arc<crate::code::Code>>> {
        self.sources.code(ctx, source)
    }

    pub fn len(&self) -> usize {
        self.entries.data.len()
    }

    pub fn string_key(&self, fact: Fact) -> Option<bool> {
        self.entries.data[fact.0].string_key
    }

    pub fn has_choices(&self, fact: Fact) -> bool {
        self.entries.data[fact.0].choices
    }

    pub fn normalizes(&self, fact: Fact) -> bool {
        self.entries.data[fact.0].normalizes
    }

    pub fn unresolved(&self, fact: Fact) -> bool {
        self.entries.data[fact.0].unresolved
    }

    pub(super) fn singleton(&self, fact: Fact) -> bool {
        self.entries.data[fact.0].singleton
    }

    pub(super) fn depth(&self, fact: Fact) -> usize {
        self.entries.data[fact.0].depth
    }

    pub(super) fn max_depth(&self) -> usize {
        self.max_depth
    }

    fn intern(&mut self, ctx: &mut CallContext, node: Node) -> Result<Fact> {
        ctx.checkpoint()?;
        let hash = node.hash(ctx, &self.sources)?;
        if !self.buckets.data.is_empty() {
            let mut index = self.buckets.data[hash as usize & (self.buckets.data.len() - 1)];
            while index != EMPTY {
                ctx.charge(1)?;
                let previous = &self.entries.data[index];
                if previous.hash == hash && node.same(ctx, &previous.node)? {
                    return Ok(Fact(index));
                }
                index = previous.next;
            }
        }
        if self.entries.data.len() >= self.buckets.data.len() / 2 {
            self.grow(ctx)?;
        }
        let bucket = hash as usize & (self.buckets.data.len() - 1);
        let fact = Fact(self.entries.data.len());
        ctx.charge(match &node {
            Node::Tuple(values) => values.data.len() as u64,
            Node::Shape(fields, ..) => fields.data.len() as u64,
            _ => 1,
        })?;
        let choices = match &node {
            Node::Union(_) | Node::Choice(_) => true,
            Node::Tuple(values) => values.data.iter().any(|&value| self.has_choices(value)),
            Node::Shape(fields, ..) => fields
                .data
                .iter()
                .any(|field| field.optional || self.has_choices(field.value)),
            _ => false,
        };
        let string_key = match &node {
            Node::Atom(Atom::Unknown) | Node::Named(_) | Node::Nominal { .. } => None,
            Node::Atom(Atom::String | Atom::Symbol | Atom::Any)
            | Node::String(_)
            | Node::Symbol(_) => Some(true),
            Node::Union(arms) | Node::Choice(arms) => {
                let mut matches = false;
                let mut known = true;
                for &arm in &arms.data {
                    ctx.charge(1)?;
                    if let Some(allowed) = self.string_key(arm) {
                        matches |= allowed;
                    } else {
                        known = false;
                    }
                }
                known.then_some(matches)
            }
            _ => Some(false),
        };
        let normalizes = match &node {
            Node::Choice(_) => true,
            Node::Protected(value, _) | Node::Offset(value) => self.normalizes(*value),
            Node::Named(_) | Node::Nominal { .. } => true,
            Node::Array(element) => self.normalizes(*element),
            Node::Hash(key, value, _) => self.normalizes(*key) || self.normalizes(*value),
            Node::Tuple(values) | Node::Union(values) => {
                ctx.charge(values.data.len() as u64)?;
                values.data.iter().any(|&value| self.normalizes(value))
            }
            Node::Shape(fields, ..) => {
                ctx.charge(fields.data.len() as u64)?;
                fields.data.iter().any(|field| self.normalizes(field.value))
            }
            _ => false,
        };
        let unresolved = match &node {
            Node::Protected(value, _) | Node::Offset(value) => self.unresolved(*value),
            Node::Named(_) => true,
            Node::TypeValue(ty) => self.unresolved(*ty),
            Node::Array(element) => self.unresolved(*element),
            Node::Hash(key, value, _) => self.unresolved(*key) || self.unresolved(*value),
            Node::Tuple(values) | Node::Union(values) | Node::Choice(values) => {
                ctx.charge(values.data.len() as u64)?;
                values.data.iter().any(|&value| self.unresolved(value))
            }
            Node::Shape(fields, ..) => {
                ctx.charge(fields.data.len() as u64)?;
                fields.data.iter().any(|field| self.unresolved(field.value))
            }
            _ => false,
        };
        // Float literals stay outside canonical equality: mixed numeric equality and NaN
        // cannot be decided by comparing fact IDs, including inside containers.
        let singleton = match &node {
            Node::Atom(Atom::Nil)
            | Node::Boolean(_)
            | Node::Integer(_)
            | Node::String(_)
            | Node::Symbol(_)
            | Node::Range(..)
            | Node::Regex(_)
            | Node::Enumeration { .. }
            | Node::EnumMember { index: Some(_), .. } => true,
            Node::Instance { kind, .. } => kind.concrete(),
            Node::Tuple(values) => {
                ctx.charge(values.data.len() as u64)?;
                values.data.iter().all(|&value| self.singleton(value))
            }
            Node::Shape(fields, false, keys, HashKind::PLAIN) if *keys == Atom::String.fact() => {
                ctx.charge(fields.data.len() as u64)?;
                fields
                    .data
                    .iter()
                    .all(|field| !field.optional && self.singleton(field.value))
            }
            _ => false,
        };
        let depth = match &node {
            Node::Protected(value, _) | Node::Offset(value) => self.depth(*value),
            Node::Array(element) => self.depth(*element).saturating_add(1),
            Node::Hash(key, value, _) => self.depth(*key).max(self.depth(*value)).saturating_add(1),
            Node::Tuple(values) | Node::Union(values) | Node::Choice(values) => {
                ctx.charge(values.data.len() as u64)?;
                values
                    .data
                    .iter()
                    .map(|&value| self.depth(value))
                    .max()
                    .unwrap_or(0)
                    .saturating_add(usize::from(matches!(node, Node::Tuple(_))))
            }
            Node::Shape(fields, ..) => {
                ctx.charge(fields.data.len() as u64)?;
                fields
                    .data
                    .iter()
                    .map(|field| self.depth(field.value))
                    .max()
                    .unwrap_or(0)
                    .saturating_add(1)
            }
            _ => 0,
        };
        let escapes = self.node_escapes(ctx, &node)?;
        self.entries.push(
            ctx,
            Entry {
                node,
                hash,
                next: self.buckets.data[bucket],
                choices,
                string_key,
                normalizes,
                unresolved,
                singleton,
                depth,
                escapes,
                exported: Fact(EMPTY),
            },
        )?;
        self.buckets.data[bucket] = fact.0;
        self.max_depth = self.max_depth.max(depth);
        Ok(fact)
    }

    fn grow(&mut self, ctx: &mut CallContext) -> Result<()> {
        let Some(capacity) = self.buckets.data.len().max(8).checked_mul(2) else {
            return ctx.fail(crate::ErrorKind::Memory, "checker fact table size overflow");
        };
        let mut buckets = Buffer::with_capacity(ctx, capacity)?;
        ctx.charge(capacity as u64)?;
        buckets.data.resize(capacity, EMPTY);
        // Reserve and charge the entire rebuild before changing existing chains.
        ctx.charge(self.entries.data.len() as u64)?;
        for (index, entry) in self.entries.data.iter_mut().enumerate() {
            let bucket = entry.hash as usize & (capacity - 1);
            entry.next = buckets.data[bucket];
            buckets.data[bucket] = index;
        }
        self.buckets = buckets;
        Ok(())
    }

    pub fn boolean(&mut self, ctx: &mut CallContext, value: bool) -> Result<Fact> {
        self.intern(ctx, Node::Boolean(value))
    }

    pub fn builtin(
        &mut self,
        ctx: &mut CallContext,
        value: crate::builtin::Builtin,
    ) -> Result<Fact> {
        self.intern(ctx, Node::Builtin(value))
    }

    /// Describes a method bound to a particular analysis world without retaining its code.
    pub fn callable(
        &mut self,
        ctx: &mut CallContext,
        owner: usize,
        target: Callable,
    ) -> Result<Fact> {
        self.intern(ctx, Node::Callable { owner, target })
    }

    pub fn type_value(&mut self, ctx: &mut CallContext, ty: Fact) -> Result<Fact> {
        self.intern(ctx, Node::TypeValue(ty))
    }

    pub fn instance(&mut self, ctx: &mut CallContext, class: Fact, slot: usize) -> Result<Fact> {
        self.instance_kind(ctx, class, slot, InstanceKind::Concrete)
    }

    /// Describes a concrete object, one symbolic input, or a collection of possible inputs.
    pub fn instance_kind(
        &mut self,
        ctx: &mut CallContext,
        class: Fact,
        slot: usize,
        kind: InstanceKind,
    ) -> Result<Fact> {
        self.intern(ctx, Node::Instance { class, slot, kind })
    }

    pub(super) fn protected(
        &mut self,
        ctx: &mut CallContext,
        shape: Fact,
        tag: crate::hash::Tag,
    ) -> Result<Fact> {
        assert!(tag.protected());
        self.intern(ctx, Node::Protected(shape, tag))
    }

    pub(super) fn offset(&mut self, ctx: &mut CallContext, values: Fact) -> Result<Fact> {
        self.intern(ctx, Node::Offset(values))
    }

    pub(super) fn integer_range(
        &mut self,
        ctx: &mut CallContext,
        bounds: super::integers::Bounds,
    ) -> Result<Fact> {
        match (bounds.min, bounds.max) {
            (None, None) => Ok(Atom::Int.fact()),
            (Some(a), Some(b)) if a > b => Ok(Atom::Never.fact()),
            (Some(a), Some(b)) if a == b => self.integer(ctx, a),
            _ => self.intern(ctx, Node::IntegerBounds(bounds)),
        }
    }

    pub fn integer(&mut self, ctx: &mut CallContext, value: i64) -> Result<Fact> {
        self.intern(ctx, Node::Integer(value))
    }

    pub fn string(&mut self, ctx: &mut CallContext, value: &[u8]) -> Result<Fact> {
        let value = ctx.bytes(value)?;
        self.intern(ctx, Node::String(value))
    }

    pub fn symbol(&mut self, ctx: &mut CallContext, value: &[u8]) -> Result<Fact> {
        let value = ctx.bytes(value)?;
        self.intern(ctx, Node::Symbol(value))
    }

    pub fn array(&mut self, ctx: &mut CallContext, element: Fact) -> Result<Fact> {
        self.intern(ctx, Node::Array(element))
    }

    pub(super) fn range(
        &mut self,
        ctx: &mut CallContext,
        start: Option<i64>,
        end: Option<i64>,
        exclusive: bool,
    ) -> Result<Fact> {
        self.intern(ctx, Node::Range(start, end, exclusive))
    }

    pub(super) fn regex(&mut self, ctx: &mut CallContext, value: Value) -> Result<Fact> {
        self.intern(ctx, Node::Regex(value))
    }

    pub(super) fn float(&mut self, ctx: &mut CallContext, value: f64) -> Result<Fact> {
        self.intern(ctx, Node::Float(value.to_bits()))
    }

    pub fn tuple(&mut self, ctx: &mut CallContext, elements: &[Fact]) -> Result<Fact> {
        let mut values = Buffer::with_capacity(ctx, elements.len())?;
        values.extend(ctx, elements)?;
        self.intern(ctx, Node::Tuple(values))
    }

    pub fn hash(&mut self, ctx: &mut CallContext, key: Fact, value: Fact) -> Result<Fact> {
        self.hash_kind(ctx, key, value, false)
    }

    pub(super) fn hash_kind(
        &mut self,
        ctx: &mut CallContext,
        key: Fact,
        value: Fact,
        kind: impl Into<HashKind>,
    ) -> Result<Fact> {
        self.intern(ctx, Node::Hash(key, value, kind.into()))
    }

    pub fn shape(
        &mut self,
        ctx: &mut CallContext,
        fields: &[(&[u8], Fact, bool)],
        open: bool,
    ) -> Result<Fact> {
        let mut values = Buffer::with_capacity(ctx, fields.len())?;
        for &(name, value, optional) in fields {
            ctx.charge(1)?;
            let name = ctx.bytes(name)?;
            values.push(
                ctx,
                Field {
                    name,
                    value,
                    optional,
                },
            )?;
        }
        self.shape_fields(ctx, values, open, Atom::String.fact(), true)
    }

    pub(super) fn shape_fields(
        &mut self,
        ctx: &mut CallContext,
        mut fields: Buffer<Field>,
        open: bool,
        keys: Fact,
        kind: impl Into<HashKind>,
    ) -> Result<Fact> {
        let mut work = 0usize;
        for field in &fields.data {
            ctx.charge(1)?;
            work = work.saturating_add(field.name.as_bytes().unwrap().len().saturating_add(1));
        }
        ctx.charge(work.saturating_mul(fields.data.len().max(1).ilog2() as usize + 1) as u64)?;
        let mut ordered = Buffer::with_capacity(ctx, fields.data.len())?;
        for (index, field) in fields.data.drain(..).enumerate() {
            ordered.push(ctx, (index, field))?;
        }
        // The index preserves duplicate-key order without an untracked sort buffer.
        ordered.data.sort_unstable_by(|(ai, a), (bi, b)| {
            a.name.as_bytes().cmp(&b.name.as_bytes()).then(ai.cmp(bi))
        });
        ordered.data.dedup_by(|(_, later), (_, earlier)| {
            if later.name.as_bytes() == earlier.name.as_bytes() {
                earlier.value = later.value;
                earlier.optional = later.optional;
                true
            } else {
                false
            }
        });
        for (_, field) in ordered.data.drain(..) {
            fields.push(ctx, field)?;
        }
        self.intern(ctx, Node::Shape(fields, open, keys, kind.into()))
    }

    pub fn nominal(
        &mut self,
        ctx: &mut CallContext,
        owner: usize,
        declaration: usize,
        name: &[u8],
        symbols: Option<&[&[u8]]>,
    ) -> Result<Fact> {
        let name = ctx.bytes(name)?;
        let symbols = if let Some(symbols) = symbols {
            let mut values = Buffer::with_capacity(ctx, symbols.len())?;
            for symbol in symbols {
                let value = ctx.bytes(symbol)?;
                values.push(ctx, value)?;
            }
            Some(values)
        } else {
            None
        };
        self.intern(
            ctx,
            Node::Nominal {
                identity: NominalId::Binding(owner, declaration),
                name,
                symbols,
            },
        )
    }

    /// Admits immutable enum metadata without evaluating a declaration body.
    pub fn enumeration(&mut self, ctx: &mut CallContext, value: &Value) -> Result<Fact> {
        ctx.checkpoint()?;
        let crate::value::Kind::Enum(enumeration) = &value.0 else {
            unreachable!()
        };
        for &fact in &self.enumerations.data {
            ctx.charge(1)?;
            let Node::Enumeration { value, .. } = self.node(fact) else {
                unreachable!()
            };
            let crate::value::Kind::Enum(previous) = &value.0 else {
                unreachable!()
            };
            if std::sync::Arc::ptr_eq(&enumeration.definition, &previous.definition) {
                return Ok(fact);
            }
        }
        let mut symbols = Buffer::with_capacity(ctx, enumeration.definition.members.len())?;
        for member in &enumeration.definition.members {
            let symbol = ctx.bytes(member.symbol.as_bytes())?;
            symbols.push(ctx, symbol)?;
        }
        let name = ctx.bytes(enumeration.definition.name.as_bytes())?;
        let nominal = self.intern(
            ctx,
            Node::Nominal {
                identity: NominalId::Enumeration(self.enumerations.data.len()),
                name,
                symbols: Some(symbols),
            },
        )?;
        let value = ctx.import(value)?;
        let fact = self.intern(ctx, Node::Enumeration { nominal, value })?;
        self.enumerations.push(ctx, fact)?;
        Ok(fact)
    }

    /// Represents one member of an admitted enum type.
    pub fn enum_member(
        &mut self,
        ctx: &mut CallContext,
        enumeration: Fact,
        index: usize,
    ) -> Result<Fact> {
        debug_assert!(matches!(self.node(enumeration), Node::Enumeration { .. }));
        self.intern(
            ctx,
            Node::EnumMember {
                enumeration,
                index: Some(index),
            },
        )
    }

    /// Represents the successful value domain of a resolved enum contract.
    pub(super) fn enum_members(
        &mut self,
        ctx: &mut CallContext,
        enumeration: Fact,
    ) -> Result<Fact> {
        let Node::Enumeration { value, .. } = self.node(enumeration) else {
            unreachable!()
        };
        let crate::value::Kind::Enum(value) = &value.0 else {
            unreachable!()
        };
        if value.definition.members.len() == 1 {
            return self.enum_member(ctx, enumeration, 0);
        }
        self.intern(
            ctx,
            Node::EnumMember {
                enumeration,
                index: None,
            },
        )
    }

    pub(super) fn enum_contract(&self, value: Fact) -> Option<Fact> {
        if let Node::Nominal {
            identity: NominalId::Enumeration(index),
            ..
        } = self.node(value)
        {
            Some(self.enumerations.data[*index])
        } else {
            None
        }
    }

    /// Keeps conversion order and nested any fallbacks in annotation unions.
    pub(super) fn choice(&mut self, ctx: &mut CallContext, options: &[Fact]) -> Result<Fact> {
        ctx.charge(options.len() as u64)?;
        if !options.iter().any(|&value| {
            self.normalizes(value)
                || value == Atom::Any.fact()
                || matches!(self.node(value), Node::Choice(_))
        }) {
            return self.union(ctx, options);
        }
        let mut values = Buffer::with_capacity(ctx, options.len())?;
        values.extend(ctx, options)?;
        self.intern(ctx, Node::Choice(values))
    }

    pub(super) fn enum_nominal(&self, value: Fact) -> Option<Fact> {
        let Node::EnumMember { enumeration, .. } = self.node(value) else {
            return None;
        };
        let Node::Enumeration { nominal, .. } = self.node(*enumeration) else {
            unreachable!()
        };
        Some(*nominal)
    }

    pub fn union(&mut self, ctx: &mut CallContext, alternatives: &[Fact]) -> Result<Fact> {
        ctx.checkpoint()?;
        let mut arms = Buffer::empty();
        for &fact in alternatives {
            ctx.charge(1)?;
            match self.node(fact) {
                Node::Atom(Atom::Never) => (),
                Node::Union(values) => arms.extend(ctx, &values.data)?,
                _ => arms.push(ctx, fact)?,
            }
        }
        ctx.charge(
            arms.data
                .len()
                .saturating_mul(arms.data.len().max(1).ilog2() as usize + 1) as u64,
        )?;
        arms.data.sort_unstable();
        arms.data.dedup();
        let mut bools = 0;
        let mut symbols = false;
        let mut integers = false;
        let mut floats = false;
        let mut strings = false;
        let mut ranges = false;
        let mut regexes = false;
        let mut any = false;
        for &fact in &arms.data {
            ctx.charge(1)?;
            match self.node(fact) {
                Node::Atom(Atom::Bool) => bools = 3,
                Node::Boolean(false) => bools |= 1,
                Node::Boolean(true) => bools |= 2,
                Node::Atom(Atom::Symbol) => symbols = true,
                Node::Atom(Atom::Int) => integers = true,
                Node::Atom(Atom::Float) => floats = true,
                Node::Atom(Atom::String) => strings = true,
                Node::Atom(Atom::Range) => ranges = true,
                Node::Atom(Atom::Regex) => regexes = true,
                Node::Atom(Atom::Any) => any = true,
                _ => (),
            }
        }
        ctx.charge(arms.data.len() as u64)?;
        arms.data.retain(|&fact| match self.node(fact) {
            Node::Boolean(_) => bools != 3,
            Node::Symbol(_) => !symbols,
            Node::Integer(_) | Node::IntegerBounds(_) => !integers,
            Node::Float(_) => !floats,
            Node::String(_) => !strings,
            Node::Range(..) => !ranges,
            Node::Regex(_) => !regexes,
            Node::Atom(Atom::Unknown) => !any,
            _ => true,
        });
        if bools == 3 && arms.data.binary_search(&Atom::Bool.fact()).is_err() {
            arms.push(ctx, Atom::Bool.fact())?;
            ctx.charge(
                arms.data
                    .len()
                    .saturating_mul(arms.data.len().max(1).ilog2() as usize + 1)
                    as u64,
            )?;
            arms.data.sort_unstable();
        }
        match arms.data.as_slice() {
            [] => Ok(Atom::Never.fact()),
            [fact] => Ok(*fact),
            _ => self.intern(ctx, Node::Union(arms)),
        }
    }

    pub fn annotation(
        &mut self,
        ctx: &mut CallContext,
        ty: &Type,
        mut resolve: impl FnMut(&mut CallContext, &str) -> Result<Option<Fact>>,
    ) -> Result<Fact> {
        enum Task<'a> {
            Visit(&'a Type),
            Nullable,
            Array,
            Hash,
            Shape(&'a [crate::types::Field], bool),
            Union(usize),
        }
        let mut tasks = Buffer::empty();
        let mut values = Buffer::empty();
        tasks.push(ctx, Task::Visit(ty))?;
        while let Some(task) = tasks.data.pop() {
            ctx.charge(1)?;
            let fact = match task {
                Task::Visit(ty) => {
                    if ty.nullable {
                        tasks.push(ctx, Task::Nullable)?;
                    }
                    match &ty.kind {
                        TypeKind::Scalar(Scalar::Number) => {
                            self.union(ctx, &[Atom::Int.fact(), Atom::Float.fact()])?
                        }
                        TypeKind::Scalar(scalar) => scalar_atom(*scalar).fact(),
                        TypeKind::Array(None) => self.array(ctx, Atom::Any.fact())?,
                        TypeKind::Array(Some(element)) => {
                            tasks.push(ctx, Task::Array)?;
                            tasks.push(ctx, Task::Visit(element))?;
                            continue;
                        }
                        TypeKind::Hash(None) => {
                            self.hash(ctx, Atom::Unknown.fact(), Atom::Unknown.fact())?
                        }
                        TypeKind::Hash(Some(pair)) => {
                            tasks.push(ctx, Task::Hash)?;
                            tasks.push(ctx, Task::Visit(&pair.1))?;
                            tasks.push(ctx, Task::Visit(&pair.0))?;
                            continue;
                        }
                        TypeKind::Shape(fields, open) => {
                            tasks.push(ctx, Task::Shape(fields, *open))?;
                            for field in fields.iter().rev() {
                                tasks.push(ctx, Task::Visit(&field.ty))?;
                            }
                            continue;
                        }
                        TypeKind::Union(options) => {
                            tasks.push(ctx, Task::Union(options.len()))?;
                            for option in options.iter().rev() {
                                tasks.push(ctx, Task::Visit(option))?;
                            }
                            continue;
                        }
                        TypeKind::Named => {
                            ctx.work_bytes(ty.name.len())?;
                            if let Some(fact) = resolve(ctx, &ty.name)? {
                                fact
                            } else {
                                let name = ctx.bytes(ty.name.as_bytes())?;
                                self.intern(ctx, Node::Named(name))?
                            }
                        }
                    }
                }
                Task::Nullable => {
                    let value = values.data.pop().unwrap();
                    if value == Atom::Any.fact() {
                        value
                    } else {
                        self.choice(ctx, &[value, Atom::Nil.fact()])?
                    }
                }
                Task::Array => self.array(ctx, values.data.pop().unwrap())?,
                Task::Hash => {
                    let value = values.data.pop().unwrap();
                    let key = values.data.pop().unwrap();
                    self.hash(ctx, key, value)?
                }
                Task::Shape(fields, open) => {
                    let start = values.data.len() - fields.len();
                    let mut result = Buffer::with_capacity(ctx, fields.len())?;
                    for (field, &value) in fields.iter().zip(&values.data[start..]) {
                        let name = ctx.bytes(&field.name)?;
                        result.push(
                            ctx,
                            Field {
                                name,
                                value,
                                optional: field.optional,
                            },
                        )?;
                    }
                    values.data.truncate(start);
                    self.shape_fields(ctx, result, open, Atom::Unknown.fact(), false)?
                }
                Task::Union(count) => {
                    let start = values.data.len() - count;
                    let fact = self.choice(ctx, &values.data[start..])?;
                    values.data.truncate(start);
                    fact
                }
            };
            values.push(ctx, fact)?;
        }
        debug_assert_eq!(values.data.len(), 1);
        Ok(values.data[0])
    }

    pub fn split(&mut self, ctx: &mut CallContext, fact: Fact) -> Result<Option<Buffer<Fact>>> {
        if !self.has_choices(fact) {
            return Ok(None);
        }
        let mut path = Buffer::empty();
        let mut current = fact;
        let mut variants = loop {
            ctx.charge(1)?;
            match self.node(current) {
                Node::Union(arms) | Node::Choice(arms) => {
                    let mut result = Buffer::with_capacity(ctx, arms.data.len())?;
                    result.extend(ctx, &arms.data)?;
                    break result;
                }
                Node::Tuple(elements) => {
                    let mut found = None;
                    for (index, &element) in elements.data.iter().enumerate() {
                        ctx.charge(1)?;
                        if self.has_choices(element) {
                            found = Some((index, element));
                            break;
                        }
                    }
                    let (index, element) = found.unwrap();
                    path.push(ctx, (current, index))?;
                    current = element;
                }
                Node::Shape(fields, ..) => {
                    let mut found = None;
                    for (index, field) in fields.data.iter().enumerate() {
                        ctx.charge(1)?;
                        if field.optional || self.has_choices(field.value) {
                            found = Some((index, field.value, field.optional));
                            break;
                        }
                    }
                    let (index, value, optional) = found.unwrap();
                    if optional {
                        let absent = self.replace(ctx, current, index, None)?;
                        let present = self.replace(ctx, current, index, Some(value))?;
                        let mut result = Buffer::with_capacity(ctx, 2)?;
                        result.extend(ctx, &[absent, present])?;
                        break result;
                    }
                    path.push(ctx, (current, index))?;
                    current = value;
                }
                _ => unreachable!(),
            }
        };
        for &(parent, index) in path.data.iter().rev() {
            for value in &mut variants.data {
                ctx.charge(1)?;
                *value = self.replace(ctx, parent, index, Some(*value))?;
            }
        }
        Ok(Some(variants))
    }

    pub(super) fn replace(
        &mut self,
        ctx: &mut CallContext,
        parent: Fact,
        index: usize,
        replacement: Option<Fact>,
    ) -> Result<Fact> {
        let node = match self.node(parent) {
            Node::Tuple(elements) => {
                let mut values = Buffer::with_capacity(ctx, elements.data.len())?;
                values.extend(ctx, &elements.data)?;
                values.data[index] = replacement.unwrap();
                Node::Tuple(values)
            }
            Node::Shape(fields, open, keys, plain) => {
                let mut values = Buffer::with_capacity(ctx, fields.data.len())?;
                for (i, field) in fields.data.iter().enumerate() {
                    ctx.charge(1)?;
                    if i == index && replacement.is_none() {
                        continue;
                    }
                    values.push(
                        ctx,
                        Field {
                            name: field.name.clone(),
                            value: if i == index {
                                replacement.unwrap()
                            } else {
                                field.value
                            },
                            optional: i != index && field.optional,
                        },
                    )?;
                }
                Node::Shape(values, *open, *keys, *plain)
            }
            _ => unreachable!(),
        };
        self.intern(ctx, node)
    }
}

fn scalar_atom(scalar: Scalar) -> Atom {
    match scalar {
        Scalar::Any => Atom::Any,
        Scalar::Nil => Atom::Nil,
        Scalar::Bool => Atom::Bool,
        Scalar::Int => Atom::Int,
        Scalar::Float => Atom::Float,
        Scalar::String => Atom::String,
        Scalar::Symbol => Atom::Symbol,
        Scalar::Duration => Atom::Duration,
        Scalar::Time => Atom::Time,
        Scalar::Money => Atom::Money,
        Scalar::Range => Atom::Range,
        Scalar::Number => unreachable!(),
    }
}

impl Node {
    fn hash(&self, ctx: &mut CallContext, sources: &super::sources::Sources) -> Result<u64> {
        let mut hash = DefaultHasher::new();
        std::mem::discriminant(self).hash(&mut hash);
        ctx.charge(1)?;
        match self {
            Self::Atom(value) => value.hash(&mut hash),
            Self::Boolean(value) => value.hash(&mut hash),
            Self::Integer(value) => value.hash(&mut hash),
            Self::IntegerBounds(value) => value.hash(&mut hash),
            Self::Float(value) => value.hash(&mut hash),
            Self::Builtin(value) => value.name().hash(&mut hash),
            Self::Callable { owner, target } => (sources.key(ctx, *owner)?, target).hash(&mut hash),
            Self::Offset(value) => value.hash(&mut hash),
            Self::Protected(value, tag) => (value, *tag as u8).hash(&mut hash),
            Self::TypeValue(value) => value.hash(&mut hash),
            Self::Instance { class, slot, kind } => (class, slot, kind).hash(&mut hash),
            Self::Enumeration { nominal, .. } => nominal.hash(&mut hash),
            Self::EnumMember { enumeration, index } => (enumeration, index).hash(&mut hash),
            Self::Range(start, end, exclusive) => (start, end, exclusive).hash(&mut hash),
            Self::Regex(value) => {
                let crate::value::Kind::Regex(regex) = &value.0 else {
                    unreachable!()
                };
                let bytes = regex.source.as_bytes().unwrap();
                ctx.work_bytes(bytes.len())?;
                (bytes, regex.flags()).hash(&mut hash);
            }
            Self::String(value) | Self::Symbol(value) | Self::Named(value) => {
                let bytes = value.as_bytes().unwrap();
                ctx.work_bytes(bytes.len())?;
                bytes.hash(&mut hash);
            }
            Self::Array(element) => element.hash(&mut hash),
            Self::Hash(key, value, plain) => (key, value, plain).hash(&mut hash),
            Self::Tuple(values) | Self::Union(values) | Self::Choice(values) => {
                ctx.charge(values.data.len() as u64)?;
                values.data.hash(&mut hash);
            }
            Self::Shape(fields, open, keys, plain) => {
                plain.hash(&mut hash);
                open.hash(&mut hash);
                keys.hash(&mut hash);
                fields.data.len().hash(&mut hash);
                for field in &fields.data {
                    ctx.charge(1)?;
                    let name = field.name.as_bytes().unwrap();
                    ctx.work_bytes(name.len())?;
                    (name, field.value, field.optional).hash(&mut hash);
                }
            }
            Self::Nominal { identity, .. } => {
                std::mem::discriminant(identity).hash(&mut hash);
                match identity {
                    NominalId::Binding(owner, declaration) => {
                        (sources.key(ctx, *owner)?, declaration).hash(&mut hash)
                    }
                    NominalId::Enumeration(index) => index.hash(&mut hash),
                }
            }
        }
        Ok(hash.finish())
    }

    fn same(&self, ctx: &mut CallContext, other: &Self) -> Result<bool> {
        ctx.charge(1)?;
        Ok(match (self, other) {
            (Self::Atom(a), Self::Atom(b)) => a == b,
            (Self::Boolean(a), Self::Boolean(b)) => a == b,
            (Self::Integer(a), Self::Integer(b)) => a == b,
            (Self::IntegerBounds(a), Self::IntegerBounds(b)) => a == b,
            (Self::Float(a), Self::Float(b)) => a == b,
            (Self::Builtin(a), Self::Builtin(b)) => a == b,
            (
                Self::Callable {
                    owner: a,
                    target: at,
                },
                Self::Callable {
                    owner: b,
                    target: bt,
                },
            ) => a == b && at == bt,
            (Self::Offset(a), Self::Offset(b)) => a == b,
            (Self::Protected(a, at), Self::Protected(b, bt)) => a == b && at == bt,
            (Self::TypeValue(a), Self::TypeValue(b)) => a == b,
            (
                Self::Instance {
                    class: a,
                    slot: ai,
                    kind: ak,
                },
                Self::Instance {
                    class: b,
                    slot: bi,
                    kind: bk,
                },
            ) => a == b && ai == bi && ak == bk,
            (Self::Enumeration { nominal: a, .. }, Self::Enumeration { nominal: b, .. }) => a == b,
            (
                Self::EnumMember {
                    enumeration: a,
                    index: ai,
                },
                Self::EnumMember {
                    enumeration: b,
                    index: bi,
                },
            ) => a == b && ai == bi,
            (Self::Range(a, b, c), Self::Range(x, y, z)) => a == x && b == y && c == z,
            (Self::Regex(a), Self::Regex(b)) => {
                let (crate::value::Kind::Regex(a), crate::value::Kind::Regex(b)) = (&a.0, &b.0)
                else {
                    unreachable!()
                };
                a.equal(ctx, b)?
            }
            (Self::String(a), Self::String(b))
            | (Self::Symbol(a), Self::Symbol(b))
            | (Self::Named(a), Self::Named(b)) => same_bytes(ctx, a, b)?,
            (Self::Array(a), Self::Array(b)) => a == b,
            (Self::Hash(ak, av, ap), Self::Hash(bk, bv, bp)) => ak == bk && av == bv && ap == bp,
            (Self::Tuple(a), Self::Tuple(b))
            | (Self::Union(a), Self::Union(b))
            | (Self::Choice(a), Self::Choice(b)) => {
                ctx.charge(a.data.len().min(b.data.len()) as u64)?;
                a.data == b.data
            }
            (Self::Shape(a, ao, ak, ap), Self::Shape(b, bo, bk, bp)) => {
                if ao != bo || ak != bk || ap != bp || a.data.len() != b.data.len() {
                    return Ok(false);
                }
                for (a, b) in a.data.iter().zip(&b.data) {
                    if a.value != b.value
                        || a.optional != b.optional
                        || !same_bytes(ctx, &a.name, &b.name)?
                    {
                        return Ok(false);
                    }
                }
                true
            }
            (Self::Nominal { identity: a, .. }, Self::Nominal { identity: b, .. }) => a == b,
            _ => false,
        })
    }
}

pub(super) fn same_bytes(ctx: &mut CallContext, a: &Value, b: &Value) -> Result<bool> {
    let (a, b) = (a.as_bytes().unwrap(), b.as_bytes().unwrap());
    ctx.work_bytes(a.len().min(b.len()))?;
    Ok(a == b)
}
