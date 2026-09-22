use super::*;
use crate::bytecode::Method as Native;

mod fill;

#[derive(Clone, Copy)]
pub(super) enum Mutation {
    Filter { hash: bool, keep: bool },
    Fill,
    Missing,
}

impl Walker<'_> {
    pub(in crate::checking::flow) fn mutable_block(
        &mut self,
        state: &State,
        pc: usize,
        site: impl Into<MemberSite>,
        args: &Arguments,
    ) -> Result<()> {
        let receiver = state.addresses.data.last().unwrap().value;
        let site = site.into();
        let selected = site.text(self.program, self.facts);
        let name = selected.as_str();
        if let Some(variants) =
            crate::checking::objects::variants(self.ctx, self.facts, receiver, name, site.scope)?
        {
            for receiver in variants.data {
                self.ctx.charge(1)?;
                let mut next = state.snapshot(self.ctx)?;
                next.addresses.data.last_mut().unwrap().value = receiver;
                self.mutable_block(&next, pc, site, args)?;
            }
            return Ok(());
        }
        for i in 0..self.facts.arm_count(receiver) {
            self.ctx.charge(1)?;
            let source = self.facts.arm(receiver, i);
            if source == Atom::Never.fact() {
                continue;
            }
            let mut state = state.snapshot(self.ctx)?;
            state.addresses.data.last_mut().unwrap().value = source;
            if matches!(self.facts.atom(source), Some(Atom::Unknown | Atom::Any)) {
                let args = args.snapshot(self.ctx)?;
                let edges = self.dynamic_mutation(&mut state, pc, site, args, false)?;
                self.member_edges(pc, state, edges)?;
                continue;
            }
            use crate::checking::objects::{Selection, absent_is_native};
            match crate::checking::objects::select(self.ctx, self.facts, source, site.call, name)? {
                Some(Selection::Field(_) | Selection::Missing) => {
                    state.addresses.data.pop().unwrap();
                    let arguments = args.snapshot(self.ctx)?;
                    let edges = self.member(&mut state, pc, source, site, arguments)?;
                    self.member_edges(pc, state, edges)?;
                    continue;
                }
                Some(Selection::Uncertain(field)) => {
                    let mut present = state.snapshot(self.ctx)?;
                    present.addresses.data.pop().unwrap();
                    let arguments = args.snapshot(self.ctx)?;
                    let edges =
                        self.member_field(&mut present, pc, field, site, arguments, false)?;
                    self.member_edges(pc, present, edges)?;
                    if !absent_is_native(site.call, name) {
                        self.emit_error(&state, pc, handlers::bit(ErrorClass::Runtime))?;
                        continue;
                    }
                }
                Some(Selection::Native) | None => (),
            }
            let protection = state
                .addresses
                .data
                .last()
                .unwrap()
                .protection(self.ctx, self.facts)?;
            if protection.readonly != Attached::No {
                if protection.report {
                    self.collection_error(&state, pc, source, site, args, ErrorClass::Runtime)?;
                } else {
                    self.protected_error(&state, pc, state.addresses.data.last().unwrap())?;
                }
                if protection.readonly == Attached::Yes {
                    continue;
                }
            }
            if builtins::primitive_member(self.ctx, self.facts, source, name)? {
                state.addresses.data.pop().unwrap();
                if let Some(edges) =
                    self.member_without_collection(&mut state, pc, source, site, args)?
                {
                    for edge in edges.into_iter().flatten() {
                        self.extra.push(self.ctx, edge)?;
                    }
                } else {
                    self.native_continue(pc, state)?;
                }
                continue;
            }
            let kind = match self.facts.node(source) {
                Node::Tuple(_) | Node::Array(_) => Some(Receiver::Array),
                Node::Shape(_, _, _, kind) | Node::Hash(_, _, kind) if kind.single() => {
                    Some(Receiver::Hash)
                }
                Node::Named(_)
                | Node::Nominal { .. }
                | Node::Instance { .. }
                | Node::Shape(..)
                | Node::Hash(..)
                | Node::Atom(Atom::Unknown | Atom::Any)
                | Node::Builtin(_)
                | Node::TypeValue(_)
                | Node::Offset(_) => {
                    self.incomplete(pc)?;
                    continue;
                }
                _ => None,
            };
            // An attached block on `clear` always fails before any mutation.
            let native = match kind {
                Some(Receiver::Array) => Some(crate::members::names::Receiver::Array),
                Some(Receiver::Hash) => Some(crate::members::names::Receiver::Hash),
                _ => None,
            };
            if args.block.is_some()
                && native
                    .and_then(|kind| {
                        kind.rejects_block(site.method, !args.positional.data.is_empty(), name)
                    })
                    .is_some()
            {
                self.collection_error(&state, pc, source, site, args, ErrorClass::Runtime)?;
                continue;
            }
            let specialized = match (name, kind) {
                ("delete_if" | "keep_if", Some(_)) => true,
                ("delete", Some(_)) => args.block.is_some(),
                ("fill", Some(Receiver::Array)) => args.block.is_some(),
                _ => false,
            };
            if !specialized {
                if matches!(name, "delete_if" | "keep_if") || !args.keywords.data.is_empty() {
                    self.collection_error(&state, pc, source, site, args, ErrorClass::Runtime)?;
                } else if self
                    .mutate(&mut state, pc, site, &args.positional.data, false, false)?
                    .is_none()
                {
                    self.native_continue(pc, state)?;
                }
                continue;
            }
            let kind = kind.unwrap();
            let arity = match name {
                "fill" => args.positional.data.len() <= 2,
                "delete" => args.positional.data.len() == 1,
                _ => args.positional.data.is_empty(),
            };
            if site.scope || !args.keywords.data.is_empty() || !arity {
                self.collection_error(&state, pc, source, site, args, ErrorClass::Runtime)?;
                continue;
            }
            let Some(block) = &args.block else {
                self.emit_error(&state, pc, handlers::bit(ErrorClass::Runtime))?;
                self.issue(pc, IssueKind::MissingBlock)?;
                continue;
            };
            let mutation = match name {
                "delete_if" | "keep_if" => Mutation::Filter {
                    hash: kind == Receiver::Hash,
                    keep: name == "keep_if",
                },
                "fill" => Mutation::Fill,
                "delete" => Mutation::Missing,
                _ => unreachable!(),
            };
            let driver = Driver {
                method: match mutation {
                    Mutation::Filter { keep: true, .. } => Method::Select,
                    Mutation::Filter { .. } => Method::Reject,
                    Mutation::Fill => Method::Map,
                    Mutation::Missing => Method::Fetch,
                },
                mutation: Some(mutation),
                callback: Callback::Block(block),
                pattern: None,
                count_overflow: false,
                exact: true,
                site: Some(site),
                offset: Offset::None,
            };
            match mutation {
                Mutation::Filter { .. } => self.mutable_filter(state, pc, source, kind, driver)?,
                Mutation::Fill => self.fill_block(state, pc, source, driver, args)?,
                Mutation::Missing => self.delete_block(state, pc, source, kind, driver, args)?,
            }
        }
        Ok(())
    }

    fn mutable_initial(&mut self, state: State) -> Result<IterationState> {
        Ok(IterationState {
            state,
            output: self.facts.tuple(self.ctx, &[])?,
            auxiliary: self.facts.boolean(self.ctx, false)?,
            previous: Atom::Never.fact(),
        })
    }

    fn mutable_filter(
        &mut self,
        state: State,
        pc: usize,
        source: Fact,
        kind: Receiver,
        driver: Driver<'_>,
    ) -> Result<()> {
        let initial = self.mutable_initial(state)?;
        let depth = self.collection_depth(&initial, driver, source)?;
        if let Node::Tuple(items) = self.facts.node(source) {
            let count = items.data.len();
            let entry = initial.state.snapshot(self.ctx)?;
            let mut current = initial.alternatives(self.ctx)?;
            for position in 0..count {
                self.ctx.charge(1)?;
                if current.data.is_empty() {
                    break;
                }
                let Node::Tuple(items) = self.facts.node(source) else {
                    unreachable!()
                };
                let element = items.data[position];
                let index = self.facts.integer(self.ctx, position as i64)?;
                let item = self.collection_item(kind, driver, element, index)?;
                current = self.iteration_pass(current, &entry, (pc, driver, item), depth)?;
            }
            for current in current.data {
                self.mutable_done(current, pc, source, driver.mutation.unwrap())?;
            }
            return Ok(());
        }
        let iteration = self.facts.iteration(self.ctx, source)?;
        if iteration.empty != Atom::Never.fact() {
            let empty = initial.snapshot(self.ctx)?;
            self.mutable_done(empty, pc, source, driver.mutation.unwrap())?;
        }
        if iteration.item == Atom::Never.fact() {
            return Ok(());
        }
        let item = self.collection_item(kind, driver, iteration.item, Atom::Int.fact())?;
        let first = self.collection_step(initial, pc, driver, item, depth)?;
        self.iteration_loop(
            first,
            driver,
            source,
            |walker, current, depth| walker.collection_step(current, pc, driver, item, depth),
            |walker, current| {
                walker.mutable_done(current, pc, source, driver.mutation.unwrap())?;
                Ok(iteration.repeat != Atom::Never.fact())
            },
        )?;
        Ok(())
    }

    pub(super) fn mutable_filter_result(
        &mut self,
        current: IterationState,
        item: Item,
        value: Fact,
        hash: bool,
        keep: bool,
        depth: usize,
    ) -> Result<Option<IterationState>> {
        let kept = self.facts.filter(self.ctx, value, Test::Truth, keep)?;
        let removed = self.facts.filter(self.ctx, value, Test::Truth, !keep)?;
        let mut result = None;
        if kept != Atom::Never.fact() {
            let mut next = current.snapshot(self.ctx)?;
            if !hash {
                next.output = self.group_append(next.output, item.element)?;
            }
            result = Some(next);
        }
        if removed != Atom::Never.fact() {
            let mut next = current;
            next.auxiliary = self.facts.boolean(self.ctx, true)?;
            if hash {
                next.output = self.group_append(next.output, item.pair.unwrap().0)?;
            }
            if let Some(kept) = &mut result {
                kept.join(self.ctx, self.facts, &next, false, depth, self.program)?;
            } else {
                result = Some(next);
            }
        }
        Ok(result)
    }

    fn mutable_done(
        &mut self,
        current: IterationState,
        pc: usize,
        source: Fact,
        mutation: Mutation,
    ) -> Result<()> {
        if matches!(mutation, Mutation::Missing) {
            return self.mutable_value(current.state, pc, current.output);
        }
        if self
            .facts
            .filter(self.ctx, current.auxiliary, Test::Truth, false)?
            != Atom::Never.fact()
        {
            let state = current.state.snapshot(self.ctx)?;
            self.mutable_value(state, pc, source)?;
        }
        if self
            .facts
            .filter(self.ctx, current.auxiliary, Test::Truth, true)?
            == Atom::Never.fact()
        {
            return Ok(());
        }
        let (receiver, value) = if matches!(mutation, Mutation::Filter { hash: true, .. }) {
            let receiver = current.state.addresses.data.last().unwrap().value;
            let receiver = self.delete_keys(receiver, current.output)?;
            (receiver, receiver)
        } else {
            (current.output, current.output)
        };
        self.mutable_commit(current.state, pc, receiver, value)
    }

    fn mutable_value(&mut self, mut state: State, pc: usize, value: Fact) -> Result<()> {
        state.addresses.data.pop().unwrap();
        state.stack.push(self.ctx, Operand::new(value))?;
        self.native_continue(pc, state)
    }

    fn mutable_commit(
        &mut self,
        mut state: State,
        pc: usize,
        receiver: Fact,
        value: Fact,
    ) -> Result<()> {
        let address = state.addresses.data.pop().unwrap();
        let change = Change::Mutation {
            address: &address,
            method: Some(Native::Replace),
            args: &[],
            fresh: false,
        };
        let edges = self.publish_result(
            &mut state,
            pc,
            &address,
            receiver,
            change,
            MutationOutput::Value(Operand::new(value)),
        )?;
        self.member_edges(pc, state, edges)
    }

    fn delete_keys(&mut self, receiver: Fact, keys: Fact) -> Result<Fact> {
        let mut result = Atom::Never.fact();
        for i in 0..self.facts.arm_count(keys) {
            self.ctx.charge(1)?;
            let keys = self.facts.arm(keys, i);
            let mut current = receiver;
            if let Node::Tuple(items) = self.facts.node(keys) {
                let mut keys = Buffer::empty();
                keys.extend(self.ctx, &items.data)?;
                for key in keys.data {
                    current = self
                        .facts
                        .collection_mutate(self.ctx, current, Native::Delete, &[key])?
                        .receiver;
                }
            } else if let Node::Array(key) = self.facts.node(keys) {
                let key = *key;
                let depth = self.facts.depth(receiver);
                loop {
                    self.ctx.charge(1)?;
                    let next = self
                        .facts
                        .collection_mutate(self.ctx, current, Native::Delete, &[key])?
                        .receiver;
                    let next = self.facts.widen(self.ctx, current, next, depth)?;
                    if next == current {
                        break;
                    }
                    current = next;
                }
            } else {
                unreachable!();
            }
            result = self.facts.union(self.ctx, &[result, current])?;
        }
        Ok(result)
    }

    fn delete_block(
        &mut self,
        state: State,
        pc: usize,
        source: Fact,
        kind: Receiver,
        driver: Driver<'_>,
        args: &Arguments,
    ) -> Result<()> {
        let site = driver.site.unwrap();
        let target = args.positional.data[0];
        for i in 0..self.facts.arm_count(target) {
            self.ctx.charge(1)?;
            let mut key = self.facts.arm(target, i);
            if kind == Receiver::Hash {
                key = self.lookup_key(&state, pc, (source, site, args), kind, key)?;
                if key == Atom::Never.fact() {
                    continue;
                }
            }
            let (found, missing) = if kind == Receiver::Hash {
                self.lookup_value(source, kind, key)?
            } else {
                self.deleted_values(source, key)?
            };
            if found != Atom::Never.fact() {
                let updated = self
                    .facts
                    .collection_mutate(self.ctx, source, Native::Delete, &[key])?
                    .receiver;
                let next = state.snapshot(self.ctx)?;
                self.mutable_commit(next, pc, updated, found)?;
            }
            if missing {
                let state = state.snapshot(self.ctx)?;
                let initial = self.mutable_initial(state)?;
                let depth = self.collection_depth(&initial, driver, source)?;
                let item = Item {
                    arguments: [key, Atom::Nil.fact()],
                    count: 1,
                    element: key,
                    index: Atom::Int.fact(),
                    pair: None,
                };
                for current in self.collection_step(initial, pc, driver, item, depth)?.data {
                    self.mutable_done(current, pc, source, Mutation::Missing)?;
                }
            }
        }
        Ok(())
    }

    fn deleted_values(&mut self, source: Fact, key: Fact) -> Result<(Fact, bool)> {
        let tuple = matches!(self.facts.node(source), Node::Tuple(_));
        let mut values = Buffer::empty();
        match self.facts.node(source) {
            Node::Tuple(items) => values.extend(self.ctx, &items.data)?,
            Node::Array(element) => values.push(self.ctx, *element)?,
            _ => unreachable!(),
        }
        let mut found = Atom::Never.fact();
        let mut missing = true;
        for value in values.data {
            let mut always = tuple;
            for i in 0..self.facts.arm_count(value) {
                self.ctx.charge(1)?;
                let arm = self.facts.arm(value, i);
                if arm == Atom::Never.fact() {
                    always = false;
                    continue;
                }
                let equal = self.facts.definitely_equal(arm, key);
                always &= equal == Some(true);
                if equal != Some(false) {
                    found = self.facts.union(self.ctx, &[found, arm])?;
                }
            }
            missing &= !always;
        }
        Ok((found, missing))
    }
}
