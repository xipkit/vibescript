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
mod hash;
mod range;

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
        mut receiver: Value,
        result: Value,
    ) -> Result<(Value, Value)> {
        match self {
            Self::Replace(value) => Ok((value, result)),
            Self::DeleteKeys(mut keys) => {
                for key in keys.data.drain(..) {
                    ctx.charge(1)?;
                    receiver = receiver.delete_hash(ctx, &key)?.0;
                }
                Ok((receiver.clone(), receiver))
            }
        }
    }
}

pub(crate) enum Iteration {
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
            Self::Forever { .. }
            | Self::Order(_)
            | Self::Hash(_)
            | Self::Text(_)
            | Self::Regex(_)
            | Self::Substitute(_) => None,
        }
    }

    pub fn advance(&mut self, ctx: &mut CallContext, returned: Option<Value>) -> Result<Progress> {
        match self {
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

pub(crate) struct Loop {
    method: MethodKind,
    receiver: Value,
    position: i128,
    length: i128,
    start: i64,
    stride: i64,
    width: usize,
    cycles: i64,
    block: bool,
    collapse_pair: bool,
    pub waiting: bool,
    pub mutation: Option<Mutation>,
    pending: [Value; 2],
    pending_index: u64,
    // Aggregation state, or the pattern for grep/count/predicate methods.
    accumulator: Option<Value>,
    count: i64,
    dropping: bool,
    output: Buffer<Value>,
    other: Buffer<Value>,
    inputs: Buffer<Value>,
    hash: Hash,
    /// Block keys already seen by `uniq`, indexing `other`.
    keys: crate::sets::Index,
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
    use MethodKind::*;
    if crate::regex::substitute::method(name) {
        return crate::regex::substitute::Driver::new(
            ctx,
            name,
            receiver,
            args,
            keywords,
            block_arity.is_some(),
        )
        .and_then(|state| {
            state
                .map(|state| boxed(ctx, state).map(Iteration::Substitute))
                .transpose()
        });
    }
    let keywords = !keywords.is_empty();
    if matches!(name, "match" | "scan") {
        return crate::regex::operations::Driver::new(
            ctx,
            name,
            receiver,
            args,
            keywords,
            block_arity.is_some(),
        )
        .and_then(|state| {
            state
                .map(|state| boxed(ctx, state).map(Iteration::Regex))
                .transpose()
        });
    }
    if crate::text::iteration::method(name) {
        return crate::text::iteration::Driver::new(
            ctx,
            name,
            receiver,
            args,
            keywords,
            block_arity.is_some(),
        )
        .and_then(|state| {
            state
                .map(|state| boxed(ctx, state).map(Iteration::Text))
                .transpose()
        });
    }
    if ordering::method(name) && !matches!(receiver.0, Kind::Range(_)) {
        return ordering::Driver::new(ctx, name, receiver, args, block_arity.is_some()).and_then(
            |state| {
                state
                    .map(|state| boxed(ctx, state).map(Iteration::Order))
                    .transpose()
            },
        );
    }
    if hash_blocks::method(name) {
        return hash_blocks::Driver::new(
            ctx,
            name,
            receiver,
            args,
            keywords,
            block_arity.is_some(),
        )
        .and_then(|state| {
            state
                .map(|state| boxed(ctx, state).map(Iteration::Hash))
                .transpose()
        });
    }
    if matches!(&receiver.0, Kind::Hash(hash) if hash.tag.protected())
        && matches!(name, "delete_if" | "keep_if" | "delete")
    {
        let Kind::Hash(hash) = &receiver.0 else {
            unreachable!()
        };
        return Err(hash.tag.mutation_error(name));
    }
    let chunk_by_block =
        name == "chunk" && block_arity.is_some() && matches!(receiver.0, Kind::Array(_));
    let Some(method) = MethodKind::parse(name).or(chunk_by_block.then_some(Chunk)) else {
        return Ok(None);
    };
    let supported = match &receiver.0 {
        Kind::Array(_) => !matches!(
            method,
            EachKey
                | EachValue
                | FetchValues
                | TransformKeys
                | TransformValues
                | Times
                | Upto
                | Downto
                | Step
        ),
        Kind::Hash(_) => matches!(
            method,
            Each | EachIndex
                | EachKey
                | EachValue
                | Map
                | MapIndex
                | Select
                | Reject
                | Fetch
                | FetchValues
                | TransformKeys
                | TransformValues
                | DeleteIf
                | KeepIf
                | Delete
        ),
        Kind::Range(_) => matches!(
            method,
            Each | Map | Select | Reject | Find | Reduce | Count | Step | Sum | Min | Max
        ),
        Kind::Int(_) | Kind::Big(_) => matches!(method, Times | Upto | Downto | Step),
        _ => false,
    };
    if !supported {
        return Ok(None);
    }
    let has_block = block_arity.is_some();
    match receiver.0 {
        Kind::Array(_) => array::check(name, method, args, keywords, has_block)?,
        Kind::Hash(_) => hash::check(name, method, args, keywords, has_block)?,
        Kind::Range(_) => range::check(name, method, args, keywords, has_block)?,
        _ => {}
    }
    let is_range = matches!(receiver.0, Kind::Range(_));
    if is_range && matches!(method, Sum | Min | Max) {
        bounds::aggregate(name, method, args, keywords, has_block)?;
    }
    bounds::stepping(ctx, name, method, receiver, args, keywords, has_block)?;
    if method == Chunk {
        // Reference order: positional arguments, then keywords, before the
        // block runs or the receiver is read.
        if !args.is_empty() {
            return Err(argument(
                "array.chunk does not take arguments when a block is supplied",
            ));
        }
        if keywords {
            return Err(argument("array.chunk does not take keyword arguments"));
        }
    }
    if !has_block
        && matches!(
            method,
            Index | Rindex | Fetch | ToHash | Uniq | Fill | Delete
        )
    {
        return Ok(None);
    }
    if !has_block && method == Sum && args.is_empty() && !is_range {
        return Ok(None);
    }
    let is_hash = matches!(receiver.0, Kind::Hash(_));
    let is_int = matches!(receiver.0, Kind::Int(_) | Kind::Big(_));
    let rejects_keywords = is_range
        || (is_int && method != Times)
        || matches!(
            method,
            EachIndex
                | MapIndex
                | FlatMap
                | FilterMap
                | Find
                | Reduce
                | Any
                | All
                | NoneMatch
                | ToHash
                | Uniq
                | Sum
                | DeleteIf
                | KeepIf
                | Fill
                | Delete
                | SliceWhen
                | ChunkWhile
                | Chunk
        )
        || (is_hash && method == Map);
    if keywords && rejects_keywords {
        return Err(argument(&format!(
            "{name} does not accept keyword arguments"
        )));
    }
    let max_args = match method {
        Each | Map | Select if !is_hash && !is_range => usize::MAX,
        Cycle | Find | Count | Any | All | NoneMatch | Sum | Grep | GrepV | EachSlice
        | EachCons | Upto | Downto => 1,
        Fetch | Fill => 2,
        Delete => 1,
        FetchValues => usize::MAX,
        Reduce => 1,
        Step if is_int => 2,
        Step => 1,
        _ => 0,
    };
    if args.len() > max_args || (is_range && matches!(method, Count | Find) && !args.is_empty()) {
        return Err(argument(&format!("invalid arguments to {name}")));
    }
    if method == Find && args.first().is_some_and(|v| !matches!(v.0, Kind::Nil)) {
        return Err(argument("find takes no fallback; a miss returns nil"));
    }
    let mut state = Loop {
        method,
        receiver: receiver.clone(),
        position: 0,
        length: 0,
        start: 0,
        stride: 1,
        width: 1,
        cycles: 1,
        block: has_block,
        collapse_pair: block_arity == Some(1),
        waiting: false,
        mutation: None,
        pending: [Value::nil(), Value::nil()],
        pending_index: 0,
        accumulator: None,
        count: 0,
        dropping: method == DropWhile,
        output: Buffer::empty(),
        other: Buffer::empty(),
        inputs: Buffer::empty(),
        hash: Hash::empty(),
        keys: crate::sets::Index::new(),
    };
    if matches!(
        method,
        Grep | GrepV | EachSlice | EachCons | Upto | Downto | Step
    ) && args.is_empty()
    {
        return Err(argument(&format!("{name} requires an argument")));
    }
    if matches!(method, EachSlice | EachCons) {
        let width = args[0].require_int()?;
        if width <= 0 {
            return Err(argument("slice or window size must be positive"));
        }
        state.width = usize::try_from(width).unwrap_or(usize::MAX);
    }
    if method == Cycle {
        state.cycles = match args.first().filter(|v| !matches!(v.0, Kind::Nil)) {
            Some(value) => value.require_int()?.max(0),
            None => -1,
        };
    }
    if matches!(method, Grep | GrepV | Any | All | NoneMatch | Count) {
        state.accumulator = args.first().cloned();
        if state.accumulator.is_some() && !matches!(method, Grep | GrepV) {
            state.block = false;
        }
    }
    if method == Reduce {
        state.accumulator = args.first().cloned();
    }
    if method == Sum {
        state.accumulator = Some(args.first().cloned().unwrap_or_else(|| Value::int(0)));
    }
    let optional = matches!(
        method,
        Count | Any | All | NoneMatch | One | Tally | Grep | GrepV | Sum | Min | Max | FetchValues
    );
    if !has_block && !optional {
        return Err(argument(&format!("{name} requires a block")));
    }
    state.length = match &receiver.0 {
        Kind::Array(array) => array.buffer.data.len() as i128,
        Kind::Hash(hash) => hash.buffer.data.len() as i128,
        Kind::Range(range) => {
            state.start = range
                .start
                .ok_or_else(|| argument("cannot iterate a beginless range"))?;
            state.stride = if range.start > range.end { -1 } else { 1 };
            let length = range.length()?;
            if method == Step {
                let stride = args[0].require_int()?;
                state.stride *= stride;
                (length + i128::from(stride) - 1) / i128::from(stride)
            } else {
                length
            }
        }
        Kind::Int(n) => {
            if method == Times {
                i128::from((*n).max(0))
            } else {
                let limit = i128::from(args[0].require_int()?);
                state.start = *n;
                state.stride = if method == Downto { -1 } else { 1 };
                if method == Step && args.len() == 2 {
                    state.stride = args[1].require_int()?;
                }
                let distance =
                    (limit - i128::from(state.start)) * i128::from(state.stride).signum();
                if distance < 0 {
                    0
                } else {
                    distance / i128::from(state.stride).abs() + 1
                }
            }
        }
        _ => unreachable!(),
    };
    if method == EachCons {
        state.length = (state.length - state.width as i128 + 1).max(0);
    }
    if matches!(method, SliceWhen | ChunkWhile) && state.length != 0 {
        state.position = 1;
    }
    if method == Cycle && state.cycles == 0 {
        state.length = 0;
    }
    if method == Fetch {
        ops::arity(&args[..args.len().min(1)], 1)?;
        let key = if matches!(receiver.0, Kind::Array(_)) {
            let n = crate::sequence::integer(&args[0])?;
            if matches!(args[0].0, Kind::Float(n) if n.trunc() != n) {
                return Err(argument("fetch index must be integer"));
            }
            Value::int(n)
        } else {
            args[0].clone()
        };
        if let Some(value) =
            collections::lookup(ctx, receiver, &key, true, Some("hash.fetch key is an"))?
        {
            state.accumulator = Some(value);
            state.length = 0;
        } else {
            state.inputs.push(ctx, key)?;
            state.length = 1;
        }
    }
    if method == FetchValues {
        state.inputs.extend(ctx, args)?;
        state.length = args.len() as i128;
    }
    if method == Fill {
        let original = receiver.as_array().unwrap().len();
        let (start, end, length) = mutate::fill_span(ctx, args, original)?;
        state.start = start as i64;
        state.width = end;
        state.length = length as i128;
        if start == end && length == original {
            state.accumulator = Some(receiver.clone());
            state.length = 0;
        }
    }
    if method == Delete {
        let (updated, removed) = mutate::call(ctx, Method::Delete, name, receiver.clone(), args)?;
        let changed = match (&receiver.0, &updated.0) {
            (Kind::Array(before), Kind::Array(after)) => {
                before.buffer.data.len() != after.buffer.data.len()
            }
            (Kind::Hash(before), Kind::Hash(after)) => {
                before.buffer.data.len() != after.buffer.data.len()
            }
            _ => unreachable!(),
        };
        if changed {
            state.mutation = Some(Mutation::Replace(updated));
            state.accumulator = Some(removed);
            state.length = 0;
        } else {
            state.inputs.push(ctx, args[0].clone())?;
            state.length = 1;
        }
    }
    Ok(Some(Iteration::Loop(boxed(ctx, state)?)))
}

impl Loop {
    pub fn advance(&mut self, ctx: &mut CallContext, returned: Option<Value>) -> Result<Progress> {
        if let Some(value) = returned {
            self.waiting = false;
            if self.accept(ctx, value)? {
                return self.finish(ctx).map(Progress::Done);
            }
        }
        loop {
            ctx.charge(1)?;
            if self.position >= self.length {
                if self.method != MethodKind::Cycle || self.length == 0 || self.cycles == 1 {
                    return self.finish(ctx).map(Progress::Done);
                }
                if self.cycles > 0 {
                    self.cycles -= 1;
                }
                self.position = 0;
            }
            if self.method == MethodKind::Fill
                && (self.position < i128::from(self.start) || self.position >= self.width as i128)
            {
                let value = self
                    .receiver
                    .as_array()
                    .unwrap()
                    .get(self.position as usize)
                    .cloned()
                    .unwrap_or_default();
                self.output.push(ctx, value)?;
                self.position += 1;
                continue;
            }
            let (args, count) = self.arguments(ctx)?;
            self.pending = args.clone();
            use MethodKind::*;
            if self.method == Reduce && self.accumulator.is_none() {
                self.accumulator = Some(args[0].clone());
                continue;
            }
            if self.method == FetchValues {
                if let Some(value) = collections::lookup(
                    ctx,
                    &self.receiver,
                    &args[0],
                    true,
                    Some("hash.fetch_values key is an"),
                )? {
                    self.output.push(ctx, value)?;
                    continue;
                }
                if !self.block {
                    return Err(collections::missing_key(
                        ctx,
                        "hash.fetch_values",
                        &args[0],
                    )?);
                }
            }
            if matches!(self.method, Grep | GrepV) {
                let matched = ops::case_matches(
                    ctx,
                    Some(&args[0]),
                    self.accumulator.as_ref().unwrap(),
                    false,
                )?;
                if matched != (self.method == Grep) {
                    continue;
                }
            }
            if self.block && !(self.method == DropWhile && !self.dropping) {
                let (args, count) = if self.method == Reduce {
                    ([self.accumulator.take().unwrap(), args[0].clone()], 2)
                } else {
                    (args, count)
                };
                self.waiting = true;
                let [first, second] = args;
                return Ok(Progress::Yield([first, second, Value::nil()], count));
            }
            let value = if let (Count, Some(pattern)) = (self.method, self.accumulator.as_ref()) {
                Value::boolean(ops::equal(ctx, &args[0], pattern, 0)?)
            } else if let (Any | All | NoneMatch, Some(pattern)) =
                (self.method, self.accumulator.as_ref())
            {
                Value::boolean(ops::case_matches(ctx, Some(&args[0]), pattern, false)?)
            } else {
                args[0].clone()
            };
            if self.accept(ctx, value)? {
                return self.finish(ctx).map(Progress::Done);
            }
        }
    }

    fn arguments(&mut self, ctx: &mut CallContext) -> Result<([Value; 2], usize)> {
        use MethodKind::*;
        let mut args = [Value::nil(), Value::nil()];
        let mut count = 1;
        let index = self.position;
        self.position += if self.method == EachSlice {
            self.width as i128
        } else {
            1
        };
        self.pending_index = index as u64;
        if matches!(self.method, SliceWhen | ChunkWhile) {
            let array = self.receiver.as_array().unwrap();
            args = [
                array[index as usize - 1].clone(),
                array[index as usize].clone(),
            ];
            count = 2;
        } else if self.method == Fill {
            args[0] = Value::int(index as i64);
        } else if matches!(self.method, Fetch | FetchValues | Delete) {
            args[0] = self.inputs.data[index as usize].clone();
        } else {
            match &self.receiver.0 {
                Kind::Array(array) => {
                    let index = if matches!(self.method, ReverseEach | Rindex) {
                        array.buffer.data.len() - 1 - index as usize
                    } else {
                        index as usize
                    };
                    self.pending_index = index as u64;
                    args[0] = if matches!(self.method, EachSlice | EachCons) {
                        let end = array
                            .buffer
                            .data
                            .len()
                            .min(index.saturating_add(self.width));
                        array_copy(ctx, &array.buffer.data[index..end])?
                    } else {
                        array.buffer.data[index].clone()
                    };
                    if matches!(self.method, EachIndex | MapIndex) {
                        args[1] = Value::int(index as i64);
                        count = 2;
                    }
                }
                Kind::Hash(hash) => {
                    let (key, value) = &hash.buffer.data[index as usize];
                    match self.method {
                        EachKey | TransformKeys => args[0] = key.clone(),
                        EachValue | TransformValues => args[0] = value.clone(),
                        EachIndex | MapIndex => {
                            args[0] = array_copy(ctx, &[key.clone(), value.clone()])?;
                            args[1] = Value::int(index as i64);
                            count = 2;
                        }
                        Each | Map if self.collapse_pair => {
                            args[0] = array_copy(ctx, &[key.clone(), value.clone()])?;
                        }
                        _ => {
                            args = [key.clone(), value.clone()];
                            count = 2;
                        }
                    }
                }
                Kind::Range(_) | Kind::Int(_) => {
                    args[0] = Value::int(
                        (i128::from(self.start) + index * i128::from(self.stride)) as i64,
                    )
                }
                _ => unreachable!(),
            }
        }
        Ok((args, count))
    }

    fn accept(&mut self, ctx: &mut CallContext, value: Value) -> Result<bool> {
        use MethodKind::*;
        let truthy = value.truthy();
        let depth = match self.method {
            Map | MapIndex | Grep | GrepV | FetchValues | Fill => value.depth() + 1,
            FlatMap if value.as_array().is_none() => value.depth() + 1,
            FilterMap if truthy => value.depth() + 1,
            Partition | GroupBy => self.pending[0].depth() + 2,
            GroupStable => self.pending[0].depth() + 3,
            _ => 0,
        };
        if depth > MAX_VALUE_DEPTH {
            return ctx.guard(ErrorKind::Recursion, "value nesting too deep");
        }
        match self.method {
            Each | EachIndex | EachKey | EachValue | EachSlice | EachCons | ReverseEach | Cycle
            | Times | Upto | Downto | Step => (),
            Fetch | Reduce | Delete => self.accumulator = Some(value),
            Map | MapIndex | Grep | GrepV | FetchValues | Fill => self.output.push(ctx, value)?,
            FlatMap => {
                if let Some(items) = value.as_array() {
                    self.output.extend(ctx, items)?;
                } else {
                    self.output.push(ctx, value)?;
                }
            }
            FilterMap if truthy => self.output.push(ctx, value)?,
            FilterMap => (),
            Select | Reject | DeleteIf | KeepIf => {
                let keep = truthy == matches!(self.method, Select | KeepIf);
                if matches!(self.receiver.0, Kind::Hash(_))
                    && matches!(self.method, DeleteIf | KeepIf)
                {
                    if !keep {
                        self.other.push(ctx, self.pending[0].clone())?;
                    }
                } else if keep {
                    if matches!(self.receiver.0, Kind::Hash(_)) {
                        self.hash
                            .insert(ctx, self.pending[0].clone(), self.pending[1].clone())?;
                    } else {
                        self.output.push(ctx, self.pending[0].clone())?;
                    }
                }
            }
            TakeWhile => {
                if !truthy {
                    return Ok(true);
                }
                self.output.push(ctx, self.pending[0].clone())?;
            }
            DropWhile => {
                if !truthy {
                    self.dropping = false;
                }
                if !self.dropping {
                    self.output.push(ctx, self.pending[0].clone())?;
                }
            }
            SliceWhen | ChunkWhile => {
                if truthy == (self.method == SliceWhen) {
                    self.flush_adjacent(ctx, self.pending_index as usize)?;
                }
            }
            Chunk => self.accept_chunk_key(ctx, value)?,
            Find | Index | Rindex if truthy => {
                self.accumulator = Some(if self.method == Find {
                    self.pending[0].clone()
                } else {
                    Value::int(self.pending_index as i64)
                });
                return Ok(true);
            }
            Find | Index | Rindex => (),
            Count | One => {
                if truthy {
                    self.count = self
                        .count
                        .checked_add(1)
                        .ok_or_else(|| argument("count overflow"))?;
                    if self.method == One && self.count == 2 {
                        return Ok(true);
                    }
                }
            }
            Any | All | NoneMatch => {
                if truthy != (self.method == All) {
                    self.count = 1;
                    return Ok(true);
                }
            }
            Partition => {
                if truthy {
                    &mut self.output
                } else {
                    &mut self.other
                }
                .push(ctx, self.pending[0].clone())?;
            }
            ToHash => {
                let pair = value
                    .as_array()
                    .ok_or_else(|| argument("array.to_h expects an array of two-element pairs"))?;
                if pair.len() != 2 {
                    return Err(argument("array.to_h pair must have exactly two elements"));
                }
                let key = ctx.bytes(pair[0].key_name_for("array.to_h pair key is an")?)?;
                self.hash.insert(ctx, key, pair[1].clone())?;
            }
            TransformKeys | TransformValues => {
                let (key, original) =
                    &self.receiver.as_hash().unwrap()[self.pending_index as usize];
                let (key, value) = if self.method == TransformKeys {
                    let key = value.key_name_for("hash.transform_keys block returned an")?;
                    (ctx.bytes(key)?, original.clone())
                } else {
                    (key.clone(), value)
                };
                self.hash.insert(ctx, key, value)?;
            }
            GroupBy | GroupStable | Tally => {
                let site = match self.method {
                    GroupBy => "array.group_by block returned an",
                    GroupStable => "array.group_by_stable block returned an",
                    _ => "array.tally value is an",
                };
                let existing = self.hash.find(ctx, value.key_name_for(site)?)?;
                let (key, group) = if let Some(index) = existing {
                    let (key, group) = &mut self.hash.buffer.data[index];
                    (key.clone(), std::mem::take(group))
                } else {
                    if self.method == GroupStable {
                        self.other.push(ctx, value.clone())?;
                    }
                    (
                        ctx.bytes(value.key_name_for(site)?)?,
                        if self.method == Tally {
                            Value::int(0)
                        } else {
                            Value::from_array(ctx, Buffer::empty())?
                        },
                    )
                };
                let next = if self.method == Tally {
                    Value::int(
                        group
                            .require_int()?
                            .checked_add(1)
                            .ok_or_else(|| argument("tally overflow"))?,
                    )
                } else {
                    group.push(ctx, &self.pending[..1])?
                };
                self.hash.insert(ctx, key, next)?;
            }
            Uniq => {
                let hash = crate::sets::key_hash(ctx, &value)?;
                if let crate::sets::Entry::Vacant(slot) =
                    self.keys.entry(ctx, &self.other.data, &value, hash)?
                {
                    self.other.push(ctx, value)?;
                    self.keys.fill(slot, hash, self.other.data.len() - 1);
                    self.output.push(ctx, self.pending[0].clone())?;
                }
            }
            Sum => {
                let previous = self.accumulator.take().unwrap();
                let array = matches!(self.receiver.0, Kind::Array(_));
                if matches!(previous.0, Kind::Bytes(_)) != matches!(value.0, Kind::Bytes(_)) {
                    return Err(argument(if array {
                        ops::SUM_INCOMPATIBLE
                    } else {
                        "sum cannot add incompatible values"
                    }));
                }
                let sum = ops::binary(ctx, "+", previous, value);
                self.accumulator = Some(if array {
                    sum.map_err(ops::sum_incompatible)?
                } else {
                    sum?
                });
            }
            Min | Max => {
                let next = value.require_int()?;
                let replace = match &self.accumulator {
                    None => true,
                    Some(best) => {
                        let best = best.require_int()?;
                        if self.method == Min {
                            next < best
                        } else {
                            next > best
                        }
                    }
                };
                if replace {
                    self.accumulator = Some(value);
                }
            }
        }
        Ok(false)
    }

    /// Applies one `chunk` block result. `accumulator` holds the active
    /// group's key (`None` when no group is open) and `other` its members;
    /// finished rows are `[key, group]` pairs in `output`.
    fn accept_chunk_key(&mut self, ctx: &mut CallContext, key: Value) -> Result<()> {
        enum Control {
            Normal,
            Separator,
            Alone,
        }
        let control = match &key.0 {
            Kind::Nil => Control::Separator,
            Kind::Symbol(_) => {
                let name = key.as_bytes().unwrap();
                ctx.work_bytes(name.len())?;
                match name {
                    b"_separator" => Control::Separator,
                    b"_alone" => Control::Alone,
                    _ if name.first() == Some(&b'_') => {
                        let mut message = Buffer::empty();
                        message.extend(ctx, b"array.chunk reserved key :")?;
                        message.extend(ctx, name)?;
                        let mut error = Error::from_bytes(ctx, &message.data)?;
                        error.kind = ErrorKind::Argument;
                        return Err(error);
                    }
                    _ => Control::Normal,
                }
            }
            _ => Control::Normal,
        };
        match control {
            Control::Separator => self.flush_chunk(ctx),
            Control::Alone => {
                self.flush_chunk(ctx)?;
                self.chunk_depth_guard(ctx, &key)?;
                self.accumulator = Some(key);
                self.other.push(ctx, self.pending[0].clone())?;
                self.flush_chunk(ctx)
            }
            Control::Normal => {
                self.chunk_depth_guard(ctx, &key)?;
                let same = match &self.accumulator {
                    Some(current) => ops::equal(ctx, &key, current, 0)?,
                    None => false,
                };
                if !same {
                    self.flush_chunk(ctx)?;
                    self.accumulator = Some(key);
                }
                self.other.push(ctx, self.pending[0].clone())
            }
        }
    }

    // The result nests keys two levels and items three levels deep.
    fn chunk_depth_guard(&self, ctx: &mut CallContext, key: &Value) -> Result<()> {
        let depth = key.depth().max(self.pending[0].depth() + 1) + 2;
        if depth > MAX_VALUE_DEPTH {
            return ctx.guard(ErrorKind::Recursion, "value nesting too deep");
        }
        Ok(())
    }

    fn flush_chunk(&mut self, ctx: &mut CallContext) -> Result<()> {
        let Some(key) = self.accumulator.take() else {
            return Ok(());
        };
        let group = Value::from_array(ctx, std::mem::replace(&mut self.other, Buffer::empty()))?;
        let row = array_copy(ctx, &[key, group])?;
        self.output.push(ctx, row)
    }

    fn flush_adjacent(&mut self, ctx: &mut CallContext, end: usize) -> Result<()> {
        let array = self.receiver.as_array().unwrap();
        let part = array_copy(ctx, &array[self.start as usize..end])?;
        if part.depth() + 1 > MAX_VALUE_DEPTH {
            return ctx.guard(ErrorKind::Recursion, "value nesting too deep");
        }
        self.output.push(ctx, part)?;
        self.start = end as i64;
        Ok(())
    }

    fn finish(&mut self, ctx: &mut CallContext) -> Result<Value> {
        use MethodKind::*;
        if matches!(self.method, SliceWhen | ChunkWhile) && self.length != 0 {
            self.flush_adjacent(ctx, self.length as usize)?;
        }
        if self.method == Chunk {
            self.flush_chunk(ctx)?;
        }
        match self.method {
            Each | EachIndex | EachKey | EachValue | ReverseEach | Times | Upto | Downto | Step => {
                Ok(self.receiver.clone())
            }
            EachSlice | EachCons | Cycle => Ok(Value::nil()),
            Fetch | Reduce | Find | Index | Rindex | Sum | Min | Max | Delete => {
                Ok(self.accumulator.take().unwrap_or_default())
            }
            DeleteIf | KeepIf | Fill => {
                if let Some(value) = self.accumulator.take() {
                    return Ok(value);
                }
                if matches!(self.receiver.0, Kind::Hash(_)) {
                    if !self.other.data.is_empty() {
                        self.mutation = Some(Mutation::DeleteKeys(std::mem::replace(
                            &mut self.other,
                            Buffer::empty(),
                        )));
                    }
                    return Ok(self.receiver.clone());
                }
                if self.method != Fill
                    && self.output.data.len() == self.receiver.as_array().unwrap().len()
                {
                    return Ok(self.receiver.clone());
                }
                let value =
                    Value::from_array(ctx, std::mem::replace(&mut self.output, Buffer::empty()))?;
                self.mutation = Some(Mutation::Replace(value.clone()));
                Ok(value)
            }
            Count => Ok(Value::int(self.count)),
            Any => Ok(Value::boolean(self.count != 0)),
            All | NoneMatch => Ok(Value::boolean(self.count == 0)),
            One => Ok(Value::boolean(self.count == 1)),
            ToHash | TransformKeys | TransformValues | GroupBy | Tally => {
                Value::from_hash(ctx, std::mem::replace(&mut self.hash, Hash::empty()))
            }
            Select | Reject if matches!(self.receiver.0, Kind::Hash(_)) => {
                Value::from_hash(ctx, std::mem::replace(&mut self.hash, Hash::empty()))
            }
            GroupStable => {
                for (index, (_, group)) in self.hash.buffer.data.iter().enumerate() {
                    ctx.charge(1)?;
                    let pair = array_copy(ctx, &[self.other.data[index].clone(), group.clone()])?;
                    self.output.push(ctx, pair)?;
                }
                Value::from_array(ctx, std::mem::replace(&mut self.output, Buffer::empty()))
            }
            Partition => {
                let yes =
                    Value::from_array(ctx, std::mem::replace(&mut self.output, Buffer::empty()))?;
                let no =
                    Value::from_array(ctx, std::mem::replace(&mut self.other, Buffer::empty()))?;
                array_copy(ctx, &[yes, no])
            }
            _ => Value::from_array(ctx, std::mem::replace(&mut self.output, Buffer::empty())),
        }
    }
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

fn array_copy(ctx: &mut CallContext, values: &[Value]) -> Result<Value> {
    let mut output = Buffer::empty();
    output.extend(ctx, values)?;
    Value::from_array(ctx, output)
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
        assert_eq!(ctx.stats().retained_memory_bytes, size_of::<State<Loop>>());
        drop(state);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
        ctx.options.limits.memory_bytes = Some(size_of::<State<Loop>>() - 1);
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
