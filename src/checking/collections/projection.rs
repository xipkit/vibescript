//! Read-only projections: `values_at` on arrays and hashes, and the hash
//! members `slice`, `except` and `compact`.
//!
//! These members never invoke script code. Hash results are fresh plain hashes
//! even when the receiver is a host object or protected data; `slice` stores
//! its selected keys as strings while `except` and `compact` keep the stored
//! key representation.

use super::{EXACT, outcome};
use crate::{
    CallContext, Result, Value,
    budget::Buffer,
    checking::{
        facts::{Atom, Fact, Facts, Field, HashKind, Node},
        scalar::{Operation, Test},
    },
};

/// The keys one hash-key argument can name.
enum Keys {
    /// Literal names; one of them is selected.
    Literal(Buffer<Value>),
    /// A string or symbol whose bytes are unknown.
    Dynamic,
}

/// The values a literal range selector reads in `array.values_at`.
enum Window {
    /// The selected values and whether the selection may still fail.
    Values(Fact, bool),
    /// The range certainly starts before the array.
    Fails,
    /// The runtime may reach a limit guard the summary cannot describe.
    Unsupported,
}

/// A buffer holding `count` copies of `value`.
fn vec_of(ctx: &mut CallContext, value: Fact, count: usize) -> Result<Buffer<Fact>> {
    let mut values = Buffer::with_capacity(ctx, count)?;
    for _ in 0..count {
        ctx.charge(1)?;
        values.data.push(value);
    }
    Ok(values)
}

impl Facts {
    /// Classifies each key argument of `slice` or `except`. Keys must be
    /// strings or symbols; other known alternatives are contradictions and
    /// gradual values may fail at runtime.
    fn hash_keys(
        &mut self,
        ctx: &mut CallContext,
        args: &[Fact],
        result: &mut Operation,
    ) -> Result<Option<Buffer<Keys>>> {
        let mut keys = Buffer::with_capacity(ctx, args.len())?;
        let mut possible = true;
        for &arg in args {
            ctx.charge(1)?;
            let mut names = Buffer::empty();
            let mut dynamic = false;
            let mut valid = false;
            for i in 0..self.arm_count(arg) {
                ctx.charge(1)?;
                let arm = self.arm(arg, i);
                match self.node(arm) {
                    Node::Atom(Atom::Never) => (),
                    Node::String(name) | Node::Symbol(name) => {
                        let name = name.clone();
                        names.push(ctx, name)?;
                        valid = true;
                    }
                    Node::Atom(Atom::String | Atom::Symbol) => {
                        dynamic = true;
                        valid = true;
                    }
                    Node::Atom(Atom::Unknown | Atom::Any) => {
                        dynamic = true;
                        valid = true;
                        result.throws = true;
                    }
                    Node::Named(_) | Node::Nominal { .. } | Node::Choice(_) => {
                        result.unsupported = true;
                    }
                    _ => result.rejected = true,
                }
            }
            possible &= valid;
            keys.push(
                ctx,
                if dynamic {
                    Keys::Dynamic
                } else {
                    Keys::Literal(names)
                },
            )?;
        }
        Ok(possible.then_some(keys))
    }

    /// Models `hash.slice(*keys)`: present keys are copied as string keys into
    /// a closed plain hash; absent keys are skipped.
    pub(super) fn slice_member(
        &mut self,
        ctx: &mut CallContext,
        receiver: Fact,
        args: &[Fact],
    ) -> Result<Operation> {
        let mut result = outcome(Atom::Never.fact());
        let Some(keys) = self.hash_keys(ctx, args, &mut result)? else {
            return Ok(result);
        };
        let string = Atom::String.fact();
        let mut dynamic = false;
        for key in &keys.data {
            dynamic |= matches!(key, Keys::Dynamic);
        }
        result.value = if dynamic {
            let values = self.stored_values(ctx, receiver)?;
            if values == Atom::Never.fact() {
                self.shape_fields(ctx, Buffer::empty(), false, string, HashKind::PLAIN)?
            } else {
                self.hash_kind(ctx, string, values, HashKind::PLAIN)?
            }
        } else {
            let mut fields = Buffer::empty();
            for key in &keys.data {
                ctx.charge(1)?;
                let Keys::Literal(names) = key else {
                    unreachable!()
                };
                let alternatives = names.data.len();
                for name in &names.data {
                    ctx.charge(1)?;
                    let bytes = name.as_bytes().unwrap();
                    if let Some((value, optional)) = self.stored_field(ctx, receiver, bytes)? {
                        let name = ctx.bytes(bytes)?;
                        fields.push(
                            ctx,
                            Field {
                                name,
                                value,
                                optional: optional || alternatives > 1,
                            },
                        )?;
                    }
                }
            }
            // A key repeated with a different selection keeps its later field;
            // alternatives that were selected earlier stay possible.
            self.merge_selected_fields(ctx, &mut fields)?;
            self.shape_fields(ctx, fields, false, string, HashKind::PLAIN)?
        };
        Ok(result)
    }

    /// Models `hash.except(*keys)`: excluded keys are removed and every other
    /// entry keeps its stored key and value in a plain hash.
    pub(super) fn except_member(
        &mut self,
        ctx: &mut CallContext,
        receiver: Fact,
        args: &[Fact],
    ) -> Result<Operation> {
        let mut result = outcome(Atom::Never.fact());
        let Some(keys) = self.hash_keys(ctx, args, &mut result)? else {
            return Ok(result);
        };
        result.value = match self.node(receiver) {
            Node::Hash(keys, values, _) => {
                let (keys, values) = (*keys, *values);
                self.hash_kind(ctx, keys, values, HashKind::PLAIN)?
            }
            Node::Shape(fields, open, stored, _) => {
                let (open, stored) = (*open, *stored);
                let mut kept = Buffer::with_capacity(ctx, fields.data.len())?;
                for index in 0..fields.data.len() {
                    ctx.charge(1)?;
                    let Node::Shape(fields, ..) = self.node(receiver) else {
                        unreachable!()
                    };
                    let field = &fields.data[index];
                    let (name, value, optional) = (field.name.clone(), field.value, field.optional);
                    let bytes = name.as_bytes().unwrap();
                    let mut certain = false;
                    let mut possible = false;
                    for key in &keys.data {
                        ctx.charge(1)?;
                        match key {
                            Keys::Dynamic => possible = true,
                            Keys::Literal(names) => {
                                for candidate in &names.data {
                                    let candidate = candidate.as_bytes().unwrap();
                                    ctx.work_bytes(candidate.len().min(bytes.len()) + 1)?;
                                    if candidate == bytes {
                                        possible = true;
                                        certain |= names.data.len() == 1;
                                    }
                                }
                            }
                        }
                    }
                    if certain {
                        continue;
                    }
                    kept.push(
                        ctx,
                        Field {
                            name,
                            value,
                            optional: optional || possible,
                        },
                    )?;
                }
                self.shape_fields(ctx, kept, open, stored, HashKind::PLAIN)?
            }
            _ => unreachable!(),
        };
        Ok(result)
    }

    /// Models `hash.compact`: entries whose value is `nil` are dropped.
    pub(super) fn hash_compact_member(
        &mut self,
        ctx: &mut CallContext,
        receiver: Fact,
    ) -> Result<Operation> {
        let nil = Atom::Nil.fact();
        let value = match self.node(receiver) {
            Node::Hash(keys, values, _) => {
                let (keys, values) = (*keys, *values);
                let values = self.filter(ctx, values, Test::Nil, false)?;
                if values == Atom::Never.fact() {
                    self.shape_fields(ctx, Buffer::empty(), false, keys, HashKind::PLAIN)?
                } else {
                    self.hash_kind(ctx, keys, values, HashKind::PLAIN)?
                }
            }
            Node::Shape(fields, open, keys, _) => {
                let (open, keys) = (*open, *keys);
                let mut kept = Buffer::with_capacity(ctx, fields.data.len())?;
                for index in 0..fields.data.len() {
                    ctx.charge(1)?;
                    let Node::Shape(fields, ..) = self.node(receiver) else {
                        unreachable!()
                    };
                    let field = &fields.data[index];
                    let (name, value, optional) = (field.name.clone(), field.value, field.optional);
                    if value == nil {
                        continue;
                    }
                    let present = self.filter(ctx, value, Test::Nil, false)?;
                    let absent = self.filter(ctx, value, Test::Nil, true)?;
                    if present == Atom::Never.fact() {
                        continue;
                    }
                    kept.push(
                        ctx,
                        Field {
                            name,
                            value: present,
                            optional: optional || absent != Atom::Never.fact(),
                        },
                    )?;
                }
                self.shape_fields(ctx, kept, open, keys, HashKind::PLAIN)?
            }
            _ => unreachable!(),
        };
        Ok(outcome(value))
    }

    /// Models `values_at` on one array or hash arm. Every non-range selector
    /// contributes one element with ordinary lookup semantics; literal range
    /// selectors on arrays contribute a window. The result keeps exact
    /// positions whenever every selector has a known width.
    pub(super) fn values_at_member(
        &mut self,
        ctx: &mut CallContext,
        receiver: Fact,
        args: &[Fact],
    ) -> Result<Operation> {
        let mut result = outcome(Atom::Never.fact());
        let array = matches!(self.node(receiver), Node::Array(_) | Node::Tuple(_));
        let mut positions = Buffer::empty();
        let mut exact = true;
        let mut possible = true;
        for &selector in args {
            ctx.charge(1)?;
            let mut single = None;
            let mut windows = Buffer::<Fact>::empty();
            for i in 0..self.arm_count(selector) {
                ctx.charge(1)?;
                let arm = self.arm(selector, i);
                match self.node(arm) {
                    Node::Atom(Atom::Never) => (),
                    Node::Range(start, end, exclusive) if array => {
                        let (start, end, exclusive) = (*start, *end, *exclusive);
                        match self.values_window(ctx, receiver, start, end, exclusive)? {
                            Window::Values(window, throws) => {
                                result.throws |= throws;
                                windows.push(ctx, window)?;
                            }
                            Window::Fails => result.rejected = true,
                            Window::Unsupported => result.unsupported = true,
                        }
                    }
                    Node::Atom(Atom::Range) if array => result.unsupported = true,
                    Node::Range(..) | Node::Atom(Atom::Range) => result.rejected = true,
                    _ => {
                        let next = self.collection_index(ctx, receiver, &[arm])?;
                        result.rejected |= next.rejected;
                        result.unsupported |= next.unsupported;
                        result.throws |= next.throws;
                        if next.value != Atom::Never.fact() {
                            let value = match single {
                                Some(value) => self.union(ctx, &[value, next.value])?,
                                None => next.value,
                            };
                            single = Some(value);
                        }
                    }
                }
            }
            if single.is_none() && windows.data.is_empty() {
                possible = false;
                continue;
            }
            // One element, or one window width shared by every alternative.
            let mut width = single.map(|_| Some(1));
            for &window in &windows.data {
                ctx.charge(1)?;
                let next = match self.node(window) {
                    Node::Tuple(items) => Some(items.data.len()),
                    _ => None,
                };
                width = Some(match width {
                    None => next,
                    Some(known) if known == next => known,
                    Some(_) => None,
                });
            }
            let width = width.flatten();
            exact &= width.is_some();
            if !exact {
                if let Some(value) = single {
                    positions.push(ctx, value)?;
                }
                for window in windows.data {
                    ctx.charge(1)?;
                    let element = self.elements(ctx, window)?;
                    positions.push(ctx, element)?;
                }
                continue;
            }
            match single {
                Some(value) => {
                    let mut alternatives = Buffer::empty();
                    alternatives.push(ctx, value)?;
                    for &window in &windows.data {
                        ctx.charge(1)?;
                        let Node::Tuple(items) = self.node(window) else {
                            unreachable!()
                        };
                        alternatives.push(ctx, items.data[0])?;
                    }
                    let value = self.union(ctx, &alternatives.data)?;
                    positions.push(ctx, value)?;
                }
                None => {
                    for index in 0..width.unwrap() {
                        let mut alternatives = Buffer::with_capacity(ctx, windows.data.len())?;
                        for &window in &windows.data {
                            ctx.charge(1)?;
                            let Node::Tuple(items) = self.node(window) else {
                                unreachable!()
                            };
                            alternatives.data.push(items.data[index]);
                        }
                        let value = self.union(ctx, &alternatives.data)?;
                        positions.push(ctx, value)?;
                    }
                }
            }
        }
        if !possible {
            return Ok(result);
        }
        result.value = if exact {
            self.tuple(ctx, &positions.data)?
        } else {
            let element = self.union(ctx, &positions.data)?;
            if element == Atom::Never.fact() {
                self.tuple(ctx, &[])?
            } else {
                self.array(ctx, element)?
            }
        };
        Ok(result)
    }

    /// The values a literal range selects in `array.values_at`. The runtime
    /// resolves negative bounds against the length, fails when the start is
    /// still negative, and reads `nil` beyond the array. Windows that could
    /// exceed the runtime's size guard stay unsupported because that guard is
    /// a limit error.
    fn values_window(
        &mut self,
        ctx: &mut CallContext,
        receiver: Fact,
        start: Option<i64>,
        end: Option<i64>,
        exclusive: bool,
    ) -> Result<Window> {
        let inclusive = i128::from(end.is_none() || !exclusive);
        let first = i128::from(start.unwrap_or(0));
        let length = match self.node(receiver) {
            Node::Tuple(items) => Some(items.data.len() as i128),
            _ => None,
        };
        let Some(length) = length else {
            let element = self.elements(ctx, receiver)?;
            let value = self.nullable(ctx, element)?;
            // Without a length, only non-negative bounds fix the width; any
            // other window is no longer than the array.
            let width = match end {
                Some(last) if first >= 0 && last >= 0 => {
                    Some((i128::from(last) - first + inclusive).max(0))
                }
                Some(last) if last >= 0 && i128::from(last) + 1 > isize::MAX as i128 => {
                    return Ok(Window::Unsupported);
                }
                _ => None,
            };
            return Ok(match width {
                Some(width) if width > isize::MAX as i128 => Window::Unsupported,
                Some(width) if width <= EXACT as i128 => {
                    let values = vec_of(ctx, value, width as usize)?;
                    Window::Values(self.tuple(ctx, &values.data)?, false)
                }
                _ => Window::Values(self.array(ctx, value)?, first < 0),
            });
        };
        let first = if first < 0 { first + length } else { first };
        if first < 0 {
            return Ok(Window::Fails);
        }
        let last = end.map_or(length - 1, i128::from);
        let last = if last < 0 { last + length } else { last };
        let count = (last - first + inclusive).max(0);
        if count > isize::MAX as i128 {
            return Ok(Window::Unsupported);
        }
        if count > EXACT as i128 {
            let element = self.elements(ctx, receiver)?;
            let value = self.nullable(ctx, element)?;
            return Ok(Window::Values(self.array(ctx, value)?, false));
        }
        let nil = Atom::Nil.fact();
        let mut selected = Buffer::with_capacity(ctx, count as usize)?;
        for offset in 0..count {
            ctx.charge(1)?;
            let Node::Tuple(items) = self.node(receiver) else {
                unreachable!()
            };
            let value = usize::try_from(first + offset)
                .ok()
                .and_then(|index| items.data.get(index).copied())
                .unwrap_or(nil);
            selected.data.push(value);
        }
        Ok(Window::Values(self.tuple(ctx, &selected.data)?, false))
    }

    /// Reads the stored field a hash key selects, as `(value, optional)`.
    fn stored_field(
        &mut self,
        ctx: &mut CallContext,
        receiver: Fact,
        key: &[u8],
    ) -> Result<Option<(Fact, bool)>> {
        Ok(match self.node(receiver) {
            Node::Hash(_, values, _) => {
                let values = *values;
                (values != Atom::Never.fact()).then_some((values, true))
            }
            Node::Shape(_, open, ..) => {
                let open = *open;
                match self.selected_field(ctx, receiver, key)? {
                    Some(field) => Some(field),
                    None => open.then_some((Atom::Unknown.fact(), true)),
                }
            }
            _ => unreachable!(),
        })
    }

    /// Every value a hash may store.
    fn stored_values(&mut self, ctx: &mut CallContext, receiver: Fact) -> Result<Fact> {
        match self.node(receiver) {
            Node::Hash(_, values, _) => Ok(*values),
            Node::Shape(..) => self.shape_values(ctx, receiver, false),
            _ => unreachable!(),
        }
    }

    /// Joins repeated selected field names: a field is required when any
    /// selection of it is certain, and its values are the union of every
    /// selection.
    fn merge_selected_fields(
        &mut self,
        ctx: &mut CallContext,
        fields: &mut Buffer<Field>,
    ) -> Result<()> {
        let mut merged: Buffer<Field> = Buffer::with_capacity(ctx, fields.data.len())?;
        for field in fields.data.drain(..) {
            ctx.charge(1)?;
            let bytes = field.name.as_bytes().unwrap();
            let mut existing = None;
            for (index, earlier) in merged.data.iter().enumerate() {
                ctx.work_bytes(bytes.len().min(earlier.name.as_bytes().unwrap().len()) + 1)?;
                if earlier.name.as_bytes().unwrap() == bytes {
                    existing = Some(index);
                    break;
                }
            }
            match existing {
                Some(index) => {
                    let earlier = merged.data[index].value;
                    let value = self.union(ctx, &[earlier, field.value])?;
                    let earlier = &mut merged.data[index];
                    earlier.value = value;
                    earlier.optional &= field.optional;
                }
                None => merged.push(ctx, field)?,
            }
        }
        *fields = merged;
        Ok(())
    }
}
