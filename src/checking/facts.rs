use crate::{
    CallContext, Result, Value,
    budget::Buffer,
    types::{Scalar, Type, TypeKind},
};
use std::hash::{DefaultHasher, Hash, Hasher};

const EMPTY: usize = usize::MAX;

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

#[derive(Debug)]
pub(super) enum Node {
    Atom(Atom),
    Boolean(bool),
    Integer(i64),
    String(Value),
    Symbol(Value),
    Array(Fact),
    Tuple(Buffer<Fact>),
    // The last flag identifies ordinary hashes; annotations may describe objects too.
    Hash(Fact, Fact, bool),
    Shape(Buffer<Field>, bool, Fact, bool),
    Union(Buffer<Fact>),
    Named(Value),
    Nominal {
        owner: usize,
        declaration: usize,
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
}

#[derive(Debug)]
pub(super) struct Facts {
    entries: Buffer<Entry>,
    buckets: Buffer<usize>,
    max_depth: usize,
}

impl Facts {
    pub fn new(ctx: &mut CallContext) -> Result<Self> {
        let mut facts = Self {
            entries: Buffer::empty(),
            buckets: Buffer::empty(),
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
        ] {
            let fact = facts.intern(ctx, Node::Atom(atom))?;
            debug_assert_eq!(fact, atom.fact());
        }
        Ok(facts)
    }

    pub fn node(&self, fact: Fact) -> &Node {
        &self.entries.data[fact.0].node
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
        let hash = node.hash(ctx)?;
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
            Node::Union(_) => true,
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
            Node::Union(arms) => {
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
            Node::Named(_) => true,
            Node::Array(element) => self.unresolved(*element),
            Node::Hash(key, value, _) => self.unresolved(*key) || self.unresolved(*value),
            Node::Tuple(values) | Node::Union(values) => {
                ctx.charge(values.data.len() as u64)?;
                values.data.iter().any(|&value| self.unresolved(value))
            }
            Node::Shape(fields, ..) => {
                ctx.charge(fields.data.len() as u64)?;
                fields.data.iter().any(|field| self.unresolved(field.value))
            }
            _ => false,
        };
        let singleton = match &node {
            Node::Atom(Atom::Nil)
            | Node::Boolean(_)
            | Node::Integer(_)
            | Node::String(_)
            | Node::Symbol(_) => true,
            Node::Tuple(values) => {
                ctx.charge(values.data.len() as u64)?;
                values.data.iter().all(|&value| self.singleton(value))
            }
            Node::Shape(fields, false, keys, true) if *keys == Atom::String.fact() => {
                ctx.charge(fields.data.len() as u64)?;
                fields
                    .data
                    .iter()
                    .all(|field| !field.optional && self.singleton(field.value))
            }
            _ => false,
        };
        let depth = match &node {
            Node::Array(element) => self.depth(*element).saturating_add(1),
            Node::Hash(key, value, _) => self.depth(*key).max(self.depth(*value)).saturating_add(1),
            Node::Tuple(values) | Node::Union(values) => {
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
        plain: bool,
    ) -> Result<Fact> {
        self.intern(ctx, Node::Hash(key, value, plain))
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
        plain: bool,
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
        self.intern(ctx, Node::Shape(fields, open, keys, plain))
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
                owner,
                declaration,
                name,
                symbols,
            },
        )
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
        let mut strings = false;
        let mut any = false;
        for &fact in &arms.data {
            ctx.charge(1)?;
            match self.node(fact) {
                Node::Atom(Atom::Bool) => bools = 3,
                Node::Boolean(false) => bools |= 1,
                Node::Boolean(true) => bools |= 2,
                Node::Atom(Atom::Symbol) => symbols = true,
                Node::Atom(Atom::Int) => integers = true,
                Node::Atom(Atom::String) => strings = true,
                Node::Atom(Atom::Any) => any = true,
                _ => (),
            }
        }
        ctx.charge(arms.data.len() as u64)?;
        arms.data.retain(|&fact| match self.node(fact) {
            Node::Boolean(_) => bools != 3,
            Node::Symbol(_) => !symbols,
            Node::Integer(_) => !integers,
            Node::String(_) => !strings,
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
                    self.union(ctx, &[value, Atom::Nil.fact()])?
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
                    let fact = self.union(ctx, &values.data[start..])?;
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
                Node::Union(arms) => {
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

    fn replace(
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
    fn hash(&self, ctx: &mut CallContext) -> Result<u64> {
        let mut hash = DefaultHasher::new();
        std::mem::discriminant(self).hash(&mut hash);
        ctx.charge(1)?;
        match self {
            Self::Atom(value) => value.hash(&mut hash),
            Self::Boolean(value) => value.hash(&mut hash),
            Self::Integer(value) => value.hash(&mut hash),
            Self::String(value) | Self::Symbol(value) | Self::Named(value) => {
                let bytes = value.as_bytes().unwrap();
                ctx.work_bytes(bytes.len())?;
                bytes.hash(&mut hash);
            }
            Self::Array(element) => element.hash(&mut hash),
            Self::Hash(key, value, plain) => (key, value, plain).hash(&mut hash),
            Self::Tuple(values) | Self::Union(values) => {
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
            Self::Nominal {
                owner, declaration, ..
            } => (owner, declaration).hash(&mut hash),
        }
        Ok(hash.finish())
    }

    fn same(&self, ctx: &mut CallContext, other: &Self) -> Result<bool> {
        ctx.charge(1)?;
        Ok(match (self, other) {
            (Self::Atom(a), Self::Atom(b)) => a == b,
            (Self::Boolean(a), Self::Boolean(b)) => a == b,
            (Self::Integer(a), Self::Integer(b)) => a == b,
            (Self::String(a), Self::String(b))
            | (Self::Symbol(a), Self::Symbol(b))
            | (Self::Named(a), Self::Named(b)) => same_bytes(ctx, a, b)?,
            (Self::Array(a), Self::Array(b)) => a == b,
            (Self::Hash(ak, av, ap), Self::Hash(bk, bv, bp)) => ak == bk && av == bv && ap == bp,
            (Self::Tuple(a), Self::Tuple(b)) | (Self::Union(a), Self::Union(b)) => {
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
            (
                Self::Nominal {
                    owner: ao,
                    declaration: ad,
                    ..
                },
                Self::Nominal {
                    owner: bo,
                    declaration: bd,
                    ..
                },
            ) => ao == bo && ad == bd,
            _ => false,
        })
    }
}

pub(super) fn same_bytes(ctx: &mut CallContext, a: &Value, b: &Value) -> Result<bool> {
    let (a, b) = (a.as_bytes().unwrap(), b.as_bytes().unwrap());
    ctx.work_bytes(a.len().min(b.len()))?;
    Ok(a == b)
}
