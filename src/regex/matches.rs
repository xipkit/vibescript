use super::{program::View, search::ABSENT};
use crate::{
    CallContext, Error, ErrorKind, Result, Value,
    budget::{Buffer, Charge},
    hash::Hash,
    ops,
    value::{Bytes, Heap, Kind},
};
use std::sync::Arc;

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
        Error::new(
            ErrorKind::Type,
            format!("{} is a method and must be called", self.name()),
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
        if !keywords.is_empty() || block {
            return Err(Error::new(
                ErrorKind::Argument,
                "match offset does not accept keywords or blocks",
            ));
        }
        ops::arity(args, 1)?;
        let values = self.values.as_array().unwrap();
        let index = crate::sequence::integer(&args[0])?;
        let index = if index < 0 {
            values.len() as i128 + i128::from(index)
        } else {
            i128::from(index)
        };
        if index < 0 || index >= values.len() as i128 {
            return Err(Error::new(
                ErrorKind::Argument,
                "match capture index out of bounds",
            ));
        }
        Ok(values[index as usize].clone())
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
    let mut projected = count
        .saturating_mul(size_of::<Value>())
        .saturating_add(3 * Heap::<Value>::header_bytes())
        .saturating_add((2 * count + groups).saturating_mul(size_of::<Value>()))
        .saturating_add(2 * (size_of::<Offset>() + 2 * size_of::<usize>()))
        .saturating_add(2 * (size_of::<Hash>() + 2 * size_of::<usize>()))
        .saturating_add(7 * size_of::<(Value, Value)>())
        .saturating_add(7 * Bytes::header_bytes() + 57);
    let mut named_count = 0usize;
    for (index, pair) in indices.chunks_exact(2).enumerate() {
        ctx.charge(1)?;
        projected = projected.saturating_add(window_bytes(text, pair[0], pair[1]));
        if index != 0 {
            let (start, end) = program.names[index];
            if start != end {
                named_count += 1;
                projected = projected.saturating_add(Bytes::header_bytes() + end - start);
            }
        }
    }
    projected = projected
        .saturating_add(window_bytes(text, 0, indices[0]))
        .saturating_add(window_bytes(text, indices[1], text.len()))
        .saturating_add(
            named_count.saturating_mul(2 * size_of::<(Value, Value)>() + size_of::<[usize; 8]>()),
        )
        .saturating_add(if named_count >= 16 { 256 } else { 0 });
    ctx.check_memory(projected)?;
    let mut values = Buffer::with_capacity(ctx, count)?;
    let mut starts = Buffer::with_capacity(ctx, count)?;
    let mut ends = Buffer::with_capacity(ctx, count)?;
    for pair in indices.chunks_exact(2) {
        ctx.charge(1)?;
        if pair[0] == ABSENT {
            values.data.push(Value::nil());
            starts.data.push(Value::nil());
            ends.data.push(Value::nil());
        } else {
            values
                .data
                .push(super::window(ctx, subject, pair[0], pair[1])?);
            starts
                .data
                .push(Value::int(ops::runes(ctx, &text[..pair[0]])?.0 as i64));
            ends.data
                .push(Value::int(ops::runes(ctx, &text[..pair[1]])?.0 as i64));
        }
    }
    let mut named = Hash::empty();
    for (index, &(start, end)) in program.names.iter().enumerate().skip(1) {
        ctx.charge(1)?;
        if start == end {
            continue;
        }
        let name = &program.source[start..end];
        if matches!(values.data[index].0, Kind::Nil) && named.find(ctx, name)?.is_some() {
            continue;
        }
        let key = ctx.bytes(name)?;
        named.insert(ctx, key, values.data[index].clone())?;
    }
    let named = crate::arguments::ordered_hash(ctx, named.buffer)?;
    let named = Value::from_hash(ctx, named)?;
    let mut captures = Buffer::with_capacity(ctx, groups)?;
    captures.extend(ctx, &values.data[1..])?;
    let captures = Value::from_array(ctx, captures)?;
    let starts = Value::from_array(ctx, starts)?;
    let ends = Value::from_array(ctx, ends)?;
    let begin = Offset::value(ctx, starts, false)?;
    let end = Offset::value(ctx, ends, true)?;
    let pre = super::window(ctx, subject, 0, indices[0])?;
    let post = super::window(ctx, subject, indices[1], text.len())?;
    let mut entries = Buffer::with_capacity(ctx, 7)?;
    for (name, value) in [
        ("begin", begin),
        ("captures", captures),
        ("end", end),
        ("named_captures", named),
        ("post_match", post),
        ("pre_match", pre),
        ("to_s", values.data[0].clone()),
    ] {
        let name = ctx.bytes(name.as_bytes())?;
        entries.data.push((name, value));
    }
    let mut hash = Hash::from_entries(ctx, entries)?;
    hash.object = true;
    hash.match_data = true;
    Value::from_hash(ctx, hash)
}

pub(super) fn window_bytes(text: &[u8], start: usize, end: usize) -> usize {
    if start == ABSENT || (start == 0 && end == text.len()) {
        0
    } else {
        Bytes::header_bytes() + end - start
    }
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
        if let Some(named) = hash.find(ctx, b"named_captures")?
            && let Kind::Hash(named) = &hash.buffer.data[named].1.0
            && let Some(found) = named.find(ctx, key)?
        {
            return Ok(Some(named.buffer.data[found].1.clone()));
        }
        return Ok(None);
    }
    if !matches!(index.0, Kind::Int(_) | Kind::Big(_) | Kind::Float(_)) {
        return Ok(None);
    }
    let Some(captures) = hash.find(ctx, b"captures")? else {
        return Ok(None);
    };
    let Some(captures) = hash.buffer.data[captures].1.as_array() else {
        return Ok(None);
    };
    let index = crate::sequence::integer(index)?;
    let index = if index < 0 {
        captures.len() as i128 + 1 + i128::from(index)
    } else {
        i128::from(index)
    };
    Ok(Some(if index == 0 {
        hash.buffer.data[whole].1.clone()
    } else if index < 0 || index > captures.len() as i128 {
        Value::nil()
    } else {
        captures[index as usize - 1].clone()
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
        let program = Program::compile(&mut ctx, Value::bytes(source.as_bytes())).unwrap();
        let mut search = Search::new(&mut ctx, program.view(), true).unwrap();
        let indices = search
            .find(&mut ctx, program.view(), subject.as_bytes().unwrap(), 0)
            .unwrap()
            .unwrap();
        let baseline = ctx.stats();
        ctx.options.limits.memory_bytes = Some(baseline.retained_memory_bytes + 65536);
        assert_eq!(
            data(&mut ctx, program.view(), &subject, &indices.data)
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
            unreachable!()
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
}
