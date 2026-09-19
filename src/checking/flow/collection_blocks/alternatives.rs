use super::*;

struct Run {
    current: IterationState,
    depth: usize,
    queued: bool,
}

impl IterationState {
    pub(super) fn alternatives(self, ctx: &mut CallContext) -> Result<Buffer<Self>> {
        let mut result = Buffer::empty();
        result.push(ctx, self)?;
        Ok(result)
    }
}

impl Walker<'_> {
    pub(super) fn iteration_snapshot(
        &mut self,
        states: &[IterationState],
    ) -> Result<Buffer<IterationState>> {
        let mut result = Buffer::empty();
        for current in states {
            let current = current.snapshot(self.ctx)?;
            result.push(self.ctx, current)?;
        }
        Ok(result)
    }

    pub(super) fn iteration_depth(
        &mut self,
        states: &mut [IterationState],
        driver: Driver<'_>,
        source: Fact,
    ) -> Result<usize> {
        let mut depth = 0;
        for current in states.iter() {
            depth = depth.max(self.collection_depth(current, driver, source)?);
        }
        for current in states {
            self.ctx.charge(1)?;
            current.state.widening.get_or_insert(depth);
        }
        Ok(depth)
    }

    pub(super) fn iteration_widen(
        &mut self,
        states: &mut Buffer<IterationState>,
        next: Buffer<IterationState>,
        depth: usize,
    ) -> Result<bool> {
        let mut changed = false;
        for mut next in next.data {
            let mut found = false;
            for current in &mut states.data {
                self.ctx.charge(1)?;
                if current.state.compatible(self.ctx, &next.state)? {
                    changed |=
                        current.join(self.ctx, self.facts, &next, true, depth, self.program)?;
                    found = true;
                    break;
                }
            }
            if !found {
                next.state.widening.get_or_insert(depth);
                states.push(self.ctx, next)?;
                changed = true;
            }
        }
        Ok(changed)
    }

    pub(super) fn iteration_join(
        &mut self,
        alternatives: &mut Buffer<IterationState>,
        next: IterationState,
        depth: usize,
    ) -> Result<()> {
        for current in &mut alternatives.data {
            self.ctx.charge(1)?;
            if current.state.compatible(self.ctx, &next.state)? {
                current.join(self.ctx, self.facts, &next, false, depth, self.program)?;
                return Ok(());
            }
        }
        alternatives.push(self.ctx, next)
    }

    pub(super) fn iteration_next(
        &mut self,
        current: Buffer<IterationState>,
        pc: usize,
        driver: Driver<'_>,
        item: Item,
        depth: usize,
    ) -> Result<Buffer<IterationState>> {
        let mut result = Buffer::empty();
        for before in current.data {
            self.ctx.charge(1)?;
            for next in self.collection_step(before, pc, driver, item, depth)?.data {
                self.iteration_join(&mut result, next, depth)?;
            }
        }
        Ok(result)
    }

    fn iteration_enqueue(
        &mut self,
        runs: &mut Buffer<Run>,
        pending: &mut Buffer<usize>,
        mut next: IterationState,
        driver: Driver<'_>,
        source: Fact,
    ) -> Result<()> {
        for (index, run) in runs.data.iter_mut().enumerate() {
            self.ctx.charge(1)?;
            if !run.current.state.compatible(self.ctx, &next.state)? {
                continue;
            }
            let output = run.current.output;
            let changed =
                run.current
                    .join(self.ctx, self.facts, &next, true, run.depth, self.program)?;
            if driver.method == Method::Count && run.current.output != output {
                run.current.output = Atom::Int.fact();
            }
            if changed && !run.queued {
                pending.push(self.ctx, index)?;
                run.queued = true;
            }
            return Ok(());
        }
        // Freeze each import lifetime from its first completed iteration.
        let depth = self.collection_depth(&next, driver, source)?;
        next.state.widening.get_or_insert(depth);
        pending.push(self.ctx, runs.data.len())?;
        runs.push(
            self.ctx,
            Run {
                current: next,
                depth,
                queued: true,
            },
        )
    }

    pub(super) fn iteration_loop(
        &mut self,
        first: Buffer<IterationState>,
        driver: Driver<'_>,
        source: Fact,
        mut advance: impl FnMut(&mut Self, IterationState, usize) -> Result<Buffer<IterationState>>,
        mut finish: impl FnMut(&mut Self, IterationState) -> Result<bool>,
    ) -> Result<Buffer<IterationState>> {
        let mut runs = Buffer::empty();
        let mut pending = Buffer::empty();
        for current in first.data {
            self.iteration_enqueue(&mut runs, &mut pending, current, driver, source)?;
        }
        while let Some(index) = pending.data.pop() {
            self.ctx.charge(1)?;
            runs.data[index].queued = false;
            let run = &runs.data[index];
            let current = run.current.snapshot(self.ctx)?;
            let depth = run.depth;
            let done = current.snapshot(self.ctx)?;
            if finish(self, done)? {
                for next in advance(self, current, depth)?.data {
                    self.iteration_enqueue(&mut runs, &mut pending, next, driver, source)?;
                }
            }
        }
        let mut result = Buffer::empty();
        for run in runs.data {
            result.push(self.ctx, run.current)?;
        }
        Ok(result)
    }
}
