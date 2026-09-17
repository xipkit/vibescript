use super::*;
use crate::checking::facts::Field;

pub(super) const CLASSES: [ErrorClass; 8] = [
    ErrorClass::Runtime,
    ErrorClass::Standard,
    ErrorClass::Assertion,
    ErrorClass::Limit,
    ErrorClass::Type,
    ErrorClass::ZeroDivision,
    ErrorClass::LocalJump,
    ErrorClass::Argument,
];

pub(super) fn bit(class: ErrorClass) -> u8 {
    1 << class as u8
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Phase {
    Body,
    Rescue(usize),
    Else,
    Ensure,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Transfer {
    Value(Operand),
    Return {
        pc: usize,
        value: Fact,
    },
    Block {
        pc: usize,
        completion: blocks::Completion,
        value: Fact,
    },
    Jump {
        target: usize,
        index: usize,
        breaking: bool,
        value: Fact,
    },
    Retry(usize),
    InvalidRetry,
    Error(u8),
}

impl Transfer {
    fn compatible(self, other: Self) -> bool {
        match (self, other) {
            (Self::Value(_), Self::Value(_)) | (Self::Error(_), Self::Error(_)) => true,
            (Self::Return { pc: a, .. }, Self::Return { pc: b, .. }) => a == b,
            (
                Self::Block {
                    pc: a,
                    completion: b,
                    ..
                },
                Self::Block {
                    pc: x,
                    completion: y,
                    ..
                },
            ) => a == x && b == y,
            (Self::Retry(a), Self::Retry(b)) => a == b,
            (Self::InvalidRetry, Self::InvalidRetry) => true,
            (
                Self::Jump {
                    target: a,
                    index: b,
                    breaking: c,
                    ..
                },
                Self::Jump {
                    target: x,
                    index: y,
                    breaking: z,
                    ..
                },
            ) => (a, b, c) == (x, y, z),
            _ => false,
        }
    }

    pub(super) fn join(
        &mut self,
        ctx: &mut CallContext,
        facts: &mut Facts,
        other: Self,
        depth: Option<usize>,
    ) -> Result<bool> {
        let before = *self;
        match (&mut *self, other) {
            (Self::Value(a), Self::Value(b)) => *a = a.join(ctx, facts, b, depth)?,
            (Self::Return { value: a, .. }, Self::Return { value: b, .. })
            | (Self::Block { value: a, .. }, Self::Block { value: b, .. })
            | (Self::Jump { value: a, .. }, Self::Jump { value: b, .. }) => {
                *a = facts.joined(ctx, *a, b, depth)?
            }
            (Self::Error(a), Self::Error(b)) => *a |= b,
            (Self::Retry(_), Self::Retry(_)) => (),
            (Self::InvalidRetry, Self::InvalidRetry) => (),
            _ => unreachable!(),
        }
        Ok(*self != before)
    }
}

pub(super) struct Entry {
    pub state: State,
    pub polarity: usize,
    pub queued: bool,
}

impl State {
    pub(super) fn compatible(&self, ctx: &mut CallContext, other: &Self) -> Result<bool> {
        if self.stack.data.len() != other.stack.data.len()
            || self.loops.data.len() != other.loops.data.len()
            || self.arguments.data.len() != other.arguments.data.len()
            || self.addresses.data.len() != other.addresses.data.len()
            || self.raises.data.len() != other.raises.data.len()
            || self.attempts.data.len() != other.attempts.data.len()
        {
            return Ok(false);
        }
        for (a, b) in self.attempts.data.iter().zip(&other.attempts.data) {
            ctx.charge(1)?;
            if a.spec != b.spec
                || a.phase != b.phase
                || a.stack != b.stack
                || a.loops != b.loops
                || a.arguments != b.arguments
                || a.addresses != b.addresses
                || a.raises != b.raises
                || !match (a.pending, b.pending) {
                    (None, None) => true,
                    (Some(a), Some(b)) => a.compatible(b),
                    _ => false,
                }
            {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn restore(&mut self, attempt: Attempt) {
        self.stack.data.truncate(attempt.stack);
        self.loops.data.truncate(attempt.loops);
        self.arguments.data.truncate(attempt.arguments);
        self.addresses.data.truncate(attempt.addresses);
        self.raises.data.truncate(attempt.raises);
    }

    pub(super) fn current_error(&self, ctx: &mut CallContext, ambient: u16) -> Result<u16> {
        ctx.charge(self.attempts.data.len() as u64)?;
        Ok(self
            .attempts
            .data
            .iter()
            .rev()
            .find_map(|h| matches!(h.phase, Phase::Rescue(_)).then_some(u16::from(h.error)))
            .unwrap_or(ambient))
    }
}

impl Walker<'_> {
    fn block_exit(
        &mut self,
        state: &State,
        pc: usize,
        completion: blocks::Completion,
        value: Fact,
    ) -> Result<()> {
        if let (Some(report), Some(captures)) = (&mut self.report, &state.captures) {
            captures.record(
                self.ctx,
                self.facts,
                &mut report.block_exits,
                pc,
                completion,
                value,
            )?;
        }
        Ok(())
    }

    fn declare_slots(&mut self, state: &mut State, pc: usize, slots: &[usize]) -> Result<()> {
        for &slot in slots {
            self.ctx.charge(1)?;
            let binding = state.locals.get(self.ctx, slot)?;
            if binding.missing {
                let value = self
                    .facts
                    .union(self.ctx, &[binding.value, Atom::Nil.fact()])?;
                self.store(state, pc, slot, Operand::local(value, slot))?;
            }
        }
        Ok(())
    }

    fn clear_rescue(&mut self, state: &mut State, attempt: Attempt) -> Result<()> {
        if let Phase::Rescue(index) = attempt.phase {
            if let Some(slot) = self.program.handlers[attempt.spec].rescues[index].binding {
                state.store(self.ctx, self.facts, slot, Atom::Never.fact())?;
                state.locals.set(
                    self.ctx,
                    slot,
                    Binding {
                        value: Atom::Never.fact(),
                        missing: true,
                    },
                )?;
            }
        }
        Ok(())
    }

    fn prepare_ensure(
        &mut self,
        state: &mut State,
        pc: usize,
        transfer: Transfer,
    ) -> Result<Option<usize>> {
        let attempt = *state.attempts.data.last().unwrap();
        let spec = &self.program.handlers[attempt.spec];
        self.declare_slots(state, pc, &spec.body_locals)?;
        for clause in &spec.rescues {
            self.declare_slots(state, pc, &clause.locals)?;
        }
        self.declare_slots(state, pc, &spec.alternate_locals)?;
        self.clear_rescue(state, attempt)?;
        state.restore(attempt);
        if let Some(ensure) = spec.ensure {
            let attempt = state.attempts.data.last_mut().unwrap();
            attempt.phase = Phase::Ensure;
            attempt.error = 0;
            attempt.pending = Some(transfer);
            Ok(Some(ensure))
        } else {
            state.attempts.data.pop();
            Ok(None)
        }
    }

    pub(super) fn normal_attempt(
        &mut self,
        mut state: State,
        pc: usize,
        body: bool,
    ) -> Result<Edges> {
        let attempt = *state.attempts.data.last().unwrap();
        let spec = &self.program.handlers[attempt.spec];
        let mut value = state.stack.data.pop().unwrap();
        if let Phase::Rescue(index) = attempt.phase {
            if let Some(slot) = spec.rescues[index].binding {
                value.invalidate(slot);
            }
        }
        if body {
            self.declare_slots(&mut state, pc, &spec.body_locals)?;
            for clause in &spec.rescues {
                self.declare_slots(&mut state, pc, &clause.locals)?;
            }
            if let Some(alternate) = spec.alternate {
                state.attempts.data.last_mut().unwrap().phase = Phase::Else;
                return Ok([Some((alternate, state)), None]);
            }
        }
        if let Some(ensure) = self.prepare_ensure(&mut state, pc, Transfer::Value(value))? {
            Ok([Some((ensure, state)), None])
        } else {
            state.stack.push(self.ctx, value)?;
            Ok([Some((spec.end, state)), None])
        }
    }

    pub(super) fn transfer(
        &mut self,
        mut state: State,
        pc: usize,
        transfer: Transfer,
    ) -> Result<Edges> {
        if let Transfer::Error(classes) = transfer {
            self.emit_error(&state, pc, classes)?;
            return Ok([None, None]);
        }
        while let Some(attempt) = state.attempts.data.last().copied() {
            let exits = match transfer {
                Transfer::Return { .. } | Transfer::Block { .. } | Transfer::InvalidRetry => true,
                Transfer::Jump { index, .. } => attempt.loops > index,
                Transfer::Retry(index) => state.attempts.data.len() - 1 > index,
                _ => unreachable!(),
            };
            if !exits {
                break;
            }
            self.ctx.charge(1)?;
            if attempt.phase == Phase::Ensure {
                state.attempts.data.pop();
            } else if let Some(ensure) = self.prepare_ensure(&mut state, pc, transfer)? {
                return Ok([Some((ensure, state)), None]);
            }
        }
        match transfer {
            Transfer::Return { pc, value: actual } => {
                if let Some(ty) = self.function.return_type {
                    let expected = self.contracts[ty];
                    let relation = self.facts.relation(self.ctx, actual, expected)?;
                    if relation != Relation::Accepted {
                        if let Some(report) = self.report.as_mut() {
                            report.throws |= bit(ErrorClass::Runtime);
                        }
                    }
                    if relation == Relation::Rejected {
                        self.issue(pc, IssueKind::Return { actual, expected })?;
                    }
                }
                if let Some(report) = self.report.as_mut() {
                    report.returns = self.facts.union(self.ctx, &[report.returns, actual])?;
                }
                self.block_exit(&state, pc, blocks::Completion::Value, actual)?;
                Ok([None, None])
            }
            Transfer::Block {
                pc,
                completion,
                value,
            } => {
                self.block_exit(&state, pc, completion, value)?;
                Ok([None, None])
            }
            Transfer::Jump {
                target,
                index,
                breaking,
                value,
            } => {
                state.loops.data.truncate(index + 1);
                let current = state.loops.data.last_mut().unwrap();
                if breaking {
                    current.result = value;
                }
                state.stack.data.truncate(current.base);
                state.arguments.data.truncate(current.argument_base);
                state.addresses.data.truncate(current.address_base);
                state.raises.data.truncate(current.raise_base);
                Ok([Some((target, state)), None])
            }
            Transfer::Retry(index) => {
                let attempt = state.attempts.data[index];
                self.clear_rescue(&mut state, attempt)?;
                state.restore(attempt);
                let current = &mut state.attempts.data[index];
                current.phase = Phase::Body;
                current.error = 0;
                current.pending = None;
                Ok([
                    Some((self.program.handlers[attempt.spec].body, state)),
                    None,
                ])
            }
            Transfer::InvalidRetry => {
                if let Some(report) = self.report.as_mut() {
                    report.throws |= bit(ErrorClass::LocalJump);
                }
                self.block_exit(
                    &state,
                    pc,
                    blocks::Completion::Error(ErrorClass::LocalJump),
                    Atom::Never.fact(),
                )?;
                Ok([None, None])
            }
            _ => unreachable!(),
        }
    }

    pub(super) fn end_ensure(&mut self, mut state: State, pc: usize) -> Result<Edges> {
        let attempt = state.attempts.data.pop().unwrap();
        match attempt.pending.unwrap() {
            Transfer::Value(value) => {
                state.stack.push(self.ctx, value)?;
                Ok([Some((self.program.handlers[attempt.spec].end, state)), None])
            }
            pending => self.transfer(state, pc, pending),
        }
    }

    fn error_value(&mut self, class: ErrorClass) -> Result<Fact> {
        let class = self.facts.string(self.ctx, class.name().as_bytes())?;
        let string = Atom::String.fact();
        let backtrace = self.facts.array(self.ctx, string)?;
        let mut fields = Buffer::empty();
        for (name, value) in [
            ("type", class),
            ("class", class),
            ("message", string),
            ("to_s", string),
            ("code_frame", string),
            ("backtrace", backtrace),
        ] {
            let name = self.ctx.bytes(name.as_bytes())?;
            fields.push(
                self.ctx,
                Field {
                    name,
                    value,
                    optional: false,
                },
            )?;
        }
        let shape = self
            .facts
            .shape_fields(self.ctx, fields, false, Atom::String.fact(), true)?;
        self.facts
            .protected(self.ctx, shape, crate::hash::Tag::Error)
    }

    pub(super) fn emit_error(&mut self, state: &State, pc: usize, classes: u8) -> Result<()> {
        self.ctx.checkpoint()?;
        if state.attempts.data.is_empty() {
            if let Some(report) = self.report.as_mut() {
                report.throws |= classes;
            }
            if state.captures.is_some() {
                for class in CLASSES {
                    self.ctx.charge(1)?;
                    if classes & bit(class) != 0 {
                        self.block_exit(
                            state,
                            pc,
                            blocks::Completion::Error(class),
                            Atom::Never.fact(),
                        )?;
                    }
                }
            }
            return Ok(());
        }
        for class in CLASSES {
            if classes & bit(class) == 0 {
                continue;
            }
            self.ctx.charge(1)?;
            let state = state.snapshot(self.ctx)?;
            if let Some(edge) = self.error(state, pc, class)? {
                self.extra.push(self.ctx, edge)?;
            }
        }
        Ok(())
    }

    fn error(
        &mut self,
        mut state: State,
        pc: usize,
        class: ErrorClass,
    ) -> Result<Option<(usize, State)>> {
        while let Some(attempt) = state.attempts.data.last().copied() {
            self.ctx.charge(1)?;
            if attempt.phase == Phase::Ensure {
                state.attempts.data.pop();
                continue;
            }
            state.restore(attempt);
            let spec = &self.program.handlers[attempt.spec];
            self.declare_slots(&mut state, pc, &spec.body_locals)?;
            if attempt.phase == Phase::Body {
                for (index, clause) in spec.rescues.iter().enumerate() {
                    self.ctx.charge(clause.classes.len() as u64 + 1)?;
                    if clause.classes.iter().any(|filter| filter.matches(class)) {
                        if clause.empty {
                            break;
                        }
                        let current = state.attempts.data.last_mut().unwrap();
                        current.phase = Phase::Rescue(index);
                        current.error = bit(class);
                        if let Some(slot) = clause.binding {
                            let value = self.error_value(class)?;
                            state.store(self.ctx, self.facts, slot, value)?;
                        }
                        return Ok(Some((clause.start, state)));
                    }
                    self.declare_slots(&mut state, pc, &clause.locals)?;
                }
            }
            if let Some(ensure) =
                self.prepare_ensure(&mut state, pc, Transfer::Error(bit(class)))?
            {
                return Ok(Some((ensure, state)));
            }
        }
        if let Some(report) = self.report.as_mut() {
            report.throws |= bit(class);
        }
        self.block_exit(
            &state,
            pc,
            blocks::Completion::Error(class),
            Atom::Never.fact(),
        )?;
        Ok(None)
    }
}
