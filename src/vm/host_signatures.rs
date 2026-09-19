use super::*;

impl Run {
    pub(super) fn host_arguments(
        &mut self,
        ctx: &mut CallContext,
        program: &Program,
        method: &crate::capability::BoundMethod,
        args: &mut Arguments,
    ) -> Result<()> {
        if let Some(signature) = method.signature() {
            let count = args.positional.data.len();
            let name = method.name();
            if count < signature.required {
                return Err(Error::new(
                    ErrorKind::Argument,
                    format!(
                        "{name} expects at least {} arguments, got {count}",
                        signature.required
                    ),
                ));
            }
            if count > signature.params.len() {
                return Err(Error::new(
                    ErrorKind::Argument,
                    format!(
                        "{name} expects at most {} arguments, got {count}",
                        signature.params.len()
                    ),
                ));
            }
            if !args.keywords.buffer.data.is_empty() {
                return Err(Error::new(
                    ErrorKind::Argument,
                    format!("{name} does not take keyword arguments"),
                ));
            }
            if args.block.is_some() && !signature.source.accepts_block {
                return Err(Error::new(
                    ErrorKind::Argument,
                    format!("{name} does not take a block"),
                ));
            }
            for (index, value) in args.positional.data.iter_mut().enumerate() {
                let Some(ty) = &signature.params[index] else {
                    continue;
                };
                *value = self.host_value(
                    ctx,
                    program,
                    ty,
                    value.clone(),
                    crate::types::Context::HostArgument(
                        name,
                        &signature.source.params[index].name,
                        index,
                    ),
                )?;
            }
        }
        Ok(())
    }

    pub(super) fn host_result(
        &mut self,
        ctx: &mut CallContext,
        program: &Program,
        method: &crate::capability::BoundMethod,
        value: Value,
    ) -> Result<Value> {
        let Some(ty) = method
            .signature()
            .and_then(|signature| signature.result.as_ref())
        else {
            return Ok(value);
        };
        let value = ctx.import(&value)?;
        self.host_value(
            ctx,
            program,
            ty,
            value,
            crate::types::Context::Return(method.name()),
        )
    }

    fn host_value(
        &mut self,
        ctx: &mut CallContext,
        program: &Program,
        ty: &crate::types::Type,
        value: Value,
        context: crate::types::Context<'_>,
    ) -> Result<Value> {
        crate::types::prepare(ctx, ty, |ctx, name| {
            match resolve_type(
                program,
                ctx,
                &self.frames,
                &mut self.storage,
                None,
                name,
                false,
            ) {
                Ok(value) => Ok(value),
                Err(error) => Err(crate::types::host_resolution(ctx, context, name, error)?),
            }
        })?
        .normalize_with(ctx, value, context)
    }
}
