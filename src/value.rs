use crate::{
    CallContext, Error, ErrorKind, Result,
    budget::{Buffer, Charge, MAX_VALUE_DEPTH},
};
use std::{fmt, mem::size_of, sync::Arc};

#[derive(Debug)]
pub(crate) struct Heap<T> {
    pub buffer: Buffer<T>,
    header: Option<Charge>,
    depth: usize,
}

impl<T> Heap<T> {
    pub fn new(ctx: &mut CallContext, buffer: Buffer<T>, depth: usize) -> Result<Arc<Self>> {
        if depth > MAX_VALUE_DEPTH {
            return ctx.fail(ErrorKind::Recursion, "value nesting too deep");
        }
        let header = ctx.reserve(size_of::<Self>() + 2 * size_of::<usize>())?;
        Ok(Arc::new(Self {
            buffer,
            header,
            depth,
        }))
    }
    fn untracked(data: Vec<T>, depth: usize) -> Arc<Self> {
        Arc::new(Self {
            buffer: Buffer::untracked(data),
            header: None,
            depth,
        })
    }
}

impl<T: Clone> Heap<T> {
    fn make_mut<'a>(
        ctx: &mut CallContext,
        heap: &'a mut Arc<Self>,
        capacity: usize,
    ) -> Result<&'a mut Self> {
        if Arc::get_mut(heap).is_none() {
            let mut buffer = Buffer::with_capacity(ctx, capacity)?;
            buffer.extend(ctx, &heap.buffer.data)?;
            *heap = Self::new(ctx, buffer, heap.depth)?;
        }
        Ok(Arc::get_mut(heap).unwrap())
    }
}

#[derive(Clone, Debug)]
pub(crate) enum Kind {
    Nil,
    Bool(bool),
    Int(i64),
    Float(f64),
    Bytes(Arc<Heap<u8>>),
    Symbol(Arc<Heap<u8>>),
    Array(Arc<Heap<Value>>),
    Hash(Arc<Heap<(Value, Value)>>),
}

/// An immutable Vibescript value. Clones share storage; script updates preserve each clone's value.
#[derive(Clone, Debug)]
pub struct Value(pub(crate) Kind);

impl Default for Value {
    fn default() -> Self {
        Self::nil()
    }
}

impl Value {
    /// Creates nil.
    pub const fn nil() -> Self {
        Self(Kind::Nil)
    }
    /// Creates a boolean.
    pub const fn boolean(value: bool) -> Self {
        Self(Kind::Bool(value))
    }
    /// Creates a signed integer. This core reports overflow instead of promoting to bignum.
    pub const fn int(value: i64) -> Self {
        Self(Kind::Int(value))
    }
    /// Creates a floating-point value.
    pub const fn float(value: f64) -> Self {
        Self(Kind::Float(value))
    }
    /// Creates caller-owned bytes; importing them into a call is accounted separately.
    pub fn bytes(value: impl Into<Vec<u8>>) -> Self {
        Self(Kind::Bytes(Heap::untracked(value.into(), 0)))
    }
    /// Creates a symbol.
    pub fn symbol(value: impl Into<Vec<u8>>) -> Self {
        Self(Kind::Symbol(Heap::untracked(value.into(), 0)))
    }
    /// Creates a caller-owned array.
    pub fn array(values: Vec<Value>) -> Self {
        let depth = 1 + values.iter().map(Self::depth).max().unwrap_or(0);
        Self(Kind::Array(Heap::untracked(values, depth)))
    }
    /// Creates a caller-owned insertion-ordered hash with string keys, replacing duplicate values.
    pub fn hash(entries: Vec<(Vec<u8>, Value)>) -> Self {
        let mut values: Vec<(Value, Value)> = Vec::with_capacity(entries.len());
        for (key, value) in entries {
            if let Some(entry) = values
                .iter_mut()
                .find(|(k, _)| k.as_bytes() == Some(key.as_slice()))
            {
                entry.1 = value;
            } else {
                values.push((Self::bytes(key), value));
            }
        }
        let depth = 1 + values.iter().map(|(_, v)| v.depth()).max().unwrap_or(0);
        Self(Kind::Hash(Heap::untracked(values, depth)))
    }
    /// Returns an integer if this value is an integer.
    pub fn as_int(&self) -> Option<i64> {
        if let Kind::Int(n) = self.0 {
            Some(n)
        } else {
            None
        }
    }
    /// Returns a number converted to f64, if numeric.
    pub fn as_float(&self) -> Option<f64> {
        match self.0 {
            Kind::Int(n) => Some(n as f64),
            Kind::Float(n) => Some(n),
            _ => None,
        }
    }
    /// Returns the raw bytes of a string or symbol, including invalid UTF-8.
    pub fn as_bytes(&self) -> Option<&[u8]> {
        match &self.0 {
            Kind::Bytes(h) | Kind::Symbol(h) => Some(&h.buffer.data),
            _ => None,
        }
    }
    /// Returns the elements of an array.
    pub fn as_array(&self) -> Option<&[Value]> {
        if let Kind::Array(h) = &self.0 {
            Some(&h.buffer.data)
        } else {
            None
        }
    }
    /// Returns insertion-ordered hash entries.
    pub fn as_hash(&self) -> Option<&[(Value, Value)]> {
        if let Kind::Hash(h) = &self.0 {
            Some(&h.buffer.data)
        } else {
            None
        }
    }
    /// Reports Vibescript truthiness: only nil and false are false.
    pub fn truthy(&self) -> bool {
        !matches!(self.0, Kind::Nil | Kind::Bool(false))
    }
    /// Reports this value's language type.
    pub fn type_name(&self) -> &'static str {
        match self.0 {
            Kind::Nil => "nil",
            Kind::Bool(_) => "bool",
            Kind::Int(_) => "int",
            Kind::Float(_) => "float",
            Kind::Bytes(_) => "string",
            Kind::Symbol(_) => "symbol",
            Kind::Array(_) => "array",
            Kind::Hash(_) => "hash",
        }
    }

    pub(crate) fn copy_bytes(ctx: &mut CallContext, bytes: &[u8], symbol: bool) -> Result<Self> {
        let mut buf = Buffer::with_capacity(ctx, bytes.len())?;
        buf.extend(ctx, bytes)?;
        let heap = Heap::new(ctx, buf, 0)?;
        Ok(Self(if symbol {
            Kind::Symbol(heap)
        } else {
            Kind::Bytes(heap)
        }))
    }
    pub(crate) fn from_bytes(ctx: &mut CallContext, bytes: Buffer<u8>) -> Result<Self> {
        Ok(Self(Kind::Bytes(Heap::new(ctx, bytes, 0)?)))
    }
    pub(crate) fn from_array(ctx: &mut CallContext, values: Buffer<Value>) -> Result<Self> {
        let mut depth = 1;
        for v in &values.data {
            ctx.charge(1)?;
            depth = depth.max(v.depth() + 1);
        }
        Ok(Self(Kind::Array(Heap::new(ctx, values, depth)?)))
    }
    pub(crate) fn from_hash(ctx: &mut CallContext, values: Buffer<(Value, Value)>) -> Result<Self> {
        let mut depth = 1;
        for (_, v) in &values.data {
            ctx.charge(1)?;
            depth = depth.max(v.depth() + 1);
        }
        Ok(Self(Kind::Hash(Heap::new(ctx, values, depth)?)))
    }
    pub(crate) fn push(self, ctx: &mut CallContext, values: &[Value]) -> Result<Self> {
        let Kind::Array(mut heap) = self.0 else {
            return Err(Error::new(ErrorKind::Type, "expected array"));
        };
        let mut depth = heap.depth;
        for value in values {
            ctx.charge(1)?;
            depth = depth.max(value.depth() + 1);
        }
        if depth > MAX_VALUE_DEPTH {
            return ctx.fail(ErrorKind::Recursion, "value nesting too deep");
        }
        if !values.is_empty() {
            let Some(capacity) = heap.buffer.data.len().checked_add(values.len()) else {
                return ctx.fail(ErrorKind::Memory, "array size overflow");
            };
            let writable = Heap::make_mut(ctx, &mut heap, capacity)?;
            writable.buffer.extend(ctx, values)?;
            writable.depth = depth;
        }
        Ok(Self(Kind::Array(heap)))
    }
    pub(crate) fn set_array_index(
        self,
        ctx: &mut CallContext,
        index: usize,
        value: Value,
    ) -> Result<Self> {
        let Kind::Array(mut heap) = self.0 else {
            return Err(Error::new(ErrorKind::Type, "expected array"));
        };
        let mut depth = heap.depth.max(value.depth() + 1);
        if heap.depth > 1
            && heap.buffer.data[index].depth() + 1 == heap.depth
            && value.depth() + 1 < heap.depth
        {
            depth = value.depth() + 1;
            for (i, item) in heap.buffer.data.iter().enumerate() {
                ctx.charge(1)?;
                if i != index {
                    depth = depth.max(item.depth() + 1);
                }
            }
        }
        if depth > MAX_VALUE_DEPTH {
            return ctx.fail(ErrorKind::Recursion, "value nesting too deep");
        }
        let len = heap.buffer.data.len();
        let writable = Heap::make_mut(ctx, &mut heap, len)?;
        writable.buffer.data[index] = value;
        writable.depth = depth;
        Ok(Self(Kind::Array(heap)))
    }
    fn depth(&self) -> usize {
        match &self.0 {
            Kind::Array(h) => h.depth,
            Kind::Hash(h) => h.depth,
            _ => 0,
        }
    }
    pub(crate) fn require_bytes(&self) -> Result<&[u8]> {
        self.as_bytes()
            .ok_or_else(|| Error::new(ErrorKind::Type, "expected string or symbol"))
    }
    pub(crate) fn require_int(&self) -> Result<i64> {
        self.as_int()
            .ok_or_else(|| Error::new(ErrorKind::Type, "expected integer"))
    }
}

impl CallContext {
    /// Imports a host value, copying foreign storage and sharing values already owned by this call.
    pub fn import(&mut self, value: &Value) -> Result<Value> {
        self.import_depth(value, 0)
    }

    fn import_depth(&mut self, value: &Value, depth: usize) -> Result<Value> {
        self.charge(1)?;
        if depth > MAX_VALUE_DEPTH {
            return self.fail(ErrorKind::Recursion, "value nesting too deep");
        }
        match &value.0 {
            Kind::Bytes(h) | Kind::Symbol(h) => {
                if self.owns(&h.header) {
                    return Ok(value.clone());
                }
                Value::copy_bytes(self, &h.buffer.data, matches!(value.0, Kind::Symbol(_)))
            }
            Kind::Array(h) => {
                if self.owns(&h.header) {
                    return Ok(value.clone());
                }
                let mut buf = Buffer::with_capacity(self, h.buffer.data.len())?;
                for v in &h.buffer.data {
                    let v = self.import_depth(v, depth + 1)?;
                    buf.data.push(v);
                }
                Value::from_array(self, buf)
            }
            Kind::Hash(h) => {
                if self.owns(&h.header) {
                    return Ok(value.clone());
                }
                let mut buf = Buffer::with_capacity(self, h.buffer.data.len())?;
                for (k, v) in &h.buffer.data {
                    let k = self.import_depth(k, depth + 1)?;
                    let v = self.import_depth(v, depth + 1)?;
                    buf.data.push((k, v));
                }
                Value::from_hash(self, buf)
            }
            _ => Ok(value.clone()),
        }
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.0 {
            Kind::Nil => f.write_str("nil"),
            Kind::Bool(b) => write!(f, "{b}"),
            Kind::Int(n) => write!(f, "{n}"),
            Kind::Float(n) => write!(f, "{n}"),
            Kind::Bytes(h) | Kind::Symbol(h) => {
                write!(f, "{}", String::from_utf8_lossy(&h.buffer.data))
            }
            Kind::Array(h) => {
                f.write_str("[")?;
                for (i, v) in h.buffer.data.iter().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    write!(f, "{v}")?;
                }
                f.write_str("]")
            }
            Kind::Hash(h) => {
                f.write_str("{")?;
                for (i, (k, v)) in h.buffer.data.iter().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    write!(f, "{k}: {v}")?;
                }
                f.write_str("}")
            }
        }
    }
}
