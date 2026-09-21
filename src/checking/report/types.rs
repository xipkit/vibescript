use super::super::facts::{Atom, Fact, Facts, Node, NominalId};
use crate::{
    CallContext, Result,
    budget::{Buffer, Charge},
};

const LIMIT: usize = 4096;
const DEPTH: usize = 16;
const ITEMS: usize = 16;

pub(super) struct Writer<'a> {
    pub ctx: &'a mut CallContext,
    bytes: Buffer<u8>,
    truncated: bool,
}

impl<'a> Writer<'a> {
    pub fn new(ctx: &'a mut CallContext) -> Self {
        Self {
            ctx,
            bytes: Buffer::empty(),
            truncated: false,
        }
    }

    pub fn finish(self) -> (String, Option<Charge>) {
        let (bytes, charge) = self.bytes.into_parts();
        (
            String::from_utf8(bytes).expect("diagnostic writer emits UTF-8"),
            charge,
        )
    }

    pub fn text(&mut self, text: &str) -> Result<()> {
        if self.truncated {
            return Ok(());
        }
        let mut length = text.len().min(LIMIT - 3 - self.bytes.data.len());
        while !text.is_char_boundary(length) {
            length -= 1;
        }
        self.ctx.work_bytes(length)?;
        self.bytes.extend(self.ctx, &text.as_bytes()[..length])?;
        if length != text.len() {
            self.bytes.extend(self.ctx, b"...")?;
            self.truncated = true;
        }
        Ok(())
    }

    pub fn escaped(&mut self, bytes: &[u8]) -> Result<()> {
        for chunk in bytes.utf8_chunks() {
            for ch in chunk.valid().chars() {
                if self.truncated {
                    return Ok(());
                }
                self.ctx.charge(1)?;
                for escaped in ch.escape_debug() {
                    self.text(escaped.encode_utf8(&mut [0; 4]))?;
                }
            }
            for &byte in chunk.invalid() {
                if self.truncated {
                    return Ok(());
                }
                let hex = b"0123456789abcdef";
                let escaped = [
                    b'\\',
                    b'x',
                    hex[(byte >> 4) as usize],
                    hex[(byte & 15) as usize],
                ];
                self.text(std::str::from_utf8(&escaped).unwrap())?;
            }
        }
        Ok(())
    }

    pub fn quoted(&mut self, bytes: &[u8]) -> Result<()> {
        self.text("\"")?;
        self.escaped(bytes)?;
        self.text("\"")
    }

    pub fn number(&mut self, number: usize) -> Result<()> {
        // A decimal usize fits in this fixed stack buffer.
        let mut digits = [0; 20];
        let mut number = number;
        let mut start = digits.len();
        loop {
            start -= 1;
            digits[start] = b'0' + (number % 10) as u8;
            number /= 10;
            if number == 0 {
                break;
            }
        }
        self.text(std::str::from_utf8(&digits[start..]).unwrap())
    }

    pub fn fact(&mut self, facts: &Facts, fact: Fact) -> Result<()> {
        self.describe(facts, fact, 0)
    }

    pub fn mismatch(&mut self, facts: &Facts, actual: Fact, expected: Fact) -> Result<()> {
        self.text("expected ")?;
        self.fact(facts, expected)?;
        self.text(", got ")?;
        self.fact(facts, actual)?;
        if let (Some(a), Some(b)) = (nominal(facts, actual), nominal(facts, expected)) {
            if a != b {
                self.text(" (different declaration)")?;
            }
        }
        Ok(())
    }

    fn describe(&mut self, facts: &Facts, fact: Fact, depth: usize) -> Result<()> {
        if self.truncated {
            return Ok(());
        }
        self.ctx.charge(1)?;
        if depth == DEPTH {
            return self.text("...");
        }
        match facts.node(fact) {
            Node::Atom(atom) => self.text(match atom {
                Atom::Never => "never",
                Atom::Unknown => "unknown",
                Atom::Any => "any",
                Atom::Nil => "nil",
                Atom::Bool => "bool",
                Atom::Int => "int",
                Atom::Float => "float",
                Atom::String => "string",
                Atom::Symbol => "symbol",
                Atom::Duration => "duration",
                Atom::Time => "time",
                Atom::Money => "money",
                Atom::Range => "range",
                Atom::Regex => "regex",
            }),
            Node::Boolean(_) => self.text("bool"),
            Node::Integer(_) | Node::IntegerBounds(_) => self.text("int"),
            Node::Float(_) => self.text("float"),
            Node::String(_) => self.text("string"),
            Node::Symbol(_) => self.text("symbol"),
            Node::Range(..) => self.text("range"),
            Node::Regex(_) => self.text("regex"),
            Node::Builtin(_) | Node::Offset(_) => self.text("builtin"),
            Node::Callable { .. } => self.text("attached method"),
            Node::Protected(value, ..) => self.describe(facts, *value, depth + 1),
            Node::TypeValue(value) => {
                self.text("type<")?;
                self.describe(facts, *value, depth + 1)?;
                self.text(">")
            }
            Node::Instance { class, .. } => self.describe(facts, *class, depth + 1),
            Node::Enumeration { nominal, .. } => {
                self.text("enum ")?;
                self.describe(facts, *nominal, depth + 1)
            }
            Node::EnumMember { enumeration, .. } => {
                let Node::Enumeration { nominal, .. } = facts.node(*enumeration) else {
                    unreachable!()
                };
                self.describe(facts, *nominal, depth + 1)
            }
            Node::Array(item) => {
                self.text("array<")?;
                self.describe(facts, *item, depth + 1)?;
                self.text(">")
            }
            Node::Tuple(items) => {
                self.text("[")?;
                self.list(facts, &items.data, depth, ", ")?;
                self.text("]")
            }
            Node::Hash(key, value, _) => {
                self.text("hash<")?;
                self.describe(facts, *key, depth + 1)?;
                self.text(", ")?;
                self.describe(facts, *value, depth + 1)?;
                self.text(">")
            }
            Node::Shape(fields, open, _, _) => {
                self.text("{")?;
                for (index, field) in fields.data.iter().take(ITEMS).enumerate() {
                    if self.truncated {
                        break;
                    }
                    if index > 0 {
                        self.text(", ")?;
                    }
                    self.quoted(field.name.as_bytes().unwrap())?;
                    if field.optional {
                        self.text("?")?;
                    }
                    self.text(": ")?;
                    self.describe(facts, field.value, depth + 1)?;
                }
                if *open || fields.data.len() > ITEMS {
                    if !fields.data.is_empty() {
                        self.text(", ")?;
                    }
                    self.text("...")?;
                }
                self.text("}")
            }
            Node::Union(items) | Node::Choice(items) => self.list(facts, &items.data, depth, " | "),
            Node::Named(name) | Node::Nominal { name, .. } => {
                self.escaped(name.as_bytes().unwrap())
            }
        }
    }

    fn list(&mut self, facts: &Facts, items: &[Fact], depth: usize, separator: &str) -> Result<()> {
        for (index, &item) in items.iter().take(ITEMS).enumerate() {
            if self.truncated {
                break;
            }
            if index > 0 {
                self.text(separator)?;
            }
            self.describe(facts, item, depth + 1)?;
        }
        if items.len() > ITEMS {
            self.text(separator)?;
            self.text("...")?;
        }
        Ok(())
    }
}

impl crate::shapes::TypeWriter for Writer<'_> {
    fn write(&mut self, bytes: &[u8]) -> Result<()> {
        self.escaped(bytes)
    }
    fn node(&mut self) -> Result<()> {
        self.ctx.charge(1)
    }
}

fn nominal(facts: &Facts, mut fact: Fact) -> Option<NominalId> {
    for _ in 0..DEPTH {
        match facts.node(fact) {
            Node::Nominal { identity, .. } => return Some(*identity),
            Node::Instance { class, .. } => fact = *class,
            Node::Protected(inner, ..) => fact = *inner,
            Node::Enumeration { nominal, .. } => fact = *nominal,
            Node::EnumMember { enumeration, .. } => fact = *enumeration,
            _ => return None,
        }
    }
    None
}
