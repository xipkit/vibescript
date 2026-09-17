use super::*;
use crate::checking::blocks::{Capture, Closure};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum Kind {
    Plain,
    Receiving { function: usize, given: bool },
    Invoked { given: bool },
}

pub(super) struct Context {
    pub kind: Kind,
    pub captures: Buffer<Capture>,
    pub arguments: Buffer<Fact>,
}

impl Context {
    pub fn plain() -> Self {
        Self {
            kind: Kind::Plain,
            captures: Buffer::empty(),
            arguments: Buffer::empty(),
        }
    }

    pub fn receiving(ctx: &mut CallContext, block: &Closure) -> Result<Self> {
        ctx.charge(1)?;
        let mut result = Self::plain();
        result.kind = Kind::Receiving {
            function: block.function,
            given: block.given,
        };
        for link in &block.captures.data {
            ctx.charge(1)?;
            result.captures.push(
                ctx,
                Capture {
                    slot: link.slot,
                    value: link.value,
                },
            )?;
        }
        Ok(result)
    }

    pub fn snapshot(&self, ctx: &mut CallContext) -> Result<Self> {
        let mut next = Self::plain();
        next.kind = self.kind;
        next.captures.extend(ctx, &self.captures.data)?;
        next.arguments.extend(ctx, &self.arguments.data)?;
        Ok(next)
    }

    pub fn hash(&self, ctx: &mut CallContext, hash: &mut impl Hasher) -> Result<()> {
        ctx.charge((self.captures.data.len() + self.arguments.data.len()) as u64 + 1)?;
        self.kind.hash(hash);
        self.captures.data.hash(hash);
        self.arguments.data.hash(hash);
        Ok(())
    }

    pub fn equal(&self, ctx: &mut CallContext, other: &Self) -> Result<bool> {
        ctx.charge((self.captures.data.len() + self.arguments.data.len()) as u64 + 1)?;
        Ok(self.kind == other.kind
            && self.captures.data == other.captures.data
            && self.arguments.data == other.arguments.data)
    }

    pub fn compatible(&self, ctx: &mut CallContext, other: &Self) -> Result<bool> {
        ctx.charge(1)?;
        if self.kind != other.kind
            || self.captures.data.len() != other.captures.data.len()
            || self.arguments.data.len() != other.arguments.data.len()
        {
            return Ok(false);
        }
        for (a, b) in self.captures.data.iter().zip(&other.captures.data) {
            ctx.charge(1)?;
            if a.slot != b.slot {
                return Ok(false);
            }
        }
        Ok(true)
    }

    pub fn widen(
        &mut self,
        ctx: &mut CallContext,
        facts: &mut Facts,
        other: &Self,
        depth: usize,
    ) -> Result<bool> {
        let mut changed = false;
        for (a, b) in self.captures.data.iter_mut().zip(&other.captures.data) {
            ctx.charge(1)?;
            let value = facts.widen(ctx, a.value, b.value, depth)?;
            changed |= a.value != value;
            a.value = value;
        }
        for (a, b) in self.arguments.data.iter_mut().zip(&other.arguments.data) {
            ctx.charge(1)?;
            let value = facts.widen(ctx, *a, *b, depth)?;
            changed |= *a != value;
            *a = value;
        }
        Ok(changed)
    }

    pub fn incoming(&self, ctx: &mut CallContext) -> Result<Option<Closure>> {
        ctx.charge(1)?;
        let Kind::Receiving { function, given } = self.kind else {
            return Ok(None);
        };
        let mut captures = Buffer::empty();
        for capture in &self.captures.data {
            ctx.charge(1)?;
            captures.push(
                ctx,
                super::blocks::Link {
                    slot: capture.slot,
                    parent: usize::MAX,
                    value: capture.value,
                },
            )?;
        }
        Ok(Some(Closure {
            function,
            given,
            captures,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::checking::{
        blocks::{Completion, Exit, Link},
        slots::Slots,
    };
    use crate::{CallOptions, ErrorKind, Limits};

    #[test]
    fn capture_context_copies_charge_linear_work_and_observe_exact_step_limits() {
        for count in [0, 1, 8, 17, 257] {
            let mut owner = CallContext::new(CallOptions::default());
            let mut captures = Buffer::empty();
            for slot in 0..count {
                captures
                    .push(
                        &mut owner,
                        Link {
                            slot,
                            parent: slot,
                            value: Atom::Int.fact(),
                        },
                    )
                    .unwrap();
            }
            let closure = Closure {
                function: 1,
                given: false,
                captures,
            };
            let mut ctx = CallContext::new(CallOptions::default());
            let context = Context::receiving(&mut ctx, &closure).unwrap();
            let copied = context.incoming(&mut ctx).unwrap().unwrap();
            let steps = ctx.stats().steps;
            assert!(steps >= 2 * count as u64 + 2);
            assert_eq!(copied.captures.data.len(), count);
            drop((context, copied));
            assert_eq!(ctx.stats().retained_memory_bytes, 0);
            for limit in [steps - 1, steps] {
                let mut ctx = CallContext::new(CallOptions {
                    limits: Limits {
                        steps: Some(limit),
                        ..Limits::default()
                    },
                    ..CallOptions::default()
                });
                let result = Context::receiving(&mut ctx, &closure)
                    .and_then(|context| context.incoming(&mut ctx));
                assert_eq!(
                    result.as_ref().err().map(|e| e.kind),
                    if limit < steps {
                        Some(ErrorKind::Steps)
                    } else {
                        None
                    }
                );
                drop(result);
                assert_eq!(ctx.stats().retained_memory_bytes, 0);
            }
        }
    }

    #[test]
    fn summary_metadata_fast_paths_observe_exhaustion_cancellation_and_deadlines() {
        for reason in [ErrorKind::Steps, ErrorKind::Cancelled, ErrorKind::Deadline] {
            for operation in 0..5 {
                let mut ctx = CallContext::new(CallOptions::default());
                match reason {
                    ErrorKind::Steps => ctx.options.limits.steps = Some(0),
                    ErrorKind::Cancelled => ctx.options.cancellation.cancel(),
                    ErrorKind::Deadline => ctx.options.deadline = Some(std::time::Instant::now()),
                    _ => unreachable!(),
                }
                let error = match operation {
                    0 => {
                        let exit = |pc| Exit {
                            pc,
                            completion: Completion::Value,
                            value: Atom::Int.fact(),
                            captures: Slots::new(0, Atom::Never.fact()),
                            written: Slots::new(0, false),
                        };
                        exit(1).equal(&mut ctx, &exit(2)).unwrap_err()
                    }
                    1 => Slots::new(0, false)
                        .equal(&mut ctx, &Slots::new(1, false))
                        .unwrap_err(),
                    2 => {
                        let mut other = Context::plain();
                        other.kind = Kind::Invoked { given: false };
                        Context::plain().compatible(&mut ctx, &other).unwrap_err()
                    }
                    3 => Context::plain().incoming(&mut ctx).unwrap_err(),
                    4 => {
                        let closure = Closure {
                            function: 1,
                            given: false,
                            captures: Buffer::empty(),
                        };
                        match Context::receiving(&mut ctx, &closure) {
                            Ok(_) => panic!("ignored {reason:?}"),
                            Err(error) => error,
                        }
                    }
                    _ => unreachable!(),
                };
                assert_eq!(error.kind, reason);
                assert_eq!(ctx.checkpoint().unwrap_err(), error);
                assert_eq!(ctx.stats().retained_memory_bytes, 0);
            }
        }
    }
}
