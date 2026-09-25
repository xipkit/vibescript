//! Finds the places a repair edits in a parsed source: its functions with
//! their annotations, and the expressions and statements at a span.

use vibescript::surface::syntax::*;

/// A function or method, with where it sits.
pub(crate) struct DefSite<'t> {
    pub def: &'t Def,
    /// The enclosing classes and modules, joined by `.`, or empty.
    pub owner: String,
    /// From `def` through its `end`.
    pub span: Span,
}

impl DefSite<'_> {
    /// A name that tells functions apart across a migration: owner and name.
    pub fn key(&self) -> String {
        format!("{}#{}", self.owner, self.def.name)
    }
}

/// Every function in source order, including those nested in classes,
/// modules and other bodies.
pub(crate) fn defs(tree: &Tree) -> Vec<DefSite<'_>> {
    let mut out = Vec::new();
    for stmt in &tree.body {
        stmt_defs(tree, stmt, "", &mut out);
    }
    out
}

fn stmt_defs<'t>(tree: &'t Tree, stmt: &'t Stmt, owner: &str, out: &mut Vec<DefSite<'t>>) {
    match &stmt.kind {
        StmtKind::Def(def) => def_site(tree, def, owner, out),
        StmtKind::Class(class) => class_defs(tree, class, owner, out),
        _ => each_child_stmt(stmt, &mut |inner| stmt_defs(tree, inner, owner, out)),
    }
}

fn def_site<'t>(tree: &'t Tree, def: &'t Def, owner: &str, out: &mut Vec<DefSite<'t>>) {
    out.push(DefSite {
        def,
        owner: owner.to_owned(),
        span: Span {
            start: tree.tokens[def.keyword].start,
            end: tree.tokens[def.end].end,
        },
    });
    for stmt in def_bodies(def) {
        stmt_defs(tree, stmt, owner, out);
    }
}

fn class_defs<'t>(tree: &'t Tree, class: &'t Class, owner: &str, out: &mut Vec<DefSite<'t>>) {
    let owner = if owner.is_empty() {
        class.name.clone()
    } else {
        format!("{owner}.{}", class.name)
    };
    for member in &class.members {
        match member {
            Member::Def(def) => def_site(tree, def, &owner, out),
            Member::Class(inner) => class_defs(tree, inner, &owner, out),
            Member::Stmt(stmt) => stmt_defs(tree, stmt, &owner, out),
            _ => (),
        }
    }
}

/// Every statement of a function's body and rescue clauses.
pub(crate) fn def_bodies(def: &Def) -> impl Iterator<Item = &Stmt> {
    let rescued = def.rescue.iter().flat_map(|rescued| {
        rescued
            .rescues
            .iter()
            .flat_map(|clause| clause.body.iter())
            .chain(rescued.alternate.iter().flatten())
            .chain(rescued.ensure.iter().flatten())
    });
    def.body.iter().chain(rescued)
}

/// Visits the statements directly inside a statement's bodies, such as an
/// `if`'s branches or a block's body, but not those of a nested `def`.
fn each_child_stmt<'t>(stmt: &'t Stmt, visit: &mut dyn FnMut(&'t Stmt)) {
    let mut exprs = Vec::new();
    match &stmt.kind {
        StmtKind::Expr(expr) => exprs.push(expr),
        StmtKind::Assign(assign) => exprs.extend(&assign.values),
        StmtKind::If(node) => {
            for (condition, body) in &node.branches {
                exprs.push(condition);
                body.iter().for_each(&mut *visit);
            }
            if let Some((_, body)) = &node.alternate {
                body.iter().for_each(&mut *visit);
            }
        }
        StmtKind::While(node) => {
            exprs.push(&node.condition);
            node.body.iter().for_each(&mut *visit);
        }
        StmtKind::For(node) => {
            exprs.push(&node.iterable);
            node.body.iter().for_each(&mut *visit);
        }
        StmtKind::Modifier(node) => {
            visit(&node.body);
            exprs.push(&node.condition);
        }
        StmtKind::Flow(_, Some(value)) => exprs.push(value),
        StmtKind::Raise(_, value, message) => exprs.extend(value.iter().chain(message)),
        _ => (),
    }
    for expr in exprs {
        each_expr(expr, &mut |node| {
            if let Node::Stmt(inner) = node {
                visit(inner);
                return false;
            }
            true
        });
    }
}

/// A node a walk reaches.
#[derive(Clone, Copy)]
pub(crate) enum Node<'t> {
    Stmt(&'t Stmt),
    Expr(&'t Expr),
}

/// Walks an expression in source order, visiting each expression and each
/// statement of a block or `begin` body. `visit` returns whether to descend.
pub(crate) fn each_expr<'t>(expr: &'t Expr, visit: &mut dyn FnMut(Node<'t>) -> bool) {
    if !visit(Node::Expr(expr)) {
        return;
    }
    let stmts = |body: &'t [Stmt], visit: &mut dyn FnMut(Node<'t>) -> bool| {
        for stmt in body {
            each_stmt(stmt, visit);
        }
    };
    match &expr.kind {
        ExprKind::Template(parts) => {
            for part in parts.iter().flatten() {
                each_expr(part, visit);
            }
        }
        ExprKind::Array(items) => {
            for item in items {
                each_expr(item, visit);
            }
        }
        ExprKind::Hash(entries) => {
            for entry in entries {
                each_expr(&entry.value, visit);
            }
        }
        ExprKind::Unary(_, operand) | ExprKind::Group(_, operand, _) => each_expr(operand, visit),
        ExprKind::Binary(_, left, right) | ExprKind::Rescue(left, _, right) => {
            each_expr(left, visit);
            each_expr(right, visit);
        }
        ExprKind::Range(left, _, right) => {
            for operand in left.iter().chain(right) {
                each_expr(operand, visit);
            }
        }
        ExprKind::Ternary(condition, _, yes, no) => {
            each_expr(condition, visit);
            each_expr(yes, visit);
            each_expr(no, visit);
        }
        ExprKind::Call(call) => {
            if let Some(receiver) = &call.receiver {
                each_expr(receiver, visit);
            }
            for arg in call.args.iter().flat_map(|args| &args.items) {
                each_expr(&arg.value, visit);
            }
            if let Some(block) = &call.block {
                stmts(&block.body, visit);
            }
        }
        ExprKind::Computed(callee, args) => {
            each_expr(callee, visit);
            for arg in &args.items {
                each_expr(&arg.value, visit);
            }
        }
        ExprKind::BlockCall(callee, block) => {
            each_expr(callee, visit);
            stmts(&block.body, visit);
        }
        ExprKind::Index(receiver, _, selectors, _) => {
            each_expr(receiver, visit);
            for selector in selectors {
                each_expr(selector, visit);
            }
        }
        ExprKind::Yield(_, args) => {
            for arg in args.iter().flat_map(|args| &args.items) {
                each_expr(&arg.value, visit);
            }
        }
        ExprKind::If(node) => {
            for (condition, body) in &node.branches {
                each_expr(condition, visit);
                stmts(body, visit);
            }
            if let Some((_, body)) = &node.alternate {
                stmts(body, visit);
            }
        }
        ExprKind::Case(node) => {
            if let Some(subject) = &node.subject {
                each_expr(subject, visit);
            }
            for when in &node.whens {
                for (value, _) in &when.values {
                    each_expr(value, visit);
                }
                each_expr(&when.result, visit);
            }
            if let Some((_, alternate)) = &node.alternate {
                each_expr(alternate, visit);
            }
        }
        ExprKind::Loop(stmt) => each_stmt(stmt, visit),
        ExprKind::Begin(node) => {
            stmts(&node.body, visit);
            for clause in &node.rescued.rescues {
                stmts(&clause.body, visit);
            }
            for body in node.rescued.alternate.iter().chain(&node.rescued.ensure) {
                stmts(body, visit);
            }
        }
        _ => (),
    }
}

/// Walks a statement and everything inside it in source order, including
/// nested functions and classes.
pub(crate) fn each_stmt<'t>(stmt: &'t Stmt, visit: &mut dyn FnMut(Node<'t>) -> bool) {
    if !visit(Node::Stmt(stmt)) {
        return;
    }
    let stmts = |body: &'t [Stmt], visit: &mut dyn FnMut(Node<'t>) -> bool| {
        for stmt in body {
            each_stmt(stmt, visit);
        }
    };
    match &stmt.kind {
        StmtKind::Expr(expr) => each_expr(expr, visit),
        StmtKind::Assign(assign) => {
            for target in &assign.targets {
                each_target(target, visit);
            }
            for value in &assign.values {
                each_expr(value, visit);
            }
        }
        StmtKind::If(node) => {
            for (condition, body) in &node.branches {
                each_expr(condition, visit);
                stmts(body, visit);
            }
            if let Some((_, body)) = &node.alternate {
                stmts(body, visit);
            }
        }
        StmtKind::While(node) => {
            each_expr(&node.condition, visit);
            stmts(&node.body, visit);
        }
        StmtKind::For(node) => {
            each_target(&node.target, visit);
            each_expr(&node.iterable, visit);
            stmts(&node.body, visit);
        }
        StmtKind::Modifier(node) => {
            each_stmt(&node.body, visit);
            each_expr(&node.condition, visit);
        }
        StmtKind::Flow(_, Some(value)) => each_expr(value, visit),
        StmtKind::Raise(_, value, message) => {
            for expr in value.iter().chain(message) {
                each_expr(expr, visit);
            }
        }
        StmtKind::Def(def) => each_def(def, visit),
        StmtKind::Class(class) => each_class(class, visit),
        _ => (),
    }
}

fn each_def<'t>(def: &'t Def, visit: &mut dyn FnMut(Node<'t>) -> bool) {
    for param in &def.params {
        if let Some(default) = &param.default {
            each_expr(default, visit);
        }
    }
    for stmt in def_bodies(def) {
        each_stmt(stmt, visit);
    }
}

fn each_class<'t>(class: &'t Class, visit: &mut dyn FnMut(Node<'t>) -> bool) {
    for member in &class.members {
        match member {
            Member::Def(def) => each_def(def, visit),
            Member::Class(inner) => each_class(inner, visit),
            Member::Stmt(stmt) => each_stmt(stmt, visit),
            Member::Ivar(_, _, Some(value)) => each_expr(value, visit),
            _ => (),
        }
    }
}

fn each_target<'t>(target: &'t Target, visit: &mut dyn FnMut(Node<'t>) -> bool) {
    match target {
        Target::Expr(expr) => each_expr(expr, visit),
        Target::Splat(_, Some(inner)) | Target::Typed(inner, _) => each_target(inner, visit),
        Target::Group(_, parts) => {
            for part in parts {
                each_target(part, visit);
            }
        }
        Target::Splat(_, None) => (),
    }
}

/// Walks a whole tree.
pub(crate) fn each_node<'t>(tree: &'t Tree, visit: &mut dyn FnMut(Node<'t>) -> bool) {
    for stmt in &tree.body {
        each_stmt(stmt, visit);
    }
}

/// The innermost expression whose span is exactly `start..end`, or else the
/// smallest one that contains it.
pub(crate) fn expr_at(tree: &Tree, start: usize, end: usize) -> Option<&Expr> {
    let mut exact: Option<&Expr> = None;
    let mut containing: Option<&Expr> = None;
    each_node(tree, &mut |node| {
        let Node::Expr(expr) = node else {
            return true;
        };
        if expr.span.start > end || expr.span.end < start {
            return false;
        }
        if expr.span.start == start && expr.span.end == end {
            exact = Some(expr);
        }
        if expr.span.start <= start
            && expr.span.end >= end
            && containing.is_none_or(|kept| {
                kept.span.end - kept.span.start >= expr.span.end - expr.span.start
            })
        {
            containing = Some(expr);
        }
        true
    });
    exact.or(containing)
}

/// The innermost function whose span contains `offset`.
pub(crate) fn def_containing<'s, 't>(
    sites: &'s [DefSite<'t>],
    offset: usize,
) -> Option<&'s DefSite<'t>> {
    sites
        .iter()
        .filter(|site| site.span.start <= offset && offset < site.span.end)
        .min_by_key(|site| site.span.end - site.span.start)
}
