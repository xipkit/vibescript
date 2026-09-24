use crate::{
    CallContext, ErrorKind, Result,
    budget::Buffer,
    bytecode::{Capture, Invocation, Op, Program},
    types::{self, Type, TypeKind},
};

#[derive(Clone, Copy)]
pub(super) struct TypeSource {
    pub depth: usize,
    pub function: usize,
    pub name: usize,
    pub capture: usize,
}

#[derive(Clone, Copy)]
struct Bypass {
    start: usize,
    end: usize,
    slot: usize,
}

struct Layout {
    // Slots after compiled locals relay bindings used only by descendant blocks.
    additional: Buffer<Capture>,
    shadows: Buffer<bool>,
    // Assignment values whose same-name calls skip the assigned binding.
    bypasses: Buffer<Bypass>,
    parent: Option<usize>,
    // The parent's instruction that attaches this block.
    attach: usize,
    forwarding: bool,
    // The function or a nested block yields to, or tests for, the function's incoming block.
    observes_block: bool,
    types: Buffer<TypeSource>,
}

pub(super) struct Layouts {
    pub source_owner: usize,
    pub files: super::file_bindings::Layout,
    functions: Buffer<Layout>,
    named_annotations: Buffer<bool>,
    properties: Buffer<Buffer<Property>>,
}

/// A namespace accessor method and the type of the property it reads or writes.
#[derive(Clone, Copy, Debug)]
pub(super) struct Property {
    /// The accessor's position among the namespace's instance methods.
    pub method: usize,
    pub ty: Option<usize>,
}

impl Layouts {
    pub fn new(ctx: &mut CallContext, program: &Program, source_owner: usize) -> Result<Self> {
        ctx.checkpoint()?;
        let mut functions = Buffer::empty();
        for function in &program.functions {
            ctx.charge(1)?;
            let mut shadows = Buffer::with_capacity(ctx, function.locals)?;
            let mut forwarding = false;
            let mut observes_block = false;
            let mut bypasses = Buffer::empty();
            let mut open = Buffer::empty();
            ctx.charge(function.locals as u64)?;
            shadows.data.resize(function.locals, false);
            for (pc, op) in function.code.iter().enumerate() {
                ctx.charge(1)?;
                match *op {
                    Op::Shadow(slot) => shadows.data[slot] = true,
                    Op::Bypass(slot) => {
                        open.push(ctx, bypasses.data.len())?;
                        let end = function.code.len();
                        bypasses.push(
                            ctx,
                            Bypass {
                                start: pc,
                                end,
                                slot,
                            },
                        )?;
                    }
                    Op::BypassEnd(count) => {
                        for _ in 0..count {
                            ctx.charge(1)?;
                            let index = open.data.pop().unwrap();
                            bypasses.data[index].end = pc;
                        }
                    }
                    _ => (),
                }
                forwarding |= matches!(op, Op::Yield(_));
                observes_block |= matches!(op, Op::Yield(_) | Op::BlockGiven(..) | Op::CheckBlock);
            }
            functions.push(
                ctx,
                Layout {
                    additional: Buffer::empty(),
                    shadows,
                    bypasses,
                    parent: None,
                    attach: 0,
                    forwarding,
                    observes_block,
                    types: Buffer::empty(),
                },
            )?;
        }
        for (index, function) in program.functions.iter().enumerate() {
            for (pc, op) in function.code.iter().enumerate() {
                ctx.charge(1)?;
                if let Op::Attach(child) = *op {
                    let layout = &mut functions.data[child];
                    assert!(layout.parent.is_none() || layout.parent == Some(index));
                    layout.parent = Some(index);
                    layout.attach = pc;
                }
            }
        }
        let mut named_annotations = Buffer::empty();
        for ty in &program.types {
            let named = super::type_bindings::named_annotation(ctx, ty)?;
            named_annotations.push(ctx, named)?;
        }
        // Accessors are few beside the other methods, and every declaration check reads them.
        let mut properties = Buffer::with_capacity(ctx, program.namespaces.len())?;
        for (module, namespace) in program.namespaces.iter().enumerate() {
            let mut accessors = Buffer::empty();
            for (method, definition) in namespace.instance_methods.iter().enumerate() {
                ctx.charge(1)?;
                if let Some((name, _)) = &program.functions[definition.function].accessor {
                    let ty = super::namespaces::property_type(ctx, program, module, name)?;
                    accessors.push(ctx, Property { method, ty })?;
                }
            }
            properties.push(ctx, accessors)?;
        }
        let mut layouts = Self {
            source_owner,
            files: super::file_bindings::Layout::new(ctx, program)?,
            functions,
            named_annotations,
            properties,
        };
        for (function, body) in program.functions.iter().enumerate() {
            ctx.charge(1)?;
            if layouts.functions.data[function].forwarding {
                let mut child = function;
                while let Some(parent) = layouts.functions.data[child].parent {
                    ctx.charge(1)?;
                    if layouts.functions.data[parent].forwarding {
                        break;
                    }
                    layouts.functions.data[parent].forwarding = true;
                    child = parent;
                }
            }
            if layouts.functions.data[function].observes_block {
                let mut child = function;
                while let Some(parent) = layouts.functions.data[child].parent {
                    ctx.charge(1)?;
                    if layouts.functions.data[parent].observes_block {
                        break;
                    }
                    layouts.functions.data[parent].observes_block = true;
                    child = parent;
                }
            }
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
        layouts.type_captures(ctx, program)?;
        Ok(layouts)
    }

    fn type_captures(&mut self, ctx: &mut CallContext, program: &Program) -> Result<()> {
        for (function, body) in program.functions.iter().enumerate() {
            ctx.charge(1)?;
            if self.functions.data[function].parent.is_none() {
                continue;
            }
            let mut pending: Buffer<&Type> = Buffer::empty();
            for parameter in &body.params {
                ctx.charge(1)?;
                if let Some(ty) = parameter.ty {
                    pending.push(ctx, &program.types[ty])?;
                }
            }
            if let Some(ty) = body.return_type {
                pending.push(ctx, &program.types[ty])?;
            }
            for op in &body.code {
                ctx.charge(1)?;
                if let Op::Normalize(ty, _) | Op::Check(ty, _) = *op {
                    pending.push(ctx, &program.types[ty])?;
                }
            }
            for op in &body.code {
                ctx.charge(1)?;
                let name = match op {
                    Op::Method(site, _)
                    | Op::AddressMember(site)
                    | Op::CallMember(site)
                    | Op::Mutate(site, _)
                    | Op::PrepareMember(site, _)
                    | Op::Invoke(Invocation::Member(site, _)) => Some(site.name),
                    Op::ResolveCall(_, name, _)
                    | Op::CallName(_, name)
                    | Op::Invoke(Invocation::ImplicitMember(_, name)) => Some(*name),
                    _ => None,
                };
                if name.is_some_and(|name| {
                    matches!(
                        program.members[name].as_str(),
                        "is_type?" | "send" | "public_send" | "reduce" | "inject"
                    )
                }) {
                    self.capture_type(ctx, program, function, None)?;
                    break;
                }
            }
            while let Some(ty) = pending.data.pop() {
                ctx.charge(1)?;
                match &ty.kind {
                    TypeKind::Array(Some(item)) => pending.push(ctx, item)?,
                    TypeKind::Hash(Some(pair)) => {
                        pending.push(ctx, &pair.0)?;
                        pending.push(ctx, &pair.1)?;
                    }
                    TypeKind::Shape(fields, _) => {
                        for field in fields {
                            ctx.charge(1)?;
                            pending.push(ctx, &field.ty)?;
                        }
                    }
                    TypeKind::Union(options) | TypeKind::Tuple(options) => {
                        for option in options {
                            ctx.charge(1)?;
                            pending.push(ctx, option)?;
                        }
                    }
                    TypeKind::Named => self.capture_type(ctx, program, function, Some(&ty.name))?,
                    _ => (),
                }
            }
            let types = &mut self.functions.data[function].types.data;
            ctx.charge(
                types
                    .len()
                    .saturating_mul(types.len().max(1).ilog2() as usize + 1) as u64,
            )?;
            types.sort_unstable_by_key(|source| (source.depth, source.name));
        }
        Ok(())
    }

    fn capture_type(
        &mut self,
        ctx: &mut CallContext,
        program: &Program,
        function: usize,
        name: Option<&str>,
    ) -> Result<()> {
        let filter = name.map(|name| {
            name.split_once('.')
                .map_or((name, false), |(name, _)| (name, true))
        });
        if let Some((name, _)) = filter {
            ctx.work_bytes(name.len())?;
        }
        let mut parent = self.functions.data[function].parent;
        let mut depth = 0;
        while let Some(owner) = parent {
            ctx.charge(1)?;
            let body = &program.functions[owner];
            for (slot, candidate) in body.local_names.iter().enumerate() {
                ctx.charge(1)?;
                if let Some((name, qualified)) = filter {
                    if !types::binding_name_matches(
                        ctx,
                        candidate.as_bytes(),
                        name.as_bytes(),
                        !qualified,
                    )? {
                        continue;
                    }
                }
                let sources = &self.functions.data[function].types.data;
                ctx.charge(sources.len() as u64)?;
                if sources
                    .iter()
                    .any(|source| source.function == owner && source.name == slot)
                {
                    continue;
                }
                let source = Capture { depth, slot };
                let capture = self.relay_type(ctx, program, function, source)?;
                self.functions.data[function].types.push(
                    ctx,
                    TypeSource {
                        depth,
                        function: owner,
                        name: slot,
                        capture,
                    },
                )?;
            }
            depth += 1;
            parent = self.functions.data[owner].parent;
        }
        Ok(())
    }

    fn relay_type(
        &mut self,
        ctx: &mut CallContext,
        program: &Program,
        function: usize,
        mut source: Capture,
    ) -> Result<usize> {
        let mut child = function;
        let mut first = None;
        loop {
            ctx.charge(1)?;
            if let Some(slot) = self.find(ctx, program, child, source)? {
                return Ok(first.unwrap_or(slot));
            }
            let slot = self.locals(ctx, program, child)?;
            first.get_or_insert(slot);
            self.functions.data[child].additional.push(ctx, source)?;
            if source.depth == 0 {
                return Ok(first.unwrap());
            }
            source.depth -= 1;
            child = self.functions.data[child].parent.unwrap();
        }
    }

    /// Reports whether an annotation requires live name resolution.
    /// Returns a namespace's accessor methods in declaration order.
    pub fn properties(&self, ctx: &mut CallContext, module: usize) -> Result<&[Property]> {
        ctx.charge(1)?;
        Ok(&self.properties.data[module].data)
    }

    pub fn named_annotation(&self, ctx: &mut CallContext, ty: usize) -> Result<bool> {
        ctx.charge(1)?;
        Ok(self.named_annotations.data[ty])
    }

    /// Returns captured candidate bindings in lexical scope order.
    pub fn type_sources(&self, ctx: &mut CallContext, function: usize) -> Result<&[TypeSource]> {
        ctx.checkpoint()?;
        Ok(&self.functions.data[function].types.data)
    }

    /// Finds a binding owner's position in a block's enclosing lexical chain.
    pub fn depth(
        &self,
        ctx: &mut CallContext,
        function: usize,
        owner: usize,
    ) -> Result<Option<usize>> {
        let mut parent = self.functions.data[function].parent;
        let mut depth = 0;
        while let Some(function) = parent {
            ctx.charge(1)?;
            if function == owner {
                return Ok(Some(depth));
            }
            depth += 1;
            parent = self.functions.data[function].parent;
        }
        Ok(None)
    }

    /// Reports whether a same-name call at `pc` skips `slot`, because an assignment to
    /// that binding is evaluating the value that contains the call.
    pub fn bypassed(
        &self,
        ctx: &mut CallContext,
        function: usize,
        pc: usize,
        slot: usize,
    ) -> Result<bool> {
        let bypasses = &self.functions.data[function].bypasses.data;
        ctx.charge(bypasses.len() as u64 + 1)?;
        Ok(bypasses
            .iter()
            .any(|bypass| bypass.slot == slot && bypass.start < pc && pc < bypass.end))
    }

    /// Returns a block's enclosing function and the instruction there that attaches it.
    pub fn attachment(
        &self,
        ctx: &mut CallContext,
        function: usize,
    ) -> Result<Option<(usize, usize)>> {
        ctx.charge(1)?;
        let layout = &self.functions.data[function];
        Ok(layout.parent.map(|parent| (parent, layout.attach)))
    }

    pub fn forwarding(&self, ctx: &mut CallContext, function: usize) -> Result<bool> {
        ctx.charge(1)?;
        Ok(self.functions.data[function].forwarding)
    }

    /// Reports whether a function or one of its nested blocks can observe the function's
    /// incoming block through `yield`, `block_given?` or a required-block check.
    pub fn observes_block(&self, ctx: &mut CallContext, function: usize) -> Result<bool> {
        ctx.charge(1)?;
        Ok(self.functions.data[function].observes_block)
    }

    pub fn initializer_block(
        &self,
        ctx: &mut CallContext,
        program: &Program,
        mut function: usize,
    ) -> Result<bool> {
        while program.functions[function].name == "<block>" {
            ctx.charge(1)?;
            let Some(parent) = self.functions.data[function].parent else {
                break;
            };
            if program.functions[parent].initializer {
                return Ok(true);
            }
            function = parent;
        }
        Ok(false)
    }

    pub fn return_home(
        &self,
        ctx: &mut CallContext,
        program: &Program,
        mut function: usize,
    ) -> Result<bool> {
        loop {
            ctx.charge(1)?;
            let Some(parent) = self.functions.data[function].parent else {
                return Ok(function != 0 && !program.functions[function].initializer);
            };
            function = parent;
        }
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
