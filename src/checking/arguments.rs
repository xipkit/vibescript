use super::facts::{Atom, Fact, Facts, HashKind, Node};
use crate::{CallContext, Result, budget::Buffer, bytecode::Parameter, syntax::ParamKind};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum Input {
    Supplied(Fact),
    Default,
    Either(Fact),
}

impl Input {
    pub fn widen(
        self,
        ctx: &mut CallContext,
        facts: &mut Facts,
        other: Self,
        depth: usize,
    ) -> Result<Self> {
        ctx.checkpoint()?;
        let (a, b) = match (self, other) {
            (Self::Default, Self::Default) => return Ok(Self::Default),
            (Self::Default, Self::Supplied(value) | Self::Either(value))
            | (Self::Supplied(value) | Self::Either(value), Self::Default) => {
                (Atom::Never.fact(), value)
            }
            (Self::Supplied(a) | Self::Either(a), Self::Supplied(b) | Self::Either(b)) => (a, b),
        };
        let value = facts.widen(ctx, a, b, depth)?;
        Ok(
            if matches!((self, other), (Self::Supplied(_), Self::Supplied(_))) {
                Self::Supplied(value)
            } else {
                Self::Either(value)
            },
        )
    }

    /// Admits the values of both inputs; a default on either side stays possible.
    fn join(self, ctx: &mut CallContext, facts: &mut Facts, other: Self) -> Result<Self> {
        Ok(match (self, other) {
            (Self::Default, Self::Default) => Self::Default,
            (Self::Supplied(a), Self::Supplied(b)) => Self::Supplied(facts.union(ctx, &[a, b])?),
            (Self::Default, Self::Supplied(value) | Self::Either(value))
            | (Self::Supplied(value) | Self::Either(value), Self::Default) => Self::Either(value),
            (Self::Supplied(a) | Self::Either(a), Self::Supplied(b) | Self::Either(b)) => {
                Self::Either(facts.union(ctx, &[a, b])?)
            }
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Keyword {
    pub name: Fact,
    pub value: Fact,
}

/// The possible numbers of values in a splatted run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Counts {
    /// Bit `n` marks `n` values as possible, for counts below 64.
    exact: u64,
    /// Every count from this one on is possible.
    from: Option<usize>,
}

impl Counts {
    /// No possible count, the identity of [`Self::or`].
    const NONE: Self = Self {
        exact: 0,
        from: None,
    };

    /// Exactly `count` values; a count past the bit set stands for itself and every larger one.
    fn exact(count: usize) -> Self {
        if count < 64 {
            Self {
                exact: 1 << count,
                from: None,
            }
        } else {
            Self {
                exact: 0,
                from: Some(count),
            }
        }
    }

    /// Any number of values, as in an array of unknown length.
    fn any() -> Self {
        Self {
            exact: 0,
            from: Some(0),
        }
    }

    /// Every count that either admits.
    fn or(self, other: Self) -> Self {
        Self {
            exact: self.exact | other.exact,
            from: match (self.from, other.from) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (from, None) | (None, from) => from,
            },
        }
    }

    /// The counts of this run followed by another.
    fn then(self, other: Self) -> Self {
        let lower = |from: Option<usize>, count: usize| Some(from.map_or(count, |f| f.min(count)));
        let mut exact = 0u64;
        let mut from = None;
        let mut left = self.exact;
        while left != 0 {
            let a = left.trailing_zeros() as usize;
            left &= left - 1;
            let mut right = other.exact;
            while right != 0 {
                let b = right.trailing_zeros() as usize;
                right &= right - 1;
                if a + b < 64 {
                    exact |= 1 << (a + b);
                } else {
                    from = lower(from, a + b);
                }
            }
        }
        // An unbounded run reaches every count past the other run's fewest values.
        if let Some(start) = self.from {
            from = lower(from, start.saturating_add(other.min()));
        }
        if let Some(start) = other.from {
            from = lower(from, self.min().saturating_add(start));
        }
        Self { exact, from }
    }

    fn contains(self, count: usize) -> bool {
        count < 64 && self.exact >> count & 1 != 0 || self.from.is_some_and(|from| count >= from)
    }

    fn min(self) -> usize {
        let exact = (self.exact != 0).then(|| self.exact.trailing_zeros() as usize);
        match (exact, self.from) {
            (Some(a), Some(b)) => a.min(b),
            (count, None) | (None, count) => count.unwrap_or(0),
        }
    }

    /// The largest possible count, or `None` when counts are unbounded.
    fn max(self) -> Option<usize> {
        match self.from {
            Some(_) => None,
            None => Some(63usize.saturating_sub(self.exact.leading_zeros() as usize)),
        }
    }
}

/// A run of splatted positional values whose count is not known exactly.
#[derive(Debug)]
struct Spread {
    /// The position in [`Arguments::positional`] where the run begins; later
    /// entries follow it.
    at: usize,
    /// Every value the run can contain.
    element: Fact,
    counts: Counts,
    /// Each exact run the splats can supply, while they are few and every length is known.
    runs: Option<Buffer<Buffer<Fact>>>,
}

/// Lists at most this many exact runs before a spread keeps only its element and counts.
const RUNS: usize = 16;

impl Spread {
    fn snapshot(&self, ctx: &mut CallContext) -> Result<Self> {
        Ok(Self {
            at: self.at,
            element: self.element,
            counts: self.counts,
            runs: copy_runs(ctx, self.runs.as_ref())?,
        })
    }
}

fn copy_runs(
    ctx: &mut CallContext,
    runs: Option<&Buffer<Buffer<Fact>>>,
) -> Result<Option<Buffer<Buffer<Fact>>>> {
    let Some(runs) = runs else {
        return Ok(None);
    };
    let mut copied = Buffer::with_capacity(ctx, runs.data.len())?;
    for run in &runs.data {
        let mut values = Buffer::with_capacity(ctx, run.data.len())?;
        values.extend(ctx, &run.data)?;
        copied.push(ctx, values)?;
    }
    Ok(Some(copied))
}

/// Adds `run` unless an equal run is listed, returning `false` once the list would be too long.
fn add_run(
    ctx: &mut CallContext,
    runs: &mut Buffer<Buffer<Fact>>,
    run: Buffer<Fact>,
) -> Result<bool> {
    ctx.charge((runs.data.len() * run.data.len().max(1)) as u64)?;
    if runs.data.iter().any(|listed| listed.data == run.data) {
        return Ok(true);
    }
    if runs.data.len() == RUNS {
        return Ok(false);
    }
    runs.push(ctx, run)?;
    Ok(true)
}

/// A splat operand split by whether the runtime can expand each alternative.
pub(super) struct Splat {
    /// The alternatives that expand: arrays for `*`, hashes for `**`, and gradual values.
    pub admitted: Fact,
    /// The alternatives the runtime rejects before the call.
    pub rejected: Fact,
}

/// Splits `value` into the alternatives that `*` (or `**` when `keyword`) expands and the rest.
pub(super) fn split_splat(
    ctx: &mut CallContext,
    facts: &mut Facts,
    value: Fact,
    keyword: bool,
) -> Result<Splat> {
    ctx.checkpoint()?;
    let arms = splat_arms(ctx, facts, value)?;
    let mut admitted = Buffer::empty();
    let mut rejected = Buffer::empty();
    for &arm in &arms.data {
        ctx.charge(1)?;
        let expands = match facts.node(arm) {
            Node::Atom(Atom::Never) => continue,
            Node::Atom(Atom::Unknown | Atom::Any) | Node::Named(_) => true,
            Node::Tuple(_) | Node::Array(_) => !keyword,
            Node::Hash(..) | Node::Shape(..) | Node::Protected(..) => keyword,
            _ => false,
        };
        if expands {
            admitted.push(ctx, arm)?;
        } else {
            rejected.push(ctx, arm)?;
        }
    }
    Ok(Splat {
        admitted: facts.union(ctx, &admitted.data)?,
        rejected: facts.union(ctx, &rejected.data)?,
    })
}

#[derive(Debug)]
pub(super) struct Arguments {
    pub positional: Buffer<Fact>,
    pub keywords: Buffer<Keyword>,
    pub block: Option<super::blocks::Closure>,
    pub options_hash: bool,
    /// Positional values from splats whose length is not known exactly.
    spread: Option<Spread>,
    /// Keywords that a hash splat may or may not supply.
    optional: Buffer<Keyword>,
    /// The values of keywords whose names a hash splat does not reveal.
    unlisted: Option<Fact>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Failure {
    Require {
        message: Fact,
        class: crate::ErrorClass,
    },
    NonCallable,
    /// Invoking a native method without its receiver.
    Unbound,
    Undefined,
    HostArity,
    HostKeywords,
    HostBlock,
    HostBlockDriver,
    HostGrant,
    HostResult {
        actual: Fact,
        expected: Fact,
    },
    HostTypeBinding {
        parameter: Option<usize>,
        expected: Fact,
        ambiguous: bool,
    },
    BuiltinArity,
    BuiltinBlock,
    BuiltinKeywords,
    BuiltinKeyword(Fact),
    BuiltinKeywordType {
        name: Fact,
        actual: Fact,
        expected: Fact,
    },
    BuiltinValue,
    DetachedValue(Fact),
    TypeLiteral(Fact),
    BuiltinDomain(Fact),
    JsonValue(Fact),
    Missing(usize),
    ExtraPositionals,
    ExtraKeyword(Fact),
    Type {
        parameter: usize,
        actual: Fact,
        expected: Fact,
    },
}

pub(super) struct Bound {
    pub inputs: Buffer<Input>,
    pub failures: Buffer<Failure>,
    /// Some possible argument shape fails to bind, although another binds.
    pub uncertain: bool,
}

/// The argument shapes that a splat of uncertain size can produce.
struct Shapes {
    /// The splatted values of each shape, from the fewest.
    runs: Buffer<Buffer<Fact>>,
    /// Whether the longest run also stands for every longer one.
    tail: bool,
    /// Keywords that each shape may or may not include.
    maybe: Buffer<Keyword>,
    /// The value of keywords that match no parameter, when there can be some.
    other: Option<Fact>,
}

/// Binds at most this many argument shapes before binding gradually.
const SHAPES: usize = 64;

impl Arguments {
    /// Rejects detached methods before entering a script or host body.
    pub fn admit(
        &mut self,
        ctx: &mut CallContext,
        facts: &mut Facts,
        failures: &mut Buffer<Failure>,
    ) -> Result<bool> {
        for value in self.positional.data.iter_mut().chain(
            self.keywords
                .data
                .iter_mut()
                .map(|keyword| &mut keyword.value),
        ) {
            ctx.charge(1)?;
            if facts.escapes(*value) {
                failures.push(ctx, Failure::DetachedValue(*value))?;
                *value = facts.exported(ctx, *value)?;
                if *value == Atom::Never.fact() {
                    return Ok(false);
                }
            }
        }
        // A splatted value that cannot be exported can only be absent.
        if let Some(spread) = &mut self.spread {
            ctx.charge(1)?;
            if facts.escapes(spread.element) {
                failures.push(ctx, Failure::DetachedValue(spread.element))?;
                spread.runs = None;
                spread.element = facts.exported(ctx, spread.element)?;
                if spread.element == Atom::Never.fact() {
                    if !spread.counts.contains(0) {
                        return Ok(false);
                    }
                    self.spread = None;
                }
            }
        }
        let mut index = 0;
        while index < self.optional.data.len() {
            ctx.charge(1)?;
            let value = self.optional.data[index].value;
            if facts.escapes(value) {
                failures.push(ctx, Failure::DetachedValue(value))?;
                let value = facts.exported(ctx, value)?;
                if value == Atom::Never.fact() {
                    self.optional.data.remove(index);
                    continue;
                }
                self.optional.data[index].value = value;
            }
            index += 1;
        }
        if let Some(value) = self.unlisted {
            ctx.charge(1)?;
            if facts.escapes(value) {
                failures.push(ctx, Failure::DetachedValue(value))?;
                let value = facts.exported(ctx, value)?;
                self.unlisted = (value != Atom::Never.fact()).then_some(value);
            }
        }
        Ok(true)
    }

    pub fn new() -> Self {
        Self {
            positional: Buffer::empty(),
            keywords: Buffer::empty(),
            block: None,
            options_hash: true,
            spread: None,
            optional: Buffer::empty(),
            unlisted: None,
        }
    }

    pub fn snapshot(&self, ctx: &mut CallContext) -> Result<Self> {
        ctx.checkpoint()?;
        let mut args = Self::new();
        args.options_hash = self.options_hash;
        args.positional.extend(ctx, &self.positional.data)?;
        args.keywords.extend(ctx, &self.keywords.data)?;
        args.block = self.block.as_ref().map(|b| b.snapshot(ctx)).transpose()?;
        args.spread = self
            .spread
            .as_ref()
            .map(|spread| spread.snapshot(ctx))
            .transpose()?;
        args.optional.extend(ctx, &self.optional.data)?;
        args.unlisted = self.unlisted;
        Ok(args)
    }

    /// Joins each position's values over the argument counts within `required..=capacity`, or
    /// returns `None` when the splat allows no such count.
    pub fn positions(
        &self,
        ctx: &mut CallContext,
        facts: &mut Facts,
        required: usize,
        capacity: usize,
    ) -> Result<Option<Buffer<Fact>>> {
        ctx.checkpoint()?;
        let fixed = self.positional.data.len();
        let Some(spread) = &self.spread else {
            if !(required..=capacity).contains(&fixed) {
                return Ok(None);
            }
            let mut positions = Buffer::with_capacity(ctx, fixed)?;
            positions.extend(ctx, &self.positional.data)?;
            return Ok(Some(positions));
        };
        // Each run length, with the exact run when one is listed.
        let mut lengths: Buffer<(usize, Option<&Buffer<Fact>>)> = Buffer::empty();
        if let Some(runs) = &spread.runs {
            for run in &runs.data {
                lengths.push(ctx, (run.data.len(), Some(run)))?;
            }
        } else {
            for count in required.max(fixed)..=capacity {
                ctx.charge(1)?;
                if spread.counts.contains(count - fixed) {
                    lengths.push(ctx, (count - fixed, None))?;
                }
            }
        }
        let mut positions = Buffer::empty();
        let mut admitted = false;
        for &(length, run) in &lengths.data {
            let count = fixed + length;
            if !(required..=capacity).contains(&count) {
                continue;
            }
            admitted = true;
            for index in 0..count {
                ctx.charge(1)?;
                let value = if index < spread.at {
                    self.positional.data[index]
                } else if index < spread.at + length {
                    run.map_or(spread.element, |run| run.data[index - spread.at])
                } else {
                    self.positional.data[index - length]
                };
                if index < positions.data.len() {
                    positions.data[index] = facts.union(ctx, &[positions.data[index], value])?;
                } else {
                    positions.push(ctx, value)?;
                }
            }
        }
        Ok(admitted.then_some(positions))
    }

    /// Reports whether the argument count or keyword names depend on a splat's contents.
    pub fn uncertain(&self) -> bool {
        self.spread.is_some() || !self.optional.data.is_empty() || self.unlisted.is_some()
    }

    /// Reports whether two pending argument lists can be joined position by position.
    pub fn same_shape(&self, ctx: &mut CallContext, other: &Self) -> Result<bool> {
        ctx.charge(1)?;
        if self.options_hash != other.options_hash
            || self.positional.data.len() != other.positional.data.len()
            || self.keywords.data.len() != other.keywords.data.len()
            || self.optional.data.len() != other.optional.data.len()
            || self.block.is_some() != other.block.is_some()
            || self.spread.as_ref().map(|spread| spread.at)
                != other.spread.as_ref().map(|spread| spread.at)
            || self.unlisted.is_some() != other.unlisted.is_some()
        {
            return Ok(false);
        }
        for (a, b) in self
            .keywords
            .data
            .iter()
            .zip(&other.keywords.data)
            .chain(self.optional.data.iter().zip(&other.optional.data))
        {
            ctx.charge(1)?;
            if a.name != b.name {
                return Ok(false);
            }
        }
        Ok(true)
    }

    pub fn keyword(&mut self, ctx: &mut CallContext, name: Fact, value: Fact) -> Result<()> {
        ctx.checkpoint()?;
        ctx.charge(self.optional.data.len() as u64)?;
        self.optional.data.retain(|keyword| keyword.name != name);
        for keyword in &mut self.keywords.data {
            ctx.charge(1)?;
            if keyword.name == name {
                keyword.value = value;
                return Ok(());
            }
        }
        self.keywords.push(ctx, Keyword { name, value })
    }

    /// Adds a keyword that a splat supplies on only some paths.
    fn optional_keyword(
        &mut self,
        ctx: &mut CallContext,
        facts: &mut Facts,
        name: Fact,
        value: Fact,
    ) -> Result<()> {
        ctx.checkpoint()?;
        for keyword in self.keywords.data.iter_mut().chain(&mut self.optional.data) {
            ctx.charge(1)?;
            if keyword.name == name {
                keyword.value = facts.union(ctx, &[keyword.value, value])?;
                return Ok(());
            }
        }
        // Without this splat's key, an earlier unlisted key of the same name supplies it.
        let value = match self.unlisted {
            Some(unlisted) => facts.union(ctx, &[value, unlisted])?,
            None => value,
        };
        self.optional.push(ctx, Keyword { name, value })
    }

    /// Adds keywords with unknown names, which may replace any earlier keyword.
    fn unlisted_keywords(
        &mut self,
        ctx: &mut CallContext,
        facts: &mut Facts,
        value: Fact,
    ) -> Result<()> {
        ctx.checkpoint()?;
        for keyword in self.keywords.data.iter_mut().chain(&mut self.optional.data) {
            ctx.charge(1)?;
            keyword.value = facts.union(ctx, &[keyword.value, value])?;
        }
        self.unlisted = Some(match self.unlisted {
            Some(unlisted) => facts.union(ctx, &[unlisted, value])?,
            None => value,
        });
        Ok(())
    }

    /// Adds a run of positional values, each one of `element`, with one of the given counts,
    /// or exactly one of `runs` when they are listed.
    fn spread_values(
        &mut self,
        ctx: &mut CallContext,
        facts: &mut Facts,
        element: Fact,
        counts: Counts,
        runs: Option<Buffer<Buffer<Fact>>>,
    ) -> Result<()> {
        ctx.checkpoint()?;
        let Some(spread) = &mut self.spread else {
            self.spread = Some(Spread {
                at: self.positional.data.len(),
                element,
                counts,
                runs,
            });
            return Ok(());
        };
        // One run absorbs the values between two runs; its possible counts stay exact.
        let trailing = self.positional.data.len() - spread.at;
        spread.runs = match (spread.runs.take(), runs) {
            (Some(before), Some(after)) => {
                let mut joined = Buffer::empty();
                let mut listed = true;
                'runs: for first in &before.data {
                    for second in &after.data {
                        let mut run = Buffer::with_capacity(
                            ctx,
                            first.data.len() + trailing + second.data.len(),
                        )?;
                        run.extend(ctx, &first.data)?;
                        run.extend(ctx, &self.positional.data[spread.at..])?;
                        run.extend(ctx, &second.data)?;
                        if !add_run(ctx, &mut joined, run)? {
                            listed = false;
                            break 'runs;
                        }
                    }
                }
                listed.then_some(joined)
            }
            _ => None,
        };
        let mut values = Buffer::with_capacity(ctx, trailing + 2)?;
        values.extend(ctx, &self.positional.data[spread.at..])?;
        values.extend(ctx, &[spread.element, element])?;
        spread.element = facts.union(ctx, &values.data)?;
        ctx.charge(128)?;
        spread.counts = spread.counts.then(Counts::exact(trailing)).then(counts);
        self.positional.data.truncate(spread.at);
        Ok(())
    }

    /// Expands the admitted alternatives of `*value`, keeping each exact array's positions and
    /// the count bounds of the rest.
    pub fn splat(
        &mut self,
        ctx: &mut CallContext,
        facts: &mut Facts,
        admitted: Fact,
    ) -> Result<()> {
        ctx.checkpoint()?;
        let arms = splat_arms(ctx, facts, admitted)?;
        // Each array's items, or `None` for one of unknown length.
        let mut arrays: Buffer<Option<Buffer<Fact>>> = Buffer::empty();
        let mut elements = Buffer::empty();
        for &arm in &arms.data {
            ctx.charge(1)?;
            match facts.node(arm) {
                Node::Array(element) if *element == Atom::Never.fact() => {
                    arrays.push(ctx, Some(Buffer::empty()))?;
                }
                Node::Tuple(items) => {
                    let mut copied = Buffer::with_capacity(ctx, items.data.len())?;
                    copied.extend(ctx, &items.data)?;
                    elements.extend(ctx, &copied.data)?;
                    arrays.push(ctx, Some(copied))?;
                }
                Node::Array(element) => {
                    let element = *element;
                    elements.push(ctx, element)?;
                    arrays.push(ctx, None)?;
                }
                Node::Atom(Atom::Any) => {
                    elements.push(ctx, Atom::Any.fact())?;
                    arrays.push(ctx, None)?;
                }
                _ => {
                    elements.push(ctx, Atom::Unknown.fact())?;
                    arrays.push(ctx, None)?;
                }
            }
        }
        if arrays.data.is_empty() {
            return Ok(());
        }
        let first = arrays.data[0].as_ref().map(|items| items.data.len());
        let uniform = first.is_some()
            && arrays
                .data
                .iter()
                .all(|items| items.as_ref().map(|items| items.data.len()) == first);
        if uniform {
            // Arrays of one length keep their positions, each joining its alternatives.
            let mut positions = arrays.data.swap_remove(0).unwrap();
            for items in &arrays.data {
                for (position, &item) in
                    positions.data.iter_mut().zip(&items.as_ref().unwrap().data)
                {
                    ctx.charge(1)?;
                    *position = facts.union(ctx, &[*position, item])?;
                }
            }
            self.positional.extend(ctx, &positions.data)?;
        } else {
            let mut counts = Counts::NONE;
            let mut runs = Some(Buffer::empty());
            for items in arrays.data {
                ctx.charge(1)?;
                counts = counts.or(items
                    .as_ref()
                    .map_or(Counts::any(), |items| Counts::exact(items.data.len())));
                runs = match (runs, items) {
                    (Some(mut runs), Some(items)) => {
                        add_run(ctx, &mut runs, items)?.then_some(runs)
                    }
                    _ => None,
                };
            }
            let element = facts.union(ctx, &elements.data)?;
            self.spread_values(ctx, facts, element, counts, runs)?;
        }
        Ok(())
    }

    /// Expands the admitted alternatives of `**value`, keeping the names that every hash is
    /// known to contain.
    pub fn keyword_splat(
        &mut self,
        ctx: &mut CallContext,
        facts: &mut Facts,
        admitted: Fact,
    ) -> Result<()> {
        ctx.checkpoint()?;
        let arms = splat_arms(ctx, facts, admitted)?;
        let mut hashes = Buffer::empty();
        let mut unlisted = Buffer::empty();
        for &arm in &arms.data {
            ctx.charge(1)?;
            let arm = match facts.node(arm) {
                Node::Protected(shape, ..) => *shape,
                _ => arm,
            };
            match facts.node(arm) {
                Node::Shape(_, open, ..) => {
                    if *open {
                        unlisted.push(ctx, Atom::Unknown.fact())?;
                    }
                }
                Node::Hash(_, value, _) => {
                    let value = *value;
                    unlisted.push(ctx, value)?;
                }
                Node::Atom(Atom::Any) => unlisted.push(ctx, Atom::Any.fact())?,
                _ => unlisted.push(ctx, Atom::Unknown.fact())?,
            }
            hashes.push(ctx, arm)?;
        }
        if hashes.data.is_empty() {
            return Ok(());
        }
        // Each listed name with its values, and whether every admitted hash requires it.
        let mut listed: Buffer<(Fact, Fact, bool)> = Buffer::empty();
        for (index, &hash) in hashes.data.iter().enumerate() {
            ctx.charge(1)?;
            let mut fields = Buffer::empty();
            if let Node::Shape(shape, ..) = facts.node(hash) {
                for field in &shape.data {
                    ctx.charge(1)?;
                    fields.push(ctx, (field.name.clone(), field.value, !field.optional))?;
                }
            }
            let mut names = Buffer::with_capacity(ctx, fields.data.len())?;
            for (name, ..) in &fields.data {
                let name = facts.symbol(ctx, name.as_bytes().unwrap())?;
                names.push(ctx, name)?;
            }
            // A name this hash lacks is only sometimes present.
            for entry in &mut listed.data {
                ctx.charge(names.data.len() as u64)?;
                entry.2 &= names.data.contains(&entry.0);
            }
            for (&name, &(_, value, required)) in names.data.iter().zip(&fields.data) {
                ctx.charge(listed.data.len() as u64)?;
                if let Some(entry) = listed.data.iter_mut().find(|entry| entry.0 == name) {
                    entry.1 = facts.union(ctx, &[entry.1, value])?;
                    entry.2 &= required;
                } else {
                    listed.push(ctx, (name, value, required && index == 0))?;
                }
            }
        }
        let unlisted = if unlisted.data.is_empty() {
            None
        } else {
            Some(facts.union(ctx, &unlisted.data)?)
        };
        if let Some(value) = unlisted {
            for entry in &mut listed.data {
                ctx.charge(1)?;
                if !entry.2 {
                    entry.1 = facts.union(ctx, &[entry.1, value])?;
                }
            }
            self.unlisted_keywords(ctx, facts, value)?;
        }
        for &(name, value, required) in &listed.data {
            if !required {
                self.optional_keyword(ctx, facts, name, value)?;
            }
        }
        for &(name, value, required) in &listed.data {
            if required {
                self.keyword(ctx, name, value)?;
            }
        }
        Ok(())
    }

    pub fn join(&mut self, ctx: &mut CallContext, facts: &mut Facts, other: &Self) -> Result<bool> {
        ctx.checkpoint()?;
        assert_eq!(self.options_hash, other.options_hash);
        assert_eq!(self.positional.data.len(), other.positional.data.len());
        assert_eq!(self.keywords.data.len(), other.keywords.data.len());
        assert_eq!(self.optional.data.len(), other.optional.data.len());
        let mut changed = false;
        if let (Some(a), Some(b)) = (&mut self.block, &other.block) {
            changed |= a.join(ctx, facts, b)?;
        } else {
            assert_eq!(self.block.is_some(), other.block.is_some());
        }
        for (left, right) in self.positional.data.iter_mut().zip(&other.positional.data) {
            ctx.charge(1)?;
            let value = facts.union(ctx, &[*left, *right])?;
            changed |= *left != value;
            *left = value;
        }
        for (left, right) in self
            .keywords
            .data
            .iter_mut()
            .zip(&other.keywords.data)
            .chain(self.optional.data.iter_mut().zip(&other.optional.data))
        {
            ctx.charge(1)?;
            assert_eq!(left.name, right.name);
            let value = facts.union(ctx, &[left.value, right.value])?;
            changed |= left.value != value;
            left.value = value;
        }
        match (&mut self.spread, &other.spread) {
            (Some(left), Some(right)) => {
                assert_eq!(left.at, right.at);
                let element = facts.union(ctx, &[left.element, right.element])?;
                let counts = left.counts.or(right.counts);
                changed |= left.element != element || left.counts != counts;
                left.element = element;
                left.counts = counts;
                if let (Some(runs), Some(others)) = (&mut left.runs, &right.runs) {
                    let listed = runs.data.len();
                    let mut kept = true;
                    for run in &others.data {
                        let mut copied = Buffer::with_capacity(ctx, run.data.len())?;
                        copied.extend(ctx, &run.data)?;
                        if !add_run(ctx, runs, copied)? {
                            kept = false;
                            break;
                        }
                    }
                    changed |= !kept || runs.data.len() != listed;
                    if !kept {
                        left.runs = None;
                    }
                } else {
                    changed |= left.runs.is_some();
                    left.runs = None;
                }
            }
            (left, right) => assert_eq!(left.is_some(), right.is_some()),
        }
        match (self.unlisted, other.unlisted) {
            (Some(left), Some(right)) => {
                let value = facts.union(ctx, &[left, right])?;
                changed |= left != value;
                self.unlisted = Some(value);
            }
            (left, right) => assert_eq!(left.is_some(), right.is_some()),
        }
        Ok(changed)
    }

    /// Binds argument shape; each supplied value is normalized when its parameter is reached.
    pub fn bind(
        self,
        ctx: &mut CallContext,
        facts: &mut Facts,
        params: &[Parameter],
    ) -> Result<Bound> {
        let options = self.options_hash;
        self.bind_options(ctx, facts, params, options)
    }

    /// Host keywords bind by name without becoming a positional options hash.
    pub fn bind_host(
        self,
        ctx: &mut CallContext,
        facts: &mut Facts,
        params: &[Parameter],
    ) -> Result<Bound> {
        self.bind_options(ctx, facts, params, false)
    }

    fn bind_options(
        self,
        ctx: &mut CallContext,
        facts: &mut Facts,
        params: &[Parameter],
        options: bool,
    ) -> Result<Bound> {
        if self.uncertain() {
            self.bind_shapes(ctx, facts, params, options)
        } else {
            self.bind_exact(ctx, facts, params, options)
        }
    }

    /// Lists the argument shapes a splat of uncertain size can produce for `params`, or `None`
    /// when there are too many to bind one by one.
    fn shapes(
        &self,
        ctx: &mut CallContext,
        facts: &mut Facts,
        params: &[Parameter],
    ) -> Result<Option<Shapes>> {
        ctx.checkpoint()?;
        let mut runs = Buffer::empty();
        let mut tail = false;
        match &self.spread {
            None => runs.push(ctx, Buffer::empty())?,
            Some(Spread {
                runs: Some(listed), ..
            }) => {
                runs = copy_runs(ctx, Some(listed))?.unwrap();
                ctx.charge(runs.data.len() as u64)?;
                runs.data.sort_by_key(|run| run.data.len());
            }
            Some(spread) => {
                let counts = spread.counts;
                let min = counts.min();
                // Past every parameter, more values only lengthen the rest array.
                let saturated = (params.len() + 1)
                    .saturating_sub(self.positional.data.len())
                    .max(min);
                let top = counts.max().map_or(saturated, |max| max.min(saturated));
                tail = counts.max().is_none_or(|max| max > top);
                for count in min..=top {
                    ctx.charge(1)?;
                    if counts.contains(count) || count == top && tail {
                        let mut run = Buffer::with_capacity(ctx, count)?;
                        for _ in 0..count {
                            run.push(ctx, spread.element)?;
                        }
                        runs.push(ctx, run)?;
                    }
                }
            }
        }
        let mut maybe = Buffer::empty();
        maybe.extend(ctx, &self.optional.data)?;
        if let Some(value) = self.unlisted {
            for param in params {
                ctx.charge(1)?;
                if !matches!(param.kind, ParamKind::Positional | ParamKind::Keyword) {
                    continue;
                }
                let name = facts.symbol(ctx, param.name.as_bytes())?;
                ctx.charge((self.keywords.data.len() + maybe.data.len()) as u64)?;
                if !self
                    .keywords
                    .data
                    .iter()
                    .chain(&maybe.data)
                    .any(|keyword| keyword.name == name)
                {
                    maybe.push(ctx, Keyword { name, value })?;
                }
            }
        }
        let others = if self.unlisted.is_some() { 2 } else { 1 };
        let total = u32::try_from(maybe.data.len())
            .ok()
            .and_then(|names| 1usize.checked_shl(names))
            .and_then(|combinations| combinations.checked_mul(runs.data.len()))
            .and_then(|shapes| shapes.checked_mul(others));
        if total.is_none_or(|total| total > SHAPES) {
            return Ok(None);
        }
        Ok(Some(Shapes {
            runs,
            tail,
            maybe,
            other: self.unlisted,
        }))
    }

    /// Binds each possible argument shape and joins the inputs of those that bind. Failures are
    /// known only when every shape fails; they are then the failures of the fewest arguments.
    fn bind_shapes(
        self,
        ctx: &mut CallContext,
        facts: &mut Facts,
        params: &[Parameter],
        options: bool,
    ) -> Result<Bound> {
        ctx.checkpoint()?;
        let Some(shapes) = self.shapes(ctx, facts, params)? else {
            return bind_gradually(ctx, facts, params);
        };
        let at = self
            .spread
            .as_ref()
            .map_or(self.positional.data.len(), |spread| spread.at);
        let mut inputs: Option<Buffer<Input>> = None;
        let mut failures = None;
        let mut uncertain = false;
        let longest = shapes.runs.data.len() - 1;
        for (index, run) in shapes.runs.data.iter().enumerate() {
            for mask in 0..1usize << shapes.maybe.data.len() {
                for other in std::iter::once(None).chain(shapes.other.map(Some)) {
                    ctx.charge(1)?;
                    let mut exact = Self::new();
                    exact.options_hash = self.options_hash;
                    exact.positional.extend(ctx, &self.positional.data[..at])?;
                    exact.positional.extend(ctx, &run.data)?;
                    exact.positional.extend(ctx, &self.positional.data[at..])?;
                    exact.keywords.extend(ctx, &self.keywords.data)?;
                    for (index, &keyword) in shapes.maybe.data.iter().enumerate() {
                        if mask & 1 << index != 0 {
                            exact.keywords.push(ctx, keyword)?;
                        }
                    }
                    if let Some(value) = other {
                        // A name fact that matches no parameter stands for every other key.
                        exact.keywords.push(
                            ctx,
                            Keyword {
                                name: Atom::Symbol.fact(),
                                value,
                            },
                        )?;
                    }
                    let mut bound = exact.bind_exact(ctx, facts, params, options)?;
                    if !bound.failures.data.is_empty() {
                        uncertain = true;
                        failures.get_or_insert(bound.failures);
                        continue;
                    }
                    if shapes.tail && index == longest {
                        generalize_rest(ctx, facts, params, &mut bound.inputs)?;
                    }
                    inputs = Some(match inputs {
                        None => bound.inputs,
                        Some(mut joined) => {
                            for (left, &right) in joined.data.iter_mut().zip(&bound.inputs.data) {
                                ctx.charge(1)?;
                                *left = left.join(ctx, facts, right)?;
                            }
                            joined
                        }
                    });
                }
            }
        }
        Ok(match inputs {
            Some(inputs) => Bound {
                inputs,
                failures: Buffer::empty(),
                uncertain,
            },
            None => Bound {
                inputs: Buffer::empty(),
                failures: failures.unwrap(),
                uncertain: false,
            },
        })
    }

    fn bind_exact(
        mut self,
        ctx: &mut CallContext,
        facts: &mut Facts,
        params: &[Parameter],
        options: bool,
    ) -> Result<Bound> {
        ctx.checkpoint()?;
        if options {
            self.collapse(ctx, facts, params)?;
        }
        let mut used = Buffer::with_capacity(ctx, self.keywords.data.len())?;
        for _ in &self.keywords.data {
            ctx.charge(1)?;
            used.data.push(false);
        }
        let mut bound = Bound {
            inputs: Buffer::empty(),
            failures: Buffer::empty(),
            uncertain: false,
        };
        let mut positional = 0;
        let mut rest = false;
        // Keyword-rest values are built after all named parameters mark their keys.
        for (index, param) in params.iter().enumerate() {
            ctx.charge(1)?;
            let value = match param.kind {
                ParamKind::Rest => {
                    let value = facts.tuple(ctx, &self.positional.data[positional..])?;
                    positional = self.positional.data.len();
                    Some(value)
                }
                ParamKind::KeywordRest => {
                    rest = true;
                    None
                }
                ParamKind::Positional if positional < self.positional.data.len() => {
                    let value = self.positional.data[positional];
                    positional += 1;
                    Some(value)
                }
                _ => {
                    let name = facts.symbol(ctx, param.name.as_bytes())?;
                    let mut value = None;
                    for (i, keyword) in self.keywords.data.iter().enumerate() {
                        ctx.charge(1)?;
                        if keyword.name == name {
                            used.data[i] = true;
                            value = Some(keyword.value);
                            break;
                        }
                    }
                    if value.is_none() && !param.default {
                        bound.failures.push(ctx, Failure::Missing(index))?;
                    }
                    value
                }
            };
            bound
                .inputs
                .push(ctx, value.map_or(Input::Default, Input::Supplied))?;
        }
        if positional < self.positional.data.len() {
            bound.failures.push(ctx, Failure::ExtraPositionals)?;
        }
        if rest {
            let mut remaining = Buffer::empty();
            for (i, &keyword) in self.keywords.data.iter().enumerate() {
                ctx.charge(1)?;
                if !used.data[i] {
                    remaining.push(ctx, keyword)?;
                }
            }
            let value = keyword_shape(ctx, facts, &remaining.data)?;
            for (index, param) in params.iter().enumerate() {
                ctx.charge(1)?;
                if param.kind == ParamKind::KeywordRest {
                    bound.inputs.data[index] = Input::Supplied(value);
                }
            }
        } else {
            for (index, keyword) in self.keywords.data.iter().enumerate() {
                ctx.charge(1)?;
                if !used.data[index] {
                    bound
                        .failures
                        .push(ctx, Failure::ExtraKeyword(keyword.name))?;
                }
            }
        }
        Ok(bound)
    }

    fn collapse(
        &mut self,
        ctx: &mut CallContext,
        facts: &mut Facts,
        params: &[Parameter],
    ) -> Result<()> {
        if self.keywords.data.is_empty() {
            return Ok(());
        }
        for param in params {
            ctx.charge(1)?;
            if matches!(param.kind, ParamKind::Keyword | ParamKind::KeywordRest) {
                return Ok(());
            }
        }
        let mut positional = self.positional.data.len();
        for param in params {
            ctx.charge(1)?;
            if param.kind == ParamKind::Positional {
                if positional > 0 {
                    positional -= 1;
                    continue;
                }
                let name = facts.symbol(ctx, param.name.as_bytes())?;
                for keyword in &self.keywords.data {
                    ctx.charge(1)?;
                    if keyword.name == name {
                        return Ok(());
                    }
                }
            }
            let value = keyword_shape(ctx, facts, &self.keywords.data)?;
            self.positional.push(ctx, value)?;
            self.keywords = Buffer::empty();
            break;
        }
        Ok(())
    }
}

/// Lists the alternatives of a splat operand, including those of annotation choices.
fn splat_arms(ctx: &mut CallContext, facts: &Facts, value: Fact) -> Result<Buffer<Fact>> {
    let mut arms = Buffer::empty();
    let mut pending = Buffer::empty();
    pending.push(ctx, value)?;
    while let Some(value) = pending.data.pop() {
        ctx.charge(1)?;
        match facts.node(value) {
            Node::Union(values) | Node::Choice(values) => pending.extend(ctx, &values.data)?,
            _ => arms.push(ctx, value)?,
        }
    }
    Ok(arms)
}

/// Widens the rest array of the longest bound shape to every longer one.
fn generalize_rest(
    ctx: &mut CallContext,
    facts: &mut Facts,
    params: &[Parameter],
    inputs: &mut Buffer<Input>,
) -> Result<()> {
    for (param, input) in params.iter().zip(&mut inputs.data) {
        ctx.charge(1)?;
        if param.kind != ParamKind::Rest {
            continue;
        }
        if let Input::Supplied(values) = *input {
            let element = facts.elements(ctx, values)?;
            if element != Atom::Never.fact() {
                *input = Input::Supplied(facts.array(ctx, element)?);
            }
        }
    }
    Ok(())
}

/// Binds every parameter to a gradual value when the argument shapes are too many to list.
fn bind_gradually(ctx: &mut CallContext, facts: &mut Facts, params: &[Parameter]) -> Result<Bound> {
    let mut inputs = Buffer::empty();
    for param in params {
        ctx.charge(1)?;
        let value = general_input(ctx, facts, param.kind, None)?;
        inputs.push(
            ctx,
            if param.default {
                Input::Either(value)
            } else {
                Input::Supplied(value)
            },
        )?;
    }
    Ok(Bound {
        inputs,
        failures: Buffer::empty(),
        uncertain: true,
    })
}

pub(super) fn keyword_shape(
    ctx: &mut CallContext,
    facts: &mut Facts,
    keywords: &[Keyword],
) -> Result<Fact> {
    let mut names = Buffer::empty();
    for keyword in keywords {
        ctx.charge(1)?;
        let Node::Symbol(name) = facts.node(keyword.name) else {
            // Keywords whose names are unknown make a hash with unknown keys.
            let mut values = Buffer::with_capacity(ctx, keywords.len())?;
            for keyword in keywords {
                ctx.charge(1)?;
                values.push(ctx, keyword.value)?;
            }
            let value = facts.union(ctx, &values.data)?;
            return facts.hash_kind(ctx, Atom::String.fact(), value, HashKind::PLAIN);
        };
        names.push(ctx, name.clone())?;
    }
    let mut fields = Buffer::empty();
    for (name, keyword) in names.data.iter().zip(keywords) {
        ctx.charge(1)?;
        fields.push(ctx, (name.as_bytes().unwrap(), keyword.value, false))?;
    }
    facts.shape(ctx, &fields.data, false)
}

pub(super) fn general_inputs(
    ctx: &mut CallContext,
    facts: &mut Facts,
    params: &[Parameter],
    contracts: &[Fact],
) -> Result<Buffer<Input>> {
    let mut values = Buffer::empty();
    for param in params {
        ctx.charge(1)?;
        let fact = general_input(ctx, facts, param.kind, param.ty.map(|ty| contracts[ty]))?;
        values.push(
            ctx,
            if param.default {
                Input::Either(fact)
            } else {
                Input::Supplied(fact)
            },
        )?;
    }
    Ok(values)
}

pub(super) fn general_input(
    ctx: &mut CallContext,
    facts: &mut Facts,
    kind: ParamKind,
    contract: Option<Fact>,
) -> Result<Fact> {
    // Rest arguments are assembled before their annotation is applied.
    match kind {
        ParamKind::Rest => facts.array(ctx, Atom::Unknown.fact()),
        ParamKind::KeywordRest => facts.hash_kind(
            ctx,
            Atom::String.fact(),
            Atom::Unknown.fact(),
            HashKind::PLAIN,
        ),
        _ => contract.map_or(Ok(Atom::Unknown.fact()), |expected| {
            facts.value_domain(ctx, expected)
        }),
    }
}
