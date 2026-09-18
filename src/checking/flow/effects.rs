use super::*;
use crate::checking::facts::Node;

impl Walker<'_> {
    pub(super) fn dynamic(&mut self, value: Fact) -> Result<bool> {
        self.ctx.charge(self.facts.arm_count(value) as u64)?;
        Ok((0..self.facts.arm_count(value)).any(|i| {
            matches!(
                self.facts.node(self.facts.arm(value, i)),
                Node::Atom(Atom::Unknown | Atom::Any) | Node::Named(_) | Node::Nominal { .. }
            )
        }))
    }

    pub(super) fn wrapping_guard(&mut self, value: Fact) -> Result<bool> {
        self.wrapping_depth(value, 1)
    }

    pub(super) fn wrapping_depth(&mut self, value: Fact, layers: usize) -> Result<bool> {
        self.ctx.charge(1)?;
        if self.facts.depth(value).saturating_add(layers) > crate::budget::MAX_VALUE_DEPTH {
            return Ok(true);
        }
        let mut pending = Buffer::empty();
        pending.push(self.ctx, value)?;
        let mut visited = Slots::new(self.facts.len(), false);
        while let Some(value) = pending.data.pop() {
            self.ctx.charge(1)?;
            if visited.get(self.ctx, value.0)? {
                continue;
            }
            visited.set(self.ctx, value.0, true)?;
            match self.facts.node(value) {
                Node::Atom(Atom::Unknown | Atom::Any)
                | Node::Named(_)
                | Node::Nominal { .. }
                | Node::Shape(_, true, _, _) => return Ok(true),
                Node::Array(element) | Node::Hash(_, element, _) | Node::Protected(element, _) => {
                    pending.push(self.ctx, *element)?
                }
                Node::Tuple(values) | Node::Union(values) => {
                    pending.extend(self.ctx, &values.data)?
                }
                Node::Shape(fields, ..) => {
                    for field in &fields.data {
                        self.ctx.charge(1)?;
                        pending.push(self.ctx, field.value)?;
                    }
                }
                _ => (),
            }
        }
        Ok(false)
    }

    pub(super) fn potential_errors(&mut self, state: &State, op: Op) -> Result<u8> {
        let runtime = handlers::bit(ErrorClass::Runtime);
        let top = || state.stack.data.last().map(|v| v.value);
        let values = match op {
            Op::Array(count) => {
                let base = state.stack.data.len() - count;
                for operand in &state.stack.data[base..] {
                    if self.wrapping_guard(operand.value)? {
                        return Ok(handlers::bit(ErrorClass::Limit));
                    }
                }
                return Ok(0);
            }
            Op::Hash(count) => {
                let base = state.stack.data.len() - count * 2;
                for pair in state.stack.data[base..].chunks_exact(2) {
                    if self.wrapping_guard(pair[1].value)? {
                        return Ok(handlers::bit(ErrorClass::Limit));
                    }
                }
                return Ok(0);
            }
            Op::Normalize(..) => return Ok(0),
            Op::Range(start, end, _) => {
                let n = usize::from(start) + usize::from(end);
                &state.stack.data[state.stack.data.len() - n..]
            }
            Op::CaseCompare(_, true) => {
                let matcher = top().unwrap();
                if self.dynamic(matcher)? {
                    return Ok(runtime);
                }
                return Ok(0);
            }
            Op::Unary("!") => return Ok(0),
            Op::Unary(_) => {
                return Ok(if self.dynamic(top().unwrap())? {
                    u8::MAX
                } else {
                    0
                });
            }
            Op::Index(count) => &state.stack.data[state.stack.data.len() - count - 1..],
            Op::AddressIndex(count) | Op::AddressTarget(count, _) => {
                &state.stack.data[state.stack.data.len() - count..]
            }
            Op::Method(site, count) => {
                let base = state.stack.data.len() - count - 1;
                let receiver = state.stack.data[base].value;
                if builtins::value_member(
                    self.ctx,
                    self.facts,
                    receiver,
                    &self.program.members[site.name],
                )? {
                    return Ok(0);
                }
                if self.dynamic(receiver)? {
                    return Ok(u8::MAX);
                }
                if count == 0 {
                    return Ok(0);
                }
                if matches!(
                    site.method,
                    Some(
                        Method::Itself
                            | Method::Dup
                            | Method::Length
                            | Method::Size
                            | Method::Empty
                            | Method::IsNil
                    )
                ) {
                    return Ok(0);
                }
                &state.stack.data[base + 1..]
            }
            Op::Mutate(site, count) => {
                if builtins::value_member(
                    self.ctx,
                    self.facts,
                    state.addresses.data.last().unwrap().value,
                    &self.program.members[site.name],
                )? {
                    return Ok(0);
                }
                if self.dynamic(state.addresses.data.last().unwrap().value)? {
                    return Ok(u8::MAX);
                }
                if matches!(
                    site.method,
                    Some(Method::Push | Method::Prepend | Method::Clear)
                ) {
                    return Ok(0);
                }
                &state.stack.data[state.stack.data.len() - count..]
            }
            Op::Invoke(Invocation::Member(site, addressed)) => {
                let receiver = if addressed {
                    state.addresses.data.last().map(|a| a.value)
                } else {
                    top()
                };
                if let Some(receiver) = receiver {
                    if builtins::value_member(
                        self.ctx,
                        self.facts,
                        receiver,
                        &self.program.members[site.name],
                    )? {
                        return Ok(0);
                    }
                    if self.dynamic(receiver)? {
                        return Ok(u8::MAX);
                    }
                }
                let args = &state.arguments.data.last().unwrap().arguments;
                self.ctx.charge(args.positional.data.len() as u64)?;
                return Ok(
                    if args
                        .positional
                        .data
                        .iter()
                        .any(|&v| uncertain(self.facts, v))
                    {
                        runtime
                    } else {
                        0
                    },
                );
            }
            Op::Argument(ArgumentOp::Splat | ArgumentOp::KeywordSplat) => {
                return Ok(if self.dynamic(top().unwrap())? {
                    runtime
                } else {
                    0
                });
            }
            Op::LoopStart { iterable: true, .. } => {
                let value = top().unwrap();
                return Ok(if self.dynamic(value)? {
                    u8::MAX
                } else if value == Atom::Range.fact() {
                    runtime
                } else {
                    0
                });
            }
            _ => return Ok(0),
        };
        self.ctx.charge(values.len() as u64)?;
        let mut errors = 0;
        for operand in values {
            if self.dynamic(operand.value)? {
                return Ok(u8::MAX);
            }
            if uncertain(self.facts, operand.value) {
                errors |= runtime;
            }
        }
        Ok(errors)
    }

    pub(super) fn binary_errors(
        &mut self,
        op: &str,
        left: Fact,
        right: Fact,
        rejected: bool,
    ) -> Result<(u8, bool)> {
        let runtime = handlers::bit(ErrorClass::Runtime);
        let zero = handlers::bit(ErrorClass::ZeroDivision);
        let mut errors = if rejected {
            handlers::bit(if matches!(op, "<" | "<=" | ">" | ">=") {
                ErrorClass::Argument
            } else {
                ErrorClass::Runtime
            })
        } else {
            0
        };
        if self.dynamic(left)? || self.dynamic(right)? {
            return Ok((u8::MAX, false));
        }
        let mut all_stop = true;
        for a in 0..self.facts.arm_count(left) {
            for b in 0..self.facts.arm_count(right) {
                self.ctx.charge(1)?;
                let a = self.facts.arm(left, a);
                let b = self.facts.arm(right, b);
                let (x, y) = (self.facts.atom(a), self.facts.atom(b));
                let mut stops = false;
                if matches!(op, "/" | "%") && x == Some(Atom::Int) && y == Some(Atom::Int) {
                    match self.facts.node(b) {
                        Node::Integer(0) => {
                            errors |= zero;
                            stops = true;
                        }
                        Node::Integer(_) => (),
                        _ => errors |= zero,
                    }
                }
                if x == Some(Atom::Money)
                    && y == Some(Atom::Money)
                    && matches!(op, "<" | "<=" | ">" | ">=")
                {
                    errors |= handlers::bit(ErrorClass::Argument);
                }
                if matches!(op, "+" | "-" | "*" | "/" | "%")
                    && (matches!(x, Some(Atom::Time | Atom::Duration | Atom::Money))
                        || matches!(y, Some(Atom::Time | Atom::Duration | Atom::Money)))
                    && x != Some(Atom::String)
                    && y != Some(Atom::String)
                {
                    errors |= runtime;
                    if x == Some(Atom::Duration) && matches!(op, "/" | "%") {
                        let known = match self.facts.node(b) {
                            Node::Integer(n) => Some(*n == 0),
                            Node::Float(bits) => Some(f64::from_bits(*bits) == 0.0),
                            _ => None,
                        };
                        if known != Some(false) {
                            errors |= zero;
                        }
                        stops |= known == Some(true);
                    }
                    if x == Some(Atom::Money)
                        && op == "/"
                        && matches!(self.facts.node(b), Node::Integer(0))
                    {
                        stops = true;
                    }
                }
                if matches!(op, "=~" | "!~")
                    && ((x == Some(Atom::Regex) && y == Some(Atom::String))
                        || (x == Some(Atom::String) && y == Some(Atom::Regex)))
                {
                    errors |= handlers::bit(ErrorClass::Limit);
                }
                if op == "**" {
                    errors |= runtime | handlers::bit(ErrorClass::Limit);
                }
                if op == "%" && x == Some(Atom::String) {
                    errors |= runtime
                        | handlers::bit(ErrorClass::Argument)
                        | handlers::bit(ErrorClass::Limit);
                }
                if op == "*" && x == Some(Atom::String) && y == Some(Atom::Int) {
                    match self.facts.node(b) {
                        Node::Integer(n) if *n < 0 => {
                            errors |= runtime;
                            stops = true;
                        }
                        Node::Integer(_) => (),
                        _ => errors |= runtime,
                    }
                }
                all_stop &= stops;
            }
        }
        Ok((errors, all_stop))
    }
}

fn uncertain(facts: &Facts, value: Fact) -> bool {
    if let Node::Float(bits) = facts.node(value) {
        let value = f64::from_bits(*bits);
        !value.is_finite() || value < i64::MIN as f64 || value >= -(i64::MIN as f64)
    } else {
        !facts.singleton(value)
    }
}
