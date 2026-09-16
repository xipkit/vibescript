use super::{
    operations::{self, Cursor},
    search::{ABSENT, Search},
    value::Regex,
};
use crate::{
    CallContext, Error, ErrorKind, Result, Value, budget::Buffer, iteration::Progress, json, ops,
    scan, value::Kind,
};
use std::sync::Arc;

pub(crate) fn method(name: &str) -> bool {
    matches!(name, "sub" | "sub!" | "gsub" | "gsub!")
}

fn argument(message: &str) -> Error {
    Error::new(ErrorKind::Argument, message)
}

struct Spec {
    pattern: Value,
    regex: bool,
    all: bool,
    bang: bool,
}

fn arguments(
    ctx: &mut CallContext,
    name: &str,
    receiver: &Value,
    args: &[Value],
    keywords: &[(Value, Value)],
    block: bool,
) -> Result<Spec> {
    let mut regex = false;
    if !keywords.is_empty() {
        if keywords.len() != 1 || keywords[0].0.as_bytes() != Some(b"regex") {
            return Err(argument(
                "string substitution supports only the regex keyword",
            ));
        }
        let Kind::Bool(enabled) = keywords[0].1.0 else {
            return Err(argument("regex keyword must be bool"));
        };
        regex = enabled;
    }
    let Some(pattern) = args.first() else {
        return Err(argument("string substitution expects a pattern"));
    };
    match pattern.0 {
        Kind::Bytes(_) => (),
        Kind::Regex(_) => {
            if !keywords.is_empty() {
                return Err(argument("regex values do not accept the regex keyword"));
            }
            regex = true;
        }
        _ => {
            return Err(Error::new(
                ErrorKind::Type,
                "pattern must be a string or regex",
            ));
        }
    }
    ops::arity(args, if block { 1 } else { 2 })?;
    if !block && !matches!(args[1].0, Kind::Bytes(_)) {
        return Err(Error::new(ErrorKind::Type, "replacement must be a string"));
    }
    if regex {
        if matches!(&pattern.0, Kind::Bytes(bytes) if bytes.data.len() > super::MAX_PATTERN) {
            return ctx.guard(ErrorKind::Memory, "regex pattern exceeds 16 KiB");
        }
        super::text_limit(ctx, receiver.require_bytes()?)?;
        if !block {
            super::text_limit(ctx, args[1].require_bytes()?)?;
        }
    }
    Ok(Spec {
        pattern: pattern.clone(),
        regex,
        all: name.starts_with('g'),
        bang: name.ends_with('!'),
    })
}

struct Location {
    whole: [usize; 2],
    captures: Option<Buffer<usize>>,
}

impl Location {
    fn indices(&self) -> &[usize] {
        self.captures
            .as_ref()
            .map_or(&self.whole, |v| v.data.as_slice())
    }
}

struct Matcher {
    pattern: Value,
    regex: Option<Arc<Regex>>,
    search: Option<Search>,
    cursor: Cursor,
    position: usize,
    done: bool,
    named: bool,
}

impl Matcher {
    fn new(ctx: &mut CallContext, spec: &Spec, captures: bool) -> Result<Self> {
        let regex = if spec.regex {
            Some(operations::pattern(ctx, &spec.pattern)?)
        } else {
            None
        };
        let search = regex
            .as_ref()
            .map(|re| Search::new(ctx, re.view(), captures))
            .transpose()?;
        let mut named = false;
        if let Some(regex) = &regex {
            for &(start, end) in regex.view().names {
                ctx.charge(1)?;
                named |= start != end;
            }
        }
        Ok(Self {
            pattern: spec.pattern.clone(),
            regex,
            search,
            cursor: Cursor::default(),
            position: 0,
            done: false,
            named,
        })
    }

    fn reset(&mut self) {
        self.cursor = Cursor::default();
        self.position = 0;
        self.done = false;
    }

    fn next(&mut self, ctx: &mut CallContext, text: &[u8]) -> Result<Option<Location>> {
        ctx.checkpoint()?;
        if let Some(regex) = &self.regex {
            return Ok(self
                .cursor
                .next(ctx, self.search.as_mut().unwrap(), regex.view(), text)?
                .map(|indices| Location {
                    whole: [indices.data[0], indices.data[1]],
                    captures: Some(indices),
                }));
        }
        if self.done {
            return Ok(None);
        }
        let pattern = self.pattern.require_bytes()?;
        let Some(found) = ops::find(ctx, &text[self.position..], pattern, false)? else {
            self.done = true;
            return Ok(None);
        };
        let start = self.position + found;
        let end = start + pattern.len();
        self.position = end;
        if pattern.is_empty() {
            if end == text.len() {
                self.done = true;
            } else {
                ctx.charge(1)?;
                self.position += scan::rune(&text[end..]).1;
            }
        }
        Ok(Some(Location {
            whole: [start, end],
            captures: None,
        }))
    }

    fn expand(
        &self,
        ctx: &mut CallContext,
        template: &[u8],
        text: &[u8],
        loc: &[usize],
        emit: &mut impl FnMut(&mut CallContext, &[u8]) -> Result<()>,
    ) -> Result<()> {
        let Some(regex) = &self.regex else {
            return emit(ctx, template);
        };
        let view = regex.view();
        let mut position = 0;
        while position < template.len() {
            let start = position;
            while position < template.len() && template[position] != b'\\' {
                if (position - start) % 4096 == 0 {
                    ctx.checkpoint()?;
                }
                position += 1;
            }
            ctx.work_bytes(position - start)?;
            emit(ctx, &template[start..position])?;
            if position == template.len() {
                break;
            }
            ctx.charge(1)?;
            let Some(&next) = template.get(position + 1) else {
                emit(ctx, b"\\")?;
                break;
            };
            position += 2;
            match next {
                b'0' | b'&' => emit_group(ctx, emit, text, loc, 0)?,
                b'1'..=b'9' if !self.named => {
                    emit_group(ctx, emit, text, loc, usize::from(next - b'0'))?
                }
                b'1'..=b'9' => (),
                b'\x60' => emit(ctx, &text[..loc[0]])?,
                b'\'' => emit(ctx, &text[loc[1]..])?,
                b'\\' => emit(ctx, b"\\")?,
                b'+' => {
                    for group in (1..loc.len() / 2).rev() {
                        ctx.charge(1)?;
                        if loc[2 * group] != ABSENT
                            && (!self.named || view.names[group].0 != view.names[group].1)
                        {
                            emit_group(ctx, emit, text, loc, group)?;
                            break;
                        }
                    }
                }
                b'k' if template.get(position) == Some(&b'<') => {
                    position += 1;
                    let start = position;
                    while position < template.len() && template[position] != b'>' {
                        ctx.charge(1)?;
                        position += 1;
                    }
                    if position == template.len() {
                        return Err(argument("invalid group name reference format"));
                    }
                    let name = &template[start..position];
                    position += 1;
                    let mut defined = false;
                    let mut group = None;
                    for (index, &(start, end)) in view.names.iter().enumerate().skip(1) {
                        ctx.charge(1)?;
                        if start == end || !json::bytes_equal(ctx, &view.source[start..end], name)?
                        {
                            continue;
                        }
                        defined = true;
                        if loc[2 * index] != ABSENT {
                            group = Some(index);
                        }
                    }
                    if !defined {
                        return Err(argument("undefined group name reference"));
                    }
                    if let Some(group) = group {
                        emit_group(ctx, emit, text, loc, group)?;
                    }
                }
                _ => emit(ctx, &template[position - 2..position])?,
            }
        }
        Ok(())
    }
}

fn emit_group(
    ctx: &mut CallContext,
    emit: &mut impl FnMut(&mut CallContext, &[u8]) -> Result<()>,
    text: &[u8],
    loc: &[usize],
    group: usize,
) -> Result<()> {
    if 2 * group + 1 < loc.len() && loc[2 * group] != ABSENT {
        emit(ctx, &text[loc[2 * group]..loc[2 * group + 1]])?;
    }
    Ok(())
}

fn pass(
    ctx: &mut CallContext,
    matcher: &mut Matcher,
    spec: &Spec,
    text: &[u8],
    template: &[u8],
    first: Location,
    mut emit: impl FnMut(&mut CallContext, &[u8]) -> Result<()>,
) -> Result<()> {
    let mut location = first;
    let mut appended = 0;
    loop {
        ctx.charge(1)?;
        emit(ctx, &text[appended..location.whole[0]])?;
        matcher.expand(ctx, template, text, location.indices(), &mut emit)?;
        appended = location.whole[1];
        drop(location);
        if !spec.all {
            break;
        }
        let Some(next) = matcher.next(ctx, text)? else {
            break;
        };
        location = next;
    }
    emit(ctx, &text[appended..])
}

pub(crate) fn member(
    ctx: &mut CallContext,
    name: &str,
    receiver: &Value,
    args: &[Value],
    keywords: &[(Value, Value)],
) -> Result<Option<Value>> {
    if !method(name) || !matches!(receiver.0, Kind::Bytes(_)) {
        return Ok(None);
    }
    let spec = arguments(ctx, name, receiver, args, keywords, false)?;
    let text = receiver.require_bytes()?;
    let template = args[1].require_bytes()?;
    let mut matcher = Matcher::new(ctx, &spec, true)?;
    let Some(first) = matcher.next(ctx, text)? else {
        return Ok(Some(if spec.bang {
            Value::nil()
        } else {
            receiver.clone()
        }));
    };
    if !spec.regex {
        if json::bytes_equal(ctx, spec.pattern.require_bytes()?, template)? {
            return Ok(Some(receiver.clone()));
        }
        super::text_limit(ctx, template)?;
    }
    let mut length = 0;
    let mut unchanged = true;
    pass(
        ctx,
        &mut matcher,
        &spec,
        text,
        template,
        first,
        |ctx, bytes| {
            if bytes.len() > super::MAX_TEXT - length {
                return ctx.guard(ErrorKind::OutputLimit, "substitution output exceeds 1 MiB");
            }
            let end = length + bytes.len();
            unchanged &= end <= text.len()
                && json::bytes_equal(
                    ctx,
                    &text[length.min(text.len())..end.min(text.len())],
                    bytes,
                )?;
            length = end;
            ctx.work_bytes(bytes.len())
        },
    )?;
    if unchanged && length == text.len() {
        return Ok(Some(receiver.clone()));
    }
    let mut output = Buffer::with_capacity(ctx, length)?;
    matcher.reset();
    let first = matcher.next(ctx, text)?.unwrap();
    pass(
        ctx,
        &mut matcher,
        &spec,
        text,
        template,
        first,
        |ctx, bytes| output.extend(ctx, bytes),
    )?;
    Value::from_bytes(ctx, output).map(Some)
}

pub(crate) struct Driver {
    matcher: Matcher,
    spec: Spec,
    receiver: Value,
    output: Buffer<u8>,
    appended: usize,
    pending: [usize; 2],
    matched: bool,
    pub waiting: bool,
}

impl Driver {
    pub fn new(
        ctx: &mut CallContext,
        name: &str,
        receiver: &Value,
        args: &[Value],
        keywords: &[(Value, Value)],
        block: bool,
    ) -> Result<Option<Self>> {
        if !block || !method(name) || !matches!(receiver.0, Kind::Bytes(_)) {
            return Ok(None);
        }
        let spec = arguments(ctx, name, receiver, args, keywords, true)?;
        let matcher = Matcher::new(ctx, &spec, false)?;
        Ok(Some(Self {
            matcher,
            spec,
            receiver: receiver.clone(),
            output: Buffer::empty(),
            appended: 0,
            pending: [0; 2],
            matched: false,
            waiting: false,
        }))
    }

    fn append(&mut self, ctx: &mut CallContext, bytes: &[u8]) -> Result<()> {
        if bytes.len() > super::MAX_TEXT - self.output.data.len() {
            return ctx.guard(ErrorKind::OutputLimit, "substitution output exceeds 1 MiB");
        }
        let required = self.output.data.len() + bytes.len();
        if required > self.output.data.capacity() {
            let capacity = required
                .max(self.output.data.capacity().saturating_mul(2))
                .min(super::MAX_TEXT);
            self.output.ensure(ctx, capacity)?;
        }
        self.output.extend(ctx, bytes)
    }

    fn finish(&mut self, ctx: &mut CallContext) -> Result<Progress> {
        let receiver = self.receiver.clone();
        self.append(ctx, &receiver.require_bytes()?[self.appended..])?;
        let output = std::mem::replace(&mut self.output, Buffer::empty());
        if !self.matched && self.spec.bang {
            return Ok(Progress::Done(Value::nil()));
        }
        if json::bytes_equal(ctx, &output.data, receiver.require_bytes()?)? {
            return Ok(Progress::Done(receiver));
        }
        Value::from_bytes(ctx, output).map(Progress::Done)
    }

    pub fn advance(&mut self, ctx: &mut CallContext, returned: Option<Value>) -> Result<Progress> {
        ctx.checkpoint()?;
        let receiver = self.receiver.clone();
        let text = receiver.require_bytes()?;
        if self.waiting {
            let returned = returned.unwrap();
            let replacement = crate::text::bounded::render(ctx, &returned, super::MAX_TEXT)?;
            drop(returned);
            if !self.spec.all && self.spec.regex {
                self.append(ctx, &text[..self.pending[0]])?;
            }
            self.append(ctx, replacement.require_bytes()?)?;
            drop(replacement);
            self.appended = self.pending[1];
            self.waiting = false;
            if !self.spec.all {
                return self.finish(ctx);
            }
        }
        let Some(location) = self.matcher.next(ctx, text)? else {
            return self.finish(ctx);
        };
        self.pending = location.whole;
        self.matched = true;
        if self.spec.all || !self.spec.regex {
            self.append(ctx, &text[self.appended..self.pending[0]])?;
        }
        let value = super::window(ctx, &receiver, self.pending[0], self.pending[1])?;
        self.waiting = true;
        Ok(Progress::Yield([value, Value::nil(), Value::nil()], 1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CallOptions;

    #[test]
    fn templates_project_capture_expansion_before_allocating_output() {
        let mut ctx = CallContext::new(CallOptions::default());
        ctx.options.limits.steps = None;
        let subject = ctx.bytes(&vec![b'x'; 65536]).unwrap();
        let pattern = Value::regex(b"(x+)", "").unwrap();
        let output = member(
            &mut ctx,
            "gsub",
            &subject,
            &[pattern.clone(), Value::bytes(b"\\0")],
            &[],
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            output.as_bytes().unwrap().as_ptr(),
            subject.as_bytes().unwrap().as_ptr()
        );
        drop(output);
        let baseline = ctx.stats();
        let error = member(
            &mut ctx,
            "gsub",
            &subject,
            &[pattern, Value::bytes(b"\\1".repeat(17))],
            &[],
        )
        .unwrap_err();
        assert_eq!(error.kind, ErrorKind::OutputLimit);
        assert_eq!(ctx.stats().peak_memory_bytes, baseline.peak_memory_bytes);
        assert_eq!(
            ctx.stats().retained_memory_bytes,
            baseline.retained_memory_bytes
        );
        ctx.charge(1).unwrap();
    }

    #[test]
    fn literal_no_match_reuses_storage_and_ignores_unused_replacement_limits() {
        let mut ctx = CallContext::new(CallOptions::default());
        ctx.options.limits.steps = None;
        let subject = ctx.bytes(&vec![b'x'; (1 << 20) + 1]).unwrap();
        let baseline = ctx.stats();
        ctx.options.limits.memory_bytes = Some(baseline.retained_memory_bytes + 256);
        let replacement = Value::bytes(vec![b'y'; (1 << 20) + 1]);
        let args = [Value::bytes(b"z"), replacement];
        let output = member(&mut ctx, "gsub", &subject, &args, &[])
            .unwrap()
            .unwrap();
        assert_eq!(
            output.as_bytes().unwrap().as_ptr(),
            subject.as_bytes().unwrap().as_ptr()
        );
        assert!(matches!(
            member(&mut ctx, "gsub!", &subject, &args, &[])
                .unwrap()
                .unwrap()
                .0,
            Kind::Nil
        ));
        assert_eq!(
            ctx.stats().retained_memory_bytes,
            baseline.retained_memory_bytes
        );
    }

    #[test]
    fn interrupted_searches_release_scratch_and_latch_exhaustion() {
        for regex in [false, true] {
            let mut ctx = CallContext::new(CallOptions::default());
            let subject = ctx.bytes(&vec![b'a'; 65536]).unwrap();
            let pattern = Value::bytes(if regex {
                b"(a|aa)*z".as_slice()
            } else {
                b"aaaaab".as_slice()
            });
            let keywords = if regex {
                vec![(Value::symbol(b"regex"), Value::boolean(true))]
            } else {
                vec![]
            };
            let baseline = ctx.stats().retained_memory_bytes;
            ctx.options.limits.steps = Some(ctx.stats().steps + 512);
            let error = member(
                &mut ctx,
                "sub",
                &subject,
                &[pattern, Value::bytes(b"x")],
                &keywords,
            )
            .unwrap_err();
            assert_eq!(error.kind, ErrorKind::Steps);
            assert_eq!(ctx.stats().retained_memory_bytes, baseline);
            assert_eq!(ctx.charge(0).unwrap_err().kind, ErrorKind::Steps);
            drop(subject);
            assert_eq!(ctx.stats().retained_memory_bytes, 0);
        }
    }
}
