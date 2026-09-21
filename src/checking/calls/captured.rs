use super::*;
use crate::checking::{
    environment::Environment,
    facts::{InstanceKind, Node},
    globals::layout::{Captured, Definition},
    pending::Pending,
    slots::Slots,
};

fn report() -> Report {
    Report {
        returns: Atom::Never.fact(),
        normal_returns: Atom::Never.fact(),
        throws: 0,
        issues: Buffer::empty(),
        incomplete: Buffer::empty(),
        block_exits: Buffer::empty(),
    }
}

fn exit(globals: Globals, completion: blocks::Completion, value: Fact) -> blocks::Exit {
    blocks::Exit {
        pc: 0,
        completion,
        value,
        captures: Slots::new(0, Atom::Never.fact()),
        written: Slots::new(0, false),
        refined: Slots::new(0, false),
        pending: Pending::new(),
        globals,
    }
}

impl Solver<'_, '_> {
    /// Publishes immutable source and heap snapshots without executing any user code.
    pub(super) fn prepare_captures(
        &mut self,
        ctx: &mut CallContext,
        facts: &mut Facts,
    ) -> Result<()> {
        let root = self.root_handle(ctx)?;
        let receiving = root.view().source;
        let objects = self.state.storage.layout.captured_objects().len();
        for index in 0..self.state.values.captured.sources.data.len() {
            ctx.charge(1)?;
            let source = &self.state.values.captured.sources.data[index];
            let (id, owner) = (source.id, source.owner);
            if self.state.storage.layout.find(ctx, id)?.is_some() {
                continue;
            }
            let Some(code) = facts.source_code(ctx, id)? else {
                continue;
            };
            let Some(loader) = root.view().world.loader else {
                continue;
            };
            let environment = Environment::module(ctx, facts, &code, owner, loader)?;
            let handle = Handle::owned(ctx, facts, environment)?;
            let captured =
                self.capture_environment(ctx, facts, index, code.program.namespaces.len())?;
            self.state.worlds.insert(ctx, handle.clone())?;
            let roots = self.roots(ctx, facts)?;
            let view = handle.view();
            self.state.storage.prepare_captured(
                ctx,
                facts,
                Definition {
                    source: id,
                    owner,
                    program: view.world.program,
                    files: &view.layouts.files,
                    roots: &roots.data,
                    receiving: Some(receiving),
                },
                &captured,
            )?;
        }
        self.state
            .storage
            .capture_objects(ctx, facts, &self.state.values.captured.objects.data)?;
        if objects != self.state.storage.layout.captured_objects().len() {
            // A symbolic parameter may alias a newly read host object. Revisit earlier
            // writes with its slot present before retaining their call summaries.
            for index in 0..self.state.jobs.data.len() {
                ctx.charge(1)?;
                self.enqueue(ctx, index)?;
            }
        }
        Ok(())
    }

    fn capture_environment(
        &self,
        ctx: &mut CallContext,
        facts: &Facts,
        source: usize,
        count: usize,
    ) -> Result<Captured> {
        let files = self.state.values.captured.environment(ctx, source)?;
        let mut namespaces = Buffer::with_capacity(ctx, count)?;
        let mut initialized = Buffer::with_capacity(ctx, count)?;
        ctx.charge(count as u64)?;
        namespaces.data.resize(count, None);
        initialized.data.resize(count, false);
        if let Some(fields) = files {
            let Node::Shape(fields, ..) = facts.node(fields) else {
                unreachable!()
            };
            for field in &fields.data {
                let name = field.name.as_bytes().unwrap();
                ctx.work_bytes(name.len())?;
                if name.first() != Some(&0) || name.len() < 3 {
                    continue;
                }
                let index = std::str::from_utf8(&name[2..])
                    .ok()
                    .and_then(|index| index.parse::<usize>().ok());
                let Some(index) = index.filter(|&index| index < count) else {
                    continue;
                };
                match name[1] {
                    b'n' => {
                        let Node::Instance {
                            slot,
                            kind: InstanceKind::Captured,
                            ..
                        } = *facts.node(field.value)
                        else {
                            return ctx
                                .guard(crate::ErrorKind::Type, "invalid namespace environment");
                        };
                        namespaces.data[index] =
                            Some(self.state.values.captured.objects.data[slot].fields);
                    }
                    b'i' => {
                        initialized.data[index] =
                            matches!(facts.node(field.value), Node::Boolean(true))
                    }
                    _ => (),
                }
            }
        }
        Ok(Captured {
            files,
            namespaces,
            initialized,
        })
    }

    pub(super) fn activate_captures(
        &mut self,
        ctx: &mut CallContext,
        facts: &mut Facts,
        sources: &[SourceId],
        mut alternatives: Buffer<Globals>,
        current_error: u16,
        result: &mut Report,
    ) -> Result<Buffer<Globals>> {
        for &source in sources {
            ctx.charge(1)?;
            let Some(slot) = self.state.storage.layout.source(ctx, source)?.activation else {
                continue;
            };
            let mut normal = Buffer::empty();
            for globals in alternatives.data {
                let status = globals.values.data[slot];
                for index in 0..facts.arm_count(status) {
                    ctx.charge(1)?;
                    let arm = facts.arm(status, index);
                    let mut globals = globals.snapshot(ctx)?;
                    globals.values.data[slot] = arm;
                    match facts.node(arm) {
                        Node::Boolean(true) => globals.join_into(ctx, facts, &mut normal)?,
                        Node::Symbol(value) if value.as_bytes() == Some(b"loading") => {
                            globals.join_into(ctx, facts, &mut normal)?;
                        }
                        Node::Boolean(false) => {
                            result.throws |= 1 << crate::ErrorClass::Runtime as u8;
                            result.block_exits.push(
                                ctx,
                                exit(
                                    globals,
                                    blocks::Completion::Error(crate::ErrorClass::Runtime),
                                    Atom::Never.fact(),
                                ),
                            )?;
                        }
                        Node::Atom(Atom::Nil) => {
                            let loading = facts.symbol(ctx, b"loading")?;
                            globals.store(ctx, facts, slot, loading)?;
                            let mut pending = Buffer::empty();
                            pending.push(ctx, globals)?;
                            let (world, handle) = self.state.worlds.get(ctx, source)?;
                            let mut initialized = report();
                            let completed =
                                self.state.adapter(world, &handle).initialize_namespaces(
                                    ctx,
                                    facts,
                                    pending,
                                    current_error,
                                    &mut initialized,
                                )?;
                            result.throws |= initialized.throws;
                            result
                                .incomplete
                                .extend(ctx, &initialized.incomplete.data)?;
                            for mut exception in initialized.block_exits.data {
                                let failed = facts.boolean(ctx, false)?;
                                exception.globals.store(ctx, facts, slot, failed)?;
                                result.block_exits.push(ctx, exception)?;
                            }
                            for mut globals in completed.data {
                                let completed = facts.boolean(ctx, true)?;
                                globals.store(ctx, facts, slot, completed)?;
                                globals.join_into(ctx, facts, &mut normal)?;
                            }
                        }
                        _ => result.incomplete.push(ctx, 0)?,
                    }
                }
            }
            alternatives = normal;
            if alternatives.data.is_empty() {
                break;
            }
        }
        Ok(alternatives)
    }

    pub(super) fn materialize_root(
        &mut self,
        ctx: &mut CallContext,
        facts: &mut Facts,
        root: usize,
        current_error: u16,
        globals: &Globals,
    ) -> Result<Outcome> {
        let loaded = self.load_root(ctx, facts, root)?;
        let mut outcome = Outcome {
            value: loaded.value,
            throws: loaded.throws,
            incomplete: loaded.incomplete,
            ..Outcome::empty()
        };
        if loaded.incomplete || loaded.value == Atom::Never.fact() {
            return Ok(outcome);
        }
        let Some(batch) = loaded.captured else {
            return Ok(outcome);
        };
        let mut globals = globals.snapshot(ctx)?;
        globals.expand(ctx, &self.state.storage.layout)?;
        let mut pending = Buffer::empty();
        pending.push(ctx, globals)?;
        let mut sources = Buffer::empty();
        sources.extend(ctx, &self.state.values.captured.batches.data[batch].data)?;
        let mut report = report();
        let normal = self.activate_captures(
            ctx,
            facts,
            &sources.data,
            pending,
            current_error,
            &mut report,
        )?;
        outcome.incomplete |= !report.incomplete.data.is_empty();
        outcome.exits = report.block_exits;
        outcome.value = Atom::Never.fact();
        for globals in normal.data {
            outcome.value = loaded.value;
            outcome
                .exits
                .push(ctx, exit(globals, blocks::Completion::Value, loaded.value))?;
        }
        Ok(outcome)
    }
}
