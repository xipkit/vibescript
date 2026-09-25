//! Checks function bodies: statements, locals, flow and narrowing.

use super::{
    Checker,
    flow::{Branch, Flow, LocalId, Mark, VarState},
    program::{FnId, NsId},
    sigs::BlockSig,
    ty::{Kind, Ty},
};
use crate::{
    diagnostic::{Code, Diagnostic, Edit, Fix, Span},
    syntax::{Expr, Node, Statement, Stmt, Target},
};
use std::collections::HashMap;

/// A local variable or parameter.
pub(crate) struct Local {
    pub name: String,
    pub declared: Ty,
    /// Where it was declared, for "declared here" notes and fixes.
    pub offset: usize,
    /// Whether an annotation fixed its type.
    pub annotated: bool,
    /// The declaring hash literal's field types, when a fix may declare the
    /// local as a dictionary.
    pub dictionary: Option<Ty>,
}

/// The states a loop or block is left or continued with.
#[derive(Default)]
pub(crate) struct Exits {
    /// The state at each `break`, which leaves the loop or the call.
    pub breaks: Vec<Branch>,
    /// The state at each `next`, which starts the next iteration.
    pub nexts: Vec<Branch>,
    /// The types of the values `break` gives.
    pub values: Vec<Ty>,
}

/// An enclosing construct that `next` and `break` refer to.
pub(crate) enum Context {
    Loop {
        mark: Mark,
        exits: Exits,
    },
    Block {
        mark: Mark,
        exits: Exits,
        /// The type the block must return, when known.
        result: Option<Ty>,
        /// The declared result a `break` returns through, and the function
        /// that declares it, when the runtime checks break values against it.
        break_to: Option<(Ty, String)>,
        /// Whether the block's value is used at all.
        used: bool,
        /// The values `next` and the tail gave, for inference.
        results: Vec<Ty>,
    },
}

impl Context {
    pub fn exits(&mut self) -> &mut Exits {
        match self {
            Context::Loop { exits, .. } | Context::Block { exits, .. } => exits,
        }
    }

    fn mark(&self) -> Mark {
        match self {
            Context::Loop { mark, .. } | Context::Block { mark, .. } => *mark,
        }
    }
}

/// The state of the function being checked.
pub(crate) struct Frame {
    pub owner: Option<NsId>,
    /// Whether `self` is an instance.
    pub instance: bool,
    /// The declared result; `None` for a function that returns `nil`.
    pub result: Option<Ty>,
    pub main: bool,
    pub name: String,
    /// The function's own typed block parameter.
    pub block: Option<BlockSig>,
    /// A pseudo-local that is assigned where `block_given?` holds.
    pub block_given: Option<LocalId>,
    /// In `initialize`: pseudo-locals for the instance variables it must assign.
    pub initialize: Vec<(String, LocalId)>,
    pub locals: Vec<Local>,
    pub names: HashMap<String, LocalId>,
    /// Names each open block scope shadowed, to restore when it closes.
    pub scopes: Vec<Vec<(String, Option<LocalId>)>>,
    pub flow: Flow,
    pub contexts: Vec<Context>,
    /// Whether the body is a class or module body, whose capitalized
    /// assignments are constants.
    pub namespace_body: bool,
}

impl Frame {
    pub fn new(owner: Option<NsId>, instance: bool, result: Option<Ty>, name: String) -> Self {
        Self {
            owner,
            instance,
            result,
            main: false,
            name,
            block: None,
            block_given: None,
            initialize: Vec::new(),
            locals: Vec::new(),
            names: HashMap::new(),
            scopes: Vec::new(),
            flow: Flow::new(),
            contexts: Vec::new(),
            namespace_body: false,
        }
    }
}

/// What a statement's value is for.
#[derive(Clone, Copy)]
pub(crate) enum Want {
    /// Its value is discarded.
    Discard,
    /// Its value is used; the type, when given, is context for literals.
    Infer(Option<Ty>),
    /// Its value must be assignable to this type.
    Check(Ty),
}

impl Want {
    pub fn hint(self) -> Option<Ty> {
        match self {
            Want::Discard => None,
            Want::Infer(hint) => hint,
            Want::Check(ty) => Some(ty),
        }
    }
}

/// Narrowings a condition implies for locals, when it holds and when not.
#[derive(Default, Clone)]
pub(crate) struct Narrow {
    pub then: Vec<(LocalId, Ty)>,
    pub otherwise: Vec<(LocalId, Ty)>,
}

impl<'a> Checker<'a> {
    pub(super) fn report(&mut self, diagnostic: Diagnostic) {
        if self.mute > 0 {
            return;
        }
        self.diagnostics.push(diagnostic);
    }

    /// Checks every function and namespace body.
    pub(super) fn check_all(&mut self) {
        for ns in 0..self.program.namespaces.len() {
            self.check_namespace_body(ns as NsId);
        }
        for id in 0..self.program.fns.len() {
            self.check_function(id);
        }
    }

    fn check_namespace_body(&mut self, ns: NsId) {
        let module = self.program.namespaces[ns as usize].module;
        let name = self.program.namespaces[ns as usize].name.clone();
        let mut frame = Frame::new(Some(ns), false, None, name);
        frame.namespace_body = true;
        let previous = self.enter_frame(frame);
        self.stmts(&module.body, Want::Discard);
        // Instance-variable defaults run for each instance.
        let defaults: Vec<&'a Stmt> = self
            .parsed
            .additions
            .defaults
            .iter()
            .filter(|(owner, _)| *owner == module.offset)
            .map(|(_, stmt)| stmt)
            .collect();
        let name = self.frame.name.clone();
        let body = self.enter_frame(Frame::new(Some(ns), true, None, name));
        for stmt in defaults {
            self.stmt(stmt, Want::Discard);
        }
        self.leave_frame(body);
        self.leave_frame(previous);
    }

    fn check_function(&mut self, id: FnId) {
        let decl = &self.program.fns[id];
        let def = decl.def;
        let sig = decl.sig.clone();
        let owner = decl.owner;
        let instance = decl.instance;
        let main = decl.main;
        let accessor = def.accessor.is_some();
        let mut frame = Frame::new(owner, instance, sig.result, sig.name.clone());
        frame.main = main;
        frame.block = sig.block.clone();
        let previous = self.enter_frame(frame);
        if instance && def.name == "initialize" {
            self.track_initialize(owner);
        }
        for (param, declared) in def.params.iter().zip(&sig.params) {
            let local = self.declare(&param.name, declared.ty, def.offset as usize, true);
            self.assign_local(local, declared.ty);
            if let Some(ivar) = &param.ivar {
                self.assign_ivar(ivar, declared.ty, Span::at(def.offset as usize), accessor);
            }
        }
        if sig.block.as_ref().is_some_and(|block| block.optional) {
            let given = self.pseudo_local();
            self.frame.block_given = Some(given);
        }
        if accessor {
            // Properties read and write their declared instance variable.
            self.leave_frame(previous);
            return;
        }
        let want = match (main, sig.result) {
            (true, _) => Want::Infer(None),
            (false, Some(result)) => Want::Check(result),
            (false, None) => Want::Discard,
        };
        let body = &def.body;
        self.stmts(body, want);
        if self.frame.flow.live {
            if let (Some(result), false) = (sig.result, main) {
                if body.is_empty() && !self.types.assignable(Ty::NIL, result) {
                    let span = self.spans.token(def.offset as usize);
                    let expected = self.types.display(result);
                    self.report(
                        Diagnostic::error(
                            Code::TYPE_MISMATCH,
                            span,
                            format!(
                                "`{}` returns {expected}, but its body is empty and returns nil",
                                def.name
                            ),
                        )
                        .with_types(expected, "nil"),
                    );
                }
            }
            self.finish_initialize(Span::at(def.offset as usize));
        }
        self.leave_frame(previous);
    }

    /// Declares the instance variables `initialize` must assign.
    fn track_initialize(&mut self, owner: Option<NsId>) {
        let Some(ns) = owner else {
            return;
        };
        let mut required: Vec<(String, Ty)> = self.program.namespaces[ns as usize]
            .ivars
            .iter()
            .filter(|(_, ivar)| !ivar.default)
            .map(|(name, ivar)| (name.clone(), ivar.ty))
            .collect();
        required.sort_by(|a, b| a.0.cmp(&b.0));
        for (name, ty) in required {
            if self.types.assignable(Ty::NIL, ty) {
                continue;
            }
            let id = self.pseudo_local();
            self.frame.initialize.push((name, id));
        }
    }

    /// Reports the instance variables a path through `initialize` left unassigned.
    pub(super) fn finish_initialize(&mut self, span: Span) {
        let missing: Vec<String> = self
            .frame
            .initialize
            .iter()
            .filter(|(_, id)| !self.frame.flow.get(*id).assigned)
            .map(|(name, _)| format!("@{name}"))
            .collect();
        if missing.is_empty() {
            return;
        }
        // Report each variable once per function.
        let ids: Vec<LocalId> = self.frame.initialize.iter().map(|(_, id)| *id).collect();
        for id in ids {
            let state = self.frame.flow.get(id);
            self.frame.flow.set(
                id,
                VarState {
                    assigned: true,
                    ..state
                },
            );
        }
        self.frame.initialize.clear();
        self.report(Diagnostic::error(
            Code::UNINITIALIZED_IVAR,
            span,
            format!(
                "`initialize` does not assign {} on every path; assign {} or give {} a default in the class body",
                missing.join(", "),
                if missing.len() == 1 { "it" } else { "them" },
                if missing.len() == 1 { "it" } else { "them" },
            ),
        ));
    }

    // Locals -----------------------------------------------------------

    /// Declares a local in the innermost scope.
    pub(super) fn declare(
        &mut self,
        name: &str,
        declared: Ty,
        offset: usize,
        annotated: bool,
    ) -> LocalId {
        let id = self.frame.flow.add(declared);
        debug_assert_eq!(id as usize, self.frame.locals.len());
        self.frame.locals.push(Local {
            name: name.to_owned(),
            declared,
            offset,
            annotated,
            dictionary: None,
        });
        let previous = self.frame.names.insert(name.to_owned(), id);
        if let Some(scope) = self.frame.scopes.last_mut() {
            scope.push((name.to_owned(), previous));
        }
        id
    }

    /// A flow fact that is not a named local, such as whether a block was given.
    fn pseudo_local(&mut self) -> LocalId {
        let id = self.frame.flow.add(Ty::BOOL);
        self.frame.locals.push(Local {
            name: String::new(),
            declared: Ty::BOOL,
            offset: 0,
            annotated: true,
            dictionary: None,
        });
        id
    }

    pub(super) fn local(&self, name: &str) -> Option<LocalId> {
        self.frame.names.get(name).copied()
    }

    /// Records an assignment of a value of type `ty`, narrowing the local to it.
    pub(super) fn assign_local(&mut self, id: LocalId, ty: Ty) {
        let declared = self.frame.locals[id as usize].declared;
        let narrowed = self.narrowed(declared, ty);
        self.frame.flow.set(
            id,
            VarState {
                ty: narrowed,
                assigned: true,
            },
        );
    }

    /// The alternatives of `declared` that a value of type `ty` may be.
    pub(super) fn narrowed(&mut self, declared: Ty, ty: Ty) -> Ty {
        if ty == Ty::ERROR || declared == Ty::ERROR || declared == Ty::ANY {
            return declared;
        }
        let alternatives = self.types.members(declared);
        if alternatives.len() < 2 {
            return declared;
        }
        let values = self.types.members(ty);
        let kept: Vec<Ty> = alternatives
            .into_iter()
            .filter(|&alt| values.iter().any(|&v| self.types.assignable(v, alt)))
            .collect();
        if kept.is_empty() {
            declared
        } else {
            self.types.union(&kept)
        }
    }

    pub(super) fn open_scope(&mut self) {
        self.frame.scopes.push(Vec::new());
    }

    pub(super) fn close_scope(&mut self) {
        let Some(scope) = self.frame.scopes.pop() else {
            return;
        };
        for (name, previous) in scope.into_iter().rev() {
            match previous {
                Some(id) => {
                    self.frame.names.insert(name, id);
                }
                None => {
                    self.frame.names.remove(&name);
                }
            }
        }
    }

    pub(super) fn apply(&mut self, narrowings: &[(LocalId, Ty)]) {
        for &(id, ty) in narrowings {
            let state = self.frame.flow.get(id);
            if Some(id) == self.frame.block_given {
                // Where `block_given?` holds, the block counts as given.
                self.frame.flow.set(id, VarState { ty, assigned: true });
                continue;
            }
            if !state.assigned {
                continue;
            }
            self.frame.flow.set(id, VarState { ty, ..state });
        }
    }

    pub(super) fn join(&mut self, branches: Vec<Branch>) {
        let declared: Vec<Ty> = self.frame.locals.iter().map(|l| l.declared).collect();
        let lookup = move |id: LocalId| declared.get(id as usize).copied().unwrap_or(Ty::BOOL);
        self.frame.flow.join(&mut self.types, branches, &lookup);
    }

    /// Widens the locals a loop body assigns back to their declared types,
    /// since the body may run again after narrowing them.
    pub(super) fn widen_for_loop(&mut self, body: &[Stmt]) {
        let mut names = Vec::new();
        assigned_names(body, &mut names);
        self.steps += names.len() as u64 + body.len() as u64;
        for name in names {
            if let Some(id) = self.local(&name) {
                let state = self.frame.flow.get(id);
                let declared = self.frame.locals[id as usize].declared;
                self.frame.flow.set(
                    id,
                    VarState {
                        ty: declared,
                        ..state
                    },
                );
            }
        }
    }

    // Statements -------------------------------------------------------

    /// Checks statements in order; the last one's value is `want`ed.
    pub(super) fn stmts(&mut self, body: &'a [Stmt], want: Want) -> Ty {
        let Some((last, rest)) = body.split_last() else {
            return Ty::NIL;
        };
        for stmt in rest {
            self.stmt(stmt, Want::Discard);
        }
        self.stmt(last, want)
    }

    /// Makes `frame` current, keeping the work the replaced frame did.
    pub(super) fn enter_frame(&mut self, frame: Frame) -> Frame {
        let previous = std::mem::replace(&mut self.frame, frame);
        self.steps += previous.flow.steps;
        previous
    }

    /// Restores a frame [`Self::enter_frame`] replaced.
    pub(super) fn leave_frame(&mut self, previous: Frame) {
        let finished = std::mem::replace(&mut self.frame, previous);
        self.steps += finished.flow.steps;
        // The restored frame's work was counted when it was replaced.
        self.frame.flow.steps = 0;
    }

    pub(super) fn stmt(&mut self, stmt: &'a Stmt, want: Want) -> Ty {
        self.steps += 1;
        if !self.frame.flow.live {
            // Unreachable code is still checked, from a live state.
            self.frame.flow.live = true;
        }
        match &stmt.node {
            Statement::Expr(expr) => match want {
                Want::Discard => {
                    self.expr_want(expr, Want::Discard);
                    Ty::NIL
                }
                Want::Infer(hint) => self.expr(expr, hint),
                Want::Check(expected) => {
                    let purpose = self.purposes.last().cloned().unwrap_or(Purpose::Result);
                    self.expr_against(expr, expected, &purpose)
                }
            },
            Statement::Assign(target, op, value) => {
                self.mark_target(target);
                let ty = self.assignment(stmt, target, op, value);
                self.statement_value(stmt, ty, want)
            }
            Statement::If(branches, alternate, _) => {
                self.if_statement(stmt.offset as usize, branches, alternate, want)
            }
            Statement::While(condition, body, _) => {
                let ty = self.while_loop(condition, body);
                self.statement_value(stmt, ty, want)
            }
            Statement::For(target, iterable, body) => {
                let ty = self.for_loop(target, iterable, body);
                self.statement_value(stmt, ty, want)
            }
            Statement::Return(value) => {
                self.return_statement(stmt, value.as_ref());
                Ty::NEVER
            }
            Statement::Break(value) => {
                self.break_statement(stmt, value.as_ref());
                Ty::NEVER
            }
            Statement::Next(value) => {
                self.next_statement(stmt, value.as_ref());
                Ty::NEVER
            }
            Statement::Raise(value, message) => {
                self.raise(value.as_deref(), message.as_deref());
                self.frame.flow.live = false;
                Ty::NEVER
            }
            Statement::Retry => {
                self.frame.flow.live = false;
                Ty::NEVER
            }
            Statement::Module(_) | Statement::UnboundClass(_) | Statement::Unsupported => {
                self.statement_value(stmt, Ty::NIL, want)
            }
        }
    }

    /// Checks a statement's value against what is wanted of it.
    fn statement_value(&mut self, stmt: &Stmt, ty: Ty, want: Want) -> Ty {
        if let Want::Check(expected) = want {
            if self.frame.flow.live && !self.types.assignable(ty, expected) {
                let span = self.spans.stmt(stmt);
                let purpose = self.purposes.last().cloned().unwrap_or(Purpose::Result);
                self.mismatch(span, expected, ty, &purpose);
            }
        }
        ty
    }

    fn if_statement(
        &mut self,
        if_offset: usize,
        branches: &'a [(Expr, crate::compilation::Buffer<Stmt>)],
        alternate: &'a [Stmt],
        want: Want,
    ) -> Ty {
        let mut results = Vec::new();
        let mut explored = Vec::new();
        let entry = self.frame.flow.mark();
        for (condition, body) in branches {
            let narrow = self.condition(condition);
            let mark = self.frame.flow.mark();
            self.apply(&narrow.then);
            let ty = self.stmts(body, want);
            if self.frame.flow.live {
                results.push(ty);
            }
            explored.push(self.frame.flow.rollback(mark));
            self.apply(&narrow.otherwise);
        }
        let ty = if alternate.is_empty() {
            if let (Want::Check(expected), true) = (want, self.frame.flow.live) {
                if !self.types.assignable(Ty::NIL, expected) {
                    let span = self.spans.token(if_offset);
                    let expected_text = self.types.display(expected);
                    let purpose = self.purposes.last().cloned().unwrap_or(Purpose::Result);
                    let what = self.purpose_text(&purpose, &expected_text);
                    self.report(
                        Diagnostic::error(
                            Code::TYPE_MISMATCH,
                            span,
                            format!("{what}, but this `if` has no `else`, so it gives nil when no branch runs"),
                        )
                        .with_types(expected_text, "nil"),
                    );
                }
            }
            Ty::NIL
        } else {
            self.stmts(alternate, want)
        };
        if self.frame.flow.live {
            results.push(ty);
        }
        explored.push(self.frame.flow.rollback(entry));
        self.join(explored);
        self.types.union(&results)
    }

    /// Checks a `while` loop and returns its value: `nil`, or what `break`
    /// gives.
    fn while_loop(&mut self, condition: &'a Expr, body: &'a [Stmt]) -> Ty {
        self.widen_for_loop(body);
        let before = self.frame.flow.mark();
        let infinite =
            matches!(&condition.node, Node::Literal(v) if v.type_name() == "bool" && v.truthy());
        let narrow = self.condition(condition);
        self.apply(&narrow.then);
        self.frame.contexts.push(Context::Loop {
            mark: before,
            exits: Exits::default(),
        });
        self.stmts(body, Want::Discard);
        let context = self.frame.contexts.pop().unwrap();
        let mut values = self.finish_loop(before, context, !infinite);
        if !infinite {
            values.push(Ty::NIL);
        }
        self.types.union(&values)
    }

    /// Joins the states a loop or block can be left with and returns the
    /// types of the values `break` gave. A loop that ends by itself is left
    /// from its end, each `next` and, when it may not run, from before it;
    /// an infinite one only by `break`.
    pub(super) fn finish_loop(
        &mut self,
        before: Mark,
        mut context: Context,
        ends: bool,
    ) -> Vec<Ty> {
        let exits = std::mem::take(context.exits());
        let end = self.frame.flow.rollback(before);
        let mut branches = exits.breaks;
        if ends {
            branches.extend(exits.nexts);
            branches.push(end);
            branches.push(Branch {
                live: true,
                changes: Vec::new(),
            });
        }
        self.join(branches);
        exits.values
    }

    fn for_loop(&mut self, target: &'a Target, iterable: &'a Expr, body: &'a [Stmt]) -> Ty {
        let ty = self.expr(iterable, None);
        let element = self.iterated(ty, iterable);
        self.widen_for_loop(body);
        let before = self.frame.flow.mark();
        self.bind_target(target, element, false);
        self.frame.contexts.push(Context::Loop {
            mark: before,
            exits: Exits::default(),
        });
        self.stmts(body, Want::Discard);
        let context = self.frame.contexts.pop().unwrap();
        self.finish_loop(before, context, true);
        ty
    }

    /// The element type `for` binds when iterating a value of type `ty`.
    fn iterated(&mut self, ty: Ty, iterable: &Expr) -> Ty {
        if let Some(element) = self.types.element(ty) {
            return element;
        }
        if let Some(value) = self.types.hash_value(ty) {
            return self.types.tuple(vec![Ty::STRING, value]);
        }
        let span = self.spans.expr(iterable);
        let found = self.types.display(ty);
        self.report(
            Diagnostic::error(
                Code::TYPE_MISMATCH,
                span,
                format!("`for` iterates an array, hash or range, not {found}"),
            )
            .with_types("array | hash | range", found),
        );
        Ty::ERROR
    }

    fn return_statement(&mut self, stmt: &'a Stmt, value: Option<&'a Expr>) {
        if self.frame.main {
            if let Some(value) = value {
                self.expr(value, None);
            }
            self.frame.flow.live = false;
            return;
        }
        match (self.frame.result, value) {
            (Some(result), Some(value)) => {
                self.expr_against(value, result, &Purpose::Result);
            }
            (Some(result), None) => {
                if !self.types.assignable(Ty::NIL, result) {
                    let span = self.spans.stmt(stmt);
                    self.mismatch(span, result, Ty::NIL, &Purpose::Result);
                }
            }
            (None, Some(value)) => {
                let ty = self.expr(value, None);
                let literal_nil = matches!(&value.node, Node::Literal(v) if v.type_name() == "nil");
                if !literal_nil && ty != Ty::ERROR {
                    let span = self.spans.stmt(stmt);
                    let found = self.types.display(ty);
                    self.report(Diagnostic::error(
                        Code::RETURN_WITHOUT_TYPE,
                        span,
                        format!(
                            "`{}` declares no result type, so it returns nil; declare `-> {found}` to return this value",
                            self.frame.name
                        ),
                    ));
                }
            }
            (None, None) => (),
        }
        if self.frame.flow.live {
            let span = self.spans.stmt(stmt);
            self.finish_initialize(span);
        }
        self.frame.flow.live = false;
    }

    /// `raise message`, `raise error`, `raise Class` or `raise Class, message`.
    fn raise(&mut self, value: Option<&'a Expr>, message: Option<&'a Expr>) {
        let class = |checker: &Self, expr: &Expr| match &expr.node {
            Node::Var(name) => {
                checker.local(name).is_none() && crate::ErrorClass::from_name(name).is_some()
            }
            _ => false,
        };
        if let Some(message) = message {
            self.expr_against(message, Ty::STRING, &Purpose::Operand);
            if let Some(value) = value {
                if !class(self, value) {
                    let ty = self.expr(value, None);
                    if ty != Ty::ERROR {
                        let span = self.spans.expr(value);
                        self.report(Diagnostic::error(
                            Code::TYPE_MISMATCH,
                            span,
                            "`raise` with a message takes an error class first, such as `raise ArgumentError, \"...\"`",
                        ));
                    }
                }
            }
            return;
        }
        let Some(value) = value else {
            return;
        };
        if class(self, value) {
            return;
        }
        let ty = self.expr(value, None);
        if ty == Ty::ERROR || ty == Ty::STRING || ty == Ty::ERROR_VALUE || ty == Ty::NEVER {
            return;
        }
        let span = self.spans.expr(value);
        let found = self.types.display(ty);
        self.report(
            Diagnostic::error(
                Code::TYPE_MISMATCH,
                span,
                format!(
                    "`raise` takes a message, an error class or a rescued error, found {found}"
                ),
            )
            .with_types("string | error", found),
        );
    }

    fn break_statement(&mut self, stmt: &'a Stmt, value: Option<&'a Expr>) {
        let break_to = match self.frame.contexts.last() {
            Some(Context::Block { break_to, .. }) => break_to.clone(),
            _ => None,
        };
        let ty = match (value, break_to) {
            (Some(value), Some((result, function))) => {
                self.expr_against(value, result, &Purpose::Break(function))
            }
            (Some(value), None) => self.expr(value, None),
            (None, Some((result, function))) => {
                if self.frame.flow.live && !self.types.assignable(Ty::NIL, result) {
                    let span = self.spans.stmt(stmt);
                    self.mismatch(span, result, Ty::NIL, &Purpose::Break(function));
                }
                Ty::NIL
            }
            (None, None) => Ty::NIL,
        };
        if self.frame.flow.live {
            if let Some(context) = self.frame.contexts.last_mut() {
                context.exits().values.push(ty);
            }
        }
        self.exit_context(true);
        self.frame.flow.live = false;
    }

    /// Records the state at a `break` or `next` for the enclosing loop or block.
    fn exit_context(&mut self, leaves: bool) {
        if !self.frame.flow.live {
            return;
        }
        let Some(context) = self.frame.contexts.last() else {
            return;
        };
        let branch = self.frame.flow.peek(context.mark());
        let exits = self.frame.contexts.last_mut().unwrap().exits();
        if leaves {
            exits.breaks.push(branch);
        } else {
            exits.nexts.push(branch);
        }
    }

    fn next_statement(&mut self, stmt: &'a Stmt, value: Option<&'a Expr>) {
        let block = match self.frame.contexts.last() {
            Some(Context::Block { result, used, .. }) => Some((*result, *used)),
            _ => None,
        };
        match block {
            Some((result, used)) => {
                let ty = match (value, result) {
                    (Some(value), Some(result)) => {
                        self.expr_against(value, result, &Purpose::BlockResult)
                    }
                    (Some(value), None) => self.expr(value, None),
                    (None, Some(result)) => {
                        if used && !self.types.assignable(Ty::NIL, result) {
                            let span = self.spans.stmt(stmt);
                            self.mismatch(span, result, Ty::NIL, &Purpose::BlockResult);
                        }
                        Ty::NIL
                    }
                    (None, None) => Ty::NIL,
                };
                if let Some(Context::Block { results, .. }) = self.frame.contexts.last_mut() {
                    results.push(ty);
                }
            }
            None => {
                if let Some(value) = value {
                    self.expr(value, None);
                }
            }
        }
        self.exit_context(false);
        self.frame.flow.live = false;
    }

    // Assignment -------------------------------------------------------

    /// Marks the reads an assignment to `target` writes through.
    fn mark_target(&mut self, target: &Target) {
        match target {
            Target::Value(Expr {
                node: Node::Index(receiver, _) | Node::Member(receiver, _),
                ..
            }) => self.mark_write_chain(receiver),
            Target::Typed(inner, _) => self.mark_target(inner),
            Target::Tuple(parts) => {
                for part in parts.iter().filter_map(|(part, _)| part.as_ref()) {
                    self.mark_target(part);
                }
            }
            Target::Value(_) => (),
        }
    }

    /// Marks `receiver` and the reads it goes through as reads a write
    /// goes through.
    pub(super) fn mark_write_chain(&mut self, receiver: &Expr) {
        let mut current = receiver;
        loop {
            self.write_chain
                .insert(std::ptr::from_ref(current) as usize);
            current = match &current.node {
                Node::Index(inner, _) | Node::Member(inner, _) | Node::Method(inner, ..) => inner,
                _ => return,
            };
        }
    }

    /// Whether a write goes through the read `expr`.
    pub(super) fn in_write_chain(&self, expr: &Expr) -> bool {
        self.write_chain
            .contains(&(std::ptr::from_ref(expr) as usize))
    }

    fn assignment(&mut self, stmt: &'a Stmt, target: &'a Target, op: &str, value: &'a Expr) -> Ty {
        match op {
            "=" => self.assign(target, value),
            "||=" | "&&=" => {
                let outer = self.memo.replace(super::Memo::default());
                let current = self.target_read(target);
                if current != Ty::ERROR && current != Ty::BOOL {
                    let span = self.target_span(target);
                    let found = self.types.display(current);
                    self.report(
                        Diagnostic::error(
                            Code::CONDITION_NOT_BOOL,
                            span,
                            format!(
                                "`{op}` tests its target, which must be a bool, found {found}; assign under an explicit nil test instead"
                            ),
                        )
                        .with_types("bool", found),
                    );
                }
                let ty = self.expr(value, Some(current));
                self.memo.as_mut().unwrap().replay = true;
                self.target_write(target, ty, value);
                self.restore_memo(outer);
                ty
            }
            _ => {
                let operator = &op[..op.len() - 1];
                // The write reuses the types the read found for the target's
                // receiver and selectors instead of checking them again.
                let outer = self.memo.replace(super::Memo::default());
                let current = self.target_read(target);
                let right = self.expr(value, None);
                let span = self.spans.stmt(stmt);
                self.memo.as_mut().unwrap().replay = true;
                let result = match self.optional_element(target, op, value, current) {
                    Some(present) => {
                        self.binary_types(operator, present, right, span, Some((None, value)))
                    }
                    None => self.binary_types(
                        operator,
                        current,
                        right,
                        span,
                        Some((target_expr(target), value)),
                    ),
                };
                self.target_write(target, result, value);
                self.restore_memo(outer);
                result
            }
        }
    }

    /// Reports a compound assignment to an array element or hash entry
    /// that may be missing, `x[i] += v`, offering `x[i] = x.fetch(i) + v`,
    /// and returns the element's type without nil. The receiver's types
    /// replay from the target's read.
    fn optional_element(
        &mut self,
        target: &'a Target,
        op: &str,
        value: &'a Expr,
        current: Ty,
    ) -> Option<Ty> {
        let Target::Value(expr) = target else {
            return None;
        };
        let Node::Index(receiver, selectors) = &expr.node else {
            return None;
        };
        if selectors.len() != 1 || !self.types.has_nil(current) {
            return None;
        }
        let present = self.types.without_nil(current);
        if present == Ty::NEVER {
            return None;
        }
        let span = self.spans.expr(expr);
        let found = self.types.display(current);
        let mut diagnostic = Diagnostic::error(
            Code::OPTIONAL_USE,
            span,
            format!(
                "this element may be nil ({found}), as it is when missing; read it with `fetch`, which raises when it is missing, or test it with `!= nil` first"
            ),
        );
        let receiver_ty = self.expr(receiver, None);
        let stored = match self.types.kind(receiver_ty).clone() {
            Kind::Array(element) => Some(element),
            Kind::Hash(value) => Some(value),
            _ => None,
        };
        if stored.is_some_and(|stored| stored == present)
            && self.stable(receiver)
            && self.stable(&selectors[0])
        {
            if let Some(fix) = self.fetch_assignment(expr, receiver, &selectors[0], op, value) {
                diagnostic = diagnostic.with_fix(fix);
            }
        }
        self.report(diagnostic);
        Some(present)
    }

    /// Whether an expression reads the same value each time, so a fix may
    /// repeat it: a local, an instance variable, a literal, or an element
    /// of one at a key that is one, as in `grid[0]`.
    fn stable(&self, expr: &Expr) -> bool {
        match &expr.node {
            Node::Var(name) => name.starts_with('@') || self.local(name).is_some(),
            Node::Integer(_) | Node::Literal(_) => true,
            Node::Index(receiver, selectors) => {
                selectors.len() == 1 && self.stable(receiver) && self.stable(&selectors[0])
            }
            _ => false,
        }
    }

    /// `x[i] = x.fetch(i) + v` in place of `x[i] += v`.
    fn fetch_assignment(
        &self,
        target: &Expr,
        receiver: &Expr,
        selector: &Expr,
        op: &str,
        value: &Expr,
    ) -> Option<Fix> {
        let target_end = self.spans.expr(target).end;
        let value_span = self.spans.expr(value);
        let between = self.source.get(target_end..value_span.start)?;
        let at = target_end + between.find(op)?;
        let receiver_span = self.spans.expr(receiver);
        let selector_span = self.spans.expr(selector);
        let receiver_text = self.source.get(receiver_span.start..receiver_span.end)?;
        let selector_text = self.source.get(selector_span.start..selector_span.end)?;
        let operator = &op[..op.len() - 1];
        let grouped = matches!(
            value.node,
            Node::Integer(_)
                | Node::BigInteger(..)
                | Node::Literal(_)
                | Node::Template(..)
                | Node::Var(_)
                | Node::Array(_)
                | Node::Call(..)
                | Node::Method(..)
                | Node::Member(..)
                | Node::Index(..)
        );
        let (open, close) = if grouped { ("", "") } else { ("(", ")") };
        let mut edits = vec![
            Edit {
                span: Span::new(at, at + op.len()),
                replacement: "=".to_owned(),
            },
            Edit {
                span: Span::at(value_span.start),
                replacement: format!("{receiver_text}.fetch({selector_text}) {operator} {open}"),
            },
        ];
        if !close.is_empty() {
            edits.push(Edit {
                span: Span::at(value_span.end),
                replacement: close.to_owned(),
            });
        }
        Some(Fix::edits(
            format!("read it with `{receiver_text}.fetch({selector_text})`"),
            edits,
        ))
    }

    fn target_span(&self, target: &Target) -> Span {
        match target {
            Target::Value(expr) => self.spans.expr(expr),
            Target::Typed(inner, _) => self.target_span(inner),
            Target::Tuple(_) => Span::at(target.offset().unwrap_or(0) as usize),
        }
    }

    /// The current value of an assignment target, for compound assignment.
    fn target_read(&mut self, target: &'a Target) -> Ty {
        match target {
            Target::Value(expr) => self.expr(expr, None),
            Target::Typed(inner, _) => self.target_read(inner),
            Target::Tuple(_) => Ty::ERROR,
        }
    }

    /// Stores a value of type `ty` computed from `value` into a target.
    fn target_write(&mut self, target: &'a Target, ty: Ty, value: &'a Expr) {
        match target {
            Target::Value(expr) => match &expr.node {
                Node::Var(name) if name.starts_with("@@") => {
                    self.write_class_variable(name, ty, expr, value);
                }
                Node::Var(name) if name.starts_with('@') => {
                    let span = self.spans.expr(expr);
                    self.write_ivar(&name[1..], ty, span, value);
                }
                Node::Var(name) => {
                    if let Some(id) = self.local(name) {
                        let declared = self.frame.locals[id as usize].declared;
                        if !self.types.assignable(ty, declared) {
                            let span = self.spans.expr(value);
                            self.local_changed(id, span, ty);
                        }
                        self.assign_local(id, ty);
                    }
                }
                Node::Index(receiver, selectors) => {
                    self.index_write(expr, receiver, selectors, ty, value, false);
                }
                Node::Member(receiver, name) => {
                    self.setter(expr, receiver, name, ty, value, false);
                }
                _ => (),
            },
            Target::Typed(inner, _) => self.target_write(inner, ty, value),
            Target::Tuple(_) => (),
        }
    }

    fn assign(&mut self, target: &'a Target, value: &'a Expr) -> Ty {
        match target {
            Target::Typed(inner, annotation) => {
                let Target::Value(Expr {
                    node: Node::Var(name),
                    offset,
                    ..
                }) = &**inner
                else {
                    let ty = self.expr(value, None);
                    self.bind_target(target, ty, true);
                    return ty;
                };
                let declared = self.annotation(annotation, self.frame.owner, *offset as usize);
                let ty = self.expr_against(value, declared, &Purpose::Local(name.to_string()));
                let id = match self.local(name) {
                    Some(id) => {
                        let existing = self.frame.locals[id as usize].declared;
                        if existing != declared && existing != Ty::ERROR && declared != Ty::ERROR {
                            let span = self.spans.token(*offset as usize);
                            let first = self.types.display(existing);
                            let offset_first = self.frame.locals[id as usize].offset;
                            self.report(
                                Diagnostic::error(
                                    Code::LOCAL_TYPE_CHANGED,
                                    span,
                                    format!(
                                        "`{name}` is already declared as {first}; a local keeps the type of its first declaration"
                                    ),
                                )
                                .with_label(self.spans.token(offset_first), "declared here")
                                .with_types(first, self.types.display(declared)),
                            );
                        }
                        id
                    }
                    None => self.declare(name, declared, *offset as usize, true),
                };
                self.assign_local(id, ty);
                ty
            }
            Target::Value(expr) => match &expr.node {
                Node::Var(name) if name.starts_with("@@") => {
                    let key = (self.frame.owner, name.to_string());
                    match self.constants.get(&key).copied() {
                        Some(declared) if self.frame.owner.is_some() => {
                            self.expr_against(value, declared, &Purpose::Ivar(name[1..].to_owned()))
                        }
                        _ => {
                            let ty = self.expr(value, None);
                            self.write_class_variable(name, ty, expr, value);
                            ty
                        }
                    }
                }
                Node::Var(name) if name.starts_with('@') => {
                    let span = self.spans.expr(expr);
                    let ivar = &name[1..];
                    let expected = self.ivar_type(ivar, span);
                    let ty = match expected {
                        Some(expected) => {
                            self.expr_against(value, expected, &Purpose::Ivar(ivar.to_owned()))
                        }
                        None => self.expr(value, None),
                    };
                    self.mark_ivar_assigned(ivar);
                    ty
                }
                Node::Var(name) if self.frame.namespace_body && is_constant(name) => {
                    let ty = self.expr(value, None);
                    let key = (self.frame.owner, name.to_string());
                    self.constants.insert(key, ty);
                    ty
                }
                Node::Var(name) => {
                    if name.as_str() == "self" {
                        return self.expr(value, None);
                    }
                    match self.local(name) {
                        Some(id) => {
                            let declared = self.frame.locals[id as usize].declared;
                            let ty = self.expr(value, Some(declared));
                            if !self.types.assignable(ty, declared) {
                                let span = self.spans.expr(value);
                                self.local_changed(id, span, ty);
                            }
                            self.assign_local(id, ty);
                            ty
                        }
                        None => {
                            let ty = self.expr(value, None);
                            let declared = self.local_type(name, ty, value, expr.offset as usize);
                            let id = self.declare(name, declared, expr.offset as usize, false);
                            if let (Node::Hash(_), Kind::Shape(..)) =
                                (&value.node, self.types.kind(ty))
                            {
                                let uniform = self.uniform_field_type(ty);
                                self.frame.locals[id as usize].dictionary = uniform;
                            }
                            self.assign_local(id, ty);
                            ty
                        }
                    }
                }
                Node::Index(receiver, selectors) => {
                    self.index_write(expr, receiver, selectors, Ty::ERROR, value, true)
                }
                Node::Member(receiver, name) => {
                    self.setter(expr, receiver, name, Ty::ERROR, value, true)
                }
                Node::Scope(..) => self.expr(value, None),
                _ => self.expr(value, None),
            },
            Target::Tuple(_) => {
                let ty = match &value.node {
                    Node::Array(items) => {
                        let types: Vec<Ty> =
                            items.iter().map(|item| self.expr(item, None)).collect();
                        self.types.tuple(types)
                    }
                    _ => self.expr(value, None),
                };
                self.bind_target(target, ty, true);
                ty
            }
        }
    }

    /// The type a local first assigned a value of type `ty` gets; `nil`,
    /// `[]` and `{}` need a declared type.
    fn local_type(&mut self, name: &str, ty: Ty, value: &Expr, offset: usize) -> Ty {
        if ty == Ty::ERROR {
            return Ty::ERROR;
        }
        if literal_without_type(value) && self.needs_context(ty) {
            let span = self.spans.token(offset);
            let what = match self.types.kind(ty) {
                Kind::Nil => "`nil`",
                Kind::EmptyHash => "`{}`",
                _ => "an empty array",
            };
            self.report(Diagnostic::error(
                Code::NEEDS_TYPE,
                span,
                format!(
                    "{what} does not say what `{name}` holds; declare it, as in `{name}: T = ...`"
                ),
            ));
            return Ty::ERROR;
        }
        ty
    }

    /// Whether a value's type leaves out what a local would hold.
    pub(super) fn needs_context(&self, ty: Ty) -> bool {
        match self.types.kind(ty) {
            Kind::Nil | Kind::EmptyHash | Kind::Never => true,
            Kind::Array(element) => *element == Ty::NEVER || self.needs_context_inner(*element),
            Kind::Hash(value) => *value == Ty::NEVER,
            _ => false,
        }
    }

    fn needs_context_inner(&self, ty: Ty) -> bool {
        match self.types.kind(ty) {
            Kind::Array(element) => *element == Ty::NEVER || self.needs_context_inner(*element),
            Kind::EmptyHash => true,
            _ => false,
        }
    }

    fn uniform_field_type(&mut self, shape: Ty) -> Option<Ty> {
        let Kind::Shape(fields, false) = self.types.kind(shape).clone() else {
            return None;
        };
        let first = fields.first()?.ty;
        fields
            .iter()
            .all(|f| f.ty == first && !f.optional)
            .then_some(first)
    }

    /// Reports an assignment that changes a local's type.
    pub(super) fn local_changed(&mut self, id: LocalId, span: Span, ty: Ty) {
        if ty == Ty::ERROR {
            return;
        }
        let local = &self.frame.locals[id as usize];
        let declared = local.declared;
        if declared == Ty::ERROR {
            return;
        }
        let name = local.name.clone();
        let first = self.spans.token(local.offset);
        let expected = self.types.display(declared);
        let found = self.types.display(ty);
        let how = if local.annotated {
            "declared"
        } else {
            "fixed by its first assignment"
        };
        self.report(
            Diagnostic::error(
                Code::LOCAL_TYPE_CHANGED,
                span,
                format!("`{name}` is {expected}, {how}; it cannot hold {found}"),
            )
            .with_label(first, "declared here")
            .with_types(expected, found),
        );
    }

    /// Binds a destructuring or block parameter target to a value of type `ty`.
    pub(super) fn bind_target(&mut self, target: &'a Target, ty: Ty, assignment: bool) {
        match target {
            Target::Value(expr) => match &expr.node {
                Node::Var(name) if !name.starts_with('@') => {
                    let id = match (assignment, self.local(name)) {
                        (true, Some(id)) => {
                            let declared = self.frame.locals[id as usize].declared;
                            if !self.types.assignable(ty, declared) {
                                let span = self.spans.expr(expr);
                                self.local_changed(id, span, ty);
                            }
                            id
                        }
                        _ => {
                            let declared = if assignment && self.needs_context(ty) {
                                Ty::ERROR
                            } else {
                                ty
                            };
                            self.declare(name, declared, expr.offset as usize, false)
                        }
                    };
                    self.assign_local(id, ty);
                }
                Node::Var(name) => {
                    let span = self.spans.expr(expr);
                    if let Some(expected) = self.ivar_type(&name[1..], span) {
                        if !self.types.assignable(ty, expected) {
                            self.mismatch(span, expected, ty, &Purpose::Ivar(name[1..].to_owned()));
                        }
                    }
                    self.mark_ivar_assigned(&name[1..]);
                }
                _ => {
                    self.expr(expr, None);
                }
            },
            Target::Typed(inner, annotation) => {
                let declared = self.annotation(
                    annotation,
                    self.frame.owner,
                    target.offset().unwrap_or(0) as usize,
                );
                if !self.types.assignable(ty, declared) {
                    let span = self.target_span(inner);
                    self.mismatch(span, declared, ty, &Purpose::Annotation);
                }
                match &**inner {
                    Target::Value(Expr {
                        node: Node::Var(name),
                        offset,
                        ..
                    }) if !name.starts_with('@') => {
                        let id = match (assignment, self.local(name)) {
                            (true, Some(id)) => id,
                            _ => self.declare(name, declared, *offset as usize, true),
                        };
                        self.assign_local(id, declared);
                    }
                    inner => self.bind_target(inner, declared, assignment),
                }
            }
            Target::Tuple(parts) => {
                let count = parts.len();
                for (index, (part, rest)) in parts.iter().enumerate() {
                    let Some(part) = part else {
                        continue;
                    };
                    let element = if *rest {
                        self.rest_of(ty, index, count)
                    } else {
                        self.element_of(ty, index)
                    };
                    self.bind_target(part, element, assignment);
                }
            }
        }
    }

    /// The type of element `index` when destructuring a value of type `ty`.
    pub(super) fn element_of(&mut self, ty: Ty, index: usize) -> Ty {
        match self.types.kind(ty).clone() {
            Kind::Tuple(items) => items.get(index).copied().unwrap_or(Ty::NIL),
            Kind::Array(element) => self.types.optional(element),
            Kind::Error | Kind::Any => ty,
            _ if index == 0 => ty,
            _ => Ty::NIL,
        }
    }

    fn rest_of(&mut self, ty: Ty, index: usize, count: usize) -> Ty {
        match self.types.kind(ty).clone() {
            Kind::Tuple(items) => {
                let end = items.len().saturating_sub(count - index - 1).max(index);
                let slice: Vec<Ty> = items
                    .get(index..end)
                    .map(<[Ty]>::to_vec)
                    .unwrap_or_default();
                let element = self.types.union(&slice);
                self.types.array(element)
            }
            Kind::Array(_) => ty,
            Kind::Error | Kind::Any => ty,
            _ => self.types.array(ty),
        }
    }

    // Instance variables -----------------------------------------------

    /// The declared type of an instance variable of the current class,
    /// reporting an undeclared one.
    pub(super) fn ivar_type(&mut self, name: &str, span: Span) -> Option<Ty> {
        let Some(ns) = self.frame.owner else {
            self.report(Diagnostic::error(
                Code::UNDECLARED_IVAR,
                span,
                format!("`@{name}` is outside any class; instance variables belong to a class"),
            ));
            return None;
        };
        let namespace = &self.program.namespaces[ns as usize];
        if let Some(ivar) = namespace.ivars.get(name) {
            return Some(ivar.ty);
        }
        let class = namespace.name.clone();
        self.report(Diagnostic::error(
            Code::UNDECLARED_IVAR,
            span,
            format!(
                "`@{name}` is not declared in `{class}`; declare it in the class body, as in `@{name}: T`"
            ),
        ));
        None
    }

    /// Assigns a class variable, which its class or module body declares
    /// as `@@name: T = value`; every assignment must keep that type.
    fn write_class_variable(&mut self, name: &str, ty: Ty, target: &Expr, value: &Expr) {
        let Some(ns) = self.frame.owner else {
            return;
        };
        let key = (Some(ns), name.to_owned());
        if let Some(declared) = self.constants.get(&key).copied() {
            if !self.types.assignable(ty, declared) {
                let span = self.spans.expr(value);
                self.mismatch(span, declared, ty, &Purpose::Ivar(name[1..].to_owned()));
            }
            return;
        }
        let span = self.spans.expr(target);
        let class = self.program.namespaces[ns as usize].name.clone();
        let mut diagnostic = Diagnostic::error(
            Code::UNDECLARED_IVAR,
            span,
            format!(
                "class variable `{name}` is not declared in `{class}`; declare it in the body, as in `{name}: T = value`"
            ),
        );
        let nameable = !self.needs_context(ty)
            && !matches!(self.types.kind(ty), Kind::Error | Kind::Never | Kind::Any);
        if nameable && self.frame.namespace_body && self.class_body_assignment(ns, target) {
            let written = self.types.display(ty);
            diagnostic = diagnostic.with_fix(Fix::insert(
                format!("declare `{name}: {written}`"),
                span.end,
                format!(": {written}"),
            ));
        }
        self.report(diagnostic);
        // Later reads and writes check against the first value's type.
        let ty = if nameable { ty } else { Ty::ERROR };
        self.constants.insert(key, ty);
    }

    /// Whether `target` is the target of a plain assignment that stands
    /// directly in the body of namespace `ns`, where a declaration can
    /// replace it.
    fn class_body_assignment(&self, ns: NsId, target: &Expr) -> bool {
        self.program.namespaces[ns as usize]
            .module
            .body
            .iter()
            .any(|stmt| match &stmt.node {
                Statement::Assign(Target::Value(assigned), "=", _) => {
                    assigned.offset == target.offset && stmt.offset == target.offset
                }
                _ => false,
            })
    }

    pub(super) fn mark_ivar_assigned(&mut self, name: &str) {
        if let Some(&(_, id)) = self.frame.initialize.iter().find(|(ivar, _)| ivar == name) {
            let state = self.frame.flow.get(id);
            self.frame.flow.set(
                id,
                VarState {
                    assigned: true,
                    ..state
                },
            );
        }
    }

    /// Assigns a parameter to an instance variable, as `def initialize(@name)` does.
    fn assign_ivar(&mut self, name: &str, ty: Ty, span: Span, accessor: bool) {
        if !accessor {
            if let Some(expected) = self.ivar_type(name, span) {
                if !self.types.assignable(ty, expected) {
                    self.mismatch(span, expected, ty, &Purpose::Ivar(name.to_owned()));
                }
            }
        }
        self.mark_ivar_assigned(name);
    }

    fn write_ivar(&mut self, name: &str, ty: Ty, span: Span, value: &Expr) {
        if let Some(expected) = self.ivar_type(name, span) {
            if !self.types.assignable(ty, expected) {
                let span = self.spans.expr(value);
                self.mismatch(span, expected, ty, &Purpose::Ivar(name.to_owned()));
            }
        }
        self.mark_ivar_assigned(name);
    }

    // Conditions and narrowing -----------------------------------------

    /// Checks a condition, which must be a `bool`, and the narrowings it implies.
    pub(super) fn condition(&mut self, expr: &'a Expr) -> Narrow {
        let (ty, narrow) = self.condition_parts(expr);
        if ty != Ty::BOOL && ty != Ty::ERROR && ty != Ty::NEVER {
            let span = self.spans.expr(expr);
            let found = self.types.display(ty);
            let mut diagnostic = Diagnostic::error(
                Code::CONDITION_NOT_BOOL,
                span,
                format!("a condition must be a bool, found {found}"),
            )
            .with_types("bool", found.clone());
            // A nil test keeps an optional value's meaning, unless it may be false.
            let optional = self.types.has_nil(ty) && ty != Ty::NIL;
            let rest = self.types.without_nil(ty);
            if optional
                && !self.types.assignable(Ty::BOOL, rest)
                && !self.types.members(rest).contains(&Ty::BOOL)
            {
                let text = &self.source[span.start..span.end];
                let replacement = if is_simple(expr) {
                    format!("{text} != nil")
                } else {
                    format!("({text}) != nil")
                };
                diagnostic = diagnostic.with_fix(Fix::replace(
                    format!("test for nil: `{replacement}`"),
                    span,
                    replacement,
                ));
            }
            self.report(diagnostic);
        }
        narrow
    }

    /// The type of a condition expression and what it narrows.
    fn condition_parts(&mut self, expr: &'a Expr) -> (Ty, Narrow) {
        match &expr.node {
            Node::Unary("!", inner) => {
                let (ty, narrow) = self.condition_parts(inner);
                self.require_bool(inner, ty, "!");
                (
                    Ty::BOOL,
                    Narrow {
                        then: narrow.otherwise,
                        otherwise: narrow.then,
                    },
                )
            }
            Node::Binary("&&", left, right) => {
                let (lt, ln) = self.condition_parts(left);
                self.require_bool(left, lt, "&&");
                let mark = self.frame.flow.mark();
                self.apply(&ln.then);
                let (rt, rn) = self.condition_parts(right);
                self.require_bool(right, rt, "&&");
                self.frame.flow.rollback(mark);
                let mut then = ln.then.clone();
                then.extend(rn.then.iter().copied());
                let otherwise =
                    self.join_narrowings(&ln.otherwise, &merge(&ln.then, &rn.otherwise));
                (Ty::BOOL, Narrow { then, otherwise })
            }
            Node::Binary("||", left, right) => {
                let (lt, ln) = self.condition_parts(left);
                self.require_bool(left, lt, "||");
                let mark = self.frame.flow.mark();
                self.apply(&ln.otherwise);
                let (rt, rn) = self.condition_parts(right);
                self.require_bool(right, rt, "||");
                self.frame.flow.rollback(mark);
                let mut otherwise = ln.otherwise.clone();
                otherwise.extend(rn.otherwise.iter().copied());
                let then = self.join_narrowings(&ln.then, &merge(&ln.otherwise, &rn.then));
                (Ty::BOOL, Narrow { then, otherwise })
            }
            Node::Binary(op @ ("==" | "!="), left, right) => {
                let ty = self.expr(expr, None);
                let subject = match (&left.node, &right.node) {
                    (_, Node::Literal(v)) if v.type_name() == "nil" => Some(&**left),
                    (Node::Literal(v), _) if v.type_name() == "nil" => Some(&**right),
                    _ => None,
                };
                let mut narrow = Narrow::default();
                if let Some(id) = subject.and_then(|subject| self.narrowable(subject)) {
                    let current = self.frame.flow.get(id).ty;
                    let without = self.types.without_nil(current);
                    let optional = self.types.has_nil(current) || current == Ty::ANY;
                    if !optional && current != Ty::ERROR && current != Ty::NEVER {
                        let span = self.spans.expr(expr);
                        let name = self.frame.locals[id as usize].name.clone();
                        let found = self.types.display(current);
                        let always = if *op == "==" { "false" } else { "true" };
                        self.report(Diagnostic::warning(
                            Code::UNREACHABLE_NARROWING,
                            span,
                            format!(
                                "`{name}` is {found}, never nil, so this test is always {always}"
                            ),
                        ));
                    }
                    let nil = if optional { Ty::NIL } else { current };
                    if *op == "==" {
                        narrow.then.push((id, nil));
                        narrow.otherwise.push((id, without));
                    } else {
                        narrow.then.push((id, without));
                        narrow.otherwise.push((id, nil));
                    }
                }
                (ty, narrow)
            }
            Node::Method(receiver, name, args, _)
                if name.as_str() == "is_type?" && args.len() == 1 =>
            {
                let ty = self.expr(expr, None);
                let mut narrow = Narrow::default();
                if let (Some(id), Some(tested)) =
                    (self.narrowable(receiver), self.type_atom(&args[0].value))
                {
                    let current = self.frame.flow.get(id).ty;
                    let then = self.intersect(current, tested);
                    if then == Ty::NEVER && current != Ty::NEVER {
                        let span = self.spans.expr(expr);
                        let found = self.types.display(current);
                        let wanted = self.types.display(tested);
                        self.report(Diagnostic::warning(
                            Code::CAST,
                            span,
                            format!("a value of type {found} is never {wanted}, so this test is always false"),
                        ));
                    }
                    let otherwise = if current == Ty::ANY {
                        Ty::ANY
                    } else {
                        self.types.without(current, tested)
                    };
                    narrow.then.push((id, then));
                    narrow.otherwise.push((id, otherwise));
                }
                (ty, narrow)
            }
            Node::Var(name) if name.as_str() == "block_given?" && self.local(name).is_none() => {
                let mut narrow = Narrow::default();
                if let Some(id) = self.frame.block_given {
                    narrow.then.push((id, Ty::BOOL));
                }
                (Ty::BOOL, narrow)
            }
            _ => (self.expr(expr, None), Narrow::default()),
        }
    }

    fn require_bool(&mut self, expr: &Expr, ty: Ty, op: &str) {
        if ty == Ty::BOOL || ty == Ty::ERROR || ty == Ty::NEVER {
            return;
        }
        let span = self.spans.expr(expr);
        let found = self.types.display(ty);
        self.report(
            Diagnostic::error(
                Code::LOGICAL_NOT_BOOL,
                span,
                format!("`{op}` takes bool operands, found {found}"),
            )
            .with_types("bool", found),
        );
    }

    /// Narrowings that hold on either of two paths.
    fn join_narrowings(&mut self, a: &[(LocalId, Ty)], b: &[(LocalId, Ty)]) -> Vec<(LocalId, Ty)> {
        let mut joined = Vec::new();
        for &(id, ty) in a {
            if let Some(&(_, other)) = b.iter().rev().find(|(other, _)| *other == id) {
                joined.push((id, self.types.union(&[ty, other])));
            }
        }
        joined
    }

    /// The local an expression reads, if narrowing may apply to it.
    pub(super) fn narrowable(&self, expr: &Expr) -> Option<LocalId> {
        match &expr.node {
            Node::Var(name) => {
                let id = self.local(name)?;
                self.frame.flow.get(id).assigned.then_some(id)
            }
            _ => None,
        }
    }

    /// The type an `is_type?` atom such as `:int` tests for.
    pub(super) fn type_atom(&mut self, atom: &Expr) -> Option<Ty> {
        let Node::Literal(value) = &atom.node else {
            return None;
        };
        let name = super::symbol_text(value)?;
        let name = name.as_str();
        let (name, nullable) = match name.strip_suffix('?') {
            Some(base) => (base, true),
            None => (name, false),
        };
        let ty = match name {
            "nil" => Ty::NIL,
            "bool" => Ty::BOOL,
            "int" => Ty::INT,
            "float" => Ty::FLOAT,
            "number" => Ty::NUMBER,
            "string" => Ty::STRING,
            "symbol" => Ty::SYMBOL,
            "array" => self.types.array(Ty::ANY),
            "hash" | "object" => self.types.hash(Ty::ANY),
            "range" => Ty::RANGE,
            "duration" => Ty::DURATION,
            "time" => Ty::TIME,
            "money" => Ty::MONEY,
            other => {
                let base = other.rsplit('.').next().unwrap_or(other);
                if let Some(&id) = self.program.enum_names.get(base) {
                    self.types.intern(Kind::EnumValue(id))
                } else {
                    let ns = self
                        .program
                        .namespaces
                        .iter()
                        .position(|ns| ns.is_class && ns.module.name.as_str() == base)?;
                    self.types.intern(Kind::Instance(ns as NsId))
                }
            }
        };
        Some(if nullable {
            self.types.optional(ty)
        } else {
            ty
        })
    }

    /// The part of `current` that `tested` describes: the alternatives it
    /// covers, or the tested type itself when `current` is `any`.
    fn intersect(&mut self, current: Ty, tested: Ty) -> Ty {
        if current == Ty::ANY || current == Ty::ERROR {
            return tested;
        }
        let tested_members = self.types.members(tested);
        let mut kept = Vec::new();
        for member in self.types.members(current) {
            let matches = tested_members.iter().any(|&t| {
                self.types.assignable(member, t)
                    || same_base(self.types.kind(member), self.types.kind(t))
            });
            if matches {
                kept.push(member);
            }
        }
        if kept.is_empty() {
            Ty::NEVER
        } else {
            self.types.union(&kept)
        }
    }

    /// The function a frame is checking, by id, for messages.
    pub(super) fn current_function(&self) -> &str {
        &self.frame.name
    }
}

/// Whether an expression is `nil`, `[]` or `{}`, or an array of empty
/// arrays, whose type says nothing about what it will hold.
fn literal_without_type(expr: &Expr) -> bool {
    match &expr.node {
        Node::Literal(value) => value.type_name() == "nil",
        Node::Hash(entries) => entries.is_empty(),
        Node::Array(items) => items.iter().all(literal_without_type),
        _ => false,
    }
}

fn merge(a: &[(LocalId, Ty)], b: &[(LocalId, Ty)]) -> Vec<(LocalId, Ty)> {
    let mut merged = a.to_vec();
    merged.extend_from_slice(b);
    merged
}

fn same_base(a: &Kind, b: &Kind) -> bool {
    matches!(
        (a, b),
        (Kind::Array(_) | Kind::Tuple(_), Kind::Array(_))
            | (
                Kind::Hash(_) | Kind::Shape(..) | Kind::EmptyHash,
                Kind::Hash(_)
            )
    )
}

pub(super) fn is_constant(name: &str) -> bool {
    name.chars().next().is_some_and(char::is_uppercase)
}

fn is_simple(expr: &Expr) -> bool {
    matches!(
        expr.node,
        Node::Var(_) | Node::Member(..) | Node::Method(..) | Node::Index(..) | Node::Call(..)
    )
}

fn target_expr(target: &Target) -> Option<&Expr> {
    match target {
        Target::Value(expr) => Some(expr),
        Target::Typed(inner, _) => target_expr(inner),
        Target::Tuple(_) => None,
    }
}

/// The names of the locals a loop body may assign, including in nested blocks.
pub(super) fn assigned_names(body: &[Stmt], names: &mut Vec<String>) {
    for stmt in body {
        match &stmt.node {
            Statement::Assign(target, _, value) => {
                target_names(target, names);
                expr_assigned(value, names);
            }
            Statement::If(branches, alternate, _) => {
                for (condition, body) in branches.iter() {
                    expr_assigned(condition, names);
                    assigned_names(body, names);
                }
                assigned_names(alternate, names);
            }
            Statement::While(condition, body, _) => {
                expr_assigned(condition, names);
                assigned_names(body, names);
            }
            Statement::For(target, iterable, body) => {
                target_names(target, names);
                expr_assigned(iterable, names);
                assigned_names(body, names);
            }
            Statement::Expr(expr) => expr_assigned(expr, names),
            Statement::Return(Some(expr))
            | Statement::Break(Some(expr))
            | Statement::Next(Some(expr)) => expr_assigned(expr, names),
            _ => (),
        }
    }
}

fn target_names(target: &Target, names: &mut Vec<String>) {
    match target {
        Target::Value(Expr {
            node: Node::Var(name),
            ..
        }) => names.push(name.to_string()),
        Target::Typed(inner, _) => target_names(inner, names),
        Target::Tuple(parts) => {
            for (part, _) in parts.iter() {
                if let Some(part) = part {
                    target_names(part, names);
                }
            }
        }
        _ => (),
    }
}

fn expr_assigned(expr: &Expr, names: &mut Vec<String>) {
    let mut pending = vec![expr];
    while let Some(expr) = pending.pop() {
        match &expr.node {
            Node::Compound(stmt) => assigned_names(std::slice::from_ref(&**stmt), names),
            Node::Try(attempt) => {
                assigned_names(&attempt.body, names);
                assigned_names(&attempt.alternate, names);
                assigned_names(&attempt.ensure, names);
                for rescue in attempt.rescues.iter() {
                    assigned_names(&rescue.body, names);
                }
            }
            Node::BlockCall(call, block) => {
                pending.push(call);
                assigned_names(&block.body, names);
            }
            Node::Conditional(branches, alternate) => {
                for (c, v) in branches.iter() {
                    pending.push(c);
                    pending.push(v);
                }
                pending.push(alternate);
            }
            Node::Case(subject, whens, alternate) => {
                pending.extend(subject.as_deref());
                for when in whens.iter() {
                    pending.push(&when.result);
                }
                pending.extend(alternate.as_deref());
            }
            Node::Binary(_, l, r) => {
                pending.push(l);
                pending.push(r);
            }
            Node::Unary(_, v) => pending.push(v),
            Node::Call(_, args, _) => pending.extend(args.iter().map(|a| &a.value)),
            Node::Method(recv, _, args, _) | Node::SafeMethod(recv, _, args, _) => {
                pending.push(recv);
                pending.extend(args.iter().map(|a| &a.value));
            }
            Node::Array(items) => pending.extend(items.iter()),
            Node::Hash(entries) => pending.extend(entries.iter().map(|(_, v)| v)),
            _ => (),
        }
    }
}

/// Why a value is checked against a type, for messages.
#[derive(Clone)]
pub(crate) enum Purpose {
    Result,
    BlockResult,
    Local(String),
    Ivar(String),
    Argument {
        index: usize,
        name: String,
        function: String,
    },
    Keyword {
        name: String,
        function: String,
    },
    Element,
    Field(String),
    Annotation,
    Yield(usize),
    Operand,
    /// A `break` out of a block, which returns from this function.
    Break(String),
}

impl<'a> Checker<'a> {
    /// What a position expects, for messages: "`f` returns int".
    pub(super) fn purpose_text(&self, purpose: &Purpose, expected_text: &str) -> String {
        match purpose {
            Purpose::Result => format!("`{}` returns {expected_text}", self.current_function()),
            Purpose::BlockResult => format!("the block returns {expected_text}"),
            Purpose::Local(name) => format!("`{name}` is {expected_text}"),
            Purpose::Ivar(name) => format!("`@{name}` is {expected_text}"),
            Purpose::Argument {
                index,
                name,
                function,
            } => {
                format!(
                    "argument {} (`{name}`) of `{function}` is {expected_text}",
                    index + 1
                )
            }
            Purpose::Keyword { name, function } => {
                format!("keyword `{name}:` of `{function}` is {expected_text}")
            }
            Purpose::Element => format!("elements here are {expected_text}"),
            Purpose::Field(name) => format!("field `{name}` is {expected_text}"),
            Purpose::Annotation => format!("the annotation says {expected_text}"),
            Purpose::Yield(index) => format!("block argument {} is {expected_text}", index + 1),
            Purpose::Operand => format!("the operand must be {expected_text}"),
            Purpose::Break(function) => format!(
                "a `break` out of the block returns from `{function}`, which returns {expected_text}"
            ),
        }
    }

    /// Reports that a value of type `found` is not assignable to `expected`.
    pub(super) fn mismatch(&mut self, span: Span, expected: Ty, found: Ty, purpose: &Purpose) {
        if expected == Ty::ERROR || found == Ty::ERROR {
            return;
        }
        let expected_text = self.types.display(expected);
        let found_text = self.types.display(found);
        let what = self.purpose_text(purpose, &expected_text);
        let mut diagnostic = Diagnostic::error(
            Code::TYPE_MISMATCH,
            span,
            format!("{what}, found {found_text}"),
        )
        .with_types(expected_text.clone(), found_text);
        if found == Ty::ANY {
            diagnostic.code = Code::ANY_USE;
            diagnostic.message = format!(
                "{what}, found any; narrow the value first with `is_type?`, `.as({expected_text})` or `JSON.parse_as`"
            );
        } else if self.types.has_nil(found) {
            let without = self.types.without_nil(found);
            if without != Ty::NEVER && self.types.assignable(without, expected) {
                diagnostic.code = Code::OPTIONAL_USE;
                diagnostic.message =
                    format!("{what}, but this value may be nil; test it with `!= nil` first");
            }
        }
        self.report(diagnostic);
    }
}
