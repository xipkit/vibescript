use super::facts::{Atom, Fact, Facts, Node};
use crate::{CallContext, Result, budget::Buffer, bytecode::Parameter, syntax::ParamKind};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum Input {
    Supplied(Fact),
    Default,
    Either(Fact),
}

impl Input {
    pub fn widen(
        self,
        ctx: &mut CallContext,
        facts: &mut Facts,
        other: Self,
        depth: usize,
    ) -> Result<Self> {
        ctx.checkpoint()?;
        let (a, b) = match (self, other) {
            (Self::Default, Self::Default) => return Ok(Self::Default),
            (Self::Default, Self::Supplied(value) | Self::Either(value))
            | (Self::Supplied(value) | Self::Either(value), Self::Default) => {
                (Atom::Never.fact(), value)
            }
            (Self::Supplied(a) | Self::Either(a), Self::Supplied(b) | Self::Either(b)) => (a, b),
        };
        let value = facts.widen(ctx, a, b, depth)?;
        Ok(
            if matches!((self, other), (Self::Supplied(_), Self::Supplied(_))) {
                Self::Supplied(value)
            } else {
                Self::Either(value)
            },
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Keyword {
    pub name: Fact,
    pub value: Fact,
}

#[derive(Debug)]
pub(super) struct Arguments {
    pub positional: Buffer<Fact>,
    pub keywords: Buffer<Keyword>,
    pub block: Option<super::blocks::Closure>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Failure {
    NonCallable,
    Undefined,
    HostArity,
    HostKeywords,
    HostBlock,
    HostResult {
        actual: Fact,
        expected: Fact,
    },
    HostTypeBinding {
        parameter: Option<usize>,
        expected: Fact,
        ambiguous: bool,
    },
    BuiltinArity,
    BuiltinBlock,
    BuiltinKeywords,
    BuiltinKeyword(Fact),
    BuiltinKeywordType {
        name: Fact,
        actual: Fact,
        expected: Fact,
    },
    BuiltinValue,
    DetachedValue(Fact),
    TypeLiteral(Fact),
    BuiltinDomain(Fact),
    JsonValue(Fact),
    Missing(usize),
    ExtraPositionals,
    ExtraKeyword(Fact),
    Type {
        parameter: usize,
        actual: Fact,
        expected: Fact,
    },
}

pub(super) struct Bound {
    pub inputs: Buffer<Input>,
    pub failures: Buffer<Failure>,
}

impl Arguments {
    /// Rejects detached methods before entering a script or host body.
    pub fn admit(
        &mut self,
        ctx: &mut CallContext,
        facts: &mut Facts,
        failures: &mut Buffer<Failure>,
    ) -> Result<bool> {
        for value in self.positional.data.iter_mut().chain(
            self.keywords
                .data
                .iter_mut()
                .map(|keyword| &mut keyword.value),
        ) {
            ctx.charge(1)?;
            if facts.escapes(*value) {
                failures.push(ctx, Failure::DetachedValue(*value))?;
                *value = facts.exported(ctx, *value)?;
                if *value == Atom::Never.fact() {
                    return Ok(false);
                }
            }
        }
        Ok(true)
    }

    pub fn new() -> Self {
        Self {
            positional: Buffer::empty(),
            keywords: Buffer::empty(),
            block: None,
        }
    }

    pub fn snapshot(&self, ctx: &mut CallContext) -> Result<Self> {
        ctx.checkpoint()?;
        let mut args = Self::new();
        args.positional.extend(ctx, &self.positional.data)?;
        args.keywords.extend(ctx, &self.keywords.data)?;
        args.block = self.block.as_ref().map(|b| b.snapshot(ctx)).transpose()?;
        Ok(args)
    }

    pub fn keyword(&mut self, ctx: &mut CallContext, name: Fact, value: Fact) -> Result<()> {
        ctx.checkpoint()?;
        for keyword in &mut self.keywords.data {
            ctx.charge(1)?;
            if keyword.name == name {
                keyword.value = value;
                return Ok(());
            }
        }
        self.keywords.push(ctx, Keyword { name, value })
    }

    pub fn join(&mut self, ctx: &mut CallContext, facts: &mut Facts, other: &Self) -> Result<bool> {
        ctx.checkpoint()?;
        assert_eq!(self.positional.data.len(), other.positional.data.len());
        assert_eq!(self.keywords.data.len(), other.keywords.data.len());
        let mut changed = false;
        if let (Some(a), Some(b)) = (&mut self.block, &other.block) {
            changed |= a.join(ctx, facts, b)?;
        } else {
            assert_eq!(self.block.is_some(), other.block.is_some());
        }
        for (left, right) in self.positional.data.iter_mut().zip(&other.positional.data) {
            ctx.charge(1)?;
            let value = facts.union(ctx, &[*left, *right])?;
            changed |= *left != value;
            *left = value;
        }
        for (left, right) in self.keywords.data.iter_mut().zip(&other.keywords.data) {
            ctx.charge(1)?;
            assert_eq!(left.name, right.name);
            let value = facts.union(ctx, &[left.value, right.value])?;
            changed |= left.value != value;
            left.value = value;
        }
        Ok(changed)
    }

    /// Binds argument shape; each supplied value is normalized when its parameter is reached.
    pub fn bind(
        mut self,
        ctx: &mut CallContext,
        facts: &mut Facts,
        params: &[Parameter],
    ) -> Result<Bound> {
        ctx.checkpoint()?;
        self.collapse(ctx, facts, params)?;
        let mut used = Buffer::with_capacity(ctx, self.keywords.data.len())?;
        for _ in &self.keywords.data {
            ctx.charge(1)?;
            used.data.push(false);
        }
        let mut bound = Bound {
            inputs: Buffer::empty(),
            failures: Buffer::empty(),
        };
        let mut positional = 0;
        let mut rest = false;
        // Keyword-rest values are built after all named parameters mark their keys.
        for (index, param) in params.iter().enumerate() {
            ctx.charge(1)?;
            let value = match param.kind {
                ParamKind::Rest => {
                    let value = facts.tuple(ctx, &self.positional.data[positional..])?;
                    positional = self.positional.data.len();
                    Some(value)
                }
                ParamKind::KeywordRest => {
                    rest = true;
                    None
                }
                ParamKind::Positional if positional < self.positional.data.len() => {
                    let value = self.positional.data[positional];
                    positional += 1;
                    Some(value)
                }
                _ => {
                    let name = facts.symbol(ctx, param.name.as_bytes())?;
                    let mut value = None;
                    for (i, keyword) in self.keywords.data.iter().enumerate() {
                        ctx.charge(1)?;
                        if keyword.name == name {
                            used.data[i] = true;
                            value = Some(keyword.value);
                            break;
                        }
                    }
                    if value.is_none() && !param.default {
                        bound.failures.push(ctx, Failure::Missing(index))?;
                    }
                    value
                }
            };
            bound
                .inputs
                .push(ctx, value.map_or(Input::Default, Input::Supplied))?;
        }
        if positional < self.positional.data.len() {
            bound.failures.push(ctx, Failure::ExtraPositionals)?;
        }
        if rest {
            let mut remaining = Buffer::empty();
            for (i, &keyword) in self.keywords.data.iter().enumerate() {
                ctx.charge(1)?;
                if !used.data[i] {
                    remaining.push(ctx, keyword)?;
                }
            }
            let value = keyword_shape(ctx, facts, &remaining.data)?;
            for (index, param) in params.iter().enumerate() {
                ctx.charge(1)?;
                if param.kind == ParamKind::KeywordRest {
                    bound.inputs.data[index] = Input::Supplied(value);
                }
            }
        } else {
            for (index, keyword) in self.keywords.data.iter().enumerate() {
                ctx.charge(1)?;
                if !used.data[index] {
                    bound
                        .failures
                        .push(ctx, Failure::ExtraKeyword(keyword.name))?;
                }
            }
        }
        Ok(bound)
    }

    fn collapse(
        &mut self,
        ctx: &mut CallContext,
        facts: &mut Facts,
        params: &[Parameter],
    ) -> Result<()> {
        if self.keywords.data.is_empty() {
            return Ok(());
        }
        for param in params {
            ctx.charge(1)?;
            if matches!(param.kind, ParamKind::Keyword | ParamKind::KeywordRest) {
                return Ok(());
            }
        }
        let mut positional = self.positional.data.len();
        for param in params {
            ctx.charge(1)?;
            if param.kind == ParamKind::Positional {
                if positional > 0 {
                    positional -= 1;
                    continue;
                }
                let name = facts.symbol(ctx, param.name.as_bytes())?;
                for keyword in &self.keywords.data {
                    ctx.charge(1)?;
                    if keyword.name == name {
                        return Ok(());
                    }
                }
            }
            let value = keyword_shape(ctx, facts, &self.keywords.data)?;
            self.positional.push(ctx, value)?;
            self.keywords = Buffer::empty();
            break;
        }
        Ok(())
    }
}

pub(super) fn keyword_shape(
    ctx: &mut CallContext,
    facts: &mut Facts,
    keywords: &[Keyword],
) -> Result<Fact> {
    let mut names = Buffer::empty();
    for keyword in keywords {
        ctx.charge(1)?;
        let Node::Symbol(name) = facts.node(keyword.name) else {
            unreachable!()
        };
        names.push(ctx, name.clone())?;
    }
    let mut fields = Buffer::empty();
    for (name, keyword) in names.data.iter().zip(keywords) {
        ctx.charge(1)?;
        fields.push(ctx, (name.as_bytes().unwrap(), keyword.value, false))?;
    }
    facts.shape(ctx, &fields.data, false)
}

pub(super) fn general_inputs(
    ctx: &mut CallContext,
    facts: &mut Facts,
    params: &[Parameter],
    contracts: &[Fact],
) -> Result<Buffer<Input>> {
    let mut values = Buffer::empty();
    for param in params {
        ctx.charge(1)?;
        let fact = if let Some(ty) = param.ty {
            facts.value_domain(ctx, contracts[ty])?
        } else {
            match param.kind {
                ParamKind::Rest => facts.array(ctx, Atom::Unknown.fact())?,
                ParamKind::KeywordRest => {
                    facts.hash(ctx, Atom::String.fact(), Atom::Unknown.fact())?
                }
                _ => Atom::Unknown.fact(),
            }
        };
        values.push(
            ctx,
            if param.default {
                Input::Either(fact)
            } else {
                Input::Supplied(fact)
            },
        )?;
    }
    Ok(values)
}
