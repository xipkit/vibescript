use super::*;
use crate::{budget::Charge, code::Code};
use std::{ops::Deref, sync::Arc};

pub(super) struct Program {
    pub code: Arc<Code>,
    pub index: usize,
    pub global_base: usize,
    _charge: Option<Charge>,
}

impl Deref for Program {
    type Target = crate::bytecode::Program;

    fn deref(&self) -> &Self::Target {
        &self.code.program
    }
}

pub(super) fn load(
    ctx: &mut CallContext,
    storage: &mut Storage,
    code: &Arc<Code>,
) -> Result<Arc<Program>> {
    for program in &storage.programs.data {
        ctx.charge(1)?;
        if Arc::ptr_eq(&program.code, code) {
            return Ok(program.clone());
        }
    }
    let global_base = storage.globals.data.len();
    let Some(end) = global_base.checked_add(code.program.globals.len()) else {
        return ctx.fail(ErrorKind::Memory, "allocation size overflow");
    };
    storage
        .programs
        .ensure(ctx, storage.programs.data.len() + 1)?;
    storage.globals.ensure(ctx, end)?;
    let charge = ctx.reserve(size_of::<Program>() + 2 * size_of::<usize>())?;
    Code::retain(ctx, code)?;
    let program = Arc::new(Program {
        code: code.clone(),
        index: storage.programs.data.len(),
        global_base,
        _charge: charge,
    });
    storage.globals.data.resize(end, None);
    storage.programs.data.push(program.clone());
    Ok(program)
}
