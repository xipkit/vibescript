use crate::{
    CallContext, Error, ErrorKind, Result, Value,
    budget::{Buffer, CHUNK},
    ops,
    value::Kind,
};
use std::{
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
    mem::{discriminant, size_of},
};

const VACANT: u32 = u32::MAX;
/// Bytes hashed from each end of a longer string, symbol or pattern source.
const SAMPLE_BYTES: usize = 256;
/// Elements hashed from each end of a longer array key.
const SAMPLE_ELEMENTS: usize = 16;
/// Hash keys with more entries contribute only their size and kind.
const HASHED_ENTRIES: usize = 32;
/// Words hashed from each end of a big integer no float can equal.
const SAMPLE_WORDS: usize = 16;

/// Reports whether two values are the same set key.
///
/// Scalar set keys separate kinds and collapse NaNs. Nested values use
/// ordinary equality, including numeric coercion and unequal NaNs.
pub(crate) fn same_key(ctx: &mut CallContext, value: &Value, key: &Value) -> Result<bool> {
    if matches!((&value.0, &key.0), (Kind::Float(a), Kind::Float(b)) if a.is_nan() && b.is_nan()) {
        return Ok(true);
    }
    let same_type = discriminant(&value.0) == discriminant(&key.0)
        || (crate::time::stamp(value).is_some() && crate::time::stamp(key).is_some());
    Ok(same_type && ops::equal(ctx, value, key, 0)?)
}

/// Reports whether any of `values` is the same set key as `key`, by scanning.
#[cfg(test)]
pub(crate) fn contains(ctx: &mut CallContext, values: &[Value], key: &Value) -> Result<bool> {
    for value in values {
        ctx.charge(1)?;
        if same_key(ctx, value, key)? {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Hashes a value consistently with [`same_key`]: equal keys always hash alike.
///
/// Long strings, arrays and big integers contribute bounded samples, and
/// nested containers contribute only their kind and size, so hashing one key
/// costs bounded work however large or shared its contents are. Collisions
/// fall back to charged comparisons. The hasher has fixed keys, which keeps
/// probe counts, and so work counters, reproducible.
pub(crate) fn key_hash(ctx: &mut CallContext, value: &Value) -> Result<u64> {
    let mut hasher = DefaultHasher::new();
    // Times and zoned times compare as instants, so they share a kind here.
    if crate::time::stamp(value).is_some() {
        hasher.write_u8(0xff);
    } else {
        discriminant(&value.0).hash(&mut hasher);
    }
    hash_value(ctx, &mut hasher, value, true)?;
    Ok(hasher.finish())
}

/// Feeds `value` in a form consistent with [`ops::equal`]: numbers hash by
/// their exact value whatever their representation, and instants by stamp.
fn hash_value(
    ctx: &mut CallContext,
    hasher: &mut DefaultHasher,
    value: &Value,
    expand: bool,
) -> Result<()> {
    ctx.charge(1)?;
    match &value.0 {
        Kind::Nil => hasher.write_u8(0),
        Kind::Bool(value) => {
            hasher.write_u8(1);
            hasher.write_u8(u8::from(*value));
        }
        Kind::Int(value) => hash_integer(hasher, *value),
        Kind::Float(value) => hash_float(hasher, *value),
        Kind::Big(value) => {
            // Canonical big integers lie outside the compact range, so only a
            // float can equal one, and only when the float is exact.
            if let Some(float) = value.exact_float() {
                hash_float(hasher, float);
            } else {
                hasher.write_u8(3);
                hasher.write_u8(u8::from(value.negative));
                let words = value.words();
                hasher.write_usize(words.len());
                let (head, tail) = sample(words, SAMPLE_WORDS);
                ctx.work_bytes((head.len() + tail.len()) * size_of::<u32>())?;
                head.hash(hasher);
                tail.hash(hasher);
            }
        }
        Kind::Money(value) => {
            hasher.write_u8(5);
            value.hash(hasher);
        }
        Kind::Duration(value) => {
            hasher.write_u8(6);
            hasher.write_i64(*value);
        }
        Kind::Time(_) | Kind::Zoned(_) => {
            hasher.write_u8(7);
            crate::time::stamp(value).hash(hasher);
        }
        Kind::Bytes(value) => {
            hasher.write_u8(8);
            hash_bytes(ctx, hasher, &value.data)?;
        }
        Kind::Symbol(value) => {
            hasher.write_u8(9);
            hash_bytes(ctx, hasher, &value.data)?;
        }
        Kind::Range(value) => {
            hasher.write_u8(10);
            (value.start, value.end, value.exclusive).hash(hasher);
        }
        Kind::Regex(value) => {
            hasher.write_u8(11);
            value.flags().hash(hasher);
            hash_bytes(ctx, hasher, value.source.require_bytes()?)?;
        }
        Kind::Shape(value) => {
            hasher.write_u8(12);
            hash_bytes(ctx, hasher, &value.definition.text)?;
        }
        Kind::EnumMember(value) => {
            hasher.write_u8(13);
            hasher.write_usize(value.index);
        }
        Kind::Array(array) => {
            let values = &array.buffer.data;
            hasher.write_u8(14);
            hasher.write_usize(values.len());
            if expand {
                let (head, tail) = sample(values, SAMPLE_ELEMENTS);
                for value in head.iter().chain(tail) {
                    hash_value(ctx, hasher, value, false)?;
                }
            }
        }
        Kind::Hash(hash) => {
            let entries = &hash.buffer.data;
            hasher.write_u8(15);
            hasher.write_u8(u8::from(hash.object));
            hasher.write_usize(entries.len());
            if expand && entries.len() <= HASHED_ENTRIES {
                // Equal hashes can differ in insertion order, so entries
                // combine commutatively. Keys are unique byte strings.
                let mut sum = 0u64;
                for (key, value) in entries {
                    let mut entry = DefaultHasher::new();
                    hash_bytes(ctx, &mut entry, key.require_bytes()?)?;
                    hash_value(ctx, &mut entry, value, false)?;
                    sum = sum.wrapping_add(entry.finish());
                }
                hasher.write_u64(sum);
            }
        }
        // Identity-compared kinds share a bucket per kind rather than hash
        // addresses, which would make probe counts vary between runs.
        _ => discriminant(&value.0).hash(hasher),
    }
    Ok(())
}

fn hash_integer(hasher: &mut DefaultHasher, value: i64) {
    hasher.write_u8(2);
    hasher.write_i64(value);
}

fn hash_float(hasher: &mut DefaultHasher, value: f64) {
    if value.is_nan() {
        hasher.write_u8(4);
    } else if value.fract() == 0.0 && value >= i64::MIN as f64 && value < -(i64::MIN as f64) {
        // Integral floats equal the integer they convert to; -0.0 becomes 0.
        hash_integer(hasher, value as i64);
    } else {
        hasher.write_u8(4);
        hasher.write_u64(value.to_bits());
    }
}

fn hash_bytes(ctx: &mut CallContext, hasher: &mut DefaultHasher, bytes: &[u8]) -> Result<()> {
    hasher.write_usize(bytes.len());
    let (head, tail) = sample(bytes, SAMPLE_BYTES);
    ctx.work_bytes(head.len() + tail.len())?;
    hasher.write(head);
    hasher.write(tail);
    Ok(())
}

/// Splits a slice into at most `count` leading and `count` trailing items.
fn sample<T>(items: &[T], count: usize) -> (&[T], &[T]) {
    if items.len() <= 2 * count {
        (items, &[])
    } else {
        (&items[..count], &items[items.len() - count..])
    }
}

/// Where a key belongs in an [`Index`].
pub(crate) enum Entry {
    /// The position of an equal key.
    Found(usize),
    /// The vacant slot that [`Index::fill`] records a new key in.
    Vacant(usize),
}

/// An open-addressing table of set keys held in a caller's slice.
///
/// Slots pair 32 bits of each key's hash with its position, so growth never
/// rehashes values. Probes, growth and every bucket comparison are charged;
/// collisions cost work but never change which keys are equal.
pub(crate) struct Index {
    slots: Buffer<(u32, u32)>,
    len: usize,
}

impl Index {
    pub fn new() -> Self {
        Self {
            slots: Buffer::empty(),
            len: 0,
        }
    }

    /// Returns the position in `members` of a key equal to `value`.
    pub fn find(
        &self,
        ctx: &mut CallContext,
        members: &[Value],
        value: &Value,
        hash: u64,
    ) -> Result<Option<usize>> {
        if self.slots.data.is_empty() {
            return Ok(None);
        }
        match self.probe(ctx, members, value, hash)? {
            Entry::Found(position) => Ok(Some(position)),
            Entry::Vacant(_) => Ok(None),
        }
    }

    /// Finds `value` or reserves room for it, returning the slot to fill.
    ///
    /// The slot remains valid until the next call that changes this index.
    pub fn entry(
        &mut self,
        ctx: &mut CallContext,
        members: &[Value],
        value: &Value,
        hash: u64,
    ) -> Result<Entry> {
        self.reserve(ctx)?;
        self.probe(ctx, members, value, hash)
    }

    /// Records the key at `position` in a slot returned by [`Self::entry`].
    pub fn fill(&mut self, slot: usize, hash: u64, position: usize) {
        debug_assert_eq!(self.slots.data[slot].1, VACANT);
        // `reserve` bounds the number of entries, and so positions, below u32::MAX.
        self.slots.data[slot] = (hash as u32, position as u32);
        self.len += 1;
    }

    fn probe(
        &self,
        ctx: &mut CallContext,
        members: &[Value],
        value: &Value,
        hash: u64,
    ) -> Result<Entry> {
        let hash = hash as u32;
        let mask = self.slots.data.len() - 1;
        let mut slot = hash as usize & mask;
        loop {
            ctx.charge(1)?;
            let (stored, position) = self.slots.data[slot];
            if position == VACANT {
                return Ok(Entry::Vacant(slot));
            }
            let position = position as usize;
            if stored == hash && same_key(ctx, &members[position], value)? {
                return Ok(Entry::Found(position));
            }
            slot = (slot + 1) & mask;
        }
    }

    fn reserve(&mut self, ctx: &mut CallContext) -> Result<()> {
        let needed = self.len + 1;
        let capacity = self.slots.data.len();
        if capacity != 0 && needed <= capacity / 4 * 3 {
            return Ok(());
        }
        let Some(size) = needed
            .checked_mul(2)
            .and_then(usize::checked_next_power_of_two)
            .filter(|_| needed < VACANT as usize)
        else {
            return ctx.fail(ErrorKind::Memory, "set index size overflow");
        };
        let size = size.max(8);
        let mut slots = Buffer::with_capacity(ctx, size)?;
        while slots.data.len() < size {
            let end = size.min(slots.data.len() + CHUNK / size_of::<(u32, u32)>());
            ctx.work_bytes((end - slots.data.len()) * size_of::<(u32, u32)>())?;
            slots.data.resize(end, (0, VACANT));
        }
        let mask = size - 1;
        for chunk in self.slots.data.chunks(CHUNK / size_of::<(u32, u32)>()) {
            ctx.work_bytes(std::mem::size_of_val(chunk))?;
            for &(hash, position) in chunk {
                if position == VACANT {
                    continue;
                }
                let mut slot = hash as usize & mask;
                loop {
                    ctx.charge(1)?;
                    if slots.data[slot].1 == VACANT {
                        break;
                    }
                    slot = (slot + 1) & mask;
                }
                slots.data[slot] = (hash, position);
            }
        }
        self.slots = slots;
        Ok(())
    }
}

/// Appends each value whose key `output` does not already hold.
fn append_unique(
    ctx: &mut CallContext,
    index: &mut Index,
    output: &mut Buffer<Value>,
    values: &[Value],
) -> Result<()> {
    for value in values {
        ctx.charge(1)?;
        let hash = key_hash(ctx, value)?;
        if let Entry::Vacant(slot) = index.entry(ctx, &output.data, value, hash)? {
            output.push(ctx, value.clone())?;
            index.fill(slot, hash, output.data.len() - 1);
        }
    }
    Ok(())
}

/// Returns `values` without repeated set keys, keeping first occurrences.
pub(crate) fn unique(ctx: &mut CallContext, values: &[Value]) -> Result<Value> {
    let mut output = Buffer::empty();
    append_unique(ctx, &mut Index::new(), &mut output, values)?;
    Value::from_array(ctx, output)
}

pub(crate) fn call(
    ctx: &mut CallContext,
    name: &str,
    receiver: &Value,
    args: &[Value],
    keywords: bool,
) -> Result<Option<Value>> {
    let Some(array) = receiver.as_array() else {
        return Ok(None);
    };
    if !matches!(name, "union" | "difference") {
        return Ok(None);
    }
    ctx.checkpoint()?;
    if keywords {
        return Err(Error::new(
            ErrorKind::Argument,
            format!("array.{name} does not accept keyword arguments"),
        ));
    }
    for arg in args {
        ctx.charge(1)?;
        require_array(arg)?;
    }
    let value = if name == "union" {
        let mut output = Buffer::empty();
        let mut index = Index::new();
        append_unique(ctx, &mut index, &mut output, array)?;
        for arg in args {
            ctx.charge(1)?;
            append_unique(ctx, &mut index, &mut output, arg.as_array().unwrap())?;
        }
        Value::from_array(ctx, output)?
    } else {
        difference(ctx, array, args)?
    };
    Ok(Some(value))
}

pub(crate) fn binary(
    ctx: &mut CallContext,
    op: &str,
    left: &Value,
    right: &Value,
) -> Result<Value> {
    ctx.checkpoint()?;
    let array = require_array(left)?;
    let other = require_array(right)?;
    if op == "-" {
        return difference(ctx, array, std::slice::from_ref(right));
    }
    let mut output = Buffer::empty();
    if !other.is_empty() && !array.is_empty() {
        // Both indexes are released before the result header is reserved.
        let mut members = Index::new();
        for (position, value) in other.iter().enumerate() {
            ctx.charge(1)?;
            let hash = key_hash(ctx, value)?;
            if let Entry::Vacant(slot) = members.entry(ctx, other, value, hash)? {
                members.fill(slot, hash, position);
            }
        }
        let mut emitted = Index::new();
        for value in array {
            ctx.charge(1)?;
            let hash = key_hash(ctx, value)?;
            if members.find(ctx, other, value, hash)?.is_none() {
                continue;
            }
            if let Entry::Vacant(slot) = emitted.entry(ctx, &output.data, value, hash)? {
                output.push(ctx, value.clone())?;
                emitted.fill(slot, hash, output.data.len() - 1);
            }
        }
    }
    Value::from_array(ctx, output)
}

fn difference(ctx: &mut CallContext, array: &[Value], others: &[Value]) -> Result<Value> {
    if others.is_empty() {
        return ctx.array(array);
    }
    // Distinct keys across every removal argument; values keep their storage.
    let mut members = Buffer::empty();
    let mut removal = Index::new();
    if !array.is_empty() {
        for other in others {
            ctx.charge(1)?;
            for value in other.as_array().unwrap() {
                ctx.charge(1)?;
                let hash = key_hash(ctx, value)?;
                if let Entry::Vacant(slot) = removal.entry(ctx, &members.data, value, hash)? {
                    members.push(ctx, value.clone())?;
                    removal.fill(slot, hash, members.data.len() - 1);
                }
            }
        }
    }
    let mut output = Buffer::empty();
    for value in array {
        ctx.charge(1)?;
        let hash = key_hash(ctx, value)?;
        if removal.find(ctx, &members.data, value, hash)?.is_none() {
            output.push(ctx, value.clone())?;
        }
    }
    drop(removal);
    drop(members);
    Value::from_array(ctx, output)
}

fn require_array(value: &Value) -> Result<&[Value]> {
    value.as_array().ok_or_else(|| {
        Error::new(
            ErrorKind::Type,
            "array set operations require array operands",
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CallOptions, Limits};

    #[test]
    fn small_results_drop_input_backing_and_receive_independent_import_charges() {
        for operation in ["union", "difference", "&", "-"] {
            let mut ctx = CallContext::new(CallOptions::default());
            let large = Value::bytes(vec![b'x'; 65536]);
            let input = if operation == "union" {
                Value::array(vec![Value::int(1); 4096])
            } else {
                Value::array(vec![large.clone(), Value::int(1)])
            };
            let input = ctx.import(&input).unwrap();
            let other = if operation == "&" {
                Value::array(vec![Value::int(1)])
            } else {
                Value::array(vec![large])
            };
            let other = ctx.import(&other).unwrap();
            let result = match operation {
                "&" | "-" => binary(&mut ctx, operation, &input, &other).unwrap(),
                _ => call(
                    &mut ctx,
                    operation,
                    &input,
                    if operation == "union" {
                        &[]
                    } else {
                        std::slice::from_ref(&other)
                    },
                    false,
                )
                .unwrap()
                .unwrap(),
            };
            assert_eq!(result.as_array().unwrap().len(), 1);
            assert_eq!(result.as_array().unwrap()[0].as_int(), Some(1));
            drop(input);
            drop(other);
            assert!(ctx.stats().retained_memory_bytes < 1024);
            let mut imported = CallContext::new(CallOptions::default());
            let copy = imported.import(&result).unwrap();
            assert!(imported.stats().retained_memory_bytes > 0);
            drop(result);
            assert_eq!(ctx.stats().retained_memory_bytes, 0);
            assert_eq!(copy.as_array().unwrap()[0].as_int(), Some(1));
            drop(copy);
            assert_eq!(imported.stats().retained_memory_bytes, 0);
        }
    }

    #[test]
    fn repeated_and_colliding_keys_consume_bounded_work() {
        let long = Value::array(vec![Value::int(0); 4096]);
        let deep = Value::array(vec![long.clone(), long]);
        let mut middle = vec![b'a'; 65536];
        let sampled = Value::bytes(middle.clone());
        middle[32768] = b'b';
        for values in [
            vec![Value::nil(); 4096],
            vec![deep.clone(), deep],
            vec![
                Value::bytes(vec![b'a'; 65536]),
                Value::bytes(vec![b'a'; 65536]),
            ],
            // Equal samples collide, so the comparison reads the middle.
            vec![sampled, Value::bytes(middle)],
        ] {
            let mut ctx = CallContext::new(CallOptions {
                limits: Limits {
                    steps: Some(64),
                    ..Limits::default()
                },
                ..CallOptions::default()
            });
            assert_eq!(
                unique(&mut ctx, &values).unwrap_err().kind,
                ErrorKind::Steps
            );
            assert_eq!(ctx.stats().retained_memory_bytes, 0);
            assert_eq!(ctx.charge(0).unwrap_err().kind, ErrorKind::Steps);
        }
    }

    #[test]
    fn key_hashes_agree_with_set_key_equality() {
        let mut ctx = CallContext::new(CallOptions::default());
        let big = crate::integer::parse(&mut ctx, b"18446744073709551616", 10).unwrap();
        let odd = crate::integer::parse(&mut ctx, b"18446744073709551617", 10).unwrap();
        let huge = crate::integer::parse(&mut ctx, "9".repeat(400).as_bytes(), 10).unwrap();
        let wide = crate::integer::parse(&mut ctx, "9".repeat(401).as_bytes(), 10).unwrap();
        let text = |t: &[u8]| Value::bytes(t.to_vec());
        let mut long = vec![b'x'; 4096];
        let first = text(&long);
        long[2048] = b'y';
        let hash = |entries: Vec<(&str, Value)>| {
            Value::hash(
                entries
                    .into_iter()
                    .map(|(k, v)| (k.as_bytes().to_vec(), v))
                    .collect(),
            )
        };
        let time = Value::time(86400, 5).unwrap();
        let mut values = vec![
            Value::nil(),
            Value::boolean(false),
            Value::boolean(true),
            Value::int(0),
            Value::int(1),
            Value::int(-1),
            Value::int(i64::MIN),
            Value::int(i64::MAX),
            Value::int(1 << 53),
            Value::int((1 << 53) + 1),
            big.clone(),
            odd,
            huge.clone(),
            wide,
            text(b"a"),
            Value::symbol(b"a".to_vec()),
            text(b""),
            first,
            text(&long),
            Value::range(Some(1), Some(3), false),
            Value::range(Some(1), Some(3), true),
            Value::duration(5),
            Value::money(100, "USD").unwrap(),
            Value::money(100, "EUR").unwrap(),
            time.clone(),
            Value::regex(b"a+", "").unwrap(),
            Value::regex(b"a+", "i").unwrap(),
            hash(vec![("a", Value::int(1)), ("b", Value::int(2))]),
            hash(vec![("b", Value::float(2.0)), ("a", Value::int(1))]),
            Value::object(vec![(b"a".to_vec(), Value::int(1))]),
            hash(vec![("a", Value::int(1))]),
        ];
        for f in [
            0.0,
            -0.0,
            1.0,
            -1.0,
            0.5,
            f64::NAN,
            -f64::NAN,
            f64::INFINITY,
            f64::NEG_INFINITY,
            9007199254740992.0,
            9223372036854775808.0,
            -9223372036854775808.0,
            18446744073709551616.0,
            1e300,
        ] {
            values.push(Value::float(f));
        }
        let scalars = values.len();
        // Nested copies compare with numeric coercion and unequal NaNs.
        for i in 0..scalars {
            let value = values[i].clone();
            values.push(Value::array(vec![value.clone()]));
            values.push(Value::array(vec![Value::int(7), value.clone()]));
            values.push(hash(vec![("k", value)]));
        }
        values.push(Value::array((0..40).map(Value::int).collect()));
        values.push(Value::array(
            (0..40)
                .map(|i| {
                    if i == 20 {
                        Value::int(-1)
                    } else {
                        Value::int(i)
                    }
                })
                .collect(),
        ));
        values.push(Value::array(vec![Value::array(vec![Value::int(1)])]));
        values.push(Value::array(vec![Value::array(vec![Value::float(1.0)])]));
        values.push(Value::array(vec![Value::array(vec![Value::int(2)])]));
        values.push(Value::array(vec![huge]));
        let mut equal_pairs = 0;
        for a in &values {
            for b in &values {
                let a_hash = key_hash(&mut ctx, a).unwrap();
                let b_hash = key_hash(&mut ctx, b).unwrap();
                if same_key(&mut ctx, a, b).unwrap() {
                    equal_pairs += 1;
                    assert_eq!(a_hash, b_hash, "{a:?} and {b:?}");
                }
            }
        }
        // Reflexive keys plus the numeric, NaN, instant and ordering pairs.
        assert!(equal_pairs > values.len());
        for (a, b) in [
            (Value::int(1), Value::int(2)),
            (text(b"a"), Value::symbol(b"a".to_vec())),
            (Value::int(1), Value::float(1.0)),
            (big, Value::float(18446744073709551616.0)),
        ] {
            assert!(!same_key(&mut ctx, &a, &b).unwrap());
            assert_ne!(
                key_hash(&mut ctx, &a).unwrap(),
                key_hash(&mut ctx, &b).unwrap()
            );
        }
        let nested = [
            Value::array(vec![Value::int(1)]),
            Value::array(vec![Value::float(1.0)]),
        ];
        assert!(same_key(&mut ctx, &nested[0], &nested[1]).unwrap());
        drop(values);
        drop(nested);
        drop(time);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }

    /// Reports whether two values have the same kinds and payloads throughout,
    /// telling apart the numeric kinds and NaN payloads that equality merges.
    fn identical(a: &Value, b: &Value) -> bool {
        match (&a.0, &b.0) {
            (Kind::Float(x), Kind::Float(y)) => x.to_bits() == y.to_bits(),
            (Kind::Array(x), Kind::Array(y)) => {
                x.buffer.data.len() == y.buffer.data.len()
                    && x.buffer
                        .data
                        .iter()
                        .zip(&y.buffer.data)
                        .all(|(x, y)| identical(x, y))
            }
            (Kind::Hash(x), Kind::Hash(y)) => {
                x.object == y.object
                    && x.buffer.data.len() == y.buffer.data.len()
                    && x.buffer
                        .data
                        .iter()
                        .zip(&y.buffer.data)
                        .all(|(x, y)| x.0.as_bytes() == y.0.as_bytes() && identical(&x.1, &y.1))
            }
            (Kind::Big(x), Kind::Big(y)) => x.negative == y.negative && x.words() == y.words(),
            _ => discriminant(&a.0) == discriminant(&b.0) && format!("{a:?}") == format!("{b:?}"),
        }
    }

    fn linear_union(ctx: &mut CallContext, lists: &[&[Value]]) -> Vec<Value> {
        let mut output: Vec<Value> = Vec::new();
        for value in lists.iter().flat_map(|list| list.iter()) {
            if !contains(ctx, &output, value).unwrap() {
                output.push(value.clone());
            }
        }
        output
    }

    #[test]
    fn hashed_operations_match_linear_scans() {
        let mut ctx = CallContext::new(CallOptions::default());
        let parse = |ctx: &mut CallContext, text: &str| {
            crate::integer::parse(ctx, text.as_bytes(), 10).unwrap()
        };
        let long = vec![b'x'; 600];
        let mut early = long.clone();
        early[300] = b'y';
        let mut late = long.clone();
        late[299] = b'y';
        let mut atoms = vec![
            Value::nil(),
            Value::boolean(true),
            Value::boolean(false),
            Value::int(0),
            Value::int(1),
            Value::int(-1),
            Value::int(2),
            Value::int(1 << 53),
            Value::int((1 << 53) + 1),
            Value::float(0.0),
            Value::float(-0.0),
            Value::float(1.0),
            Value::float(-1.0),
            Value::float(0.5),
            Value::float(f64::NAN),
            Value::float(-f64::NAN),
            Value::float(9007199254740992.0),
            Value::float(18446744073709551616.0),
            Value::float(1180591620717411303424.0),
            parse(&mut ctx, "18446744073709551616"),
            parse(&mut ctx, "18446744073709551617"),
            parse(&mut ctx, "1180591620717411303424"),
            Value::bytes(b"a".to_vec()),
            Value::symbol(b"a".to_vec()),
            Value::bytes(b"b".to_vec()),
            Value::bytes(long),
            Value::bytes(early),
            Value::bytes(late),
            Value::range(Some(1), Some(3), false),
            Value::range(Some(1), Some(3), true),
            Value::duration(5),
            Value::money(1000, "USD").unwrap(),
            Value::money(1000, "EUR").unwrap(),
            Value::time(100, 0).unwrap(),
            Value::time(101, 0).unwrap(),
            Value::regex(b"a+", "").unwrap(),
            Value::regex(b"a+", "i").unwrap(),
        ];
        // Deterministic xorshift, so failures reproduce.
        let mut state = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = move |bound: usize| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state % bound as u64) as usize
        };
        let singles = atoms.len();
        for _ in 0..120 {
            let width = next(4);
            let items = (0..width).map(|_| atoms[next(singles)].clone()).collect();
            atoms.push(Value::array(items));
            let keys = [b"a", b"b", b"c"];
            let mut entries = Vec::new();
            for key in keys.iter().take(next(4)) {
                entries.push((key.to_vec(), atoms[next(atoms.len())].clone()));
            }
            if next(2) == 0 {
                entries.reverse();
            }
            atoms.push(Value::hash(entries));
            // Longer than the sampled prefix and suffix.
            let items = (0..34 + next(4))
                .map(|_| atoms[next(4) + 3].clone())
                .collect();
            atoms.push(Value::array(items));
        }
        for _ in 0..400 {
            let [a, b, c] = [(); 3].map(|()| {
                (0..next(14))
                    .map(|_| atoms[next(atoms.len())].clone())
                    .collect::<Vec<_>>()
            });
            let (av, bv) = (Value::array(a.clone()), Value::array(b.clone()));
            let expected_union = linear_union(&mut ctx, &[&a, &b, &c]);
            let expected_unique = linear_union(&mut ctx, &[&a]);
            let removed = |ctx: &mut CallContext, lists: &[&[Value]]| {
                a.iter()
                    .filter(|v| !lists.iter().any(|l| contains(ctx, l, v).unwrap()))
                    .cloned()
                    .collect::<Vec<_>>()
            };
            let expected_difference = removed(&mut ctx, &[&b, &c]);
            let expected_minus = removed(&mut ctx, &[&b]);
            let mut expected_intersection: Vec<Value> = Vec::new();
            for v in &a {
                if contains(&mut ctx, &b, v).unwrap()
                    && !contains(&mut ctx, &expected_intersection, v).unwrap()
                {
                    expected_intersection.push(v.clone());
                }
            }
            let args = [bv.clone(), Value::array(c.clone())];
            for (actual, expected) in [
                (unique(&mut ctx, &a).unwrap(), expected_unique),
                (
                    call(&mut ctx, "union", &av, &args, false).unwrap().unwrap(),
                    expected_union,
                ),
                (
                    call(&mut ctx, "difference", &av, &args, false)
                        .unwrap()
                        .unwrap(),
                    expected_difference,
                ),
                (binary(&mut ctx, "-", &av, &bv).unwrap(), expected_minus),
                (
                    binary(&mut ctx, "&", &av, &bv).unwrap(),
                    expected_intersection,
                ),
            ] {
                let actual = actual.as_array().unwrap();
                assert_eq!(actual.len(), expected.len(), "{a:?} {b:?} {c:?}");
                for (x, y) in actual.iter().zip(&expected) {
                    assert!(identical(x, y), "{x:?} != {y:?} in {a:?} {b:?} {c:?}");
                }
            }
        }
    }

    #[test]
    fn index_storage_is_reserved_and_released() {
        let values = (0..4096).map(Value::int).collect::<Vec<_>>();
        let mut ctx = CallContext::new(CallOptions::default());
        let result = unique(&mut ctx, &values).unwrap();
        assert_eq!(result.as_array().unwrap().len(), 4096);
        let retained = ctx.stats().retained_memory_bytes;
        // The index held 8192 eight-byte slots beside the result.
        assert!(ctx.stats().peak_memory_bytes >= retained + 8192 * 8);
        drop(result);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);

        let mut ctx = CallContext::new(CallOptions::default());
        let steps = |ctx: &mut CallContext, count: usize| {
            let before = ctx.stats().steps;
            unique(ctx, &values[..count]).unwrap();
            ctx.stats().steps - before
        };
        let (small, large) = (steps(&mut ctx, 2048), steps(&mut ctx, 4096));
        assert!(large < small * 9 / 4, "{small} then {large}");
    }

    #[test]
    fn partial_results_are_released_after_step_and_memory_failures() {
        let input = Value::array((0..128).map(Value::int).collect::<Vec<_>>());
        let other = input.clone();
        for operation in ["union", "difference", "&", "-"] {
            for kind in [ErrorKind::Steps, ErrorKind::Memory] {
                let mut ctx = CallContext::new(CallOptions {
                    limits: Limits {
                        steps: (kind == ErrorKind::Steps).then_some(64),
                        memory_bytes: (kind == ErrorKind::Memory).then_some(128),
                        ..Limits::default()
                    },
                    ..CallOptions::default()
                });
                let empty = Value::array(Vec::new());
                let result = match operation {
                    "&" => binary(&mut ctx, operation, &input, &other),
                    "-" => binary(&mut ctx, operation, &input, &empty),
                    _ => call(&mut ctx, operation, &input, &[], false).map(Option::unwrap),
                };
                assert_eq!(result.unwrap_err().kind, kind, "{operation}");
                assert_eq!(ctx.stats().retained_memory_bytes, 0);
                assert_eq!(ctx.charge(0).unwrap_err().kind, kind);
            }
        }
    }

    #[test]
    fn cancelled_and_expired_operations_stop_even_on_empty_inputs() {
        let empty = Value::array(Vec::new());
        for operation in ["union", "difference", "&", "-"] {
            for kind in [ErrorKind::Cancelled, ErrorKind::Deadline] {
                let mut options = CallOptions::default();
                if kind == ErrorKind::Cancelled {
                    options.cancellation.cancel();
                } else {
                    options.deadline = Some(std::time::Instant::now());
                }
                let mut ctx = CallContext::new(options);
                let result = match operation {
                    "&" | "-" => binary(&mut ctx, operation, &empty, &empty),
                    _ => call(&mut ctx, operation, &empty, &[], false).map(Option::unwrap),
                };
                assert_eq!(result.unwrap_err().kind, kind);
                assert_eq!(ctx.stats().peak_memory_bytes, 0);
                assert_eq!(ctx.charge(0).unwrap_err().kind, kind);
            }
        }
    }
}
