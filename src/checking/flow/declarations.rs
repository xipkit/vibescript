use super::*;
use crate::checking::facts::{Field, Node};

impl Walker<'_> {
    pub(super) fn abstract_yield(
        &mut self,
        mut state: State,
        pc: usize,
        count: usize,
    ) -> Result<Edges> {
        let base = state.stack.data.len() - count;
        let mut args = Arguments::new();
        for operand in &state.stack.data[base..] {
            args.positional.push(self.ctx, operand.value)?;
        }
        state.stack.data.truncate(base);
        let mut failures = Buffer::empty();
        let admitted = args.admit(self.ctx, self.facts, &mut failures)?;
        for failure in failures.data {
            self.issue(
                pc,
                IssueKind::Call {
                    target: Target::Dynamic,
                    failure,
                },
            )?;
            self.emit_error(&state, pc, handlers::bit(ErrorClass::Runtime))?;
        }
        if !admitted {
            return Ok([None, None]);
        }
        self.unknown_call_effects(&mut state, pc)?;
        self.emit_error(&state, pc, u8::MAX)?;
        let escape = state.snapshot(self.ctx)?;
        self.callback_escape(escape, pc, Atom::Unknown.fact())?;
        let next = state.snapshot(self.ctx)?;
        let value = Atom::Unknown.fact();
        let transfer = if let Some(current) = next.loops.data.last() {
            Transfer::Jump {
                target: current.end,
                index: next.loops.data.len() - 1,
                breaking: true,
                value,
            }
        } else if self.block_inputs.is_some() {
            Transfer::Block {
                pc,
                completion: blocks::Completion::Break(true),
                value,
            }
        } else {
            Transfer::BlockBreak { pc, value }
        };
        let edges = self.transfer(next, pc, transfer)?;
        for edge in edges.into_iter().flatten() {
            self.extra.push(self.ctx, edge)?;
        }
        state
            .stack
            .push(self.ctx, Operand::new(Atom::Unknown.fact()))?;
        Ok([Some((pc + 1, state)), None])
    }

    pub(super) fn unknown_call_effects(&mut self, state: &mut State, pc: usize) -> Result<()> {
        // Unknown callees cannot rebind this frame's locals without its attached
        // block, but can reach shared globals and objects. A no-op remains possible.
        let layout = state.global_layout.clone();
        for source in layout.sources() {
            self.ctx.charge(1)?;
            for index in source
                .globals
                .data
                .iter()
                .copied()
                .chain(source.roots.data.iter().copied())
                .chain(source.files.clone())
            {
                self.ctx.charge(1)?;
                let slot = state.global_base + index;
                let before = state.locals.get(self.ctx, slot)?;
                let value = self
                    .facts
                    .union(self.ctx, &[before.value, Atom::Unknown.fact()])?;
                self.store(state, pc, slot, Operand::new(value))?;
                let after = state.locals.get(self.ctx, slot)?;
                state.locals.set(
                    self.ctx,
                    slot,
                    Binding {
                        missing: before.missing,
                        ..after
                    },
                )?;
            }
            for root in source
                .namespaces
                .clone()
                .step_by(crate::checking::namespaces::WIDTH)
            {
                self.ctx.charge(1)?;
                let slot = state.global_base + root;
                let fields = state.locals.get(self.ctx, slot)?.value;
                let value = self.unknown_fields(fields)?;
                self.store(state, pc, slot, Operand::new(value))?;
                let heap = state.locals.get(self.ctx, slot + 2)?.value;
                let mut alternatives = Buffer::empty();
                for i in 0..self.facts.arm_count(heap) {
                    self.ctx.charge(1)?;
                    let arm = self.facts.arm(heap, i);
                    let value = match self.facts.node(arm) {
                        Node::Tuple(entries) => {
                            let mut values = Buffer::empty();
                            values.extend(self.ctx, &entries.data)?;
                            for value in &mut values.data {
                                *value = self.unknown_fields(*value)?;
                            }
                            self.facts.tuple(self.ctx, &values.data)?
                        }
                        Node::Array(fields) => {
                            let fields = self.unknown_fields(*fields)?;
                            self.facts.array(self.ctx, fields)?
                        }
                        _ => arm,
                    };
                    alternatives.push(self.ctx, value)?;
                }
                let value = self.facts.union(self.ctx, &alternatives.data)?;
                self.store(state, pc, slot + 2, Operand::new(value))?;
            }
        }
        Ok(())
    }

    fn unknown_fields(&mut self, fields: Fact) -> Result<Fact> {
        let mut alternatives = Buffer::empty();
        alternatives.push(self.ctx, fields)?;
        for i in 0..self.facts.arm_count(fields) {
            self.ctx.charge(1)?;
            let arm = self.facts.arm(fields, i);
            let value = match self.facts.node(arm) {
                Node::Shape(fields, _, key, kind) => {
                    let (key, kind) = (*key, *kind);
                    let mut copied = Buffer::empty();
                    for field in &fields.data {
                        self.ctx.charge(1)?;
                        copied.push(
                            self.ctx,
                            Field {
                                name: field.name.clone(),
                                value: field.value,
                                optional: field.optional,
                            },
                        )?;
                    }
                    for field in &mut copied.data {
                        field.value = self
                            .facts
                            .union(self.ctx, &[field.value, Atom::Unknown.fact()])?;
                    }
                    self.facts.shape_fields(self.ctx, copied, true, key, kind)?
                }
                Node::Hash(key, value, kind) => {
                    let (key, value, kind) = (*key, *value, *kind);
                    let value = self.facts.union(self.ctx, &[value, Atom::Unknown.fact()])?;
                    self.facts.hash_kind(self.ctx, key, value, kind)?
                }
                _ => arm,
            };
            alternatives.push(self.ctx, value)?;
        }
        self.facts.union(self.ctx, &alternatives.data)
    }
}
