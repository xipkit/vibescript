//! `require` with literal names: the checker resolves each required file,
//! checks it, and types its exports by their declarations.

use super::{
    Checker, Input, Modules,
    meter::Heap,
    program::{Enum, FnDecl, Namespace, NsId},
    sigs::{BlockSig, Param, Sig},
    ty::{Field, Kind, Ty, Types},
    walk::{Item, Next, Walk},
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
    enums: Vec<Arc<Enum>>,
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

impl Exported {
    /// What the file's exports hold while an importer reads them.
    fn bytes(&self) -> usize {
        let classes: usize = self
            .classes
            .iter()
            .map(|class| {
                std::mem::size_of::<ExportedClass>()
                    + class.name.heap()
                    + class
                        .methods
                        .iter()
                        .map(|(name, sig, _)| {
                            std::mem::size_of::<(String, Sig, Visibility)>()
                                + name.heap()
                                + sig.heap()
                        })
                        .sum::<usize>()
            })
            .sum();
        self.types.bytes()
            + self.types.names.heap()
            + self.functions.heap()
            + self.enums.heap()
            + classes
    }
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
    origin: Option<&'a crate::loading::Origin>,
    hosts: Vec<(&'a String, &'a Registered)>,
    declared: &'a crate::declared::Declarations,
    depth: usize,
    pub loaded: Vec<Exports>,
    by_path: HashMap<String, Result<u32, String>>,
    by_origin: HashMap<crate::loading::Origin, u32>,
    /// Aliases `require(..., as:)` binds, to the exports they name.
    pub aliases: HashMap<String, u32>,
    /// Exported functions, which `require` also publishes by name.
    pub published: HashMap<String, Rc<Sig>>,
}

impl Heap for Exports {
    fn heap(&self) -> usize {
        self.path.heap() + self.functions.heap() + self.enums.heap()
    }
}

impl Heap for Required<'_> {
    fn heap(&self) -> usize {
        use super::meter::map;
        let origins: usize = self
            .by_origin
            .keys()
            .map(|origin| origin.name().len())
            .sum();
        // The published signatures are the loaded modules' own.
        let published: usize = self.published.keys().map(Heap::heap).sum();
        super::meter::vec(&self.hosts)
            + self.loaded.heap()
            + self.by_path.heap()
            + map(&self.by_origin)
            + origins
            + self.aliases.heap()
            + map(&self.published)
            + published
    }
}

impl<'a> Required<'a> {
    pub fn new(input: &Input<'a>, depth: usize) -> Self {
        Self {
            resolve: input.modules,
            origin: input.origin,
            hosts: input.hosts.clone(),
            declared: input.declared,
            depth,
            loaded: Vec::new(),
            by_path: HashMap::new(),
            by_origin: HashMap::new(),
            aliases: HashMap::new(),
            published: HashMap::new(),
        }
    }

    /// The exports of a literal path, once required.
    pub fn exports(&self, path: &str) -> Option<u32> {
        self.by_path
            .get(path)
            .and_then(|result| result.as_ref().ok())
            .copied()
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
        // What the paths and aliases found hold.
        let mut found = 0;
        let mut walk = Walk::new(&self.meter);
        for function in parsed.functions.iter() {
            requires(&mut walk, &function.body, &mut requests, &mut found);
        }
        // What is left of the namespaces at each level of nesting, whose
        // bodies and methods are walked in turn.
        let mut levels = vec![parsed.modules.iter().chain([].iter())];
        while let Some(level) = levels.last_mut() {
            let Some(module) = level.next() else {
                levels.pop();
                continue;
            };
            requires(&mut walk, &module.body, &mut requests, &mut found);
            for (def, _) in module.methods.iter() {
                requires(&mut walk, &def.body, &mut requests, &mut found);
            }
            for (def, _) in module.instance_methods.iter() {
                requires(&mut walk, &def.body, &mut requests, &mut found);
            }
            levels.push(module.modules.iter().chain(module.inner.iter()));
        }
        let scratch = walk.bytes() + super::meter::vec(&levels);
        drop(walk);
        self.transient(scratch + super::meter::vec(&requests) + found);
        // A check past its budget loads no files.
        if self.halted() {
            return;
        }
        requests.sort_unstable_by_key(|request| request.2);
        for (path, alias, offset) in requests {
            // A check past its budget loads no more files.
            if self.over_budget() {
                return;
            }
            let id = self.load_module(&path);
            if self.halted() {
                return;
            }
            if let Err(reason) = &id {
                self.report(Diagnostic::error(
                    Code::UNDEFINED_NAME,
                    self.spans.token(offset),
                    format!("cannot statically resolve required module {path:?}: {reason}"),
                ));
            }
            if let (Ok(id), Some(alias)) = (id, alias) {
                self.modules
                    .aliases
                    .entry(alias.trim().to_owned())
                    .or_insert(id);
            }
        }
    }

    /// A context for work the check does through the compiler, such as
    /// finding and parsing a required file, which what the check leaves of
    /// its budget bounds, beside the `held` bytes: the steps, the memory, the
    /// deadline and the cancellation.
    fn context(&self, held: usize) -> crate::CallContext {
        let budget = self.meter.budget();
        let steps = self.total_steps();
        crate::CallContext::new(crate::CallOptions {
            limits: crate::Limits {
                steps: budget.steps.map(|left| left.saturating_sub(steps)),
                memory_bytes: budget.memory.map(|left| left.saturating_sub(held)),
                ..crate::Limits::default()
            },
            cancellation: budget.cancellation.clone().unwrap_or_default(),
            deadline: budget.deadline,
            ..crate::CallOptions::default()
        })
    }

    fn load_module(&mut self, path: &str) -> Result<u32, String> {
        if let Some(known) = self.modules.by_path.get(path) {
            return known.clone();
        }
        self.modules
            .by_path
            .insert(path.to_owned(), Err("circular require".into()));
        let result = self.load_module_uncached(path);
        self.modules.by_path.insert(path.to_owned(), result.clone());
        result
    }

    fn load_module_uncached(&mut self, path: &str) -> Result<u32, String> {
        if self.modules.depth >= DEPTH {
            return Err(format!(
                "require nesting exceeds {DEPTH} files (possible circular require)"
            ));
        }
        let resolve = self
            .modules
            .resolve
            .ok_or("no module resolver is configured")?;
        // Finding the file and reading it take work and memory before they
        // are counted, so they charge a context of their own, which what
        // the check leaves of its budget bounds, as the file's parse does,
        // and running out stops the check.
        let held = self.held();
        let mut context = self.context(held);
        let resolved = resolve(path, self.modules.origin, &mut context);
        let resolving = context.stats();
        self.meter.charge(resolving.steps);
        self.observed(held + resolving.peak_memory_bytes);
        let (source, origin) = match resolved {
            Ok(resolved) => resolved,
            Err(error) if context.exhausted() => {
                self.stopped = true;
                self.meter.stop();
                return Err(error.to_string());
            }
            Err(error) => return Err(error.message),
        };
        if let Some(&id) = self.modules.by_origin.get(&origin) {
            return Ok(id);
        }
        let filename = origin.filename();
        // The file's source and syntax count toward this check's memory
        // until it is imported. Its parse charges a context of its own,
        // as a compilation does, which the steps and memory left bound,
        // and its steps are this check's.
        let held = self.held() + source.len();
        let mut context = self.context(held);
        let parse = crate::syntax::parse_with_tokens(
            &source,
            &crate::compilation::Meter(std::cell::RefCell::new(&mut context)),
        );
        let parsing = context.stats();
        self.meter.charge(parsing.steps);
        self.observed(held + parsing.peak_memory_bytes);
        let (parsed, tokens, _tokens_held) = match parse {
            Ok(parsed) => parsed,
            Err(error)
                if matches!(
                    error.kind,
                    crate::ErrorKind::Steps
                        | crate::ErrorKind::Memory
                        | crate::ErrorKind::Deadline
                        | crate::ErrorKind::Cancelled
                ) =>
            {
                self.stopped = true;
                self.meter.stop();
                return Err(error.to_string());
            }
            Err(error) => {
                let error = crate::source::parse_error(
                    &source,
                    Some(&filename),
                    crate::syntax::canonical_syntax(&source, &(), error),
                    &(),
                );
                return Err(error.to_string());
            }
        };
        let tree = self.hold(source.len() + parsing.retained_memory_bytes);
        // The file's check may spend what this one leaves.
        let steps = self.total_steps();
        let mut budget = self.meter.budget().less(steps);
        let held = self.held();
        budget.memory = budget.memory.map(|left| left.saturating_sub(held));
        let input = Input {
            source: &source,
            parsed: &parsed,
            tokens: &tokens,
            hosts: self.modules.hosts.clone(),
            declared: self.modules.declared,
            file: true,
            origin: Some(&origin),
            modules: self.modules.resolve,
            budget,
            observe: None,
            annotate: false,
        };
        let checked = super::check_nested(&input, self.modules.depth + 1);
        self.meter.charge(checked.steps);
        // What the file's check held beside this one's tables, and at most
        // its surface pass's too.
        self.observed(held + checked.peak_bytes + checked.surface_bytes);
        if checked.stopped {
            // The file's check stopped at the budget this one shares, so
            // this one stops too, without its findings or exports.
            self.stopped = true;
            self.meter.stop();
            return Err("the check ran out of its budget".into());
        }
        let source: Arc<str> = source.into();
        for mut diagnostic in checked.diagnostics.into_iter().filter(Diagnostic::is_error) {
            if diagnostic.source.is_none() {
                diagnostic.source = Some(source.clone());
            }
            let file = diagnostic
                .file
                .clone()
                .or_else(|| Some(Arc::clone(&filename)));
            self.report(diagnostic.in_file(file));
        }
        let (functions, enums) = match &checked.exported {
            Some(exported) => {
                let held = self.hold(exported.bytes());
                // A check that holding the exports stops imports none of
                // them.
                if self.halted() {
                    self.release(held);
                    return Err("the check ran out of its budget".into());
                }
                let imported = self.import(exported);
                self.release(held);
                // A check that importing them stops publishes none of them.
                if self.halted() {
                    return Err("the check ran out of its budget".into());
                }
                imported
            }
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
        self.modules.by_origin.insert(origin, id);
        self.release(tree);
        Ok(id)
    }

    /// What this check's file exports: its public functions, its enums and
    /// its classes, with its type table, which the check gives up.
    /// What [`Self::export`] copies beside the declarations it copies them
    /// from: its public functions' names and signatures, the list of its
    /// enums, and its classes' names and their methods' names and
    /// signatures. The type table moves rather than being copied.
    pub(super) fn export_bytes(&self) -> usize {
        use std::mem::size_of;
        let functions: usize = self
            .program
            .functions
            .iter()
            .filter(|(_, id)| self.program.fns[**id].def.is_some_and(|def| !def.private))
            .map(|(name, id)| {
                size_of::<(String, Sig)>() + name.len() + self.program.fns[*id].sig.heap()
            })
            .sum();
        let enums = self.parsed.enums.len() * size_of::<Arc<Enum>>();
        let classes: usize = self
            .program
            .namespaces
            .iter()
            .filter(|namespace| namespace.module.is_some() && namespace.is_class)
            .map(|namespace| {
                size_of::<ExportedClass>()
                    + namespace.name.len()
                    + namespace
                        .methods
                        .iter()
                        .filter(|(name, _)| name.as_str() != "initialize")
                        .map(|(name, &id)| {
                            size_of::<(String, Sig, Visibility)>()
                                + name.len()
                                + self.program.fns[id].sig.heap()
                        })
                        .sum::<usize>()
            })
            .sum();
        functions + enums + classes
    }

    pub(super) fn export(&mut self) -> Exported {
        let mut functions: Vec<(String, Sig)> = self
            .program
            .functions
            .iter()
            .filter(|(_, id)| self.program.fns[**id].def.is_some_and(|def| !def.private))
            .map(|(name, id)| ((*name).to_owned(), (*self.program.fns[*id].sig).clone()))
            .collect();
        functions.sort_unstable_by(|a, b| a.0.cmp(&b.0));
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
            methods.sort_unstable_by(|a, b| a.0.cmp(&b.0));
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
            self.meter.charge(1);
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
                checked: false,
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
                // A check that importing stops imports no more.
                if self.halted() {
                    break;
                }
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
        let mut functions = HashMap::with_capacity(exported.functions.len());
        for (name, sig) in &exported.functions {
            if self.halted() {
                break;
            }
            let sig = self.import_sig(&exported.types, sig, &imports);
            functions.insert(name.clone(), Rc::new(sig));
        }
        self.declaring();
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
            converts: sig.converts,
            id: None,
        }
    }

    /// A type of a required file's table in this check's: its enums and
    /// classes become the ones imported from it, and a type the file could
    /// not resolve, or one of a file it requires in turn, becomes `any`.
    fn import_ty(&mut self, from: &Types, ty: Ty, imports: &Imports) -> Ty {
        self.meter.charge(1);
        match &*from.shared(ty) {
            Kind::Error | Kind::Namespace(_) | Kind::Exports(_) => Ty::ANY,
            Kind::Array(element) => {
                let element = self.import_ty(from, *element, imports);
                self.types.array(element)
            }
            Kind::Hash(value) => {
                let value = self.import_ty(from, *value, imports);
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
                self.types.shape(fields, *open)
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
                let described = self.import_ty(from, *described, imports);
                self.types.type_lit(described)
            }
            Kind::Instance(ns) => match imports.classes.get(ns) {
                Some(&id) => self.types.intern(Kind::Instance(id)),
                None => Ty::ANY,
            },
            Kind::EnumValue(id) | Kind::EnumType(id) => {
                let Some(&imported) = imports.enums.get(*id as usize) else {
                    return Ty::ANY;
                };
                let kind = match from.kind(ty) {
                    Kind::EnumValue(_) => Kind::EnumValue(imported),
                    _ => Kind::EnumType(imported),
                };
                self.types.intern(kind)
            }
            Kind::Host(id) => {
                let name = &from.names.hosts[*id as usize];
                match self.types.names.hosts.iter().position(|host| host == name) {
                    Some(index) => self.types.intern(Kind::Host(index as u32)),
                    None => Ty::ANY,
                }
            }
            // Scalars, builtin namespaces, type variables and symbols mean
            // the same in both tables.
            kind => self.types.intern(kind.clone()),
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

/// Adds the literal paths, and aliases, of the `require` calls in `body` to
/// `out`, and what they hold to `found`, walking it with `walk`.
fn requires<'x>(
    walk: &mut Walk<'x, '_>,
    body: &'x [Stmt],
    out: &mut Vec<(String, Option<String>, usize)>,
    found: &mut usize,
) {
    // The body is a visit, empty or not.
    if walk.visit(super::meter::vec(out) + *found) {
        return;
    }
    walk.stmts(body, ());
    while let Some((item, ())) = walk.next(super::meter::vec(out) + *found) {
        match item {
            Item::Stmt(stmt) => match &stmt.node {
                Statement::Expr(e)
                | Statement::Assign(_, _, e)
                | Statement::Return(Some(e))
                | Statement::Break(Some(e))
                | Statement::Next(Some(e)) => walk.expr(e, ()),
                Statement::If(branches, alternate, _) => {
                    walk.push(Next::Clauses(branches.iter()), ());
                    walk.stmts(alternate, ());
                }
                Statement::While(condition, body, _) => {
                    walk.expr(condition, ());
                    walk.stmts(body, ());
                }
                Statement::For(_, iterable, body) => {
                    walk.expr(iterable, ());
                    walk.stmts(body, ());
                }
                _ => (),
            },
            Item::Expr(expr) => visit(expr, walk, out, found),
            Item::Target(_) => (),
        }
    }
}

fn visit<'x>(
    expr: &'x Expr,
    walk: &mut Walk<'x, '_>,
    out: &mut Vec<(String, Option<String>, usize)>,
    found: &mut usize,
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
                        }
                        _ => None,
                    });
                if let Some(path) = path {
                    *found += path.capacity() + alias.as_ref().map_or(0, String::capacity);
                    out.push((path, alias, expr.offset as usize));
                }
            }
            walk.push(Next::Arguments(args.iter()), ());
        }
        Node::Compound(stmt) => walk.push(Next::Item(Item::Stmt(stmt)), ()),
        Node::Try(attempt) => {
            walk.stmts(&attempt.body, ());
            walk.stmts(&attempt.alternate, ());
            walk.stmts(&attempt.ensure, ());
            walk.push(Next::Rescues(attempt.rescues.iter()), ());
        }
        Node::BlockCall(call, block) => {
            walk.expr(call, ());
            walk.stmts(&block.body, ());
        }
        Node::Conditional(branches, alternate) => {
            walk.push(Next::Branches(branches.iter()), ());
            walk.expr(alternate, ());
        }
        Node::Case(subject, whens, alternate) => {
            for expr in subject.iter().chain(alternate) {
                walk.expr(expr, ());
            }
            walk.push(Next::Results(whens.iter()), ());
        }
        Node::Binary(_, l, r) => {
            walk.expr(l, ());
            walk.expr(r, ());
        }
        Node::Unary(_, v) => walk.expr(v, ()),
        Node::Method(recv, _, args, _) | Node::SafeMethod(recv, _, args, _) => {
            walk.expr(recv, ());
            walk.push(Next::Arguments(args.iter()), ());
        }
        Node::Member(recv, _) | Node::SafeMember(recv, _) => walk.expr(recv, ()),
        Node::Array(items) | Node::Template(items, _) => walk.push(Next::Exprs(items.iter()), ()),
        Node::Hash(entries) => walk.push(Next::Pairs(entries.iter()), ()),
        Node::Index(recv, selectors) => {
            walk.expr(recv, ());
            walk.push(Next::Exprs(selectors.iter()), ());
        }
        _ => (),
    }
}
