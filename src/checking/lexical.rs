use crate::{
    CallContext, ErrorKind, Result,
    budget::Buffer,
    bytecode::{Capture, Op, Program},
};

struct Layout {
    // Slots after compiled locals relay bindings used only by descendant blocks.
    additional: Buffer<Capture>,
    shadows: Buffer<bool>,
    parent: Option<usize>,
}

pub(super) struct Layouts {
    functions: Buffer<Layout>,
}

impl Layouts {
    pub fn new(ctx: &mut CallContext, program: &Program) -> Result<Self> {
        ctx.checkpoint()?;
        let mut functions = Buffer::empty();
        for function in &program.functions {
            ctx.charge(1)?;
            let mut shadows = Buffer::with_capacity(ctx, function.locals)?;
            ctx.charge(function.locals as u64)?;
            shadows.data.resize(function.locals, false);
            for op in &function.code {
                ctx.charge(1)?;
                if let Op::Shadow(slot) = *op {
                    shadows.data[slot] = true;
                }
            }
            functions.push(
                ctx,
                Layout {
                    additional: Buffer::empty(),
                    shadows,
                    parent: None,
                },
            )?;
        }
        for (index, function) in program.functions.iter().enumerate() {
            for op in &function.code {
                ctx.charge(1)?;
                if let Op::Attach(child) = *op {
                    let parent = &mut functions.data[child].parent;
                    assert!(parent.is_none() || *parent == Some(index));
                    *parent = Some(index);
                }
            }
        }
        let mut layouts = Self { functions };
        for (function, body) in program.functions.iter().enumerate() {
            for source in &body.captures {
                ctx.charge(1)?;
                let Some(mut source) = *source else {
                    continue;
                };
                let mut child = function;
                while source.depth > 0 {
                    ctx.charge(1)?;
                    let Some(parent) = layouts.functions.data[child].parent else {
                        break;
                    };
                    source.depth -= 1;
                    if layouts.find(ctx, program, parent, source)?.is_some() {
                        break;
                    }
                    layouts.functions.data[parent]
                        .additional
                        .push(ctx, source)?;
                    child = parent;
                }
            }
        }
        Ok(layouts)
    }

    pub fn locals(
        &self,
        ctx: &mut CallContext,
        program: &Program,
        function: usize,
    ) -> Result<usize> {
        ctx.charge(1)?;
        match program.functions[function]
            .locals
            .checked_add(self.functions.data[function].additional.data.len())
        {
            Some(len) => Ok(len),
            None => ctx.fail(ErrorKind::Memory, "checker lexical layout size overflow"),
        }
    }

    pub fn capture(
        &self,
        ctx: &mut CallContext,
        program: &Program,
        function: usize,
        slot: usize,
    ) -> Result<Option<Capture>> {
        ctx.charge(1)?;
        let body = &program.functions[function];
        if slot < body.locals {
            return Ok(body.captures.get(slot).copied().flatten());
        }
        Ok(Some(
            self.functions.data[function].additional.data[slot - body.locals],
        ))
    }

    pub fn find(
        &self,
        ctx: &mut CallContext,
        program: &Program,
        function: usize,
        source: Capture,
    ) -> Result<Option<usize>> {
        ctx.checkpoint()?;
        let layout = &self.functions.data[function];
        for (slot, capture) in program.functions[function].captures.iter().enumerate() {
            ctx.charge(1)?;
            if capture
                .is_some_and(|capture| (capture.depth, capture.slot) == (source.depth, source.slot))
                && !layout.shadows.data[slot]
            {
                return Ok(Some(slot));
            }
        }
        for (index, capture) in layout.additional.data.iter().enumerate() {
            ctx.charge(1)?;
            if (capture.depth, capture.slot) == (source.depth, source.slot) {
                return Ok(Some(program.functions[function].locals + index));
            }
        }
        Ok(None)
    }
}
