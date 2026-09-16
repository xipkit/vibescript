use super::{Assertion, Class, Part, Set, unicode};
use crate::{CallContext, Error, ErrorKind, Result, budget::Buffer};
use std::hash::{Hash, Hasher};

const FOLD: u8 = 1;
const MULTILINE: u8 = 2;
const DOT_NEWLINE: u8 = 4;
const UNGREEDY: u8 = 8;
pub(super) const UNBOUNDED: usize = usize::MAX;

#[derive(Clone, Copy, Debug)]
pub(super) enum Kind {
    Empty,
    Rune(u32, bool),
    Class(Class),
    Any(bool),
    Assert(Assertion),
    Concat(usize, usize),
    Alt(usize, usize),
    Capture(usize, usize),
    Repeat {
        child: usize,
        min: usize,
        max: usize,
        greedy: bool,
    },
}

#[derive(Clone, Copy, Debug)]
pub(super) struct Node {
    pub kind: Kind,
    pub nullable: bool,
    pub cost: usize,
    pub minimum: usize,
    repetitions: usize,
    height: usize,
    literal: Option<bool>,
    fingerprint: u64,
}

#[derive(Debug)]
pub(super) struct Parsed {
    pub nodes: Buffer<Node>,
    pub parts: Buffer<Part>,
    pub names: Buffer<(usize, usize)>,
    pub root: usize,
}

struct Group {
    sequence: Option<usize>,
    alternate: Option<usize>,
    last: Option<usize>,
    quantified: bool,
    flags: u8,
    capture: usize,
}

impl Group {
    fn new(flags: u8, capture: usize) -> Self {
        Self {
            sequence: None,
            alternate: None,
            last: None,
            quantified: false,
            flags,
            capture,
        }
    }
}

struct Parser<'a> {
    ctx: &'a mut CallContext,
    text: &'a str,
    position: usize,
    parsed: Parsed,
    groups: Buffer<Group>,
}

pub(super) fn parse(ctx: &mut CallContext, pattern: &[u8]) -> Result<Parsed> {
    ctx.work_bytes(pattern.len())?;
    let text = std::str::from_utf8(pattern)
        .map_err(|_| Error::new(ErrorKind::Argument, "regex pattern is not valid UTF-8"))?;
    let mut parser = Parser {
        ctx,
        text,
        position: 0,
        parsed: Parsed {
            nodes: Buffer::empty(),
            parts: Buffer::empty(),
            names: Buffer::empty(),
            root: 0,
        },
        groups: Buffer::empty(),
    };
    parser.groups.push(parser.ctx, Group::new(0, 0))?;
    parser.parsed.names.push(parser.ctx, (0, 0))?;
    while parser.position < text.len() {
        parser.ctx.charge(1)?;
        let flags = parser.group().flags;
        match parser.next()? {
            '(' => parser.open()?,
            ')' => {
                if parser.groups.data.len() == 1 {
                    return Err(parser.error("unmatched closing parenthesis"));
                }
                let child = parser.finish()?;
                let group = parser.groups.data.pop().unwrap();
                let node = if group.capture == 0 {
                    child
                } else {
                    parser.node(Kind::Capture(group.capture, child))?
                };
                parser.atom(node)?;
            }
            '|' => {
                let sequence = parser.sequence()?;
                let alternate = match parser.group().alternate {
                    Some(left) => parser.node(Kind::Alt(left, sequence))?,
                    None => sequence,
                };
                let group = parser.group();
                group.alternate = Some(alternate);
                group.quantified = false;
            }
            '*' => parser.repeat(0, UNBOUNDED, false)?,
            '+' => parser.repeat(1, UNBOUNDED, false)?,
            '?' => parser.repeat(0, 1, false)?,
            '{' => {
                if let Some((min, max, end)) = parser.bounds()? {
                    parser.position = end;
                    if min > 1000 || (max != UNBOUNDED && (max > 1000 || min > max)) {
                        return Err(parser.error("invalid repetition size"));
                    }
                    parser.repeat(min, max, true)?;
                } else {
                    parser.literal('{' as u32)?;
                }
            }
            '[' => {
                let class = parser.class(flags & FOLD != 0)?;
                let node = parser.node(Kind::Class(class))?;
                parser.atom(node)?;
            }
            '.' => {
                let node = parser.node(Kind::Any(flags & DOT_NEWLINE != 0))?;
                parser.atom(node)?;
            }
            '^' | '$' => {
                let begin = text.as_bytes()[parser.position - 1] == b'^';
                let assertion = match (begin, flags & MULTILINE != 0) {
                    (true, true) => Assertion::BeginLine,
                    (true, false) => Assertion::BeginText,
                    (false, true) => Assertion::EndLine,
                    (false, false) => Assertion::EndText,
                };
                let node = parser.node(Kind::Assert(assertion))?;
                parser.atom(node)?;
            }
            '\\' => {
                if parser.peek() == Some(b'Q') {
                    parser.position += 1;
                    while parser.position < text.len() && !parser.remaining().starts_with(b"\\E") {
                        let rune = parser.next()?;
                        parser.literal(rune as u32)?;
                    }
                    if parser.remaining().starts_with(b"\\E") {
                        parser.position += 2;
                    }
                } else if let Some(assertion) = parser.peek().and_then(|byte| match byte {
                    b'A' => Some(Assertion::BeginText),
                    b'z' => Some(Assertion::EndText),
                    b'b' => Some(Assertion::Word),
                    b'B' => Some(Assertion::NotWord),
                    _ => None,
                }) {
                    parser.position += 1;
                    let node = parser.node(Kind::Assert(assertion))?;
                    parser.atom(node)?;
                } else if let Some(part) = parser.escape_set()? {
                    let start = parser.parsed.parts.data.len();
                    parser.parsed.parts.push(parser.ctx, part)?;
                    let node = parser.node(Kind::Class(Class {
                        start,
                        count: 1,
                        negated: false,
                        fold: flags & FOLD != 0,
                    }))?;
                    parser.atom(node)?;
                } else {
                    let rune = parser.escape()?;
                    parser.literal(rune)?;
                }
            }
            rune => parser.literal(rune as u32)?,
        }
    }
    if parser.groups.data.len() != 1 {
        return Err(parser.error("unclosed parenthesis"));
    }
    parser.parsed.root = parser.finish()?;
    Ok(parser.parsed)
}

impl Parser<'_> {
    fn error(&self, message: &str) -> Error {
        Error::new(
            ErrorKind::Argument,
            format!("invalid regex at byte {}: {message}", self.position),
        )
    }

    fn remaining(&self) -> &[u8] {
        &self.text.as_bytes()[self.position..]
    }
    fn peek(&self) -> Option<u8> {
        self.remaining().first().copied()
    }
    fn group(&mut self) -> &mut Group {
        self.groups.data.last_mut().unwrap()
    }

    fn next(&mut self) -> Result<char> {
        self.ctx.charge(1)?;
        let rune = self.text[self.position..]
            .chars()
            .next()
            .ok_or_else(|| self.error("unexpected end of pattern"))?;
        self.position += rune.len_utf8();
        Ok(rune)
    }

    fn node(&mut self, kind: Kind) -> Result<usize> {
        self.ctx.charge(1)?;
        if let Kind::Concat(left, right) = kind {
            if matches!(self.parsed.nodes.data[left].kind, Kind::Empty) {
                return Ok(right);
            }
            if matches!(self.parsed.nodes.data[right].kind, Kind::Empty) {
                return Ok(left);
            }
        }
        if let Kind::Alt(left, right) = kind {
            if self.contains(left, right)? {
                return Ok(left);
            }
            if let Some(class) = self.merge_characters(left, right)? {
                return self.node(Kind::Class(class));
            }
        }
        if let Kind::Class(class) = kind {
            if !class.negated && class.count == 1 {
                if let Part {
                    set: Set::Range(low, high),
                    negated: false,
                } = self.parsed.parts.data[class.start]
                {
                    if low == high {
                        return self.node(Kind::Rune(low, class.fold));
                    }
                }
            }
        }
        let node = |id: usize| self.parsed.nodes.data[id];
        let literal = match kind {
            Kind::Rune(_, fold) => Some(fold),
            Kind::Concat(a, b)
                if node(a).literal.is_some() && node(a).literal == node(b).literal =>
            {
                node(a).literal
            }
            _ => None,
        };
        let height = match kind {
            Kind::Concat(..) if literal.is_some() => 1,
            Kind::Concat(a, b) => (node(a).height
                + usize::from(!matches!(node(a).kind, Kind::Concat(..))))
            .max(node(b).height + usize::from(!matches!(node(b).kind, Kind::Concat(..)))),
            Kind::Alt(a, b) => (node(a).height
                + usize::from(!matches!(node(a).kind, Kind::Alt(..))))
            .max(node(b).height + usize::from(!matches!(node(b).kind, Kind::Alt(..)))),
            Kind::Capture(_, child) | Kind::Repeat { child, .. } => node(child).height + 1,
            _ => 1,
        };
        if height > 1000 {
            return Err(self.error("regex expression exceeds nesting limit"));
        }
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        std::mem::discriminant(&kind).hash(&mut hash);
        match kind {
            Kind::Rune(rune, fold) => (rune, fold).hash(&mut hash),
            Kind::Class(class) => {
                (class.negated, class.fold).hash(&mut hash);
                for part in &self.parsed.parts.data[class.start..class.start + class.count] {
                    self.ctx.charge(1)?;
                    part.hash(&mut hash);
                }
            }
            Kind::Any(newline) => newline.hash(&mut hash),
            Kind::Assert(assertion) => assertion.hash(&mut hash),
            Kind::Concat(a, b) | Kind::Alt(a, b) => {
                (node(a).fingerprint, node(b).fingerprint).hash(&mut hash)
            }
            Kind::Capture(slot, child) => (slot, node(child).fingerprint).hash(&mut hash),
            Kind::Repeat {
                child,
                min,
                max,
                greedy,
            } => (node(child).fingerprint, min, max, greedy).hash(&mut hash),
            Kind::Empty => (),
        }
        let fingerprint = hash.finish();
        let (nullable, cost, repetitions) = match kind {
            Kind::Empty => (true, 0, 1),
            Kind::Assert(_) => (true, 1, 1),
            Kind::Rune(..) | Kind::Class(_) | Kind::Any(_) => (false, 1, 1),
            Kind::Concat(a, b) => (
                node(a).nullable && node(b).nullable,
                node(a).cost.saturating_add(node(b).cost),
                node(a).repetitions.max(node(b).repetitions),
            ),
            Kind::Alt(a, b) => (
                node(a).nullable || node(b).nullable,
                node(a).cost.saturating_add(node(b).cost).saturating_add(1),
                node(a).repetitions.max(node(b).repetitions),
            ),
            Kind::Capture(_, child) => (
                node(child).nullable,
                node(child).cost.saturating_add(2),
                node(child).repetitions,
            ),
            Kind::Repeat {
                child, min, max, ..
            } => {
                let child = node(child);
                let copies = if max == UNBOUNDED { min.max(1) } else { max };
                let branches = if max == UNBOUNDED {
                    1 + usize::from(min == 0 && child.nullable)
                } else {
                    max - min
                };
                let repetitions = if max == 0 {
                    1
                } else {
                    child.repetitions.saturating_mul(if max == UNBOUNDED {
                        min.max(1)
                    } else {
                        max
                    })
                };
                (
                    min == 0 || child.nullable,
                    child.cost.saturating_mul(copies).saturating_add(branches),
                    repetitions,
                )
            }
        };
        let minimum = match kind {
            Kind::Rune(..) | Kind::Class(_) | Kind::Any(_) => 1,
            Kind::Capture(_, child) => node(child).minimum,
            Kind::Concat(a, b) => node(a).minimum.saturating_add(node(b).minimum),
            Kind::Alt(a, b) => node(a).minimum.min(node(b).minimum),
            Kind::Repeat { child, min, .. } => node(child).minimum.saturating_mul(min),
            _ => 0,
        };
        let id = self.parsed.nodes.data.len();
        self.parsed.nodes.push(
            self.ctx,
            Node {
                kind,
                nullable,
                cost: cost.min(super::MAX_INSTRUCTIONS + 1),
                minimum,
                repetitions,
                height,
                literal,
                fingerprint,
            },
        )?;
        Ok(id)
    }

    fn same(&mut self, left: usize, right: usize) -> Result<bool> {
        if left == right {
            return Ok(true);
        }
        if self.parsed.nodes.data[left].fingerprint != self.parsed.nodes.data[right].fingerprint {
            return Ok(false);
        }
        let mut pending = Buffer::empty();
        pending.push(self.ctx, (left, right))?;
        while let Some((left, right)) = pending.data.pop() {
            self.ctx.charge(1)?;
            if left == right {
                continue;
            }
            let (a, b) = (self.parsed.nodes.data[left], self.parsed.nodes.data[right]);
            if a.fingerprint != b.fingerprint {
                return Ok(false);
            }
            match (a.kind, b.kind) {
                (Kind::Empty, Kind::Empty) => (),
                (Kind::Rune(a, af), Kind::Rune(b, bf)) if a == b && af == bf => (),
                (Kind::Any(a), Kind::Any(b)) if a == b => (),
                (Kind::Assert(a), Kind::Assert(b)) if a == b => (),
                (Kind::Concat(a, b), Kind::Concat(c, d)) | (Kind::Alt(a, b), Kind::Alt(c, d)) => {
                    pending.push(self.ctx, (b, d))?;
                    pending.push(self.ctx, (a, c))?;
                }
                (Kind::Capture(a, b), Kind::Capture(c, d)) if a == c => {
                    pending.push(self.ctx, (b, d))?
                }
                (
                    Kind::Repeat {
                        child: a,
                        min: amin,
                        max: amax,
                        greedy: ag,
                    },
                    Kind::Repeat {
                        child: b,
                        min: bmin,
                        max: bmax,
                        greedy: bg,
                    },
                ) if amin == bmin && amax == bmax && ag == bg => pending.push(self.ctx, (a, b))?,
                (Kind::Class(a), Kind::Class(b))
                    if a.negated == b.negated && a.fold == b.fold && a.count == b.count =>
                {
                    for i in 0..a.count {
                        self.ctx.charge(1)?;
                        if self.parsed.parts.data[a.start + i]
                            != self.parsed.parts.data[b.start + i]
                        {
                            return Ok(false);
                        }
                    }
                }
                _ => return Ok(false),
            }
        }
        Ok(true)
    }

    fn contains(&mut self, left: usize, right: usize) -> Result<bool> {
        let mut pending = Buffer::empty();
        pending.push(self.ctx, left)?;
        while let Some(current) = pending.data.pop() {
            self.ctx.charge(1)?;
            if self.same(current, right)? {
                return Ok(true);
            }
            if let Kind::Alt(a, b) = self.parsed.nodes.data[current].kind {
                pending.push(self.ctx, a)?;
                pending.push(self.ctx, b)?;
            }
        }
        Ok(false)
    }

    fn merge_characters(&mut self, left: usize, right: usize) -> Result<Option<Class>> {
        let descriptor = |node: Node| match node.kind {
            Kind::Rune(rune, fold) => Some((None, rune, fold)),
            Kind::Class(class) if !class.negated => Some((Some(class), 0, class.fold)),
            _ => None,
        };
        let (Some((a, ar, af)), Some((b, br, bf))) = (
            descriptor(self.parsed.nodes.data[left]),
            descriptor(self.parsed.nodes.data[right]),
        ) else {
            return Ok(None);
        };
        if af != bf {
            return Ok(None);
        }
        if let (Some(a), Some(b)) = (a, b) {
            if a.start + a.count == b.start {
                return Ok(Some(Class {
                    start: a.start,
                    count: a.count + b.count,
                    negated: false,
                    fold: af,
                }));
            }
        }
        let start = if let Some(a) = a.filter(|a| a.start + a.count == self.parsed.parts.data.len())
        {
            a.start
        } else {
            let start = self.parsed.parts.data.len();
            if let Some(a) = a {
                for i in 0..a.count {
                    self.ctx.charge(1)?;
                    self.parsed
                        .parts
                        .push(self.ctx, self.parsed.parts.data[a.start + i])?;
                }
            } else {
                self.parsed.parts.push(
                    self.ctx,
                    Part {
                        set: Set::Range(ar, ar),
                        negated: false,
                    },
                )?;
            }
            start
        };
        if let Some(b) = b {
            for i in 0..b.count {
                self.ctx.charge(1)?;
                self.parsed
                    .parts
                    .push(self.ctx, self.parsed.parts.data[b.start + i])?;
            }
        } else {
            self.parsed.parts.push(
                self.ctx,
                Part {
                    set: Set::Range(br, br),
                    negated: false,
                },
            )?;
        }
        Ok(Some(Class {
            start,
            count: self.parsed.parts.data.len() - start,
            negated: false,
            fold: af,
        }))
    }

    fn literal(&mut self, rune: u32) -> Result<()> {
        let fold = self.group().flags & FOLD != 0;
        let node = self.node(Kind::Rune(rune, fold))?;
        self.atom(node)
    }

    fn atom(&mut self, node: usize) -> Result<()> {
        if let Some(last) = self.group().last.take() {
            let sequence = match self.group().sequence {
                Some(left) => self.node(Kind::Concat(left, last))?,
                None => last,
            };
            self.group().sequence = Some(sequence);
        }
        let group = self.group();
        group.last = Some(node);
        group.quantified = false;
        Ok(())
    }

    fn sequence(&mut self) -> Result<usize> {
        let last = self.group().last.take();
        let sequence = self.group().sequence.take();
        match (sequence, last) {
            (Some(a), Some(b)) => self.node(Kind::Concat(a, b)),
            (Some(node), None) | (None, Some(node)) => Ok(node),
            (None, None) => self.node(Kind::Empty),
        }
    }

    fn finish(&mut self) -> Result<usize> {
        let sequence = self.sequence()?;
        match self.group().alternate.take() {
            Some(alternate) => self.node(Kind::Alt(alternate, sequence)),
            None => Ok(sequence),
        }
    }

    fn repeat(&mut self, min: usize, max: usize, counted: bool) -> Result<()> {
        if self.group().quantified {
            return Err(self.error("stacked repetition operators"));
        }
        let child = self
            .group()
            .last
            .ok_or_else(|| self.error("repetition has no operand"))?;
        let mut greedy = self.group().flags & UNGREEDY == 0;
        if self.peek() == Some(b'?') {
            self.position += 1;
            greedy = !greedy;
        }
        let node = self.node(Kind::Repeat {
            child,
            min,
            max,
            greedy,
        })?;
        if counted
            && (min >= 2 || (max != UNBOUNDED && max >= 2))
            && self.parsed.nodes.data[node].repetitions > 1000
        {
            return Err(self.error("nested repetition exceeds 1000 copies"));
        }
        let group = self.group();
        group.last = Some(node);
        group.quantified = true;
        Ok(())
    }

    fn bounds(&mut self) -> Result<Option<(usize, usize, usize)>> {
        let bytes = self.text.as_bytes();
        let mut position = self.position;
        let number = |ctx: &mut CallContext, position: &mut usize| -> Result<Option<usize>> {
            let start = *position;
            let mut value = 0usize;
            while bytes.get(*position).is_some_and(u8::is_ascii_digit) {
                ctx.charge(1)?;
                value = value
                    .saturating_mul(10)
                    .saturating_add(usize::from(bytes[*position] - b'0'))
                    .min(1001);
                *position += 1;
            }
            Ok(
                (*position > start && (*position == start + 1 || bytes[start] != b'0'))
                    .then_some(value),
            )
        };
        let Some(min) = number(self.ctx, &mut position)? else {
            return Ok(None);
        };
        let max = if bytes.get(position) == Some(&b',') {
            position += 1;
            if bytes.get(position) == Some(&b'}') {
                UNBOUNDED
            } else if let Some(max) = number(self.ctx, &mut position)? {
                max
            } else {
                return Ok(None);
            }
        } else {
            min
        };
        Ok((bytes.get(position) == Some(&b'}')).then_some((min, max, position + 1)))
    }

    fn open(&mut self) -> Result<()> {
        let mut flags = self.group().flags;
        let mut capture = true;
        let mut name = (0, 0);
        if self.peek() == Some(b'?') {
            self.position += 1;
            if self.remaining().starts_with(b"P<") || self.peek() == Some(b'<') {
                self.position += if self.peek() == Some(b'P') { 2 } else { 1 };
                name.0 = self.position;
                while self
                    .peek()
                    .is_some_and(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
                {
                    self.ctx.charge(1)?;
                    self.position += 1;
                }
                name.1 = self.position;
                if name.0 == name.1 || self.peek() != Some(b'>') {
                    return Err(self.error("invalid capture name"));
                }
                self.position += 1;
            } else {
                capture = false;
                let mut negative = false;
                let mut saw_flag = false;
                loop {
                    match self.next()? {
                        ')' | ':' => {
                            if negative && !saw_flag {
                                return Err(self.error("missing flag after minus"));
                            }
                            if self.text.as_bytes()[self.position - 1] == b')' {
                                self.group().flags = flags;
                                self.group().quantified = false;
                                return Ok(());
                            }
                            break;
                        }
                        '-' if !negative => {
                            negative = true;
                            saw_flag = false;
                        }
                        flag @ ('i' | 'm' | 's' | 'U') => {
                            let bit = match flag {
                                'i' => FOLD,
                                'm' => MULTILINE,
                                's' => DOT_NEWLINE,
                                _ => UNGREEDY,
                            };
                            if negative {
                                flags &= !bit;
                            } else {
                                flags |= bit;
                            }
                            saw_flag = true;
                        }
                        _ => return Err(self.error("unsupported group or flag")),
                    }
                }
            }
        }
        let capture = if capture {
            let id = self.parsed.names.data.len();
            self.parsed.names.push(self.ctx, name)?;
            id
        } else {
            0
        };
        self.groups.push(self.ctx, Group::new(flags, capture))
    }

    fn escape(&mut self) -> Result<u32> {
        let rune = self.next()?;
        match rune {
            'a' => Ok(7),
            'f' => Ok(12),
            'n' => Ok(10),
            'r' => Ok(13),
            't' => Ok(9),
            'v' => Ok(11),
            '0'..='7' => {
                if rune != '0' && !self.peek().is_some_and(|b| matches!(b, b'0'..=b'7')) {
                    return Err(self.error("backreferences are not supported"));
                }
                let mut value = rune as u32 - '0' as u32;
                for _ in 0..2 {
                    let Some(byte @ b'0'..=b'7') = self.peek() else {
                        break;
                    };
                    self.position += 1;
                    value = value * 8 + u32::from(byte - b'0');
                }
                Ok(value)
            }
            'x' => {
                let mut value = 0u32;
                if self.peek() == Some(b'{') {
                    self.position += 1;
                    let begin = self.position;
                    while self.peek() != Some(b'}') {
                        let digit = self
                            .next()?
                            .to_digit(16)
                            .ok_or_else(|| self.error("invalid hexadecimal escape"))?;
                        value = value.saturating_mul(16).saturating_add(digit);
                        if value > 0x10ffff {
                            return Err(self.error("escape exceeds Unicode range"));
                        }
                    }
                    if self.position == begin {
                        return Err(self.error("empty hexadecimal escape"));
                    }
                    self.position += 1;
                } else {
                    for _ in 0..2 {
                        value = value * 16
                            + self
                                .next()?
                                .to_digit(16)
                                .ok_or_else(|| self.error("invalid hexadecimal escape"))?;
                    }
                }
                Ok(value)
            }
            rune if rune.is_ascii() && !rune.is_ascii_alphanumeric() => Ok(rune as u32),
            _ => Err(self.error("unsupported escape")),
        }
    }

    fn escape_set(&mut self) -> Result<Option<Part>> {
        let Some(byte) = self.peek() else {
            return Ok(None);
        };
        let set = match byte.to_ascii_lowercase() {
            b'd' => Some(Set::Digit),
            b's' => Some(Set::PerlSpace),
            b'w' => Some(Set::Word),
            _ => None,
        };
        if let Some(set) = set {
            self.position += 1;
            return Ok(Some(Part {
                set,
                negated: byte.is_ascii_uppercase(),
            }));
        }
        if !matches!(byte, b'p' | b'P') {
            return Ok(None);
        }
        self.position += 1;
        let name = if self.peek() == Some(b'{') {
            self.position += 1;
            let start = self.position;
            while self.peek() != Some(b'}') {
                self.next()?;
            }
            let end = self.position;
            self.position += 1;
            &self.text.as_bytes()[start..end]
        } else {
            let start = self.position;
            self.next()?;
            &self.text.as_bytes()[start..self.position]
        };
        let negated = (byte == b'P') ^ name.starts_with(b"^");
        let name = name.strip_prefix(b"^").unwrap_or(name);
        let (group, invert) =
            unicode::group(name).ok_or_else(|| self.error("unknown Unicode character class"))?;
        Ok(Some(Part {
            set: Set::Property(group),
            negated: negated ^ invert,
        }))
    }

    fn class(&mut self, fold: bool) -> Result<Class> {
        let start = self.parsed.parts.data.len();
        let negated = self.peek() == Some(b'^');
        if negated {
            self.position += 1;
        }
        let mut first = true;
        loop {
            self.ctx.charge(1)?;
            if !first && self.peek() == Some(b']') {
                self.position += 1;
                break;
            }
            first = false;
            if self.remaining().starts_with(b"[:") {
                let mut end = self.position + 2;
                while end + 1 < self.text.len() && &self.text.as_bytes()[end..end + 2] != b":]" {
                    self.ctx.charge(1)?;
                    end += 1;
                }
                if end + 1 < self.text.len() {
                    let name = &self.text.as_bytes()[self.position + 2..end];
                    let inverted = name.starts_with(b"^");
                    let name = name.strip_prefix(b"^").unwrap_or(name);
                    let set = match name {
                        b"alnum" => Set::Alnum,
                        b"alpha" => Set::Alpha,
                        b"ascii" => Set::Range(0, 127),
                        b"blank" => Set::Blank,
                        b"cntrl" => Set::Control,
                        b"digit" => Set::Digit,
                        b"graph" => Set::Range(33, 126),
                        b"lower" => Set::Range(97, 122),
                        b"print" => Set::Range(32, 126),
                        b"punct" => Set::Punct,
                        b"space" => Set::Space,
                        b"upper" => Set::Range(65, 90),
                        b"word" => Set::Word,
                        b"xdigit" => Set::Hex,
                        _ => return Err(self.error("unknown POSIX character class")),
                    };
                    self.parsed.parts.push(
                        self.ctx,
                        Part {
                            set,
                            negated: inverted,
                        },
                    )?;
                    self.position = end + 2;
                    continue;
                }
            }
            let mut rune = self.next()? as u32;
            if rune == '\\' as u32 {
                if let Some(part) = self.escape_set()? {
                    self.parsed.parts.push(self.ctx, part)?;
                    continue;
                }
                rune = self.escape()?;
            }
            let high = if self.peek() == Some(b'-') && self.remaining().get(1) != Some(&b']') {
                self.position += 1;
                let mut high = self.next()? as u32;
                if high == '\\' as u32 {
                    high = self.escape()?;
                }
                if high < rune {
                    return Err(self.error("reversed character range"));
                }
                high
            } else {
                rune
            };
            self.parsed.parts.push(
                self.ctx,
                Part {
                    set: Set::Range(rune, high),
                    negated: false,
                },
            )?;
        }
        Ok(Class {
            start,
            count: self.parsed.parts.data.len() - start,
            negated,
            fold,
        })
    }
}
