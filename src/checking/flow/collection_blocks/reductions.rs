use super::*;

impl Walker<'_> {
    pub(super) fn collection_fallback(
        &mut self,
        state: &State,
        pc: usize,
        receiver: Fact,
        site: MemberSite,
        args: &Arguments,
    ) -> Result<()> {
        let selected = site.text(self.program, self.facts);
        let name = selected.as_str();
        let fields = matches!(self.facts.node(receiver), Node::Shape(..) | Node::Hash(..))
            && !crate::members::hash_builtin(name);
        let text = self.facts.atom(receiver) == Some(Atom::String)
            && matches!(
                name,
                "index" | "rindex" | "count" | "find_index" | "partition"
            );
        let temporal = self.facts.atom(receiver) == Some(Atom::Time) && name == "min";
        if !fields && !text && !temporal {
            return self.collection_error(state, pc, receiver, site, args, ErrorClass::Runtime);
        }
        let mut next = state.snapshot(self.ctx)?;
        if let Some(edges) = self.member_without_collection(&mut next, pc, receiver, site, args)? {
            for edge in edges.into_iter().flatten() {
                self.extra.push(self.ctx, edge)?;
            }
        } else {
            self.native_continue(pc, next)?;
        }
        Ok(())
    }

    pub(super) fn collection_setup<'a>(
        &mut self,
        state: &State,
        pc: usize,
        receiver: Fact,
        call: (Receiver, Method, MemberSite),
        args: &'a Arguments,
    ) -> Result<Option<(Driver<'a>, Fact)>> {
        use Method::*;
        let (kind, method, site) = call;
        let count = args.positional.data.len();
        let block = args.block.as_ref();
        let maximum = match method {
            Each | Map | Select if kind == Receiver::Array => usize::MAX,
            Find | Count | Any | All | NoneMatch | Sum | Grep | GrepV => 1,
            Reduce if kind == Receiver::Array => 2,
            Reduce => 1,
            Index | Rindex if block.is_none() => 1,
            _ => 0,
        };
        let rejects_keywords = kind == Receiver::Range
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
                    | Sum
                    | ToHash
                    | SliceWhen
                    | ChunkWhile
                    | Uniq
            )
            || (kind == Receiver::Hash && method == Map);
        if site.scope
            || (matches!(method, Grep | GrepV) && count != 1)
            || count > maximum
            || (kind == Receiver::Range && matches!(method, Find | Count) && count != 0)
            || (rejects_keywords && !args.keywords.data.is_empty())
            || (matches!(method, Index | Rindex) && block.is_none() && count != 1)
        {
            self.collection_error(state, pc, receiver, site, args, ErrorClass::Runtime)?;
            return Ok(None);
        }
        if method == Find && count == 1 {
            let value = args.positional.data[0];
            if !self.collection_parameter(
                state,
                pc,
                receiver,
                site,
                args,
                (value, Atom::Nil.fact()),
            )? {
                return Ok(None);
            }
        }
        let mut callback = block.map_or(Callback::Identity, Callback::Block);
        let mut output = match method {
            Reduce => args
                .positional
                .data
                .first()
                .copied()
                .unwrap_or(Atom::Never.fact()),
            Sum => args
                .positional
                .data
                .first()
                .copied()
                .unwrap_or(self.facts.integer(self.ctx, 0)?),
            Count | One => self.facts.integer(self.ctx, 0)?,
            GroupBy | GroupStable | Tally | ToHash | TransformKeys | TransformValues => {
                self.facts.shape(self.ctx, &[], false)?
            }
            Partition => {
                let empty = self.facts.tuple(self.ctx, &[])?;
                self.facts.tuple(self.ctx, &[empty, empty])?
            }
            Find | Index | Rindex => Atom::Nil.fact(),
            Any => self.facts.boolean(self.ctx, false)?,
            All | NoneMatch => self.facts.boolean(self.ctx, true)?,
            Select | Reject if kind == Receiver::Hash => self.facts.shape(self.ctx, &[], false)?,
            _ => self.facts.tuple(self.ctx, &[])?,
        };
        if matches!(method, Count | Any | All | NoneMatch) && count == 1 {
            callback = if method == Count {
                Callback::Equal(args.positional.data[0])
            } else {
                Callback::Match(args.positional.data[0])
            };
        }
        if matches!(method, Index | Rindex) && block.is_none() {
            callback = Callback::Equal(args.positional.data[0]);
        }
        if method == Reduce
            && kind == Receiver::Array
            && (count == 2 || (count == 1 && block.is_none()))
        {
            let operation = *args.positional.data.last().unwrap();
            let expected = self
                .facts
                .union(self.ctx, &[Atom::String.fact(), Atom::Symbol.fact()])?;
            if !self.collection_parameter(state, pc, receiver, site, args, (operation, expected))? {
                return Ok(None);
            }
            let mut accepted = Buffer::empty();
            for i in 0..self.facts.arm_count(operation) {
                self.ctx.charge(1)?;
                let arm = self.facts.arm(operation, i);
                if self.facts.overlaps(self.ctx, arm, expected)? {
                    accepted.push(self.ctx, arm)?;
                }
            }
            callback = Callback::Operation(self.facts.union(self.ctx, &accepted.data)?);
            output = if count == 2 {
                args.positional.data[0]
            } else {
                Atom::Never.fact()
            };
        }
        let optional = matches!(
            method,
            Count | Any | All | NoneMatch | One | Sum | Tally | ToHash | Grep | GrepV | Uniq
        ) || matches!(callback, Callback::Operation(_) | Callback::Equal(_));
        if block.is_none() && !optional {
            self.collection_error(state, pc, receiver, site, args, ErrorClass::Runtime)?;
            return Ok(None);
        }
        if method == Count && block.is_none() && count == 0 {
            let length = match self.facts.node(receiver) {
                Node::Tuple(items) => Some(items.data.len() as i128),
                Node::Range(Some(start), Some(end), exclusive) => {
                    Some((i128::from(*start) - i128::from(*end)).abs() + i128::from(!exclusive))
                }
                Node::Range(..) => {
                    self.collection_error(state, pc, receiver, site, args, ErrorClass::Runtime)?;
                    return Ok(None);
                }
                _ => None,
            };
            let value = if let Some(length) = length {
                let Ok(length) = i64::try_from(length) else {
                    self.collection_error(state, pc, receiver, site, args, ErrorClass::Runtime)?;
                    return Ok(None);
                };
                self.facts.integer(self.ctx, length)?
            } else {
                if receiver == Atom::Range.fact() {
                    self.emit_error(state, pc, handlers::bit(ErrorClass::Runtime))?;
                }
                Atom::Int.fact()
            };
            let state = state.snapshot(self.ctx)?;
            self.collection_terminal(state, pc, value)?;
            return Ok(None);
        }
        let count_overflow = kind == Receiver::Range
            && match self.facts.node(receiver) {
                Node::Range(Some(start), Some(end), exclusive) => {
                    (i128::from(*start) - i128::from(*end)).abs() + i128::from(!exclusive)
                        > i128::from(i64::MAX)
                }
                _ => true,
            };
        Ok(Some((
            Driver {
                mutation: None,
                method,
                callback,
                pattern: matches!(method, Grep | GrepV).then(|| args.positional.data[0]),
                count_overflow,
                exact: matches!(self.facts.node(receiver), Node::Tuple(_)),
                site: Some(site),
            },
            output,
        )))
    }

    pub(in crate::checking::flow) fn collection_parameter(
        &mut self,
        state: &State,
        pc: usize,
        receiver: Fact,
        site: MemberSite,
        args: &Arguments,
        pair: (Fact, Fact),
    ) -> Result<bool> {
        let relation = self.facts.relation(self.ctx, pair.0, pair.1)?;
        if relation == Relation::Rejected {
            self.collection_error(state, pc, receiver, site, args, ErrorClass::Runtime)?;
        } else if relation != Relation::Accepted {
            self.emit_error(state, pc, handlers::bit(ErrorClass::Runtime))?;
        }
        self.facts.overlaps(self.ctx, pair.0, pair.1)
    }

    pub(super) fn collection_input(
        &mut self,
        current: IterationState,
        pc: usize,
        driver: Driver<'_>,
        item: Item,
    ) -> Result<Buffer<(IterationState, Fact)>> {
        let mut results = Buffer::empty();
        let value = match driver.callback {
            Callback::Block(_) => unreachable!(),
            Callback::Identity => item.element,
            Callback::Equal(pattern) => self.collection_equal(item.element, pattern)?,
            Callback::Match(pattern) => {
                let result =
                    self.facts
                        .case_result(self.ctx, Some(item.element), pattern, false)?;
                if result.unsupported {
                    self.incomplete(pc)?;
                }
                result.value
            }
            Callback::Operation(operation) => {
                let before = &current.state;
                for i in 0..self.facts.arm_count(operation) {
                    self.ctx.charge(1)?;
                    let arm = self.facts.arm(operation, i);
                    let name = match self.facts.node(arm) {
                        Node::String(name) | Node::Symbol(name) => name.as_bytes().unwrap(),
                        _ => {
                            self.incomplete(pc)?;
                            continue;
                        }
                    };
                    let op = match name {
                        b"+" => Some("+"),
                        b"-" => Some("-"),
                        b"*" => Some("*"),
                        b"/" => Some("/"),
                        b"%" => Some("%"),
                        b"**" => Some("**"),
                        b"<<" => Some("<<"),
                        b"&" => Some("&"),
                        _ => None,
                    };
                    let next = if let Some(op) = op {
                        let value = self.collection_binary(
                            before,
                            pc,
                            (op, false),
                            current.output,
                            item.element,
                        )?;
                        if value == Atom::Never.fact() {
                            Buffer::empty()
                        } else {
                            let mut result = Buffer::empty();
                            let state = before.snapshot(self.ctx)?;
                            result.push(self.ctx, (state, value))?;
                            result
                        }
                    } else {
                        self.native_reduction(
                            before,
                            pc,
                            driver.site.unwrap(),
                            [current.output, arm, item.element],
                        )?
                    };
                    for (state, value) in next.data {
                        let next = IterationState {
                            state,
                            output: current.output,
                            auxiliary: current.auxiliary,
                            previous: current.previous,
                        };
                        results.push(self.ctx, (next, value))?;
                    }
                }
                return Ok(results);
            }
        };
        results.push(self.ctx, (current, value))?;
        Ok(results)
    }

    fn collection_equal(&mut self, left: Fact, right: Fact) -> Result<Fact> {
        let mut result = Atom::Never.fact();
        for a in 0..self.facts.arm_count(left) {
            for b in 0..self.facts.arm_count(right) {
                self.ctx.charge(1)?;
                let left = self.facts.arm(left, a);
                let right = self.facts.arm(right, b);
                let value = if matches!(
                    self.facts.node(right),
                    Node::Range(..) | Node::Regex(_) | Node::Atom(Atom::Range | Atom::Regex)
                ) {
                    match self.facts.definitely_equal(left, right) {
                        Some(value) => self.facts.boolean(self.ctx, value)?,
                        None => Atom::Bool.fact(),
                    }
                } else {
                    self.facts
                        .case_result(self.ctx, Some(left), right, false)?
                        .value
                };
                result = self.facts.union(self.ctx, &[result, value])?;
            }
        }
        Ok(result)
    }

    pub(super) fn collection_result(
        &mut self,
        mut current: IterationState,
        pc: usize,
        driver: Driver<'_>,
        item: Item,
        value: Fact,
        depth: usize,
    ) -> Result<Option<IterationState>> {
        use Method::*;
        let method = driver.method;
        if value == Atom::Never.fact() {
            return Ok(None);
        }
        if let Some(mutating::Mutation::Filter { hash, keep }) = driver.mutation {
            return self.mutable_filter_result(current, item, value, hash, keep, depth);
        }
        match method {
            Substitute => return self.substitution_result(current, pc, value),
            Merge => {
                if self.wrapping_guard(value)? {
                    self.emit_error(&current.state, pc, handlers::bit(ErrorClass::Limit))?;
                }
                current.output = self
                    .facts
                    .collection_write(self.ctx, current.output, item.arguments[0], value)?
                    .receiver;
            }
            Sort | SortBy | Min | Max | Minmax | MinBy | MaxBy => {
                return self.ordered_result(current, pc, driver, item, value, depth);
            }
            Uniq => return self.unique_result(current, item, value, depth),
            DropWhile | Partition | GroupBy | GroupStable | Tally | ToHash | TransformKeys
            | TransformValues | SliceWhen | ChunkWhile => {
                return self.group_result(current, pc, driver, item, value, depth);
            }
            Reduce => current.output = value,
            Sum => {
                current.output =
                    self.collection_binary(&current.state, pc, ("+", true), current.output, value)?;
                if current.output == Atom::Never.fact() {
                    return Ok(None);
                }
            }
            Find | Index | Rindex | Any | All | NoneMatch | TakeWhile => {
                let stop_on_truth = matches!(method, Find | Index | Rindex | Any | NoneMatch);
                let stop = self
                    .facts
                    .filter(self.ctx, value, Test::Truth, stop_on_truth)?;
                let keep = self
                    .facts
                    .filter(self.ctx, value, Test::Truth, !stop_on_truth)?;
                if stop != Atom::Never.fact() {
                    let result = match method {
                        Find => item.element,
                        Index | Rindex => item.index,
                        Any | All | NoneMatch => self.facts.boolean(self.ctx, method == Any)?,
                        TakeWhile => current.output,
                        _ => unreachable!(),
                    };
                    let state = current.state.snapshot(self.ctx)?;
                    self.collection_terminal(state, pc, result)?;
                }
                if keep == Atom::Never.fact() {
                    return Ok(None);
                }
                if method == TakeWhile {
                    current.output = self
                        .facts
                        .collection_mutate(
                            self.ctx,
                            current.output,
                            crate::bytecode::Method::Push,
                            &[item.element],
                        )?
                        .receiver;
                }
            }
            Count | One => {
                let yes = self.facts.filter(self.ctx, value, Test::Truth, true)?;
                let no = self.facts.filter(self.ctx, value, Test::Truth, false)?;
                let mut next = if no == Atom::Never.fact() {
                    Atom::Never.fact()
                } else {
                    current.output
                };
                if yes != Atom::Never.fact() {
                    for i in 0..self.facts.arm_count(current.output) {
                        self.ctx.charge(1)?;
                        let arm = self.facts.arm(current.output, i);
                        let count = if let Node::Integer(n) = self.facts.node(arm) {
                            Some(*n)
                        } else {
                            None
                        };
                        if method == One {
                            if count != Some(0) {
                                let state = current.state.snapshot(self.ctx)?;
                                let value = self.facts.boolean(self.ctx, false)?;
                                self.collection_terminal(state, pc, value)?;
                            }
                            if count.is_none() || count == Some(0) {
                                let one = self.facts.integer(self.ctx, 1)?;
                                next = self.facts.union(self.ctx, &[next, one])?;
                            }
                        } else {
                            if driver.count_overflow && (count.is_none() || count == Some(i64::MAX))
                            {
                                self.emit_error(
                                    &current.state,
                                    pc,
                                    handlers::bit(ErrorClass::Runtime),
                                )?;
                            }
                            if count != Some(i64::MAX) {
                                let value = if let Some(count) = count {
                                    self.facts.integer(self.ctx, count + 1)?
                                } else {
                                    Atom::Int.fact()
                                };
                                next = self.facts.union(self.ctx, &[next, value])?;
                            }
                        }
                    }
                }
                if next == Atom::Never.fact() {
                    return Ok(None);
                }
                current.output = next;
            }
            _ => {
                if self.collection_guard(method, value)? {
                    self.emit_error(&current.state, pc, handlers::bit(ErrorClass::Limit))?;
                }
                current.output =
                    self.collection_accept(current.output, method, item, value, depth)?;
            }
        }
        Ok(Some(current))
    }

    pub(super) fn collection_identity(&mut self, method: Method, value: Fact) -> Result<Fact> {
        if method == Method::Reduce && value == Atom::Never.fact() {
            Ok(Atom::Nil.fact())
        } else if method == Method::One {
            let one = self.facts.integer(self.ctx, 1)?;
            self.collection_equal(value, one)
        } else {
            Ok(value)
        }
    }

    pub(super) fn collection_terminal(
        &mut self,
        mut state: State,
        pc: usize,
        value: Fact,
    ) -> Result<()> {
        state.stack.push(self.ctx, Operand::new(value))?;
        self.native_continue(pc, state)
    }

    fn collection_binary(
        &mut self,
        state: &State,
        pc: usize,
        operation: (&'static str, bool),
        left: Fact,
        right: Fact,
    ) -> Result<Fact> {
        let (op, sum) = operation;
        let mut value = Atom::Never.fact();
        for a in 0..self.facts.arm_count(left) {
            for b in 0..self.facts.arm_count(right) {
                self.ctx.charge(1)?;
                let left = self.facts.arm(left, a);
                let right = self.facts.arm(right, b);
                let dynamic = self.dynamic(left)? || self.dynamic(right)?;
                if sum
                    && !dynamic
                    && ((self.facts.atom(left) == Some(Atom::String))
                        != (self.facts.atom(right) == Some(Atom::String)))
                {
                    self.emit_error(state, pc, handlers::bit(ErrorClass::Runtime))?;
                    self.issue(pc, IssueKind::Binary { op, left, right })?;
                    continue;
                }
                let array = |fact| matches!(self.facts.node(fact), Node::Array(_) | Node::Tuple(_));
                let (left_array, right_array) = (array(left), array(right));
                if matches!(op, "<<" | "&") || (left_array && matches!(op, "+" | "-")) {
                    let valid = left_array && (op == "<<" || right_array);
                    if !valid {
                        self.emit_error(state, pc, handlers::bit(ErrorClass::Runtime))?;
                        if dynamic {
                            value = self.facts.union(self.ctx, &[value, Atom::Unknown.fact()])?;
                        } else {
                            self.issue(pc, IssueKind::Binary { op, left, right })?;
                        }
                        continue;
                    }
                    let next = if op == "<<" {
                        if self.collection_guard(Method::Map, right)? {
                            self.emit_error(state, pc, handlers::bit(ErrorClass::Limit))?;
                        }
                        self.facts
                            .collection_mutate(
                                self.ctx,
                                left,
                                crate::bytecode::Method::Push,
                                &[right],
                            )?
                            .receiver
                    } else if op == "+" {
                        if let Node::Tuple(items) = self.facts.node(right) {
                            let mut values = Buffer::empty();
                            values.extend(self.ctx, &items.data)?;
                            self.facts
                                .collection_mutate(
                                    self.ctx,
                                    left,
                                    crate::bytecode::Method::Push,
                                    &values.data,
                                )?
                                .receiver
                        } else {
                            let a = self.facts.elements(self.ctx, left)?;
                            let b = self.facts.elements(self.ctx, right)?;
                            let element = self.facts.union(self.ctx, &[a, b])?;
                            self.facts.array(self.ctx, element)?
                        }
                    } else {
                        let element = self.facts.elements(self.ctx, left)?;
                        self.facts.array(self.ctx, element)?
                    };
                    value = self.facts.union(self.ctx, &[value, next])?;
                    continue;
                }
                let result = self.facts.scalar_binary(self.ctx, op, left, right)?;
                if result.unsupported {
                    self.incomplete(pc)?;
                    continue;
                }
                let (errors, stops) = self.binary_errors(op, left, right, result.rejected)?;
                self.emit_error(state, pc, errors)?;
                if result.rejected {
                    self.issue(pc, IssueKind::Binary { op, left, right })?;
                }
                if !stops && !result.rejected {
                    value = self.facts.union(self.ctx, &[value, result.value])?;
                }
            }
        }
        Ok(value)
    }
}
