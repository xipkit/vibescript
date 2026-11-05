use super::{
    parse::Assertion,
    program::{Op, View},
    unicode,
};
use crate::checked::{Access, Checked, violation};
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
    anchored: bool,
}

/// Proves that every path to a rune or a match passes BeginText first. Other
/// assertions may fail, so following them unconditionally is conservative.
/// The search reuses these visited marks by starting with a later epoch.
fn anchored(
    ctx: &mut CallContext,
    program: View<'_>,
    start: usize,
    visited: &mut [u32],
) -> Result<bool> {
    let mut pending = Buffer::empty();
    let mut pc = start;
    loop {
        let next = if *visited.at(pc)? == 0 {
            *visited.at_mut(pc)? = 1;
            let instruction = *program.instructions.at(pc)?;
            match instruction.op {
                Op::Fail | Op::Assert(Assertion::BeginText) => None,
                Op::Split => {
                    pending.push(ctx, instruction.alternate)?;
                    Some(instruction.next)
                }
                Op::Save(_) | Op::Assert(_) => Some(instruction.next),
                _ => return Ok(false),
            }
        } else {
            None
        };
        let Some(next) = next.or_else(|| pending.data.pop()) else {
            return Ok(true);
        };
        pc = next;
        ctx.charge(1)?;
    }
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
    if rune < 128 {
        // An ASCII letter's orbit starts at its upper case, one bit away; the
        // non-ASCII members of the K and S orbits are larger than both
        // letters. Arithmetic rather than a test of `fold`, since the scan
        // does this for every ASCII byte.
        let lower = (rune as u8).is_ascii_lowercase();
        rune ^ (u32::from(lower & fold) << 5)
    } else if fold {
        least(rune)
    } else {
        rune
    }
}

/// Walks a non-ASCII rune's case-folding orbit for its least member. Out of
/// line, so that `canonical` stays a few instructions wherever it is inlined.
#[inline(never)]
fn least(rune: u32) -> u32 {
    let mut least = rune;
    let mut current = unicode::fold(rune);
    while current != rune {
        least = least.min(current);
        current = unicode::fold(current);
    }
    least
}

/// Finds the shared literal prefix or proves a text anchor. The anchor proof
/// starts at the already charged instruction where the literal prefix ends.
fn prefix(
    ctx: &mut CallContext,
    program: View<'_>,
    visited: &mut [u32],
) -> Result<(Option<Literal>, bool)> {
    let mut literal = Literal {
        runes: Buffer::empty(),
        table: Buffer::empty(),
        fold: false,
        complete: false,
        saves: Buffer::empty(),
    };
    let mut pc = program.start;
    loop {
        ctx.charge(1)?;
        let instruction = *program.instructions.at(pc)?;
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
        return Ok((None, anchored(ctx, program, pc, visited)?));
    }
    if !literal.complete {
        literal.saves = Buffer::empty();
    }
    let runes = &literal.runes.data;
    literal.table.ensure(ctx, runes.len())?;
    literal.table.data.push(0);
    let mut matched = 0;
    for rune in runes.iter().skip(1) {
        ctx.charge(1)?;
        while matched > 0 && rune != runes.at(matched)? {
            ctx.charge(1)?;
            matched = *literal.table.at(matched - 1)? as usize;
        }
        if rune == runes.at(matched)? {
            matched += 1;
        }
        literal.table.data.push(matched as u32);
    }
    Ok((Some(literal), false))
}

// Keep the byte loop independent of the fallible KMP scan's register pressure.
#[inline(never)]
fn idle_prefix(bytes: &[u8], first: u32, fold: bool) -> usize {
    bytes
        .iter()
        .position(|&byte| byte >= 128 || canonical(u32::from(byte), fold) == first)
        .unwrap_or(bytes.len())
}

impl Literal {
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
        let first = *runes.at(0)?;
        // Folded ASCII letters have two ASCII forms; the byte search stops
        // at only one needle. It already stops before non-ASCII fold members.
        let ascii = first < 128 && !(self.fold && (first as u8).is_ascii_alphabetic());
        let mut start = from;
        let mut position = from;
        let mut matched = 0;
        let mut work = 0;
        while position < text.len() {
            if work >= CHUNK {
                ctx.work_bytes(work)?;
                work = 0;
            }
            let Some(&byte) = text.get(position) else {
                break;
            };
            // An ASCII byte that cannot begin the literal.
            let idle = |byte: u8| byte < 128 && canonical(u32::from(byte), self.fold) != first;
            if matched == 0 && idle(byte) {
                // With nothing matched, a run of such bytes only moves the
                // start. Most of a scan is these runs, so they get a loop of
                // their own. It stops where the next charge falls due, which
                // leaves every charge where a byte at a time would put it.
                let window = text.span(position, text.len().min(position + (CHUNK - work)))?;
                let skipped = if ascii {
                    scan::ascii_mismatch(window, first as u8)
                } else {
                    idle_prefix(window, first, self.fold)
                };
                work += skipped;
                position += skipped;
                start = position;
                continue;
            }
            let (rune, width) = if byte < 128 {
                (u32::from(byte), 1)
            } else {
                scan::code_point(text.tail(position)?)
            };
            work += width;
            position += width;
            let rune = canonical(rune, self.fold);
            if matched > 0 && rune != *runes.at(matched)? {
                let partial = matched;
                while matched > 0 && rune != *runes.at(matched)? {
                    matched = *self.table.at(matched - 1)? as usize;
                }
                // The partial match loses its first `partial - matched` runes.
                for _ in matched..partial {
                    start += if *text.at(start)? < 128 {
                        1
                    } else {
                        scan::code_point(text.tail(start)?).1
                    };
                }
                work += 2 * (partial - matched);
            }
            if rune == *runes.at(matched)? {
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
        let (literal, anchored) = prefix(ctx, program, &mut visited.data)?;
        Ok(Self {
            current: Buffer::empty(),
            next: Buffer::empty(),
            visited,
            actions: Buffer::empty(),
            pool: Buffer::empty(),
            epoch: 1,
            slots: if captures { 2 * program.names.len() } else { 2 },
            literal,
            anchored,
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
        let Some((start, end)) = self
            .literal
            .as_ref()
            .ok_or(violation("literal search must have a literal"))?
            .scan(ctx, text, from)?
        else {
            return Ok(None);
        };
        let mut captures = self.captures(ctx)?;
        *captures.at_mut(0)? = start;
        *captures.at_mut(1)? = end;
        let mut position = start;
        let mut offset = 0;
        for &(slot, runes) in &self
            .literal
            .as_ref()
            .ok_or(violation("literal search must have a literal"))?
            .saves
            .data
        {
            ctx.charge(1)?;
            while offset < runes {
                ctx.charge(1)?;
                position += scan::rune(text.tail(position)?).1;
                offset += 1;
            }
            if slot < self.slots {
                *captures.at_mut(slot)? = position;
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
                Action::Restore(slot, value) => *captures.at_mut(slot)? = value,
                Action::Visit(pc) => {
                    if *self.visited.at(pc)? == self.epoch {
                        continue;
                    }
                    *self.visited.at_mut(pc)? = self.epoch;
                    let instruction = *program.instructions.at(pc)?;
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
                                    .push(ctx, Action::Restore(slot, *captures.at(slot)?))?;
                                *captures.at_mut(slot)? = position;
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
        if from > text.len() || (self.anchored && from > 0) {
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
            Some(previous(text, from)?)
        };
        let mut best = None;
        loop {
            ctx.checkpoint()?;
            debug_assert!(!self.anchored || position == 0 || !self.next.data.is_empty());
            if best.is_none() && self.next.data.is_empty() {
                // With no thread running, a match can only start where the literal does.
                if let Some(literal) = &self.literal {
                    let Some((start, _)) = literal.scan(ctx, text, position)? else {
                        return Ok(None);
                    };
                    if start != position {
                        position = start;
                        before = Some(previous(text, start)?);
                    }
                }
            }
            let (after, width) = if position < text.len() {
                let (rune, width, _) = scan::rune(text.tail(position)?);
                (Some(rune as u32), width)
            } else {
                (None, 0)
            };
            if best.is_none() && (!self.anchored || position == 0) {
                let mut captures = self.captures(ctx)?;
                *captures.at_mut(0)? = position;
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
                Some(scan::rune(text.tail(position + width)?).0 as u32)
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
                let instruction = *program.instructions.at(thread.pc)?;
                if matches!(instruction.op, Op::Match) {
                    *thread.captures.at_mut(1)? = position;
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
            if position == text.len()
                || ((self.anchored || best.is_some()) && self.next.data.is_empty())
            {
                return Ok(best);
            }
            position += width;
            before = after;
        }
    }
}

fn previous(bytes: &[u8], position: usize) -> Access<u32> {
    let mut start = position - 1;
    while start > position.saturating_sub(4) && bytes.at(start)? & 0xc0 == 0x80 {
        start -= 1;
    }
    let (rune, width, valid) = scan::rune(bytes.span(start, position)?);
    if valid && start + width == position {
        Ok(rune as u32)
    } else {
        Ok(0xfffd)
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
    fn invalid_program_and_literal_indices_return_internal_errors() {
        let mut ctx = context(None);
        let mut program = Program::compile(&mut ctx, Value::bytes(b"aab"), "Regex.match").unwrap();
        program.start = usize::MAX;
        let error = Search::new(&mut ctx, program.view().unwrap(), false)
            .err()
            .unwrap();
        assert_eq!(error.kind, ErrorKind::Internal);
        assert!(error.message.contains("out of range"));

        let program = Program::compile(&mut ctx, Value::bytes(b"aab"), "Regex.match").unwrap();
        let mut search = Search::new(&mut ctx, program.view().unwrap(), false).unwrap();
        let literal = search.literal.as_mut().unwrap();
        literal.table.data.fill(u32::MAX);
        let error = literal.scan(&mut ctx, b"aaac", 0).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Internal);
        assert!(error.message.contains("out of range"));
    }

    #[test]
    fn text_anchor_proof_checks_every_non_consuming_path() {
        for (pattern, expected) in [
            (r"\AID-[0-9]+\z", true),
            (r"^ID-[0-9]+$", true),
            (r"(\Aa)|(^b)", true),
            (r"(?:\Aa|\Ab)+", true),
            (r"\b(?:\Aa|^b)", true),
            (r"(?:\b)*\Aa", true),
            (r"(?:\A)+a", true),
            (r"\A", true),
            (r"\z\A", true),
            (r"(?m:^a)|\Ab", false),
            (r"(?m)^a", false),
            (r"\Aa|b", false),
            (r"a|\Ab", false),
            (r"\Aa|", false),
            (r"(?:\Aa)?", false),
            (r"(?:\Aa)*", false),
            (r"(?:\A)*a", false),
            (r"a?\Ab", false),
            (r"a\A", false),
            (r"\ba", false),
            (r"[ab]", false),
            (r"a", false),
            (r"", false),
        ] {
            let mut ctx = context(None);
            let program =
                Program::compile(&mut ctx, Value::bytes(pattern.as_bytes()), "Regex.match")
                    .unwrap();
            let search = Search::new(&mut ctx, program.view().unwrap(), false).unwrap();
            assert_eq!(search.anchored, expected, "{pattern}");
        }
    }

    #[test]
    fn anchored_searches_agree_with_the_thread_machine() {
        let mut ctx = context(None);
        let patterns = [
            r"\AID-[0-9]+\z",
            r"^a+$",
            r"(\Aa+)|(\Ab+)",
            r"\A(a+|ab)",
            r"\A(ab|a+)",
            r"\A(a+?)",
            r"\A(a*)",
            r"\A(a*)(b?)",
            r"\A(?i:é+)",
            r"\A.",
            r"\A\x{fffd}",
            r"\b\Aa*",
            r"(?:\b)*\Aa*",
            r"(?:\A)+a*",
            r"\A|\Ab",
            r"\z\A",
            r"\Aa|b+",
            r"a+|\Ab",
            r"(?:\Aa)?",
            r"(?:\Aa)*",
            r"a?\Ab",
            r"(?m:^b)|\Aa",
        ];
        let texts: [&[u8]; 14] = [
            b"",
            b"x",
            b"a",
            b"aaab",
            b"ababa",
            b"bbb",
            b"ba",
            b"ID-123",
            b"xID-123",
            b"a\nb\n",
            "éÉa".as_bytes(),
            b"\xffa",
            b"\xc3\xa9\xff",
            b"\xe7\x95",
        ];
        for pattern in patterns {
            let program =
                Program::compile(&mut ctx, Value::bytes(pattern.as_bytes()), "Regex.match")
                    .unwrap();
            let view = program.view().unwrap();
            for captures in [false, true] {
                for text in texts {
                    let mut fast = Search::new(&mut ctx, view, captures).unwrap();
                    let mut plain = Search::new(&mut ctx, view, captures).unwrap();
                    plain.anchored = false;
                    plain.literal = None;
                    for from in 0..=text.len() + 1 {
                        let fast = fast.find(&mut ctx, view, text, from).unwrap();
                        let plain = plain.find(&mut ctx, view, text, from).unwrap();
                        assert_eq!(
                            fast.as_ref().map(|found| &found.data),
                            plain.as_ref().map(|found| &found.data),
                            "{pattern} from {from} in {text:?}"
                        );
                    }
                    assert_eq!(
                        matches(&mut ctx, &mut fast, view, text),
                        matches(&mut ctx, &mut plain, view, text),
                        "{pattern} in {text:?}"
                    );
                }
            }
        }
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }

    #[test]
    fn anchored_misses_do_not_walk_the_remaining_text() {
        for from in [0, 1] {
            let mut costs = Vec::new();
            for length in [1, 16384, 1 << 20] {
                let mut ctx = context(None);
                let program =
                    Program::compile(&mut ctx, Value::bytes(br"\AID-[0-9]+\z"), "Regex.match")
                        .unwrap();
                let mut search = Search::new(&mut ctx, program.view().unwrap(), false).unwrap();
                let before = ctx.stats().steps;
                assert!(
                    search
                        .find(&mut ctx, program.view().unwrap(), &vec![b'x'; length], from)
                        .unwrap()
                        .is_none()
                );
                costs.push(ctx.stats().steps - before);
            }
            assert!(costs.iter().all(|&cost| cost == costs[0]), "{costs:?}");
            assert!(costs[0] < 10, "{costs:?}");
        }
    }

    #[test]
    fn anchored_searches_do_not_restart_threads() {
        let text = vec![b'a'; 4096];
        for captures in [false, true] {
            let mut ctx = context(None);
            let program =
                Program::compile(&mut ctx, Value::bytes(br"\Aa+z"), "Regex.match").unwrap();
            let view = program.view().unwrap();
            let mut fast = Search::new(&mut ctx, view, captures).unwrap();
            let mut plain = Search::new(&mut ctx, view, captures).unwrap();
            plain.anchored = false;
            let before = ctx.stats().steps;
            assert!(fast.find(&mut ctx, view, &text, 0).unwrap().is_none());
            let fast_steps = ctx.stats().steps - before;
            let before = ctx.stats().steps;
            assert!(plain.find(&mut ctx, view, &text, 0).unwrap().is_none());
            let plain_steps = ctx.stats().steps - before;
            assert!(fast_steps < plain_steps, "{fast_steps} >= {plain_steps}");
        }
    }

    #[test]
    fn anchored_threads_still_observe_quotas_and_cancellation() {
        for kind in [ErrorKind::Steps, ErrorKind::Cancelled] {
            let mut ctx = context(None);
            let program =
                Program::compile(&mut ctx, Value::bytes(br"\Aa+z"), "Regex.match").unwrap();
            let mut search = Search::new(&mut ctx, program.view().unwrap(), false).unwrap();
            if kind == ErrorKind::Steps {
                ctx.options.limits.steps = Some(ctx.stats().steps + 1000);
            } else {
                ctx.cancellation().cancel();
            }
            let error = search
                .find(&mut ctx, program.view().unwrap(), &vec![b'a'; 16384], 0)
                .unwrap_err();
            assert_eq!(error.kind, kind);
        }
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
            let view = program.view().unwrap();
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

    /// The rune-at-a-time scan `Literal::scan` must reproduce: what it finds,
    /// and the steps its chunked charges come to.
    fn reference_scan(
        literal: &Literal,
        text: &[u8],
        from: usize,
    ) -> (Option<(usize, usize)>, u64) {
        let runes = &literal.runes.data;
        let (mut start, mut position, mut matched) = (from, from, 0);
        let (mut work, mut steps) = (0usize, 0);
        while position < text.len() {
            if work >= CHUNK {
                steps += (work as u64).div_ceil(64);
                work = 0;
            }
            let (rune, width, _) = scan::rune(&text[position..]);
            let rune = canonical(rune as u32, literal.fold);
            work += width;
            while matched > 0 && rune != runes[matched] {
                let next = literal.table.data[matched - 1] as usize;
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
                    return (Some((start, position)), steps + (work as u64).div_ceil(64));
                }
            } else {
                start = position;
            }
        }
        (None, steps + (work as u64).div_ceil(64))
    }

    #[test]
    fn literal_scans_charge_like_rune_at_a_time_matching() {
        // Mostly the literals' own runes, so partial matches and the failure
        // links they fall back along are frequent; and runs of a byte that
        // begins none of them, long enough to be cut by a charge.
        let pieces: [&[u8]; 14] = [
            &[b'x'; 1531],
            b"xx",
            b"a",
            b"a",
            b"a",
            b"b",
            b"b",
            b"A",
            b"k",
            "\u{212a}".as_bytes(),
            "\u{e9}".as_bytes(),
            "\u{754c}".as_bytes(),
            b"\xff",
            b"\xe7\x95",
        ];
        let patterns = [
            "c",
            "ab",
            "aabaaabab",
            "abababababc",
            "(?i)aabaak",
            "(?i)1ks",
            "(?i):ks",
            "(?i)k",
            "(?i)s",
            "(?i)\u{e9}aa\u{c9}ab",
            "\u{754c}aab\u{754c}b",
            "a\u{fffd}a\u{fffd}b",
        ];
        let mut state = 0x2545_f491_4f6c_dd1du64;
        let mut next = move |bound: usize| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state % bound as u64) as usize
        };
        for pattern in patterns {
            let mut ctx = context(None);
            let program =
                Program::compile(&mut ctx, Value::bytes(pattern.as_bytes()), "Regex.match")
                    .unwrap();
            let search = Search::new(&mut ctx, program.view().unwrap(), false).unwrap();
            let literal = search.literal.as_ref().unwrap();
            let mut texts = Vec::new();
            for case in 0..200 {
                // Some texts span several chunks, so charges fall mid-scan.
                let target = if case % 20 == 0 {
                    2 * CHUNK + next(2 * CHUNK)
                } else {
                    next(300)
                };
                let mut text = Vec::new();
                while text.len() < target {
                    text.extend_from_slice(pieces[next(pieces.len())]);
                }
                texts.push(text);
            }
            // Runs that end on a charge, just before one and just after.
            for run in [CHUNK - 1, CHUNK, CHUNK + 1, 2 * CHUNK] {
                let mut text = vec![b'x'; run];
                for _ in 0..40 {
                    text.extend_from_slice(pieces[next(pieces.len())]);
                }
                texts.push(text);
            }
            // Candidates and non-ASCII bytes in the vector lanes and tails,
            // including both ASCII forms and the K/S folding orbits.
            for run in [7, 8, 9, 15, 16, 17, 31, 32, 33, CHUNK - 1, CHUNK, CHUNK + 1] {
                for tail in [
                    b"ab".as_slice(),
                    b"1kS",
                    b":kS",
                    b"k",
                    b"K",
                    b"s",
                    b"S",
                    "1\u{212a}\u{17f}".as_bytes(),
                    "\u{212a}\u{17f}".as_bytes(),
                    b"\xffab",
                    b"\xe7\x95ab",
                ] {
                    let mut text = vec![b'x'; run];
                    text.extend_from_slice(tail);
                    texts.push(text);
                }
            }
            for text in &texts {
                for from in [0, text.len().min(1), text.len() / 3, text.len()] {
                    let before = ctx.stats().steps;
                    let found = literal.scan(&mut ctx, text, from).unwrap();
                    assert_eq!(
                        (found, ctx.stats().steps - before),
                        reference_scan(literal, text, from),
                        "{pattern} from {from} in {text:?}"
                    );
                }
            }
        }
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
            let mut search = Search::new(&mut ctx, program.view().unwrap(), false).unwrap();
            let found = search
                .find(&mut ctx, program.view().unwrap(), &text, 0)
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
            let mut search = Search::new(&mut ctx, program.view().unwrap(), false).unwrap();
            let steps = ctx.stats().steps;
            if kind == ErrorKind::Steps {
                ctx.options.limits.steps = Some(steps + 1000);
            } else {
                ctx.cancellation().cancel();
            }
            let error = search
                .find(&mut ctx, program.view().unwrap(), &text, 0)
                .unwrap_err();
            assert_eq!(error.kind, kind);
            // A failing chunk charge can overshoot the quota by one chunk.
            assert!(ctx.stats().steps <= steps + 1000 + (CHUNK / 64) as u64);
            assert_eq!(ctx.checkpoint().unwrap_err(), error);
        }
        // A prefix skip stops the same way when no thread is running.
        let mut ctx = context(None);
        let program = Program::compile(&mut ctx, Value::bytes(b"ab\\d"), "Regex.match").unwrap();
        let mut search = Search::new(&mut ctx, program.view().unwrap(), false).unwrap();
        ctx.cancellation().cancel();
        let error = search
            .find(&mut ctx, program.view().unwrap(), &text, 0)
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Cancelled);
    }
}
