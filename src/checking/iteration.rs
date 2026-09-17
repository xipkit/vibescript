use super::facts::{Atom, Fact, Facts, Node};
use crate::{CallContext, Result, budget::Buffer, bytecode::Selection};

pub(super) struct Iteration {
    pub empty: Fact,
    pub source: Fact,
    pub item: Fact,
    pub repeat: Fact,
    pub rejected: bool,
    pub unsupported: bool,
}

impl Facts {
    pub fn iteration(&mut self, ctx: &mut CallContext, source: Fact) -> Result<Iteration> {
        ctx.checkpoint()?;
        let mut result = Iteration {
            empty: Atom::Never.fact(),
            source: Atom::Never.fact(),
            item: Atom::Never.fact(),
            repeat: Atom::Never.fact(),
            rejected: false,
            unsupported: false,
        };
        for i in 0..self.arm_count(source) {
            ctx.charge(1)?;
            let arm = self.arm(source, i);
            let (item, empty, multiple) = match self.node(arm) {
                Node::Atom(Atom::Never) => continue,
                Node::Atom(Atom::Unknown | Atom::Any) => (Atom::Unknown.fact(), true, true),
                Node::Atom(Atom::Range) => (Atom::Int.fact(), true, true),
                Node::Range(start, end, exclusive) => {
                    let (Some(start), Some(end)) = (*start, *end) else {
                        result.rejected = true;
                        continue;
                    };
                    let length =
                        (i128::from(start) - i128::from(end)).abs() + i128::from(!exclusive);
                    let item = match length {
                        0 => Atom::Never.fact(),
                        1 => self.integer(ctx, start)?,
                        _ => Atom::Int.fact(),
                    };
                    (item, length == 0, length > 1)
                }
                Node::Array(item) => (*item, true, true),
                Node::Tuple(items) => {
                    let length = items.data.len();
                    (self.elements(ctx, arm)?, length == 0, length > 1)
                }
                Node::Hash(keys, values, true) => {
                    let pair = [*keys, *values];
                    let item = if pair.contains(&Atom::Never.fact()) {
                        Atom::Never.fact()
                    } else {
                        self.tuple(ctx, &pair)?
                    };
                    (item, true, true)
                }
                Node::Shape(fields, open, keys, true) => {
                    let (length, open, keys) = (fields.data.len(), *open, *keys);
                    let mut empty = true;
                    let mut items = Buffer::empty();
                    for index in 0..length {
                        ctx.charge(1)?;
                        let Node::Shape(fields, ..) = self.node(arm) else {
                            unreachable!()
                        };
                        let field = &fields.data[index];
                        let (name, value) = (field.name.clone(), field.value);
                        empty &= field.optional;
                        let key = match self.atom(keys) {
                            Some(Atom::String) => self.string(ctx, name.as_bytes().unwrap())?,
                            Some(Atom::Symbol) => self.symbol(ctx, name.as_bytes().unwrap())?,
                            _ => keys,
                        };
                        let item = self.tuple(ctx, &[key, value])?;
                        items.push(ctx, item)?;
                    }
                    if open {
                        let item = self.tuple(ctx, &[keys, Atom::Unknown.fact()])?;
                        items.push(ctx, item)?;
                    }
                    (self.union(ctx, &items.data)?, empty, open || length > 1)
                }
                Node::Named(_) | Node::Nominal { .. } | Node::Hash(..) | Node::Shape(..) => {
                    result.unsupported = true;
                    continue;
                }
                _ => {
                    result.rejected = true;
                    continue;
                }
            };
            if empty {
                result.empty = self.union(ctx, &[result.empty, arm])?;
            }
            if item != Atom::Never.fact() {
                result.source = self.union(ctx, &[result.source, arm])?;
                result.item = self.union(ctx, &[result.item, item])?;
                if multiple {
                    result.repeat = self.union(ctx, &[result.repeat, item])?;
                }
            }
        }
        Ok(result)
    }

    pub fn extract(
        &mut self,
        ctx: &mut CallContext,
        source: Fact,
        selection: Selection,
    ) -> Result<Fact> {
        ctx.checkpoint()?;
        let mut values = Buffer::empty();
        for i in 0..self.arm_count(source) {
            ctx.charge(1)?;
            let arm = self.arm(source, i);
            let value = match self.node(arm) {
                Node::Atom(Atom::Never) => Atom::Never.fact(),
                Node::Atom(Atom::Unknown | Atom::Any) => match selection {
                    Selection::Rest { .. } => self.array(ctx, Atom::Unknown.fact())?,
                    _ => Atom::Unknown.fact(),
                },
                Node::Array(item) => {
                    let item = *item;
                    match selection {
                        Selection::Rest { .. } => self.array(ctx, item)?,
                        _ => self.nullable(ctx, item)?,
                    }
                }
                _ => {
                    // Destructuring treats every non-array value as one element, without dispatch.
                    let items = if let Node::Tuple(items) = self.node(arm) {
                        &items.data[..]
                    } else {
                        std::slice::from_ref(&arm)
                    };
                    match selection {
                        Selection::At(index) => {
                            items.get(index).copied().unwrap_or(Atom::Nil.fact())
                        }
                        Selection::Rest { leading, trailing } => {
                            let start = leading.min(items.len());
                            let end = items.len().saturating_sub(trailing).max(start);
                            let mut copied = Buffer::empty();
                            copied.extend(ctx, &items[start..end])?;
                            self.tuple(ctx, &copied.data)?
                        }
                        Selection::Tail {
                            leading,
                            trailing,
                            index,
                        } => {
                            let position = items
                                .len()
                                .saturating_sub(trailing)
                                .max(leading)
                                .saturating_add(index);
                            items.get(position).copied().unwrap_or(Atom::Nil.fact())
                        }
                    }
                }
            };
            values.push(ctx, value)?;
        }
        self.union(ctx, &values.data)
    }
}
