use super::{
    Part, Program, Search,
    program::{Instruction, View},
};
use crate::{
    CallContext, Error, ErrorKind, Result, Value,
    budget::{Buffer, Charge},
    ops,
    value::Kind,
};
use std::sync::Arc;

#[derive(Debug)]
struct Code {
    instructions: Vec<Instruction>,
    parts: Vec<Part>,
    names: Vec<(usize, usize)>,
    start: usize,
    minimum: usize,
}

#[derive(Debug)]
pub(crate) struct Regex {
    pub source: Value,
    pattern: Value,
    flags: u8,
    code: Arc<Code>,
    header: Option<Charge>,
    _metadata: Option<Charge>,
    _storage: [Option<Charge>; 3],
}

impl Regex {
    pub fn compile(ctx: &mut CallContext, source: Value, flags: u8) -> Result<Value> {
        if source.require_bytes()?.len() > super::MAX_PATTERN {
            return ctx.guard(ErrorKind::Memory, "regex pattern exceeds 16 KiB");
        }
        let source = ctx.import(&source)?;
        let pattern = if flags == 0 {
            source.clone()
        } else {
            let mut bytes = Buffer::with_capacity(ctx, source.require_bytes()?.len() + 8)?;
            if flags & 1 != 0 {
                bytes.extend(ctx, b"(?i)")?;
            }
            if flags & 2 != 0 {
                bytes.extend(ctx, b"(?s)")?;
            }
            bytes.extend(ctx, source.require_bytes()?)?;
            Value::from_bytes(ctx, bytes)?
        };
        let program = Program::compile_limit(ctx, pattern, super::MAX_PATTERN + 8)?;
        let header = ctx.reserve(size_of::<Self>() + 2 * size_of::<usize>())?;
        let metadata = ctx.reserve(size_of::<Code>() + 2 * size_of::<usize>())?;
        let (instructions, a) = program.instructions.into_parts();
        let (parts, b) = program.parts.into_parts();
        let (names, c) = program.names.into_parts();
        Ok(Value(Kind::Regex(Arc::new(Self {
            source,
            pattern: program.source,
            flags,
            code: Arc::new(Code {
                instructions,
                parts,
                names,
                start: program.start,
                minimum: program.minimum,
            }),
            header,
            _metadata: metadata,
            _storage: [a, b, c],
        }))))
    }

    pub fn import(ctx: &mut CallContext, value: &Arc<Self>) -> Result<Arc<Self>> {
        if ctx.owns(&value.header) || ctx.options.limits.memory_bytes.is_none() {
            return Ok(value.clone());
        }
        let source = ctx.import(&value.source)?;
        let pattern = if value.flags == 0 {
            source.clone()
        } else {
            ctx.import(&value.pattern)?
        };
        let header = ctx.reserve(size_of::<Self>() + 2 * size_of::<usize>())?;
        let metadata = ctx.reserve(size_of::<Code>() + 2 * size_of::<usize>())?;
        let storage = [
            ctx.reserve(value.code.instructions.capacity() * size_of::<Instruction>())?,
            ctx.reserve(value.code.parts.capacity() * size_of::<Part>())?,
            ctx.reserve(value.code.names.capacity() * size_of::<(usize, usize)>())?,
        ];
        Ok(Arc::new(Self {
            source,
            pattern,
            flags: value.flags,
            code: value.code.clone(),
            header,
            _metadata: metadata,
            _storage: storage,
        }))
    }

    pub fn flags(&self) -> &'static str {
        match self.flags {
            0 => "",
            1 => "i",
            2 => "m",
            3 => "im",
            _ => unreachable!(),
        }
    }

    pub(super) fn view(&self) -> View<'_> {
        View {
            instructions: &self.code.instructions,
            parts: &self.code.parts,
            names: &self.code.names,
            source: self.pattern.as_bytes().unwrap(),
            start: self.code.start,
            minimum: self.code.minimum,
        }
    }

    pub fn equal(&self, ctx: &mut CallContext, other: &Self) -> Result<bool> {
        Ok(self.flags == other.flags
            && crate::json::bytes_equal(
                ctx,
                self.source.require_bytes()?,
                other.source.require_bytes()?,
            )?)
    }

    pub fn matches(&self, ctx: &mut CallContext, text: &Value) -> Result<bool> {
        if !matches!(text.0, Kind::Bytes(_)) {
            return Ok(false);
        }
        let bytes = text.require_bytes()?;
        super::text_limit(ctx, bytes)?;
        let mut search = Search::new(ctx, self.view(), false)?;
        Ok(search.find(ctx, self.view(), bytes, 0)?.is_some())
    }

    pub fn render(
        &self,
        ctx: &mut CallContext,
        mut emit: impl FnMut(&mut CallContext, &[u8]) -> Result<()>,
    ) -> Result<()> {
        emit(ctx, b"/")?;
        let mut slashes = 0usize;
        let hex = b"0123456789abcdef";
        for &byte in self.source.require_bytes()? {
            ctx.charge(1)?;
            let one = [byte];
            match byte {
                b'/' if slashes % 2 == 0 => emit(ctx, b"\\/")?,
                7 => emit(ctx, b"\\a")?,
                9 => emit(ctx, b"\\t")?,
                10 => emit(ctx, b"\\n")?,
                11 => emit(ctx, b"\\v")?,
                12 => emit(ctx, b"\\f")?,
                13 => emit(ctx, b"\\r")?,
                0..=31 | 127 => {
                    let bytes = [
                        b'\\',
                        b'x',
                        b'{',
                        hex[usize::from(byte / 16)],
                        hex[usize::from(byte % 16)],
                        b'}',
                    ];
                    if byte < 16 {
                        emit(ctx, b"\\x{")?;
                        emit(ctx, &bytes[4..])?;
                    } else {
                        emit(ctx, &bytes)?;
                    }
                }
                _ => emit(ctx, &one)?,
            }
            slashes = if byte == b'\\' { slashes + 1 } else { 0 };
        }
        emit(ctx, b"/")?;
        emit(ctx, self.flags().as_bytes())
    }

    pub fn text(&self, ctx: &mut CallContext) -> Result<Value> {
        let mut size = 0;
        self.render(ctx, |ctx, piece| {
            size += piece.len();
            ctx.check_memory(crate::value::Bytes::header_bytes() + size)
        })?;
        let mut bytes = Buffer::with_capacity(ctx, size)?;
        self.render(ctx, |ctx, piece| bytes.extend(ctx, piece))?;
        Value::from_bytes(ctx, bytes)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Constructor {
    New,
    Union,
    Escape,
    Quote,
    LastMatch,
}

impl Constructor {
    pub fn member(self) -> &'static str {
        match self {
            Self::New => "new",
            Self::Union => "union",
            Self::Escape => "escape",
            Self::Quote => "quote",
            Self::LastMatch => "last_match",
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Self::New => "Regexp.new",
            Self::Union => "Regexp.union",
            Self::Escape => "Regexp.escape",
            Self::Quote => "Regexp.quote",
            Self::LastMatch => "Regexp.last_match",
        }
    }
    pub fn call(
        self,
        ctx: &mut CallContext,
        args: &[Value],
        keywords: &[(Value, Value)],
        block: bool,
    ) -> Result<Value> {
        if !keywords.is_empty() || block {
            return Err(Error::new(
                ErrorKind::Argument,
                "Regexp methods do not accept keywords or blocks",
            ));
        }
        if self == Self::LastMatch {
            ops::arity(args, 0)?;
            return Ok(Value::nil());
        }
        if self != Self::Union {
            ops::arity(args, 1)?;
        }
        for arg in args {
            ctx.charge(1)?;
            if !matches!(arg.0, Kind::Bytes(_)) {
                return Err(Error::new(
                    ErrorKind::Type,
                    "Regexp methods expect string arguments",
                ));
            }
        }
        if self == Self::New {
            return Regex::compile(ctx, args[0].clone(), 0);
        }
        if self == Self::Union && args.is_empty() {
            return Regex::compile(ctx, Value::bytes(b"[^\\s\\S]"), 0);
        }
        let limit = if self == Self::Union {
            super::MAX_PATTERN
        } else {
            usize::MAX - crate::value::Bytes::header_bytes()
        };
        let mut size = args.len().saturating_sub(1);
        if size > limit {
            return ctx.guard(ErrorKind::Memory, "Regexp union pattern exceeds 16 KiB");
        }
        for arg in args {
            for &byte in arg.require_bytes()? {
                ctx.charge(1)?;
                let width = 1 + usize::from(meta(byte));
                if width > limit - size {
                    return ctx.guard(ErrorKind::Memory, "Regexp escaped pattern exceeds limit");
                }
                size += width;
            }
        }
        let text = if args.len() == 1 && size == args[0].require_bytes()?.len() {
            args[0].clone()
        } else {
            let mut output = Buffer::with_capacity(ctx, size)?;
            for (index, arg) in args.iter().enumerate() {
                if index != 0 {
                    output.push(ctx, b'|')?;
                }
                for &byte in arg.require_bytes()? {
                    ctx.charge(1)?;
                    if meta(byte) {
                        output.push(ctx, b'\\')?;
                    }
                    output.push(ctx, byte)?;
                }
            }
            Value::from_bytes(ctx, output)?
        };
        if self == Self::Union {
            Regex::compile(ctx, text, 0)
        } else {
            Ok(text)
        }
    }
}

fn meta(byte: u8) -> bool {
    b"\\.+*?()|[]{}^$".contains(&byte)
}

pub(crate) fn member(
    ctx: &mut CallContext,
    name: &str,
    receiver: &Value,
    args: &[Value],
    keywords: bool,
    block: bool,
) -> Result<Option<Value>> {
    let Kind::Regex(regex) = &receiver.0 else {
        return Ok(None);
    };
    let result = match name {
        "to_s" => return Err(Error::new(ErrorKind::Name, "unknown regex method to_s")),
        "source" | "flags" => {
            ops::arity(args, 0)?;
            if name == "source" {
                regex.source.clone()
            } else {
                ctx.bytes(regex.flags().as_bytes())?
            }
        }
        "match" => {
            ops::arity(args, 1)?;
            if keywords {
                return Err(Error::new(
                    ErrorKind::Argument,
                    "regex.match does not accept keywords",
                ));
            }
            if !matches!(args[0].0, Kind::Bytes(_)) {
                return Err(Error::new(ErrorKind::Type, "regex.match expects a string"));
            }
            super::operations::first(ctx, regex, &args[0], 0)?
        }
        "match?" => {
            ops::arity(args, 1)?;
            if keywords {
                return Err(Error::new(
                    ErrorKind::Argument,
                    "regex.match? does not accept keywords",
                ));
            }
            if !matches!(args[0].0, Kind::Bytes(_)) {
                return Err(Error::new(ErrorKind::Type, "regex.match? expects a string"));
            }
            Value::boolean(regex.matches(ctx, &args[0])?)
        }
        "inspect" => {
            ops::arity(args, 0)?;
            if keywords || block {
                return Err(Error::new(
                    ErrorKind::Argument,
                    "inspect does not accept keywords or blocks",
                ));
            }
            regex.text(ctx)?
        }
        _ => return Ok(None),
    };
    Ok(Some(result))
}

pub(crate) fn binary(ctx: &mut CallContext, op: &str, a: &Value, b: &Value) -> Result<Value> {
    let (regex, text) = match (&a.0, &b.0) {
        (Kind::Regex(regex), Kind::Bytes(_)) => (regex, b),
        (Kind::Bytes(_), Kind::Regex(regex)) => (regex, a),
        _ => {
            return Err(Error::new(
                ErrorKind::Type,
                "match operator expects a string and a regex",
            ));
        }
    };
    let bytes = text.require_bytes()?;
    super::text_limit(ctx, bytes)?;
    let mut search = Search::new(ctx, regex.view(), false)?;
    let found = search.find(ctx, regex.view(), bytes, 0)?;
    if op == "!~" {
        return Ok(Value::boolean(found.is_none()));
    }
    match found {
        Some(found) => Ok(Value::int(
            ops::runes(ctx, &bytes[..found.data[0]])?.0 as i64,
        )),
        None => Ok(Value::nil()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CallOptions;

    #[test]
    fn imports_share_code_without_retaining_another_calls_charges() {
        for flags in [0, 3] {
            let mut first = CallContext::new(CallOptions::default());
            let original =
                Regex::compile(&mut first, Value::bytes(b"(?<x>a+)(b)?"), flags).unwrap();
            let mut second = CallContext::new(CallOptions::default());
            let imported = second.import(&original).unwrap();
            let (Kind::Regex(a), Kind::Regex(b)) = (&original.0, &imported.0) else {
                unreachable!()
            };
            assert!(Arc::ptr_eq(&a.code, &b.code));
            assert_eq!(
                first.stats().retained_memory_bytes,
                second.stats().retained_memory_bytes
            );
            drop(original);
            assert_eq!(first.stats().retained_memory_bytes, 0);
            assert!(second.stats().retained_memory_bytes > 0);
            let Kind::Regex(regex) = &imported.0 else {
                unreachable!()
            };
            assert!(regex.matches(&mut second, &Value::bytes(b"aab")).unwrap());
            drop(imported);
            assert_eq!(second.stats().retained_memory_bytes, 0);
        }
    }

    #[test]
    fn compiled_values_reuse_storage_and_release_search_scratch() {
        let mut ctx = CallContext::new(CallOptions::default());
        let value = Regex::compile(&mut ctx, Value::bytes(b"(?<x>a+)(b)?"), 0).unwrap();
        let baseline = ctx.stats().retained_memory_bytes;
        let Kind::Regex(regex) = &value.0 else {
            unreachable!()
        };
        for _ in 0..100 {
            assert!(regex.matches(&mut ctx, &Value::bytes(b"aaab")).unwrap());
            assert_eq!(ctx.stats().retained_memory_bytes, baseline);
        }
        drop(value);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
}
