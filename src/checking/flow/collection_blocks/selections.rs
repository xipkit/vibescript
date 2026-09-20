use super::*;

impl Walker<'_> {
    pub(super) fn unique_result(
        &mut self,
        mut current: IterationState,
        item: Item,
        key: Fact,
        depth: usize,
    ) -> Result<Option<IterationState>> {
        let contains = self.unique_contains(current.auxiliary, key)?;
        if matches!(self.facts.node(contains), Node::Boolean(true)) {
            return Ok(Some(current));
        }
        let mut kept = current.snapshot(self.ctx)?;
        kept.output = self.group_append(kept.output, item.element)?;
        // Seen keys live in an internal buffer, not a wrapped script value.
        kept.auxiliary = self.group_append(kept.auxiliary, key)?;
        if matches!(self.facts.node(contains), Node::Boolean(false)) {
            return Ok(Some(kept));
        }
        current.join(self.ctx, self.facts, &kept, false, depth, self.program)?;
        Ok(Some(current))
    }

    fn unique_contains(&mut self, seen: Fact, key: Fact) -> Result<Fact> {
        let mut result = Atom::Never.fact();
        for i in 0..self.facts.arm_count(seen) {
            self.ctx.charge(1)?;
            let arm = self.facts.arm(seen, i);
            let value = match self.facts.node(arm) {
                Node::Tuple(items) => {
                    let mut keys = Buffer::empty();
                    keys.extend(self.ctx, &items.data)?;
                    let mut certain = false;
                    let mut possible = false;
                    for key_before in keys.data {
                        self.ctx.charge(1)?;
                        let value = self.facts.set_equal(self.ctx, key_before, key)?;
                        match self.facts.node(value) {
                            Node::Boolean(true) => {
                                certain = true;
                                break;
                            }
                            Node::Boolean(false) | Node::Atom(Atom::Never) => (),
                            _ => possible = true,
                        }
                    }
                    if certain {
                        self.facts.boolean(self.ctx, true)?
                    } else if possible {
                        Atom::Bool.fact()
                    } else {
                        self.facts.boolean(self.ctx, false)?
                    }
                }
                Node::Array(element) => {
                    let value = self.facts.set_equal(self.ctx, *element, key)?;
                    if matches!(
                        self.facts.node(value),
                        Node::Boolean(false) | Node::Atom(Atom::Never)
                    ) {
                        self.facts.boolean(self.ctx, false)?
                    } else {
                        Atom::Bool.fact()
                    }
                }
                _ => unreachable!(),
            };
            result = self.facts.union(self.ctx, &[result, value])?;
        }
        Ok(result)
    }

    pub(super) fn lookup_block(
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
            let view = if let Node::Protected(shape, _) = self.facts.node(arm) {
                *shape
            } else {
                arm
            };
            let kind = match self.facts.node(view) {
                Node::Atom(Atom::Never) => continue,
                Node::Tuple(_) | Node::Array(_) if method == Method::Fetch => Receiver::Array,
                Node::Shape(_, _, _, kind) | Node::Hash(_, _, kind) if kind.single() => {
                    Receiver::Hash
                }
                Node::Named(_)
                | Node::Nominal { .. }
                | Node::Instance { .. }
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
            };
            let count = args.positional.data.len();
            if site.scope || (method == Method::Fetch && !(1..=2).contains(&count)) {
                self.collection_error(state, pc, arm, site, &args, ErrorClass::Runtime)?;
                continue;
            }
            let driver = Driver {
                mutation: None,
                method,
                callback: args
                    .block
                    .as_ref()
                    .map_or(Callback::Identity, Callback::Block),
                pattern: None,
                count_overflow: false,
                exact: true,
                site: Some(site),
                offset: Offset::None,
            };
            let initial = IterationState {
                state: state.snapshot(self.ctx)?,
                output: if method == Method::Fetch {
                    Atom::Never.fact()
                } else {
                    self.facts.tuple(self.ctx, &[])?
                },
                auxiliary: Atom::Never.fact(),
                previous: Atom::Never.fact(),
            };
            let depth = self.collection_depth(&initial, driver, arm)?;
            let mut current = initial.alternatives(self.ctx)?;
            let count = if method == Method::Fetch { 1 } else { count };
            for index in 0..count {
                self.ctx.charge(1)?;
                if current.data.is_empty() {
                    break;
                }
                let mut after = Buffer::empty();
                let key = args.positional.data[index];
                for before in current.data {
                    for k in 0..self.facts.arm_count(key) {
                        self.ctx.charge(1)?;
                        let input = self.facts.arm(key, k);
                        let key =
                            self.lookup_key(&before.state, pc, (arm, site, &args), kind, input)?;
                        if key == Atom::Never.fact() {
                            continue;
                        }
                        let (found, missing) = self.lookup_value(view, kind, key)?;
                        if found != Atom::Never.fact() {
                            let mut hit = before.snapshot(self.ctx)?;
                            hit.output = if method == Method::Fetch {
                                found
                            } else {
                                self.group_append(hit.output, found)?
                            };
                            self.iteration_join(&mut after, hit, depth)?;
                        }
                        if !missing {
                            continue;
                        }
                        let next = if args.block.is_some() {
                            let item = Item {
                                arguments: [key, Atom::Nil.fact()],
                                count: 1,
                                element: key,
                                index: Atom::Int.fact(),
                                pair: None,
                            };
                            let missing = before.snapshot(self.ctx)?;
                            self.collection_step(missing, pc, driver, item, depth)?
                        } else if method == Method::Fetch && args.positional.data.len() == 2 {
                            let mut next = before.snapshot(self.ctx)?;
                            next.output = args.positional.data[1];
                            next.alternatives(self.ctx)?
                        } else {
                            if found == Atom::Never.fact() {
                                self.collection_error(
                                    &before.state,
                                    pc,
                                    arm,
                                    site,
                                    &args,
                                    ErrorClass::Runtime,
                                )?;
                            } else {
                                self.emit_error(
                                    &before.state,
                                    pc,
                                    handlers::bit(ErrorClass::Runtime),
                                )?;
                            }
                            Buffer::empty()
                        };
                        for next in next.data {
                            self.iteration_join(&mut after, next, depth)?;
                        }
                    }
                }
                current = after;
            }
            for current in current.data {
                self.collection_done(current, pc, method, arm)?;
            }
        }
        Ok(())
    }

    pub(super) fn lookup_key(
        &mut self,
        state: &State,
        pc: usize,
        call: (Fact, MemberSite, &Arguments),
        kind: Receiver,
        key: Fact,
    ) -> Result<Fact> {
        let mut possible_error = false;
        let value = match (kind, self.facts.node(key)) {
            (_, Node::Atom(Atom::Never)) => return Ok(key),
            (_, Node::Named(_) | Node::Nominal { .. } | Node::Instance { .. }) => {
                self.incomplete(pc)?;
                return Ok(Atom::Never.fact());
            }
            (Receiver::Array, Node::Integer(_)) => key,
            (Receiver::Array, Node::IntegerBounds(bounds)) => {
                possible_error = bounds.min.is_none() || bounds.max.is_none();
                key
            }
            (Receiver::Array, Node::Float(bits)) => {
                let value = f64::from_bits(*bits);
                if value.is_finite()
                    && value.fract() == 0.0
                    && value >= i64::MIN as f64
                    && value < -(i64::MIN as f64)
                {
                    self.facts.integer(self.ctx, value as i64)?
                } else {
                    Atom::Never.fact()
                }
            }
            (Receiver::Array, Node::Atom(Atom::Int | Atom::Float | Atom::Unknown | Atom::Any)) => {
                possible_error = true;
                Atom::Int.fact()
            }
            (
                Receiver::Hash,
                Node::String(_) | Node::Symbol(_) | Node::Atom(Atom::String | Atom::Symbol),
            ) => key,
            (Receiver::Hash, Node::Atom(Atom::Unknown | Atom::Any)) => {
                possible_error = true;
                self.facts
                    .union(self.ctx, &[Atom::String.fact(), Atom::Symbol.fact()])?
            }
            _ => Atom::Never.fact(),
        };
        if value == Atom::Never.fact() {
            self.collection_error(state, pc, call.0, call.1, call.2, ErrorClass::Runtime)?;
        } else if possible_error {
            self.emit_error(state, pc, handlers::bit(ErrorClass::Runtime))?;
        }
        Ok(value)
    }

    pub(super) fn lookup_value(
        &mut self,
        receiver: Fact,
        kind: Receiver,
        key: Fact,
    ) -> Result<(Fact, bool)> {
        self.ctx.charge(1)?;
        if kind == Receiver::Array {
            if let (Node::Tuple(values), Node::Integer(index)) =
                (self.facts.node(receiver), self.facts.node(key))
            {
                let index = if *index < 0 {
                    values.data.len() as i128 + i128::from(*index)
                } else {
                    i128::from(*index)
                };
                return Ok(
                    match usize::try_from(index)
                        .ok()
                        .and_then(|index| values.data.get(index))
                        .copied()
                    {
                        Some(value) => (value, false),
                        None => (Atom::Never.fact(), true),
                    },
                );
            }
            return Ok((self.facts.elements(self.ctx, receiver)?, true));
        }
        match self.facts.node(receiver) {
            Node::Hash(_, value, _) => Ok((*value, true)),
            Node::Shape(fields, open, _, _) => {
                let open = *open;
                if let Node::String(key) | Node::Symbol(key) = self.facts.node(key) {
                    return Ok(
                        match self.facts.selected_field(
                            self.ctx,
                            receiver,
                            key.as_bytes().unwrap(),
                        )? {
                            Some((value, optional)) => (value, optional),
                            None if open => (Atom::Unknown.fact(), true),
                            None => (Atom::Never.fact(), true),
                        },
                    );
                }
                let mut values = Buffer::empty();
                if open {
                    values.push(self.ctx, Atom::Unknown.fact())?;
                }
                for field in &fields.data {
                    self.ctx.charge(1)?;
                    values.push(self.ctx, field.value)?;
                }
                Ok((self.facts.union(self.ctx, &values.data)?, true))
            }
            _ => unreachable!(),
        }
    }
}
