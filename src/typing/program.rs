//! The declarations of one program: functions, classes, modules, enums and
//! type aliases, with every signature resolved to types before any body is
//! checked, so a call reads its callee's signature and never its body.

use super::{
    Checker,
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
use std::{collections::HashMap, rc::Rc};

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
    pub methods: HashMap<String, FnId>,
    pub statics: HashMap<String, FnId>,
    pub ivars: HashMap<String, Ivar>,
    pub children: HashMap<&'a str, NsId>,
}

/// A script enum.
#[derive(Clone)]
pub(crate) struct Enum {
    pub name: String,
    pub members: Vec<String>,
    /// Each member's symbol, as `:in_review` names `InReview`.
    pub symbols: Vec<String>,
}

/// Everything a program declares.
#[derive(Default)]
pub(crate) struct Program<'a> {
    pub file: bool,
    pub file_locals: HashMap<String, (Ty, usize)>,
    pub fns: Vec<FnDecl<'a>>,
    /// Top-level functions by name.
    pub functions: HashMap<&'a str, FnId>,
    pub namespaces: Vec<Namespace<'a>>,
    /// Top-level classes and modules by name.
    pub roots: HashMap<&'a str, NsId>,
    pub enums: Vec<Enum>,
    pub enum_names: HashMap<String, u32>,
    /// Type aliases by declaring namespace (none at the top level) and name.
    pub aliases: HashMap<(Option<NsId>, &'a str), &'a compilation::Type>,
    alias_types: HashMap<(Option<NsId>, String), Ty>,
    /// Namespaces by the offset of their `class` or `module` keyword.
    pub by_offset: HashMap<u32, NsId>,
    /// Host functions registered on the engine.
    pub hosts: HashMap<String, Rc<Sig>>,
    /// Globals and capabilities the host declares, as values, by name.
    pub declared: HashMap<String, Ty>,
    /// Capabilities the host declares with members, by `Kind::Host` index.
    pub host_modules: Vec<&'a crate::signatures::Module>,
}

impl<'a> Checker<'a> {
    /// Types the globals and capabilities the host declares: a global or a
    /// capability's data by its type, a capability with members as a
    /// namespace of them, and a callable capability as a host function.
    pub(super) fn declare_hosts(&mut self, declared: &'a crate::declared::Declarations) {
        for (name, declaration) in declared {
            self.steps += 1;
            match &declaration.item {
                crate::signatures::Item::Constant(constant) => {
                    let ty = sigs::table_type(&mut self.types, &constant.ty, &[]);
                    self.program.declared.insert(name.clone(), ty);
                }
                crate::signatures::Item::Module(module) => {
                    let id = self.program.host_modules.len() as u32;
                    self.program.host_modules.push(module);
                    self.types.names.hosts.push(name.clone());
                    let ty = self.types.intern(Kind::Host(id));
                    self.program.declared.insert(name.clone(), ty);
                }
                crate::signatures::Item::Function(function) => {
                    let sig = self
                        .converter
                        .convert_owned(&mut self.types, function, None);
                    self.program
                        .hosts
                        .entry(name.clone())
                        .or_insert_with(|| Rc::new(sig));
                }
                _ => (),
            }
        }
    }

    /// Collects the declarations and resolves every signature.
    pub(super) fn declare_program(&mut self, parsed: &'a Declarations) {
        for (index, (name, members)) in parsed.enums.iter().enumerate() {
            self.program
                .enum_names
                .insert(name.to_string(), index as u32);
            self.program.enums.push(Enum {
                name: name.to_string(),
                members: members.iter().map(|m| m.to_string()).collect(),
                symbols: members.iter().map(|m| crate::enums::symbol(m)).collect(),
            });
            self.types.names.enums.push(name.to_string());
        }
        for module in &parsed.modules {
            self.namespace(module, None);
        }
        for (scope, alias) in &parsed.additions.aliases {
            let scope = scope.and_then(|offset| self.program.by_offset.get(&offset).copied());
            self.program
                .aliases
                .insert((scope, alias.name.as_str()), &alias.ty);
        }
        for (index, (name, _)) in sigs::index().modules.iter().enumerate() {
            debug_assert_eq!(self.types.names.builtins.len(), index);
            self.types.names.builtins.push((*name).to_owned());
        }
        // Required files' enums are types in annotations too.
        self.require_modules(parsed);
        // Signatures after every name is known, so annotations resolve.
        for (index, def) in parsed.functions.iter().enumerate() {
            let main = index == 0;
            let block = (!main).then(|| block_param(parsed, def.offset)).flatten();
            let id = self.function(def, None, false, block, main, Visibility::Public);
            if !main {
                self.program.functions.insert(def.name.as_str(), id);
            }
        }
        for ns in 0..self.program.namespaces.len() {
            let Some(module) = self.program.namespaces[ns].module else {
                continue;
            };
            for (def, visibility) in &module.instance_methods {
                let block = block_param(parsed, def.offset);
                let id = self.function(def, Some(ns as NsId), true, block, false, *visibility);
                self.program.namespaces[ns]
                    .methods
                    .insert(def.name.to_string(), id);
            }
            for (def, visibility) in &module.methods {
                let block = block_param(parsed, def.offset);
                let id = self.function(def, Some(ns as NsId), false, block, false, *visibility);
                self.program.namespaces[ns]
                    .statics
                    .insert(def.name.to_string(), id);
            }
        }
        for (class, ivar) in &parsed.additions.ivars {
            let Some(&ns) = self.program.by_offset.get(class) else {
                continue;
            };
            let default = parsed
                .additions
                .defaults
                .iter()
                .any(|(owner, stmt)| owner == class && stmt.offset == ivar.offset);
            let ty = self.annotation(&ivar.ty, Some(ns), ivar.offset as usize);
            self.program.namespaces[ns as usize]
                .ivars
                .insert(ivar.name.to_string(), Ivar { ty, default });
        }
        // Declared class variables have their types before any body reads them.
        for (namespace, declared) in &parsed.additions.class_vars {
            let Some(&ns) = self.program.by_offset.get(namespace) else {
                continue;
            };
            let ty = self.annotation(&declared.ty, Some(ns), declared.offset as usize);
            self.constants
                .insert((Some(ns), declared.name.to_string()), ty);
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
                        format!("property `{name}` has no type; declare it as `{name}: T`"),
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
                self.program.namespaces[ns]
                    .ivars
                    .entry(name.to_string())
                    .or_insert(Ivar { ty, default: false });
            }
        }
        self.check_names(parsed);
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
            let at = |index: usize, def: &crate::syntax::Definition| {
                module
                    .aliases
                    .iter()
                    .find(|(alias, _)| *alias == index)
                    .map_or(def.offset, |(_, offset)| *offset) as usize
            };
            for (index, (def, _)) in module.instance_methods.iter().enumerate() {
                if def.accessor.is_some() {
                    continue;
                }
                let alias = module.aliases.iter().any(|(alias, _)| *alias == index);
                let earlier = module.instance_methods[..index]
                    .iter()
                    .any(|(other, _)| other.name == def.name);
                if alias && earlier {
                    let span = self.spans.word_after(at(index, def), &def.name);
                    self.report(Diagnostic::error(
                        Code::DUPLICATE_NAME,
                        span,
                        format!(
                            "`{}` is already a method of `{}`; an alias takes a new name",
                            def.name, module.name
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

    /// Declares `module` and the namespaces nested in it, each before its
    /// children, and returns its id. The walk keeps its place on the heap,
    /// since namespaces nest as deep as the parser allows.
    fn namespace(&mut self, module: &'a Module, parent: Option<NsId>) -> NsId {
        let first = self.program.namespaces.len() as NsId;
        let mut pending = vec![(module, parent)];
        while let Some((module, parent)) = pending.pop() {
            let id = self.declare_namespace(module, parent);
            // Pushed in reverse, so declared in source order.
            for nested in module.modules.iter().chain(&module.inner).rev() {
                pending.push((nested, Some(id)));
            }
        }
        first
    }

    fn declare_namespace(&mut self, module: &'a Module, parent: Option<NsId>) -> NsId {
        let id = self.program.namespaces.len() as NsId;
        let name = match parent {
            Some(parent) => format!(
                "{}::{}",
                self.program.namespaces[parent as usize].name, module.name
            ),
            None => module.name.to_string(),
        };
        self.types.names.namespaces.push(name.clone());
        self.program.namespaces.push(Namespace {
            checked: false,
            module: Some(module),
            name,
            parent,
            is_class: module.is_class,
            methods: HashMap::new(),
            statics: HashMap::new(),
            ivars: HashMap::new(),
            children: HashMap::new(),
        });
        self.program.by_offset.insert(module.offset, id);
        match parent {
            Some(parent) => {
                self.program.namespaces[parent as usize]
                    .children
                    .insert(module.name.as_str(), id);
            }
            None => {
                self.program.roots.insert(module.name.as_str(), id);
            }
        }
        id
    }

    fn function(
        &mut self,
        def: &'a Definition,
        owner: Option<NsId>,
        instance: bool,
        block: Option<&'a BlockParam>,
        main: bool,
        visibility: Visibility,
    ) -> FnId {
        let mut params = Vec::with_capacity(def.params.len());
        for param in &def.params {
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
                            format!(
                                "parameter `{}` of `{}` has no type; declare it as `{}: T`",
                                param.name, def.name, param.name
                            ),
                        );
                        // A removed keyword form has a colon after the name,
                        // which its own diagnostic rewrites.
                        let colon = self.source[span.end..].trim_start().starts_with(':');
                        if literal != Ty::ERROR && !colon {
                            let ty = self.types.display(literal);
                            diagnostic = diagnostic.with_fix(Fix::insert(
                                format!("declare `{}: {ty}`", param.name),
                                span.end,
                                format!(": {ty}"),
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
        let name = match (owner, instance) {
            (Some(ns), true) => {
                format!("{}#{}", self.program.namespaces[ns as usize].name, def.name)
            }
            (Some(ns), false) => {
                format!("{}.{}", self.program.namespaces[ns as usize].name, def.name)
            }
            (None, _) => def.name.to_string(),
        };
        let sig = Rc::new(Sig {
            name,
            params,
            result,
            block: block_sig,
            vars: Vec::new(),
            breaks: yields(&def.body),
        });
        self.program.fns.push(FnDecl {
            def: Some(def),
            owner,
            instance,
            sig,
            main,
            visibility,
        });
        self.program.fns.len() - 1
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
                        format!(
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
    /// unknown names at `offset`.
    pub(super) fn annotation(
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
                let fields = fields
                    .iter()
                    .map(|field| Field {
                        name: String::from_utf8_lossy(&field.name).into(),
                        ty: self.annotation(&field.ty, scope, offset),
                        optional: field.optional,
                    })
                    .collect();
                self.types.shape(fields, *open)
            }
            TypeKind::Union(options) => {
                let options: Vec<Ty> = options
                    .iter()
                    .map(|option| self.annotation(option, scope, offset))
                    .collect();
                self.types.union(&options)
            }
            TypeKind::Tuple(elements) => {
                let elements = elements
                    .iter()
                    .map(|element| self.annotation(element, scope, offset))
                    .collect();
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
            self.types.optional(base)
        } else {
            base
        }
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
            format!("unknown type `{name}`"),
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
                return Some(self.namespace_type(child));
            }
            current = self.program.namespaces[ns as usize].parent;
        }
        if let Some(&ns) = self.program.roots.get(name) {
            return Some(self.namespace_type(ns));
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
        Some(self.namespace_type(child))
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
    fn namespace_type(&mut self, ns: NsId) -> Ty {
        if self.program.namespaces[ns as usize].is_class {
            self.types.intern(Kind::Instance(ns))
        } else {
            Ty::ERROR
        }
    }

    fn alias_type(&mut self, name: &str, scope: Option<NsId>, depth: usize) -> Option<Ty> {
        let key = (scope, name.to_owned());
        if let Some(&ty) = self.program.alias_types.get(&key) {
            return Some(ty);
        }
        let ty = *self.program.aliases.get(&(scope, name))?;
        // A self-referential alias resolves to an unknown type once.
        self.program.alias_types.insert(key.clone(), Ty::ERROR);
        let resolved = self.annotation_depth(ty, scope, depth + 1);
        self.program.alias_types.insert(key, resolved);
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
/// body never yields.
fn yields(body: &[crate::syntax::Stmt]) -> sigs::Breaks {
    let mut found = false;
    use crate::syntax::{Node, Statement};
    let mut statements: Vec<(&crate::syntax::Stmt, bool)> =
        body.iter().map(|stmt| (stmt, false)).collect();
    let mut expressions: Vec<(&crate::syntax::Expr, bool)> = Vec::new();
    loop {
        if let Some((expr, inside)) = expressions.pop() {
            match &expr.node {
                Node::Yield(args) => {
                    if inside {
                        return sigs::Breaks::Inside;
                    }
                    found = true;
                    expressions.extend(args.iter().map(|arg| (arg, inside)));
                }
                Node::BlockCall(call, block) => {
                    expressions.push((call, inside));
                    statements.extend(block.body.iter().map(|stmt| (stmt, true)));
                }
                Node::Compound(stmt) => statements.push((stmt, inside)),
                Node::Try(attempt) => {
                    let bodies = [&attempt.body, &attempt.alternate, &attempt.ensure];
                    for body in bodies {
                        statements.extend(body.iter().map(|stmt| (stmt, inside)));
                    }
                    for rescue in attempt.rescues.iter() {
                        statements.extend(rescue.body.iter().map(|stmt| (stmt, inside)));
                    }
                }
                Node::Conditional(branches, alternate) => {
                    for (condition, value) in branches.iter() {
                        expressions.push((condition, inside));
                        expressions.push((value, inside));
                    }
                    expressions.push((alternate, inside));
                }
                Node::Case(subject, whens, alternate) => {
                    expressions.extend(subject.as_deref().map(|e| (e, inside)));
                    for when in whens.iter() {
                        expressions.extend(when.values.iter().map(|(value, _)| (value, inside)));
                        expressions.push((&when.result, inside));
                    }
                    expressions.extend(alternate.as_deref().map(|e| (e, inside)));
                }
                Node::Binary(_, left, right) => {
                    expressions.push((left, inside));
                    expressions.push((right, inside));
                }
                Node::Range(start, end, _) => {
                    expressions.extend([start, end].into_iter().flatten().map(|e| (&**e, inside)));
                }
                Node::Unary(_, value) => expressions.push((value, inside)),
                Node::Call(_, args, _) => {
                    expressions.extend(args.iter().map(|arg| (&arg.value, inside)));
                }
                Node::ComputedCall(receiver, args) => {
                    expressions.push((receiver, inside));
                    expressions.extend(args.iter().map(|arg| (&arg.value, inside)));
                }
                Node::Method(receiver, _, args, _) | Node::SafeMethod(receiver, _, args, _) => {
                    expressions.push((receiver, inside));
                    expressions.extend(args.iter().map(|arg| (&arg.value, inside)));
                }
                Node::Scope(receiver, _, args) => {
                    expressions.push((receiver, inside));
                    for arg in args.iter().flat_map(|args| args.iter()) {
                        expressions.push((&arg.value, inside));
                    }
                }
                Node::Member(receiver, _) | Node::SafeMember(receiver, _) => {
                    expressions.push((receiver, inside));
                }
                Node::Index(receiver, selectors) => {
                    expressions.push((receiver, inside));
                    expressions.extend(selectors.iter().map(|e| (e, inside)));
                }
                Node::Array(items) | Node::Template(items, _) => {
                    expressions.extend(items.iter().map(|e| (e, inside)));
                }
                Node::Hash(entries) => {
                    expressions.extend(entries.iter().map(|(_, e)| (e, inside)));
                }
                Node::Shape(_, fallback, _) => {
                    expressions.extend(fallback.as_deref().map(|e| (e, inside)));
                }
                _ => (),
            }
            continue;
        }
        let Some((stmt, inside)) = statements.pop() else {
            return if found {
                sigs::Breaks::Result
            } else {
                sigs::Breaks::Inside
            };
        };
        match &stmt.node {
            Statement::Expr(e) => expressions.push((e, inside)),
            Statement::Assign(_, _, e) => expressions.push((e, inside)),
            Statement::Return(Some(e)) | Statement::Break(Some(e)) | Statement::Next(Some(e)) => {
                expressions.push((e, inside));
            }
            Statement::Raise(value, message) => {
                expressions.extend(value.as_deref().map(|e| (e, inside)));
                expressions.extend(message.as_deref().map(|e| (e, inside)));
            }
            Statement::If(branches, alternate, _) => {
                for (condition, body) in branches.iter() {
                    expressions.push((condition, inside));
                    statements.extend(body.iter().map(|stmt| (stmt, inside)));
                }
                statements.extend(alternate.iter().map(|stmt| (stmt, inside)));
            }
            Statement::While(condition, body, _) => {
                expressions.push((condition, inside));
                statements.extend(body.iter().map(|stmt| (stmt, true)));
            }
            Statement::For(_, iterable, body) => {
                expressions.push((iterable, inside));
                statements.extend(body.iter().map(|stmt| (stmt, true)));
            }
            _ => (),
        }
    }
}

fn block_param(parsed: &Declarations, offset: u32) -> Option<&BlockParam> {
    parsed
        .additions
        .blocks
        .iter()
        .find(|(owner, _)| *owner == offset)
        .map(|(_, block)| block)
}

/// Whether a typed block parameter is written `&name?:`.
fn block_optional(source: &str, block: &BlockParam) -> bool {
    let start = block.offset as usize + 1 + block.name.len();
    source.as_bytes().get(start) == Some(&b'?')
}
