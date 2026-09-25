//! `require` with literal names: the checker resolves each required file,
//! checks it, and types its exports by their declarations.

use super::{
    Checker, Input, Modules,
    program::FnId,
    sigs::{ParamKind, Sig},
    ty::{Kind, Ty, Types},
};
use crate::{
    capability::Registered,
    diagnostic::{Code, Diagnostic},
    syntax::{Declarations, Expr, Node, Statement, Stmt},
};
use std::{collections::HashMap, rc::Rc, sync::Arc};

/// Files `require` may nest before the checker stops following them.
const DEPTH: usize = 16;

/// The functions a required file exports.
pub(crate) struct Exports {
    pub path: String,
    pub functions: HashMap<String, Rc<Sig>>,
}

/// What the program requires.
pub(crate) struct Required<'a> {
    resolve: Option<&'a Modules<'a>>,
    hosts: Vec<(&'a String, &'a Registered)>,
    declared: &'a crate::declared::Declarations,
    depth: usize,
    pub loaded: Vec<Exports>,
    by_path: HashMap<String, Option<u32>>,
    /// Aliases `require(..., as:)` binds, to the exports they name.
    pub aliases: HashMap<String, u32>,
    /// Exported functions, which `require` also publishes by name.
    pub published: HashMap<String, Rc<Sig>>,
}

impl<'a> Required<'a> {
    pub fn new(input: &Input<'a>, depth: usize) -> Self {
        Self {
            resolve: input.modules,
            hosts: input.hosts.clone(),
            declared: input.declared,
            depth,
            loaded: Vec::new(),
            by_path: HashMap::new(),
            aliases: HashMap::new(),
            published: HashMap::new(),
        }
    }

    /// The exports of a literal path, once required.
    pub fn exports(&self, path: &str) -> Option<u32> {
        self.by_path.get(path).copied().flatten()
    }
}

impl<'a> Checker<'a> {
    /// Resolves and checks every module the program requires with literal
    /// names, before any body is checked, so its exports are known wherever
    /// they are used.
    pub(super) fn require_modules(&mut self, parsed: &'a Declarations) {
        if self.modules.resolve.is_none() {
            return;
        }
        let mut requests = Vec::new();
        let mut bodies: Vec<&[Stmt]> = parsed.functions.iter().map(|f| &f.body[..]).collect();
        let mut pending: Vec<&crate::syntax::modules::Module> = parsed.modules.iter().collect();
        while let Some(module) = pending.pop() {
            bodies.push(&module.body);
            bodies.extend(module.methods.iter().map(|(def, _)| &def.body[..]));
            bodies.extend(module.instance_methods.iter().map(|(def, _)| &def.body[..]));
            pending.extend(module.modules.iter().chain(&module.inner));
        }
        for body in bodies {
            requires(body, &mut requests);
        }
        for (path, alias) in requests {
            let id = self.load_module(&path);
            if let (Some(id), Some(alias)) = (id, alias) {
                self.modules.aliases.insert(alias, id);
            }
        }
    }

    fn load_module(&mut self, path: &str) -> Option<u32> {
        if let Some(&known) = self.modules.by_path.get(path) {
            return known;
        }
        self.modules.by_path.insert(path.to_owned(), None);
        if self.modules.depth >= DEPTH {
            return None;
        }
        let (source, filename) = (self.modules.resolve?)(path)?;
        let (parsed, tokens) = crate::syntax::parse_with_tokens(&source, &()).ok()?;
        let input = Input {
            source: &source,
            parsed: &parsed,
            tokens: &tokens,
            hosts: self.modules.hosts.clone(),
            declared: self.modules.declared,
            file: true,
            modules: self.modules.resolve,
        };
        let checked = super::check_nested(&input, self.modules.depth + 1);
        self.steps += checked.steps;
        for diagnostic in checked.diagnostics.into_iter().filter(Diagnostic::is_error) {
            let file = diagnostic
                .file
                .clone()
                .or_else(|| Some(Arc::clone(&filename)));
            self.report(diagnostic.in_file(file));
        }
        let mut functions = HashMap::new();
        for declaration in &checked.exports {
            let Ok(table) = crate::signatures::Table::parse(declaration) else {
                continue;
            };
            let functions_in = table.items.iter().filter_map(|item| match item {
                crate::signatures::Item::Function(function) => Some(function),
                _ => None,
            });
            for function in functions_in {
                let mut sig = self
                    .converter
                    .convert_owned(&mut self.types, function, None);
                sig.checks_break = true;
                let sig = Rc::new(sig);
                self.modules
                    .published
                    .entry(function.name.clone())
                    .or_insert_with(|| sig.clone());
                functions.insert(function.name.clone(), sig);
            }
        }
        let id = self.modules.loaded.len() as u32;
        self.modules.loaded.push(Exports {
            path: path.to_owned(),
            functions,
        });
        self.modules.by_path.insert(path.to_owned(), Some(id));
        Some(id)
    }

    /// The public top-level functions of a required file, as signature
    /// declarations another check can read.
    pub(super) fn export_declarations(&self) -> Vec<String> {
        let mut exports = Vec::new();
        for (&name, &id) in &self.program.functions {
            let decl: &super::program::FnDecl<'_> = &self.program.fns[id as FnId];
            if decl.def.private {
                continue;
            }
            exports.push(declaration(&self.types, name, &decl.sig));
        }
        exports.sort();
        exports
    }

    /// `receiver.name(...)` on the object `require` returned.
    pub(super) fn exported(&self, id: u32, name: &str) -> Option<Rc<Sig>> {
        self.modules.loaded[id as usize]
            .functions
            .get(name)
            .cloned()
    }

    /// Reports an unknown export.
    pub(super) fn unknown_export(&mut self, id: u32, name: &str, span: crate::diagnostic::Span) {
        let path = self.modules.loaded[id as usize].path.clone();
        self.report(Diagnostic::error(
            Code::UNKNOWN_MEMBER,
            span,
            format!("the module \"{path}\" exports no function `{name}`"),
        ));
    }

    /// The exports type of a required module.
    pub(super) fn exports_type(&mut self, id: u32) -> Ty {
        self.types.intern(Kind::Exports(id))
    }
}

/// A signature as the signature table writes it.
fn declaration(types: &Types, name: &str, sig: &Sig) -> String {
    let ty = |ty: Ty| {
        let text = types.display(ty);
        if text.contains("unknown") {
            "any".to_owned()
        } else {
            text
        }
    };
    let mut params = Vec::new();
    let star = sig.keyword_star();
    for (index, param) in sig.params.iter().enumerate() {
        if star == Some(index) {
            params.push("*".to_owned());
        }
        params.push(match param.kind {
            ParamKind::Positional | ParamKind::Keyword if param.optional => {
                format!("{}?: {}", param.name, ty(param.ty))
            }
            ParamKind::Positional | ParamKind::Keyword => {
                format!("{}: {}", param.name, ty(param.ty))
            }
            ParamKind::Rest => format!("*{}: {}", param.name, ty(param.ty)),
            ParamKind::KeywordRest => format!("**{}: {}", param.name, ty(param.ty)),
        });
    }
    if let Some(block) = &sig.block {
        let args: Vec<String> = block.params.iter().map(|&p| ty(p)).collect();
        let args = match args.len() {
            1 => args[0].clone(),
            _ => format!("({})", args.join(", ")),
        };
        let result = block
            .result
            .map(|r| format!(" -> {}", ty(r)))
            .unwrap_or_default();
        let optional = if block.optional { "?" } else { "" };
        params.push(format!("&block{optional}: {args}{result}"));
    }
    let result = sig
        .result
        .map(|r| format!(" -> {}", ty(r)))
        .unwrap_or_default();
    format!("def {name}({}){result}\n", params.join(", "))
}

/// The literal paths, and aliases, of the `require` calls in statements.
fn requires(body: &[Stmt], out: &mut Vec<(String, Option<String>)>) {
    let mut statements: Vec<&Stmt> = body.iter().collect();
    let mut expressions: Vec<&Expr> = Vec::new();
    loop {
        if let Some(expr) = expressions.pop() {
            visit(expr, &mut statements, &mut expressions, out);
            continue;
        }
        let Some(stmt) = statements.pop() else {
            break;
        };
        match &stmt.node {
            Statement::Expr(e) => expressions.push(e),
            Statement::Assign(_, _, e) => expressions.push(e),
            Statement::If(branches, alternate, _) => {
                for (condition, body) in branches.iter() {
                    expressions.push(condition);
                    statements.extend(body.iter());
                }
                statements.extend(alternate.iter());
            }
            Statement::While(condition, body, _) => {
                expressions.push(condition);
                statements.extend(body.iter());
            }
            Statement::For(_, iterable, body) => {
                expressions.push(iterable);
                statements.extend(body.iter());
            }
            Statement::Return(Some(e)) | Statement::Break(Some(e)) | Statement::Next(Some(e)) => {
                expressions.push(e)
            }
            _ => (),
        }
    }
}

fn visit<'x>(
    expr: &'x Expr,
    statements: &mut Vec<&'x Stmt>,
    expressions: &mut Vec<&'x Expr>,
    out: &mut Vec<(String, Option<String>)>,
) {
    match &expr.node {
        Node::Call(name, args, _) => {
            if name.as_str() == "require" {
                let path = args
                    .iter()
                    .find_map(|arg| match (&arg.kind, &arg.value.node) {
                        (crate::syntax::ArgumentKind::Positional, Node::Literal(value)) => value
                            .as_bytes()
                            .map(|b| String::from_utf8_lossy(b).into_owned()),
                        _ => None,
                    });
                let alias = args
                    .iter()
                    .find_map(|arg| match (&arg.kind, &arg.value.node) {
                        (crate::syntax::ArgumentKind::Keyword(key), Node::Literal(value))
                            if key.as_str() == "as" =>
                        {
                            value
                                .as_bytes()
                                .map(|b| String::from_utf8_lossy(b).into_owned())
                                .or_else(|| super::symbol_text(value))
                        }
                        _ => None,
                    });
                if let Some(path) = path {
                    out.push((path, alias));
                }
            }
            expressions.extend(args.iter().map(|a| &a.value));
        }
        Node::Compound(stmt) => statements.push(stmt),
        Node::Try(attempt) => {
            statements.extend(attempt.body.iter());
            statements.extend(attempt.alternate.iter());
            statements.extend(attempt.ensure.iter());
            for rescue in attempt.rescues.iter() {
                statements.extend(rescue.body.iter());
            }
        }
        Node::BlockCall(call, block) => {
            expressions.push(call);
            statements.extend(block.body.iter());
        }
        Node::Conditional(branches, alternate) => {
            for (c, v) in branches.iter() {
                expressions.push(c);
                expressions.push(v);
            }
            expressions.push(alternate);
        }
        Node::Case(subject, whens, alternate) => {
            expressions.extend(subject.as_deref());
            for when in whens.iter() {
                expressions.push(&when.result);
            }
            expressions.extend(alternate.as_deref());
        }
        Node::Binary(_, l, r) => {
            expressions.push(l);
            expressions.push(r);
        }
        Node::Unary(_, v) => expressions.push(v),
        Node::Method(recv, _, args, _) | Node::SafeMethod(recv, _, args, _) => {
            expressions.push(recv);
            expressions.extend(args.iter().map(|a| &a.value));
        }
        Node::Member(recv, _) | Node::SafeMember(recv, _) => expressions.push(recv),
        Node::Array(items) | Node::Template(items, _) => expressions.extend(items.iter()),
        Node::Hash(entries) => expressions.extend(entries.iter().map(|(_, v)| v)),
        Node::Index(recv, selectors) => {
            expressions.push(recv);
            expressions.extend(selectors.iter());
        }
        _ => (),
    }
}
