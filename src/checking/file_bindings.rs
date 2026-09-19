use crate::{
    CallContext, Result, Value,
    budget::Buffer,
    bytecode::{Op, Program},
};

pub(super) struct Layout {
    pub names: Buffer<Value>,
}

impl Layout {
    pub fn new(ctx: &mut CallContext, program: &Program) -> Result<Self> {
        let mut layout = Self {
            names: Buffer::empty(),
        };
        if !program.file {
            return Ok(layout);
        }
        for name in program
            .members
            .iter()
            .chain(
                program
                    .functions
                    .iter()
                    .flat_map(|function| &function.local_names),
            )
            .map(String::as_str)
            .chain(program.globals.iter().map(|(global, _)| global.name()))
            .chain((0..program.declarations.len()).map(|index| declaration_name(program, index)))
        {
            ctx.work_bytes(name.len())?;
            if !name.starts_with('\0') && layout.index(ctx, name)?.is_none() {
                let name = ctx.bytes(name.as_bytes())?;
                layout.names.push(ctx, name)?;
            }
        }
        Ok(layout)
    }

    pub fn index(&self, ctx: &mut CallContext, name: &str) -> Result<Option<usize>> {
        for (index, candidate) in self.names.data.iter().enumerate() {
            let candidate = candidate.as_bytes().unwrap();
            ctx.work_bytes(candidate.len().max(name.len()))?;
            if candidate == name.as_bytes() {
                return Ok(Some(index));
            }
        }
        Ok(None)
    }
}

pub(super) fn local(op: Op) -> Option<usize> {
    match op {
        Op::Load(slot)
        | Op::LoadOptional(slot, _)
        | Op::ReceiverBound(slot, _)
        | Op::Declare(slot)
        | Op::Store(slot)
        | Op::AddStore(slot)
        | Op::AddressLocal(slot)
        | Op::AddressBound(slot, _) => Some(slot),
        _ => None,
    }
}

/// Binding-presence alternatives must finish their instruction before rejoining.
pub(super) fn branches(op: Op) -> bool {
    local(op).is_some()
        || matches!(
            op,
            Op::FileValue(..)
                | Op::FileAddress(..)
                | Op::RootAddress(..)
                | Op::RootCall(..)
                | Op::Global(_)
                | Op::GlobalReceiver(..)
                | Op::AddressGlobal(_)
                | Op::ResolveGlobalCall(_)
                | Op::Declaration(_)
                | Op::ResolveCall(..)
                | Op::CallName(..)
                | Op::Unbound(_)
                | Op::AutoCall(_)
                | Op::HostValue(_)
        )
}

pub(super) fn declaration_name(program: &Program, index: usize) -> &str {
    match &program.declarations[index].0 {
        crate::value::Kind::Namespace(value) => &value.definition.name,
        crate::value::Kind::Enum(value) => &value.definition.name,
        _ => unreachable!(),
    }
}
