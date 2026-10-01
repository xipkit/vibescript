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
/// held at once.
#[derive(Debug, Default)]
pub(crate) struct Room {
    /// What the pass may hold, or `None` for as much as it needs.
    left: Option<usize>,
    held: AtomicUsize,
    peak: AtomicUsize,
    full: AtomicBool,
    /// The steps of the sorts the pass made, which it charges with the
    /// rest of its work.
    steps: AtomicU64,
}

impl Room {
    pub fn new(left: Option<usize>) -> Self {
        Self {
            left,
            ..Self::default()
        }
    }

    /// Takes `bytes` a copy is about to hold; whether they fit.
    #[must_use = "a copy the room refuses must not be made"]
    pub fn take(&self, bytes: usize) -> bool {
        if self.full() {
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

    /// The steps of the sorts the pass made.
    pub fn steps(&self) -> u64 {
        self.steps.load(Relaxed)
    }

    /// Sorts `list` as `sort_unstable_by` does, counting its steps, a step
    /// for each 64 of the comparisons it may make, which the pass charges
    /// and asks the budget about with the rest of its work.
    pub fn sort_unstable_by<T>(
        &self,
        list: &mut [T],
        compare: impl FnMut(&T, &T) -> std::cmp::Ordering,
    ) {
        self.steps.fetch_add(sort_steps(list.len()), Relaxed);
        list.sort_unstable_by(compare);
    }

    /// Sorts `list` as `sort_by` does, keeping equal elements in order,
    /// counting its steps as [`Self::sort_unstable_by`] does and taking the
    /// scratch a stable sort keeps for a moment; whether the room had it,
    /// leaving the list as it was when it did not.
    #[must_use = "a sort the room refuses leaves the list unsorted, and stops the pass"]
    pub fn sort_by<T>(
        &self,
        list: &mut [T],
        compare: impl FnMut(&T, &T) -> std::cmp::Ordering,
    ) -> bool {
        let scratch = std::mem::size_of_val(list);
        if !self.take(scratch) {
            return false;
        }
        self.steps.fetch_add(sort_steps(list.len()), Relaxed);
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
    room: &'r Room,
}

impl<'r> Written<'r> {
    pub fn new(room: &'r Room) -> Self {
        Self {
            text: String::new(),
            room,
        }
    }

    pub fn push_str(&mut self, piece: &str) {
        if self.room.full() {
            return;
        }
        let (length, capacity) = (self.text.len(), self.text.capacity());
        let needed = length.saturating_add(piece.len());
        if needed > capacity {
            let target = needed.max(2 * capacity).max(16);
            if !self.room.take(target) {
                return;
            }
            self.text.reserve_exact(target - length);
            self.room.give_back(capacity);
        }
        self.text.push_str(piece);
    }

    /// Writes `args`, as `write!` writes them; a refusal stops the writing.
    pub fn write(&mut self, args: std::fmt::Arguments<'_>) {
        // A refused piece leaves the room full, which `finish` reads.
        if std::fmt::Write::write_fmt(self, args).is_err() {
            debug_assert!(self.room.full());
        }
    }

    /// The text, which keeps what it took from the room; `None` once the
    /// room refused a piece.
    pub fn finish(self) -> Option<String> {
        (!self.room.full()).then_some(self.text)
    }
}

impl std::fmt::Write for Written<'_> {
    fn write_str(&mut self, piece: &str) -> std::fmt::Result {
        self.push_str(piece);
        if self.room.full() {
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
    /// found in one pass over them all rather than one for each rewrite.
    pub fn groups(&self, count: usize) -> Vec<Vec<usize>> {
        let mut groups = vec![Vec::new(); count];
        for (index, edit) in self.edits.iter().enumerate() {
            if let Some(members) = edit.group.and_then(|group| groups.get_mut(group)) {
                members.push(index);
            }
        }
        groups
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
        room: &Room,
    ) -> Option<Vec<(Span, String)>> {
        let mut edits: Vec<&Edit> = members.iter().map(|&index| &self.edits[index]).collect();
        // Each edit's place among them is its own, so the order is total.
        room.sort_unstable_by(&mut edits, |a, b| {
            (a.span.start, a.span.end, a.order).cmp(&(b.span.start, b.span.end, b.order))
        });
        let mut clusters: Vec<(Span, Vec<&Edit>)> = Vec::new();
        for edit in edits {
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
            room.sort_unstable_by(&mut members, |a, b| a.order.cmp(&b.order));
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
            text.push_str(&source[extent.start..extent.end]);
            text.push(' ');
            let local = |span: Span| Span {
                start: span.start - extent.start + 1,
                end: span.end - extent.start + 1,
            };
            let mut copied = 0;
            let mut edits = Edits::default();
            for member in members {
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
                            if !room.take(span.end - span.start) {
                                return None;
                            }
                            Piece::Text(source[span.range()].to_owned())
                        }
                        Piece::Text(text) => {
                            copied += text.len();
                            if !room.take(text.len()) {
                                return None;
                            }
                            Piece::Text(text.clone())
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
    pub fn apply_in(&mut self, source: &str, room: &Room) -> Option<String> {
        // Each edit's order is its own, so the order is total.
        room.sort_unstable_by(&mut self.edits, |a, b| {
            a.span
                .start
                .cmp(&b.span.start)
                .then((a.span.end > a.span.start).cmp(&(b.span.end > b.span.start)))
                .then(b.span.end.cmp(&a.span.end))
                .then(a.order.cmp(&b.order))
        });
        let mut out = Written::new(room);
        let whole = Span {
            start: 0,
            end: source.len(),
        };
        let mut state = State {
            conflicts: Vec::new(),
            inserted: vec![false; self.edits.len()],
        };
        self.render(source, whole, None, &mut out, &mut state);
        self.conflicts = state.conflicts;
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
    ) {
        let whole = span.start == 0 && span.end == source.len() && limit.is_none();
        let first = self
            .edits
            .partition_point(|edit| edit.span.start < span.start);
        let own: Vec<usize> = (first..limit.unwrap_or(self.edits.len()))
            .take_while(|&i| self.edits[i].span.start == span.start)
            .filter(|&i| self.edits[i].span == span && span.end > span.start)
            .collect();
        if let Some(&top) = own.last() {
            for piece in &self.edits[top].pieces {
                match piece {
                    Piece::Text(text) => out.push_str(text),
                    Piece::Source(inner) if *inner == span => {
                        self.render(source, span, Some(top), out, state);
                    }
                    Piece::Source(inner) => self.render(source, *inner, None, out, state),
                }
            }
            return;
        }
        let mut position = span.start;
        let mut index = first;
        while index < self.edits.len() {
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
            if empty {
                if !state.inserted[index] {
                    state.inserted[index] = true;
                    for piece in &edit.pieces {
                        if let Piece::Text(text) = piece {
                            out.push_str(text);
                        }
                    }
                }
                index += 1;
                position = edit.span.start;
                continue;
            }
            self.render(source, edit.span, None, out, state);
            position = edit.span.end;
            index += 1;
        }
        out.push_str(&source[position..span.end]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(start: usize, end: usize) -> Span {
        Span { start, end }
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
