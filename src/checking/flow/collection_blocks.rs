use super::*;
use crate::checking::facts::Node;
use blocks::{Closure, Completion, Parent};

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
            _ => return None,
        })
    }

    fn returns_receiver(self) -> bool {
        matches!(
            self,
            Self::Each | Self::EachIndex | Self::EachKey | Self::EachValue | Self::ReverseEach
        )
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Receiver {
    Array,
    Hash,
    Range,
}

struct IterationState {
    state: State,
    output: Fact,
}

impl IterationState {
    fn snapshot(&self, ctx: &mut CallContext) -> Result<Self> {
        ctx.charge(1)?;
        Ok(Self {
            state: self.state.snapshot(ctx)?,
            output: self.output,
        })
    }

    fn join(
        &mut self,
        ctx: &mut CallContext,
        facts: &mut Facts,
        other: &Self,
        repeat: bool,
        depth: usize,
    ) -> Result<bool> {
        let changed = self.state.join(ctx, facts, &other.state, repeat)?;
        let output = facts.widen(ctx, self.output, other.output, depth)?;
        let changed = changed || output != self.output;
        self.output = output;
        Ok(changed)
    }
}

#[derive(Clone, Copy)]
struct Item {
    arguments: [Fact; 2],
    count: usize,
    element: Fact,
    pair: Option<(Fact, Fact)>,
}

impl Walker<'_> {
    pub(super) fn collection_block(
        &mut self,
        state: &State,
        pc: usize,
        receiver: Fact,
        site: CallSite,
        args: Arguments,
        method: Method,
    ) -> Result<()> {
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
                Node::Shape(_, _, _, true) | Node::Hash(_, _, true) => Receiver::Hash,
                Node::Range(..) | Node::Atom(Atom::Range) => Receiver::Range,
                Node::Named(_)
                | Node::Nominal { .. }
                | Node::Shape(..)
                | Node::Hash(..)
                | Node::Atom(Atom::Unknown | Atom::Any) => {
                    self.incomplete(pc)?;
                    continue;
                }
                _ => {
                    self.collection_error(state, pc, arm, site, &args, ErrorClass::Runtime)?;
                    continue;
                }
            };
            use self::Method::*;
            let supported = match kind {
                Receiver::Array => !matches!(method, EachKey | EachValue),
                Receiver::Hash => matches!(
                    method,
                    Each | EachIndex | EachKey | EachValue | Map | MapIndex | Select | Reject
                ),
                Receiver::Range => matches!(method, Each | Map | Select | Reject),
            };
            if site.scope || !supported {
                self.collection_error(state, pc, arm, site, &args, ErrorClass::Runtime)?;
                continue;
            }
            let accepts_arguments =
                kind == Receiver::Array && matches!(method, Each | Map | Select);
            let rejects_keywords = kind == Receiver::Range
                || matches!(method, EachIndex | MapIndex | FlatMap | FilterMap)
                || (kind == Receiver::Hash && method == Map);
            if (!accepts_arguments && !args.positional.data.is_empty())
                || (rejects_keywords && !args.keywords.data.is_empty())
                || args.block.is_none()
            {
                self.collection_error(state, pc, arm, site, &args, ErrorClass::Runtime)?;
                continue;
            }
            let block = args.block.as_ref().unwrap();
            let output = if kind == Receiver::Hash && matches!(method, Select | Reject) {
                self.facts.shape(self.ctx, &[], false)?
            } else {
                self.facts.tuple(self.ctx, &[])?
            };
            let initial = IterationState {
                state: state.snapshot(self.ctx)?,
                output,
            };
            let depth = self.collection_depth(state, block, arm, output)?;
            if let Node::Tuple(items) = self.facts.node(view) {
                let length = items.data.len();
                let mut current = Some(initial);
                for index in 0..length {
                    self.ctx.charge(1)?;
                    let Some(before) = current else { break };
                    let position = if method == ReverseEach {
                        length - 1 - index
                    } else {
                        index
                    };
                    let Node::Tuple(items) = self.facts.node(view) else {
                        unreachable!()
                    };
                    let element = items.data[position];
                    let index = self.facts.integer(self.ctx, position as i64)?;
                    let item = self.collection_item(kind, method, block, element, index)?;
                    current = self.collection_step(before, pc, block, method, item, depth)?;
                }
                if let Some(current) = current {
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
                let item =
                    self.collection_item(kind, method, block, iteration.item, Atom::Int.fact())?;
                let Some(mut current) =
                    self.collection_step(initial, pc, block, method, item, depth)?
                else {
                    continue;
                };
                // Callback jobs arrive over several solver passes. Freeze from this
                // loop's first completed iteration, not the growing global fact arena.
                let depth = self.collection_depth(&current.state, block, arm, current.output)?;
                current.state.widening.get_or_insert(depth);
                loop {
                    self.ctx.charge(1)?;
                    let done = current.snapshot(self.ctx)?;
                    self.collection_done(done, pc, method, arm)?;
                    if iteration.repeat == Atom::Never.fact() {
                        break;
                    }
                    let before = current.snapshot(self.ctx)?;
                    let Some(next) =
                        self.collection_step(before, pc, block, method, item, depth)?
                    else {
                        break;
                    };
                    if !current.join(self.ctx, self.facts, &next, true, depth)? {
                        break;
                    }
                }
            }
        }
        Ok(())
    }

    fn collection_depth(
        &mut self,
        state: &State,
        block: &Closure,
        receiver: Fact,
        output: Fact,
    ) -> Result<usize> {
        self.ctx.charge(1)?;
        let mut depth = self.facts.depth(receiver).max(self.facts.depth(output));
        for &contract in self.contracts {
            self.ctx.charge(1)?;
            depth = depth.max(self.facts.depth(contract));
        }
        for link in &block.captures.data {
            self.ctx.charge(1)?;
            let value = match link.parent {
                Parent::Local(slot) => state.locals.get(self.ctx, slot)?.value,
                Parent::Capture(slot) => state.captures.as_ref().unwrap().value(self.ctx, slot)?,
            };
            depth = depth.max(self.facts.depth(value));
        }
        Ok(depth)
    }

    fn collection_error(
        &mut self,
        state: &State,
        pc: usize,
        receiver: Fact,
        site: CallSite,
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
        method: Method,
        block: &Closure,
        element: Fact,
        index: Fact,
    ) -> Result<Item> {
        self.ctx.charge(1)?;
        let mut item = Item {
            arguments: [element, Atom::Nil.fact()],
            count: 1,
            element,
            pair: None,
        };
        use self::Method::*;
        if kind == Receiver::Hash {
            let key = self
                .facts
                .extract(self.ctx, element, crate::bytecode::Selection::At(0))?;
            let value = self
                .facts
                .extract(self.ctx, element, crate::bytecode::Selection::At(1))?;
            item.pair = Some((key, value));
            let collapse = self.program.functions[block.function].block_arity == 1;
            match method {
                EachKey => item.arguments[0] = key,
                EachValue => item.arguments[0] = value,
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
        before: IterationState,
        pc: usize,
        block: &Closure,
        method: Method,
        item: Item,
        depth: usize,
    ) -> Result<Option<IterationState>> {
        let mut callback = block.snapshot(self.ctx)?;
        for link in &mut callback.captures.data {
            self.ctx.charge(1)?;
            link.value = match link.parent {
                Parent::Local(slot) => before.state.locals.get(self.ctx, slot)?.value,
                Parent::Capture(slot) => before
                    .state
                    .captures
                    .as_ref()
                    .unwrap()
                    .value(self.ctx, slot)?,
            };
        }
        let mut args = Arguments::new();
        args.positional
            .extend(self.ctx, &item.arguments[..item.count])?;
        args.block = Some(callback);
        let current_error = before.state.current_error(self.ctx, self.current_error)?;
        let result = self.calls.invoke(
            self.ctx,
            self.facts,
            Target::Block(block.function),
            args,
            current_error,
        )?;
        if result.incomplete {
            self.incomplete(pc)?;
            return Ok(None);
        }
        assert!(result.failures.data.is_empty());
        let mut after: Option<IterationState> = None;
        for exit in result.exits.data {
            self.ctx.charge(1)?;
            let Some(state) = self.capture_exit(&before.state, pc, block, &exit)? else {
                continue;
            };
            match exit.completion {
                Completion::Error(class) => self.emit_error(&state, pc, handlers::bit(class))?,
                Completion::Return(depth) => self.callback_return(state, pc, depth, exit.value)?,
                Completion::Break(_) => {
                    let mut state = state;
                    state.stack.push(self.ctx, Operand::new(exit.value))?;
                    self.extra.push(self.ctx, (pc + 1, state))?;
                }
                Completion::Value => {
                    if self.collection_guard(method, exit.value)? {
                        self.emit_error(&state, pc, handlers::bit(ErrorClass::Limit))?;
                    }
                    let output =
                        self.collection_accept(before.output, method, item, exit.value, depth)?;
                    let next = IterationState { state, output };
                    if let Some(after) = &mut after {
                        after.join(self.ctx, self.facts, &next, false, depth)?;
                    } else {
                        after = Some(next);
                    }
                }
            }
        }
        Ok(after)
    }

    fn collection_guard(&mut self, method: Method, value: Fact) -> Result<bool> {
        use self::Method::*;
        self.ctx.charge(1)?;
        if !matches!(method, Map | MapIndex | FlatMap | FilterMap) {
            return Ok(false);
        }
        let mut pending = Buffer::empty();
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
            if self.facts.depth(arm) >= crate::budget::MAX_VALUE_DEPTH {
                return Ok(true);
            }
            pending.push(self.ctx, arm)?;
        }
        let mut visited = Slots::new(self.facts.len(), false);
        while let Some(value) = pending.data.pop() {
            self.ctx.charge(1)?;
            if visited.get(self.ctx, value.0)? {
                continue;
            }
            visited.set(self.ctx, value.0, true)?;
            match self.facts.node(value) {
                Node::Atom(Atom::Unknown | Atom::Any)
                | Node::Named(_)
                | Node::Nominal { .. }
                | Node::Shape(_, true, _, _) => return Ok(true),
                Node::Array(element) | Node::Hash(_, element, _) | Node::Protected(element, _) => {
                    pending.push(self.ctx, *element)?
                }
                Node::Tuple(values) | Node::Union(values) => {
                    pending.extend(self.ctx, &values.data)?
                }
                Node::Shape(fields, ..) => {
                    for field in &fields.data {
                        self.ctx.charge(1)?;
                        pending.push(self.ctx, field.value)?;
                    }
                }
                _ => (),
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
            Each | EachIndex | EachKey | EachValue | ReverseEach => Ok(output),
            Map | MapIndex => Ok(self
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
                        | Node::Nominal { .. } => {
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
        } else {
            current.output
        };
        current.state.stack.push(self.ctx, Operand::new(value))?;
        self.extra.push(self.ctx, (pc + 1, current.state))
    }
}
