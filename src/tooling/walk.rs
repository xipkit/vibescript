//! Builds an [`Outline`] from parsed declarations and the parse record.
//!
//! Syntax nests as deeply as the parser's depth guard, so every walk here
//! keeps its own stack rather than recursing.

use super::{Function, Item, ItemKind, Outline, Parameter, ParameterKind, Rescue, StatementKind};
use crate::{
    Position,
    compilation::Type,
    source::Source,
    syntax::{
        Declarations, Definition, Expr, Node, ParamKind, Statement, Stmt, Target,
        modules::Module,
        record::{Record, Top},
    },
};
use std::collections::HashSet;

pub(super) fn outline(source: &str, declarations: &Declarations, record: &Record) -> Outline {
    let Ok(index) = Source::compile(source, &()) else {
        return Outline::default();
    };
    let at = |offset: u32| index.position(offset);
    let mut items = Vec::with_capacity(record.top.len());
    for (offset, top) in &record.top {
        let position = at(*offset);
        items.push(match top {
            Top::Function(index) => {
                let definition = &declarations.functions[index + 1];
                let mut item = Item::new(ItemKind::Function, &definition.name, position);
                item.function = Some(function(definition, &at));
                item
            }
            Top::Alias(index, target) => {
                let definition = &declarations.functions[index + 1];
                let mut item = Item::new(ItemKind::Alias, &definition.name, position);
                item.function = Some(function(definition, &at));
                item.target = Some(target.clone());
                item
            }
            Top::Enum(index) => {
                let (name, members) = &declarations.enums[*index];
                let mut item = Item::new(ItemKind::Enum, name, position);
                item.children = members
                    .iter()
                    .zip(&record.enums[*index])
                    .map(|(member, offset)| Item::new(ItemKind::EnumMember, member, at(*offset)))
                    .collect();
                item
            }
            Top::Module(index) => module(source, &declarations.modules[*index], record, &at),
            Top::Statement(index) => {
                let stmt = &declarations.functions[0].body[*index];
                Item::new(ItemKind::Statement(kind(source, stmt)), "", position)
            }
        });
    }
    Outline { items }
}

impl Item {
    fn new(kind: ItemKind, name: &str, position: Position) -> Self {
        Self {
            kind,
            name: name.to_owned(),
            position,
            function: None,
            target: None,
            children: Vec::new(),
        }
    }
}

/// Converts a class or module and its nested modules, innermost first.
fn module(source: &str, root: &Module, record: &Record, at: &impl Fn(u32) -> Position) -> Item {
    struct Frame<'a> {
        module: &'a Module,
        nested: Vec<(u32, Item)>,
        next: usize,
    }
    let mut stack = vec![Frame {
        module: root,
        nested: Vec::new(),
        next: 0,
    }];
    loop {
        let frame = stack.last_mut().expect("the root frame is popped last");
        if let Some(nested) = frame.module.modules.get(frame.next) {
            frame.next += 1;
            stack.push(Frame {
                module: nested,
                nested: Vec::new(),
                next: 0,
            });
            continue;
        }
        let frame = stack.pop().expect("the frame was just inspected");
        let item = members(source, frame.module, frame.nested, record, at);
        match stack.last_mut() {
            Some(parent) => parent.nested.push((frame.module.offset, item)),
            None => return item,
        }
    }
}

fn members(
    source: &str,
    module: &Module,
    nested: Vec<(u32, Item)>,
    record: &Record,
    at: &impl Fn(u32) -> Position,
) -> Item {
    let mut children = nested;
    let mut properties = HashSet::new();
    for (index, (definition, _)) in module.instance_methods.iter().enumerate() {
        if let Some((name, _)) = &definition.accessor {
            if properties.insert((definition.offset, &**name)) {
                let item = Item::new(ItemKind::Property, name, at(definition.offset));
                children.push((definition.offset, item));
            }
            continue;
        }
        let alias = record
            .aliases
            .iter()
            .find(|alias| alias.class == module.offset && alias.index == index);
        let (kind, offset) = match alias {
            Some(alias) => (ItemKind::Alias, alias.offset),
            None => (ItemKind::Method, definition.offset),
        };
        let mut item = Item::new(kind, &definition.name, at(offset));
        item.function = Some(function(definition, at));
        item.target = alias.map(|alias| alias.target.clone());
        children.push((offset, item));
    }
    for (definition, _) in &module.methods {
        let mut item = Item::new(
            ItemKind::ClassMethod,
            &definition.name,
            at(definition.offset),
        );
        item.function = Some(function(definition, at));
        children.push((definition.offset, item));
    }
    for stmt in &module.body {
        let item = match constant(stmt).filter(|_| !module.is_class) {
            Some(name) => Item::new(ItemKind::Constant, name, at(stmt.offset)),
            None => Item::new(ItemKind::Statement(kind(source, stmt)), "", at(stmt.offset)),
        };
        children.push((stmt.offset, item));
    }
    children.sort_by_key(|(offset, _)| *offset);
    let kind = if module.is_class {
        ItemKind::Class
    } else {
        ItemKind::Module
    };
    let mut item = Item::new(kind, &module.name, at(module.offset));
    item.children = children.into_iter().map(|(_, item)| item).collect();
    item
}

/// The syntactic kind of a statement, distinguishing `until` from `while`
/// by its keyword.
fn kind(text: &str, stmt: &Stmt) -> StatementKind {
    let keyword_is = |offset: u32, word: &str| {
        text.get(offset as usize..)
            .is_some_and(|rest| rest.starts_with(word))
    };
    match &stmt.node {
        Statement::Expr(Expr {
            node: Node::Try(attempt),
            ..
        }) if !attempt.modifier => StatementKind::Begin,
        Statement::Expr(_) => StatementKind::Expression,
        Statement::Assign(..) => StatementKind::Assignment,
        Statement::If(..) => StatementKind::If,
        Statement::While(_, _, modifier) => {
            if keyword_is(modifier.unwrap_or(stmt.offset), "until") {
                StatementKind::Until
            } else {
                StatementKind::While
            }
        }
        Statement::For(..) => StatementKind::For,
        Statement::Return(_) => StatementKind::Return,
        Statement::Raise(..) => StatementKind::Raise,
        Statement::Break(_) => StatementKind::Break,
        Statement::Next(_) => StatementKind::Next,
        Statement::Retry => StatementKind::Retry,
        // Declarations never reach here; a nested class is an expression-like statement.
        Statement::Module(_) | Statement::UnboundClass(_) => StatementKind::Expression,
    }
}

/// The name a plain assignment to a capitalized name declares.
fn constant(stmt: &Stmt) -> Option<&str> {
    let Statement::Assign(Target::Value(target), "=", _) = &stmt.node else {
        return None;
    };
    let Node::Var(name) = &target.node else {
        return None;
    };
    name.chars()
        .next()
        .is_some_and(crate::syntax::unicode::upper)
        .then_some(&**name)
}

fn function(definition: &Definition, at: &impl Fn(u32) -> Position) -> Function {
    Function {
        params: definition
            .params
            .iter()
            .map(|param| Parameter {
                name: param.name.to_string(),
                kind: match param.kind {
                    ParamKind::Positional => ParameterKind::Positional,
                    ParamKind::Keyword => ParameterKind::Keyword,
                    ParamKind::Rest => ParameterKind::Rest,
                    ParamKind::KeywordRest => ParameterKind::KeywordRest,
                },
                type_annotation: param.ty.as_ref().map(type_text),
                default: param.default.is_some(),
                instance: param.ivar.is_some(),
            })
            .collect(),
        return_type: definition.return_type.as_ref().map(type_text),
        locals: locals(&definition.body),
        rescues: rescues(&definition.body, at),
        last_statement: last_statement(&definition.body).map(at),
    }
}

fn type_text(ty: &Type) -> String {
    let mut text = Vec::new();
    // Formatting into a vector cannot fail.
    let formatted = crate::shapes::format(ty, &mut text);
    debug_assert!(formatted.is_ok());
    String::from_utf8_lossy(&text).into_owned()
}

enum Visit<'a> {
    Stmt(&'a Stmt),
    Expr(&'a Expr),
    Target(&'a Target),
}

/// Collects the names a body assigns outside blocks, in source order.
fn locals(body: &[Stmt]) -> Vec<String> {
    let mut names = Vec::new();
    let mut seen = HashSet::new();
    let mut pending: Vec<Visit<'_>> = body.iter().rev().map(Visit::Stmt).collect();
    while let Some(visit) = pending.pop() {
        let start = pending.len();
        match visit {
            Visit::Stmt(stmt) => match &stmt.node {
                Statement::Raise(value, message) => {
                    pending.extend(value.iter().chain(message).map(|e| Visit::Expr(e)));
                }
                Statement::Retry | Statement::Module(_) | Statement::UnboundClass(_) => (),
                Statement::Expr(expr) => pending.push(Visit::Expr(expr)),
                Statement::Assign(target, _, value) => {
                    pending.push(Visit::Target(target));
                    pending.push(Visit::Expr(value));
                }
                Statement::If(branches, alternate, modifier) => {
                    for (condition, body) in branches.iter() {
                        let condition = Visit::Expr(condition);
                        if modifier.is_some() {
                            pending.extend(body.iter().map(Visit::Stmt));
                            pending.push(condition);
                        } else {
                            pending.push(condition);
                            pending.extend(body.iter().map(Visit::Stmt));
                        }
                    }
                    pending.extend(alternate.iter().map(Visit::Stmt));
                }
                Statement::While(condition, body, modifier) => {
                    if modifier.is_some() {
                        pending.extend(body.iter().map(Visit::Stmt));
                        pending.push(Visit::Expr(condition));
                    } else {
                        pending.push(Visit::Expr(condition));
                        pending.extend(body.iter().map(Visit::Stmt));
                    }
                }
                Statement::For(target, iterable, body) => {
                    pending.push(Visit::Target(target));
                    pending.push(Visit::Expr(iterable));
                    pending.extend(body.iter().map(Visit::Stmt));
                }
                Statement::Return(value) | Statement::Break(value) | Statement::Next(value) => {
                    pending.extend(value.iter().map(Visit::Expr));
                }
            },
            Visit::Target(target) => match target {
                Target::Value(expr) => {
                    if let Node::Var(name) = &expr.node {
                        if !name.starts_with('@') && seen.insert(name.to_string()) {
                            names.push(name.to_string());
                        }
                    }
                }
                Target::Tuple(parts) => {
                    pending.extend(
                        parts
                            .iter()
                            .filter_map(|(t, _)| t.as_ref().map(Visit::Target)),
                    );
                }
                Target::Typed(target, _) => pending.push(Visit::Target(target)),
            },
            Visit::Expr(expr) => children(expr, &mut pending),
        }
        // Children were pushed in source order; visit them first to last.
        pending[start..].reverse();
    }
    names
}

/// Queues an expression's subexpressions in source order, except block bodies.
fn children<'a>(expr: &'a Expr, pending: &mut Vec<Visit<'a>>) {
    let arguments = |args: &'a [crate::syntax::Argument], pending: &mut Vec<Visit<'a>>| {
        pending.extend(args.iter().map(|argument| Visit::Expr(&argument.value)));
    };
    match &expr.node {
        Node::Try(attempt) => {
            pending.extend(attempt.body.iter().map(Visit::Stmt));
            for rescue in attempt.rescues.iter() {
                pending.extend(rescue.body.iter().map(Visit::Stmt));
            }
            pending.extend(attempt.alternate.iter().map(Visit::Stmt));
            pending.extend(attempt.ensure.iter().map(Visit::Stmt));
        }
        Node::Regex(..)
        | Node::Integer(_)
        | Node::BigInteger(..)
        | Node::Literal(_)
        | Node::Var(_) => (),
        Node::Shape(_, fallback, _) => pending.extend(fallback.iter().map(|e| Visit::Expr(e))),
        Node::Template(values, _) | Node::Array(values) | Node::Yield(values) => {
            pending.extend(values.iter().map(Visit::Expr));
        }
        Node::Hash(pairs) => pending.extend(pairs.iter().map(|(_, value)| Visit::Expr(value))),
        Node::Unary(_, operand) => pending.push(Visit::Expr(operand)),
        Node::Binary(_, left, right) => {
            pending.push(Visit::Expr(left));
            pending.push(Visit::Expr(right));
        }
        Node::Range(start, end, _) => {
            pending.extend(start.iter().chain(end).map(|e| Visit::Expr(e)));
        }
        Node::Conditional(branches, alternate) => {
            for (condition, result) in branches.iter() {
                pending.push(Visit::Expr(condition));
                pending.push(Visit::Expr(result));
            }
            pending.push(Visit::Expr(alternate));
        }
        Node::Case(target, whens, alternate) => {
            pending.extend(target.iter().map(|e| Visit::Expr(e)));
            for when in whens.iter() {
                pending.extend(when.values.iter().map(|(value, _)| Visit::Expr(value)));
                pending.push(Visit::Expr(&when.result));
            }
            pending.extend(alternate.iter().map(|e| Visit::Expr(e)));
        }
        Node::Compound(stmt) => pending.push(Visit::Stmt(stmt)),
        Node::Call(_, args, _) => arguments(args, pending),
        Node::ComputedCall(callee, args) => {
            pending.push(Visit::Expr(callee));
            arguments(args, pending);
        }
        Node::BlockCall(callee, _) => pending.push(Visit::Expr(callee)),
        Node::Member(receiver, _) | Node::SafeMember(receiver, _) => {
            pending.push(Visit::Expr(receiver));
        }
        Node::Scope(receiver, _, args) => {
            pending.push(Visit::Expr(receiver));
            if let Some(args) = args {
                arguments(args, pending);
            }
        }
        Node::Method(receiver, _, args, _) | Node::SafeMethod(receiver, _, args, _) => {
            pending.push(Visit::Expr(receiver));
            arguments(args, pending);
        }
        Node::Index(receiver, indices) => {
            pending.push(Visit::Expr(receiver));
            pending.extend(indices.iter().map(Visit::Expr));
        }
    }
}

/// The statement bodies nested in one statement's control flow, in source
/// order, and the start of each `elsif` branch.
fn nested<'a>(stmt: &'a Stmt, bodies: &mut Vec<&'a [Stmt]>, branches: &mut Vec<u32>) {
    match &stmt.node {
        Statement::If(arms, alternate, modifier) => {
            for (index, (condition, body)) in arms.iter().enumerate() {
                if index > 0 && modifier.is_none() {
                    branches.push(condition.offset);
                }
                bodies.push(body);
            }
            bodies.push(alternate);
        }
        Statement::While(_, body, _) | Statement::For(_, _, body) => bodies.push(body),
        Statement::Expr(Expr {
            node: Node::Try(attempt),
            ..
        }) if !attempt.modifier => {
            bodies.push(&attempt.body);
            bodies.push(&attempt.alternate);
            for rescue in attempt.rescues.iter() {
                bodies.push(&rescue.body);
            }
            bodies.push(&attempt.ensure);
        }
        _ => (),
    }
}

/// The greatest statement offset in a body, descending into control flow.
fn last_statement(body: &[Stmt]) -> Option<u32> {
    let mut last = None;
    let mut pending = vec![body];
    let mut bodies = Vec::new();
    let mut branches = Vec::new();
    while let Some(stmts) = pending.pop() {
        for stmt in stmts {
            last = last.max(Some(stmt.offset));
            nested(stmt, &mut bodies, &mut branches);
            pending.append(&mut bodies);
            last = last.max(branches.drain(..).max());
        }
    }
    last
}

/// Named rescue clauses outside blocks, each `begin`'s own clauses before the
/// clauses nested in its bodies, in source order.
fn rescues(body: &[Stmt], at: &impl Fn(u32) -> Position) -> Vec<Rescue> {
    let mut out = Vec::new();
    let mut stack = vec![body.iter()];
    let mut bodies = Vec::new();
    let mut branches = Vec::new();
    while let Some(stmts) = stack.last_mut() {
        let Some(stmt) = stmts.next() else {
            stack.pop();
            continue;
        };
        if let Statement::Expr(Expr {
            node: Node::Try(attempt),
            ..
        }) = &stmt.node
        {
            if !attempt.modifier {
                for rescue in attempt.rescues.iter() {
                    if let Some(binding) = &rescue.binding {
                        out.push(Rescue {
                            binding: binding.to_string(),
                            position: at(rescue.offset),
                            last_statement: last_statement(&rescue.body).map(at),
                        });
                    }
                }
            }
        }
        nested(stmt, &mut bodies, &mut branches);
        branches.clear();
        stack.extend(bodies.drain(..).rev().map(|body| body.iter()));
    }
    out
}
