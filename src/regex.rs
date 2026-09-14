use crate::{
    CallContext, Error, ErrorKind, Result, Value,
    budget::Buffer,
    ops, scan,
    value::{Bytes, Kind},
};
use program::{Program, View};
use search::{ABSENT, Search};

pub(crate) mod matches;
pub(crate) mod operations;
mod parse;
mod program;
mod search;
pub(crate) mod substitute;
mod unicode;
pub(crate) mod value;

const MAX_PATTERN: usize = 16 << 10;
const MAX_TEXT: usize = 1 << 20;
const MAX_INSTRUCTIONS: usize = 100_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Assertion {
    BeginText,
    EndText,
    BeginLine,
    EndLine,
    Word,
    NotWord,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Set {
    Range(u32, u32),
    Property(usize),
    Digit,
    PerlSpace,
    Word,
    Alnum,
    Alpha,
    Blank,
    Control,
    Punct,
    Space,
    Hex,
}

impl Set {
    fn contains(self, rune: u32) -> bool {
        let alpha = matches!(rune, 65..=90 | 97..=122);
        let digit = matches!(rune, 48..=57);
        match self {
            Self::Range(low, high) => rune >= low && rune <= high,
            Self::Property(group) => unicode::contains(group, rune),
            Self::Digit => digit,
            Self::PerlSpace => matches!(rune, 9 | 10 | 12 | 13 | 32),
            Self::Space => matches!(rune, 9..=13 | 32),
            Self::Word => alpha || digit || rune == 95,
            Self::Alnum => alpha || digit,
            Self::Alpha => alpha,
            Self::Blank => matches!(rune, 9 | 32),
            Self::Control => matches!(rune, 0..=31 | 127),
            Self::Punct => matches!(rune, 33..=47 | 58..=64 | 91..=96 | 123..=126),
            Self::Hex => digit || matches!(rune, 65..=70 | 97..=102),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct Part {
    set: Set,
    negated: bool,
}

#[derive(Clone, Copy, Debug)]
struct Class {
    start: usize,
    count: usize,
    negated: bool,
    fold: bool,
}

impl Class {
    fn matches(self, ctx: &mut CallContext, parts: &[Part], rune: u32) -> Result<bool> {
        for part in &parts[self.start..self.start + self.count] {
            ctx.charge(1)?;
            if unicode::folded(rune, self.fold, |point| part.set.contains(point)) != part.negated {
                return Ok(!self.negated);
            }
        }
        Ok(self.negated)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Utility {
    Match,
    Replace,
    ReplaceAll,
}

impl Utility {
    pub fn name(self) -> &'static str {
        match self {
            Self::Match => "Regex.match",
            Self::Replace => "Regex.replace",
            Self::ReplaceAll => "Regex.replace_all",
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
                format!("{} does not accept keywords or blocks", self.name()),
            ));
        }
        ops::arity(args, if self == Self::Match { 2 } else { 3 })?;
        if args.iter().any(|arg| !matches!(arg.0, Kind::Bytes(_))) {
            return Err(Error::new(
                ErrorKind::Type,
                "Regex utilities expect string arguments",
            ));
        }
        let (pattern, text) = if self == Self::Match {
            (&args[0], &args[1])
        } else {
            (&args[1], &args[0])
        };
        text_limit(ctx, text.require_bytes()?)?;
        if self != Self::Match {
            text_limit(ctx, args[2].require_bytes()?)?;
        }
        let program = Program::compile(ctx, pattern.clone())?;
        let mut search = Search::new(ctx, program.view(), self != Self::Match)?;
        if self == Self::Match {
            return match search.find(ctx, program.view(), text.require_bytes()?, 0)? {
                Some(indices) => window(ctx, text, indices.data[0], indices.data[1]),
                None => Ok(Value::nil()),
            };
        }
        replace(
            ctx,
            program.view(),
            &mut search,
            text,
            args[2].require_bytes()?,
            self == Self::ReplaceAll,
        )
    }
}

fn text_limit(ctx: &mut CallContext, bytes: &[u8]) -> Result<()> {
    if bytes.len() > MAX_TEXT {
        ctx.guard(
            ErrorKind::Memory,
            "regex text, replacement or output exceeds 1 MiB",
        )
    } else {
        ctx.checkpoint()
    }
}

fn window(ctx: &mut CallContext, subject: &Value, start: usize, end: usize) -> Result<Value> {
    let bytes = subject.require_bytes()?;
    if start == 0 && end == bytes.len() {
        Ok(subject.clone())
    } else {
        ctx.bytes(&bytes[start..end])
    }
}

pub(crate) fn member(
    ctx: &mut CallContext,
    name: &str,
    receiver: &Value,
    args: &[Value],
    keywords: bool,
) -> Result<Option<Value>> {
    if name != "match?" || !matches!(receiver.0, Kind::Bytes(_)) {
        return Ok(None);
    }
    if keywords || !(1..=2).contains(&args.len()) {
        return Err(Error::new(
            ErrorKind::Argument,
            "string.match? expects a pattern and optional offset without keywords",
        ));
    }
    if !matches!(args[0].0, Kind::Bytes(_) | Kind::Regex(_)) {
        return Err(Error::new(
            ErrorKind::Type,
            "string.match? expects a string pattern",
        ));
    }
    let offset = if args.len() == 2 {
        crate::sequence::integer(&args[1])?
    } else {
        0
    };
    if offset < 0 {
        return Err(Error::new(
            ErrorKind::Argument,
            "string.match? offset must be non-negative",
        ));
    }
    let text = receiver.require_bytes()?;
    text_limit(ctx, text)?;
    let compiled = if matches!(args[0].0, Kind::Regex(_)) {
        None
    } else {
        Some(Program::compile(ctx, args[0].clone())?)
    };
    let program = match &args[0].0 {
        Kind::Regex(regex) => regex.view(),
        _ => compiled.as_ref().unwrap().view(),
    };
    let mut position = 0;
    let mut remaining = offset as u64;
    while remaining > 0 && position < text.len() {
        ctx.charge(1)?;
        position += scan::rune(&text[position..]).1;
        remaining -= 1;
    }
    if remaining > 0 {
        return Ok(Some(Value::boolean(false)));
    }
    let mut search = Search::new(ctx, program, false)?;
    Ok(Some(Value::boolean(
        search.find(ctx, program, text, position)?.is_some(),
    )))
}

fn replacement_name<'a>(
    ctx: &mut CallContext,
    template: &'a [u8],
) -> Result<Option<(&'a [u8], usize)>> {
    let braced = template.first() == Some(&b'{');
    let start = usize::from(braced);
    let mut end = start;
    let letters = unicode::group(b"L").unwrap().0;
    let digits = unicode::group(b"Nd").unwrap().0;
    while end < template.len() {
        ctx.charge(1)?;
        let (rune, width, _) = scan::rune(&template[end..]);
        if rune != '_'
            && !unicode::contains(letters, rune as u32)
            && !unicode::contains(digits, rune as u32)
        {
            break;
        }
        end += width;
    }
    if end == start || (braced && template.get(end) != Some(&b'}')) {
        return Ok(None);
    }
    Ok(Some((&template[start..end], end + usize::from(braced))))
}

fn expand(
    ctx: &mut CallContext,
    program: View<'_>,
    mut template: &[u8],
    text: &[u8],
    indices: &[usize],
    mut emit: impl FnMut(&mut CallContext, &[u8]) -> Result<()>,
) -> Result<()> {
    let source = program.source;
    while !template.is_empty() {
        let mut offset = 0;
        while offset < template.len() {
            let chunk = &template[offset..offset + (template.len() - offset).min(4096)];
            if let Some(found) = chunk.iter().position(|&byte| byte == b'$') {
                ctx.work_bytes(found + 1)?;
                offset += found;
                break;
            }
            ctx.work_bytes(chunk.len())?;
            offset += chunk.len();
        }
        emit(ctx, &template[..offset])?;
        template = &template[offset..];
        if template.is_empty() {
            break;
        }
        template = &template[1..];
        if template.first() == Some(&b'$') {
            template = &template[1..];
            emit(ctx, b"$")?;
            continue;
        }
        let Some((name, consumed)) = replacement_name(ctx, template)? else {
            emit(ctx, b"$")?;
            continue;
        };
        template = &template[consumed..];
        let numeric = name.len() <= 9
            && (name.len() == 1 || name[0] != b'0')
            && name.iter().all(u8::is_ascii_digit);
        let mut slot = None;
        if numeric {
            let index = name
                .iter()
                .fold(0usize, |n, &byte| n * 10 + usize::from(byte - b'0'));
            if index < indices.len() / 2 && indices[2 * index] != ABSENT {
                slot = Some(index);
            }
        } else {
            for (index, &(start, end)) in program.names.iter().enumerate() {
                ctx.charge(1)?;
                if end - start == name.len() {
                    ctx.work_bytes(name.len())?;
                    if &source[start..end] == name && indices[2 * index] != ABSENT {
                        slot = Some(index);
                        break;
                    }
                }
            }
        }
        if let Some(slot) = slot {
            emit(ctx, &text[indices[2 * slot]..indices[2 * slot + 1]])?;
        }
    }
    Ok(())
}

fn replacements(
    ctx: &mut CallContext,
    program: View<'_>,
    search: &mut Search,
    text: &[u8],
    replacement: &[u8],
    all: bool,
    mut emit: impl FnMut(&mut CallContext, &[u8]) -> Result<()>,
) -> Result<bool> {
    let mut position = 0;
    let mut copied = 0;
    let mut previous_end = None;
    let mut matched = false;
    while position <= text.len() {
        ctx.charge(1)?;
        let Some(indices) = search.find(ctx, program, text, position)? else {
            break;
        };
        let start = indices.data[0];
        let end = indices.data[1];
        if start != end || previous_end != Some(end) {
            emit(ctx, &text[copied..start])?;
            expand(ctx, program, replacement, text, &indices.data, &mut emit)?;
            copied = end;
            matched = true;
            if !all {
                break;
            }
        }
        previous_end = Some(end);
        position = if start == end {
            if end == text.len() {
                break;
            }
            end + scan::rune(&text[end..]).1
        } else {
            end
        };
    }
    if matched {
        emit(ctx, &text[copied..])?;
    }
    Ok(matched)
}

fn replace(
    ctx: &mut CallContext,
    program: View<'_>,
    search: &mut Search,
    text: &Value,
    replacement: &[u8],
    all: bool,
) -> Result<Value> {
    let bytes = text.require_bytes()?;
    let mut size = 0usize;
    let matched = replacements(
        ctx,
        program,
        search,
        bytes,
        replacement,
        all,
        |ctx, piece| {
            if piece.len() > MAX_TEXT - size {
                return ctx.guard(ErrorKind::Memory, "regex output exceeds 1 MiB");
            }
            size += piece.len();
            ctx.work_bytes(piece.len())?;
            ctx.check_memory(Bytes::header_bytes() + size)
        },
    )?;
    if !matched {
        return Ok(text.clone());
    }
    let mut output = Buffer::with_capacity(ctx, size)?;
    replacements(
        ctx,
        program,
        search,
        bytes,
        replacement,
        all,
        |ctx, piece| output.extend(ctx, piece),
    )?;
    Value::from_bytes(ctx, output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CallOptions;

    #[test]
    fn compiled_program_search_and_capture_charges_have_independent_lifetimes() {
        let mut ctx = CallContext::new(CallOptions::default());
        let program = Program::compile(&mut ctx, Value::bytes(b"(a+)(b?)")).unwrap();
        assert!(ctx.stats().retained_memory_bytes > 0);
        let mut search = Search::new(&mut ctx, program.view(), true).unwrap();
        let captures = search
            .find(&mut ctx, program.view(), b"zaab", 0)
            .unwrap()
            .unwrap();
        assert_eq!(captures.data, [1, 4, 1, 3, 3, 4]);
        drop(search);
        drop(program);
        assert_eq!(
            ctx.stats().retained_memory_bytes,
            captures.data.capacity() * size_of::<usize>()
        );
        drop(captures);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }

    #[test]
    fn instruction_expansion_is_rejected_before_program_allocation() {
        let mut ctx = CallContext::new(CallOptions::default());
        let source = format!("(?:{}){{1000}}", "a".repeat(101));
        let error = Program::compile(&mut ctx, Value::bytes(source.into_bytes())).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Memory);
        assert!(ctx.stats().peak_memory_bytes < 65536);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
        assert_eq!(error.class(), Some(crate::ErrorClass::Limit));
        ctx.charge(1).unwrap();
    }

    #[test]
    fn replacement_expansion_is_projected_before_allocating_output() {
        let mut ctx = CallContext::new(CallOptions::default());
        ctx.options.limits.steps = None;
        let text = ctx.bytes(&vec![b'x'; 65536]).unwrap();
        let program = Program::compile(&mut ctx, Value::bytes(b"(x+)")).unwrap();
        let mut search = Search::new(&mut ctx, program.view(), true).unwrap();
        drop(
            search
                .find(&mut ctx, program.view(), text.as_bytes().unwrap(), 0)
                .unwrap(),
        );
        let baseline = ctx.stats();
        let error = replace(
            &mut ctx,
            program.view(),
            &mut search,
            &text,
            b"$1$1$1$1$1$1$1$1$1$1$1$1$1$1$1$1$1",
            true,
        )
        .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Memory);
        assert_eq!(ctx.stats().peak_memory_bytes, baseline.peak_memory_bytes);
        assert_eq!(error.class(), Some(crate::ErrorClass::Limit));
        ctx.charge(1).unwrap();
    }

    #[test]
    fn unmatched_replacement_needs_no_copy_of_the_subject() {
        let mut ctx = CallContext::new(CallOptions::default());
        let text = ctx.bytes(&vec![b'x'; 65536]).unwrap();
        let input_memory = ctx.stats().retained_memory_bytes;
        let program = Program::compile(&mut ctx, Value::bytes(b"^z")).unwrap();
        let mut search = Search::new(&mut ctx, program.view(), true).unwrap();
        assert!(
            search
                .find(&mut ctx, program.view(), text.as_bytes().unwrap(), 0)
                .unwrap()
                .is_none()
        );
        let baseline = ctx.stats();
        ctx.options.limits.memory_bytes = Some(baseline.peak_memory_bytes + 1024);
        let output = replace(
            &mut ctx,
            program.view(),
            &mut search,
            &text,
            b"replacement",
            true,
        )
        .unwrap();
        assert_eq!(
            output.as_bytes().unwrap().as_ptr(),
            text.as_bytes().unwrap().as_ptr()
        );
        drop(search);
        drop(program);
        assert_eq!(ctx.stats().retained_memory_bytes, input_memory);
    }

    #[test]
    fn capture_state_storage_cannot_bypass_the_memory_budget() {
        let mut ctx = CallContext::new(CallOptions::default());
        let program = Program::compile(&mut ctx, Value::bytes(b"(a?){1000}")).unwrap();
        let baseline = ctx.stats().retained_memory_bytes;
        ctx.options.limits.memory_bytes = Some(baseline + 32768);
        let mut search = Search::new(&mut ctx, program.view(), true).unwrap();
        let error = search.find(&mut ctx, program.view(), b"a", 0).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Memory);
        drop(search);
        assert_eq!(ctx.stats().retained_memory_bytes, baseline);
        assert_eq!(ctx.charge(0).unwrap_err().kind, ErrorKind::Memory);
    }
}
