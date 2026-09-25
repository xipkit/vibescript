//! Checks calls: to script functions, methods and constructors, to builtins
//! through the signature table with overload selection, and to host
//! functions; blocks against their signatures, and `yield`.

use super::{
    Checker, ReceiverType,
    check::{Context, Purpose, Want},
    program::NsId,
    sigs::{self, BlockSig, ParamKind, Sig},
    ty::{Kind, Ty},
};
use crate::{
    diagnostic::{Code, Diagnostic, Span},
    syntax::{Argument, ArgumentKind, Block, Expr, Node, Target},
};
use std::rc::Rc;

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
    fn positional(&self) -> usize {
        self.args
            .iter()
            .filter(|arg| matches!(arg.kind, ArgumentKind::Positional))
            .count()
            + self.selectors.len()
            + usize::from(self.extra.is_some())
    }

    fn splat(&self) -> bool {
        self.args
            .iter()
            .any(|arg| matches!(arg.kind, ArgumentKind::Splat | ArgumentKind::KeywordSplat))
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
        if !bare && self.local(name).is_some() {
            let span = call.name_span;
            self.report(Diagnostic::error(
                Code::NOT_CALLABLE,
                span,
                format!("`{name}` is a local, not a function; a local cannot be called"),
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
            "block_given?" => return Ty::BOOL,
            _ => (),
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
        if bare {
            // A value the host supplies when each call starts.
            return Ty::ANY;
        }
        self.report(Diagnostic::error(
            Code::UNDEFINED_NAME,
            call.name_span,
            format!("`{name}` is not a function, method or builtin in scope"),
        ));
        self.loose_args(&call);
        Ty::ERROR
    }

    /// `require` with literal module names.
    fn require(&mut self, call: &Call<'a, '_>) {
        for arg in call.args {
            let literal = matches!(&arg.value.node, Node::Literal(value) if value.as_bytes().is_some() || super::symbol_text(value).is_some());
            self.expr(&arg.value, None);
            if !literal {
                let span = self.spans.expr(&arg.value);
                let what = match &arg.kind {
                    ArgumentKind::Keyword(name) if name.as_str() == "as" => "its alias",
                    _ => "the module name",
                };
                self.report(Diagnostic::error(
                    Code::DYNAMIC_REQUIRE,
                    span,
                    format!("`require` takes {what} as a string literal, so the compiler can resolve and check the module"),
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
            format!("a value of type {found} cannot be called"),
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
        let ty = self.expr(receiver, None);
        let name_span = self.spans.member(receiver, name);
        if let Some(span) = name_span {
            if ty != Ty::ERROR {
                // A safe call runs the member on the value without nil.
                let called = if safe { self.types.without_nil(ty) } else { ty };
                let receiver_type =
                    ReceiverType::new(self.types.display(called), self.types.bases(called));
                self.calls.push((span.start, receiver_type));
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
        if safe {
            if ty == Ty::ERROR {
                self.loose_args(&call);
                return Ty::ERROR;
            }
            let without = self.types.without_nil(ty);
            if without == Ty::NEVER {
                self.loose_args(&call);
                return Ty::NIL;
            }
            let result = self.dispatch(&call, receiver, without);
            return self.types.optional(result);
        }
        self.dispatch(&call, receiver, ty)
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
                    format!(
                        "`{}` is not defined for nil, and this value is {found}; test it with `!= nil` first, or call through `&.`",
                        call.name
                    ),
                );
                let without = self.types.without_nil(ty);
                if let Some(fix) = self.fetch_fix(receiver, ty, without) {
                    diagnostic = diagnostic.with_fix(fix);
                }
                self.report(diagnostic);
            }
        }
        let Some((&first, rest)) = others.split_first() else {
            self.loose_args(call);
            return Ty::ERROR;
        };
        let outer = self.memo.replace(super::Memo::default());
        results.push(self.member(call, first));
        // The other alternatives reuse the arguments' types, without
        // checking them or reporting their errors again.
        self.memo.as_mut().unwrap().replay = true;
        self.mute += 1;
        for &alternative in rest {
            let mark = self.frame.flow.mark();
            results.push(self.member(call, alternative));
            self.frame.flow.rollback(mark);
        }
        self.mute -= 1;
        self.restore_memo(outer);
        self.types.union(&results)
    }

    /// Whether a receiver type has a member named `name`.
    fn answers(&mut self, ty: Ty, name: &str) -> bool {
        sigs::declares(&self.types, ty, name) || matches!(name, "as" | "to_s")
    }

    /// Checks a member call on a receiver of a single type.
    fn member(&mut self, call: &Call<'a, '_>, ty: Ty) -> Ty {
        let kind = self.types.kind(ty).clone();
        match kind {
            Kind::Error | Kind::Never => {
                self.loose_args(call);
                ty
            }
            Kind::Nil if matches!(call.name, "==" | "!=") => Ty::BOOL,
            Kind::Any => {
                if matches!(call.name, "is_type?" | "as" | "==" | "!=") {
                    return self.table_member(call, ty);
                }
                self.report(Diagnostic::error(
                    Code::ANY_USE,
                    call.name_span,
                    format!(
                        "this value has type any; narrow it with `is_type?`, `.as(T)` or `JSON.parse_as` before calling `{}`",
                        call.name
                    ),
                ));
                self.loose_args(call);
                Ty::ERROR
            }
            Kind::Instance(ns) => {
                let method = self.program.namespaces[ns as usize]
                    .methods
                    .get(call.name)
                    .copied();
                match method {
                    Some(id) if call.name != "initialize" => {
                        let sig = self.program.fns[id].sig.clone();
                        self.call_sigs(call, &[(sig, Vec::new())])
                    }
                    _ if call.name == "to_s" && call.args.is_empty() => Ty::STRING,
                    _ => self.table_member(call, ty),
                }
            }
            Kind::Namespace(ns) => self.namespace_member(call, ns, ty),
            Kind::Exports(id) => match self.exported(id, call.name) {
                Some(sig) => self.call_sigs(call, &[(sig, Vec::new())]),
                None => {
                    self.unknown_export(id, call.name, call.name_span);
                    self.loose_args(call);
                    Ty::ERROR
                }
            },
            Kind::Builtin(index) => self.builtin_member(call, index, ty),
            _ => self.table_member(call, ty),
        }
    }

    /// `Class.new`, `Class.method` and `Module.function`.
    fn namespace_member(&mut self, call: &Call<'a, '_>, ns: NsId, ty: Ty) -> Ty {
        let namespace = &self.program.namespaces[ns as usize];
        if call.name == "new" && namespace.is_class {
            let initialize = namespace.methods.get("initialize").copied();
            let instance = self.types.intern(Kind::Instance(ns));
            match initialize {
                Some(id) => {
                    let sig = self.program.fns[id].sig.clone();
                    let mut sig = (*sig).clone();
                    sig.name = format!("{}.new", self.program.namespaces[ns as usize].name);
                    self.call_sigs(call, &[(Rc::new(sig), Vec::new())]);
                }
                None => {
                    let sig = Sig {
                        name: format!("{}.new", self.program.namespaces[ns as usize].name),
                        params: Vec::new(),
                        result: None,
                        block: None,
                        vars: Vec::new(),
                        class_vars: 0,
                    };
                    self.call_sigs(call, &[(Rc::new(sig), Vec::new())]);
                }
            }
            return instance;
        }
        if let Some(&id) = namespace.statics.get(call.name) {
            let sig = self.program.fns[id].sig.clone();
            return self.call_sigs(call, &[(sig, Vec::new())]);
        }
        if call.args.is_empty() && call.block.is_none() {
            if let Some(&ty) = self.constants.get(&(Some(ns), call.name.to_owned())) {
                return ty;
            }
            if let Some(&child) = namespace.children.get(call.name) {
                return self.types.intern(Kind::Namespace(child));
            }
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
                    return self.builtin_member(&renamed, index, ty);
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
            let result = self.call_sigs(call, &candidates);
            return self.member_result(call, ty, result);
        }
        if call.name == "as" {
            return self.cast(call, ty);
        }
        if call.name == "to_s" && call.args.is_empty() && call.block.is_none() {
            return Ty::STRING;
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
                        return self.table_member(&renamed, ty);
                    }
                }
                self.loose_args(call);
                return Ty::ERROR;
            }
        }
        let found = self.types.display(ty);
        self.report(Diagnostic::error(
            Code::UNKNOWN_MEMBER,
            call.name_span,
            format!("{found} has no member `{}`", call.name),
        ));
        self.loose_args(call);
        Ty::ERROR
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
        // A class or enum names its own type.
        let literal = match self.types.kind(literal).clone() {
            Kind::EnumType(id) => {
                let member = self.types.intern(Kind::EnumValue(id));
                self.types.type_lit(member)
            }
            Kind::Namespace(ns) if self.program.namespaces[ns as usize].is_class => {
                let instance = self.types.intern(Kind::Instance(ns));
                self.types.type_lit(instance)
            }
            _ => literal,
        };
        let Kind::TypeLit(target) = self.types.kind(literal).clone() else {
            if literal != Ty::ERROR {
                let span = self.spans.expr(&arg.value);
                let found = self.types.display(literal);
                self.report(
                    Diagnostic::error(
                        Code::TYPE_MISMATCH,
                        span,
                        format!("`as` takes a type, found {found}"),
                    )
                    .with_types("type<T>", found),
                );
            }
            return Ty::ERROR;
        };
        if ty != Ty::ANY && ty != Ty::ERROR {
            let possible = self.types.members(target).into_iter().any(|t| {
                self.types.members(ty).into_iter().any(|m| {
                    self.types.assignable(m, t)
                        || self.types.assignable(t, m)
                        || (m == Ty::SYMBOL && matches!(self.types.kind(t), Kind::EnumValue(_)))
                })
            });
            if !possible {
                let found = self.types.display(ty);
                let wanted = self.types.display(target);
                self.report(Diagnostic::error(
                    Code::CAST,
                    call.name_span,
                    format!("a value of type {found} can never be {wanted}"),
                ));
            }
        }
        if let Some(block) = call.block {
            self.block(block, &[], Want::Discard);
        }
        target
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
        match self.types.kind(ty).clone() {
            Kind::EnumType(id) if args.is_none() => {
                let decl = &self.program.enums[id as usize];
                if decl.members.iter().any(|member| member == name) {
                    return self.types.intern(Kind::EnumValue(id));
                }
                let enum_name = decl.name.clone();
                self.report(Diagnostic::error(
                    Code::UNKNOWN_ENUM_MEMBER,
                    call.name_span,
                    format!("`{enum_name}` has no member `{name}`"),
                ));
                Ty::ERROR
            }
            Kind::Namespace(ns) if args.is_none() && block.is_none() => {
                if let Some(&ty) = self.constants.get(&(Some(ns), name.to_owned())) {
                    return ty;
                }
                if let Some(&child) = self.program.namespaces[ns as usize].children.get(name) {
                    return self.types.intern(Kind::Namespace(child));
                }
                self.dispatch(&call, receiver, ty)
            }
            _ => self.dispatch(&call, receiver, ty),
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
        let setter = format!("{name}=");
        let setter = setter.as_str();
        let ty = self.expr(receiver, None);
        let name_span = self.spans.member(receiver, name);
        if let (Some(span), false) = (name_span, ty == Ty::ERROR) {
            let receiver_type = ReceiverType::new(self.types.display(ty), self.types.bases(ty));
            self.calls.push((span.start, receiver_type));
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
            // A compound assignment checks the computed value against the setter.
            if let Kind::Instance(ns) = self.types.kind(ty).clone() {
                if let Some(&id) = self.program.namespaces[ns as usize].methods.get(setter) {
                    let sig = self.program.fns[id].sig.clone();
                    if let Some(param) = sig.params.first() {
                        if !self.types.assignable(value_ty, param.ty) {
                            let span = self.spans.expr(value);
                            self.mismatch(span, param.ty, value_ty, &Purpose::Operand);
                        }
                    }
                }
            }
            return value_ty;
        }
        let outer = self.memo.replace(super::Memo::default());
        self.dispatch(&call, receiver, ty);
        // The assigned value was checked as the setter's argument.
        let assigned = self
            .memo
            .as_ref()
            .and_then(|memo| {
                memo.types
                    .get(&(std::ptr::from_ref(value) as usize))
                    .copied()
            })
            .unwrap_or(Ty::ERROR);
        self.restore_memo(outer);
        assigned
    }

    /// Restores an enclosing memo, keeping what the inner one recorded when
    /// the enclosing one records too.
    pub(super) fn restore_memo(&mut self, outer: Option<super::Memo>) {
        let inner = std::mem::replace(&mut self.memo, outer);
        if let (Some(inner), Some(outer)) = (inner, self.memo.as_mut()) {
            if !outer.replay {
                outer.types.extend(inner.types);
            }
        }
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
    /// the call's shape, and returns the call's type.
    pub(super) fn call_sigs(&mut self, call: &Call<'a, '_>, candidates: &[Candidate]) -> Ty {
        let chosen = if candidates.len() == 1 {
            Some(0)
        } else {
            self.select(call, candidates)
        };
        let Some(chosen) = chosen else {
            let list = candidates
                .iter()
                .map(|(sig, _)| format!("`{}`", sig.describe(&self.types)))
                .collect::<Vec<_>>()
                .join(", ");
            let block = if call.block.is_some() {
                " and a block"
            } else {
                ""
            };
            self.report(Diagnostic::error(
                Code::NO_OVERLOAD,
                call.name_span,
                format!(
                    "no signature of `{}` takes {} positional argument(s){block}; it has {list}",
                    call.name,
                    call.positional()
                ),
            ));
            self.loose_args(call);
            return Ty::ERROR;
        };
        let (sig, bindings) = &candidates[chosen];
        self.check_call(call, sig, bindings.clone())
    }

    /// The candidate whose shape accepts the call: its positional count,
    /// keyword names, block, and the parameters the block declares.
    fn select(&mut self, call: &Call<'a, '_>, candidates: &[Candidate]) -> Option<usize> {
        let positional = call.positional();
        let splat = call.splat();
        let keywords: Vec<&str> = call.keywords().collect();
        let declared = call.block.map(block_arity);
        let fits = |sig: &Sig, relaxed: bool| {
            let (min, max) = sig.positional();
            let count = splat || (positional >= min && max.is_none_or(|max| positional <= max));
            let keyword_ok = keywords
                .iter()
                .all(|name| sig.keyword(name).is_some() || sig.keyword_rest().is_some())
                && sig
                    .params
                    .iter()
                    .filter(|p| p.kind == ParamKind::Keyword && !p.optional)
                    .all(|p| keywords.contains(&p.name.as_str()));
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

    /// Checks one call against one signature.
    fn check_call(&mut self, call: &Call<'a, '_>, sig: &Sig, mut bindings: Vec<Option<Ty>>) -> Ty {
        bindings.resize(sig.vars.len(), None);
        let function = sig.name.clone();
        let positional_params: Vec<&sigs::Param> = sig
            .params
            .iter()
            .filter(|p| p.kind == ParamKind::Positional)
            .collect();
        let rest = sig.rest().map(|p| p.ty);
        let rest_element = rest.map(|ty| self.types.element(ty).unwrap_or(Ty::ANY));
        let mut index = 0;
        let mut splatted = false;
        let mut arguments: Vec<(&'a Expr, bool)> = Vec::new();
        for arg in call.args {
            match &arg.kind {
                ArgumentKind::Positional => arguments.push((&arg.value, false)),
                ArgumentKind::Splat => arguments.push((&arg.value, true)),
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
                            format!("a splat spreads an array, found {found}"),
                        )
                        .with_types("array<any>", found),
                    );
                }
                if let Some(element) = self.types.element(ty) {
                    for param in positional_params.iter().skip(index) {
                        self.unify(param.ty, element, &mut bindings);
                    }
                    if let Some(rest) = rest_element {
                        self.unify(rest, element, &mut bindings);
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
                function: function.clone(),
            };
            self.argument(value, param_ty, &mut bindings, &purpose);
            index += 1;
        }
        if !splatted {
            let (min, max) = sig.positional();
            if index < min || max.is_some_and(|max| index > max) {
                let expected = match max {
                    Some(max) if max == min => format!("{min}"),
                    Some(max) => format!("{min} to {max}"),
                    None => format!("at least {min}"),
                };
                self.report(Diagnostic::error(
                    Code::NO_OVERLOAD,
                    call.name_span,
                    format!("`{function}` takes {expected} positional argument(s), got {index}"),
                ));
            }
        }
        let mut given = Vec::new();
        for arg in call.args {
            match &arg.kind {
                ArgumentKind::Keyword(name) => {
                    given.push(name.as_str());
                    let param = sig.keyword(name).map(|p| p.ty).or_else(|| {
                        sig.keyword_rest()
                            .map(|p| self.types.hash_value(p.ty).unwrap_or(Ty::ANY))
                    });
                    match param {
                        Some(param_ty) => {
                            let purpose = Purpose::Keyword {
                                name: name.to_string(),
                                function: function.clone(),
                            };
                            self.argument(&arg.value, param_ty, &mut bindings, &purpose);
                        }
                        None => {
                            self.expr(&arg.value, None);
                            let span = self.spans.token(arg.value.offset as usize);
                            let span = self.keyword_span(&arg.value).unwrap_or(span);
                            self.report(Diagnostic::error(
                                Code::UNKNOWN_KEYWORD,
                                span,
                                format!("`{function}` has no keyword `{name}:`"),
                            ));
                        }
                    }
                }
                ArgumentKind::KeywordSplat => {
                    let ty = self.expr(&arg.value, None);
                    if self.operand(&arg.value, ty) && self.types.hash_value(ty).is_none() {
                        let span = self.spans.expr(&arg.value);
                        let found = self.types.display(ty);
                        self.report(
                            Diagnostic::error(
                                Code::TYPE_MISMATCH,
                                span,
                                format!("a keyword splat spreads a hash, found {found}"),
                            )
                            .with_types("hash<string, any>", found),
                        );
                    }
                }
                _ => (),
            }
        }
        if !call
            .args
            .iter()
            .any(|a| matches!(a.kind, ArgumentKind::KeywordSplat))
        {
            for param in &sig.params {
                if param.kind == ParamKind::Keyword
                    && !param.optional
                    && !given.contains(&param.name.as_str())
                {
                    self.report(Diagnostic::error(
                        Code::MISSING_KEYWORD,
                        call.name_span,
                        format!("`{function}` needs the keyword `{}:`", param.name),
                    ));
                }
            }
        }
        match (&sig.block, call.block) {
            (Some(block_sig), Some(block)) => {
                let block_sig = block_sig.clone();
                self.call_block(block, &block_sig, &mut bindings, sig);
            }
            (Some(block_sig), None) => {
                if !block_sig.optional {
                    self.report(Diagnostic::error(
                        Code::MISSING_BLOCK,
                        call.name_span,
                        format!("`{function}` needs a block"),
                    ));
                }
            }
            (None, Some(block)) => {
                let span = self.spans.token(block.offset as usize);
                self.report(Diagnostic::error(
                    Code::UNEXPECTED_BLOCK,
                    span,
                    format!("`{function}` takes no block"),
                ));
                self.block(block, &[], Want::Discard);
            }
            (None, None) => (),
        }
        self.bounds(call, sig, &bindings);
        match sig.result {
            Some(result) => self.types.close(result, &bindings),
            None => Ty::NIL,
        }
    }

    /// The type of a member call's result. Iterating members return their
    /// receiver unchanged, so a shape or tuple keeps its exact type.
    fn member_result(&mut self, call: &Call<'a, '_>, receiver: Ty, result: Ty) -> Ty {
        // `fetch` of a field a shape declares gives that field's type.
        if let (Kind::Shape(fields, _), "fetch", Some(first)) = (
            self.types.kind(receiver).clone(),
            call.name,
            call.args.first(),
        ) {
            if let Some(key) = super::expr::string_literal(&first.value) {
                if let Some(field) = fields.iter().find(|field| *field.name == *key) {
                    return field.ty;
                }
            }
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
        let ty = self.expr(value, None);
        self.unify(param, ty, bindings);
        let expected = self.types.subst(param, bindings);
        if !self.types.has_var(expected) && !self.types.assignable(ty, expected) {
            let span = self.spans.expr(value);
            self.mismatch(span, expected, ty, purpose);
        }
        ty
    }

    /// Binds the type variables of `pattern` from a value of type `actual`.
    pub(super) fn unify(&mut self, pattern: Ty, actual: Ty, bindings: &mut [Option<Ty>]) {
        if !self.types.has_var(pattern) || actual == Ty::NEVER {
            return;
        }
        match (
            self.types.kind(pattern).clone(),
            self.types.kind(actual).clone(),
        ) {
            (Kind::Var(index), _) => {
                let Some(slot) = bindings.get_mut(index as usize) else {
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
            (Kind::Array(p), Kind::Array(a)) => self.unify(p, a, bindings),
            (Kind::Array(p), Kind::Tuple(items)) => {
                let element = self.types.union(&items);
                self.unify(p, element, bindings);
            }
            (Kind::Hash(p), _) => {
                if let Some(value) = self.types.hash_value(actual) {
                    self.unify(p, value, bindings);
                }
            }
            (Kind::Tuple(ps), Kind::Tuple(items)) if ps.len() == items.len() => {
                for (p, a) in ps.iter().zip(items.iter()) {
                    self.unify(*p, *a, bindings);
                }
            }
            (Kind::Tuple(ps), Kind::Array(a)) => {
                for p in ps.iter() {
                    self.unify(*p, a, bindings);
                }
            }
            (Kind::TypeLit(p), Kind::TypeLit(a)) => self.unify(p, a, bindings),
            (Kind::Shape(pf, _), Kind::Shape(af, _)) => {
                for field in pf.iter() {
                    if let Some(found) = af.iter().find(|f| f.name == field.name) {
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
                }
            }
            _ => (),
        }
    }

    /// Reports a type variable bound to a type outside its bound, such as
    /// `sort` on an array of a union.
    fn bounds(&mut self, call: &Call<'a, '_>, sig: &Sig, bindings: &[Option<Ty>]) {
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
                format!("{found} is not {bound_text}")
            } else {
                format!(
                    "{found} is a union, and `{}` needs one {bound_text} type",
                    call.name
                )
            };
            self.report(
                Diagnostic::error(
                    Code::BOUND,
                    call.name_span,
                    format!(
                        "`{}` needs {} to be {bound_text}: {reason}",
                        call.name, var.name
                    ),
                )
                .with_types(bound_text, found),
            );
        }
    }

    // Blocks -----------------------------------------------------------

    /// Checks a block passed to a function with block signature `block_sig`,
    /// binding type variables from the block's result.
    fn call_block(
        &mut self,
        block: &'a Block,
        block_sig: &BlockSig,
        bindings: &mut [Option<Ty>],
        sig: &Sig,
    ) {
        // Without an initial value, `reduce` folds from the first element.
        for &param in &block_sig.params {
            if let Kind::Var(index) = *self.types.kind(param) {
                if bindings.get(index as usize).is_some_and(Option::is_none) && sig.class_vars > 0 {
                    let element = bindings[0];
                    bindings[index as usize] = element;
                }
            }
        }
        let params: Vec<Ty> = block_sig
            .params
            .iter()
            .map(|&p| self.types.close(p, bindings))
            .collect();
        let rest = block_sig.rest.map(|r| self.types.close(r, bindings));
        let (want, infer) = match block_sig.result {
            Some(result) => {
                let expected = self.types.subst(result, bindings);
                if self.types.has_var(expected) {
                    (Want::Infer(None), Some(result))
                } else {
                    (Want::Check(expected), None)
                }
            }
            None => (Want::Discard, None),
        };
        let mut all = params.clone();
        if let Some(rest) = rest {
            all.push(rest);
        }
        let result = self.block_with_rest(block, &params, rest, want);
        if let Some(pattern) = infer {
            self.unify(pattern, result, bindings);
        }
    }

    /// Checks a block whose parameters have the given types.
    pub(super) fn block(&mut self, block: &'a Block, params: &[Ty], want: Want) -> Ty {
        let params: Vec<Ty> = if params.is_empty() && !block.params.is_empty() {
            vec![Ty::ERROR; block.params.len()]
        } else {
            params.to_vec()
        };
        self.block_with_rest(block, &params, None, want)
    }

    fn block_with_rest(
        &mut self,
        block: &'a Block,
        params: &[Ty],
        rest: Option<Ty>,
        want: Want,
    ) -> Ty {
        self.open_scope();
        let before = self.frame.flow.mark();
        let (result, used) = match want {
            Want::Check(expected) => (Some(expected), true),
            Want::Infer(_) => (None, true),
            Want::Discard => (None, false),
        };
        self.frame.contexts.push(Context::Block {
            mark: before,
            exits: super::check::Exits::default(),
            result,
            used,
            results: Vec::new(),
        });
        let targets = &block.params;
        if block.implicit {
            let first = params.first().copied().unwrap_or(Ty::NIL);
            if block.infer_it {
                let id = self.declare("it", first, block.offset as usize, false);
                self.assign_local(id, first);
            }
            for index in 0..9 {
                let name = format!("_{}", index + 1);
                let ty = params.get(index).copied().or(rest).unwrap_or(Ty::NIL);
                let id = self.declare(&name, ty, block.offset as usize, false);
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
                            format!(
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
        self.purposes.push(Purpose::BlockResult);
        let tail = self.stmts(&block.body, want);
        self.purposes.pop();
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
            Context::Loop { .. } => Vec::new(),
        };
        if live {
            results.push(tail);
        }
        self.finish_loop(before, context, true);
        self.close_scope();
        self.types.union(&results)
    }

    /// Binds a block parameter, which is always a new local of the block.
    fn bind_target_param(&mut self, target: &'a Target, ty: Ty) {
        match target {
            Target::Value(Expr {
                node: Node::Var(name),
                offset,
                ..
            }) if !name.starts_with('@') => {
                let id = self.declare(name, ty, *offset as usize, false);
                self.assign_local(id, ty);
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
                format!(
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
                    self.expr_against(arg, param, &Purpose::Yield(index));
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
                format!(
                    "the block takes {} argument(s), but `yield` passes {}",
                    block.params.len(),
                    args.len()
                ),
            ));
        }
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
/// numbered parameter or `it` it reads.
fn block_arity(block: &Block) -> usize {
    if !block.implicit {
        return block.params.len();
    }
    let mut arity = 0;
    let mut pending: Vec<&Expr> = Vec::new();
    let mut statements: Vec<&crate::syntax::Stmt> = block.body.iter().collect();
    while let Some(stmt) = statements.pop() {
        match &stmt.node {
            crate::syntax::Statement::Expr(e) => pending.push(e),
            crate::syntax::Statement::Assign(_, _, e) => pending.push(e),
            crate::syntax::Statement::Return(Some(e))
            | crate::syntax::Statement::Next(Some(e))
            | crate::syntax::Statement::Break(Some(e)) => pending.push(e),
            crate::syntax::Statement::If(branches, alternate, _) => {
                for (c, body) in branches.iter() {
                    pending.push(c);
                    statements.extend(body.iter());
                }
                statements.extend(alternate.iter());
            }
            _ => (),
        }
        while let Some(e) = pending.pop() {
            match &e.node {
                Node::Var(name) if name.as_str() == "it" => arity = arity.max(1),
                Node::Var(name) => {
                    if let Some(n) = name.strip_prefix('_').and_then(|n| n.parse::<usize>().ok()) {
                        arity = arity.max(n);
                    }
                }
                Node::Binary(_, l, r) => {
                    pending.push(l);
                    pending.push(r);
                }
                Node::Unary(_, v) => pending.push(v),
                Node::Method(r, _, args, _) | Node::SafeMethod(r, _, args, _) => {
                    pending.push(r);
                    pending.extend(args.iter().map(|a| &a.value));
                }
                Node::Member(r, _) | Node::SafeMember(r, _) => pending.push(r),
                Node::Call(_, args, _) => pending.extend(args.iter().map(|a| &a.value)),
                Node::Index(r, s) => {
                    pending.push(r);
                    pending.extend(s.iter());
                }
                Node::Array(items) | Node::Template(items, _) => pending.extend(items.iter()),
                Node::Hash(entries) => pending.extend(entries.iter().map(|(_, v)| v)),
                Node::Conditional(branches, alternate) => {
                    for (c, v) in branches.iter() {
                        pending.push(c);
                        pending.push(v);
                    }
                    pending.push(alternate);
                }
                _ => (),
            }
        }
    }
    arity
}
