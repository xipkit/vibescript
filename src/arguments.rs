use crate::{
    CallContext, Error, ErrorKind, Result, Value,
    budget::{Buffer, CHUNK},
    bytecode::{ArgumentOp, Invocation, Parameter},
    hash::Hash,
    syntax::ParamKind,
    value::Kind,
};
use std::{cmp::Ordering, mem};

pub(crate) enum Target {
    Plain(Invocation),
    Offset(std::sync::Arc<crate::regex::matches::Offset>),
}

pub(crate) struct Arguments {
    pub positional: Buffer<Value>,
    pub keywords: Hash,
    pub options_hash: bool,
    pub target: Option<Target>,
    pub block: Option<Block>,
}

#[derive(Clone, Copy)]
pub(crate) struct Block {
    pub function: usize,
    pub parent: usize,
}

impl Arguments {
    pub fn empty() -> Self {
        Self {
            positional: Buffer::empty(),
            keywords: Hash::empty(),
            options_hash: true,
            target: None,
            block: None,
        }
    }

    pub fn from_values(ctx: &mut CallContext, values: &[Value]) -> Result<Self> {
        let mut arguments = Self::empty();
        arguments.positional = Buffer::with_capacity(ctx, values.len())?;
        arguments.positional.extend(ctx, values)?;
        Ok(arguments)
    }

    pub fn push(
        &mut self,
        ctx: &mut CallContext,
        op: ArgumentOp,
        name: &str,
        value: Value,
    ) -> Result<()> {
        match op {
            ArgumentOp::Positional => self.positional.push(ctx, value),
            ArgumentOp::Splat => {
                let values = value.as_array().ok_or_else(|| {
                    Error::new(ErrorKind::Type, "splat argument must be an array")
                })?;
                let Some(length) = self.positional.data.len().checked_add(values.len()) else {
                    return ctx.fail(ErrorKind::Memory, "argument count overflow");
                };
                self.positional.ensure(ctx, length)?;
                for value in values {
                    ctx.charge(1)?;
                    self.positional.data.push(value.clone());
                }
                Ok(())
            }
            ArgumentOp::Keyword(_) => {
                let key = ctx.bytes(name.as_bytes())?;
                self.keywords.insert(ctx, key, value)
            }
            ArgumentOp::KeywordSplat => {
                let Kind::Hash(hash) = value.0 else {
                    return Err(Error::new(
                        ErrorKind::Type,
                        "keyword splat argument must be a hash",
                    ));
                };
                for (key, value) in &hash.buffer.data {
                    self.keywords.insert(ctx, key.clone(), value.clone())?;
                }
                Ok(())
            }
        }
    }

    fn collapse(&mut self, ctx: &mut CallContext, params: &[Parameter]) -> Result<()> {
        if !self.options_hash || self.keywords.buffer.data.is_empty() {
            return Ok(());
        }
        for param in params {
            ctx.charge(1)?;
            if matches!(param.kind, ParamKind::Keyword | ParamKind::KeywordRest) {
                return Ok(());
            }
        }
        let mut positional = self.positional.data.len();
        for param in params {
            ctx.charge(1)?;
            if param.kind == ParamKind::Positional {
                if positional > 0 {
                    positional -= 1;
                    continue;
                }
                if self.keywords.find(ctx, param.name.as_bytes())?.is_some() {
                    return Ok(());
                }
            }
            let hash = mem::replace(&mut self.keywords, Hash::empty());
            let hash = ordered_hash(ctx, hash.buffer)?;
            let value = Value::from_hash(ctx, hash)?;
            self.positional.push(ctx, value)?;
            break;
        }
        Ok(())
    }
}

enum Source {
    Positional(usize),
    Keyword(usize),
    Rest(usize),
    KeywordRest,
    Default,
}

pub(crate) struct Binding {
    arguments: Arguments,
    sources: Buffer<Source>,
    used: Buffer<bool>,
}

impl Binding {
    pub fn new(
        ctx: &mut CallContext,
        params: &[Parameter],
        mut arguments: Arguments,
    ) -> Result<Self> {
        arguments.collapse(ctx, params)?;
        let mut used = Buffer::with_capacity(ctx, arguments.keywords.buffer.data.len())?;
        for _ in &arguments.keywords.buffer.data {
            ctx.charge(1)?;
            used.data.push(false);
        }
        let mut sources = Buffer::with_capacity(ctx, params.len())?;
        let mut positional = 0;
        let mut keyword_rest = false;
        for param in params {
            ctx.charge(1)?;
            let source = match param.kind {
                ParamKind::Rest => {
                    let source = Source::Rest(positional);
                    positional = arguments.positional.data.len();
                    source
                }
                ParamKind::KeywordRest => {
                    keyword_rest = true;
                    Source::KeywordRest
                }
                ParamKind::Positional if positional < arguments.positional.data.len() => {
                    let source = Source::Positional(positional);
                    positional += 1;
                    source
                }
                _ => {
                    if let Some(index) = arguments.keywords.find(ctx, param.name.as_bytes())? {
                        used.data[index] = true;
                        Source::Keyword(index)
                    } else if param.default {
                        Source::Default
                    } else {
                        let label = if param.kind == ParamKind::Keyword {
                            "keyword argument"
                        } else {
                            "argument"
                        };
                        return Err(Error::new(
                            ErrorKind::Argument,
                            format!("missing {label} {}", param.name),
                        ));
                    }
                }
            };
            sources.data.push(source);
        }
        if positional < arguments.positional.data.len() {
            return Err(Error::new(
                ErrorKind::Argument,
                "unexpected positional arguments",
            ));
        }
        if !keyword_rest {
            for (index, used) in used.data.iter().enumerate() {
                ctx.charge(1)?;
                if !used {
                    return Err(Error::new(
                        ErrorKind::Argument,
                        format!(
                            "unexpected keyword argument {}",
                            arguments.keywords.buffer.data[index].0
                        ),
                    ));
                }
            }
        }
        Ok(Self {
            arguments,
            sources,
            used,
        })
    }

    pub fn value(&self, ctx: &mut CallContext, param: usize) -> Result<Option<Value>> {
        ctx.charge(1)?;
        Ok(Some(match self.sources.data[param] {
            Source::Positional(index) => self.arguments.positional.data[index].clone(),
            Source::Keyword(index) => self.arguments.keywords.buffer.data[index].1.clone(),
            Source::Rest(start) => ctx.array(&self.arguments.positional.data[start..])?,
            Source::KeywordRest => {
                let mut buffer = Buffer::empty();
                for (index, pair) in self.arguments.keywords.buffer.data.iter().enumerate() {
                    ctx.charge(1)?;
                    if !self.used.data[index] {
                        buffer.push(ctx, pair.clone())?;
                    }
                }
                let hash = ordered_hash(ctx, buffer)?;
                Value::from_hash(ctx, hash)?
            }
            Source::Default => return Ok(None),
        }))
    }
}

fn compare_keys(ctx: &mut CallContext, a: &Value, b: &Value) -> Result<Ordering> {
    let a = a.require_bytes()?;
    let b = b.require_bytes()?;
    for (a, b) in a.chunks(CHUNK).zip(b.chunks(CHUNK)) {
        ctx.work_bytes(a.len().max(b.len()))?;
        let order = a.cmp(b);
        if order != Ordering::Equal {
            return Ok(order);
        }
    }
    Ok(a.len().cmp(&b.len()))
}

pub(crate) fn ordered_hash(
    ctx: &mut CallContext,
    mut buffer: Buffer<(Value, Value)>,
) -> Result<Hash> {
    fn sift(ctx: &mut CallContext, values: &mut [(Value, Value)], mut root: usize) -> Result<()> {
        while root < values.len() / 2 {
            ctx.charge(1)?;
            let mut child = root * 2 + 1;
            if child + 1 < values.len()
                && compare_keys(ctx, &values[child].0, &values[child + 1].0)? == Ordering::Less
            {
                child += 1;
            }
            if compare_keys(ctx, &values[root].0, &values[child].0)? != Ordering::Less {
                break;
            }
            values.swap(root, child);
            root = child;
        }
        Ok(())
    }
    for root in (0..buffer.data.len() / 2).rev() {
        sift(ctx, &mut buffer.data, root)?;
    }
    for end in (1..buffer.data.len()).rev() {
        ctx.charge(1)?;
        buffer.data.swap(0, end);
        sift(ctx, &mut buffer.data[..end], 0)?;
    }
    Hash::from_entries(ctx, buffer)
}
