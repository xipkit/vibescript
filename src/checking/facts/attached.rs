use super::*;

impl Facts {
    pub fn escapes(&self, value: Fact) -> bool {
        self.entries.data[value.0].escapes
    }

    pub(super) fn node_escapes(&self, ctx: &mut CallContext, node: &Node) -> Result<bool> {
        ctx.charge(1)?;
        Ok(match node {
            Node::Callable { .. } => true,
            Node::Array(value) | Node::Protected(value, ..) => self.escapes(*value),
            Node::Hash(_, value, kind) if !kind.object() => self.escapes(*value),
            Node::Tuple(values) | Node::Union(values) | Node::Choice(values) => {
                ctx.charge(values.data.len() as u64)?;
                values.data.iter().any(|&value| self.escapes(value))
            }
            Node::Shape(fields, _, _, kind) if !kind.object() => {
                ctx.charge(fields.data.len() as u64)?;
                fields.data.iter().any(|field| self.escapes(field.value))
            }
            _ => false,
        })
    }

    fn exported_child(&self, value: Fact) -> Fact {
        if self.escapes(value) {
            let exported = self.entries.data[value.0].exported;
            assert_ne!(exported, Fact(EMPTY));
            exported
        } else {
            value
        }
    }

    /// Filters detachable methods from data while leaving namespace objects intact.
    pub fn exported(&mut self, ctx: &mut CallContext, value: Fact) -> Result<Fact> {
        ctx.checkpoint()?;
        if !self.escapes(value) {
            return Ok(value);
        }
        let mut pending = Buffer::empty();
        pending.push(ctx, (value, false))?;
        while let Some((value, ready)) = pending.data.pop() {
            ctx.charge(1)?;
            if !self.escapes(value) || self.entries.data[value.0].exported != Fact(EMPTY) {
                continue;
            }
            if !ready {
                pending.push(ctx, (value, true))?;
                match self.node(value) {
                    Node::Array(child) | Node::Protected(child, ..) | Node::Hash(_, child, _) => {
                        pending.push(ctx, (*child, false))?;
                    }
                    Node::Tuple(children) | Node::Union(children) | Node::Choice(children) => {
                        for &child in &children.data {
                            pending.push(ctx, (child, false))?;
                        }
                    }
                    Node::Shape(fields, ..) => {
                        for field in &fields.data {
                            pending.push(ctx, (field.value, false))?;
                        }
                    }
                    _ => (),
                }
                continue;
            }
            let filtered = match *self.node(value) {
                Node::Callable { .. } => Atom::Never.fact(),
                Node::Protected(child, tag, certainty) => {
                    let child = self.exported_child(child);
                    if child == Atom::Never.fact() {
                        child
                    } else {
                        self.protected_as(ctx, child, tag, certainty)?
                    }
                }
                Node::Array(child) => {
                    let child = self.exported_child(child);
                    if child == Atom::Never.fact() {
                        self.tuple(ctx, &[])?
                    } else {
                        self.array(ctx, child)?
                    }
                }
                Node::Hash(key, child, kind) => {
                    let child = self.exported_child(child);
                    let plain = if child == Atom::Never.fact() {
                        self.shape(ctx, &[], false)?
                    } else {
                        self.hash_kind(ctx, key, child, HashKind::PLAIN)?
                    };
                    // The exported plain copy is plain; every other possible
                    // provenance, including a still possible protected object,
                    // keeps its methods as the remaining copy.
                    if !kind.plain() {
                        let object = self.hash_as(ctx, value, kind.without(HashKind::PLAIN))?;
                        let object = self.pruned_contract(ctx, object)?;
                        self.union(ctx, &[plain, object])?
                    } else {
                        plain
                    }
                }
                Node::Tuple(ref children)
                | Node::Union(ref children)
                | Node::Choice(ref children) => {
                    let tuple = matches!(self.node(value), Node::Tuple(_));
                    let mut values = Buffer::empty();
                    let mut missing = false;
                    for &child in &children.data {
                        let child = self.exported_child(child);
                        missing |= child == Atom::Never.fact();
                        values.push(ctx, child)?;
                    }
                    if tuple && missing {
                        Atom::Never.fact()
                    } else if tuple {
                        self.tuple(ctx, &values.data)?
                    } else {
                        self.union(ctx, &values.data)?
                    }
                }
                Node::Shape(ref fields, open, keys, kind) => {
                    let mut next = Buffer::empty();
                    let mut missing = false;
                    for field in &fields.data {
                        let child = self.exported_child(field.value);
                        if child == Atom::Never.fact() {
                            missing |= !field.optional;
                        } else {
                            next.push(
                                ctx,
                                Field {
                                    name: field.name.clone(),
                                    value: child,
                                    optional: field.optional,
                                },
                            )?;
                        }
                    }
                    let plain = if missing {
                        Atom::Never.fact()
                    } else {
                        self.shape_fields(ctx, next, open, keys, HashKind::PLAIN)?
                    };
                    if !kind.plain() {
                        let object = self.hash_as(ctx, value, kind.without(HashKind::PLAIN))?;
                        let object = self.pruned_contract(ctx, object)?;
                        self.union(ctx, &[plain, object])?
                    } else {
                        plain
                    }
                }
                _ => unreachable!(),
            };
            self.entries.data[value.0].exported = filtered;
        }
        Ok(self.exported_child(value))
    }
}
