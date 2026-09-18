use super::facts::{Atom, Fact, Facts, Field, Node};
use crate::{
    CallContext, Result,
    budget::Buffer,
    bytecode::{CallSite, Method},
};

#[derive(Debug)]
pub(super) struct Mutation {
    pub receiver: Fact,
    pub value: Fact,
    pub rejected: bool,
    pub unsupported: bool,
}

impl Mutation {
    fn new(receiver: Fact, value: Fact) -> Self {
        Self {
            receiver,
            value,
            rejected: false,
            unsupported: false,
        }
    }

    fn empty() -> Self {
        Self::new(Atom::Never.fact(), Atom::Never.fact())
    }

    fn rejected() -> Self {
        Self {
            rejected: true,
            ..Self::empty()
        }
    }

    fn unsupported() -> Self {
        Self {
            unsupported: true,
            ..Self::empty()
        }
    }

    fn updated(receiver: Fact) -> Self {
        Self::new(receiver, receiver)
    }
}

impl Facts {
    pub fn collection_mutation_member(
        &mut self,
        ctx: &mut CallContext,
        receiver: Fact,
        site: CallSite,
        name: &str,
        args: &[Fact],
    ) -> Result<Mutation> {
        ctx.checkpoint()?;
        let mut result = Mutation::empty();
        for i in 0..self.arm_count(receiver) {
            ctx.charge(1)?;
            let arm = self.arm(receiver, i);
            let hash_field = matches!(self.node(arm), Node::Hash(..) | Node::Shape(..))
                && !crate::members::hash_builtin(name);
            let next =
                if self.atom(arm) == Some(Atom::String) && matches!(name, "unshift" | "append") {
                    Mutation::rejected()
                } else if hash_field || site.scope {
                    let next = self.collection_member(ctx, arm, site, name, args)?;
                    Mutation {
                        receiver: if next.value == Atom::Never.fact() {
                            Atom::Never.fact()
                        } else {
                            arm
                        },
                        value: next.value,
                        rejected: next.rejected,
                        unsupported: next.unsupported,
                    }
                } else if let Some(method) = site.method {
                    self.collection_mutate(ctx, arm, method, args)?
                } else {
                    Mutation::unsupported()
                };
            self.merge_mutation(ctx, &mut result, next)?;
        }
        Ok(result)
    }

    fn merge_mutation(
        &mut self,
        ctx: &mut CallContext,
        into: &mut Mutation,
        next: Mutation,
    ) -> Result<()> {
        into.receiver = self.union(ctx, &[into.receiver, next.receiver])?;
        into.value = self.union(ctx, &[into.value, next.value])?;
        into.rejected |= next.rejected;
        into.unsupported |= next.unsupported;
        Ok(())
    }

    pub fn collection_write(
        &mut self,
        ctx: &mut CallContext,
        receiver: Fact,
        index: Fact,
        value: Fact,
    ) -> Result<Mutation> {
        ctx.checkpoint()?;
        let mut result = Mutation::empty();
        if value == Atom::Never.fact() {
            return Ok(result);
        }
        for i in 0..self.arm_count(receiver) {
            for j in 0..self.arm_count(index) {
                ctx.charge(1)?;
                let next = self.write_arm(ctx, self.arm(receiver, i), self.arm(index, j), value)?;
                self.merge_mutation(ctx, &mut result, next)?;
            }
        }
        Ok(result)
    }

    fn write_arm(
        &mut self,
        ctx: &mut CallContext,
        receiver: Fact,
        index: Fact,
        value: Fact,
    ) -> Result<Mutation> {
        if receiver == Atom::Never.fact() || index == Atom::Never.fact() {
            return Ok(Mutation::empty());
        }
        match self.node(receiver) {
            Node::Atom(Atom::Unknown | Atom::Any) => {
                return Ok(Mutation::new(Atom::Unknown.fact(), value));
            }
            Node::Named(_) | Node::Nominal { .. } => return Ok(Mutation::unsupported()),
            Node::Hash(..) | Node::Shape(..) => {
                if !self.plain_hash(receiver) {
                    return Ok(Mutation::unsupported());
                }
                return self.hash_write(ctx, receiver, index, value);
            }
            Node::Tuple(_) | Node::Array(_) => (),
            _ => return Ok(Mutation::rejected()),
        }
        if let Some(failure) = self.numeric_selector(index) {
            return Ok(failure);
        }
        let updated = match self.node(receiver) {
            Node::Tuple(elements) => {
                if elements.data.is_empty() {
                    return Ok(Mutation::rejected());
                }
                let at = if let Node::Integer(index) = self.node(index) {
                    let at = offset(*index, elements.data.len(), false);
                    if at < 0 || at >= elements.data.len() as i128 {
                        return Ok(Mutation::rejected());
                    }
                    Some(at as usize)
                } else {
                    None
                };
                let mut values = Buffer::empty();
                values.extend(ctx, &elements.data)?;
                for (i, item) in values.data.iter_mut().enumerate() {
                    ctx.charge(1)?;
                    if at == Some(i) {
                        *item = value;
                    } else if at.is_none() {
                        *item = self.union(ctx, &[*item, value])?;
                    }
                }
                self.tuple(ctx, &values.data)?
            }
            Node::Array(element) => {
                if *element == Atom::Never.fact() {
                    return Ok(Mutation::rejected());
                }
                let element = self.union(ctx, &[*element, value])?;
                self.array(ctx, element)?
            }
            _ => unreachable!(),
        };
        Ok(Mutation::new(updated, value))
    }

    fn hash_contents(&mut self, ctx: &mut CallContext, receiver: Fact) -> Result<(Fact, Fact)> {
        match self.node(receiver) {
            Node::Hash(keys, values, _) => Ok((*keys, *values)),
            Node::Shape(_, _, keys, _) => {
                let keys = *keys;
                Ok((keys, self.shape_values(ctx, receiver, false)?))
            }
            _ => unreachable!(),
        }
    }

    fn hash_write(
        &mut self,
        ctx: &mut CallContext,
        receiver: Fact,
        index: Fact,
        value: Fact,
    ) -> Result<Mutation> {
        match self.atom(index) {
            Some(Atom::String | Atom::Symbol | Atom::Unknown | Atom::Any) => (),
            None if matches!(self.node(index), Node::Named(_) | Node::Nominal { .. }) => {
                return Ok(Mutation::unsupported());
            }
            _ => return Ok(Mutation::rejected()),
        }
        if let (Node::Shape(fields, open, keys, plain), Node::String(key) | Node::Symbol(key)) =
            (self.node(receiver), self.node(index))
        {
            let (open, plain, keys, key) = (*open, *plain, *keys, key.clone());
            let mut copied = Buffer::empty();
            for field in &fields.data {
                ctx.charge(1)?;
                copied.push(
                    ctx,
                    Field {
                        name: field.name.clone(),
                        value: field.value,
                        optional: field.optional,
                    },
                )?;
            }
            copied.push(
                ctx,
                Field {
                    name: key,
                    value,
                    optional: false,
                },
            )?;
            let keys = self.union(ctx, &[keys, Atom::String.fact()])?;
            let updated = self.shape_fields(ctx, copied, open, keys, plain)?;
            return Ok(Mutation::new(updated, value));
        }
        let (keys, values) = self.hash_contents(ctx, receiver)?;
        let keys = self.union(ctx, &[keys, Atom::String.fact()])?;
        let values = self.union(ctx, &[values, value])?;
        Ok(Mutation::new(
            self.hash_kind(ctx, keys, values, true)?,
            value,
        ))
    }

    pub fn collection_mutate(
        &mut self,
        ctx: &mut CallContext,
        receiver: Fact,
        method: Method,
        args: &[Fact],
    ) -> Result<Mutation> {
        ctx.checkpoint()?;
        let mut result = Mutation::empty();
        for &arg in args {
            ctx.charge(1)?;
            if arg == Atom::Never.fact() {
                return Ok(result);
            }
        }
        for i in 0..self.arm_count(receiver) {
            ctx.charge(1)?;
            let next = self.mutate_arm(ctx, self.arm(receiver, i), method, args)?;
            self.merge_mutation(ctx, &mut result, next)?;
        }
        Ok(result)
    }

    fn mutate_arm(
        &mut self,
        ctx: &mut CallContext,
        receiver: Fact,
        method: Method,
        args: &[Fact],
    ) -> Result<Mutation> {
        use Method::*;
        match self.node(receiver) {
            Node::Atom(Atom::Never) => return Ok(Mutation::empty()),
            Node::Atom(Atom::Unknown | Atom::Any) => {
                return Ok(Mutation::new(Atom::Unknown.fact(), Atom::Unknown.fact()));
            }
            Node::Named(_) | Node::Nominal { .. } => return Ok(Mutation::unsupported()),
            Node::String(_) | Node::Atom(Atom::String) => {
                return self.string_mutation(ctx, receiver, method, args);
            }
            Node::Hash(..) | Node::Shape(..) => {
                if !self.plain_hash(receiver) {
                    return Ok(Mutation::unsupported());
                }
                return self.hash_mutation(ctx, receiver, method, args);
            }
            Node::Tuple(_) | Node::Array(_) => (),
            _ => return Ok(Mutation::rejected()),
        }
        match method {
            Push | Prepend => {
                self.array_insert(ctx, receiver, matches!(method, Prepend), None, args)
            }
            Clear if args.is_empty() => Ok(Mutation::updated(self.tuple(ctx, &[])?)),
            Insert if !args.is_empty() => {
                let mut result = Mutation::empty();
                for i in 0..self.arm_count(args[0]) {
                    ctx.charge(1)?;
                    let index = self.arm(args[0], i);
                    let next = self.array_insert(ctx, receiver, false, Some(index), &args[1..])?;
                    self.merge_mutation(ctx, &mut result, next)?;
                }
                Ok(result)
            }
            Pop | Shift if args.len() <= 1 => {
                let mut result = Mutation::empty();
                for i in 0..args.first().map_or(1, |&count| self.arm_count(count)) {
                    ctx.charge(1)?;
                    let count = args.first().map(|&count| self.arm(count, i));
                    let next = self.array_remove(ctx, receiver, matches!(method, Shift), count)?;
                    self.merge_mutation(ctx, &mut result, next)?;
                }
                Ok(result)
            }
            Delete if args.len() == 1 => {
                let mut result = Mutation::empty();
                for i in 0..self.arm_count(args[0]) {
                    ctx.charge(1)?;
                    let next = self.array_delete(ctx, receiver, self.arm(args[0], i))?;
                    self.merge_mutation(ctx, &mut result, next)?;
                }
                Ok(result)
            }
            Fill if (1..=3).contains(&args.len()) => {
                let mut result = Mutation::empty();
                for i in 0..args.get(1).map_or(1, |&v| self.arm_count(v)) {
                    for j in 0..args.get(2).map_or(1, |&v| self.arm_count(v)) {
                        ctx.charge(1)?;
                        let start = args.get(1).map(|&v| self.arm(v, i));
                        let count = args.get(2).map(|&v| self.arm(v, j));
                        let next = self.array_fill(ctx, receiver, args[0], start, count)?;
                        self.merge_mutation(ctx, &mut result, next)?;
                    }
                }
                Ok(result)
            }
            _ => Ok(Mutation::rejected()),
        }
    }

    fn numeric_selector(&self, index: Fact) -> Option<Mutation> {
        match self.atom(index) {
            Some(Atom::Never) => Some(Mutation::empty()),
            Some(Atom::Int | Atom::Float | Atom::Unknown | Atom::Any) => None,
            None if matches!(self.node(index), Node::Named(_) | Node::Nominal { .. }) => {
                Some(Mutation::unsupported())
            }
            _ => Some(Mutation::rejected()),
        }
    }

    fn array_insert(
        &mut self,
        ctx: &mut CallContext,
        receiver: Fact,
        front: bool,
        index: Option<Fact>,
        args: &[Fact],
    ) -> Result<Mutation> {
        if let Some(failure) = index.and_then(|index| self.numeric_selector(index)) {
            return Ok(failure);
        }
        if args.is_empty() {
            return Ok(Mutation::updated(receiver));
        }
        let mut gap = false;
        if let Node::Tuple(elements) = self.node(receiver) {
            let at = match index.map(|index| self.node(index)) {
                Some(Node::Integer(index)) => Some(offset(*index, elements.data.len(), true)),
                Some(_) => None,
                None => Some(if front {
                    0
                } else {
                    elements.data.len() as i128
                }),
            };
            if let Some(at) = at {
                if at < 0 {
                    return Ok(Mutation::rejected());
                }
                if at <= elements.data.len() as i128 {
                    let mut out = Buffer::empty();
                    out.extend(ctx, &elements.data[..at as usize])?;
                    out.extend(ctx, args)?;
                    out.extend(ctx, &elements.data[at as usize..])?;
                    return Ok(Mutation::updated(self.tuple(ctx, &out.data)?));
                }
            }
            gap = true;
        } else if index.is_some() {
            gap = true;
        }
        let old = self.elements(ctx, receiver)?;
        let mut values = Buffer::empty();
        values.push(ctx, old)?;
        values.extend(ctx, args)?;
        if gap {
            values.push(ctx, Atom::Nil.fact())?;
        }
        let element = self.union(ctx, &values.data)?;
        Ok(Mutation::updated(self.array(ctx, element)?))
    }

    fn array_remove(
        &mut self,
        ctx: &mut CallContext,
        receiver: Fact,
        front: bool,
        count: Option<Fact>,
    ) -> Result<Mutation> {
        if let Some(failure) = count.and_then(|count| self.numeric_selector(count)) {
            return Ok(failure);
        }
        let known = match count.map(|count| self.node(count)) {
            Some(Node::Integer(count)) if *count < 0 => return Ok(Mutation::rejected()),
            Some(Node::Integer(count)) => Some(usize::try_from(*count).unwrap_or(usize::MAX)),
            Some(_) => None,
            None => Some(1),
        };
        if let (Node::Tuple(elements), Some(n)) = (self.node(receiver), known) {
            let n = n.min(elements.data.len());
            let (remaining, removed) = if front {
                (&elements.data[n..], &elements.data[..n])
            } else {
                (
                    &elements.data[..elements.data.len() - n],
                    &elements.data[elements.data.len() - n..],
                )
            };
            let mut rest = Buffer::empty();
            let mut out = Buffer::empty();
            rest.extend(ctx, remaining)?;
            out.extend(ctx, removed)?;
            let remaining = self.tuple(ctx, &rest.data)?;
            let value = if count.is_some() {
                self.tuple(ctx, &out.data)?
            } else {
                out.data.first().copied().unwrap_or(Atom::Nil.fact())
            };
            return Ok(Mutation::new(remaining, value));
        }
        if known == Some(0) {
            return Ok(Mutation::new(receiver, self.tuple(ctx, &[])?));
        }
        let element = self.elements(ctx, receiver)?;
        let remaining = self.array(ctx, element)?;
        let value = if count.is_some() {
            remaining
        } else {
            self.nullable(ctx, element)?
        };
        Ok(Mutation::new(remaining, value))
    }

    pub(super) fn definitely_equal(&self, a: Fact, b: Fact) -> Option<bool> {
        if let (
            Node::EnumMember {
                enumeration: a,
                index: ai,
            },
            Node::EnumMember {
                enumeration: b,
                index: bi,
            },
        ) = (self.node(a), self.node(b))
        {
            return if a != b {
                Some(false)
            } else if let (Some(a), Some(b)) = (ai, bi) {
                Some(a == b)
            } else {
                None
            };
        }
        // Canonical IDs prove equal values only for singleton facts, never shared runtime storage.
        if self.singleton(a) && self.singleton(b) {
            return Some(a == b);
        }
        for (enumeration, other) in [(a, b), (b, a)] {
            if matches!(
                self.node(enumeration),
                Node::Enumeration { .. } | Node::EnumMember { .. }
            ) && !matches!(
                self.node(other),
                Node::Union(_)
                    | Node::Atom(Atom::Unknown | Atom::Any)
                    | Node::Named(_)
                    | Node::Nominal { .. }
            ) {
                return Some(false);
            }
        }
        let collection = |value| match self.node(value) {
            Node::Array(_) | Node::Tuple(_) => Some(false),
            Node::Hash(..) | Node::Shape(..) => Some(true),
            _ => None,
        };
        match (collection(a), collection(b)) {
            (Some(a), Some(b)) if a != b => return Some(false),
            (Some(_), None)
                if self
                    .atom(b)
                    .is_some_and(|a| !matches!(a, Atom::Any | Atom::Unknown)) =>
            {
                return Some(false);
            }
            (None, Some(_))
                if self
                    .atom(a)
                    .is_some_and(|a| !matches!(a, Atom::Any | Atom::Unknown)) =>
            {
                return Some(false);
            }
            _ => (),
        }
        if let (Some(a), Some(b)) = (self.atom(a), self.atom(b)) {
            if matches!(a, Atom::Unknown | Atom::Any) || matches!(b, Atom::Unknown | Atom::Any) {
                return None;
            }
            if a != b && !matches!((a, b), (Atom::Int, Atom::Float) | (Atom::Float, Atom::Int)) {
                return Some(false);
            }
        }
        None
    }

    fn array_delete(
        &mut self,
        ctx: &mut CallContext,
        receiver: Fact,
        target: Fact,
    ) -> Result<Mutation> {
        let mut items = Buffer::empty();
        let tuple = matches!(self.node(receiver), Node::Tuple(_));
        match self.node(receiver) {
            Node::Tuple(values) => items.extend(ctx, &values.data)?,
            Node::Array(element) => items.push(ctx, *element)?,
            _ => unreachable!(),
        }
        let mut kept = Buffer::empty();
        let mut removed = Buffer::empty();
        let mut uncertain = !tuple;
        let mut definite = false;
        for &item in &items.data {
            let mut remaining = Buffer::empty();
            let mut matched = Buffer::empty();
            let mut may_keep = false;
            for i in 0..self.arm_count(item) {
                ctx.charge(1)?;
                let arm = self.arm(item, i);
                if arm == Atom::Never.fact() {
                    continue;
                }
                let equal = self.definitely_equal(arm, target);
                if equal != Some(true) {
                    remaining.push(ctx, arm)?;
                    may_keep = true;
                }
                if equal != Some(false) {
                    matched.push(ctx, arm)?;
                }
            }
            let can_remove = !matched.data.is_empty();
            uncertain |= may_keep && can_remove;
            definite |= !may_keep && can_remove && tuple;
            if may_keep {
                let value = self.union(ctx, &remaining.data)?;
                kept.push(ctx, value)?;
            }
            removed.extend(ctx, &matched.data)?;
        }
        if !definite {
            removed.push(ctx, Atom::Nil.fact())?;
        }
        let receiver = if uncertain {
            let element = self.union(ctx, &kept.data)?;
            self.array(ctx, element)?
        } else {
            self.tuple(ctx, &kept.data)?
        };
        Ok(Mutation::new(receiver, self.union(ctx, &removed.data)?))
    }

    fn array_fill(
        &mut self,
        ctx: &mut CallContext,
        receiver: Fact,
        value: Fact,
        start: Option<Fact>,
        count: Option<Fact>,
    ) -> Result<Mutation> {
        let range = start.is_some_and(|start| self.atom(start) == Some(Atom::Range));
        if range && count.is_some() {
            return Ok(Mutation::rejected());
        }
        for selector in [start, count].into_iter().flatten() {
            if self.atom(selector) == Some(Atom::Nil) || (range && Some(selector) == start) {
                continue;
            }
            if let Some(failure) = self.numeric_selector(selector) {
                return Ok(failure);
            }
        }
        let scalar = |fact: Option<Fact>| match fact.map(|fact| self.node(fact)) {
            None | Some(Node::Atom(Atom::Nil)) => Some(None),
            Some(Node::Integer(n)) => Some(Some(*n)),
            _ => None,
        };
        let (known_start, known_count) = (scalar(start), scalar(count));
        if known_count.is_some_and(|count| count.is_some_and(|count| count < 0)) {
            return Ok(Mutation::updated(receiver));
        }
        if !range
            && known_start.is_some_and(|start| start.unwrap_or(0) == 0)
            && known_count == Some(None)
            && matches!(self.node(receiver), Node::Array(_))
        {
            return Ok(Mutation::updated(self.array(ctx, value)?));
        }
        if known_count == Some(Some(0)) && known_start.is_some_and(|start| start.unwrap_or(0) <= 0)
        {
            return Ok(Mutation::updated(receiver));
        }
        if let (Node::Tuple(elements), Some(start), Some(count)) =
            (self.node(receiver), known_start, known_count)
        {
            let length = elements.data.len() as i128;
            let start = i128::from(start.unwrap_or(0));
            let start = if start < 0 {
                (start + length).max(0)
            } else {
                start
            };
            let count = count.map_or(length - start, i128::from);
            if count < 0 {
                return Ok(Mutation::updated(receiver));
            }
            let end = start + count;
            if end > isize::MAX as i128 {
                return Ok(Mutation::rejected());
            }
            if end <= length {
                let mut copied = Buffer::empty();
                copied.extend(ctx, &elements.data)?;
                for item in &mut copied.data[start as usize..end as usize] {
                    ctx.charge(1)?;
                    *item = value;
                }
                return Ok(Mutation::updated(self.tuple(ctx, &copied.data)?));
            }
            let old = self.elements(ctx, receiver)?;
            let values = self.union(
                ctx,
                &[
                    old,
                    if count > 0 { value } else { Atom::Never.fact() },
                    if start > length {
                        Atom::Nil.fact()
                    } else {
                        Atom::Never.fact()
                    },
                ],
            )?;
            return Ok(Mutation::updated(self.array(ctx, values)?));
        }
        let old = self.elements(ctx, receiver)?;
        let value = if known_count == Some(Some(0)) {
            Atom::Never.fact()
        } else {
            value
        };
        let values = self.union(ctx, &[old, value, Atom::Nil.fact()])?;
        Ok(Mutation::updated(self.array(ctx, values)?))
    }

    fn hash_mutation(
        &mut self,
        ctx: &mut CallContext,
        receiver: Fact,
        method: Method,
        args: &[Fact],
    ) -> Result<Mutation> {
        match method {
            Method::Clear if args.is_empty() => {
                Ok(Mutation::updated(self.shape(ctx, &[], false)?))
            }
            Method::Store if args.len() == 2 => {
                self.collection_write(ctx, receiver, args[0], args[1])
            }
            Method::Replace if args.len() == 1 => {
                let mut result = Mutation::empty();
                for i in 0..self.arm_count(args[0]) {
                    ctx.charge(1)?;
                    let value = self.arm(args[0], i);
                    let next = match self.node(value) {
                        Node::Hash(..) | Node::Shape(..) if self.plain_hash(value) => {
                            Mutation::updated(value)
                        }
                        Node::Hash(..)
                        | Node::Shape(..)
                        | Node::Named(_)
                        | Node::Nominal { .. }
                        | Node::Atom(Atom::Unknown | Atom::Any) => Mutation::unsupported(),
                        _ => Mutation::rejected(),
                    };
                    self.merge_mutation(ctx, &mut result, next)?;
                }
                Ok(result)
            }
            Method::Delete if args.len() == 1 => {
                let mut result = Mutation::empty();
                for i in 0..self.arm_count(args[0]) {
                    ctx.charge(1)?;
                    let next = self.hash_delete(ctx, receiver, self.arm(args[0], i))?;
                    self.merge_mutation(ctx, &mut result, next)?;
                }
                Ok(result)
            }
            _ => Ok(Mutation::rejected()),
        }
    }

    fn hash_delete(
        &mut self,
        ctx: &mut CallContext,
        receiver: Fact,
        key: Fact,
    ) -> Result<Mutation> {
        match self.atom(key) {
            Some(Atom::String | Atom::Symbol | Atom::Unknown | Atom::Any) => (),
            None if matches!(self.node(key), Node::Named(_) | Node::Nominal { .. }) => {
                return Ok(Mutation::unsupported());
            }
            _ => return Ok(Mutation::rejected()),
        }
        if let Node::Hash(_, value, _) = self.node(receiver) {
            let result = self.nullable(ctx, *value)?;
            return Ok(Mutation::new(receiver, result));
        }
        let literal = match self.node(key) {
            Node::String(key) | Node::Symbol(key) => Some(key.clone()),
            _ => None,
        };
        let Node::Shape(fields, open, keys, plain) = self.node(receiver) else {
            unreachable!()
        };
        let (open, keys, plain) = (*open, *keys, *plain);
        let mut copied = Buffer::empty();
        let mut removed = Buffer::empty();
        if open {
            removed.push(ctx, Atom::Unknown.fact())?;
        }
        let mut definite = false;
        for field in &fields.data {
            ctx.charge(1)?;
            let selected = if let Some(key) = &literal {
                super::facts::same_bytes(ctx, key, &field.name)?
            } else {
                false
            };
            if selected || literal.is_none() {
                removed.push(ctx, field.value)?;
            }
            if selected {
                definite |= !field.optional;
            } else {
                copied.push(
                    ctx,
                    Field {
                        name: field.name.clone(),
                        value: field.value,
                        optional: field.optional || literal.is_none(),
                    },
                )?;
            }
        }
        if !definite {
            removed.push(ctx, Atom::Nil.fact())?;
        }
        let value = self.union(ctx, &removed.data)?;
        let receiver = self.shape_fields(ctx, copied, open, keys, plain)?;
        Ok(Mutation::new(receiver, value))
    }

    fn string_mutation(
        &mut self,
        ctx: &mut CallContext,
        receiver: Fact,
        method: Method,
        args: &[Fact],
    ) -> Result<Mutation> {
        let (strings, index) = match method {
            Method::Clear if args.is_empty() => {
                return Ok(Mutation::new(receiver, self.string(ctx, b"")?));
            }
            Method::Replace if args.len() == 1 => (args, None),
            Method::Prepend => (args, None),
            Method::Insert if args.len() == 2 => (&args[1..], Some(args[0])),
            _ => return Ok(Mutation::rejected()),
        };
        let mut result = Mutation::new(receiver, Atom::String.fact());
        for &value in strings {
            let mut accepted = Buffer::empty();
            for i in 0..self.arm_count(value) {
                ctx.charge(1)?;
                let arm = self.arm(value, i);
                match self.atom(arm) {
                    Some(Atom::String) => accepted.push(ctx, arm)?,
                    Some(Atom::Unknown | Atom::Any) => accepted.push(ctx, Atom::String.fact())?,
                    None if matches!(self.node(arm), Node::Named(_) | Node::Nominal { .. }) => {
                        result.unsupported = true
                    }
                    _ => result.rejected = true,
                }
            }
            if accepted.data.is_empty() {
                result.receiver = Atom::Never.fact();
                result.value = Atom::Never.fact();
            } else if matches!(method, Method::Replace) {
                result.value = self.union(ctx, &accepted.data)?;
            }
        }
        if strings.is_empty() {
            result.value = receiver;
        }
        if let Some(index) = index {
            let mut possible = false;
            for i in 0..self.arm_count(index) {
                ctx.charge(1)?;
                let index = self.arm(index, i);
                if let Some(failure) = self.numeric_selector(index) {
                    result.rejected |= failure.rejected;
                    result.unsupported |= failure.unsupported;
                    continue;
                }
                if let (Node::String(text), Node::Integer(index)) =
                    (self.node(receiver), self.node(index))
                {
                    let len = crate::ops::runes(ctx, text.as_bytes().unwrap())?.0;
                    let index = offset(*index, len, true);
                    if index < 0 || index > len as i128 {
                        result.rejected = true;
                        continue;
                    }
                }
                possible = true;
            }
            if !possible {
                result.receiver = Atom::Never.fact();
                result.value = Atom::Never.fact();
            }
        }
        Ok(result)
    }
}

fn offset(index: i64, length: usize, insertion: bool) -> i128 {
    if index < 0 {
        i128::from(index) + length as i128 + i128::from(insertion)
    } else {
        i128::from(index)
    }
}
