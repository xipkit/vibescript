use super::*;
use crate::checking::facts::{Node, NominalId};
use crate::checking::namespaces::{self, Selected};
use crate::syntax::modules::Visibility;

mod state;

enum Selection {
    Function(usize),
    Field(Fact, bool),
    Rejected,
    Incomplete,
}

impl Walker<'_> {
    pub(super) fn namespace_receiver(&mut self, value: Fact) -> Result<bool> {
        for i in 0..self.facts.arm_count(value) {
            self.ctx.charge(1)?;
            let arm = self.facts.arm(value, i);
            if let Node::TypeValue(ty) = self.facts.node(arm) {
                if matches!(self.facts.node(*ty), Node::Nominal { symbols: None, .. }) {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }

    pub(super) fn member_variants(
        &mut self,
        receiver: Fact,
        name: &str,
    ) -> Result<Option<Buffer<Fact>>> {
        if self.facts.arm_count(receiver) > 1 && self.namespace_receiver(receiver)? {
            let mut variants = Buffer::empty();
            for i in 0..self.facts.arm_count(receiver) {
                self.ctx.charge(1)?;
                variants.push(self.ctx, self.facts.arm(receiver, i))?;
            }
            return Ok(Some(variants));
        }
        super::super::objects::variants(self.ctx, self.facts, receiver, name)
    }

    pub(super) fn namespace_value(&mut self, module: usize) -> Result<Fact> {
        namespaces::value(
            self.ctx,
            self.facts,
            self.program,
            self.layouts.source_owner,
            module,
        )
    }

    pub(super) fn declaration_value(&mut self, index: usize) -> Result<Fact> {
        match &self.program.declarations[index].0 {
            Kind::Namespace(namespace) => self.namespace_value(namespace.definition.index),
            Kind::Enum(_) => self
                .facts
                .enumeration(self.ctx, &self.program.declarations[index]),
            _ => unreachable!(),
        }
    }

    fn namespace_index(&self, receiver: Fact) -> Option<usize> {
        let Node::TypeValue(ty) = self.facts.node(receiver) else {
            return None;
        };
        let Node::Nominal {
            identity: NominalId::Binding(owner, index),
            ..
        } = *self.facts.node(*ty)
        else {
            return None;
        };
        if owner != self.layouts.source_owner {
            return None;
        }
        match &self.program.declarations.get(index)?.0 {
            Kind::Namespace(namespace) => Some(namespace.definition.index),
            _ => None,
        }
    }

    fn namespace_slot(&self, state: &State, module: usize) -> usize {
        state.global_base + namespaces::slot(state.global_count, self.program, module)
    }

    fn namespace_field(&mut self, state: &State, module: usize, name: &str) -> Result<Selected> {
        let fields = state
            .locals
            .get(self.ctx, self.namespace_slot(state, module))?
            .value;
        namespaces::field(self.ctx, self.facts, fields, name)
    }

    fn refine_namespace(
        &mut self,
        state: &mut State,
        module: usize,
        name: &str,
        present: bool,
    ) -> Result<()> {
        let slot = self.namespace_slot(state, module);
        let binding = state.locals.get(self.ctx, slot)?;
        let value = namespaces::refine(self.ctx, self.facts, binding.value, name, present)?;
        state
            .locals
            .set(self.ctx, slot, Binding { value, ..binding })
    }

    // Constants precede roots, except ordinary named calls prefer a declared function
    // or a namespace method with the same spelling. Computed identifier calls do not.
    pub(super) fn namespace_constant(
        &mut self,
        state: &State,
        name: &str,
        named: bool,
    ) -> Result<Option<Selected>> {
        let Some(module) = self.function.namespace else {
            return Ok(None);
        };
        if !name
            .chars()
            .next()
            .is_some_and(crate::syntax::unicode::upper)
        {
            return Ok(None);
        }
        if named {
            self.ctx.work_bytes(name.len())?;
            if self.program.names.contains_key(name) {
                return Ok(None);
            }
            for method in &self.program.namespaces[module].methods {
                self.ctx.work_bytes(method.name.len().max(name.len()))?;
                if method.name == name {
                    return Ok(None);
                }
            }
        }
        let field = self.namespace_field(state, module, name)?;
        Ok((field.incomplete || field.value != Atom::Never.fact()).then_some(field))
    }

    fn namespace_selection(
        &mut self,
        state: &State,
        receiver: Fact,
        name: &str,
        scope: bool,
        implicit: bool,
    ) -> Result<Selection> {
        self.ctx.charge(1)?;
        let Some(module) = self.namespace_index(receiver) else {
            return Ok(Selection::Incomplete);
        };
        let definition = &self.program.namespaces[module];
        if scope {
            return self.namespace_field_selection(state, module, name);
        }
        if name == "new" && definition.constructor.is_some() {
            return Ok(Selection::Incomplete);
        }
        for method in &definition.methods {
            self.ctx.work_bytes(method.name.len().max(name.len()))?;
            if method.name == name {
                let allowed = match method.visibility {
                    Visibility::Public => true,
                    Visibility::Private => implicit,
                    Visibility::Protected => implicit || self.function.namespace == Some(module),
                };
                return Ok(if allowed {
                    Selection::Function(method.function)
                } else {
                    Selection::Rejected
                });
            }
        }
        if crate::members::names::universal(name) {
            return Ok(Selection::Incomplete);
        }
        self.namespace_field_selection(state, module, name)
    }

    fn namespace_field_selection(
        &mut self,
        state: &State,
        module: usize,
        name: &str,
    ) -> Result<Selection> {
        let field = self.namespace_field(state, module, name)?;
        Ok(if field.incomplete {
            Selection::Incomplete
        } else if field.value == Atom::Never.fact() {
            Selection::Rejected
        } else {
            Selection::Field(field.value, field.missing)
        })
    }

    pub(super) fn implicit_namespace_target(
        &mut self,
        state: &State,
        pc: usize,
        name: usize,
    ) -> Result<Option<Target>> {
        let module = self.function.namespace.unwrap();
        let receiver = self.namespace_value(module)?;
        Ok(Some(
            match self.namespace_selection(
                state,
                receiver,
                &self.program.members[name],
                false,
                true,
            )? {
                Selection::Function(function) => Target::Function(function),
                Selection::Field(value, missing) => {
                    if missing {
                        self.namespace_error(state, pc, receiver, name, &Arguments::new())?;
                    }
                    self.value_target(value)?
                }
                Selection::Incomplete => Target::Unsupported,
                Selection::Rejected => {
                    self.namespace_error(state, pc, receiver, name, &Arguments::new())?;
                    return Ok(None);
                }
            },
        ))
    }

    fn namespace_error(
        &mut self,
        state: &State,
        pc: usize,
        receiver: Fact,
        name: usize,
        args: &Arguments,
    ) -> Result<()> {
        let arguments = self.facts.tuple(self.ctx, &args.positional.data)?;
        self.issue(
            pc,
            IssueKind::Member {
                name,
                receiver,
                arguments,
            },
        )?;
        self.emit_error(state, pc, handlers::bit(ErrorClass::Runtime))
    }

    pub(super) fn namespace_call_target(
        &mut self,
        state: &mut State,
        pc: usize,
        receiver: Fact,
        site: CallSite,
    ) -> Result<Option<Edges>> {
        let name = &self.program.members[site.name];
        match self.namespace_selection(state, receiver, name, site.scope, false)? {
            Selection::Function(function) => {
                state.arguments.data.last_mut().unwrap().target = Target::Function(function);
                Ok(None)
            }
            Selection::Field(value, missing) => {
                if missing {
                    self.namespace_error(state, pc, receiver, site.name, &Arguments::new())?;
                }
                self.set_call_target(state, pc, value)
            }
            Selection::Incomplete => self.incomplete(pc).map(Some),
            Selection::Rejected => {
                self.namespace_error(state, pc, receiver, site.name, &Arguments::new())?;
                Ok(Some([None, None]))
            }
        }
    }

    pub(super) fn namespace_member(
        &mut self,
        state: &mut State,
        pc: usize,
        receiver: Fact,
        site: MemberSite,
        args: Arguments,
        implicit: bool,
    ) -> Result<Option<Edges>> {
        let selected = site.text(self.program, self.facts);
        match self.namespace_selection(state, receiver, selected.as_str(), site.scope, implicit)? {
            Selection::Function(function) => {
                self.invoke(state, pc, Target::Function(function), args)
            }
            Selection::Field(value, missing) => {
                if missing {
                    self.namespace_error(state, pc, receiver, site.name, &args)?;
                }
                if site.auto {
                    if site.scope {
                        state.stack.push(self.ctx, Operand::new(value))?;
                        Ok(None)
                    } else if self.dynamic(value)? {
                        self.incomplete(pc).map(Some)
                    } else {
                        Ok((!self.read_value(state, pc, value, None)?).then_some([None, None]))
                    }
                } else {
                    let target = self.value_target(value)?;
                    self.invoke(state, pc, target, args)
                }
            }
            Selection::Incomplete => self.incomplete(pc).map(Some),
            Selection::Rejected => {
                self.namespace_error(state, pc, receiver, site.name, &args)?;
                Ok(Some([None, None]))
            }
        }
    }

    pub(super) fn namespace_address_call(
        &mut self,
        state: &mut State,
        pc: usize,
        site: MemberSite,
        args: Arguments,
        address_result: bool,
    ) -> Result<Option<Edges>> {
        let receiver = state.addresses.data.pop().unwrap().value;
        if !address_result {
            return self.member(state, pc, receiver, site, args);
        }
        let outer = self.native_results.replace(Buffer::empty());
        let result = self.member(state, pc, receiver, site, args);
        let results = std::mem::replace(&mut self.native_results, outer).unwrap();
        let edges = result?;
        for mut next in results.data {
            self.ctx.charge(1)?;
            let value = next.stack.data.pop().unwrap().value;
            next.addresses.push(self.ctx, Address::new(None, value))?;
            self.native_continue(pc, next)?;
        }
        if edges.is_none() {
            let value = state.stack.data.pop().unwrap().value;
            state.addresses.push(self.ctx, Address::new(None, value))?;
        }
        Ok(edges)
    }
}
