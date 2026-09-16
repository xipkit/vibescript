use crate::{CallContext, HostCallback, Result, bytecode::Program};
use std::{collections::BTreeMap, fmt, sync::Arc};

pub(crate) struct Code {
    pub program: Program,
    pub hosts: Vec<HostCallback>,
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

    pub fn compile(source: &str, registered: &BTreeMap<String, HostCallback>) -> Result<Arc<Self>> {
        Self::compile_mode(source, registered, false)
    }

    pub fn compile_file(
        source: &str,
        registered: &BTreeMap<String, HostCallback>,
    ) -> Result<Arc<Self>> {
        Self::compile_mode(source, registered, true)
    }

    fn compile_mode(
        source: &str,
        registered: &BTreeMap<String, HostCallback>,
        file: bool,
    ) -> Result<Arc<Self>> {
        let names = registered.keys().cloned().collect();
        let mut program = if file {
            crate::bytecode::compile_file(source, names)
        } else {
            crate::bytecode::compile(source, names)
        }
        .map_err(|error| crate::source::parse_error(source, error))?;
        let hosts = program
            .hosts
            .iter()
            .map(|name| registered[name].clone())
            .collect();
        Ok(Arc::new_cyclic(|owner| {
            program.owner = owner.clone();
            for definition in &program.namespaces {
                assert!(definition.owner.set(owner.clone()).is_ok());
            }
            Self { program, hosts }
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
