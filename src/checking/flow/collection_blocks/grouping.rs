use super::*;

struct Key {
    canonical: Fact,
    original: Fact,
}

impl Walker<'_> {
    pub(super) fn canonical_group_keys(
        &mut self,
        state: &State,
        pc: usize,
        site: MemberSite,
        value: Fact,
    ) -> Result<Fact> {
        let keys = self.group_keys(state, pc, site, value)?;
        let mut values = Buffer::empty();
        for key in keys.data {
            values.push(self.ctx, key.canonical)?;
        }
        self.facts.union(self.ctx, &values.data)
    }

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
        site: MemberSite,
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
        site: MemberSite,
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
                Node::Named(_) | Node::Nominal { .. } | Node::Instance { .. } => {
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
            Chunk => return self.chunk_result(current, pc, item, value, depth),
            _ => unreachable!(),
        }
        Ok((current.output != Atom::Never.fact()).then_some(current))
    }

    /// Models one `array.chunk` block key. `previous` is the open group's key
    /// (`Nil` when no group is open), `auxiliary` its members and `output`
    /// the finished `[key, group]` rows. Nil and `:_separator` close the open
    /// group and skip the item, `:_alone` emits the item on its own, other
    /// symbols starting with `_` raise, and any other key either extends the
    /// open group or closes it and opens a new one; when the checker cannot
    /// decide equality both continuations are kept.
    fn chunk_result(
        &mut self,
        mut current: IterationState,
        pc: usize,
        item: Item,
        value: Fact,
        depth: usize,
    ) -> Result<Option<IterationState>> {
        #[derive(Clone, Copy)]
        struct Control {
            separator: bool,
            alone: bool,
            normal: bool,
            invalid: bool,
        }
        let never = Atom::Never.fact();
        let active = self
            .facts
            .filter(self.ctx, current.previous, Test::Nil, false)?;
        let closed = self
            .facts
            .filter(self.ctx, current.previous, Test::Nil, true)?
            != never;
        let mut flushed = None;
        let empty = self.facts.tuple(self.ctx, &[])?;
        let single = self.facts.tuple(self.ctx, &[item.element])?;
        let mut output = never;
        let mut pending = never;
        let mut key = never;
        let item_deep = self.wrapping_depth(item.element, 3)?;
        for i in 0..self.facts.arm_count(value) {
            self.ctx.charge(1)?;
            let arm = self.facts.arm(value, i);
            let control = match self.facts.node(arm) {
                Node::Atom(Atom::Never) => continue,
                Node::Atom(Atom::Nil) => Control {
                    separator: true,
                    alone: false,
                    normal: false,
                    invalid: false,
                },
                Node::Symbol(name) => {
                    let name = name.as_bytes().unwrap();
                    let (separator, alone) = (name == b"_separator", name == b"_alone");
                    let invalid = !separator && !alone && name.first() == Some(&b'_');
                    Control {
                        separator,
                        alone,
                        normal: !separator && !alone && !invalid,
                        invalid,
                    }
                }
                Node::Atom(Atom::Symbol | Atom::Unknown | Atom::Any) => Control {
                    separator: true,
                    alone: true,
                    normal: true,
                    invalid: true,
                },
                Node::Named(_) | Node::Nominal { .. } | Node::Instance { .. } => {
                    self.incomplete(pc)?;
                    continue;
                }
                _ => Control {
                    separator: false,
                    alone: false,
                    normal: true,
                    invalid: false,
                },
            };
            if control.invalid {
                self.emit_error(&current.state, pc, handlers::bit(ErrorClass::Runtime))?;
            }
            if !control.separator && !control.alone && !control.normal {
                continue;
            }
            let flushed = match flushed {
                Some(value) => value,
                None => {
                    let value = self.chunk_flush(
                        &current.state,
                        pc,
                        current.output,
                        current.previous,
                        current.auxiliary,
                    )?;
                    flushed = Some(value);
                    value
                }
            };
            if control.separator {
                output = self.facts.widen(self.ctx, output, flushed, depth)?;
                pending = self.facts.widen(self.ctx, pending, empty, depth)?;
                key = self.facts.union(self.ctx, &[key, Atom::Nil.fact()])?;
            }
            if (control.alone || control.normal) && (item_deep || self.wrapping_depth(arm, 2)?) {
                self.emit_error(&current.state, pc, handlers::bit(ErrorClass::Limit))?;
            }
            if control.alone {
                let row = self.facts.tuple(self.ctx, &[arm, single])?;
                let next = self.group_append(flushed, row)?;
                output = self.facts.widen(self.ctx, output, next, depth)?;
                pending = self.facts.widen(self.ctx, pending, empty, depth)?;
                key = self.facts.union(self.ctx, &[key, Atom::Nil.fact()])?;
            }
            if control.normal {
                let same = if active != never {
                    self.facts.definitely_equal(arm, active)
                } else {
                    Some(false)
                };
                if same != Some(false) {
                    let grown = self.group_append(current.auxiliary, item.element)?;
                    output = self.facts.widen(self.ctx, output, current.output, depth)?;
                    pending = self.facts.widen(self.ctx, pending, grown, depth)?;
                    key = self.facts.widen(self.ctx, key, active, depth)?;
                }
                if same != Some(true) || closed {
                    output = self.facts.widen(self.ctx, output, flushed, depth)?;
                    pending = self.facts.widen(self.ctx, pending, single, depth)?;
                    key = self.facts.widen(self.ctx, key, arm, depth)?;
                }
            }
        }
        current.output = output;
        current.auxiliary = pending;
        current.previous = key;
        Ok((current.output != never).then_some(current))
    }

    // Nil preserves the no-open-group alternative when iteration states join.
    pub(super) fn chunk_flush(
        &mut self,
        state: &State,
        pc: usize,
        output: Fact,
        key: Fact,
        pending: Fact,
    ) -> Result<Fact> {
        let closed = self.facts.filter(self.ctx, key, Test::Nil, true)? != Atom::Never.fact();
        let key = self.facts.filter(self.ctx, key, Test::Nil, false)?;
        if key == Atom::Never.fact() {
            return Ok(output);
        }
        let row = self.facts.tuple(self.ctx, &[key, pending])?;
        if self.wrapping_guard(row)? {
            self.emit_error(state, pc, handlers::bit(ErrorClass::Limit))?;
        }
        let appended = self.group_append(output, row)?;
        if closed {
            self.facts.union(self.ctx, &[output, appended])
        } else {
            Ok(appended)
        }
    }

    fn group_pair(
        &mut self,
        state: &State,
        pc: usize,
        site: MemberSite,
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
            Node::Named(_) | Node::Nominal { .. } | Node::Instance { .. } => {
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
