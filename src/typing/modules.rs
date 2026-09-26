//! `require` with literal names: the checker resolves each required file,
//! checks it, and types its exports by their declarations.

use super::{
    Checker, Input, Modules,
    program::{Enum, FnDecl, Namespace, NsId},
    sigs::{BlockSig, Param, Sig},
    ty::{Field, Kind, Ty, Types},
};
use crate::{
    capability::Registered,
    diagnostic::{Code, Diagnostic},
    syntax::{Declarations, Expr, Node, Statement, Stmt, modules::Visibility},
};
use std::{collections::HashMap, fmt, rc::Rc, sync::Arc};

/// Files `require` may nest before the checker stops following them.
const DEPTH: usize = 16;

/// The functions and enums a required file exports.
pub(crate) struct Exports {
    pub path: String,
    pub functions: HashMap<String, Rc<Sig>>,
    pub enums: HashMap<String, u32>,
}

/// What a required file exports, typed by its declarations in the file's
/// own type table, which a requiring check imports into its own.
pub(crate) struct Exported {
    types: Types,
    /// Its public top-level functions.
    functions: Vec<(String, Sig)>,
    /// Its enums: name, members and each member's symbol.
    enums: Vec<Enum>,
    /// Its classes, which are not exported by name, but whose instances
    /// are values its functions may return.
    classes: Vec<ExportedClass>,
}

/// A class a required file declares.
struct ExportedClass {
    /// Its id in the file.
    id: NsId,
    name: String,
    /// Its instance methods, by name, with their visibility.
    methods: Vec<(String, Sig, Visibility)>,
}

impl fmt::Debug for Exported {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Exported")
            .field(
                "functions",
                &self
                    .functions
                    .iter()
                    .map(|(name, _)| name)
                    .collect::<Vec<_>>(),
            )
            .field(
                "enums",
                &self.enums.iter().map(|e| &e.name).collect::<Vec<_>>(),
            )
            .field(
                "classes",
                &self
                    .classes
                    .iter()
                    .map(|class| &class.name)
                    .collect::<Vec<_>>(),
            )
            .finish()
    }
}

/// How the ids of a required file's enums and classes map to the ids its
/// importer gave them.
struct Imports {
    enums: Vec<u32>,
    classes: HashMap<NsId, NsId>,
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
        let (functions, enums) = match &checked.exported {
            Some(exported) => self.import(exported),
            None => (HashMap::new(), HashMap::new()),
        };
        for (name, sig) in &functions {
            self.modules
                .published
                .entry(name.clone())
                .or_insert_with(|| sig.clone());
        }
        let id = self.modules.loaded.len() as u32;
        self.modules.loaded.push(Exports {
            path: path.to_owned(),
            functions,
            enums,
        });
        self.modules.by_path.insert(path.to_owned(), Some(id));
        Some(id)
    }

    /// What this check's file exports: its public functions, its enums and
    /// its classes, with its type table, which the check gives up.
    pub(super) fn export(&mut self) -> Exported {
        let mut functions: Vec<(String, Sig)> = self
            .program
            .functions
            .iter()
            .filter(|(_, id)| self.program.fns[**id].def.is_some_and(|def| !def.private))
            .map(|(name, id)| ((*name).to_owned(), (*self.program.fns[*id].sig).clone()))
            .collect();
        functions.sort_by(|a, b| a.0.cmp(&b.0));
        // The file's own enums come first; imported ones follow.
        let enums = self.program.enums[..self.parsed.enums.len()].to_vec();
        let mut classes = Vec::new();
        for (ns, namespace) in self.program.namespaces.iter().enumerate() {
            if namespace.module.is_none() || !namespace.is_class {
                continue;
            }
            let mut methods: Vec<(String, Sig, Visibility)> = namespace
                .methods
                .iter()
                .filter(|(name, _)| name.as_str() != "initialize")
                .map(|(name, &id)| {
                    let decl = &self.program.fns[id];
                    (name.clone(), (*decl.sig).clone(), decl.visibility)
                })
                .collect();
            methods.sort_by(|a, b| a.0.cmp(&b.0));
            classes.push(ExportedClass {
                id: ns as NsId,
                name: namespace.name.clone(),
                methods,
            });
        }
        Exported {
            types: std::mem::replace(&mut self.types, Types::new()),
            functions,
            enums,
            classes,
        }
    }

    /// Imports what a required file exports: its enums, bound by name
    /// where the name is free, as the runtime binds them; its classes,
    /// whose instances its functions may return but whose names stay
    /// private to it; and its functions, typed in this check's types.
    fn import(&mut self, exported: &Exported) -> (HashMap<String, Rc<Sig>>, HashMap<String, u32>) {
        let mut imports = Imports {
            enums: Vec::new(),
            classes: HashMap::new(),
        };
        let mut enums = HashMap::new();
        for declared in &exported.enums {
            self.steps += 1;
            let id = self.program.enums.len() as u32;
            self.program.enums.push(declared.clone());
            self.types.names.enums.push(declared.name.clone());
            let free = !self.program.enum_names.contains_key(&declared.name)
                && !self.program.roots.contains_key(declared.name.as_str());
            if free {
                self.program.enum_names.insert(declared.name.clone(), id);
            }
            enums.insert(declared.name.clone(), id);
            imports.enums.push(id);
        }
        for class in &exported.classes {
            let id = self.program.namespaces.len() as NsId;
            self.types.names.namespaces.push(class.name.clone());
            self.program.namespaces.push(Namespace {
                module: None,
                name: class.name.clone(),
                parent: None,
                is_class: true,
                methods: HashMap::new(),
                statics: HashMap::new(),
                ivars: HashMap::new(),
                children: HashMap::new(),
            });
            imports.classes.insert(class.id, id);
        }
        for class in &exported.classes {
            let owner = imports.classes[&class.id];
            for (name, sig, visibility) in &class.methods {
                let sig = self.import_sig(&exported.types, sig, &imports);
                let id = self.program.fns.len();
                self.program.fns.push(FnDecl {
                    def: None,
                    owner: Some(owner),
                    instance: true,
                    sig: Rc::new(sig),
                    main: false,
                    visibility: *visibility,
                });
                self.program.namespaces[owner as usize]
                    .methods
                    .insert(name.clone(), id);
            }
        }
        let functions = exported
            .functions
            .iter()
            .map(|(name, sig)| {
                let sig = self.import_sig(&exported.types, sig, &imports);
                (name.clone(), Rc::new(sig))
            })
            .collect();
        (functions, enums)
    }

    /// A required file's signature in this check's types.
    fn import_sig(&mut self, from: &Types, sig: &Sig, imports: &Imports) -> Sig {
        let params = sig
            .params
            .iter()
            .map(|param| Param {
                ty: self.import_ty(from, param.ty, imports),
                ..param.clone()
            })
            .collect();
        let block = sig.block.as_ref().map(|block| BlockSig {
            params: block
                .params
                .iter()
                .map(|&ty| self.import_ty(from, ty, imports))
                .collect(),
            rest: block.rest.map(|ty| self.import_ty(from, ty, imports)),
            result: block.result.map(|ty| self.import_ty(from, ty, imports)),
            optional: block.optional,
        });
        Sig {
            name: sig.name.clone(),
            params,
            result: sig.result.map(|ty| self.import_ty(from, ty, imports)),
            block,
            vars: Vec::new(),
            breaks: sig.breaks,
        }
    }

    /// A type of a required file's table in this check's: its enums and
    /// classes become the ones imported from it, and a type the file could
    /// not resolve, or one of a file it requires in turn, becomes `any`.
    fn import_ty(&mut self, from: &Types, ty: Ty, imports: &Imports) -> Ty {
        self.steps += 1;
        match from.kind(ty).clone() {
            Kind::Error | Kind::Namespace(_) | Kind::Exports(_) => Ty::ANY,
            Kind::Array(element) => {
                let element = self.import_ty(from, element, imports);
                self.types.array(element)
            }
            Kind::Hash(value) => {
                let value = self.import_ty(from, value, imports);
                self.types.hash(value)
            }
            Kind::Shape(fields, open) => {
                let fields = fields
                    .iter()
                    .map(|field| Field {
                        name: field.name.clone(),
                        ty: self.import_ty(from, field.ty, imports),
                        optional: field.optional,
                    })
                    .collect();
                self.types.shape(fields, open)
            }
            Kind::Tuple(items) => {
                let items = items
                    .iter()
                    .map(|&item| self.import_ty(from, item, imports))
                    .collect();
                self.types.tuple(items)
            }
            Kind::Union(items) => {
                let items: Vec<Ty> = items
                    .iter()
                    .map(|&item| self.import_ty(from, item, imports))
                    .collect();
                self.types.union(&items)
            }
            Kind::TypeLit(described) => {
                let described = self.import_ty(from, described, imports);
                self.types.type_lit(described)
            }
            Kind::Instance(ns) => match imports.classes.get(&ns) {
                Some(&id) => self.types.intern(Kind::Instance(id)),
                None => Ty::ANY,
            },
            Kind::EnumValue(id) | Kind::EnumType(id) => {
                let Some(&imported) = imports.enums.get(id as usize) else {
                    return Ty::ANY;
                };
                let kind = match from.kind(ty) {
                    Kind::EnumValue(_) => Kind::EnumValue(imported),
                    _ => Kind::EnumType(imported),
                };
                self.types.intern(kind)
            }
            Kind::Host(id) => {
                let name = &from.names.hosts[id as usize];
                match self.types.names.hosts.iter().position(|host| host == name) {
                    Some(index) => self.types.intern(Kind::Host(index as u32)),
                    None => Ty::ANY,
                }
            }
            // Scalars, builtin namespaces, type variables and symbols mean
            // the same in both tables.
            kind => self.types.intern(kind),
        }
    }

    /// `receiver.name(...)` on the object `require` returned.
    pub(super) fn exported(&self, id: u32, name: &str) -> Option<Rc<Sig>> {
        self.modules.loaded[id as usize]
            .functions
            .get(name)
            .cloned()
    }

    /// `receiver.Name` on the object `require` returned, for an enum.
    pub(super) fn exported_enum(&self, id: u32, name: &str) -> Option<u32> {
        self.modules.loaded[id as usize].enums.get(name).copied()
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
