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
    /// Whether any edge can lead back to an earlier block, through a loop or `retry`.
    pub cyclic: bool,
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
        let mut cyclic = false;
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
                | Op::ImplicitAddress(_, target)
                | Op::FileValue(_, target, _)
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
                cyclic |= target <= pc;
            }
            cyclic |= matches!(op, Op::Retry);
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
        Ok(Self {
            blocks,
            cyclic,
            starts,
        })
    }

    pub fn at(&self, ctx: &mut CallContext, pc: usize) -> Result<usize> {
        ctx.charge(1)?;
        match self.starts.data.get(pc) {
            Some(&index) if index < self.blocks.data.len() => Ok(index),
            _ => panic!("jump target {pc} is not a basic block"),
        }
    }
}

/// Queued block entries of one function walk.
///
/// Without a cycle every edge leads to a later block, so taking the earliest queued block first
/// walks each block after all of its predecessors. The latest queued entry, taken otherwise,
/// would walk a chain of branch joins again for every branch that later reaches it. Loop and
/// `retry` walks keep that order, since their widening depends on the order of the states that
/// reach a backedge.
#[derive(Debug)]
pub(super) struct Worklist {
    heap: Buffer<(usize, usize)>,
    ordered: bool,
}

impl Worklist {
    pub fn new(graph: &Graph) -> Self {
        Self {
            heap: Buffer::empty(),
            ordered: !graph.cyclic,
        }
    }

    pub fn push(&mut self, ctx: &mut CallContext, entry: (usize, usize)) -> Result<()> {
        self.heap.push(ctx, entry)?;
        if !self.ordered {
            return Ok(());
        }
        let heap = &mut self.heap.data;
        let mut child = heap.len() - 1;
        while child > 0 {
            ctx.charge(1)?;
            let parent = (child - 1) / 2;
            if heap[parent] <= heap[child] {
                break;
            }
            heap.swap(parent, child);
            child = parent;
        }
        Ok(())
    }

    pub fn pop(&mut self, ctx: &mut CallContext) -> Result<Option<(usize, usize)>> {
        let heap = &mut self.heap.data;
        if !self.ordered || heap.is_empty() {
            return Ok(heap.pop());
        }
        let first = heap.swap_remove(0);
        let mut parent = 0;
        loop {
            ctx.charge(1)?;
            let (left, right) = (2 * parent + 1, 2 * parent + 2);
            let mut least = parent;
            if left < heap.len() && heap[left] < heap[least] {
                least = left;
            }
            if right < heap.len() && heap[right] < heap[least] {
                least = right;
            }
            if least == parent {
                return Ok(Some(first));
            }
            heap.swap(parent, least);
            parent = least;
        }
    }
}
