use super::*;

struct Key {
    canonical: Fact,
    original: Fact,
}

impl Walker<'_> {
    pub(super) fn group_append(&mut self, array: Fact, value: Fact) -> Result<Fact> {
        Ok(self
            .facts
            .collection_mutate(self.ctx, array, crate::bytecode::Method::Push, &[value])?
            .receiver)
    }

    fn group_lookup(&mut self, hash: Fact, key: Fact) -> Result<Fact> {
        Ok(self.facts.collection_index(self.ctx, hash, &[key])?.value)
    }

    fn group_write(&mut self, hash: Fact, key: Fact, value: Fact) -> Result<Fact> {
        Ok(self
            .facts
            .collection_write(self.ctx, hash, key, value)?
            .receiver)
    }

    fn group_error(
        &mut self,
        state: &State,
        pc: usize,
        site: CallSite,
        actual: Fact,
        expected: Fact,
    ) -> Result<()> {
        self.emit_error(state, pc, handlers::bit(ErrorClass::Runtime))?;
        self.issue(
            pc,
            IssueKind::CallbackResult {
                name: site.name,
                actual,
                expected,
            },
        )
    }

    fn group_keys(
        &mut self,
        state: &State,
        pc: usize,
        site: CallSite,
        value: Fact,
    ) -> Result<Buffer<Key>> {
        let expected = self
            .facts
            .union(self.ctx, &[Atom::String.fact(), Atom::Symbol.fact()])?;
        let mut keys = Buffer::empty();
        for i in 0..self.facts.arm_count(value) {
            self.ctx.charge(1)?;
            let arm = self.facts.arm(value, i);
            let (canonical, original) = match self.facts.node(arm) {
                Node::Atom(Atom::Never) => continue,
                Node::String(_) => (arm, arm),
                Node::Symbol(value) => {
                    let value = value.clone();
                    (self.facts.string(self.ctx, value.as_bytes().unwrap())?, arm)
                }
                Node::Atom(Atom::String | Atom::Symbol) => (Atom::String.fact(), arm),
                Node::Atom(Atom::Unknown | Atom::Any) => {
                    self.emit_error(state, pc, handlers::bit(ErrorClass::Runtime))?;
                    (Atom::String.fact(), expected)
                }
                Node::Named(_) | Node::Nominal { .. } => {
                    self.incomplete(pc)?;
                    continue;
                }
                _ => {
                    self.group_error(state, pc, site, arm, expected)?;
                    continue;
                }
            };
            keys.push(
                self.ctx,
                Key {
                    canonical,
                    original,
                },
            )?;
        }
        Ok(keys)
    }

    pub(super) fn group_result(
        &mut self,
        mut current: IterationState,
        pc: usize,
        driver: Driver<'_>,
        item: Item,
        value: Fact,
        depth: usize,
    ) -> Result<Option<IterationState>> {
        use Method::*;
        match driver.method {
            DropWhile => {
                let yes = self.facts.filter(self.ctx, value, Test::Truth, true)?;
                let no = self.facts.filter(self.ctx, value, Test::Truth, false)?;
                let mut output = Atom::Never.fact();
                let mut phase = Atom::Never.fact();
                if yes != Atom::Never.fact() {
                    output = current.output;
                    phase = self.facts.boolean(self.ctx, true)?;
                }
                if no != Atom::Never.fact() {
                    let kept = self.group_append(current.output, item.element)?;
                    output = self.facts.widen(self.ctx, output, kept, depth)?;
                    let no = self.facts.boolean(self.ctx, false)?;
                    phase = self.facts.union(self.ctx, &[phase, no])?;
                }
                current.output = output;
                current.auxiliary = phase;
            }
            Partition => {
                if self.wrapping_depth(item.element, 2)? {
                    self.emit_error(&current.state, pc, handlers::bit(ErrorClass::Limit))?;
                }
                let first = self.facts.extract(
                    self.ctx,
                    current.output,
                    crate::bytecode::Selection::At(0),
                )?;
                let second = self.facts.extract(
                    self.ctx,
                    current.output,
                    crate::bytecode::Selection::At(1),
                )?;
                let mut output = Atom::Never.fact();
                for selected in [true, false] {
                    if self.facts.filter(self.ctx, value, Test::Truth, selected)?
                        == Atom::Never.fact()
                    {
                        continue;
                    }
                    let a = if selected {
                        self.group_append(first, item.element)?
                    } else {
                        first
                    };
                    let b = if selected {
                        second
                    } else {
                        self.group_append(second, item.element)?
                    };
                    let next = self.facts.tuple(self.ctx, &[a, b])?;
                    output = self.facts.widen(self.ctx, output, next, depth)?;
                }
                current.output = output;
            }
            GroupBy | GroupStable | Tally => {
                let layers = if driver.method == GroupStable { 3 } else { 2 };
                if driver.method != Tally && self.wrapping_depth(item.element, layers)? {
                    self.emit_error(&current.state, pc, handlers::bit(ErrorClass::Limit))?;
                }
                let keys = self.group_keys(&current.state, pc, driver.site.unwrap(), value)?;
                let mut output = Atom::Never.fact();
                let mut order = Atom::Never.fact();
                for key in keys.data {
                    let old = self.group_lookup(current.output, key.canonical)?;
                    for i in 0..self.facts.arm_count(old) {
                        self.ctx.charge(1)?;
                        let old = self.facts.arm(old, i);
                        let missing = old == Atom::Nil.fact();
                        let next = if driver.method == Tally {
                            if !driver.exact {
                                Atom::Int.fact()
                            } else if missing {
                                self.facts.integer(self.ctx, 1)?
                            } else if let Node::Integer(n) = self.facts.node(old) {
                                let Some(n) = n.checked_add(1) else {
                                    self.emit_error(
                                        &current.state,
                                        pc,
                                        handlers::bit(ErrorClass::Runtime),
                                    )?;
                                    continue;
                                };
                                self.facts.integer(self.ctx, n)?
                            } else {
                                Atom::Int.fact()
                            }
                        } else {
                            let group = if missing {
                                self.facts.tuple(self.ctx, &[])?
                            } else {
                                old
                            };
                            self.group_append(group, item.element)?
                        };
                        let next = self.group_write(current.output, key.canonical, next)?;
                        output = self.facts.widen(self.ctx, output, next, depth)?;
                        if driver.method == GroupStable {
                            let next = if missing {
                                self.group_append(current.auxiliary, key.original)?
                            } else {
                                current.auxiliary
                            };
                            order = self.facts.widen(self.ctx, order, next, depth)?;
                        }
                    }
                }
                current.output = output;
                if driver.method == GroupStable {
                    current.auxiliary = order;
                }
            }
            ToHash => {
                let mut output = Atom::Never.fact();
                for i in 0..self.facts.arm_count(value) {
                    self.ctx.charge(1)?;
                    let arm = self.facts.arm(value, i);
                    let Some((key, value)) =
                        self.group_pair(&current.state, pc, driver.site.unwrap(), arm)?
                    else {
                        continue;
                    };
                    let keys = self.group_keys(&current.state, pc, driver.site.unwrap(), key)?;
                    for key in keys.data {
                        if self.wrapping_guard(value)? {
                            self.emit_error(&current.state, pc, handlers::bit(ErrorClass::Limit))?;
                        }
                        let next = self.group_write(current.output, key.canonical, value)?;
                        output = self.facts.widen(self.ctx, output, next, depth)?;
                    }
                }
                current.output = output;
            }
            TransformKeys => {
                let (_, original) = item.pair.unwrap();
                let keys = self.group_keys(&current.state, pc, driver.site.unwrap(), value)?;
                let mut output = Atom::Never.fact();
                for key in keys.data {
                    let next = self.group_write(current.output, key.canonical, original)?;
                    output = self.facts.widen(self.ctx, output, next, depth)?;
                }
                current.output = output;
            }
            TransformValues => {
                if self.wrapping_guard(value)? {
                    self.emit_error(&current.state, pc, handlers::bit(ErrorClass::Limit))?;
                }
                current.output = self.group_write(current.output, item.pair.unwrap().0, value)?;
            }
            SliceWhen | ChunkWhile => {
                let boundary = driver.method == SliceWhen;
                let mut output = Atom::Never.fact();
                let mut pending = Atom::Never.fact();
                for split in [true, false] {
                    if self
                        .facts
                        .filter(self.ctx, value, Test::Truth, split == boundary)?
                        == Atom::Never.fact()
                    {
                        continue;
                    }
                    let (next, group) = if split {
                        (
                            self.group_flush(
                                &current.state,
                                pc,
                                current.output,
                                current.auxiliary,
                            )?,
                            self.facts.tuple(self.ctx, &[item.element])?,
                        )
                    } else {
                        (
                            current.output,
                            self.group_append(current.auxiliary, item.element)?,
                        )
                    };
                    output = self.facts.widen(self.ctx, output, next, depth)?;
                    pending = self.facts.widen(self.ctx, pending, group, depth)?;
                }
                current.output = output;
                current.auxiliary = pending;
                current.previous = item.element;
            }
            _ => unreachable!(),
        }
        Ok((current.output != Atom::Never.fact()).then_some(current))
    }

    fn group_pair(
        &mut self,
        state: &State,
        pc: usize,
        site: CallSite,
        value: Fact,
    ) -> Result<Option<(Fact, Fact)>> {
        match self.facts.node(value) {
            Node::Tuple(items) if items.data.len() == 2 => Ok(Some((items.data[0], items.data[1]))),
            Node::Array(element) => {
                let element = *element;
                self.emit_error(state, pc, handlers::bit(ErrorClass::Runtime))?;
                Ok(Some((element, element)))
            }
            Node::Atom(Atom::Unknown | Atom::Any) => {
                self.emit_error(state, pc, handlers::bit(ErrorClass::Runtime))?;
                Ok(Some((Atom::Unknown.fact(), Atom::Unknown.fact())))
            }
            Node::Named(_) | Node::Nominal { .. } => {
                self.incomplete(pc)?;
                Ok(None)
            }
            _ => {
                let key = self
                    .facts
                    .union(self.ctx, &[Atom::String.fact(), Atom::Symbol.fact()])?;
                let expected = self.facts.tuple(self.ctx, &[key, Atom::Any.fact()])?;
                self.group_error(state, pc, site, value, expected)?;
                Ok(None)
            }
        }
    }

    pub(super) fn group_flush(
        &mut self,
        state: &State,
        pc: usize,
        output: Fact,
        pending: Fact,
    ) -> Result<Fact> {
        if self.wrapping_guard(pending)? {
            self.emit_error(state, pc, handlers::bit(ErrorClass::Limit))?;
        }
        self.group_append(output, pending)
    }

    pub(super) fn group_stable_output(&mut self, groups: Fact, order: Fact) -> Result<Fact> {
        let mut output = Atom::Never.fact();
        for i in 0..self.facts.arm_count(order) {
            self.ctx.charge(1)?;
            let arm = self.facts.arm(order, i);
            let next = if let Node::Tuple(keys) = self.facts.node(arm) {
                let mut copied = Buffer::empty();
                copied.extend(self.ctx, &keys.data)?;
                let mut pairs = Buffer::empty();
                for key in copied.data {
                    let group = self.group_lookup(groups, key)?;
                    let group = self.facts.filter(self.ctx, group, Test::Nil, false)?;
                    let pair = self.facts.tuple(self.ctx, &[key, group])?;
                    pairs.push(self.ctx, pair)?;
                }
                self.facts.tuple(self.ctx, &pairs.data)?
            } else {
                let key = self.facts.elements(self.ctx, arm)?;
                let group = self.group_lookup(groups, key)?;
                let group = self.facts.filter(self.ctx, group, Test::Nil, false)?;
                let pair = self.facts.tuple(self.ctx, &[key, group])?;
                self.facts.array(self.ctx, pair)?
            };
            output = self.facts.union(self.ctx, &[output, next])?;
        }
        Ok(output)
    }
}
