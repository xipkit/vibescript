//! `require` with literal names: the checker resolves each required file,
//! checks it, and types its exports by their declarations.

use super::{
    Checker, Input, Modules,
    counted::{CountedMap, CountedSet, CountedVec, ScratchVec},
    meter::Heap,
    program::{Enum, FnDecl, FnId, Namespace, NsId},
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

/// Why a file's own require of itself, while it loads, does not load.
const CIRCULAR: &str = "circular require";

/// The functions and enums a required file exports.
pub(crate) struct Exports {
    pub path: String,
    pub functions: CountedMap<String, Rc<Sig>>,
    pub enums: CountedMap<String, u32>,
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
    /// What importing them lists beside them: the ids its enums and
    /// classes take in the importer, at the lengths they take.
    fn imports(&self) -> usize {
        self.enums.len() * std::mem::size_of::<u32>()
            + super::meter::table::<(NsId, NsId)>(self.classes.len())
    }

    /// What the file's type table, which the exports took from its check,
    /// holds.
    pub(super) fn types_bytes(&self) -> usize {
        self.types.bytes()
    }

    /// What the file's exports hold while an importer reads them.
    pub(super) fn bytes(&self) -> usize {
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
    hosts: &'a [(&'a String, &'a Registered)],
    declared: &'a crate::declared::Declarations,
    depth: usize,
    pub loaded: CountedVec<Exports>,
    by_path: CountedMap<String, Result<u32, String>>,
    by_origin: CountedMap<crate::loading::Origin, u32>,
    /// Aliases `require(..., as:)` binds, to the exports they name.
    pub aliases: CountedMap<String, u32>,
    /// Exported functions, which `require` also publishes by name.
    pub published: CountedMap<String, Rc<Sig>>,
    /// The sources and file names that the diagnostics of required files
    /// keep, by address, so each is counted once however many keep it.
    retained: CountedSet<usize>,
    /// What they hold.
    pub kept: usize,
}

/// Its path; its tables are counted as they grow.
impl super::counted::Owned for Exports {
    fn owned(&self) -> usize {
        self.path.capacity()
    }
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
        self.loaded.heap()
            // The paths and reasons it keeps are counted as they are kept.
            + map(&self.by_path)
            + map(&self.by_origin)
            + origins
            + self.aliases.heap()
            + map(&self.published)
            + published
            + super::meter::set(&self.retained)
    }
}

impl<'a> Required<'a> {
    pub fn new(input: &Input<'a>, depth: usize) -> Self {
        Self {
            resolve: input.modules,
            origin: input.origin,
            hosts: input.hosts,
            declared: input.declared,
            depth,
            loaded: CountedVec::new(),
            by_path: CountedMap::new(),
            by_origin: CountedMap::new(),
            aliases: CountedMap::new(),
            published: CountedMap::new(),
            retained: CountedSet::new(),
            kept: 0,
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
        // The requests found, in a list counted, with the paths and aliases
        // they copy, while it lives.
        let mut requests = ScratchVec::new(&self.meter);
        // What the paths and aliases found hold.
        let mut found = 0;
        // A walk the budget stops visits no more declarations.
        let mut walk = Walk::new(&self.meter);
        for function in parsed.functions.iter() {
            if self.halted() {
                return;
            }
            requires(&mut walk, &function.body, &mut requests, &mut found);
        }
        // What is left of the namespaces at each level of nesting, whose
        // bodies and methods are walked in turn, in a list counted, with
        // its growth admitted first, while it lives: a namespace's nested
        // namespaces, above its inner ones, so they are walked first.
        let mut levels = ScratchVec::new(&self.meter);
        if levels.push(parsed.modules.iter()).is_err() {
            return;
        }
        while let Some(level) = levels.last_mut() {
            if self.halted() {
                return;
            }
            let Some(module) = level.next() else {
                levels.pop();
                continue;
            };
            requires(&mut walk, &module.body, &mut requests, &mut found);
            for (def, _) in module.methods.iter().chain(module.instance_methods.iter()) {
                if self.halted() {
                    return;
                }
                requires(&mut walk, &def.body, &mut requests, &mut found);
            }
            if levels.push(module.inner.iter()).is_err()
                || levels.push(module.modules.iter()).is_err()
            {
                return;
            }
        }
        drop(levels);
        let scratch = walk.bytes();
        drop(walk);
        // A check past its budget loads no files. The requests, with the
        // paths and aliases they copy, are held while the files load.
        if self.transient(scratch) {
            return;
        }
        let mut requests = requests.into_vec();
        let Some(held) = self.hold(super::meter::vec(&requests) + found) else {
            return;
        };
        if super::counted::sort_unstable_by(&self.meter, &mut requests, |a, b| a.2.cmp(&b.2))
            .is_err()
        {
            self.release(held);
            return;
        }
        for (path, alias, offset) in requests {
            // A check past its budget loads no more files.
            if self.over_budget() {
                self.release(held);
                return;
            }
            let id = self.load_module(&path);
            if self.halted() {
                self.release(held);
                return;
            }
            if let Err(reason) = &id {
                self.report(Diagnostic::error(
                    Code::UNDEFINED_NAME,
                    self.spans.token(offset),
                    text!(
                        self,
                        "cannot statically resolve required module {path:?}: {reason}"
                    ),
                ));
            }
            if let (Ok(id), Some(alias)) = (id, alias) {
                // A new alias's copy of its name, and its room in the
                // table, are counted as it is kept; a check that stops loads
                // no more files.
                let alias = alias.trim();
                if !self.modules.aliases.contains_key(alias)
                    && self
                        .modules
                        .aliases
                        .insert_made(
                            self.meter.declarations(),
                            alias.len(),
                            || alias.to_owned(),
                            id,
                        )
                        .is_err()
                {
                    self.release(held);
                    return;
                }
            }
        }
        self.release(held);
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

    /// Counts the source and the file name `diagnostic` keeps, each once
    /// however many diagnostics keep it. Returns whether the check has
    /// stopped.
    #[must_use = "the budget may have stopped the check, which must then do no more work"]
    fn retain(&mut self, diagnostic: &Diagnostic) -> bool {
        let mut bytes = 0;
        let declarations = self.meter.declarations();
        let retained = &mut self.modules.retained;
        for (address, length) in [
            diagnostic
                .source
                .as_ref()
                .map(|source| (Arc::as_ptr(source).cast::<u8>() as usize, source.len())),
            diagnostic
                .file
                .as_ref()
                .map(|file| (Arc::as_ptr(file).cast::<u8>() as usize, file.len())),
        ]
        .into_iter()
        .flatten()
        {
            match retained.insert(declarations, address) {
                Ok(true) => bytes += length,
                Ok(false) => (),
                Err(_) => return true,
            }
        }
        self.modules.kept += bytes;
        self.grow(bytes)
    }

    fn load_module(&mut self, path: &str) -> Result<u32, String> {
        if let Some(known) = self.modules.by_path.get(path) {
            return known.clone();
        }
        // The table keeps a copy of the path, and of the reason a file did
        // not load, each counted, with the path's room in the table, as it
        // is kept, and by the measures after.
        let circular: Result<u32, String> = Err(CIRCULAR.into());
        let bytes = path.len() + super::counted::Owned::owned(&circular);
        let Ok(mut kept) = self.meter.tables().keep(bytes) else {
            return Err("the check ran out of its budget".into());
        };
        if self
            .modules
            .by_path
            .reserve(self.meter.declarations(), 1)
            .is_err()
        {
            return Err("the check ran out of its budget".into());
        }
        self.modules
            .by_path
            .insert_kept(&mut kept, path.to_owned(), circular);
        self.grown += bytes;
        let result = self.load_module_uncached(path);
        if let Err(reason) = &result {
            if self.grow(reason.len()) {
                return Err("the check ran out of its budget".into());
            }
        }
        // The placeholder gives back what it owned once the result takes
        // its place.
        if let Some(entry) = self.modules.by_path.get_mut(path) {
            let placeholder = std::mem::replace(entry, result.clone());
            self.grown = self
                .grown
                .saturating_sub(super::counted::Owned::owned(&placeholder));
        }
        result
    }

    fn load_module_uncached(&mut self, path: &str) -> Result<u32, String> {
        if self.modules.depth >= DEPTH {
            return Err(text!(
                self,
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
        let charged = self.meter.charge(resolving.steps);
        self.observed(held + resolving.peak_memory_bytes);
        if charged {
            self.stopped = true;
            return Err("the check ran out of its budget".into());
        }
        let (source, origin) = match resolved {
            Ok(resolved) => resolved,
            Err(error) if context.exhausted() => {
                self.stopped = true;
                self.meter.stop();
                return Err(text!(self, "{error}"));
            }
            Err(error) => return Err(error.message),
        };
        if let Some(&id) = self.modules.by_origin.get(&origin) {
            return Ok(id);
        }
        let filename = origin.filename();
        // The file's source and syntax, and its origin's copy of its name,
        // count toward this check's memory until it is imported. Its parse
        // charges a context of its own, as a compilation does, which the
        // steps and memory left bound, and its steps are this check's.
        let read = source.len() + origin.name().len();
        let held = self.held() + read;
        let mut context = self.context(held);
        let parse = crate::syntax::parse_with_tokens(
            &source,
            &crate::compilation::Meter(std::cell::RefCell::new(&mut context)),
        );
        let parsing = context.stats();
        let charged = self.meter.charge(parsing.steps);
        self.observed(held + parsing.peak_memory_bytes);
        if charged {
            self.stopped = true;
            return Err("the check ran out of its budget".into());
        }
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
                return Err(text!(self, "{error}"));
            }
            Err(error) => {
                // The error is recovered and located within what the parse
                // left of the budget, in the parse's context, as a host's
                // syntax error is, and its steps are this check's too.
                let error = {
                    let work = crate::compilation::Meter(std::cell::RefCell::new(&mut context));
                    let error = crate::syntax::host_syntax(&source, &work, error);
                    crate::source::parse_error(&source, Some(&filename), error, &work)
                };
                let reporting = context.stats();
                let charged = self
                    .meter
                    .charge(reporting.steps.saturating_sub(parsing.steps));
                self.observed(held + reporting.peak_memory_bytes);
                if charged || context.exhausted() {
                    self.stopped = true;
                    self.meter.stop();
                    return Err("the check ran out of its budget".into());
                }
                return Err(text!(self, "{error}"));
            }
        };
        let Some(tree) = self.hold(read + parsing.retained_memory_bytes) else {
            self.stopped = true;
            return Err("the check ran out of its budget".into());
        };
        // The file's check may spend what this one leaves.
        let steps = self.total_steps();
        let mut budget = self.meter.budget().less(steps);
        let held = self.held();
        budget.memory = budget.memory.map(|left| left.saturating_sub(held));
        let input = Input {
            source: &source,
            parsed: &parsed,
            tokens: &tokens,
            hosts: self.modules.hosts,
            declared: self.modules.declared,
            file: true,
            origin: Some(&origin),
            modules: self.modules.resolve,
            budget,
            observe: None,
            annotate: false,
        };
        let checked = super::check_nested(&input, self.modules.depth + 1);
        let charged = self.meter.charge(checked.steps);
        let (peak, stopped) = (checked.peak(), checked.stopped);
        // This check keeps the file's diagnostics and imports its exports;
        // the rest of what the file's check found, its facts and its
        // receivers, goes before this one measures again.
        let (diagnostics, exported) = checked.into_kept();
        // What the file's check held at most beside this one's tables, its
        // surface pass's with it.
        self.observed(held + peak);
        // The file's steps are checked against the budget, and its memory
        // too, before its exports are imported.
        if stopped || charged || self.over_budget() {
            // The file's check stopped at the budget this one shares, or
            // its steps took this one past it, so this one stops too,
            // without its findings or exports.
            self.stopped = true;
            self.meter.stop();
            self.release(tree);
            return Err("the check ran out of its budget".into());
        }
        // The diagnostics, until each is kept, and the exports, with what
        // importing them builds, are held from here; a check that holding
        // them stops keeps and imports none of them.
        let found = super::meter::vec(&diagnostics)
            + diagnostics
                .iter()
                .map(super::meter::Heap::heap)
                .sum::<usize>()
            + exported
                .as_ref()
                .map_or(0, |exported| exported.bytes() + exported.imports());
        let Some(found) = self.hold(found) else {
            self.release(tree);
            return Err("the check ran out of its budget".into());
        };
        // The copy of the source the file's diagnostics share is made only
        // for a first diagnostic that needs it, and counted before it is,
        // while the source it copies is held as well.
        let mut shared: Option<Arc<str>> = None;
        for mut diagnostic in diagnostics.into_iter().filter(Diagnostic::is_error) {
            if diagnostic.source.is_none() {
                if shared.is_none() {
                    if self.transient(source.len()) {
                        self.release(found);
                        self.release(tree);
                        return Err("the check ran out of its budget".into());
                    }
                    shared = Some(Arc::from(source.as_str()));
                }
                diagnostic.source = shared.clone();
            }
            let file = diagnostic
                .file
                .clone()
                .or_else(|| Some(Arc::clone(&filename)));
            let diagnostic = diagnostic.in_file(file);
            // The source and the file name it keeps outlive the file's
            // check, and its reservation of them.
            if self.retain(&diagnostic) {
                self.release(found);
                self.release(tree);
                return Err("the check ran out of its budget".into());
            }
            self.report(diagnostic);
        }
        let (functions, enums) = match &exported {
            Some(exported) => {
                let imported = self.import(exported);
                // A check that importing them stops publishes none of them.
                if self.halted() {
                    self.release(found);
                    self.release(tree);
                    return Err("the check ran out of its budget".into());
                }
                imported
            }
            None => (CountedMap::new(), CountedMap::new()),
        };
        // The exports are let go once imported.
        drop(exported);
        self.release(found);
        // Each function published by a new name, with its copy of its name
        // and room for it, is counted as the table takes it; the file's
        // exports and its origin, with their copies of their names and
        // room for them in the tables, before they are kept.
        for (name, sig) in &functions {
            if self.modules.published.contains_key(name) {
                continue;
            }
            let declarations = self.meter.declarations();
            if self
                .modules
                .published
                .insert_made(declarations, name.len(), || name.clone(), Rc::clone(sig))
                .is_err()
            {
                self.release(tree);
                return Err("the check ran out of its budget".into());
            }
        }
        let declarations = self.meter.declarations();
        let kept = declarations.keep(path.len() + origin.name().len());
        let Ok(mut kept) = kept else {
            self.release(tree);
            return Err("the check ran out of its budget".into());
        };
        if self.modules.loaded.reserve(declarations, 1).is_err()
            || self.modules.by_origin.reserve(declarations, 1).is_err()
        {
            self.release(tree);
            return Err("the check ran out of its budget".into());
        }
        let id = self.modules.loaded.len() as u32;
        self.modules.loaded.push_kept(
            &mut kept,
            Exports {
                path: path.to_owned(),
                functions,
                enums,
            },
        );
        self.modules.by_origin.insert_kept(&mut kept, origin, id);
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
                size_of::<(String, Sig)>() + name.len() + self.program.fns[*id].sig.as_ref().heap()
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
                                + self.program.fns[id].sig.as_ref().heap()
                        })
                        .sum::<usize>()
            })
            .sum();
        functions + enums + classes
    }

    /// `None` when a sort the budget refuses stops the check, which then
    /// exports nothing.
    pub(super) fn export(&mut self) -> Option<Exported> {
        // Each list is made at the length it takes, which
        // [`Self::export_bytes`] counted.
        let program = &self.program;
        let public = |id: FnId| program.fns[id].def.is_some_and(|def| !def.private);
        let exported = program.functions.values().filter(|&&id| public(id)).count();
        let mut functions: Vec<(String, Sig)> = Vec::with_capacity(exported);
        functions.extend(
            program
                .functions
                .iter()
                .filter(|&(_, &id)| public(id))
                .map(|(name, &id)| ((*name).to_owned(), (*program.fns[id].sig).clone())),
        );
        super::counted::sort_unstable_by(&self.meter, &mut functions, |a, b| a.0.cmp(&b.0)).ok()?;
        // The file's own enums come first; imported ones follow.
        let enums = program.enums[..self.parsed.enums.len()].to_vec();
        let class = |namespace: &Namespace<'_>| namespace.module.is_some() && namespace.is_class;
        let count = program
            .namespaces
            .iter()
            .filter(|&namespace| class(namespace))
            .count();
        let mut classes = Vec::with_capacity(count);
        for (ns, namespace) in program.namespaces.iter().enumerate() {
            if !class(namespace) {
                continue;
            }
            let exported = |name: &&String| name.as_str() != "initialize";
            let count = namespace.methods.keys().filter(exported).count();
            let mut methods: Vec<(String, Sig, Visibility)> = Vec::with_capacity(count);
            methods.extend(
                namespace
                    .methods
                    .iter()
                    .filter(|(name, _)| exported(name))
                    .map(|(name, &id)| {
                        let decl = &program.fns[id];
                        (name.clone(), (*decl.sig).clone(), decl.visibility)
                    }),
            );
            super::counted::sort_unstable_by(&self.meter, &mut methods, |a, b| a.0.cmp(&b.0))
                .ok()?;
            classes.push(ExportedClass {
                id: ns as NsId,
                name: namespace.name.clone(),
                methods,
            });
        }
        Some(Exported {
            types: std::mem::replace(&mut self.types, Types::new()),
            functions,
            enums,
            classes,
        })
    }

    /// Imports what a required file exports: its enums, bound by name
    /// where the name is free, as the runtime binds them; its classes,
    /// whose instances its functions may return but whose names stay
    /// private to it; and its functions, typed in this check's types.
    fn import(
        &mut self,
        exported: &Exported,
    ) -> (CountedMap<String, Rc<Sig>>, CountedMap<String, u32>) {
        // Held by the caller, at the lengths they take.
        let mut imports = Imports {
            enums: Vec::with_capacity(exported.enums.len()),
            classes: HashMap::with_capacity(exported.classes.len()),
        };
        let mut enums = CountedMap::new();
        for declared in &exported.enums {
            // A check this stops imports no more, and its caller none of
            // what it imported.
            if self.meter.charge(1) {
                break;
            }
            // The enum, its copies of its name and room for it in every
            // table it goes in are counted before any changes.
            let declarations = self.meter.declarations();
            let program = &mut self.program;
            let free = !program.enum_names.contains_key(&declared.name)
                && !program.roots.contains_key(declared.name.as_str());
            let Ok(mut kept) = declarations.keep(3 * declared.name.len()) else {
                break;
            };
            if program.enums.reserve(declarations, 1).is_err()
                || self.types.names.enums.reserve(declarations, 1).is_err()
                || (free && program.enum_names.reserve(declarations, 1).is_err())
                || enums.reserve(declarations, 1).is_err()
            {
                break;
            }
            let id = program.enums.len() as u32;
            program.enums.push_within(declared.clone());
            self.types
                .names
                .enums
                .push_kept(&mut kept, declared.name.clone());
            if free {
                program
                    .enum_names
                    .insert_kept(&mut kept, declared.name.clone(), id);
            }
            enums.insert_kept(&mut kept, declared.name.clone(), id);
            imports.enums.push(id);
        }
        for class in &exported.classes {
            // The class, its two copies of its name and room for it in both
            // tables are counted before either changes.
            let declarations = self.meter.declarations();
            let Ok(mut kept) = declarations.keep(2 * class.name.len()) else {
                break;
            };
            if self.program.namespaces.reserve(declarations, 1).is_err()
                || self
                    .types
                    .names
                    .namespaces
                    .reserve(declarations, 1)
                    .is_err()
            {
                break;
            }
            let id = self.program.namespaces.len() as NsId;
            self.types
                .names
                .namespaces
                .push_kept(&mut kept, class.name.clone());
            self.program.namespaces.push_kept(
                &mut kept,
                Namespace {
                    checked: false,
                    module: None,
                    name: class.name.clone(),
                    parent: None,
                    is_class: true,
                    methods: CountedMap::new(),
                    statics: CountedMap::new(),
                    ivars: CountedMap::new(),
                    children: CountedMap::new(),
                },
            );
            imports.classes.insert(class.id, id);
        }
        for class in &exported.classes {
            // A check that importing stops imports no more, and visits no
            // more classes.
            if self.halted() {
                break;
            }
            // A class the budget refused to import has no methods either.
            let Some(&owner) = imports.classes.get(&class.id) else {
                break;
            };
            for (name, sig, visibility) in &class.methods {
                // A check that importing stops imports no more.
                if self.halted() {
                    break;
                }
                // The method, its name, its copy of the signature, which
                // holds no more than the file's, and room for it in both
                // tables are counted before any is made.
                let declarations = self.meter.declarations();
                let methods = &mut self.program.namespaces[owner as usize].methods;
                let Ok(mut kept) = declarations.keep(name.len() + sig.heap()) else {
                    break;
                };
                if methods.reserve(declarations, 1).is_err()
                    || self.program.fns.reserve(declarations, 1).is_err()
                {
                    break;
                }
                let Some(sig) = self.import_sig(&exported.types, sig, &imports) else {
                    break;
                };
                let sig = Rc::new(sig);
                let id = self.program.fns.len();
                self.program.fns.push_within(FnDecl {
                    def: None,
                    owner: Some(owner),
                    instance: true,
                    sig,
                    main: false,
                    visibility: *visibility,
                });
                self.program.namespaces[owner as usize].methods.insert_kept(
                    &mut kept,
                    name.clone(),
                    id,
                );
            }
        }
        let mut functions = CountedMap::new();
        for (name, sig) in &exported.functions {
            if self.halted() {
                break;
            }
            // Its copy of the signature, which holds no more than the
            // file's, is counted before it is made, and its name, with its
            // room in the table, as the table takes it.
            if self.meter.declarations().keep(sig.heap()).is_err() {
                break;
            }
            let Some(sig) = self.import_sig(&exported.types, sig, &imports) else {
                break;
            };
            let declarations = self.meter.declarations();
            if functions
                .insert_made(declarations, name.len(), || name.clone(), Rc::new(sig))
                .is_err()
            {
                break;
            }
        }
        if self.declaring() {
            return (CountedMap::new(), CountedMap::new());
        }
        (functions, enums)
    }

    /// A required file's signature in this check's types, which its caller
    /// counts before it is made; `None` once the check stops, which makes
    /// no more of it.
    fn import_sig(&mut self, from: &Types, sig: &Sig, imports: &Imports) -> Option<Sig> {
        let mut params = Vec::with_capacity(sig.params.len());
        for param in &sig.params {
            let ty = self.import_ty(from, param.ty, imports)?;
            params.push(Param {
                ty,
                ..param.clone()
            });
        }
        let block = match &sig.block {
            Some(block) => Some(BlockSig {
                params: self.import_all(from, &block.params, imports)?,
                rest: self.import_some(from, block.rest, imports)?,
                result: self.import_some(from, block.result, imports)?,
                optional: block.optional,
            }),
            None => None,
        };
        Some(Sig {
            name: sig.name.clone(),
            params,
            result: self.import_some(from, sig.result, imports)?,
            block,
            vars: Vec::new(),
            breaks: sig.breaks,
            converts: sig.converts,
            id: None,
        })
    }

    /// [`Self::import_ty`] of each of `types`, in a list made at their
    /// length; `None` once the check stops.
    fn import_all(&mut self, from: &Types, types: &[Ty], imports: &Imports) -> Option<Vec<Ty>> {
        let mut imported = Vec::with_capacity(types.len());
        for &ty in types {
            imported.push(self.import_ty(from, ty, imports)?);
        }
        Some(imported)
    }

    /// [`Self::import_ty`] of a type that may be absent; `None` once the
    /// check stops.
    fn import_some(
        &mut self,
        from: &Types,
        ty: Option<Ty>,
        imports: &Imports,
    ) -> Option<Option<Ty>> {
        match ty {
            Some(ty) => self.import_ty(from, ty, imports).map(Some),
            None => Some(None),
        }
    }

    /// A type of a required file's table in this check's: its enums and
    /// classes become the ones imported from it, and a type the file could
    /// not resolve, or one of a file it requires in turn, becomes `any`.
    /// The list a shape's, a tuple's or a union's parts are copied into is
    /// held, with the names it copies, before it is made. `None` once the
    /// check stops, which imports no more of it.
    fn import_ty(&mut self, from: &Types, ty: Ty, imports: &Imports) -> Option<Ty> {
        if self.meter.charge(1) {
            return None;
        }
        let imported = match &*from.shared(ty) {
            Kind::Error | Kind::Namespace(_) | Kind::Exports(_) => Ty::ANY,
            Kind::Array(element) => {
                let element = self.import_ty(from, *element, imports)?;
                self.types.array(element)
            }
            Kind::Hash(value) => {
                let value = self.import_ty(from, *value, imports)?;
                self.types.hash(value)
            }
            Kind::Shape(fields, open) => {
                let names: usize = fields.iter().map(|field| field.name.len()).sum();
                let held = self.hold(std::mem::size_of_val(&**fields) + names)?;
                let copied = self.import_fields(from, fields, imports);
                let shape = copied.map(|fields| self.types.shape(fields, *open));
                self.release(held);
                shape?
            }
            Kind::Tuple(items) => {
                let held = self.hold(std::mem::size_of_val(&**items))?;
                let copied = self.import_all(from, items, imports);
                let tuple = copied.map(|items| self.types.tuple(items));
                self.release(held);
                tuple?
            }
            Kind::Union(items) => {
                let held = self.hold(std::mem::size_of_val(&**items))?;
                let copied = self.import_all(from, items, imports);
                let union = copied.map(|items| self.types.union(&items));
                self.release(held);
                union?
            }
            Kind::TypeLit(described) => {
                let described = self.import_ty(from, *described, imports)?;
                self.types.type_lit(described)
            }
            Kind::Instance(ns) => match imports.classes.get(ns) {
                Some(&id) => self.types.intern(Kind::Instance(id)),
                None => Ty::ANY,
            },
            Kind::EnumValue(id) | Kind::EnumType(id) => {
                let Some(&imported) = imports.enums.get(*id as usize) else {
                    return Some(Ty::ANY);
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
        };
        // A check the table stopped as it took the type imports no more.
        (!self.halted()).then_some(imported)
    }

    /// [`Self::import_ty`] of each of a shape's `fields`, with a copy of each
    /// name, in a list made at their length; `None` once the check stops.
    fn import_fields(
        &mut self,
        from: &Types,
        fields: &[Field],
        imports: &Imports,
    ) -> Option<Vec<Field>> {
        let mut imported = Vec::with_capacity(fields.len());
        for field in fields {
            imported.push(Field {
                name: field.name.clone(),
                ty: self.import_ty(from, field.ty, imports)?,
                optional: field.optional,
            });
        }
        Some(imported)
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
            text!(self, "the module \"{path}\" exports no function `{name}`"),
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
    out: &mut ScratchVec<(String, Option<String>, usize)>,
    found: &mut usize,
) {
    // The body is a visit, empty or not; the requests count themselves.
    if walk.visit(0) {
        return;
    }
    walk.stmts(body, ());
    while let Some((item, ())) = walk.next(0) {
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
    out: &mut ScratchVec<(String, Option<String>, usize)>,
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
                    out.add((path, alias, expr.offset as usize));
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
