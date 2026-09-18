use super::*;
use crate::checking::facts::{Node, NominalId};
use crate::syntax::modules::Visibility;

enum Selection {
    Function(usize),
    Field(Fact),
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
        let definition = &self.program.namespaces[module];
        self.ctx.work_bytes(definition.name.len())?;
        let declaration = self.program.declaration_names[&definition.name];
        let ty = self.facts.nominal(
            self.ctx,
            self.layouts.source_owner,
            declaration,
            definition.name.as_bytes(),
            None,
        )?;
        self.facts.type_value(self.ctx, ty)
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

    pub(super) fn declaration_pending(&self, index: usize) -> bool {
        matches!(&self.program.declarations[index].0, Kind::Namespace(namespace) if namespace.definition.body.is_some())
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

    fn namespace_field(&mut self, module: usize, name: &str) -> Result<Option<Fact>> {
        for (field, nested) in &self.program.namespaces[module].nested {
            self.ctx.work_bytes(field.len().max(name.len()))?;
            if field == name {
                return self.namespace_value(*nested).map(Some);
            }
        }
        Ok(None)
    }

    // Constants precede roots, except ordinary named calls prefer a declared function
    // or a namespace method with the same spelling. Computed identifier calls do not.
    pub(super) fn namespace_constant(&mut self, name: &str, named: bool) -> Result<Option<Fact>> {
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
        self.namespace_field(module, name)
    }

    fn namespace_selection(
        &mut self,
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
        if definition.body.is_some() {
            return Ok(Selection::Incomplete);
        }
        if scope {
            return Ok(self
                .namespace_field(module, name)?
                .map_or(Selection::Rejected, Selection::Field));
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
        if let Some(field) = self.namespace_field(module, name)? {
            return Ok(Selection::Field(field));
        }
        Ok(Selection::Rejected)
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
            match self.namespace_selection(receiver, &self.program.members[name], false, true)? {
                Selection::Function(function) => Target::Function(function),
                Selection::Field(value) => self.value_target(value)?,
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
        match self.namespace_selection(receiver, name, site.scope, false)? {
            Selection::Function(function) => {
                state.arguments.data.last_mut().unwrap().target = Target::Function(function);
                Ok(None)
            }
            Selection::Field(value) => self.set_call_target(state, pc, value),
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
        match self.namespace_selection(receiver, selected.as_str(), site.scope, implicit)? {
            Selection::Function(function) => {
                self.invoke(state, pc, Target::Function(function), args)
            }
            Selection::Field(value) => {
                if site.auto {
                    state.stack.push(self.ctx, Operand::new(value))?;
                    Ok(None)
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
