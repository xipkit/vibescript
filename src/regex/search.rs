use super::{
    Assertion,
    program::{Op, View},
    unicode,
};
use crate::{CallContext, Result, budget::Buffer, scan};

pub(super) const ABSENT: usize = usize::MAX;

struct Thread {
    pc: usize,
    captures: Buffer<usize>,
}

enum Action {
    Visit(usize),
    Restore(usize, usize),
}

struct Boundary {
    position: usize,
    before: Option<u32>,
    after: Option<u32>,
}

pub(super) struct Search {
    current: Buffer<Thread>,
    next: Buffer<Thread>,
    visited: Buffer<u32>,
    actions: Buffer<Action>,
    pool: Buffer<Buffer<usize>>,
    epoch: u32,
    slots: usize,
}

impl Search {
    pub fn new(ctx: &mut CallContext, program: View<'_>, captures: bool) -> Result<Self> {
        let mut visited = Buffer::with_capacity(ctx, program.instructions.len())?;
        for chunk in program.instructions.chunks(4096 / size_of::<u32>()) {
            ctx.work_bytes(chunk.len() * size_of::<u32>())?;
            visited.data.resize(visited.data.len() + chunk.len(), 0);
        }
        Ok(Self {
            current: Buffer::empty(),
            next: Buffer::empty(),
            visited,
            actions: Buffer::empty(),
            pool: Buffer::empty(),
            epoch: 0,
            slots: if captures { 2 * program.names.len() } else { 2 },
        })
    }

    fn advance_epoch(&mut self, ctx: &mut CallContext) -> Result<()> {
        self.epoch = self.epoch.wrapping_add(1);
        if self.epoch == 0 {
            for chunk in self.visited.data.chunks_mut(1024) {
                ctx.work_bytes(std::mem::size_of_val(chunk))?;
                chunk.fill(0);
            }
            self.epoch = 1;
        }
        Ok(())
    }

    fn captures(&mut self, ctx: &mut CallContext) -> Result<Buffer<usize>> {
        let mut captures = self.pool.data.pop().unwrap_or_else(Buffer::empty);
        captures.ensure(ctx, self.slots)?;
        captures.data.clear();
        for start in (0..self.slots).step_by(512) {
            let count = (self.slots - start).min(512);
            ctx.work_bytes(count * size_of::<usize>())?;
            captures.data.resize(start + count, ABSENT);
        }
        Ok(captures)
    }

    fn add(
        &mut self,
        ctx: &mut CallContext,
        program: View<'_>,
        pc: usize,
        mut captures: Buffer<usize>,
        boundary: Boundary,
    ) -> Result<()> {
        let Boundary {
            position,
            before,
            after,
        } = boundary;
        self.actions.push(ctx, Action::Visit(pc))?;
        while let Some(action) = self.actions.data.pop() {
            ctx.charge(1)?;
            match action {
                Action::Restore(slot, value) => captures.data[slot] = value,
                Action::Visit(pc) => {
                    if self.visited.data[pc] == self.epoch {
                        continue;
                    }
                    self.visited.data[pc] = self.epoch;
                    let instruction = program.instructions[pc];
                    match instruction.op {
                        Op::Fail => (),
                        Op::Split => {
                            self.actions
                                .push(ctx, Action::Visit(instruction.alternate))?;
                            self.actions.push(ctx, Action::Visit(instruction.next))?;
                        }
                        Op::Save(slot) => {
                            if slot < self.slots {
                                self.actions
                                    .push(ctx, Action::Restore(slot, captures.data[slot]))?;
                                captures.data[slot] = position;
                            }
                            self.actions.push(ctx, Action::Visit(instruction.next))?;
                        }
                        Op::Assert(assertion) => {
                            if assertion.matches(position, before, after) {
                                self.actions.push(ctx, Action::Visit(instruction.next))?;
                            }
                        }
                        _ => {
                            let mut saved = self.pool.data.pop().unwrap_or_else(Buffer::empty);
                            saved.data.clear();
                            saved.extend(ctx, &captures.data)?;
                            self.next.push(
                                ctx,
                                Thread {
                                    pc,
                                    captures: saved,
                                },
                            )?;
                        }
                    }
                }
            }
        }
        self.pool.push(ctx, captures)
    }

    pub fn find(
        &mut self,
        ctx: &mut CallContext,
        program: View<'_>,
        text: &[u8],
        from: usize,
    ) -> Result<Option<Buffer<usize>>> {
        self.current.data.clear();
        self.next.data.clear();
        if from > text.len() {
            return Ok(None);
        }
        self.advance_epoch(ctx)?;
        let mut position = from;
        let mut before = if from == 0 {
            None
        } else {
            Some(previous(text, from))
        };
        let mut best = None;
        loop {
            ctx.checkpoint()?;
            let (after, width) = if position < text.len() {
                let (rune, width, _) = scan::rune(&text[position..]);
                (Some(rune as u32), width)
            } else {
                (None, 0)
            };
            if best.is_none() {
                let mut captures = self.captures(ctx)?;
                captures.data[0] = position;
                self.add(
                    ctx,
                    program,
                    program.start,
                    captures,
                    Boundary {
                        position,
                        before,
                        after,
                    },
                )?;
            }
            std::mem::swap(&mut self.current, &mut self.next);
            self.advance_epoch(ctx)?;
            let following = if position + width < text.len() {
                Some(scan::rune(&text[position + width..]).0 as u32)
            } else {
                None
            };
            let mut current = std::mem::replace(&mut self.current, Buffer::empty());
            let mut matched = false;
            for mut thread in current.data.drain(..) {
                ctx.charge(1)?;
                if matched {
                    self.pool.push(ctx, thread.captures)?;
                    continue;
                }
                let instruction = program.instructions[thread.pc];
                if matches!(instruction.op, Op::Match) {
                    thread.captures.data[1] = position;
                    if let Some(old) = best.replace(thread.captures) {
                        self.pool.push(ctx, old)?;
                    }
                    matched = true;
                    continue;
                }
                let accepts = match (after, instruction.op) {
                    (Some(rune), Op::Rune(expected, fold)) => {
                        unicode::folded(rune, fold, |point| point == expected)
                    }
                    (Some(rune), Op::Class(class)) => class.matches(ctx, program.parts, rune)?,
                    (Some(rune), Op::Any(newline)) => newline || rune != '\n' as u32,
                    _ => false,
                };
                if accepts {
                    self.add(
                        ctx,
                        program,
                        instruction.next,
                        thread.captures,
                        Boundary {
                            position: position + width,
                            before: after,
                            after: following,
                        },
                    )?;
                } else {
                    self.pool.push(ctx, thread.captures)?;
                }
            }
            self.current = current;
            if position == text.len() || (best.is_some() && self.next.data.is_empty()) {
                return Ok(best);
            }
            position += width;
            before = after;
        }
    }
}

fn previous(bytes: &[u8], position: usize) -> u32 {
    let mut start = position - 1;
    while start > position.saturating_sub(4) && bytes[start] & 0xc0 == 0x80 {
        start -= 1;
    }
    let (rune, width, valid) = scan::rune(&bytes[start..position]);
    if valid && start + width == position {
        rune as u32
    } else {
        0xfffd
    }
}

impl Assertion {
    fn matches(self, position: usize, before: Option<u32>, after: Option<u32>) -> bool {
        let word = |rune: Option<u32>| {
            rune.is_some_and(|r| r < 128 && ((r as u8).is_ascii_alphanumeric() || r == '_' as u32))
        };
        match self {
            Self::BeginText => position == 0,
            Self::EndText => after.is_none(),
            Self::BeginLine => before.is_none() || before == Some('\n' as u32),
            Self::EndLine => after.is_none() || after == Some('\n' as u32),
            Self::Word => word(before) != word(after),
            Self::NotWord => word(before) == word(after),
        }
    }
}
