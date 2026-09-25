use crate::{CallContext, Error, Result, bytecode::Program, capability::Registered};
use std::{collections::BTreeMap, fmt, sync::Arc};

pub(crate) struct Code {
    pub program: Program,
    pub hosts: Vec<Registered>,
    pub origin: Option<crate::loading::Origin>,
    pub exports: Vec<(String, Export)>,
    /// Whether the code and the files it requires are type checked statically.
    pub static_types: bool,
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

    #[cfg(test)]
    pub fn compile(
        source: &str,
        registered: &BTreeMap<String, Registered>,
        static_types: bool,
    ) -> Result<Arc<Self>> {
        let typing = static_types.then_some(Typing { loader: None });
        Self::compile_typed(source, registered.iter(), false, None, &(), typing)
    }

    /// Compiles host source, charging the work to `work`. In static mode,
    /// `loader` resolves the files the source requires.
    pub fn compile_metered(
        source: &str,
        registered: &BTreeMap<String, Registered>,
        work: &dyn crate::compilation::Work,
        static_types: Option<&crate::loading::Loader>,
    ) -> Result<Arc<Self>> {
        let typing = static_types.map(|loader| Typing {
            loader: Some(loader),
        });
        Self::compile_typed(source, registered.iter(), false, None, work, typing)
    }

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
        let typing = receiving.static_types.then_some(Typing {
            loader: Some(loader),
        });
        Self::compile_typed(
            source,
            receiving.program.hosts.iter().zip(&receiving.hosts),
            true,
            Some(origin),
            &crate::compilation::Meter(std::cell::RefCell::new(ctx)),
            typing,
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
        Self::compile_typed(source, registered, file, origin, work, None)
    }

    fn compile_typed<'a>(
        source: &str,
        registered: impl Iterator<Item = (&'a String, &'a Registered)> + Clone,
        file: bool,
        origin: Option<crate::loading::Origin>,
        work: &dyn crate::compilation::Work,
        typing: Option<Typing<'_>>,
    ) -> Result<Arc<Self>> {
        let static_types = typing.is_some();
        work.checkpoint()?;
        let mut names = Vec::new();
        for (name, _) in registered.clone() {
            work.bytes(name.len())?;
            names.push(name.clone());
        }
        let filename = origin.as_ref().map(crate::loading::Origin::filename);
        let parse_error =
            |error| crate::source::parse_error(source, filename.as_ref(), error, work);
        let mut program = if let Some(typing) = typing {
            let (parsed, tokens) =
                crate::syntax::parse_with_tokens(source, work).map_err(parse_error)?;
            let resolve = |path: &str| typing.loader.and_then(|loader| loader.source(path));
            let mut checked = crate::typing::check(&crate::typing::Input {
                source,
                parsed: &parsed,
                tokens: &tokens,
                hosts: registered.clone().collect(),
                file,
                modules: typing.loader.is_some().then_some(&resolve),
            });
            work.charge(usize::try_from(checked.steps).unwrap_or(usize::MAX))?;
            work.charge(tokens.len())?;
            crate::surface::add_to(&mut checked, source, &tokens);
            work.checkpoint()?;
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
            crate::bytecode::compile_parsed(source, parsed, names, file, work)
        } else if file {
            crate::bytecode::compile_file(source, names, work)
        } else {
            crate::bytecode::compile(source, names, work)
        }
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
                origin,
                exports,
                static_types,
            }
        }))
    }
}

/// How a static compilation resolves what it requires.
#[derive(Clone, Copy)]
struct Typing<'a> {
    loader: Option<&'a crate::loading::Loader>,
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
