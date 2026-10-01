//! Checks calls: to script functions, methods and constructors, to builtins
//! through the signature table with overload selection, and to host
//! functions; blocks against their signatures, and `yield`.

use super::{
    Checker, ReceiverType,
    check::{Context, Purpose, Want},
    counted::{CountedVec, ScratchVec},
    program::{FnId, NsId},
    sigs::{self, BlockSig, ParamKind, Sig},
    ty::{Kind, Ty, Types},
};
use crate::{
    diagnostic::{Code, Diagnostic, Fix, Span},
    syntax::{Argument, ArgumentKind, Block, Expr, Node, Target, modules::Visibility},
};
use std::rc::Rc;

pub(super) use super::check::BreakTo;

/// The parameters past which a call's keywords are found by search among
/// the signature's in name order, rather than by a pass over them.
const INDEXED_PARAMS: usize = 16;

/// One call site's arguments.
#[derive(Clone, Copy)]
pub(super) struct Call<'a, 'n> {
    pub name: &'n str,
    /// The member or function name's span.
    pub name_span: Span,
    pub args: &'a [Argument],
    pub block: Option<&'a Block>,
    /// A value passed after the arguments: an assigned value for `name=`
    /// and `[]=`.
    pub extra: Option<&'a Expr>,
    /// Positional selectors passed as plain expressions, for `[]`.
    pub selectors: &'a [Expr],
}

/// A candidate signature with the bindings its receiver gave.
type Candidate = (Rc<Sig>, Vec<Option<Ty>>);

impl<'a> Call<'a, '_> {
    fn empty(&self) -> bool {
        self.block.is_none()
            && self.extra.is_none()
            && self.selectors.is_empty()
            && self
                .args
                .iter()
                .all(|arg| match (&arg.kind, &arg.value.node) {
                    (ArgumentKind::Splat, Node::Array(items)) => items.is_empty(),
                    (ArgumentKind::KeywordSplat, Node::Hash(items)) => items.is_empty(),
                    _ => false,
                })
    }

    fn positional(&self) -> usize {
        self.args
            .iter()
            .map(|arg| match (&arg.kind, &arg.value.node) {
                (ArgumentKind::Positional, _) => 1,
                (ArgumentKind::Splat, Node::Array(items)) => items.len(),
                _ => 0,
            })
            .sum::<usize>()
            + self.selectors.len()
            + usize::from(self.extra.is_some())
    }

    fn keywords(&self) -> impl Iterator<Item = &'a str> {
        self.args.iter().filter_map(|arg| match &arg.kind {
            ArgumentKind::Keyword(name) => Some(name.as_str()),
            _ => None,
        })
    }
}

impl<'a> Checker<'a> {
    /// A call by a bare name: a method of `self`, a function, a host
    /// function or a builtin.
    pub(super) fn call_name(
        &mut self,
        expr: &'a Expr,
        name: &str,
        args: &'a [Argument],
        block: Option<&'a Block>,
    ) -> Ty {
        let call = Call {
            name,
            name_span: self.spans.token(expr.offset as usize),
            args,
            block,
            extra: None,
            selectors: &[],
        };
        let bare = args.is_empty() && block.is_none();
        if self.local(name).is_some() {
            let span = call.name_span;
            self.report(Diagnostic::error(
                Code::NOT_CALLABLE,
                span,
                text!(
                    self,
                    "`{name}` is a local, not a function; a local cannot be called"
                ),
            ));
            self.loose_args(&call);
            return Ty::ERROR;
        }
        match name {
            "raise" => {
                self.loose_args(&call);
                self.frame.flow.live = false;
                return Ty::NEVER;
            }
            "require" => {
                self.require(&call);
                let path = call
                    .args
                    .iter()
                    .find_map(|arg| match (&arg.kind, &arg.value.node) {
                        (ArgumentKind::Positional, Node::Literal(value)) => value.as_bytes(),
                        _ => None,
                    });
                let id = path.and_then(|path| self.modules.exports(&String::from_utf8_lossy(path)));
                return match id {
                    Some(id) => self.exports_type(id),
                    None => Ty::ANY,
                };
            }
            "block_given?" => {
                if !bare {
                    self.report(Diagnostic::error(
                        Code::NO_OVERLOAD,
                        call.name_span,
                        "`block_given?` takes no arguments or block",
                    ));
                    self.loose_args(&call);
                }
                return Ty::BOOL;
            }
            _ => (),
        }
        if let Some(ns) = self.frame.owner {
            let namespace = &self.program.namespaces[ns as usize];
            let methods = if self.frame.instance {
                &namespace.methods
            } else {
                &namespace.statics
            };
            if name.chars().next().is_some_and(char::is_uppercase)
                && !self.program.functions.contains_key(name)
                && !methods.contains_key(name)
                && self.constants.contains_key(&(Some(ns), self.copy(name)))
            {
                self.report(Diagnostic::error(
                    Code::NOT_CALLABLE,
                    call.name_span,
                    text!(self, "`{name}` is a namespace constant, not a function"),
                ));
                self.loose_args(&call);
                return Ty::ERROR;
            }
        }
        if let Some(&ty) = self.program.declared.get(name) {
            let found = self.types.display(ty);
            self.report(Diagnostic::error(
                Code::NOT_CALLABLE,
                call.name_span,
                text!(
                    self,
                    "`{name}` is a {found} the host declares, not a function"
                ),
            ));
            self.loose_args(&call);
            return Ty::ERROR;
        }
        if self.program.declared_calls.contains(name) {
            let sig = self.program.hosts[name].clone();
            return self.call_sigs(&call, &[(sig, Vec::new())]);
        }
        if let Some(ns) = self.frame.owner {
            if name == "new"
                && !self.frame.instance
                && self.program.namespaces[ns as usize].is_class
            {
                // `new` inside a class method constructs the class.
                let ty = self.types.intern(Kind::Namespace(ns));
                return self.namespace_member(&call, ns, ty);
            }
            let namespace = &self.program.namespaces[ns as usize];
            let found = if self.frame.instance {
                namespace.methods.get(name).copied()
            } else {
                namespace.statics.get(name).copied()
            };
            if let Some(id) = found {
                if self.frame.instance {
                    self.call_on_self(id, call.name_span);
                }
                let sig = self.program.fns[id].sig.clone();
                return self.call_sigs(&call, &[(sig, Vec::new())]);
            }
        }
        if let Some(&id) = self.program.functions.get(name) {
            let sig = self.program.fns[id].sig.clone();
            return self.call_sigs(&call, &[(sig, Vec::new())]);
        }
        if let Some(sig) = self.program.hosts.get(name).cloned() {
            return self.call_sigs(&call, &[(sig, Vec::new())]);
        }
        if let Some(sig) = self.modules.published.get(name).cloned() {
            return self.call_sigs(&call, &[(sig, Vec::new())]);
        }
        if let Some(functions) = sigs::index().globals.get(name) {
            let candidates: Vec<Candidate> = functions
                .iter()
                .map(|&function| {
                    (
                        self.converter.convert(&mut self.types, function, None),
                        Vec::new(),
                    )
                })
                .collect();
            if name == "loop" && block.is_some() {
                // `loop` ends only by `break`, whose values are its value.
                let (_, breaks) = self.call_sigs_parts(&call, &candidates);
                return self.types.union(&breaks);
            }
            return self.call_sigs(&call, &candidates);
        }
        if let Some(rename) = sigs::index().renames.get(&("global", name)) {
            // A removed spelling: the surface diagnostics report it.
            if let Some((None, canonical)) = rename.canonical() {
                if let Some(functions) = sigs::index().globals.get(canonical) {
                    let candidates: Vec<Candidate> = functions
                        .iter()
                        .map(|&function| {
                            (
                                self.converter.convert(&mut self.types, function, None),
                                Vec::new(),
                            )
                        })
                        .collect();
                    return self.call_sigs(&call, &candidates);
                }
            }
            self.loose_args(&call);
            return Ty::ERROR;
        }
        if bare
            && sigs::index()
                .renames
                .keys()
                .any(|(scope, _)| *scope == name)
        {
            // A removed namespace, such as `Regexp`: the surface
            // diagnostics report its spellings.
            return Ty::ANY;
        }
        let message = if bare {
            text!(
                self,
                "`{name}` is not a local, function or builtin in scope, and the host declares no global or capability of that name"
            )
        } else {
            text!(
                self,
                "`{name}` is not a function, method or builtin in scope"
            )
        };
        let diagnostic = Diagnostic::error(Code::UNDEFINED_NAME, call.name_span, message);
        self.foreign_call(expr, &call, diagnostic);
        Ty::ERROR
    }

    /// `require` with literal module names.
    fn require(&mut self, call: &Call<'a, '_>) {
        let positional = call
            .args
            .iter()
            .filter(|arg| matches!(arg.kind, ArgumentKind::Positional))
            .count();
        let aliases = call.keywords().filter(|name| *name == "as").count();
        if positional != 1 || aliases > 1 {
            self.report(Diagnostic::error(
                Code::NO_OVERLOAD,
                call.name_span,
                "`require` takes one module name and at most one `as:` alias",
            ));
        }
        if call.block.is_some() {
            self.report(Diagnostic::error(
                Code::UNEXPECTED_BLOCK,
                call.name_span,
                "`require` does not take a block",
            ));
        }
        let path = call.args.iter().find_map(|arg| {
            matches!(arg.kind, ArgumentKind::Positional)
                .then(|| super::expr::string_literal(&arg.value))
                .flatten()
        });
        let exports = path.as_deref().and_then(|path| self.modules.exports(path));
        for arg in call.args {
            self.expr(&arg.value, None);
            let span = self.spans.expr(&arg.value);
            let is_alias =
                matches!(&arg.kind, ArgumentKind::Keyword(name) if name.as_str() == "as");
            if let ArgumentKind::Keyword(name) = &arg.kind {
                if !is_alias {
                    self.report(Diagnostic::error(
                        Code::UNKNOWN_KEYWORD,
                        span,
                        text!(
                            self,
                            "`require` has no keyword `{name}`; its only keyword is `as`"
                        ),
                    ));
                    continue;
                }
            }
            let literal = match &arg.value.node {
                Node::Literal(value) => value
                    .as_bytes()
                    .and_then(|bytes| std::str::from_utf8(bytes).ok()),
                _ => None,
            };
            let literal = literal.filter(|_| {
                matches!(
                    arg.kind,
                    ArgumentKind::Positional | ArgumentKind::Keyword(_)
                )
            });
            let Some(literal) = literal else {
                self.report(Diagnostic::error(
                    Code::DYNAMIC_REQUIRE,
                    span,
                    "`require` takes its module name and alias as string literals, without splats",
                ));
                continue;
            };
            if !is_alias {
                continue;
            }
            let alias = literal.trim();
            let mut chars = alias.chars();
            if !chars
                .next()
                .is_some_and(|c| c == '_' || crate::syntax::unicode::letter(c))
                || !chars.all(|c| {
                    matches!(c, '_' | '?' | '!')
                        || crate::syntax::unicode::letter(c)
                        || crate::syntax::unicode::digit(c)
                })
                || crate::syntax::keyword(alias)
            {
                self.report(Diagnostic::error(
                    Code::INVALID_REQUIRE_ALIAS,
                    span,
                    "a `require` alias must be an identifier other than a keyword",
                ));
                continue;
            }
            let local_conflict = self.local(alias).is_some_and(|id| {
                let state = self.frame.flow.get(id);
                !matches!(self.types.kind(state.ty), Kind::Exports(id) if Some(*id) == exports)
            });
            let conflict = local_conflict
                || self.program.functions.contains_key(alias)
                || self.program.hosts.contains_key(alias)
                || self.program.declared.contains_key(alias)
                || self.constant(alias, self.frame.owner).is_some()
                || sigs::index().globals.contains_key(alias)
                || self
                    .modules
                    .aliases
                    .get(alias)
                    .is_some_and(|id| Some(*id) != exports);
            if conflict {
                self.report(Diagnostic::error(
                    Code::DUPLICATE_NAME,
                    span,
                    text!(
                        self,
                        "`require` alias `{alias}` is already defined; choose a free name"
                    ),
                ));
            }
        }
    }

    /// Checks arguments and a block without a signature, as after an error.
    pub(super) fn loose_args(&mut self, call: &Call<'a, '_>) {
        for arg in call.args {
            self.expr(&arg.value, None);
        }
        for selector in call.selectors {
            self.expr(selector, None);
        }
        if let Some(extra) = call.extra {
            self.expr(extra, None);
        }
        if let Some(block) = call.block {
            self.block(block, &[], Want::Discard);
        }
    }

    /// `(expr)(args)`: only functions are callable, and they are not values.
    pub(super) fn computed_call(
        &mut self,
        _expr: &'a Expr,
        receiver: &'a Expr,
        args: &'a [Argument],
        block: Option<&'a Block>,
    ) -> Ty {
        let ty = self.expr(receiver, None);
        let call = Call {
            name: "call",
            name_span: self.spans.expr(receiver),
            args,
            block,
            extra: None,
            selectors: &[],
        };
        self.loose_args(&call);
        if ty == Ty::ERROR {
            return Ty::ERROR;
        }
        if ty == Ty::ANY {
            self.usable(receiver, ty, "calling it");
            return Ty::ERROR;
        }
        let found = self.types.display(ty);
        self.report(Diagnostic::error(
            Code::NOT_CALLABLE,
            call.name_span,
            text!(self, "a value of type {found} cannot be called"),
        ));
        Ty::ERROR
    }

    /// A call with an attached block.
    pub(super) fn block_call(&mut self, expr: &'a Expr, call: &'a Expr, block: &'a Block) -> Ty {
        match &call.node {
            Node::Call(name, args, _) => self.call_name(expr, name, args, Some(block)),
            Node::Var(name) => self.call_name(expr, name, &[], Some(block)),
            Node::Method(receiver, name, args, _) => {
                self.method_call(expr, receiver, name, args, Some(block), false)
            }
            Node::SafeMethod(receiver, name, args, _) => {
                self.method_call(expr, receiver, name, args, Some(block), true)
            }
            Node::Member(receiver, name) => {
                self.method_call(expr, receiver, name, &[], Some(block), false)
            }
            Node::SafeMember(receiver, name) => {
                self.method_call(expr, receiver, name, &[], Some(block), true)
            }
            Node::Scope(receiver, name, args) => {
                self.scope(expr, receiver, name, args.as_deref(), Some(block))
            }
            Node::ComputedCall(receiver, args) => {
                self.computed_call(expr, receiver, args, Some(block))
            }
            _ => {
                self.expr(call, None);
                self.block(block, &[], Want::Discard);
                Ty::ERROR
            }
        }
    }

    /// A call on an explicit receiver: `receiver.name(args) { block }`.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn method_call(
        &mut self,
        expr: &'a Expr,
        receiver: &'a Expr,
        name: &'a str,
        args: &'a [Argument],
        block: Option<&'a Block>,
        safe: bool,
    ) -> Ty {
        if !safe && crate::bytecode::mutating_member(name) {
            self.mark_write_chain(receiver);
        }
        // A mutating member's receiver chain is addressed, and records
        // the types of its parts for [`Self::field_addresses`].
        let addressed = crate::bytecode::mutating_member(name)
            && matches!(receiver.node, Node::Member(..) | Node::SafeMember(..))
            && !self.memo.get().is_some_and(|memo| memo.replay);
        let outer = addressed.then(|| self.set_memo(Some(super::Memo::default())));
        let ty = self.member_receiver(receiver, name);
        if let Some(outer) = outer {
            self.field_addresses(receiver, name);
            self.restore_memo(outer);
        }
        if ty == Ty::ERROR {
            self.foreign_namespace(receiver, name);
        }
        if ty != Ty::ERROR && block.is_none() {
            let called = if safe { self.types.without_nil(ty) } else { ty };
            let base = crate::members::direct::Base::of(&self.types.bases(called));
            self.facts.record_base(self.meter.tables(), expr, base);
            let class = match self.types.kind(called) {
                Kind::Instance(ns) => {
                    let namespace = &self.program.namespaces[*ns as usize];
                    (namespace.module.is_some()
                        && !matches!(name, "initialize" | "class")
                        && namespace.methods.contains_key(name))
                    .then_some(namespace.name.as_str())
                }
                _ => None,
            };
            self.facts.record_class(self.meter.tables(), expr, class);
        }
        let name_span = self.spans.member(receiver, name);
        if let Some(span) = name_span {
            if ty != Ty::ERROR {
                // A safe call runs the member on the value without nil.
                let called = if safe { self.types.without_nil(ty) } else { ty };
                let mut receiver_type =
                    ReceiverType::new(self.types.display(called), self.types.bases(called));
                receiver_type.user_method = self.types.members(called).iter().all(|&ty| match self
                    .types
                    .kind(ty)
                {
                    Kind::Instance(ns) => {
                        name != "initialize"
                            && self.program.namespaces[*ns as usize]
                                .methods
                                .contains_key(name)
                    }
                    Kind::Namespace(ns) => self.program.namespaces[*ns as usize]
                        .statics
                        .contains_key(name),
                    Kind::Exports(id) => self.modules.loaded[*id as usize]
                        .functions
                        .contains_key(name),
                    _ => false,
                });
                // A check that counting it stops keeps no more receivers;
                // one it keeps is counted, with what it owns, as it is kept,
                // and by the measures after.
                let bytes = super::meter::Heap::heap(&receiver_type);
                if self
                    .calls
                    .push(self.meter.tables(), (span.start, receiver_type))
                    .is_err()
                {
                    return Ty::ERROR;
                }
                self.grown += bytes;
            }
        }
        let call = Call {
            name,
            name_span: name_span.unwrap_or_else(|| self.spans.expr(expr)),
            args,
            block,
            extra: None,
            selectors: &[],
        };
        // A mutating member updates the place its receiver names, which a
        // shape types; a temporary's update is not seen again.
        let shapes = if crate::bytecode::mutating_member(name) && self.place(receiver) {
            self.shapes(ty)
        } else {
            Vec::new()
        };
        if name == "replace" && !shapes.is_empty() {
            return self.shape_replace(&call, receiver, ty, safe, &shapes);
        }
        let removes = shapes
            .iter()
            .any(|&shape| self.removes_required(shape, name, args));
        let reported = self.diagnostics.len();
        let result = self.checked_member(&call, receiver, ty, safe);
        // A call that is wrong already is reported for that instead.
        if removes && self.diagnostics.len() == reported {
            self.shape_mutation(&call, receiver, "remove one it requires");
        }
        result
    }

    /// Checks a member call on a receiver of type `ty`, through `&.` when
    /// `safe`.
    fn checked_member(
        &mut self,
        call: &Call<'a, '_>,
        receiver: &'a Expr,
        ty: Ty,
        safe: bool,
    ) -> Ty {
        if safe {
            if ty == Ty::ERROR {
                self.loose_args(call);
                return Ty::ERROR;
            }
            let without = self.types.without_nil(ty);
            if without == Ty::NEVER {
                self.loose_args(call);
                return Ty::NIL;
            }
            let result = self.dispatch(call, receiver, without);
            return self.types.optional(result);
        }
        self.dispatch(call, receiver, ty)
    }

    /// Checks `receiver.replace(other)` on a record: `other` must be a
    /// record of the receiver's shape, which `replace` otherwise could leave
    /// without fields it requires or with fields it does not declare.
    fn shape_replace(
        &mut self,
        call: &Call<'a, '_>,
        receiver: &'a Expr,
        ty: Ty,
        safe: bool,
        shapes: &[Ty],
    ) -> Ty {
        let argument = match call.args {
            [
                Argument {
                    kind: ArgumentKind::Positional,
                    value,
                },
            ] => Some(value),
            _ => None,
        };
        // A record literal takes its fields' types from the shape it
        // replaces; the call checks it against a dictionary.
        let literal = argument
            .filter(|value| matches!(value.node, Node::Hash(_)))
            .map(|value| {
                self.mute += 1;
                let ty = self.expr(value, Some(shapes[0]));
                self.mute -= 1;
                ty
            });
        let outer = self.set_memo(Some(super::Memo::default()));
        let reported = self.diagnostics.len();
        let result = self.checked_member(call, receiver, ty, safe);
        let valid = self.diagnostics.len() == reported;
        // Another argument's type, as the call just checked it.
        self.memo.get_mut().unwrap().replay = true;
        self.mute += 1;
        let other = literal.or_else(|| argument.map(|value| self.expr(value, None)));
        self.mute -= 1;
        self.restore_memo(outer);
        // A literal that the shape does not type is not one of its records.
        let mismatched = match (literal, other) {
            (Some(Ty::ERROR), _) => true,
            (_, Some(other)) if other != Ty::ERROR => shapes
                .iter()
                .any(|&shape| !self.types.assignable(other, shape)),
            _ => false,
        };
        if mismatched && valid {
            self.shape_mutation(
                call,
                receiver,
                "give it fields other than those it declares",
            );
        }
        result
    }

    /// Checks `receiver.name` for a receiver type, one alternative at a time.
    fn dispatch(&mut self, call: &Call<'a, '_>, receiver: &'a Expr, ty: Ty) -> Ty {
        if call.name == "as" && !matches!(self.types.kind(ty), Kind::Instance(_)) {
            // A cast narrows the whole value, whichever alternative it holds.
            return self.cast(call, ty);
        }
        let alternatives = self.types.members(ty);
        if alternatives.len() == 1 {
            return self.member(call, ty);
        }
        // `nil` must answer the member too, or the value needs a nil test.
        let mut others: Vec<Ty> = Vec::new();
        let mut nil = false;
        for &alternative in &alternatives {
            if alternative == Ty::NIL {
                nil = true;
            } else {
                others.push(alternative);
            }
        }
        let mut results = Vec::new();
        if nil {
            if self.answers(Ty::NIL, call.name) {
                others.push(Ty::NIL);
            } else {
                let found = self.types.display(ty);
                let mut diagnostic = Diagnostic::error(
                    Code::OPTIONAL_USE,
                    call.name_span,
                    text!(
                        self,
                        "`{}` is not defined for nil, and this value is {found}; test it with `!= nil` first, or call through `&.`",
                        call.name
                    ),
                );
                let without = self.types.without_nil(ty);
                if let Some(fix) = self.fetch_fix(receiver, ty, without) {
                    diagnostic = diagnostic.with_fix(fix);
                } else if matches!(receiver.node, Node::Index(..)) {
                    diagnostic = diagnostic.with_label(
                        self.spans.expr(receiver),
                        "bind this indexed value to a local, then test that local with `!= nil`",
                    );
                }
                self.report(diagnostic);
            }
        }
        let Some((&first, rest)) = others.split_first() else {
            self.loose_args(call);
            return Ty::ERROR;
        };
        let outer = self.set_memo(Some(super::Memo::default()));
        let mark = self.frame.flow.mark();
        results.push(self.member(call, first));
        let mut branches = Vec::new();
        let branch = self.frame.flow.rollback(mark);
        self.explore(&mut branches, branch);
        // Reuse evaluated argument types, but check every receiver's contract.
        self.memo.get_mut().unwrap().replay = true;
        for &alternative in rest {
            let mark = self.frame.flow.mark();
            results.push(self.member(call, alternative));
            let branch = self.frame.flow.rollback(mark);
            self.explore(&mut branches, branch);
        }
        self.join_explored(branches);
        self.restore_memo(outer);
        self.types.union(&results)
    }

    /// Whether a receiver type has a member named `name`.
    fn answers(&mut self, ty: Ty, name: &str) -> bool {
        sigs::declares(&self.types, ty, name) || matches!(name, "as" | "to_s")
    }

    /// Checks a member call on a receiver of a single type.
    fn member(&mut self, call: &Call<'a, '_>, ty: Ty) -> Ty {
        let kind = &*self.types.shared(ty);
        match kind {
            Kind::Error | Kind::Never => {
                self.loose_args(call);
                ty
            }
            Kind::Nil if matches!(call.name, "==" | "!=") => Ty::BOOL,
            Kind::Tuple(_) if crate::bytecode::mutating_member(call.name) => {
                self.report(Diagnostic::error(
                    Code::TUPLE_MUTATION,
                    call.name_span,
                    "tuples have fixed elements; assign an element by its literal index or copy to a declared array before calling a mutating member",
                ));
                self.loose_args(call);
                Ty::ERROR
            }
            Kind::Any => {
                if matches!(call.name, "is_type?" | "as" | "==" | "!=") {
                    return self.table_member(call, ty);
                }
                self.report(Diagnostic::error(
                    Code::ANY_USE,
                    call.name_span,
                    text!(self,
                        "this value has type any; narrow it with `is_type?`, `.as(T)` or `JSON.parse_as` before calling `{}`",
                        call.name
                    ),
                ));
                self.loose_args(call);
                Ty::ERROR
            }
            Kind::Instance(ns) => {
                let method = self.program.namespaces[*ns as usize]
                    .methods
                    .get(call.name)
                    .copied();
                match method {
                    Some(id) if call.name != "initialize" => {
                        self.visibility(call.name, call.name_span, id, *ns, true);
                        let sig = self.program.fns[id].sig.clone();
                        self.call_sigs(call, &[(sig, Vec::new())])
                    }
                    _ if matches!(call.name, "to_s" | "inspect") => {
                        self.no_rendering(call, ty, "an instance")
                    }
                    _ => self.table_member(call, ty),
                }
            }
            Kind::Namespace(ns) => self.namespace_member(call, *ns, ty),
            Kind::Exports(id) => match self.exported(*id, call.name) {
                Some(sig) => self.call_sigs(call, &[(sig, Vec::new())]),
                None => match self.exported_enum(*id, call.name) {
                    Some(enumeration) if call.args.is_empty() && call.block.is_none() => {
                        self.types.intern(Kind::EnumType(enumeration))
                    }
                    _ => {
                        self.unknown_export(*id, call.name, call.name_span);
                        self.loose_args(call);
                        Ty::ERROR
                    }
                },
            },
            Kind::Builtin(index) => self.builtin_member(call, *index, ty),
            Kind::Host(index) => self.host_member(call, *index, ty),
            _ => self.table_member(call, ty),
        }
    }

    /// Whether an expression names a place a mutating member updates: a
    /// local or instance variable, or an element of one. Any other value,
    /// such as a property's, is a copy.
    fn place(&self, expr: &Expr) -> bool {
        match &expr.node {
            Node::Var(name) => name.starts_with('@') || self.local(name).is_some(),
            Node::Index(receiver, _) => self.place(receiver),
            _ => false,
        }
    }

    /// Reports each `.m` in the receiver chain of mutating member `name`
    /// whose receiver may be a hash holding a field `m`: the runtime then
    /// updates that field, which the checker types as the member's result.
    fn field_addresses(&mut self, receiver: &Expr, name: &str) {
        let mut node = receiver;
        while let Node::Member(inner, member) | Node::SafeMember(inner, member) = &node.node {
            let types = &self.memo.get().unwrap().types;
            let called = types.get(&(std::ptr::from_ref(node) as usize)).copied();
            let Some(inner_ty) = types.get(&(std::ptr::from_ref(&**inner) as usize)).copied()
            else {
                return;
            };
            // An unknown member was reported already.
            let fields =
                called.is_some_and(|called| called != Ty::ERROR)
                    && self.types.members(inner_ty).into_iter().any(|ty| {
                        match self.types.kind(ty) {
                            Kind::Shape(fields, open) => {
                                *open || Types::field(fields, member.as_bytes()).is_some()
                            }
                            Kind::Hash(_) => true,
                            _ => false,
                        }
                    });
            if fields {
                let span = self
                    .spans
                    .member(inner, member)
                    .unwrap_or_else(|| self.spans.expr(node));
                self.report(Diagnostic::error(
                    Code::FIELD_ACCESS,
                    span,
                    text!(self,
                        "`.{member}` in front of `{name}` updates the field `{member}` when the hash has one, not the result of `{member}`; index the field, `[\"{member}\"]`, or update a local holding the result"
                    ),
                ));
                return;
            }
            node = inner;
        }
    }

    /// The shapes among `ty`'s alternatives.
    fn shapes(&self, ty: Ty) -> Vec<Ty> {
        self.types
            .members(ty)
            .into_iter()
            .filter(|&ty| matches!(self.types.kind(ty), Kind::Shape(..)))
            .collect()
    }

    /// Whether `name`, a hash member that removes keys, could remove a field
    /// the shape `ty` requires: `delete` of a string literal naming an
    /// optional field, or of one it does not declare, removes none.
    fn removes_required(&self, ty: Ty, name: &str, args: &[Argument]) -> bool {
        let Kind::Shape(fields, _) = self.types.kind(ty) else {
            return false;
        };
        if name == "delete" {
            let key = match args.first() {
                Some(Argument {
                    kind: ArgumentKind::Positional,
                    value,
                }) => super::expr::string_literal(value),
                _ => None,
            };
            if let Some(key) = key {
                return fields
                    .iter()
                    .any(|field| !field.optional && *field.name == *key);
            }
        }
        matches!(name, "delete" | "delete_if" | "keep_if" | "clear")
            && fields.iter().any(|field| !field.optional)
    }

    /// Reports a hash member that could leave a record in `receiver`
    /// without fields its shape requires, or with fields it does not
    /// declare, offering to declare a local a dictionary when its fields
    /// share a type.
    fn shape_mutation(&mut self, call: &Call<'a, '_>, receiver: &Expr, what: &str) {
        let mut diagnostic = Diagnostic::error(
            Code::SHAPE_MUTATION,
            call.name_span,
            text!(
                self,
                "a record keeps the fields its shape declares, but `{}` could {what}; assign it a new record, or declare a dictionary, `hash<string, V>`",
                call.name
            ),
        );
        if let Node::Var(name) = &receiver.node {
            if let Some(id) = self.local(name) {
                let local = &self.frame.locals[id as usize];
                if let (Some(value), false) = (local.dictionary, local.annotated) {
                    let value_text = self.types.display(value);
                    let name_span = self.spans.token(local.offset);
                    diagnostic = diagnostic.with_fix(Fix::insert(
                        text!(self, "declare `{name}: hash<string, {value_text}>`"),
                        name_span.end,
                        text!(self, ": hash<string, {value_text}>"),
                    ));
                }
            }
        }
        self.report(diagnostic);
    }

    /// Reports `to_s` or `inspect` called on an instance or a namespace that
    /// does not define it: the runtime renders them only through
    /// interpolation and the output helpers.
    fn no_rendering(&mut self, call: &Call<'a, '_>, ty: Ty, what: &str) -> Ty {
        let found = self.types.display(ty);
        self.report(Diagnostic::error(
            Code::UNKNOWN_MEMBER,
            call.name_span,
            text!(self,
                "{found} has no member `{}`; {what} renders through interpolation, `p` and `puts`, or define `def {} -> string`",
                call.name, call.name
            ),
        ));
        self.loose_args(call);
        Ty::ERROR
    }

    /// A member of a capability the host declares, such as `SMS.send`.
    fn host_member(&mut self, call: &Call<'a, '_>, index: u32, ty: Ty) -> Ty {
        let module = self.program.host_modules[index as usize];
        let mut candidates = Vec::new();
        for member in module
            .members
            .iter()
            .filter(|member| member.name() == call.name)
        {
            match member {
                crate::signatures::Member::Function(function) => {
                    let sig = self
                        .converter
                        .convert_owned(&mut self.types, function, None)
                        .host();
                    candidates.push((Rc::new(sig), Vec::new()));
                }
                crate::signatures::Member::Module(module) => {
                    self.non_callable_member(call);
                    self.loose_args(call);
                    let known = self
                        .program
                        .host_modules
                        .iter()
                        .position(|&known| std::ptr::eq(known, module));
                    let id = match known {
                        Some(id) => id,
                        None => {
                            // Counted before it is kept; a check that stops
                            // names no more modules.
                            let name = text!(self, "{}.{}", self.types.display(ty), module.name);
                            let declarations = self.meter.declarations();
                            let Ok(mut kept) = declarations.keep(name.capacity()) else {
                                return Ty::ERROR;
                            };
                            if self.program.host_modules.reserve(declarations, 1).is_err()
                                || self.types.names.hosts.reserve(declarations, 1).is_err()
                            {
                                return Ty::ERROR;
                            }
                            let id = self.program.host_modules.len();
                            self.program.host_modules.push_within(module);
                            self.types.names.hosts.push_kept(&mut kept, name);
                            id
                        }
                    };
                    return self.types.intern(Kind::Host(id as u32));
                }
                crate::signatures::Member::Constant(constant) => {
                    self.non_callable_member(call);
                    self.loose_args(call);
                    return sigs::table_type(&mut self.types, &constant.ty, &[]);
                }
            }
        }
        if candidates.is_empty() {
            if sigs::declares(&self.types, ty, call.name) {
                return self.table_member(call, ty);
            }
            self.report(Diagnostic::error(
                Code::UNKNOWN_MEMBER,
                call.name_span,
                text!(
                    self,
                    "{} declares no member `{}`",
                    self.types.display(ty),
                    call.name
                ),
            ));
            self.loose_args(call);
            return Ty::ERROR;
        }
        self.call_sigs(call, &candidates)
    }

    fn non_callable_member(&mut self, call: &Call<'a, '_>) {
        if !call.args.is_empty() || call.block.is_some() {
            self.report(Diagnostic::error(
                Code::NOT_CALLABLE,
                call.name_span,
                text!(self, "`{}` is data, not a callable member", call.name),
            ));
        }
    }

    /// `Class.new`, `Class.method` and `Module.function`.
    fn namespace_member(&mut self, call: &Call<'a, '_>, ns: NsId, ty: Ty) -> Ty {
        let namespace = &self.program.namespaces[ns as usize];
        if call.name == "new" && namespace.is_class {
            let initialize = namespace.methods.get("initialize").copied();
            let instance = self.types.intern(Kind::Instance(ns));
            let sig = match initialize {
                Some(id) => {
                    let sig = self.program.fns[id].sig.clone();
                    let mut sig = (*sig).clone();
                    sig.name = text!(self, "{}.new", self.program.namespaces[ns as usize].name);
                    // A break out of the block is the value of `new`,
                    // unchecked by the initializer's result.
                    if sig.breaks == sigs::Breaks::Result {
                        sig.breaks = sigs::Breaks::Call;
                    }
                    sig
                }
                None => Sig {
                    name: text!(self, "{}.new", self.program.namespaces[ns as usize].name),
                    params: Vec::new(),
                    result: None,
                    block: None,
                    vars: Vec::new(),
                    breaks: sigs::Breaks::Call,
                    converts: true,
                    id: None,
                },
            };
            let (_, breaks) = self.call_sigs_parts(call, &[(Rc::new(sig), Vec::new())]);
            return self.with_breaks(instance, &breaks);
        }
        if let Some(&id) = namespace.statics.get(call.name) {
            self.visibility(call.name, call.name_span, id, ns, false);
            let sig = self.program.fns[id].sig.clone();
            return self.call_sigs(call, &[(sig, Vec::new())]);
        }
        if call.args.is_empty() && call.block.is_none() {
            if let Some(&ty) = self.constants.get(&(Some(ns), self.copy(call.name))) {
                return ty;
            }
            if let Some(&child) = namespace.children.get(call.name) {
                return self.types.intern(Kind::Namespace(child));
            }
        }
        if call.name == "inspect" {
            return self.no_rendering(call, ty, "a class or module");
        }
        self.table_member(call, ty)
    }

    /// A member of a builtin namespace, such as `Math.sqrt` or `Math::PI`.
    fn builtin_member(&mut self, call: &Call<'a, '_>, index: u32, ty: Ty) -> Ty {
        let module: &'static crate::signatures::Module = sigs::index().modules[index as usize].1;
        let mut candidates = Vec::new();
        for member in module
            .members
            .iter()
            .filter(|member| member.name() == call.name)
        {
            match member {
                crate::signatures::Member::Function(function) => {
                    candidates.push((
                        self.converter.convert(&mut self.types, function, None),
                        Vec::new(),
                    ));
                }
                crate::signatures::Member::Module(_) => continue,
                crate::signatures::Member::Constant(constant) => {
                    self.loose_args(call);
                    return sigs::table_type(&mut self.types, &constant.ty, &[]);
                }
            }
        }
        if candidates.is_empty() {
            if let Some(rename) = sigs::index()
                .renames
                .get(&(module.name.as_str(), call.name))
            {
                if let Some((None, canonical)) = rename.canonical() {
                    let renamed = Call {
                        name: canonical,
                        ..*call
                    };
                    self.mute += 1;
                    let result = self.builtin_member(&renamed, index, ty);
                    self.mute -= 1;
                    self.removed_rename(call, canonical);
                    return result;
                }
                self.loose_args(call);
                return Ty::ERROR;
            }
            return self.table_member(call, ty);
        }
        self.call_sigs(call, &candidates)
    }

    /// A member from the signature table's classes.
    fn table_member(&mut self, call: &Call<'a, '_>, ty: Ty) -> Ty {
        if call.empty() {
            for base in sigs::bases(&self.types, ty) {
                if let Some(rename) = sigs::index().renames.get(&(*base, call.name)) {
                    if !rename.pattern.contains('(') {
                        if let Some((None, canonical)) = rename.canonical() {
                            if canonical != call.name {
                                let renamed = Call {
                                    name: canonical,
                                    ..*call
                                };
                                let result = self.table_member(&renamed, ty);
                                self.removed_rename(call, canonical);
                                return result;
                            }
                        }
                    }
                }
            }
        }
        let found = sigs::members(&mut self.types, ty, call.name);
        if !found.is_empty() {
            let candidates: Vec<Candidate> = found
                .into_iter()
                .map(|(function, class, bindings)| {
                    (
                        self.converter
                            .convert(&mut self.types, function, Some(class)),
                        bindings,
                    )
                })
                .collect();
            let (result, breaks) = self.call_sigs_parts(call, &candidates);
            let result = self.member_result(call, ty, result);
            return self.with_breaks(result, &breaks);
        }
        if call.name == "as" {
            return self.cast(call, ty);
        }
        // A removed spelling is the surface diagnostics' to report; type
        // the call as its replacement when that is a plain rename.
        for base in sigs::bases(&self.types, ty) {
            if let Some(rename) = sigs::index().renames.get(&(*base, call.name)) {
                if let Some((None, canonical)) = rename.canonical() {
                    if canonical != call.name {
                        let renamed = Call {
                            name: canonical,
                            ..*call
                        };
                        self.mute += 1;
                        let result = self.table_member(&renamed, ty);
                        self.mute -= 1;
                        self.removed_rename(call, canonical);
                        return result;
                    }
                }
                if *base == "string" && call.name == "replace" {
                    self.report(Diagnostic::error(
                        Code::REMOVED_NAME,
                        call.name_span,
                        "`string.replace` was removed; assign the replacement string directly",
                    ));
                }
                self.loose_args(call);
                return Ty::ERROR;
            }
        }
        let found = self.types.display(ty);
        let mut diagnostic = Diagnostic::error(
            Code::UNKNOWN_MEMBER,
            call.name_span,
            text!(self, "{found} has no member `{}`", call.name),
        );
        let canonical = match (self.types.kind(ty), call.name) {
            (Kind::Array(_), "filter") => Some("select"),
            (Kind::String, "trim") => Some("strip"),
            (Kind::Array(_) | Kind::String, "includes") => Some("include?"),
            _ => None,
        };
        if let Some(canonical) = canonical {
            diagnostic = diagnostic.with_fix(
                Fix::replace(
                    text!(self, "the Vibescript member is `{canonical}`; check its arguments and block in `vibes prelude`"),
                    call.name_span,
                    canonical,
                )
                .suggestion(),
            );
        }
        self.report(diagnostic);
        self.loose_args(call);
        Ty::ERROR
    }

    fn removed_rename(&mut self, call: &Call<'a, '_>, canonical: &str) {
        let advice = text!(self, "use `{canonical}`");
        let mut diagnostic = Diagnostic::error(
            Code::REMOVED_NAME,
            call.name_span,
            text!(self, "`{}` was removed; {advice}", call.name),
        );
        if call.empty() {
            diagnostic = diagnostic.with_fix(Fix::replace(advice, call.name_span, canonical));
        }
        self.report(diagnostic);
    }

    fn removed_scoped_call(&mut self, call: &Call<'a, '_>, rename: &crate::signatures::Rename) {
        let code = match call.name {
            "nil?" => Code::NIL_PREDICATE,
            "eql?" | "equal?" => Code::IDENTITY_EQUALITY,
            "itself" | "tap" | "yield_self" => Code::IDENTITY_CALL,
            "send" | "public_send" | "respond_to?" => Code::DISPATCH_BY_NAME,
            "new" if rename.receiver == "Hash" => Code::HASH_NEW,
            _ => Code::REMOVED_NAME,
        };
        let advice = match &rename.replacement {
            crate::signatures::Replacement::Manual(hint) => hint.clone(),
            crate::signatures::Replacement::Rewrite(template) => text!(self, "use `{template}`"),
        };
        self.report(Diagnostic::error(
            code,
            call.name_span,
            text!(self, "`{}` was removed; {advice}", call.name),
        ));
    }

    /// `value.as(T)`: a checked cast of `any` or a union to `T`.
    fn cast(&mut self, call: &Call<'a, '_>, ty: Ty) -> Ty {
        let [arg] = call.args else {
            self.report(Diagnostic::error(
                Code::NO_OVERLOAD,
                call.name_span,
                "`as` takes one type, as in `value.as(int)`",
            ));
            self.loose_args(call);
            return Ty::ERROR;
        };
        let literal = self.expr(&arg.value, None);
        let literal = self.nominal_type(literal);
        let Kind::TypeLit(target) = *self.types.kind(literal) else {
            if literal != Ty::ERROR {
                let span = self.spans.expr(&arg.value);
                let found = self.types.display(literal);
                self.report(
                    Diagnostic::error(
                        Code::TYPE_MISMATCH,
                        span,
                        text!(self, "`as` takes a type, found {found}"),
                    )
                    .with_types("type<T>", found),
                );
            }
            return Ty::ERROR;
        };
        if ty != Ty::ANY && ty != Ty::ERROR {
            // Some alternative of one fits the other, compared through the
            // unions' indexes rather than pair by pair.
            let symbol = self.types.members(ty).contains(&Ty::SYMBOL);
            let possible = self
                .types
                .members(ty)
                .into_iter()
                .any(|m| self.types.assignable(m, target))
                || self.types.members(target).into_iter().any(|t| {
                    self.types.assignable(t, ty)
                        || (symbol && matches!(self.types.kind(t), Kind::EnumValue(_)))
                });
            if !possible {
                let found = self.types.display(ty);
                let wanted = self.types.display(target);
                self.report(Diagnostic::error(
                    Code::CAST,
                    call.name_span,
                    text!(self, "a value of type {found} can never be {wanted}"),
                ));
            }
        }
        if let Some(block) = call.block {
            self.block(block, &[], Want::Discard);
        }
        target
    }

    /// The type literal a class or enum used as a value names, since each
    /// names its own type; any other type as it is.
    fn nominal_type(&mut self, ty: Ty) -> Ty {
        match *self.types.kind(ty) {
            Kind::EnumType(id) => {
                let member = self.types.intern(Kind::EnumValue(id));
                self.types.type_lit(member)
            }
            Kind::Namespace(ns) if self.program.namespaces[ns as usize].is_class => {
                let instance = self.types.intern(Kind::Instance(ns));
                self.types.type_lit(instance)
            }
            _ => ty,
        }
    }

    /// `A::B`, `Enum::Member`, `Math::PI` and `Module::function(args)`.
    pub(super) fn scope(
        &mut self,
        expr: &'a Expr,
        receiver: &'a Expr,
        name: &'a str,
        args: Option<&'a [Argument]>,
        block: Option<&'a Block>,
    ) -> Ty {
        let ty = self.expr(receiver, None);
        let call = Call {
            name,
            name_span: self
                .spans
                .member(receiver, name)
                .unwrap_or_else(|| self.spans.expr(expr)),
            args: args.unwrap_or(&[]),
            block,
            extra: None,
            selectors: &[],
        };
        match *self.types.kind(ty) {
            Kind::EnumValue(_) | Kind::AnyEnum => {
                let span = self
                    .spans
                    .member_operator(receiver, name)
                    .unwrap_or(call.name_span);
                self.report(
                    Diagnostic::error(
                        Code::SCOPED_CALL,
                        span,
                        "an enum value has no constants; call its members with a dot",
                    )
                    .with_fix(Fix::replace(
                        "call the member with a dot",
                        span,
                        ".",
                    )),
                );
                self.dispatch(&call, receiver, ty)
            }
            Kind::EnumType(id) if args.is_none() => {
                let decl = &self.program.enums[id as usize];
                if decl.member(name).is_some() {
                    return self.types.intern(Kind::EnumValue(id));
                }
                let enum_name = decl.name.clone();
                self.report(Diagnostic::error(
                    Code::UNKNOWN_ENUM_MEMBER,
                    call.name_span,
                    text!(self, "`{enum_name}` has no member `{name}`"),
                ));
                Ty::ERROR
            }
            Kind::Namespace(ns) if args.is_none() && block.is_none() => {
                if let Some(&ty) = self.constants.get(&(Some(ns), self.copy(name))) {
                    return ty;
                }
                if let Some(&child) = self.program.namespaces[ns as usize].children.get(name) {
                    return self.types.intern(Kind::Namespace(child));
                }
                if name.chars().next().is_some_and(char::is_uppercase)
                    && self.program.namespaces[ns as usize]
                        .statics
                        .contains_key(name)
                {
                    let span = self.spans.member_operator(receiver, name).unwrap();
                    self.report(
                        Diagnostic::error(
                            Code::SCOPED_CALL,
                            span,
                            "`::` names constants, nested types and enum members; call this method with a dot",
                        )
                        .with_fix(Fix::replace("call the method with a dot", span, ".")),
                    );
                }
                self.dispatch(&call, receiver, ty)
            }
            _ => {
                let before = self.diagnostics.len();
                let result = self.dispatch(&call, receiver, ty);
                // The surface walk leaves scoped calls on literal values alone.
                // Report a removed member if dispatch otherwise failed silently.
                if result == Ty::ERROR && self.diagnostics.len() == before {
                    for base in sigs::bases(&self.types, ty) {
                        if let Some(rename) = sigs::index().renames.get(&(*base, name)) {
                            self.removed_scoped_call(&call, rename);
                            break;
                        }
                    }
                }
                result
            }
        }
    }

    /// `receiver.name = value`: a setter.
    pub(super) fn setter(
        &mut self,
        expr: &'a Expr,
        receiver: &'a Expr,
        name: &'a str,
        value_ty: Ty,
        value: &'a Expr,
        evaluate: bool,
    ) -> Ty {
        let setter = text!(self, "{name}=");
        let setter = setter.as_str();
        let ty = self.member_receiver(receiver, setter);
        let name_span = self.spans.member(receiver, name);
        if let (Some(span), false) = (name_span, ty == Ty::ERROR) {
            let receiver_type = ReceiverType::new(self.types.display(ty), self.types.bases(ty));
            // A check that counting it stops keeps no more receivers; one
            // it keeps is counted, with what it owns, as it is kept, and by
            // the measures after.
            let bytes = super::meter::Heap::heap(&receiver_type);
            if self
                .calls
                .push(self.meter.tables(), (span.start, receiver_type))
                .is_err()
            {
                return Ty::ERROR;
            }
            self.grown += bytes;
        }
        if let Kind::Host(index) = *self.types.kind(ty) {
            let module = self.program.host_modules[index as usize];
            if let Some(crate::signatures::Member::Constant(constant)) =
                module.members.iter().find(|member| member.name() == name)
            {
                let expected = sigs::table_type(&mut self.types, &constant.ty, &[]);
                if evaluate {
                    return self.expr_against(value, expected, &Purpose::Operand);
                }
                if !self.types.assignable(value_ty, expected) {
                    self.mismatch(
                        self.spans.expr(value),
                        expected,
                        value_ty,
                        &Purpose::Operand,
                    );
                }
                return value_ty;
            }
            self.report(Diagnostic::error(
                Code::UNKNOWN_MEMBER,
                name_span.unwrap_or_else(|| self.spans.expr(expr)),
                text!(
                    self,
                    "{} has no writable data member `{name}`",
                    self.types.display(ty)
                ),
            ));
            return if evaluate {
                self.expr(value, None)
            } else {
                value_ty
            };
        }
        let call = Call {
            name: setter,
            name_span: name_span.unwrap_or_else(|| self.spans.expr(expr)),
            args: &[],
            block: None,
            extra: evaluate.then_some(value),
            selectors: &[],
        };
        if !evaluate {
            // A compound assignment checks the computed value against the
            // setter, which must exist as it must for a plain assignment.
            // Reading the member reported a `nil` receiver already.
            for alternative in self.types.members(ty) {
                if alternative == Ty::NIL {
                    continue;
                }
                let method = match *self.types.kind(alternative) {
                    Kind::Instance(ns) => self.program.namespaces[ns as usize]
                        .methods
                        .get(setter)
                        .map(|&id| (ns, id, true)),
                    Kind::Namespace(ns) => self.program.namespaces[ns as usize]
                        .statics
                        .get(setter)
                        .map(|&id| (ns, id, false)),
                    Kind::Error | Kind::Any | Kind::Never => continue,
                    _ => None,
                };
                let Some((ns, id, instance)) = method else {
                    let found = self.types.display(alternative);
                    self.report(Diagnostic::error(
                        Code::UNKNOWN_MEMBER,
                        call.name_span,
                        text!(self, "{found} has no member `{setter}`"),
                    ));
                    break;
                };
                let span = name_span.unwrap_or_else(|| self.spans.expr(expr));
                self.visibility(setter, span, id, ns, instance);
                let sig = self.program.fns[id].sig.clone();
                if let Some(param) = sig.params.first() {
                    if !self.types.assignable(value_ty, param.ty) {
                        let span = self.spans.expr(value);
                        self.mismatch(span, param.ty, value_ty, &Purpose::Operand);
                    }
                }
            }
            return value_ty;
        }
        let outer = self.set_memo(Some(super::Memo::default()));
        self.dispatch(&call, receiver, ty);
        // The assigned value was checked as the setter's argument.
        let assigned = self
            .memo
            .get()
            .and_then(|memo| {
                memo.types
                    .get(&(std::ptr::from_ref(value) as usize))
                    .copied()
            })
            .unwrap_or(Ty::ERROR);
        self.restore_memo(outer);
        assigned
    }

    /// Reports a call through a receiver that the method's visibility
    /// forbids, as the runtime does: a private method is called only
    /// without a receiver, and a protected one only from its own class's
    /// methods, instance methods on an instance and class methods on the
    /// class. `instance` is whether the method is an instance method.
    pub(super) fn visibility(
        &mut self,
        name: &str,
        span: Span,
        id: FnId,
        ns: NsId,
        instance: bool,
    ) {
        let visibility = self.program.fns[id].visibility;
        let allowed = match visibility {
            Visibility::Public => true,
            Visibility::Private => false,
            Visibility::Protected => {
                self.frame.owner == Some(ns) && self.frame.instance == instance
            }
        };
        if allowed {
            return;
        }
        let namespace = &self.program.namespaces[ns as usize];
        let class = namespace.name.clone();
        let (word, rule) = match visibility {
            Visibility::Private => (
                "private",
                text!(
                    self,
                    "only `{class}`'s own methods can call it, without a receiver"
                ),
            ),
            _ if instance => (
                "protected",
                text!(
                    self,
                    "only `{class}`'s instance methods can call it, on an instance of `{class}`"
                ),
            ),
            _ if namespace.is_class => (
                "protected",
                text!(self, "only `{class}`'s class methods can call it"),
            ),
            _ => (
                "protected",
                text!(self, "only `{class}`'s own methods can call it"),
            ),
        };
        let mut diagnostic = Diagnostic::error(
            Code::VISIBILITY,
            span,
            text!(self, "`{name}` is {word} in `{class}`: {rule}"),
        );
        if let Some(def) = self.program.fns[id].def {
            let declared = self.spans.token(def.offset as usize);
            diagnostic = diagnostic.with_label(declared, text!(self, "declared {word} here"));
        }
        self.report(diagnostic);
    }

    /// A method of a script class called with index syntax, `[]` or `[]=`.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn method_on(
        &mut self,
        expr: &'a Expr,
        ty: Ty,
        name: &'static str,
        block: Option<&'a Block>,
        selectors: &'a [Expr],
        extra: Option<&'a Expr>,
    ) -> Ty {
        let call = Call {
            name,
            name_span: self.spans.expr(expr),
            args: &[],
            block,
            extra,
            selectors,
        };
        self.member(&call, ty)
    }

    // Signatures -------------------------------------------------------

    /// Checks a call against its candidate signatures, selecting one by
    /// the call's shape, and returns the call's type: the signature's
    /// result or a value a `break` out of its block gives.
    pub(super) fn call_sigs(&mut self, call: &Call<'a, '_>, candidates: &[Candidate]) -> Ty {
        let (result, breaks) = self.call_sigs_parts(call, candidates);
        self.with_breaks(result, &breaks)
    }

    /// A call's type, `result`, widened by the values `break` gives.
    fn with_breaks(&mut self, result: Ty, breaks: &[Ty]) -> Ty {
        if breaks.is_empty() {
            return result;
        }
        // The values join first, rather than a copy of them with the result.
        let breaks = self.types.union(breaks);
        self.types.union(&[breaks, result])
    }

    /// [`Self::call_sigs`], with the signature's result and the types of
    /// the values a `break` out of the call's block gives kept apart.
    fn call_sigs_parts(&mut self, call: &Call<'a, '_>, candidates: &[Candidate]) -> (Ty, Vec<Ty>) {
        let chosen = if candidates.len() == 1 {
            Some(0)
        } else {
            self.select(call, candidates)
        };
        let Some(chosen) = chosen else {
            // Each written through the meter, which the parameters' names
            // and types can make long.
            let mut list = super::counted::Text::new(&self.meter);
            for (index, (sig, _)) in candidates.iter().enumerate() {
                if index > 0 {
                    list.push_str(", ");
                }
                list.push('`');
                sig.describe(&self.types, &mut list);
                list.push('`');
            }
            let list = list.finish();
            let block = if call.block.is_some() {
                " and a block"
            } else {
                ""
            };
            self.report(Diagnostic::error(
                Code::NO_OVERLOAD,
                call.name_span,
                text!(
                    self,
                    "no signature of `{}` takes {} positional argument(s){block}; it has {list}",
                    call.name,
                    call.positional()
                ),
            ));
            self.loose_args(call);
            return (Ty::ERROR, Vec::new());
        };
        let (sig, bindings) = &candidates[chosen];
        self.check_call(call, sig, bindings.clone())
    }

    /// The candidate whose shape accepts the call: its positional count,
    /// keyword names, block, and the parameters the block declares.
    fn select(&mut self, call: &Call<'a, '_>, candidates: &[Candidate]) -> Option<usize> {
        let mut positional = call.positional();
        let mut splat = false;
        for arg in call.args.iter().filter(|arg| {
            matches!(arg.kind, ArgumentKind::Splat) && !matches!(arg.value.node, Node::Array(_))
        }) {
            let ty = match &arg.value.node {
                Node::Var(name) => self.local(name).map(|id| self.frame.flow.get(id).ty),
                _ => None,
            };
            if let Some(Kind::Tuple(items)) = ty.map(|ty| self.types.kind(ty)) {
                positional += items.len();
            } else {
                splat = true;
            }
        }
        // The names borrow the syntax's, but for a splatted hash's keys that
        // are not UTF-8, which are copied, at most three bytes for each; the
        // list is counted before it is made.
        let splatted = || {
            call.args
                .iter()
                .filter(|arg| matches!(arg.kind, ArgumentKind::KeywordSplat))
                .filter_map(|arg| match &arg.value.node {
                    Node::Hash(entries) => Some(entries.iter().map(|(name, _)| name)),
                    _ => None,
                })
                .flatten()
        };
        let count = call.keywords().count() + splatted().count();
        let copied: usize = splatted()
            .filter(|name| std::str::from_utf8(name).is_err())
            .map(|name| 3 * name.len())
            .sum();
        // A check past its budget chooses the first candidate, which it
        // checks no further.
        if self.transient(count * std::mem::size_of::<std::borrow::Cow<'_, str>>() + copied) {
            return Some(0);
        }
        let mut keywords: Vec<std::borrow::Cow<'_, str>> = Vec::with_capacity(count);
        keywords.extend(call.keywords().map(std::borrow::Cow::Borrowed));
        keywords.extend(splatted().map(|name| String::from_utf8_lossy(name)));
        let declared = match call.block {
            Some(block) => {
                let (arity, scratch) = block_arity(&self.meter, block);
                if self.transient(scratch) {
                    return Some(0);
                }
                Some(arity)
            }
            None => None,
        };
        let fits = |sig: &Sig, relaxed: bool| {
            let (min, max) = sig.positional();
            let count = positional >= min
                && if splat {
                    max.is_none()
                } else {
                    max.is_none_or(|max| positional <= max)
                };
            let keyword_ok = keywords
                .iter()
                .all(|name| sig.keyword(name).is_some() || sig.keyword_rest().is_some())
                && sig
                    .params
                    .iter()
                    .filter(|p| p.kind == ParamKind::Keyword && !p.optional)
                    .all(|p| keywords.iter().any(|name| *name == p.name));
            let block_ok = match (&sig.block, declared) {
                (None, None) => true,
                (None, Some(_)) => false,
                (Some(block), None) => block.optional,
                (Some(block), Some(declared)) => {
                    let params = block.params.len();
                    let exact = declared == params || (block.rest.is_some() && declared >= params);
                    exact || (relaxed && (declared <= params || params == 1))
                }
            };
            count && keyword_ok && block_ok
        };
        for relaxed in [false, true] {
            let fitting: Vec<usize> = candidates
                .iter()
                .enumerate()
                .filter(|(_, (sig, _))| fits(sig, relaxed))
                .map(|(index, _)| index)
                .collect();
            if let Some(&first) = fitting.first() {
                if relaxed && declared.is_some() {
                    // Prefer the fewest block parameters that cover the block's.
                    let best = fitting.iter().copied().min_by_key(|&index| {
                        let params = candidates[index]
                            .0
                            .block
                            .as_ref()
                            .map_or(0, |b| b.params.len());
                        (params < declared.unwrap_or(0), params)
                    });
                    return best;
                }
                return Some(first);
            }
        }
        None
    }

    /// Checks one call against one signature, returning its result and the
    /// types of the values a `break` out of its block gives.
    fn check_call(
        &mut self,
        call: &Call<'a, '_>,
        sig: &Sig,
        mut bindings: Vec<Option<Ty>>,
    ) -> (Ty, Vec<Ty>) {
        bindings.resize(sig.vars.len(), None);
        let function = sig.name.as_str();
        let stay = (!sig.converts).then_some(super::check::BUILTIN_SYMBOL);
        self.symbols(stay, |this| {
            this.check_positional(call, sig, &mut bindings);
            if !this.halted() {
                this.check_keywords(call, sig, &mut bindings);
            }
        });
        // A check past its budget checks nothing more of the call.
        if self.halted() {
            return (Ty::ERROR, Vec::new());
        }
        let mut breaks = Vec::new();
        match (&sig.block, call.block) {
            (Some(block_sig), Some(block)) => {
                let block_sig = block_sig.clone();
                // A script function returns a break value through its
                // declared result, or, yielding inside a loop or a block,
                // sees it there as a value of its result type.
                let break_to = match (sig.breaks, sig.result) {
                    (sigs::Breaks::Result | sigs::Breaks::Inside, Some(result)) => Some(BreakTo {
                        ty: self.types.close(result, &bindings),
                        function: self.copy(function),
                        inside: sig.breaks == sigs::Breaks::Inside,
                    }),
                    _ => None,
                };
                let call_value = match sig.breaks {
                    sigs::Breaks::Call => true,
                    sigs::Breaks::Result => break_to.is_none(),
                    sigs::Breaks::Inside | sigs::Breaks::Never => false,
                };
                breaks = self.symbols(stay, |this| {
                    this.call_block(block, &block_sig, &mut bindings, break_to)
                });
                if !call_value {
                    breaks.clear();
                }
            }
            (Some(block_sig), None) => {
                if !block_sig.optional {
                    self.report(Diagnostic::error(
                        Code::MISSING_BLOCK,
                        call.name_span,
                        text!(self, "`{function}` needs a block"),
                    ));
                }
            }
            (None, Some(block)) => {
                let span = self.spans.token(block.offset as usize);
                self.report(Diagnostic::error(
                    Code::UNEXPECTED_BLOCK,
                    span,
                    text!(self, "`{function}` takes no block"),
                ));
                self.block(block, &[], Want::Discard);
            }
            (None, None) => (),
        }
        let held = self.bounds(call, sig, &bindings);
        if sig.converts {
            self.script_called(sig.id, call.name_span);
        }
        let result = match sig.result {
            // A call reported for its bounds gives no second error.
            Some(_) if !held => Ty::ERROR,
            Some(result) => self.types.close_result(result, &bindings),
            None => Ty::NIL,
        };
        (result, breaks)
    }

    fn check_positional(&mut self, call: &Call<'a, '_>, sig: &Sig, bindings: &mut [Option<Ty>]) {
        let function = sig.name.as_str();
        let positional_params: Vec<&sigs::Param> = sig
            .params
            .iter()
            .filter(|p| p.kind == ParamKind::Positional)
            .collect();
        let rest = sig.rest().map(|p| p.ty);
        let rest_element = rest.map(|ty| self.types.element(ty).unwrap_or(Ty::ANY));
        let mut index = 0;
        let mut splatted = false;
        // The arguments, a literal splat's elements each, are counted and
        // held before they are listed, at the length they take.
        let count = call
            .args
            .iter()
            .map(|arg| match (&arg.kind, &arg.value.node) {
                (ArgumentKind::Positional, _) => 1,
                (ArgumentKind::Splat, Node::Array(items)) => items.len(),
                (ArgumentKind::Splat, _) => 1,
                _ => 0,
            })
            .sum::<usize>()
            + call.selectors.len()
            + usize::from(call.extra.is_some());
        let Some(held) = self.hold(
            positional_params.capacity() * std::mem::size_of::<&sigs::Param>()
                + count * std::mem::size_of::<(&Expr, bool)>(),
        ) else {
            return;
        };
        let mut arguments: Vec<(&'a Expr, bool)> = Vec::with_capacity(count);
        for arg in call.args {
            match &arg.kind {
                ArgumentKind::Positional => arguments.push((&arg.value, false)),
                ArgumentKind::Splat => match &arg.value.node {
                    Node::Array(items) => arguments.extend(items.iter().map(|item| (item, false))),
                    _ => arguments.push((&arg.value, true)),
                },
                _ => (),
            }
        }
        for selector in call.selectors {
            arguments.push((selector, false));
        }
        if let Some(extra) = call.extra {
            arguments.push((extra, false));
        }
        for (value, splat) in arguments {
            if splat {
                splatted = true;
                let ty = self.expr(value, None);
                if self.operand(value, ty) && self.types.element(ty).is_none() {
                    let span = self.spans.expr(value);
                    let found = self.types.display(ty);
                    self.report(
                        Diagnostic::error(
                            Code::TYPE_MISMATCH,
                            span,
                            text!(self, "a splat spreads an array, found {found}"),
                        )
                        .with_types("array<any>", found),
                    );
                }
                if let Kind::Tuple(items) = &*self.types.shared(ty) {
                    splatted = false;
                    for &element in items.iter() {
                        let param = positional_params.get(index).map(|p| p.ty).or(rest_element);
                        if let Some(param) = param {
                            self.spread_argument(value, element, param, bindings, function);
                        }
                        index += 1;
                    }
                } else if let Some(element) = self.types.element(ty) {
                    for param in positional_params.iter().skip(index) {
                        self.spread_argument(value, element, param.ty, bindings, function);
                    }
                    if let Some(rest) = rest_element {
                        self.spread_argument(value, element, rest, bindings, function);
                    }
                    let (min, max) = sig.positional();
                    if index < min || max.is_some() {
                        self.report(Diagnostic::error(
                            Code::NO_OVERLOAD,
                            call.name_span,
                            text!(self, "the length of this splat is unknown; `{function}` must accept every possible argument count"),
                        ));
                    }
                }
                continue;
            }
            let (param_ty, name) = match positional_params.get(index) {
                Some(param) => (param.ty, param.name.clone()),
                None => match (rest_element, sig.rest()) {
                    (Some(element), Some(param)) => (element, param.name.clone()),
                    _ => {
                        self.expr(value, None);
                        index += 1;
                        continue;
                    }
                },
            };
            let purpose = Purpose::Argument {
                index,
                name,
                function: self.copy(function),
            };
            // The names the purpose copies are held while the argument is
            // checked.
            let Some(argument_held) = self.hold(super::meter::Heap::heap(&purpose)) else {
                self.release(held);
                return;
            };
            let actual = self.argument(value, param_ty, bindings, &purpose);
            self.release(argument_held);
            if splatted {
                for param in positional_params.iter().skip(index + 1) {
                    self.spread_argument(value, actual, param.ty, bindings, function);
                }
                if let Some(rest) = rest_element {
                    self.spread_argument(value, actual, rest, bindings, function);
                }
            }
            index += 1;
        }
        if !splatted {
            let (min, max) = sig.positional();
            if index < min || max.is_some_and(|max| index > max) {
                let expected = match max {
                    Some(max) if max == min => text!(self, "{min}"),
                    Some(max) => text!(self, "{min} to {max}"),
                    None => text!(self, "at least {min}"),
                };
                self.report(Diagnostic::error(
                    Code::NO_OVERLOAD,
                    call.name_span,
                    text!(
                        self,
                        "`{function}` takes {expected} positional argument(s), got {index}"
                    ),
                ));
            }
        }
        self.release(held);
    }

    fn check_keywords(&mut self, call: &Call<'a, '_>, sig: &Sig, bindings: &mut [Option<Ty>]) {
        let function = sig.name.as_str();
        // The call reads the signature's parameters, a step for each 64 of
        // them. Of a signature of many, the keyword parameters are put in
        // name order, in a list counted while it lives, and each keyword
        // given is found by search rather than by a pass over them.
        if self.types.work(sig.params.len()) {
            return;
        }
        let keywords = call.args.iter().any(|arg| {
            matches!(
                arg.kind,
                ArgumentKind::Keyword(_) | ArgumentKind::KeywordSplat
            )
        });
        let indexed = keywords && sig.params.len() > INDEXED_PARAMS;
        let mut by_name = ScratchVec::new(&self.meter);
        if indexed {
            if by_name.reserve(sig.params.len()).is_err() {
                return;
            }
            for (at, param) in sig.params.iter().enumerate() {
                if param.kind == ParamKind::Keyword {
                    by_name.add((param.name.as_str(), at));
                }
            }
            if super::counted::sort_unstable_by(&self.meter, &mut by_name, Ord::cmp).is_err() {
                return;
            }
        }
        // The first parameter of the name, as a pass over them finds it.
        let keyword = |name: &str| {
            if indexed {
                let at = by_name.partition_point(|&(other, _)| other < name);
                by_name
                    .get(at)
                    .filter(|&&(other, _)| other == name)
                    .map(|&(_, at)| sig.params[at].ty)
            } else {
                sig.keyword(name).map(|param| param.ty)
            }
        };
        let rest = sig.keyword_rest().map(|param| param.ty);
        // The keywords given, kept while their values are checked.
        let mut given = Vec::new();
        let mut held = 0;
        for arg in call.args {
            match &arg.kind {
                ArgumentKind::Keyword(name) => {
                    let Some(bytes) = self.hold(std::mem::size_of::<String>() + name.len()) else {
                        self.release(held);
                        return;
                    };
                    held += bytes;
                    given.push(self.copy(name));
                    let param = keyword(name)
                        .or_else(|| rest.map(|ty| self.types.hash_value(ty).unwrap_or(Ty::ANY)));
                    match param {
                        Some(param_ty) => {
                            let purpose = Purpose::Keyword {
                                name: self.copy(name),
                                function: self.copy(function),
                            };
                            let Some(purpose_held) = self.hold(super::meter::Heap::heap(&purpose))
                            else {
                                self.release(held);
                                return;
                            };
                            self.argument(&arg.value, param_ty, bindings, &purpose);
                            self.release(purpose_held);
                        }
                        None => {
                            self.expr(&arg.value, None);
                            let span = self.spans.token(arg.value.offset as usize);
                            let span = self.keyword_span(&arg.value).unwrap_or(span);
                            self.report(Diagnostic::error(
                                Code::UNKNOWN_KEYWORD,
                                span,
                                text!(self, "`{function}` has no keyword `{name}:`"),
                            ));
                        }
                    }
                }
                ArgumentKind::KeywordSplat => {
                    let ty = self.expr(&arg.value, None);
                    if let Kind::Shape(fields, _) = &*self.types.shared(ty) {
                        let Some(bytes) = self.hold(fields.len() * std::mem::size_of::<String>())
                        else {
                            self.release(held);
                            return;
                        };
                        held += bytes;
                        for field in fields.iter() {
                            if !field.optional {
                                let Some(bytes) = self.hold(field.name.len()) else {
                                    self.release(held);
                                    return;
                                };
                                held += bytes;
                                given.push(self.copy(&field.name));
                            }
                            let expected = keyword(&field.name).or_else(|| {
                                rest.map(|ty| self.types.hash_value(ty).unwrap_or(Ty::ANY))
                            });
                            if let Some(expected) = expected {
                                self.spread_argument(
                                    &arg.value, field.ty, expected, bindings, function,
                                );
                            } else {
                                self.report(Diagnostic::error(
                                    Code::UNKNOWN_KEYWORD,
                                    self.spans.expr(&arg.value),
                                    text!(self, "`{function}` has no keyword `{}:`", field.name),
                                ));
                            }
                        }
                    } else if let Some(element) = self.types.hash_value(ty) {
                        if ty != Ty::EMPTY_HASH {
                            if let Some(rest) = sig.keyword_rest() {
                                let expected = self.types.hash_value(rest.ty).unwrap_or(Ty::ANY);
                                self.spread_argument(
                                    &arg.value, element, expected, bindings, function,
                                );
                                for param in
                                    sig.params.iter().filter(|p| p.kind == ParamKind::Keyword)
                                {
                                    self.spread_argument(
                                        &arg.value, element, param.ty, bindings, function,
                                    );
                                }
                            } else {
                                self.report(Diagnostic::error(Code::UNKNOWN_KEYWORD, self.spans.expr(&arg.value), text!(self, "a dictionary splat has unknown keys; `{function}` needs a keyword rest parameter")));
                            }
                        }
                    }
                    if self.operand(&arg.value, ty) && self.types.hash_value(ty).is_none() {
                        let span = self.spans.expr(&arg.value);
                        let found = self.types.display(ty);
                        self.report(
                            Diagnostic::error(
                                Code::TYPE_MISMATCH,
                                span,
                                text!(self, "a keyword splat spreads a hash, found {found}"),
                            )
                            .with_types("hash<string, any>", found),
                        );
                    }
                }
                _ => (),
            }
        }
        // Sorted, so each required keyword is found by search; a check the
        // sort stops looks for none.
        if super::counted::sort_unstable_by(&self.meter, &mut given, Ord::cmp).is_err() {
            return;
        }
        for param in &sig.params {
            if param.kind == ParamKind::Keyword
                && !param.optional
                && given.binary_search(&param.name).is_err()
            {
                self.report(Diagnostic::error(
                    Code::MISSING_KEYWORD,
                    call.name_span,
                    text!(self, "`{function}` needs the keyword `{}:`", param.name),
                ));
            }
        }
        self.release(held);
    }

    /// The type of a member call's result. Iterating members return their
    /// receiver unchanged, so a shape or tuple keeps its exact type.
    fn member_result(&mut self, call: &Call<'a, '_>, receiver: Ty, result: Ty) -> Ty {
        // `fetch` of a field a shape declares gives that field's type.
        if let (Kind::Shape(fields, _), "fetch", Some(first)) =
            (&*self.types.shared(receiver), call.name, call.args.first())
        {
            if let Some(key) = super::expr::string_literal(&first.value) {
                if let Some(field) = Types::field(fields, key.as_bytes()) {
                    return field.ty;
                }
            }
        }
        // Renaming nested keys leaves no shape inside with its fields.
        if call.name == "deep_transform_keys" && result != Ty::ERROR {
            return self.types.rekeyed(result);
        }
        let exact = matches!(self.types.kind(receiver), Kind::Shape(..) | Kind::Tuple(_));
        let iterating = matches!(
            call.name,
            "each" | "each_with_index" | "each_key" | "each_value" | "reverse_each"
        );
        if exact && iterating && result != Ty::ERROR {
            receiver
        } else {
            result
        }
    }

    /// The span of `name:` before a keyword argument's value.
    fn keyword_span(&self, value: &Expr) -> Option<Span> {
        let start = value.offset as usize;
        let before = self.source.get(..start)?;
        let colon = before.trim_end().strip_suffix(':')?;
        let name_start = colon
            .char_indices()
            .rev()
            .take_while(|(_, c)| c.is_alphanumeric() || *c == '_' || *c == '?' || *c == '!')
            .last()
            .map(|(i, _)| i)?;
        Some(Span::new(name_start, colon.len() + 1))
    }

    /// Checks one argument against a parameter type that may mention the
    /// signature's type variables, binding them.
    fn argument(
        &mut self,
        value: &'a Expr,
        param: Ty,
        bindings: &mut [Option<Ty>],
        purpose: &Purpose,
    ) -> Ty {
        let expected = self.types.subst(param, bindings);
        if !self.types.has_var(expected) {
            return self.expr_against(value, expected, purpose);
        }
        let mut ty = self.expr(value, None);
        let takes_type = matches!(self.types.kind(param), Kind::TypeLit(_));
        if takes_type {
            ty = self.nominal_type(ty);
        }
        self.unify(param, ty, bindings);
        let expected = self.types.close(param, bindings);
        if takes_type
            && !matches!(
                self.types.kind(ty),
                Kind::TypeLit(_) | Kind::Error | Kind::Never | Kind::Any
            )
        {
            self.not_a_type(value, ty, purpose);
        } else if !self.types.assignable(ty, expected) {
            let span = self.spans.expr(value);
            self.mismatch(span, expected, ty, purpose);
        }
        ty
    }

    fn spread_argument(
        &mut self,
        value: &Expr,
        actual: Ty,
        param: Ty,
        bindings: &mut [Option<Ty>],
        function: &str,
    ) {
        self.unify(param, actual, bindings);
        let expected = self.types.close(param, bindings);
        if !self.types.assignable(actual, expected) {
            self.mismatch(
                self.spans.expr(value),
                expected,
                actual,
                &Purpose::Argument {
                    index: 0,
                    name: "splat element".to_owned(),
                    function: self.copy(function),
                },
            );
        }
    }

    /// Reports a value passed where a type literal is expected.
    fn not_a_type(&mut self, value: &Expr, ty: Ty, purpose: &Purpose) {
        let span = self.spans.expr(value);
        let found = self.types.display(ty);
        let what = self.purpose_text(purpose, "a type");
        let mut message = text!(self, "{what}, found {found}");
        if matches!(&value.node, Node::Hash(entries) if !entries.is_empty()) {
            message.push_str(
                "; braces make a type only where every field names one, as in `{ status: Status }`",
            );
        }
        self.report(
            Diagnostic::error(Code::TYPE_MISMATCH, span, message).with_types("type<T>", found),
        );
    }

    /// Binds the type variables of `pattern` from a value of type `actual`.
    pub(super) fn unify(&mut self, pattern: Ty, actual: Ty, bindings: &mut [Option<Ty>]) {
        if !self.types.has_var(pattern) || actual == Ty::NEVER {
            return;
        }
        match (&*self.types.shared(pattern), &*self.types.shared(actual)) {
            (_, Kind::Union(arms)) => {
                for actual in arms.iter() {
                    self.unify(pattern, *actual, bindings);
                }
            }
            (Kind::Var(index), _) => {
                let Some(slot) = bindings.get_mut(*index as usize) else {
                    return;
                };
                *slot = Some(match *slot {
                    None => actual,
                    Some(bound) => {
                        if self.types.assignable(actual, bound) {
                            bound
                        } else if self.types.assignable(bound, actual) {
                            actual
                        } else {
                            self.types.union(&[bound, actual])
                        }
                    }
                });
            }
            (Kind::Array(p), Kind::Array(a)) => self.unify(*p, *a, bindings),
            (Kind::Array(p), Kind::Tuple(items)) => {
                let element = self.types.union(items);
                self.unify(*p, element, bindings);
            }
            (Kind::Hash(p), _) => {
                if let Some(value) = self.types.hash_value(actual) {
                    self.unify(*p, value, bindings);
                }
            }
            (Kind::Tuple(ps), Kind::Tuple(items)) if ps.len() == items.len() => {
                for (p, a) in ps.iter().zip(items.iter()) {
                    self.unify(*p, *a, bindings);
                }
            }
            (Kind::Tuple(ps), Kind::Array(a)) => {
                for p in ps.iter() {
                    self.unify(*p, *a, bindings);
                }
            }
            (Kind::TypeLit(p), Kind::TypeLit(a)) => self.unify(*p, *a, bindings),
            (Kind::Shape(pf, _), Kind::Shape(af, _)) => {
                for field in pf.iter() {
                    if let Some(found) = Types::field(af, field.name.as_bytes()) {
                        self.unify(field.ty, found.ty, bindings);
                    }
                }
            }
            (Kind::Union(arms), _) => {
                let (vars, concrete): (Vec<Ty>, Vec<Ty>) =
                    arms.iter().partition(|&&arm| self.types.has_var(arm));
                if vars.len() == 1 {
                    let covered = self.types.union(&concrete);
                    let rest = if concrete.is_empty() {
                        actual
                    } else {
                        let kept: Vec<Ty> = self
                            .types
                            .members(actual)
                            .into_iter()
                            .filter(|&m| !self.types.assignable(m, covered))
                            .collect();
                        self.types.union(&kept)
                    };
                    self.unify(vars[0], rest, bindings);
                } else if let Some(&arm) = vars
                    .iter()
                    .find(|&&arm| {
                        matches!(
                            (self.types.kind(arm), self.types.kind(actual)),
                            (Kind::Array(_), Kind::Array(_) | Kind::Tuple(_))
                                | (Kind::Hash(_), Kind::Hash(_) | Kind::Shape(..))
                        )
                    })
                    .or_else(|| {
                        vars.iter()
                            .find(|&&arm| matches!(self.types.kind(arm), Kind::Var(_)))
                    })
                {
                    self.unify(arm, actual, bindings);
                }
            }
            _ => (),
        }
    }

    /// Reports a type variable bound to a type outside its bound, such as
    /// `sort` on an array of a union.
    /// Reports each type variable of `sig` bound outside its bound, and
    /// returns whether they all held.
    fn bounds(&mut self, call: &Call<'a, '_>, sig: &Sig, bindings: &[Option<Ty>]) -> bool {
        let mut held = true;
        for (index, var) in sig.vars.iter().enumerate() {
            let (Some(bound), Some(Some(ty))) = (var.bound, bindings.get(index)) else {
                continue;
            };
            let ty = *ty;
            if ty == Ty::ERROR || ty == Ty::NEVER {
                continue;
            }
            let members = self.types.members(ty);
            let numeric = members.iter().all(|&m| m == Ty::INT || m == Ty::FLOAT);
            let single = members.len() == 1 || numeric;
            if single && self.types.assignable(ty, bound) {
                continue;
            }
            let found = self.types.display(ty);
            let bound_text = self.types.display(bound);
            let reason = if single {
                text!(self, "{found} is not {bound_text}")
            } else {
                text!(
                    self,
                    "{found} is a union, and `{}` needs one {bound_text} type",
                    call.name
                )
            };
            let mut diagnostic = Diagnostic::error(
                Code::BOUND,
                call.name_span,
                text!(
                    self,
                    "`{}` needs {} to be {bound_text}: {reason}",
                    call.name,
                    var.name
                ),
            )
            .with_types(bound_text, found);
            if call.name == "sum" && call.args.is_empty() && call.block.is_none() {
                diagnostic = self.sum_start(diagnostic, call, ty);
            }
            self.report(diagnostic);
            held = false;
        }
        held
    }

    /// Explains that `sum` without a starting value begins at the int 0,
    /// and passes the element type's zero where one literal writes it.
    fn sum_start(
        &mut self,
        mut diagnostic: Diagnostic,
        call: &Call<'a, '_>,
        element: Ty,
    ) -> Diagnostic {
        let zero = if element == Ty::FLOAT {
            Some("0.0")
        } else if element == Ty::DURATION {
            Some("0.seconds")
        } else if self.types.assignable(element, Ty::NUMBER) {
            Some("0")
        } else {
            None
        };
        let example = match (zero, element == Ty::MONEY) {
            (Some(zero), _) => text!(self, "`sum({zero})`"),
            (None, true) => "`sum(money_cents(0, \"USD\"))`".to_owned(),
            (None, false) => {
                return diagnostic;
            }
        };
        diagnostic.message.push_str(&text!(
            self,
            "; without a starting value `sum` begins at the int 0, so pass one, as in {example}"
        ));
        let end = call.name_span.end;
        let parenthesized = self.source[end..].trim_start().starts_with('(');
        if let (Some(zero), false) = (zero, parenthesized) {
            diagnostic = diagnostic.with_fix(Fix::insert(
                text!(self, "start the sum at `{zero}`"),
                end,
                text!(self, "({zero})"),
            ));
        }
        diagnostic
    }

    // Blocks -----------------------------------------------------------

    /// Checks a block passed to a function with block signature `block_sig`,
    /// binding type variables from the block's result, and returns the
    /// types of the values `break` gives. `break_to` is the declared result
    /// break values must fit, and the function that declares it.
    fn call_block(
        &mut self,
        block: &'a Block,
        block_sig: &BlockSig,
        bindings: &mut [Option<Ty>],
        break_to: Option<BreakTo>,
    ) -> Vec<Ty> {
        // Counted before they are listed.
        let Some(held) = self.hold(block_sig.params.len() * std::mem::size_of::<Ty>()) else {
            return Vec::new();
        };
        let params: Vec<Ty> = block_sig
            .params
            .iter()
            .map(|&p| self.types.close(p, bindings))
            .collect();
        let rest = block_sig.rest.map(|r| self.types.close(r, bindings));
        let mut widen = None;
        let (want, infer) = match block_sig.result {
            Some(result) => {
                let expected = self.types.subst(result, bindings);
                let empty = match self.types.kind(expected) {
                    Kind::Array(element) => *element == Ty::NEVER,
                    Kind::EmptyHash => true,
                    _ => false,
                };
                if self.types.has_var(expected) {
                    (Want::Infer(Some(expected)), Some(result))
                } else if let (Kind::Var(index), true) = (&*self.types.shared(result), empty) {
                    // An empty literal bound the variable, and the block's
                    // result may widen it: `reduce([]) { |all, x| all.push(x) }`.
                    widen = Some((*index as usize, expected));
                    (Want::Infer(Some(expected)), None)
                } else {
                    (Want::Check(expected), None)
                }
            }
            None => (Want::Discard, None),
        };
        let (result, breaks) = self.block_with_rest(block, &params, rest, want, break_to);
        self.release(held);
        if let Some(pattern) = infer {
            self.unify(pattern, result, bindings);
            let expected = self.types.close(pattern, bindings);
            if !self.types.assignable(result, expected) {
                let span = self.spans.token(block.offset as usize);
                self.mismatch(span, expected, result, &Purpose::BlockResult);
            }
        }
        if let Some((index, expected)) = widen {
            if result != Ty::ERROR && self.types.assignable(expected, result) {
                bindings[index] = Some(result);
            } else if !self.types.assignable(result, expected) {
                let span = self.spans.token(block.offset as usize);
                self.mismatch(span, expected, result, &Purpose::BlockResult);
            }
        }
        breaks
    }

    /// Checks a block whose parameters have the given types.
    pub(super) fn block(&mut self, block: &'a Block, params: &[Ty], want: Want) -> Ty {
        // Counted before they are copied.
        let count = if params.is_empty() {
            block.params.len()
        } else {
            params.len()
        };
        let Some(held) = self.hold(count * std::mem::size_of::<Ty>()) else {
            return Ty::ERROR;
        };
        let params: Vec<Ty> = if params.is_empty() && !block.params.is_empty() {
            vec![Ty::ERROR; block.params.len()]
        } else {
            params.to_vec()
        };
        let ty = self.block_with_rest(block, &params, None, want, None).0;
        self.release(held);
        ty
    }

    /// Checks a block and returns the type of its value and the types of
    /// the values `break` gives.
    fn block_with_rest(
        &mut self,
        block: &'a Block,
        params: &[Ty],
        rest: Option<Ty>,
        want: Want,
        break_to: Option<BreakTo>,
    ) -> (Ty, Vec<Ty>) {
        let plain = params
            .iter()
            .chain(rest.as_ref())
            .all(|&ty| self.types.plain(ty));
        self.facts.record_block(self.meter.tables(), block, plain);
        // Union receivers supply different block parameter types on each pass.
        let outer_memo = self.set_memo(None);
        if !self.open_scope() {
            self.put_back_memo(outer_memo);
            return (Ty::ERROR, Vec::new());
        }
        // The widening below charges for listing these names.
        let span = self.assigns.body(&self.meter, &block.body);
        let names = self.assigns.distinct(span, |count, _| {
            !self.transient(count * std::mem::size_of::<&str>())
        });
        for name in names {
            if let Some(id) = self.local(name) {
                if self.frame.ambient.contains(&id)
                    && !name.chars().next().is_some_and(char::is_uppercase)
                {
                    let ty = self.frame.locals[id as usize].declared;
                    if self
                        .declare(name, ty, block.offset as usize, false)
                        .is_none()
                    {
                        break;
                    }
                }
            }
        }
        let before = self.frame.flow.mark();
        let (result, used) = match want {
            Want::Check(expected) => (Some(expected), true),
            Want::Infer(_) => (None, true),
            Want::Discard => (None, false),
        };
        let context = Context::Block {
            mark: before,
            exits: super::check::Exits::default(),
            result,
            hint: want.hint(),
            break_to,
            used,
            results: CountedVec::new(),
        };
        // A block the budget refuses room for is not checked.
        if self
            .frame
            .contexts
            .push(self.meter.tables(), context)
            .is_err()
        {
            self.close_scope();
            self.put_back_memo(outer_memo);
            return (Ty::ERROR, Vec::new());
        }
        let targets = &block.params;
        if block.implicit {
            let first = params.first().copied().unwrap_or(Ty::NIL);
            if block.infer_it {
                if let Some(id) = self.declare("it", first, block.offset as usize, false) {
                    self.assign_local(id, first);
                }
            }
            for index in 0..9 {
                let name = text!(self, "_{}", index + 1);
                let ty = params.get(index).copied().or(rest).unwrap_or(Ty::NIL);
                let Some(id) = self.declare(&name, ty, block.offset as usize, false) else {
                    break;
                };
                self.assign_local(id, ty);
            }
        } else if targets.len() > 1
            && params.len() == 1
            && rest.is_none()
            && self.types.element(params[0]).is_some()
        {
            // Several parameters destructure a single argument.
            let whole = params[0];
            for (index, target) in targets.iter().enumerate() {
                let element = self.element_of(whole, index);
                self.bind_target(target, element, false);
            }
        } else {
            for (index, target) in targets.iter().enumerate() {
                let ty = match params.get(index).copied().or(rest) {
                    Some(ty) => ty,
                    None => {
                        let span = target
                            .offset()
                            .map_or(Span::at(block.offset as usize), |o| {
                                self.spans.token(o as usize)
                            });
                        self.report(Diagnostic::error(
                            Code::BLOCK_PARAMETERS,
                            span,
                            text!(
                                self,
                                "this block declares {} parameter(s), but it is given {}",
                                targets.len(),
                                params.len()
                            ),
                        ));
                        Ty::ERROR
                    }
                };
                self.bind_target_param(target, ty);
            }
        }
        self.widen_for_loop(&block.body);
        // Room for the purpose is counted before it is kept; a block the
        // budget refuses it is not checked.
        let tail = if self
            .purposes
            .push(self.meter.tables(), Purpose::BlockResult)
            .is_ok()
        {
            let tail = self.stmts(&block.body, want);
            self.purposes.pop();
            tail
        } else {
            Ty::ERROR
        };
        if let (Want::Check(expected), true) = (want, block.body.is_empty()) {
            if !self.types.assignable(Ty::NIL, expected) {
                let span = self.spans.token(block.offset as usize);
                self.mismatch(span, expected, Ty::NIL, &Purpose::BlockResult);
            }
        }
        let live = self.frame.flow.live;
        let mut context = self.frame.contexts.pop().unwrap();
        let mut results = match &mut context {
            Context::Block { results, .. } => std::mem::take(results),
            Context::Loop { .. } => CountedVec::new(),
        };
        // A check the budget stops keeps no more of them.
        if live && results.push(self.meter.tables(), tail).is_err() {
            results.clear();
        }
        let breaks = self.finish_loop(before, context, true);
        self.close_scope();
        self.put_back_memo(outer_memo);
        (self.types.union(&results), breaks)
    }

    /// Binds a block parameter, which is always a new local of the block.
    fn bind_target_param(&mut self, target: &'a Target, ty: Ty) {
        match target {
            Target::Value(Expr {
                node: Node::Var(name),
                offset,
                ..
            }) if !name.starts_with('@') => {
                if let Some(id) = self.declare(name, ty, *offset as usize, false) {
                    self.assign_local(id, ty);
                }
            }
            _ => self.bind_target(target, ty, false),
        }
    }

    /// `yield args`, checked against the function's `&block` declaration.
    pub(super) fn yield_expr(&mut self, expr: &'a Expr, args: &'a [Expr], want: Want) -> Ty {
        let span = self.spans.token(expr.offset as usize);
        let Some(block) = self.frame.block.clone() else {
            for arg in args {
                self.expr(arg, None);
            }
            self.report(Diagnostic::error(
                Code::UNDECLARED_BLOCK,
                span,
                text!(self,
                    "`{}` yields but declares no block; add a typed block parameter, as in `&block: (T) -> R`",
                    self.frame.name
                ),
            ));
            return Ty::ERROR;
        };
        if let Some(given) = self.frame.block_given {
            if !self.frame.flow.get(given).assigned {
                self.report(Diagnostic::error(
                    Code::UNGUARDED_YIELD,
                    span,
                    "the block is optional, so `yield` must be guarded by `block_given?`",
                ));
            }
        }
        for (index, arg) in args.iter().enumerate() {
            match block.params.get(index) {
                Some(&param) => {
                    self.symbols(None, |this| {
                        this.expr_against(arg, param, &Purpose::Yield(index))
                    });
                }
                None => {
                    self.expr(arg, None);
                }
            }
        }
        if args.len() != block.params.len() && !(args.len() == 1 && block.params.is_empty()) {
            self.report(Diagnostic::error(
                Code::NO_OVERLOAD,
                span,
                text!(
                    self,
                    "the block takes {} argument(s), but `yield` passes {}",
                    block.params.len(),
                    args.len()
                ),
            ));
        }
        self.yield_breaks(span);
        // The block may be the file's own, and assign its locals.
        self.script_called(None, span);
        match block.result {
            Some(result) => result,
            None => {
                if !matches!(want, Want::Discard) {
                    self.report(Diagnostic::error(
                        Code::YIELD_VALUE,
                        span,
                        "the block declares no result type, so the value of `yield` cannot be used; declare `-> R`",
                    ));
                }
                Ty::NIL
            }
        }
    }
}

/// How many parameters a block declares: its explicit list, or the highest
/// numbered parameter or `it` it reads, charging the walk that finds them
/// to `meter`; with the bytes of the stack the walk kept.
fn block_arity(meter: &super::meter::Meter, block: &Block) -> (usize, usize) {
    use super::walk::{Item, Next, Walk};
    use crate::syntax::Statement;
    if !block.implicit {
        return (block.params.len(), 0);
    }
    let mut arity = 0;
    let mut walk = Walk::new(meter);
    // The block is a visit, empty or not.
    walk.visit(0);
    walk.stmts(&block.body, ());
    while let Some((item, ())) = walk.next(0) {
        match item {
            Item::Stmt(stmt) => match &stmt.node {
                Statement::Expr(e)
                | Statement::Assign(_, _, e)
                | Statement::Return(Some(e))
                | Statement::Next(Some(e))
                | Statement::Break(Some(e)) => walk.expr(e, ()),
                Statement::If(branches, alternate, _) => {
                    walk.push(Next::Clauses(branches.iter()), ());
                    walk.stmts(alternate, ());
                }
                _ => (),
            },
            Item::Expr(e) => match &e.node {
                Node::Var(name) if name.as_str() == "it" => arity = arity.max(1),
                Node::Var(name) => {
                    if let Some(n) = name.strip_prefix('_').and_then(|n| n.parse::<usize>().ok()) {
                        arity = arity.max(n);
                    }
                }
                Node::Binary(_, l, r) => {
                    walk.expr(l, ());
                    walk.expr(r, ());
                }
                Node::Unary(_, v) => walk.expr(v, ()),
                Node::Method(r, _, args, _) | Node::SafeMethod(r, _, args, _) => {
                    walk.expr(r, ());
                    walk.push(Next::Arguments(args.iter()), ());
                }
                Node::Member(r, _) | Node::SafeMember(r, _) => walk.expr(r, ()),
                Node::Call(name, args, _) => {
                    if name.as_str() == "it" {
                        arity = arity.max(1);
                    }
                    walk.push(Next::Arguments(args.iter()), ());
                }
                Node::Index(r, s) => {
                    walk.expr(r, ());
                    walk.push(Next::Exprs(s.iter()), ());
                }
                Node::Array(items) | Node::Template(items, _) => {
                    walk.push(Next::Exprs(items.iter()), ());
                }
                Node::Hash(entries) => walk.push(Next::Pairs(entries.iter()), ()),
                Node::Conditional(branches, alternate) => {
                    walk.push(Next::Branches(branches.iter()), ());
                    walk.expr(alternate, ());
                }
                _ => (),
            },
            Item::Target(_) => (),
        }
    }
    (arity, walk.bytes())
}
