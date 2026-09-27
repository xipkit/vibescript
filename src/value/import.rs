//! Iterative import of host values into a call's budget.
//!
//! Containers are copied with an explicit frame stack held in a [`Buffer`], so the frame
//! storage is charged to the importing call and released with every other partial result
//! when an import fails. Native recursion only remains for leaf kinds whose own import
//! routines recurse through environments, which are bounded separately by
//! `MAX_ENVIRONMENT_DEPTH`.

use super::{Bytes, Heap, Kind, Value};
use crate::{
    CallContext, ErrorKind, Result,
    budget::{Buffer, CHUNK, MAX_VALUE_DEPTH},
    hash::Hash,
    range::Range,
};
use std::sync::Arc;

enum Frame {
    Array {
        source: Arc<Heap<Value>>,
        out: Buffer<Value>,
    },
    Hash {
        source: Arc<Hash>,
        out: Buffer<(Value, Value)>,
        key: Option<Value>,
    },
}

impl Frame {
    fn next_child(&self) -> Option<Value> {
        match self {
            Self::Array { source, out } => source.buffer.data.get(out.data.len()).cloned(),
            Self::Hash { source, out, key } => {
                let (k, v) = source.buffer.data.get(out.data.len())?;
                Some(if key.is_none() { k } else { v }.clone())
            }
        }
    }

    fn accept(&mut self, value: Value) {
        // Output capacity was reserved for every entry when the frame was opened.
        match self {
            Self::Array { out, .. } => out.data.push(value),
            Self::Hash { out, key, .. } => match key.take() {
                None => *key = Some(value),
                Some(k) => out.data.push((k, value)),
            },
        }
    }

    fn finish(self, ctx: &mut CallContext) -> Result<Value> {
        match self {
            Self::Array { out, .. } => Value::from_array(ctx, out),
            Self::Hash { source, out, .. } => {
                let mut hash = Hash::from_entries(ctx, out)?;
                hash.object = source.object;
                hash.tag = source.tag;
                Value::from_hash(ctx, hash)
            }
        }
    }
}

/// The hash keys one import has copied, which the rest of it shares: the
/// records of a JSON-shaped document repeat the same few keys. Short keys
/// of small hashes are kept in a small table by a hash of their bytes, so
/// each lookup reads one slot, and a key another key displaced is simply
/// copied again.
struct Keys {
    slots: Buffer<Option<Value>>,
    /// How many records the import has started, up to two.
    records: u8,
}

impl Keys {
    /// Counts a record whose next key is its first when `first`, and reports
    /// whether the table is in use: from the second record on, when keys
    /// can repeat.
    #[inline(never)]
    fn records(&mut self, first: bool) -> bool {
        if first && self.records < 2 {
            self.records += 1;
        }
        self.records == 2
    }

    const SLOTS: usize = 64;
    const LENGTH: usize = 64;
    /// Hashes with fewer entries than this are records, whose keys the table
    /// shares; larger ones are dictionaries, whose keys it does not look up.
    const RECORD: usize = 16;

    fn new() -> Self {
        Self {
            slots: Buffer::empty(),
            records: 0,
        }
    }

    /// Imports the hash key `key`, sharing an equal key imported before.
    #[inline(never)]
    fn import(&mut self, ctx: &mut CallContext, key: &Value) -> Result<Value> {
        let Kind::Bytes(bytes) = &key.0 else {
            return ctx.import_scalar(key);
        };
        if ctx.owns(&bytes.header) || bytes.data.len() > Self::LENGTH {
            return ctx.import_scalar(key);
        }
        let slot = bytes
            .data
            .iter()
            .fold(0xcbf2_9ce4_8422_2325_u64, |hash, &byte| {
                (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3)
            }) as usize
            % Self::SLOTS;
        if let Some(Some(imported)) = self.slots.data.get(slot) {
            if imported.as_bytes() == Some(bytes.data.as_slice()) {
                return Ok(imported.clone());
            }
        }
        let imported = ctx.import_scalar(key)?;
        if self.slots.data.is_empty() {
            self.slots.ensure(ctx, Self::SLOTS)?;
            self.slots.data.resize(Self::SLOTS, None);
        }
        self.slots.data[slot] = Some(imported.clone());
        Ok(imported)
    }
}

impl CallContext {
    pub(crate) fn import_value(&mut self, value: &Value, rooted: bool) -> Result<Value> {
        self.charge(1)?;
        if value.depth() > MAX_VALUE_DEPTH {
            return self.guard(ErrorKind::Recursion, "value nesting too deep");
        }
        let mut frames: Buffer<Frame> = Buffer::empty();
        let mut keys = Keys::new();
        let mut produced = self.open(value, rooted, &mut frames)?;
        while let Some(frame) = frames.data.last_mut() {
            if let Some(value) = produced.take() {
                frame.accept(value);
            }
            if let Frame::Array { source, out } = frame {
                // Plain scalars import as copies, so a run of them is charged
                // its per-element steps at once and copied into the capacity
                // reserved when the frame opened.
                let rest = &source.buffer.data[out.data.len()..];
                let run = rest
                    .iter()
                    .take(CHUNK)
                    .take_while(|value| {
                        matches!(
                            value.0,
                            Kind::Nil
                                | Kind::Bool(_)
                                | Kind::Int(_)
                                | Kind::Float(_)
                                | Kind::Money(_)
                                | Kind::Duration(_)
                                | Kind::Time(_)
                        )
                    })
                    .count();
                if run > 0 {
                    self.charge_each(run as u64)?;
                    out.data.extend_from_slice(&rest[..run]);
                    continue;
                }
            }
            // Records, unlike dictionaries, repeat their keys.
            let key = matches!(
                frame,
                Frame::Hash { key: None, source, out, .. }
                    if source.buffer.data.len() < Keys::RECORD && keys.records(out.data.is_empty())
            );
            match frame.next_child() {
                Some(child) => {
                    self.charge(1)?;
                    if key {
                        produced = Some(keys.import(self, &child)?);
                    } else {
                        produced = self.open(&child, rooted, &mut frames)?;
                    }
                }
                None => {
                    let frame = frames.data.pop().unwrap();
                    produced = Some(frame.finish(self)?);
                }
            }
        }
        match produced {
            Some(value) => Ok(value),
            None => unreachable!("import loop ends with the root value"),
        }
    }

    /// Imports a leaf, shares a container already charged to this call, or opens a frame.
    fn open(
        &mut self,
        value: &Value,
        rooted: bool,
        frames: &mut Buffer<Frame>,
    ) -> Result<Option<Value>> {
        match &value.0 {
            Kind::Array(heap) => {
                if !rooted && self.owns(&heap.header) {
                    return Ok(Some(value.clone()));
                }
                let out = Buffer::with_capacity(self, heap.buffer.data.len())?;
                frames.push(
                    self,
                    Frame::Array {
                        source: heap.clone(),
                        out,
                    },
                )?;
                Ok(None)
            }
            Kind::Hash(hash) => {
                if !rooted && self.owns(&hash.header) {
                    return Ok(Some(value.clone()));
                }
                let out = Buffer::with_capacity(self, hash.buffer.data.len())?;
                frames.push(
                    self,
                    Frame::Hash {
                        source: hash.clone(),
                        out,
                        key: None,
                    },
                )?;
                Ok(None)
            }
            _ => self.import_scalar(value).map(Some),
        }
    }

    // Both the frame opener and the record key table import scalars; kept
    // inline in each, since an outlined call made every dictionary entry's
    // import slower.
    #[inline(always)]
    fn import_scalar(&mut self, value: &Value) -> Result<Value> {
        match &value.0 {
            Kind::Host(method) => Ok(Value(Kind::Host(crate::capability::BoundMethod::import(
                self, method,
            )?))),
            Kind::Instance(instance) => crate::objects::import(self, instance)
                .map(|instance| Value(Kind::Instance(instance))),
            Kind::Function(function) => Ok(Value(Kind::Function(
                crate::exports::Function::import(self, function)?,
            ))),
            Kind::Namespace(namespace) => Ok(Value(Kind::Namespace(
                crate::namespace::Namespace::import(self, namespace)?,
            ))),
            Kind::Offset(offset) => Ok(Value(Kind::Offset(crate::regex::matches::Offset::import(
                self, offset,
            )?))),
            Kind::Regex(regex) => Ok(Value(Kind::Regex(crate::regex::value::Regex::import(
                self, regex,
            )?))),
            Kind::Shape(shape) => Ok(Value(Kind::Shape(crate::shapes::Shape::import(
                self, shape,
            )?))),
            Kind::Enum(e) => Ok(Value(Kind::Enum(crate::enums::Enumeration::import(
                self, e,
            )?))),
            Kind::EnumMember(m) => Ok(Value(Kind::EnumMember(crate::enums::Member::import(
                self, m,
            )?))),
            Kind::Zoned(time) => Ok(Value(Kind::Zoned(crate::time::Zoned::import(self, time)?))),
            Kind::Big(n) => Ok(Value(Kind::Big(crate::integer::Big::import(self, n)?))),
            Kind::Range(r) => Ok(Value(Kind::Range(Range::import(self, r)?))),
            Kind::Bytes(h) | Kind::Symbol(h) => {
                let bytes = Bytes::import(self, h)?;
                Ok(Value(if matches!(value.0, Kind::Symbol(_)) {
                    Kind::Symbol(bytes)
                } else {
                    Kind::Bytes(bytes)
                }))
            }
            Kind::Array(_) | Kind::Hash(_) => unreachable!("containers open import frames"),
            _ => Ok(value.clone()),
        }
    }
}
