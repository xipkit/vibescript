use super::*;

struct Frame {
    input: Fact,
    initial: State,
    arm: usize,
    result: Option<IterationState>,
    walk: Option<Walk>,
}

struct Walk {
    source: Fact,
    schedule: Schedule,
    current: Option<IterationState>,
    stable: Option<IterationState>,
    seed: Option<IterationState>,
    after: Option<IterationState>,
    choice: usize,
    targets: Buffer<Fact>,
    position: usize,
    key: Option<Fact>,
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
    Child(Fact, State),
    Done(Option<IterationState>),
}

impl Walker<'_> {
    pub(super) fn deep_hash(
        &mut self,
        state: &State,
        pc: usize,
        driver: Driver<'_>,
        input: Fact,
    ) -> Result<Option<IterationState>> {
        let mut frames = Buffer::empty();
        let initial = state.snapshot(self.ctx)?;
        frames.push(
            self.ctx,
            Frame {
                input,
                initial,
                arm: 0,
                result: None,
                walk: None,
            },
        )?;
        loop {
            self.ctx.charge(1)?;
            let level = frames.data.len();
            let frame = frames.data.last_mut().unwrap();
            if let Some(walk) = &mut frame.walk {
                match self.deep_advance(walk, pc, driver)? {
                    Advance::Child(input, state) => {
                        frames.push(
                            self.ctx,
                            Frame {
                                input,
                                initial: state,
                                arm: 0,
                                result: None,
                                walk: None,
                            },
                        )?;
                    }
                    Advance::Done(done) => {
                        if let Some(done) = done {
                            let depth = self.collection_depth(&done, driver, frame.input)?;
                            self.hash_join(&mut frame.result, done, depth)?;
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
                    Node::Hash(_, _, false) | Node::Shape(_, _, _, false)
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
                    let done = self.deep_unknown(&frame.initial, pc, driver, arm)?;
                    let depth = self.collection_depth(&done, driver, arm)?;
                    self.hash_join(&mut frame.result, done, depth)?;
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
                    Node::Array(_) | Node::Hash(_, _, true) | Node::Shape(_, _, _, true) => {
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
                    Node::Named(_) | Node::Nominal { .. } | Node::Hash(..) | Node::Shape(..) => {
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
                        self.hash_join(&mut frame.result, done, depth)?;
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
                    self.hash_join(&mut frame.result, done, depth)?;
                }
                frame.walk = Some(Walk {
                    source: arm,
                    schedule,
                    current: Some(initial),
                    stable: None,
                    seed: None,
                    after: None,
                    choice: 0,
                    targets: Buffer::empty(),
                    position: 0,
                    key: None,
                    depth,
                });
                continue;
            }
            let done = frames.data.pop().unwrap().result;
            let Some(parent) = frames.data.last_mut() else {
                return Ok(done);
            };
            let walk = parent.walk.as_mut().unwrap();
            let before = walk.current.take().unwrap();
            walk.position += 1;
            if let Some(mut child) = done {
                child.output = if let Some(key) = walk.key.take() {
                    let previous = walk.targets.data[walk.choice];
                    walk.targets.data[walk.choice] =
                        self.facts.union(self.ctx, &[previous, key])?;
                    self.facts
                        .collection_write(self.ctx, before.output, key, child.output)?
                        .receiver
                } else {
                    self.group_append(before.output, child.output)?
                };
                walk.current = Some(child);
            }
        }
    }

    fn deep_advance(&mut self, walk: &mut Walk, pc: usize, driver: Driver<'_>) -> Result<Advance> {
        if let Schedule::Tuple(length) = walk.schedule {
            let Some(current) = walk.current.take() else {
                return Ok(Advance::Done(None));
            };
            if walk.position == length {
                return Ok(Advance::Done(Some(current)));
            }
            let Node::Tuple(items) = self.facts.node(walk.source) else {
                unreachable!()
            };
            let input = items.data[walk.position];
            let state = current.state.snapshot(self.ctx)?;
            walk.current = Some(current);
            return Ok(Advance::Child(input, state));
        }
        let Schedule::Repeated {
            item: entries,
            hash,
            repeat,
        } = walk.schedule
        else {
            unreachable!()
        };
        if entries == Atom::Never.fact() {
            return Ok(Advance::Done(None));
        }
        if walk.seed.is_none() {
            walk.seed = walk.current.take();
            for _ in 0..self.facts.arm_count(entries) {
                walk.targets.push(self.ctx, Atom::Never.fact())?;
            }
        }
        if walk.position != 0 {
            if let Some(current) = walk.current.take() {
                self.hash_join(&mut walk.after, current, walk.depth)?;
            }
            walk.position = 0;
            walk.choice += 1;
        }
        loop {
            self.ctx.charge(1)?;
            if walk.choice == self.facts.arm_count(entries) {
                let Some(mut current) = walk.after.take() else {
                    let done = walk.stable.take();
                    return self.deep_done(walk, done);
                };
                if !repeat {
                    return self.deep_done(walk, Some(current));
                }
                let changed = if let Some(stable) = &mut walk.stable {
                    stable.join(self.ctx, self.facts, &current, true, walk.depth)?
                } else {
                    walk.depth = self.collection_depth(&current, driver, walk.source)?;
                    current.state.widening.get_or_insert(walk.depth);
                    walk.stable = Some(current);
                    true
                };
                if !changed {
                    let done = walk.stable.take();
                    return self.deep_done(walk, done);
                }
                walk.seed = Some(walk.stable.as_ref().unwrap().snapshot(self.ctx)?);
                walk.choice = 0;
            }
            let entry = self.facts.arm(entries, walk.choice);
            let mut current = walk.seed.as_ref().unwrap().snapshot(self.ctx)?;
            let input = if hash {
                let key = self
                    .facts
                    .extract(self.ctx, entry, crate::bytecode::Selection::At(0))?;
                let value =
                    self.facts
                        .extract(self.ctx, entry, crate::bytecode::Selection::At(1))?;
                let parent = current.output;
                let item = Item {
                    arguments: [key, Atom::Nil.fact()],
                    count: 1,
                    element: key,
                    index: Atom::Int.fact(),
                    pair: None,
                };
                let Some(mut next) = self.collection_step(current, pc, driver, item, walk.depth)?
                else {
                    walk.choice += 1;
                    continue;
                };
                let key =
                    self.canonical_group_keys(&next.state, pc, driver.site.unwrap(), next.output)?;
                if key == Atom::Never.fact() {
                    walk.choice += 1;
                    continue;
                }
                next.output = parent;
                current = next;
                walk.key = Some(key);
                value
            } else {
                entry
            };
            let state = current.state.snapshot(self.ctx)?;
            walk.current = Some(current);
            return Ok(Advance::Child(input, state));
        }
    }

    fn deep_done(&mut self, walk: &Walk, done: Option<IterationState>) -> Result<Advance> {
        let Some(mut done) = done else {
            return Ok(Advance::Done(None));
        };
        let Schedule::Repeated {
            item, hash: true, ..
        } = walk.schedule
        else {
            return Ok(Advance::Done(Some(done)));
        };
        // Normal completion has visited every required source field, even though
        // the fact lattice does not retain the hash's insertion order.
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
                return Ok(Advance::Done(None));
            }
            if matches!(self.facts.node(target), Node::String(_)) {
                done.output = self.hash_require_key(done.output, target)?;
                if done.output == Atom::Never.fact() {
                    return Ok(Advance::Done(None));
                }
            }
        }
        Ok(Advance::Done(Some(done)))
    }

    fn deep_unknown(
        &mut self,
        state: &State,
        pc: usize,
        driver: Driver<'_>,
        input: Fact,
    ) -> Result<IterationState> {
        // An unknown child can be a scalar or contain any number of hash keys.
        // Summarize those visits without unfolding an unbounded recursive type.
        let mut current = IterationState {
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
        let mut depth = self.collection_depth(&current, driver, input)?;
        let mut first = true;
        loop {
            self.ctx.charge(1)?;
            let before = current.snapshot(self.ctx)?;
            let Some(mut next) = self.collection_step(before, pc, driver, item, depth)? else {
                break;
            };
            let key =
                self.canonical_group_keys(&next.state, pc, driver.site.unwrap(), next.output)?;
            if key == Atom::Never.fact() {
                break;
            }
            next.output = input;
            if first {
                depth = self.collection_depth(&next, driver, input)?;
                current.state.widening.get_or_insert(depth);
                first = false;
            }
            if !current.join(self.ctx, self.facts, &next, true, depth)? {
                break;
            }
        }
        Ok(current)
    }
}
