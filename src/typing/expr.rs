//! Checks expressions: literals, names, operators, indexing, conditionals,
//! `case` and `begin`/`rescue`.

use super::{
    Checker,
    assigns::TrySpans,
    check::{Purpose, Want, is_constant},
    flow::{Branch, VarState},
    program::{FnId, NsId},
    sigs,
    ty::{Field, Kind, Ty, Types},
};
use crate::{
    diagnostic::{Code, Diagnostic, Edit, Fix, Span},
    syntax::{Expr, Node, Stmt, Try, When},
    value::Kind as ValueKind,
};

impl<'a> Checker<'a> {
    /// The type of an expression. `hint` is the type the position expects,
    /// which gives `nil`, empty literals and symbols naming enum members
    /// their type; it is not checked here.
    pub(super) fn expr(&mut self, expr: &'a Expr, hint: Option<Ty>) -> Ty {
        self.expr_want(expr, Want::Infer(hint))
    }

    /// Checks that an expression's value is assignable to `expected`,
    /// reporting at the innermost expression that produces the value.
    pub(super) fn expr_against(&mut self, expr: &'a Expr, expected: Ty, purpose: &Purpose) -> Ty {
        match &expr.node {
            Node::Conditional(..) | Node::Case(..) | Node::Try(_) | Node::Compound(_) => {
                self.purposes.push(purpose.clone());
                let ty = self.expr_want(expr, Want::Check(expected));
                self.purposes.pop();
                ty
            }
            _ => {
                let ty = self.expr_want(expr, Want::Infer(Some(expected)));
                if !self.types.assignable(ty, expected) {
                    let span = self.spans.expr(expr);
                    self.mismatch_at(expr, span, expected, ty, purpose);
                }
                ty
            }
        }
    }

    /// Reports a mismatch at `expr`, offering `fetch` for an index whose
    /// result must not be nil.
    fn mismatch_at(
        &mut self,
        expr: &'a Expr,
        span: Span,
        expected: Ty,
        found: Ty,
        purpose: &Purpose,
    ) {
        let before = self.diagnostics.len();
        self.mismatch(span, expected, found, purpose);
        if self.diagnostics.len() > before {
            if let Some(fix) = self.fetch_fix(expr, found, expected) {
                self.diagnostics.last_mut().unwrap().fixes.push(fix);
            }
        }
    }

    /// For `x[i]` whose type is `T?` where `T` is expected, the rewrite to
    /// `x.fetch(i)`.
    pub(super) fn fetch_fix(&mut self, expr: &'a Expr, found: Ty, expected: Ty) -> Option<Fix> {
        let Node::Index(receiver, selectors) = &expr.node else {
            return None;
        };
        // `fetch` reads a copy, which a write through it would change.
        if self.in_write_chain(expr) {
            return None;
        }
        if selectors.len() != 1 || !self.types.has_nil(found) {
            return None;
        }
        let receiver_ty = self
            .fetch_receivers
            .get(&super::key(expr))
            .copied()
            .flatten()?;
        let fetched = match self.types.kind(receiver_ty) {
            Kind::Array(element) | Kind::Hash(element) => *element,
            Kind::MatchData => Ty::STRING,
            Kind::Shape(fields, _) => {
                let Node::Literal(key) = &selectors[0].node else {
                    return None;
                };
                let key = key.as_bytes()?;
                Types::field(fields, key)?.ty
            }
            _ => return None,
        };
        // Nullable collection elements stay nullable even when fetch finds them.
        if !self.types.assignable(fetched, expected) {
            return None;
        }
        let without = self.types.without_nil(found);
        if without == Ty::NEVER || !self.types.assignable(without, expected) {
            return None;
        }
        let brackets = self.spans.index_brackets(receiver, expr)?;
        let selector = self.spans.expr(&selectors[0]);
        let text = self.source.get(selector.start..selector.end)?;
        Some(Fix::edits(
            format!("read it with `fetch({text})`, which raises when it is missing"),
            vec![Edit {
                span: brackets,
                replacement: format!(".fetch({text})"),
            }],
        ))
    }

    pub(super) fn expr_want(&mut self, expr: &'a Expr, want: Want) -> Ty {
        self.meter.charge(1);
        if self.over_budget() {
            return Ty::ERROR;
        }
        if super::too_tall(expr.height()) {
            let span = self.spans.expr(expr);
            self.too_deep(span);
            return Ty::ANY;
        }
        let key = std::ptr::from_ref(expr) as usize;
        if let Some(memo) = self.memo.get() {
            if memo.replay {
                if let Some(&ty) = memo.types.get(&key) {
                    return ty;
                }
            }
        }
        let ty = self.expr_uncached(expr, want);
        if let Some(memo) = self.memo.get_mut() {
            if !memo.replay {
                memo.types.insert(key, ty);
            }
        }
        // A discarded compound form reports no value, although it has one.
        let reported = !matches!(want, Want::Discard)
            || !matches!(
                expr.node,
                Node::Compound(_)
                    | Node::Try(_)
                    | Node::Conditional(..)
                    | Node::Case(..)
                    | Node::Yield(_)
            );
        if reported {
            let plain = self.types.plain(ty);
            let number = match ty {
                Ty::INT => Some(super::Number::Int),
                Ty::FLOAT => Some(super::Number::Float),
                _ => None,
            };
            self.facts.record_value(expr, plain, number);
        }
        ty
    }

    fn expr_uncached(&mut self, expr: &'a Expr, want: Want) -> Ty {
        let hint = want.hint();
        match &expr.node {
            Node::Integer(_) | Node::BigInteger(..) => Ty::INT,
            Node::Literal(value) => self.literal(expr, value, hint),
            Node::Template(parts, symbol) => {
                for part in parts.iter() {
                    self.expr(part, None);
                }
                if *symbol { Ty::SYMBOL } else { Ty::STRING }
            }
            Node::Regex(..) => Ty::REGEX,
            Node::Var(name) => self.variable(expr, name),
            Node::Array(items) => self.array_literal(items, hint),
            Node::Hash(entries) => self.hash_literal(expr, entries, hint),
            Node::Shape(ty, fallback, names) => {
                self.shape_literal(expr, ty, fallback.as_deref(), names, hint)
            }
            Node::Unary(op, value) => self.unary(expr, op, value),
            Node::Binary(op, left, right) => self.binary(expr, op, left, right),
            Node::Range(start, end, _) => {
                // A range counts integers; float endpoints convert.
                for bound in [start, end].into_iter().flatten() {
                    let ty = self.expr(bound, None);
                    if self.operand(bound, ty) && !self.types.assignable(ty, Ty::NUMBER) {
                        let span = self.spans.expr(bound);
                        self.mismatch(span, Ty::NUMBER, ty, &Purpose::Operand);
                    }
                }
                Ty::RANGE
            }
            Node::Conditional(branches, alternate) => self.conditional(branches, alternate, want),
            Node::Case(subject, whens, alternate) => {
                self.case(expr, subject.as_deref(), whens, alternate.as_deref(), want)
            }
            Node::Compound(stmt) => self.stmt(stmt, want),
            Node::Try(attempt) => self.attempt(attempt, want),
            Node::Call(name, args, _) => self.call_name(expr, name, args, None),
            Node::ComputedCall(receiver, args) => self.computed_call(expr, receiver, args, None),
            Node::BlockCall(call, block) => self.block_call(expr, call, block),
            Node::Yield(args) => self.yield_expr(expr, args, want),
            Node::Member(receiver, name) => {
                self.method_call(expr, receiver, name, &[], None, false)
            }
            Node::SafeMember(receiver, name) => {
                self.method_call(expr, receiver, name, &[], None, true)
            }
            Node::Method(receiver, name, args, _) => {
                self.method_call(expr, receiver, name, args, None, false)
            }
            Node::SafeMethod(receiver, name, args, _) => {
                self.method_call(expr, receiver, name, args, None, true)
            }
            Node::Scope(receiver, name, args) => {
                self.scope(expr, receiver, name, args.as_deref(), None)
            }
            Node::Index(receiver, selectors) => self.index(expr, receiver, selectors),
        }
    }

    fn literal(&mut self, expr: &'a Expr, value: &crate::Value, hint: Option<Ty>) -> Ty {
        match &value.0 {
            ValueKind::Nil => Ty::NIL,
            ValueKind::Bool(_) => Ty::BOOL,
            ValueKind::Int(_) | ValueKind::Big(_) => Ty::INT,
            ValueKind::Float(_) => Ty::FLOAT,
            ValueKind::Bytes(_) => Ty::STRING,
            ValueKind::Symbol(symbol) => {
                let name = String::from_utf8_lossy(&symbol.data).into_owned();
                self.symbol_literal(expr, &name, hint)
            }
            _ => Ty::ANY,
        }
    }

    /// A symbol literal: an enum member or a literal symbol where the hint
    /// expects one, otherwise a symbol.
    fn symbol_literal(&mut self, expr: &Expr, name: &str, hint: Option<Ty>) -> Ty {
        let Some(hint) = hint else {
            return Ty::SYMBOL;
        };
        let alternatives = self.types.members(hint);
        let mut enums = Vec::new();
        for &alternative in &alternatives {
            match &*self.types.shared(alternative) {
                Kind::SymbolLit(literal) if &**literal == name => return alternative,
                Kind::Symbol | Kind::Any => return Ty::SYMBOL,
                Kind::EnumValue(id) => {
                    if self.program.enums[*id as usize].symbol(name).is_some() {
                        if let Some(why) = self.symbols_stay {
                            self.enum_symbol(expr, *id, name, why);
                        }
                        return alternative;
                    }
                    enums.push(*id);
                }
                _ => (),
            }
        }
        if let [id] = enums[..] {
            let decl = std::sync::Arc::clone(&self.program.enums[id as usize]);
            self.types.work(decl.symbols.len());
            if self.over_budget() {
                return Ty::ERROR;
            }
            let (members, _) = super::listed(&decl.symbols, |out, symbol| {
                out.push(':');
                out.push_str(symbol);
            });
            let enum_name = decl.name.clone();
            let span = self.spans.expr(expr);
            self.report(Diagnostic::error(
                Code::UNKNOWN_ENUM_MEMBER,
                span,
                format!("`:{name}` is not a member of `{enum_name}`, whose members are {members}"),
            ));
            return Ty::ERROR;
        }
        Ty::SYMBOL
    }

    fn variable(&mut self, expr: &'a Expr, name: &str) -> Ty {
        if name == "self" {
            let span = self.spans.expr(expr);
            self.self_escapes(span);
            return self.self_type();
        }
        if name.starts_with("@@") {
            let key = (self.frame.owner, name.to_owned());
            if let Some(&ty) = self.constants.get(&key) {
                return ty;
            }
            let span = self.spans.expr(expr);
            self.report(Diagnostic::error(
                Code::UNDECLARED_IVAR,
                span,
                if self.frame.owner.is_none() {
                    format!("class variable `{name}` is outside a class")
                } else { format!(
                    "class variable `{name}` is not declared; declare it in the class body, as in `{name}: T = value`"
                ) },
            ));
            return Ty::ERROR;
        }
        if let Some(ivar) = name.strip_prefix('@') {
            let span = self.spans.expr(expr);
            if !self.frame.instance {
                // Namespace state is not typed; it keeps its runtime checks.
                return Ty::ANY;
            }
            self.read_ivar(ivar, span);
            return self.ivar_type(ivar, span).unwrap_or(Ty::ERROR);
        }
        if let Some(id) = self.local(name) {
            let state = self.frame.flow.get(id);
            self.shared_read(id, name);
            if !state.assigned && self.frame.flow.live {
                let span = self.spans.expr(expr);
                let declared = self.frame.locals[id as usize].offset;
                self.report(
                    Diagnostic::error(
                        Code::UNASSIGNED_LOCAL,
                        span,
                        format!("`{name}` is not assigned on every path that reaches this read"),
                    )
                    .with_label(self.spans.token(declared), "first assigned here"),
                );
                // Report once per local.
                self.frame.flow.set(
                    id,
                    super::flow::VarState {
                        assigned: true,
                        ..state
                    },
                );
            }
            return state.ty;
        }
        if name == "block_given?" {
            return Ty::BOOL;
        }
        if is_constant(name) {
            if let Some(ns) = self.frame.owner {
                if let Some(&ty) = self.constants.get(&(Some(ns), name.to_owned())) {
                    return ty;
                }
                if let Some(&child) = self.program.namespaces[ns as usize].children.get(name) {
                    return self.types.intern(Kind::Namespace(child));
                }
            }
        }
        if let Some(&ty) = self.program.declared.get(name) {
            return ty;
        }
        if let Some(ty) = self.declaration(name) {
            return ty;
        }
        if let Some(&id) = self.modules.aliases.get(name) {
            return self.exports_type(id);
        }
        if is_constant(name) {
            if let Some(ty) = self.constant(name, self.frame.owner) {
                return ty;
            }
        }
        self.call_name(expr, name, &[], None)
    }

    /// A top-level class, module or enum by name, in any case.
    fn declaration(&mut self, name: &str) -> Option<Ty> {
        if let Some(&id) = self.program.enum_names.get(name) {
            return Some(self.types.intern(Kind::EnumType(id)));
        }
        let ns = *self.program.roots.get(name)?;
        Some(self.types.intern(Kind::Namespace(ns)))
    }

    /// The type of `self` in the current function.
    pub(super) fn self_type(&mut self) -> Ty {
        match (self.frame.owner, self.frame.instance) {
            (Some(ns), true) => self.types.intern(Kind::Instance(ns)),
            (Some(ns), false) => self.types.intern(Kind::Namespace(ns)),
            (None, _) => Ty::ANY,
        }
    }

    /// A capitalized name: a constant, class, module, enum or builtin
    /// namespace visible from `scope`.
    pub(super) fn constant(&mut self, name: &str, scope: Option<NsId>) -> Option<Ty> {
        let mut current = scope;
        while let Some(ns) = current {
            if let Some(&ty) = self.constants.get(&(Some(ns), name.to_owned())) {
                return Some(ty);
            }
            if let Some(&child) = self.program.namespaces[ns as usize].children.get(name) {
                return Some(self.types.intern(Kind::Namespace(child)));
            }
            current = self.program.namespaces[ns as usize].parent;
        }
        if let Some(&ns) = self.program.roots.get(name) {
            return Some(self.types.intern(Kind::Namespace(ns)));
        }
        if let Some(&id) = self.program.enum_names.get(name) {
            return Some(self.types.intern(Kind::EnumType(id)));
        }
        if let Some(index) = sigs::index().module(name) {
            return Some(self.types.intern(Kind::Builtin(index)));
        }
        self.constants.get(&(None, name.to_owned())).copied()
    }

    // Literals ---------------------------------------------------------

    /// The alternative of a hint that fits a literal of the given form.
    fn literal_hint(&mut self, hint: Option<Ty>, fits: impl Fn(&Kind) -> bool) -> Option<Ty> {
        let hint = hint?;
        self.types
            .members(hint)
            .into_iter()
            .find(|&alternative| fits(self.types.kind(alternative)))
    }

    fn array_literal(&mut self, items: &'a [Expr], hint: Option<Ty>) -> Ty {
        if let Some(hint) = hint {
            let alternatives: Vec<Ty> = self
                .types
                .members(hint)
                .into_iter()
                .filter(|&ty| matches!(self.types.kind(ty), Kind::Array(_) | Kind::Tuple(_)))
                .collect();
            if alternatives.len() > 1 {
                let held = self.hold(2 * types_bytes(items.len()));
                let mut values = Vec::with_capacity(items.len());
                for (index, item) in items.iter().enumerate() {
                    let hints: Vec<Ty> = alternatives
                        .iter()
                        .filter_map(|&ty| match self.types.kind(ty) {
                            Kind::Array(element) => Some(*element),
                            Kind::Tuple(elements) => elements.get(index).copied(),
                            _ => None,
                        })
                        .collect();
                    let element = self.types.union(&hints);
                    values.push(self.expr(item, Some(element)));
                }
                let tuple = self.types.tuple(values.clone());
                let fitting = alternatives
                    .into_iter()
                    .find(|&alternative| self.types.assignable(tuple, alternative));
                let result = match fitting {
                    Some(alternative) => alternative,
                    None => {
                        let element = self.types.union(&values);
                        self.types.array(element)
                    }
                };
                self.release(held);
                return result;
            }
        }
        let hint = self.literal_hint(hint, |kind| {
            matches!(kind, Kind::Array(_) | Kind::Tuple(_) | Kind::Any)
        });
        let shared = hint.map(|hint| (hint, self.types.shared(hint)));
        match shared.as_ref().map(|(hint, kind)| (*hint, &**kind)) {
            Some((hint, Kind::Tuple(elements))) if elements.len() == items.len() => {
                if self.types.has_var(hint) {
                    let held = self.hold(types_bytes(items.len()));
                    let actual = items
                        .iter()
                        .zip(elements.iter())
                        .map(|(item, &element)| self.expr(item, Some(element)))
                        .collect();
                    let tuple = self.types.tuple(actual);
                    self.release(held);
                    return tuple;
                }
                for (item, &element) in items.iter().zip(elements.iter()) {
                    self.expr_against(item, element, &Purpose::Element);
                }
                hint
            }
            Some((hint, Kind::Array(element))) => {
                if self.types.has_var(hint) {
                    let held = self.hold(types_bytes(items.len()));
                    let actual: Vec<Ty> = items
                        .iter()
                        .map(|item| self.expr(item, Some(*element)))
                        .collect();
                    let element = self.types.union(&actual);
                    self.release(held);
                    return self.types.array(element);
                }
                for item in items {
                    self.expr_against(item, *element, &Purpose::Element);
                }
                hint
            }
            _ => {
                let held = self.hold(types_bytes(items.len()));
                let types: Vec<Ty> = items.iter().map(|item| self.expr(item, None)).collect();
                let element = self.types.union(&types);
                self.release(held);
                self.types.array(element)
            }
        }
    }

    /// The shapes among `hint`'s alternatives a literal with `entries` may
    /// be checked against: those whose fields its keys fit, preferring
    /// those that declare no field the literal leaves out, and of them the
    /// widest, which accepts the values the others do, when there is one.
    fn fitting_shapes(
        &mut self,
        entries: &[(crate::compilation::Bytes, Expr)],
        hint: Option<Ty>,
    ) -> Vec<Ty> {
        let mut exact = Vec::new();
        let mut loose = Vec::new();
        let Some(hint) = hint else {
            return Vec::new();
        };
        for alternative in self.types.members(hint) {
            let shared = self.types.shared(alternative);
            let Kind::Shape(fields, open) = &*shared else {
                continue;
            };
            let Some(given) = self.given_fields(fields, *open, entries) else {
                continue;
            };
            let required = fields
                .iter()
                .zip(&given)
                .all(|(field, &given)| field.optional || given);
            if !required {
                continue;
            }
            if given.iter().all(|&given| given) {
                exact.push(alternative);
            } else {
                loose.push(alternative);
            }
        }
        let candidates = if exact.is_empty() { loose } else { exact };
        let widest = candidates.iter().copied().find(|&wide| {
            candidates
                .iter()
                .all(|&other| self.types.assignable(other, wide))
        });
        match widest {
            Some(widest) => vec![widest],
            None => candidates,
        }
    }

    /// Which of a shape's `fields` a literal with `entries` gives, found by
    /// searching the sorted fields for each key, so the keys need no table
    /// of their own; `None` when a key names no field of a closed shape,
    /// or the check stopped at its budget.
    fn given_fields(
        &mut self,
        fields: &[Field],
        open: bool,
        entries: &[(crate::compilation::Bytes, Expr)],
    ) -> Option<Vec<bool>> {
        self.types.work(fields.len() + entries.len());
        self.transient(fields.len());
        let mut given = vec![false; fields.len()];
        for (index, (key, _)) in entries.iter().enumerate() {
            if index % 4096 == 4095 && self.over_budget() {
                return None;
            }
            match fields.binary_search_by(|field| field.name.as_bytes().cmp(key)) {
                Ok(at) => given[at] = true,
                Err(_) if open => (),
                Err(_) => return None,
            }
        }
        (!self.types.stopped()).then_some(given)
    }

    /// Types a record literal that fits several shapes, none of which
    /// accepts all the others' values: each value against the union of the
    /// fields' types, and the literal as the first of the shapes that
    /// accepts its values, or as its own shape when none does.
    fn record_of_shapes(
        &mut self,
        entries: &'a [(crate::compilation::Bytes, Expr)],
        shapes: &[Ty],
    ) -> Ty {
        let held = self.hold(fields_bytes(entries));
        let mut fields = Vec::with_capacity(entries.len());
        for (key, entry) in entries {
            let hints: Vec<Ty> = shapes
                .iter()
                .filter_map(|&shape| match self.types.kind(shape) {
                    Kind::Shape(fields, _) => Types::field(fields, key).map(|field| field.ty),
                    _ => None,
                })
                .collect();
            let hint = (!hints.is_empty()).then(|| self.types.union(&hints));
            let ty = self.expr(entry, hint);
            fields.push(Field {
                name: String::from_utf8_lossy(key).as_ref().into(),
                ty,
                optional: false,
            });
        }
        let actual = self.types.shape(fields, false);
        self.release(held);
        shapes
            .iter()
            .copied()
            .find(|&shape| self.types.assignable(actual, shape))
            .unwrap_or(actual)
    }

    fn hash_literal(
        &mut self,
        expr: &'a Expr,
        entries: &'a [(crate::compilation::Bytes, Expr)],
        hint: Option<Ty>,
    ) -> Ty {
        let shapes = self.fitting_shapes(entries, hint);
        if shapes.len() > 1 {
            return self.record_of_shapes(entries, &shapes);
        }
        let hint = shapes.first().copied().or_else(|| {
            self.literal_hint(hint, |kind| {
                matches!(
                    kind,
                    Kind::Hash(_) | Kind::Shape(..) | Kind::Any | Kind::EmptyHash
                )
            })
        });
        let shared = hint.map(|hint| (hint, self.types.shared(hint)));
        match shared.as_ref().map(|(hint, kind)| (*hint, &**kind)) {
            Some((hint, Kind::Hash(value))) => {
                if self.types.has_var(hint) {
                    let held = self.hold(types_bytes(entries.len()));
                    let actual: Vec<Ty> = entries
                        .iter()
                        .map(|(_, entry)| self.expr(entry, Some(*value)))
                        .collect();
                    let value = self.types.union(&actual);
                    self.release(held);
                    return self.types.hash(value);
                }
                for (_, entry) in entries {
                    self.expr_against(entry, *value, &Purpose::Element);
                }
                hint
            }
            Some((hint, Kind::Shape(fields, open))) => {
                // Which fields the literal gives, by position, and its
                // values' types, which name its own shape only if it lacks
                // one.
                let held = self.hold(fields.len() + types_bytes(entries.len()));
                let mut present = vec![false; fields.len()];
                let mut types = Vec::with_capacity(entries.len());
                for (key, entry) in entries {
                    let key = String::from_utf8_lossy(key).into_owned();
                    match fields.binary_search_by(|field| field.name.as_bytes().cmp(key.as_bytes()))
                    {
                        Ok(index) => {
                            let expected = fields[index].ty;
                            types.push(self.expr_against(entry, expected, &Purpose::Field(key)));
                            present[index] = true;
                        }
                        Err(_) => {
                            types.push(self.expr(entry, None));
                            if !open {
                                let span = self.spans.expr(entry);
                                let shape = self.types.display(hint);
                                self.report(Diagnostic::error(
                                    Code::UNKNOWN_FIELD,
                                    span,
                                    format!("{shape} has no field `{key}`"),
                                ));
                            }
                        }
                    }
                }
                let (missing, count) = super::listed(
                    fields
                        .iter()
                        .zip(&present)
                        .filter(|(field, present)| !field.optional && !**present),
                    |out, (field, _)| out.push_str(&format!("`{}`", field.name)),
                );
                if count == 0 {
                    self.release(held);
                } else {
                    let span = self.spans.expr(expr);
                    let shape = self.types.display(hint);
                    self.transient(fields_bytes(entries));
                    let actual = entries
                        .iter()
                        .zip(types)
                        .map(|((key, _), ty)| Field {
                            name: String::from_utf8_lossy(key).as_ref().into(),
                            ty,
                            optional: false,
                        })
                        .collect();
                    let found = self.types.shape(actual, false);
                    let found = self.types.display(found);
                    self.report(
                        Diagnostic::error(
                            Code::TYPE_MISMATCH,
                            span,
                            format!("this hash lacks {missing} that {shape} requires"),
                        )
                        .with_types(shape, found),
                    );
                    self.release(held);
                    return Ty::ERROR;
                }
                hint
            }
            Some((hint, Kind::EmptyHash)) if entries.is_empty() => hint,
            _ => {
                if entries.is_empty() {
                    return Ty::EMPTY_HASH;
                }
                self.shape_of(entries)
            }
        }
    }

    /// The exact shape of a hash literal, checking each entry once.
    fn shape_of(&mut self, entries: &'a [(crate::compilation::Bytes, Expr)]) -> Ty {
        let held = self.hold(fields_bytes(entries));
        let mut fields = Vec::with_capacity(entries.len());
        for (key, entry) in entries {
            let ty = self.expr(entry, None);
            fields.push(Field {
                name: String::from_utf8_lossy(key).into(),
                ty,
                optional: false,
            });
        }
        let shape = self.types.shape(fields, false);
        self.release(held);
        shape
    }

    /// A braced group that is a type literal unless one of its names is a
    /// value in scope, in which case it is a hash.
    fn shape_literal(
        &mut self,
        expr: &'a Expr,
        ty: &'a crate::compilation::Type,
        fallback: Option<&'a Expr>,
        names: &[crate::compilation::Name],
        hint: Option<Ty>,
    ) -> Ty {
        if let Some(fallback) = fallback {
            let bound = names.iter().any(|name| self.value_name(name));
            if bound {
                return self.expr(fallback, hint);
            }
        }
        let described = self.annotation(ty, self.frame.owner, expr.offset as usize);
        self.types.type_lit(described)
    }

    /// Whether a name is a local or a function in scope, which makes a
    /// braced group that names it a hash rather than a type.
    fn value_name(&self, name: &str) -> bool {
        if self.local(name).is_some() {
            return true;
        }
        if let Some(ns) = self.frame.owner {
            let namespace = &self.program.namespaces[ns as usize];
            if namespace.methods.contains_key(name) || namespace.statics.contains_key(name) {
                return true;
            }
        }
        self.program.functions.contains_key(name) || self.program.hosts.contains_key(name)
    }

    // Operators --------------------------------------------------------

    fn unary(&mut self, expr: &'a Expr, op: &str, value: &'a Expr) -> Ty {
        let ty = self.expr(value, None);
        if op == "!" {
            if ty != Ty::BOOL && ty != Ty::ERROR && ty != Ty::NEVER {
                let span = self.spans.expr(value);
                let found = self.types.display(ty);
                self.report(
                    Diagnostic::error(
                        Code::LOGICAL_NOT_BOOL,
                        span,
                        format!("`!` takes a bool, found {found}"),
                    )
                    .with_types("bool", found),
                );
            }
            return Ty::BOOL;
        }
        if !self.operand(value, ty) {
            return Ty::ERROR;
        }
        if op == "*" {
            return ty;
        }
        let numeric = self.types.assignable(ty, Ty::NUMBER) || ty == Ty::DURATION;
        let string = op == "+" && ty == Ty::STRING;
        if numeric || string {
            return ty;
        }
        let span = self.spans.expr(expr);
        let found = self.types.display(ty);
        self.report(
            Diagnostic::error(
                Code::NO_OPERATOR,
                span,
                format!("unary `{op}` is not defined for {found}"),
            )
            .with_types("number", found),
        );
        Ty::ERROR
    }

    /// Checks that an operand is neither `any` nor possibly `nil`.
    pub(super) fn operand(&mut self, expr: &'a Expr, ty: Ty) -> bool {
        self.usable(expr, ty, "using an operator on it")
    }

    /// Checks that a value is neither `any` nor possibly `nil` before
    /// `doing` something with it, such as "indexing it".
    pub(super) fn usable(&mut self, expr: &'a Expr, ty: Ty, doing: &str) -> bool {
        if ty == Ty::ERROR || ty == Ty::NEVER {
            return false;
        }
        if ty == Ty::ANY {
            let span = self.spans.expr(expr);
            self.report(Diagnostic::error(
                Code::ANY_USE,
                span,
                format!(
                    "this value has type any; narrow it with `is_type?`, `.as(T)` or `JSON.parse_as` before {doing}"
                ),
            ));
            return false;
        }
        if self.types.has_nil(ty) {
            let span = self.spans.expr(expr);
            let found = self.types.display(ty);
            let without = self.types.without_nil(ty);
            let mut diagnostic = Diagnostic::error(
                Code::OPTIONAL_USE,
                span,
                format!("this value may be nil ({found}); test it with `!= nil` before {doing}"),
            );
            if let Some(fix) = self.fetch_fix(expr, ty, without) {
                diagnostic = diagnostic.with_fix(fix);
            }
            self.report(diagnostic);
            return false;
        }
        true
    }

    fn binary(&mut self, expr: &'a Expr, op: &'static str, left: &'a Expr, right: &'a Expr) -> Ty {
        match op {
            "&&" | "||" => self.condition_value(expr),
            "==" | "!=" | "===" => {
                let lt = self.member_receiver(left, op);
                let rt = self.expr(right, None);
                if op != "===" {
                    self.equality_visibility(expr, op, lt);
                }
                // The runtime never finds an enum member equal to a symbol.
                for (member, other, value) in [(lt, rt, right), (rt, lt, left)] {
                    if let (Kind::EnumValue(id), true, Node::Literal(v)) = (
                        &*self.types.shared(member),
                        other == Ty::SYMBOL,
                        &value.node,
                    ) {
                        if let Some(symbol) = super::symbol_text(v) {
                            self.enum_symbol(value, *id, &symbol, "they never compare equal");
                        }
                    }
                }
                if op == "===" {
                    return Ty::BOOL;
                }
                // A class's own `==` or `!=` gives what its method returns,
                // `nil` without `-> T`; a `!=` the runtime answers by
                // negating `==` is a `bool`. Each instance the left operand
                // may be runs its class's method with the right operand, a
                // `nil` too, as in a nil test of an optional instance.
                let mut results = Vec::new();
                let mut checked = Vec::new();
                for alternative in self.types.members(lt) {
                    let Kind::Instance(ns) = *self.types.kind(alternative) else {
                        results.push(Ty::BOOL);
                        continue;
                    };
                    let methods = &self.program.namespaces[ns as usize].methods;
                    let (id, result) = match (methods.get(op), methods.get("==")) {
                        (Some(&id), _) => (id, self.program.fns[id].sig.result.unwrap_or(Ty::NIL)),
                        // The runtime answers `!=` by negating the class's
                        // `==`, which takes the right operand.
                        (None, Some(&id)) if op == "!=" => (id, Ty::BOOL),
                        _ => {
                            results.push(Ty::BOOL);
                            continue;
                        }
                    };
                    if !checked.contains(&id) {
                        checked.push(id);
                        let span = self.spans.operator(expr.offset as usize);
                        self.operator_operand(id, rt, span);
                    }
                    results.push(result);
                }
                if checked.is_empty() {
                    return Ty::BOOL;
                }
                self.types.union(&results)
            }
            _ => {
                if op == "<<" {
                    self.mark_write_chain(left);
                }
                let lt = self.member_receiver(left, op);
                let hint = match op {
                    "<<" => self.types.element(lt),
                    _ => None,
                };
                // Appending to an array stores the value as it is.
                let stay = hint.map(|_| super::check::BUILTIN_SYMBOL);
                let rt = self.symbols(stay, |this| this.expr(right, hint));
                let span = self.spans.operator(expr.offset as usize);
                self.binary_types(op, lt, rt, span, Some((Some(left), right)))
            }
        }
    }

    /// Reports `==` or `!=` on an instance whose class hides the method the
    /// runtime calls: its own, or for `!=` without one, `==`.
    fn equality_visibility(&mut self, expr: &'a Expr, op: &str, left: Ty) {
        for alternative in self.types.members(left) {
            let Kind::Instance(ns) = *self.types.kind(alternative) else {
                continue;
            };
            let methods = &self.program.namespaces[ns as usize].methods;
            let (name, method) = match methods.get(op) {
                None if op == "!=" => ("==", methods.get("==")),
                method => (op, method),
            };
            if let Some(&id) = method {
                let span = self.spans.containing(expr.offset as usize);
                self.visibility(name, span, id, ns, true);
            }
        }
    }

    /// Checks `&&` and `||` outside a condition.
    fn condition_value(&mut self, expr: &'a Expr) -> Ty {
        let mark = self.frame.flow.mark();
        self.condition(expr);
        let branch = self.frame.flow.rollback(mark);
        let skipped = super::flow::Branch {
            live: true,
            changes: Vec::new(),
        };
        self.join(vec![branch, skipped]);
        Ty::BOOL
    }

    /// The result of a binary operator on operands of types `left` and
    /// `right`, reporting an operator that is not defined for them.
    pub(super) fn binary_types(
        &mut self,
        op: &str,
        left: Ty,
        right: Ty,
        span: Span,
        operands: Option<(Option<&'a Expr>, &'a Expr)>,
    ) -> Ty {
        if left == Ty::ERROR || right == Ty::ERROR {
            return Ty::ERROR;
        }
        if left == Ty::NEVER || right == Ty::NEVER {
            return Ty::NEVER;
        }
        if let Kind::Instance(ns) = *self.types.kind(left) {
            return self.operator_method(ns, op, left, right, span);
        }
        if let Some((left_expr, right_expr)) = operands {
            let left_ok = match left_expr {
                Some(left_expr) => self.operand(left_expr, left),
                None => left != Ty::ANY,
            };
            // `<<` stores its operand, which an `any` or optional element
            // type accepts as it is.
            let stored = op == "<<"
                && matches!(self.types.kind(left), Kind::Array(_))
                && self
                    .types
                    .element(left)
                    .is_some_and(|element| self.types.assignable(right, element));
            let right_ok = if (op == "%" && left == Ty::STRING) || stored {
                true
            } else {
                self.operand(right_expr, right)
            };
            if !left_ok || !right_ok {
                return Ty::ERROR;
            }
        }
        if let Some(result) = self.operator_result(op, left, right, span) {
            return result;
        }
        let left_text = self.types.display(left);
        let right_text = self.types.display(right);
        self.report(Diagnostic::error(
            Code::NO_OPERATOR,
            span,
            format!("`{op}` is not defined for {left_text} and {right_text}"),
        ));
        Ty::ERROR
    }

    /// An operator a script class defines as a method, such as `def +(other)`.
    fn operator_method(&mut self, ns: NsId, op: &str, left: Ty, right: Ty, span: Span) -> Ty {
        let method = self.program.namespaces[ns as usize]
            .methods
            .get(op)
            .copied();
        let Some(id) = method else {
            if matches!(op, "==" | "!=") {
                return Ty::BOOL;
            }
            let left_text = self.types.display(left);
            let right_text = self.types.display(right);
            self.report(Diagnostic::error(
                Code::NO_OPERATOR,
                span,
                format!("`{op}` is not defined for {left_text} and {right_text}; `{left_text}` defines no `{op}` method"),
            ));
            return Ty::ERROR;
        };
        self.visibility(op, self.spans.containing(span.start), id, ns, true);
        self.operator_operand(id, right, span);
        self.program.fns[id].sig.result.unwrap_or(Ty::NIL)
    }

    /// Checks the right operand of an operator against the first parameter
    /// of the method `id` that implements it.
    fn operator_operand(&mut self, id: FnId, right: Ty, span: Span) {
        let sig = self.program.fns[id].sig.clone();
        if let Some(param) = sig.params.first() {
            // A rest parameter collects the operand into its array.
            let expected = match param.kind {
                sigs::ParamKind::Rest => self.types.element(param.ty),
                _ => Some(param.ty),
            };
            if let Some(expected) = expected
                && !self.types.assignable(right, expected)
            {
                self.mismatch(span, expected, right, &Purpose::Operand);
            }
        }
    }

    /// The operator table.
    fn operator_result(&mut self, op: &str, left: Ty, right: Ty, span: Span) -> Option<Ty> {
        let number = |types: &mut super::ty::Types, ty: Ty| types.assignable(ty, Ty::NUMBER);
        let ln = number(&mut self.types, left);
        let rn = number(&mut self.types, right);
        let numeric = || {
            if left == Ty::INT && right == Ty::INT {
                Ty::INT
            } else if left == Ty::FLOAT || right == Ty::FLOAT {
                Ty::FLOAT
            } else {
                Ty::NUMBER
            }
        };
        let lk = &*self.types.shared(left);
        let rk = &*self.types.shared(right);
        let is_array = |kind: &Kind| matches!(kind, Kind::Array(_) | Kind::Tuple(_));
        Some(match op {
            "+" => {
                if ln && rn {
                    numeric()
                } else if left == Ty::STRING && right == Ty::STRING {
                    Ty::STRING
                } else if is_array(lk) && is_array(rk) {
                    let a = self.types.element(left)?;
                    let b = self.types.element(right)?;
                    let element = self.types.union(&[a, b]);
                    self.types.array(element)
                } else if left == Ty::TIME && (right == Ty::DURATION || rn)
                    || right == Ty::TIME && (left == Ty::DURATION || ln)
                {
                    Ty::TIME
                } else if (left == Ty::DURATION && (right == Ty::DURATION || rn))
                    || (ln && right == Ty::DURATION)
                {
                    Ty::DURATION
                } else if left == Ty::MONEY && right == Ty::MONEY {
                    Ty::MONEY
                } else {
                    return None;
                }
            }
            "-" => {
                if ln && rn {
                    numeric()
                } else if left == Ty::TIME && right == Ty::TIME {
                    Ty::FLOAT
                } else if left == Ty::TIME && (right == Ty::DURATION || rn) {
                    Ty::TIME
                } else if left == Ty::DURATION && (right == Ty::DURATION || rn) {
                    Ty::DURATION
                } else if left == Ty::MONEY && right == Ty::MONEY {
                    Ty::MONEY
                } else if is_array(lk) && is_array(rk) {
                    let a = self.types.element(left)?;
                    self.types.array(a)
                } else {
                    return None;
                }
            }
            "*" => {
                if ln && rn {
                    numeric()
                } else if left == Ty::STRING && rn {
                    Ty::STRING
                } else if (left == Ty::DURATION && rn) || (ln && right == Ty::DURATION) {
                    Ty::DURATION
                } else if (left == Ty::MONEY && right == Ty::INT)
                    || (left == Ty::INT && right == Ty::MONEY)
                {
                    Ty::MONEY
                } else {
                    return None;
                }
            }
            "/" => {
                // True division: numbers always divide to a float (ADR-008),
                // and so do two durations.
                if (ln && rn) || (left == Ty::DURATION && right == Ty::DURATION) {
                    Ty::FLOAT
                } else if left == Ty::DURATION && rn {
                    Ty::DURATION
                } else if left == Ty::MONEY && right == Ty::INT {
                    Ty::MONEY
                } else {
                    return None;
                }
            }
            "//" => {
                if ln && rn {
                    numeric()
                } else {
                    return None;
                }
            }
            "%" => {
                if left == Ty::STRING {
                    Ty::STRING
                } else if ln && rn {
                    numeric()
                } else if left == Ty::DURATION && right == Ty::DURATION {
                    Ty::DURATION
                } else {
                    return None;
                }
            }
            "**" => {
                if ln && rn {
                    numeric()
                } else {
                    return None;
                }
            }
            "<" | "<=" | ">" | ">=" | "<=>" => {
                let comparable = (ln && rn)
                    || (left == right
                        && matches!(
                            lk,
                            Kind::String | Kind::Symbol | Kind::Time | Kind::Duration | Kind::Money
                        ));
                if !comparable {
                    return None;
                }
                if op == "<=>" { Ty::INT } else { Ty::BOOL }
            }
            "=~" => {
                if (left == Ty::STRING && right == Ty::REGEX)
                    || (left == Ty::REGEX && right == Ty::STRING)
                {
                    self.types.optional(Ty::INT)
                } else {
                    return None;
                }
            }
            "!~" => {
                if (left == Ty::STRING && right == Ty::REGEX)
                    || (left == Ty::REGEX && right == Ty::STRING)
                {
                    Ty::BOOL
                } else {
                    return None;
                }
            }
            "<<" => {
                let Kind::Array(element) = lk else {
                    return None;
                };
                if !self.types.assignable(right, *element) {
                    let expected = self.types.display(*element);
                    let found = self.types.display(right);
                    self.report(
                        Diagnostic::error(
                            Code::TYPE_MISMATCH,
                            span,
                            format!("`<<` appends to array<{expected}>, found {found}"),
                        )
                        .with_types(expected, found),
                    );
                    return Some(left);
                }
                left
            }
            "&" if is_array(lk) && is_array(rk) => {
                let a = self.types.element(left)?;
                self.types.array(a)
            }
            _ => return None,
        })
    }

    // Indexing ---------------------------------------------------------

    fn index(&mut self, expr: &'a Expr, receiver: &'a Expr, selectors: &'a [Expr]) -> Ty {
        let ty = self.member_receiver(receiver, "[]");
        let read = self.index_type(expr, receiver, ty, selectors);
        if self.types.has_nil(read) {
            let recorded = self
                .fetch_receivers
                .entry(super::key(expr))
                .or_insert(Some(ty));
            if *recorded != Some(ty) {
                *recorded = None;
            }
        }
        if !self.in_write_chain(expr) {
            return read;
        }
        // A write through an array element or hash entry reads it as
        // present: the runtime raises when it is missing.
        let element = match self.types.kind(ty) {
            Kind::Array(element) | Kind::Hash(element) => *element,
            _ => return read,
        };
        if read == self.types.optional(element) {
            element
        } else {
            read
        }
    }

    /// The result of `receiver[selectors]` for a receiver of type `ty`.
    fn index_type(
        &mut self,
        expr: &'a Expr,
        receiver: &'a Expr,
        ty: Ty,
        selectors: &'a [Expr],
    ) -> Ty {
        if ty == Ty::ERROR || ty == Ty::NEVER {
            for selector in selectors {
                self.expr(selector, None);
            }
            return ty;
        }
        if !self.usable(receiver, ty, "indexing it") {
            for selector in selectors {
                self.expr(selector, None);
            }
            return Ty::ERROR;
        }
        let kind = &*self.types.shared(ty);
        match (&kind, selectors) {
            (Kind::Array(element), [selector]) => {
                let key = self.expr(selector, Some(Ty::INT));
                if key == Ty::RANGE {
                    self.fetch_receivers.insert(super::key(expr), None);
                    return self.types.optional(ty);
                }
                self.selector(selector, key, Ty::INT);
                self.types.optional(*element)
            }
            (Kind::Array(_), [start, length]) => {
                let a = self.expr(start, Some(Ty::INT));
                self.selector(start, a, Ty::INT);
                let b = self.expr(length, Some(Ty::INT));
                self.selector(length, b, Ty::INT);
                self.types.optional(ty)
            }
            (Kind::Tuple(items), [selector]) => {
                if let Some(index) = int_literal(selector) {
                    let position = if index < 0 {
                        items.len() as i64 + index
                    } else {
                        index
                    };
                    return match usize::try_from(position).ok().and_then(|p| items.get(p)) {
                        Some(&item) => item,
                        None => {
                            let span = self.spans.expr(selector);
                            let tuple = self.types.display(ty);
                            self.report(Diagnostic::error(
                                Code::TUPLE_INDEX,
                                span,
                                format!(
                                    "{tuple} has {} elements; index {index} is outside it",
                                    items.len()
                                ),
                            ));
                            Ty::ERROR
                        }
                    };
                }
                let key = self.expr(selector, Some(Ty::INT));
                if key == Ty::RANGE {
                    let element = self.types.union(items);
                    let array = self.types.array(element);
                    return self.types.optional(array);
                }
                self.selector(selector, key, Ty::INT);
                let element = self.types.union(items);
                self.types.optional(element)
            }
            (Kind::Hash(value), [selector]) => {
                let key = self.expr(selector, Some(Ty::STRING));
                self.selector(selector, key, Ty::STRING);
                self.types.optional(*value)
            }
            (Kind::EmptyHash, [selector]) => {
                self.expr(selector, Some(Ty::STRING));
                Ty::NIL
            }
            (Kind::Shape(fields, open), [selector]) => {
                let key = self.expr(selector, Some(Ty::STRING));
                match string_literal(selector) {
                    Some(name) => match Types::field(fields, name.as_bytes()) {
                        Some(field) if field.optional => self.types.optional(field.ty),
                        Some(field) => field.ty,
                        None if *open => Ty::ANY,
                        None => {
                            let span = self.spans.expr(selector);
                            let shape = self.types.display(ty);
                            self.report(Diagnostic::error(
                                Code::UNKNOWN_FIELD,
                                span,
                                format!("{shape} has no field \"{name}\""),
                            ));
                            Ty::ERROR
                        }
                    },
                    None => {
                        self.selector(selector, key, Ty::STRING);
                        self.dynamic_key(receiver, selector, ty);
                        Ty::ERROR
                    }
                }
            }
            (Kind::String, [selector]) => {
                let key = self.expr(selector, None);
                if key != Ty::RANGE {
                    self.selector(selector, key, Ty::INT);
                }
                self.types.optional(Ty::STRING)
            }
            (Kind::String, [start, length]) => {
                let a = self.expr(start, Some(Ty::INT));
                self.selector(start, a, Ty::INT);
                let b = self.expr(length, Some(Ty::INT));
                self.selector(length, b, Ty::INT);
                self.types.optional(Ty::STRING)
            }
            (Kind::MatchData, [selector]) => {
                let key = self.expr(selector, None);
                let expected = self.types.union(&[Ty::STRING, Ty::NUMBER]);
                self.selector(selector, key, expected);
                match string_literal(selector).as_deref() {
                    Some("begin" | "end") => {
                        self.report(Diagnostic::error(
                            Code::NOT_CALLABLE,
                            self.spans.expr(expr),
                            "a match offset is a method; call `.begin(index)` or `.end(index)`",
                        ));
                        Ty::ERROR
                    }
                    Some("captures") => {
                        let element = self.types.optional(Ty::STRING);
                        self.types.array(element)
                    }
                    Some("named_captures") => {
                        let value = self.types.optional(Ty::STRING);
                        self.types.hash(value)
                    }
                    Some("to_s" | "pre_match" | "post_match") => Ty::STRING,
                    Some(_) => self.types.optional(Ty::STRING),
                    None if key == Ty::STRING => {
                        self.report(Diagnostic::error(
                            Code::DYNAMIC_KEY,
                            self.spans.expr(selector),
                            "index match data with a literal string or a capture index",
                        ));
                        Ty::ERROR
                    }
                    None => self.types.optional(Ty::STRING),
                }
            }
            (Kind::Instance(_), _) => {
                let name = "[]";
                self.method_on(expr, ty, name, None, selectors, None)
            }
            (Kind::Union(_), _) => {
                let alternatives = self.types.members(ty);
                let outer = self.set_memo(Some(super::Memo::default()));
                let result = self.index_type(expr, receiver, alternatives[0], selectors);
                let mut results = vec![result];
                // The other alternatives reuse the selectors' types.
                self.memo.get_mut().unwrap().replay = true;
                for &alternative in &alternatives[1..] {
                    let mark = self.frame.flow.mark();
                    results.push(self.index_type(expr, receiver, alternative, selectors));
                    self.frame.flow.rollback(mark);
                }
                self.restore_memo(outer);
                self.types.union(&results)
            }
            _ => {
                for selector in selectors {
                    self.expr(selector, None);
                }
                let span = self.spans.expr(expr);
                let found = self.types.display(ty);
                self.report(Diagnostic::error(
                    Code::NOT_INDEXABLE,
                    span,
                    format!(
                        "{found} cannot be indexed with {} selector(s)",
                        selectors.len()
                    ),
                ));
                Ty::ERROR
            }
        }
    }

    fn selector(&mut self, selector: &'a Expr, ty: Ty, expected: Ty) {
        if ty == Ty::ERROR || self.types.assignable(ty, expected) {
            return;
        }
        let span = self.spans.expr(selector);
        if ty == Ty::ANY || self.types.has_nil(ty) {
            self.operand(selector, ty);
            return;
        }
        self.mismatch(span, expected, ty, &Purpose::Operand);
    }

    /// Reports a shape indexed with a runtime key, offering to declare the
    /// local as a dictionary when its fields share a type.
    fn dynamic_key(&mut self, receiver: &'a Expr, selector: &'a Expr, shape: Ty) {
        let span = self.spans.expr(selector);
        let shape_text = self.types.display(shape);
        let mut diagnostic = Diagnostic::error(
            Code::DYNAMIC_KEY,
            span,
            format!(
                "{shape_text} is a record, not a dictionary: read its fields with literal keys, or declare it as `hash<string, V>`"
            ),
        );
        if let Node::Var(name) = &receiver.node {
            if let Some(id) = self.local(name) {
                let local = &self.frame.locals[id as usize];
                if let (Some(value), false) = (local.dictionary, local.annotated) {
                    let value_text = self.types.display(value);
                    let name_span = self.spans.token(local.offset);
                    diagnostic = diagnostic.with_fix(Fix::insert(
                        format!("declare `{name}: hash<string, {value_text}>`"),
                        name_span.end,
                        format!(": hash<string, {value_text}>"),
                    ));
                }
            }
        }
        self.report(diagnostic);
    }

    /// Checks `receiver[selectors] = value`, or its compound form when
    /// `value_ty` already holds the computed value.
    /// Checks a write of a computed value of type `value_ty` through the
    /// `[]=` of an instance of class `ns`, as a compound assignment makes:
    /// the method must take the selectors, whose types replay from the
    /// read, and the value.
    fn computed_index_write(
        &mut self,
        expr: &'a Expr,
        ns: super::program::NsId,
        selectors: &'a [Expr],
        value_ty: Ty,
        value: &'a Expr,
    ) {
        let span = self.spans.expr(expr);
        let Some(&id) = self.program.namespaces[ns as usize].methods.get("[]=") else {
            let found = self.program.namespaces[ns as usize].name.clone();
            self.report(Diagnostic::error(
                Code::UNKNOWN_MEMBER,
                span,
                format!("{found} has no member `[]=`"),
            ));
            return;
        };
        self.visibility("[]=", span, id, ns, true);
        let sig = self.program.fns[id].sig.clone();
        let held = self.hold((selectors.len() + 1) * std::mem::size_of::<(Ty, Span)>());
        let mut values: Vec<(Ty, Span)> = Vec::with_capacity(selectors.len() + 1);
        for selector in selectors {
            values.push((self.expr(selector, None), self.spans.expr(selector)));
        }
        values.push((value_ty, self.spans.expr(value)));
        self.release(held);
        for (index, (ty, span)) in values.into_iter().enumerate() {
            if let Some(param) = sig.params.get(index) {
                if !self.types.assignable(ty, param.ty) {
                    let purpose = Purpose::Argument {
                        index,
                        name: param.name.clone(),
                        function: "[]=".to_owned(),
                    };
                    self.mismatch(span, param.ty, ty, &purpose);
                }
            }
        }
    }

    pub(super) fn index_write(
        &mut self,
        expr: &'a Expr,
        receiver: &'a Expr,
        selectors: &'a [Expr],
        value_ty: Ty,
        value: &'a Expr,
        evaluate: bool,
    ) -> Ty {
        let ty = if evaluate {
            self.member_receiver(receiver, "[]=")
        } else {
            self.mute += 1;
            let ty = self.expr(receiver, None);
            self.mute -= 1;
            ty
        };
        if ty == Ty::ERROR {
            for selector in selectors {
                self.expr(selector, None);
            }
            return if evaluate {
                self.expr(value, None)
            } else {
                value_ty
            };
        }
        if !self.usable(receiver, ty, "indexing it") {
            return if evaluate {
                self.expr(value, None)
            } else {
                value_ty
            };
        }
        let element = match (&*self.types.shared(ty), selectors) {
            (Kind::Array(element), [selector]) => {
                if evaluate {
                    // Only a single index is assignable, not a range.
                    let key = self.expr(selector, Some(Ty::INT));
                    self.selector(selector, key, Ty::INT);
                }
                Some(*element)
            }
            (Kind::Tuple(items), [selector]) => {
                let element = int_literal(selector).and_then(|index| {
                    let index = if index < 0 {
                        items.len() as i64 + index
                    } else {
                        index
                    };
                    usize::try_from(index)
                        .ok()
                        .and_then(|index| items.get(index))
                        .copied()
                });
                if element.is_none() {
                    if evaluate {
                        self.expr(selector, None);
                    }
                    self.report(Diagnostic::error(
                        Code::TUPLE_MUTATION,
                        self.spans.expr(selector),
                        "a tuple write needs a literal index of one of its fixed elements",
                    ));
                }
                element
            }
            (Kind::Hash(value), [selector]) => {
                if evaluate {
                    let key = self.expr(selector, Some(Ty::STRING));
                    self.selector(selector, key, Ty::STRING);
                }
                Some(*value)
            }
            (Kind::Shape(fields, open), [selector]) => match string_literal(selector) {
                Some(name) => match Types::field(fields, name.as_bytes()) {
                    Some(field) => Some(field.ty),
                    None if *open => Some(Ty::ANY),
                    None => {
                        let span = self.spans.expr(selector);
                        let shape = self.types.display(ty);
                        self.report(Diagnostic::error(
                            Code::UNKNOWN_FIELD,
                            span,
                            format!("{shape} has no field \"{name}\""),
                        ));
                        None
                    }
                },
                None => {
                    if evaluate {
                        self.expr(selector, None);
                    }
                    self.dynamic_key(receiver, selector, ty);
                    None
                }
            },
            (Kind::Instance(ns), _) => {
                if evaluate {
                    let outer = self.set_memo(Some(super::Memo::default()));
                    self.method_on(expr, ty, "[]=", None, selectors, Some(value));
                    self.memo.get_mut().unwrap().replay = true;
                    let assigned = self.expr(value, None);
                    self.restore_memo(outer);
                    return assigned;
                }
                self.computed_index_write(expr, *ns, selectors, value_ty, value);
                None
            }
            _ => {
                if evaluate {
                    for selector in selectors {
                        self.expr(selector, None);
                    }
                }
                let span = self.spans.expr(expr);
                let found = self.types.display(ty);
                self.report(Diagnostic::error(
                    Code::NOT_INDEXABLE,
                    span,
                    format!("{found} cannot be assigned through an index"),
                ));
                None
            }
        };
        match element {
            Some(element) => {
                if evaluate {
                    self.symbols(Some(super::check::INDEX_SYMBOL), |this| {
                        this.expr_against(value, element, &Purpose::Element)
                    })
                } else {
                    if !self.types.assignable(value_ty, element) {
                        let span = self.spans.expr(value);
                        self.mismatch(span, element, value_ty, &Purpose::Element);
                    }
                    value_ty
                }
            }
            None => {
                if evaluate {
                    self.expr(value, None)
                } else {
                    value_ty
                }
            }
        }
    }

    // Conditionals, case and rescue ------------------------------------

    fn conditional(&mut self, branches: &'a [(Expr, Expr)], alternate: &'a Expr, want: Want) -> Ty {
        let mut results = Vec::new();
        let mut explored = Vec::new();
        let entry = self.frame.flow.mark();
        for (condition, value) in branches {
            let narrow = self.condition(condition);
            let mark = self.frame.flow.mark();
            self.apply(&narrow.then);
            let ty = self.branch_value(value, want);
            if self.frame.flow.live {
                results.push(ty);
            }
            let branch = self.frame.flow.rollback(mark);
            self.explore(&mut explored, branch);
            self.apply(&narrow.otherwise);
        }
        let ty = self.branch_value(alternate, want);
        if self.frame.flow.live {
            results.push(ty);
        }
        let branch = self.frame.flow.rollback(entry);
        self.explore(&mut explored, branch);
        self.join_explored(explored);
        self.types.union(&results)
    }

    /// The value of one branch of a conditional, checked when wanted.
    fn branch_value(&mut self, value: &'a Expr, want: Want) -> Ty {
        match want {
            Want::Check(expected) => {
                let purpose = self.purposes.last().cloned().unwrap_or(Purpose::Result);
                self.expr_against(value, expected, &purpose)
            }
            Want::Infer(hint) => self.expr(value, hint),
            Want::Discard => self.expr_want(value, Want::Discard),
        }
    }

    fn case(
        &mut self,
        expr: &'a Expr,
        subject: Option<&'a Expr>,
        whens: &'a [When],
        alternate: Option<&'a Expr>,
        want: Want,
    ) -> Ty {
        let subject_ty = subject.map(|subject| self.expr(subject, None));
        // The values the `when`s name, kept while the rest are checked.
        let mut covered: Vec<String> = Vec::new();
        let mut held = 0;
        let mut results = Vec::new();
        let mut explored = Vec::new();
        let entry = self.frame.flow.mark();
        for when in whens {
            for (value, _splat) in when.values.iter() {
                match subject_ty {
                    Some(subject) => {
                        // `case` compares values as `===` does, without turning a
                        // symbol into an enum member.
                        let hint = (!matches!(self.types.kind(subject), Kind::EnumValue(_)))
                            .then_some(subject);
                        let ty = self.expr(value, hint);
                        if let Some(name) = self.covered_value(value, ty, subject) {
                            held += self.hold(std::mem::size_of::<String>() + name.len());
                            covered.push(name);
                        }
                    }
                    None => {
                        self.condition(value);
                    }
                }
            }
            let mark = self.frame.flow.mark();
            let ty = self.branch_value(&when.result, want);
            if self.frame.flow.live {
                results.push(ty);
            }
            let branch = self.frame.flow.rollback(mark);
            self.explore(&mut explored, branch);
        }
        let exhaustive = match subject_ty {
            Some(subject) => self.exhaustive(expr, subject, &covered, alternate.is_some()),
            None => false,
        };
        drop(covered);
        self.release(held);
        match alternate {
            Some(alternate) => {
                let ty = self.branch_value(alternate, want);
                if self.frame.flow.live {
                    results.push(ty);
                }
            }
            None if !exhaustive => {
                if let Want::Check(expected) = want {
                    if !self.types.assignable(Ty::NIL, expected) {
                        let span = self.spans.token(expr.offset as usize);
                        let expected_text = self.types.display(expected);
                        self.report(
                            Diagnostic::error(
                                Code::TYPE_MISMATCH,
                                span,
                                format!("this `case` gives nil when no `when` matches, but {expected_text} is expected; add an `else`"),
                            )
                            .with_types(expected_text, "nil"),
                        );
                    }
                }
                results.push(Ty::NIL);
            }
            None => (),
        }
        let branch = self.frame.flow.rollback(entry);
        self.explore(&mut explored, branch);
        self.join_explored(explored);
        self.types.union(&results)
    }

    /// The enum member or bool a `when` value names, for exhaustiveness.
    fn covered_value(&mut self, value: &Expr, ty: Ty, subject: Ty) -> Option<String> {
        match *self.types.kind(subject) {
            Kind::EnumValue(id) => {
                if let (Node::Literal(v), true) = (&value.node, ty == Ty::SYMBOL) {
                    let symbol = super::symbol_text(v)?;
                    self.enum_symbol(
                        value,
                        id,
                        &symbol,
                        "a `when` over it never matches a symbol",
                    );
                    // The fix makes it the member, so it counts as covered.
                    return Some(symbol);
                }
                if self.types.kind(ty) != &Kind::EnumValue(id) {
                    if ty != Ty::ERROR && !self.types.assignable(ty, subject) {
                        let span = self.spans.expr(value);
                        self.mismatch(span, subject, ty, &Purpose::Operand);
                    }
                    return None;
                }
                match &value.node {
                    Node::Literal(v) => super::symbol_text(v),
                    Node::Scope(_, name, None) => {
                        let decl = &self.program.enums[id as usize];
                        let index = decl.member(name)?;
                        Some(decl.symbols[index].clone())
                    }
                    _ => None,
                }
            }
            Kind::Bool => match &value.node {
                Node::Literal(v) if v.type_name() == "bool" => Some(v.truthy().to_string()),
                _ => None,
            },
            _ => None,
        }
    }

    /// Reports a symbol compared with a member of enum `id`, which the
    /// runtime never finds equal, offering the member it names.
    fn enum_symbol(&mut self, value: &Expr, id: u32, symbol: &str, why: &str) {
        let decl = &self.program.enums[id as usize];
        let enum_name = decl.name.clone();
        let member = decl.symbol(symbol).map(|index| decl.members[index].clone());
        let span = self.spans.expr(value);
        let mut diagnostic = Diagnostic::error(
            Code::TYPE_MISMATCH,
            span,
            format!("`:{symbol}` is a symbol, not a member of `{enum_name}`, and {why}"),
        )
        .with_types(enum_name.clone(), "symbol");
        match member {
            Some(member) => {
                let replacement = format!("{enum_name}::{member}");
                diagnostic = diagnostic.with_fix(Fix::replace(
                    format!("name the member: `{replacement}`"),
                    span,
                    replacement,
                ));
            }
            None => diagnostic.code = Code::UNKNOWN_ENUM_MEMBER,
        }
        self.report(diagnostic);
    }

    /// Reports a `case` over an enum or bool that misses a value; returns
    /// whether it covers every value.
    fn exhaustive(
        &mut self,
        expr: &Expr,
        subject: Ty,
        covered: &[String],
        alternate: bool,
    ) -> bool {
        let covered: std::collections::HashSet<&str> = covered.iter().map(String::as_str).collect();
        self.transient(super::meter::set(&covered));
        // The values the `case` misses, as their `when`s name them.
        let (missing, name): (String, &str) = match *self.types.kind(subject) {
            Kind::EnumValue(id) => {
                let decl = std::sync::Arc::clone(&self.program.enums[id as usize]);
                // Counting the members it covers takes time that grows
                // with the `when`s alone, not with the enum.
                let known = covered
                    .iter()
                    .filter(|symbol| decl.symbol(symbol).is_some())
                    .count();
                if known == decl.symbols.len() {
                    return true;
                }
                if alternate {
                    return false;
                }
                self.types.work(decl.symbols.len());
                if self.over_budget() {
                    return false;
                }
                let (missing, _) = super::listed(
                    decl.symbols
                        .iter()
                        .zip(&decl.members)
                        .filter(|(symbol, _)| !covered.contains(symbol.as_str())),
                    |out, (_, member)| {
                        out.push_str(&format!("`{}::{member}`", decl.name));
                    },
                );
                let span = self.spans.token(expr.offset as usize);
                self.non_exhaustive(span, &decl.name, missing);
                return false;
            }
            Kind::Bool => (
                super::listed(
                    ["true", "false"]
                        .into_iter()
                        .filter(|value| !covered.contains(value)),
                    |out, value| out.push_str(&format!("`{value}`")),
                )
                .0,
                "bool",
            ),
            _ => return false,
        };
        if missing.is_empty() {
            return true;
        }
        if !alternate {
            let span = self.spans.token(expr.offset as usize);
            self.non_exhaustive(span, name, missing);
        }
        false
    }

    /// Reports a `case` over `name` that does not handle the `missing`
    /// values.
    fn non_exhaustive(&mut self, span: crate::diagnostic::Span, name: &str, missing: String) {
        self.report(Diagnostic::error(
            Code::NON_EXHAUSTIVE_CASE,
            span,
            format!("this `case` over {name} does not handle {missing}; add a `when` for each, or an `else`"),
        ));
    }

    fn attempt(&mut self, attempt: &'a Try, want: Want) -> Ty {
        // A rescue or the ensure may run after any part of the body, so
        // what the body assigns may have its value or its earlier one; the
        // ensure, or a `retry` running the body again, may also follow any
        // part of a rescue, and the ensure any part of the `else`.
        let spans = self.assigns.attempt(&self.meter, attempt);
        if spans.retry {
            self.widen(spans.retried());
        }
        let entry = self.frame.flow.mark();
        let body_want = if attempt.alternate.is_empty() {
            want
        } else {
            Want::Discard
        };
        let mut results = Vec::new();
        let body = self.stmts(&attempt.body, body_want);
        // A check past its budget skips the rescues and the ensure, and
        // the work of joining them.
        if self.halted() {
            self.frame.flow.rollback(entry);
            return Ty::ERROR;
        }
        if !attempt.alternate.is_empty() {
            let alternate = self.stmts(&attempt.alternate, want);
            if self.frame.flow.live {
                results.push(alternate);
            }
        } else if self.frame.flow.live {
            results.push(body);
        }
        let mut explored = Vec::new();
        let branch = self.frame.flow.rollback(entry);
        self.explore(&mut explored, branch);
        if !attempt.rescues.is_empty() {
            self.widen(spans.body);
        }
        for rescue in attempt.rescues.iter() {
            self.open_scope();
            if let Some(binding) = &rescue.binding {
                let id = self.declare(binding, Ty::ERROR_VALUE, rescue.offset as usize, true);
                self.assign_local(id, Ty::ERROR_VALUE);
            }
            let mark = self.frame.flow.mark();
            let ty = self.stmts(&rescue.body, want);
            if self.frame.flow.live {
                results.push(ty);
            }
            let branch = self.frame.flow.rollback(mark);
            self.explore(&mut explored, branch);
            self.close_scope();
        }
        if attempt.ensure.is_empty() {
            self.join_explored(explored);
        } else {
            self.ensure(&attempt.ensure, explored, spans);
        }
        self.types.union(&results)
    }

    /// Checks an ensure from what holds wherever it may start: before the
    /// body, less the narrowing of what the body, the `else` and the rescues
    /// assign. Then joins the `explored` ends of the body and the rescues,
    /// and applies what the ensure proves on the way out: a local it assigns
    /// has the state it leaves, and any other the state both the join and
    /// the ensure's guards and exits prove.
    fn ensure(&mut self, ensure: &'a [Stmt], explored: Vec<Branch>, spans: TrySpans) {
        let mark = self.frame.flow.mark();
        self.widen(spans.ensured());
        self.stmts(ensure, Want::Discard);
        let ensured = self.frame.flow.live;
        let branch = self.frame.flow.rollback(mark);
        // Taken from the flow, the ensure's changes are held until they
        // apply.
        let held = self.hold(super::meter::Heap::heap(&branch));
        self.join_explored(explored);
        if self.halted() {
            self.release(held);
            return;
        }
        for (id, state) in branch.changes {
            if self
                .assigns
                .writes(spans.ensure, &self.frame.locals[id as usize].name)
            {
                self.frame.flow.set(id, state);
            } else {
                let joined = self.frame.flow.get(id);
                let ty = self.both(joined.ty, state.ty);
                self.frame.flow.set(id, VarState { ty, ..joined });
            }
        }
        self.release(held);
        self.frame.flow.live = self.frame.flow.live && ensured;
    }

    /// The type of a value known to have both types `a` and `b`: the
    /// narrower when one accepts the other, else the alternatives of `a`
    /// that `b` accepts.
    fn both(&mut self, a: Ty, b: Ty) -> Ty {
        if a == b || b == Ty::ANY {
            return a;
        }
        if a == Ty::ANY || self.types.assignable(b, a) {
            return b;
        }
        if self.types.assignable(a, b) {
            return a;
        }
        let members: Vec<Ty> = self
            .types
            .members(a)
            .into_iter()
            .filter(|&member| self.types.assignable(member, b))
            .collect();
        if members.is_empty() {
            a
        } else {
            self.types.union(&members)
        }
    }
}

/// The bytes of a list of one type for each of `count` elements.
fn types_bytes(count: usize) -> usize {
    count * std::mem::size_of::<Ty>()
}

/// The bytes of a literal's entries as a shape's fields, with their names.
fn fields_bytes(entries: &[(crate::compilation::Bytes, Expr)]) -> usize {
    entries.len() * std::mem::size_of::<Field>()
        + entries.iter().map(|(key, _)| key.len()).sum::<usize>()
}

/// The integer a literal selector spells, including a negated one.
pub(super) fn int_literal(expr: &Expr) -> Option<i64> {
    match &expr.node {
        Node::Integer(n) => i64::try_from(*n).ok(),
        Node::Literal(value) => value.as_int(),
        Node::Unary("-", inner) => int_literal(inner).map(|n| -n),
        _ => None,
    }
}

/// The string a literal key spells.
pub(super) fn string_literal(expr: &Expr) -> Option<String> {
    match &expr.node {
        Node::Literal(value) => value
            .as_bytes()
            .map(|bytes| String::from_utf8_lossy(bytes).into_owned()),
        _ => None,
    }
}
