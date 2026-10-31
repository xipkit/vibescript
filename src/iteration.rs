use crate::{
    CallContext, Error, ErrorKind, Result, Value,
    budget::{Buffer, Charge, MAX_VALUE_DEPTH},
    bytecode::Method,
    collections,
    hash::Hash,
    hash_blocks, mutate, ops, ordering,
    value::Kind,
};

mod array;
mod bounds;
mod common;
mod driver;
mod hash;
mod range;

pub(crate) use common::Pool;
pub(crate) use driver::{Loop, start_pooled};

#[derive(Clone, Copy, PartialEq, Eq)]
enum MethodKind {
    Each,
    EachIndex,
    EachKey,
    EachValue,
    EachSlice,
    EachCons,
    ReverseEach,
    Cycle,
    Map,
    MapIndex,
    FlatMap,
    FilterMap,
    Select,
    Reject,
    TakeWhile,
    DropWhile,
    SliceWhen,
    ChunkWhile,
    // `array.chunk { |item| key }`: consecutive equal keys form `[key, group]`
    // rows. Not parsed by name: the blockless spelling is the sized form
    // in `collections`, so `start` resolves this only for arrays with a block.
    Chunk,
    Find,
    Index,
    Rindex,
    Reduce,
    Count,
    Any,
    All,
    NoneMatch,
    One,
    Partition,
    GroupBy,
    GroupStable,
    Tally,
    ToHash,
    Fetch,
    FetchValues,
    TransformKeys,
    TransformValues,
    Grep,
    GrepV,
    Uniq,
    Sum,
    Min,
    Max,
    Times,
    Upto,
    Downto,
    Step,
    DeleteIf,
    KeepIf,
    Fill,
    Delete,
}

impl MethodKind {
    fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "each" => Self::Each,
            "each_with_index" => Self::EachIndex,
            "each_key" => Self::EachKey,
            "each_value" => Self::EachValue,
            "each_slice" => Self::EachSlice,
            "each_cons" => Self::EachCons,
            "reverse_each" => Self::ReverseEach,
            "cycle" => Self::Cycle,
            "map" => Self::Map,
            "map_with_index" => Self::MapIndex,
            "flat_map" => Self::FlatMap,
            "filter_map" => Self::FilterMap,
            "select" => Self::Select,
            "reject" => Self::Reject,
            "take_while" => Self::TakeWhile,
            "drop_while" => Self::DropWhile,
            "slice_when" => Self::SliceWhen,
            "chunk_while" => Self::ChunkWhile,
            "find" => Self::Find,
            "index" => Self::Index,
            "rindex" => Self::Rindex,
            "reduce" => Self::Reduce,
            "count" => Self::Count,
            "any?" => Self::Any,
            "all?" => Self::All,
            "none?" => Self::NoneMatch,
            "one?" => Self::One,
            "partition" => Self::Partition,
            "group_by" => Self::GroupBy,
            "group_by_stable" => Self::GroupStable,
            "tally" => Self::Tally,
            "to_h" => Self::ToHash,
            "fetch" => Self::Fetch,
            "fetch_values" => Self::FetchValues,
            "transform_keys" => Self::TransformKeys,
            "transform_values" => Self::TransformValues,
            "grep" => Self::Grep,
            "grep_v" => Self::GrepV,
            "uniq" => Self::Uniq,
            "sum" => Self::Sum,
            "min" => Self::Min,
            "max" => Self::Max,
            "times" => Self::Times,
            "upto" => Self::Upto,
            "downto" => Self::Downto,
            "step" => Self::Step,
            "delete_if" => Self::DeleteIf,
            "keep_if" => Self::KeepIf,
            "fill" => Self::Fill,
            "delete" => Self::Delete,
            _ => return None,
        })
    }
}

pub(crate) fn method(name: &str) -> bool {
    MethodKind::parse(name).is_some()
        || ordering::method(name)
        || hash_blocks::method(name)
        || crate::text::iteration::method(name)
        || matches!(name, "match" | "scan")
        || crate::regex::substitute::method(name)
}

pub(crate) enum Progress {
    Yield([Value; 3], usize),
    Done(Value),
}

pub(crate) enum Mutation {
    Replace(Value),
    DeleteKeys(Buffer<Value>),
}

impl Mutation {
    pub fn apply(
        self,
        ctx: &mut CallContext,
        receiver: Value,
        result: Value,
    ) -> Result<(Value, Value)> {
        match self {
            Self::Replace(value) => Ok((value, result)),
            Self::DeleteKeys(mut keys) => {
                // Resolve every key once against the hash as it stands after
                // the block ran, then compact, shrink, recompute depth and
                // rebuild the index in one batched pass. Removing the keys one
                // at a time shifts the buffer and rebuilds the whole index per
                // key, which makes a bulk `delete_if` quadratic in the hash
                // size. A key the block already removed resolves to none and
                // is skipped, as `delete_hash` would skip it.
                let Kind::Hash(mut hash) = receiver.0 else {
                    unreachable!()
                };
                // Reserve nothing up front: a block that removed many of its
                // own keys leaves them unresolvable, and the eager worst-case
                // reservation could refuse a deletion whose every key is
                // skipped. The buffer grows as keys resolve.
                let mut indices = Buffer::empty();
                for key in keys.data.drain(..) {
                    ctx.charge(1)?;
                    if let Some(index) = hash.find(ctx, key.require_bytes()?)? {
                        indices.push(ctx, index)?;
                    }
                }
                // The drained keys' storage is dead: release it before the
                // batched allocations, so a deletion near the quota does not
                // carry the empty buffer's reservation through the hash copy,
                // the position bitmap and the index rebuild.
                drop(keys);
                if !indices.data.is_empty() {
                    Hash::make_mut(ctx, &mut hash)?.remove_keys(ctx, indices)?;
                }
                let updated = Value(Kind::Hash(hash));
                Ok((updated.clone(), updated))
            }
        }
    }
}

pub(crate) enum Iteration {
    Common(Box<State<common::Driver>>),
    Pooled,
    Loop(Box<State<Loop>>),
    Forever { waiting: bool },
    Order(Box<State<ordering::Driver>>),
    Hash(Box<State<hash_blocks::Driver>>),
    Text(Box<State<crate::text::iteration::Driver>>),
    Regex(Box<State<crate::regex::operations::Driver>>),
    Substitute(Box<State<crate::regex::substitute::Driver>>),
}

/// A suspended driver and the reservation for its allocation.
pub(crate) struct State<T> {
    value: T,
    _charge: Option<Charge>,
}

impl<T> std::ops::Deref for State<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.value
    }
}

impl<T> std::ops::DerefMut for State<T> {
    fn deref_mut(&mut self) -> &mut T {
        &mut self.value
    }
}

fn boxed<T>(ctx: &mut CallContext, value: T) -> Result<Box<State<T>>> {
    let charge = ctx.reserve(size_of::<State<T>>())?;
    Ok(Box::new(State {
        value,
        _charge: charge,
    }))
}

impl Iteration {
    pub fn waiting(&self) -> bool {
        match self {
            Self::Pooled => unreachable!(),
            Self::Common(state) => state.waiting,
            Self::Loop(state) => state.waiting,
            Self::Forever { waiting } => *waiting,
            Self::Order(state) => state.waiting,
            Self::Hash(state) => state.waiting(),
            Self::Text(state) => state.waiting,
            Self::Regex(state) => state.waiting,
            Self::Substitute(state) => state.waiting,
        }
    }

    pub fn take_mutation(&mut self) -> Option<Mutation> {
        match self {
            Self::Loop(state) => state.mutation.take(),
            Self::Pooled
            | Self::Common(_)
            | Self::Forever { .. }
            | Self::Order(_)
            | Self::Hash(_)
            | Self::Text(_)
            | Self::Regex(_)
            | Self::Substitute(_) => None,
        }
    }

    pub fn advance(&mut self, ctx: &mut CallContext, returned: Option<Value>) -> Result<Progress> {
        match self {
            Self::Pooled => unreachable!(),
            Self::Common(state) => state.advance(ctx, returned),
            Self::Loop(state) => state.advance(ctx, returned),
            Self::Forever { waiting } => {
                drop(returned);
                *waiting = true;
                Ok(Progress::Yield(
                    [Value::nil(), Value::nil(), Value::nil()],
                    0,
                ))
            }
            Self::Order(state) => state.advance(ctx, returned),
            Self::Hash(state) => state.advance(ctx, returned),
            Self::Text(state) => state.advance(ctx, returned),
            Self::Regex(state) => state.advance(ctx, returned),
            Self::Substitute(state) => state.advance(ctx, returned),
        }
    }
}

fn argument(message: &str) -> Error {
    Error::new(ErrorKind::Argument, message)
}

pub(crate) fn forever(
    ctx: &mut CallContext,
    args: &[Value],
    keywords: &[(Value, Value)],
    block: bool,
) -> Result<Iteration> {
    ctx.checkpoint()?;
    if !args.is_empty() {
        return Err(argument("loop does not take arguments"));
    }
    if !keywords.is_empty() {
        return Err(argument("loop does not take keyword arguments"));
    }
    if !block {
        return Err(argument("loop requires a block"));
    }
    Ok(Iteration::Forever { waiting: false })
}

pub(crate) fn start(
    ctx: &mut CallContext,
    name: &str,
    receiver: &Value,
    args: &[Value],
    keywords: &[(Value, Value)],
    block_arity: Option<usize>,
) -> Result<Option<Iteration>> {
    start_pooled(ctx, name, receiver, args, keywords, block_arity, None)
}

pub(crate) fn without_block(
    ctx: &mut CallContext,
    name: &str,
    receiver: &Value,
    args: &[Value],
) -> Result<Option<Value>> {
    let Some(mut state) = start(ctx, name, receiver, args, &[], None)? else {
        return Ok(None);
    };
    match state.advance(ctx, None)? {
        Progress::Done(value) => Ok(Some(value)),
        Progress::Yield(..) => unreachable!(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CallOptions, Limits};

    #[test]
    fn suspended_driver_reservations_are_released_and_preflighted() {
        let mut ctx = CallContext::new(CallOptions::default());
        let state = start(&mut ctx, "times", &Value::int(1), &[], &[], Some(0))
            .unwrap()
            .unwrap();
        assert_eq!(
            ctx.stats().retained_memory_bytes,
            size_of::<State<common::Driver>>()
        );
        drop(state);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
        ctx.options.limits.memory_bytes = Some(size_of::<State<common::Driver>>() - 1);
        assert!(
            matches!(start(&mut ctx, "times", &Value::int(1), &[], &[], Some(0)), Err(error) if error.kind == ErrorKind::Memory)
        );
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }

    #[test]
    fn chunk_reserved_key_diagnostics_preflight_memory() {
        let mut ctx = CallContext::new(CallOptions {
            limits: Limits {
                memory_bytes: Some(100_000),
                ..Limits::default()
            },
            ..CallOptions::default()
        });
        let receiver = ctx.array(&[Value::int(1)]).unwrap();
        let key = ctx.import(&Value::symbol(vec![b'_'; 8192])).unwrap();
        let mut state = start(&mut ctx, "chunk", &receiver, &[], &[], Some(0))
            .unwrap()
            .unwrap();
        assert!(matches!(
            state.advance(&mut ctx, None).unwrap(),
            Progress::Yield(..)
        ));
        ctx.options.limits.memory_bytes = Some(ctx.stats().retained_memory_bytes + 1);
        let Err(error) = state.advance(&mut ctx, Some(key)) else {
            panic!("reserved key accepted")
        };
        assert_eq!(error.kind, ErrorKind::Memory);
        assert_eq!(ctx.charge(1).unwrap_err().kind, ErrorKind::Memory);
        drop((state, receiver));
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
}
