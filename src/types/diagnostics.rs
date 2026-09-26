use super::{Type, Value};
use crate::{
    CallContext, Error, ErrorKind, Result,
    budget::Buffer,
    enums::compare_names,
    hash::Hash,
    shapes::{self, TypeWriter},
    value::Kind,
};
use std::{cmp::Ordering, sync::Arc};

const DEPTH: usize = 16;
const SAMPLES: usize = 16;

#[derive(Clone, Copy)]
pub(crate) enum Context<'a> {
    Value,
    Argument(&'a [u8]),
    HostArgument(&'a str, &'a str, usize),
    Return(&'a str),
    Ivar(&'a [u8]),
    /// A value named by its whole subject, such as `local variable count`.
    Subject(&'a [u8]),
    Json,
    /// The value of a checked cast, `value.as(T)`.
    Cast,
}

struct Writer<'a> {
    ctx: &'a mut CallContext,
    bytes: Buffer<u8>,
}

impl TypeWriter for Writer<'_> {
    fn write(&mut self, bytes: &[u8]) -> Result<()> {
        self.bytes.extend(self.ctx, bytes)
    }

    fn node(&mut self) -> Result<()> {
        self.ctx.charge(1)
    }
}

pub(crate) fn host_resolution(
    ctx: &mut CallContext,
    context: Context<'_>,
    name: &str,
    error: Error,
) -> Result<Error> {
    if error.kind != ErrorKind::Type {
        return Ok(error);
    }
    let mut writer = Writer {
        ctx,
        bytes: Buffer::empty(),
    };
    match context {
        Context::HostArgument(method, label, index) => {
            writer.write(method.as_bytes())?;
            writer.write(b" argument ")?;
            if label.is_empty() {
                writer.write((index + 1).to_string().as_bytes())?;
            } else {
                writer.write(label.as_bytes())?;
            }
            writer.write(b" type check failed: ")?;
        }
        Context::Return(method) => {
            writer.write(b"return type check failed for ")?;
            writer.write(method.as_bytes())?;
            writer.write(b": ")?;
        }
        Context::Argument(name) => {
            writer.write(b"argument ")?;
            writer.write(name)?;
            writer.write(b" type check failed: ")?;
        }
        Context::Ivar(name) => {
            writer.write(b"instance variable @")?;
            writer.write(name)?;
            writer.write(b" type check failed: ")?;
        }
        Context::Subject(subject) => {
            writer.write(subject)?;
            writer.write(b" type check failed: ")?;
        }
        Context::Cast => writer.write(b"cast type check failed: ")?,
        Context::Json => writer.write(b"JSON.parse_as type check failed: ")?,
        _ => unreachable!(),
    }
    if error.message == "unknown named type" {
        writer.write(b"unknown type ")?;
        writer.write(name.as_bytes())?;
    } else {
        writer.write(error.message.as_bytes())?;
    }
    let mut error = Error::from_bytes(writer.ctx, &writer.bytes.data)?;
    error.kind = ErrorKind::Type;
    Ok(error)
}

pub(super) fn mismatch(
    ctx: &mut CallContext,
    ty: &Type,
    value: &Value,
    context: Context<'_>,
) -> Result<Error> {
    let mut writer = Writer {
        ctx,
        bytes: Buffer::empty(),
    };
    match context {
        Context::Value => (),
        Context::Argument(name) => {
            writer.write(b"argument ")?;
            writer.write(name)?;
            writer.byte(b' ')?;
        }
        Context::HostArgument(method, name, index) => {
            writer.write(method.as_bytes())?;
            writer.write(b" argument ")?;
            if name.is_empty() {
                writer.write((index + 1).to_string().as_bytes())?;
            } else {
                writer.write(name.as_bytes())?;
            }
            writer.byte(b' ')?;
        }
        Context::Return(name) => {
            writer.write(b"return value for ")?;
            writer.write(name.as_bytes())?;
            writer.byte(b' ')?;
        }
        Context::Ivar(name) => {
            writer.write(b"instance variable @")?;
            writer.write(name)?;
            writer.byte(b' ')?;
        }
        Context::Subject(subject) => {
            writer.write(subject)?;
            writer.byte(b' ')?;
        }
        Context::Json => writer.write(b"JSON.parse_as value ")?,
        Context::Cast => writer.write(b"cast value ")?,
    }
    writer.write(b"expected ")?;
    shapes::format(ty, &mut writer)?;
    writer.write(b", got ")?;
    let actual = actual(writer.ctx, value, &mut [0; DEPTH], 0)?;
    writer.write(&actual.data)?;
    drop(actual);
    let mut error = Error::from_bytes(writer.ctx, &writer.bytes.data)?;
    error.kind = ErrorKind::Type;
    Ok(error)
}

fn actual(
    ctx: &mut CallContext,
    value: &Value,
    ancestors: &mut [usize; DEPTH],
    depth: usize,
) -> Result<Buffer<u8>> {
    ctx.charge(1)?;
    let mut out = Buffer::empty();
    match &value.0 {
        Kind::Enum(enumeration) => {
            out.extend(ctx, b"enum ")?;
            out.extend(ctx, enumeration.definition.name.as_bytes())?;
        }
        Kind::EnumMember(member) => {
            out.extend(ctx, member.enumeration.definition.name.as_bytes())?;
        }
        Kind::Array(array) => {
            let values = &array.buffer.data;
            if values.is_empty() {
                out.extend(ctx, b"array<empty>")?;
                return Ok(out);
            }
            if depth >= DEPTH || seen(ctx, ancestors, depth, Arc::as_ptr(array) as usize)? {
                out.extend(ctx, b"array<...>")?;
                return Ok(out);
            }
            let mut types = Buffer::with_capacity(ctx, values.len().min(SAMPLES))?;
            for value in values.iter().take(SAMPLES) {
                let text = actual(ctx, value, ancestors, depth + 1)?;
                insert(ctx, &mut types, text)?;
            }
            out.extend(ctx, b"array<")?;
            union(ctx, &mut out, &types.data, values.len() > SAMPLES)?;
            out.push(ctx, b'>')?;
        }
        Kind::Hash(hash) => {
            if hash.buffer.data.is_empty() {
                out.extend(ctx, b"{}")?;
                return Ok(out);
            }
            if depth >= DEPTH {
                out.extend(ctx, b"hash<string, ...>")?;
                return Ok(out);
            }
            if seen(ctx, ancestors, depth, Arc::as_ptr(hash) as usize)? {
                out.extend(ctx, b"{ ... }")?;
                return Ok(out);
            }
            let fields = fields(ctx, hash)?;
            let count = hash.buffer.data.len().min(SAMPLES);
            if count <= 6 {
                out.extend(ctx, b"{ ")?;
                for (index, &field) in fields[..count].iter().enumerate() {
                    if index > 0 {
                        out.extend(ctx, b", ")?;
                    }
                    let (key, value) = &hash.buffer.data[field];
                    out.extend(ctx, key.require_bytes()?)?;
                    out.extend(ctx, b": ")?;
                    let text = actual(ctx, value, ancestors, depth + 1)?;
                    out.extend(ctx, &text.data)?;
                }
                out.extend(ctx, b" }")?;
            } else {
                let mut types = Buffer::with_capacity(ctx, count)?;
                for &field in &fields[..count] {
                    let text = actual(ctx, &hash.buffer.data[field].1, ancestors, depth + 1)?;
                    insert(ctx, &mut types, text)?;
                }
                out.extend(ctx, b"hash<string, ")?;
                union(ctx, &mut out, &types.data, hash.buffer.data.len() > SAMPLES)?;
                out.push(ctx, b'>')?;
            }
        }
        _ => out.extend(ctx, value.type_name().as_bytes())?,
    }
    Ok(out)
}

fn seen(
    ctx: &mut CallContext,
    ancestors: &mut [usize; DEPTH],
    depth: usize,
    id: usize,
) -> Result<bool> {
    ctx.charge(depth as u64)?;
    if ancestors[..depth].contains(&id) {
        return Ok(true);
    }
    ancestors[depth] = id;
    Ok(false)
}

fn insert(ctx: &mut CallContext, types: &mut Buffer<Buffer<u8>>, text: Buffer<u8>) -> Result<()> {
    let mut position = 0;
    while position < types.data.len() {
        match compare_names(ctx, &text.data, &types.data[position].data)? {
            Ordering::Less => break,
            Ordering::Equal => return Ok(()),
            Ordering::Greater => position += 1,
        }
    }
    ctx.charge((types.data.len() - position) as u64)?;
    types.data.insert(position, text);
    Ok(())
}

fn union(
    ctx: &mut CallContext,
    out: &mut Buffer<u8>,
    types: &[Buffer<u8>],
    truncated: bool,
) -> Result<()> {
    for (index, text) in types.iter().enumerate() {
        if index > 0 {
            out.extend(ctx, b" | ")?;
        }
        out.extend(ctx, &text.data)?;
    }
    if truncated {
        if !types.is_empty() {
            out.extend(ctx, b" | ")?;
        }
        out.extend(ctx, b"...")?;
    } else if types.is_empty() {
        out.extend(ctx, b"empty")?;
    }
    Ok(())
}

fn fields(ctx: &mut CallContext, hash: &Hash) -> Result<[usize; SAMPLES]> {
    let mut fields = [0; SAMPLES];
    for (index, (key, _)) in hash.buffer.data.iter().enumerate() {
        ctx.charge(1)?;
        let count = index.min(SAMPLES);
        let mut position = count;
        while position > 0 {
            let candidate = &hash.buffer.data[fields[position - 1]].0;
            if compare_names(ctx, key.require_bytes()?, candidate.require_bytes()?)?
                != Ordering::Less
            {
                break;
            }
            if position < SAMPLES {
                fields[position] = fields[position - 1];
            }
            position -= 1;
        }
        if position < SAMPLES {
            fields[position] = index;
        }
    }
    Ok(fields)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CallOptions, CancellationToken, types};

    fn input() -> Value {
        Value::hash(vec![
            (
                vec![b'x'; 1024],
                Value::array(vec![Value::int(1), Value::bytes("text")]),
            ),
            (vec![b'y'; 1024], Value::boolean(true)),
        ])
    }

    fn imported() -> (CallContext, Value) {
        let mut ctx = CallContext::new(CallOptions::default());
        let value = ctx.import(&input()).unwrap();
        (ctx, value)
    }

    fn error(ctx: &mut CallContext, value: &Value) -> Error {
        let ty = Type::named("int".into());
        types::prepare(ctx, &ty, |_, _| unreachable!())
            .and_then(|prepared| {
                prepared.normalize_with(ctx, value.clone(), Context::Argument(b"payload"))
            })
            .unwrap_err()
    }

    #[test]
    fn every_diagnostic_work_boundary_is_latched_and_releases_scratch() {
        let (mut ctx, value) = imported();
        let before = ctx.stats();
        assert_eq!(error(&mut ctx, &value).kind, ErrorKind::Type);
        let required = ctx.stats().steps - before.steps;
        for allowance in 0..=required {
            let (mut ctx, value) = imported();
            let before = ctx.stats();
            ctx.options.limits.steps = Some(before.steps + allowance);
            let failure = error(&mut ctx, &value);
            if allowance < required {
                assert_eq!(failure.kind, ErrorKind::Steps, "allowance {allowance}");
                assert_eq!(ctx.checkpoint().unwrap_err().kind, ErrorKind::Steps);
            } else {
                assert_eq!(failure.kind, ErrorKind::Type);
                ctx.checkpoint().unwrap();
            }
            assert_eq!(
                ctx.stats().retained_memory_bytes,
                before.retained_memory_bytes
            );
            drop(value);
            assert_eq!(ctx.stats().retained_memory_bytes, 0);
        }
    }

    #[test]
    fn diagnostic_storage_obeys_memory_limits_before_allocation() {
        let (mut ctx, value) = imported();
        let before = ctx.stats();
        assert_eq!(error(&mut ctx, &value).kind, ErrorKind::Type);
        let peak = ctx.stats().peak_memory_bytes;
        let mut limits: Vec<_> = (before.retained_memory_bytes..=peak).step_by(64).collect();
        limits.extend([peak - 1, peak]);
        for limit in limits {
            let (mut ctx, value) = imported();
            let previous_peak = ctx.stats().peak_memory_bytes;
            let retained = ctx.stats().retained_memory_bytes;
            ctx.options.limits.memory_bytes = Some(limit);
            let failure = error(&mut ctx, &value);
            if limit < peak {
                assert_eq!(failure.kind, ErrorKind::Memory, "limit {limit}");
                assert_eq!(ctx.checkpoint().unwrap_err().kind, ErrorKind::Memory);
            } else {
                assert_eq!(failure.kind, ErrorKind::Type);
                ctx.checkpoint().unwrap();
            }
            assert!(ctx.stats().peak_memory_bytes <= limit.max(previous_peak));
            assert_eq!(ctx.stats().retained_memory_bytes, retained);
            drop(value);
            assert_eq!(ctx.stats().retained_memory_bytes, 0);
        }
    }

    #[test]
    fn recursive_shared_graphs_and_wide_key_scans_consume_work() {
        let mut repeated = Value::array(vec![Value::int(1)]);
        for _ in 0..12 {
            repeated = Value::array(vec![repeated; SAMPLES]);
        }
        let wide = Value::hash(
            (0..10000)
                .rev()
                .map(|n| (format!("key{n:05}").into_bytes(), Value::int(1)))
                .collect(),
        );
        for value in [repeated, wide] {
            let mut ctx = CallContext::new(CallOptions::default());
            ctx.options.limits.steps = Some(200);
            assert_eq!(error(&mut ctx, &value).kind, ErrorKind::Steps);
            assert_eq!(ctx.checkpoint().unwrap_err().kind, ErrorKind::Steps);
            assert_eq!(ctx.stats().retained_memory_bytes, 0);
        }
    }

    #[test]
    fn cancellation_and_deadlines_win_without_creating_a_type_message() {
        for deadline in [false, true] {
            let mut options = CallOptions::default();
            let expected = if deadline {
                options.deadline = Some(std::time::Instant::now());
                ErrorKind::Deadline
            } else {
                let cancellation = CancellationToken::new();
                cancellation.cancel();
                options.cancellation = cancellation;
                ErrorKind::Cancelled
            };
            let mut ctx = CallContext::new(options);
            assert_eq!(
                mismatch(
                    &mut ctx,
                    &Type::named("int".into()),
                    &input(),
                    Context::Argument(b"payload")
                )
                .unwrap_err()
                .kind,
                expected
            );
            assert_eq!(ctx.stats().peak_memory_bytes, 0);
        }
    }
}
