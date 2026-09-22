use super::{
    facts::{Atom, Fact, Facts, HashKind, Node},
    integers::Bounds,
    scalar::{Operation, Test},
};
use crate::{CallContext, Result, Value, budget::Buffer, bytecode::CallSite};

mod capture_indexing;
mod leaves;
mod pairing;
mod projection;
mod ranges;
mod reshaping;
mod sets;

/// The longest literal range or window that analysis materializes as an exact
/// tuple; longer results keep their element facts in a general array.
const EXACT: usize = 256;

/// One arm of a count argument after the runtime's integer conversion.
enum Count {
    /// Converts to exactly this integer (literal integers and finite floats).
    Exact(i64),
    /// An integer whose sign or size is only bounded.
    Bounded(Bounds),
    /// A float that converts only when finite and within the integer range.
    Float,
    Invalid,
    Unknown,
    Never,
    Unsupported,
}

fn outcome(value: Fact) -> Operation {
    Operation {
        value,
        rejected: false,
        unsupported: false,
        throws: false,
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

/// A path that may fail at runtime without proving a contradiction.
fn throws() -> Operation {
    Operation {
        throws: true,
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
        into.throws |= next.throws;
        Ok(())
    }

    pub(super) fn plain_hash(&self, value: Fact) -> bool {
        matches!(
            self.node(value),
            Node::Shape(_, _, _, HashKind::PLAIN) | Node::Hash(_, _, HashKind::PLAIN)
        )
    }

    /// The set of provenances a hash fact may have.
    pub(super) fn hash_mode(&self, value: Fact) -> HashKind {
        match self.node(value) {
            Node::Hash(_, _, kind) | Node::Shape(_, _, _, kind) => *kind,
            _ => unreachable!(),
        }
    }

    /// Copies hash facts while preserving fields and replacing their provenance
    /// set; a plain or object copy therefore drops every protected possibility.
    pub(super) fn hash_as(
        &mut self,
        ctx: &mut CallContext,
        value: Fact,
        kind: HashKind,
    ) -> Result<Fact> {
        ctx.charge(1)?;
        match self.node(value) {
            Node::Hash(keys, values, _) => self.hash_kind(ctx, *keys, *values, kind),
            Node::Shape(fields, open, keys, _) => {
                let (open, keys) = (*open, *keys);
                let mut copied = Buffer::empty();
                for field in &fields.data {
                    ctx.charge(1)?;
                    copied.push(
                        ctx,
                        super::facts::Field {
                            name: field.name.clone(),
                            value: field.value,
                            optional: field.optional,
                        },
                    )?;
                }
                self.shape_fields(ctx, copied, open, keys, kind)
            }
            _ => unreachable!(),
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

    pub(super) fn selected_field(
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
        self.collection_index_mode(ctx, receiver, args, false)
    }

    pub(super) fn stored_collection_index(
        &mut self,
        ctx: &mut CallContext,
        receiver: Fact,
        args: &[Fact],
    ) -> Result<Operation> {
        self.collection_index_mode(ctx, receiver, args, true)
    }

    fn collection_index_mode(
        &mut self,
        ctx: &mut CallContext,
        receiver: Fact,
        args: &[Fact],
        stored: bool,
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
                    let next = if stored
                        && length.is_none()
                        && matches!(self.node(root), Node::Hash(..) | Node::Shape(..))
                    {
                        self.stored_hash_index(ctx, root, index)?
                    } else {
                        self.index_arm(ctx, root, index, length)?
                    };
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
        if matches!(self.node(receiver), Node::Protected(..)) {
            return super::builtins::protected::index(ctx, self, receiver, index, length);
        }
        let selector = self.atom(index);
        let unknown = |a| matches!(a, Some(Atom::Unknown | Atom::Any));
        if unknown(self.atom(receiver)) {
            return Ok(outcome(Atom::Unknown.fact()));
        }
        if matches!(
            self.node(receiver),
            Node::Named(_) | Node::Nominal { .. } | Node::Instance { .. }
        ) {
            return Ok(unsupported());
        }
        if receiver == Atom::Never.fact()
            || index == Atom::Never.fact()
            || length == Some(Atom::Never.fact())
        {
            return Ok(outcome(Atom::Never.fact()));
        }
        if length.is_none() && matches!(self.node(receiver), Node::Hash(..) | Node::Shape(..)) {
            return self.hash_index(ctx, receiver, index);
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
        if length.is_none() {
            if let (Node::Tuple(values), Some(bounds)) =
                (self.node(receiver), self.integer_bounds(index))
            {
                let size = values.data.len() as i128;
                let mut selected = Buffer::empty();
                for (index, &value) in values.data.iter().enumerate() {
                    ctx.charge(1)?;
                    if bounds.includes(index as i128) || bounds.includes(index as i128 - size) {
                        selected.push(ctx, value)?;
                    }
                }
                if bounds.min.is_none_or(|n| i128::from(n) < -size)
                    || bounds.max.is_none_or(|n| i128::from(n) >= size)
                {
                    selected.push(ctx, Atom::Nil.fact())?;
                }
                return Ok(outcome(self.union(ctx, &selected.data)?));
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

    /// Models the lookup the runtime performs before evaluating call arguments.
    ///
    /// Missing non-builtin members fail here. Open shapes and general hashes
    /// retain the possible failure without proving it. Scoped calls do not
    /// prepare their member, so their lookup failures follow the arguments.
    pub fn prepare_collection_member(
        &mut self,
        ctx: &mut CallContext,
        receiver: Fact,
        name: &str,
    ) -> Result<Operation> {
        let mut result = outcome(Atom::Never.fact());
        if let Some(variants) = super::objects::variants(ctx, self, receiver, name, false)? {
            for receiver in variants.data {
                let next = self.prepare_collection_member(ctx, receiver, name)?;
                self.merge_operation(ctx, &mut result, next)?;
            }
            return Ok(result);
        }
        let looked_up =
            !crate::members::hash_builtin(name) && !matches!(name, "tap" | "yield_self");
        for i in 0..self.arm_count(receiver) {
            ctx.charge(1)?;
            let arm = self.arm(receiver, i);
            let next = match self.node(arm) {
                Node::Protected(shape, ..) if looked_up => {
                    if self.selected_field(ctx, *shape, name.as_bytes())?.is_none() {
                        rejected()
                    } else {
                        outcome(arm)
                    }
                }
                Node::Shape(_, false, _, _) if looked_up => {
                    match self.selected_field(ctx, arm, name.as_bytes())? {
                        None => rejected(),
                        Some((_, optional)) => Operation {
                            throws: optional,
                            ..outcome(arm)
                        },
                    }
                }
                Node::Shape(_, true, _, _) if looked_up => {
                    if self
                        .selected_field(ctx, arm, name.as_bytes())?
                        .is_some_and(|(_, optional)| !optional)
                    {
                        outcome(arm)
                    } else {
                        Operation {
                            throws: true,
                            ..outcome(arm)
                        }
                    }
                }
                Node::Hash(..) if looked_up => Operation {
                    throws: true,
                    ..outcome(arm)
                },
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
        if let Some(variants) = super::objects::variants(ctx, self, receiver, name, site.scope)? {
            for receiver in variants.data {
                let next = self.collection_member(ctx, receiver, site, name, args)?;
                self.merge_operation(ctx, &mut result, next)?;
            }
            return Ok(result);
        }
        for i in 0..self.arm_count(receiver) {
            ctx.charge(1)?;
            let arm = self.arm(receiver, i);
            let next = self.member_arm(ctx, arm, site, name, args)?;
            self.merge_operation(ctx, &mut result, next)?;
        }
        Ok(result)
    }

    /// Reads or calls a stored field without executing it.
    ///
    /// `certain` distinguishes a field known to be present from one that may be
    /// absent: calling non-callable data is a contradiction only in the first case.
    fn field_operation(
        &mut self,
        ctx: &mut CallContext,
        site: CallSite,
        field: Fact,
        certain: bool,
    ) -> Result<Operation> {
        let data = self.known_non_callable(ctx, field)?;
        Ok(if site.auto && (site.scope || data) {
            outcome(field)
        } else if !site.auto && data {
            if certain { rejected() } else { throws() }
        } else if site.auto && !certain && field == Atom::Unknown.fact() {
            // A possibly present unknown field reads as a gradual value; a stored
            // host method or offset would fail the bare read instead.
            Operation {
                throws: true,
                ..outcome(field)
            }
        } else {
            unsupported()
        })
    }

    /// Dispatches a member after the caller has resolved stored-field overrides.
    ///
    /// The flow walker analyzes possible field paths itself, so this entry skips
    /// [`super::objects::select`] and models only the native alternative.
    pub fn native_member(
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
            let next = match self.member_guards(ctx, arm, site, name, args)? {
                Some(next) => next,
                None => self.native_member_arm(ctx, arm, site, name, args)?,
            };
            self.merge_operation(ctx, &mut result, next)?;
        }
        Ok(result)
    }

    fn member_guards(
        &mut self,
        ctx: &mut CallContext,
        receiver: Fact,
        site: CallSite,
        name: &str,
        args: &[Fact],
    ) -> Result<Option<Operation>> {
        if matches!(self.node(receiver), Node::Protected(..)) {
            let mut arguments = super::arguments::Arguments::new();
            arguments.positional.extend(ctx, args)?;
            let result =
                super::builtins::protected::member(ctx, self, receiver, site, name, &arguments)?;
            return Ok(Some(Operation {
                value: result.value,
                rejected: !result.failures.data.is_empty(),
                unsupported: result.incomplete,
                throws: result.throws != 0 && result.failures.data.is_empty(),
            }));
        }
        if receiver == Atom::Never.fact() {
            return Ok(Some(outcome(receiver)));
        }
        if matches!(
            self.node(receiver),
            Node::Named(_) | Node::Nominal { .. } | Node::Instance { .. }
        ) {
            return Ok(Some(unsupported()));
        }
        Ok(None)
    }

    fn member_arm(
        &mut self,
        ctx: &mut CallContext,
        receiver: Fact,
        site: CallSite,
        name: &str,
        args: &[Fact],
    ) -> Result<Operation> {
        if let Some(result) = self.member_guards(ctx, receiver, site, name, args)? {
            return Ok(result);
        }
        match super::objects::select(ctx, self, receiver, site, name)? {
            Some(super::objects::Selection::Field(field)) => {
                return self.field_operation(ctx, site, field, true);
            }
            Some(super::objects::Selection::Missing) => return Ok(rejected()),
            Some(super::objects::Selection::Uncertain(field)) => {
                let native = super::objects::absent_is_native(site, name);
                let mut result = self.field_operation(ctx, site, field, !native)?;
                let absent = if native {
                    self.native_member_arm(ctx, receiver, site, name, args)?
                } else {
                    throws()
                };
                self.merge_operation(ctx, &mut result, absent)?;
                return Ok(result);
            }
            Some(super::objects::Selection::Native) | None => (),
        }
        self.native_member_arm(ctx, receiver, site, name, args)
    }

    fn native_member_arm(
        &mut self,
        ctx: &mut CallContext,
        receiver: Fact,
        site: CallSite,
        name: &str,
        args: &[Fact],
    ) -> Result<Operation> {
        let array = matches!(self.node(receiver), Node::Array(_) | Node::Tuple(_));
        let hash = matches!(self.node(receiver), Node::Hash(..) | Node::Shape(..));
        let string = self.atom(receiver) == Some(Atom::String);
        let range = matches!(
            self.node(receiver),
            Node::Range(..) | Node::Atom(Atom::Range)
        );
        let unknown = matches!(self.atom(receiver), Some(Atom::Unknown | Atom::Any));
        if site.scope {
            return Ok(if unknown { unsupported() } else { rejected() });
        }
        if hash && !crate::members::hash_builtin(name) {
            // Reached only when the caller resolved the field itself; the walker
            // has already analyzed the stored value or its absence.
            return Ok(outcome(Atom::Never.fact()));
        }
        if array && crate::combinatorics::method(name) {
            return self.combinatoric_member(ctx, receiver, name, args);
        }
        let arity = match name {
            "compact" if array || hash => 0..=0,
            "chunk" | "window" if array => 1..=1,
            "inspect" if array || hash => 0..=0,
            "to_a" | "size" | "exclude_end?" if range => 0..=0,
            "include?" | "cover?" | "member?" if range => 1..=1,
            "to_a" if hash => 0..=0,
            "join" | "flatten" if array => 0..=1,
            "transpose" if array => 0..=0,
            "zip" if array => 0..=usize::MAX,
            "values_at" if array || hash => 0..=usize::MAX,
            "slice" | "except" if hash => 0..=usize::MAX,
            "length" | "size" | "bytesize" | "empty?" | "keys" | "values" | "reverse"
            | "itself" | "dup" | "nil?" => 0..=0,
            "at" | "getbyte" | "take" | "drop" => 1..=1,
            "include?" | "member?" if array => 1..=1,
            "key?" | "has_key?" | "include?" | "member?" if hash => 1..=1,
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
            "key?" | "has_key?" | "include?" | "member?" if hash => {
                self.hash_membership(ctx, receiver, args[0])
            }
            "compact" if array => self.compact_member(ctx, receiver),
            "compact" if hash => self.hash_compact_member(ctx, receiver),
            "chunk" if array => self.chunk_member(ctx, receiver, args[0], false),
            "window" if array => self.chunk_member(ctx, receiver, args[0], true),
            "inspect" if array || hash => self.inspect_member(ctx, receiver),
            "to_a" | "size" | "exclude_end?" | "include?" | "cover?" | "member?" | "first"
            | "last"
                if range =>
            {
                self.range_member(ctx, receiver, name, args)
            }
            "join" if array => self.join_member(ctx, receiver, args),
            "flatten" if array => self.flatten_member(ctx, receiver, args),
            "transpose" if array => self.transpose_member(ctx, receiver),
            "zip" if array => self.zip_member(ctx, receiver, args),
            "values_at" => self.values_at_member(ctx, receiver, args),
            "slice" if hash => self.slice_member(ctx, receiver, args),
            "except" if hash => self.except_member(ctx, receiver, args),
            "to_a" if hash => {
                let iteration = self.iteration(ctx, receiver)?;
                if iteration.unsupported {
                    return Ok(unsupported());
                }
                let value = if iteration.item == Atom::Never.fact() {
                    self.tuple(ctx, &[])?
                } else {
                    self.array(ctx, iteration.item)?
                };
                Ok(outcome(value))
            }
            "include?" | "member?" if array => {
                let Node::Tuple(values) = self.node(receiver) else {
                    return Ok(outcome(Atom::Bool.fact()));
                };
                let mut possible = false;
                for &value in &values.data {
                    ctx.charge(1)?;
                    match self.definitely_equal(value, args[0]) {
                        Some(true) => return Ok(outcome(self.boolean(ctx, true)?)),
                        Some(false) => (),
                        None => possible = true,
                    }
                }
                Ok(outcome(if possible {
                    Atom::Bool.fact()
                } else {
                    self.boolean(ctx, false)?
                }))
            }
            "itself" | "dup" => Ok(outcome(receiver)),
            "nil?" => Ok(outcome(self.test_result(ctx, receiver, Test::Nil)?)),
            "length" | "size" if array || hash || string => {
                let value = match self.node(receiver) {
                    Node::Tuple(values) => match i64::try_from(values.data.len()) {
                        Ok(length) => self.integer(ctx, length)?,
                        Err(_) => Atom::Int.fact(),
                    },
                    Node::Array(_) => self.integer_range(
                        ctx,
                        Bounds {
                            min: Some(0),
                            max: None,
                        },
                    )?,
                    _ => Atom::Int.fact(),
                };
                Ok(outcome(value))
            }
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

    /// Models the natively materialized array members (`sample`, `shuffle`,
    /// `rotate`, `product` and the tuple generators). Counts must be numeric
    /// and product dimensions must be arrays; every result aliases the
    /// receiver's elements without narrowing them. Literal lengths select the
    /// known-empty and single-empty-row results the runtime produces.
    fn combinatoric_member(
        &mut self,
        ctx: &mut CallContext,
        receiver: Fact,
        name: &str,
        args: &[Fact],
    ) -> Result<Operation> {
        let arity = match name {
            "shuffle" => 0..=0,
            "sample" | "rotate" | "permutation" => 0..=1,
            "combination" | "repeated_combination" | "repeated_permutation" => 1..=1,
            _ => 0..=usize::MAX,
        };
        if !arity.contains(&args.len()) {
            return Ok(rejected());
        }
        let element = self.elements(ctx, receiver)?;
        let length = match self.node(receiver) {
            Node::Tuple(values) => Some(values.data.len()),
            _ => None,
        };
        let empty = length == Some(0);
        if name == "product" {
            return self.product_member(ctx, element, args);
        }
        let same_elements = |facts: &mut Self, ctx: &mut CallContext| -> Result<Fact> {
            if empty {
                facts.tuple(ctx, &[])
            } else {
                facts.array(ctx, element)
            }
        };
        if args.is_empty() {
            return Ok(outcome(match name {
                "shuffle" | "rotate" => self.combinatoric_array(ctx, element, length)?,
                "sample" => {
                    if empty {
                        Atom::Nil.fact()
                    } else if length.is_some() {
                        element
                    } else {
                        self.nullable(ctx, element)?
                    }
                }
                _ => {
                    if empty {
                        let row = self.tuple(ctx, &[])?;
                        self.tuple(ctx, &[row])?
                    } else {
                        let row = self.array(ctx, element)?;
                        self.array(ctx, row)?
                    }
                }
            }));
        }
        let mut result = outcome(Atom::Never.fact());
        for i in 0..self.arm_count(args[0]) {
            ctx.charge(1)?;
            let count = self.arm(args[0], i);
            let (bounds, uncertain) = match self.count_arm(count, name == "sample") {
                Count::Exact(value) => (Some(Bounds::point(value)), false),
                Count::Bounded(bounds) => {
                    (Some(bounds), bounds.min.is_none() || bounds.max.is_none())
                }
                Count::Float => (None, true),
                Count::Unknown => {
                    let value = match name {
                        "rotate" => self.combinatoric_array(ctx, element, length)?,
                        "sample" => same_elements(self, ctx)?,
                        _ => {
                            let row = self.array(ctx, element)?;
                            self.array(ctx, row)?
                        }
                    };
                    self.merge_operation(
                        ctx,
                        &mut result,
                        Operation {
                            throws: true,
                            ..outcome(value)
                        },
                    )?;
                    continue;
                }
                Count::Never => continue,
                Count::Invalid => {
                    self.merge_operation(ctx, &mut result, rejected())?;
                    continue;
                }
                Count::Unsupported => {
                    self.merge_operation(ctx, &mut result, unsupported())?;
                    continue;
                }
            };
            let negative = bounds.is_some_and(|b| b.max.is_some_and(|max| max < 0));
            let non_negative = bounds.is_some_and(|b| b.min.is_some_and(|min| min >= 0));
            let zero = bounds == Some(Bounds::point(0));
            let exact = bounds.and_then(|b| b.min.filter(|_| b.min == b.max));
            let mut next = match name {
                "rotate" => outcome(self.combinatoric_array(ctx, element, length)?),
                "sample" => {
                    if negative {
                        rejected()
                    } else if zero {
                        outcome(self.tuple(ctx, &[])?)
                    } else if let (Some(count), Some(length)) = (exact, length) {
                        let count = usize::try_from(count).unwrap_or(usize::MAX).min(length);
                        outcome(self.combinatoric_array(ctx, element, Some(count))?)
                    } else {
                        // A count that may still be negative fails only at runtime.
                        Operation {
                            throws: !non_negative,
                            ..outcome(same_elements(self, ctx)?)
                        }
                    }
                }
                _ => {
                    let repeated = name.starts_with("repeated_");
                    let min = bounds.and_then(|b| b.min);
                    let known_empty = negative
                        || (repeated && empty && min.is_some_and(|min| min > 0))
                        || (!repeated
                            && length.is_some_and(|length| {
                                min.is_some_and(|min| {
                                    u64::try_from(min).is_ok_and(|v| v > length as u64)
                                })
                            }));
                    if known_empty {
                        outcome(self.tuple(ctx, &[])?)
                    } else if zero {
                        let row = self.tuple(ctx, &[])?;
                        outcome(self.tuple(ctx, &[row])?)
                    } else {
                        let row = self.array(ctx, element)?;
                        outcome(self.array(ctx, row)?)
                    }
                }
            };
            // A broad integer domain includes big integers; floats must be
            // finite. Keep their possible index-conversion failures.
            next.throws |= uncertain;
            self.merge_operation(ctx, &mut result, next)?;
        }
        Ok(result)
    }

    fn combinatoric_array(
        &mut self,
        ctx: &mut CallContext,
        element: Fact,
        length: Option<usize>,
    ) -> Result<Fact> {
        let Some(length) = length else {
            return self.array(ctx, element);
        };
        let mut values = Buffer::with_capacity(ctx, length)?;
        for _ in 0..length {
            ctx.charge(1)?;
            values.data.push(element);
        }
        self.tuple(ctx, &values.data)
    }

    /// Classifies one count argument arm the way the runtime's integer
    /// conversion does. `sample` additionally accepts a float equal to
    /// `2^63`, which the other members reject.
    fn count_arm(&self, count: Fact, sample: bool) -> Count {
        const LIMIT: f64 = 9_223_372_036_854_775_808.0;
        match self.node(count) {
            Node::Integer(value) => Count::Exact(*value),
            Node::IntegerBounds(bounds) => Count::Bounded(*bounds),
            Node::Atom(Atom::Int) => Count::Bounded(Bounds::ALL),
            Node::Float(bits) => {
                let value = f64::from_bits(*bits);
                if !value.is_finite() || value < i64::MIN as f64 {
                    Count::Invalid
                } else if value < LIMIT {
                    Count::Exact(value as i64)
                } else if sample && value == LIMIT {
                    Count::Exact(i64::MAX)
                } else {
                    Count::Invalid
                }
            }
            Node::Atom(Atom::Float) => Count::Float,
            Node::Atom(Atom::Unknown | Atom::Any) => Count::Unknown,
            Node::Atom(Atom::Never) => Count::Never,
            Node::Array(_)
            | Node::Tuple(_)
            | Node::Hash(..)
            | Node::Shape(..)
            | Node::Protected(..) => Count::Invalid,
            _ if self.atom(count).is_some() => Count::Invalid,
            _ => Count::Unsupported,
        }
    }

    /// Models `product`: every supplied dimension is validated before any
    /// empty shortcut applies, mirroring the runtime. Valid and failing arms
    /// of each argument are recorded independently so a rejected alternative
    /// neither hides a later invalid argument nor erases the successful
    /// result; unknown dimensions admit the call without dropping known
    /// failures.
    fn product_member(
        &mut self,
        ctx: &mut CallContext,
        element: Fact,
        args: &[Fact],
    ) -> Result<Operation> {
        let mut result = outcome(Atom::Never.fact());
        let mut dims = Buffer::empty();
        dims.push(ctx, element)?;
        let mut vacant = element == Atom::Never.fact();
        let mut viable = true;
        for &arg in args {
            ctx.charge(1)?;
            let mut alternatives = Buffer::empty();
            let mut admitted = false;
            let mut certainly_empty = true;
            for i in 0..self.arm_count(arg) {
                ctx.charge(1)?;
                let arm = self.arm(arg, i);
                match self.node(arm) {
                    Node::Array(_) | Node::Tuple(_) => {
                        let items = self.elements(ctx, arm)?;
                        admitted = true;
                        certainly_empty &= items == Atom::Never.fact();
                        alternatives.push(ctx, items)?;
                    }
                    Node::Atom(Atom::Unknown | Atom::Any) => {
                        admitted = true;
                        certainly_empty = false;
                        result.throws = true;
                        alternatives.push(ctx, Atom::Unknown.fact())?;
                    }
                    Node::Atom(Atom::Never) => (),
                    Node::Hash(..) | Node::Shape(..) | Node::Protected(..) => {
                        result.rejected = true;
                    }
                    _ if self.atom(arm).is_some() => result.rejected = true,
                    _ => result.unsupported = true,
                }
            }
            if !admitted {
                viable = false;
                continue;
            }
            vacant |= certainly_empty;
            let dim = self.union(ctx, &alternatives.data)?;
            dims.push(ctx, dim)?;
        }
        if !viable {
            return Ok(result);
        }
        result.value = if vacant {
            self.tuple(ctx, &[])?
        } else {
            let row = self.tuple(ctx, &dims.data)?;
            self.array(ctx, row)?
        };
        Ok(result)
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
