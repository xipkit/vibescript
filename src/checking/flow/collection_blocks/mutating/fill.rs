use super::*;

#[derive(Clone, Copy)]
enum Bound {
    Default,
    Known(i64),
    Unknown,
}

#[derive(Clone, Copy)]
struct Span {
    start: i128,
    end: i128,
    length: i128,
}

impl Walker<'_> {
    pub(super) fn fill_block(
        &mut self,
        state: State,
        pc: usize,
        source: Fact,
        driver: Driver<'_>,
        args: &Arguments,
    ) -> Result<()> {
        let original = match self.facts.node(source) {
            Node::Tuple(items) => Some(items.data.len() as i128),
            Node::Array(element) if *element == Atom::Never.fact() => Some(0),
            _ => None,
        };
        let start = args
            .positional
            .data
            .first()
            .copied()
            .unwrap_or(Atom::Nil.fact());
        let count = args
            .positional
            .data
            .get(1)
            .copied()
            .unwrap_or(Atom::Nil.fact());
        for a in 0..self.facts.arm_count(start) {
            for b in 0..self.facts.arm_count(count) {
                self.ctx.charge(1)?;
                let start = self.facts.arm(start, a);
                let count = self.facts.arm(count, b);
                let mut span = None;
                let mut iterations = None;
                let mut full = false;
                let mut gap = true;
                if self.facts.atom(start) == Some(Atom::Range) {
                    if args.positional.data.len() > 1 {
                        self.collection_error(
                            &state,
                            pc,
                            source,
                            driver.site.unwrap(),
                            args,
                            ErrorClass::Runtime,
                        )?;
                        continue;
                    }
                    if let Node::Range(first, last, exclusive) = *self.facts.node(start) {
                        if let Some(length) = original {
                            let first = first.map_or(0, i128::from);
                            let first = if first < 0 { first + length } else { first };
                            if first < 0 {
                                self.collection_error(
                                    &state,
                                    pc,
                                    source,
                                    driver.site.unwrap(),
                                    args,
                                    ErrorClass::Runtime,
                                )?;
                                continue;
                            }
                            let end = last.map_or(length - 1, i128::from);
                            let end = if end < 0 { end + length } else { end };
                            let end = (end + i128::from(last.is_none() || !exclusive)).max(first);
                            span = Some(Span {
                                start: first,
                                end,
                                length: length.max(end),
                            });
                        } else {
                            if first.is_some_and(|n| n < 0) {
                                self.emit_error(&state, pc, handlers::bit(ErrorClass::Runtime))?;
                            }
                            if last.is_some_and(|n| {
                                n >= 0
                                    && i128::from(n) + i128::from(!exclusive) > isize::MAX as i128
                            }) {
                                self.collection_error(
                                    &state,
                                    pc,
                                    source,
                                    driver.site.unwrap(),
                                    args,
                                    ErrorClass::Limit,
                                )?;
                                continue;
                            }
                            let first = first.unwrap_or(0);
                            full =
                                first == 0 && (last.is_none() || (last == Some(-1) && !exclusive));
                            gap = first > 0;
                            let relative_end = last.is_none_or(|last| last < 0);
                            let last = last
                                .map(i128::from)
                                .map(|last| last + i128::from(!exclusive));
                            if first < 0 && relative_end {
                                iterations =
                                    Some((last.unwrap_or(0) - i128::from(first)).max(0) as u128);
                            } else if first >= 0 && !relative_end {
                                iterations =
                                    Some((last.unwrap() - i128::from(first)).max(0) as u128);
                            }
                        }
                    } else {
                        self.emit_error(
                            &state,
                            pc,
                            handlers::bit(ErrorClass::Runtime) | handlers::bit(ErrorClass::Limit),
                        )?;
                    }
                } else {
                    let Some(first) =
                        self.fill_bound(&state, pc, (source, driver.site.unwrap(), args), start)?
                    else {
                        continue;
                    };
                    let Some(count) =
                        self.fill_bound(&state, pc, (source, driver.site.unwrap(), args), count)?
                    else {
                        continue;
                    };
                    if matches!(count, Bound::Known(n) if n < 0) {
                        let next = state.snapshot(self.ctx)?;
                        self.mutable_value(next, pc, source)?;
                        continue;
                    }
                    let first = match first {
                        Bound::Default => Some(0),
                        Bound::Known(n) => Some(i128::from(n)),
                        Bound::Unknown => None,
                    };
                    let count = match count {
                        Bound::Default => Some(None),
                        Bound::Known(n) => Some(Some(i128::from(n))),
                        Bound::Unknown => None,
                    };
                    if let (Some(length), Some(first), Some(count)) = (original, first, count) {
                        let first = if first < 0 {
                            (first + length).max(0)
                        } else {
                            first
                        };
                        let count = count.unwrap_or(length - first);
                        if count < 0 {
                            let next = state.snapshot(self.ctx)?;
                            self.mutable_value(next, pc, source)?;
                            continue;
                        }
                        let end = first + count;
                        span = Some(Span {
                            start: first,
                            end,
                            length: length.max(end),
                        });
                    } else {
                        iterations = count.flatten().map(|count| count as u128);
                        if let (Some(first), Some(Some(count))) = (first, count) {
                            if first >= 0 && first + count > isize::MAX as i128 {
                                self.collection_error(
                                    &state,
                                    pc,
                                    source,
                                    driver.site.unwrap(),
                                    args,
                                    ErrorClass::Limit,
                                )?;
                                continue;
                            }
                            if first <= 0 && count == 0 {
                                let next = state.snapshot(self.ctx)?;
                                self.mutable_value(next, pc, source)?;
                                continue;
                            }
                        }
                        full = first == Some(0) && count == Some(None);
                        gap = count != Some(None) && first.is_none_or(|first| first > 0);
                        if first.is_none() || count.is_none() {
                            self.emit_error(&state, pc, handlers::bit(ErrorClass::Limit))?;
                        }
                    }
                }
                if span.is_some_and(|span| span.end > isize::MAX as i128) {
                    self.collection_error(
                        &state,
                        pc,
                        source,
                        driver.site.unwrap(),
                        args,
                        ErrorClass::Limit,
                    )?;
                    continue;
                }
                let next = state.snapshot(self.ctx)?;
                if let (Some(span), Some(original)) = (span, original) {
                    if span.start == span.end && span.length == original {
                        self.mutable_value(next, pc, source)?;
                    // Keep expansion proportional to the source shape.
                    } else if span.length <= (original + 1) * 2 {
                        self.fill_exact(next, pc, source, driver, span)?;
                    } else {
                        let mut retained = Atom::Never.fact();
                        if let Node::Tuple(items) = self.facts.node(source) {
                            let mut selected = Buffer::empty();
                            for (position, &item) in items.data.iter().enumerate() {
                                self.ctx.charge(1)?;
                                if (position as i128) < span.start || (position as i128) >= span.end
                                {
                                    selected.push(self.ctx, item)?;
                                }
                            }
                            retained = self.facts.union(self.ctx, &selected.data)?;
                        }
                        if span.start > original {
                            retained = self.facts.union(self.ctx, &[retained, Atom::Nil.fact()])?;
                        }
                        self.fill_generic(
                            next,
                            pc,
                            source,
                            driver,
                            Some((span.end - span.start) as u128),
                            retained,
                        )?;
                    }
                } else {
                    let retained = if full {
                        Atom::Never.fact()
                    } else {
                        let old = self.facts.elements(self.ctx, source)?;
                        if gap {
                            self.facts.union(self.ctx, &[old, Atom::Nil.fact()])?
                        } else {
                            old
                        }
                    };
                    self.fill_generic(next, pc, source, driver, iterations, retained)?;
                }
            }
        }
        Ok(())
    }

    fn fill_bound(
        &mut self,
        state: &State,
        pc: usize,
        call: (Fact, MemberSite, &Arguments),
        value: Fact,
    ) -> Result<Option<Bound>> {
        let value = match self.facts.node(value) {
            Node::Atom(Atom::Never) => return Ok(None),
            Node::Atom(Atom::Nil) => Bound::Default,
            Node::Integer(n) => Bound::Known(*n),
            Node::Float(bits) => {
                let n = f64::from_bits(*bits);
                if n.is_finite() && n >= i64::MIN as f64 && n < -(i64::MIN as f64) {
                    Bound::Known(n as i64)
                } else {
                    self.collection_error(state, pc, call.0, call.1, call.2, ErrorClass::Runtime)?;
                    return Ok(None);
                }
            }
            Node::Atom(Atom::Int | Atom::Float | Atom::Unknown | Atom::Any) => {
                self.emit_error(state, pc, handlers::bit(ErrorClass::Runtime))?;
                Bound::Unknown
            }
            Node::Named(_) | Node::Nominal { .. } | Node::Instance { .. } => {
                self.incomplete(pc)?;
                return Ok(None);
            }
            _ => {
                self.collection_error(state, pc, call.0, call.1, call.2, ErrorClass::Runtime)?;
                return Ok(None);
            }
        };
        Ok(Some(value))
    }

    fn fill_exact(
        &mut self,
        state: State,
        pc: usize,
        source: Fact,
        driver: Driver<'_>,
        span: Span,
    ) -> Result<()> {
        let mut initial = self.mutable_initial(state)?;
        initial.auxiliary = self.facts.boolean(self.ctx, true)?;
        let depth = self.collection_depth(&initial, driver, source)?;
        let mut current = Some(initial);
        for position in 0..span.length {
            self.ctx.charge(1)?;
            let Some(mut before) = current else {
                break;
            };
            let index = self.facts.integer(self.ctx, position as i64)?;
            if position < span.start || position >= span.end {
                let value = self
                    .facts
                    .collection_index(self.ctx, source, &[index])?
                    .value;
                before.output = self.group_append(before.output, value)?;
                current = Some(before);
            } else {
                let item = Item {
                    arguments: [index, Atom::Nil.fact()],
                    count: 1,
                    element: index,
                    index,
                    pair: None,
                };
                current = self.collection_step(before, pc, driver, item, depth)?;
            }
        }
        if let Some(current) = current {
            self.mutable_done(current, pc, source, Mutation::Fill)?;
        }
        Ok(())
    }

    fn fill_generic(
        &mut self,
        state: State,
        pc: usize,
        source: Fact,
        driver: Driver<'_>,
        mut iterations: Option<u128>,
        retained: Fact,
    ) -> Result<()> {
        let full = retained == Atom::Never.fact();
        let mut current = self.mutable_initial(state)?;
        if !full {
            current.output = self.facts.array(self.ctx, retained)?;
        }
        if iterations.is_none() || iterations == Some(0) {
            current.auxiliary = if full {
                self.facts.boolean(self.ctx, false)?
            } else {
                Atom::Bool.fact()
            };
            let done = current.snapshot(self.ctx)?;
            self.mutable_done(done, pc, source, Mutation::Fill)?;
            if iterations == Some(0) {
                return Ok(());
            }
        }
        current.auxiliary = self.facts.boolean(self.ctx, true)?;
        let item = Item {
            arguments: [Atom::Int.fact(), Atom::Nil.fact()],
            count: 1,
            element: Atom::Int.fact(),
            index: Atom::Int.fact(),
            pair: None,
        };
        let depth = self.collection_depth(&current, driver, source)?;
        let Some(mut current) = self.collection_step(current, pc, driver, item, depth)? else {
            return Ok(());
        };
        if let Some(count) = &mut iterations {
            *count -= 1;
        }
        let depth = self.collection_depth(&current, driver, source)?;
        current.state.widening.get_or_insert(depth);
        loop {
            self.ctx.charge(1)?;
            if iterations.is_none() || iterations == Some(0) {
                let done = current.snapshot(self.ctx)?;
                self.mutable_done(done, pc, source, Mutation::Fill)?;
                if iterations == Some(0) {
                    break;
                }
            }
            let before = current.snapshot(self.ctx)?;
            let Some(next) = self.collection_step(before, pc, driver, item, depth)? else {
                break;
            };
            if let Some(count) = &mut iterations {
                *count -= 1;
            }
            if !current.join(self.ctx, self.facts, &next, true, depth)? {
                self.mutable_done(current, pc, source, Mutation::Fill)?;
                break;
            }
        }
        Ok(())
    }
}
