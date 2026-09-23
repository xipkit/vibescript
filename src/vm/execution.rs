use super::*;
use crate::{CallOptions, Outcome, Script};

pub(crate) struct Execution {
    pub(super) context: CallContext,
    pub(super) run: Option<Run>,
    code: Arc<crate::code::Code>,
    function: usize,
}

impl Execution {
    pub(crate) fn new(
        script: &Script,
        name: &str,
        args: &[Value],
        keywords: &[(String, Value)],
        options: CallOptions,
    ) -> Result<Self> {
        let mut context = CallContext::new(options);
        context.strict_effects = script.inner.strict_effects;
        context.random_source = script.inner.random_source.clone();
        context.output_writer = script.inner.output_writer.clone();
        context.error_writer = script.inner.error_writer.clone();
        context.checkpoint()?;
        let function = *script.inner.code.program.names.get(name).ok_or_else(|| {
            crate::members::suggest::missing_function(&script.inner.code.program, name)
        })?;
        let mut execution = Self {
            context,
            run: None,
            code: script.inner.code.clone(),
            function,
        };
        execution.context.code_roots = Some(Buffer::empty());
        execution.context.host_roots = Some(Buffer::empty());
        execution.run = Some(
            Run::new(
                &execution.code,
                &script.inner.loader,
                &mut execution.context,
                function,
                args,
                keywords,
            )
            .map_err(|error| diagnose(&execution.code.program, &[], function, error))?,
        );
        Ok(execution)
    }

    pub(crate) fn run(mut self) -> Result<Outcome> {
        let result = self.run.as_mut().unwrap().run(&mut self.context);
        self.finish(result)
    }

    pub(super) fn finish(mut self, result: Result<Value>) -> Result<Outcome> {
        let frames = self
            .run
            .as_ref()
            .map_or(&[][..], |run| run.frames.data.as_slice());
        let result =
            result.map_err(|error| diagnose(&self.code.program, frames, self.function, error));
        self.release_execution();
        let value = result?;
        crate::objects::finish(&mut self.context)?;
        self.context.code_roots = None;
        self.context.host_roots = None;
        self.context.checkpoint()?;
        Ok(Outcome {
            value,
            stats: self.context.stats(),
        })
    }

    fn release_execution(&mut self) {
        self.context.enum_rebind = crate::enums::Rebind::default();
        self.context.capability_names = Buffer::empty();
        self.run = None;
        self.context.random = None;
    }
}

impl Drop for Execution {
    fn drop(&mut self) {
        self.release_execution();
        crate::objects::cleanup(&mut self.context);
        self.context.code_roots = None;
        self.context.host_roots = None;
    }
}
