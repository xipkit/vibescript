//! Protected object profiles admitted by structural contracts.
//!
//! A `hash` or shape annotation without a concrete value may be satisfied at
//! runtime by a plain hash, a host object, a match object or a rescued error.
//! The two protected forms have fixed field profiles, so a contract admits one
//! when the profile passes boundary normalization without losing protection.
//! The admitted alternative is the profile refined by the contract:
//! fields keep the profile's values where the contract accepts them and shrink
//! to the contract's domain otherwise, so `captures:array<int>` admits only a
//! zero-capture match and `named_captures:{x?:int}` only an empty map.
//!
//! The match profile's fields also constrain each other: every named capture
//! value is one of the captures array's elements, and a match without a
//! single group has an empty named map. Refinement is fieldwise, so a final
//! consistency pass prunes admitted match alternatives whose refined fields
//! no runtime match could carry together and narrows the survivors.

use super::facts::{Atom, Certainty, Fact, Facts, Field, HashKind, Node};
use crate::{CallContext, ErrorKind, Result, budget::Buffer, hash::Tag};

const UNBUILT: Fact = Fact(usize::MAX);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Key {
    contract: Fact,
    tag: Tag,
}

impl Key {
    fn bucket(self, mask: usize) -> usize {
        self.contract
            .0
            .wrapping_mul(0x9e3779b1)
            .wrapping_add((self.tag as usize).wrapping_mul(0x85ebca77))
            & mask
    }
}

#[derive(Debug)]
struct Entry {
    key: Key,
    value: Fact,
    next: usize,
}

/// The constraint a refined captures array imposes on the named captures of
/// the same match.
enum Captures {
    /// No element can exist, so the array is exactly empty.
    Empty,
    /// Every element lies in this domain.
    Elements(Fact),
    /// The refined fact admits elements of no derivable domain.
    Unknown,
}

const NAMED_CAPTURES: &[u8] = b"named_captures";

/// A chained, metered table from `(contract, tag)` to the admitted protected
/// alternative, so repeated dispatch on one contract is amortised constant
/// time.
#[derive(Debug)]
pub(super) struct Memo {
    entries: Buffer<Entry>,
    buckets: Buffer<usize>,
}

impl Memo {
    pub fn new() -> Self {
        Self {
            entries: Buffer::empty(),
            buckets: Buffer::empty(),
        }
    }

    fn get(&self, ctx: &mut CallContext, key: Key) -> Result<Option<Fact>> {
        ctx.charge(1)?;
        if self.buckets.data.is_empty() {
            return Ok(None);
        }
        let mut index = self.buckets.data[key.bucket(self.buckets.data.len() - 1)];
        while index != usize::MAX {
            ctx.charge(1)?;
            let entry = &self.entries.data[index];
            if entry.key == key {
                return Ok(Some(entry.value));
            }
            index = entry.next;
        }
        Ok(None)
    }

    fn insert(&mut self, ctx: &mut CallContext, key: Key, value: Fact) -> Result<()> {
        if self.entries.data.len() >= self.buckets.data.len() / 2 {
            let Some(capacity) = self.buckets.data.len().max(8).checked_mul(2) else {
                return ctx.fail(ErrorKind::Memory, "checker profile table size overflow");
            };
            let mut buckets = Buffer::with_capacity(ctx, capacity)?;
            ctx.charge(capacity as u64 + self.entries.data.len() as u64)?;
            buckets.data.resize(capacity, usize::MAX);
            for (index, entry) in self.entries.data.iter_mut().enumerate() {
                let bucket = entry.key.bucket(capacity - 1);
                entry.next = buckets.data[bucket];
                buckets.data[bucket] = index;
            }
            self.buckets = buckets;
        }
        let bucket = key.bucket(self.buckets.data.len() - 1);
        let index = self.entries.data.len();
        self.entries.push(
            ctx,
            Entry {
                key,
                value,
                next: self.buckets.data[bucket],
            },
        )?;
        self.buckets.data[bucket] = index;
        Ok(())
    }
}

fn slot(tag: Tag) -> usize {
    match tag {
        Tag::Match => 0,
        Tag::Error => 1,
        Tag::None => unreachable!(),
    }
}

impl Facts {
    /// The field profile every protected object of `tag` carries at runtime:
    /// the seven match fields produced by `String#match` or the six fields of
    /// a rescued error, as an object-provenance closed shape.
    pub(super) fn tag_profile(&mut self, ctx: &mut CallContext, tag: Tag) -> Result<Fact> {
        let index = slot(tag);
        if self.profiles[index] != UNBUILT {
            return Ok(self.profiles[index]);
        }
        let string = Atom::String.fact();
        let mut fields = Buffer::empty();
        let mut push = |ctx: &mut CallContext, name: &str, value: Fact| -> Result<()> {
            let name = ctx.bytes(name.as_bytes())?;
            fields.push(
                ctx,
                Field {
                    name,
                    value,
                    optional: false,
                },
            )
        };
        match tag {
            Tag::Match => {
                let capture = self.nullable(ctx, string)?;
                let captures = self.array(ctx, capture)?;
                let named = self.hash_kind(ctx, string, capture, HashKind::PLAIN)?;
                let position = self.nullable(ctx, Atom::Int.fact())?;
                let positions = self.array(ctx, position)?;
                let offset = self.offset(ctx, positions)?;
                push(ctx, "begin", offset)?;
                push(ctx, "captures", captures)?;
                push(ctx, "end", offset)?;
                push(ctx, "named_captures", named)?;
                push(ctx, "post_match", string)?;
                push(ctx, "pre_match", string)?;
                push(ctx, "to_s", string)?;
            }
            Tag::Error => {
                let backtrace = self.array(ctx, string)?;
                push(ctx, "type", string)?;
                push(ctx, "class", string)?;
                push(ctx, "message", string)?;
                push(ctx, "to_s", string)?;
                push(ctx, "code_frame", string)?;
                push(ctx, "backtrace", backtrace)?;
            }
            Tag::None => unreachable!(),
        }
        let profile = self.shape_fields(ctx, fields, false, string, HashKind::OBJECT)?;
        self.profiles[index] = profile;
        Ok(profile)
    }

    /// The protected alternative `contract` admits for `tag`, or never when the
    /// profile cannot pass the contract.
    ///
    /// The result is the profile normalized against the contract exactly as the
    /// runtime boundary would validate an actual object, wrapped as a contract
    /// alternative: certainly read-only, but never a static contradiction.
    /// Admitted match alternatives additionally pass the cross-field
    /// consistency check, so fields that constrain each other are only ever
    /// admitted together.
    pub(super) fn protected_variant(
        &mut self,
        ctx: &mut CallContext,
        contract: Fact,
        tag: Tag,
    ) -> Result<Fact> {
        ctx.checkpoint()?;
        let key = Key { contract, tag };
        if let Some(value) = self.variants.get(ctx, key)? {
            return Ok(value);
        }
        let profile = self.tag_profile(ctx, tag)?;
        let protected = self.protected_as(ctx, profile, tag, Certainty::Contract)?;
        let refined = self.normalized(ctx, protected, contract)?;
        let value = self.consistent_alternatives(ctx, refined)?;
        self.variants.insert(ctx, key, value)?;
        Ok(value)
    }

    /// Prunes the contract-admitted match alternatives whose refined fields
    /// contradict each other, so no rescue arm stays reachable through a
    /// profile no runtime match could carry.
    ///
    /// Fieldwise refinement leaves cross-field contradictions alive: a match
    /// whose captures array cannot hold one element keeps a required named
    /// capture, or non-nullable captures keep a nil named value. Runtime
    /// match objects can do neither, because every named capture value is
    /// one of the captures array's elements and a match without a single
    /// group has an empty named map. Each admitted match shape is rechecked
    /// against those invariants. Boundary conversions that remove the tag are
    /// already represented by the ordinary object alternative.
    fn consistent_alternatives(&mut self, ctx: &mut CallContext, refined: Fact) -> Result<Fact> {
        let mut arms = Buffer::empty();
        for index in 0..self.arm_count(refined) {
            ctx.charge(1)?;
            let arm = self.arm(refined, index);
            let shape = match self.node(arm) {
                Node::Protected(shape, Tag::Match, Certainty::Contract) => Some(*shape),
                Node::Protected(..) => None,
                _ => continue,
            };
            let arm = match shape {
                Some(shape) => {
                    let refined = self.consistent_match(ctx, shape)?;
                    if refined == Atom::Never.fact() {
                        Atom::Never.fact()
                    } else if refined == shape {
                        arm
                    } else {
                        self.protected_as(ctx, refined, Tag::Match, Certainty::Contract)?
                    }
                }
                None => arm,
            };
            if arm != Atom::Never.fact() {
                arms.push(ctx, arm)?;
            }
        }
        self.union(ctx, &arms.data)
    }

    /// Refines one contract-admitted match shape against the invariants its
    /// captures fields impose on each other, or never when no runtime match
    /// can carry the refined fields together.
    fn consistent_match(&mut self, ctx: &mut CallContext, shape: Fact) -> Result<Fact> {
        if !matches!(self.node(shape), Node::Shape(..)) {
            return Ok(shape);
        }
        let Some((captures, _)) = self.selected_field(ctx, shape, b"captures")? else {
            return Ok(shape);
        };
        let Some((named, _)) = self.selected_field(ctx, shape, NAMED_CAPTURES)? else {
            return Ok(shape);
        };
        let refined = match self.capture_domain(ctx, captures)? {
            Captures::Empty => {
                // There are no groups, so the runtime named map is exactly
                // the empty hash; the shape survives only when that value
                // fits the refined named domain.
                let empty = self.shape(ctx, &[], false)?;
                self.normalized(ctx, empty, named)?
            }
            Captures::Elements(domain) => self.named_within(ctx, named, domain)?,
            Captures::Unknown => return Ok(shape),
        };
        if refined == Atom::Never.fact() {
            return Ok(refined);
        }
        if refined == named {
            return Ok(shape);
        }
        let Node::Shape(fields, ..) = self.node(shape) else {
            unreachable!()
        };
        let mut found = None;
        for (index, field) in fields.data.iter().enumerate() {
            ctx.charge(1)?;
            if field.name.as_bytes() == Some(NAMED_CAPTURES) {
                found = Some(index);
                break;
            }
        }
        match found {
            Some(index) => self.replace(ctx, shape, index, Some(refined)),
            None => Ok(shape),
        }
    }

    /// The domain every captures element of a refined match lies in, or no
    /// constraint when the refined array fact admits elements of no
    /// derivable domain.
    fn capture_domain(&mut self, ctx: &mut CallContext, captures: Fact) -> Result<Captures> {
        let mut domains = Buffer::empty();
        let mut work = Buffer::empty();
        work.push(ctx, captures)?;
        while let Some(captures) = work.data.pop() {
            ctx.charge(1)?;
            match self.node(captures) {
                Node::Tuple(items) => {
                    domains.extend(ctx, &items.data)?;
                }
                Node::Array(element) => {
                    if *element != Atom::Never.fact() {
                        domains.push(ctx, *element)?;
                    }
                }
                Node::Union(arms) | Node::Choice(arms) => {
                    work.extend(ctx, &arms.data)?;
                }
                _ => return Ok(Captures::Unknown),
            }
        }
        if domains.data.is_empty() {
            Ok(Captures::Empty)
        } else {
            Ok(Captures::Elements(self.union(ctx, &domains.data)?))
        }
    }

    /// Narrows a refined named-capture map to values that can also occur in
    /// the captures array, or never when no map of the refined domain fits.
    ///
    /// Every named value is one of the captures elements, so each named
    /// domain shrinks to its overlap with the captures element domain; the
    /// unnamed elements are untouched, which is why only the named map is
    /// narrowed and never the captures array. Facts without a structural
    /// map form pass through unchanged, keeping gradual contracts gradual.
    fn named_within(&mut self, ctx: &mut CallContext, named: Fact, domain: Fact) -> Result<Fact> {
        enum Form {
            Values(Fact, Fact, HashKind),
            Fields(Buffer<Field>, bool, Fact, HashKind),
            Arms(Buffer<Fact>),
        }
        let mut results = Buffer::empty();
        let mut work = Buffer::empty();
        work.push(ctx, named)?;
        while let Some(named) = work.data.pop() {
            ctx.charge(1)?;
            let form = match self.node(named) {
                Node::Hash(keys, values, kind) => Form::Values(*keys, *values, *kind),
                Node::Shape(fields, open, keys, kind) => {
                    let mut copied = Buffer::with_capacity(ctx, fields.data.len())?;
                    for field in &fields.data {
                        ctx.charge(1)?;
                        copied.push(
                            ctx,
                            Field {
                                name: field.name.clone(),
                                value: field.value,
                                optional: field.optional,
                            },
                        )?;
                    }
                    Form::Fields(copied, *open, *keys, *kind)
                }
                Node::Union(arms) => {
                    let mut copied = Buffer::with_capacity(ctx, arms.data.len())?;
                    for &arm in &arms.data {
                        ctx.charge(1)?;
                        copied.push(ctx, arm)?;
                    }
                    Form::Arms(copied)
                }
                _ => {
                    results.push(ctx, named)?;
                    continue;
                }
            };
            match form {
                Form::Values(keys, values, kind) => {
                    let values = self.normalized(ctx, values, domain)?;
                    if values == Atom::Never.fact() {
                        // No value can occur, so no key can occur either.
                        let empty = self.shape_fields(
                            ctx,
                            Buffer::empty(),
                            false,
                            Atom::Never.fact(),
                            kind,
                        )?;
                        results.push(ctx, empty)?;
                    } else {
                        let map = self.hash_kind(ctx, keys, values, kind)?;
                        results.push(ctx, map)?;
                    }
                }
                Form::Fields(mut fields, open, keys, kind) => {
                    let mut refined = Buffer::with_capacity(ctx, fields.data.len())?;
                    let mut possible = true;
                    for mut field in fields.data.drain(..) {
                        ctx.charge(1)?;
                        let value = self.normalized(ctx, field.value, domain)?;
                        if value == Atom::Never.fact() {
                            // A named field no capture element can satisfy
                            // kills this arm when required; optional ones
                            // survive as absent.
                            if !field.optional {
                                possible = false;
                                break;
                            }
                            if open {
                                field.value = value;
                                refined.push(ctx, field)?;
                            }
                            continue;
                        }
                        field.value = value;
                        refined.push(ctx, field)?;
                    }
                    if possible {
                        let shape = self.shape_fields(ctx, refined, open, keys, kind)?;
                        results.push(ctx, shape)?;
                    }
                }
                Form::Arms(arms) => {
                    work.extend(ctx, &arms.data)?;
                }
            }
        }
        self.union(ctx, &results.data)
    }

    /// Drops the tag bits of a contract hash whose profile the contract cannot
    /// admit, so later dispatch only expands protected alternatives that a
    /// runtime input could actually be.
    pub(super) fn pruned_contract(
        &mut self,
        ctx: &mut CallContext,
        contract: Fact,
    ) -> Result<Fact> {
        let kind = match self.node(contract) {
            Node::Hash(_, _, kind) | Node::Shape(_, _, _, kind) => *kind,
            _ => return Ok(contract),
        };
        if !kind.tagged() {
            return Ok(contract);
        }
        let mut admitted = kind.untagged();
        for (tag, bit) in [(Tag::Match, HashKind::MATCH), (Tag::Error, HashKind::ERROR)] {
            ctx.charge(1)?;
            if kind.has(bit) && self.protected_variant(ctx, contract, tag)? != Atom::Never.fact() {
                admitted = admitted.join(bit);
            }
        }
        if admitted == kind {
            Ok(contract)
        } else {
            self.hash_as(ctx, contract, admitted)
        }
    }
}
