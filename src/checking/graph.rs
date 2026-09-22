use crate::{CallContext, Result, budget::Buffer, bytecode::Op};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Exit {
    Next,
    Jump(usize),
    Branch(usize),
    Stop,
}

#[derive(Debug)]
pub(super) struct Block {
    pub start: usize,
    pub end: usize,
    pub exit: Exit,
}

#[derive(Debug)]
pub(super) struct Graph {
    pub blocks: Buffer<Block>,
    // The block starting at each instruction, for the edges that every walk resolves.
    starts: Buffer<usize>,
}

impl Graph {
    pub fn new(ctx: &mut CallContext, code: &[Op]) -> Result<Self> {
        Self::build(ctx, code, false)
    }

    pub fn file(ctx: &mut CallContext, code: &[Op]) -> Result<Self> {
        Self::build(ctx, code, true)
    }

    fn build(ctx: &mut CallContext, code: &[Op], file: bool) -> Result<Self> {
        let mut leaders = Buffer::with_capacity(ctx, code.len() + 1)?;
        let mut exits = Buffer::with_capacity(ctx, code.len())?;
        let mut loops = Buffer::empty();
        for _ in 0..=code.len() {
            ctx.charge(1)?;
            leaders.data.push(false);
        }
        leaders.data[0] = true;
        leaders.data[code.len()] = true;
        for (pc, &op) in code.iter().enumerate() {
            ctx.charge(1)?;
            if file && super::file_bindings::branches(op) {
                leaders.data[pc] = true;
                leaders.data[pc + 1] = true;
            }
            let exit = match op {
                Op::Jump(target) => Exit::Jump(target),
                Op::JumpFalse(target)
                | Op::JumpTrue(target)
                | Op::JumpNil(target)
                | Op::AddressJumpNil(target, _)
                | Op::Bind(_, target)
                | Op::ReceiverBound(_, target)
                | Op::AddressBound(_, target)
                | Op::NamespaceConstant(_, target)
                | Op::NamespaceConstantAddress(_, target)
                | Op::AmbientValue(_, target)
                | Op::AmbientAddress(_, target)
                | Op::FileValue(_, target)
                | Op::FileAddress(_, target)
                | Op::RootAddress(_, target)
                | Op::TypeShadowed(_, target)
                | Op::RaiseStart(_, target) => Exit::Branch(target),
                Op::LoopStart { next, end, .. } => {
                    loops.push(ctx, (next, end))?;
                    leaders.data[next] = true;
                    leaders.data[end] = true;
                    Exit::Next
                }
                Op::LoopTest | Op::IterNext => Exit::Branch(loops.data.last().unwrap().1),
                Op::LoopBody => Exit::Jump(loops.data.last().unwrap().0),
                Op::LoopEnd => {
                    loops.data.pop().unwrap();
                    Exit::Next
                }
                Op::Break(_) => loops
                    .data
                    .last()
                    .map_or(Exit::Stop, |(_, end)| Exit::Jump(*end)),
                Op::Next(_) => loops
                    .data
                    .last()
                    .map_or(Exit::Stop, |(next, _)| Exit::Jump(*next)),
                Op::TryBegin(_)
                | Op::TryBody
                | Op::TryEnd
                | Op::EnsureEnd
                | Op::Yield(_)
                | Op::Method(_, _)
                | Op::CallMember(_)
                | Op::PrepareMember(..)
                | Op::CallValue
                | Op::TextPart
                | Op::ResolveCall(..)
                | Op::ResolveGlobalCall(_)
                | Op::CallName(..)
                | Op::RootCall(..)
                | Op::Mutate(_, _)
                | Op::AddressMember(_)
                | Op::AddressMemberTarget(..)
                | Op::AddressNamespaceField(_)
                | Op::NamespaceAddress(..)
                | Op::InitNamespace(_)
                | Op::Binary(_)
                | Op::AddStore(_)
                | Op::Index(_)
                | Op::Shovel(_)
                | Op::AddressIndex(_)
                | Op::AddressTarget(..)
                | Op::AddressStore
                | Op::Invoke(_)
                | Op::InvokeRoot(_) => {
                    leaders.data[pc + 1] = true;
                    Exit::Next
                }
                Op::Return | Op::Finish | Op::Raise(_) | Op::Retry => Exit::Stop,
                _ => Exit::Next,
            };
            if exit != Exit::Next {
                leaders.data[pc + 1] = true;
            }
            if let Exit::Jump(target) | Exit::Branch(target) = exit {
                leaders.data[target] = true;
            }
            exits.data.push(exit);
        }
        assert!(loops.data.is_empty());
        let mut blocks = Buffer::empty();
        let mut starts = Buffer::with_capacity(ctx, code.len() + 1)?;
        starts.data.resize(code.len() + 1, usize::MAX);
        starts.data[0] = 0;
        let mut start = 0;
        for end in 1..=code.len() {
            ctx.charge(1)?;
            if leaders.data[end] {
                blocks.push(
                    ctx,
                    Block {
                        start,
                        end,
                        exit: exits.data[end - 1],
                    },
                )?;
                starts.data[end] = blocks.data.len();
                start = end;
            }
        }
        Ok(Self { blocks, starts })
    }

    pub fn at(&self, ctx: &mut CallContext, pc: usize) -> Result<usize> {
        ctx.charge(1)?;
        match self.starts.data.get(pc) {
            Some(&index) if index < self.blocks.data.len() => Ok(index),
            _ => panic!("jump target {pc} is not a basic block"),
        }
    }
}
