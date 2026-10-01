//! The declarations of one program: functions, classes, modules, enums and
//! type aliases, with every signature resolved to types before any body is
//! checked, so a call reads its callee's signature and never its body.

use super::{
    Checker,
    counted::{
        CountedBTreeSet, CountedMap, CountedSet, CountedVec, ScratchMap, ScratchSet, ScratchVec,
    },
    meter::{Heap, map, vec},
    sigs::{self, BlockSig, Param, ParamKind, Sig},
    ty::{Field, Kind, Ty},
};
use crate::{
    compilation::{self, TypeKind},
    diagnostic::{Code, Diagnostic, Fix},
    syntax::{
        BlockParam, Declarations, Definition,
        modules::{Module, Visibility},
    },
    types::Scalar,
};
use std::{collections::HashMap, rc::Rc, sync::Arc};

pub(crate) type FnId = usize;
pub(crate) type NsId = u32;

/// A script function or method.
pub(crate) struct FnDecl<'a> {
    /// Its definition, or none for a method of a class a required file
    /// declares.
    pub def: Option<&'a Definition>,
    /// The class or module that declares it.
    pub owner: Option<NsId>,
    /// Whether it is an instance method, rather than a top-level function or
    /// a `def self.` method.
    pub instance: bool,
    pub sig: Rc<Sig>,
    /// Whether it is the top-level statements.
    pub main: bool,
    /// Who may call it with a receiver; a function outside a class is public.
    pub visibility: Visibility,
}

/// An instance variable a class declares, directly or with a property.
#[derive(Clone, Copy)]
pub(crate) struct Ivar {
    pub ty: Ty,
    pub default: bool,
}

/// A script class or module.
pub(crate) struct Namespace<'a> {
    pub checked: bool,
    /// Its declaration, or none for a class a required file declares.
    pub module: Option<&'a Module>,
    pub name: String,
    pub parent: Option<NsId>,
    pub is_class: bool,
    pub methods: CountedMap<String, FnId>,
    pub statics: CountedMap<String, FnId>,
    pub ivars: CountedMap<String, Ivar>,
    pub children: CountedMap<&'a str, NsId>,
}

/// A script enum, which a required file's importers share.
pub(crate) struct Enum {
    pub name: String,
    pub members: Vec<String>,
    /// Each member's symbol, as `:in_review` names `InReview`.
    pub symbols: Vec<String>,
    /// The positions of the members in the order of their names, and of
    /// the symbols in the order of the symbols, which naming one searches
    /// rather than scanning them all.
    by_member: Vec<u32>,
    by_symbol: Vec<u32>,
}

impl Enum {
    pub fn new(name: String, members: Vec<String>) -> Self {
        let symbols: Vec<String> = members.iter().map(|m| crate::enums::symbol(m)).collect();
        let sorted = |names: &[String]| {
            let mut order: Vec<u32> = (0..names.len() as u32).collect();
            order.sort_unstable_by(|&a, &b| names[a as usize].cmp(&names[b as usize]));
            order
        };
        Self {
            name,
            by_member: sorted(&members),
            by_symbol: sorted(&symbols),
            members,
            symbols,
        }
    }

    /// The position of the member named `name`.
    pub fn member(&self, name: &str) -> Option<usize> {
        find(&self.members, &self.by_member, name)
    }

    /// The position of the member whose symbol is `symbol`.
    pub fn symbol(&self, symbol: &str) -> Option<usize> {
        find(&self.symbols, &self.by_symbol, symbol)
    }
}

/// The position of `name` among `names`, which `order` sorts. Enum
/// members are unique, and so are their symbols.
fn find(names: &[String], order: &[u32], name: &str) -> Option<usize> {
    let at = order
        .binary_search_by(|&index| names[index as usize].as_str().cmp(name))
        .ok()?;
    Some(order[at] as usize)
}

impl Heap for Enum {
    fn heap(&self) -> usize {
        self.name.heap()
            + self.members.heap()
            + self.symbols.heap()
            + self.by_member.heap()
            + self.by_symbol.heap()
    }
}

/// Its signature, which an `Rc` shares, is counted where it is made.
impl super::counted::Owned for FnDecl<'_> {
    fn owned(&self) -> usize {
        0
    }
}

/// Its name; its tables are counted as they grow.
impl super::counted::Owned for Namespace<'_> {
    fn owned(&self) -> usize {
        self.name.capacity()
    }
}

impl super::counted::Owned for Ivar {
    fn owned(&self) -> usize {
        0
    }
}

impl Heap for FnDecl<'_> {
    fn heap(&self) -> usize {
        self.sig.heap()
    }
}

impl Heap for Namespace<'_> {
    fn heap(&self) -> usize {
        self.name.heap()
            + self.methods.heap()
            + self.statics.heap()
            + self.ivars.heap()
            + self.children.heap()
    }
}

impl Heap for Ivar {
    fn heap(&self) -> usize {
        0
    }
}

/// Everything a program declares.
#[derive(Default)]
pub(crate) struct Program<'a> {
    pub file: bool,
    /// A required file's top-level locals, which its functions and methods
    /// see: each one's type and where it is declared.
    pub file_locals: CountedMap<String, (Ty, usize)>,
    /// The names a required file's functions and methods assign, which a
    /// call of script code may change.
    pub file_written: CountedSet<String>,
    /// Each call of the file's own code in its body, which may run before
    /// the file assigns the top-level locals that code reads.
    pub file_calls: CountedVec<super::check::FileCall>,
    /// The file's top-level locals each of its functions and methods
    /// reads, and the others it calls.
    pub file_uses: CountedMap<FnId, (CountedBTreeSet<String>, CountedVec<FnId>)>,
    pub fns: CountedVec<FnDecl<'a>>,
    /// Top-level functions by name.
    pub functions: CountedMap<&'a str, FnId>,
    pub namespaces: CountedVec<Namespace<'a>>,
    /// Top-level classes and modules by name.
    pub roots: CountedMap<&'a str, NsId>,
    pub enums: CountedVec<Arc<Enum>>,
    pub enum_names: CountedMap<String, u32>,
    /// Type aliases by declaring namespace (none at the top level) and name.
    pub aliases: CountedMap<(Option<NsId>, &'a str), &'a compilation::Type>,
    alias_types: CountedMap<(Option<NsId>, String), Ty>,
    /// Namespaces by the offset of their `class` or `module` keyword.
    pub by_offset: CountedMap<u32, NsId>,
    /// Host functions registered on the engine.
    pub hosts: CountedMap<String, Rc<Sig>>,
    pub declared_calls: CountedSet<String>,
    /// Globals and capabilities the host declares, as values, by name.
    pub declared: CountedMap<String, Ty>,
    /// Capabilities the host declares with members, by `Kind::Host` index.
    pub host_modules: CountedVec<&'a crate::signatures::Module>,
}

impl Program<'_> {
    /// What the declarations hold, but for the tables checking grows,
    /// which [`Self::grown`] counts.
    pub fn heap(&self) -> usize {
        self.file_locals.heap()
            + self.file_written.heap()
            + self.fns.heap()
            + self.functions.heap()
            + self.namespaces.heap()
            + self.roots.heap()
            + self.enums.heap()
            + self.enum_names.heap()
            + self.aliases.heap()
            + self.by_offset.heap()
            + self.hosts.heap()
            + self.declared_calls.heap()
            + self.declared.heap()
            + vec(self.host_modules.as_vec())
    }

    /// The storage of the tables checking grows, whose elements' payloads
    /// the checker counts as it adds them.
    pub fn grown(&self) -> usize {
        map(&self.alias_types) + vec(self.file_calls.as_vec()) + map(&self.file_uses)
    }
}

/// A retained alias's text as it is written, a step a byte, in a list
/// counted before it grows and while it lives.
struct AliasText<'m> {
    text: ScratchVec<u8>,
    meter: &'m super::meter::Meter,
}

impl crate::shapes::TypeWriter for AliasText<'_> {
    fn write(&mut self, bytes: &[u8]) -> crate::Result<()> {
        if self.meter.charge(bytes.len() as u64) || self.text.extend_from_slice(bytes).is_err() {
            return Err(crate::Error::new(
                crate::ErrorKind::Steps,
                "the check ran out of its budget",
            ));
        }
        Ok(())
    }
}

impl<'a> Checker<'a> {
    /// Verifies the source contracts of explicitly retained script type values.
    pub(super) fn check_retained_declarations(
        &mut self,
        declared: &crate::declared::Declarations,
        parsed: &crate::syntax::Declarations,
    ) {
        if !declared
            .values()
            .any(|declaration| declaration.retained().is_some())
        {
            return;
        }
        // Each alias's text is written a step a byte into a list counted
        // before it grows and while it lives, which stops once the check
        // does, and kept by name in a map counted while it lives.
        let mut aliases = ScratchMap::new(&self.meter);
        for (scope, alias) in &parsed.additions.aliases {
            if scope.is_none() {
                let mut text = AliasText {
                    text: ScratchVec::new(&self.meter),
                    meter: &self.meter,
                };
                if crate::shapes::format(&alias.ty, &mut text).is_err()
                    || aliases.insert(alias.name.as_str(), text.text).is_err()
                {
                    return;
                }
            }
        }
        // The tables of what the source declares, counted before they are
        // made.
        let outline = parsed.outline.len();
        let tables =
            super::meter::table::<(&str, &str)>(outline) + super::meter::table::<&str>(outline);
        if self.transient(tables) {
            return;
        }
        let carried: std::collections::HashMap<_, _> = parsed
            .outline
            .iter()
            .map(|declaration| {
                (
                    declaration.name.as_str(),
                    &self.source[declaration.start..declaration.end],
                )
            })
            .collect();
        let nominal: std::collections::HashSet<_> = parsed
            .outline
            .iter()
            .filter(|declaration| declaration.kind != crate::DeclarationKind::Function)
            .map(|declaration| declaration.name.as_str())
            .collect();
        if self.transient(map(&carried) + nominal.heap()) {
            return;
        }
        for (name, declaration) in declared {
            let Some((value, retained)) = declaration.retained() else {
                continue;
            };
            // A check its charges stop compares no more.
            if self.halted() {
                return;
            }
            let valid = match &value.0 {
                crate::value::Kind::Namespace(namespace) => {
                    namespace.definition.name == *name
                        && retained.is_some_and(|retained| {
                            retained.aliases.iter().all(|(name, ty)| {
                                !self.meter.charge(ty.len() as u64)
                                    && aliases
                                        .get(name.as_str())
                                        .is_some_and(|text| **text == ty[..])
                            }) && retained.declarations.iter().all(|(name, source)| {
                                if self.meter.charge(source.len() as u64) {
                                    return false;
                                }
                                let bound = !nominal.contains(name.as_str())
                                    || declared
                                        .get(name)
                                        .and_then(crate::declared::Declaration::retained)
                                        .is_some();
                                bound
                                    && carried
                                        .get(name.as_str())
                                        .is_some_and(|&found| found == source)
                            })
                        })
                }
                crate::value::Kind::Enum(enumeration) => self
                    .program
                    .enum_names
                    .get(name.as_str())
                    .is_some_and(|&id| {
                        let found = &self.program.enums[id as usize];
                        !self
                            .meter
                            .charge(enumeration.definition.members.len() as u64)
                            && enumeration.definition.name == *name
                            && found.members.iter().eq(enumeration
                                .definition
                                .members
                                .iter()
                                .map(|member| &member.name))
                    }),
                _ => false,
            };
            if !valid {
                self.report(Diagnostic::error(Code::TYPE_MISMATCH, crate::diagnostic::Span::at(0),
                    text!(self, "retained declaration `{name}` must keep its original declarations and enum members")));
            }
        }
    }

    /// Types the globals and capabilities the host declares: a global or a
    /// capability's data by its type, a capability with members as a
    /// namespace of them, and a callable capability as a host function.
    pub(super) fn declare_hosts(&mut self, declared: &'a crate::declared::Declarations) {
        for (name, declaration) in declared {
            if self.meter.charge(1) {
                return;
            }
            if declaration.retained().is_some() {
                continue;
            }
            // Each declaration, its copies of the name and room for them
            // in the tables they go in are counted before any changes; a
            // check they stop declares no more.
            let declarations = self.meter.declarations();
            let program = &mut self.program;
            match &declaration.item {
                crate::signatures::Item::Constant(constant) => {
                    let ty = sigs::table_type(&mut self.types, &constant.ty, &[]);
                    let Ok(mut kept) = declarations.keep(name.len()) else {
                        return;
                    };
                    if program.declared.reserve(declarations, 1).is_err() {
                        return;
                    }
                    program.declared.insert_kept(&mut kept, name.clone(), ty);
                }
                crate::signatures::Item::Module(module) => {
                    let Ok(mut kept) = declarations.keep(2 * name.len()) else {
                        return;
                    };
                    if program.host_modules.reserve(declarations, 1).is_err()
                        || self.types.names.hosts.reserve(declarations, 1).is_err()
                        || program.declared.reserve(declarations, 1).is_err()
                    {
                        return;
                    }
                    let id = program.host_modules.len() as u32;
                    program.host_modules.push_within(module);
                    self.types.names.hosts.push_kept(&mut kept, name.clone());
                    let ty = self.types.intern(Kind::Host(id));
                    self.program
                        .declared
                        .insert_kept(&mut kept, name.clone(), ty);
                }
                crate::signatures::Item::Function(function) => {
                    let sig = Rc::new(
                        self.converter
                            .convert_owned(&mut self.types, function, None)
                            .host(),
                    );
                    let Ok(mut kept) = declarations.keep(2 * name.len() + sig.heap()) else {
                        return;
                    };
                    if program.declared_calls.reserve(declarations, 1).is_err()
                        || program.hosts.reserve(declarations, 1).is_err()
                    {
                        return;
                    }
                    program.declared_calls.insert_kept(&mut kept, name.clone());
                    program.hosts.insert_kept(&mut kept, name.clone(), sig);
                }
                _ => (),
            }
        }
    }

    /// Whether an enum of `members` fits the budget: building it copies
    /// each member, writes its symbol and sorts both, which is charged, and
    /// what it keeps counted, before it is built.
    fn enum_fits(&mut self, members: &[crate::compilation::Name]) -> bool {
        let count = members.len();
        let sorting = count * (usize::BITS - count.leading_zeros()) as usize;
        if self.types.work(count + 2 * sorting) {
            return false;
        }
        let bytes = members.iter().map(|member| 3 * member.len()).sum::<usize>()
            + count * (2 * std::mem::size_of::<String>() + 2 * std::mem::size_of::<u32>());
        self.meter.declarations().keep(bytes).is_ok() && !self.over_budget()
    }

    /// Collects the declarations and resolves every signature.
    pub(super) fn declare_program(&mut self, parsed: &'a Declarations) {
        // Every enum is declared, since the file's enums are found by
        // their places, but a check that stops declares the rest without
        // members.
        let mut stopped = self.halted();
        for (index, (name, members)) in parsed.enums.iter().enumerate() {
            // Its places are kept however the budget stands, and counted:
            // its names as the tables take them, and the enum an `Arc`
            // shares here.
            let always = self.meter.declarations().regardless();
            always.kept(name.len() + std::mem::size_of::<Enum>() + 16);
            self.program
                .enum_names
                .insert_regardless(always, name.to_string(), index as u32);
            // A check past its budget declares the enum without members,
            // which it never reads.
            let members = if !stopped && self.enum_fits(members) {
                members.iter().map(|m| m.to_string()).collect()
            } else {
                Vec::new()
            };
            let always = self.meter.declarations().regardless();
            self.program
                .enums
                .push_regardless(always, Arc::new(Enum::new(name.to_string(), members)));
            self.types
                .names
                .enums
                .push_regardless(always, name.to_string());
            stopped = self.declaring();
        }
        // A check that runs out of its budget stops declaring.
        for (index, module) in parsed.modules.iter().enumerate() {
            if self.paced(index) || self.namespace(module, None) {
                return;
            }
        }
        for (scope, alias) in &parsed.additions.aliases {
            let scope = scope.and_then(|offset| self.program.by_offset.get(&offset).copied());
            let declarations = self.meter.declarations();
            if self
                .program
                .aliases
                .insert(declarations, (scope, alias.name.as_str()), &alias.ty)
                .is_err()
            {
                return;
            }
        }
        // The builtin namespaces' names have their places, which are kept
        // however the budget stands.
        for (index, (name, _)) in sigs::index().modules.iter().enumerate() {
            debug_assert_eq!(self.types.names.builtins.len(), index);
            let always = self.meter.declarations().regardless();
            self.types
                .names
                .builtins
                .push_regardless(always, (*name).to_owned());
        }
        // Required files' enums are types in annotations too.
        self.require_modules(parsed);
        // Each function's typed block parameter, by the offset of its `def`,
        // found once rather than by searching every one for each function:
        // the first one recorded for it, as a search finds, counted while
        // the functions are declared.
        let recorded = &parsed.additions.blocks;
        let Some(blocks_held) =
            self.hold(super::meter::table::<(u32, &BlockParam)>(recorded.len()))
        else {
            return;
        };
        let mut blocks: HashMap<u32, &BlockParam> = HashMap::with_capacity(recorded.len());
        // Each entry is a step, and the budget is checked as a walk checks
        // it, while the index is filled.
        let pace = super::walk::PACE as usize;
        for (index, (owner, block)) in recorded.iter().enumerate() {
            if index % pace == pace - 1 && self.meter.pace(pace as u64, 0) {
                self.release(blocks_held);
                return;
            }
            blocks.entry(*owner).or_insert(block);
        }
        if self.meter.charge((recorded.len() % pace) as u64) {
            self.release(blocks_held);
            return;
        }
        let block_param = |offset: u32| blocks.get(&offset).copied();
        // Signatures after every name is known, so annotations resolve.
        for (index, def) in parsed.functions.iter().enumerate() {
            if self.paced(index) {
                return;
            }
            let main = index == 0;
            let block = (!main).then(|| block_param(def.offset)).flatten();
            let Some(id) = self.function(def, None, false, block, main, Visibility::Public) else {
                self.release(blocks_held);
                return;
            };
            let declarations = self.meter.declarations();
            if !main
                && self
                    .program
                    .functions
                    .insert(declarations, def.name.as_str(), id)
                    .is_err()
            {
                self.release(blocks_held);
                return;
            }
        }
        // The methods declared so far, whose walks for yields charge a
        // step each.
        let mut methods = 0;
        for ns in 0..self.program.namespaces.len() {
            if self.halted() {
                return;
            }
            let Some(module) = self.program.namespaces[ns].module else {
                continue;
            };
            for (def, visibility) in &module.instance_methods {
                methods += 1;
                if self.paced(methods) {
                    return;
                }
                let block = block_param(def.offset);
                let Some(id) =
                    self.function(def, Some(ns as NsId), true, block, false, *visibility)
                else {
                    self.release(blocks_held);
                    return;
                };
                if !self.declare_method(ns, def.name.as_str(), id, false) {
                    self.release(blocks_held);
                    return;
                }
            }
            for (def, visibility) in &module.methods {
                methods += 1;
                if self.paced(methods) {
                    return;
                }
                let block = block_param(def.offset);
                let Some(id) =
                    self.function(def, Some(ns as NsId), false, block, false, *visibility)
                else {
                    self.release(blocks_held);
                    return;
                };
                if !self.declare_method(ns, def.name.as_str(), id, true) {
                    self.release(blocks_held);
                    return;
                }
            }
        }
        self.release(blocks_held);
        // Which instance variables have defaults, found by class and offset
        // rather than by scanning every default for each variable.
        if self.transient(super::meter::table::<(u32, u32)>(
            parsed.additions.defaults.len(),
        )) {
            return;
        }
        let defaults: std::collections::HashSet<(u32, u32)> = parsed
            .additions
            .defaults
            .iter()
            .map(|(owner, stmt)| (*owner, stmt.offset))
            .collect();
        if self.transient(super::meter::set(&defaults)) {
            return;
        }
        for (class, ivar) in &parsed.additions.ivars {
            let Some(&ns) = self.program.by_offset.get(class) else {
                continue;
            };
            let default = defaults.contains(&(*class, ivar.offset));
            let ty = self.annotation(&ivar.ty, Some(ns), ivar.offset as usize);
            if !self.declare_ivar(ns as usize, &ivar.name, Ivar { ty, default }, true) {
                return;
            }
        }
        // Declared class variables have their types before any body reads them.
        for (namespace, declared) in &parsed.additions.class_vars {
            let Some(&ns) = self.program.by_offset.get(namespace) else {
                continue;
            };
            let ty = self.annotation(&declared.ty, Some(ns), declared.offset as usize);
            if self.keep_constant((Some(ns), self.copy(&declared.name)), ty) {
                return;
            }
        }
        // Properties declare their instance variables and their types.
        for ns in 0..self.program.namespaces.len() {
            let Some(module) = self.program.namespaces[ns].module else {
                continue;
            };
            for (def, _) in &module.instance_methods {
                let Some((name, setter)) = &def.accessor else {
                    continue;
                };
                let typed = match setter {
                    true => def.params.first().is_some_and(|param| param.ty.is_some()),
                    false => def.return_type.is_some(),
                };
                if !typed
                    && !self.program.namespaces[ns]
                        .ivars
                        .contains_key(name.as_str())
                {
                    let span = self.spans.word_after(def.offset as usize, name);
                    self.report(Diagnostic::error(
                        Code::MISSING_PARAMETER_TYPE,
                        span,
                        text!(
                            self,
                            "property `{name}` has no type; declare it as `{name}: T`"
                        ),
                    ));
                }
                let id = self.program.namespaces[ns].methods[def.name.as_str()];
                let sig = self.program.fns[id].sig.clone();
                let ty = if *setter {
                    sig.params.first().map(|p| p.ty)
                } else {
                    sig.result
                };
                let ty = ty.unwrap_or(Ty::ANY);
                // A getter's result must hold the variable's values, and a
                // setter's parameter must be one of them, whichever
                // declaration came first.
                let existing = self.program.namespaces[ns]
                    .ivars
                    .get(name.as_str())
                    .map(|ivar| ivar.ty);
                // A method defined with the accessor's name replaces it,
                // and its body's writes are checked as any method's are.
                let replaced = !self.program.fns[id]
                    .def
                    .is_some_and(|method| std::ptr::eq(method, def));
                if let (Some(declared), false) = (existing, replaced) {
                    let fits = if *setter {
                        self.types.assignable(ty, declared)
                    } else {
                        self.types.assignable(declared, ty)
                    };
                    if !fits && typed {
                        let span = self.spans.word_after(def.offset as usize, name);
                        let declared_text = self.types.display(declared);
                        let found = self.types.display(ty);
                        let (what, how) = if *setter {
                            ("setter", "takes")
                        } else {
                            ("getter", "returns")
                        };
                        self.report(
                            Diagnostic::error(
                                Code::TYPE_MISMATCH,
                                span,
                                text!(self,
                                    "the {what} `{name}` {how} {found}, but `@{name}` is {declared_text}; declare them with one type"
                                ),
                            )
                            .with_types(declared_text, found),
                        );
                    }
                }
                if !self.declare_ivar(ns, name, Ivar { ty, default: false }, false) {
                    return;
                }
            }
        }
        self.check_names(parsed);
    }

    /// Declares method `name` of namespace `ns`, a static one when
    /// `statics`, with a copy of its name, counted as the table takes it.
    /// Returns whether it did: a check the budget stops declares no more.
    fn declare_method(&mut self, ns: usize, name: &str, id: FnId, statics: bool) -> bool {
        let declarations = self.meter.declarations();
        let namespace = &mut self.program.namespaces[ns];
        let table = if statics {
            &mut namespace.statics
        } else {
            &mut namespace.methods
        };
        if let Some(entry) = table.get_mut(name) {
            *entry = id;
            return true;
        }
        table.insert(declarations, name.to_owned(), id).is_ok()
    }

    /// Declares instance variable `name` of namespace `ns`, with a copy of
    /// its name, counted as the table takes it; one declared already is
    /// replaced when `replace`, and kept otherwise. Returns whether it did:
    /// a check the budget stops declares no more.
    fn declare_ivar(&mut self, ns: usize, name: &str, ivar: Ivar, replace: bool) -> bool {
        let declarations = self.meter.declarations();
        let ivars = &mut self.program.namespaces[ns].ivars;
        if let Some(entry) = ivars.get_mut(name) {
            if replace {
                *entry = ivar;
            }
            return true;
        }
        ivars.insert(declarations, name.to_owned(), ivar).is_ok()
    }

    /// Reports a class alias that takes the name of a method the class
    /// already defines, which would replace it, and a function named
    /// `require`, which would shadow the `require` the compiler resolves.
    /// A method may take the name, since a receiver calls it.
    fn check_names(&mut self, parsed: &'a Declarations) {
        for item in &parsed.outline {
            if item.kind == crate::DeclarationKind::Function && item.name == "require" {
                let span = self.spans.word_after(item.start, "require");
                self.reserved(span);
            }
        }
        for ns in 0..self.program.namespaces.len() {
            let Some(module) = self.program.namespaces[ns].module else {
                continue;
            };
            // Only an alias can repeat an earlier method's name. The aliases
            // are listed in the order of the methods they add, so each is
            // found by search, and the names before each method are kept in
            // a set, counted while it lives.
            if module.aliases.is_empty() {
                continue;
            }
            let alias = |index: usize| {
                module
                    .aliases
                    .binary_search_by_key(&index, |&(alias, _)| alias)
                    .ok()
                    .map(|at| module.aliases[at].1 as usize)
            };
            let mut earlier = ScratchSet::new(&self.meter);
            for (index, (def, _)) in module.instance_methods.iter().enumerate() {
                let repeated = def.accessor.is_none() && earlier.contains(def.name.as_str());
                let offset = if repeated { alias(index) } else { None };
                if earlier.insert(def.name.as_str()).is_err() {
                    return;
                }
                if let Some(offset) = offset {
                    let span = self.spans.word_after(offset, &def.name);
                    self.report(Diagnostic::error(
                        Code::DUPLICATE_NAME,
                        span,
                        text!(
                            self,
                            "`{}` is already a method of `{}`; an alias takes a new name",
                            def.name,
                            module.name
                        ),
                    ));
                }
            }
        }
    }

    fn reserved(&mut self, span: crate::diagnostic::Span) {
        self.report(Diagnostic::error(
            Code::RESERVED_NAME,
            span,
            "`require` is reserved: the compiler resolves it statically, so a function cannot take its name",
        ));
    }

    /// Checks the budget once for every [`super::walk::PACE`] of the
    /// declarations a loop has visited, `count` of them so far, whose steps
    /// they charge themselves, as a walk does: the steps, the deadline and
    /// the cancellation. Returns whether the check has stopped.
    #[must_use = "the budget may have stopped the check, which must then do no more work"]
    pub(super) fn paced(&self, count: usize) -> bool {
        let pace = super::walk::PACE as usize;
        (count % pace == pace - 1 && self.meter.pace(0, 0)) || self.halted()
    }

    /// Declares `module` and the namespaces nested in it, each before its
    /// children. The walk keeps its place on the heap, since namespaces nest
    /// as deep as the parser allows: what is left of each level's
    /// namespaces, whichever many a level holds. It charges a step for each
    /// namespace, and stops with the check. Returns whether the check has
    /// stopped.
    #[must_use = "the budget may have stopped the check, which must then do no more work"]
    fn namespace(&mut self, module: &'a Module, parent: Option<NsId>) -> bool {
        let Some(first) = self.declare_namespace(module, parent) else {
            return true;
        };
        if self.declaring() {
            return true;
        }
        let mut levels = vec![(module.modules.iter().chain(module.inner.iter()), first)];
        // The namespaces declared since the meter was last charged.
        let mut unpaced = 1;
        while let Some((level, parent)) = levels.last_mut() {
            let parent = *parent;
            let Some(module) = level.next() else {
                levels.pop();
                continue;
            };
            if unpaced == 64 {
                unpaced = 0;
                if self.meter.pace(64, super::meter::vec(&levels)) {
                    break;
                }
            }
            let Some(id) = self.declare_namespace(module, Some(parent)) else {
                break;
            };
            unpaced += 1;
            if self.declaring() {
                break;
            }
            levels.push((module.modules.iter().chain(module.inner.iter()), id));
        }
        self.meter.charge(unpaced) || self.halted()
    }

    /// Declares `module` in `parent`, or at the top level. Its two copies
    /// of its qualified name, and room for it in every table it goes in,
    /// are counted before any changes; `None` when the budget refuses them,
    /// which stops the check and declares nothing.
    fn declare_namespace(&mut self, module: &'a Module, parent: Option<NsId>) -> Option<NsId> {
        let declarations = self.meter.declarations();
        let program = &mut self.program;
        let outer = parent.map(|parent| program.namespaces[parent as usize].name.as_str());
        let length = outer.map_or(0, |outer| outer.len() + 2) + module.name.len();
        let mut kept = declarations.keep(2 * length).ok()?;
        program.namespaces.reserve(declarations, 1).ok()?;
        self.types.names.namespaces.reserve(declarations, 1).ok()?;
        program.by_offset.reserve(declarations, 1).ok()?;
        match parent {
            Some(parent) => program.namespaces[parent as usize]
                .children
                .reserve(declarations, 1)
                .ok()?,
            None => program.roots.reserve(declarations, 1).ok()?,
        }
        let mut name = String::with_capacity(length);
        if let Some(parent) = parent {
            name.push_str(&program.namespaces[parent as usize].name);
            name.push_str("::");
        }
        name.push_str(&module.name);
        let id = program.namespaces.len() as NsId;
        self.types
            .names
            .namespaces
            .push_kept(&mut kept, name.clone());
        program.namespaces.push_kept(
            &mut kept,
            Namespace {
                checked: false,
                module: Some(module),
                name,
                parent,
                is_class: module.is_class,
                methods: CountedMap::new(),
                statics: CountedMap::new(),
                ivars: CountedMap::new(),
                children: CountedMap::new(),
            },
        );
        program.by_offset.insert_within(module.offset, id);
        match parent {
            Some(parent) => {
                program.namespaces[parent as usize]
                    .children
                    .insert_within(module.name.as_str(), id);
            }
            None => {
                program.roots.insert_within(module.name.as_str(), id);
            }
        }
        Some(id)
    }

    fn function(
        &mut self,
        def: &'a Definition,
        owner: Option<NsId>,
        instance: bool,
        block: Option<&'a BlockParam>,
        main: bool,
        visibility: Visibility,
    ) -> Option<FnId> {
        // The parameters, each with a copy of its name, the block's, and
        // the function's qualified name, counted before they are listed; a
        // check that stops declares no more functions.
        let qualified = owner.map_or(0, |ns| self.program.namespaces[ns as usize].name.len() + 1)
            + def.name.len();
        let held = self.hold(
            def.params.len() * std::mem::size_of::<Param>()
                + def
                    .params
                    .iter()
                    .map(|param| param.name.len())
                    .sum::<usize>()
                + block.map_or(0, |block| block.params.len()) * std::mem::size_of::<Ty>()
                + qualified,
        )?;
        let mut params = Vec::with_capacity(def.params.len());
        for (index, param) in def.params.iter().enumerate() {
            // A check its budget stops declares no more of them, and the
            // budget is checked as a walk checks it.
            if self.paced(index) {
                self.release(held);
                return None;
            }
            let kind = match param.kind {
                crate::syntax::ParamKind::Positional => ParamKind::Positional,
                crate::syntax::ParamKind::Rest => ParamKind::Rest,
                crate::syntax::ParamKind::Keyword => ParamKind::Keyword,
                crate::syntax::ParamKind::KeywordRest => ParamKind::KeywordRest,
            };
            let ty = match (&param.ty, &param.default) {
                (Some(ty), _) => self.annotation(ty, owner, def.offset as usize),
                (None, default) => {
                    // A literal default types the body's uses and the fix.
                    let literal = default.as_ref().map_or(Ty::ERROR, literal_type);
                    if !main && def.accessor.is_none() {
                        let span = self.spans.word_after(def.offset as usize, &param.name);
                        let mut diagnostic = Diagnostic::error(
                            Code::MISSING_PARAMETER_TYPE,
                            span,
                            text!(
                                self,
                                "parameter `{}` of `{}` has no type; declare it as `{}: T`",
                                param.name,
                                def.name,
                                param.name
                            ),
                        );
                        // A removed keyword form has a colon after the name,
                        // which its own diagnostic rewrites.
                        let colon = self.source[span.end..].trim_start().starts_with(':');
                        if literal != Ty::ERROR && !colon {
                            let ty = self.types.display(literal);
                            diagnostic = diagnostic.with_fix(Fix::insert(
                                text!(self, "declare `{}: {ty}`", param.name),
                                span.end,
                                text!(self, ": {ty}"),
                            ));
                        }
                        self.report(diagnostic);
                    }
                    literal
                }
            };
            let ty = match kind {
                // An unannotated rest parameter still collects an array or hash.
                ParamKind::Rest if param.ty.is_none() => self.types.array(Ty::ERROR),
                ParamKind::KeywordRest if param.ty.is_none() => self.types.hash(Ty::ERROR),
                _ => ty,
            };
            params.push(Param {
                name: param.name.to_string(),
                kind,
                ty,
                optional: param.default.is_some(),
            });
        }
        let result = match &def.return_type {
            Some(ty) => Some(self.annotation(ty, owner, def.offset as usize)),
            None if def.accessor.as_ref().is_some_and(|(_, setter)| !setter) => Some(Ty::ANY),
            None => None,
        };
        let block_sig = block.map(|block| BlockSig {
            params: block
                .params
                .iter()
                .map(|ty| self.annotation(ty, owner, block.offset as usize))
                .collect(),
            rest: None,
            result: block
                .result
                .as_ref()
                .map(|ty| self.annotation(ty, owner, block.offset as usize)),
            optional: block_optional(self.source, block),
        });
        let mut name = String::with_capacity(qualified);
        if let Some(ns) = owner {
            name.push_str(&self.program.namespaces[ns as usize].name);
            name.push(if instance { '#' } else { '.' });
        }
        name.push_str(&def.name);
        let (breaks, scratch) = yields(&self.meter, &def.body);
        if self.transient(scratch) {
            self.release(held);
            return None;
        }
        let sig = Rc::new(Sig {
            name,
            params,
            result,
            block: block_sig,
            vars: Vec::new(),
            breaks,
            converts: true,
            id: Some(self.program.fns.len()),
        });
        // The program counts them from here, and room for the function,
        // before they are let go.
        let declarations = self.meter.declarations();
        let kept = declarations.keep(sig.heap()).is_ok()
            && self.program.fns.reserve(declarations, 1).is_ok();
        self.release(held);
        if !kept {
            return None;
        }
        self.program.fns.push_within(FnDecl {
            def: Some(def),
            owner,
            instance,
            sig,
            main,
            visibility,
        });
        if self.declaring() {
            return None;
        }
        Some(self.program.fns.len() - 1)
    }

    /// Reports the parameters of top-level `function` that `count` string
    /// arguments cannot bind.
    pub(super) fn entry_arguments(&mut self, function: &str, count: usize) {
        let Some(&id) = self.program.functions.get(function) else {
            return;
        };
        let Some(def) = self.program.fns[id].def else {
            return;
        };
        let sig = self.program.fns[id].sig.clone();
        let strings = self.types.array(Ty::STRING);
        let mut index = 0;
        for param in &sig.params {
            if index >= count {
                break;
            }
            let (accepts, wanted) = match param.kind {
                ParamKind::Positional => {
                    index += 1;
                    (self.types.assignable(Ty::STRING, param.ty), "string")
                }
                ParamKind::Rest => {
                    index = count;
                    (self.types.assignable(strings, param.ty), "array<string>")
                }
                _ => continue,
            };
            if !accepts {
                let span = self.spans.word_after(def.offset as usize, &param.name);
                let declared = self.types.display(param.ty);
                self.report(
                    Diagnostic::error(
                        Code::TYPE_MISMATCH,
                        span,
                        text!(self,
                            "the command line passes strings, but `{}` of `{function}` is {declared}",
                            param.name
                        ),
                    )
                    .with_types(wanted, declared),
                );
            }
        }
    }

    /// Resolves an annotation in the scope of namespace `scope`, reporting
    /// unknown names at `offset`. An annotation that nests deeper than
    /// [`ANNOTATED`] levels, through its parts and the aliases it names,
    /// is unknown, and on WASI one taller than the syntax the checker
    /// descends into is refused as that syntax is.
    pub(super) fn annotation(
        &mut self,
        ty: &compilation::Type,
        scope: Option<NsId>,
        offset: usize,
    ) -> Ty {
        // A check past its budget builds no more types, such as each arm
        // of a wide union.
        if self.halted() {
            return Ty::ERROR;
        }
        if super::too_tall(self.annotating) {
            let span = self.spans.token(offset);
            self.too_deep(span);
            return Ty::ERROR;
        }
        if self.annotating >= ANNOTATED {
            return Ty::ERROR;
        }
        self.annotating += 1;
        let resolved = self.annotation_within(ty, scope, offset);
        self.annotating -= 1;
        resolved
    }

    /// [`Self::annotation`], one level deeper.
    fn annotation_within(
        &mut self,
        ty: &compilation::Type,
        scope: Option<NsId>,
        offset: usize,
    ) -> Ty {
        let base = match &ty.kind {
            TypeKind::Scalar(scalar) => match scalar {
                Scalar::Any => Ty::ANY,
                Scalar::Int => Ty::INT,
                Scalar::Float => Ty::FLOAT,
                Scalar::Number => Ty::NUMBER,
                Scalar::String => Ty::STRING,
                Scalar::Bool => Ty::BOOL,
                Scalar::Nil => Ty::NIL,
                Scalar::Duration => Ty::DURATION,
                Scalar::Time => Ty::TIME,
                Scalar::Money => Ty::MONEY,
                Scalar::Range => Ty::RANGE,
                Scalar::Symbol => Ty::SYMBOL,
                Scalar::Regex => Ty::REGEX,
                Scalar::MatchData => Ty::MATCH_DATA,
                Scalar::Error => Ty::ERROR_VALUE,
                Scalar::EnumValue => Ty::ANY_ENUM,
                Scalar::EnumType => Ty::ANY_ENUM_TYPE,
            },
            TypeKind::Array(element) => {
                let element = match element {
                    Some(element) => self.annotation(element, scope, offset),
                    None => Ty::ANY,
                };
                self.types.array(element)
            }
            TypeKind::Hash(pair) => {
                let value = match pair {
                    Some(pair) => {
                        self.annotation(&pair.0, scope, offset);
                        self.annotation(&pair.1, scope, offset)
                    }
                    None => Ty::ANY,
                };
                self.types.hash(value)
            }
            TypeKind::Shape(fields, open) => {
                // An annotation names each field once, so one of more fields
                // than a shape may have is too large before its fields are
                // typed and sorted.
                if fields.len() > super::ty::MAX_FIELDS {
                    self.types.too_large.get_or_insert(("shape", fields.len()));
                    self.too_large(offset);
                    return Ty::ERROR;
                }
                // Counted before they are listed; the table counts them from
                // when it takes them.
                let Some(held) = self.hold(
                    fields.len() * std::mem::size_of::<Field>()
                        + fields.iter().map(|field| field.name.len()).sum::<usize>(),
                ) else {
                    return Ty::ERROR;
                };
                let fields = fields
                    .iter()
                    .map(|field| Field {
                        name: String::from_utf8_lossy(&field.name).into(),
                        ty: self.annotation(&field.ty, scope, offset),
                        optional: field.optional,
                    })
                    .collect();
                self.release(held);
                let shape = self.types.shape(fields, *open);
                self.too_large(offset);
                shape
            }
            TypeKind::Union(options) => {
                // Counted while they are listed; the table counts them
                // while it joins them.
                let Some(held) = self.hold(options.len() * std::mem::size_of::<Ty>()) else {
                    return Ty::ERROR;
                };
                let options: Vec<Ty> = options
                    .iter()
                    .map(|option| self.annotation(option, scope, offset))
                    .collect();
                self.release(held);
                let union = self.types.union(&options);
                self.too_large(offset);
                union
            }
            TypeKind::Tuple(elements) => {
                // Counted while they are listed.
                let Some(held) = self.hold(elements.len() * std::mem::size_of::<Ty>()) else {
                    return Ty::ERROR;
                };
                let elements = elements
                    .iter()
                    .map(|element| self.annotation(element, scope, offset))
                    .collect();
                self.release(held);
                self.types.tuple(elements)
            }
            TypeKind::Literal(described) => {
                let described = match described {
                    Some(described) => self.annotation(described, scope, offset),
                    None => Ty::ANY,
                };
                self.types.type_lit(described)
            }
            TypeKind::Named => self.named_type(&ty.name, scope, offset),
        };
        if ty.nullable {
            // `nil` can take a union of the most alternatives past them.
            let optional = self.types.optional(base);
            self.too_large(offset);
            optional
        } else {
            base
        }
    }

    /// Reports, at `offset`, a union or shape too large for the checker to
    /// build since it last looked.
    pub(super) fn too_large(&mut self, offset: usize) {
        let Some((what, size)) = self.types.too_large.take() else {
            return;
        };
        let (most, parts) = match what {
            "union" => (super::ty::MAX_ALTERNATIVES, "alternatives"),
            _ => (super::ty::MAX_FIELDS, "fields"),
        };
        let span = self.spans.token(offset);
        self.report(Diagnostic::error(
            Code::TYPE_TOO_LARGE,
            span,
            text!(self,
                "this {what} has {size} {parts}, more than the {most} the checker relates; declare a wider type, such as a dictionary or an array of a smaller union"
            ),
        ));
    }

    /// Resolves a named type: an alias, class or enum visible from `scope`,
    /// or one of the signature table's aliases.
    pub(super) fn named_type(&mut self, name: &str, scope: Option<NsId>, offset: usize) -> Ty {
        if let Some(ty) = self.lookup_named_type(name, scope, 0) {
            return ty;
        }
        let span = self.spans.word_after(offset, name);
        self.report(Diagnostic::error(
            Code::UNKNOWN_TYPE,
            span,
            text!(self, "unknown type `{name}`"),
        ));
        Ty::ERROR
    }

    fn lookup_named_type(&mut self, name: &str, scope: Option<NsId>, depth: usize) -> Option<Ty> {
        if depth > 64 {
            return Some(Ty::ERROR);
        }
        if let Some((head, rest)) = name.split_once("::") {
            let ns = self.namespace_named(head, scope)?;
            return self.lookup_in(rest, ns, depth);
        }
        let mut current = scope;
        loop {
            if let Some(ty) = self.alias_type(name, current, depth) {
                return Some(ty);
            }
            let Some(ns) = current else {
                break;
            };
            if let Some(&child) = self.program.namespaces[ns as usize].children.get(name) {
                return self.namespace_type(child);
            }
            current = self.program.namespaces[ns as usize].parent;
        }
        if let Some(&ns) = self.program.roots.get(name) {
            return self.namespace_type(ns);
        }
        if let Some(&id) = self.program.enum_names.get(name) {
            return Some(self.types.intern(Kind::EnumValue(id)));
        }
        if let Some(alias) = sigs::index().aliases.get(name) {
            return Some(sigs::table_type(&mut self.types, alias, &[]));
        }
        if let Some(scalar) = sigs::scalar(name) {
            return Some(scalar);
        }
        None
    }

    fn lookup_in(&mut self, name: &str, ns: NsId, depth: usize) -> Option<Ty> {
        if let Some((head, rest)) = name.split_once("::") {
            let child = *self.program.namespaces[ns as usize].children.get(head)?;
            return self.lookup_in(rest, child, depth);
        }
        if let Some(ty) = self.alias_type(name, Some(ns), depth) {
            return Some(ty);
        }
        let child = *self.program.namespaces[ns as usize].children.get(name)?;
        self.namespace_type(child)
    }

    fn namespace_named(&self, name: &str, scope: Option<NsId>) -> Option<NsId> {
        let mut current = scope;
        while let Some(ns) = current {
            if let Some(&child) = self.program.namespaces[ns as usize].children.get(name) {
                return Some(child);
            }
            current = self.program.namespaces[ns as usize].parent;
        }
        self.program.roots.get(name).copied()
    }

    /// The type an annotation naming a class or module denotes: instances of
    /// a class. A module has no instances.
    fn namespace_type(&mut self, ns: NsId) -> Option<Ty> {
        if self.program.namespaces[ns as usize].is_class {
            Some(self.types.intern(Kind::Instance(ns)))
        } else {
            None
        }
    }

    fn alias_type(&mut self, name: &str, scope: Option<NsId>, depth: usize) -> Option<Ty> {
        let key = (scope, self.copy(name));
        if let Some(&ty) = self.program.alias_types.get(&key) {
            return Some(ty);
        }
        let ty = *self.program.aliases.get(&(scope, name))?;
        // A self-referential alias resolves to an unknown type once. Its
        // name, and room for it in the table, are counted as it is kept,
        // and by the measures after with the checker's growth.
        let bytes = key.1.capacity();
        if self
            .program
            .alias_types
            .insert(self.meter.tables(), key.clone(), Ty::ERROR)
            .is_err()
        {
            return Some(Ty::ERROR);
        }
        self.grown += bytes;
        let resolved = self.annotation_depth(ty, scope, depth + 1);
        if let Some(entry) = self.program.alias_types.get_mut(&key) {
            *entry = resolved;
        }
        Some(resolved)
    }

    fn annotation_depth(
        &mut self,
        ty: &compilation::Type,
        scope: Option<NsId>,
        depth: usize,
    ) -> Ty {
        if depth > 64 {
            return Ty::ERROR;
        }
        self.annotation(ty, scope, 0)
    }
}

/// How deep the checker resolves an annotation, through its parts and the
/// aliases it names, each a level. The compiler refuses a chain of more
/// than 64 aliases, and a type that nests deeper than 128 levels once they
/// are expanded, so an annotation it compiles stays well within this; a
/// deeper one is unknown here, and the compiler reports it. The bound keeps
/// the checker's recursion through aliases off the end of its stack.
const ANNOTATED: u32 = 512;

/// The type of a literal default value, or unknown for any other default.
fn literal_type(expr: &crate::syntax::Expr) -> Ty {
    use crate::{syntax::Node, value::Kind as Value};
    match &expr.node {
        Node::Integer(_) | Node::BigInteger(..) => Ty::INT,
        Node::Template(_, false) => Ty::STRING,
        Node::Literal(value) => match &value.0 {
            Value::Int(_) | Value::Big(_) => Ty::INT,
            Value::Float(_) => Ty::FLOAT,
            Value::Bool(_) => Ty::BOOL,
            Value::Bytes(_) => Ty::STRING,
            Value::Symbol(_) => Ty::SYMBOL,
            _ => Ty::ERROR,
        },
        _ => Ty::ERROR,
    }
}

/// Where a `break` out of the block a function body yields to goes: out
/// of the function when every `yield` stands outside loops and blocks, and
/// otherwise into the loop or call around a `yield`, or nowhere when the
/// body never yields; with the bytes of the stack the walk kept, which is
/// charged to `meter`.
fn yields(meter: &super::meter::Meter, body: &[crate::syntax::Stmt]) -> (sigs::Breaks, usize) {
    use super::walk::{Item, Next, Walk};
    use crate::syntax::{Node, Statement};
    // Each entry carries whether it stands inside a loop or a block.
    let mut walk: Walk<'_, '_, bool> = Walk::new(meter);
    // The body is a visit, empty or not.
    walk.visit(0);
    walk.stmts(body, false);
    let mut found = false;
    while let Some((item, inside)) = walk.next(0) {
        match item {
            Item::Expr(expr) => match &expr.node {
                Node::Yield(args) => {
                    if inside {
                        return (sigs::Breaks::Inside, walk.bytes());
                    }
                    found = true;
                    walk.push(Next::Exprs(args.iter()), inside);
                }
                Node::BlockCall(call, block) => {
                    walk.expr(call, inside);
                    walk.stmts(&block.body, true);
                }
                Node::Compound(stmt) => walk.push(Next::Item(Item::Stmt(stmt)), inside),
                Node::Try(attempt) => {
                    walk.stmts(&attempt.body, inside);
                    walk.stmts(&attempt.alternate, inside);
                    walk.stmts(&attempt.ensure, inside);
                    walk.push(Next::Rescues(attempt.rescues.iter()), inside);
                }
                Node::Conditional(branches, alternate) => {
                    walk.push(Next::Branches(branches.iter()), inside);
                    walk.expr(alternate, inside);
                }
                Node::Case(subject, whens, alternate) => {
                    for expr in subject.iter().chain(alternate) {
                        walk.expr(expr, inside);
                    }
                    walk.push(Next::Whens(whens.iter()), inside);
                }
                Node::Binary(_, left, right) => {
                    walk.expr(left, inside);
                    walk.expr(right, inside);
                }
                Node::Range(start, end, _) => {
                    for expr in start.iter().chain(end) {
                        walk.expr(expr, inside);
                    }
                }
                Node::Unary(_, value) => walk.expr(value, inside),
                Node::Call(_, args, _) => walk.push(Next::Arguments(args.iter()), inside),
                Node::ComputedCall(receiver, args)
                | Node::Method(receiver, _, args, _)
                | Node::SafeMethod(receiver, _, args, _) => {
                    walk.expr(receiver, inside);
                    walk.push(Next::Arguments(args.iter()), inside);
                }
                Node::Scope(receiver, _, args) => {
                    walk.expr(receiver, inside);
                    if let Some(args) = args {
                        walk.push(Next::Arguments(args.iter()), inside);
                    }
                }
                Node::Member(receiver, _) | Node::SafeMember(receiver, _) => {
                    walk.expr(receiver, inside);
                }
                Node::Index(receiver, selectors) => {
                    walk.expr(receiver, inside);
                    walk.push(Next::Exprs(selectors.iter()), inside);
                }
                Node::Array(items) | Node::Template(items, _) => {
                    walk.push(Next::Exprs(items.iter()), inside);
                }
                Node::Hash(entries) => walk.push(Next::Pairs(entries.iter()), inside),
                Node::Shape(_, Some(fallback), _) => walk.expr(fallback, inside),
                _ => (),
            },
            Item::Stmt(stmt) => match &stmt.node {
                Statement::Expr(e)
                | Statement::Assign(_, _, e)
                | Statement::Return(Some(e))
                | Statement::Break(Some(e))
                | Statement::Next(Some(e)) => walk.expr(e, inside),
                Statement::Raise(value, message) => {
                    for expr in value.iter().chain(message) {
                        walk.expr(expr, inside);
                    }
                }
                Statement::If(branches, alternate, _) => {
                    walk.push(Next::Clauses(branches.iter()), inside);
                    walk.stmts(alternate, inside);
                }
                Statement::While(condition, body, _) => {
                    walk.expr(condition, inside);
                    walk.stmts(body, true);
                }
                Statement::For(_, iterable, body) => {
                    walk.expr(iterable, inside);
                    walk.stmts(body, true);
                }
                _ => (),
            },
            Item::Target(_) => (),
        }
    }
    let breaks = if found {
        sigs::Breaks::Result
    } else {
        sigs::Breaks::Never
    };
    (breaks, walk.bytes())
}

/// Whether a typed block parameter is written `&name?:`.
fn block_optional(source: &str, block: &BlockParam) -> bool {
    let start = block.offset as usize + 1 + block.name.len();
    source.as_bytes().get(start) == Some(&b'?')
}
