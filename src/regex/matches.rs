use super::{program::View, search::ABSENT};
use crate::checked::{Checked, violation};
use crate::{
    CallContext, Error, ErrorKind, Result, Value,
    budget::{Buffer, Charge},
    hash::Hash,
    numeric::to_i64,
    ops,
    value::{Bytes, Heap, Kind},
};
use std::sync::{Arc, OnceLock};

// These immutable language constants contain no invocation data. Imports still
// account for them in the destination budget, just as for compiler constants.
fn keys() -> &'static [Value; 7] {
    static KEYS: OnceLock<[Value; 7]> = OnceLock::new();
    KEYS.get_or_init(|| {
        [
            "begin",
            "captures",
            "end",
            "named_captures",
            "post_match",
            "pre_match",
            "to_s",
        ]
        .map(|name| Value::bytes(name.as_bytes()))
    })
}

fn empty_captures() -> Value {
    static EMPTY: OnceLock<Value> = OnceLock::new();
    EMPTY.get_or_init(|| Value::array(Vec::new())).clone()
}

fn empty_named() -> Value {
    static EMPTY: OnceLock<Value> = OnceLock::new();
    EMPTY.get_or_init(|| Value::hash(Vec::new())).clone()
}

fn window(ctx: &mut CallContext, subject: &Value, start: usize, end: usize) -> Result<Value> {
    if start == end {
        static EMPTY: OnceLock<Value> = OnceLock::new();
        Ok(EMPTY.get_or_init(|| Value::bytes(b"")).clone())
    } else {
        super::window(ctx, subject, start, end)
    }
}

fn data_window_bytes(text: &[u8], start: usize, end: usize) -> usize {
    if start == end {
        0
    } else {
        window_bytes(text, start, end)
    }
}

#[derive(Debug)]
pub(crate) struct Offset {
    values: Value,
    end: bool,
    header: Option<Charge>,
}

impl Offset {
    fn value(ctx: &mut CallContext, values: Value, end: bool) -> Result<Value> {
        let header = ctx.reserve(size_of::<Self>() + 2 * size_of::<usize>())?;
        Ok(Value(Kind::Offset(Arc::new(Self {
            values,
            end,
            header,
        }))))
    }

    pub fn import(ctx: &mut CallContext, value: &Arc<Self>) -> Result<Arc<Self>> {
        if ctx.owns(&value.header) {
            return Ok(value.clone());
        }
        let values = ctx.import(&value.values)?;
        let header = ctx.reserve(size_of::<Self>() + 2 * size_of::<usize>())?;
        Ok(Arc::new(Self {
            values,
            end: value.end,
            header,
        }))
    }

    pub fn name(&self) -> &'static str {
        if self.end {
            "match_data.end"
        } else {
            "match_data.begin"
        }
    }

    pub fn value_error(&self) -> Error {
        let name = if self.end { "end" } else { "begin" };
        Error::new(
            ErrorKind::Type,
            format!("{name} is a method and cannot be used as a value; call it with {name}(...)"),
        )
    }

    pub fn call(
        &self,
        ctx: &mut CallContext,
        args: &[Value],
        keywords: &[(Value, Value)],
        block: bool,
    ) -> Result<Value> {
        ctx.checkpoint()?;
        let name = self.name();
        if !keywords.is_empty() {
            return Err(Error::new(
                ErrorKind::Argument,
                format!("{name} does not accept keyword arguments"),
            ));
        }
        if block {
            return Err(Error::new(
                ErrorKind::Argument,
                format!("{name} does not accept blocks"),
            ));
        }
        if args.len() != 1 {
            return Err(Error::new(
                ErrorKind::Argument,
                format!("{name} expects a capture index"),
            ));
        }
        let values = self
            .values
            .as_array()
            .ok_or(violation("capture offsets must be an array"))?;
        let index = crate::sequence::integer(args.at(0)?).map_err(|mut error| {
            error.message = format!("{name} capture index must be integer");
            error
        })?;
        let index = if index < 0 {
            values.len() as i128 + i128::from(index)
        } else {
            i128::from(index)
        };
        let Some(value) = usize::try_from(index)
            .ok()
            .and_then(|index| values.get(index))
        else {
            return Err(Error::new(
                ErrorKind::Argument,
                format!("{name} capture index out of bounds"),
            ));
        };
        Ok(value.clone())
    }
}

pub(super) fn data(
    ctx: &mut CallContext,
    program: View<'_>,
    subject: &Value,
    indices: &[usize],
) -> Result<Value> {
    let text = subject.require_bytes()?;
    let count = indices.len() / 2;
    let groups = count - 1;
    let mut projected = (2 + usize::from(groups != 0))
        .saturating_mul(Heap::<Value>::header_bytes())
        .saturating_add((2 * count + groups).saturating_mul(size_of::<Value>()))
        .saturating_add(2 * (size_of::<Offset>() + 2 * size_of::<usize>()))
        .saturating_add(size_of::<Hash>() + 2 * size_of::<usize>())
        .saturating_add(7 * size_of::<(Value, Value)>());
    let mut named_count = 0usize;
    for (index, pair) in indices.chunks_exact(2).enumerate() {
        let &[start, end] = pair else {
            continue;
        };
        ctx.charge(1)?;
        projected = projected.saturating_add(data_window_bytes(text, start, end));
        if index != 0 {
            let (start, end) = *program.names.at(index)?;
            if start != end {
                named_count += 1;
                projected = projected.saturating_add(Bytes::header_bytes() + end - start);
            }
        }
    }
    projected = projected
        .saturating_add(data_window_bytes(text, 0, *indices.at(0)?))
        .saturating_add(data_window_bytes(text, *indices.at(1)?, text.len()))
        .saturating_add(if named_count == 0 {
            0
        } else {
            size_of::<Hash>() + 2 * size_of::<usize>()
        })
        .saturating_add(
            named_count.saturating_mul(2 * size_of::<(Value, Value)>() + size_of::<[usize; 8]>()),
        )
        .saturating_add(if named_count >= 16 { 256 } else { 0 });
    ctx.check_memory(projected)?;
    let mut captures = Buffer::with_capacity(ctx, groups)?;
    let mut whole = Value::nil();
    let mut starts = Buffer::with_capacity(ctx, count)?;
    let mut ends = Buffer::with_capacity(ctx, count)?;
    for (index, pair) in indices.chunks_exact(2).enumerate() {
        let &[start, end] = pair else {
            continue;
        };
        ctx.charge(1)?;
        let value = if start == ABSENT {
            starts.data.push(Value::nil());
            ends.data.push(Value::nil());
            Value::nil()
        } else {
            let value = window(ctx, subject, start, end)?;
            starts
                .data
                .push(Value::int(to_i64(ops::runes(ctx, text.span(0, start)?)?.0)));
            ends.data
                .push(Value::int(to_i64(ops::runes(ctx, text.span(0, end)?)?.0)));
            value
        };
        if index == 0 {
            whole = value;
        } else {
            captures.data.push(value);
        }
    }
    let mut named = Hash::empty();
    for (index, &(start, end)) in program.names.iter().enumerate().skip(1) {
        ctx.charge(1)?;
        if start == end {
            continue;
        }
        let name = program.source.span(start, end)?;
        let value = captures.at(index - 1)?;
        if matches!(value.0, Kind::Nil) && named.find(ctx, name)?.is_some() {
            continue;
        }
        let key = ctx.bytes(name)?;
        named.insert(ctx, key, value.clone())?;
    }
    let named = if named_count == 0 {
        empty_named()
    } else {
        let named = crate::arguments::ordered_hash(ctx, named.into_buffer())?;
        Value::from_hash(ctx, named)?
    };
    let captures = if groups == 0 {
        empty_captures()
    } else {
        Value::from_array(ctx, captures)?
    };
    let starts = Value::from_array(ctx, starts)?;
    let ends = Value::from_array(ctx, ends)?;
    let begin = Offset::value(ctx, starts, false)?;
    let end = Offset::value(ctx, ends, true)?;
    let pre = window(ctx, subject, 0, *indices.at(0)?)?;
    let post = window(ctx, subject, *indices.at(1)?, text.len())?;
    let mut entries = Buffer::with_capacity(ctx, 7)?;
    for (name, value) in keys()
        .iter()
        .zip([begin, captures, end, named, post, pre, whole])
    {
        entries.data.push((name.clone(), value));
    }
    let mut hash = Hash::from_entries(ctx, entries)?;
    hash.object = true;
    hash.tag = crate::hash::Tag::Match;
    Value::from_hash(ctx, hash)
}

pub(super) fn window_bytes(text: &[u8], start: usize, end: usize) -> usize {
    if start == ABSENT || (start == 0 && end == text.len()) {
        0
    } else {
        Bytes::header_bytes() + end - start
    }
}

/// Reads a required capture, preserving numeric indexing and named-group selection.
pub(super) fn fetch(
    ctx: &mut CallContext,
    hash: &Hash,
    args: &[Value],
    keywords: bool,
    block: bool,
) -> Result<Value> {
    if args.len() != 1 || keywords || block {
        return Err(Error::new(
            ErrorKind::Argument,
            "match_data.fetch expects one capture index or name, without keywords or a block",
        ));
    }
    let group = args.at(0)?;
    if let Some(key) = group.as_bytes() {
        let named = hash
            .find(ctx, b"named_captures")?
            .ok_or(violation("match data must contain named_captures"))?;
        let Kind::Hash(named) = &hash.buffer.at(named)?.1.0 else {
            return Err(crate::checked::internal("named captures must be a hash"));
        };
        if let Some(found) = named.find(ctx, key)? {
            let value = &named.buffer.at(found)?.1;
            if !matches!(value.0, Kind::Nil) {
                return Ok(value.clone());
            }
        }
        return Err(crate::collections::missing_key(
            ctx,
            "match_data.fetch",
            group,
        )?);
    }
    let index = crate::sequence::integer(group)?;
    if let Some(value) = self::index(ctx, hash, group)? {
        if !matches!(value.0, Kind::Nil) {
            return Ok(value);
        }
    }
    let captures = hash
        .find(ctx, b"captures")?
        .ok_or(violation("match data must contain captures"))?;
    let length = hash
        .buffer
        .at(captures)?
        .1
        .as_array()
        .ok_or(violation("captures must be an array"))?
        .len() as i128
        + 1;
    let message = if i128::from(index) < -length || i128::from(index) >= length {
        format!(
            "match_data.fetch index {index} outside of capture bounds: {}...{length}",
            -length
        )
    } else {
        format!("match_data.fetch capture {index} did not participate")
    };
    Err(Error::new(ErrorKind::Argument, message))
}

pub(crate) fn index(ctx: &mut CallContext, hash: &Hash, index: &Value) -> Result<Option<Value>> {
    if !hash.object {
        return Ok(None);
    }
    let Some(whole) = hash.find(ctx, b"to_s")? else {
        return Ok(None);
    };
    if let Some(key) = index.as_bytes() {
        if hash.find(ctx, key)?.is_some() {
            return Ok(None);
        }
        let Some(named) = hash.find(ctx, b"named_captures")? else {
            return Ok(None);
        };
        let Kind::Hash(named) = &hash.buffer.at(named)?.1.0 else {
            return Ok(None);
        };
        let Some(found) = named.find(ctx, key)? else {
            return Ok(None);
        };
        return Ok(Some(named.buffer.at(found)?.1.clone()));
    }
    if !matches!(index.0, Kind::Int(_) | Kind::Big(_) | Kind::Float(_)) {
        return Ok(None);
    }
    let Some(captures) = hash.find(ctx, b"captures")? else {
        return Ok(None);
    };
    let Some(captures) = hash.buffer.at(captures)?.1.as_array() else {
        return Ok(None);
    };
    let index = crate::sequence::integer(index)?;
    let index = if index < 0 {
        captures.len() as i128 + 1 + i128::from(index)
    } else {
        i128::from(index)
    };
    Ok(Some(if index == 0 {
        hash.buffer.at(whole)?.1.clone()
    } else {
        usize::try_from(index - 1)
            .ok()
            .and_then(|index| captures.get(index))
            .cloned()
            .unwrap_or_default()
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        CallOptions, Limits,
        regex::{Program, Search},
    };

    #[test]
    fn match_data_rejects_all_capture_copies_before_allocating() {
        let mut ctx = CallContext::new(CallOptions {
            limits: Limits {
                steps: None,
                ..Limits::default()
            },
            ..CallOptions::default()
        });
        let subject = ctx
            .bytes(&[b"x".as_slice(), &vec![b'a'; 32768], b"y"].concat())
            .unwrap();
        let source = format!("{}a+{}", "(".repeat(16), ")".repeat(16));
        let program =
            Program::compile(&mut ctx, Value::bytes(source.as_bytes()), "Regex.match").unwrap();
        let mut search = Search::new(&mut ctx, program.view().unwrap(), true).unwrap();
        let indices = search
            .find(
                &mut ctx,
                program.view().unwrap(),
                subject.as_bytes().unwrap(),
                0,
            )
            .unwrap()
            .unwrap();
        let baseline = ctx.stats();
        ctx.options.limits.memory_bytes = Some(baseline.retained_memory_bytes + 65536);
        assert_eq!(
            data(&mut ctx, program.view().unwrap(), &subject, &indices.data)
                .unwrap_err()
                .kind,
            ErrorKind::Memory
        );
        assert_eq!(ctx.stats().peak_memory_bytes, baseline.peak_memory_bytes);
        assert_eq!(
            ctx.stats().retained_memory_bytes,
            baseline.retained_memory_bytes
        );
        assert_eq!(ctx.checkpoint().unwrap_err().kind, ErrorKind::Memory);
        drop(indices);
        drop(search);
        drop(program);
        drop(subject);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }

    #[test]
    fn imported_offset_arrays_release_origin_storage() {
        let mut first = CallContext::new(CallOptions::default());
        let array = first.array(&[Value::int(7), Value::nil()]).unwrap();
        let original = Offset::value(&mut first, array, false).unwrap();
        let mut second = CallContext::new(CallOptions::default());
        let imported = second.import(&original).unwrap();
        drop(original);
        assert_eq!(first.stats().retained_memory_bytes, 0);
        let Kind::Offset(offset) = &imported.0 else {
            panic!("unexpected test fixture")
        };
        assert_eq!(
            offset
                .call(&mut second, &[Value::int(0)], &[], false)
                .unwrap()
                .require_int()
                .unwrap(),
            7
        );
        assert!(matches!(
            offset
                .call(&mut second, &[Value::int(-1)], &[], false)
                .unwrap()
                .0,
            Kind::Nil
        ));
        drop(imported);
        assert_eq!(second.stats().retained_memory_bytes, 0);
    }

    #[test]
    fn importing_match_constants_charges_the_destination_and_releases_the_origin() {
        let mut first = CallContext::new(CallOptions::default());
        let subject = first.bytes(b"").unwrap();
        let program = Program::compile(&mut first, Value::bytes(b""), "Regex.match").unwrap();
        let original = data(&mut first, program.view().unwrap(), &subject, &[0, 0]).unwrap();
        let mut second = CallContext::new(CallOptions::default());
        let imported = second.import(&original).unwrap();
        for (key, _) in imported.as_hash().unwrap() {
            let Kind::Bytes(bytes) = &key.0 else {
                panic!("match key must be bytes")
            };
            // Re-importing a key in its destination shares its existing charge.
            let copied = second.import(key).unwrap();
            let Kind::Bytes(copied) = &copied.0 else {
                panic!("imported match key must be bytes")
            };
            assert!(bytes.ptr_eq(copied));
        }
        drop(original);
        drop(subject);
        drop(program);
        assert_eq!(first.stats().retained_memory_bytes, 0);
        assert!(second.stats().retained_memory_bytes > 0);
        drop(imported);
        assert_eq!(second.stats().retained_memory_bytes, 0);
    }
}
