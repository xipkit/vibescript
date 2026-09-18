use super::{
    facts::{Atom, Fact, Facts, Node, same_bytes},
    scalar::Operation,
};
use crate::{CallContext, Result, budget::Buffer, bytecode::Method};
use std::hash::{Hash, Hasher};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum Attached {
    No,
    Maybe,
    Yes,
}

impl Attached {
    fn or(self, other: Self) -> Self {
        match (self, other) {
            (Self::Yes, _) | (_, Self::Yes) => Self::Yes,
            (Self::Maybe, _) | (_, Self::Maybe) => Self::Maybe,
            _ => Self::No,
        }
    }
    fn and(self, other: Self) -> Self {
        match (self, other) {
            (Self::No, _) | (_, Self::No) => Self::No,
            (Self::Yes, Self::Yes) => Self::Yes,
            _ => Self::Maybe,
        }
    }

    fn join(self, other: Self) -> Self {
        if self == other { self } else { Self::Maybe }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct Hop {
    container: Fact,
    key: Fact,
}

#[derive(Debug)]
pub(super) struct Address {
    protected: Attached,
    pub root: Option<usize>,
    pub attached: Attached,
    pub value: Fact,
    pub selectors: Buffer<Fact>,
    pub supported: bool,
    path: Buffer<Hop>,
}

pub(super) enum Change<'a> {
    Store {
        same: bool,
        fresh: bool,
    },
    Mutation {
        address: &'a Address,
        method: Option<Method>,
        args: &'a [Fact],
        fresh: bool,
    },
}

impl Address {
    pub fn hash(&self, ctx: &mut CallContext, hash: &mut impl Hasher) -> Result<()> {
        ctx.charge((self.path.data.len() + self.selectors.data.len()) as u64 + 1)?;
        self.protected.hash(hash);
        self.root.hash(hash);
        self.attached.hash(hash);
        self.value.hash(hash);
        self.supported.hash(hash);
        self.path.data.hash(hash);
        self.selectors.data.hash(hash);
        Ok(())
    }

    pub fn equal(&self, ctx: &mut CallContext, other: &Self) -> Result<bool> {
        ctx.charge((self.path.data.len() + self.selectors.data.len()) as u64 + 1)?;
        Ok(self.protected == other.protected
            && self.root == other.root
            && self.attached == other.attached
            && self.value == other.value
            && self.supported == other.supported
            && self.path.data == other.path.data
            && self.selectors.data == other.selectors.data)
    }

    pub fn compatible(&self, other: &Self) -> bool {
        self.root == other.root
            && self.path.data.len() == other.path.data.len()
            && self.selectors.data.len() == other.selectors.data.len()
    }

    pub fn origin(&self) -> Option<usize> {
        if self.attached == Attached::Yes && self.path.data.is_empty() {
            self.root
        } else {
            None
        }
    }

    pub fn new(root: Option<usize>, value: Fact) -> Self {
        Self {
            protected: Attached::No,
            root,
            value,
            attached: if root.is_some() {
                Attached::Yes
            } else {
                Attached::No
            },
            selectors: Buffer::empty(),
            supported: true,
            path: Buffer::empty(),
        }
    }

    pub fn snapshot(&self, ctx: &mut CallContext) -> Result<Self> {
        let mut address = Self::new(self.root, self.value);
        address.attached = self.attached;
        address.protected = self.protected;
        address.supported = self.supported;
        address.path.extend(ctx, &self.path.data)?;
        address.selectors.extend(ctx, &self.selectors.data)?;
        Ok(address)
    }

    pub fn join(
        &mut self,
        ctx: &mut CallContext,
        facts: &mut Facts,
        other: &Self,
        depth: Option<usize>,
    ) -> Result<bool> {
        let mut changed = false;
        let next = facts.joined(ctx, self.value, other.value, depth)?;
        changed |= next != self.value;
        self.value = next;
        let protected = self.protected.join(other.protected);
        changed |= protected != self.protected;
        self.protected = protected;
        let attached = self.attached.join(other.attached);
        changed |= attached != self.attached;
        self.attached = attached;
        if self.root != other.root {
            match (self.root, other.root) {
                (None, Some(root)) => {
                    self.root = Some(root);
                    changed = true;
                }
                (Some(_), None) => (),
                _ => {
                    changed |= self.supported;
                    self.supported = false;
                }
            }
        }
        if self.path.data.len() != other.path.data.len()
            || self.selectors.data.len() != other.selectors.data.len()
        {
            changed |= self.supported;
            self.supported = false;
            return Ok(changed);
        }
        changed |= self.supported && !other.supported;
        self.supported &= other.supported;
        for (a, b) in self.path.data.iter_mut().zip(&other.path.data) {
            ctx.charge(1)?;
            let next = Hop {
                container: facts.joined(ctx, a.container, b.container, depth)?,
                key: facts.joined(ctx, a.key, b.key, depth)?,
            };
            changed |= *a != next;
            *a = next;
        }
        for (a, b) in self.selectors.data.iter_mut().zip(&other.selectors.data) {
            ctx.charge(1)?;
            let next = facts.joined(ctx, *a, *b, depth)?;
            changed |= *a != next;
            *a = next;
        }
        Ok(changed)
    }

    pub fn index(
        &mut self,
        ctx: &mut CallContext,
        facts: &mut Facts,
        args: &[Fact],
    ) -> Result<Operation> {
        self.protected = self.protection(ctx, facts)?;
        let result = facts.collection_index(ctx, self.value, args)?;
        if let [key] = args {
            let stored = stored(ctx, facts, self.value, *key)?;
            let key = captured(ctx, facts, self.value, *key)?;
            self.path.push(
                ctx,
                Hop {
                    container: self.value,
                    key,
                },
            )?;
            self.attached = self.attached.and(stored);
        } else {
            self.attached = Attached::No;
            self.path.data.clear();
        }
        self.value = result.value;
        Ok(result)
    }

    pub fn protection(&self, ctx: &mut CallContext, facts: &Facts) -> Result<Attached> {
        let mut protected = None;
        for i in 0..facts.arm_count(self.value) {
            ctx.charge(1)?;
            let value = facts.arm(self.value, i);
            if value == Atom::Never.fact() {
                continue;
            }
            let next = if matches!(facts.node(value), Node::Protected(..)) {
                Attached::Yes
            } else {
                Attached::No
            };
            protected = Some(protected.map_or(next, |previous: Attached| previous.join(next)));
        }
        Ok(self.protected.or(protected.unwrap_or(Attached::No)))
    }

    pub fn target(
        &mut self,
        ctx: &mut CallContext,
        facts: &mut Facts,
        args: &[Fact],
        read: bool,
    ) -> Result<Option<Operation>> {
        self.selectors.extend(ctx, args)?;
        if let [key] = self.selectors.data.as_mut_slice() {
            *key = captured(ctx, facts, self.value, *key)?;
        }
        if read {
            facts.collection_index(ctx, self.value, args).map(Some)
        } else {
            Ok(None)
        }
    }

    pub fn rebuild(
        &self,
        ctx: &mut CallContext,
        facts: &mut Facts,
        mut value: Fact,
    ) -> Result<Operation> {
        let mut unsupported = !self.supported;
        for hop in self.path.data.iter().rev() {
            ctx.charge(1)?;
            let next = facts.collection_write(ctx, hop.container, hop.key, value)?;
            value = next.receiver;
            unsupported |= next.unsupported;
        }
        Ok(Operation {
            value,
            rejected: false,
            unsupported,
        })
    }

    pub fn refresh(
        &mut self,
        ctx: &mut CallContext,
        facts: &mut Facts,
        root: Fact,
        change: &Change<'_>,
    ) -> Result<()> {
        if self.attached == Attached::No {
            return Ok(());
        }
        let retention = match change {
            Change::Store { same: true, .. } => Attached::Yes,
            Change::Store { fresh: true, .. } => Attached::No,
            Change::Store { .. } => Attached::Maybe,
            Change::Mutation {
                address,
                method,
                args,
                fresh,
            } => self.retention(ctx, facts, address, *method, args, *fresh)?,
        };
        if retention == Attached::No {
            self.attached = Attached::No;
            return Ok(());
        }
        let mut selected = root;
        let mut updated = Buffer::empty();
        let mut attached = self.attached.and(retention);
        for hop in &self.path.data {
            ctx.charge(1)?;
            attached = attached.and(stored(ctx, facts, selected, hop.key)?);
            if attached == Attached::No {
                self.attached = Attached::No;
                return Ok(());
            }
            let value = facts.collection_index(ctx, selected, &[hop.key])?;
            self.supported &= !value.unsupported;
            updated.push(
                ctx,
                Hop {
                    container: selected,
                    key: hop.key,
                },
            )?;
            selected = value.value;
        }
        if attached == Attached::Yes {
            self.value = selected;
            self.path = updated;
        } else {
            selected = compatible_storage(ctx, facts, self.value, selected)?;
            if selected == Atom::Never.fact() {
                self.attached = Attached::No;
                return Ok(());
            }
            self.value = facts.union(ctx, &[self.value, selected])?;
            for (hop, updated) in self.path.data.iter_mut().zip(&updated.data) {
                ctx.charge(1)?;
                hop.container = facts.union(ctx, &[hop.container, updated.container])?;
            }
        }
        self.attached = attached;
        Ok(())
    }

    fn retention(
        &self,
        ctx: &mut CallContext,
        facts: &Facts,
        changed: &Self,
        method: Option<Method>,
        args: &[Fact],
        fresh: bool,
    ) -> Result<Attached> {
        if changed.attached != Attached::Yes {
            return Ok(Attached::Maybe);
        }
        for (a, b) in self.path.data.iter().zip(&changed.path.data) {
            ctx.charge(1)?;
            match same_key(ctx, facts, a.key, b.key)? {
                Some(true) => (),
                Some(false) => return Ok(Attached::Yes),
                None => return Ok(Attached::Maybe),
            }
        }
        if self.path.data.len() <= changed.path.data.len() {
            return Ok(Attached::Yes);
        }
        let key = self.path.data[changed.path.data.len()].key;
        let selectors = if method.is_some() {
            args
        } else {
            &changed.selectors.data
        };
        if fresh
            && matches!(method, None | Some(Method::Store))
            && !selectors.is_empty()
            && same_key(ctx, facts, selectors[0], key)? == Some(true)
        {
            return Ok(Attached::No);
        }
        child_retained(ctx, facts, changed.value, key, method, selectors)
    }
}

fn captured(
    ctx: &mut CallContext,
    facts: &mut Facts,
    receiver: Fact,
    selector: Fact,
) -> Result<Fact> {
    let mut keys = Buffer::empty();
    for i in 0..facts.arm_count(receiver) {
        for j in 0..facts.arm_count(selector) {
            ctx.charge(1)?;
            let receiver = facts.arm(receiver, i);
            let selector = facts.arm(selector, j);
            let value = match (facts.node(receiver), facts.node(selector)) {
                (Node::Tuple(values), Node::Integer(index)) => {
                    let at = if *index < 0 {
                        *index as i128 + values.data.len() as i128
                    } else {
                        *index as i128
                    };
                    if at >= 0 && at < values.data.len() as i128 {
                        facts.integer(ctx, at as i64)?
                    } else {
                        selector
                    }
                }
                (Node::Array(_), Node::Integer(index)) if *index < 0 => Atom::Int.fact(),
                _ => selector,
            };
            keys.push(ctx, value)?;
        }
    }
    facts.union(ctx, &keys.data)
}

fn stored(
    ctx: &mut CallContext,
    facts: &Facts,
    receiver: Fact,
    selector: Fact,
) -> Result<Attached> {
    let mut result = None;
    for i in 0..facts.arm_count(receiver) {
        for j in 0..facts.arm_count(selector) {
            ctx.charge(1)?;
            let receiver = facts.arm(receiver, i);
            let selector = facts.arm(selector, j);
            let next = match (facts.node(receiver), facts.node(selector)) {
                (Node::Tuple(values), _) if values.data.is_empty() => Attached::No,
                (Node::Array(element), _) if *element == Atom::Never.fact() => Attached::No,
                (Node::Tuple(values), Node::Integer(index)) => {
                    let at = if *index < 0 {
                        *index as i128 + values.data.len() as i128
                    } else {
                        *index as i128
                    };
                    if at >= 0 && at < values.data.len() as i128 {
                        Attached::Yes
                    } else {
                        Attached::No
                    }
                }
                (Node::Array(_) | Node::Tuple(_), Node::Atom(Atom::Range) | Node::Range(..)) => {
                    Attached::No
                }
                (Node::Array(_) | Node::Tuple(_), _) => Attached::Maybe,
                (Node::Shape(fields, open, _, _), Node::String(key) | Node::Symbol(key)) => {
                    let mut found = None;
                    for field in &fields.data {
                        ctx.charge(1)?;
                        if same_bytes(ctx, &field.name, key)? {
                            found = Some(!field.optional);
                            break;
                        }
                    }
                    match found {
                        Some(true) => Attached::Yes,
                        Some(false) => Attached::Maybe,
                        None if *open => Attached::Maybe,
                        None => Attached::No,
                    }
                }
                (Node::Hash(..) | Node::Shape(..) | Node::Atom(Atom::Unknown | Atom::Any), _) => {
                    Attached::Maybe
                }
                _ => Attached::No,
            };
            result = Some(result.map_or(next, |old: Attached| old.join(next)));
        }
    }
    Ok(result.unwrap_or(Attached::No))
}

fn same_key(ctx: &mut CallContext, facts: &Facts, a: Fact, b: Fact) -> Result<Option<bool>> {
    Ok(match (facts.node(a), facts.node(b)) {
        (Node::Integer(a), Node::Integer(b)) => Some(a == b),
        (Node::String(a) | Node::Symbol(a), Node::String(b) | Node::Symbol(b)) => {
            Some(same_bytes(ctx, a, b)?)
        }
        (Node::Integer(_), Node::String(_) | Node::Symbol(_))
        | (Node::String(_) | Node::Symbol(_), Node::Integer(_)) => Some(false),
        _ => None,
    })
}

fn compatible_storage(
    ctx: &mut CallContext,
    facts: &mut Facts,
    before: Fact,
    after: Fact,
) -> Result<Fact> {
    let family = |facts: &Facts, value| match facts.node(value) {
        Node::Array(_) | Node::Tuple(_) => Some(100),
        Node::Hash(..) | Node::Shape(..) => Some(101),
        _ => facts
            .atom(value)
            .filter(|atom| !matches!(atom, Atom::Unknown | Atom::Any))
            .map(|atom| atom as usize),
    };
    let mut possible = Buffer::empty();
    for i in 0..facts.arm_count(after) {
        let candidate = facts.arm(after, i);
        for j in 0..facts.arm_count(before) {
            ctx.charge(1)?;
            let (a, b) = (
                family(facts, facts.arm(before, j)),
                family(facts, candidate),
            );
            if a.is_none() || b.is_none() || a == b {
                possible.push(ctx, candidate)?;
                break;
            }
        }
    }
    facts.union(ctx, &possible.data)
}

fn child_retained(
    ctx: &mut CallContext,
    facts: &Facts,
    receiver: Fact,
    key: Fact,
    method: Option<Method>,
    args: &[Fact],
) -> Result<Attached> {
    let mut result = None;
    for i in 0..facts.arm_count(receiver) {
        ctx.charge(1)?;
        let receiver = facts.arm(receiver, i);
        let array = matches!(facts.node(receiver), Node::Tuple(_) | Node::Array(_));
        let hash = matches!(facts.node(receiver), Node::Hash(..) | Node::Shape(..));
        let same = |ctx: &mut CallContext| -> Result<Option<bool>> {
            args.first()
                .map_or(Ok(None), |&target| same_key(ctx, facts, target, key))
        };
        let next = if facts.atom(receiver) == Some(Atom::String) {
            Attached::Yes
        } else {
            match method {
                Some(Method::Push) if array => Attached::Yes,
                Some(Method::Clear) if array || hash => Attached::No,
                Some(Method::Prepend) if array && args.is_empty() => Attached::Yes,
                Some(Method::Insert) if array && args.len() == 1 => Attached::Yes,
                Some(Method::Store) | None if (array || hash) && same(ctx)? == Some(false) => {
                    Attached::Yes
                }
                Some(Method::Delete) if hash => match same(ctx)? {
                    Some(true) => Attached::No,
                    Some(false) => Attached::Yes,
                    None => Attached::Maybe,
                },
                Some(Method::Pop | Method::Shift) if array => {
                    let count = match args.first().map(|&v| facts.node(v)) {
                        None => Some(1),
                        Some(Node::Integer(n)) if *n >= 0 => Some(*n as i128),
                        _ => None,
                    };
                    if count == Some(0) {
                        Attached::Yes
                    } else if let (Some(count), Node::Tuple(values), Node::Integer(index)) =
                        (count, facts.node(receiver), facts.node(key))
                    {
                        let remaining = (values.data.len() as i128 - count).max(0);
                        if *index as i128 >= remaining {
                            Attached::No
                        } else if matches!(method, Some(Method::Pop)) {
                            Attached::Yes
                        } else {
                            Attached::Maybe
                        }
                    } else {
                        Attached::Maybe
                    }
                }
                Some(Method::Insert) if array && !args.is_empty() => {
                    if let (Node::Integer(at), Node::Integer(index)) =
                        (facts.node(args[0]), facts.node(key))
                    {
                        let at = if *at >= 0 {
                            Some(*at as i128)
                        } else if let Node::Tuple(values) = facts.node(receiver) {
                            Some(*at as i128 + values.data.len() as i128 + 1)
                        } else {
                            None
                        };
                        if at.is_some_and(|at| (*index as i128) < at) {
                            Attached::Yes
                        } else {
                            Attached::Maybe
                        }
                    } else {
                        Attached::Maybe
                    }
                }
                _ => Attached::Maybe,
            }
        };
        result = Some(result.map_or(next, |old: Attached| old.join(next)));
    }
    Ok(result.unwrap_or(Attached::Maybe))
}
