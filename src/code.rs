use crate::{CallContext, Result, bytecode::Program, capability::Registered};
use std::{collections::BTreeMap, fmt, sync::Arc};

pub(crate) struct Code {
    pub program: Program,
    pub hosts: Vec<Registered>,
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

    pub fn compile(source: &str, registered: &BTreeMap<String, Registered>) -> Result<Arc<Self>> {
        Self::compile_mode(source, registered.iter(), false, None, &())
    }

    /// Compiles host source, charging the work to `work`.
    pub fn compile_metered(
        source: &str,
        registered: &BTreeMap<String, Registered>,
        work: &dyn crate::compilation::Work,
    ) -> Result<Arc<Self>> {
        Self::compile_mode(source, registered.iter(), false, None, work)
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
    ) -> Result<Arc<Self>> {
        Self::compile_mode(
            source,
            receiving.program.hosts.iter().zip(&receiving.hosts),
            true,
            Some(origin),
            &crate::compilation::Meter(std::cell::RefCell::new(ctx)),
        )
    }

    fn compile_mode<'a>(
        source: &str,
        registered: impl Iterator<Item = (&'a String, &'a Registered)> + Clone,
        file: bool,
        origin: Option<crate::loading::Origin>,
        work: &dyn crate::compilation::Work,
    ) -> Result<Arc<Self>> {
        work.checkpoint()?;
        let mut names = Vec::new();
        for (name, _) in registered.clone() {
            work.bytes(name.len())?;
            names.push(name.clone());
        }
        let filename = origin.as_ref().map(crate::loading::Origin::filename);
        let mut program = if file {
            crate::bytecode::compile_file(source, names, work)
        } else {
            crate::bytecode::compile(source, names, work)
        }
        .map_err(|error| crate::source::parse_error(source, filename.as_ref(), error, work))?;
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
