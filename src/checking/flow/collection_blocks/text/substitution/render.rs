use super::*;
use crate::budget::MAX_VALUE_DEPTH;

#[derive(Clone, Copy)]
pub(super) struct Rendered {
    pub value: Fact,
    minimum: usize,
    maximum: usize,
    pub limited: bool,
    exact: bool,
}

impl Rendered {
    fn failed() -> Self {
        Self {
            value: Atom::Never.fact(),
            minimum: 0,
            maximum: 0,
            limited: true,
            exact: false,
        }
    }

    fn unknown(maximum: usize, limited: bool) -> Self {
        Self {
            value: Atom::String.fact(),
            minimum: 0,
            maximum,
            limited,
            exact: false,
        }
    }
}

#[derive(Clone, Copy)]
enum Join {
    Union,
    Tuple,
    Hash,
    Protected,
}

struct Frame {
    value: Fact,
    depth: usize,
    index: usize,
    kind: Join,
    output: Rendered,
}

struct Cached {
    input: Fact,
    depth: usize,
    output: Rendered,
}

enum Part {
    Value(Rendered),
    Frame(Frame),
}

impl Walker<'_> {
    pub(super) fn substitution_bytes(&mut self, bytes: &[u8]) -> Result<Rendered> {
        self.ctx.charge(1)?;
        if bytes.len() > MAX_TEXT {
            return Ok(Rendered::failed());
        }
        Ok(Rendered {
            value: self.facts.string(self.ctx, bytes)?,
            minimum: bytes.len(),
            maximum: bytes.len(),
            limited: false,
            exact: true,
        })
    }

    pub(super) fn substitution_string(&mut self, value: Fact) -> Result<Rendered> {
        let mut result = Rendered {
            limited: false,
            ..Rendered::failed()
        };
        for i in 0..self.facts.arm_count(value) {
            self.ctx.charge(1)?;
            let arm = self.facts.arm(value, i);
            let next = match self.facts.node(arm) {
                Node::Atom(Atom::Never) => continue,
                Node::String(bytes) => {
                    let length = bytes.as_bytes().unwrap().len();
                    if length > MAX_TEXT {
                        Rendered::failed()
                    } else {
                        Rendered {
                            value: arm,
                            minimum: length,
                            maximum: length,
                            limited: false,
                            exact: true,
                        }
                    }
                }
                _ => Rendered::unknown(MAX_TEXT, false),
            };
            result = self.substitution_join(result, next)?;
        }
        Ok(result)
    }

    fn substitution_join(&mut self, a: Rendered, b: Rendered) -> Result<Rendered> {
        self.ctx.charge(1)?;
        if a.value == Atom::Never.fact() {
            return Ok(Rendered {
                limited: a.limited || b.limited,
                ..b
            });
        }
        if b.value == Atom::Never.fact() {
            return Ok(Rendered {
                limited: a.limited || b.limited,
                ..a
            });
        }
        Ok(Rendered {
            value: self.facts.union(self.ctx, &[a.value, b.value])?,
            minimum: a.minimum.min(b.minimum),
            maximum: a.maximum.max(b.maximum),
            limited: a.limited || b.limited,
            exact: a.exact
                && b.exact
                && a.value == b.value
                && matches!(self.facts.node(a.value), Node::String(_)),
        })
    }

    pub(super) fn substitution_concat(&mut self, a: Rendered, b: Rendered) -> Result<Rendered> {
        self.substitution_combine(a, b, true)
    }

    fn substitution_measure_concat(&mut self, a: Rendered, b: Rendered) -> Result<Rendered> {
        self.substitution_combine(a, b, false)
    }

    fn substitution_combine(
        &mut self,
        a: Rendered,
        b: Rendered,
        materialize: bool,
    ) -> Result<Rendered> {
        self.ctx.charge(1)?;
        if a.value == Atom::Never.fact() || b.value == Atom::Never.fact() {
            return Ok(Rendered {
                limited: a.limited || b.limited,
                ..Rendered::failed()
            });
        }
        let minimum = a.minimum.saturating_add(b.minimum);
        let maximum = a.maximum.saturating_add(b.maximum);
        if minimum > MAX_TEXT {
            return Ok(Rendered::failed());
        }
        let value = if a.maximum == 0 {
            b.value
        } else if b.maximum == 0 {
            a.value
        } else if !materialize {
            Atom::String.fact()
        } else if let (Node::String(left), Node::String(right)) =
            (self.facts.node(a.value), self.facts.node(b.value))
        {
            let mut bytes = Buffer::with_capacity(self.ctx, minimum)?;
            bytes.extend(self.ctx, left.as_bytes().unwrap())?;
            bytes.extend(self.ctx, right.as_bytes().unwrap())?;
            self.facts.string(self.ctx, &bytes.data)?
        } else {
            Atom::String.fact()
        };
        Ok(Rendered {
            value,
            minimum,
            maximum: maximum.min(MAX_TEXT),
            limited: a.limited || b.limited || maximum > MAX_TEXT,
            exact: a.exact && b.exact,
        })
    }

    fn substitution_part(&mut self, value: Fact, depth: usize) -> Result<Part> {
        self.ctx.charge(1)?;
        let node = self.facts.node(value);
        if depth >= MAX_VALUE_DEPTH
            && matches!(
                node,
                Node::Tuple(_)
                    | Node::Array(_)
                    | Node::Hash(..)
                    | Node::Shape(..)
                    | Node::Protected(..)
            )
        {
            return Ok(Part::Value(Rendered::failed()));
        }
        let kind = match node {
            Node::Union(_) => Some(Join::Union),
            Node::Tuple(_) => Some(Join::Tuple),
            Node::Shape(fields, false, _, HashKind::Plain) => {
                let mut required = true;
                for field in &fields.data {
                    self.ctx.charge(1)?;
                    required &= !field.optional;
                }
                required.then_some(Join::Hash)
            }
            Node::Protected(..) => Some(Join::Protected),
            _ => None,
        };
        if let Some(kind) = kind {
            let output = match kind {
                Join::Tuple => self.substitution_bytes(b"[")?,
                Join::Hash => self.substitution_bytes(b"{")?,
                _ => Rendered {
                    limited: false,
                    ..Rendered::failed()
                },
            };
            return Ok(Part::Frame(Frame {
                value,
                depth,
                index: 0,
                kind,
                output,
            }));
        }
        let literal = match self.facts.node(value) {
            Node::Atom(Atom::Never) => {
                return Ok(Part::Value(Rendered {
                    limited: false,
                    ..Rendered::failed()
                }));
            }
            Node::Atom(Atom::Nil) => Some(Value::nil()),
            Node::Boolean(v) => Some(Value::boolean(*v)),
            Node::Integer(v) => Some(Value::int(*v)),
            Node::Float(bits) => Some(Value::float(f64::from_bits(*bits))),
            Node::String(_) => return self.substitution_string(value).map(Part::Value),
            Node::Symbol(v) | Node::Regex(v) => Some(v.clone()),
            Node::Range(start, end, exclusive) => Some(Value(Kind::Range(
                crate::range::Range::new(self.ctx, *start, *end, *exclusive)?,
            ))),
            Node::Builtin(_) | Node::Offset(_) => {
                return self.substitution_bytes(b"<builtin>").map(Part::Value);
            }
            _ => None,
        };
        if let Some(value) = literal {
            let value = match crate::text::bounded::render(self.ctx, &value, MAX_TEXT) {
                Ok(value) => value,
                Err(error) if !self.ctx.exhausted() && error.class() == Some(ErrorClass::Limit) => {
                    return Ok(Part::Value(Rendered::failed()));
                }
                Err(error) => return Err(error),
            };
            return self
                .substitution_bytes(value.as_bytes().unwrap())
                .map(Part::Value);
        }
        if builtins::namespace(self.ctx, self.facts, value)? {
            return self.substitution_bytes(b"<object>").map(Part::Value);
        }
        let rendered = match self.facts.atom(value) {
            Some(Atom::Bool) => Rendered::unknown(5, false),
            Some(Atom::Float) => Rendered::unknown(64, false),
            _ => Rendered::unknown(MAX_TEXT, true),
        };
        Ok(Part::Value(rendered))
    }

    pub(super) fn substitution_render(&mut self, value: Fact) -> Result<Rendered> {
        let mut memo = Buffer::empty();
        let mut rendered = self.substitution_measure(value, &mut memo)?;
        if rendered.exact && !matches!(self.facts.node(rendered.value), Node::String(_)) {
            let mut bytes = Buffer::with_capacity(self.ctx, rendered.maximum)?;
            self.substitution_emit(value, &memo, &mut bytes)?;
            debug_assert_eq!(bytes.data.len(), rendered.minimum);
            rendered.value = self.facts.string(self.ctx, &bytes.data)?;
        }
        Ok(rendered)
    }

    fn substitution_cached(
        &mut self,
        value: Fact,
        depth: usize,
        memo: &Buffer<Cached>,
    ) -> Result<Part> {
        for entry in &memo.data {
            self.ctx.charge(1)?;
            if entry.input == value && entry.depth == depth {
                return Ok(Part::Value(entry.output));
            }
        }
        self.substitution_part(value, depth)
    }

    fn substitution_measure(&mut self, value: Fact, memo: &mut Buffer<Cached>) -> Result<Rendered> {
        let mut stack: Buffer<Frame> = Buffer::empty();
        let mut part = self.substitution_part(value, 0)?;
        loop {
            self.ctx.charge(1)?;
            let mut frame = match part {
                Part::Frame(frame) => frame,
                Part::Value(value) => {
                    let Some(mut frame) = stack.data.pop() else {
                        return Ok(value);
                    };
                    frame.output = match frame.kind {
                        Join::Union => self.substitution_join(frame.output, value)?,
                        Join::Protected => value,
                        _ => self.substitution_measure_concat(frame.output, value)?,
                    };
                    frame.index += 1;
                    frame
                }
            };
            if !matches!(frame.kind, Join::Union | Join::Protected)
                && frame.output.value == Atom::Never.fact()
            {
                memo.push(
                    self.ctx,
                    Cached {
                        input: frame.value,
                        depth: frame.depth,
                        output: frame.output,
                    },
                )?;
                part = Part::Value(frame.output);
                continue;
            }
            let child = match self.facts.node(frame.value) {
                Node::Union(items) | Node::Tuple(items) => items
                    .data
                    .get(frame.index)
                    .copied()
                    .map(|value| (value, None)),
                Node::Shape(fields, ..) => fields
                    .data
                    .get(frame.index)
                    .map(|field| (field.value, Some(field.name.clone()))),
                Node::Protected(shape, _) if frame.index == 0 => self
                    .facts
                    .selected_field(self.ctx, *shape, b"to_s")?
                    .map(|(value, _)| (value, None)),
                Node::Protected(..) => None,
                _ => unreachable!(),
            };
            let Some((child, name)) = child else {
                if matches!(frame.kind, Join::Tuple | Join::Hash) {
                    let suffix = self.substitution_bytes(if matches!(frame.kind, Join::Tuple) {
                        b"]"
                    } else {
                        b"}"
                    })?;
                    frame.output = self.substitution_measure_concat(frame.output, suffix)?;
                    if matches!(frame.kind, Join::Hash)
                        && frame.index > 1
                        && frame.output.value != Atom::Never.fact()
                    {
                        // Shape fields are canonicalized; their insertion order is not retained.
                        frame.output.value = Atom::String.fact();
                        frame.output.exact = false;
                    }
                }
                memo.push(
                    self.ctx,
                    Cached {
                        input: frame.value,
                        depth: frame.depth,
                        output: frame.output,
                    },
                )?;
                part = Part::Value(frame.output);
                continue;
            };
            if matches!(frame.kind, Join::Tuple | Join::Hash) && frame.index != 0 {
                let comma = self.substitution_bytes(b", ")?;
                frame.output = self.substitution_measure_concat(frame.output, comma)?;
            }
            if let Some(name) = name {
                let name = self.substitution_bytes(name.as_bytes().unwrap())?;
                frame.output = self.substitution_measure_concat(frame.output, name)?;
                let colon = self.substitution_bytes(b": ")?;
                frame.output = self.substitution_measure_concat(frame.output, colon)?;
            }
            if !matches!(frame.kind, Join::Union | Join::Protected)
                && frame.output.value == Atom::Never.fact()
            {
                memo.push(
                    self.ctx,
                    Cached {
                        input: frame.value,
                        depth: frame.depth,
                        output: frame.output,
                    },
                )?;
                part = Part::Value(frame.output);
                continue;
            }
            let depth = frame.depth + usize::from(matches!(frame.kind, Join::Tuple | Join::Hash));
            stack.push(self.ctx, frame)?;
            part = self.substitution_cached(child, depth, memo)?;
        }
    }

    fn substitution_emit(
        &mut self,
        value: Fact,
        memo: &Buffer<Cached>,
        output: &mut Buffer<u8>,
    ) -> Result<()> {
        let mut stack = Buffer::empty();
        let mut part = self.substitution_part(value, 0)?;
        loop {
            self.ctx.charge(1)?;
            let mut frame = match part {
                Part::Frame(frame) => frame,
                Part::Value(rendered) => {
                    let Node::String(value) = self.facts.node(rendered.value) else {
                        unreachable!("only exact rendering is emitted");
                    };
                    output.extend(self.ctx, value.as_bytes().unwrap())?;
                    let Some(frame) = stack.data.pop() else {
                        return Ok(());
                    };
                    frame
                }
            };
            if frame.index == 0 {
                match frame.kind {
                    Join::Tuple => output.extend(self.ctx, b"[")?,
                    Join::Hash => output.extend(self.ctx, b"{")?,
                    _ => (),
                }
            }
            if matches!(frame.kind, Join::Union) {
                let mut selected = None;
                for i in 0..self.facts.arm_count(frame.value) {
                    self.ctx.charge(1)?;
                    let value = self.facts.arm(frame.value, i);
                    let candidate = self.substitution_cached(value, frame.depth, memo)?;
                    if matches!(candidate, Part::Value(v) if v.value != Atom::Never.fact()) {
                        selected = Some(value);
                        break;
                    }
                }
                part = self.substitution_part(selected.unwrap(), frame.depth)?;
                continue;
            }
            let child = match self.facts.node(frame.value) {
                Node::Tuple(items) => items.data.get(frame.index).copied().map(|v| (v, None)),
                Node::Shape(fields, ..) => fields
                    .data
                    .get(frame.index)
                    .map(|field| (field.value, Some(field.name.clone()))),
                Node::Protected(shape, _) if frame.index == 0 => self
                    .facts
                    .selected_field(self.ctx, *shape, b"to_s")?
                    .map(|(value, _)| (value, None)),
                Node::Protected(..) => None,
                _ => unreachable!(),
            };
            let Some((child, name)) = child else {
                match frame.kind {
                    Join::Tuple => output.extend(self.ctx, b"]")?,
                    Join::Hash => output.extend(self.ctx, b"}")?,
                    _ => (),
                }
                let Some(parent) = stack.data.pop() else {
                    return Ok(());
                };
                part = Part::Frame(parent);
                continue;
            };
            if matches!(frame.kind, Join::Tuple | Join::Hash) && frame.index != 0 {
                output.extend(self.ctx, b", ")?;
            }
            if let Some(name) = name {
                output.extend(self.ctx, name.as_bytes().unwrap())?;
                output.extend(self.ctx, b": ")?;
            }
            let depth = frame.depth + usize::from(matches!(frame.kind, Join::Tuple | Join::Hash));
            frame.index += 1;
            stack.push(self.ctx, frame)?;
            part = self.substitution_part(child, depth)?;
        }
    }
}
