use super::*;
use crate::checking::facts::Node;
use crate::members::names::{self, Receiver};

#[derive(Clone, Copy)]
enum Resolution {
    Native,
    Call(Target),
    Field(Fact),
    /// A field that may be present with this value.
    Possible(Fact),
    Property,
    Missing,
    /// The member may be absent, which fails without proving a contradiction.
    Absent,
    Failed,
    Incomplete,
}

struct Forward {
    receiver: Fact,
    site: MemberSite,
    consumed: usize,
    implicit: bool,
}

impl Walker<'_> {
    fn native_receiver(&self, value: Fact) -> Option<Receiver> {
        builtins::native_receiver(self.facts, value)
    }

    fn forward_lookup(
        &mut self,
        state: &State,
        receiver: Fact,
        bytes: &[u8],
        name: Option<&str>,
        implicit: bool,
    ) -> Result<Buffer<Resolution>> {
        let mut output = Buffer::empty();
        if crate::checking::objects::may_be_protected(self.ctx, self.facts, receiver)? {
            output.push(self.ctx, Resolution::Incomplete)?;
            return Ok(output);
        }
        if self.namespace_receiver(receiver)? {
            use crate::checking::flow::namespaces::Selection;
            let selected = if let Some(name) = name {
                self.namespace_selection(state, receiver, name, false, implicit)?
            } else {
                Selection::Rejected
            };
            let resolution = match selected {
                Selection::Call(Target::Helper {
                    name: "send" | "public_send",
                    ..
                }) => Resolution::Native,
                Selection::Call(target) => Resolution::Call(target),
                Selection::Field(value, missing) => {
                    if missing {
                        output.push(self.ctx, Resolution::Missing)?;
                    }
                    Resolution::Field(value)
                }
                Selection::Rejected => Resolution::Missing,
                Selection::Failed => Resolution::Failed,
                Selection::Incomplete => Resolution::Incomplete,
            };
            output.push(self.ctx, resolution)?;
            return Ok(output);
        }
        let Some(kind) = self.native_receiver(receiver) else {
            output.push(self.ctx, Resolution::Incomplete)?;
            return Ok(output);
        };
        let universal = name.is_some_and(names::universal);
        let data_safe = universal && !matches!(name, Some("tap" | "yield_self"));
        let view = if let Node::Protected(shape, _) = self.facts.node(receiver) {
            *shape
        } else {
            receiver
        };
        let hash = matches!(self.facts.node(view), Node::Shape(..) | Node::Hash(..));
        let plain = view != receiver || self.facts.plain_hash(view);
        if hash && plain && (data_safe || name.is_some_and(|n| kind.typed(n).is_some())) {
            output.push(self.ctx, Resolution::Native)?;
            return Ok(output);
        }
        // Presence of the field: known, impossible, or uncertain.
        let mut absent = Some(true);
        if hash {
            let stored = match self.facts.node(view) {
                Node::Shape(_, open, _, _) => {
                    let open = *open;
                    match self.facts.selected_field(self.ctx, view, bytes)? {
                        Some((value, optional)) => {
                            absent = if optional { None } else { Some(false) };
                            Some(value)
                        }
                        None if open => {
                            absent = None;
                            Some(Atom::Unknown.fact())
                        }
                        None => None,
                    }
                }
                Node::Hash(_, value, _) => {
                    absent = None;
                    Some(*value)
                }
                _ => unreachable!(),
            };
            if let Some(value) = stored {
                for i in 0..self.facts.arm_count(value) {
                    self.ctx.charge(1)?;
                    let arm = self.facts.arm(value, i);
                    if arm == Atom::Never.fact() {
                        continue;
                    }
                    let callable = matches!(
                        self.facts.node(arm),
                        Node::Builtin(_) | Node::Offset(_) | Node::Callable { .. }
                    );
                    if !data_safe || callable {
                        output.push(
                            self.ctx,
                            if absent == Some(false) {
                                Resolution::Field(arm)
                            } else {
                                Resolution::Possible(arm)
                            },
                        )?;
                    } else if self.dynamic(arm)? {
                        // An unknown stored value may be a callable override of the
                        // helper or data that leaves the native helper in place.
                        output.push(self.ctx, Resolution::Possible(arm))?;
                        absent = None;
                    } else {
                        output.push(self.ctx, Resolution::Native)?;
                    }
                }
            }
        }
        if absent != Some(false) {
            let resolution = if let Some(name) = name {
                if !hash && kind.property(name) {
                    Resolution::Property
                } else if universal || kind.available(name) {
                    Resolution::Native
                } else if absent.is_none() {
                    Resolution::Absent
                } else {
                    Resolution::Missing
                }
            } else {
                Resolution::Missing
            };
            output.push(self.ctx, resolution)?;
        }
        Ok(output)
    }

    fn forward_flags(
        &mut self,
        state: &State,
        pc: usize,
        receiver: Fact,
        site: MemberSite,
        name: &str,
        args: &mut Arguments,
    ) -> Result<bool> {
        if crate::iteration::method(name)
            || builtins::primitive_member(self.ctx, self.facts, receiver, name)?
        {
            return Ok(true);
        }
        let kind = self.native_receiver(receiver).unwrap();
        let universal = names::universal(name);
        let block_rejected = universal
            || match kind {
                Receiver::Enum | Receiver::EnumMember => {
                    matches!(name, "to_s" | "string" | "inspect")
                }
                Receiver::Time | Receiver::Zoned => {
                    matches!(name, "between?" | "to_s" | "string" | "inspect")
                }
                Receiver::Duration => !matches!(
                    name,
                    "after" | "since" | "from_now" | "ago" | "before" | "until"
                ),
                Receiver::Money => name != "format",
                Receiver::Regex => name == "inspect",
                Receiver::Bytes => matches!(
                    name,
                    "inspect"
                        | "clamp"
                        | "between?"
                        | "to_sym"
                        | "intern"
                        | "to_s"
                        | "string"
                        | "to_i"
                        | "to_f"
                        | "delete"
                        | "delete!"
                        | "tr"
                        | "tr!"
                        | "squeeze"
                        | "squeeze!"
                ),
                Receiver::Int | Receiver::Big | Receiver::Float => matches!(
                    name,
                    "inspect" | "clamp" | "between?" | "to_s" | "string" | "to_i" | "to_f"
                ),
                _ => name == "inspect",
            };
        let keywords = !args.keywords.data.is_empty();
        let keyword_rejected = block_rejected
            || kind.rejects_keywords(site.method)
            || matches!(kind, Receiver::Array) && matches!(name, "union" | "difference")
            || matches!(kind, Receiver::Bytes)
                && matches!(
                    name,
                    "center" | "ljust" | "rjust" | "partition" | "rpartition"
                );
        if (args.block.is_some() && block_rejected) || (keywords && keyword_rejected) {
            self.collection_error(state, pc, receiver, site, args, ErrorClass::Runtime)?;
            return Ok(false);
        }
        args.block = None;
        if !builtins::value_member(self.ctx, self.facts, receiver, name)?
            || matches!(kind, Receiver::Regex) && matches!(name, "source" | "flags")
        {
            args.keywords.data.clear();
        }
        Ok(true)
    }

    fn forward_arguments(&mut self, args: &Arguments, consumed: usize) -> Result<Arguments> {
        let mut forwarded = Arguments::new();
        forwarded
            .positional
            .extend(self.ctx, &args.positional.data[consumed..])?;
        forwarded.keywords.extend(self.ctx, &args.keywords.data)?;
        forwarded.block = args
            .block
            .as_ref()
            .map(|block| block.snapshot(self.ctx))
            .transpose()?;
        Ok(forwarded)
    }

    pub(in crate::checking::flow) fn forwarded_member(
        &mut self,
        state: &State,
        pc: usize,
        receiver: Fact,
        site: MemberSite,
        args: &Arguments,
    ) -> Result<()> {
        let mut pending = Buffer::empty();
        for i in 0..self.facts.arm_count(receiver) {
            self.ctx.charge(1)?;
            let receiver = self.facts.arm(receiver, i);
            if receiver != Atom::Never.fact() {
                pending.push(
                    self.ctx,
                    Forward {
                        receiver,
                        site,
                        consumed: 0,
                        implicit: false,
                    },
                )?;
            }
        }
        while let Some(call) = pending.data.pop() {
            self.ctx.charge(1)?;
            if matches!(
                self.facts.atom(call.receiver),
                Some(Atom::Unknown | Atom::Any)
            ) {
                let mut next = state.snapshot(self.ctx)?;
                next.addresses.data.last_mut().unwrap().value = call.receiver;
                let args = self.forward_arguments(args, call.consumed)?;
                let edges = self.dynamic_mutation(&mut next, pc, call.site, args, false)?;
                self.member_edges(pc, next, edges)?;
                continue;
            }
            let selected = call.site.text(self.program, self.facts);
            let bytes = selected.as_bytes();
            let name = crate::members::introspection::method_name(self.ctx, bytes)?;
            if let Some(name) = name {
                if let Some(variants) = crate::checking::objects::variants(
                    self.ctx,
                    self.facts,
                    call.receiver,
                    name,
                    call.site.scope,
                )? {
                    for receiver in variants.data {
                        pending.push(self.ctx, Forward { receiver, ..call })?;
                    }
                    continue;
                }
            }
            let resolutions =
                self.forward_lookup(state, call.receiver, bytes, name, call.implicit)?;
            for resolution in resolutions.data {
                self.ctx.charge(1)?;
                if matches!(resolution, Resolution::Native)
                    && name.is_some_and(crate::members::forwarding::supported)
                {
                    let Some(&operation) = args.positional.data.get(call.consumed) else {
                        self.collection_error(
                            state,
                            pc,
                            call.receiver,
                            call.site,
                            args,
                            ErrorClass::Runtime,
                        )?;
                        continue;
                    };
                    let expected = self
                        .facts
                        .union(self.ctx, &[Atom::String.fact(), Atom::Symbol.fact()])?;
                    for i in 0..self.facts.arm_count(operation) {
                        self.ctx.charge(1)?;
                        let operation = self.facts.arm(operation, i);
                        if operation == Atom::Never.fact()
                            || !self.collection_parameter(
                                state,
                                pc,
                                call.receiver,
                                call.site,
                                args,
                                (operation, expected),
                            )?
                        {
                            continue;
                        }
                        let value = match self.facts.node(operation) {
                            Node::String(value) | Node::Symbol(value) => value.clone(),
                            _ => {
                                self.incomplete(pc)?;
                                continue;
                            }
                        };
                        let text = crate::members::introspection::method_name(
                            self.ctx,
                            value.as_bytes().unwrap(),
                        )?;
                        let site = MemberSite {
                            call: CallSite {
                                method: text.and_then(crate::bytecode::Method::parse),
                                auto: false,
                                parenthesized: false,
                                scope: false,
                                ..call.site.call
                            },
                            selected: Some(operation),
                        };
                        pending.push(
                            self.ctx,
                            Forward {
                                receiver: call.receiver,
                                site,
                                consumed: call.consumed + 1,
                                implicit: name == Some("send"),
                            },
                        )?;
                    }
                    continue;
                }
                let mut next = state.snapshot(self.ctx)?;
                let address = next.addresses.data.last_mut().unwrap();
                if address.value == receiver {
                    address.value = call.receiver;
                }
                let mut args = self.forward_arguments(args, call.consumed)?;
                let edges = match resolution {
                    Resolution::Call(target) => {
                        next.addresses.data.pop().unwrap();
                        if matches!(
                            target,
                            Target::Function(_)
                                | Target::Method {
                                    constructor: false,
                                    ..
                                }
                        ) {
                            args.options_hash = !call.site.parenthesized;
                        }
                        self.invoke(&mut next, pc, target, args)?
                    }
                    Resolution::Incomplete => {
                        self.incomplete(pc)?;
                        continue;
                    }
                    Resolution::Failed => {
                        self.emit_error(&next, pc, handlers::bit(ErrorClass::Runtime))?;
                        continue;
                    }
                    Resolution::Missing => {
                        self.collection_error(
                            &next,
                            pc,
                            call.receiver,
                            call.site,
                            &args,
                            ErrorClass::Runtime,
                        )?;
                        continue;
                    }
                    Resolution::Absent => {
                        self.emit_error(&next, pc, handlers::bit(ErrorClass::Runtime))?;
                        continue;
                    }
                    Resolution::Field(field) => {
                        next.addresses.data.pop().unwrap();
                        let target = self.value_target(field)?;
                        self.invoke(&mut next, pc, target, args)?
                    }
                    Resolution::Possible(field) => {
                        next.addresses.data.pop().unwrap();
                        let target = self.value_target(field)?;
                        if matches!(target, Target::NonCallable)
                            && name.is_some_and(|name| {
                                crate::checking::objects::absent_is_native(call.site.call, name)
                            })
                        {
                            self.emit_error(&next, pc, handlers::bit(ErrorClass::Runtime))?;
                            continue;
                        }
                        self.invoke(&mut next, pc, target, args)?
                    }
                    Resolution::Property => {
                        next.addresses.data.pop().unwrap();
                        let property = MemberSite {
                            call: CallSite {
                                auto: true,
                                ..call.site.call
                            },
                            ..call.site
                        };
                        if let Some(edges) = self.member_without_collection(
                            &mut next,
                            pc,
                            call.receiver,
                            property,
                            &Arguments::new(),
                        )? {
                            self.member_edges(pc, next, Some(edges))?;
                            continue;
                        }
                        let value = next.stack.data.pop().unwrap().value;
                        let target = self.value_target(value)?;
                        self.invoke(&mut next, pc, target, args)?
                    }
                    Resolution::Native => {
                        let name = name.unwrap();
                        if !self.forward_flags(
                            &next,
                            pc,
                            call.receiver,
                            call.site,
                            name,
                            &mut args,
                        )? {
                            continue;
                        }
                        if crate::bytecode::mutating_member(name) {
                            if args.block.is_some() {
                                self.mutable_block(&next, pc, call.site, &args)?;
                                continue;
                            }
                            if !args.keywords.data.is_empty() {
                                self.collection_error(
                                    &next,
                                    pc,
                                    call.receiver,
                                    call.site,
                                    &args,
                                    ErrorClass::Runtime,
                                )?;
                                continue;
                            }
                            self.mutate(
                                &mut next,
                                pc,
                                call.site,
                                &args.positional.data,
                                false,
                                false,
                            )?
                        } else {
                            next.addresses.data.pop().unwrap();
                            self.member(&mut next, pc, call.receiver, call.site, args)?
                        }
                    }
                };
                self.member_edges(pc, next, edges)?;
            }
        }
        Ok(())
    }
}
