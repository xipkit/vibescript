use super::*;

pub(super) struct HostRequest {
    pub current: usize,
    pub args: Arguments,
    pub method: Arc<crate::capability::BoundMethod>,
    program: Arc<Program>,
}

pub(super) struct HostControl {
    pub block: Option<Block>,
    receiver: Option<Value>,
    location: Option<publication::Location>,
    pending: Option<Control>,
}

impl HostControl {
    pub fn new(request: &HostRequest) -> Self {
        Self {
            block: request.args.block,
            receiver: request.args.receiver.clone(),
            location: None,
            pending: None,
        }
    }

    pub fn block(&self, ctx: &mut CallContext) -> Result<Block> {
        ctx.checkpoint()?;
        if self.pending.is_some() {
            return Err(transferred());
        }
        self.block
            .ok_or_else(|| Error::new(ErrorKind::Argument, "block required"))
    }

    /// Returns an accounted snapshot of the member receiver, if the call has one.
    ///
    /// Captured instances and module environments are copied even when the
    /// receiver already belongs to this invocation.
    pub fn receiver(&self, ctx: &mut CallContext, storage: &Storage) -> Result<Option<Value>> {
        ctx.checkpoint()?;
        self.receiver
            .as_ref()
            .map(|receiver| programs::snapshot(ctx, storage, receiver))
            .transpose()
    }

    /// Stores a field in the member receiver, publishing it to the capability
    /// binding that holds the receiver.
    pub fn set_receiver_field(
        &mut self,
        ctx: &mut CallContext,
        storage: &mut Storage,
        key: &[u8],
        value: &Value,
    ) -> Result<bool> {
        let Some(receiver) = &mut self.receiver else {
            ctx.checkpoint()?;
            return Err(Error::new(
                ErrorKind::Argument,
                "host method has no member receiver",
            ));
        };
        publication::set_field(ctx, storage, receiver, &mut self.location, key, value)
    }

    pub fn completed(&mut self, result: Result<Exit>) -> Result<Value> {
        match result? {
            Exit::Value(value) => Ok(value),
            Exit::Control(control) => {
                self.pending = Some(control);
                Err(transferred())
            }
        }
    }
}

#[derive(Clone, Copy)]
pub(super) struct BlockBoundary {
    pub floor: usize,
    handlers: usize,
}

struct Borrowed<'a> {
    context: &'a mut CallContext,
    run: &'a mut Run,
    control: HostControl,
}

impl crate::host_call::Backend for Borrowed<'_> {
    fn context(&mut self) -> &mut CallContext {
        self.context
    }

    fn block_given(&self) -> bool {
        self.control.block.is_some()
    }

    fn receiver(&mut self) -> Result<Option<Value>> {
        self.control.receiver(self.context, &self.run.storage)
    }

    fn set_receiver_field(&mut self, key: &[u8], value: &Value) -> Result<bool> {
        self.control
            .set_receiver_field(self.context, &mut self.run.storage, key, value)
    }

    fn call_block(&mut self, args: &[Value]) -> Result<Value> {
        let block = self.control.block(self.context)?;
        let result = self.run.block(self.context, block, args);
        self.control.completed(result)
    }
}

impl Run {
    pub(super) fn host(&mut self, ctx: &mut CallContext) -> Result<Event> {
        let request = self.prepare_host(ctx)?;
        let mut backend = Borrowed {
            context: ctx,
            run: self,
            control: HostControl::new(&request),
        };
        let result = request.method.invoke(
            &mut crate::HostCall::new(&mut backend),
            &request.args.positional.data,
            &request.args.keywords.buffer.data,
        );
        let control = backend.control;
        self.finish_host(ctx, request, control, result)
    }

    pub(super) fn prepare_host(&mut self, ctx: &mut CallContext) -> Result<HostRequest> {
        let current = self.frames.data.len() - 1;
        let mut args = self.frames.data[current].arguments.data.pop().unwrap();
        let Some(crate::arguments::Target::Capability(method)) = args.target.take() else {
            unreachable!()
        };
        programs::snapshot_arguments(
            ctx,
            &self.storage,
            &mut args.positional.data,
            &mut args.keywords.buffer.data,
        )?;
        method.begin(
            ctx,
            &args.positional.data,
            &args.keywords.buffer.data,
            args.block.is_some(),
        )?;
        let program = self.frames.data[current].program.clone();
        self.host_arguments(ctx, &program, &method, &mut args)?;
        Ok(HostRequest {
            current,
            args,
            method,
            program,
        })
    }

    pub(super) fn finish_host(
        &mut self,
        ctx: &mut CallContext,
        request: HostRequest,
        control: HostControl,
        result: Result<Value>,
    ) -> Result<Event> {
        ctx.checkpoint()?;
        let current = request.current;
        let value = match control.pending {
            Some(Control::Return { target, value, .. }) if target == current => {
                programs::snapshot(ctx, &self.storage, &value)?
            }
            Some(control) => return Ok(Event::Control(control)),
            None => ctx.snapshot(&result?)?,
        };
        let value = self.host_result(ctx, &request.program, &request.method, value)?;
        let value = request.method.finish(ctx, value)?;
        programs::imported(ctx, &mut self.storage, &value)?;
        Ok(Event::Control(Control::Return {
            target: current,
            value,
            normalize: false,
        }))
    }

    pub(super) fn block_boundary(&self) -> BlockBoundary {
        BlockBoundary {
            floor: self.frames.data.len(),
            handlers: self.storage.handlers.data.len(),
        }
    }

    pub(super) fn start_block(
        &mut self,
        ctx: &mut CallContext,
        block: Block,
        values: &[Value],
    ) -> Result<Buffer<Value>> {
        let mut args = Buffer::with_capacity(ctx, values.len())?;
        for value in values {
            let value = ctx.import(value)?;
            crate::exports::check(ctx, &value)?;
            args.data.push(value);
        }
        ctx.snapshot_values(&mut args.data)?;
        enter_block(
            ctx,
            &mut self.frames,
            &mut self.storage,
            block,
            &args.data,
            self.stack.data.len(),
        )?;
        for value in &args.data {
            programs::imported(ctx, &mut self.storage, value)?;
        }
        Ok(args)
    }

    fn block(&mut self, ctx: &mut CallContext, block: Block, values: &[Value]) -> Result<Exit> {
        let boundary = self.block_boundary();
        let result = (|| {
            let _args = self.start_block(ctx, block, values)?;
            self.until(ctx, Some(boundary.floor))
        })();
        self.finish_block(ctx, boundary, result)
    }

    pub(super) fn finish_block(
        &mut self,
        ctx: &mut CallContext,
        boundary: BlockBoundary,
        result: Result<Exit>,
    ) -> Result<Exit> {
        let result = (|| match result {
            Ok(Exit::Value(value)) => {
                programs::snapshot(ctx, &self.storage, &value).map(Exit::Value)
            }
            Err(error) if !ctx.exhausted() => {
                let error = handlers::SavedError::new(
                    &self.root,
                    ctx,
                    &self.frames.data,
                    self.entry,
                    error,
                )?;
                Err(error.into_error(ctx)?)
            }
            Err(error) => {
                let error = diagnose(&self.root, &self.frames.data, self.entry, error);
                ctx.remember_exhaustion(&error);
                Err(error)
            }
            result => result,
        })();
        self.storage.handlers.data.truncate(boundary.handlers);
        if self.frames.data.len() > boundary.floor {
            unwind(
                &mut self.frames,
                &mut self.storage,
                &mut self.stack,
                boundary.floor,
            );
        }
        result
    }
}

fn transferred() -> Error {
    Error::new(
        ErrorKind::ControlFlow,
        "block transferred control outside the host callback",
    )
}
