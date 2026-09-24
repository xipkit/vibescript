use super::facts::{Atom, Callable, Fact, Facts, Node};
use super::slots::Slots;
use crate::{CallContext, Result, Value, budget::Buffer};

const NO: u8 = 1;
const YES: u8 = 2;
const MAYBE: u8 = NO | YES;
const LIMIT: u8 = 4;

/// Selects which runtime comparison a structural equality solve models.
///
/// Every solve uses one policy for its whole graph walk, so memo keys only need
/// the pair, the root flag and the depth to stay exact.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Policy {
    /// `==` and `!=`: numeric kinds compare by value at every depth and NaN never matches.
    Value,
    /// Set membership: root numeric kinds must match, root NaN matches itself, and
    /// abstract graphs are walked without a depth cutoff.
    Set,
    /// `eql?`: every visited pair must share a runtime type and NaN never matches.
    Strict,
    /// `equal?`: the root compares by identity (runtime type, NaN reflexivity, big
    /// integer payloads, nominal enum identity) and nested values use ordinary equality.
    Identity,
}

impl Policy {
    fn set(self) -> bool {
        self == Self::Set
    }

    /// Whether a pair at this position must share a runtime type to compare equal.
    fn typed(self, root: bool) -> bool {
        match self {
            Self::Value => false,
            Self::Strict => true,
            Self::Set | Self::Identity => root,
        }
    }

    /// Whether NaN compares equal to itself at this position.
    fn nan_reflexive(self, root: bool) -> bool {
        root && matches!(self, Self::Set | Self::Identity)
    }
}

/// A runtime type class that is known to differ from every other class under
/// every comparison policy. Classes are finer than runtime type names where the
/// runtime never treats members of two classes as equal.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Class {
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
    Array,
    Hash,
    Instance,
    Enumeration,
    EnumMember,
    Builtin,
    Offset,
    Function,
    Host,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Pair {
    left: Fact,
    right: Fact,
    root: bool,
    depth: usize,
}

impl Pair {
    fn bucket(self, mask: usize) -> usize {
        self.left
            .0
            .wrapping_mul(0x9e3779b1)
            .wrapping_add(self.right.0.wrapping_mul(0x85ebca77))
            .wrapping_add(usize::from(self.root))
            .wrapping_add(self.depth.wrapping_mul(0xc2b2ae35))
            & mask
    }
}

struct Entry {
    pair: Pair,
    value: u8,
    next: usize,
}

struct Memo {
    entries: Buffer<Entry>,
    buckets: Buffer<usize>,
}

impl Memo {
    fn new() -> Self {
        Self {
            entries: Buffer::empty(),
            buckets: Buffer::empty(),
        }
    }

    fn get(&self, ctx: &mut CallContext, pair: Pair) -> Result<Option<u8>> {
        ctx.charge(1)?;
        if self.buckets.data.is_empty() {
            return Ok(None);
        }
        let mut index = self.buckets.data[pair.bucket(self.buckets.data.len() - 1)];
        while index != usize::MAX {
            ctx.charge(1)?;
            let entry = &self.entries.data[index];
            if entry.pair == pair {
                return Ok(Some(entry.value));
            }
            index = entry.next;
        }
        Ok(None)
    }

    fn insert(&mut self, ctx: &mut CallContext, pair: Pair, value: u8) -> Result<()> {
        if self.entries.data.len() >= self.buckets.data.len() / 2 {
            let Some(capacity) = self.buckets.data.len().max(8).checked_mul(2) else {
                return ctx.fail(
                    crate::ErrorKind::Memory,
                    "checker equality table size overflow",
                );
            };
            let mut buckets = Buffer::with_capacity(ctx, capacity)?;
            ctx.charge(capacity as u64)?;
            buckets.data.resize(capacity, usize::MAX);
            ctx.charge(self.entries.data.len() as u64)?;
            for (index, entry) in self.entries.data.iter_mut().enumerate() {
                let bucket = entry.pair.bucket(capacity - 1);
                entry.next = buckets.data[bucket];
                buckets.data[bucket] = index;
            }
            self.buckets = buckets;
        }
        let bucket = pair.bucket(self.buckets.data.len() - 1);
        let index = self.entries.data.len();
        self.entries.push(
            ctx,
            Entry {
                pair,
                value,
                next: self.buckets.data[bucket],
            },
        )?;
        self.buckets.data[bucket] = index;
        Ok(())
    }
}

enum Task {
    Visit(Pair),
    Save(Pair),
    All(usize, bool),
    Hash(usize, bool, bool),
    Alternatives(usize),
}

impl Facts {
    pub(super) fn set_equal(
        &mut self,
        ctx: &mut CallContext,
        left: Fact,
        right: Fact,
    ) -> Result<Fact> {
        let value = self.compare_values(ctx, left, right, Policy::Set)?;
        self.equality_fact(ctx, value)
    }

    pub(super) fn value_equal(
        &mut self,
        ctx: &mut CallContext,
        left: Fact,
        right: Fact,
    ) -> Result<(Fact, bool)> {
        self.policy_equal(ctx, left, right, Policy::Value)
    }

    /// Models the native `eql?` (strict) or `equal?` (identity) helper.
    ///
    /// Returns the boolean result fact and whether some path can reach the native
    /// value-depth guard. Neither helper ever invokes user methods.
    pub(super) fn helper_equal(
        &mut self,
        ctx: &mut CallContext,
        left: Fact,
        right: Fact,
        strict: bool,
    ) -> Result<(Fact, bool)> {
        let policy = if strict {
            Policy::Strict
        } else {
            Policy::Identity
        };
        self.policy_equal(ctx, left, right, policy)
    }

    /// Compares two facts under an explicit policy, reporting possible depth guards.
    pub(super) fn policy_equal(
        &mut self,
        ctx: &mut CallContext,
        left: Fact,
        right: Fact,
        policy: Policy,
    ) -> Result<(Fact, bool)> {
        let value = self.compare_values(ctx, left, right, policy)?;
        Ok((self.equality_fact(ctx, value)?, value & LIMIT != 0))
    }

    fn equality_fact(&mut self, ctx: &mut CallContext, value: u8) -> Result<Fact> {
        match value & MAYBE {
            0 => Ok(Atom::Never.fact()),
            NO => self.boolean(ctx, false),
            YES => self.boolean(ctx, true),
            _ => Ok(Atom::Bool.fact()),
        }
    }

    fn compare_values(
        &mut self,
        ctx: &mut CallContext,
        left: Fact,
        right: Fact,
        policy: Policy,
    ) -> Result<u8> {
        ctx.checkpoint()?;
        let set = policy.set();
        let mut tasks = Buffer::empty();
        let mut values = Buffer::empty();
        let mut memo = Memo::new();
        tasks.push(
            ctx,
            Task::Visit(Pair {
                left,
                right,
                root: true,
                depth: 0,
            }),
        )?;
        while let Some(task) = tasks.data.pop() {
            ctx.charge(1)?;
            let value = match task {
                Task::Save(pair) => {
                    memo.insert(ctx, pair, *values.data.last().unwrap())?;
                    continue;
                }
                Task::All(count, _) | Task::Hash(count, ..) | Task::Alternatives(count) => {
                    let start = values.data.len() - count;
                    let all = !matches!(task, Task::Alternatives(_));
                    let mut result = if all { YES } else { 0 };
                    for &value in &values.data[start..] {
                        ctx.charge(1)?;
                        result = if !all {
                            result | value
                        } else if matches!(task, Task::All(_, true)) {
                            // Array comparisons stop before later elements after a mismatch or guard.
                            (result & (NO | LIMIT)) | if result & YES != 0 { value } else { 0 }
                        } else if result == 0 || value == 0 {
                            0
                        } else {
                            ((result | value) & (NO | LIMIT)) | ((result & value) & YES)
                        };
                    }
                    values.data.truncate(start);
                    if let Task::Hash(_, missing, uncertain_kind) = task {
                        // Shapes sort keys, so retain both possible outcomes when insertion order
                        // determines whether a missing/different value or a nesting guard is first.
                        if missing {
                            result = (result & LIMIT) | NO;
                        }
                        if uncertain_kind {
                            result |= NO;
                        }
                    }
                    result
                }
                Task::Visit(pair) => {
                    if !set && pair.depth > crate::budget::MAX_VALUE_DEPTH {
                        values.push(ctx, LIMIT)?;
                        continue;
                    }
                    if let Some(value) = memo.get(ctx, pair)? {
                        values.push(ctx, value)?;
                        continue;
                    }
                    tasks.push(ctx, Task::Save(pair))?;
                    let Pair {
                        left,
                        right,
                        root,
                        depth,
                    } = pair;
                    let child_depth = if set { 0 } else { depth + 1 };
                    let hash_kind = |value| match self.node(value) {
                        Node::Hash(_, _, kind) | Node::Shape(_, _, _, kind) => Some(*kind),
                        _ => None,
                    };
                    if !set {
                        if let (Some(a), Some(b)) = (hash_kind(left), hash_kind(right)) {
                            if !a.overlaps(b) {
                                values.push(ctx, NO)?;
                                continue;
                            }
                        }
                    }
                    match (self.node(left), self.node(right)) {
                        (Node::Protected(value, ..), _) => {
                            tasks.push(
                                ctx,
                                Task::Visit(Pair {
                                    left: *value,
                                    ..pair
                                }),
                            )?;
                            continue;
                        }
                        (_, Node::Protected(value, ..)) => {
                            tasks.push(
                                ctx,
                                Task::Visit(Pair {
                                    right: *value,
                                    ..pair
                                }),
                            )?;
                            continue;
                        }
                        (Node::Union(arms), _) => {
                            tasks.push(ctx, Task::Alternatives(arms.data.len()))?;
                            for &arm in &arms.data {
                                ctx.charge(1)?;
                                tasks.push(ctx, Task::Visit(Pair { left: arm, ..pair }))?;
                            }
                            continue;
                        }
                        (_, Node::Union(arms)) => {
                            tasks.push(ctx, Task::Alternatives(arms.data.len()))?;
                            for &arm in &arms.data {
                                ctx.charge(1)?;
                                tasks.push(ctx, Task::Visit(Pair { right: arm, ..pair }))?;
                            }
                            continue;
                        }
                        (Node::Tuple(a), Node::Tuple(b)) => {
                            if a.data.len() != b.data.len() {
                                NO
                            } else {
                                tasks.push(ctx, Task::All(a.data.len(), !set))?;
                                for (&left, &right) in a.data.iter().zip(&b.data).rev() {
                                    ctx.charge(1)?;
                                    tasks.push(
                                        ctx,
                                        Task::Visit(Pair {
                                            left,
                                            right,
                                            root: false,
                                            depth: child_depth,
                                        }),
                                    )?;
                                }
                                continue;
                            }
                        }
                        (Node::Shape(a, false, _, ak), Node::Shape(b, false, _, bk))
                            if !set || (ak.plain() && bk.plain()) =>
                        {
                            let mut required = true;
                            for field in a.data.iter().chain(&b.data) {
                                ctx.charge(1)?;
                                required &= !field.optional;
                            }
                            if !required {
                                self.uncertain_equality(ctx, left, right, depth, policy)?
                            } else if a.data.len() != b.data.len() {
                                NO
                            } else {
                                let mut pairs = Buffer::empty();
                                let mut missing = false;
                                for field in &a.data {
                                    ctx.charge(1)?;
                                    if let Some((value, _)) = self.selected_field(
                                        ctx,
                                        right,
                                        field.name.as_bytes().unwrap(),
                                    )? {
                                        pairs.push(
                                            ctx,
                                            Pair {
                                                left: field.value,
                                                right: value,
                                                root: false,
                                                depth: child_depth,
                                            },
                                        )?;
                                    } else {
                                        missing = true;
                                    }
                                }
                                if missing && set {
                                    NO
                                } else {
                                    tasks.push(
                                        ctx,
                                        Task::Hash(
                                            pairs.data.len(),
                                            missing,
                                            !set && (!ak.single() || !bk.single()),
                                        ),
                                    )?;
                                    for pair in pairs.data {
                                        tasks.push(ctx, Task::Visit(pair))?;
                                    }
                                    continue;
                                }
                            }
                        }
                        _ => {
                            let value = self.scalar_equal(ctx, left, right, policy, root)?;
                            if value == MAYBE {
                                self.uncertain_equality(ctx, left, right, depth, policy)?
                            } else {
                                value
                            }
                        }
                    }
                }
            };
            values.push(ctx, value)?;
        }
        assert_eq!(values.data.len(), 1);
        Ok(values.data[0])
    }

    fn uncertain_equality(
        &self,
        ctx: &mut CallContext,
        left: Fact,
        right: Fact,
        depth: usize,
        policy: Policy,
    ) -> Result<u8> {
        Ok(
            if !policy.set()
                && self.may_exceed_depth(ctx, left, depth)?
                && self.may_exceed_depth(ctx, right, depth)?
            {
                MAYBE | LIMIT
            } else {
                MAYBE
            },
        )
    }

    /// Whether a native comparison that reaches `value` at `depth` may descend
    /// past the runtime's value-depth guard.
    pub(super) fn may_exceed_depth(
        &self,
        ctx: &mut CallContext,
        value: Fact,
        depth: usize,
    ) -> Result<bool> {
        if self.depth(value).saturating_add(depth) > crate::budget::MAX_VALUE_DEPTH {
            return Ok(true);
        }
        let mut pending = Buffer::empty();
        let mut visited = Slots::new(self.len(), false);
        pending.push(ctx, value)?;
        while let Some(value) = pending.data.pop() {
            ctx.charge(1)?;
            if visited.get(ctx, value.0)? {
                continue;
            }
            visited.set(ctx, value.0, true)?;
            match self.node(value) {
                Node::Atom(Atom::Unknown | Atom::Any)
                | Node::Named(_)
                | Node::Nominal { .. }
                | Node::Choice(_)
                | Node::Shape(_, true, _, _) => return Ok(true),
                Node::Array(value) | Node::Hash(_, value, _) | Node::Protected(value, ..) => {
                    pending.push(ctx, *value)?;
                }
                Node::Tuple(values) | Node::Union(values) => pending.extend(ctx, &values.data)?,
                Node::Shape(fields, ..) => {
                    for field in &fields.data {
                        ctx.charge(1)?;
                        pending.push(ctx, field.value)?;
                    }
                }
                _ => (),
            }
        }
        Ok(false)
    }

    /// Classifies a fact by the runtime type its values must have, or none when
    /// the fact may describe several classes or an unadmitted value.
    fn equality_class(&self, value: Fact) -> Option<Class> {
        Some(match self.node(value) {
            Node::Array(_) | Node::Tuple(_) => Class::Array,
            Node::Hash(..) | Node::Shape(..) | Node::Protected(..) => Class::Hash,
            Node::Instance { .. } => Class::Instance,
            Node::Enumeration { .. } => Class::Enumeration,
            Node::EnumMember { .. } => Class::EnumMember,
            Node::Builtin(_) => Class::Builtin,
            Node::Offset(_) => Class::Offset,
            Node::Callable {
                target: Callable::Function(_),
                ..
            } => Class::Function,
            Node::Callable {
                target: Callable::Host(_),
                ..
            } => Class::Host,
            _ => match self.atom(value)? {
                Atom::Never | Atom::Unknown | Atom::Any => return None,
                Atom::Nil => Class::Nil,
                Atom::Bool => Class::Bool,
                Atom::Int => Class::Int,
                Atom::Float => Class::Float,
                Atom::String => Class::String,
                Atom::Symbol => Class::Symbol,
                Atom::Duration => Class::Duration,
                Atom::Time => Class::Time,
                Atom::Money => Class::Money,
                Atom::Range => Class::Range,
                Atom::Regex => Class::Regex,
            },
        })
    }

    fn scalar_equal(
        &self,
        ctx: &mut CallContext,
        left: Fact,
        right: Fact,
        policy: Policy,
        root: bool,
    ) -> Result<u8> {
        if left == Atom::Never.fact() || right == Atom::Never.fact() {
            return Ok(0);
        }
        let truth = |value| if value { YES } else { NO };
        if policy.typed(root) {
            // Typed positions reject any known runtime type mismatch, including the
            // numeric kinds that ordinary equality compares by value.
            if let (Some(a), Some(b)) = (self.equality_class(left), self.equality_class(right)) {
                if a != b {
                    return Ok(NO);
                }
            }
        }
        let number = |value| match self.node(value) {
            Node::Integer(value) => Some(Value::int(*value)),
            Node::Float(value) => Some(Value::float(f64::from_bits(*value))),
            _ => None,
        };
        let nan =
            |value| matches!(self.node(value), Node::Float(bits) if f64::from_bits(*bits).is_nan());
        let reflexive = policy.nan_reflexive(root);
        if nan(left) && nan(right) {
            return Ok(truth(reflexive));
        }
        if !reflexive && (nan(left) || nan(right)) {
            return Ok(NO);
        }
        if let (Some(a), Some(b)) = (number(left), number(right)) {
            return Ok(truth(crate::ops::equal(ctx, &a, &b, 0)?));
        }
        if let (Node::Builtin(a), Node::Builtin(b)) = (self.node(left), self.node(right)) {
            return Ok(truth(a == b));
        }
        Ok(self.definitely_equal(left, right).map_or(MAYBE, truth))
    }
}
