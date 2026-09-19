#[cfg(test)]
use super::{
    arguments::Input,
    calls::{self, Analysis},
};
use super::{
    calls::{Host, Target, World},
    facts::{Atom, Fact, Facts},
};
use crate::{CallContext, CallOptions, Result, Script, Value, budget::Buffer, code::Code};
use std::sync::Arc;

#[derive(Debug)]
pub(super) enum Incomplete {
    Capability(Value),
}

/// A metadata snapshot for analysis; this does not bind grants or execute initializers.
pub(super) struct Environment {
    code: Arc<Code>,
    loader: Arc<crate::loading::Loader>,
    owner: usize,
    contracts: Buffer<Fact>,
    hosts: Buffer<Host>,
    values: Buffer<Value>,
    globals: Buffer<(Value, Target)>,
    pub incomplete: Buffer<Incomplete>,
}

impl Environment {
    /// Reads compiled registrations and supplied values without invoking host code.
    pub fn new(
        ctx: &mut CallContext,
        facts: &mut Facts,
        script: &Script,
        options: &CallOptions,
    ) -> Result<Self> {
        ctx.checkpoint()?;
        if script.inner.strict_effects {
            crate::globals::validate(ctx, &options.globals)?;
        }
        let code = &script.inner.code;
        let owner = facts.source_owner(ctx, code, None)?;
        let mut result = Self::module(ctx, facts, code, owner, &script.inner.loader)?;
        for capability in &options.capabilities {
            let name = ctx.bytes(capability.name.as_bytes())?;
            result
                .incomplete
                .push(ctx, Incomplete::Capability(name.clone()))?;
            result.bind(ctx, name, Target::Value(Atom::Unknown.fact()))?;
        }
        for (name, value) in &options.globals {
            let name = ctx.bytes(name.as_bytes())?;
            let index = result.values.data.len();
            result.values.push(ctx, value.clone())?;
            result.bind(ctx, name, Target::Deferred(index))?;
        }
        Ok(result)
    }

    /// Prepares loaded source metadata without constructing or executing a runtime environment.
    pub fn module(
        ctx: &mut CallContext,
        facts: &mut Facts,
        code: &Arc<Code>,
        owner: usize,
        loader: &Arc<crate::loading::Loader>,
    ) -> Result<Self> {
        ctx.checkpoint()?;
        let mut result = Self {
            code: code.clone(),
            loader: loader.clone(),
            owner,
            contracts: Buffer::empty(),
            hosts: Buffer::empty(),
            values: Buffer::empty(),
            globals: Buffer::empty(),
            incomplete: Buffer::empty(),
        };
        for registered in &code.hosts {
            let host = Host::registered(ctx, facts, registered)?;
            result.hosts.push(ctx, host)?;
        }
        for ty in &code.program.types {
            let contract = facts.annotation(ctx, ty, |_, _| Ok(None))?;
            result.contracts.push(ctx, contract)?;
        }
        Ok(result)
    }

    fn bind(&mut self, ctx: &mut CallContext, name: Value, value: Target) -> Result<()> {
        for (key, target) in &mut self.globals.data {
            if super::facts::same_bytes(ctx, key, &name)? {
                *target = value;
                return Ok(());
            }
        }
        self.globals.push(ctx, (name, value))
    }

    /// Exposes an immutable view for the interprocedural solver.
    pub fn world(&self) -> World<'_> {
        World {
            loader: Some(&self.loader),
            program: &self.code.program,
            source_owner: self.owner,
            contracts: &self.contracts.data,
            hosts: &self.hosts.data,
            globals: &self.globals.data,
            inputs: &self.values.data,
        }
    }

    /// Analyzes a prepared entry only when its initialization and input scope are modeled.
    #[cfg(test)]
    pub fn analyze(
        &self,
        ctx: &mut CallContext,
        facts: &mut Facts,
        function: usize,
        inputs: &[Input],
    ) -> Result<Analysis> {
        ctx.checkpoint()?;
        if !self.incomplete.data.is_empty() {
            let mut incomplete = Buffer::empty();
            let source = facts.source_id(ctx, self.owner)?;
            incomplete.push(
                ctx,
                super::calls::Location {
                    source,
                    function,
                    pc: 0,
                },
            )?;
            return Ok(Analysis {
                returns: Atom::Unknown.fact(),
                throws: u8::MAX,
                issues: Buffer::empty(),
                incomplete,
                contexts: 0,
            });
        }
        calls::analyze(ctx, facts, self.world(), function, inputs)
    }
}
