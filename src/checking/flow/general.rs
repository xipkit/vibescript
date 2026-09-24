use super::*;
use crate::checking::facts::{Field, InstanceKind, Node, NominalId};

pub(in crate::checking) struct Model<'a> {
    pub initial: Option<Fact>,
    pub program: &'a Program,
    pub layouts: &'a Layouts,
    pub contracts: &'a [Fact],
}

impl Model<'_> {
    /// Derives property domains from the declaring source and constructor summary.
    pub(in crate::checking) fn fields(
        self,
        ctx: &mut CallContext,
        facts: &mut Facts,
        module: usize,
        kind: InstanceKind,
    ) -> Result<Fact> {
        let Self {
            initial,
            program,
            layouts,
            contracts,
        } = self;
        let mut fields = Buffer::empty();
        if kind != InstanceKind::Concrete {
            for method in &program.namespaces[module].instance_methods {
                ctx.charge(1)?;
                let Some((name, _)) = &program.functions[method.function].accessor else {
                    continue;
                };
                let Some(ty) =
                    crate::checking::namespaces::property_type(ctx, program, module, name)?
                else {
                    continue;
                };
                let value = if layouts.named_annotation(ctx, ty)? {
                    Atom::Unknown.fact()
                } else {
                    let value = facts.value_domain(ctx, contracts[ty])?;
                    if let Some(initial) = initial {
                        let selected =
                            crate::checking::namespaces::field(ctx, facts, initial, name)?;
                        let mut unknown = selected.incomplete;
                        for i in 0..facts.arm_count(selected.value) {
                            ctx.charge(1)?;
                            unknown |= facts.arm(selected.value, i) == Atom::Unknown.fact();
                        }
                        if selected.missing {
                            facts.nullable(ctx, value)?
                        } else if unknown {
                            facts.union(ctx, &[value, Atom::Unknown.fact()])?
                        } else {
                            value
                        }
                    } else {
                        facts.nullable(ctx, value)?
                    }
                };
                let name = ctx.bytes(name.as_bytes())?;
                fields.push(
                    ctx,
                    Field {
                        name,
                        value,
                        optional: false,
                    },
                )?;
            }
        }
        facts.shape_fields(
            ctx,
            fields,
            kind != InstanceKind::Concrete,
            Atom::String.fact(),
            crate::checking::facts::HashKind::PLAIN,
        )
    }
}

pub(super) fn allocate(
    ctx: &mut CallContext,
    facts: &mut Facts,
    state: &mut State,
    root: usize,
    class: Fact,
    fields: Fact,
    kind: InstanceKind,
) -> Result<Option<Fact>> {
    let before = state.locals.get(ctx, root)?.value;
    let Some(mut heap) = crate::checking::heaps::entries(ctx, facts, before)? else {
        return Ok(None);
    };
    let slot = heap.data.len();
    heap.push(ctx, fields)?;
    let next = facts.tuple(ctx, &heap.data)?;
    state.store(ctx, facts, root, next)?;
    let value = facts.instance_kind(ctx, class, slot, kind)?;
    if kind != InstanceKind::Concrete {
        let before = state.locals.get(ctx, root + 1)?.value;
        let aliases = facts.union(ctx, &[before, value])?;
        state.store(ctx, facts, root + 1, aliases)?;
    }
    Ok(Some(value))
}

enum Task {
    Visit(Fact, bool),
    Array,
    Hash(crate::checking::facts::HashKind),
    Tuple(usize),
    Union(usize),
    Shape(Buffer<Field>, bool, Fact, crate::checking::facts::HashKind),
}

impl Walker<'_> {
    /// Propagates possible aliases between concrete host objects and declaration inputs.
    pub(super) fn alias_captures(
        &mut self,
        state: &mut State,
        pc: usize,
        address: &Address,
        updated: Fact,
    ) -> Result<bool> {
        let Some((receiver, key)) = address.object else {
            return Ok(true);
        };
        let Node::Instance { kind, class, .. } = *self.facts.node(receiver) else {
            unreachable!()
        };
        if matches!(kind, InstanceKind::Concrete | InstanceKind::Folded) {
            return Ok(true);
        }
        let Some((_, slot)) = self.instance_slot(state, receiver)? else {
            return Ok(true);
        };
        let index = self.facts.integer(self.ctx, slot as i64)?;
        let fields = crate::checking::heaps::read(self.ctx, self.facts, updated, index)?;
        let changed = self
            .facts
            .collection_index(self.ctx, fields.value, &[key])?;
        if fields.unsupported || changed.unsupported {
            self.incomplete(pc)?;
            return Ok(false);
        }
        let mut aliases = Buffer::empty();
        if kind == InstanceKind::Captured {
            if let Some(module) = self.namespace(state, receiver)? {
                let descriptors = state.locals.get(self.ctx, module.root + 3)?.value;
                for index in 0..self.facts.arm_count(descriptors) {
                    self.ctx.charge(1)?;
                    let descriptor = self.facts.arm(descriptors, index);
                    if let Node::Instance { slot, kind, .. } = *self.facts.node(descriptor) {
                        if !kind.concrete() {
                            aliases.push(self.ctx, (module.root + 2, slot))?;
                        }
                    }
                }
            }
        } else {
            for &(descriptor, root) in state.global_layout.captured_objects() {
                self.ctx.charge(1)?;
                if matches!(*self.facts.node(descriptor), Node::Instance { class: other, .. } if other == class)
                {
                    aliases.push(self.ctx, (state.global_base + root, 0))?;
                }
            }
        }
        for (root, slot) in aliases.data {
            let heap = state.locals.get(self.ctx, root)?.value;
            let index = self.facts.integer(self.ctx, slot as i64)?;
            let before = crate::checking::heaps::read(self.ctx, self.facts, heap, index)?;
            let after = self
                .facts
                .collection_write(self.ctx, before.value, key, changed.value)?;
            let fields = self
                .facts
                .union(self.ctx, &[before.value, after.receiver])?;
            let updated = crate::checking::heaps::write(self.ctx, self.facts, heap, index, fields)?;
            if before.unsupported || after.unsupported || updated.unsupported {
                self.incomplete(pc)?;
                return Ok(false);
            }
            self.store(state, pc, root, Operand::new(updated.receiver))?;
        }
        Ok(true)
    }

    pub(super) fn general_value(
        &mut self,
        state: &mut State,
        pc: usize,
        value: Fact,
    ) -> Result<Option<Fact>> {
        let mut tasks = Buffer::empty();
        let mut values = Buffer::empty();
        tasks.push(self.ctx, Task::Visit(value, false))?;
        while let Some(task) = tasks.data.pop() {
            self.ctx.charge(1)?;
            let value = match task {
                Task::Visit(value, summary) => match self.facts.node(value) {
                    Node::Nominal {
                        identity: NominalId::Binding(..),
                        symbols: None,
                        ..
                    } => {
                        let class = self.facts.type_value(self.ctx, value)?;
                        let Some(namespace) = self.namespace(state, class)? else {
                            self.incomplete(pc)?;
                            return Ok(None);
                        };
                        // A module has no instances, so no value satisfies its type.
                        if namespace.program().namespaces[namespace.index]
                            .constructor
                            .is_none()
                        {
                            values.push(self.ctx, Atom::Never.fact())?;
                            continue;
                        }
                        let kind = if summary {
                            InstanceKind::Summary
                        } else {
                            InstanceKind::Symbolic
                        };
                        let initial = if self.constructor {
                            // Constructor summaries cannot assume their own output
                            // when admitting an existing instance as an input.
                            matches!(self.scope, blocks::Scope::Declaration { .. })
                                .then_some(Atom::Unknown.fact())
                        } else {
                            let globals = state.globals(self.ctx)?;
                            self.calls.receiver_fields(
                                self.ctx,
                                self.facts,
                                namespace.source,
                                namespace.index,
                                &globals,
                            )?
                        };
                        if initial == Some(Atom::Never.fact()) {
                            return Ok(None);
                        }
                        let fields = if namespace.source == self.source {
                            Model {
                                initial,
                                program: self.program,
                                layouts: self.layouts,
                                contracts: self.contracts,
                            }
                            .fields(
                                self.ctx,
                                self.facts,
                                namespace.index,
                                kind,
                            )?
                        } else {
                            let Some(fields) = self.calls.instance_fields(
                                self.ctx,
                                self.facts,
                                namespace.source,
                                namespace.index,
                                initial,
                            )?
                            else {
                                self.incomplete(pc)?;
                                return Ok(None);
                            };
                            fields
                        };
                        let Some(value) = allocate(
                            self.ctx,
                            self.facts,
                            state,
                            namespace.root + 2,
                            value,
                            fields,
                            kind,
                        )?
                        else {
                            self.incomplete(pc)?;
                            return Ok(None);
                        };
                        value
                    }
                    Node::Array(element) => {
                        tasks.push(self.ctx, Task::Array)?;
                        tasks.push(self.ctx, Task::Visit(*element, true))?;
                        continue;
                    }
                    Node::Hash(key, value, kind) => {
                        tasks.push(self.ctx, Task::Hash(*kind))?;
                        tasks.push(self.ctx, Task::Visit(*value, true))?;
                        tasks.push(self.ctx, Task::Visit(*key, true))?;
                        continue;
                    }
                    Node::Tuple(elements) | Node::Union(elements) => {
                        tasks.push(
                            self.ctx,
                            if matches!(self.facts.node(value), Node::Tuple(_)) {
                                Task::Tuple(elements.data.len())
                            } else {
                                Task::Union(elements.data.len())
                            },
                        )?;
                        for &element in elements.data.iter().rev() {
                            tasks.push(self.ctx, Task::Visit(element, summary))?;
                        }
                        continue;
                    }
                    Node::Shape(fields, open, key, kind) => {
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
                        tasks.push(self.ctx, Task::Shape(copied, *open, *key, *kind))?;
                        for field in fields.data.iter().rev() {
                            tasks.push(self.ctx, Task::Visit(field.value, summary))?;
                        }
                        continue;
                    }
                    _ => value,
                },
                Task::Array => {
                    let element = values.data.pop().unwrap();
                    self.facts.array(self.ctx, element)?
                }
                Task::Hash(kind) => {
                    let value = values.data.pop().unwrap();
                    let key = values.data.pop().unwrap();
                    self.facts.hash_kind(self.ctx, key, value, kind)?
                }
                Task::Tuple(count) | Task::Union(count) => {
                    let start = values.data.len() - count;
                    let value = if matches!(task, Task::Tuple(_)) {
                        self.facts.tuple(self.ctx, &values.data[start..])?
                    } else {
                        self.facts.union(self.ctx, &values.data[start..])?
                    };
                    values.data.truncate(start);
                    value
                }
                Task::Shape(mut fields, open, key, kind) => {
                    let start = values.data.len() - fields.data.len();
                    for (field, &value) in fields.data.iter_mut().zip(&values.data[start..]) {
                        self.ctx.charge(1)?;
                        field.value = value;
                    }
                    values.data.truncate(start);
                    self.facts.shape_fields(self.ctx, fields, open, key, kind)?
                }
            };
            values.push(self.ctx, value)?;
        }
        assert_eq!(values.data.len(), 1);
        Ok(values.data.pop())
    }

    pub(super) fn alias_instances(
        &mut self,
        state: &State,
        pc: usize,
        address: &Address,
        updated: Fact,
    ) -> Result<Option<(Fact, Option<Address>)>> {
        let Some((receiver, key)) = address.object else {
            return Ok(Some((updated, None)));
        };
        let Node::Instance { slot, kind, .. } = *self.facts.node(receiver) else {
            unreachable!()
        };
        if kind.concrete() {
            return Ok(Some((updated, None)));
        }
        let root = address.root.unwrap();
        let aliases = state.locals.get(self.ctx, root + 1)?.value;
        if aliases == Atom::Never.fact() {
            return Ok(Some((updated, None)));
        }
        let mut descriptors = Buffer::empty();
        for i in 0..self.facts.arm_count(aliases) {
            self.ctx.charge(1)?;
            let descriptor = self.facts.arm(aliases, i);
            if !matches!(self.facts.node(descriptor), Node::Instance { .. }) {
                self.incomplete(pc)?;
                return Ok(None);
            }
            descriptors.push(self.ctx, descriptor)?;
        }
        let index = self.facts.integer(self.ctx, slot as i64)?;
        let selected = crate::checking::heaps::read(self.ctx, self.facts, updated, index)?;
        let changed = self
            .facts
            .collection_index(self.ctx, selected.value, &[key])?;
        if selected.unsupported || changed.unsupported {
            self.incomplete(pc)?;
            return Ok(None);
        }
        let Some(mut entries_copy) =
            crate::checking::heaps::entries(self.ctx, self.facts, updated)?
        else {
            let all =
                crate::checking::heaps::read(self.ctx, self.facts, updated, Atom::Int.fact())?;
            let changed = self
                .facts
                .collection_write(self.ctx, all.value, key, changed.value)?;
            if selected.unsupported || all.unsupported || changed.unsupported {
                self.incomplete(pc)?;
                return Ok(None);
            }
            let fields = self.facts.union(self.ctx, &[all.value, changed.receiver])?;
            let updated = self.facts.array(self.ctx, fields)?;
            return Ok(Some((updated, Some(address.aliased(self.ctx)?))));
        };
        let mut shared = false;
        for (other, fields) in entries_copy.data.iter_mut().enumerate() {
            self.ctx.charge(1)?;
            if other == slot {
                if kind == InstanceKind::Summary {
                    let index = self.facts.integer(self.ctx, slot as i64)?;
                    let before = state.locals.get(self.ctx, root)?.value;
                    let before = crate::checking::heaps::read(self.ctx, self.facts, before, index)?;
                    if before.unsupported {
                        self.incomplete(pc)?;
                        return Ok(None);
                    }
                    *fields = self.facts.union(self.ctx, &[*fields, before.value])?;
                    shared = true;
                }
                continue;
            }
            let mut possible = false;
            for &descriptor in &descriptors.data {
                self.ctx.charge(1)?;
                if matches!(self.facts.node(descriptor), Node::Instance { slot, .. } if *slot == other)
                {
                    possible = true;
                    break;
                }
            }
            if possible {
                let mutation =
                    self.facts
                        .collection_write(self.ctx, *fields, key, changed.value)?;
                if mutation.unsupported {
                    self.incomplete(pc)?;
                    return Ok(None);
                }
                *fields = self.facts.union(self.ctx, &[*fields, mutation.receiver])?;
                shared = true;
            }
        }
        if !shared {
            return Ok(Some((updated, None)));
        }
        let updated = self.facts.tuple(self.ctx, &entries_copy.data)?;
        Ok(Some((updated, Some(address.aliased(self.ctx)?))))
    }
}
