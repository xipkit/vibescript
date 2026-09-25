//! Source edits that compose: an edit replaces a span with text and pieces
//! of the original source, and those pieces are rendered with the edits
//! made inside them. A rewrite of `x.nil?` can therefore embed `x` after
//! another rule has rewritten something inside `x`.

use super::syntax::Span;

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
    /// Whether no edit was made.
    pub fn is_empty(&self) -> bool {
        self.edits.is_empty()
    }

    /// Makes later edits belong to `group`, returning the group they
    /// belonged to before.
    pub fn enter(&mut self, group: Option<usize>) -> Option<usize> {
        std::mem::replace(&mut self.group, group)
    }

    /// The span and replacement of each edit in `group`, in the order they
    /// were made.
    pub fn group(&self, group: usize) -> impl Iterator<Item = (Span, &[Piece])> {
        let mut edits: Vec<&Edit> = self
            .edits
            .iter()
            .filter(|edit| edit.group == Some(group))
            .collect();
        edits.sort_by_key(|edit| edit.order);
        edits
            .into_iter()
            .map(|edit| (edit.span, edit.pieces.as_slice()))
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
    pub fn apply(&mut self, source: &str) -> String {
        self.edits.sort_by(|a, b| {
            a.span
                .start
                .cmp(&b.span.start)
                .then((a.span.end > a.span.start).cmp(&(b.span.end > b.span.start)))
                .then(b.span.end.cmp(&a.span.end))
                .then(a.order.cmp(&b.order))
        });
        let mut out = String::with_capacity(source.len());
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
        out
    }

    /// Renders `span`, where `limit` bounds which edits of exactly this span
    /// apply: those before that position in the sorted list.
    fn render(
        &self,
        source: &str,
        span: Span,
        limit: Option<usize>,
        out: &mut String,
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

/// A replacement as text, with each [`Piece::Source`] read from `source`
/// as written, for an edit applied on its own.
pub fn render(source: &str, pieces: &[Piece]) -> String {
    pieces
        .iter()
        .map(|piece| match piece {
            Piece::Text(text) => text.as_str(),
            Piece::Source(span) => &source[span.start..span.end],
        })
        .collect()
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
    fn overlapping_edits_are_reported() {
        let mut edits = Edits::default();
        edits.text(span(0, 3), "x");
        edits.text(span(2, 5), "y");
        assert_eq!(edits.apply("abcdef"), "xdef");
        assert_eq!(edits.conflicts, [span(2, 5)]);
    }
}
