use super::{
    Assertion, Class, Part,
    parse::{self, Kind, UNBOUNDED},
};
use crate::{CallContext, Error, ErrorKind, Result, Value, budget::Buffer};

#[derive(Clone, Copy, Debug)]
pub(super) enum Op {
    Fail,
    Match,
    Rune(u32, bool),
    Class(Class),
    Any(bool),
    Assert(Assertion),
    Split,
    Save(usize),
}

#[derive(Clone, Copy, Debug)]
pub(super) struct Instruction {
    pub op: Op,
    pub next: usize,
    pub alternate: usize,
}

#[derive(Debug)]
pub(super) struct Program {
    pub instructions: Buffer<Instruction>,
    pub parts: Buffer<Part>,
    pub names: Buffer<(usize, usize)>,
    pub source: Value,
    pub start: usize,
    pub minimum: usize,
}

#[derive(Clone, Copy)]
pub(super) struct View<'a> {
    pub instructions: &'a [Instruction],
    pub parts: &'a [Part],
    pub names: &'a [(usize, usize)],
    pub source: &'a [u8],
    pub start: usize,
    pub minimum: usize,
}

enum Build {
    Node(usize, usize),
    Concat(usize),
    AlternateRight(usize, usize),
    AlternateJoin(usize),
    Capture(usize),
    Loop {
        branch: usize,
        next: usize,
        greedy: bool,
        optional: bool,
        nullable: bool,
    },
    Copies {
        child: usize,
        mandatory: usize,
        optional: usize,
        greedy: bool,
    },
    Optional(usize, bool),
}

impl Program {
    pub fn view(&self) -> View<'_> {
        View {
            instructions: &self.instructions.data,
            parts: &self.parts.data,
            names: &self.names.data,
            source: self.source.as_bytes().unwrap(),
            start: self.start,
            minimum: self.minimum,
        }
    }

    /// Compiles a pattern for `method`, the operation named in its errors.
    pub fn compile(ctx: &mut CallContext, source: Value, method: &str) -> Result<Self> {
        Self::compile_limit(ctx, source, super::MAX_PATTERN, method)
    }

    pub fn compile_limit(
        ctx: &mut CallContext,
        source: Value,
        limit: usize,
        method: &str,
    ) -> Result<Self> {
        let bytes = source.require_bytes()?;
        if bytes.len() > limit {
            return super::pattern_limit(ctx, method);
        }
        let source = ctx.import(&source)?;
        let bytes = source.require_bytes()?;
        let parsed = parse::parse(ctx, bytes).map_err(|error| {
            if error.kind == ErrorKind::Argument {
                Error::new(
                    ErrorKind::Argument,
                    format!("{method} invalid regex: {}", error.message),
                )
            } else {
                error
            }
        })?;
        let capacity = parsed.nodes.data[parsed.root].cost.saturating_add(2);
        if capacity > super::MAX_INSTRUCTIONS {
            // Go's estimate saturates one instruction past the limit, and it
            // sizes each instruction at 64 bytes.
            let limit = super::MAX_INSTRUCTIONS;
            return ctx.guard(
                ErrorKind::Memory,
                &format!(
                    "{method} invalid regex: regex compiles to {} instructions, exceeding limit {limit} (about {} MiB)",
                    limit + 1,
                    (limit * 64) >> 20
                ),
            );
        }
        let mut program = Self {
            instructions: Buffer::with_capacity(ctx, capacity)?,
            parts: parsed.parts,
            names: parsed.names,
            source,
            start: 0,
            minimum: parsed.nodes.data[parsed.root].minimum,
        };
        program.emit(ctx, Op::Fail, 0, 0)?;
        program.emit(ctx, Op::Match, 0, 0)?;
        let mut tasks = Buffer::empty();
        tasks.push(ctx, Build::Node(parsed.root, 1))?;
        let mut start = 1;
        while let Some(task) = tasks.data.pop() {
            ctx.charge(1)?;
            match task {
                Build::Node(id, next) => match parsed.nodes.data[id].kind {
                    Kind::Empty => start = next,
                    Kind::Rune(rune, fold) => {
                        start = program.emit(ctx, Op::Rune(rune, fold), next, 0)?
                    }
                    Kind::Class(class) => start = program.emit(ctx, Op::Class(class), next, 0)?,
                    Kind::Any(newline) => start = program.emit(ctx, Op::Any(newline), next, 0)?,
                    Kind::Assert(assertion) => {
                        start = program.emit(ctx, Op::Assert(assertion), next, 0)?
                    }
                    Kind::Concat(left, right) => {
                        tasks.push(ctx, Build::Concat(left))?;
                        tasks.push(ctx, Build::Node(right, next))?;
                    }
                    Kind::Alt(left, right) => {
                        tasks.push(ctx, Build::AlternateRight(right, next))?;
                        tasks.push(ctx, Build::Node(left, next))?;
                    }
                    Kind::Capture(slot, child) => {
                        let end = program.emit(ctx, Op::Save(slot * 2 + 1), next, 0)?;
                        tasks.push(ctx, Build::Capture(slot))?;
                        tasks.push(ctx, Build::Node(child, end))?;
                    }
                    Kind::Repeat {
                        child,
                        min,
                        max,
                        greedy,
                    } if max == UNBOUNDED => {
                        let branch = program.emit(ctx, Op::Split, 0, 0)?;
                        tasks.push(
                            ctx,
                            Build::Copies {
                                child,
                                mandatory: min.saturating_sub(1),
                                optional: 0,
                                greedy,
                            },
                        )?;
                        tasks.push(
                            ctx,
                            Build::Loop {
                                branch,
                                next,
                                greedy,
                                optional: min == 0,
                                nullable: parsed.nodes.data[child].nullable,
                            },
                        )?;
                        tasks.push(ctx, Build::Node(child, branch))?;
                    }
                    Kind::Repeat {
                        child,
                        min,
                        max,
                        greedy,
                    } => {
                        start = next;
                        tasks.push(
                            ctx,
                            Build::Copies {
                                child,
                                mandatory: min,
                                optional: max - min,
                                greedy,
                            },
                        )?;
                    }
                },
                Build::Concat(left) => tasks.push(ctx, Build::Node(left, start))?,
                Build::AlternateRight(right, next) => {
                    tasks.push(ctx, Build::AlternateJoin(start))?;
                    tasks.push(ctx, Build::Node(right, next))?;
                }
                Build::AlternateJoin(left) => start = program.emit(ctx, Op::Split, left, start)?,
                Build::Capture(slot) => start = program.emit(ctx, Op::Save(slot * 2), start, 0)?,
                Build::Loop {
                    branch,
                    next,
                    greedy,
                    optional,
                    nullable,
                } => {
                    program.instructions.data[branch].next = if greedy { start } else { next };
                    program.instructions.data[branch].alternate = if greedy { next } else { start };
                    if optional {
                        start = if nullable {
                            program.optional(ctx, start, next, greedy)?
                        } else {
                            branch
                        };
                    }
                }
                Build::Copies {
                    child,
                    mandatory,
                    optional,
                    greedy,
                } => {
                    if optional > 0 {
                        tasks.push(
                            ctx,
                            Build::Copies {
                                child,
                                mandatory,
                                optional: optional - 1,
                                greedy,
                            },
                        )?;
                        tasks.push(ctx, Build::Optional(start, greedy))?;
                        tasks.push(ctx, Build::Node(child, start))?;
                    } else if mandatory > 0 {
                        tasks.push(
                            ctx,
                            Build::Copies {
                                child,
                                mandatory: mandatory - 1,
                                optional: 0,
                                greedy,
                            },
                        )?;
                        tasks.push(ctx, Build::Node(child, start))?;
                    }
                }
                Build::Optional(next, greedy) => {
                    start = program.optional(ctx, start, next, greedy)?
                }
            }
        }
        program.start = start;
        Ok(program)
    }

    fn emit(
        &mut self,
        ctx: &mut CallContext,
        op: Op,
        next: usize,
        alternate: usize,
    ) -> Result<usize> {
        ctx.charge(1)?;
        let index = self.instructions.data.len();
        assert!(index < self.instructions.data.capacity());
        self.instructions.data.push(Instruction {
            op,
            next,
            alternate,
        });
        Ok(index)
    }

    fn optional(
        &mut self,
        ctx: &mut CallContext,
        child: usize,
        next: usize,
        greedy: bool,
    ) -> Result<usize> {
        self.emit(
            ctx,
            Op::Split,
            if greedy { child } else { next },
            if greedy { next } else { child },
        )
    }
}
