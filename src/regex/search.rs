use super::{
    Assertion,
    program::{Op, View},
    unicode,
};
use crate::{
    CallContext, Result,
    budget::{Buffer, CHUNK},
    scan,
};

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
    literal: Option<Literal>,
}

/// The runes every match starts with, found by following the program's
/// straight-line prefix. A pattern that is only this literal is matched by
/// scanning for it alone; otherwise the scan skips positions where no match
/// can start while no thread is running. Either way the text is read in
/// linear time, like RE2's literal prefix search.
struct Literal {
    /// Canonical runes: under case folding, the least member of each orbit.
    runes: Buffer<u32>,
    /// Knuth-Morris-Pratt failure links over `runes`.
    table: Buffer<u32>,
    fold: bool,
    /// Whether a match is exactly the literal, with these capture slots
    /// recorded at these rune offsets, in program order.
    complete: bool,
    saves: Buffer<(usize, usize)>,
}

/// Returns the least member of a rune's case-folding orbit, or the rune itself
/// without folding. Two runes match under folding exactly when these agree.
fn canonical(rune: u32, fold: bool) -> u32 {
    if !fold {
        return rune;
    }
    if rune < 128 {
        // The non-ASCII members of the K and S orbits are larger than both letters.
        return (rune as u8).to_ascii_uppercase() as u32;
    }
    let mut least = rune;
    let mut current = unicode::fold(rune);
    while current != rune {
        least = least.min(current);
        current = unicode::fold(current);
    }
    least
}

impl Literal {
    fn new(ctx: &mut CallContext, program: View<'_>) -> Result<Option<Self>> {
        let mut literal = Self {
            runes: Buffer::empty(),
            table: Buffer::empty(),
            fold: false,
            complete: false,
            saves: Buffer::empty(),
        };
        let mut pc = program.start;
        loop {
            ctx.charge(1)?;
            let instruction = program.instructions[pc];
            match instruction.op {
                Op::Save(slot) => literal.saves.push(ctx, (slot, literal.runes.data.len()))?,
                Op::Rune(rune, fold) if literal.runes.data.is_empty() || fold == literal.fold => {
                    literal.fold = fold;
                    literal.runes.push(ctx, canonical(rune, fold))?;
                }
                Op::Match => {
                    literal.complete = true;
                    break;
                }
                _ => break,
            }
            pc = instruction.next;
        }
        if literal.runes.data.is_empty() {
            return Ok(None);
        }
        if !literal.complete {
            literal.saves = Buffer::empty();
        }
        let runes = &literal.runes.data;
        literal.table.ensure(ctx, runes.len())?;
        literal.table.data.push(0);
        let mut matched = 0;
        for i in 1..runes.len() {
            ctx.charge(1)?;
            while matched > 0 && runes[i] != runes[matched] {
                ctx.charge(1)?;
                matched = literal.table.data[matched - 1] as usize;
            }
            if runes[i] == runes[matched] {
                matched += 1;
            }
            literal.table.data.push(matched as u32);
        }
        Ok(Some(literal))
    }

    /// Returns the byte range of the first occurrence starting at or after `from`.
    ///
    /// Decoding, table transitions and moving the start are each counted as
    /// one unit of byte work and charged in chunks.
    fn scan(
        &self,
        ctx: &mut CallContext,
        text: &[u8],
        from: usize,
    ) -> Result<Option<(usize, usize)>> {
        let runes = &self.runes.data;
        let mut start = from;
        let mut position = from;
        let mut matched = 0;
        let mut work = 0;
        while position < text.len() {
            if work >= CHUNK {
                ctx.work_bytes(work)?;
                work = 0;
            }
            let (rune, width, _) = scan::rune(&text[position..]);
            let rune = canonical(rune as u32, self.fold);
            work += width;
            while matched > 0 && rune != runes[matched] {
                let next = self.table.data[matched - 1] as usize;
                // The partial match loses its first `matched - next` runes.
                for _ in next..matched {
                    start += scan::rune(&text[start..]).1;
                }
                work += 2 * (matched - next);
                matched = next;
            }
            position += width;
            if rune == runes[matched] {
                matched += 1;
                if matched == runes.len() {
                    ctx.work_bytes(work)?;
                    return Ok(Some((start, position)));
                }
            } else {
                start = position;
            }
        }
        ctx.work_bytes(work)?;
        Ok(None)
    }
}

impl Search {
    pub fn new(ctx: &mut CallContext, program: View<'_>, captures: bool) -> Result<Self> {
        let mut visited = Buffer::with_capacity(ctx, program.instructions.len())?;
        for chunk in program.instructions.chunks(4096 / size_of::<u32>()) {
            ctx.work_bytes(chunk.len() * size_of::<u32>())?;
            visited.data.resize(visited.data.len() + chunk.len(), 0);
        }
        let literal = Literal::new(ctx, program)?;
        Ok(Self {
            current: Buffer::empty(),
            next: Buffer::empty(),
            visited,
            actions: Buffer::empty(),
            pool: Buffer::empty(),
            epoch: 0,
            slots: if captures { 2 * program.names.len() } else { 2 },
            literal,
        })
    }

    /// Matches a pattern that is exactly its literal, filling captures at the
    /// rune offsets where the program saves them.
    fn find_literal(
        &mut self,
        ctx: &mut CallContext,
        text: &[u8],
        from: usize,
    ) -> Result<Option<Buffer<usize>>> {
        let Some((start, end)) = self.literal.as_ref().unwrap().scan(ctx, text, from)? else {
            return Ok(None);
        };
        let mut captures = self.captures(ctx)?;
        captures.data[0] = start;
        captures.data[1] = end;
        let mut position = start;
        let mut offset = 0;
        for &(slot, runes) in &self.literal.as_ref().unwrap().saves.data {
            ctx.charge(1)?;
            while offset < runes {
                ctx.charge(1)?;
                position += scan::rune(&text[position..]).1;
                offset += 1;
            }
            if slot < self.slots {
                captures.data[slot] = position;
            }
        }
        Ok(Some(captures))
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
        if self
            .literal
            .as_ref()
            .is_some_and(|literal| literal.complete)
        {
            return self.find_literal(ctx, text, from);
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
            if best.is_none() && self.next.data.is_empty() {
                // With no thread running, a match can only start where the literal does.
                if let Some(literal) = &self.literal {
                    let Some((start, _)) = literal.scan(ctx, text, position)? else {
                        return Ok(None);
                    };
                    if start != position {
                        position = start;
                        before = Some(previous(text, start));
                    }
                }
            }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CallOptions, ErrorKind, Limits, Value, regex::Program};

    fn context(steps: Option<u64>) -> CallContext {
        CallContext::new(CallOptions {
            limits: Limits {
                steps,
                ..Limits::default()
            },
            ..CallOptions::default()
        })
    }

    /// Returns every match found by stepping through `text` like `scan`.
    fn matches(
        ctx: &mut CallContext,
        search: &mut Search,
        program: View<'_>,
        text: &[u8],
    ) -> Vec<Vec<usize>> {
        let mut found = Vec::new();
        let mut from = 0;
        while let Some(indices) = search.find(ctx, program, text, from).unwrap() {
            let (start, end) = (indices.data[0], indices.data[1]);
            found.push(indices.data.clone());
            from = if start == end { end + 1 } else { end };
            if from > text.len() {
                break;
            }
        }
        found
    }

    #[test]
    fn literal_scans_agree_with_the_thread_machine() {
        let mut ctx = context(None);
        let pieces: [&[u8]; 12] = [
            b"a",
            b"b",
            b"A",
            b"k",
            b"K",
            b"s",
            "\u{17f}".as_bytes(),
            "\u{212a}".as_bytes(),
            "\u{e9}".as_bytes(),
            "\u{c9}".as_bytes(),
            b"\xff",
            b"\xef\xbf\xbd",
        ];
        let patterns = [
            "a",
            "ab",
            "aab",
            "abab",
            "(a)(b)",
            "a(ab)?",
            "(?i)k",
            "(?i)ks",
            "(?i)\u{e9}a",
            "(?i)aa(b)",
            "(?i)a(?-i)b",
            "ab\\d",
            "a+b",
            "aa[ab]",
            "\u{fffd}a",
            "(a){2}b",
            "(?i)\u{17f}\u{212a}",
            "abc|abd",
            "(ab)(c)?",
        ];
        let mut state = 0x2545_f491_4f6c_dd1du64;
        let mut next = move |bound: usize| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state % bound as u64) as usize
        };
        for pattern in patterns {
            let program =
                Program::compile(&mut ctx, Value::bytes(pattern.as_bytes()), "Regex.match")
                    .unwrap();
            let view = program.view();
            for captures in [false, true] {
                for _ in 0..60 {
                    let mut text = Vec::new();
                    for _ in 0..next(40) {
                        text.extend_from_slice(pieces[next(pieces.len())]);
                    }
                    let mut fast = Search::new(&mut ctx, view, captures).unwrap();
                    let mut plain = Search::new(&mut ctx, view, captures).unwrap();
                    plain.literal = None;
                    assert_eq!(
                        matches(&mut ctx, &mut fast, view, &text),
                        matches(&mut ctx, &mut plain, view, &text),
                        "{pattern} in {text:?}"
                    );
                }
            }
        }
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }

    #[test]
    fn long_literals_scan_in_linear_work() {
        let cost = |length: usize| {
            let mut ctx = context(None);
            let pattern = format!("(?i){}", "a".repeat(length));
            let program = Program::compile_limit(
                &mut ctx,
                Value::bytes(pattern.as_bytes()),
                1 << 15,
                "Regex.match",
            )
            .unwrap();
            // Near misses restart the literal at every position of the text.
            let mut text = [vec![b'A'; length - 1], vec![b'b']].concat().repeat(2);
            text.extend(vec![b'A'; length]);
            let before = ctx.stats().steps;
            let mut search = Search::new(&mut ctx, program.view(), false).unwrap();
            let found = search
                .find(&mut ctx, program.view(), &text, 0)
                .unwrap()
                .unwrap();
            assert_eq!(found.data[..2], [2 * length, 3 * length]);
            ctx.stats().steps - before
        };
        let (small, large) = (cost(4096), cost(8192));
        assert!(large < small * 9 / 4, "{small} then {large}");
        // The reference's 16 KiB case-insensitive literal fits the default quota.
        assert!(cost(16384) < 1_000_000);
    }

    #[test]
    fn literal_scans_observe_quotas_and_cancellation() {
        let text = vec![b'a'; 1 << 20];
        let pattern = Value::bytes(format!("{}b", "a".repeat(64)).as_bytes());
        for kind in [ErrorKind::Steps, ErrorKind::Cancelled] {
            let mut ctx = context(None);
            let program = Program::compile(&mut ctx, pattern.clone(), "Regex.match").unwrap();
            let mut search = Search::new(&mut ctx, program.view(), false).unwrap();
            let steps = ctx.stats().steps;
            if kind == ErrorKind::Steps {
                ctx.options.limits.steps = Some(steps + 1000);
            } else {
                ctx.cancellation().cancel();
            }
            let error = search.find(&mut ctx, program.view(), &text, 0).unwrap_err();
            assert_eq!(error.kind, kind);
            // A failing chunk charge can overshoot the quota by one chunk.
            assert!(ctx.stats().steps <= steps + 1000 + (CHUNK / 64) as u64);
            assert_eq!(ctx.checkpoint().unwrap_err(), error);
        }
        // A prefix skip stops the same way when no thread is running.
        let mut ctx = context(None);
        let program = Program::compile(&mut ctx, Value::bytes(b"ab\\d"), "Regex.match").unwrap();
        let mut search = Search::new(&mut ctx, program.view(), false).unwrap();
        ctx.cancellation().cancel();
        let error = search.find(&mut ctx, program.view(), &text, 0).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Cancelled);
    }
}
