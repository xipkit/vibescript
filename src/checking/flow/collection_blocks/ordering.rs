use super::*;
use crate::checking::ordering::{EQUAL, GREATER, LESS, ORDERED, UNORDERED};
use crate::sort::{Action, Sort};
use std::cmp::Ordering;

struct Sorting {
    sort: Sort,
    current: IterationState,
    comparison: Option<Ordering>,
}

impl Sorting {
    fn snapshot(&self, ctx: &mut CallContext) -> Result<Self> {
        Ok(Self {
            sort: self.sort.snapshot(ctx)?,
            current: self.current.snapshot(ctx)?,
            comparison: self.comparison,
        })
    }
}

impl Method {
    pub(super) fn ordered(self) -> bool {
        matches!(
            self,
            Self::Sort
                | Self::SortBy
                | Self::Min
                | Self::Max
                | Self::Minmax
                | Self::MinBy
                | Self::MaxBy
        )
    }
}

impl Walker<'_> {
    pub(super) fn ordered_block(
        &mut self,
        state: &State,
        pc: usize,
        receiver: Fact,
        site: MemberSite,
        args: Arguments,
        method: Method,
    ) -> Result<()> {
        use Method::*;
        for i in 0..self.facts.arm_count(receiver) {
            self.ctx.charge(1)?;
            let source = self.facts.arm(receiver, i);
            match self.facts.node(source) {
                Node::Atom(Atom::Never) => continue,
                Node::Atom(Atom::Unknown | Atom::Any)
                | Node::Named(_)
                | Node::Nominal { .. }
                | Node::Shape(_, _, _, HashKind::Any | HashKind::Object)
                | Node::Hash(_, _, HashKind::Any | HashKind::Object) => {
                    self.incomplete(pc)?;
                    continue;
                }
                Node::Tuple(_) | Node::Array(_) => (),
                _ => {
                    self.collection_fallback(state, pc, source, site, &args)?;
                    continue;
                }
            }
            if site.scope
                || !args.positional.data.is_empty()
                || (matches!(method, SortBy | MinBy | MaxBy) && args.block.is_none())
                || (matches!(method, Min | Max | Minmax) && args.block.is_some())
            {
                self.collection_error(state, pc, source, site, &args, ErrorClass::Runtime)?;
                continue;
            }
            let driver = Driver {
                mutation: None,
                method,
                callback: args
                    .block
                    .as_ref()
                    .map_or(Callback::Identity, Callback::Block),
                pattern: None,
                count_overflow: false,
                exact: matches!(self.facts.node(source), Node::Tuple(_)),
                site: Some(site),
            };
            let initial = IterationState {
                state: state.snapshot(self.ctx)?,
                output: if matches!(method, Sort | SortBy) {
                    source
                } else {
                    Atom::Nil.fact()
                },
                auxiliary: if method == SortBy {
                    self.facts.tuple(self.ctx, &[])?
                } else {
                    Atom::Never.fact()
                },
                previous: Atom::Never.fact(),
            };
            let depth = self.collection_depth(&initial, driver, source)?;
            if method == Sort {
                if let Node::Tuple(items) = self.facts.node(source) {
                    let length = items.data.len();
                    self.ordered_sort(initial, pc, driver, length, depth)?;
                } else {
                    self.generic_sort(initial, pc, driver, source, depth)?;
                }
                continue;
            }
            if let Node::Tuple(items) = self.facts.node(source) {
                let length = items.data.len();
                let mut current = Some(initial);
                for index in 0..length {
                    self.ctx.charge(1)?;
                    let Some(before) = current else {
                        break;
                    };
                    let element = self.ordered_element(source, index)?;
                    let index = self.facts.integer(self.ctx, index as i64)?;
                    let item = self.collection_item(Receiver::Array, driver, element, index)?;
                    current = self.collection_step(before, pc, driver, item, depth)?;
                }
                if let Some(current) = current {
                    self.ordered_finish(current, pc, driver, source)?;
                }
            } else {
                let iteration = self.facts.iteration(self.ctx, source)?;
                if iteration.empty != Atom::Never.fact() {
                    let empty = initial.snapshot(self.ctx)?;
                    self.ordered_finish(empty, pc, driver, source)?;
                }
                if iteration.item == Atom::Never.fact() {
                    continue;
                }
                let item = self.collection_item(
                    Receiver::Array,
                    driver,
                    iteration.item,
                    Atom::Int.fact(),
                )?;
                let Some(mut current) = self.collection_step(initial, pc, driver, item, depth)?
                else {
                    continue;
                };
                let depth = self.collection_depth(&current, driver, source)?;
                current.state.widening.get_or_insert(depth);
                loop {
                    self.ctx.charge(1)?;
                    let done = current.snapshot(self.ctx)?;
                    self.ordered_finish(done, pc, driver, source)?;
                    let before = current.snapshot(self.ctx)?;
                    let Some(next) = self.collection_step(before, pc, driver, item, depth)? else {
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

    pub(super) fn ordered_result(
        &mut self,
        mut current: IterationState,
        pc: usize,
        driver: Driver<'_>,
        item: Item,
        value: Fact,
        depth: usize,
    ) -> Result<Option<IterationState>> {
        use Method::*;
        if driver.method == Sort {
            let mut accepted = Atom::Never.fact();
            let expected = self
                .facts
                .union(self.ctx, &[Atom::Int.fact(), Atom::Float.fact()])?;
            for i in 0..self.facts.arm_count(value) {
                self.ctx.charge(1)?;
                let arm = self.facts.arm(value, i);
                if matches!(
                    self.facts.node(arm),
                    Node::Atom(Atom::Unknown | Atom::Any) | Node::Named(_) | Node::Nominal { .. }
                ) {
                    self.emit_error(&current.state, pc, handlers::bit(ErrorClass::Runtime))?;
                    accepted = self.facts.union(self.ctx, &[accepted, expected])?;
                } else if matches!(self.facts.atom(arm), Some(Atom::Int | Atom::Float)) {
                    accepted = self.facts.union(self.ctx, &[accepted, arm])?;
                } else {
                    self.emit_error(&current.state, pc, handlers::bit(ErrorClass::Runtime))?;
                    self.issue(
                        pc,
                        IssueKind::CallbackResult {
                            name: driver.site.unwrap().name,
                            actual: arm,
                            expected,
                        },
                    )?;
                }
            }
            if accepted == Atom::Never.fact() {
                return Ok(None);
            }
            current.previous = accepted;
            return Ok(Some(current));
        }
        if driver.method == SortBy {
            current.auxiliary = self.group_append(current.auxiliary, value)?;
            return Ok(Some(current));
        }
        if current.auxiliary == Atom::Never.fact() {
            current.auxiliary = value;
            current.output = item.element;
            if driver.method == Minmax {
                current.previous = value;
                current.output = self.facts.tuple(self.ctx, &[item.element, item.element])?;
            }
            return Ok(Some(current));
        }
        let order = self.required_order(&current.state, pc, driver, value, current.auxiliary)?;
        if order == 0 {
            return Ok(None);
        }
        let improve = if matches!(driver.method, Max | MaxBy) {
            GREATER
        } else {
            LESS
        };
        let best = if driver.method == Minmax {
            self.ordered_element(current.output, 0)?
        } else {
            current.output
        };
        let best = self.ordered_choice(best, item.element, order, improve, depth)?;
        current.auxiliary = self.ordered_choice(current.auxiliary, value, order, improve, depth)?;
        current.output = if driver.method == Minmax {
            let order = self.required_order(&current.state, pc, driver, value, current.previous)?;
            if order == 0 {
                return Ok(None);
            }
            let maximum = self.ordered_element(current.output, 1)?;
            let maximum = self.ordered_choice(maximum, item.element, order, GREATER, depth)?;
            current.previous =
                self.ordered_choice(current.previous, value, order, GREATER, depth)?;
            self.facts.tuple(self.ctx, &[best, maximum])?
        } else {
            best
        };
        Ok(Some(current))
    }

    fn ordered_choice(
        &mut self,
        old: Fact,
        new: Fact,
        order: u8,
        improve: u8,
        depth: usize,
    ) -> Result<Fact> {
        if order & improve == 0 {
            Ok(old)
        } else if order & (ORDERED ^ improve) == 0 {
            Ok(new)
        } else {
            self.facts.widen(self.ctx, old, new, depth)
        }
    }

    fn required_order(
        &mut self,
        state: &State,
        pc: usize,
        driver: Driver<'_>,
        left: Fact,
        right: Fact,
    ) -> Result<u8> {
        let mut accepted = 0;
        for l in 0..self.facts.arm_count(left) {
            for r in 0..self.facts.arm_count(right) {
                self.ctx.charge(1)?;
                let left = self.facts.arm(left, l);
                let right = self.facts.arm(right, r);
                let result = self.facts.order_result(self.ctx, left, right)?;
                if result & UNORDERED != 0 {
                    self.emit_error(state, pc, handlers::bit(ErrorClass::Runtime))?;
                    if result & ORDERED == 0 {
                        self.issue(
                            pc,
                            IssueKind::Ordering {
                                name: driver.site.unwrap().name,
                                left,
                                right,
                            },
                        )?;
                    }
                }
                accepted |= result & ORDERED;
            }
        }
        Ok(accepted)
    }

    fn ordered_element(&mut self, tuple: Fact, index: usize) -> Result<Fact> {
        self.facts
            .extract(self.ctx, tuple, crate::bytecode::Selection::At(index))
    }

    fn ordered_finish(
        &mut self,
        mut current: IterationState,
        pc: usize,
        driver: Driver<'_>,
        source: Fact,
    ) -> Result<()> {
        if driver.method == Method::SortBy {
            if let Node::Tuple(items) = self.facts.node(source) {
                let length = items.data.len();
                let depth = self.collection_depth(&current, driver, source)?;
                return self.ordered_sort(current, pc, driver, length, depth);
            }
            if !matches!(self.facts.node(current.auxiliary),Node::Tuple(items) if items.data.len()<2)
            {
                let key = self.facts.elements(self.ctx, current.auxiliary)?;
                if self.required_order(&current.state, pc, driver, key, key)? == 0 {
                    return Ok(());
                }
            }
        } else if driver.method == Method::Minmax && current.auxiliary == Atom::Never.fact() {
            current.output = self.facts.tuple(self.ctx, &[Atom::Nil.fact(); 2])?;
        }
        self.collection_done(current, pc, driver.method, source)
    }

    fn ordered_sign(&mut self, value: Fact) -> Result<u8> {
        let mut result = 0;
        for i in 0..self.facts.arm_count(value) {
            self.ctx.charge(1)?;
            result |= match self.facts.node(self.facts.arm(value, i)) {
                Node::Integer(n) => {
                    if *n < 0 {
                        LESS
                    } else if *n == 0 {
                        EQUAL
                    } else {
                        GREATER
                    }
                }
                Node::Float(bits) => {
                    let n = f64::from_bits(*bits);
                    if n < 0.0 {
                        LESS
                    } else if n > 0.0 {
                        GREATER
                    } else {
                        EQUAL
                    }
                }
                _ => ORDERED,
            };
        }
        Ok(result)
    }

    fn ordered_swap(&mut self, value: Fact, a: usize, b: usize) -> Result<Fact> {
        let mut output = Atom::Never.fact();
        for i in 0..self.facts.arm_count(value) {
            self.ctx.charge(1)?;
            let arm = self.facts.arm(value, i);
            let next = if let Node::Tuple(items) = self.facts.node(arm) {
                let mut items_copy = Buffer::empty();
                items_copy.extend(self.ctx, &items.data)?;
                items_copy.data.swap(a, b);
                self.facts.tuple(self.ctx, &items_copy.data)?
            } else {
                arm
            };
            output = self.facts.union(self.ctx, &[output, next])?;
        }
        Ok(output)
    }

    fn ordered_enqueue(
        &mut self,
        seen: &mut Buffer<(u64, Sorting)>,
        pending: &mut Buffer<usize>,
        run: Sorting,
        depth: usize,
    ) -> Result<()> {
        let hash = run.sort.fingerprint(self.ctx)?;
        for (index, (previous, entry)) in seen.data.iter_mut().enumerate() {
            self.ctx.charge(1)?;
            if hash == *previous
                && run.comparison == entry.comparison
                && run.sort.same(self.ctx, &entry.sort)?
            {
                if entry
                    .current
                    .join(self.ctx, self.facts, &run.current, false, depth)?
                {
                    pending.push(self.ctx, index)?;
                }
                return Ok(());
            }
        }
        pending.push(self.ctx, seen.data.len())?;
        seen.push(self.ctx, (hash, run))
    }

    fn ordered_sort(
        &mut self,
        current: IterationState,
        pc: usize,
        driver: Driver<'_>,
        length: usize,
        depth: usize,
    ) -> Result<()> {
        let mut seen: Buffer<(u64, Sorting)> = Buffer::empty();
        let mut pending: Buffer<usize> = Buffer::empty();
        let mut next = Some(Sorting {
            sort: Sort::new(length),
            current,
            comparison: None,
        });
        loop {
            self.ctx.charge(1)?;
            let mut run = if let Some(run) = next.take() {
                run
            } else if let Some(index) = pending.data.pop() {
                seen.data[index].1.snapshot(self.ctx)?
            } else {
                break;
            };
            loop {
                match run.sort.advance(self.ctx, run.comparison.take())? {
                    Action::Compare(a, b) => {
                        let order = if driver.method == Method::Sort && driver.block().is_some() {
                            let left = self.ordered_element(run.current.output, a)?;
                            let right = self.ordered_element(run.current.output, b)?;
                            let item = Item {
                                arguments: [left, right],
                                count: 2,
                                element: left,
                                index: Atom::Int.fact(),
                                pair: None,
                            };
                            let Some(current) =
                                self.collection_step(run.current, pc, driver, item, depth)?
                            else {
                                break;
                            };
                            run.current = current;
                            self.ordered_sign(run.current.previous)?
                        } else {
                            let keys = if driver.method == Method::SortBy {
                                run.current.auxiliary
                            } else {
                                run.current.output
                            };
                            let left = self.ordered_element(keys, a)?;
                            let right = self.ordered_element(keys, b)?;
                            self.required_order(&run.current.state, pc, driver, left, right)?
                        };
                        if order == 0 {
                            break;
                        }
                        if order & LESS != 0 && order & (EQUAL | GREATER) != 0 {
                            let mut less = run.snapshot(self.ctx)?;
                            less.comparison = Some(Ordering::Less);
                            self.ordered_enqueue(&mut seen, &mut pending, less, depth)?;
                            run.comparison = Some(Ordering::Equal);
                            self.ordered_enqueue(&mut seen, &mut pending, run, depth)?;
                            break;
                        }
                        run.comparison = Some(if order & LESS != 0 {
                            Ordering::Less
                        } else {
                            Ordering::Equal
                        });
                    }
                    Action::Swap(a, b) => {
                        run.current.output = self.ordered_swap(run.current.output, a, b)?;
                        if driver.method == Method::SortBy {
                            run.current.auxiliary =
                                self.ordered_swap(run.current.auxiliary, a, b)?;
                        }
                    }
                    Action::Done => {
                        self.collection_done(run.current, pc, driver.method, Atom::Never.fact())?;
                        break;
                    }
                }
            }
        }
        Ok(())
    }

    fn generic_sort(
        &mut self,
        initial: IterationState,
        pc: usize,
        driver: Driver<'_>,
        source: Fact,
        depth: usize,
    ) -> Result<()> {
        let done = initial.snapshot(self.ctx)?;
        self.collection_done(done, pc, driver.method, source)?;
        let element = self.facts.elements(self.ctx, source)?;
        if element == Atom::Never.fact() {
            return Ok(());
        }
        if driver.block().is_none() {
            self.required_order(&initial.state, pc, driver, element, element)?;
            return Ok(());
        }
        let item = Item {
            arguments: [element, element],
            count: 2,
            element,
            index: Atom::Int.fact(),
            pair: None,
        };
        let Some(mut current) = self.collection_step(initial, pc, driver, item, depth)? else {
            return Ok(());
        };
        let depth = self.collection_depth(&current, driver, source)?;
        current.state.widening.get_or_insert(depth);
        loop {
            self.ctx.charge(1)?;
            let done = current.snapshot(self.ctx)?;
            self.collection_done(done, pc, driver.method, source)?;
            let before = current.snapshot(self.ctx)?;
            let Some(next) = self.collection_step(before, pc, driver, item, depth)? else {
                break;
            };
            if !current.join(self.ctx, self.facts, &next, true, depth)? {
                break;
            }
        }
        Ok(())
    }
}
