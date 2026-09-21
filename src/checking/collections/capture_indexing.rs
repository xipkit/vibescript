use super::{Count, outcome, rejected, unsupported};
use crate::{
    CallContext, Result,
    budget::Buffer,
    checking::{
        facts::{Atom, Fact, Facts, HashKind, Node},
        scalar::Operation,
    },
};

struct Field {
    value: Fact,
    missing: bool,
    unknown_lookup: bool,
}

impl Facts {
    // Address::index follows a successful lookup with stored_child. For a
    // rooted hash this requires a byte key, even when numeric capture lookup
    // itself succeeds, and fails before evaluating mutation arguments.
    pub(super) fn stored_hash_index(
        &mut self,
        ctx: &mut CallContext,
        receiver: Fact,
        key: Fact,
    ) -> Result<Operation> {
        match self.atom(key) {
            Some(Atom::Never) => Ok(outcome(Atom::Never.fact())),
            Some(Atom::String | Atom::Symbol) => self.capture_name(ctx, receiver, key),
            Some(Atom::Any | Atom::Unknown) => Ok(Operation {
                throws: true,
                ..self.capture_name(ctx, receiver, Atom::String.fact())?
            }),
            _ if matches!(self.node(key), Node::Named(_) | Node::Choice(_)) => Ok(unsupported()),
            _ => Ok(rejected()),
        }
    }

    pub(super) fn hash_index(
        &mut self,
        ctx: &mut CallContext,
        receiver: Fact,
        key: Fact,
    ) -> Result<Operation> {
        match self.atom(key) {
            Some(Atom::Int | Atom::Float) => self.capture_number(ctx, receiver, key),
            Some(Atom::String | Atom::Symbol) => self.capture_name(ctx, receiver, key),
            Some(Atom::Any | Atom::Unknown) => {
                let mut result = self.capture_name(ctx, receiver, Atom::String.fact())?;
                let numeric = self.capture_number(ctx, receiver, Atom::Int.fact())?;
                self.merge_operation(ctx, &mut result, numeric)?;
                // An unknown selector may be a valid key or an invalid runtime kind.
                result.rejected = false;
                result.throws = true;
                Ok(result)
            }
            _ if matches!(self.node(key), Node::Named(_) | Node::Choice(_)) => Ok(unsupported()),
            _ => Ok(rejected()),
        }
    }

    // This is Hash::find, not another capture-index operation. A stored nil
    // shadows a named capture, and named_captures never follows its own fallback.
    fn stored_capture_field(
        &mut self,
        ctx: &mut CallContext,
        mut receiver: Fact,
        key: Fact,
    ) -> Result<Field> {
        while let Node::Protected(shape, _) = self.node(receiver) {
            ctx.charge(1)?;
            receiver = *shape;
        }
        match self.node(receiver) {
            Node::Shape(_, open, _, _) => {
                let open = *open;
                if let Node::String(name) | Node::Symbol(name) = self.node(key) {
                    let name = name.clone();
                    return Ok(
                        match self.selected_field(ctx, receiver, name.as_bytes().unwrap())? {
                            Some((value, missing)) => Field {
                                value,
                                missing,
                                unknown_lookup: false,
                            },
                            None => Field {
                                value: if open {
                                    Atom::Unknown.fact()
                                } else {
                                    Atom::Never.fact()
                                },
                                missing: true,
                                unknown_lookup: open,
                            },
                        },
                    );
                }
                Ok(Field {
                    value: self.shape_values(ctx, receiver, false)?,
                    missing: true,
                    unknown_lookup: false,
                })
            }
            Node::Hash(keys, value, _) => Ok(Field {
                value: if self.impossible_keys(*keys) {
                    Atom::Never.fact()
                } else {
                    *value
                },
                missing: true,
                unknown_lookup: false,
            }),
            _ => unreachable!(),
        }
    }

    fn capture_field(
        &mut self,
        ctx: &mut CallContext,
        receiver: Fact,
        name: &[u8],
    ) -> Result<Field> {
        let key = self.string(ctx, name)?;
        self.stored_capture_field(ctx, receiver, key)
    }

    fn capture_name(
        &mut self,
        ctx: &mut CallContext,
        receiver: Fact,
        key: Fact,
    ) -> Result<Operation> {
        let field = self.stored_capture_field(ctx, receiver, key)?;
        // An undeclared key in an open shape already has the gradual unknown
        // read fact, including absence; do not introduce a separate nil fact.
        if !field.missing || field.unknown_lookup {
            return Ok(outcome(field.value));
        }
        let mode = self.hash_mode(receiver);
        let object = mode.overlaps(HashKind::OBJECT.join(HashKind::MATCH).join(HashKind::ERROR));
        if !object {
            return Ok(outcome(self.nullable(ctx, field.value)?));
        }
        let whole = self.capture_field(ctx, receiver, b"to_s")?;
        let mut result = outcome(field.value);
        if mode.overlaps(HashKind::PLAIN) || whole.missing {
            result.value = self.nullable(ctx, result.value)?;
        }
        if whole.value == Atom::Never.fact() {
            return Ok(result);
        }
        let named = self.capture_field(ctx, receiver, b"named_captures")?;
        if named.missing {
            result.value = self.nullable(ctx, result.value)?;
        }
        for i in 0..self.arm_count(named.value) {
            ctx.charge(1)?;
            let arm = self.arm(named.value, i);
            let value = match self.node(arm) {
                Node::Hash(..) | Node::Shape(..) | Node::Protected(..) => {
                    let field = self.stored_capture_field(ctx, arm, key)?;
                    if field.missing && !field.unknown_lookup {
                        self.nullable(ctx, field.value)?
                    } else {
                        field.value
                    }
                }
                Node::Atom(Atom::Never) => continue,
                Node::Atom(Atom::Unknown | Atom::Any) => Atom::Unknown.fact(),
                Node::Named(_) | Node::Choice(_) => {
                    result.unsupported = true;
                    continue;
                }
                _ => Atom::Nil.fact(),
            };
            result.value = self.union(ctx, &[result.value, value])?;
        }
        Ok(result)
    }

    fn capture_number(
        &mut self,
        ctx: &mut CallContext,
        receiver: Fact,
        key: Fact,
    ) -> Result<Operation> {
        let mode = self.hash_mode(receiver);
        if !mode.overlaps(HashKind::OBJECT.join(HashKind::MATCH).join(HashKind::ERROR)) {
            return Ok(rejected());
        }
        let whole = self.capture_field(ctx, receiver, b"to_s")?;
        let captures = self.capture_field(ctx, receiver, b"captures")?;
        if whole.value == Atom::Never.fact() || captures.value == Atom::Never.fact() {
            return Ok(rejected());
        }
        let mut result = Operation {
            throws: whole.missing || captures.missing || mode.overlaps(HashKind::PLAIN),
            ..outcome(Atom::Never.fact())
        };
        for i in 0..self.arm_count(captures.value) {
            ctx.charge(1)?;
            let arm = self.arm(captures.value, i);
            let next = match self.node(arm) {
                Node::Tuple(_) | Node::Array(_) => {
                    self.capture_position(ctx, whole.value, arm, key)?
                }
                Node::Atom(Atom::Unknown | Atom::Any) => {
                    let array = self.array(ctx, Atom::Unknown.fact())?;
                    Operation {
                        throws: true,
                        ..self.capture_position(ctx, whole.value, array, key)?
                    }
                }
                Node::Atom(Atom::Never) => continue,
                Node::Named(_) | Node::Choice(_) => unsupported(),
                _ => Operation {
                    throws: true,
                    ..outcome(Atom::Never.fact())
                },
            };
            self.merge_operation(ctx, &mut result, next)?;
        }
        if result.value == Atom::Never.fact() && !result.unsupported {
            result.rejected = true;
        }
        Ok(result)
    }

    fn capture_position(
        &mut self,
        ctx: &mut CallContext,
        whole: Fact,
        captures: Fact,
        key: Fact,
    ) -> Result<Operation> {
        let (key, throws) = match self.count_arm(key, false) {
            Count::Exact(n) => (self.integer(ctx, n)?, false),
            Count::Bounded(bounds) => (key, bounds.min.is_none() || bounds.max.is_none()),
            Count::Float => (key, true),
            Count::Invalid => return Ok(rejected()),
            Count::Never => return Ok(outcome(Atom::Never.fact())),
            Count::Unknown | Count::Unsupported => return Ok(unsupported()),
        };
        let tuple = match self.node(captures) {
            Node::Tuple(values) => {
                let mut items = Buffer::with_capacity(ctx, values.data.len() + 1)?;
                items.push(ctx, whole)?;
                items.extend(ctx, &values.data)?;
                Some(self.tuple(ctx, &items.data)?)
            }
            Node::Array(element) if *element == Atom::Never.fact() => {
                Some(self.tuple(ctx, &[whole])?)
            }
            _ => None,
        };
        if let Some(tuple) = tuple {
            let mut result = self.collection_index(ctx, tuple, &[key])?;
            result.throws |= throws;
            return Ok(result);
        }
        let element = self.elements(ctx, captures)?;
        let value = if let Some(bounds) = self.integer_bounds(key) {
            let mut values = Buffer::empty();
            if bounds.min.is_none_or(|n| n <= 0) {
                values.push(ctx, whole)?;
            }
            if bounds.min != Some(0) || bounds.max != Some(0) {
                values.push(ctx, element)?;
            }
            if bounds.min.is_none_or(|n| n < -1) || bounds.max.is_none_or(|n| n > 0) {
                values.push(ctx, Atom::Nil.fact())?;
            }
            self.union(ctx, &values.data)?
        } else {
            self.union(ctx, &[whole, element, Atom::Nil.fact()])?
        };
        Ok(Operation {
            throws,
            ..outcome(value)
        })
    }
}
