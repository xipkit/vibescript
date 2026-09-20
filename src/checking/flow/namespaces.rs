use super::*;
use crate::checking::facts::{Node, NominalId};
use crate::checking::namespaces::{self, Selected};
use crate::syntax::modules::Visibility;

mod instances;
mod introspection;
mod source;
mod state;
pub(super) use source::Namespace;

pub(super) enum Selection {
    Call(Target),
    Field(Fact, bool),
    Rejected,
    Failed,
    Incomplete,
}

impl Walker<'_> {
    pub(super) fn standard_nil_receiver(&mut self, state: &State, value: Fact) -> Result<bool> {
        for i in 0..self.facts.arm_count(value) {
            self.ctx.charge(1)?;
            let arm = self.facts.arm(value, i);
            if let Some(module) = self.namespace(state, arm)? {
                let definition = &module.program().namespaces[module.index];
                let methods = if matches!(self.facts.node(arm), Node::Instance { .. }) {
                    &definition.instance_methods
                } else {
                    &definition.methods
                };
                for method in methods {
                    self.ctx.work_bytes(method.name.len())?;
                    if method.name == "nil?" {
                        return Ok(false);
                    }
                }
            } else if !self.facts.known_nil_receiver(self.ctx, arm)? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    pub(super) fn namespace_receiver(&mut self, value: Fact) -> Result<bool> {
        for i in 0..self.facts.arm_count(value) {
            self.ctx.charge(1)?;
            let arm = self.facts.arm(value, i);
            if matches!(self.facts.node(arm), Node::Instance { .. }) {
                return Ok(true);
            }
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
        scope: bool,
    ) -> Result<Option<Buffer<Fact>>> {
        if self.facts.arm_count(receiver) > 1
            && (self.namespace_receiver(receiver)? || self.dynamic(receiver)?)
        {
            let mut variants = Buffer::empty();
            for i in 0..self.facts.arm_count(receiver) {
                self.ctx.charge(1)?;
                variants.push(self.ctx, self.facts.arm(receiver, i))?;
            }
            return Ok(Some(variants));
        }
        super::super::objects::variants(self.ctx, self.facts, receiver, name, scope)
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

    pub(super) fn declaration_slot(&self, state: &State, index: usize) -> usize {
        state.global_base + state.source_slots.declarations.start + index
    }

    pub(super) fn declaration_value(&mut self, state: &State, index: usize) -> Result<Fact> {
        let declared = state
            .locals
            .get(self.ctx, self.declaration_slot(state, index))?
            .value;
        let name = crate::checking::file_bindings::declaration_name(self.program, index);
        self.file_type_value(state, name, declared)
    }

    pub(super) fn load_declaration(
        &mut self,
        state: &mut State,
        pc: usize,
        index: usize,
    ) -> Result<Fact> {
        let value = self.declaration_value(state, index)?;
        if self.program.file {
            let name = crate::checking::file_bindings::declaration_name(self.program, index);
            let slot = self.file_slot(state, name)?.unwrap();
            if state.locals.get(self.ctx, slot)?.missing {
                self.store(state, pc, slot, Operand::new(value))?;
            }
        }
        Ok(value)
    }

    fn namespace_slot(&self, state: &State, module: usize) -> usize {
        state.global_base + state.source_slots.namespace(module)
    }

    fn namespace_field(&mut self, state: &State, module: usize, name: &str) -> Result<Selected> {
        self.namespace_fields(state, self.namespace_slot(state, module), name)
    }

    fn namespace_fields(&mut self, state: &State, root: usize, name: &str) -> Result<Selected> {
        let fields = state.locals.get(self.ctx, root)?.value;
        namespaces::field(self.ctx, self.facts, fields, name)
    }

    pub(super) fn refine_namespace(
        &mut self,
        state: &mut State,
        module: usize,
        name: &str,
        present: bool,
    ) -> Result<()> {
        self.refine_namespace_fields(state, self.namespace_slot(state, module), name, present)
    }

    fn refine_namespace_fields(
        &mut self,
        state: &mut State,
        slot: usize,
        name: &str,
        present: bool,
    ) -> Result<()> {
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
            let definition = &self.program.namespaces[module];
            let methods = if self.function.instance {
                &definition.instance_methods
            } else {
                &definition.methods
            };
            for method in methods {
                self.ctx.work_bytes(method.name.len().max(name.len()))?;
                if method.name == name {
                    return Ok(None);
                }
            }
        }
        let field = self.namespace_field(state, module, name)?;
        Ok((field.incomplete || field.value != Atom::Never.fact()).then_some(field))
    }

    pub(super) fn namespace_selection(
        &mut self,
        state: &State,
        receiver: Fact,
        name: &str,
        scope: bool,
        implicit: bool,
    ) -> Result<Selection> {
        self.ctx.charge(1)?;
        let Some(module) = self.namespace(state, receiver)? else {
            return Ok(Selection::Incomplete);
        };
        if self.namespace_failed(state, module.source)? {
            return Ok(Selection::Failed);
        }
        let definition = &module.program().namespaces[module.index];
        let instance = matches!(self.facts.node(receiver), Node::Instance { .. });
        if scope {
            return if instance {
                Ok(Selection::Rejected)
            } else {
                self.namespace_field_selection(state, module.root, name)
            };
        }
        if instance && name == "class" {
            return Ok(Selection::Field(module.value(self.ctx, self.facts)?, false));
        }
        if !instance && name == "new" {
            if let Some((function, _)) = definition.constructor {
                return Ok(Selection::Call(Target::Method {
                    function: module.source.callable(function),
                    receiver,
                    constructor: true,
                }));
            }
        }
        let methods = if instance {
            &definition.instance_methods
        } else {
            &definition.methods
        };
        for method in methods {
            self.ctx.work_bytes(method.name.len().max(name.len()))?;
            if method.name == name {
                let allowed = match method.visibility {
                    Visibility::Public => true,
                    Visibility::Private => implicit,
                    Visibility::Protected => {
                        implicit
                            || (module.local(self.source, self.function.namespace)
                                && self.function.instance == instance)
                    }
                };
                return Ok(if allowed {
                    Selection::Call(if instance {
                        Target::Method {
                            function: module.source.callable(method.function),
                            receiver,
                            constructor: false,
                        }
                    } else {
                        Target::Function(module.source.callable(method.function))
                    })
                } else {
                    Selection::Rejected
                });
            }
        }
        let helper = match name {
            "nil?" => Some("nil?"),
            "itself" => Some("itself"),
            "dup" => Some("dup"),
            "clone" => Some("clone"),
            "freeze" => Some("freeze"),
            "frozen?" => Some("frozen?"),
            "eql?" => Some("eql?"),
            "equal?" => Some("equal?"),
            "send" => Some("send"),
            "public_send" => Some("public_send"),
            _ => crate::members::introspection::Predicate::parse(name).map(|p| p.name()),
        };
        if let Some(name) = helper {
            return Ok(Selection::Call(Target::Helper {
                receiver,
                name,
                implicit,
            }));
        }
        if matches!(name, "tap" | "yield_self") {
            let field = if instance {
                self.instance_field(state, receiver, name)?
            } else {
                self.namespace_fields(state, module.root, name)?
            };
            return Ok(
                if field.incomplete || field.missing && field.value != Atom::Never.fact() {
                    Selection::Incomplete
                } else if field.value != Atom::Never.fact() {
                    Selection::Field(field.value, false)
                } else {
                    Selection::Call(Target::Helper {
                        receiver,
                        name: if name == "tap" { "tap" } else { "yield_self" },
                        implicit,
                    })
                },
            );
        }
        if instance {
            let field = self.instance_field(state, receiver, name)?;
            Ok(if field.incomplete {
                Selection::Incomplete
            } else if field.value == Atom::Never.fact() {
                Selection::Rejected
            } else {
                Selection::Field(field.value, field.missing)
            })
        } else {
            self.namespace_field_selection(state, module.root, name)
        }
    }

    fn namespace_field_selection(
        &mut self,
        state: &State,
        root: usize,
        name: &str,
    ) -> Result<Selection> {
        let field = self.namespace_fields(state, root, name)?;
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
        let receiver = self.self_value(module)?;
        Ok(Some(
            match self.namespace_selection(
                state,
                receiver,
                &self.program.members[name],
                false,
                true,
            )? {
                Selection::Call(target) => target,
                Selection::Field(value, missing) => {
                    if missing {
                        self.namespace_error(state, pc, receiver, name, &Arguments::new())?;
                    }
                    self.value_target(value)?
                }
                Selection::Incomplete => Target::Unsupported,
                Selection::Failed => {
                    self.emit_error(state, pc, handlers::bit(ErrorClass::Runtime))?;
                    return Ok(None);
                }
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
            Selection::Call(target) => {
                let pending = state.arguments.data.last_mut().unwrap();
                pending.target = target;
                if matches!(
                    target,
                    Target::Function(_)
                        | Target::Method {
                            constructor: false,
                            ..
                        }
                ) {
                    pending.arguments.options_hash = !site.parenthesized;
                }
                Ok(None)
            }
            Selection::Field(value, missing) => {
                if missing {
                    self.namespace_error(state, pc, receiver, site.name, &Arguments::new())?;
                }
                self.set_call_target(state, pc, value)
            }
            Selection::Incomplete => self.incomplete(pc).map(Some),
            Selection::Failed => {
                self.emit_error(state, pc, handlers::bit(ErrorClass::Runtime))?;
                Ok(Some([None, None]))
            }
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
        mut args: Arguments,
        implicit: bool,
    ) -> Result<Option<Edges>> {
        let selected = site.text(self.program, self.facts);
        match self.namespace_selection(state, receiver, selected.as_str(), site.scope, implicit)? {
            Selection::Call(target) => {
                if matches!(
                    target,
                    Target::Function(_)
                        | Target::Method {
                            constructor: false,
                            ..
                        }
                ) {
                    args.options_hash = !site.parenthesized;
                }
                self.invoke(state, pc, target, args)
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
            Selection::Failed => {
                self.emit_error(state, pc, handlers::bit(ErrorClass::Runtime))?;
                Ok(Some([None, None]))
            }
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
