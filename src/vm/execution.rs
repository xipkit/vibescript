use super::*;
use crate::{CallOptions, Outcome, Script};
use std::collections::BTreeMap;

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

    /// Runs like [`Self::run`] and also returns the root bindings the entry
    /// leaves behind: every supplied global with the value the call left in
    /// it, the classes, modules and enums the script declares unless a global
    /// shadowed them, and the entry frame's assigned named locals, which take
    /// precedence.
    pub(crate) fn run_bindings(mut self) -> Result<(Outcome, BTreeMap<String, Value>)> {
        let run = self.run.as_mut().unwrap();
        run.storage.root_locals = Some(Buffer::empty());
        let result = run.run(&mut self.context);
        let captured = result.and_then(|value| {
            let globals = self.final_globals()?;
            let declared = self.declared_values()?;
            let run = self.run.as_mut().unwrap();
            let locals = run.storage.root_locals.take().unwrap_or_else(Buffer::empty);
            Ok((value, globals, declared, locals))
        });
        let (result, globals, declared, locals) = match captured {
            Ok((value, globals, declared, locals)) => (Ok(value), globals, declared, locals),
            Err(error) => (
                Err(error),
                Buffer::empty(),
                Buffer::empty(),
                Buffer::empty(),
            ),
        };
        let code = self.code.clone();
        let entry = self.function;
        let mut bindings = std::mem::take(&mut self.context.options.globals);
        let outcome = self.finish(result)?;
        let mut updates = globals.data.into_iter().peekable();
        for (index, value) in bindings.values_mut().enumerate() {
            if let Some((_, updated)) = updates.next_if(|(position, _)| *position == index) {
                *value = updated;
            }
        }
        for (name, &index) in &code.program.declaration_names {
            if let Some((_, value)) = declared.data.iter().find(|(i, _)| *i == index) {
                bindings
                    .entry(name.clone())
                    .or_insert_with(|| value.clone());
            }
        }
        let names = &code.program.functions[entry].local_names;
        for (slot, value) in locals.data {
            bindings.insert(names[slot].clone(), value);
        }
        Ok((outcome, bindings))
    }

    /// Materializes the root script's top-level class, module and enum
    /// declarations that no supplied global shadows, keyed by declaration
    /// index. Nested modules stay reachable through their parents.
    fn declared_values(&mut self) -> Result<Buffer<(usize, Value)>> {
        let mut values = Buffer::empty();
        let run = self.run.as_mut().unwrap();
        let program = run.root.clone();
        for (name, &index) in &program.declaration_names {
            self.context.charge(1)?;
            if name.contains("::") || self.context.options.globals.contains_key(name) {
                continue;
            }
            let value = declaration_value(&program, &mut self.context, &mut run.storage, index)?;
            values.push(&mut self.context, (index, value))?;
        }
        Ok(values)
    }

    /// Reads the current value of each supplied global the call imported,
    /// keyed by its position in name order. Unread globals keep the host's value.
    fn final_globals(&mut self) -> Result<Buffer<(usize, Value)>> {
        let mut values = Buffer::empty();
        let Some(bindings) = self.run.as_ref().unwrap().storage.bindings.clone() else {
            return Ok(values);
        };
        let names = std::mem::take(&mut self.context.options.globals);
        let result = (|| {
            for (index, name) in names.keys().enumerate() {
                let Some(value) = crate::objects::field(&mut self.context, &bindings, name)? else {
                    continue;
                };
                if !matches!(value.0, Kind::Host(_)) {
                    crate::exports::check(&mut self.context, &value)?;
                }
                values.push(&mut self.context, (index, value))?;
            }
            Ok(())
        })();
        self.context.options.globals = names;
        result.map(|()| values)
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

/// Records the entry frame's assigned, named local slots as it returns, so
/// they survive the unwind for [`Execution::run_bindings`].
pub(super) fn capture_root_locals(
    ctx: &mut CallContext,
    frame: &Frame,
    storage: &mut Storage,
) -> Result<()> {
    let function = &frame.program.functions[frame.function.unwrap()];
    let mut captured = Buffer::empty();
    for (slot, name) in function.local_names.iter().enumerate() {
        ctx.charge(1)?;
        if name.is_empty() || name.starts_with('\0') {
            continue;
        }
        if let Some(value) = &storage.locals.data[frame.local_base + slot] {
            crate::exports::check(ctx, value)?;
            captured.push(ctx, (slot, value.clone()))?;
        }
    }
    storage.root_locals = Some(captured);
    Ok(())
}
