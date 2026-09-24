//! Keyed reads and rewrites: `dig` on arrays and hashes, the hash members
//! `value?`, `remap_keys` and `flatten`, and `string.byteslice`.
//!
//! None of these members invokes script code. Hash lookups use the stored
//! entries directly, so match data and error objects never fall back to their
//! captures.

use super::{Count, outcome, rejected, unsupported};
use crate::{
    CallContext, Result, Value,
    budget::Buffer,
    bytecode::Method,
    checking::{
        facts::{Atom, Fact, Facts, Field, HashKind, Node},
        scalar::{Operation, Test},
    },
};

impl Facts {
    /// Models `dig(*keys)` on one array or hash arm. Each key steps into the
    /// current value: arrays need an index the runtime converts, where a
    /// fractional float fails and a negative index reads nil, and hashes need
    /// a string or symbol key. A step from any other value ends the walk with
    /// nil before the remaining keys are examined. Past the first step, a key
    /// refused by only some of the values read so far may still fail.
    pub(super) fn dig_member(
        &mut self,
        ctx: &mut CallContext,
        receiver: Fact,
        args: &[Fact],
    ) -> Result<Operation> {
        let mut result = outcome(Atom::Never.fact());
        let mut ended = false;
        let mut current = receiver;
        for (step, &key) in args.iter().enumerate() {
            ctx.charge(1)?;
            let mut selected = Buffer::empty();
            let mut refused = false;
            let mut possible = false;
            for i in 0..self.arm_count(current) {
                ctx.charge(1)?;
                let arm = self.arm(current, i);
                let hash = match self.node(arm) {
                    Node::Atom(Atom::Never) => continue,
                    Node::Array(_) | Node::Tuple(_) => false,
                    Node::Hash(..) | Node::Shape(..) | Node::Protected(..) => true,
                    Node::Atom(Atom::Unknown | Atom::Any) => {
                        // An unknown value may be a collection with a refused key.
                        selected.push(ctx, Atom::Unknown.fact())?;
                        result.throws = true;
                        possible = true;
                        continue;
                    }
                    Node::Named(_) | Node::Nominal { .. } | Node::Choice(_) => {
                        result.unsupported = true;
                        continue;
                    }
                    _ => {
                        ended = true;
                        possible = true;
                        continue;
                    }
                };
                for j in 0..self.arm_count(key) {
                    ctx.charge(1)?;
                    let index = self.arm(key, j);
                    let next = if hash {
                        self.dig_key(ctx, arm, index)?
                    } else {
                        self.dig_index(ctx, arm, index)?
                    };
                    selected.push(ctx, next.value)?;
                    refused |= next.rejected;
                    possible |= next.value != Atom::Never.fact();
                    result.unsupported |= next.unsupported;
                    result.throws |= next.throws;
                }
            }
            if refused {
                if step == 0 || !possible {
                    result.rejected = true;
                } else {
                    result.throws = true;
                }
            }
            current = self.union(ctx, &selected.data)?;
        }
        result.value = if ended {
            self.nullable(ctx, current)?
        } else {
            current
        };
        Ok(result)
    }

    /// One `dig` step into an array arm.
    fn dig_index(&mut self, ctx: &mut CallContext, array: Fact, index: Fact) -> Result<Operation> {
        let fractional = matches!(
            self.node(index),
            Node::Float(bits) if f64::from_bits(*bits).fract() != 0.0
        );
        if fractional {
            return Ok(rejected());
        }
        let throws = match self.count_arm(index, false) {
            Count::Exact(index) if index < 0 => return Ok(outcome(Atom::Nil.fact())),
            Count::Exact(index) => {
                if let Node::Tuple(values) = self.node(array) {
                    let selected = usize::try_from(index)
                        .ok()
                        .and_then(|index| values.data.get(index).copied());
                    return Ok(outcome(selected.unwrap_or(Atom::Nil.fact())));
                }
                false
            }
            Count::Bounded(bounds) => bounds.min.is_none() || bounds.max.is_none(),
            Count::Float | Count::Unknown => true,
            Count::Never => return Ok(outcome(Atom::Never.fact())),
            Count::Invalid => return Ok(rejected()),
            Count::Unsupported => return Ok(unsupported()),
        };
        let element = self.elements(ctx, array)?;
        Ok(Operation {
            throws,
            ..outcome(self.nullable(ctx, element)?)
        })
    }

    /// One `dig` step into a hash arm, reading its stored entries.
    fn dig_key(&mut self, ctx: &mut CallContext, hash: Fact, key: Fact) -> Result<Operation> {
        let (key, throws) = match self.atom(key) {
            Some(Atom::Never) => return Ok(outcome(Atom::Never.fact())),
            Some(Atom::String | Atom::Symbol) => (key, false),
            Some(Atom::Unknown | Atom::Any) => (Atom::String.fact(), true),
            _ if matches!(
                self.node(key),
                Node::Named(_) | Node::Nominal { .. } | Node::Choice(_)
            ) =>
            {
                return Ok(unsupported());
            }
            _ => return Ok(rejected()),
        };
        let field = self.stored_capture_field(ctx, hash, key)?;
        let value = if field.missing && !field.unknown_lookup {
            self.nullable(ctx, field.value)?
        } else {
            field.value
        };
        Ok(Operation {
            throws,
            ..outcome(value)
        })
    }

    /// Models `hash.value?(value)` and `has_value?` with runtime equality.
    pub(super) fn has_value_member(
        &mut self,
        ctx: &mut CallContext,
        receiver: Fact,
        value: Fact,
    ) -> Result<Operation> {
        let mut possible = false;
        match self.node(receiver) {
            Node::Shape(fields, open, ..) => {
                possible |= *open;
                let mut stored = Buffer::with_capacity(ctx, fields.data.len())?;
                for field in &fields.data {
                    ctx.charge(1)?;
                    stored.push(ctx, (field.value, field.optional))?;
                }
                for (field, optional) in stored.data {
                    ctx.charge(1)?;
                    match self.definitely_equal(field, value) {
                        Some(true) if !optional => return Ok(outcome(self.boolean(ctx, true)?)),
                        Some(false) => (),
                        _ => possible = true,
                    }
                }
            }
            Node::Hash(_, values, _) => {
                possible = *values != Atom::Never.fact()
                    && self.definitely_equal(*values, value) != Some(false);
            }
            _ => unreachable!(),
        }
        Ok(outcome(if possible {
            Atom::Bool.fact()
        } else {
            self.boolean(ctx, false)?
        }))
    }

    /// Models `hash.remap_keys(mapping)`: entries whose key the mapping names
    /// move to the mapped string or symbol, stored as a string key; the others
    /// keep their key. A later entry mapped onto an existing key replaces its
    /// value. The mapping must be a hash.
    pub(super) fn remap_keys_member(
        &mut self,
        ctx: &mut CallContext,
        receiver: Fact,
        mapping: Fact,
    ) -> Result<Operation> {
        let mut result = outcome(Atom::Never.fact());
        let mut hashes = Buffer::empty();
        let mut gradual = false;
        for i in 0..self.arm_count(mapping) {
            ctx.charge(1)?;
            let arm = self.arm(mapping, i);
            match self.node(arm) {
                Node::Atom(Atom::Never) => (),
                Node::Hash(..) | Node::Shape(..) => hashes.push(ctx, arm)?,
                // Protected records are hashes whose stored values are known.
                Node::Protected(shape, ..) => {
                    let shape = *shape;
                    hashes.push(ctx, shape)?;
                }
                Node::Atom(Atom::Unknown | Atom::Any) => {
                    gradual = true;
                    result.throws = true;
                }
                Node::Named(_) | Node::Nominal { .. } | Node::Choice(_) => {
                    result.unsupported = true;
                }
                _ => result.rejected = true,
            }
        }
        if hashes.data.is_empty() && !gradual {
            return Ok(result);
        }
        let mut names = Buffer::empty();
        let mut literal = !gradual && hashes.data.len() == 1;
        for &hash in &hashes.data {
            ctx.charge(1)?;
            let values = self.stored_values(ctx, hash)?;
            for i in 0..self.arm_count(values) {
                ctx.charge(1)?;
                let arm = self.arm(values, i);
                match self.node(arm) {
                    Node::String(_) | Node::Symbol(_) | Node::Atom(Atom::Never) => (),
                    Node::Atom(Atom::String | Atom::Symbol) => literal = false,
                    Node::Named(_) | Node::Nominal { .. } | Node::Choice(_) => {
                        result.unsupported = true;
                    }
                    // Only an entry whose key the mapping names reads its value.
                    _ => {
                        literal = false;
                        result.throws = true;
                    }
                }
            }
            if let Node::Shape(fields, open, ..) = self.node(hash) {
                literal &= !*open;
                for field in &fields.data {
                    ctx.charge(1)?;
                    literal &= !field.optional
                        && matches!(self.node(field.value), Node::String(_) | Node::Symbol(_));
                    names.push(ctx, (field.name.clone(), field.value))?;
                }
            } else {
                literal = false;
            }
        }
        result.value = match self.remapped_shape(ctx, receiver, &names.data, literal)? {
            Some(value) => value,
            None => {
                let keys = match self.node(receiver) {
                    Node::Hash(keys, ..) | Node::Shape(_, _, keys, _) => *keys,
                    _ => unreachable!(),
                };
                let keys = self.union(ctx, &[keys, Atom::String.fact()])?;
                let values = self.stored_values(ctx, receiver)?;
                if values == Atom::Never.fact() {
                    self.shape_fields(ctx, Buffer::empty(), false, keys, HashKind::PLAIN)?
                } else {
                    self.hash_kind(ctx, keys, values, HashKind::PLAIN)?
                }
            }
        };
        Ok(result)
    }

    /// The exact result of remapping a closed shape of required fields with a
    /// closed mapping of literal names, or `None` when a key is uncertain.
    fn remapped_shape(
        &mut self,
        ctx: &mut CallContext,
        receiver: Fact,
        names: &[(Value, Fact)],
        literal: bool,
    ) -> Result<Option<Fact>> {
        let Node::Shape(fields, false, keys, _) = self.node(receiver) else {
            return Ok(None);
        };
        if !literal {
            return Ok(None);
        }
        let keys = *keys;
        let mut mapped = false;
        let mut remapped = Buffer::with_capacity(ctx, fields.data.len())?;
        for field in &fields.data {
            ctx.charge(1)?;
            if field.optional {
                return Ok(None);
            }
            let bytes = field.name.as_bytes().unwrap();
            let mut name = field.name.clone();
            for (key, value) in names {
                ctx.work_bytes(bytes.len().min(key.as_bytes().unwrap().len()) + 1)?;
                if key.as_bytes().unwrap() == bytes {
                    let (Node::String(target) | Node::Symbol(target)) = self.node(*value) else {
                        unreachable!()
                    };
                    name = target.clone();
                    mapped = true;
                }
            }
            remapped.push(
                ctx,
                Field {
                    name,
                    value: field.value,
                    optional: false,
                },
            )?;
        }
        // Shapes do not record insertion order, which decides a collision.
        for (index, field) in remapped.data.iter().enumerate() {
            let bytes = field.name.as_bytes().unwrap();
            for earlier in &remapped.data[..index] {
                ctx.work_bytes(bytes.len().min(earlier.name.as_bytes().unwrap().len()) + 1)?;
                if earlier.name.as_bytes().unwrap() == bytes {
                    return Ok(None);
                }
            }
        }
        let keys = if mapped {
            self.union(ctx, &[keys, Atom::String.fact()])?
        } else {
            keys
        };
        Ok(Some(self.shape_fields(
            ctx,
            remapped,
            false,
            keys,
            HashKind::PLAIN,
        )?))
    }

    /// Models `hash.flatten(depth = 1)`, which flattens the hash's key-value
    /// pairs like `to_a.flatten(depth)` except that the depth must convert
    /// to an integer; nil is refused.
    pub(super) fn hash_flatten_member(
        &mut self,
        ctx: &mut CallContext,
        receiver: Fact,
        args: &[Fact],
    ) -> Result<Operation> {
        let mut result = outcome(Atom::Never.fact());
        let depth = match args.first() {
            Some(&depth) => {
                let present = self.filter(ctx, depth, Test::Nil, false)?;
                result.rejected |= present != depth;
                if present == Atom::Never.fact() {
                    return Ok(result);
                }
                present
            }
            None => self.integer(ctx, 1)?,
        };
        let iteration = self.iteration(ctx, receiver)?;
        if iteration.unsupported {
            return Ok(unsupported());
        }
        let pairs = if iteration.item == Atom::Never.fact() {
            self.tuple(ctx, &[])?
        } else {
            self.array(ctx, iteration.item)?
        };
        let flattened = self.flatten_member(ctx, pairs, &[depth])?;
        self.merge_operation(ctx, &mut result, flattened)?;
        Ok(result)
    }

    /// Models `string.byteslice(index)`, `(range)` and `(start, length)`,
    /// which select bytes rather than characters. Literal strings and
    /// selectors compute the exact result.
    pub(super) fn byteslice_member(
        &mut self,
        ctx: &mut CallContext,
        receiver: Fact,
        args: &[Fact],
    ) -> Result<Operation> {
        let mut result = outcome(Atom::Never.fact());
        let mut possible = false;
        for i in 0..self.arm_count(args[0]) {
            ctx.charge(1)?;
            let start = self.arm(args[0], i);
            if matches!(self.node(start), Node::Range(..) | Node::Atom(Atom::Range)) {
                if args.len() == 1 {
                    possible = true;
                } else {
                    result.rejected = true;
                }
                continue;
            }
            possible |= self.byte_index(start, &mut result);
        }
        if let Some(&length) = args.get(1) {
            let mut valid = false;
            for i in 0..self.arm_count(length) {
                ctx.charge(1)?;
                let arm = self.arm(length, i);
                valid |= self.byte_index(arm, &mut result);
            }
            possible &= valid;
        }
        if !possible {
            return Ok(result);
        }
        result.value = match self.literal_byteslice(ctx, receiver, args)? {
            Some(value) => value,
            None => self.nullable(ctx, Atom::String.fact())?,
        };
        Ok(result)
    }

    /// Classifies a byteslice position or length; returns whether it can convert.
    fn byte_index(&self, value: Fact, result: &mut Operation) -> bool {
        match self.count_arm(value, false) {
            Count::Exact(_) => true,
            Count::Bounded(bounds) => {
                result.throws |= bounds.min.is_none() || bounds.max.is_none();
                true
            }
            Count::Float | Count::Unknown => {
                result.throws = true;
                true
            }
            Count::Never => false,
            Count::Invalid => {
                result.rejected = true;
                false
            }
            Count::Unsupported => {
                result.unsupported = true;
                false
            }
        }
    }

    fn literal_byteslice(
        &mut self,
        ctx: &mut CallContext,
        receiver: Fact,
        args: &[Fact],
    ) -> Result<Option<Fact>> {
        let Node::String(text) = self.node(receiver) else {
            return Ok(None);
        };
        let text = text.clone();
        let mut values = Buffer::with_capacity(ctx, args.len())?;
        for &arg in args {
            ctx.charge(1)?;
            let value = match self.node(arg) {
                Node::Integer(n) => Value::int(*n),
                Node::Range(start, end, exclusive) => Value::range(*start, *end, *exclusive),
                _ => return Ok(None),
            };
            values.push(ctx, value)?;
        }
        let value = crate::sequence::method(ctx, Method::ByteSlice, text, &values.data)?;
        Ok(Some(match value.as_bytes() {
            Some(bytes) => self.string(ctx, bytes)?,
            None => Atom::Nil.fact(),
        }))
    }
}
