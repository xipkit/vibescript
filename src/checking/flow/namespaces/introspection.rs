use super::*;
use crate::members::introspection::{Predicate as Query, method_name};

impl Walker<'_> {
    pub(in super::super) fn namespace_helper(
        &mut self,
        state: &mut State,
        pc: usize,
        receiver: Fact,
        name: &'static str,
        implicit: bool,
        mut args: Arguments,
    ) -> Result<Option<Edges>> {
        let target = Target::Helper {
            receiver,
            name,
            implicit,
        };
        let mut admission = crate::checking::calls::Outcome::empty();
        let admitted = args.admit(self.ctx, self.facts, &mut admission.failures)?;
        self.call_effects(state, pc, target, &admission)?;
        if !admitted {
            return Ok(Some([None, None]));
        }
        let selected = self.facts.string(self.ctx, name.as_bytes())?;
        let site = MemberSite {
            call: CallSite {
                name: usize::MAX,
                method: None,
                auto: false,
                scope: false,
                parenthesized: false,
            },
            selected: Some(selected),
        };
        if matches!(name, "send" | "public_send") {
            state
                .addresses
                .push(self.ctx, Address::new(None, receiver))?;
            self.forwarded_member(state, pc, receiver, site, &args)?;
            return Ok(Some([None, None]));
        }
        if let Some(method) = collection_blocks::Method::parse(name) {
            self.collection_block(state, pc, receiver, site, args, method)?;
            return Ok(Some([None, None]));
        }
        let mut result = builtins::member(self.ctx, self.facts, receiver, site.call, name, &args)?
            .expect("known namespace predicate");
        if name == "respond_to?" && result.value != Atom::Never.fact() {
            result.value = self.namespace_responds(state, receiver, implicit, &args)?;
        }
        if name == "is_type?" {
            self.type_predicate_outcome(state, pc, receiver, &args, &mut result)?;
        }
        self.call_effects(state, pc, target, &result)?;
        if result.incomplete {
            return self.incomplete(pc).map(Some);
        }
        if result.value == Atom::Never.fact() {
            return Ok(Some([None, None]));
        }
        state.stack.push(self.ctx, Operand::new(result.value))?;
        Ok(None)
    }

    pub(in super::super) fn type_predicate_outcome(
        &mut self,
        state: &mut State,
        pc: usize,
        receiver: Fact,
        args: &Arguments,
        result: &mut crate::checking::calls::Outcome,
    ) -> Result<()> {
        use crate::checking::type_bindings::Resolution;
        use crate::members::introspection::Query as Parsed;
        if args.positional.data.len() != 1 || !args.keywords.data.is_empty() || args.block.is_some()
        {
            return Ok(());
        }
        let actual = args.positional.data[0];
        let mut values = Buffer::empty();
        result.incomplete = false;
        for i in 0..self.facts.arm_count(actual) {
            self.ctx.charge(1)?;
            let arm = self.facts.arm(actual, i);
            if arm == Atom::Never.fact() {
                continue;
            }
            let value = match self.facts.node(arm) {
                Node::String(value) | Node::Symbol(value) => value.clone(),
                _ => {
                    let expected = self
                        .facts
                        .union(self.ctx, &[Atom::String.fact(), Atom::Symbol.fact()])?;
                    if self.facts.relation(self.ctx, arm, expected)? != Relation::Rejected {
                        result.incomplete = true;
                    }
                    continue;
                }
            };
            let query = match Query::IsType.validate(
                self.ctx,
                std::slice::from_ref(&value),
                false,
                false,
            ) {
                Ok(Parsed::Type(query)) => query,
                Ok(_) => unreachable!(),
                Err(_) => {
                    self.ctx.checkpoint()?;
                    continue;
                }
            };
            let resolved = if query.nominal {
                match self.predicate_type(state, pc, query.name)? {
                    Some(Resolution::Known(value)) => Some(value),
                    Some(Resolution::Missing | Resolution::Ambiguous) => None,
                    Some(Resolution::Dynamic) => {
                        result.incomplete = true;
                        continue;
                    }
                    None => continue,
                    Some(Resolution::Pending(_)) => unreachable!(),
                }
            } else {
                None
            };
            let mut matched = None;
            if let Some(resolved) = resolved {
                let Node::Nominal { name, .. } = self.facts.node(resolved) else {
                    unreachable!()
                };
                let name = std::str::from_utf8(name.as_bytes().unwrap()).unwrap();
                self.ctx.work_bytes(name.len())?;
                let short = name
                    .rsplit("::")
                    .next()
                    .unwrap()
                    .rsplit('.')
                    .next()
                    .unwrap();
                if short == query.name.rsplit('.').next().unwrap() {
                    matched = Some(resolved);
                }
            }
            if query.nominal && matched.is_none() && query.name.contains('.') {
                result.throws |= handlers::bit(ErrorClass::Runtime);
                result
                    .failures
                    .push(self.ctx, Failure::BuiltinDomain(arm))?;
                continue;
            }
            for j in 0..self.facts.arm_count(receiver) {
                self.ctx.charge(1)?;
                let receiver = self.facts.arm(receiver, j);
                if receiver == Atom::Never.fact() {
                    continue;
                }
                let kind = builtins::native_receiver(self.facts, receiver);
                let native = kind.and_then(|kind| query.native_match(kind));
                let answer = if let Some(answer) = native {
                    Some(answer)
                } else if !query.nominal {
                    None
                } else if let Some(expected) = matched {
                    if receiver == Atom::Nil.fact() && value.as_bytes().unwrap().ends_with(b"?") {
                        Some(true)
                    } else {
                        self.nominal_receiver(receiver)
                            .map(|actual| actual == expected)
                    }
                } else if let Some(nominal) = self.nominal_receiver(receiver) {
                    if nominal == Atom::Never.fact() {
                        Some(false)
                    } else {
                        let Node::Nominal { name, .. } = self.facts.node(nominal) else {
                            unreachable!()
                        };
                        Some(crate::json::bytes_equal(
                            self.ctx,
                            query.name.as_bytes(),
                            name.as_bytes().unwrap(),
                        )?)
                    }
                } else {
                    None
                };
                let fact = if let Some(answer) = answer {
                    self.facts.boolean(self.ctx, answer)?
                } else {
                    Atom::Bool.fact()
                };
                values.push(self.ctx, fact)?;
            }
        }
        result.value = self.facts.union(self.ctx, &values.data)?;
        Ok(())
    }

    fn nominal_receiver(&self, receiver: Fact) -> Option<Fact> {
        match *self.facts.node(receiver) {
            Node::Instance { class, .. } => Some(class),
            Node::EnumMember { enumeration, .. } => {
                let Node::Enumeration { nominal, .. } = self.facts.node(enumeration) else {
                    unreachable!()
                };
                Some(*nominal)
            }
            Node::Nominal { .. } => Some(receiver),
            Node::Named(_) | Node::Atom(Atom::Unknown | Atom::Any) => None,
            _ => Some(Atom::Never.fact()),
        }
    }

    fn namespace_responds(
        &mut self,
        state: &State,
        receiver: Fact,
        implicit: bool,
        args: &Arguments,
    ) -> Result<Fact> {
        let Some(module) = self.namespace(state, receiver)? else {
            return Ok(Atom::Bool.fact());
        };
        let instance = matches!(self.facts.node(receiver), Node::Instance { .. });
        let private = if implicit {
            self.facts.boolean(self.ctx, true)?
        } else if let Some(&value) = args.positional.data.get(1) {
            value
        } else {
            self.facts.boolean(self.ctx, false)?
        };
        let actual = args.positional.data[0];
        let mut values = Buffer::empty();
        for i in 0..self.facts.arm_count(actual) {
            self.ctx.charge(1)?;
            let value = self.facts.arm(actual, i);
            if value == Atom::Never.fact() {
                continue;
            }
            let name = match self.facts.node(value) {
                Node::String(name) | Node::Symbol(name) => name.clone(),
                _ => {
                    let expected = self
                        .facts
                        .union(self.ctx, &[Atom::String.fact(), Atom::Symbol.fact()])?;
                    if self.facts.relation(self.ctx, value, expected)? != Relation::Rejected {
                        values.push(self.ctx, Atom::Bool.fact())?;
                    }
                    continue;
                }
            };
            let Some(name) = method_name(self.ctx, name.as_bytes().unwrap())? else {
                let value = self.facts.boolean(self.ctx, false)?;
                values.push(self.ctx, value)?;
                continue;
            };
            let definition = &module.program().namespaces[module.index];
            if instance && name == "class"
                || !instance && name == "new" && definition.constructor.is_some()
            {
                let value = self.facts.boolean(self.ctx, true)?;
                values.push(self.ctx, value)?;
                continue;
            }
            let methods = if instance {
                &definition.instance_methods
            } else {
                &definition.methods
            };
            let mut visibility = None;
            for method in methods {
                self.ctx.charge(1)?;
                self.ctx.work_bytes(name.len().max(method.name.len()))?;
                if method.name == name {
                    visibility = Some(method.visibility);
                    break;
                }
            }
            if let Some(visibility) = visibility {
                let value = if visibility == Visibility::Public {
                    self.facts.boolean(self.ctx, true)?
                } else {
                    let mut choices = Buffer::empty();
                    for i in 0..self.facts.arm_count(private) {
                        self.ctx.charge(1)?;
                        let arm = self.facts.arm(private, i);
                        if matches!(self.facts.node(arm), Node::Boolean(_)) {
                            choices.push(self.ctx, arm)?;
                        } else if self.facts.relation(self.ctx, arm, Atom::Bool.fact())?
                            != Relation::Rejected
                        {
                            choices.push(self.ctx, Atom::Bool.fact())?;
                        }
                    }
                    self.facts.union(self.ctx, &choices.data)?
                };
                values.push(self.ctx, value)?;
                continue;
            }
            let universal = crate::members::names::universal(name);
            if universal && matches!(name, "tap" | "yield_self") {
                let field = if instance {
                    self.instance_field(state, receiver, name)?
                } else {
                    self.namespace_fields(state, module.root, name)?
                };
                if field.incomplete {
                    values.push(self.ctx, Atom::Bool.fact())?;
                    continue;
                }
                if field.missing {
                    let value = self.facts.boolean(self.ctx, true)?;
                    values.push(self.ctx, value)?;
                }
                for i in 0..self.facts.arm_count(field.value) {
                    self.ctx.charge(1)?;
                    let arm = self.facts.arm(field.value, i);
                    if arm == Atom::Never.fact() {
                        continue;
                    }
                    let value = match self.facts.node(arm) {
                        Node::Builtin(_) | Node::Offset(_) | Node::Callable { .. } => {
                            self.facts.boolean(self.ctx, true)?
                        }
                        Node::Named(_) | Node::Atom(Atom::Unknown | Atom::Any) => Atom::Bool.fact(),
                        _ => self.facts.boolean(self.ctx, false)?,
                    };
                    values.push(self.ctx, value)?;
                }
            } else {
                let value = self.facts.boolean(self.ctx, universal)?;
                values.push(self.ctx, value)?;
            }
        }
        self.facts.union(self.ctx, &values.data)
    }
}
