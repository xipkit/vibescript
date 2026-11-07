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
        let receiver = self
            .receiver
            .as_ref()
            .map(|receiver| programs::snapshot(ctx, storage, receiver))
            .transpose();
        ctx.latch_internal(receiver)
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
        let stored = publication::set_field(ctx, storage, receiver, &mut self.location, key, value);
        ctx.latch_internal(stored)
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
        let mut args = self.storage.arguments.pop()?;
        let Some(crate::arguments::Target::Capability(method)) = args.target.take() else {
            return Err(crate::checked::internal(
                "host request has no capability target",
            ));
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
        let program = self.frames.at(current)?.program.clone();
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
        for value in &args.data {
            self.stack.push(ctx, value.clone())?;
        }
        enter_block(
            ctx,
            &mut self.frames,
            &mut self.storage,
            &self.stack,
            block,
            args.data.len(),
        )?;
        #[cfg(test)]
        super::host_blocks_tests::entered(&args.data);
        for value in &args.data {
            programs::imported(ctx, &mut self.storage, value)?;
        }
        Ok(args)
    }

    fn block(&mut self, ctx: &mut CallContext, block: Block, values: &[Value]) -> Result<Exit> {
        let boundary = self.block_boundary();
        let mut started = Started::new(ctx, boundary);
        let result = (|| {
            let ctx = started.context();
            let _args = self.start_block(ctx, block, values)?;
            self.until(ctx, Some(boundary.floor))
        })();
        started.finish(self, result)
    }

    pub(super) fn finish_block(
        &mut self,
        ctx: &mut CallContext,
        boundary: BlockBoundary,
        result: Result<Exit>,
    ) -> Result<Exit> {
        // Preparing the block can fail before the run loop sees the error, and
        // so can the steps below; either way the callback gets it latched.
        let result = ctx.latch_internal(result);
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
        let result = if self.frames.data.len() > boundary.floor {
            unwind(
                &mut self.frames,
                &mut self.storage,
                &mut self.stack,
                boundary.floor,
            )
            .map_err(Error::from)
            .and(result)
        } else {
            result
        };
        ctx.latch_internal(result)
    }
}

/// A block entered on a run that has not reached [`Run::finish_block`].
///
/// A host can catch a panic from inside the block and keep using its context,
/// but the run still holds the block's frames and handlers. Dropping the guard
/// before the block finishes therefore latches the invocation: later
/// checkpoints, block calls and handlers all fail, and an error latched
/// earlier, such as an exhausted quota, keeps priority.
pub(super) struct Started<'a> {
    context: &'a mut CallContext,
    boundary: BlockBoundary,
    armed: bool,
}

impl<'a> Started<'a> {
    pub fn new(context: &'a mut CallContext, boundary: BlockBoundary) -> Self {
        Self {
            context,
            boundary,
            armed: true,
        }
    }

    pub fn context(&mut self) -> &mut CallContext {
        self.context
    }

    /// Restores the run to the boundary and settles the block's result.
    pub fn finish(mut self, run: &mut Run, result: Result<Exit>) -> Result<Exit> {
        let result = run.finish_block(self.context, self.boundary, result);
        self.armed = false;
        result
    }

    /// Hands the unfinished block to an owner that latches the same way when
    /// it is dropped early, returning the boundary it must finish at.
    #[cfg(feature = "tokio")]
    pub fn lend(mut self) -> BlockBoundary {
        self.armed = false;
        self.boundary
    }
}

impl Drop for Started<'_> {
    fn drop(&mut self) {
        if self.armed {
            let _latched = self.context.latch_internal::<()>(Err(abandoned().into()));
        }
    }
}

/// The terminal failure of a block that unwound before it finished.
pub(super) fn abandoned() -> crate::checked::Violation {
    crate::checked::violation("block was abandoned before completion")
}

fn transferred() -> Error {
    Error::new(
        ErrorKind::ControlFlow,
        "block transferred control outside the host callback",
    )
}
