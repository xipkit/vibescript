use super::*;
use crate::{Value, code::Code};
use std::sync::Arc;

enum SourceProgram<'a> {
    Borrowed(&'a Program),
    Owned(Arc<Code>),
}

pub(in crate::checking::flow) struct Namespace<'a> {
    pub source: SourceId,
    pub owner: usize,
    pub index: usize,
    pub root: usize,
    program: SourceProgram<'a>,
}

impl Namespace<'_> {
    /// Returns the declaration's defining program.
    pub fn program(&self) -> &Program {
        match &self.program {
            SourceProgram::Borrowed(program) => program,
            SourceProgram::Owned(code) => &code.program,
        }
    }

    /// Reconstructs the namespace value with its defining source identity.
    pub fn value(&self, ctx: &mut CallContext, facts: &mut Facts) -> Result<Fact> {
        namespaces::value(ctx, facts, self.program(), self.owner, self.index)
    }

    /// Matches a lexical namespace without confusing equal indexes from different files.
    pub fn local(&self, source: SourceId, module: Option<usize>) -> bool {
        self.source == source && module == Some(self.index)
    }
}

impl<'a> Walker<'a> {
    /// Keeps declaration metadata and storage tied to the same prepared source.
    pub(in crate::checking::flow) fn namespace(
        &mut self,
        state: &State,
        receiver: Fact,
    ) -> Result<Option<Namespace<'a>>> {
        self.ctx.charge(1)?;
        let ty = match self.facts.node(receiver) {
            Node::TypeValue(ty) | Node::Instance { class: ty, .. } => *ty,
            _ => return Ok(None),
        };
        let Node::Nominal {
            identity: NominalId::Binding(owner, declaration),
            ..
        } = *self.facts.node(ty)
        else {
            return Ok(None);
        };
        let (source, program) = if owner == self.layouts.source_owner {
            (self.source, SourceProgram::Borrowed(self.program))
        } else {
            let source = self.facts.source_id(self.ctx, owner)?;
            let Some(code) = self.facts.source_code(self.ctx, source)? else {
                return Ok(None);
            };
            (source, SourceProgram::Owned(code))
        };
        let Some(slots) = state.global_layout.find(self.ctx, source)? else {
            return Ok(None);
        };
        let metadata = match &program {
            SourceProgram::Borrowed(program) => *program,
            SourceProgram::Owned(code) => &code.program,
        };
        let Some(Value(Kind::Namespace(namespace))) = metadata.declarations.get(declaration) else {
            return Ok(None);
        };
        let index = namespace.definition.index;
        Ok(Some(Namespace {
            source,
            owner,
            index,
            root: state.global_base + slots.namespace(index),
            program,
        }))
    }
}
