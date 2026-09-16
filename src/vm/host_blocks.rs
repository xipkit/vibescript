use super::*;

impl Run<'_> {
    pub(super) fn host(&mut self, ctx: &mut CallContext) -> Result<Event> {
        let current = self.frames.data.len() - 1;
        let mut args = self.frames.data[current].arguments.data.pop().unwrap();
        let Some(crate::arguments::Target::Capability(method)) = args.target.take() else {
            unreachable!()
        };
        let block = args.block;
        let mut pending = None;
        let mut invoke = |ctx: &mut CallContext, values: &[Value]| {
            ctx.checkpoint()?;
            if pending.is_some() {
                return Err(transferred());
            }
            match self.block(ctx, block.unwrap(), values)? {
                Exit::Value(value) => Ok(value),
                Exit::Control(control) => {
                    pending = Some(control);
                    Err(transferred())
                }
            }
        };
        let mut call = crate::HostCall::new(ctx, block.map(|_| &mut invoke as _));
        let result =
            method.invoke_block(&mut call, &args.positional.data, &args.keywords.buffer.data);
        ctx.checkpoint()?;
        let value = match pending {
            Some(Control::Return { target, value, .. }) if target == current => value,
            Some(control) => return Ok(Event::Control(control)),
            None => result?,
        };
        let value = method.finish(ctx, value)?;
        programs::imported(ctx, self.storage, &value)?;
        Ok(Event::Control(Control::Return {
            target: current,
            value,
            normalize: false,
        }))
    }

    fn block(&mut self, ctx: &mut CallContext, block: Block, values: &[Value]) -> Result<Exit> {
        let floor = self.frames.data.len();
        let handlers = self.storage.handlers.data.len();
        let result = (|| {
            let mut args = Buffer::with_capacity(ctx, values.len())?;
            for value in values {
                let value = ctx.import(value)?;
                crate::exports::check(ctx, &value)?;
                args.data.push(value);
            }
            enter_block(
                ctx,
                self.frames,
                self.storage,
                block,
                &args.data,
                self.stack.data.len(),
            )?;
            for value in &args.data {
                programs::imported(ctx, self.storage, value)?;
            }
            self.until(ctx, Some(floor))
        })();
        let result = (|| match result {
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
        self.storage.handlers.data.truncate(handlers);
        if self.frames.data.len() > floor {
            unwind(self.frames, self.storage, self.stack, floor);
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
