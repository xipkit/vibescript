use crate::{
    CallContext, Error, ErrorKind, Result, Value,
    budget::Buffer,
    value::{Bytes, Kind},
};
use std::{
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

mod data;
mod seeded;
pub(crate) use seeded::Seeded;

pub(crate) type Source = Arc<dyn Fn(&mut CallContext, &mut [u8]) -> Result<usize> + Send + Sync>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Method {
    Rand,
    Seed,
    Uuid,
    Id,
}

impl Method {
    pub fn name(self) -> &'static str {
        match self {
            Self::Rand => "rand",
            Self::Seed => "srand",
            Self::Uuid => "uuid",
            Self::Id => "random_id",
        }
    }

    pub fn call(
        self,
        ctx: &mut CallContext,
        args: &[Value],
        keywords: &[(Value, Value)],
        block: bool,
    ) -> Result<Value> {
        ctx.checkpoint()?;
        if !keywords.is_empty() || block {
            return Err(Error::new(
                ErrorKind::Argument,
                "random functions do not accept keywords or blocks",
            ));
        }
        if args.len() > usize::from(self != Self::Uuid) {
            return Err(Error::new(
                ErrorKind::Argument,
                "too many random function arguments",
            ));
        }
        match self {
            Self::Rand => rand(ctx, args.first()),
            Self::Seed => seed(ctx, args.first()),
            Self::Uuid => uuid(ctx),
            Self::Id => identifier(ctx, args.first()),
        }
    }
}

fn read(ctx: &mut CallContext, mut output: &mut [u8]) -> Result<()> {
    let source = ctx.random_source.clone();
    while !output.is_empty() {
        ctx.charge(1)?;
        ctx.work_bytes(output.len())?;
        let read = if let Some(reader) = &source {
            reader(ctx, output)
        } else {
            getrandom::fill(output)
                .map(|()| output.len())
                .map_err(|error| {
                    Error::new(ErrorKind::Host, format!("entropy source failed: {error}"))
                })
        };
        ctx.checkpoint()?;
        let count = match read {
            Ok(count) => count,
            Err(error) if error.exhaustion() => return ctx.fail(error.kind, &error.message),
            Err(error) => return Err(error),
        };
        if count == 0 || count > output.len() {
            return Err(Error::new(
                ErrorKind::Host,
                "entropy source returned an invalid byte count",
            ));
        }
        output = &mut output[count..];
    }
    Ok(())
}

fn entropy(ctx: &mut CallContext) -> Result<u64> {
    let mut bytes = [0; 8];
    read(ctx, &mut bytes)?;
    Ok(u64::from_be_bytes(bytes))
}

fn next(ctx: &mut CallContext) -> Result<u64> {
    ctx.charge(1)?;
    if let Some(random) = &mut ctx.random {
        Ok(random.next())
    } else {
        entropy(ctx)
    }
}

fn bounded(ctx: &mut CallContext, bound: u64) -> Result<u64> {
    debug_assert!(bound > 0);
    if ctx.random.is_some() && bound <= i64::MAX as u64 {
        if bound.is_power_of_two() {
            return Ok(next(ctx)? & (bound - 1));
        }
        let maximum = (i64::MAX as u64) - (1u64 << 63) % bound;
        loop {
            let raw = next(ctx)? & i64::MAX as u64;
            if raw <= maximum {
                return Ok(raw % bound);
            }
        }
    }
    let maximum = u64::MAX - bound.wrapping_neg() % bound;
    loop {
        let raw = next(ctx)?;
        if raw <= maximum {
            return Ok(raw % bound);
        }
    }
}

fn rand(ctx: &mut CallContext, argument: Option<&Value>) -> Result<Value> {
    match argument.map(|v| &v.0) {
        None | Some(Kind::Nil) => {
            if ctx.random.is_some() {
                loop {
                    let value = (next(ctx)? & i64::MAX as u64) as f64 / (1u64 << 63) as f64;
                    if value < 1.0 {
                        return Ok(Value::float(value));
                    }
                }
            } else {
                Ok(Value::float(
                    (entropy(ctx)? >> 11) as f64 / (1u64 << 53) as f64,
                ))
            }
        }
        Some(Kind::Int(bound)) if *bound > 0 => Ok(Value::int(bounded(ctx, *bound as u64)? as i64)),
        Some(Kind::Range(range)) => {
            let (Some(mut low), Some(mut high)) = (range.start, range.end) else {
                return Err(Error::new(
                    ErrorKind::Argument,
                    "random range must be bounded",
                ));
            };
            if low > high {
                std::mem::swap(&mut low, &mut high);
                if range.exclusive {
                    low += 1;
                }
            } else if range.exclusive {
                let Some(end) = high.checked_sub(1) else {
                    return Err(Error::new(ErrorKind::Argument, "random range is empty"));
                };
                high = end;
            }
            if low > high {
                return Err(Error::new(ErrorKind::Argument, "random range is empty"));
            }
            let size = (high as u64).wrapping_sub(low as u64).wrapping_add(1);
            let offset = if size == 0 {
                next(ctx)?
            } else {
                bounded(ctx, size)?
            };
            Ok(Value::int((low as u64).wrapping_add(offset) as i64))
        }
        _ => Err(Error::new(
            ErrorKind::Argument,
            "rand expects a positive 64-bit integer or bounded integer range",
        )),
    }
}

fn seed(ctx: &mut CallContext, argument: Option<&Value>) -> Result<Value> {
    let explicit = match argument.map(|v| &v.0) {
        None | Some(Kind::Nil) => None,
        Some(Kind::Int(seed)) => Some(*seed),
        _ => {
            return Err(Error::new(
                ErrorKind::Argument,
                "seed must be a 64-bit integer or nil",
            ));
        }
    };
    if ctx.random.is_none() {
        ctx.check_memory(Seeded::storage())?;
    }
    let seed = match explicit {
        Some(seed) => seed,
        None => entropy(ctx)? as i64,
    };
    let previous = ctx.random.take();
    let result = previous
        .as_ref()
        .map_or_else(Value::nil, |old| Value::int(old.seed));
    ctx.random = Some(Seeded::initialize(ctx, seed, previous)?);
    Ok(result)
}

fn uuid(ctx: &mut CallContext) -> Result<Value> {
    ctx.check_memory(Bytes::header_bytes() + 36)?;
    let mut raw = [0; 16];
    read(ctx, &mut raw)?;
    let millis = match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(duration) => duration.as_millis() as u64,
        Err(error) => (-(error.duration().as_nanos().div_ceil(1_000_000) as i128)) as u64,
    };
    raw[..6].copy_from_slice(&millis.to_be_bytes()[2..]);
    raw[6] = (raw[6] & 0x0f) | 0x70;
    raw[8] = (raw[8] & 0x3f) | 0x80;
    let mut text = [0; 36];
    let mut position = 0;
    const HEX: &[u8] = b"0123456789abcdef";
    for (i, byte) in raw.into_iter().enumerate() {
        if matches!(i, 4 | 6 | 8 | 10) {
            text[position] = b'-';
            position += 1;
        }
        text[position] = HEX[(byte >> 4) as usize];
        text[position + 1] = HEX[(byte & 15) as usize];
        position += 2;
    }
    ctx.bytes(&text)
}

fn identifier(ctx: &mut CallContext, argument: Option<&Value>) -> Result<Value> {
    let length = match argument.map(|v| &v.0) {
        None => 16,
        Some(Kind::Int(length)) if *length > 0 => *length,
        _ => {
            return Err(Error::new(
                ErrorKind::Argument,
                "random_id length must be a positive 64-bit integer",
            ));
        }
    };
    if length > 1024 {
        return ctx.fail(ErrorKind::OutputLimit, "random_id length exceeds 1024");
    }
    let length = length as usize;
    ctx.check_memory(Bytes::header_bytes() + length)?;
    let mut output = Buffer::with_capacity(ctx, length)?;
    let mut scratch = [0; 1024];
    let mut stalled = 0;
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    while output.data.len() < length {
        let needed = length - output.data.len();
        read(ctx, &mut scratch[..needed])?;
        let before = output.data.len();
        for &byte in &scratch[..needed] {
            ctx.charge(1)?;
            if byte < 248 {
                output
                    .data
                    .push(ALPHABET[usize::from(byte) % ALPHABET.len()]);
            }
        }
        if output.data.len() == before {
            stalled += 1;
            if stalled > 8 {
                return Err(Error::new(
                    ErrorKind::Host,
                    "entropy source rejected too many bytes",
                ));
            }
        } else {
            stalled = 0;
        }
    }
    Value::from_bytes(ctx, output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CallOptions;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn context(byte: u8) -> (CallContext, Arc<AtomicUsize>) {
        let reads = Arc::new(AtomicUsize::new(0));
        let count = reads.clone();
        let mut ctx = CallContext::new(CallOptions::default());
        ctx.random_source = Some(Arc::new(move |_, output| {
            count.fetch_add(1, Ordering::SeqCst);
            output.fill(byte);
            Ok(output.len())
        }));
        (ctx, reads)
    }

    #[test]
    fn output_and_seed_storage_are_checked_before_entropy_is_read() {
        for method in [Method::Uuid, Method::Id, Method::Seed] {
            let (mut ctx, reads) = context(0);
            let storage = match method {
                Method::Uuid => Bytes::header_bytes() + 36,
                Method::Id => Bytes::header_bytes() + 16,
                Method::Seed => Seeded::storage(),
                _ => unreachable!(),
            };
            ctx.options.limits.memory_bytes = Some(storage - 1);
            assert_eq!(
                method.call(&mut ctx, &[], &[], false).unwrap_err().kind,
                ErrorKind::Memory
            );
            assert_eq!(reads.load(Ordering::SeqCst), 0);
            assert_eq!(ctx.stats().peak_memory_bytes, 0);
            assert_eq!(ctx.checkpoint().unwrap_err().kind, ErrorKind::Memory);
        }
    }

    #[test]
    fn identifier_and_uuid_outputs_use_exact_capacity_and_release_storage() {
        for (method, length) in [(Method::Uuid, 36), (Method::Id, 16)] {
            let (mut ctx, reads) = context(0xab);
            let storage = Bytes::header_bytes() + length;
            ctx.options.limits.memory_bytes = Some(storage);
            let output = method.call(&mut ctx, &[], &[], false).unwrap();
            assert_eq!(output.as_bytes().unwrap().len(), length);
            assert_eq!(ctx.stats().retained_memory_bytes, storage);
            assert_eq!(ctx.stats().peak_memory_bytes, storage);
            assert_eq!(reads.load(Ordering::SeqCst), 1);
            drop(output);
            assert_eq!(ctx.stats().retained_memory_bytes, 0);
        }
    }

    #[test]
    fn reseeding_reuses_storage_and_interrupted_seeding_reclaims_it() {
        let (mut ctx, reads) = context(0);
        ctx.options.limits.memory_bytes = Some(Seeded::storage());
        assert_eq!(
            seed(&mut ctx, Some(&Value::int(7))).unwrap().type_name(),
            "nil"
        );
        assert_eq!(
            seed(&mut ctx, Some(&Value::int(42))).unwrap().as_int(),
            Some(7)
        );
        assert_eq!(ctx.stats().peak_memory_bytes, Seeded::storage());
        assert_eq!(reads.load(Ordering::SeqCst), 0);
        let before = ctx.stats().steps;
        ctx.options.limits.steps = Some(before + 10);
        assert_eq!(
            seed(&mut ctx, Some(&Value::int(1))).unwrap_err().kind,
            ErrorKind::Steps
        );
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
        assert!(ctx.random.is_none());
        assert_eq!(ctx.checkpoint().unwrap_err().kind, ErrorKind::Steps);
    }

    #[test]
    fn entropy_failures_preserve_the_previous_seed_and_release_partial_output() {
        let (mut ctx, _) = context(0);
        seed(&mut ctx, Some(&Value::int(7))).unwrap();
        ctx.random_source = Some(Arc::new(|_, _| {
            Err(Error::new(ErrorKind::Host, "no entropy"))
        }));
        assert_eq!(seed(&mut ctx, None).unwrap_err().kind, ErrorKind::Host);
        assert_eq!(ctx.random.as_ref().unwrap().seed, 7);
        let baseline = ctx.stats().retained_memory_bytes;
        assert_eq!(
            identifier(&mut ctx, None).unwrap_err().kind,
            ErrorKind::Host
        );
        assert_eq!(ctx.stats().retained_memory_bytes, baseline);
        assert_eq!(
            seed(&mut ctx, Some(&Value::int(42))).unwrap().as_int(),
            Some(7)
        );
    }

    #[test]
    fn rejection_sampling_observes_work_and_cancellation_limits() {
        let (mut ctx, reads) = context(255);
        ctx.options.limits.steps = Some(20);
        assert_eq!(
            rand(&mut ctx, Some(&Value::int(3))).unwrap_err().kind,
            ErrorKind::Steps
        );
        assert!(reads.load(Ordering::SeqCst) > 1);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
        assert_eq!(ctx.checkpoint().unwrap_err().kind, ErrorKind::Steps);

        let (mut ctx, reads) = context(255);
        let count = reads.clone();
        ctx.random_source = Some(Arc::new(move |ctx, output| {
            count.fetch_add(1, Ordering::SeqCst);
            output.fill(255);
            ctx.cancellation().cancel();
            Ok(output.len())
        }));
        assert_eq!(
            identifier(&mut ctx, None).unwrap_err().kind,
            ErrorKind::Cancelled
        );
        assert_eq!(reads.load(Ordering::SeqCst), 1);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
        assert_eq!(ctx.checkpoint().unwrap_err().kind, ErrorKind::Cancelled);
    }
}
