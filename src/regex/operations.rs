use super::{
    matches,
    program::View,
    search::{ABSENT, Search},
    value::Regex,
};
use crate::{
    CallContext, Error, ErrorKind, Result, Value,
    budget::Buffer,
    iteration::Progress,
    ops, scan,
    value::{Heap, Kind},
};
use std::sync::Arc;

pub(super) fn pattern(ctx: &mut CallContext, value: &Value) -> Result<Arc<Regex>> {
    let value = match &value.0 {
        Kind::Regex(_) => ctx.import(value)?,
        Kind::Bytes(_) => Regex::compile(ctx, value.clone(), 0)?,
        _ => {
            return Err(Error::new(
                ErrorKind::Type,
                "pattern must be a string or regex",
            ));
        }
    };
    let Kind::Regex(regex) = value.0 else {
        unreachable!()
    };
    Ok(regex)
}

fn arguments(
    ctx: &mut CallContext,
    name: &str,
    receiver: &Value,
    args: &[Value],
    keywords: bool,
) -> Result<(Arc<Regex>, i64)> {
    let valid = if name == "match" {
        (1..=2).contains(&args.len())
    } else {
        args.len() == 1
    };
    if !valid || keywords {
        return Err(Error::new(
            ErrorKind::Argument,
            "string matching expects a pattern without keywords",
        ));
    }
    if !matches!(args[0].0, Kind::Bytes(_) | Kind::Regex(_)) {
        return Err(Error::new(
            ErrorKind::Type,
            "pattern must be a string or regex",
        ));
    }
    super::text_limit(ctx, receiver.require_bytes()?)?;
    if let Kind::Bytes(bytes) = &args[0].0
        && bytes.data.len() > super::MAX_PATTERN
    {
        return ctx.fail(ErrorKind::Memory, "regex pattern exceeds 16 KiB");
    }
    let offset = if args.len() == 2 {
        crate::sequence::integer(&args[1])?
    } else {
        0
    };
    Ok((pattern(ctx, &args[0])?, offset))
}

fn offset(ctx: &mut CallContext, text: &[u8], offset: i64) -> Result<Option<usize>> {
    let offset = if offset < 0 {
        let length = ops::runes(ctx, text)?.0;
        let effective = length as i128 + i128::from(offset);
        if effective < 0 {
            return Ok(None);
        }
        effective as u64
    } else {
        offset as u64
    };
    let mut position = 0;
    let mut remaining = offset;
    while remaining != 0 && position < text.len() {
        ctx.charge(1)?;
        position += scan::rune(&text[position..]).1;
        remaining -= 1;
    }
    Ok(Some(position))
}

pub(super) fn first(
    ctx: &mut CallContext,
    regex: &Regex,
    subject: &Value,
    start: i64,
) -> Result<Value> {
    let text = subject.require_bytes()?;
    super::text_limit(ctx, text)?;
    let Some(start) = offset(ctx, text, start)? else {
        return Ok(Value::nil());
    };
    let mut search = Search::new(ctx, regex.view(), true)?;
    match search.find(ctx, regex.view(), text, start)? {
        Some(indices) => matches::data(ctx, regex.view(), subject, &indices.data),
        None => Ok(Value::nil()),
    }
}

pub(crate) fn member(
    ctx: &mut CallContext,
    name: &str,
    receiver: &Value,
    args: &[Value],
    keywords: bool,
) -> Result<Option<Value>> {
    if !matches!(receiver.0, Kind::Bytes(_)) || !matches!(name, "match" | "scan") {
        return Ok(None);
    }
    let (regex, start) = arguments(ctx, name, receiver, args, keywords)?;
    if name == "match" {
        first(ctx, &regex, receiver, start).map(Some)
    } else {
        materialize(ctx, &regex, receiver).map(Some)
    }
}

#[derive(Default)]
pub(super) struct Cursor {
    position: usize,
    previous_end: Option<usize>,
    done: bool,
}

impl Cursor {
    pub(super) fn next(
        &mut self,
        ctx: &mut CallContext,
        search: &mut Search,
        program: View<'_>,
        text: &[u8],
    ) -> Result<Option<Buffer<usize>>> {
        while !self.done {
            ctx.charge(1)?;
            let Some(indices) = search.find(ctx, program, text, self.position)? else {
                self.done = true;
                return Ok(None);
            };
            let start = indices.data[0];
            let end = indices.data[1];
            let accepted = start != end || self.previous_end != Some(end);
            self.previous_end = Some(end);
            if start == end {
                if end == text.len() {
                    self.done = true;
                } else {
                    self.position = end + scan::rune(&text[end..]).1;
                }
            } else {
                self.position = end;
            }
            if accepted {
                return Ok(Some(indices));
            }
        }
        Ok(None)
    }
}

fn element(ctx: &mut CallContext, subject: &Value, indices: &[usize]) -> Result<Value> {
    if indices.len() == 2 {
        return super::window(ctx, subject, indices[0], indices[1]);
    }
    let mut values = Buffer::with_capacity(ctx, indices.len() / 2 - 1)?;
    for pair in indices[2..].chunks_exact(2) {
        ctx.charge(1)?;
        let value = if pair[0] == ABSENT {
            Value::nil()
        } else {
            super::window(ctx, subject, pair[0], pair[1])?
        };
        values.data.push(value);
    }
    Value::from_array(ctx, values)
}

fn element_bytes(text: &[u8], indices: &[usize]) -> usize {
    if indices.len() == 2 {
        return matches::window_bytes(text, indices[0], indices[1]);
    }
    indices[2..].chunks_exact(2).fold(
        Heap::<Value>::header_bytes() + (indices.len() / 2 - 1) * size_of::<Value>(),
        |size, pair| size.saturating_add(matches::window_bytes(text, pair[0], pair[1])),
    )
}

// Preserve the reference's fixed scan guards independently of Rust's actual storage charges.
fn reference_element(indices: &[usize]) -> usize {
    let (indices, initial) = if indices.len() == 2 {
        (indices, 0)
    } else {
        (&indices[2..], 32 + (indices.len() / 2 - 1) * 32)
    };
    indices.chunks_exact(2).fold(initial, |size, pair| {
        if pair[0] == ABSENT {
            size
        } else {
            size.saturating_add(16 + pair[1] - pair[0])
        }
    })
}

fn materialize(ctx: &mut CallContext, regex: &Regex, subject: &Value) -> Result<Value> {
    let program = regex.view();
    let text = subject.require_bytes()?;
    let runes = ops::runes(ctx, text)?.0;
    let maximum = runes.checked_div(program.minimum).unwrap_or(runes + 1);
    if maximum.saturating_mul(program.names.len() * 16 + 24) > 256 << 20 {
        return ctx.fail(ErrorKind::Memory, "string.scan match table exceeds 256 MiB");
    }
    let mut search = Search::new(ctx, program, true)?;
    let mut cursor = Cursor::default();
    let mut count = 0usize;
    let mut projected = Heap::<Value>::header_bytes();
    let mut guard = 64usize;
    while let Some(indices) = cursor.next(ctx, &mut search, program, text)? {
        count += 1;
        guard = guard
            .saturating_add(32)
            .saturating_add(reference_element(&indices.data));
        if guard > super::MAX_TEXT {
            return ctx.fail(ErrorKind::OutputLimit, "string.scan output exceeds 1 MiB");
        }
        projected = projected
            .saturating_add(size_of::<Value>())
            .saturating_add(element_bytes(text, &indices.data));
        ctx.check_memory(projected)?;
    }
    let mut output = Buffer::with_capacity(ctx, count)?;
    cursor = Cursor::default();
    while let Some(indices) = cursor.next(ctx, &mut search, program, text)? {
        output.data.push(element(ctx, subject, &indices.data)?);
    }
    Value::from_array(ctx, output)
}

pub(crate) struct Driver {
    regex: Arc<Regex>,
    receiver: Value,
    search: Search,
    cursor: Cursor,
    single: bool,
    pub waiting: bool,
}

impl Driver {
    pub fn new(
        ctx: &mut CallContext,
        name: &str,
        receiver: &Value,
        args: &[Value],
        keywords: bool,
        block: bool,
    ) -> Result<Option<Self>> {
        if !block || !matches!(name, "match" | "scan") || !matches!(receiver.0, Kind::Bytes(_)) {
            return Ok(None);
        }
        let (regex, start) = arguments(ctx, name, receiver, args, keywords)?;
        let single = name == "match";
        let position = if single {
            offset(ctx, receiver.require_bytes()?, start)?
        } else {
            Some(0)
        };
        let search = Search::new(ctx, regex.view(), true)?;
        Ok(Some(Self {
            regex,
            receiver: receiver.clone(),
            search,
            cursor: Cursor {
                position: position.unwrap_or(0),
                previous_end: None,
                done: position.is_none(),
            },
            single,
            waiting: false,
        }))
    }

    pub fn advance(&mut self, ctx: &mut CallContext, returned: Option<Value>) -> Result<Progress> {
        if self.single && self.waiting {
            self.waiting = false;
            return Ok(Progress::Done(returned.unwrap()));
        }
        drop(returned);
        self.waiting = false;
        ctx.checkpoint()?;
        let Some(indices) = self.cursor.next(
            ctx,
            &mut self.search,
            self.regex.view(),
            self.receiver.require_bytes()?,
        )?
        else {
            return Ok(Progress::Done(if self.single {
                Value::nil()
            } else {
                self.receiver.clone()
            }));
        };
        ctx.check_memory(if self.single {
            0
        } else {
            element_bytes(self.receiver.require_bytes()?, &indices.data)
        })?;
        let value = if self.single {
            matches::data(ctx, self.regex.view(), &self.receiver, &indices.data)?
        } else {
            element(ctx, &self.receiver, &indices.data)?
        };
        self.waiting = true;
        Ok(Progress::Yield([value, Value::nil(), Value::nil()], 1))
    }
}
