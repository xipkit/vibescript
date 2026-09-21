use super::*;
use crate::checking::blocks::{Capture, Closure, Layer, Parent};
use crate::checking::globals::Globals;
use crate::checking::pending::Pending;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum Kind {
    Entry { general: bool, admit: bool },
    General,
    Plain,
    Initializing,
    Receiving { function: CallableId, given: bool },
    Invoked { given: bool },
}

pub(super) struct Context {
    pub scope: blocks::Scope,
    pub block_scope: blocks::Scope,
    pub kind: Kind,
    pub receiver: Option<Fact>,
    pub block_receiver: Option<Fact>,
    pub ambient: Option<CallableId>,
    pub block_ambient: Option<CallableId>,
    pub constructor: bool,
    pub globals: Globals,
    pub locals: usize,
    pub inherited: Buffer<Layer>,
    pub captures: Buffer<Capture>,
    pub arguments: Buffer<Fact>,
    pub pending: Pending,
}

impl Context {
    pub fn plain() -> Self {
        Self {
            scope: blocks::Scope::Invocation,
            block_scope: blocks::Scope::Invocation,
            kind: Kind::Plain,
            receiver: None,
            block_receiver: None,
            ambient: None,
            block_ambient: None,
            constructor: false,
            globals: Globals::empty(),
            locals: 0,
            inherited: Buffer::empty(),
            captures: Buffer::empty(),
            arguments: Buffer::empty(),
            pending: Pending::new(),
        }
    }

    pub fn receiving(ctx: &mut CallContext, block: &Closure) -> Result<Self> {
        ctx.charge(1)?;
        let mut result = Self::plain();
        result.kind = Kind::Receiving {
            function: block.function,
            given: block.given,
        };
        result.block_receiver = block.receiver;
        result.block_scope = block.scope;
        result.block_ambient = block.ambient;
        result.pending = block.pending.snapshot(ctx)?;
        result.locals = block.locals;
        result.inherited.extend(ctx, &block.inherited.data)?;
        for link in &block.captures.data {
            ctx.charge(1)?;
            result.captures.push(
                ctx,
                Capture {
                    slot: link.slot,
                    value: link.value,
                    missing: link.missing,
                    owner: link.owner,
                },
            )?;
        }
        Ok(result)
    }

    pub fn snapshot(&self, ctx: &mut CallContext) -> Result<Self> {
        ctx.charge(1)?;
        let mut next = Self::plain();
        next.kind = self.kind;
        next.scope = self.scope;
        next.block_scope = self.block_scope;
        next.receiver = self.receiver;
        next.block_receiver = self.block_receiver;
        next.ambient = self.ambient;
        next.block_ambient = self.block_ambient;
        next.constructor = self.constructor;
        next.globals = self.globals.snapshot(ctx)?;
        next.pending = self.pending.snapshot(ctx)?;
        next.locals = self.locals;
        next.inherited.extend(ctx, &self.inherited.data)?;
        next.captures.extend(ctx, &self.captures.data)?;
        next.arguments.extend(ctx, &self.arguments.data)?;
        Ok(next)
    }

    pub fn hash(&self, ctx: &mut CallContext, hash: &mut impl Hasher) -> Result<()> {
        ctx.charge((self.captures.data.len() + self.arguments.data.len()) as u64 + 1)?;
        self.pending.hash(ctx, hash)?;
        self.globals.hash(ctx, hash)?;
        self.kind.hash(hash);
        self.scope.hash(hash);
        self.block_scope.hash(hash);
        self.receiver.hash(hash);
        self.block_receiver.hash(hash);
        self.ambient.hash(hash);
        self.block_ambient.hash(hash);
        self.constructor.hash(hash);
        ctx.charge(self.inherited.data.len() as u64 + 1)?;
        self.locals.hash(hash);
        self.inherited.data.hash(hash);
        self.captures.data.hash(hash);
        self.arguments.data.hash(hash);
        Ok(())
    }

    pub fn equal(&self, ctx: &mut CallContext, other: &Self) -> Result<bool> {
        ctx.charge((self.captures.data.len() + self.arguments.data.len()) as u64 + 1)?;
        ctx.charge(self.inherited.data.len() as u64 + 1)?;
        Ok(self.kind == other.kind
            && self.scope == other.scope
            && self.block_scope == other.block_scope
            && self.receiver == other.receiver
            && self.block_receiver == other.block_receiver
            && self.ambient == other.ambient
            && self.block_ambient == other.block_ambient
            && self.constructor == other.constructor
            && self.locals == other.locals
            && self.inherited.data == other.inherited.data
            && self.captures.data == other.captures.data
            && self.arguments.data == other.arguments.data
            && self.pending.equal(ctx, &other.pending)?
            && self.globals.equal(ctx, &other.globals)?)
    }

    pub fn compatible(&self, ctx: &mut CallContext, other: &Self) -> Result<bool> {
        ctx.charge(self.inherited.data.len() as u64 + 1)?;
        if self.kind != other.kind
            || self.scope != other.scope
            || self.block_scope != other.block_scope
            || self.receiver != other.receiver
            || self.block_receiver != other.block_receiver
            || self.ambient != other.ambient
            || self.block_ambient != other.block_ambient
            || self.constructor != other.constructor
            || self.locals != other.locals
            || self.inherited.data != other.inherited.data
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
        Ok(self.pending.compatible(ctx, &other.pending)?
            && self.globals.compatible(ctx, &other.globals)?)
    }

    pub fn expands(&self, ctx: &mut CallContext, next: &Self) -> Result<bool> {
        ctx.charge(self.inherited.data.len() as u64 + 1)?;
        Ok(self.kind == next.kind
            && self.scope == next.scope
            && self.block_scope == next.block_scope
            && self.receiver == next.receiver
            && self.block_receiver == next.block_receiver
            && self.ambient == next.ambient
            && self.block_ambient == next.block_ambient
            && self.constructor == next.constructor
            && self.locals == next.locals
            && ((self.inherited.data.len() < next.inherited.data.len()
                && next.inherited.data.ends_with(&self.inherited.data))
                || self.globals.pending.addresses.data.len()
                    < next.globals.pending.addresses.data.len()))
    }

    pub fn widen(
        &mut self,
        ctx: &mut CallContext,
        facts: &mut Facts,
        other: &Self,
        depth: usize,
    ) -> Result<bool> {
        let mut changed = self.pending.join(ctx, facts, &other.pending, Some(depth))?;
        changed |= self.globals.join(ctx, facts, &other.globals, Some(depth))?;
        for (a, b) in self.captures.data.iter_mut().zip(&other.captures.data) {
            ctx.charge(1)?;
            let value = facts.widen(ctx, a.value, b.value, depth)?;
            let owner = a.owner.join(a.value, b.owner, b.value);
            changed |= a.value != value || (!a.missing && b.missing) || a.owner != owner;
            a.value = value;
            a.missing |= b.missing;
            a.owner = owner;
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
        let (scope, function, receiver, ambient, given, locals, inherited, base) = match self.kind {
            Kind::Plain | Kind::General | Kind::Initializing | Kind::Entry { .. } => {
                return Ok(None);
            }
            Kind::Receiving { function, given } => (
                self.block_scope,
                function,
                self.block_receiver,
                self.block_ambient,
                given,
                self.locals,
                &self.inherited.data[..],
                0,
            ),
            Kind::Invoked { .. } => {
                let Some((first, inherited)) = self.inherited.data.split_first() else {
                    return Ok(None);
                };
                (
                    first.scope,
                    first.function,
                    first.receiver,
                    first.ambient,
                    first.given,
                    first.locals,
                    inherited,
                    self.locals,
                )
            }
        };
        let mut layers = Buffer::empty();
        layers.extend(ctx, inherited)?;
        let mut captures = Buffer::empty();
        for capture in &self.captures.data {
            ctx.charge(1)?;
            if capture.slot < base {
                continue;
            }
            captures.push(
                ctx,
                super::blocks::Link {
                    slot: capture.slot - base,
                    parent: Parent::Local(usize::MAX),
                    value: capture.value,
                    missing: capture.missing,
                    owner: capture.owner,
                },
            )?;
        }
        let mut pending = Pending::new();
        for address in &self.pending.addresses.data {
            ctx.charge(1)?;
            let root = address.root.unwrap();
            if root < base {
                continue;
            }
            let mut address = address.snapshot(ctx)?;
            address.root = Some(root - base);
            pending.addresses.push(ctx, address)?;
        }
        Ok(Some(Closure {
            scope,
            receiver,
            ambient,
            pending,
            destinations: Buffer::empty(),
            function,
            given,
            locals,
            inherited: layers,
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

    fn identities(ctx: &mut CallContext, facts: &mut Facts) -> [CallableId; 2] {
        std::array::from_fn(|_| {
            let code = crate::code::Code::compile("def run;7;end", &Default::default()).unwrap();
            let owner = facts.source_owner(ctx, &code, None).unwrap();
            facts
                .source_id(ctx, owner)
                .unwrap()
                .callable(code.program.names["run"])
        })
    }

    fn closure(ctx: &mut CallContext, function: CallableId) -> Closure {
        let mut captures = Buffer::empty();
        captures
            .push(
                ctx,
                Link {
                    slot: 0,
                    parent: Parent::Local(0),
                    value: Atom::Int.fact(),
                    missing: false,
                    owner: blocks::Owner::Function(function),
                },
            )
            .unwrap();
        Closure {
            scope: blocks::Scope::Invocation,
            function,
            receiver: None,
            ambient: Some(function),
            given: false,
            locals: 1,
            inherited: Buffer::empty(),
            captures,
            pending: Pending::new(),
            destinations: Buffer::empty(),
        }
    }

    #[test]
    fn incoming_and_forwarded_blocks_preserve_source_homes_and_ambient_bindings() {
        let mut ctx = CallContext::new(CallOptions::default());
        let mut facts = Facts::new(&mut ctx).unwrap();
        let [a, b] = identities(&mut ctx, &mut facts);
        assert_eq!(a.index, b.index);
        let first = closure(&mut ctx, a);
        let second = closure(&mut ctx, b);
        let mut context = Context::receiving(&mut ctx, &first).unwrap();
        let other = Context::receiving(&mut ctx, &second).unwrap();
        assert!(!context.equal(&mut ctx, &other).unwrap());
        assert!(!context.compatible(&mut ctx, &other).unwrap());
        let copied = context.incoming(&mut ctx).unwrap().unwrap();
        assert_eq!(copied.function, a);
        assert_eq!(copied.ambient, Some(a));
        assert_eq!(copied.captures.data[0].owner, blocks::Owner::Function(a));

        context.kind = Kind::Invoked { given: false };
        context
            .inherited
            .push(
                &mut ctx,
                Layer {
                    scope: blocks::Scope::Invocation,
                    function: b,
                    receiver: None,
                    ambient: Some(b),
                    given: false,
                    locals: 1,
                },
            )
            .unwrap();
        context
            .captures
            .push(
                &mut ctx,
                Capture {
                    slot: 1,
                    value: Atom::String.fact(),
                    missing: false,
                    owner: blocks::Owner::Function(b),
                },
            )
            .unwrap();
        let forwarded = context.incoming(&mut ctx).unwrap().unwrap();
        assert_eq!(forwarded.function, b);
        assert_eq!(forwarded.ambient, Some(b));
        assert_eq!(forwarded.captures.data.len(), 1);
        assert_eq!(forwarded.captures.data[0].slot, 0);
        assert_eq!(forwarded.captures.data[0].owner, blocks::Owner::Function(b));
        let mut changed = context.snapshot(&mut ctx).unwrap();
        changed.inherited.data[0].function = a;
        assert!(!context.equal(&mut ctx, &changed).unwrap());
        assert!(!context.compatible(&mut ctx, &changed).unwrap());
        drop((
            context, other, copied, forwarded, changed, first, second, facts,
        ));
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }

    #[test]
    fn widening_capture_owners_does_not_merge_equal_indexes_from_different_sources() {
        let mut ctx = CallContext::new(CallOptions::default());
        let mut facts = Facts::new(&mut ctx).unwrap();
        let [a, b] = identities(&mut ctx, &mut facts);
        let mut first = Context::plain();
        first
            .captures
            .push(
                &mut ctx,
                Capture {
                    slot: 0,
                    value: Atom::Int.fact(),
                    missing: false,
                    owner: blocks::Owner::Function(a),
                },
            )
            .unwrap();
        let mut second = first.snapshot(&mut ctx).unwrap();
        second.captures.data[0].owner = blocks::Owner::Function(b);
        assert!(!first.equal(&mut ctx, &second).unwrap());
        let depth = facts.max_depth();
        assert!(first.widen(&mut ctx, &mut facts, &second, depth).unwrap());
        assert_eq!(first.captures.data[0].owner, blocks::Owner::Unknown);
        assert_eq!(first.captures.data[0].value, Atom::Int.fact());
        drop((first, second, facts));
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }

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
                            parent: Parent::Local(slot),
                            value: Atom::Int.fact(),
                            missing: false,
                            owner: super::super::blocks::Owner::Unknown,
                        },
                    )
                    .unwrap();
            }
            let closure = Closure {
                scope: blocks::Scope::Invocation,
                receiver: None,
                ambient: None,
                pending: Pending::new(),
                destinations: Buffer::empty(),
                function: SourceId::ROOT.callable(1),
                given: false,
                locals: count,
                inherited: Buffer::empty(),
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
                            globals: Globals::empty(),
                            pending: Pending::new(),
                            pc,
                            completion: Completion::Value,
                            value: Atom::Int.fact(),
                            captures: Slots::new(0, Atom::Never.fact()),
                            written: Slots::new(0, false),
                            refined: Slots::new(0, false),
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
                            scope: blocks::Scope::Invocation,
                            receiver: None,
                            ambient: None,
                            pending: Pending::new(),
                            destinations: Buffer::empty(),
                            function: SourceId::ROOT.callable(1),
                            given: false,
                            locals: 0,
                            inherited: Buffer::empty(),
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
