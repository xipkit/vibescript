use crate::{
    CallContext, Error, ErrorKind, Result, Value,
    budget::{Buffer, MAX_VALUE_DEPTH},
    hash::Hash,
    iteration::Progress,
};

pub(crate) fn method(name: &str) -> bool {
    matches!(name, "merge" | "deep_transform_keys")
}

pub(crate) enum Driver {
    Merge(Merge),
    Deep(Deep),
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
        if receiver.as_hash().is_none() {
            return Ok(None);
        }
        match name {
            "merge" => {
                if keywords {
                    return Err(Error::new(
                        ErrorKind::Argument,
                        "hash.merge does not accept keyword arguments",
                    ));
                }
                for (index, arg) in args.iter().enumerate() {
                    ctx.charge(1)?;
                    if arg.as_hash().is_none() {
                        return Err(Error::new(
                            ErrorKind::Argument,
                            format!("hash.merge argument {} must be a hash", index + 1),
                        ));
                    }
                }
                let mut sources = Buffer::empty();
                sources.extend(ctx, std::slice::from_ref(receiver))?;
                sources.extend(ctx, args)?;
                Ok(Some(Self::Merge(Merge {
                    sources,
                    source: 0,
                    position: 0,
                    output: Hash::empty(),
                    pending: None,
                    block,
                })))
            }
            "deep_transform_keys" => {
                if !args.is_empty() {
                    return Err(Error::new(
                        ErrorKind::Argument,
                        "hash.deep_transform_keys does not take arguments",
                    ));
                }
                if !block {
                    return Err(Error::new(
                        ErrorKind::Argument,
                        "hash.deep_transform_keys requires a block",
                    ));
                }
                let mut frames = Buffer::empty();
                frames.push(ctx, WalkFrame::new(receiver.clone()))?;
                Ok(Some(Self::Deep(Deep {
                    frames,
                    waiting: false,
                })))
            }
            _ => Ok(None),
        }
    }

    pub fn waiting(&self) -> bool {
        match self {
            Self::Merge(state) => state.pending.is_some(),
            Self::Deep(state) => state.waiting,
        }
    }

    pub fn advance(&mut self, ctx: &mut CallContext, returned: Option<Value>) -> Result<Progress> {
        match self {
            Self::Merge(state) => state.advance(ctx, returned),
            Self::Deep(state) => state.advance(ctx, returned),
        }
    }
}

pub(crate) struct Merge {
    sources: Buffer<Value>,
    source: usize,
    position: usize,
    output: Hash,
    pending: Option<Value>,
    block: bool,
}

impl Merge {
    fn advance(&mut self, ctx: &mut CallContext, returned: Option<Value>) -> Result<Progress> {
        if let Some(value) = returned {
            self.output
                .insert(ctx, self.pending.take().unwrap(), value)?;
        }
        loop {
            ctx.charge(1)?;
            let Some(source) = self.sources.data.get(self.source) else {
                let output = std::mem::replace(&mut self.output, Hash::empty());
                return Ok(Progress::Done(Value::from_hash(ctx, output)?));
            };
            let Some((key, value)) = source.as_hash().unwrap().get(self.position) else {
                self.source += 1;
                self.position = 0;
                continue;
            };
            self.position += 1;
            if self.source != 0 && self.block {
                if let Some(index) = self.output.find(ctx, key.require_bytes()?)? {
                    self.pending = Some(key.clone());
                    let old = self.output.buffer.data[index].1.clone();
                    return Ok(Progress::Yield([key.clone(), old, value.clone()], 3));
                }
            }
            self.output.insert(ctx, key.clone(), value.clone())?;
        }
    }
}

enum Output {
    Array(Buffer<Value>),
    Hash(Hash),
}

struct WalkFrame {
    input: Value,
    position: usize,
    key: Option<Value>,
    output: Output,
}

impl WalkFrame {
    fn new(input: Value) -> Self {
        let output = if input.as_array().is_some() {
            Output::Array(Buffer::empty())
        } else {
            Output::Hash(Hash::empty())
        };
        Self {
            input,
            position: 0,
            key: None,
            output,
        }
    }

    fn append(&mut self, ctx: &mut CallContext, value: Value) -> Result<()> {
        match &mut self.output {
            Output::Array(values) => values.push(ctx, value)?,
            Output::Hash(hash) => hash.insert(ctx, self.key.take().unwrap(), value)?,
        }
        self.position += 1;
        Ok(())
    }

    fn finish(self, ctx: &mut CallContext) -> Result<Value> {
        match self.output {
            Output::Array(values) => Value::from_array(ctx, values),
            Output::Hash(hash) => Value::from_hash(ctx, hash),
        }
    }
}

pub(crate) struct Deep {
    frames: Buffer<WalkFrame>,
    waiting: bool,
}

impl Deep {
    fn advance(&mut self, ctx: &mut CallContext, returned: Option<Value>) -> Result<Progress> {
        if let Some(value) = returned {
            self.waiting = false;
            let key = value.key_name_for("hash.deep_transform_keys block returned an")?;
            let key = ctx.bytes(key)?;
            self.frames.data.last_mut().unwrap().key = Some(key);
        }
        loop {
            ctx.charge(1)?;
            let frame = self.frames.data.last_mut().unwrap();
            let child = if let Some(entries) = frame.input.as_hash() {
                if let Some((key, value)) = entries.get(frame.position) {
                    if frame.key.is_none() {
                        self.waiting = true;
                        return Ok(Progress::Yield(
                            [key.clone(), Value::nil(), Value::nil()],
                            1,
                        ));
                    }
                    Some(value.clone())
                } else {
                    None
                }
            } else {
                frame.input.as_array().unwrap().get(frame.position).cloned()
            };
            if let Some(child) = child {
                if child.as_hash().is_some() || child.as_array().is_some() {
                    if self.frames.data.len() >= MAX_VALUE_DEPTH {
                        return ctx.guard(ErrorKind::Recursion, "value nesting too deep");
                    }
                    self.frames.push(ctx, WalkFrame::new(child))?;
                } else {
                    frame.append(ctx, child)?;
                }
            } else {
                let value = self.frames.data.pop().unwrap().finish(ctx)?;
                if let Some(parent) = self.frames.data.last_mut() {
                    parent.append(ctx, value)?;
                } else {
                    return Ok(Progress::Done(value));
                }
            }
        }
    }
}
