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
    budget::{Buffer, MAX_VALUE_DEPTH},
    hash::Hash,
    range::Range,
};

enum Frame<'a> {
    Array {
        source: &'a Heap<Value>,
        out: Buffer<Value>,
    },
    Hash {
        source: &'a Hash,
        out: Buffer<(Value, Value)>,
        key: Option<Value>,
    },
}

impl<'a> Frame<'a> {
    fn next_child(&self) -> Option<&'a Value> {
        match self {
            Self::Array { source, out } => {
                let source: &'a Heap<Value> = source;
                source.buffer.data.get(out.data.len())
            }
            Self::Hash { source, out, key } => {
                let source: &'a Hash = source;
                let (k, v) = source.buffer.data.get(out.data.len())?;
                Some(if key.is_none() { k } else { v })
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

impl CallContext {
    pub(crate) fn import_value(&mut self, value: &Value, rooted: bool) -> Result<Value> {
        self.charge(1)?;
        if value.depth() > MAX_VALUE_DEPTH {
            return self.guard(ErrorKind::Recursion, "value nesting too deep");
        }
        let mut frames: Buffer<Frame<'_>> = Buffer::empty();
        let mut produced = self.open(value, rooted, &mut frames)?;
        while let Some(frame) = frames.data.last_mut() {
            if let Some(value) = produced.take() {
                frame.accept(value);
            }
            match frame.next_child() {
                Some(child) => {
                    self.charge(1)?;
                    produced = self.open(child, rooted, &mut frames)?;
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
    fn open<'a>(
        &mut self,
        value: &'a Value,
        rooted: bool,
        frames: &mut Buffer<Frame<'a>>,
    ) -> Result<Option<Value>> {
        match &value.0 {
            Kind::Array(heap) => {
                if !rooted && self.owns(&heap.header) {
                    return Ok(Some(value.clone()));
                }
                let out = Buffer::with_capacity(self, heap.buffer.data.len())?;
                frames.push(self, Frame::Array { source: heap, out })?;
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
                        source: hash,
                        out,
                        key: None,
                    },
                )?;
                Ok(None)
            }
            _ => self.import_scalar(value).map(Some),
        }
    }

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
