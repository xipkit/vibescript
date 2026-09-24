//! Statements that can never run, as the Go reference's `vibes analyze`
//! linter reports them: its scopes, positions and terminator rules.

use super::Unreachable;
use crate::{
    Position, Result,
    source::Source,
    syntax::{self, Block, Expr, Node, Statement, Stmt, Target, Try, modules::Module},
};
use std::{collections::HashMap, sync::Arc};

/// Parses source and reports its unreachable statements, sorted by position
/// and then by scope.
pub(super) fn unreachable(text: &str) -> Result<Vec<Unreachable>> {
    let parsed = syntax::parse(text, &())
        .map_err(|error| crate::source::parse_error(text, None, error, &()))?;
    let source = Source::compile(text, &())?;
    let main = &parsed.functions[0];
    let mut lint = Lint {
        source: &source,
        interpolations: &parsed.interpolations,
        reports: Vec::new(),
        tasks: Vec::new(),
        results: Vec::new(),
    };
    lint.script(&main.body, &parsed.modules);
    for definition in &parsed.functions[1..] {
        lint.body(&Arc::from(definition.name.as_str()), &definition.body);
    }
    let mut namespaces = Vec::new();
    qualified(&parsed.modules, "", &mut namespaces);
    for (name, module) in namespaces {
        lint.body(&Arc::from(format!("{name}.<class body>")), &module.body);
        for definition in last_definitions(&module.instance_methods) {
            let scope = format!("{name}#{}", definition.name.as_str());
            lint.body(&Arc::from(scope), &definition.body);
        }
        for definition in last_definitions(&module.methods) {
            let scope = format!("{name}.{}", definition.name.as_str());
            lint.body(&Arc::from(scope), &definition.body);
        }
    }
    let mut unreachable = lint.reports;
    unreachable.sort_by(|a, b| {
        (a.position.line, a.position.column, &a.function).cmp(&(
            b.position.line,
            b.position.column,
            &b.function,
        ))
    });
    Ok(unreachable)
}

/// Collects namespaces with `Outer::Inner` names, as the reference registers them.
fn qualified<'a>(modules: &'a [Module], prefix: &str, out: &mut Vec<(String, &'a Module)>) {
    for module in modules {
        let name = if prefix.is_empty() {
            module.name.as_str().to_owned()
        } else {
            format!("{prefix}::{}", module.name.as_str())
        };
        qualified(&module.modules, &name, out);
        out.push((name, module));
    }
}

/// Keeps the last definition of each method name, as a later definition replaces an earlier one.
fn last_definitions<T>(methods: &[(syntax::Definition, T)]) -> Vec<&syntax::Definition> {
    let mut last = HashMap::new();
    for (index, (definition, _)) in methods.iter().enumerate() {
        last.insert(definition.name.as_str(), index);
    }
    methods
        .iter()
        .enumerate()
        .filter(|(index, (definition, _))| last[definition.name.as_str()] == *index)
        .map(|(_, (definition, _))| definition)
        .collect()
}

/// The position the reference reports for a statement.
fn statement_offset(text: &str, stmt: &Stmt) -> u32 {
    match &stmt.node {
        Statement::If(_, _, Some(keyword)) | Statement::While(_, _, Some(keyword)) => *keyword,
        Statement::Assign(target, _, _) => {
            let mut target = target;
            loop {
                target = match target {
                    Target::Value(expr) => return expression_offset(text, expr),
                    Target::Typed(target, _) => target,
                    Target::Tuple(parts) => match parts.iter().find_map(|(t, _)| t.as_ref()) {
                        Some(target) => target,
                        None => return stmt.offset,
                    },
                };
            }
        }
        Statement::Expr(expr) => {
            // A compound statement continued by an operator after its `end`
            // keeps the compound statement's position.
            let mut current = expr;
            loop {
                current = match &current.node {
                    Node::Compound(_) => break,
                    Node::Try(attempt) if !attempt.modifier => break,
                    Node::Member(receiver, _)
                    | Node::SafeMember(receiver, _)
                    | Node::Method(receiver, _, _, _)
                    | Node::SafeMethod(receiver, _, _, _)
                    | Node::Index(receiver, _)
                    | Node::Scope(receiver, _, _)
                    | Node::ComputedCall(receiver, _)
                    | Node::BlockCall(receiver, _)
                    | Node::Binary(_, receiver, _)
                    | Node::Range(Some(receiver), _, _) => receiver,
                    Node::Conditional(branches, _) if !branches.is_empty() => &branches[0].0,
                    _ => return expression_offset(text, expr),
                };
            }
            if current.offset == stmt.offset {
                stmt.offset
            } else {
                expression_offset(text, expr)
            }
        }
        _ => stmt.offset,
    }
}

/// The position the reference gives an expression. The syntax tree keeps the
/// first character of an operator, while the reference's lexer positions most
/// multi-character operators at their last character (but `<=>` and `===` at
/// their first), gives a negated numeric literal its digits' position, and
/// places `value rescue fallback` at `rescue`. Member access, calls and
/// blocks take their receiver's position.
fn expression_offset(text: &str, expr: &Expr) -> u32 {
    let mut current = expr;
    loop {
        let offset = current.offset;
        let operator = |op: &str| {
            let at = offset as usize;
            if !matches!(op, "<=>" | "===")
                && text.get(at..).is_some_and(|rest| rest.starts_with(op))
            {
                offset + op.len() as u32 - 1
            } else {
                offset
            }
        };
        current = match &current.node {
            Node::Binary(op, _, _) => return operator(op),
            Node::Range(Some(_), _, exclusive) | Node::Range(None, _, exclusive) => {
                return operator(if *exclusive { "..." } else { ".." });
            }
            Node::Try(attempt) if attempt.modifier => {
                return attempt
                    .rescues
                    .first()
                    .map_or(offset, |rescue| rescue.offset);
            }
            Node::Integer(_) | Node::BigInteger(..) | Node::Literal(_)
                if text.as_bytes().get(offset as usize) == Some(&b'-') =>
            {
                return offset + 1;
            }
            Node::Unary("-", operand)
                if matches!(
                    operand.node,
                    Node::Integer(_) | Node::BigInteger(..) | Node::Literal(_)
                ) =>
            {
                return operand.offset;
            }
            Node::Member(receiver, _)
            | Node::SafeMember(receiver, _)
            | Node::Method(receiver, _, _, _)
            | Node::SafeMethod(receiver, _, _, _)
            | Node::Scope(receiver, _, _)
            | Node::ComputedCall(receiver, _)
            | Node::BlockCall(receiver, _)
                if receiver.offset == offset =>
            {
                receiver
            }
            _ => return offset,
        };
    }
}

/// How pending results combine into a statement's termination.
enum Combine {
    /// Every body terminates.
    All(usize),
    /// A begin block: its body, optional else, non-empty rescues and optional ensure.
    Try {
        alternate: bool,
        rescues: usize,
        ensure: bool,
    },
}

enum Task<'a> {
    /// Walks a statement list, then pushes whether it terminates.
    Body(Arc<str>, &'a [Stmt]),
    /// Continues a statement list after the statement before `next`.
    Resume(Arc<str>, &'a [Stmt], usize),
    /// Evaluates one statement and pushes whether it terminates.
    Stmt(Arc<str>, &'a Stmt),
    /// Evaluates a begin block and pushes whether it terminates.
    Try(Arc<str>, &'a Try),
    /// Walks an expression for nested bodies and blocks; pushes nothing.
    Expr(Arc<str>, &'a Expr),
    Target(Arc<str>, &'a Target),
    Block(Arc<str>, &'a Block),
    Push(bool),
    Discard,
    Combine(Combine),
}

/// An explicit-stack walk, since statements nest as deeply as the syntax limit.
struct Lint<'a> {
    source: &'a Source,
    interpolations: &'a [(u32, u32)],
    reports: Vec<Unreachable>,
    tasks: Vec<Task<'a>>,
    results: Vec<bool>,
}

impl<'a> Lint<'a> {
    fn body(&mut self, scope: &Arc<str>, stmts: &'a [Stmt]) -> bool {
        self.tasks.push(Task::Body(scope.clone(), stmts));
        self.run()
    }

    /// Walks top-level code as the reference's `<script>` entrypoint sees it:
    /// executable statements, plus class declarations with bodies once any
    /// executable statement exists.
    fn script(&mut self, top: &'a [Stmt], modules: &'a [Module]) {
        if top
            .iter()
            .all(|stmt| matches!(stmt.node, Statement::Module(_)))
        {
            return;
        }
        let scope: Arc<str> = Arc::from("<script>");
        let mut terminated = false;
        for stmt in top {
            let class = match &stmt.node {
                Statement::Module(name) => {
                    let Some(module) = modules.iter().find(|m| m.name == *name) else {
                        continue;
                    };
                    if module.body.is_empty() {
                        continue;
                    }
                    Some(module)
                }
                _ => None,
            };
            if terminated {
                self.report(&scope, statement_offset(self.source.text(), stmt));
                continue;
            }
            terminated = match class {
                Some(module) => {
                    self.body(&scope, &module.body);
                    false
                }
                None => {
                    self.tasks.push(Task::Stmt(scope.clone(), stmt));
                    self.run()
                }
            };
        }
    }

    fn report(&mut self, scope: &Arc<str>, offset: u32) {
        let position = self.position(offset);
        self.reports.push(Unreachable {
            function: scope.to_string(),
            position,
        });
    }

    /// The reference parses each interpolation's content separately, after
    /// trimming surrounding whitespace, so a position inside one counts lines
    /// and columns from the content's first non-space character.
    fn position(&self, offset: u32) -> Position {
        let innermost = self
            .interpolations
            .iter()
            .filter(|(start, end)| *start <= offset && offset < *end)
            .max_by_key(|(start, _)| *start);
        let Some(&(start, _)) = innermost else {
            return self.source.position(offset);
        };
        let text = self
            .source
            .text()
            .get(start as usize..offset as usize)
            .unwrap_or_default();
        let mut position = Position { line: 1, column: 1 };
        for ch in text.trim_start_matches(char::is_whitespace).chars() {
            if ch == '\n' {
                position.line += 1;
                position.column = 1;
            } else {
                position.column += 1;
            }
        }
        position
    }

    fn run(&mut self) -> bool {
        while let Some(task) = self.tasks.pop() {
            match task {
                Task::Body(scope, stmts) => self.tasks.push(Task::Resume(scope, stmts, 0)),
                Task::Resume(scope, stmts, next) => {
                    if next > 0 && self.results.pop() == Some(true) {
                        for stmt in &stmts[next..] {
                            self.report(&scope, statement_offset(self.source.text(), stmt));
                        }
                        self.results.push(true);
                    } else if next == stmts.len() {
                        self.results.push(false);
                    } else {
                        self.tasks
                            .push(Task::Resume(scope.clone(), stmts, next + 1));
                        self.tasks.push(Task::Stmt(scope, &stmts[next]));
                    }
                }
                Task::Stmt(scope, stmt) => self.statement(scope, stmt),
                Task::Try(scope, attempt) => self.attempt(scope, attempt),
                Task::Expr(scope, expr) => self.expression(scope, expr),
                Task::Target(scope, target) => match target {
                    Target::Value(expr) => self.tasks.push(Task::Expr(scope, expr)),
                    Target::Typed(target, _) => self.tasks.push(Task::Target(scope, target)),
                    Target::Tuple(parts) => {
                        for target in parts.iter().filter_map(|(target, _)| target.as_ref()) {
                            self.tasks.push(Task::Target(scope.clone(), target));
                        }
                    }
                },
                Task::Block(scope, block) => {
                    let position = self.position(block.offset);
                    let scope: Arc<str> = Arc::from(format!(
                        "{scope} block at {}:{}",
                        position.line, position.column
                    ));
                    self.tasks.push(Task::Discard);
                    self.tasks.push(Task::Body(scope, &block.body));
                }
                Task::Push(value) => self.results.push(value),
                Task::Discard => {
                    self.results.pop();
                }
                Task::Combine(Combine::All(count)) => {
                    let start = self.results.len() - count;
                    let all = self.results.drain(start..).all(|terminated| terminated);
                    self.results.push(all);
                }
                Task::Combine(Combine::Try {
                    alternate,
                    rescues,
                    ensure,
                }) => {
                    let ensure = ensure && self.results.pop().unwrap_or(false);
                    let start = self.results.len() - rescues;
                    let rescued = self.results.drain(start..).all(|terminated| terminated);
                    let alternate = alternate && self.results.pop().unwrap_or(false);
                    let body = self.results.pop().unwrap_or(false);
                    let normal = body || alternate;
                    self.results.push(
                        ensure
                            || if rescues == 0 {
                                normal
                            } else {
                                normal && rescued
                            },
                    );
                }
            }
        }
        self.results.pop().unwrap_or(false)
    }

    fn statement(&mut self, scope: Arc<str>, stmt: &'a Stmt) {
        let tasks = &mut self.tasks;
        match &stmt.node {
            Statement::Return(value) | Statement::Break(value) | Statement::Next(value) => {
                tasks.push(Task::Push(true));
                if let Some(value) = value {
                    tasks.push(Task::Expr(scope, value));
                }
            }
            Statement::Raise(value, message) => {
                tasks.push(Task::Push(true));
                for expr in value.iter().chain(message) {
                    tasks.push(Task::Expr(scope.clone(), expr));
                }
            }
            Statement::Retry => tasks.push(Task::Push(true)),
            Statement::Module(_) | Statement::UnboundClass(_) => tasks.push(Task::Push(false)),
            Statement::Assign(target, _, value) => {
                tasks.push(Task::Push(false));
                tasks.push(Task::Expr(scope.clone(), value));
                tasks.push(Task::Target(scope, target));
            }
            Statement::Expr(Expr {
                node: Node::Try(attempt),
                ..
            }) if !attempt.modifier => tasks.push(Task::Try(scope, attempt)),
            Statement::Expr(expr) => {
                tasks.push(Task::Push(false));
                tasks.push(Task::Expr(scope, expr));
            }
            Statement::If(branches, alternate, _) => {
                if alternate.is_empty() {
                    tasks.push(Task::Push(false));
                    for (condition, body) in branches.iter().rev() {
                        tasks.push(Task::Discard);
                        tasks.push(Task::Body(scope.clone(), body));
                        tasks.push(Task::Expr(scope.clone(), condition));
                    }
                } else {
                    tasks.push(Task::Combine(Combine::All(branches.len() + 1)));
                    tasks.push(Task::Body(scope.clone(), alternate));
                    for (condition, body) in branches.iter().rev() {
                        tasks.push(Task::Body(scope.clone(), body));
                        tasks.push(Task::Expr(scope.clone(), condition));
                    }
                }
            }
            Statement::While(condition, body, _) => {
                tasks.push(Task::Push(false));
                tasks.push(Task::Discard);
                tasks.push(Task::Body(scope.clone(), body));
                tasks.push(Task::Expr(scope, condition));
            }
            Statement::For(target, iterable, body) => {
                tasks.push(Task::Push(false));
                tasks.push(Task::Discard);
                tasks.push(Task::Body(scope.clone(), body));
                tasks.push(Task::Expr(scope.clone(), iterable));
                tasks.push(Task::Target(scope, target));
            }
        }
    }

    fn attempt(&mut self, scope: Arc<str>, attempt: &'a Try) {
        let rescues: Vec<_> = attempt
            .rescues
            .iter()
            .filter(|rescue| !rescue.body.is_empty())
            .collect();
        let tasks = &mut self.tasks;
        tasks.push(Task::Combine(Combine::Try {
            alternate: !attempt.alternate.is_empty(),
            rescues: rescues.len(),
            ensure: !attempt.ensure.is_empty(),
        }));
        if !attempt.ensure.is_empty() {
            tasks.push(Task::Body(scope.clone(), &attempt.ensure));
        }
        for rescue in rescues.iter().rev() {
            tasks.push(Task::Body(scope.clone(), &rescue.body));
        }
        if !attempt.alternate.is_empty() {
            tasks.push(Task::Body(scope.clone(), &attempt.alternate));
        }
        tasks.push(Task::Body(scope, &attempt.body));
    }

    fn expression(&mut self, scope: Arc<str>, expr: &'a Expr) {
        let tasks = &mut self.tasks;
        let mut walk = |expr: &'a Expr| tasks.push(Task::Expr(scope.clone(), expr));
        match &expr.node {
            Node::Try(attempt) if attempt.modifier => {
                // `value rescue fallback` holds one expression on each side.
                let bodies =
                    std::iter::once(&attempt.body).chain(attempt.rescues.iter().map(|r| &r.body));
                for stmt in bodies.flatten() {
                    self.tasks.push(Task::Discard);
                    self.tasks.push(Task::Stmt(scope.clone(), stmt));
                }
            }
            Node::Try(attempt) => {
                self.tasks.push(Task::Discard);
                self.tasks.push(Task::Try(scope, attempt));
            }
            Node::Compound(stmt) => {
                self.tasks.push(Task::Discard);
                self.tasks.push(Task::Stmt(scope, stmt));
            }
            Node::Regex(..)
            | Node::Integer(_)
            | Node::BigInteger(..)
            | Node::Literal(_)
            | Node::Var(_) => {}
            Node::Shape(_, fallback, _) => fallback.iter().for_each(|e| walk(e)),
            Node::Template(parts, _) | Node::Array(parts) | Node::Yield(parts) => {
                parts.iter().for_each(walk);
            }
            Node::Hash(pairs) => pairs.iter().for_each(|(_, value)| walk(value)),
            Node::Unary(_, value) => walk(value),
            Node::Binary(_, left, right) => {
                walk(left);
                walk(right);
            }
            Node::Range(start, end, _) => start.iter().chain(end).for_each(|e| walk(e)),
            Node::Conditional(branches, alternate) => {
                for (condition, result) in branches {
                    walk(condition);
                    walk(result);
                }
                walk(alternate);
            }
            Node::Case(target, whens, alternate) => {
                target.iter().for_each(|e| walk(e));
                for when in whens {
                    when.values.iter().for_each(|(value, _)| walk(value));
                    walk(&when.result);
                }
                alternate.iter().for_each(|e| walk(e));
            }
            Node::Call(_, args, _) => args.iter().for_each(|arg| walk(&arg.value)),
            Node::ComputedCall(callee, args)
            | Node::Method(callee, _, args, _)
            | Node::SafeMethod(callee, _, args, _) => {
                walk(callee);
                args.iter().for_each(|arg| walk(&arg.value));
            }
            Node::Scope(object, _, args) => {
                walk(object);
                args.iter().flatten().for_each(|arg| walk(&arg.value));
            }
            Node::Member(object, _) | Node::SafeMember(object, _) => walk(object),
            Node::Index(object, indexes) => {
                walk(object);
                indexes.iter().for_each(walk);
            }
            Node::BlockCall(callee, block) => {
                walk(callee);
                self.tasks.push(Task::Block(scope, block));
            }
        }
    }
}
