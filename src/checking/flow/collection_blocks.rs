use super::*;
use crate::checking::facts::{HashKind, Node};
use blocks::{Closure, Completion, Parent};

mod alternatives;
mod grouping;
mod hashes;
mod loops;
mod mutating;
mod ordering;
mod reductions;
mod schedules;
mod selections;
pub(super) mod text;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Method {
    Each,
    EachIndex,
    EachKey,
    EachValue,
    ReverseEach,
    Map,
    MapIndex,
    FlatMap,
    FilterMap,
    Select,
    Reject,
    Find,
    Index,
    Rindex,
    Reduce,
    Count,
    Any,
    All,
    NoneMatch,
    One,
    Sum,
    TakeWhile,
    DropWhile,
    Partition,
    GroupBy,
    GroupStable,
    Tally,
    ToHash,
    TransformKeys,
    TransformValues,
    SliceWhen,
    ChunkWhile,
    EachSlice,
    EachCons,
    Cycle,
    Times,
    Upto,
    Downto,
    Step,
    Tap,
    YieldSelf,
    Grep,
    GrepV,
    Uniq,
    Fetch,
    FetchValues,
    Loop,
    Sort,
    SortBy,
    Min,
    Max,
    Minmax,
    MinBy,
    MaxBy,
    Merge,
    DeepTransformKeys,
    Substitute,
}

impl Method {
    pub fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "each" => Self::Each,
            "each_with_index" => Self::EachIndex,
            "each_key" => Self::EachKey,
            "each_value" => Self::EachValue,
            "reverse_each" => Self::ReverseEach,
            "map" => Self::Map,
            "map_with_index" => Self::MapIndex,
            "flat_map" | "collect_concat" => Self::FlatMap,
            "filter_map" => Self::FilterMap,
            "select" => Self::Select,
            "reject" => Self::Reject,
            "find" => Self::Find,
            "index" | "find_index" => Self::Index,
            "rindex" => Self::Rindex,
            "reduce" => Self::Reduce,
            "count" => Self::Count,
            "any?" => Self::Any,
            "all?" => Self::All,
            "none?" => Self::NoneMatch,
            "one?" => Self::One,
            "sum" => Self::Sum,
            "take_while" => Self::TakeWhile,
            "drop_while" => Self::DropWhile,
            "partition" => Self::Partition,
            "group_by" => Self::GroupBy,
            "group_by_stable" => Self::GroupStable,
            "tally" => Self::Tally,
            "to_h" => Self::ToHash,
            "transform_keys" => Self::TransformKeys,
            "transform_values" => Self::TransformValues,
            "slice_when" => Self::SliceWhen,
            "chunk_while" => Self::ChunkWhile,
            "each_slice" => Self::EachSlice,
            "each_cons" => Self::EachCons,
            "cycle" => Self::Cycle,
            "times" => Self::Times,
            "upto" => Self::Upto,
            "downto" => Self::Downto,
            "step" => Self::Step,
            "tap" => Self::Tap,
            "yield_self" => Self::YieldSelf,
            "grep" => Self::Grep,
            "grep_v" => Self::GrepV,
            "uniq" => Self::Uniq,
            "fetch" => Self::Fetch,
            "fetch_values" => Self::FetchValues,
            "sort" => Self::Sort,
            "sort_by" => Self::SortBy,
            "min" => Self::Min,
            "max" => Self::Max,
            "minmax" => Self::Minmax,
            "min_by" => Self::MinBy,
            "max_by" => Self::MaxBy,
            "merge" => Self::Merge,
            "deep_transform_keys" => Self::DeepTransformKeys,
            _ => return None,
        })
    }

    fn returns_receiver(self) -> bool {
        matches!(
            self,
            Self::Each
                | Self::EachIndex
                | Self::EachKey
                | Self::EachValue
                | Self::ReverseEach
                | Self::Times
                | Self::Upto
                | Self::Downto
                | Self::Step
                | Self::Tap
        )
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Receiver {
    Array,
    Hash,
    Range,
}

#[derive(Clone, Copy)]
enum Callback<'a> {
    Block(&'a Closure),
    Identity,
    Equal(Fact),
    Match(Fact),
    Operation(Fact),
}

// Value-form index/rindex start position. `Unknown` means positions may be
// skipped, so a certain match can no longer terminate the scan.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Offset {
    None,
    Known(usize),
    Unknown,
}

#[derive(Clone, Copy)]
struct Driver<'a> {
    method: Method,
    mutation: Option<mutating::Mutation>,
    callback: Callback<'a>,
    pattern: Option<Fact>,
    count_overflow: bool,
    exact: bool,
    site: Option<MemberSite>,
    offset: Offset,
}

impl<'a> Driver<'a> {
    fn block(self) -> Option<&'a Closure> {
        if let Callback::Block(block) = self.callback {
            Some(block)
        } else {
            None
        }
    }

    fn skips(self, position: usize) -> bool {
        match (self.method, self.offset) {
            (Method::Index, Offset::Known(offset)) => position < offset,
            (Method::Rindex, Offset::Known(offset)) => position > offset,
            _ => false,
        }
    }
}

struct IterationState {
    state: State,
    output: Fact,
    auxiliary: Fact,
    previous: Fact,
}

impl IterationState {
    fn snapshot(&self, ctx: &mut CallContext) -> Result<Self> {
        ctx.charge(1)?;
        Ok(Self {
            state: self.state.snapshot(ctx)?,
            output: self.output,
            auxiliary: self.auxiliary,
            previous: self.previous,
        })
    }

    fn join(
        &mut self,
        ctx: &mut CallContext,
        facts: &mut Facts,
        other: &Self,
        repeat: bool,
        depth: usize,
        program: &Program,
    ) -> Result<bool> {
        let changed = self.state.join(ctx, facts, &other.state, repeat, program)?;
        let output = facts.widen(ctx, self.output, other.output, depth)?;
        let auxiliary = facts.widen(ctx, self.auxiliary, other.auxiliary, depth)?;
        let previous = facts.widen(ctx, self.previous, other.previous, depth)?;
        let changed = changed
            || output != self.output
            || auxiliary != self.auxiliary
            || previous != self.previous;
        self.output = output;
        self.auxiliary = auxiliary;
        self.previous = previous;
        Ok(changed)
    }
}

#[derive(Clone, Copy)]
struct Item {
    arguments: [Fact; 2],
    count: usize,
    element: Fact,
    index: Fact,
    pair: Option<(Fact, Fact)>,
}

impl Walker<'_> {
    pub(super) fn collection_block(
        &mut self,
        state: &State,
        pc: usize,
        receiver: Fact,
        site: MemberSite,
        args: Arguments,
        method: Method,
    ) -> Result<()> {
        if matches!(method, Method::Merge | Method::DeepTransformKeys) {
            return self.hash_block(state, pc, receiver, site, args, method);
        }
        if method.ordered() {
            return self.ordered_block(state, pc, receiver, site, args, method);
        }
        if matches!(method, Method::Fetch | Method::FetchValues) {
            return self.lookup_block(state, pc, receiver, site, args, method);
        }
        if method.scheduled() {
            return self.scheduled_block(state, pc, receiver, site, args, method);
        }
        for i in 0..self.facts.arm_count(receiver) {
            self.ctx.charge(1)?;
            let arm = self.facts.arm(receiver, i);
            let view = if let Node::Protected(shape, _) = self.facts.node(arm) {
                *shape
            } else {
                arm
            };
            let kind = match self.facts.node(view) {
                Node::Atom(Atom::Never) => continue,
                Node::Tuple(_) | Node::Array(_) => Receiver::Array,
                Node::Shape(_, _, _, HashKind::Plain | HashKind::Object)
                | Node::Hash(_, _, HashKind::Plain | HashKind::Object) => Receiver::Hash,
                Node::Range(..) | Node::Atom(Atom::Range) => Receiver::Range,
                Node::Named(_)
                | Node::Nominal { .. }
                | Node::Instance { .. }
                | Node::Shape(..)
                | Node::Hash(..)
                | Node::Atom(Atom::Unknown | Atom::Any) => {
                    self.incomplete(pc)?;
                    continue;
                }
                _ => {
                    self.collection_fallback(state, pc, arm, site, &args)?;
                    continue;
                }
            };
            use self::Method::*;
            let supported = match kind {
                Receiver::Array => !matches!(
                    method,
                    EachKey | EachValue | TransformKeys | TransformValues
                ),
                Receiver::Hash => matches!(
                    method,
                    Each | EachIndex
                        | EachKey
                        | EachValue
                        | Map
                        | MapIndex
                        | Select
                        | Reject
                        | TransformKeys
                        | TransformValues
                ),
                Receiver::Range => {
                    matches!(
                        method,
                        Each | Map | Select | Reject | Find | Reduce | Count | Sum
                    )
                }
            };
            if !supported {
                self.collection_fallback(state, pc, arm, site, &args)?;
                continue;
            }
            let Some((driver, output)) =
                self.collection_setup(state, pc, arm, (kind, method, site), &args)?
            else {
                continue;
            };
            let initial = IterationState {
                state: state.snapshot(self.ctx)?,
                output,
                auxiliary: match method {
                    GroupStable | SliceWhen | ChunkWhile | Uniq => {
                        self.facts.tuple(self.ctx, &[])?
                    }
                    DropWhile => self.facts.boolean(self.ctx, true)?,
                    _ => Atom::Never.fact(),
                },
                previous: Atom::Never.fact(),
            };
            let depth = self.collection_depth(&initial, driver, arm)?;
            if let Node::Tuple(items) = self.facts.node(view) {
                let length = items.data.len();
                let mut current = initial.alternatives(self.ctx)?;
                for index in 0..length {
                    self.ctx.charge(1)?;
                    if current.data.is_empty() {
                        break;
                    }
                    let position = if matches!(method, ReverseEach | Rindex) {
                        length - 1 - index
                    } else {
                        index
                    };
                    if driver.skips(position) {
                        continue;
                    }
                    let Node::Tuple(items) = self.facts.node(view) else {
                        unreachable!()
                    };
                    let element = items.data[position];
                    let index = self.facts.integer(self.ctx, position as i64)?;
                    let item = self.collection_item(kind, driver, element, index)?;
                    current = self.iteration_next(current, pc, driver, item, depth)?;
                }
                for current in current.data {
                    self.collection_done(current, pc, method, arm)?;
                }
            } else {
                if view == Atom::Range.fact() {
                    self.emit_error(state, pc, handlers::bit(ErrorClass::Runtime))?;
                }
                let iteration = self.facts.iteration(self.ctx, arm)?;
                if iteration.rejected {
                    self.collection_error(state, pc, arm, site, &args, ErrorClass::Runtime)?;
                }
                if iteration.unsupported {
                    self.incomplete(pc)?;
                }
                if iteration.empty != Atom::Never.fact() {
                    let empty = initial.snapshot(self.ctx)?;
                    self.collection_done(empty, pc, method, arm)?;
                }
                if iteration.item == Atom::Never.fact() {
                    continue;
                }
                let item = self.collection_item(kind, driver, iteration.item, Atom::Int.fact())?;
                let first = self.collection_step(initial, pc, driver, item, depth)?;
                self.iteration_loop(
                    first,
                    driver,
                    arm,
                    |walker, current, depth| {
                        walker.collection_step(current, pc, driver, item, depth)
                    },
                    |walker, current| {
                        walker.collection_done(current, pc, method, arm)?;
                        Ok(iteration.repeat != Atom::Never.fact())
                    },
                )?;
            }
        }
        Ok(())
    }

    fn collection_depth(
        &mut self,
        current: &IterationState,
        driver: Driver<'_>,
        receiver: Fact,
    ) -> Result<usize> {
        self.ctx.charge(1)?;
        let state = &current.state;
        let block = driver.block();
        let extra = match driver.method {
            Method::GroupStable => 2,
            Method::GroupBy | Method::Partition | Method::SliceWhen | Method::ChunkWhile => 1,
            _ => 0,
        };
        let mut depth = self
            .facts
            .depth(receiver)
            .saturating_add(extra)
            .max(self.facts.depth(current.output))
            .max(self.facts.depth(current.auxiliary))
            .max(self.facts.depth(current.previous));
        for &contract in self.contracts {
            self.ctx.charge(1)?;
            depth = depth.max(self.facts.depth(contract));
        }
        for link in block.into_iter().flat_map(|block| &block.captures.data) {
            self.ctx.charge(1)?;
            let value = match link.parent {
                Parent::Local(slot) => state.locals.get(self.ctx, slot)?.value,
                Parent::Capture(slot) => state.captures.as_ref().unwrap().value(self.ctx, slot)?,
            };
            depth = depth.max(self.facts.depth(value));
        }
        Ok(depth)
    }

    pub(in crate::checking::flow) fn collection_error(
        &mut self,
        state: &State,
        pc: usize,
        receiver: Fact,
        site: MemberSite,
        args: &Arguments,
        class: ErrorClass,
    ) -> Result<()> {
        self.emit_error(state, pc, handlers::bit(class))?;
        let arguments = self.facts.tuple(self.ctx, &args.positional.data)?;
        self.issue(
            pc,
            IssueKind::Member {
                name: site.name,
                receiver,
                arguments,
            },
        )
    }

    fn collection_item(
        &mut self,
        kind: Receiver,
        driver: Driver<'_>,
        element: Fact,
        index: Fact,
    ) -> Result<Item> {
        self.ctx.charge(1)?;
        let mut item = Item {
            arguments: [element, Atom::Nil.fact()],
            count: 1,
            element,
            index,
            pair: None,
        };
        let method = driver.method;
        use self::Method::*;
        if kind == Receiver::Hash {
            let key = self
                .facts
                .extract(self.ctx, element, crate::bytecode::Selection::At(0))?;
            let value = self
                .facts
                .extract(self.ctx, element, crate::bytecode::Selection::At(1))?;
            item.pair = Some((key, value));
            let collapse = match driver.block() {
                Some(block) => self.block_arity(block.function)? == 1,
                None => false,
            };
            match method {
                EachKey | TransformKeys => item.arguments[0] = key,
                EachValue | TransformValues => item.arguments[0] = value,
                EachIndex | MapIndex => {
                    item.arguments[1] = index;
                    item.count = 2;
                }
                Each | Map if collapse => (),
                _ => {
                    item.arguments = [key, value];
                    item.count = 2;
                }
            }
        } else if matches!(method, EachIndex | MapIndex) {
            item.arguments[1] = index;
            item.count = 2;
        }
        Ok(item)
    }

    fn collection_step(
        &mut self,
        mut before: IterationState,
        pc: usize,
        driver: Driver<'_>,
        mut item: Item,
        depth: usize,
    ) -> Result<Buffer<IterationState>> {
        let method = driver.method;
        if matches!(method, Method::SliceWhen | Method::ChunkWhile)
            && before.previous == Atom::Never.fact()
        {
            before.previous = item.element;
            before.auxiliary = self.facts.tuple(self.ctx, &[item.element])?;
            return before.alternatives(self.ctx);
        }
        let mut after = Buffer::empty();
        if matches!(method, Method::Grep | Method::GrepV) {
            let pattern = driver.pattern.unwrap();
            let keep = self.facts.case_filter(
                self.ctx,
                item.element,
                pattern,
                false,
                method == Method::Grep,
            )?;
            let discard = self.facts.case_filter(
                self.ctx,
                item.element,
                pattern,
                false,
                method != Method::Grep,
            )?;
            if discard != Atom::Never.fact() {
                let discarded = before.snapshot(self.ctx)?;
                after.push(self.ctx, discarded)?;
            }
            if keep == Atom::Never.fact() {
                return Ok(after);
            }
            item.element = keep;
            item.arguments[0] = keep;
        }
        if method == Method::DropWhile {
            let skipping = self
                .facts
                .filter(self.ctx, before.auxiliary, Test::Truth, false)?;
            let invoking = self
                .facts
                .filter(self.ctx, before.auxiliary, Test::Truth, true)?;
            if skipping != Atom::Never.fact() {
                let mut next = before.snapshot(self.ctx)?;
                next.auxiliary = self.facts.boolean(self.ctx, false)?;
                next.output = self.group_append(next.output, item.element)?;
                after.push(self.ctx, next)?;
            }
            if invoking == Atom::Never.fact() {
                return Ok(after);
            }
            before.auxiliary = self.facts.boolean(self.ctx, true)?;
        }
        if method == Method::Reduce && before.output == Atom::Never.fact() {
            before.output = item.element;
            return before.alternatives(self.ctx);
        }
        let Some(block) = driver.block() else {
            for (before, value) in self.collection_input(before, pc, driver, item)?.data {
                if let Some(next) =
                    self.collection_result(before, pc, driver, item, value, depth)?
                {
                    self.iteration_join(&mut after, next, depth)?;
                }
            }
            return Ok(after);
        };
        let mut callback = block.snapshot(self.ctx)?;
        self.prepare_callback(&before.state, &mut callback)?;
        let mut args = Arguments::new();
        if method == Method::Reduce {
            args.positional
                .extend(self.ctx, &[before.output, item.element])?;
        } else if method == Method::Merge {
            args.positional.extend(
                self.ctx,
                &[item.arguments[0], item.arguments[1], item.element],
            )?;
        } else if matches!(method, Method::SliceWhen | Method::ChunkWhile) {
            args.positional
                .extend(self.ctx, &[before.previous, item.element])?;
        } else {
            args.positional
                .extend(self.ctx, &item.arguments[..item.count])?;
        }
        args.block = Some(callback.snapshot(self.ctx)?);
        let current_error = before.state.current_error(self.ctx, self.current_error)?;
        let globals = before.state.global_call(self.ctx)?;
        let result = self.calls.invoke(
            self.ctx,
            self.facts,
            Target::Block(block.function),
            args,
            current_error,
            &globals,
        )?;
        if result.incomplete {
            self.incomplete(pc)?;
            return Ok(after);
        }
        for failure in result.failures.data {
            self.issue(
                pc,
                IssueKind::Call {
                    target: Target::Block(block.function),
                    failure,
                },
            )?;
            self.emit_error(&before.state, pc, handlers::bit(ErrorClass::Runtime))?;
        }
        for exit in result.exits.data {
            self.ctx.charge(1)?;
            let mut state = self.capture_exit(&before.state, &callback, &exit)?;
            state.apply_globals(self.ctx, self.facts, &exit.globals)?;
            match exit.completion {
                Completion::Error(class) => self.emit_error(&state, pc, handlers::bit(class))?,
                Completion::Return(depth) => self.callback_return(state, pc, depth, exit.value)?,
                Completion::Escape => self.callback_escape(state, pc, exit.value)?,
                Completion::Break(_) => {
                    let mut state = state;
                    if driver.mutation.is_some() {
                        state.addresses.data.pop().unwrap();
                    }
                    state.stack.push(self.ctx, Operand::new(exit.value))?;
                    self.native_continue(pc, state)?;
                }
                Completion::Value => {
                    let next = IterationState {
                        state,
                        output: before.output,
                        auxiliary: before.auxiliary,
                        previous: before.previous,
                    };
                    if let Some(next) =
                        self.collection_result(next, pc, driver, item, exit.value, depth)?
                    {
                        self.iteration_join(&mut after, next, depth)?;
                    }
                }
            }
        }
        Ok(after)
    }

    fn collection_guard(&mut self, method: Method, value: Fact) -> Result<bool> {
        use self::Method::*;
        self.ctx.charge(1)?;
        if !matches!(
            method,
            Map | MapIndex | FlatMap | FilterMap | Grep | GrepV | FetchValues
        ) {
            return Ok(false);
        }
        for i in 0..self.facts.arm_count(value) {
            self.ctx.charge(1)?;
            let arm = self.facts.arm(value, i);
            if method == FlatMap && matches!(self.facts.node(arm), Node::Array(_) | Node::Tuple(_))
            {
                continue;
            }
            if method == FilterMap
                && self.facts.filter(self.ctx, arm, Test::Truth, true)? == Atom::Never.fact()
            {
                continue;
            }
            if self.wrapping_guard(arm)? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn collection_accept(
        &mut self,
        output: Fact,
        method: Method,
        item: Item,
        value: Fact,
        depth: usize,
    ) -> Result<Fact> {
        use self::Method::*;
        match method {
            Each | EachIndex | EachKey | EachValue | ReverseEach | EachSlice | EachCons | Cycle
            | Times | Upto | Downto | Step | Tap | Loop => Ok(output),
            YieldSelf | Fetch | DeepTransformKeys => Ok(value),
            Map | MapIndex | Grep | GrepV | FetchValues => Ok(self
                .facts
                .collection_mutate(self.ctx, output, crate::bytecode::Method::Push, &[value])?
                .receiver),
            FlatMap => {
                let mut result = Atom::Never.fact();
                for i in 0..self.facts.arm_count(value) {
                    self.ctx.charge(1)?;
                    let arm = self.facts.arm(value, i);
                    let next = match self.facts.node(arm) {
                        Node::Tuple(items) => {
                            let mut args = Buffer::empty();
                            args.extend(self.ctx, &items.data)?;
                            self.facts
                                .collection_mutate(
                                    self.ctx,
                                    output,
                                    crate::bytecode::Method::Push,
                                    &args.data,
                                )?
                                .receiver
                        }
                        Node::Array(element) => {
                            let mut element = *element;
                            for j in 0..self.facts.arm_count(output) {
                                let previous = self.facts.arm(output, j);
                                let previous = self.facts.elements(self.ctx, previous)?;
                                element = self.facts.union(self.ctx, &[element, previous])?;
                            }
                            self.facts.array(self.ctx, element)?
                        }
                        Node::Atom(Atom::Unknown | Atom::Any)
                        | Node::Named(_)
                        | Node::Nominal { .. }
                        | Node::Instance { .. } => {
                            self.facts.array(self.ctx, Atom::Unknown.fact())?
                        }
                        _ => {
                            self.facts
                                .collection_mutate(
                                    self.ctx,
                                    output,
                                    crate::bytecode::Method::Push,
                                    &[arm],
                                )?
                                .receiver
                        }
                    };
                    result = self.facts.widen(self.ctx, result, next, depth)?;
                }
                Ok(result)
            }
            Find | Index | Rindex | Reduce | Count | Any | All | NoneMatch | One | Sum
            | TakeWhile | DropWhile | Partition | GroupBy | GroupStable | Tally | ToHash
            | TransformKeys | TransformValues | SliceWhen | ChunkWhile | Uniq | Sort | SortBy
            | Min | Max | Minmax | MinBy | MaxBy | Merge | Substitute => unreachable!(),
            FilterMap | Select | Reject => {
                let keep = self
                    .facts
                    .filter(self.ctx, value, Test::Truth, method != Reject)?;
                let discard = self
                    .facts
                    .filter(self.ctx, value, Test::Truth, method == Reject)?;
                if keep == Atom::Never.fact() {
                    return Ok(output);
                }
                let kept = if let Some((key, value)) = item.pair {
                    self.facts
                        .collection_write(self.ctx, output, key, value)?
                        .receiver
                } else {
                    let value = if method == FilterMap {
                        keep
                    } else {
                        item.element
                    };
                    self.facts
                        .collection_mutate(
                            self.ctx,
                            output,
                            crate::bytecode::Method::Push,
                            &[value],
                        )?
                        .receiver
                };
                if discard == Atom::Never.fact() {
                    Ok(kept)
                } else {
                    self.facts.widen(self.ctx, output, kept, depth)
                }
            }
        }
    }

    fn collection_done(
        &mut self,
        mut current: IterationState,
        pc: usize,
        method: Method,
        receiver: Fact,
    ) -> Result<()> {
        let value = if method.returns_receiver() {
            receiver
        } else if method == Method::GroupStable {
            self.group_stable_output(current.output, current.auxiliary)?
        } else if matches!(method, Method::SliceWhen | Method::ChunkWhile)
            && current.previous != Atom::Never.fact()
        {
            self.group_flush(&current.state, pc, current.output, current.auxiliary)?
        } else {
            self.collection_identity(method, current.output)?
        };
        current.state.stack.push(self.ctx, Operand::new(value))?;
        self.native_continue(pc, current.state)
    }
}
