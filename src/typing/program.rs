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
    diagnostic::{Code, Diagnostic},
    syntax::{BlockParam, Declarations, Definition, modules::Module},
    types::Scalar,
};
use std::{collections::HashMap, rc::Rc};

pub(crate) type FnId = usize;
pub(crate) type NsId = u32;

/// A script function or method.
pub(crate) struct FnDecl<'a> {
    pub def: &'a Definition,
    /// The class or module that declares it.
    pub owner: Option<NsId>,
    /// Whether it is an instance method, rather than a top-level function or
    /// a `def self.` method.
    pub instance: bool,
    pub sig: Rc<Sig>,
    /// Whether it is the top-level statements.
    pub main: bool,
}

/// An instance variable a class declares, directly or with a property.
#[derive(Clone, Copy)]
pub(crate) struct Ivar {
    pub ty: Ty,
    pub default: bool,
}

/// A script class or module.
pub(crate) struct Namespace<'a> {
    pub module: &'a Module,
    pub name: String,
    pub parent: Option<NsId>,
    pub is_class: bool,
    pub methods: HashMap<&'a str, FnId>,
    pub statics: HashMap<&'a str, FnId>,
    pub ivars: HashMap<String, Ivar>,
    pub children: HashMap<&'a str, NsId>,
}

/// A script enum.
pub(crate) struct Enum {
    pub name: String,
    pub members: Vec<String>,
    /// Each member's symbol, as `:in_review` names `InReview`.
    pub symbols: Vec<String>,
}

/// Everything a program declares.
#[derive(Default)]
pub(crate) struct Program<'a> {
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
}

impl<'a> Checker<'a> {
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
        // Signatures after every name is known, so annotations resolve.
        for (index, def) in parsed.functions.iter().enumerate() {
            let main = index == 0;
            let block = (!main).then(|| block_param(parsed, def.offset)).flatten();
            let id = self.function(def, None, false, block, main);
            if !main {
                self.program.functions.insert(def.name.as_str(), id);
            }
        }
        for ns in 0..self.program.namespaces.len() {
            let module = self.program.namespaces[ns].module;
            for (def, _) in &module.instance_methods {
                let block = block_param(parsed, def.offset);
                let id = self.function(def, Some(ns as NsId), true, block, false);
                self.program.namespaces[ns]
                    .methods
                    .insert(def.name.as_str(), id);
            }
            for (def, _) in &module.methods {
                let block = block_param(parsed, def.offset);
                let id = self.function(def, Some(ns as NsId), false, block, false);
                self.program.namespaces[ns]
                    .statics
                    .insert(def.name.as_str(), id);
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
        // Properties declare their instance variables.
        for ns in 0..self.program.namespaces.len() {
            let module = self.program.namespaces[ns].module;
            for (def, _) in &module.instance_methods {
                let Some((name, setter)) = &def.accessor else {
                    continue;
                };
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
    }

    fn namespace(&mut self, module: &'a Module, parent: Option<NsId>) -> NsId {
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
            module,
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
        for nested in module.modules.iter().chain(&module.inner) {
            self.namespace(nested, Some(id));
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
    ) -> FnId {
        let mut params = Vec::with_capacity(def.params.len());
        for param in &def.params {
            let kind = match param.kind {
                crate::syntax::ParamKind::Positional => ParamKind::Positional,
                crate::syntax::ParamKind::Rest => ParamKind::Rest,
                crate::syntax::ParamKind::Keyword => ParamKind::Keyword,
                crate::syntax::ParamKind::KeywordRest => ParamKind::KeywordRest,
            };
            let ty = match &param.ty {
                Some(ty) => self.annotation(ty, owner, def.offset as usize),
                None => {
                    if !main && def.accessor.is_none() {
                        let span = self.spans.word_after(def.offset as usize, &param.name);
                        self.report(Diagnostic::error(
                            Code::MISSING_PARAMETER_TYPE,
                            span,
                            format!(
                                "parameter `{}` of `{}` has no type; declare it as `{}: T`",
                                param.name, def.name, param.name
                            ),
                        ));
                    }
                    Ty::ERROR
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
            class_vars: 0,
        });
        self.program.fns.push(FnDecl {
            def,
            owner,
            instance,
            sig,
            main,
        });
        self.program.fns.len() - 1
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
