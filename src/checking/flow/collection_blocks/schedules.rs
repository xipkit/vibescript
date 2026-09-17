use super::*;

#[derive(Clone, Copy)]
enum Repetitions {
    Exact(u128),
    Finite,
    Infinite,
}

#[derive(Clone, Copy)]
enum Pass {
    Item(Fact),
    Tuple {
        source: Fact,
        width: usize,
        stride: usize,
    },
}

#[derive(Clone, Copy)]
enum Domain {
    Integer,
    Positive,
    Nonzero,
    Count,
}

impl Method {
    pub(super) fn scheduled(self) -> bool {
        matches!(
            self,
            Self::EachSlice
                | Self::EachCons
                | Self::Cycle
                | Self::Times
                | Self::Upto
                | Self::Downto
                | Self::Step
                | Self::Tap
                | Self::YieldSelf
        )
    }
}

impl Walker<'_> {
    pub(super) fn scheduled_block(
        &mut self,
        state: &State,
        pc: usize,
        receiver: Fact,
        site: CallSite,
        args: Arguments,
        method: Method,
    ) -> Result<()> {
        use Method::*;
        for i in 0..self.facts.arm_count(receiver) {
            self.ctx.charge(1)?;
            let arm = self.facts.arm(receiver, i);
            let view = if let Node::Protected(shape, _) = self.facts.node(arm) {
                *shape
            } else {
                arm
            };
            let universal = matches!(method, Tap | YieldSelf);
            let range = matches!(
                self.facts.node(view),
                Node::Range(..) | Node::Atom(Atom::Range)
            );
            let supported = match self.facts.node(view) {
                Node::Atom(Atom::Never) => continue,
                Node::Named(_)
                | Node::Nominal { .. }
                | Node::Atom(Atom::Unknown | Atom::Any)
                | Node::Shape(_, _, _, false)
                | Node::Hash(_, _, false)
                | Node::Builtin(_)
                | Node::TypeValue(_)
                | Node::Offset(_) => {
                    self.incomplete(pc)?;
                    continue;
                }
                Node::Tuple(_) | Node::Array(_) => {
                    universal || matches!(method, EachSlice | EachCons | Cycle)
                }
                Node::Range(..) | Node::Atom(Atom::Range) => universal || method == Step,
                Node::Integer(_) | Node::Atom(Atom::Int) => {
                    universal || matches!(method, Times | Upto | Downto | Step)
                }
                Node::Hash(..) => {
                    if universal {
                        self.incomplete(pc)?;
                        continue;
                    }
                    false
                }
                Node::Shape(_, open, _, _) if universal => {
                    if *open {
                        self.incomplete(pc)?;
                        continue;
                    }
                    if self
                        .facts
                        .selected_field(self.ctx, view, self.program.members[site.name].as_bytes())?
                        .is_some()
                    {
                        self.collection_fallback(state, pc, arm, site, &args)?;
                        continue;
                    }
                    true
                }
                _ => universal,
            };
            if !supported {
                self.collection_fallback(state, pc, arm, site, &args)?;
                continue;
            }
            let arity = args.positional.data.len();
            let (minimum, maximum) = match method {
                EachSlice | EachCons | Upto | Downto => (1, 1),
                Step if !range => (1, 2),
                Step => (1, 1),
                Cycle => (0, 1),
                _ => (0, 0),
            };
            let keywords = universal || range || matches!(method, Upto | Downto | Step);
            if site.scope
                || arity < minimum
                || arity > maximum
                || (keywords && !args.keywords.data.is_empty())
            {
                self.collection_error(state, pc, arm, site, &args, ErrorClass::Runtime)?;
                continue;
            }
            let first = args
                .positional
                .data
                .first()
                .copied()
                .unwrap_or(Atom::Nil.fact());
            let parameter = match method {
                EachSlice | EachCons => self.schedule_number(
                    state,
                    pc,
                    (arm, site, &args),
                    first,
                    Domain::Positive,
                    false,
                )?,
                Cycle => self.schedule_number(
                    state,
                    pc,
                    (arm, site, &args),
                    first,
                    Domain::Count,
                    false,
                )?,
                Upto | Downto => self.schedule_number(
                    state,
                    pc,
                    (arm, site, &args),
                    first,
                    Domain::Integer,
                    true,
                )?,
                Step => self.schedule_number(
                    state,
                    pc,
                    (arm, site, &args),
                    first,
                    if range {
                        Domain::Positive
                    } else {
                        Domain::Integer
                    },
                    true,
                )?,
                _ => Atom::Nil.fact(),
            };
            if parameter == Atom::Never.fact() {
                continue;
            }
            let stride = if method == Step && !range && arity == 2 {
                self.schedule_number(
                    state,
                    pc,
                    (arm, site, &args),
                    args.positional.data[1],
                    Domain::Nonzero,
                    true,
                )?
            } else {
                self.facts.integer(self.ctx, 1)?
            };
            if stride == Atom::Never.fact() {
                continue;
            }
            if !universal
                && matches!(self.facts.node(arm), Node::Atom(Atom::Int))
                && (method != Times || args.block.is_some())
            {
                self.emit_error(state, pc, handlers::bit(ErrorClass::Limit))?;
            }
            let Some(block) = args.block.as_ref() else {
                self.collection_error(state, pc, arm, site, &args, ErrorClass::Runtime)?;
                continue;
            };
            let driver = Driver {
                method,
                callback: Callback::Block(block),
                count_overflow: false,
                exact: matches!(self.facts.node(view), Node::Tuple(_)),
                site,
            };
            let initial = IterationState {
                state: state.snapshot(self.ctx)?,
                output: Atom::Nil.fact(),
                auxiliary: Atom::Never.fact(),
                previous: Atom::Never.fact(),
            };
            if universal {
                self.schedule_run(
                    initial,
                    pc,
                    driver,
                    arm,
                    Pass::Item(arm),
                    Repetitions::Exact(1),
                )?;
            } else if matches!(method, EachSlice | EachCons | Cycle) {
                self.schedule_array(initial, pc, driver, arm, parameter)?;
            } else {
                self.schedule_numeric(initial, pc, driver, arm, parameter, stride)?;
            }
        }
        Ok(())
    }

    fn schedule_number(
        &mut self,
        state: &State,
        pc: usize,
        call: (Fact, CallSite, &Arguments),
        value: Fact,
        domain: Domain,
        bounds: bool,
    ) -> Result<Fact> {
        let (receiver, site, args) = call;
        let mut accepted = Atom::Never.fact();
        for i in 0..self.facts.arm_count(value) {
            self.ctx.charge(1)?;
            let arm = self.facts.arm(value, i);
            let good = match self.facts.node(arm) {
                Node::Atom(Atom::Never) => continue,
                Node::Atom(Atom::Nil) if matches!(domain, Domain::Count) => true,
                Node::Integer(n) => match domain {
                    Domain::Positive => *n > 0,
                    Domain::Nonzero => *n != 0,
                    _ => true,
                },
                Node::Atom(Atom::Int | Atom::Unknown | Atom::Any) => {
                    if bounds {
                        self.emit_error(state, pc, handlers::bit(ErrorClass::Limit))?;
                    }
                    if !bounds
                        || !matches!(domain, Domain::Integer)
                        || self.facts.atom(arm) != Some(Atom::Int)
                    {
                        self.emit_error(state, pc, handlers::bit(ErrorClass::Runtime))?;
                    }
                    accepted = self.facts.union(self.ctx, &[accepted, Atom::Int.fact()])?;
                    continue;
                }
                Node::Named(_) | Node::Nominal { .. } => {
                    self.incomplete(pc)?;
                    continue;
                }
                _ => false,
            };
            if good {
                accepted = self.facts.union(self.ctx, &[accepted, arm])?;
            } else {
                self.collection_error(state, pc, receiver, site, args, ErrorClass::Runtime)?;
            }
        }
        Ok(accepted)
    }

    fn schedule_literal(&self, value: Fact) -> Option<i64> {
        if let Node::Integer(n) = self.facts.node(value) {
            Some(*n)
        } else {
            None
        }
    }

    fn schedule_array(
        &mut self,
        initial: IterationState,
        pc: usize,
        driver: Driver<'_>,
        source: Fact,
        parameter: Fact,
    ) -> Result<()> {
        use Method::*;
        let method = driver.method;
        let literal = self.schedule_literal(parameter);
        let cycle = method == Cycle;
        let count = if cycle {
            if let Some(n) = literal {
                Repetitions::Exact(n.max(0) as u128)
            } else if parameter == Atom::Nil.fact() {
                Repetitions::Infinite
            } else {
                Repetitions::Finite
            }
        } else {
            Repetitions::Exact(1)
        };
        if matches!(count, Repetitions::Exact(0)) {
            return self.collection_done(initial, pc, method, source);
        }
        let width = if cycle {
            Some(1)
        } else {
            literal.map(|n| n as usize)
        };
        if let Node::Tuple(items) = self.facts.node(source) {
            let length = items.data.len();
            if length == 0 || (method == EachCons && width.is_some_and(|w| w > length)) {
                return self.collection_done(initial, pc, method, source);
            }
            if let Some(width) = width {
                if cycle && matches!(count, Repetitions::Finite) {
                    let empty = initial.snapshot(self.ctx)?;
                    self.collection_done(empty, pc, method, source)?;
                }
                let stride = if method == EachSlice { width } else { 1 };
                return self.schedule_run(
                    initial,
                    pc,
                    driver,
                    source,
                    Pass::Tuple {
                        source,
                        width,
                        stride,
                    },
                    count,
                );
            }
        }
        let iteration = self.facts.iteration(self.ctx, source)?;
        let empty = iteration.empty != Atom::Never.fact()
            || (!cycle && method == EachCons)
            || (cycle && matches!(count, Repetitions::Finite));
        if empty {
            let empty = initial.snapshot(self.ctx)?;
            self.collection_done(empty, pc, method, source)?;
        }
        if iteration.item == Atom::Never.fact() {
            return Ok(());
        }
        let item = if cycle {
            iteration.item
        } else if method == EachCons {
            if let Some(width) = width {
                let mut values = Buffer::empty();
                for _ in 0..width {
                    self.ctx.charge(1)?;
                    values.push(self.ctx, iteration.item)?;
                }
                self.facts.tuple(self.ctx, &values.data)?
            } else {
                self.facts.array(self.ctx, iteration.item)?
            }
        } else {
            self.facts.array(self.ctx, iteration.item)?
        };
        let count = if cycle && matches!(count, Repetitions::Infinite) {
            Repetitions::Infinite
        } else {
            Repetitions::Finite
        };
        self.schedule_run(initial, pc, driver, source, Pass::Item(item), count)
    }

    fn schedule_numeric(
        &mut self,
        initial: IterationState,
        pc: usize,
        driver: Driver<'_>,
        source: Fact,
        parameter: Fact,
        stride: Fact,
    ) -> Result<()> {
        use Method::*;
        let method = driver.method;
        let limit = self.schedule_literal(parameter);
        let step = self.schedule_literal(stride);
        let span = match self.facts.node(source) {
            Node::Integer(n) if method == Times => Some((0, 1, i128::from((*n).max(0)))),
            Node::Integer(n) => limit.zip(step).map(|(limit, step)| {
                let start = i128::from(*n);
                let step = if method == Downto {
                    -1
                } else {
                    i128::from(step)
                };
                let distance = (i128::from(limit) - start) * step.signum();
                (
                    start,
                    step,
                    if distance < 0 {
                        0
                    } else {
                        distance / step.abs() + 1
                    },
                )
            }),
            Node::Range(Some(start), Some(end), exclusive) => limit.map(|step| {
                let start = i128::from(*start);
                let end = i128::from(*end);
                let length = (end - start).abs() + i128::from(!exclusive);
                let stride = i128::from(step) * if end < start { -1 } else { 1 };
                (
                    start,
                    stride,
                    (length + i128::from(step) - 1) / i128::from(step),
                )
            }),
            Node::Range(..) => {
                self.emit_error(&initial.state, pc, handlers::bit(ErrorClass::Runtime))?;
                let arguments = self.facts.tuple(self.ctx, &[parameter])?;
                self.issue(
                    pc,
                    IssueKind::Member {
                        name: driver.site.name,
                        receiver: source,
                        arguments,
                    },
                )?;
                return Ok(());
            }
            _ => None,
        };
        if source == Atom::Range.fact() {
            self.emit_error(&initial.state, pc, handlers::bit(ErrorClass::Runtime))?;
        }
        let (item, count) = if let Some((start, _, length)) = span {
            let item = if length == 1 {
                self.facts.integer(self.ctx, start as i64)?
            } else {
                Atom::Int.fact()
            };
            (item, Repetitions::Exact(length as u128))
        } else {
            let empty = initial.snapshot(self.ctx)?;
            self.collection_done(empty, pc, method, source)?;
            (Atom::Int.fact(), Repetitions::Finite)
        };
        self.schedule_run(initial, pc, driver, source, Pass::Item(item), count)
    }

    fn schedule_pass(
        &mut self,
        mut current: IterationState,
        pc: usize,
        driver: Driver<'_>,
        pass: Pass,
        depth: usize,
    ) -> Result<Option<IterationState>> {
        match pass {
            Pass::Item(element) => {
                let item =
                    self.collection_item(Receiver::Array, driver, element, Atom::Int.fact())?;
                self.collection_step(current, pc, driver, item, depth)
            }
            Pass::Tuple {
                source,
                width,
                stride,
            } => {
                let Node::Tuple(items) = self.facts.node(source) else {
                    unreachable!()
                };
                let length = items.data.len();
                let end = if driver.method == Method::EachCons {
                    length - width + 1
                } else {
                    length
                };
                let mut index = 0;
                while index < end {
                    self.ctx.charge(1)?;
                    let Node::Tuple(items) = self.facts.node(source) else {
                        unreachable!()
                    };
                    let element = if driver.method == Method::Cycle {
                        items.data[index]
                    } else {
                        let mut values = Buffer::empty();
                        values.extend(
                            self.ctx,
                            &items.data[index..length.min(index.saturating_add(width))],
                        )?;
                        self.facts.tuple(self.ctx, &values.data)?
                    };
                    let item =
                        self.collection_item(Receiver::Array, driver, element, Atom::Int.fact())?;
                    let Some(next) = self.collection_step(current, pc, driver, item, depth)? else {
                        return Ok(None);
                    };
                    current = next;
                    index = index.saturating_add(stride);
                }
                Ok(Some(current))
            }
        }
    }

    fn schedule_run(
        &mut self,
        initial: IterationState,
        pc: usize,
        driver: Driver<'_>,
        source: Fact,
        pass: Pass,
        mut count: Repetitions,
    ) -> Result<()> {
        if matches!(count, Repetitions::Exact(0)) {
            return self.collection_done(initial, pc, driver.method, source);
        }
        let depth = self.collection_depth(&initial, driver, source)?;
        let Some(mut current) = self.schedule_pass(initial, pc, driver, pass, depth)? else {
            return Ok(());
        };
        if let Repetitions::Exact(remaining) = &mut count {
            *remaining -= 1;
        }
        // Freeze from the first completed pass; large counts converge by widening.
        let depth = self.collection_depth(&current, driver, source)?;
        current.state.widening.get_or_insert(depth);
        loop {
            self.ctx.charge(1)?;
            // Infinite cycles keep callback exits without inventing a normal return.
            if matches!(count, Repetitions::Exact(0) | Repetitions::Finite) {
                let done = current.snapshot(self.ctx)?;
                self.collection_done(done, pc, driver.method, source)?;
                if matches!(count, Repetitions::Exact(0)) {
                    return Ok(());
                }
            }
            let before = current.snapshot(self.ctx)?;
            let Some(next) = self.schedule_pass(before, pc, driver, pass, depth)? else {
                return Ok(());
            };
            if let Repetitions::Exact(remaining) = &mut count {
                *remaining -= 1;
                if *remaining == 0 {
                    return self.collection_done(next, pc, driver.method, source);
                }
            }
            if !current.join(self.ctx, self.facts, &next, true, depth)? {
                if !matches!(count, Repetitions::Infinite) {
                    self.collection_done(current, pc, driver.method, source)?;
                }
                return Ok(());
            }
        }
    }
}
