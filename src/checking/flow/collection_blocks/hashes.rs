use super::*;

mod deep;

impl Walker<'_> {
    pub(super) fn hash_block(
        &mut self,
        state: &State,
        pc: usize,
        receiver: Fact,
        site: MemberSite,
        args: Arguments,
        method: Method,
    ) -> Result<()> {
        for i in 0..self.facts.arm_count(receiver) {
            self.ctx.charge(1)?;
            let arm = self.facts.arm(receiver, i);
            let view = self.hash_view(arm);
            match self.facts.node(view) {
                Node::Atom(Atom::Never) => continue,
                Node::Shape(_, _, _, HashKind::Plain | HashKind::Object)
                | Node::Hash(_, _, HashKind::Plain | HashKind::Object) => (),
                Node::Named(_)
                | Node::Nominal { .. }
                | Node::Shape(..)
                | Node::Hash(..)
                | Node::Atom(Atom::Unknown | Atom::Any) => {
                    self.incomplete(pc)?;
                    continue;
                }
                _ => {
                    self.collection_fallback(state, pc, arm, site, &args)?;
                    continue;
                }
            }
            if site.scope
                || (method == Method::Merge && !args.keywords.data.is_empty())
                || (method == Method::DeepTransformKeys
                    && (!args.positional.data.is_empty() || args.block.is_none()))
            {
                self.collection_error(state, pc, arm, site, &args, ErrorClass::Runtime)?;
                continue;
            }
            let driver = Driver {
                method,
                mutation: None,
                callback: args
                    .block
                    .as_ref()
                    .map_or(Callback::Identity, Callback::Block),
                pattern: None,
                count_overflow: false,
                exact: false,
                site: Some(site),
            };
            if method == Method::DeepTransformKeys {
                if let Some(done) = self.deep_hash(state, pc, driver, view)? {
                    self.collection_terminal(done.state, pc, done.output)?;
                }
                continue;
            }
            let mut sources = Buffer::empty();
            let mut valid = true;
            for &source in &args.positional.data {
                let mut accepted = Buffer::empty();
                for j in 0..self.facts.arm_count(source) {
                    self.ctx.charge(1)?;
                    let candidate = self.hash_view(self.facts.arm(source, j));
                    let value = match self.facts.node(candidate) {
                        Node::Shape(_, _, _, HashKind::Plain)
                        | Node::Hash(_, _, HashKind::Plain) => candidate,
                        Node::Atom(Atom::Never) => continue,
                        Node::Atom(Atom::Unknown | Atom::Any) => {
                            self.emit_error(state, pc, handlers::bit(ErrorClass::Runtime))?;
                            let keys = self
                                .facts
                                .union(self.ctx, &[Atom::String.fact(), Atom::Symbol.fact()])?;
                            self.facts
                                .hash_kind(self.ctx, keys, Atom::Unknown.fact(), true)?
                        }
                        Node::Hash(_, _, HashKind::Any | HashKind::Object)
                        | Node::Shape(_, _, _, HashKind::Any | HashKind::Object) => {
                            self.plain_hash_data(candidate)?
                        }
                        Node::Named(_) | Node::Nominal { .. } => {
                            self.incomplete(pc)?;
                            continue;
                        }
                        _ => {
                            self.collection_error(
                                state,
                                pc,
                                arm,
                                site,
                                &args,
                                ErrorClass::Runtime,
                            )?;
                            continue;
                        }
                    };
                    accepted.push(self.ctx, value)?;
                }
                let source = self.facts.union(self.ctx, &accepted.data)?;
                if source == Atom::Never.fact() {
                    valid = false;
                    break;
                }
                sources.push(self.ctx, source)?;
            }
            if !valid {
                continue;
            }
            let initial = IterationState {
                state: state.snapshot(self.ctx)?,
                output: self.plain_hash_data(view)?,
                auxiliary: Atom::Never.fact(),
                previous: Atom::Never.fact(),
            };
            let mut current = Some(initial);
            for source in sources.data {
                let Some(before) = current else { break };
                current = self.merge_source(before, pc, driver, source)?;
            }
            if let Some(done) = current {
                self.collection_terminal(done.state, pc, done.output)?;
            }
        }
        Ok(())
    }

    fn hash_view(&self, value: Fact) -> Fact {
        if let Node::Protected(shape, _) = self.facts.node(value) {
            *shape
        } else {
            value
        }
    }

    fn plain_hash_data(&mut self, input: Fact) -> Result<Fact> {
        self.facts.hash_as(self.ctx, input, HashKind::Plain)
    }

    fn hash_join(
        &mut self,
        target: &mut Option<IterationState>,
        value: IterationState,
        depth: usize,
    ) -> Result<()> {
        if let Some(target) = target {
            target.join(self.ctx, self.facts, &value, false, depth)?;
        } else {
            *target = Some(value);
        }
        Ok(())
    }

    fn merge_source(
        &mut self,
        initial: IterationState,
        pc: usize,
        driver: Driver<'_>,
        source: Fact,
    ) -> Result<Option<IterationState>> {
        let depth = self.collection_depth(&initial, driver, source)?;
        let base = initial.output;
        let mut result = None;
        for index in 0..self.facts.arm_count(source) {
            self.ctx.charge(1)?;
            let arm = self.facts.arm(source, index);
            if let Some(done) = self.merge_passive(&initial, driver, arm, depth)? {
                self.hash_join(&mut result, done, depth)?;
                continue;
            }
            let iteration = self.facts.iteration(self.ctx, arm)?;
            if iteration.empty != Atom::Never.fact() {
                let empty = initial.snapshot(self.ctx)?;
                self.hash_join(&mut result, empty, depth)?;
            }
            if iteration.item == Atom::Never.fact() {
                continue;
            }
            let Some(mut current) =
                self.merge_round(&initial, pc, driver, base, iteration.item, depth)?
            else {
                continue;
            };
            if iteration.repeat != Atom::Never.fact() {
                let depth = self.collection_depth(&current, driver, source)?;
                current.state.widening.get_or_insert(depth);
                loop {
                    self.ctx.charge(1)?;
                    let Some(next) =
                        self.merge_round(&current, pc, driver, base, iteration.item, depth)?
                    else {
                        break;
                    };
                    if !current.join(self.ctx, self.facts, &next, true, depth)? {
                        break;
                    }
                }
            }
            if let Node::Shape(fields, _, _, _) = self.facts.node(arm) {
                let length = fields.data.len();
                for index in 0..length {
                    self.ctx.charge(1)?;
                    let Node::Shape(fields, ..) = self.facts.node(arm) else {
                        unreachable!()
                    };
                    let field = &fields.data[index];
                    if field.optional {
                        continue;
                    }
                    let name = field.name.clone();
                    let key = self.facts.string(self.ctx, name.as_bytes().unwrap())?;
                    current.output = self.hash_require_key(current.output, key)?;
                }
            }
            if current.output == Atom::Never.fact() {
                continue;
            }
            let depth = self.collection_depth(&current, driver, source)?;
            self.hash_join(&mut result, current, depth)?;
        }
        Ok(result)
    }

    fn hash_require_key(&mut self, output: Fact, key: Fact) -> Result<Fact> {
        let mut value = Atom::Never.fact();
        for index in 0..self.facts.arm_count(output) {
            let arm = self.facts.arm(output, index);
            if arm == Atom::Never.fact() {
                continue;
            }
            let found = self.lookup_value(arm, Receiver::Hash, key)?.0;
            value = self.facts.union(self.ctx, &[value, found])?;
        }
        if value == Atom::Never.fact() {
            return Ok(value);
        }
        Ok(self
            .facts
            .collection_write(self.ctx, output, key, value)?
            .receiver)
    }

    fn merge_passive(
        &mut self,
        initial: &IterationState,
        driver: Driver<'_>,
        source: Fact,
        depth: usize,
    ) -> Result<Option<IterationState>> {
        let Node::Shape(fields, false, keys, _) = self.facts.node(source) else {
            return Ok(None);
        };
        let (length, keys) = (fields.data.len(), *keys);
        let mut entries = Buffer::empty();
        for index in 0..length {
            self.ctx.charge(1)?;
            let Node::Shape(fields, ..) = self.facts.node(source) else {
                unreachable!()
            };
            let field = &fields.data[index];
            let (name, value, optional) = (field.name.clone(), field.value, field.optional);
            let key = if self.facts.atom(keys) == Some(Atom::Symbol) {
                self.facts.symbol(self.ctx, name.as_bytes().unwrap())?
            } else {
                self.facts.string(self.ctx, name.as_bytes().unwrap())?
            };
            if driver.block().is_some() {
                for i in 0..self.facts.arm_count(initial.output) {
                    let base = self.facts.arm(initial.output, i);
                    if self.lookup_value(base, Receiver::Hash, key)?.0 != Atom::Never.fact() {
                        return Ok(None);
                    }
                }
            }
            entries.push(self.ctx, (key, value, optional))?;
        }
        let mut current = initial.snapshot(self.ctx)?;
        for (key, value, optional) in entries.data {
            let next = self
                .facts
                .collection_write(self.ctx, current.output, key, value)?
                .receiver;
            current.output = if optional {
                self.facts.widen(self.ctx, current.output, next, depth)?
            } else {
                next
            };
        }
        Ok(Some(current))
    }

    fn merge_round(
        &mut self,
        before: &IterationState,
        pc: usize,
        driver: Driver<'_>,
        base: Fact,
        entries: Fact,
        depth: usize,
    ) -> Result<Option<IterationState>> {
        let mut result = None;
        for i in 0..self.facts.arm_count(entries) {
            self.ctx.charge(1)?;
            let entry = self.facts.arm(entries, i);
            let key = self
                .facts
                .extract(self.ctx, entry, crate::bytecode::Selection::At(0))?;
            let value = self
                .facts
                .extract(self.ctx, entry, crate::bytecode::Selection::At(1))?;
            // Each source has unique keys. Repeated abstract visits must look up
            // conflicts in the preceding sources, not in this source's partial output.
            for j in 0..self.facts.arm_count(base) {
                self.ctx.charge(1)?;
                let previous = self.facts.arm(base, j);
                let (found, missing) = self.lookup_value(previous, Receiver::Hash, key)?;
                if missing || driver.block().is_none() {
                    let mut next = before.snapshot(self.ctx)?;
                    next.output = self
                        .facts
                        .collection_write(self.ctx, next.output, key, value)?
                        .receiver;
                    self.hash_join(&mut result, next, depth)?;
                }
                if found == Atom::Never.fact() || driver.block().is_none() {
                    continue;
                }
                let item = Item {
                    arguments: [key, found],
                    count: 2,
                    element: value,
                    index: Atom::Int.fact(),
                    pair: Some((key, value)),
                };
                let before = before.snapshot(self.ctx)?;
                if let Some(next) = self.collection_step(before, pc, driver, item, depth)? {
                    self.hash_join(&mut result, next, depth)?;
                }
            }
        }
        Ok(result)
    }
}
