use super::*;
use crate::checking::facts::Node;

mod forwarding;
mod reduction;
pub(super) use reduction::NativeFrame;
mod site;
pub(super) use site::MemberSite;

impl Walker<'_> {
    pub(super) fn native_continue(&mut self, pc: usize, state: State) -> Result<()> {
        if let Some(results) = &mut self.native_results {
            results.push(self.ctx, state)
        } else {
            self.extra.push(self.ctx, (pc + 1, state))
        }
    }

    pub(super) fn member_edges(
        &mut self,
        pc: usize,
        state: State,
        edges: Option<Edges>,
    ) -> Result<()> {
        if let Some(edges) = edges {
            for edge in edges.into_iter().flatten() {
                self.extra.push(self.ctx, edge)?;
            }
        } else {
            self.native_continue(pc, state)?;
        }
        Ok(())
    }

    pub(super) fn type_shadowed(&mut self, state: &State, guard: usize) -> Result<(bool, bool)> {
        let mut possible = false;
        for name in &self.program.type_guards[guard] {
            self.ctx.work_bytes(name.len())?;
            for (slot, candidate) in self.function.local_names.iter().enumerate() {
                self.ctx.charge(1)?;
                if crate::enums::compare_names(self.ctx, candidate.as_bytes(), name.as_bytes())?
                    == std::cmp::Ordering::Equal
                {
                    let binding = state.locals.get(self.ctx, slot)?;
                    if !binding.missing {
                        return Ok((true, true));
                    }
                    possible |= binding.value != Atom::Never.fact();
                }
            }
            if crate::builtin::Global::parse(name).is_some()
                || self.program.names.contains_key(name)
                || self.program.declaration_names.contains_key(name)
                || self.calls.global(self.ctx, name)?
            {
                return Ok((true, true));
            }
            if let Some((_, binding)) = self.ambient_binding(state, name)? {
                if !binding.missing {
                    return Ok((true, true));
                }
                possible = true;
            }
            for host in &self.program.hosts {
                if crate::enums::compare_names(self.ctx, host.as_bytes(), name.as_bytes())?
                    == std::cmp::Ordering::Equal
                {
                    return Ok((true, true));
                }
            }
        }
        Ok((false, possible))
    }

    pub(super) fn member(
        &mut self,
        state: &mut State,
        pc: usize,
        receiver: Fact,
        site: impl Into<MemberSite>,
        args: Arguments,
    ) -> Result<Option<Edges>> {
        let site = site.into();
        let selected = site.text(self.program, self.facts);
        let name = selected.as_str();
        if let Some(variants) = self.member_variants(receiver, name, site.scope)? {
            for receiver in variants.data {
                self.ctx.charge(1)?;
                let mut next = state.snapshot(self.ctx)?;
                let arguments = args.snapshot(self.ctx)?;
                let edges = self.member(&mut next, pc, receiver, site, arguments)?;
                self.member_edges(pc, next, edges)?;
            }
            return Ok(Some([None, None]));
        }
        if matches!(self.facts.atom(receiver), Some(Atom::Unknown | Atom::Any)) {
            return self.dynamic_call(state, pc, args);
        }
        if self.namespace_receiver(receiver)? {
            return self.namespace_member(state, pc, receiver, site, args, false);
        }
        use crate::checking::objects::Selection;
        match crate::checking::objects::select(self.ctx, self.facts, receiver, site.call, name)? {
            Some(Selection::Field(field)) => {
                return self.member_field(state, pc, field, site, args, true);
            }
            Some(Selection::Missing) => {
                self.collection_error(state, pc, receiver, site, &args, ErrorClass::Runtime)?;
                return Ok(Some([None, None]));
            }
            Some(Selection::Uncertain(field)) => {
                let mut present = state.snapshot(self.ctx)?;
                let arguments = args.snapshot(self.ctx)?;
                let edges = self.member_field(&mut present, pc, field, site, arguments, false)?;
                self.member_edges(pc, present, edges)?;
                if !crate::checking::objects::absent_is_native(site.call, name) {
                    self.emit_error(state, pc, handlers::bit(ErrorClass::Runtime))?;
                    return Ok(Some([None, None]));
                }
            }
            Some(Selection::Native) | None => (),
        }
        self.member_native(state, pc, receiver, site, args)
    }

    /// Reads or invokes a stored object field selected for a member site.
    ///
    /// An uncertain non-callable field can still admit a native fallback. With
    /// no such fallback, both the present and absent paths fail.
    pub(super) fn member_field(
        &mut self,
        state: &mut State,
        pc: usize,
        field: Fact,
        site: MemberSite,
        args: Arguments,
        certain: bool,
    ) -> Result<Option<Edges>> {
        if site.auto {
            if site.scope {
                state.stack.push(self.ctx, Operand::new(field))?;
                return Ok(None);
            }
            if self.dynamic(field)? {
                return self.dynamic_call(state, pc, Arguments::new());
            }
            return Ok(if self.read_value(state, pc, field, None)? {
                None
            } else {
                Some([None, None])
            });
        }
        let target = self.value_target(field)?;
        if !certain
            && matches!(target, Target::NonCallable)
            && crate::checking::objects::absent_is_native(
                site.call,
                site.text(self.program, self.facts).as_str(),
            )
        {
            self.emit_error(state, pc, handlers::bit(ErrorClass::Runtime))?;
            return Ok(Some([None, None]));
        }
        self.invoke(state, pc, target, args)
    }

    /// Dispatches a member natively after field overrides have been resolved.
    pub(super) fn member_native(
        &mut self,
        state: &mut State,
        pc: usize,
        receiver: Fact,
        site: MemberSite,
        args: Arguments,
    ) -> Result<Option<Edges>> {
        let selected = site.text(self.program, self.facts);
        let name = selected.as_str();
        if self.rejects_block(receiver, site, &args, name)? {
            self.collection_error(state, pc, receiver, site, &args, ErrorClass::Runtime)?;
            return Ok(Some([None, None]));
        }
        if let Some(method) = collection_blocks::text::TextMethod::parse(name) {
            if method.materializes() {
                let mut strings = true;
                for i in 0..self.facts.arm_count(receiver) {
                    self.ctx.charge(1)?;
                    strings &= self.facts.atom(self.facts.arm(receiver, i)) == Some(Atom::String);
                }
                if strings {
                    let mut args = args;
                    args.block = None;
                    return self.member_without_collection(state, pc, receiver, site, &args);
                }
            }
            self.text_block(state, pc, receiver, site, args, method)?;
            return Ok(Some([None, None]));
        }
        if let Some(method) = collection_blocks::Method::parse_call(name, args.block.is_some()) {
            self.collection_block(state, pc, receiver, site, args, method)?;
            return Ok(Some([None, None]));
        }
        self.member_without_collection(state, pc, receiver, site, &args)
    }

    /// Reports whether every arm of `receiver` is a collection whose member
    /// always rejects the attached block, mirroring the runtime guard in
    /// `members::call_keywords`. Mixed unions and object receivers stay on
    /// their existing paths; field overrides are resolved by the caller.
    pub(super) fn rejects_block(
        &mut self,
        receiver: Fact,
        site: MemberSite,
        args: &Arguments,
        name: &str,
    ) -> Result<bool> {
        use crate::members::names::Receiver;
        if args.block.is_none() {
            return Ok(false);
        }
        let arguments = !args.positional.data.is_empty();
        let mut arms = 0;
        for i in 0..self.facts.arm_count(receiver) {
            self.ctx.charge(1)?;
            let arm = self.facts.arm(receiver, i);
            if arm == Atom::Never.fact() {
                continue;
            }
            let kind = match self.facts.node(arm) {
                Node::Tuple(_) | Node::Array(_) => Receiver::Array,
                Node::Hash(_, _, kind) | Node::Shape(_, _, _, kind) if kind.plain() => {
                    Receiver::Hash
                }
                _ => return Ok(false),
            };
            if kind.rejects_block(site.method, arguments, name).is_none() {
                return Ok(false);
            }
            arms += 1;
        }
        Ok(arms > 0)
    }

    pub(super) fn member_without_collection(
        &mut self,
        state: &mut State,
        pc: usize,
        receiver: Fact,
        site: MemberSite,
        args: &Arguments,
    ) -> Result<Option<Edges>> {
        let selected = site.text(self.program, self.facts);
        let name = selected.as_str();
        if name == "is_type?" {
            if let Some(variants) = self.predicate_arguments(args)? {
                for arguments in variants.data {
                    let mut next = state.snapshot(self.ctx)?;
                    let edges =
                        self.member_without_collection(&mut next, pc, receiver, site, &arguments)?;
                    self.member_edges(pc, next, edges)?;
                }
                return Ok(Some([None, None]));
            }
        }
        let (value, rejected, incomplete, throws) = if let Some(result) =
            builtins::member(self.ctx, self.facts, receiver, site.call, name, args)?
        {
            let mut result = result;
            if name == "is_type?" {
                let alternatives =
                    self.type_predicate_outcome(state, pc, receiver, args, &mut result)?;
                for mut alternative in alternatives.data {
                    let edges =
                        self.member_without_collection(&mut alternative, pc, receiver, site, args)?;
                    self.member_edges(pc, alternative, edges)?;
                }
            }
            (
                result.value,
                !result.failures.data.is_empty(),
                result.incomplete,
                result.throws,
            )
        } else {
            let reshaping = matches!(self.facts.node(receiver), Node::Array(_) | Node::Tuple(_))
                && matches!(name, "compact" | "chunk" | "window");
            if reshaping
                && name == "compact"
                && (!args.keywords.data.is_empty() || args.block.is_some())
            {
                self.collection_error(state, pc, receiver, site, args, ErrorClass::Runtime)?;
                return Ok(Some([None, None]));
            }
            if (!args.keywords.data.is_empty() && !(reshaping && name != "compact"))
                || args.block.is_some()
            {
                return self.incomplete(pc).map(Some);
            }
            let result = self.facts.native_member(
                self.ctx,
                receiver,
                site.call,
                name,
                &args.positional.data,
            )?;
            if reshaping
                && name != "compact"
                && result.value != Atom::Never.fact()
                && self.wrapping_guard(receiver)?
            {
                self.emit_error(state, pc, handlers::bit(ErrorClass::Limit))?;
            }
            let throws = if result.throws {
                handlers::bit(ErrorClass::Runtime)
            } else {
                0
            };
            (result.value, result.rejected, result.unsupported, throws)
        };
        let throws = if value != Atom::Never.fact()
            && self.native_limit(receiver, name, &args.positional.data)?
        {
            throws | handlers::bit(ErrorClass::Limit)
        } else {
            throws
        };
        self.emit_error(
            state,
            pc,
            throws
                | if rejected {
                    handlers::bit(ErrorClass::Runtime)
                } else {
                    0
                },
        )?;
        if rejected {
            let arguments = self.facts.tuple(self.ctx, &args.positional.data)?;
            self.issue(
                pc,
                IssueKind::Member {
                    name: site.name,
                    receiver,
                    arguments,
                },
            )?;
        }
        if incomplete {
            return self.incomplete(pc).map(Some);
        }
        if value == Atom::Never.fact() {
            return Ok(Some([None, None]));
        }
        state.stack.push(self.ctx, Operand::new(value))?;
        Ok(None)
    }

    /// Reports whether a modeled native member can reach a runtime limit guard
    /// that its operation summary does not describe: `zip` wraps rows around
    /// elements that may already sit at the value depth limit, and a general
    /// range may be too large for `to_a` to materialize.
    pub(super) fn native_limit(
        &mut self,
        receiver: Fact,
        name: &str,
        args: &[Fact],
    ) -> Result<bool> {
        for i in 0..self.facts.arm_count(receiver) {
            self.ctx.charge(1)?;
            let arm = self.facts.arm(receiver, i);
            match self.facts.node(arm) {
                Node::Array(_) | Node::Tuple(_) if name == "zip" => {
                    if self.wrapping_guard(arm)? {
                        return Ok(true);
                    }
                    for &arg in args {
                        if self.wrapping_guard(arg)? {
                            return Ok(true);
                        }
                    }
                }
                Node::Range(..) | Node::Atom(Atom::Range)
                    if name == "to_a" && args.is_empty() && self.facts.range_array_limit(arm) =>
                {
                    return Ok(true);
                }
                _ => (),
            }
        }
        Ok(false)
    }
}
