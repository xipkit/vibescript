use super::{
    facts::{Atom, Fact, Facts, Node},
    scalar::{Operation, Test},
};
use crate::{CallContext, Result, Value, budget::Buffer, bytecode::CallSite};

fn outcome(value: Fact) -> Operation {
    Operation {
        value,
        rejected: false,
        unsupported: false,
    }
}

fn rejected() -> Operation {
    Operation {
        rejected: true,
        ..outcome(Atom::Never.fact())
    }
}

fn unsupported() -> Operation {
    Operation {
        unsupported: true,
        ..outcome(Atom::Never.fact())
    }
}

impl Facts {
    fn merge_operation(
        &mut self,
        ctx: &mut CallContext,
        into: &mut Operation,
        next: Operation,
    ) -> Result<()> {
        into.value = self.union(ctx, &[into.value, next.value])?;
        into.rejected |= next.rejected;
        into.unsupported |= next.unsupported;
        Ok(())
    }

    pub(super) fn plain_hash(&self, value: Fact) -> bool {
        matches!(
            self.node(value),
            Node::Shape(_, _, _, true) | Node::Hash(_, _, true)
        )
    }

    fn object_index(&self, ctx: &mut CallContext, value: Fact, member: &[u8]) -> Result<bool> {
        if self.plain_hash(value) {
            return Ok(false);
        }
        match self.node(value) {
            Node::Hash(..) | Node::Shape(_, true, _, _) => Ok(true),
            Node::Shape(..) => Ok(self.selected_field(ctx, value, b"to_s")?.is_some()
                && self.selected_field(ctx, value, member)?.is_some()),
            _ => Ok(false),
        }
    }

    pub(super) fn nullable(&mut self, ctx: &mut CallContext, value: Fact) -> Result<Fact> {
        self.union(ctx, &[value, Atom::Nil.fact()])
    }

    pub(super) fn elements(&mut self, ctx: &mut CallContext, value: Fact) -> Result<Fact> {
        match self.node(value) {
            Node::Array(element) => Ok(*element),
            Node::Tuple(elements) => {
                let mut copied = Buffer::empty();
                copied.extend(ctx, &elements.data)?;
                self.union(ctx, &copied.data)
            }
            _ => unreachable!(),
        }
    }

    fn selected_field(
        &self,
        ctx: &mut CallContext,
        value: Fact,
        key: &[u8],
    ) -> Result<Option<(Fact, bool)>> {
        let Node::Shape(fields, ..) = self.node(value) else {
            unreachable!()
        };
        let (mut start, mut end) = (0, fields.data.len());
        while start < end {
            let mid = start + (end - start) / 2;
            let field = &fields.data[mid];
            let name = field.name.as_bytes().unwrap();
            ctx.work_bytes(name.len().min(key.len()).saturating_add(1))?;
            match name.cmp(key) {
                std::cmp::Ordering::Less => start = mid + 1,
                std::cmp::Ordering::Greater => end = mid,
                std::cmp::Ordering::Equal => return Ok(Some((field.value, field.optional))),
            }
        }
        Ok(None)
    }

    pub(super) fn shape_values(
        &mut self,
        ctx: &mut CallContext,
        value: Fact,
        absent: bool,
    ) -> Result<Fact> {
        let Node::Shape(fields, open, _, _) = self.node(value) else {
            unreachable!()
        };
        let mut values = Buffer::empty();
        if *open {
            values.push(ctx, Atom::Unknown.fact())?;
        }
        if absent {
            values.push(ctx, Atom::Nil.fact())?;
        }
        for field in &fields.data {
            values.push(ctx, field.value)?;
        }
        self.union(ctx, &values.data)
    }

    pub fn collection_index(
        &mut self,
        ctx: &mut CallContext,
        receiver: Fact,
        args: &[Fact],
    ) -> Result<Operation> {
        ctx.checkpoint()?;
        if args.is_empty() || args.len() > 2 {
            return Ok(rejected());
        }
        let mut result = outcome(Atom::Never.fact());
        for r in 0..self.arm_count(receiver) {
            for a in 0..self.arm_count(args[0]) {
                for b in 0..args.get(1).map_or(1, |&arg| self.arm_count(arg)) {
                    ctx.charge(1)?;
                    let root = self.arm(receiver, r);
                    let index = self.arm(args[0], a);
                    let length = args.get(1).map(|&arg| self.arm(arg, b));
                    let next = self.index_arm(ctx, root, index, length)?;
                    self.merge_operation(ctx, &mut result, next)?;
                }
            }
        }
        Ok(result)
    }

    fn index_arm(
        &mut self,
        ctx: &mut CallContext,
        receiver: Fact,
        index: Fact,
        length: Option<Fact>,
    ) -> Result<Operation> {
        let selector = self.atom(index);
        let unknown = |a| matches!(a, Some(Atom::Unknown | Atom::Any));
        if unknown(self.atom(receiver)) {
            return Ok(outcome(Atom::Unknown.fact()));
        }
        if matches!(self.node(receiver), Node::Named(_) | Node::Nominal { .. }) {
            return Ok(unsupported());
        }
        if receiver == Atom::Never.fact()
            || index == Atom::Never.fact()
            || length == Some(Atom::Never.fact())
        {
            return Ok(outcome(Atom::Never.fact()));
        }
        if length.is_none() && matches!(self.node(receiver), Node::Hash(..) | Node::Shape(..)) {
            if matches!(selector, Some(Atom::Int | Atom::Float))
                && self.object_index(ctx, receiver, b"captures")?
            {
                return Ok(unsupported());
            }
            if !unknown(selector) && !matches!(selector, Some(Atom::String | Atom::Symbol)) {
                return Ok(if matches!(self.node(index), Node::Named(_)) {
                    unsupported()
                } else {
                    rejected()
                });
            }
            let value = match self.node(receiver) {
                Node::Hash(_, value, _) => self.nullable(ctx, *value)?,
                Node::Shape(_, open, _, _) => {
                    let open = *open;
                    if let Node::String(key) | Node::Symbol(key) = self.node(index) {
                        let field = self.selected_field(ctx, receiver, key.as_bytes().unwrap())?;
                        if (!open || field.is_some())
                            && field.is_none_or(|(_, optional)| optional)
                            && self.object_index(ctx, receiver, b"named_captures")?
                        {
                            return Ok(unsupported());
                        }
                        match field {
                            Some((value, false)) => value,
                            Some((value, true)) => self.nullable(ctx, value)?,
                            None if open => Atom::Unknown.fact(),
                            None => Atom::Nil.fact(),
                        }
                    } else {
                        self.shape_values(ctx, receiver, true)?
                    }
                }
                _ => unreachable!(),
            };
            return Ok(outcome(value));
        }
        let array = matches!(self.node(receiver), Node::Array(_) | Node::Tuple(_));
        let string = self.atom(receiver) == Some(Atom::String);
        if !array && !string {
            return Ok(rejected());
        }
        if unknown(selector) || length.is_some_and(|value| unknown(self.atom(value))) {
            return Ok(outcome(Atom::Unknown.fact()));
        }
        if selector.is_none() || length.is_some_and(|value| self.atom(value).is_none()) {
            return Ok(
                if matches!(self.node(index), Node::Named(_))
                    || length.is_some_and(|value| matches!(self.node(value), Node::Named(_)))
                {
                    unsupported()
                } else {
                    rejected()
                },
            );
        }
        if !matches!(selector, Some(Atom::Int | Atom::Float | Atom::Range))
            || (selector == Some(Atom::Range) && length.is_some())
            || length
                .is_some_and(|value| !matches!(self.atom(value), Some(Atom::Int | Atom::Float)))
        {
            return Ok(rejected());
        }
        if string {
            if let (Node::String(bytes), Node::Integer(index), None) =
                (self.node(receiver), self.node(index), length)
            {
                let bytes = bytes.clone();
                let value = crate::ops::index(ctx, &bytes, &Value::int(*index))?;
                return Ok(outcome(match value.as_bytes() {
                    Some(bytes) => self.string(ctx, bytes)?,
                    None => Atom::Nil.fact(),
                }));
            }
            return Ok(outcome(self.nullable(ctx, Atom::String.fact())?));
        }
        if let (Node::Tuple(values), Node::Integer(index)) = (self.node(receiver), self.node(index))
        {
            let start = if *index < 0 {
                values.data.len() as i128 + i128::from(*index)
            } else {
                i128::from(*index)
            };
            let size = values.data.len() as i128;
            if length.is_none() {
                return Ok(outcome(if start >= 0 && start < size {
                    values.data[start as usize]
                } else {
                    Atom::Nil.fact()
                }));
            }
            if let Node::Integer(length) = self.node(length.unwrap()) {
                if start < 0 || start > size || *length < 0 {
                    return Ok(outcome(Atom::Nil.fact()));
                }
                let end = (start + i128::from(*length)).min(size) as usize;
                let mut selected = Buffer::empty();
                selected.extend(ctx, &values.data[start as usize..end])?;
                return Ok(outcome(self.tuple(ctx, &selected.data)?));
            }
        }
        let element = self.elements(ctx, receiver)?;
        let value = if length.is_some() || selector == Some(Atom::Range) {
            self.array(ctx, element)?
        } else {
            element
        };
        Ok(outcome(self.nullable(ctx, value)?))
    }

    pub fn prepare_collection_member(
        &mut self,
        ctx: &mut CallContext,
        receiver: Fact,
        name: &str,
    ) -> Result<Operation> {
        let mut result = outcome(Atom::Never.fact());
        for i in 0..self.arm_count(receiver) {
            ctx.charge(1)?;
            let arm = self.arm(receiver, i);
            let next = match self.node(arm) {
                Node::Shape(_, false, _, _) if !crate::members::hash_builtin(name) => {
                    if self.selected_field(ctx, arm, name.as_bytes())?.is_none() {
                        rejected()
                    } else {
                        outcome(arm)
                    }
                }
                Node::Named(_) | Node::Nominal { .. } => unsupported(),
                _ => outcome(arm),
            };
            self.merge_operation(ctx, &mut result, next)?;
        }
        Ok(result)
    }

    pub fn collection_member(
        &mut self,
        ctx: &mut CallContext,
        receiver: Fact,
        site: CallSite,
        name: &str,
        args: &[Fact],
    ) -> Result<Operation> {
        ctx.checkpoint()?;
        let mut result = outcome(Atom::Never.fact());
        for i in 0..self.arm_count(receiver) {
            ctx.charge(1)?;
            let arm = self.arm(receiver, i);
            let next = self.member_arm(ctx, arm, site, name, args)?;
            self.merge_operation(ctx, &mut result, next)?;
        }
        Ok(result)
    }

    fn member_arm(
        &mut self,
        ctx: &mut CallContext,
        receiver: Fact,
        site: CallSite,
        name: &str,
        args: &[Fact],
    ) -> Result<Operation> {
        if receiver == Atom::Never.fact() {
            return Ok(outcome(receiver));
        }
        if matches!(self.node(receiver), Node::Named(_) | Node::Nominal { .. }) {
            return Ok(unsupported());
        }
        let array = matches!(self.node(receiver), Node::Array(_) | Node::Tuple(_));
        let hash = matches!(self.node(receiver), Node::Hash(..) | Node::Shape(..));
        let string = self.atom(receiver) == Some(Atom::String);
        let unknown = matches!(self.atom(receiver), Some(Atom::Unknown | Atom::Any));
        if site.scope {
            return Ok(if unknown || (hash && !self.plain_hash(receiver)) {
                unsupported()
            } else {
                rejected()
            });
        }
        if hash && !crate::members::hash_builtin(name) {
            let value = match self.node(receiver) {
                Node::Shape(_, open, _, _) => {
                    match self.selected_field(ctx, receiver, name.as_bytes())? {
                        Some((value, _)) => value,
                        None if *open => Atom::Unknown.fact(),
                        None => return Ok(rejected()),
                    }
                }
                Node::Hash(_, value, _) => *value,
                _ => unreachable!(),
            };
            return Ok(if site.auto {
                outcome(value)
            } else if self.known_primitive(ctx, value)? {
                rejected()
            } else {
                unsupported()
            });
        }
        // Structural contracts can also describe capability objects whose fields override builtins.
        if hash && !self.plain_hash(receiver) {
            return Ok(unsupported());
        }
        let arity = match name {
            "length" | "size" | "bytesize" | "empty?" | "keys" | "values" | "reverse"
            | "itself" | "dup" | "nil?" => 0..=0,
            "at" | "getbyte" | "take" | "drop" => 1..=1,
            "first" | "last" => 0..=1,
            "slice" if !hash => 1..=2,
            _ => return Ok(unsupported()),
        };
        if !arity.contains(&args.len()) {
            return Ok(rejected());
        }
        if unknown {
            return Ok(outcome(Atom::Unknown.fact()));
        }
        match name {
            "itself" | "dup" => Ok(outcome(receiver)),
            "nil?" => Ok(outcome(self.test_result(ctx, receiver, Test::Nil)?)),
            "length" | "size" if array || hash || string => Ok(outcome(Atom::Int.fact())),
            "bytesize" if string || self.atom(receiver) == Some(Atom::Symbol) => {
                Ok(outcome(Atom::Int.fact()))
            }
            "empty?" if array || hash || string => {
                let empty = match self.node(receiver) {
                    Node::Tuple(values) => Some(values.data.is_empty()),
                    Node::String(value) => Some(value.as_bytes().unwrap().is_empty()),
                    Node::Shape(fields, open, _, _) => {
                        let mut required = false;
                        for field in &fields.data {
                            ctx.charge(1)?;
                            required |= !field.optional;
                        }
                        if required {
                            Some(false)
                        } else if !open && fields.data.is_empty() {
                            Some(true)
                        } else {
                            None
                        }
                    }
                    _ => None,
                };
                Ok(outcome(match empty {
                    Some(empty) => self.boolean(ctx, empty)?,
                    None => Atom::Bool.fact(),
                }))
            }
            "keys" | "values" if hash => {
                let element = match self.node(receiver) {
                    Node::Shape(fields, open, keys, _) => {
                        if fields.data.is_empty() && !open {
                            return Ok(outcome(self.tuple(ctx, &[])?));
                        }
                        if name == "keys" {
                            *keys
                        } else {
                            self.shape_values(ctx, receiver, false)?
                        }
                    }
                    Node::Hash(keys, values, _) => {
                        if name == "keys" {
                            *keys
                        } else {
                            *values
                        }
                    }
                    _ => unreachable!(),
                };
                Ok(outcome(self.array(ctx, element)?))
            }
            "reverse" if array => {
                if let Node::Tuple(values) = self.node(receiver) {
                    let mut reversed = Buffer::empty();
                    for &value in values.data.iter().rev() {
                        reversed.push(ctx, value)?;
                    }
                    Ok(outcome(self.tuple(ctx, &reversed.data)?))
                } else {
                    Ok(outcome(receiver))
                }
            }
            "reverse" if string => Ok(outcome(Atom::String.fact())),
            "first" | "last" | "take" | "drop" if array => {
                self.array_end(ctx, receiver, name, args)
            }
            "at" if array => {
                let mut result = outcome(Atom::Never.fact());
                for i in 0..self.arm_count(args[0]) {
                    ctx.charge(1)?;
                    let index = self.arm(args[0], i);
                    let next = if self.atom(index) == Some(Atom::Range) {
                        rejected()
                    } else {
                        self.collection_index(ctx, receiver, &[index])?
                    };
                    self.merge_operation(ctx, &mut result, next)?;
                }
                Ok(result)
            }
            "slice" if array || string => {
                if string && args.len() == 1 {
                    let mut result = outcome(Atom::Never.fact());
                    for i in 0..self.arm_count(args[0]) {
                        ctx.charge(1)?;
                        let index = self.arm(args[0], i);
                        let next = if self.atom(index) == Some(Atom::String) {
                            outcome(self.nullable(ctx, Atom::String.fact())?)
                        } else {
                            self.collection_index(ctx, receiver, &[index])?
                        };
                        self.merge_operation(ctx, &mut result, next)?;
                    }
                    return Ok(result);
                }
                self.collection_index(ctx, receiver, args)
            }
            "getbyte" if string || self.atom(receiver) == Some(Atom::Symbol) => {
                let mut result = outcome(Atom::Never.fact());
                for i in 0..self.arm_count(args[0]) {
                    let index = self.arm(args[0], i);
                    ctx.charge(1)?;
                    let next = match self.atom(index) {
                        Some(Atom::Int | Atom::Float) => {
                            outcome(self.nullable(ctx, Atom::Int.fact())?)
                        }
                        Some(Atom::Unknown | Atom::Any) => outcome(Atom::Unknown.fact()),
                        None => unsupported(),
                        _ => rejected(),
                    };
                    self.merge_operation(ctx, &mut result, next)?;
                }
                Ok(result)
            }
            _ => Ok(rejected()),
        }
    }

    fn array_end(
        &mut self,
        ctx: &mut CallContext,
        receiver: Fact,
        name: &str,
        args: &[Fact],
    ) -> Result<Operation> {
        if args.is_empty() {
            let index = self.integer(ctx, if name == "first" { 0 } else { -1 })?;
            return self.collection_index(ctx, receiver, &[index]);
        }
        let mut result = outcome(Atom::Never.fact());
        for i in 0..self.arm_count(args[0]) {
            ctx.charge(1)?;
            let count = self.arm(args[0], i);
            let next = if let (Node::Tuple(values), Node::Integer(count)) =
                (self.node(receiver), self.node(count))
            {
                if *count < 0 {
                    rejected()
                } else {
                    let count = usize::try_from(*count)
                        .unwrap_or(usize::MAX)
                        .min(values.data.len());
                    let (start, end) = match name {
                        "last" => (values.data.len() - count, values.data.len()),
                        "drop" => (count, values.data.len()),
                        _ => (0, count),
                    };
                    let mut selected = Buffer::empty();
                    selected.extend(ctx, &values.data[start..end])?;
                    outcome(self.tuple(ctx, &selected.data)?)
                }
            } else {
                match self.atom(count) {
                    Some(Atom::Int | Atom::Float) => {
                        let element = self.elements(ctx, receiver)?;
                        outcome(self.array(ctx, element)?)
                    }
                    Some(Atom::Any | Atom::Unknown) => outcome(Atom::Unknown.fact()),
                    None => unsupported(),
                    _ => rejected(),
                }
            };
            self.merge_operation(ctx, &mut result, next)?;
        }
        Ok(result)
    }
}
