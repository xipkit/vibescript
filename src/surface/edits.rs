//! Source edits that compose: an edit replaces a span with text and pieces
//! of the original source, and those pieces are rendered with the edits
//! made inside them. A rewrite of `x.nil?` can therefore embed `x` after
//! another rule has rewritten something inside `x`.

use super::syntax::Span;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering::Relaxed};

/// The memory the rules' pass may hold beyond what its footprint counts:
/// the text it copies from the source and renders, which the footprint
/// cannot know in advance. Each copy takes its bytes before it is made,
/// and gives them back once it is dropped; a copy that would pass what is
/// left is refused, which stops the pass. It records the most the copies
/// held at once. It also keeps the pass's steps, and asks the budget
/// whether they are within it before each sort the pass makes.
#[derive(Default)]
pub(crate) struct Room<'b> {
    /// What the pass may hold, or `None` for as much as it needs.
    left: Option<usize>,
    held: AtomicUsize,
    peak: AtomicUsize,
    full: AtomicBool,
    stopped: AtomicBool,
    /// The steps the pass was charged other than its sorts'.
    charged: AtomicU64,
    /// The steps of the sorts the pass made.
    sorted: AtomicU64,
    /// Whether the steps that bring the check to a total are within its
    /// budget, deadline and cancellation; `None` for a pass without a
    /// budget.
    within: Option<Within<'b>>,
    /// The total it was last asked about, and the asks since, which ask it
    /// again only for steps charged since, or every [`ASK`]th time, for
    /// the deadline and the cancellation.
    asked: AtomicU64,
    asks: AtomicU64,
}

/// How many times the room answers whether the pass may go on, with no
/// steps charged since it last asked the budget, before it asks it again.
const ASK: u64 = 64;

/// Whether steps that bring a pass to a total are within its budget,
/// deadline and cancellation.
pub(crate) type Within<'b> = &'b (dyn Fn(u64) -> bool + Sync);

impl<'b> Room<'b> {
    /// A room of `left` bytes, or as many as the pass needs when `None`,
    /// for a check charged `charged` steps when the pass starts, whose
    /// `within` says whether a total of steps is within its budget.
    pub fn new(left: Option<usize>, within: Within<'b>, charged: u64) -> Self {
        Self {
            left,
            charged: AtomicU64::new(charged),
            within: Some(within),
            // No total has been asked about yet, so the first ask is made.
            asked: AtomicU64::new(u64::MAX),
            ..Self::default()
        }
    }

    /// Takes `bytes` a copy is about to hold; whether they fit.
    #[must_use = "a copy the room refuses must not be made"]
    pub fn take(&self, bytes: usize) -> bool {
        if self.full() || !self.within() {
            return false;
        }
        let held = self.held.load(Relaxed).saturating_add(bytes);
        if self.left.is_some_and(|left| held > left) {
            self.full.store(true, Relaxed);
            return false;
        }
        self.held.store(held, Relaxed);
        self.peak.fetch_max(held, Relaxed);
        true
    }

    /// Copies text with exact storage and a checkpoint before each byte batch.
    pub fn copy(&self, text: &str) -> Option<String> {
        if !self.take(text.len()) {
            return None;
        }
        let mut copied = String::with_capacity(text.len());
        if !self.append(&mut copied, text) {
            self.give_back(copied.capacity());
            return None;
        }
        Some(copied)
    }

    fn append(&self, out: &mut String, piece: &str) -> bool {
        let mut start = 0;
        while start < piece.len() {
            let mut end = (start + 4096).min(piece.len());
            while !piece.is_char_boundary(end) {
                end -= 1;
            }
            if !self.charge((end - start).div_ceil(64) as u64) {
                return false;
            }
            out.push_str(&piece[start..end]);
            start = end;
        }
        true
    }

    /// Compares source text in charged batches, stopping at a refusal.
    pub fn same_text(&self, left: &str, right: &str) -> Option<bool> {
        if !self.charge(1) {
            return None;
        }
        if left.len() != right.len() {
            return Some(false);
        }
        for (left, right) in left
            .as_bytes()
            .chunks(4096)
            .zip(right.as_bytes().chunks(4096))
        {
            if !self.charge(left.len().div_ceil(64) as u64) {
                return None;
            }
            if left != right {
                return Some(false);
            }
        }
        Some(true)
    }

    /// Gives back `bytes` a copy held, once it is dropped.
    pub fn give_back(&self, bytes: usize) {
        let held = self.held.load(Relaxed);
        self.held.store(held.saturating_sub(bytes), Relaxed);
    }

    /// Whether a copy was refused, which stops the pass.
    pub fn full(&self) -> bool {
        self.full.load(Relaxed)
    }

    /// The most the copies held at once.
    pub fn peak(&self) -> usize {
        self.peak.load(Relaxed)
    }

    /// The steps the check was charged when the pass started, and those
    /// the pass charged since other than its sorts'.
    pub fn charged(&self) -> u64 {
        self.charged.load(Relaxed)
    }

    /// The steps the check has been charged, with the pass's sorts'.
    pub fn total(&self) -> u64 {
        self.charged().saturating_add(self.sorted.load(Relaxed))
    }

    /// Whether the steps the check has been charged are within its
    /// budget, which also asks its deadline and cancellation.
    pub fn within(&self) -> bool {
        if self.full() || self.stopped.load(Relaxed) {
            return false;
        }
        let Some(within) = self.within else {
            return true;
        };
        // Steps charged since the budget was last asked are always asked
        // about; with none, the deadline and the cancellation are asked
        // every `ASK` times, as they cost more than the room's own counts.
        let total = self.total();
        let asks = self.asks.load(Relaxed);
        if total == self.asked.load(Relaxed) && asks % ASK != ASK - 1 {
            self.asks.store(asks + 1, Relaxed);
            return true;
        }
        self.asks.store(0, Relaxed);
        self.asked.store(total, Relaxed);
        if !within(total) {
            self.stopped.store(true, Relaxed);
            return false;
        }
        true
    }

    /// Charges `steps` of the pass's work, before it does it; whether they
    /// are within the budget.
    pub fn charge(&self, steps: u64) -> bool {
        // One pass charges its room at a time.
        let charged = self.charged.load(Relaxed);
        self.charged.store(charged.saturating_add(steps), Relaxed);
        self.within()
    }

    /// Charges a sort of `length` elements, a step for each 64 of the
    /// comparisons it may make, and asks the budget, before it is made;
    /// whether it may be.
    fn charge_sort(&self, length: usize) -> bool {
        if !self.within() {
            return false;
        }
        let steps = sort_steps(length);
        self.sorted.fetch_add(steps, Relaxed);
        self.within()
    }

    /// Sorts `list` as `sort_unstable_by` does, once its steps are charged
    /// and the budget asked; whether it did, leaving the list as it was
    /// when the budget has run out, which stops the pass.
    #[must_use = "a sort the budget refuses leaves the list unsorted, and stops the pass"]
    pub fn sort_unstable_by<T>(
        &self,
        list: &mut [T],
        compare: impl FnMut(&T, &T) -> std::cmp::Ordering,
    ) -> bool {
        if !self.charge_sort(list.len()) {
            return false;
        }
        list.sort_unstable_by(compare);
        true
    }

    /// Sorts `list` as `sort_by` does, keeping equal elements in order,
    /// once its steps are charged and the budget asked, as
    /// [`Self::sort_unstable_by`] does, and the scratch a stable sort keeps
    /// for a moment taken; whether it did, leaving the list as it was when
    /// the budget has run out or the room did not have the scratch.
    #[must_use = "a sort the budget or the room refuses leaves the list unsorted, and stops the pass"]
    pub fn sort_by<T>(
        &self,
        list: &mut [T],
        compare: impl FnMut(&T, &T) -> std::cmp::Ordering,
    ) -> bool {
        let scratch = std::mem::size_of_val(list);
        if !self.charge_sort(list.len()) || !self.take(scratch) {
            return false;
        }
        list.sort_by(compare);
        self.give_back(scratch);
        true
    }
}

/// The steps of sorting `length` elements, as the checker counts them.
fn sort_steps(length: usize) -> u64 {
    let comparisons = length.saturating_mul((usize::BITS - length.leading_zeros()) as usize);
    (comparisons / 64) as u64
}

/// Text the pass writes, taking from its [`Room`] before the string grows
/// to take each piece, with its old and new storage while it grows. Once
/// the room refuses a piece, it writes nothing more.
pub(crate) struct Written<'r> {
    text: String,
    room: &'r Room<'r>,
}

impl<'r> Written<'r> {
    pub fn new(room: &'r Room<'r>) -> Self {
        Self {
            text: String::new(),
            room,
        }
    }

    pub fn push_str(&mut self, piece: &str) {
        if !self.room.within() {
            return;
        }
        let (length, capacity) = (self.text.len(), self.text.capacity());
        let needed = length.saturating_add(piece.len());
        if needed > capacity {
            let target = needed.max(2 * capacity).max(16);
            if !self.room.take(target) || !self.room.charge(length.div_ceil(64) as u64) {
                return;
            }
            self.text.reserve_exact(target - length);
            self.room.give_back(capacity);
        }
        self.room.append(&mut self.text, piece);
    }

    /// Writes `args`, as `write!` writes them; a refusal stops the writing.
    pub fn write(&mut self, args: std::fmt::Arguments<'_>) {
        // A refused piece leaves the room full, which `finish` reads.
        if std::fmt::Write::write_fmt(self, args).is_err() {
            debug_assert!(!self.room.within());
        }
    }

    /// The text, which keeps what it took from the room; `None` once the
    /// room refused a piece.
    pub fn finish(self) -> Option<String> {
        self.room.within().then_some(self.text)
    }
}

impl std::fmt::Write for Written<'_> {
    fn write_str(&mut self, piece: &str) -> std::fmt::Result {
        self.push_str(piece);
        if !self.room.within() {
            return Err(std::fmt::Error);
        }
        Ok(())
    }
}

/// Part of an edit's replacement.
#[derive(Clone, Debug)]
pub enum Piece {
    /// Text written as is.
    Text(String),
    /// A span of the original source, rendered with the edits inside it.
    Source(Span),
}

#[derive(Clone, Debug)]
struct Edit {
    span: Span,
    pieces: Vec<Piece>,
    order: usize,
    /// The rewrite the edit belongs to, if any.
    group: Option<usize>,
}

/// What rendering tracks across nested spans.
struct State {
    conflicts: Vec<Span>,
    /// Which insertions have been written; each is written once.
    inserted: Vec<bool>,
}

/// The edits made to one source.
#[derive(Debug, Default)]
pub struct Edits {
    edits: Vec<Edit>,
    /// Spans where two edits overlapped without one containing the other.
    pub conflicts: Vec<Span>,
    /// The rewrite that edits made now belong to.
    group: Option<usize>,
}

impl Edits {
    /// Makes later edits belong to `group`, returning the group they
    /// belonged to before.
    pub fn enter(&mut self, group: Option<usize>) -> Option<usize> {
        std::mem::replace(&mut self.group, group)
    }

    /// The edits of `group`, flattened as [`Self::flatten_group`] flattens
    /// them.
    #[cfg(test)]
    pub fn flatten(&self, source: &str, group: usize) -> Vec<(Span, String)> {
        let members: Vec<usize> = (0..self.edits.len())
            .filter(|&index| self.edits[index].group == Some(group))
            .collect();
        self.flatten_group(source, &members, &Room::default())
            .unwrap()
    }

    /// The edits of each of `count` rewrites, by the order they were made,
    /// found in one pass over them all rather than one for each rewrite;
    /// `None` once `stop`, which it asks every [`POLL`](super::parse::POLL)
    /// edits, says the compilation has stopped.
    pub fn groups(&self, count: usize, stop: super::parse::Stop<'_>) -> Option<Vec<Vec<usize>>> {
        let mut groups = vec![Vec::new(); count];
        for (index, edit) in self.edits.iter().enumerate() {
            if index % super::parse::POLL as usize == 0 && stop() {
                return None;
            }
            if let Some(members) = edit.group.and_then(|group| groups.get_mut(group)) {
                members.push(index);
            }
        }
        Some(groups)
    }

    /// The edits `members` lists, which [`Self::groups`] gives for a
    /// rewrite, applied on their own, as non-overlapping replacements of
    /// `source`: edits that nest, overlap or meet are rendered together, the
    /// later seeing the earlier as when every edit is applied. The source
    /// each copies and the text each renders take from `room` before they
    /// are made, and the replacements keep what they took; `None` once the
    /// room refuses one.
    pub fn flatten_group(
        &self,
        source: &str,
        members: &[usize],
        room: &Room<'_>,
    ) -> Option<Vec<(Span, String)>> {
        let mut edits: Vec<&Edit> = members.iter().map(|&index| &self.edits[index]).collect();
        // Each edit's place among them is its own, so the order is total.
        let sorted = room.sort_unstable_by(&mut edits, |a, b| {
            (a.span.start, a.span.end, a.order).cmp(&(b.span.start, b.span.end, b.order))
        });
        if !sorted {
            return None;
        }
        // The budget is asked before each sort, and before each edit, as
        // the walk asks it, as they are gathered and as they are rendered.
        let mut clusters: Vec<(Span, Vec<&Edit>)> = Vec::new();
        for edit in edits {
            if !room.within() {
                return None;
            }
            match clusters.last_mut() {
                Some((extent, members)) if edit.span.start <= extent.end => {
                    extent.end = extent.end.max(edit.span.end);
                    members.push(edit);
                }
                _ => clusters.push((edit.span, vec![edit])),
            }
        }
        let mut flattened = Vec::with_capacity(clusters.len());
        for (extent, mut members) in clusters {
            if !room.sort_unstable_by(&mut members, |a, b| a.order.cmp(&b.order)) {
                return None;
            }
            // Padding keeps every edit inside the rendered text, so none
            // spans all of it and insertions at either end stay in. Its
            // copy, and the pieces copied for the edits, are dropped once
            // the text is rendered.
            let padded = extent.end - extent.start + 2;
            if !room.take(padded) {
                return None;
            }
            let mut text = String::with_capacity(padded);
            text.push(' ');
            if !room.append(&mut text, &source[extent.start..extent.end]) {
                return None;
            }
            text.push(' ');
            let local = |span: Span| Span {
                start: span.start - extent.start + 1,
                end: span.end - extent.start + 1,
            };
            let mut copied = 0;
            let mut edits = Edits::default();
            for member in members {
                if !room.within() {
                    return None;
                }
                let mut pieces = Vec::with_capacity(member.pieces.len());
                for piece in &member.pieces {
                    pieces.push(match piece {
                        Piece::Source(span)
                            if span.start >= extent.start && span.end <= extent.end =>
                        {
                            Piece::Source(local(*span))
                        }
                        Piece::Source(span) => {
                            copied += span.end - span.start;
                            Piece::Text(room.copy(&source[span.range()])?)
                        }
                        Piece::Text(text) => {
                            copied += text.len();
                            Piece::Text(room.copy(text)?)
                        }
                    });
                }
                edits.replace(local(member.span), pieces);
            }
            let mut rendered = edits.apply_in(&text, room)?;
            room.give_back(padded + copied);
            // The padding is taken off in place, rather than copying what
            // it pads.
            rendered.pop();
            rendered.remove(0);
            flattened.push((extent, rendered));
        }
        Some(flattened)
    }

    /// Replaces `span`. Edits of the same span stack: a later one sees the
    /// earlier ones' result through a [`Piece::Source`] of that span.
    pub fn replace(&mut self, span: Span, pieces: Vec<Piece>) {
        let order = self.edits.len();
        self.edits.push(Edit {
            span,
            pieces,
            order,
            group: self.group,
        });
    }

    /// Replaces `span` with plain text.
    pub fn text(&mut self, span: Span, text: impl Into<String>) {
        self.replace(span, vec![Piece::Text(text.into())]);
    }

    /// Inserts text at `offset`. Insertions at one offset keep their order.
    pub fn insert(&mut self, offset: usize, text: impl Into<String>) {
        self.text(
            Span {
                start: offset,
                end: offset,
            },
            text,
        );
    }

    /// Wraps `span` in `before` and `after`, keeping the edits inside it.
    pub fn wrap(&mut self, span: Span, before: &str, after: &str) {
        self.replace(
            span,
            vec![
                Piece::Text(before.to_owned()),
                Piece::Source(span),
                Piece::Text(after.to_owned()),
            ],
        );
    }

    /// Renders `source` with every edit applied.
    #[cfg(test)]
    pub fn apply(&mut self, source: &str) -> String {
        self.apply_in(source, &Room::default()).unwrap()
    }

    /// Renders `source` with every edit applied, writing the text through
    /// `room`; `None` once the room refuses it.
    pub fn apply_in(&mut self, source: &str, room: &Room<'_>) -> Option<String> {
        // Each edit's order is its own, so the order is total.
        let sorted = room.sort_unstable_by(&mut self.edits, |a, b| {
            a.span
                .start
                .cmp(&b.span.start)
                .then((a.span.end > a.span.start).cmp(&(b.span.end > b.span.start)))
                .then(b.span.end.cmp(&a.span.end))
                .then(a.order.cmp(&b.order))
        });
        if !sorted {
            return None;
        }
        let mut out = Written::new(room);
        let whole = Span {
            start: 0,
            end: source.len(),
        };
        let scratch = self.edits.len() * size_of::<bool>();
        if !room.take(scratch) {
            return None;
        }
        let mut state = State {
            conflicts: Vec::new(),
            inserted: vec![false; self.edits.len()],
        };
        let rendered = self.render(source, whole, None, &mut out, &mut state);
        self.conflicts = state.conflicts;
        drop(state.inserted);
        room.give_back(self.edits.len() * size_of::<bool>());
        rendered?;
        out.finish()
    }

    /// Renders `span`, where `limit` bounds which edits of exactly this span
    /// apply: those before that position in the sorted list.
    fn render(
        &self,
        source: &str,
        span: Span,
        limit: Option<usize>,
        out: &mut Written<'_>,
        state: &mut State,
    ) -> Option<()> {
        if !out.room.charge(1) {
            return None;
        }
        let whole = span.start == 0 && span.end == source.len() && limit.is_none();
        let first = self
            .edits
            .partition_point(|edit| edit.span.start < span.start);
        let mut top = None;
        for i in first..limit.unwrap_or(self.edits.len()) {
            if !out.room.charge(1) {
                return None;
            }
            if self.edits[i].span.start != span.start {
                break;
            }
            if self.edits[i].span == span && span.end > span.start {
                top = Some(i);
            }
        }
        if let Some(top) = top {
            for piece in &self.edits[top].pieces {
                if !out.room.charge(1) {
                    return None;
                }
                match piece {
                    Piece::Text(text) => out.push_str(text),
                    Piece::Source(inner) if *inner == span => {
                        self.render(source, span, Some(top), out, state)?;
                    }
                    Piece::Source(inner) => self.render(source, *inner, None, out, state)?,
                }
            }
            return out.room.within().then_some(());
        }
        let mut position = span.start;
        let mut index = first;
        while index < self.edits.len() {
            if !out.room.charge(1) {
                return None;
            }
            let edit = &self.edits[index];
            let past = edit.span.start > span.end
                || (edit.span.start == span.end
                    && span.end > span.start
                    && edit.span.end > edit.span.start);
            if past {
                break;
            }
            let inside = edit.span.end <= span.end && edit.span != span;
            let empty = edit.span.start == edit.span.end;
            if !inside || edit.span.start < position {
                if edit.span.start < position && edit.span.end > position {
                    state.conflicts.push(edit.span);
                }
                index += 1;
                continue;
            }
            if empty && edit.span.start == span.end && span.end > span.start && !whole {
                // An insertion at the end belongs to the enclosing text.
                break;
            }
            out.push_str(&source[position..edit.span.start]);
            if !out.room.within() {
                return None;
            }
            if empty {
                if !state.inserted[index] {
                    state.inserted[index] = true;
                    for piece in &edit.pieces {
                        if !out.room.charge(1) {
                            return None;
                        }
                        if let Piece::Text(text) = piece {
                            out.push_str(text);
                        }
                    }
                }
                index += 1;
                position = edit.span.start;
                continue;
            }
            self.render(source, edit.span, None, out, state)?;
            position = edit.span.end;
            index += 1;
        }
        out.push_str(&source[position..span.end]);
        out.room.within().then_some(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(start: usize, end: usize) -> Span {
        Span { start, end }
    }

    #[test]
    fn admission_and_small_sorts_observe_a_stop() {
        let room = Room::new(None, &|_| false, 0);
        assert!(!room.take(1));
        assert!(!room.sort_unstable_by(&mut [1], Ord::cmp));
        assert!(!room.within());
    }

    #[test]
    fn long_writes_charge_bytes_and_stop_between_chunks() {
        let room = Room::new(None, &|steps| steps <= 64, 0);
        let mut out = Written::new(&room);
        out.push_str(&"x".repeat(100_000));
        assert!(out.finish().is_none());
        assert!(room.total() > 64);
    }

    #[test]
    fn rendering_stops_between_replacement_pieces() {
        let calls = AtomicUsize::new(0);
        let within = |_| calls.fetch_add(1, Relaxed) < 4;
        let room = Room::new(None, &within, 0);
        let mut edits = Edits::default();
        edits.replace(span(0, 1), vec![Piece::Text("x".into()); 10_000]);
        assert!(edits.apply_in("a", &room).is_none());
        assert!(calls.load(Relaxed) <= 6);
    }

    #[test]
    fn nested_edits_render_inside_their_parents() {
        let source = "unless x.nil? then 1 end";
        let mut edits = Edits::default();
        edits.text(span(0, 6), "if");
        // `x.nil?` becomes `x == nil`, and the condition is negated around it.
        edits.replace(
            span(7, 13),
            vec![Piece::Source(span(7, 8)), Piece::Text(" == nil".into())],
        );
        edits.wrap(span(7, 13), "!(", ")");
        edits.text(span(7, 8), "y");
        assert_eq!(edits.apply(source), "if !(y == nil) then 1 end");
        assert!(edits.conflicts.is_empty());
    }

    #[test]
    fn insertions_keep_their_order_and_place() {
        let source = "def f(a, b)\nend";
        let mut edits = Edits::default();
        edits.insert(7, ": int");
        edits.insert(10, ": string");
        edits.insert(11, " -> int");
        edits.insert(11, "!");
        assert_eq!(edits.apply(source), "def f(a: int, b: string) -> int!\nend");
    }

    #[test]
    fn a_group_flattens_into_separate_replacements() {
        let source = "f %w[a b] + g";
        let mut edits = Edits::default();
        let outer = edits.enter(Some(0));
        edits.text(span(2, 9), "[\"a\", \"b\"]");
        edits.wrap(span(2, 9), "(", ")");
        edits.text(span(12, 13), "h");
        edits.insert(11, "!");
        edits.insert(0, "(");
        edits.text(span(0, 1), "f");
        edits.enter(outer);
        edits.text(span(1, 2), " ");
        assert_eq!(
            edits.flatten(source, 0),
            [
                (span(0, 1), "(f".to_owned()),
                (span(2, 9), "([\"a\", \"b\"])".to_owned()),
                (span(11, 11), "!".to_owned()),
                (span(12, 13), "h".to_owned())
            ]
        );
    }

    #[test]
    fn overlapping_edits_are_reported() {
        let mut edits = Edits::default();
        edits.text(span(0, 3), "x");
        edits.text(span(2, 5), "y");
        assert_eq!(edits.apply("abcdef"), "xdef");
        assert_eq!(edits.conflicts, [span(2, 5)]);
    }
}
