use super::*;
use crate::checking::facts::Node;

impl Walker<'_> {
    fn dynamic(&mut self, value: Fact) -> Result<bool> {
        self.ctx.charge(self.facts.arm_count(value) as u64)?;
        Ok((0..self.facts.arm_count(value)).any(|i| {
            matches!(
                self.facts.node(self.facts.arm(value, i)),
                Node::Atom(Atom::Unknown | Atom::Any) | Node::Named(_) | Node::Nominal { .. }
            )
        }))
    }

    pub(super) fn potential_errors(&mut self, state: &State, op: Op) -> Result<u8> {
        let runtime = handlers::bit(ErrorClass::Runtime);
        let top = || state.stack.data.last().map(|v| v.value);
        let values = match op {
            Op::Normalize(ty, _) => {
                return Ok(
                    if self
                        .facts
                        .relation(self.ctx, top().unwrap(), self.contracts[ty])?
                        == Relation::Accepted
                    {
                        0
                    } else {
                        runtime
                    },
                );
            }
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
            Op::Invoke(Invocation::Member(_, addressed)) => {
                let receiver = if addressed {
                    state.addresses.data.last().map(|a| a.value)
                } else {
                    top()
                };
                if let Some(receiver) = receiver {
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
                if matches!(x, Some(Atom::Duration | Atom::Time | Atom::Money))
                    || matches!(y, Some(Atom::Duration | Atom::Time | Atom::Money))
                {
                    errors |= runtime | zero;
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
