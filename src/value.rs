use crate::{
    CallContext, Error, ErrorKind, Result,
    budget::{Buffer, Charge, MAX_VALUE_DEPTH},
    hash::Hash,
    range::Range,
};
use std::{collections::HashMap, fmt, mem::size_of, sync::Arc};

#[cfg(test)]
mod depth_tests;
pub(crate) mod destroy;
mod import;
mod render;

/// An element type whose nested containers can be destroyed without deep recursion.
///
/// `Heap` is only instantiated with [`Value`]; the bound lets its `Drop` stay generic
/// while sites elsewhere keep naming `Heap::<Value>` for size accounting.
pub(crate) trait Element: Sized {
    /// Destroys a buffer stolen from a heap taller than `destroy::SHALLOW`.
    fn destroy(buffer: Buffer<Self>);
}

impl Element for Value {
    fn destroy(buffer: Buffer<Self>) {
        destroy::values(buffer);
    }
}

#[derive(Debug)]
pub(crate) struct Heap<T: Element> {
    pub buffer: Buffer<T>,
    header: Option<Charge>,
    depth: usize,
    drop_parent: Option<Value>,
}

impl<T: Element> Drop for Heap<T> {
    fn drop(&mut self) {
        if self.depth > destroy::SHALLOW && !self.buffer.data.is_empty() {
            T::destroy(std::mem::replace(&mut self.buffer, Buffer::empty()));
        }
    }
}

impl<T: Element> Heap<T> {
    pub(crate) fn header_bytes() -> usize {
        size_of::<Self>() + 2 * size_of::<usize>()
    }

    pub fn new(ctx: &mut CallContext, buffer: Buffer<T>, depth: usize) -> Result<Arc<Self>> {
        if depth > MAX_VALUE_DEPTH {
            return ctx.guard(ErrorKind::Recursion, "value nesting too deep");
        }
        let header = ctx.reserve(Self::header_bytes())?;
        Ok(Arc::new(Self {
            buffer,
            header,
            depth,
            drop_parent: None,
        }))
    }
    fn untracked(data: Vec<T>, depth: usize) -> Arc<Self> {
        Arc::new(Self {
            buffer: Buffer::untracked(data),
            header: None,
            depth,
            drop_parent: None,
        })
    }
}

impl<T: Element + Clone> Heap<T> {
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

#[derive(Debug)]
pub(crate) struct Bytes {
    pub data: Arc<Vec<u8>>,
    _storage: Option<Charge>,
    header: Option<Charge>,
}

impl Bytes {
    pub(crate) fn header_bytes() -> usize {
        size_of::<Self>() + size_of::<Vec<u8>>() + 4 * size_of::<usize>()
    }
    fn new(ctx: &mut CallContext, buffer: Buffer<u8>) -> Result<Arc<Self>> {
        let header = ctx.reserve(Self::header_bytes())?;
        let (data, storage) = buffer.into_parts();
        Ok(Self::from_parts(data, storage, header))
    }
    pub(crate) fn from_parts(
        data: Vec<u8>,
        storage: Option<Charge>,
        header: Option<Charge>,
    ) -> Arc<Self> {
        Arc::new(Self {
            data: Arc::new(data),
            _storage: storage,
            header,
        })
    }
    fn untracked(data: Vec<u8>) -> Arc<Self> {
        Arc::new(Self {
            data: Arc::new(data),
            _storage: None,
            header: None,
        })
    }
    fn import(ctx: &mut CallContext, bytes: &Arc<Self>) -> Result<Arc<Self>> {
        if ctx.owns(&bytes.header) || ctx.options.limits.memory_bytes.is_none() {
            return Ok(bytes.clone());
        }
        // Charge the entire backing capacity, including unused space retained from the host.
        let storage = ctx.reserve(bytes.data.capacity())?;
        let header = ctx.reserve(Self::header_bytes())?;
        Ok(Arc::new(Self {
            data: bytes.data.clone(),
            _storage: storage,
            header,
        }))
    }
}

#[derive(Clone, Debug)]
pub(crate) enum Kind {
    Host(Arc<crate::capability::BoundMethod>),
    Function(Arc<crate::exports::Function>),
    Instance(Arc<crate::objects::Instance>),
    Namespace(Arc<crate::namespace::Namespace>),
    Offset(Arc<crate::regex::matches::Offset>),
    Regex(Arc<crate::regex::value::Regex>),
    Shape(Arc<crate::shapes::Shape>),
    Nil,
    Builtin(crate::builtin::Builtin),
    Enum(Arc<crate::enums::Enumeration>),
    EnumMember(Arc<crate::enums::Member>),
    Bool(bool),
    Int(i64),
    Big(Arc<crate::integer::Big>),
    Float(f64),
    Money(crate::money::Money),
    Duration(i64),
    Time(crate::time::Stamp),
    Zoned(Arc<crate::time::Zoned>),
    Bytes(Arc<Bytes>),
    Symbol(Arc<Bytes>),
    Array(Arc<Heap<Value>>),
    Hash(Arc<Hash>),
    Range(Arc<Range>),
}

/// An immutable Vibescript value. Clones share storage; script updates preserve each clone's value.
#[derive(Clone)]
pub struct Value(pub(crate) Kind);

impl fmt::Debug for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        render::debug(self, f)
    }
}

impl Default for Value {
    fn default() -> Self {
        Self::nil()
    }
}

impl Value {
    /// Compiles a regex with optional `i` and `m` flags; imports charge retained storage per call.
    pub fn regex(pattern: &[u8], flags: &str) -> Result<Self> {
        let mut bits = 0;
        for flag in flags.bytes() {
            let bit = match flag {
                b'i' => 1,
                b'm' => 2,
                _ => return Err(Error::new(ErrorKind::Argument, "unsupported regex flag")),
            };
            if bits & bit != 0 {
                return Err(Error::new(ErrorKind::Argument, "repeated regex flag"));
            }
            bits |= bit;
        }
        crate::regex::value::Regex::compile(
            &mut crate::integer::unlimited_context(),
            Self::bytes(pattern),
            bits,
            "Regexp.new",
        )
    }
    /// Returns a regex's source bytes and canonical flag letters.
    pub fn as_regex(&self) -> Option<(&[u8], &str)> {
        if let Kind::Regex(regex) = &self.0 {
            Some((regex.source.as_bytes().unwrap(), regex.flags()))
        } else {
            None
        }
    }

    /// Returns a type literal's canonical annotation bytes, or `None` for other values.
    pub fn as_type_literal(&self) -> Option<&[u8]> {
        if let Kind::Shape(shape) = &self.0 {
            Some(&shape.definition.text)
        } else {
            None
        }
    }
    /// Creates nil.
    pub const fn nil() -> Self {
        Self(Kind::Nil)
    }
    /// Creates a boolean.
    pub const fn boolean(value: bool) -> Self {
        Self(Kind::Bool(value))
    }
    /// Creates a compact signed integer.
    pub const fn int(value: i64) -> Self {
        Self(Kind::Int(value))
    }
    /// Parses a caller-owned integer in base 2 through 36, with an optional sign.
    ///
    /// Values outside the signed 64-bit range use shared immutable storage.
    /// Importing the result into a call charges its retained storage separately.
    pub fn parse_integer(text: &str, radix: u32) -> Result<Self> {
        crate::integer::parse(
            &mut crate::integer::unlimited_context(),
            text.as_bytes(),
            radix,
        )
    }
    /// Reports whether this value is an integer, including arbitrary-precision values.
    pub fn is_integer(&self) -> bool {
        matches!(self.0, Kind::Int(_) | Kind::Big(_))
    }
    /// Creates a floating-point value.
    pub const fn float(value: f64) -> Self {
        Self(Kind::Float(value))
    }
    /// Creates a caller-owned integer range; a missing endpoint represents an open bound.
    pub fn range(start: Option<i64>, end: Option<i64>, exclusive: bool) -> Self {
        Self(Kind::Range(Range::untracked(start, end, exclusive)))
    }
    /// Returns a range's start, end, and whether its end is excluded.
    pub fn as_range(&self) -> Option<(Option<i64>, Option<i64>, bool)> {
        if let Kind::Range(r) = &self.0 {
            Some((r.start, r.end, r.exclusive))
        } else {
            None
        }
    }
    /// Creates caller-owned bytes; importing them into a call is accounted separately.
    pub fn bytes(value: impl Into<Vec<u8>>) -> Self {
        Self(Kind::Bytes(Bytes::untracked(value.into())))
    }
    /// Creates a symbol.
    pub fn symbol(value: impl Into<Vec<u8>>) -> Self {
        Self(Kind::Symbol(Bytes::untracked(value.into())))
    }
    /// Creates a caller-owned array.
    pub fn array(values: Vec<Value>) -> Self {
        let depth = 1 + values.iter().map(Self::depth).max().unwrap_or(0);
        Self(Kind::Array(Heap::untracked(values, depth)))
    }
    /// Creates a caller-owned insertion-ordered hash with string keys, replacing duplicate values.
    pub fn hash(entries: Vec<(Vec<u8>, Value)>) -> Self {
        let mut values: Vec<(Value, Value)> = Vec::with_capacity(entries.len());
        let mut positions: HashMap<Arc<Vec<u8>>, usize> = HashMap::with_capacity(entries.len());
        for (key, value) in entries {
            if let Some(&i) = positions.get(&key) {
                values[i].1 = value;
            } else {
                let bytes = Bytes::untracked(key);
                positions.insert(bytes.data.clone(), values.len());
                values.push((Self(Kind::Bytes(bytes)), value));
            }
        }
        let depth = 1 + values.iter().map(|(_, v)| v.depth()).max().unwrap_or(0);
        Self(Kind::Hash(Hash::untracked(values, depth)))
    }
    /// Creates a host namespace whose fields take precedence over hash methods.
    ///
    /// Objects support scoped member access and may contain [`crate::HostMethod`]
    /// descriptors. Imports and script mutations follow ordinary value semantics.
    pub fn object(entries: Vec<(Vec<u8>, Value)>) -> Self {
        let mut value = Self::hash(entries);
        let Kind::Hash(hash) = &mut value.0 else {
            unreachable!()
        };
        Arc::get_mut(hash).unwrap().object = true;
        value
    }
    /// Returns an integer when this value fits the compact signed 64-bit representation.
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
            Kind::Big(ref n) => Some(n.to_float()),
            Kind::Float(n) => Some(n),
            _ => None,
        }
    }
    /// Constructs an inline duration from a signed number of whole seconds.
    pub fn duration(seconds: i64) -> Self {
        Self(Kind::Duration(seconds))
    }
    /// Returns a duration's signed whole-second count.
    pub fn as_duration(&self) -> Option<i64> {
        if let Kind::Duration(seconds) = self.0 {
            Some(seconds)
        } else {
            None
        }
    }
    /// Constructs an inline UTC timestamp from Unix seconds and nanoseconds within the second.
    pub fn time(seconds: i64, nanoseconds: u32) -> Result<Self> {
        if nanoseconds >= 1_000_000_000 {
            return Err(Error::new(
                ErrorKind::Argument,
                "time nanoseconds must be below one second",
            ));
        }
        Ok(Self(Kind::Time(crate::time::Stamp::new(
            seconds,
            nanoseconds,
        ))))
    }
    /// Returns a timestamp's Unix seconds and nanoseconds, independent of its display timezone.
    pub fn as_time(&self) -> Option<(i64, u32)> {
        crate::time::stamp(self).map(|stamp| (stamp.seconds(), stamp.nanos()))
    }
    /// Constructs inline money from signed cents and a three-letter ASCII currency.
    pub fn money(cents: i64, currency: &str) -> Result<Self> {
        crate::money::Money::new(cents, currency.as_bytes()).map(|money| Self(Kind::Money(money)))
    }
    /// Returns a money value's signed cents and uppercase currency code.
    pub fn as_money(&self) -> Option<(i64, &str)> {
        if let Kind::Money(money) = &self.0 {
            Some((money.cents(), money.currency()))
        } else {
            None
        }
    }
    /// Returns an enum type's declared name.
    pub fn as_enum_type(&self) -> Option<&str> {
        if let Kind::Enum(value) = &self.0 {
            Some(&value.definition.name)
        } else {
            None
        }
    }
    /// Returns an enum member's type name, declared name, and normalized symbol.
    ///
    /// Equal names from separately compiled scripts still represent distinct types.
    pub fn as_enum_member(&self) -> Option<(&str, &str, &str)> {
        if let Kind::EnumMember(value) = &self.0 {
            let definition = value.definition();
            Some((
                &value.enumeration.definition.name,
                &definition.name,
                &definition.symbol,
            ))
        } else {
            None
        }
    }
    /// Returns the raw bytes of a string or symbol, including invalid UTF-8.
    pub fn as_bytes(&self) -> Option<&[u8]> {
        match &self.0 {
            Kind::Bytes(h) | Kind::Symbol(h) => Some(&h.data),
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
    /// Returns insertion-ordered hash or namespace entries.
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
            Kind::Host(_) => "builtin",
            Kind::Function(_) => "function",
            Kind::Regex(_) => "regex",
            Kind::Namespace(_) => "class",
            Kind::Instance(_) => "instance",
            Kind::Shape(_) => "shape",
            Kind::Nil => "nil",
            Kind::Builtin(_) | Kind::Offset(_) => "builtin",
            Kind::Enum(_) => "enum",
            Kind::EnumMember(_) => "enum value",
            Kind::Bool(_) => "bool",
            Kind::Int(_) | Kind::Big(_) => "int",
            Kind::Float(_) => "float",
            Kind::Money(_) => "money",
            Kind::Duration(_) => "duration",
            Kind::Time(_) | Kind::Zoned(_) => "time",
            Kind::Bytes(_) => "string",
            Kind::Symbol(_) => "symbol",
            Kind::Array(_) => "array",
            Kind::Hash(ref hash) if hash.object => "object",
            Kind::Hash(_) => "hash",
            Kind::Range(_) => "range",
        }
    }

    pub(crate) fn copy_bytes(ctx: &mut CallContext, bytes: &[u8], symbol: bool) -> Result<Self> {
        let mut buf = Buffer::with_capacity(ctx, bytes.len())?;
        buf.extend(ctx, bytes)?;
        let heap = Bytes::new(ctx, buf)?;
        Ok(Self(if symbol {
            Kind::Symbol(heap)
        } else {
            Kind::Bytes(heap)
        }))
    }
    pub(crate) fn from_bytes(ctx: &mut CallContext, bytes: Buffer<u8>) -> Result<Self> {
        Ok(Self(Kind::Bytes(Bytes::new(ctx, bytes)?)))
    }
    pub(crate) fn compiler_constant(&self) -> Self {
        let (Kind::Bytes(bytes) | Kind::Symbol(bytes)) = &self.0 else {
            return self.clone();
        };
        if bytes._storage.is_none() && bytes.header.is_none() {
            return self.clone();
        }
        // Compiled constants share the backing bytes, but must not retain the
        // temporary syntax tree's invocation budget through the module cache.
        let compiled = Arc::new(Bytes {
            data: bytes.data.clone(),
            _storage: None,
            header: None,
        });
        Self(if matches!(self.0, Kind::Symbol(_)) {
            Kind::Symbol(compiled)
        } else {
            Kind::Bytes(compiled)
        })
    }
    pub(crate) fn from_array(ctx: &mut CallContext, values: Buffer<Value>) -> Result<Self> {
        let mut depth = 1;
        for v in &values.data {
            ctx.charge(1)?;
            depth = depth.max(v.depth() + 1);
        }
        Ok(Self(Kind::Array(Heap::new(ctx, values, depth)?)))
    }
    pub(crate) fn from_hash(ctx: &mut CallContext, hash: Hash) -> Result<Self> {
        Ok(Self(Kind::Hash(hash.into_arc(ctx)?)))
    }
    pub(crate) fn set_hash_index(
        self,
        ctx: &mut CallContext,
        key: Value,
        value: Value,
    ) -> Result<Self> {
        let Kind::Hash(mut hash) = self.0 else {
            return Err(Error::new(ErrorKind::Type, "expected hash"));
        };
        Hash::make_mut(ctx, &mut hash)?.insert(ctx, key, value)?;
        Ok(Self(Kind::Hash(hash)))
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
            return ctx.guard(ErrorKind::Recursion, "value nesting too deep");
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
    pub(crate) fn keep_array_range(
        self,
        ctx: &mut CallContext,
        start: usize,
        end: usize,
    ) -> Result<Self> {
        let Kind::Array(mut heap) = self.0 else {
            unreachable!()
        };
        let length = end - start;
        if start == 0 && end == heap.buffer.data.len() {
            return Ok(Self(Kind::Array(heap)));
        }
        if length == 0
            || length < heap.buffer.data.capacity() / 2
            || Arc::get_mut(&mut heap).is_none()
        {
            let mut buffer = Buffer::with_capacity(ctx, length)?;
            buffer.extend(ctx, &heap.buffer.data[start..end])?;
            return Self::from_array(ctx, buffer);
        }
        let writable = Arc::get_mut(&mut heap).unwrap();
        if start != 0 {
            for i in 0..length {
                ctx.charge(1)?;
                writable.buffer.data[i] = std::mem::take(&mut writable.buffer.data[start + i]);
            }
        }
        while writable.buffer.data.len() > length {
            ctx.charge(1)?;
            writable.buffer.data.pop();
        }
        if writable.depth > 1 {
            writable.depth = 1;
            for value in &writable.buffer.data {
                ctx.charge(1)?;
                writable.depth = writable.depth.max(value.depth() + 1);
            }
        }
        Ok(Self(Kind::Array(heap)))
    }
    pub(crate) fn delete_hash(self, ctx: &mut CallContext, key: &Value) -> Result<(Self, Self)> {
        let Kind::Hash(mut hash) = self.0 else {
            unreachable!()
        };
        let Some(index) = hash.find(ctx, key.require_bytes()?)? else {
            return Ok((Self(Kind::Hash(hash)), Self::nil()));
        };
        let removed = Hash::make_mut(ctx, &mut hash)?.remove(ctx, index)?;
        Ok((Self(Kind::Hash(hash)), removed))
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
            return ctx.guard(ErrorKind::Recursion, "value nesting too deep");
        }
        let len = heap.buffer.data.len();
        let writable = Heap::make_mut(ctx, &mut heap, len)?;
        writable.buffer.data[index] = value;
        writable.depth = depth;
        Ok(Self(Kind::Array(heap)))
    }
    pub(crate) fn depth(&self) -> usize {
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
    /// Parses an integer in base 2 through 36, charging conversion work and retained storage.
    pub fn parse_integer(&mut self, text: &str, radix: u32) -> Result<Value> {
        crate::integer::parse(self, text.as_bytes(), radix)
    }

    /// Imports a host value, sharing immutable bytes and charging retained storage to this call.
    ///
    /// Containers are copied iteratively; the traversal frames are charged to this call and
    /// released together with any partial result when the import fails.
    pub fn import(&mut self, value: &Value) -> Result<Value> {
        self.import_value(value, false)
    }

    /// Imports source-heap references rooted in temporarily allocated containers.
    pub(crate) fn import_rooted(&mut self, value: &Value) -> Result<Value> {
        self.import_value(value, true)
    }

    /// Copies mutable state with a fresh graph memo, preserving aliases and cycles
    /// within this snapshot without sharing the invocation's ordinary import cache.
    pub(crate) fn snapshot(&mut self, value: &Value) -> Result<Value> {
        let mut values = [value.clone()];
        self.snapshot_values(&mut values)?;
        Ok(std::mem::take(&mut values[0]))
    }

    /// Copies several roots with one memo, so aliases crossing argument slots survive.
    pub(crate) fn snapshot_values(&mut self, values: &mut [Value]) -> Result<()> {
        self.checkpoint()?;
        debug_assert!(!self.importing_objects);
        let previous = self.snapshot_objects.replace(Buffer::empty());
        let result = values.iter_mut().try_for_each(|value| {
            *value = self.import_rooted(value)?;
            Ok(())
        });
        self.snapshot_objects = previous;
        result
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        render::display(self, f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CallOptions;

    #[test]
    fn shared_bytes_have_independent_budget_lifetimes() {
        let mut first = CallContext::new(CallOptions::default());
        let original = first.bytes(&[b'x'; 8192]).unwrap();
        let mut second = CallContext::new(CallOptions::default());
        let imported = second.import(&original).unwrap();
        assert_eq!(
            original.as_bytes().unwrap().as_ptr(),
            imported.as_bytes().unwrap().as_ptr()
        );
        assert!(first.stats().retained_memory_bytes >= 8192);
        let retained = second.stats().retained_memory_bytes;
        assert!(retained >= 8192);
        let alias = second.import(&imported).unwrap();
        assert_eq!(second.stats().retained_memory_bytes, retained);
        drop(original);
        assert_eq!(first.stats().retained_memory_bytes, 0);
        assert_eq!(imported.as_bytes().unwrap(), &[b'x'; 8192]);
        drop(imported);
        assert_eq!(second.stats().retained_memory_bytes, retained);
        drop(alias);
        assert_eq!(second.stats().retained_memory_bytes, 0);
    }
}
