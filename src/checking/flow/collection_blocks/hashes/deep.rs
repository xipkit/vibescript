use super::*;

struct Frame {
    input: Fact,
    initial: State,
    arm: usize,
    result: Buffer<IterationState>,
    walk: Option<Walk>,
}

struct Visit {
    input: Fact,
    current: IterationState,
    key: Option<Fact>,
    choice: usize,
}

struct Walk {
    source: Fact,
    schedule: Schedule,
    current: Buffer<IterationState>,
    stable: Buffer<IterationState>,
    after: Buffer<IterationState>,
    pending: Buffer<Visit>,
    active: Option<Visit>,
    targets: Buffer<Fact>,
    position: usize,
    started: bool,
    depth: usize,
}

#[derive(Clone, Copy)]
enum Schedule {
    Tuple(usize),
    Repeated {
        item: Fact,
        hash: bool,
        repeat: bool,
    },
}

enum Advance {
    Child,
    Done(Buffer<IterationState>),
}

impl Walker<'_> {
    pub(super) fn deep_hash(
        &mut self,
        state: &State,
        pc: usize,
        driver: Driver<'_>,
        input: Fact,
    ) -> Result<Buffer<IterationState>> {
        let mut frames = Buffer::empty();
        let initial = state.snapshot(self.ctx)?;
        frames.push(
            self.ctx,
            Frame {
                input,
                initial,
                arm: 0,
                result: Buffer::empty(),
                walk: None,
            },
        )?;
        loop {
            self.ctx.charge(1)?;
            let level = frames.data.len();
            let frame = frames.data.last_mut().unwrap();
            if let Some(walk) = &mut frame.walk {
                match self.deep_advance(walk, pc, driver)? {
                    Advance::Child => {
                        let visit = walk.active.as_ref().unwrap();
                        let input = visit.input;
                        let initial = visit.current.state.snapshot(self.ctx)?;
                        frames.push(
                            self.ctx,
                            Frame {
                                input,
                                initial,
                                arm: 0,
                                result: Buffer::empty(),
                                walk: None,
                            },
                        )?;
                    }
                    Advance::Done(done) => {
                        for done in done.data {
                            let depth = self.collection_depth(&done, driver, frame.input)?;
                            self.iteration_join(&mut frame.result, done, depth)?;
                        }
                        frame.walk = None;
                    }
                }
                continue;
            }
            if frame.arm < self.facts.arm_count(frame.input) {
                let mut arm = self.hash_view(self.facts.arm(frame.input, frame.arm));
                frame.arm += 1;
                if matches!(
                    self.facts.node(arm),
                    Node::Hash(_, _, kind) | Node::Shape(_, _, _, kind) if !kind.plain()
                ) {
                    // Nested hash data is traversed directly, without member dispatch.
                    arm = self.plain_hash_data(arm)?;
                }
                let container = matches!(
                    self.facts.node(arm),
                    Node::Tuple(_) | Node::Array(_) | Node::Hash(..) | Node::Shape(..)
                );
                if container && level > crate::budget::MAX_VALUE_DEPTH {
                    self.emit_error(&frame.initial, pc, handlers::bit(ErrorClass::Limit))?;
                    continue;
                }
                if let Node::Atom(Atom::Unknown | Atom::Any) = self.facts.node(arm) {
                    for done in self.deep_unknown(&frame.initial, pc, driver, arm)?.data {
                        let depth = self.collection_depth(&done, driver, arm)?;
                        self.iteration_join(&mut frame.result, done, depth)?;
                    }
                    continue;
                }
                let (schedule, empty, output) = match self.facts.node(arm) {
                    Node::Atom(Atom::Never) => continue,
                    Node::Tuple(items) => {
                        let count = items.data.len();
                        (
                            Schedule::Tuple(count),
                            false,
                            self.facts.tuple(self.ctx, &[])?,
                        )
                    }
                    Node::Array(_)
                    | Node::Hash(_, _, HashKind::PLAIN)
                    | Node::Shape(_, _, _, HashKind::PLAIN) => {
                        let hash = !matches!(self.facts.node(arm), Node::Array(_));
                        let iteration = self.facts.iteration(self.ctx, arm)?;
                        let output = if hash {
                            self.facts.shape(self.ctx, &[], false)?
                        } else {
                            self.facts.tuple(self.ctx, &[])?
                        };
                        (
                            Schedule::Repeated {
                                item: iteration.item,
                                hash,
                                repeat: iteration.repeat != Atom::Never.fact(),
                            },
                            iteration.empty != Atom::Never.fact(),
                            output,
                        )
                    }
                    Node::Named(_)
                    | Node::Nominal { .. }
                    | Node::Instance { .. }
                    | Node::Hash(..)
                    | Node::Shape(..) => {
                        self.incomplete(pc)?;
                        continue;
                    }
                    _ => {
                        let done = IterationState {
                            state: frame.initial.snapshot(self.ctx)?,
                            output: arm,
                            auxiliary: Atom::Never.fact(),
                            previous: Atom::Never.fact(),
                        };
                        let depth = self.collection_depth(&done, driver, arm)?;
                        self.iteration_join(&mut frame.result, done, depth)?;
                        continue;
                    }
                };
                let initial = IterationState {
                    state: frame.initial.snapshot(self.ctx)?,
                    output,
                    auxiliary: Atom::Never.fact(),
                    previous: Atom::Never.fact(),
                };
                let depth = self.collection_depth(&initial, driver, arm)?;
                if empty {
                    let done = initial.snapshot(self.ctx)?;
                    self.iteration_join(&mut frame.result, done, depth)?;
                }
                let mut targets = Buffer::empty();
                if let Schedule::Repeated {
                    item, hash: true, ..
                } = schedule
                {
                    for _ in 0..self.facts.arm_count(item) {
                        targets.push(self.ctx, Atom::Never.fact())?;
                    }
                }
                frame.walk = Some(Walk {
                    source: arm,
                    schedule,
                    current: initial.alternatives(self.ctx)?,
                    stable: Buffer::empty(),
                    after: Buffer::empty(),
                    pending: Buffer::empty(),
                    active: None,
                    targets,
                    position: 0,
                    started: false,
                    depth,
                });
                continue;
            }
            let done = frames.data.pop().unwrap().result;
            let Some(parent) = frames.data.last_mut() else {
                return Ok(done);
            };
            let walk = parent.walk.as_mut().unwrap();
            let visit = walk.active.take().unwrap();
            for mut child in done.data {
                child.output = if let Some(key) = visit.key {
                    let previous = walk.targets.data[visit.choice];
                    walk.targets.data[visit.choice] =
                        self.facts.union(self.ctx, &[previous, key])?;
                    self.facts
                        .collection_write(self.ctx, visit.current.output, key, child.output)?
                        .receiver
                } else {
                    self.group_append(visit.current.output, child.output)?
                };
                self.iteration_join(&mut walk.after, child, walk.depth)?;
            }
        }
    }

    fn deep_advance(&mut self, walk: &mut Walk, pc: usize, driver: Driver<'_>) -> Result<Advance> {
        loop {
            self.ctx.charge(1)?;
            if matches!(walk.schedule, Schedule::Repeated { item, .. } if item == Atom::Never.fact())
            {
                return Ok(Advance::Done(Buffer::empty()));
            }
            if let Some(visit) = walk.pending.data.pop() {
                assert!(walk.active.is_none());
                walk.active = Some(visit);
                return Ok(Advance::Child);
            }
            if walk.started {
                let after = std::mem::replace(&mut walk.after, Buffer::empty());
                match walk.schedule {
                    Schedule::Tuple(_) => {
                        walk.current = after;
                        walk.position += 1;
                    }
                    Schedule::Repeated { repeat, .. } => {
                        if after.data.is_empty() {
                            let done = std::mem::replace(&mut walk.stable, Buffer::empty());
                            return self.deep_done(walk, done);
                        }
                        if !repeat {
                            return self.deep_done(walk, after);
                        }
                        if walk.stable.data.is_empty() {
                            walk.stable = after;
                            walk.depth =
                                self.iteration_depth(&mut walk.stable.data, driver, walk.source)?;
                        } else if !self.iteration_widen(&mut walk.stable, after, walk.depth)? {
                            let done = std::mem::replace(&mut walk.stable, Buffer::empty());
                            return self.deep_done(walk, done);
                        }
                        walk.current = self.iteration_snapshot(&walk.stable.data)?;
                    }
                }
            }
            if walk.current.data.is_empty()
                || matches!(walk.schedule, Schedule::Tuple(length) if walk.position == length)
            {
                let done = std::mem::replace(&mut walk.current, Buffer::empty());
                return self.deep_done(walk, done);
            }
            walk.started = true;
            let current = std::mem::replace(&mut walk.current, Buffer::empty());
            for current in current.data {
                self.ctx.charge(1)?;
                match walk.schedule {
                    Schedule::Tuple(_) => {
                        let Node::Tuple(items) = self.facts.node(walk.source) else {
                            unreachable!()
                        };
                        walk.pending.push(
                            self.ctx,
                            Visit {
                                input: items.data[walk.position],
                                current,
                                key: None,
                                choice: 0,
                            },
                        )?;
                    }
                    Schedule::Repeated {
                        item: entries,
                        hash,
                        ..
                    } => {
                        for choice in 0..self.facts.arm_count(entries) {
                            let entry = self.facts.arm(entries, choice);
                            let before = current.snapshot(self.ctx)?;
                            if !hash {
                                walk.pending.push(
                                    self.ctx,
                                    Visit {
                                        input: entry,
                                        current: before,
                                        key: None,
                                        choice,
                                    },
                                )?;
                                continue;
                            }
                            let key = self.facts.extract(
                                self.ctx,
                                entry,
                                crate::bytecode::Selection::At(0),
                            )?;
                            let input = self.facts.extract(
                                self.ctx,
                                entry,
                                crate::bytecode::Selection::At(1),
                            )?;
                            let parent = before.output;
                            let item = Item {
                                arguments: [key, Atom::Nil.fact()],
                                count: 1,
                                element: key,
                                index: Atom::Int.fact(),
                                pair: None,
                            };
                            for mut next in self
                                .collection_step(before, pc, driver, item, walk.depth)?
                                .data
                            {
                                let key = self.canonical_group_keys(
                                    &next.state,
                                    pc,
                                    driver.site.unwrap(),
                                    next.output,
                                )?;
                                if key == Atom::Never.fact() {
                                    continue;
                                }
                                next.output = parent;
                                walk.pending.push(
                                    self.ctx,
                                    Visit {
                                        input,
                                        current: next,
                                        key: Some(key),
                                        choice,
                                    },
                                )?;
                            }
                        }
                    }
                }
            }
        }
    }

    fn deep_done(&mut self, walk: &Walk, done: Buffer<IterationState>) -> Result<Advance> {
        let Schedule::Repeated {
            item, hash: true, ..
        } = walk.schedule
        else {
            return Ok(Advance::Done(done));
        };
        let mut result = Buffer::empty();
        for mut done in done.data {
            // Normal completion visits every required field, even when insertion order is abstract.
            for (index, &target) in walk.targets.data.iter().enumerate() {
                self.ctx.charge(1)?;
                let entry = self.facts.arm(item, index);
                let key = self
                    .facts
                    .extract(self.ctx, entry, crate::bytecode::Selection::At(0))?;
                let (Node::String(name) | Node::Symbol(name)) = self.facts.node(key) else {
                    continue;
                };
                let name = name.clone();
                if !self
                    .facts
                    .selected_field(self.ctx, walk.source, name.as_bytes().unwrap())?
                    .is_some_and(|(_, optional)| !optional)
                {
                    continue;
                }
                if target == Atom::Never.fact() {
                    done.output = Atom::Never.fact();
                    break;
                }
                if matches!(self.facts.node(target), Node::String(_)) {
                    done.output = self.hash_require_key(done.output, target)?;
                    if done.output == Atom::Never.fact() {
                        break;
                    }
                }
            }
            if done.output != Atom::Never.fact() {
                result.push(self.ctx, done)?;
            }
        }
        Ok(Advance::Done(result))
    }

    fn deep_unknown(
        &mut self,
        state: &State,
        pc: usize,
        driver: Driver<'_>,
        input: Fact,
    ) -> Result<Buffer<IterationState>> {
        // Unknown children may be scalar or contain arbitrarily many nested hash keys.
        let current = IterationState {
            state: state.snapshot(self.ctx)?,
            output: input,
            auxiliary: Atom::Never.fact(),
            previous: Atom::Never.fact(),
        };
        let key = self
            .facts
            .union(self.ctx, &[Atom::String.fact(), Atom::Symbol.fact()])?;
        let item = Item {
            arguments: [key, Atom::Nil.fact()],
            count: 1,
            element: key,
            index: Atom::Int.fact(),
            pair: None,
        };
        let first = current.alternatives(self.ctx)?;
        self.iteration_loop(
            first,
            driver,
            input,
            |walker, current, depth| {
                let mut result = Buffer::empty();
                for mut next in walker
                    .collection_step(current, pc, driver, item, depth)?
                    .data
                {
                    let key = walker.canonical_group_keys(
                        &next.state,
                        pc,
                        driver.site.unwrap(),
                        next.output,
                    )?;
                    if key != Atom::Never.fact() {
                        next.output = input;
                        result.push(walker.ctx, next)?;
                    }
                }
                Ok(result)
            },
            |_, _| Ok(true),
        )
    }
}
