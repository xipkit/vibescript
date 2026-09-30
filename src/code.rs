use crate::{CallContext, Error, Result, bytecode::Program, capability::Registered};
use std::{collections::BTreeMap, fmt, sync::Arc};

pub(crate) struct Code {
    pub program: Program,
    pub hosts: Vec<Registered>,
    /// The globals and capabilities the host declares, which every call
    /// must supply as declared.
    pub declared: Arc<crate::declared::Declarations>,
    pub origin: Option<crate::loading::Origin>,
    pub exports: Vec<(String, Export)>,
}

pub(crate) enum Export {
    Function(usize),
    Enum(usize),
}

impl Code {
    pub fn retain(ctx: &mut CallContext, owner: &Arc<Self>) -> Result<()> {
        let Some(mut roots) = ctx.code_roots.take() else {
            return Ok(());
        };
        // Callback destructors must run after the invocation releases its heap locks.
        let result = (|| {
            for previous in &roots.data {
                ctx.charge(1)?;
                if Arc::ptr_eq(previous, owner) {
                    return Ok(());
                }
            }
            roots.push(ctx, owner.clone())
        })();
        ctx.code_roots = Some(roots);
        result
    }

    /// Compiles host source without a module loader.
    #[cfg(test)]
    pub fn compile(source: &str, registered: &BTreeMap<String, Registered>) -> Result<Arc<Self>> {
        Self::compile_typed(
            source,
            registered.iter(),
            &Arc::default(),
            false,
            None,
            &(),
            None,
        )
    }

    /// Compiles host source, charging the work to `work`, with `loader`
    /// resolving the files it requires.
    pub fn compile_metered(
        source: &str,
        registered: &BTreeMap<String, Registered>,
        declared: &Arc<crate::declared::Declarations>,
        work: &dyn crate::compilation::Work,
        loader: &crate::loading::Loader,
    ) -> Result<Arc<Self>> {
        Self::compile_typed(
            source,
            registered.iter(),
            declared,
            false,
            None,
            work,
            Some(loader),
        )
    }

    /// Compiles a required file without a module loader.
    #[cfg(test)]
    pub fn compile_file(
        source: &str,
        registered: &BTreeMap<String, Registered>,
    ) -> Result<Arc<Self>> {
        Self::compile_mode(source, registered.iter(), true, None, &())
    }

    pub fn compile_module(
        ctx: &mut CallContext,
        source: &str,
        receiving: &Self,
        origin: crate::loading::Origin,
        loader: &crate::loading::Loader,
    ) -> Result<Arc<Self>> {
        Self::compile_typed(
            source,
            receiving.program.hosts.iter().zip(&receiving.hosts),
            &receiving.declared,
            true,
            Some(origin),
            &crate::compilation::Meter(std::cell::RefCell::new(ctx)),
            Some(loader),
        )
    }

    #[cfg(test)]
    fn compile_mode<'a>(
        source: &str,
        registered: impl Iterator<Item = (&'a String, &'a Registered)> + Clone,
        file: bool,
        origin: Option<crate::loading::Origin>,
        work: &dyn crate::compilation::Work,
    ) -> Result<Arc<Self>> {
        Self::compile_typed(
            source,
            registered,
            &Arc::default(),
            file,
            origin,
            work,
            None,
        )
    }

    fn compile_typed<'a>(
        source: &str,
        registered: impl Iterator<Item = (&'a String, &'a Registered)> + Clone,
        declared: &Arc<crate::declared::Declarations>,
        file: bool,
        origin: Option<crate::loading::Origin>,
        work: &dyn crate::compilation::Work,
        loader: Option<&crate::loading::Loader>,
    ) -> Result<Arc<Self>> {
        work.checkpoint()?;
        let mut names = Vec::new();
        for (name, _) in registered.clone() {
            crate::syntax::host_function_name(work, crate::syntax::HostName::FUNCTION, name)?;
            names.push(name.clone());
        }
        let filename = origin.as_ref().map(crate::loading::Origin::filename);
        let parse_error =
            |error| crate::source::parse_error(source, filename.as_ref(), error, work);
        let (parsed, tokens, _tokens_held) = crate::syntax::parse_with_tokens(source, work)
            .map_err(|error| {
                parse_error(if file {
                    crate::syntax::canonical_syntax(source, work, error)
                } else {
                    crate::syntax::host_syntax(source, work, error)
                })
            })?;
        let resolve = |path: &str, origin: Option<&crate::loading::Origin>, ctx: &mut _| {
            loader.unwrap().source(path, origin, ctx)
        };
        let mut checked = crate::typing::check(&crate::typing::Input {
            source,
            parsed: &parsed,
            tokens: &tokens,
            hosts: registered.clone().collect(),
            declared,
            file,
            origin: origin.as_ref(),
            modules: loader.is_some().then_some(&resolve),
            budget: work.budget(),
            observe: None,
            annotate: false,
        });
        work.charge(usize::try_from(checked.steps).unwrap_or(usize::MAX))?;
        work.checkpoint()?;
        if checked.stopped {
            // Charging the steps or the checkpoint fails first unless the
            // check stopped for memory, which it does not charge.
            return Err(work.allocation_error("memory quota exceeded while checking types"));
        }
        // The checker's tables, by its own account, held this much at most
        // while it ran, which counts toward the call's peak and its quota.
        drop(work.reserve(checked.peak_bytes + checked.surface_bytes)?);
        // What the check found stays while the compiler reads it.
        let _found = work.reserve(checked.bytes())?;
        if checked.diagnostics.iter().any(|d| d.is_error()) {
            let mut text = crate::source::Source::compile(source, work)?;
            text.filename = filename.clone();
            let diagnostics = checked
                .diagnostics
                .into_iter()
                .map(|diagnostic| match diagnostic.file {
                    Some(_) => diagnostic,
                    None => diagnostic.in_file(filename.clone()),
                })
                .collect();
            return Err(Error::from_diagnostics(
                crate::ErrorKind::Type,
                diagnostics,
                &text,
            ));
        }
        checked.facts.keep_type_checks = loader.is_some_and(|loader| loader.keep_type_checks);
        let mut program =
            crate::bytecode::compile_parsed(source, parsed, names, file, &checked.facts, work)
                .map_err(parse_error)?;
        program.source.filename = filename;
        let mut hosts = Vec::new();
        for (name, host) in registered {
            work.bytes(name.len())?;
            hosts.push(host.clone());
        }
        let mut exports = Vec::new();
        if file {
            for (name, &index) in &program.names {
                work.bytes(name.len())?;
                if index != 0 && !program.functions[index].private {
                    exports.push((name.clone(), Export::Function(index)));
                }
            }
            for (name, &index) in &program.declaration_names {
                work.bytes(name.len())?;
                if matches!(program.declarations[index].0, crate::value::Kind::Enum(_)) {
                    exports.push((name.clone(), Export::Enum(index)));
                }
            }
            work.charge(
                exports
                    .len()
                    .saturating_mul(exports.len().max(1).ilog2() as usize + 1),
            )?;
            exports.sort_unstable_by(|a, b| a.0.cmp(&b.0));
        }
        work.charge(program.namespaces.len())?;
        work.checkpoint()?;
        Ok(Arc::new_cyclic(|owner| {
            program.owner = owner.clone();
            for definition in &program.namespaces {
                assert!(definition.owner.set(owner.clone()).is_ok());
            }
            Self {
                program,
                hosts,
                declared: declared.clone(),
                origin,
                exports,
            }
        }))
    }
}

impl fmt::Debug for Code {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Code")
            .field("functions", &self.program.functions.len())
            .field("namespaces", &self.program.namespaces.len())
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests;
