//! Seeded, type-directed generation of programs the checker should accept.
//!
//! The generator keeps its own model of each local's declared and narrowed
//! type and builds expressions of a wanted type from literals, locals,
//! operators, builtin members, and the functions, methods, classes, enums,
//! namespaces and required files it declares. Values reach typed positions,
//! typed locals above all, whose checks the release build leaves out, so
//! the build that keeps them verifies each type the checker inferred.
//!
//! Some statements are deliberately risky: they narrow a local and then
//! assign it where the checker must notice, in a block, a loop, a rescue or
//! a branch, before using it narrowed; they write through shapes and
//! tuples, pass symbols where enums are expected, break out of blocks with
//! values of other types, and read properties before `initialize` assigns
//! them. A sound checker rejects the unsound ones; the rest must run the
//! same in both builds.

use super::{
    harness::Case,
    host::{Global, Host},
    rng::Rng,
};

/// A type, as the generator models it.
#[derive(Clone, Debug, PartialEq)]
pub enum Ty {
    Int,
    Float,
    Str,
    Sym,
    Bool,
    Nil,
    Any,
    Opt(Box<Ty>),
    Union(Vec<Ty>),
    Array(Box<Ty>),
    Hash(Box<Ty>),
    Shape(Vec<Field>),
    Tuple(Vec<Ty>),
    Enum(usize),
    Class(usize),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Field {
    pub name: String,
    pub ty: Ty,
    pub optional: bool,
}

impl Ty {
    fn opt(ty: Ty) -> Ty {
        match ty {
            Ty::Opt(_) | Ty::Nil | Ty::Any => ty,
            other => Ty::Opt(Box::new(other)),
        }
    }

    fn array(ty: Ty) -> Ty {
        Ty::Array(Box::new(ty))
    }

    fn hash(ty: Ty) -> Ty {
        Ty::Hash(Box::new(ty))
    }

    /// The type without `nil`.
    fn strip(&self) -> Ty {
        match self {
            Ty::Opt(inner) => (**inner).clone(),
            Ty::Union(options) => {
                let kept: Vec<Ty> = options
                    .iter()
                    .filter(|ty| **ty != Ty::Nil)
                    .cloned()
                    .collect();
                if kept.len() == 1 {
                    kept[0].clone()
                } else {
                    Ty::Union(kept)
                }
            }
            other => other.clone(),
        }
    }

    fn nilable(&self) -> bool {
        match self {
            Ty::Opt(_) | Ty::Nil | Ty::Any => true,
            Ty::Union(options) => options.iter().any(Ty::nilable),
            _ => false,
        }
    }

    fn comparable(&self) -> bool {
        matches!(self, Ty::Int | Ty::Float | Ty::Str | Ty::Sym)
    }

    fn scalar(&self) -> bool {
        matches!(self, Ty::Int | Ty::Float | Ty::Str | Ty::Sym | Ty::Bool)
    }
}

/// A function's, method's or block's parameter.
#[derive(Clone, Debug)]
struct Param {
    name: String,
    ty: Ty,
    kind: ParamKind,
}

#[derive(Clone, Debug, PartialEq)]
enum ParamKind {
    Required,
    Default(String),
    Keyword(Option<String>),
    Rest,
}

#[derive(Clone, Debug)]
struct BlockSig {
    params: Vec<Ty>,
    result: Option<Ty>,
    optional: bool,
}

#[derive(Clone, Debug)]
struct FnDef {
    name: String,
    params: Vec<Param>,
    result: Option<Ty>,
    block: Option<BlockSig>,
    /// Callables call only those of lower rank, so no program recurses.
    rank: usize,
}

#[derive(Clone, Debug)]
struct FieldDef {
    name: String,
    ty: Ty,
    access: Access,
    default: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Access {
    Plain,
    Getter,
    Property,
}

#[derive(Clone, Debug)]
struct ClassDef {
    name: String,
    fields: Vec<FieldDef>,
    init: Vec<Param>,
    methods: Vec<FnDef>,
    to_s: bool,
    /// Class variables, which instance and class methods share.
    class_vars: Vec<(String, Ty)>,
    /// Class methods, called on the class.
    statics: Vec<FnDef>,
    /// Whether it defines `+`, taking and giving an instance, and `==` and
    /// `<`, taking an instance and giving a bool.
    plus: bool,
    compare: bool,
    /// The element type `[]` gives and `[]=` takes, with an int index.
    element: Option<Ty>,
}

#[derive(Clone, Debug)]
struct Local {
    name: String,
    declared: Ty,
    current: Ty,
    /// Whether the checker's type is the declared one: a typed local or
    /// parameter. A block parameter may have a narrower type, such as a
    /// shape where the generator modelled a dictionary.
    exact: bool,
    /// Whether it is a global the host declares, which the checker never
    /// narrows and every function sees.
    global: bool,
}

impl Local {
    fn typed(name: String, ty: Ty) -> Self {
        Self {
            name,
            declared: ty.clone(),
            current: ty,
            exact: true,
            global: false,
        }
    }

    fn inferred(name: String, ty: Ty) -> Self {
        Self {
            exact: false,
            ..Self::typed(name, ty)
        }
    }
}

/// What the code being generated can see.
#[derive(Clone, Debug)]
struct Env {
    locals: Vec<Local>,
    /// `None` at top level; the function's result otherwise.
    function: Option<Option<Ty>>,
    /// The class whose method this is, and whether it is `initialize`.
    class: Option<(usize, bool)>,
    /// The class whose class method this is, whose class variables are in
    /// scope but not its instance variables.
    statics: Option<usize>,
    rank: usize,
    /// Inside a block: the type `next` and the block's value must have,
    /// or `None` when the block's value is discarded.
    block: Option<Option<Ty>>,
    in_loop: bool,
    yields: Option<BlockSig>,
    /// Whether the required file's names are in scope.
    library: bool,
    /// Whether the file the required file requires in turn is in scope, as
    /// `util`.
    util: bool,
}

impl Env {
    fn top() -> Self {
        Self {
            locals: Vec::new(),
            function: None,
            class: None,
            statics: None,
            rank: usize::MAX,
            block: None,
            in_loop: false,
            yields: None,
            library: false,
            util: false,
        }
    }

    fn set_current(&mut self, name: &str, ty: Ty) {
        if let Some(local) = self
            .locals
            .iter_mut()
            .rev()
            .find(|local| local.name == name)
        {
            local.current = ty;
        }
    }

    /// Forgets narrowing, as after code that may assign any local.
    fn widen_all(&mut self) {
        for local in &mut self.locals {
            local.current = local.declared.clone();
        }
    }

    fn class_index(&self) -> Option<usize> {
        self.class.map(|(index, _)| index)
    }
}

const MEMBERS: [&[&str]; 3] = [
    &["Red", "Green", "Blue"],
    &["Draft", "Done"],
    &["North", "East", "South", "West"],
];

/// Generates one program from `seed`: a quarter call builtins by their
/// signatures ([`super::builtins`]), the rest are type-directed.
pub fn program(seed: u64) -> Case {
    if Rng::new(seed ^ 0x6275_696c).chance(25) {
        return super::builtins::program(seed);
    }
    Gen::new(seed).program()
}

struct Gen {
    rng: Rng,
    enums: usize,
    classes: Vec<ClassDef>,
    functions: Vec<FnDef>,
    namespace: Vec<FnDef>,
    /// Functions of the module nested in `N`, `N::M`.
    inner: Vec<FnDef>,
    library: Vec<FnDef>,
    library_state: bool,
    /// The functions of the file the required file requires, as `util`.
    util: Vec<FnDef>,
    /// How many more risky constructs may take their unsound form.
    unsound: u32,
    fresh: std::cell::Cell<usize>,
    lines: Vec<String>,
    indent: usize,
    /// The host's globals, with the generator's model of their types, and
    /// what [`super::host::Host`] declares.
    globals: Vec<(String, Ty)>,
    host: Host,
    /// The script's function the host calls with arguments.
    entry: Option<FnDef>,
}

impl Gen {
    fn new(seed: u64) -> Self {
        Self {
            rng: Rng::new(seed),
            enums: 0,
            classes: Vec::new(),
            functions: Vec::new(),
            namespace: Vec::new(),
            inner: Vec::new(),
            library: Vec::new(),
            library_state: false,
            util: Vec::new(),
            unsound: 0,
            fresh: std::cell::Cell::new(0),
            lines: Vec::new(),
            indent: 0,
            globals: Vec::new(),
            host: Host::default(),
            entry: None,
        }
    }

    /// The scope code at a file's top level starts with: the host's
    /// globals.
    fn top_env(&self) -> Env {
        let mut env = Env::top();
        for (name, ty) in &self.globals {
            let mut local = Local::typed(name.clone(), ty.clone());
            local.global = true;
            env.locals.push(local);
        }
        env
    }

    fn name(&self, prefix: &str) -> String {
        self.fresh.set(self.fresh.get() + 1);
        format!("{prefix}{}", self.fresh.get())
    }

    fn line(&mut self, text: impl AsRef<str>) {
        for part in text.as_ref().split('\n') {
            self.lines
                .push(format!("{}{part}", "  ".repeat(self.indent)));
        }
    }

    fn take_lines(&mut self) -> String {
        let mut text = self.lines.join("\n");
        text.push('\n');
        self.lines.clear();
        text
    }

    // ----- Types -----

    fn render(&self, ty: &Ty) -> String {
        match ty {
            Ty::Int => "int".into(),
            Ty::Float => "float".into(),
            Ty::Str => "string".into(),
            Ty::Sym => "symbol".into(),
            Ty::Bool => "bool".into(),
            Ty::Nil => "nil".into(),
            Ty::Any => "any".into(),
            Ty::Opt(inner) => match &**inner {
                Ty::Union(_) => format!("{} | nil", self.render(inner)),
                other => format!("{}?", self.render(other)),
            },
            Ty::Union(options) => options
                .iter()
                .map(|ty| match ty {
                    Ty::Opt(_) => format!("{} | nil", self.render(&ty.strip())),
                    other => self.render(other),
                })
                .collect::<Vec<_>>()
                .join(" | "),
            Ty::Array(inner) => format!("array<{}>", self.render(inner)),
            Ty::Hash(inner) => format!("hash<string, {}>", self.render(inner)),
            Ty::Shape(fields) => {
                let fields: Vec<String> = fields
                    .iter()
                    .map(|field| {
                        let optional = if field.optional { "?" } else { "" };
                        format!("{}{optional}: {}", field.name, self.render(&field.ty))
                    })
                    .collect();
                format!("{{ {} }}", fields.join(", "))
            }
            Ty::Tuple(items) => {
                let items: Vec<String> = items.iter().map(|ty| self.render(ty)).collect();
                format!("[{}]", items.join(", "))
            }
            Ty::Enum(index) => format!("E{index}"),
            Ty::Class(index) => self.classes[*index].name.clone(),
        }
    }

    fn scalar_ty(&mut self) -> Ty {
        match self.rng.weighted(&[5, 2, 4, 1, 2]) {
            0 => Ty::Int,
            1 => Ty::Float,
            2 => Ty::Str,
            3 => Ty::Sym,
            _ => Ty::Bool,
        }
    }

    /// A random type of at most `depth` levels of nesting.
    fn ty(&mut self, depth: usize) -> Ty {
        if depth == 0 {
            return self.scalar_ty();
        }
        let enums = if self.enums > 0 { 2 } else { 0 };
        let classes = if self.classes.is_empty() { 0 } else { 1 };
        match self.rng.weighted(&[10, 4, 5, 2, 2, 2, enums, classes, 1]) {
            0 => self.scalar_ty(),
            1 => Ty::opt(self.ty(depth - 1).strip()),
            2 => Ty::array(self.ty(depth - 1)),
            3 => Ty::hash(self.ty(depth - 1)),
            4 => {
                let count = 1 + self.rng.below(3);
                let mut fields = Vec::new();
                for name in ["id", "name", "tags"].into_iter().take(count) {
                    fields.push(Field {
                        name: name.to_owned(),
                        ty: self.ty(depth - 1),
                        optional: self.rng.chance(20),
                    });
                }
                Ty::Shape(fields)
            }
            5 => Ty::Tuple(vec![self.ty(depth - 1), self.ty(depth - 1)]),
            6 => Ty::Enum(self.rng.below(self.enums)),
            7 => Ty::Class(self.rng.below(self.classes.len())),
            _ => {
                let a = self.scalar_ty();
                let mut b = self.scalar_ty();
                if a == b {
                    b = if a == Ty::Int { Ty::Str } else { Ty::Int };
                }
                Ty::Union(vec![a, b])
            }
        }
    }

    /// A random type a parameter annotation can name.
    fn param_ty(&mut self, depth: usize) -> Ty {
        self.ty(depth)
    }

    /// A random type a block signature's parameter list can name.
    fn block_param_ty(&mut self) -> Ty {
        self.param_ty(1)
    }

    /// Whether a value of type `value` is assignable to `target`, as the
    /// generator approximates the checker's rule.
    fn assignable(target: &Ty, value: &Ty) -> bool {
        if target == value || *target == Ty::Any {
            return true;
        }
        match (target, value) {
            (_, Ty::Union(options)) => options.iter().all(|ty| Self::assignable(target, ty)),
            (_, Ty::Opt(inner)) => {
                Self::assignable(target, &Ty::Nil) && Self::assignable(target, inner)
            }
            (Ty::Opt(_), Ty::Nil) => true,
            (Ty::Opt(inner), _) => Self::assignable(inner, value),
            (Ty::Union(options), _) => options.iter().any(|ty| Self::assignable(ty, value)),
            (Ty::Array(a), Ty::Array(b)) => Self::assignable(a, b),
            (Ty::Array(a), Ty::Tuple(items)) => items.iter().all(|ty| Self::assignable(a, ty)),
            (Ty::Hash(a), Ty::Hash(b)) => Self::assignable(a, b),
            (Ty::Hash(a), Ty::Shape(fields)) => {
                fields.iter().all(|field| Self::assignable(a, &field.ty))
            }
            (Ty::Tuple(a), Ty::Tuple(b)) => {
                a.len() == b.len() && a.iter().zip(b).all(|(a, b)| Self::assignable(a, b))
            }
            (Ty::Shape(a), Ty::Shape(b)) => {
                a.len() == b.len()
                    && a.iter().all(|field| {
                        b.iter().any(|other| {
                            other.name == field.name
                                && other.optional == field.optional
                                && Self::assignable(&field.ty, &other.ty)
                        })
                    })
            }
            _ => false,
        }
    }

    // ----- Programs -----

    fn program(&mut self) -> Case {
        self.unsound = u32::from(self.rng.chance(75));
        if self.rng.chance(35) {
            self.setup_host();
        }
        let mut case = match self.rng.weighted(&[6, 4, 1]) {
            0 => self.structural(false),
            1 => self.focused(),
            _ => self.structural(true),
        };
        case.host = self.host.clone();
        case
    }

    /// A program of declarations and top-level statements; a `large` one
    /// has several of each declaration and tens of statements, hundreds of
    /// lines in all.
    fn structural(&mut self, large: bool) -> Case {
        let mut modules = Vec::new();
        if self.rng.chance(if large { 50 } else { 20 }) {
            // The required file may require one of its own, which in the
            // unsound form requires it back.
            let util = self.rng.chance(40).then(|| self.util_file());
            modules.push(("lib.vibe".to_owned(), self.library_file()));
            if let Some(util) = util {
                modules.push(("util.vibe".to_owned(), util));
            }
        }
        self.enums = self.rng.below(3);
        let class_count = if large {
            2 + self.rng.below(4)
        } else {
            self.rng.weighted(&[3, 3, 1])
        };
        for index in 0..class_count {
            let class = self.class_sig(index);
            self.classes.push(class);
        }
        let function_count = if large {
            3 + self.rng.below(6)
        } else {
            1 + self.rng.below(4)
        };
        for rank in 0..function_count {
            let def = self.fn_sig(format!("f{rank}"), 10 + rank);
            self.functions.push(def);
        }
        if self.rng.chance(if large { 60 } else { 25 }) {
            for rank in 0..1 + self.rng.below(2) {
                let def = self.fn_sig(format!("g{rank}"), 6 + rank);
                self.namespace.push(def);
            }
            if self.rng.chance(40) {
                for rank in 0..1 + self.rng.below(2) {
                    let def = self.fn_sig(format!("k{rank}"), 4 + rank);
                    self.inner.push(def);
                }
            }
        }
        let mut text = self.declarations();
        let mut env = self.top_env();
        if !modules.is_empty() {
            env.library = true;
            self.line("lib = require(\"lib\")");
        }
        let statements = if large {
            20 + self.rng.below(40)
        } else {
            3 + self.rng.below(6)
        };
        for _ in 0..statements {
            if large && self.rng.chance(10) {
                self.risky(&mut env, 2);
            } else {
                self.stmt(&mut env, 2);
            }
        }
        self.observe_all(&env);
        text.push_str(&self.take_lines());
        if !self.host.is_empty() && self.rng.chance(50) {
            text.push_str(&self.entry());
        }
        Case {
            main: text,
            modules,
            host: Host::default(),
        }
    }

    /// A short program around risky constructs, in a function or at top
    /// level.
    fn focused(&mut self) -> Case {
        self.enums = self.rng.below(3);
        if self.rng.chance(40) {
            let class = self.class_sig(0);
            self.classes.push(class);
        }
        let def = self.fn_sig("f0".to_owned(), 10);
        self.functions.push(def);
        let mut text = self.declarations();
        let mut env = self.top_env();
        if self.rng.chance(50) {
            let result = self.rng.chance(70).then(|| self.ty(1));
            let def = FnDef {
                name: "probe".to_owned(),
                params: Vec::new(),
                result: result.clone(),
                block: None,
                rank: 20,
            };
            self.line(self.signature(&def));
            self.indent += 1;
            let mut inner = self.fn_env(&def, None);
            for _ in 0..1 + self.rng.below(3) {
                self.risky(&mut inner, 2);
            }
            if let Some(ty) = result {
                let value = self.expr(&mut inner, &ty, 1, true);
                self.line(value);
            }
            self.indent -= 1;
            self.line("end");
            self.line("p(probe)");
        } else {
            for _ in 0..1 + self.rng.below(3) {
                self.risky(&mut env, 2);
            }
        }
        self.observe_all(&env);
        text.push_str(&self.take_lines());
        Case::new(text)
    }

    /// The enums, classes, namespace and functions, with their bodies.
    fn declarations(&mut self) -> String {
        let mut text = String::new();
        for index in 0..self.enums {
            text.push_str(&enum_source(index));
        }
        for index in 0..self.classes.len() {
            text.push_str(&self.class_source(index));
        }
        if !self.namespace.is_empty() || !self.inner.is_empty() {
            self.line("module N");
            self.indent += 1;
            self.line("LIMIT = 3");
            for def in self.namespace.clone() {
                self.function(&def, None, true);
            }
            if !self.inner.is_empty() {
                self.line("module M");
                self.indent += 1;
                for def in self.inner.clone() {
                    self.function(&def, None, true);
                }
                self.indent -= 1;
                self.line("end");
            }
            self.indent -= 1;
            self.line("end");
            text.push_str(&self.take_lines());
        }
        for def in self.functions.clone() {
            self.function(&def, None, false);
            text.push_str(&self.take_lines());
        }
        text
    }

    /// A required file: exported functions, an enum, and state its
    /// functions share. It declares before the script's own types exist.
    /// The file the required file requires: functions of low rank, and in
    /// the unsound form a `require` of the requiring file, a cycle the
    /// compiler rejects.
    fn util_file(&mut self) -> String {
        if self.rng.chance(10) && self.unsound() {
            self.line("back = require(\"lib\")");
        }
        for rank in 0..1 + self.rng.below(2) {
            let mut def = self.fn_sig(format!("u{rank}"), rank);
            def.block = None;
            self.util.push(def.clone());
            let prefix = if self.rng.chance(50) { "export " } else { "" };
            let mut env = self.fn_env(&def, None);
            self.line(format!("{prefix}{}", self.signature(&def)));
            self.indent += 1;
            self.body(&mut env, def.result.clone(), 1);
            self.indent -= 1;
            self.line("end");
        }
        self.take_lines()
    }

    fn library_file(&mut self) -> String {
        if !self.util.is_empty() {
            self.line("util = require(\"util\")");
        }
        if self.rng.chance(50) {
            self.line("enum L\n  Up\n  Down\nend");
        }
        self.library_state = self.rng.chance(60);
        if self.library_state {
            self.line("limit: int? = 5");
            self.line("count = 0");
            self.line("def reset_limit\n  limit = nil\nend");
            self.line("def bump -> int\n  count += 1\n  count\nend");
        }
        for rank in 0..1 + self.rng.below(2) {
            let mut def = self.fn_sig(format!("h{rank}"), 1 + rank);
            def.block = None;
            self.library.push(def.clone());
            let prefix = if self.rng.chance(50) { "export " } else { "" };
            let mut env = self.fn_env(&def, None);
            env.util = !self.util.is_empty();
            self.line(format!("{prefix}{}", self.signature(&def)));
            self.indent += 1;
            if self.library_state && def.result == Some(Ty::Int) && self.rng.chance(70) {
                // File locals are visible to the file's functions; narrowing
                // one must not survive a call that may assign it.
                self.line("if limit != nil");
                self.indent += 1;
                if self.rng.chance(60) {
                    self.line("reset_limit");
                }
                if self.rng.chance(40) {
                    self.line("bump");
                }
                self.line("return limit");
                self.indent -= 1;
                self.line("end");
            }
            self.body(&mut env, def.result.clone(), 2);
            self.indent -= 1;
            self.line("end");
        }
        self.take_lines()
    }

    // ----- Declarations -----

    fn fn_sig(&mut self, name: String, rank: usize) -> FnDef {
        let mut params = Vec::new();
        let mut keywords = false;
        for index in 0..self.rng.below(4) {
            let ty = self.param_ty(2);
            let name = format!("a{index}");
            if keywords {
                let default = self.rng.chance(60).then(|| self.literal(&ty, 1));
                params.push(Param {
                    name,
                    ty,
                    kind: ParamKind::Keyword(default),
                });
                continue;
            }
            let defaulted = params
                .iter()
                .any(|param: &Param| matches!(param.kind, ParamKind::Default(_)));
            let kind = match self.rng.weighted(&[6, 2, 1, 1]) {
                0 if !defaulted => ParamKind::Required,
                0 | 1 => ParamKind::Default(self.literal(&ty, 1)),
                2 => {
                    keywords = true;
                    ParamKind::Keyword(self.rng.chance(60).then(|| self.literal(&ty, 1)))
                }
                _ => {
                    keywords = true;
                    params.push(Param {
                        name,
                        ty: Ty::array(ty),
                        kind: ParamKind::Rest,
                    });
                    continue;
                }
            };
            params.push(Param { name, ty, kind });
        }
        let result = self.rng.chance(85).then(|| self.ty(2));
        let block = self.rng.chance(25).then(|| BlockSig {
            params: (0..1 + self.rng.below(2))
                .map(|_| self.block_param_ty())
                .collect(),
            result: self.rng.chance(70).then(|| self.ty(1)),
            optional: self.rng.chance(30),
        });
        FnDef {
            name,
            params,
            result,
            block,
            rank,
        }
    }

    fn signature(&self, def: &FnDef) -> String {
        let mut parts = Vec::new();
        let mut star = false;
        for param in &def.params {
            let ty = self.render(&param.ty);
            match &param.kind {
                ParamKind::Required => parts.push(format!("{}: {ty}", param.name)),
                ParamKind::Default(value) => parts.push(format!("{}: {ty} = {value}", param.name)),
                ParamKind::Keyword(default) => {
                    if !star {
                        parts.push("*".to_owned());
                        star = true;
                    }
                    match default {
                        Some(value) => parts.push(format!("{}: {ty} = {value}", param.name)),
                        None => parts.push(format!("{}: {ty}", param.name)),
                    }
                }
                ParamKind::Rest => {
                    star = true;
                    parts.push(format!("*{}: {ty}", param.name));
                }
            }
        }
        if let Some(block) = &def.block {
            let params: Vec<String> = block.params.iter().map(|ty| self.render(ty)).collect();
            let params = if params.len() == 1 {
                params[0].clone()
            } else {
                format!("({})", params.join(", "))
            };
            let result = block
                .result
                .as_ref()
                .map(|ty| format!(" -> {}", self.render(ty)))
                .unwrap_or_default();
            let optional = if block.optional { "?" } else { "" };
            parts.push(format!("&block{optional}: {params}{result}"));
        }
        let params = if parts.is_empty() {
            String::new()
        } else {
            format!("({})", parts.join(", "))
        };
        let result = def
            .result
            .as_ref()
            .map(|ty| format!(" -> {}", self.render(ty)))
            .unwrap_or_default();
        format!("def {}{params}{result}", def.name)
    }

    fn fn_env(&self, def: &FnDef, class: Option<usize>) -> Env {
        let mut env = self.top_env();
        env.function = Some(def.result.clone());
        env.class = class.map(|class| (class, false));
        env.rank = def.rank;
        env.yields = def.block.clone();
        for param in &def.params {
            env.locals
                .push(Local::typed(param.name.clone(), param.ty.clone()));
        }
        env
    }

    /// Generates a function or method with its body.
    fn function(&mut self, def: &FnDef, class: Option<usize>, namespace: bool) {
        let mut env = self.fn_env(def, class);
        let mut signature = self.signature(def);
        if namespace {
            signature = signature.replacen("def ", "def self.", 1);
        }
        self.line(signature);
        self.indent += 1;
        self.body(&mut env, def.result.clone(), 2);
        self.indent -= 1;
        self.line("end");
    }

    /// Statements, then the final value of `result`'s type.
    fn body(&mut self, env: &mut Env, result: Option<Ty>, depth: usize) {
        for _ in 0..self.rng.below(4) {
            if self.rng.chance(20) {
                self.risky(env, depth);
            } else {
                self.stmt(env, depth);
            }
        }
        if let Some(ty) = result {
            if self.rng.chance(8) {
                self.tail_loop(env, &ty, depth);
                return;
            }
            let value = self.expr(env, &ty, depth, true);
            self.line(value);
        }
    }

    /// Ends a body with a loop, which gives the value its body had last: a
    /// `for` over a literal with elements always runs it, a `while` may
    /// not, and a `next` may skip its end.
    fn tail_loop(&mut self, env: &mut Env, ty: &Ty, depth: usize) {
        let counter = self.name("i");
        let is_while = ty.nilable() || self.unsound();
        if is_while {
            let limit = self.rng.below(3);
            self.line(format!("{counter} = 0"));
            self.line(format!("while {counter} < {limit}"));
        } else {
            let items: Vec<String> = (0..1 + self.rng.below(3)).map(|n| n.to_string()).collect();
            self.line(format!("for {counter} in [{}]", items.join(", ")));
        }
        self.nested(env, |generator, env| {
            if is_while {
                generator.line(format!("{counter} += 1"));
            }
            env.in_loop = true;
            env.block = None;
            generator.stmts(env, depth.saturating_sub(1));
            let value = generator.expr(env, ty, depth.saturating_sub(1), true);
            generator.line(value);
        });
        self.line("end");
    }

    fn class_sig(&mut self, index: usize) -> ClassDef {
        let mut fields = Vec::new();
        for field in 0..1 + self.rng.below(3) {
            // A class's own type in its fields would need an instance to
            // start from.
            let ty = self.param_ty(1);
            let access = match self.rng.weighted(&[3, 3, 2]) {
                0 => Access::Plain,
                1 => Access::Getter,
                _ => Access::Property,
            };
            let default = self.rng.chance(35).then(|| self.literal(&ty, 1));
            fields.push(FieldDef {
                name: format!("v{field}"),
                ty,
                access,
                default,
            });
        }
        let mut init = Vec::new();
        for (position, field) in fields.iter().enumerate() {
            if field.default.is_none() || self.rng.chance(30) {
                init.push(Param {
                    name: format!("p{position}"),
                    ty: field.ty.clone(),
                    kind: ParamKind::Required,
                });
            }
        }
        let mut methods = Vec::new();
        for method in 0..1 + self.rng.below(3) {
            let mut def = self.fn_sig(format!("m{method}"), 2 + method);
            def.block = None;
            methods.push(def);
        }
        let mut class_vars = Vec::new();
        if self.rng.chance(35) {
            for var in 0..1 + self.rng.below(2) {
                let ty = match self.rng.below(4) {
                    0 => Ty::opt(Ty::Int),
                    1 => Ty::Str,
                    2 => Ty::array(Ty::Int),
                    _ => Ty::Int,
                };
                class_vars.push((format!("@@c{var}"), ty));
            }
        }
        let mut statics = Vec::new();
        if self.rng.chance(30) {
            for rank in 0..1 + self.rng.below(2) {
                let mut def = self.fn_sig(format!("s{rank}"), 2 + rank);
                def.block = None;
                statics.push(def);
            }
        }
        let element = self.rng.chance(25).then(|| match self.rng.below(3) {
            0 => Ty::Int,
            1 => Ty::Str,
            _ => Ty::opt(Ty::Int),
        });
        ClassDef {
            name: format!("C{index}"),
            fields,
            init,
            methods,
            to_s: self.rng.chance(30),
            class_vars,
            statics,
            plus: self.rng.chance(25),
            compare: self.rng.chance(25),
            element,
        }
    }

    fn class_source(&mut self, index: usize) -> String {
        let class = self.classes[index].clone();
        self.line(format!("class {}", class.name));
        self.indent += 1;
        for field in &class.fields {
            let ty = self.render(&field.ty);
            match (field.access, &field.default) {
                (Access::Plain, Some(value)) => {
                    self.line(format!("@{}: {ty} = {value}", field.name))
                }
                (Access::Plain, None) => self.line(format!("@{}: {ty}", field.name)),
                (Access::Getter, _) => self.line(format!("getter {}: {ty}", field.name)),
                (Access::Property, _) => self.line(format!("property {}: {ty}", field.name)),
            }
        }
        for (name, ty) in &class.class_vars {
            let value = self.literal(ty, 1);
            self.line(format!("{name}: {} = {value}", self.render(ty)));
        }
        let params: Vec<String> = class
            .init
            .iter()
            .map(|param| format!("{}: {}", param.name, self.render(&param.ty)))
            .collect();
        let params = if params.is_empty() {
            String::new()
        } else {
            format!("({})", params.join(", "))
        };
        self.line(format!("def initialize{params}"));
        self.indent += 1;
        let mut env = self.top_env();
        env.function = Some(None);
        env.class = Some((index, true));
        env.rank = 0;
        for param in &class.init {
            env.locals
                .push(Local::typed(param.name.clone(), param.ty.clone()));
        }
        // Reads before assignment: a method called on self, the object's
        // own string form, or a getter, ahead of the assignments.
        if self.rng.chance(25) && self.unsound() {
            let method = self.rng.below(class.methods.len());
            let call = self.method_call_on(&mut env, index, method, "self", 1);
            let getter = class
                .fields
                .iter()
                .find(|field| field.access != Access::Plain)
                .map(|field| field.name.clone());
            match (self.rng.below(5), getter) {
                (0, _) => self.line(format!("p({call})")),
                (1, _) => self.line(format!("[1].each {{ |_i| p({call}) }}")),
                (2, _) => self.line("puts \"#{self}\""),
                (3, Some(getter)) => self.line(format!("p(self.{getter})")),
                (_, Some(getter)) => self.line(format!("p({getter})")),
                _ => self.line(format!("p({call})")),
            }
        }
        for (position, field) in class.fields.iter().enumerate() {
            let value = class
                .init
                .iter()
                .find(|param| param.name == format!("p{position}"))
                .map(|param| param.name.clone())
                .or_else(|| field.default.clone());
            let Some(value) = value else { continue };
            let unsound = self.rng.chance(10) && self.unsound();
            match self.rng.weighted(&[if unsound { 0 } else { 60 }, 1, 1]) {
                0 => self.line(format!("@{} = {value}", field.name)),
                // Assigned on one path only, or not at all.
                1 => {
                    let condition = self.expr(&mut env, &Ty::Bool, 0, false);
                    self.line(format!("@{} = {value} if {condition}", field.name));
                }
                _ => {}
            }
        }
        self.indent -= 1;
        self.line("end");
        for method in &class.methods {
            self.function(method, Some(index), false);
        }
        for def in &class.statics {
            let mut env = self.fn_env(def, None);
            env.statics = Some(index);
            self.line(self.signature(def).replacen("def ", "def self.", 1));
            self.indent += 1;
            self.body(&mut env, def.result.clone(), 1);
            self.indent -= 1;
            self.line("end");
        }
        self.operators(index);
        if class.to_s {
            self.line("def to_s -> string");
            let field = &class.fields[0];
            self.line(format!("  \"#{{@{}}}\"", field.name));
            self.line("end");
        }
        self.indent -= 1;
        self.line("end");
        self.take_lines()
    }

    // ----- Statements -----

    /// Declares a new local holding `value`.
    fn bind(&mut self, env: &mut Env, ty: Ty, value: String) -> String {
        let name = self.name("x");
        self.line(format!("{name}: {} = {value}", self.render(&ty)));
        env.locals.push(Local::typed(name.clone(), ty.clone()));
        name
    }

    fn stmt(&mut self, env: &mut Env, depth: usize) {
        let nested = if depth > 0 { 1 } else { 0 };
        let choice = self.rng.weighted(&[
            8,
            4,
            3,
            3,
            3 * nested,
            2 * nested,
            3 * nested,
            3,
            nested,
            if env.function.is_some() { 1 } else { 0 },
            if env.in_loop || env.block.is_some() {
                2
            } else {
                0
            },
            2 * nested,
            if env.yields.is_some() { 3 } else { 0 },
            if env.class.is_some() || env.statics.is_some() {
                3
            } else {
                0
            },
        ]);
        match choice {
            0 => {
                let ty = self.ty(2);
                let value = self.expr(env, &ty, depth, true);
                self.bind(env, ty, value);
            }
            1 => {
                let ty = self.ty(1);
                let value = self.expr(env, &ty, depth, false);
                // An undeclared local takes its value's inferred type, which
                // the generator models only for scalars.
                let name = self.name("x");
                self.line(format!("{name} = {value}"));
                if ty.scalar() {
                    env.locals.push(Local::typed(name, ty));
                }
            }
            2 => self.reassign(env, depth),
            3 => {
                let ty = self.ty(2);
                let value = self.expr(env, &ty, depth, false);
                let simple = value.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
                if simple && self.rng.chance(20) {
                    // A call without parentheses ends at the line's end, so
                    // a symbol on the next line is a statement of its own.
                    self.line(format!("p {value}"));
                    self.line(":done");
                } else {
                    self.line(format!("p({value})"));
                }
            }
            4 => self.if_stmt(env, depth),
            5 => self.while_stmt(env, depth),
            6 => self.block_stmt(env, depth),
            7 => self.mutate(env, depth),
            8 => self.rescue_stmt(env, depth),
            9 => {
                let condition = self.expr(env, &Ty::Bool, depth.saturating_sub(1), false);
                match env.function.clone().flatten() {
                    Some(ty) => {
                        let value = self.expr(env, &ty, depth.saturating_sub(1), true);
                        self.line(format!("return {value} if {condition}"));
                    }
                    None => self.line(format!("return if {condition}")),
                }
            }
            10 => self.jump(env, depth),
            11 => {
                let result = self.ty(1);
                let value = self.case_expr(env, &result, depth - 1);
                self.bind(env, result, value);
            }
            12 => {
                let call = self.yield_expr(env, depth);
                self.line(call);
            }
            _ => self.ivar_write(env, depth),
        }
    }

    fn reassign(&mut self, env: &mut Env, depth: usize) {
        let locals: Vec<Local> = env
            .locals
            .iter()
            .filter(|local| local.exact)
            .cloned()
            .collect();
        if locals.is_empty() {
            return;
        }
        let local = self.rng.pick(&locals).clone();
        let value = self.expr(env, &local.declared, depth, true);
        self.line(format!("{} = {value}", local.name));
        env.set_current(&local.name, local.declared.clone());
        if local.declared == Ty::Int && self.rng.chance(30) {
            self.line(format!("{} += 1", local.name));
        }
    }

    fn ivar_write(&mut self, env: &mut Env, depth: usize) {
        if let Some(class) = env.class_index().or(env.statics) {
            let vars = self.classes[class].class_vars.clone();
            if !vars.is_empty() && (env.statics.is_some() || self.rng.chance(40)) {
                let (name, ty) = self.rng.pick(&vars).clone();
                let value = self.expr(env, &ty, depth.saturating_sub(1), true);
                match (&ty, self.rng.below(2)) {
                    (Ty::Int, 0) => self.line(format!("{name} += {value}")),
                    (Ty::Array(element), 0) => {
                        let item = self.expr(env, element, 0, true);
                        self.line(format!("{name} << {item}"));
                    }
                    _ => self.line(format!("{name} = {value}")),
                }
                return;
            }
        }
        let Some(class) = env.class_index() else {
            return;
        };
        let fields = self.classes[class].fields.clone();
        let field = self.rng.pick(&fields).clone();
        let value = self.expr(env, &field.ty, depth.saturating_sub(1), true);
        match (&field.ty, self.rng.below(3)) {
            (Ty::Int, 0) => self.line(format!("@{} += {value}", field.name)),
            (Ty::Array(element), 0) => {
                let item = self.expr(env, element, 0, true);
                self.line(format!("@{} << {item}", field.name));
            }
            (_, 1) if field.access == Access::Property => {
                self.line(format!("self.{} = {value}", field.name))
            }
            _ => self.line(format!("@{} = {value}", field.name)),
        }
    }

    fn if_stmt(&mut self, env: &mut Env, depth: usize) {
        let condition = self.expr(env, &Ty::Bool, depth - 1, false);
        self.line(format!("if {condition}"));
        self.nested(env, |generator, env| generator.stmts(env, depth - 1));
        if self.rng.chance(50) {
            self.line("else");
            self.nested(env, |generator, env| generator.stmts(env, depth - 1));
        }
        self.line("end");
    }

    fn stmts(&mut self, env: &mut Env, depth: usize) {
        for _ in 0..1 + self.rng.below(3) {
            if self.rng.chance(15) {
                self.risky(env, depth);
            } else {
                self.stmt(env, depth);
            }
        }
    }

    /// Generates nested code in a copy of `env`: the locals it declares
    /// stay inside, and its assignments forget narrowing.
    fn nested(&mut self, env: &mut Env, inner: impl FnOnce(&mut Self, &mut Env)) {
        let mut copy = env.clone();
        self.indent += 1;
        inner(self, &mut copy);
        self.indent -= 1;
        env.widen_all();
    }

    fn while_stmt(&mut self, env: &mut Env, depth: usize) {
        let counter = self.name("i");
        let limit = 1 + self.rng.below(3);
        self.line(format!("{counter} = 0"));
        self.line(format!("while {counter} < {limit}"));
        self.nested(env, |generator, env| {
            generator.line(format!("{counter} += 1"));
            env.in_loop = true;
            env.block = None;
            generator.stmts(env, depth - 1);
        });
        self.line("end");
    }

    /// An iterating call with a block of statements.
    fn block_stmt(&mut self, env: &mut Env, depth: usize) {
        let element = self.ty(1);
        // A typed local gives the block's parameter exactly this type.
        let value = self.expr(env, &Ty::array(element.clone()), depth - 1, true);
        let receiver = self.bind(env, Ty::array(element.clone()), value);
        let param = self.name("e");
        let (call, result) = match self.rng.below(5) {
            0 => ("each", None),
            1 => ("map", Some(self.ty(1))),
            2 => ("select", Some(Ty::Bool)),
            3 => ("each_with_index", None),
            _ => ("reverse_each", None),
        };
        let params = if call == "each_with_index" {
            format!("|{param}, {}|", self.name("n"))
        } else {
            format!("|{param}|")
        };
        let target = self.rng.chance(40).then(|| self.name("r"));
        let prefix = target
            .as_ref()
            .map(|name| format!("{name} = "))
            .unwrap_or_default();
        self.line(format!("{prefix}{receiver}.{call} {{ {params}"));
        self.nested(env, |generator, env| {
            env.locals
                .push(Local::typed(param.clone(), element.clone()));
            env.block = Some(result.clone());
            env.in_loop = false;
            for _ in 0..1 + generator.rng.below(2) {
                if generator.rng.chance(20) {
                    generator.risky(env, depth - 1);
                } else {
                    generator.stmt(env, depth - 1);
                }
            }
            if let Some(ty) = &result {
                let value = generator.expr(env, ty, depth - 1, true);
                generator.line(value);
            }
        });
        self.line("}");
        if let Some(name) = target {
            self.line(format!("p({name})"));
        }
    }

    fn jump(&mut self, env: &mut Env, depth: usize) {
        let condition = self.expr(env, &Ty::Bool, depth.saturating_sub(1), false);
        if env.in_loop {
            let word = if self.rng.chance(50) { "next" } else { "break" };
            self.line(format!("{word} if {condition}"));
            return;
        }
        let Some(result) = env.block.clone() else {
            return;
        };
        match (self.rng.below(3), result) {
            (0, Some(ty)) => {
                let value = self.expr(env, &ty, depth.saturating_sub(1), true);
                self.line(format!("next {value} if {condition}"));
            }
            (0, None) => self.line(format!("next if {condition}")),
            (1, _) => {
                // A break's value joins the call's type.
                let ty = self.ty(1);
                let value = self.expr(env, &ty, depth.saturating_sub(1), false);
                self.line(format!("break {value} if {condition}"));
            }
            _ => match env.function.clone() {
                Some(Some(ty)) => {
                    let value = self.expr(env, &ty, depth.saturating_sub(1), true);
                    self.line(format!("return {value} if {condition}"));
                }
                _ => self.line(format!("next if {condition}")),
            },
        }
    }

    fn rescue_stmt(&mut self, env: &mut Env, depth: usize) {
        self.line("begin");
        self.nested(env, |generator, env| {
            generator.stmts(env, depth - 1);
            if generator.rng.chance(40) {
                generator.line("raise \"boom\"");
            }
        });
        self.line("rescue => error");
        self.nested(env, |generator, env| {
            generator.line("p(error.message)");
            generator.stmts(env, depth - 1);
        });
        if self.rng.chance(30) {
            self.line("ensure");
            self.nested(env, |generator, env| generator.stmts(env, depth - 1));
        }
        self.line("end");
    }

    /// Updates a collection in place.
    fn mutate(&mut self, env: &mut Env, depth: usize) {
        let collections: Vec<Local> = env
            .locals
            .iter()
            .filter(|local| {
                local.exact
                    && matches!(
                        local.current,
                        Ty::Array(_) | Ty::Hash(_) | Ty::Shape(_) | Ty::Tuple(_) | Ty::Class(_)
                    )
            })
            .cloned()
            .collect();
        if collections.is_empty() {
            return;
        }
        let local = self.rng.pick(&collections).clone();
        let name = local.name.clone();
        let d = depth.saturating_sub(1);
        match &local.current {
            Ty::Array(element) => {
                let value = self.expr(env, element, d, true);
                match self.rng.below(7) {
                    0 => self.line(format!("{name} << {value}")),
                    1 => self.line(format!("{name}.push({value})")),
                    2 => self.line(format!("{name}[0] = {value}")),
                    3 => self.line(format!("{name}.pop")),
                    4 => self.line(format!("{name}.prepend({value})")),
                    5 => self.line(format!("{name}.insert(0, {value})")),
                    _ => self.line(format!("{name}.delete_if {{ |d| d == {value} }}")),
                }
            }
            Ty::Hash(value_ty) => {
                let value = self.expr(env, value_ty, d, true);
                let key = self.string_literal();
                match self.rng.below(3) {
                    0 => self.line(format!("{name}[{key}] = {value}")),
                    1 => self.line(format!("{name}.delete({key})")),
                    _ => self.line(format!("{name} = {name}.merge({{ key: {value} }})")),
                }
            }
            Ty::Shape(fields) => {
                let field = self.rng.pick(fields).clone();
                let value = self.expr(env, &field.ty, d, true);
                self.line(format!("{name}[\"{}\"] = {value}", field.name));
            }
            Ty::Tuple(items) => {
                let index = self.rng.below(items.len());
                let value = self.expr(env, &items[index], d, true);
                let negative = index as i64 - items.len() as i64;
                match self.rng.below(8) {
                    0..=2 => self.line(format!("{name}[{index}] = {value}")),
                    3 => self.line(format!("{name}[{negative}] = {value}")),
                    4 => self.line(format!("{name}.fill({value})")),
                    5 => self.line(format!("{name}.pop")),
                    6 => self.line(format!("{name}.delete({value})")),
                    _ => self.line(format!("{name}.clear")),
                }
            }
            Ty::Class(class) if self.rng.chance(50) && self.classes[*class].element.is_some() => {
                let element = self.classes[*class].element.clone().unwrap();
                let index = self.expr(env, &Ty::Int, 0, false);
                if element == Ty::Int && self.rng.chance(40) {
                    self.line(format!("{name}[{index}] += 1"));
                } else {
                    let value = self.expr(env, &element, d, true);
                    self.line(format!("{name}[{index}] = {value}"));
                }
            }
            Ty::Class(class) if self.rng.chance(50) && self.classes[*class].plus => {
                let other = self.expr(env, &Ty::Class(*class), d, false);
                self.line(format!("{name} += {other}"));
            }
            Ty::Class(class) => {
                let fields: Vec<FieldDef> = self.classes[*class]
                    .fields
                    .iter()
                    .filter(|field| field.access == Access::Property)
                    .cloned()
                    .collect();
                if fields.is_empty() {
                    return;
                }
                let field = self.rng.pick(&fields).clone();
                let value = self.expr(env, &field.ty, d, true);
                self.line(format!("{name}.{} = {value}", field.name));
            }
            _ => {}
        }
    }

    // ----- Risky statements -----

    /// Whether the next risky construct takes its unsound form, which a
    /// sound checker rejects. A program has at most one, so that rejecting
    /// another cannot hide a checker that accepts it.
    fn unsound(&mut self) -> bool {
        if self.unsound > 0 && self.rng.chance(50) {
            self.unsound -= 1;
            true
        } else {
            false
        }
    }

    /// One construct from the areas where checkers go wrong.
    fn risky(&mut self, env: &mut Env, depth: usize) {
        let unsound = self.unsound();
        match self.rng.weighted(&[12, 3, 3, 3, 3, 3, 2, 3, 2, 2, 2, 2, 2]) {
            12 => self.ensure_flow(env, unsound),
            11 => self.retry_flow(env, unsound),
            0 => self.narrow_and_interfere(env, unsound),
            1 => self.any_narrowing(env, unsound),
            2 => self.enum_symbols(env, unsound),
            3 => self.shape_writes(unsound),
            4 => self.break_values(env, unsound),
            5 => self.next_values(env, unsound),
            6 => self.destructure(unsound),
            7 => self.optional_case(env, unsound),
            8 => self.variance(),
            9 => self.definite_assignment(unsound),
            _ => self.tuple_writes(unsound, depth),
        }
    }

    /// An optional local, narrowed, then used narrowed; in the unsound
    /// form, assigned in between somewhere the checker must notice.
    fn narrow_and_interfere(&mut self, env: &mut Env, unsound: bool) {
        let inner = match self.rng.below(4) {
            0 => Ty::Int,
            1 => Ty::Str,
            2 => self.ty(1).strip(),
            _ => Ty::array(Ty::Int),
        };
        let union = self.rng.chance(20);
        let other = if inner == Ty::Str { Ty::Int } else { Ty::Str };
        let declared = if union {
            Ty::Union(vec![inner.clone(), other.clone()])
        } else {
            Ty::opt(inner.clone())
        };
        let initial = self.expr(env, &inner, 0, true);
        let name = self.name("v");
        self.line(format!("{name}: {} = {initial}", self.render(&declared)));
        let spoil = if union {
            self.expr(env, &other, 0, true)
        } else {
            "nil".to_owned()
        };
        // The sound form assigns a value of the narrowed type.
        let spoil = if unsound {
            spoil
        } else {
            self.expr(env, &inner, 0, true)
        };
        let (test, negated) = if union {
            let atom = atom(&inner);
            (
                format!("{name}.is_type?(:{atom})"),
                format!("!{name}.is_type?(:{atom})"),
            )
        } else {
            (format!("{name} != nil"), format!("{name} == nil"))
        };
        let assign = format!("{name} = {spoil}");
        let condition = self.expr(env, &Ty::Bool, 0, false);
        let interference = match self.rng.below(19) {
            0 => assign.clone(),
            1 => format!("[1].each {{ |_q| {assign} }}"),
            2 => format!("[1, 2].map {{ |_q|\n  {assign}\n  _q\n}}"),
            3 => format!("1.times {{ {assign} }}"),
            4 => format!("loop {{\n  {assign}\n  break\n}}"),
            5 => format!("q_ = 0\nwhile q_ < 1\n  q_ += 1\n  {assign}\nend"),
            6 => format!("begin\n  raise \"x\"\nrescue\n  {assign}\nend"),
            7 => format!("begin\n  q_ = 1\nensure\n  {assign}\nend"),
            8 => format!("if {condition}\n  {assign}\nend"),
            9 => format!("[[1]].each {{ |_q| _q.each {{ |_r| {assign} }} }}"),
            10 => format!("{{ a: 1 }}.each {{ |_k, _w| {assign} }}"),
            11 => format!("[1].select {{ |_q|\n  {assign}\n  true\n}}"),
            12 => format!("if {condition}\n  p(0)\nelsif true\n  {assign}\nend"),
            13 => format!("(0..1).each {{ |_q| {assign} }}"),
            14 => format!("[1].each_with_index {{ |_q, _n| {assign} }}"),
            15 => format!("{assign} if {condition}"),
            // A block written inside an index or a member read assigns too.
            16 => format!("_i = [1].map {{ |_q| {assign}; _q }}[0]"),
            17 => format!("_i = [1].map {{ |_q| {assign}; _q }}.length"),
            _ => match self.functions.iter().find(|def| {
                def.block
                    .as_ref()
                    .is_some_and(|block| !block.optional && block.result.is_none())
                    && def.rank < env.rank
            }) {
                Some(def) => {
                    // A script function's block assigns.
                    let def = def.clone();
                    let call = self.call_with_block(env, &def, 0, None);
                    match call.find('|') {
                        Some(start) => {
                            let end = start + 1 + call[start + 1..].find('|').unwrap();
                            format!("{} {assign};{}", &call[..=end], &call[end + 1..])
                        }
                        None => assign.clone(),
                    }
                }
                None => assign.clone(),
            },
        };
        let interfere = self.rng.chance(85);
        let returning = env.function.clone();
        let form = self.rng.below(8);
        if form == 7 {
            // A loop's second pass sees what its first assigned.
            let (head, tail) = match self.rng.below(3) {
                0 => ("[1, 2].each { |_p|", "}"),
                1 => ("2.times {", "}"),
                _ => ("w_ = 0\nwhile w_ < 2\n  w_ += 1", "end"),
            };
            self.line(format!("if {test}"));
            self.indent += 1;
            self.line(head);
            self.indent += 1;
            let use_it = self.use_narrowed(env, &name, &inner);
            self.line(use_it);
            if interfere {
                self.line(assign);
            }
            self.indent -= 1;
            self.line(tail);
            self.indent -= 1;
            self.line("end");
        } else {
            let (open, close) = match form {
                0 => (format!("if {test}"), true),
                1 => (format!("if {negated}\n  p(0)\nelse"), true),
                2 if returning.is_some() => match returning.clone().flatten() {
                    Some(ty) => {
                        let value = self.expr(env, &ty, 0, true);
                        (format!("return {value} if {negated}"), false)
                    }
                    None => (format!("return if {negated}"), false),
                },
                3 => (format!("raise \"none\" if {negated}"), false),
                4 => (format!("if {test} && {condition}"), true),
                5 => (format!("if !({negated})"), true),
                6 => (format!("if {condition} || {negated}\n  p(1)\nelse"), true),
                _ => (format!("if {test}"), true),
            };
            self.line(open);
            if close {
                self.indent += 1;
            }
            if interfere {
                self.line(interference);
            }
            let use_it = self.use_narrowed(env, &name, &inner);
            self.line(use_it);
            if close {
                self.indent -= 1;
                self.line("end");
            }
        }
        env.locals.push(Local::typed(name, declared));
    }

    /// A use of `name` as `ty`, at a position whose check the release
    /// build leaves out.
    fn use_narrowed(&mut self, env: &mut Env, name: &str, ty: &Ty) -> String {
        let rendered = self.render(ty);
        match self.rng.below(6) {
            0 | 1 => format!("{}: {rendered} = {name}", self.name("t")),
            2 => match ty {
                Ty::Int => format!("p({name} + 1)"),
                Ty::Str => format!("p({name}.upcase)"),
                Ty::Array(_) => format!("p({name}.length)"),
                _ => format!("{}: {rendered} = {name}", self.name("t")),
            },
            3 => match env.function.clone().flatten() {
                Some(result) if result == *ty => format!("return {name}"),
                _ => format!("{}: array<{rendered}> = [{name}]", self.name("t")),
            },
            4 => {
                let callee = self.functions.iter().find(|def| {
                    def.rank < env.rank
                        && def.params.first().is_some_and(|param| {
                            param.kind == ParamKind::Required && param.ty == *ty
                        })
                        && def.params.iter().skip(1).all(|param| {
                            !matches!(param.kind, ParamKind::Required | ParamKind::Keyword(None))
                        })
                        && def.block.as_ref().is_none_or(|block| block.optional)
                });
                match callee {
                    Some(def) => format!("p({}({name}))", def.name),
                    None => format!(
                        "{}: {{ value: {rendered} }} = {{ value: {name} }}",
                        self.name("t")
                    ),
                }
            }
            _ => format!(
                "{}: {{ value: {rendered} }} = {{ value: {name} }}",
                self.name("t")
            ),
        }
    }

    /// A value of type `any` narrowed by `is_type?`; the unsound form
    /// assigns it another value before the use.
    fn any_narrowing(&mut self, env: &mut Env, unsound: bool) {
        let target = match self.rng.below(6) {
            0 => Ty::Int,
            1 => Ty::Str,
            2 => Ty::Float,
            3 => Ty::Bool,
            4 => Ty::array(Ty::Any),
            _ => Ty::Sym,
        };
        let name = self.name("d");
        let untyped: Vec<String> = self
            .globals
            .iter()
            .filter(|(_, ty)| *ty == Ty::Any)
            .map(|(name, _)| name.clone())
            .collect();
        let source = match self.rng.below(4) {
            // A global the host declares as `any` narrows through a local.
            3 if !untyped.is_empty() => self.rng.pick(&untyped).clone(),
            0 if target != Ty::Sym => {
                let json = self.json_of(&target).replace('"', "\\\"");
                format!("JSON.parse(\"{json}\")")
            }
            1 => {
                let ty = self.ty(1);
                self.expr(env, &ty, 0, true)
            }
            _ => self.expr(env, &target, 0, true),
        };
        self.line(format!("{name}: any = {source}"));
        let spoil_ty = if unsound {
            self.scalar_ty()
        } else {
            target.clone()
        };
        let spoil = self.expr(env, &spoil_ty, 0, true);
        let atom = atom(&target);
        self.line(format!("if {name}.is_type?(:{atom})"));
        self.indent += 1;
        match self.rng.below(5) {
            0 => self.line(format!("[1].each {{ |_q| {name} = {spoil} }}")),
            1 => self.line(format!("{name} = {spoil}")),
            2 => {
                let condition = self.expr(env, &Ty::Bool, 0, false);
                self.line(format!("{name} = {spoil} if {condition}"));
            }
            _ => {}
        }
        let use_it = self.use_narrowed(env, &name, &target);
        self.line(use_it);
        self.indent -= 1;
        self.line("end");
        env.locals.push(Local::typed(name, Ty::Any));
    }

    /// Symbol literals where an enum is expected, reaching an exhaustive
    /// `case` and the enum's members. The checker accepts every form; the
    /// unsound form reads the member back through a builtin, where no
    /// typed boundary converts it.
    fn enum_symbols(&mut self, env: &mut Env, unsound: bool) {
        if self.enums == 0 {
            return self.narrow_and_interfere(env, unsound);
        }
        let index = self.rng.below(self.enums);
        let members = MEMBERS[index];
        let member = members[self.rng.below(members.len())];
        let symbol = format!(":{}", member.to_lowercase());
        let name = self.name("es");
        let first = format!("E{index}::{}", members[0]);
        let collection = self.rng.below(3);
        match collection {
            0 => self.line(format!("{name}: array<E{index}> = [{first}]")),
            1 => self.line(format!("{name}: hash<string, E{index}> = {{ a: {first} }}")),
            _ => self.line(format!("{name}: E{index}? = {first}")),
        }
        let read = match (collection, self.rng.below(7)) {
            (0, 0) => {
                self.line(format!("{name} << {symbol}"));
                format!("{name}.fetch(1)")
            }
            (0, 1) => {
                self.line(format!("{name}.push({symbol})"));
                format!("{name}.fetch(-1)")
            }
            (0, 2) => format!("{name}.fetch(5, {symbol})"),
            (0, 3) => {
                self.line(format!("{name}[0] = {symbol}"));
                format!("{name}.fetch(0)")
            }
            (0, 4) => {
                self.line(format!("{name}.fill({symbol})"));
                format!("{name}.fetch(0)")
            }
            (0, 5) => {
                self.line(format!("{name}.insert(0, {symbol})"));
                format!("{name}.fetch(0)")
            }
            (0, _) => format!("{name}.map {{ |q| {symbol} }}.fetch(0)"),
            (1, 0) => {
                self.line(format!("{name}[\"b\"] = {symbol}"));
                format!("{name}.fetch(\"b\")")
            }
            (1, 1) => format!("{name}.fetch(\"z\", {symbol})"),
            (1, 2) => format!("{name}.transform_values {{ |q| {symbol} }}.fetch(\"a\")"),
            (1, _) => {
                self.line(format!("{name} = {name}.merge({{ c: {symbol} }})"));
                format!("{name}.fetch(\"c\")")
            }
            (_, 0) => {
                self.line(format!("{name} = {symbol}"));
                format!("{name}.as(E{index})")
            }
            (_, 1) => format!("[{name}].compact.first(1).fetch(0)"),
            (_, 2) => format!("({name} == nil ? {first} : {symbol})"),
            (_, _) => format!("[{name}, {symbol}].compact.fetch(1)"),
        };
        let item = self.name("ev");
        if unsound {
            self.line(format!("{item} = {read}"));
        } else {
            self.line(format!("{item}: E{index} = {read}"));
        }
        let arms: Vec<String> = members
            .iter()
            .enumerate()
            .map(|(position, member)| format!("  when E{index}::{member} then {position}"))
            .collect();
        let typed = self.name("n");
        self.line(format!(
            "{typed}: int = case {item}\n{}\nend",
            arms.join("\n")
        ));
        if self.rng.chance(50) {
            self.line(format!("p({item}.name)"));
        }
    }

    /// Writes through a shape; the unsound forms could remove or retype
    /// its fields.
    fn shape_writes(&mut self, unsound: bool) {
        let name = self.name("s");
        let optional = self.rng.chance(40);
        let shape = if optional {
            "{ a: int, b?: int }"
        } else {
            "{ a: int, b: int }"
        };
        let literal = if optional && self.rng.chance(50) {
            "{ a: 1 }"
        } else {
            "{ a: 1, b: 2 }"
        };
        let wrapped = self.rng.chance(25);
        if wrapped {
            self.line(format!(
                "{name}: {{ inner: {shape} }} = {{ inner: {literal} }}"
            ));
        } else {
            self.line(format!("{name}: {shape} = {literal}"));
        }
        let target = if wrapped {
            format!("{name}[\"inner\"]")
        } else {
            name.clone()
        };
        let write = if unsound {
            match self.rng.below(14) {
                0 => format!("{target}.delete(\"a\")"),
                1 => format!("{target}.clear"),
                2 => format!("{target}.delete_if {{ |k, v| v > 0 }}"),
                3 => format!("{target}.keep_if {{ |k, v| v < 0 }}"),
                4 => format!("{target}.replace({{ c: 3 }})"),
                5 => format!("{target}[\"b\"] = nil"),
                6 => format!("{target} = {target}.merge({{ c: 3 }})"),
                7 => format!("{target} = {target}.except(\"a\")"),
                8 => format!("{target} = {target}.select {{ |k, v| false }}"),
                9 => format!("{target} = {target}.transform_keys {{ |k| k + \"x\" }}"),
                10 => format!("{target} = {target}.compact"),
                11 => format!("{target} = {target}.remap_keys({{ a: \"z\" }})"),
                12 => format!("{target}.delete(\"b\") {{ |k| 0 }}"),
                _ => format!("{target} = {target}.slice(\"b\")"),
            }
        } else {
            match self.rng.below(5) {
                0 => format!("{target}[\"a\"] = 5"),
                1 => format!("{target}[\"b\"] = 6"),
                2 => format!("{target}[\"a\"] += 1"),
                3 => format!("{target}.delete(\"zz\")"),
                _ => format!("{target}.each {{ |k, v| p(k) }}"),
            }
        };
        self.line(write);
        let read = match self.rng.below(3) {
            0 => format!("{}: int = {target}[\"a\"]", self.name("t")),
            1 => format!("{}: {shape} = {target}", self.name("t")),
            _ => format!("p({target}[\"a\"] + 1)"),
        };
        self.line(read);
    }

    /// Tuple elements written through an index, an alias or a member; the
    /// unsound forms could change their length or types.
    fn tuple_writes(&mut self, unsound: bool, depth: usize) {
        let _ = depth;
        let name = self.name("tp");
        let nested = self.rng.chance(30);
        if nested {
            self.line(format!("{name}: array<[int, string]> = [[1, \"a\"]]"));
        } else {
            self.line(format!("{name}: [int, string] = [1, \"a\"]"));
        }
        let target = if nested {
            format!("{name}[0]")
        } else {
            name.clone()
        };
        let write = if unsound {
            let index = self.rng.range(-3, 2);
            match self.rng.below(9) {
                0 => format!("{target}[{index}] = 5"),
                1 => format!("{target}[{index}] = \"s\""),
                2 => format!("{target}.fill(7)"),
                3 => format!("{target}.pop"),
                4 => format!("{target}.shift"),
                5 => format!("{target}.clear"),
                6 => format!("{target}.delete(1)"),
                7 => format!("{target}.keep_if {{ |q| q == 1 }}"),
                _ => {
                    let i = self.name("i");
                    format!("{i} = {}\n{target}[{i}] = 9", self.rng.range(0, 1))
                }
            }
        } else {
            match self.rng.below(5) {
                0 => format!("{target}[0] = 5"),
                1 => format!("{target}[1] = \"s\""),
                2 => format!("{target}[-2] = 4"),
                3 => format!("{target}[-1] = \"t\""),
                _ => format!("{target}[0] += 1"),
            }
        };
        self.line(write);
        let read = if nested {
            format!("{}: string = {name}.fetch(0)[1]", self.name("t"))
        } else {
            format!("{}: string = {name}[1]", self.name("t"))
        };
        self.line(read);
    }

    /// A block that breaks with a value, whose type joins the call's; the
    /// unsound form declares the call's type without it.
    fn break_values(&mut self, env: &mut Env, unsound: bool) {
        let other = self.scalar_ty();
        let value = self.expr(env, &other, 0, true);
        let calls: [(&str, Ty, &str, &str); 13] = [
            ("[1, 2].each", Ty::array(Ty::Int), "|q|", "nil"),
            ("[1, 2].map", Ty::array(Ty::Int), "|q|", "q"),
            ("3.times", Ty::Int, "|q|", "nil"),
            ("[1, 2].select", Ty::array(Ty::Int), "|q|", "true"),
            ("1.upto(3)", Ty::Int, "|q|", "nil"),
            (
                "[1, 2].each_with_index",
                Ty::array(Ty::Int),
                "|q, n|",
                "nil",
            ),
            ("{ a: 1 }.each", Ty::hash(Ty::Int), "|k, q|", "nil"),
            ("[1, 2].find", Ty::opt(Ty::Int), "|q|", "false"),
            ("\"ab\".each_char", Ty::Str, "|q|", "nil"),
            ("[1, 2].reverse_each", Ty::array(Ty::Int), "|q|", "nil"),
            ("[1, 2].sort_by", Ty::array(Ty::Int), "|q|", "q"),
            ("[1, 2].count", Ty::Int, "|q|", "true"),
            ("(1..3).map", Ty::array(Ty::Int), "|q|", "q"),
        ];
        let (call, ty, params, body) = calls[self.rng.below(calls.len())].clone();
        let condition = if params == "|q|" && !call.contains('"') && self.rng.chance(50) {
            "q == 2"
        } else {
            "true"
        };
        let name = self.name("b");
        let declared = if unsound {
            ty
        } else {
            let mut options = vec![ty.strip(), other];
            if ty.nilable() {
                options.push(Ty::Nil);
            }
            options.dedup();
            if options.len() == 1 {
                options.pop().unwrap()
            } else {
                Ty::Union(options)
            }
        };
        self.line(format!(
            "{name}: {} = {call} {{ {params}\n  break {value} if {condition}\n  {body}\n}}",
            self.render(&declared)
        ));
        self.line(format!("p({name})"));
    }

    /// A block with `next`; the unsound form gives it a value of another
    /// type than the block's last expression.
    fn next_values(&mut self, env: &mut Env, unsound: bool) {
        let name = self.name("nx");
        let (call, ty) = match self.rng.below(7) {
            0 => ("[1, 2].map", Ty::array(Ty::Int)),
            1 => ("[1, 2].filter_map", Ty::array(Ty::Int)),
            2 => ("[1, 2].flat_map", Ty::array(Ty::Int)),
            3 => ("[1, 2].sum(0)", Ty::Int),
            4 => ("[1, 2].max_by", Ty::opt(Ty::Int)),
            5 => ("[1, 2].group_by", Ty::hash(Ty::array(Ty::Int))),
            _ => ("[1, 2].to_h", Ty::hash(Ty::Int)),
        };
        let last = match call {
            "[1, 2].to_h" => "[q.to_s, q]",
            "[1, 2].group_by" => "q.to_s",
            _ => "q",
        };
        let next = if unsound {
            let other = self.scalar_ty();
            let value = self.expr(env, &other, 0, true);
            if call.ends_with("to_h") && self.rng.chance(50) {
                format!("[q.to_s, {value}]")
            } else if self.rng.chance(20) {
                String::new()
            } else {
                value
            }
        } else {
            match call {
                "[1, 2].to_h" => "[\"n\", 0]".to_owned(),
                "[1, 2].group_by" => "\"n\"".to_owned(),
                "[1, 2].filter_map" if self.rng.chance(50) => "nil".to_owned(),
                _ => "0".to_owned(),
            }
        };
        let condition = if self.rng.chance(50) { "q > 1" } else { "true" };
        self.line(format!(
            "{name}: {} = {call} {{ |q|\n  next {next} if {condition}\n  {last}\n}}",
            self.render(&ty)
        ));
        self.line(format!("p({name})"));
        let callee = self.functions.iter().find(|def| {
            def.block
                .as_ref()
                .is_some_and(|block| block.result.is_some() && !block.optional)
                && def.rank < env.rank
        });
        if let Some(def) = callee {
            let def = def.clone();
            let call = self.call_with_block(env, &def, 0, Some(unsound));
            self.line(format!("p({call})"));
        }
    }

    /// Destructuring of arrays and tuples, in assignments and block
    /// parameters; the unsound form reads a missing element as present.
    fn destructure(&mut self, unsound: bool) {
        let a = self.name("da");
        let b = self.name("db");
        let (source, ty, exact) = match self.rng.below(9) {
            0 => ("[1]", Ty::Int, false),
            1 => ("[1, 2, 3].first(1)", Ty::Int, false),
            2 => ("7.divmod(2)", Ty::Int, true),
            3 => ("\"a,b\".partition(\",\")", Ty::Str, true),
            4 => ("[[1, 2]].fetch(0)", Ty::Int, false),
            5 => ("[1].minmax", Ty::Int, false),
            6 => ("{ a: 1 }.to_a.fetch(0)", Ty::Int, true),
            7 => ("7.5.divmod(2)", Ty::Float, true),
            _ => ("[1, 2].partition { |q| q > 5 }", Ty::array(Ty::Int), true),
        };
        self.line(format!("{a}, {b} = {source}"));
        let ty = if exact || unsound { ty } else { Ty::opt(ty) };
        self.line(format!("{}: {} = {b}", self.name("t"), self.render(&ty)));
        if self.rng.chance(50) {
            let block = match self.rng.below(6) {
                0 => "[1, 2, 3].each_slice(2) { |q, r|",
                1 => "[1, 2, 3].combination(2).each { |q, r|",
                2 => "[1, 2, 3].window(2).each { |q, r|",
                3 => "[[1], [2, 3]].each { |q, r|",
                4 => "[1, 2, 3].each_cons(2) { |q, r|",
                _ => "[[1, 2]].each_with_index { |(q, r), n|",
            };
            let ty = if unsound { "int" } else { "int?" };
            self.line(format!("{block}\n  {}: {ty} = r\n}}", self.name("t")));
        }
    }

    /// A `case` whose subject may match no arm; the sound form has an
    /// `else`.
    fn optional_case(&mut self, env: &mut Env, unsound: bool) {
        let name = self.name("oc");
        let typed = self.name("t");
        let fallback = if unsound { "" } else { "\n  else 9" };
        match self.rng.below(5) {
            0 if self.enums > 0 => {
                let index = self.rng.below(self.enums);
                let members = MEMBERS[index];
                self.line(format!("{name}: E{index}? = nil"));
                if self.rng.chance(50) {
                    self.line(format!("{name} = E{index}::{}", members[0]));
                }
                let arms: Vec<String> = members
                    .iter()
                    .enumerate()
                    .map(|(position, member)| format!("  when E{index}::{member} then {position}"))
                    .collect();
                self.line(format!(
                    "{typed}: int = case {name}\n{}{fallback}\nend",
                    arms.join("\n")
                ));
            }
            1 => {
                self.line(format!("{name}: bool? = nil"));
                self.line(format!(
                    "{typed}: int = case {name}\n  when true then 1\n  when false then 0{fallback}\nend"
                ));
            }
            2 => {
                let subject = self.expr(env, &Ty::Int, 0, true);
                self.line(format!(
                    "{typed}: int = case {subject}\n  when 1 then 1\n  when 2 then 2{fallback}\nend"
                ));
            }
            3 if self.enums > 0 => {
                let index = self.rng.below(self.enums);
                let members = MEMBERS[index];
                self.line(format!("{name}: enum_value = E{index}::{}", members[0]));
                let arms: Vec<String> = members
                    .iter()
                    .enumerate()
                    .map(|(position, member)| format!("  when E{index}::{member} then {position}"))
                    .collect();
                self.line(format!(
                    "{typed}: int = case {name}\n{}{fallback}\nend",
                    arms.join("\n")
                ));
            }
            _ => {
                let subject = self.expr(env, &Ty::Bool, 0, false);
                self.line(format!(
                    "{typed}: int = case {subject}\n  when true then 1\n  when false then 0\nend"
                ));
            }
        }
        self.line(format!("p({typed})"));
    }

    /// A collection assigned where a wider element type is expected,
    /// updated through the wider name, and read through the narrower one,
    /// which value semantics keep apart.
    fn variance(&mut self) {
        let name = self.name("vs");
        let wide = self.name("vw");
        let (narrow, widened, extra) = match self.rng.below(5) {
            0 => ("array<int>", "array<int?>", "nil"),
            1 => ("array<int>", "array<any>", "\"s\""),
            2 => ("hash<string, int>", "hash<string, int?>", "nil"),
            3 => ("array<[int, int]>", "array<array<int>>", "[1]"),
            _ => ("array<int>", "array<int | string>", "\"s\""),
        };
        let literal = match narrow {
            "array<[int, int]>" => "[[1, 2]]",
            other if other.starts_with("array") => "[1, 2]",
            _ => "{ a: 1 }",
        };
        self.line(format!("{name}: {narrow} = {literal}"));
        self.line(format!("{wide}: {widened} = {name}"));
        if narrow.starts_with("array") {
            self.line(format!("{wide} << {extra}"));
            self.line(format!("{wide}[0] = {extra}"));
            let element = if narrow == "array<[int, int]>" {
                "[int, int]"
            } else {
                "int"
            };
            self.line(format!("{}: {element} = {name}.fetch(0)", self.name("t")));
        } else {
            self.line(format!("{wide}[\"a\"] = {extra}"));
            self.line(format!("{}: int = {name}.fetch(\"a\")", self.name("t")));
        }
        self.line(format!("p({name}, {wide})"));
    }

    /// A local assigned on some paths, read after them; the sound form
    /// assigns it first.
    fn definite_assignment(&mut self, unsound: bool) {
        let name = self.name("da");
        if !unsound {
            self.line(format!("{name} = 0"));
        }
        let form = match self.rng.below(9) {
            0 => format!("loop {{\n  break if true\n  {name} = 1\n}}"),
            1 => format!("empty_: array<int> = []\nempty_.each {{ |q| {name} = 1 }}"),
            2 => format!("begin\n  raise \"x\"\n  {name} = 1\nrescue\n  p(0)\nend"),
            3 => format!("q_ = 0\nwhile q_ > 0\n  {name} = 1\nend"),
            4 => format!("if true\n  p(0)\nelsif false\n  {name} = 1\nend"),
            5 => format!("{name} = 1 if false"),
            6 => format!("empty_: array<int> = []\nfor q in empty_\n  {name} = 1\nend"),
            7 => format!("[1].each {{ |q| {name} = 1 if q > 5 }}"),
            _ => format!("if false\n  {name} = 1\nelse\n  raise \"x\" if false\nend"),
        };
        self.line(form);
        self.line(format!("{}: int = {name}", self.name("t")));
    }

    // ----- Expressions -----

    fn string_literal(&mut self) -> String {
        let words = [
            "\"a\"", "\"b\"", "\"id\"", "\"\"", "\"x y\"", "\"42\"", "\"Zed\"",
        ];
        (*self.rng.pick(&words)).to_owned()
    }

    /// A literal of `ty`, for defaults and initial values, which always
    /// have a declared context.
    fn literal(&mut self, ty: &Ty, depth: usize) -> String {
        match ty {
            Ty::Int => ["0", "1", "2", "-3", "7", "100"][self.rng.below(6)].to_owned(),
            Ty::Float => ["0.5", "1.0", "-2.25", "3.0"][self.rng.below(4)].to_owned(),
            Ty::Str => self.string_literal(),
            Ty::Sym => [":a", ":b", ":ok"][self.rng.below(3)].to_owned(),
            Ty::Bool => if self.rng.chance(50) { "true" } else { "false" }.to_owned(),
            Ty::Nil => "nil".to_owned(),
            Ty::Opt(inner) => {
                if depth == 0 || self.rng.chance(50) {
                    "nil".to_owned()
                } else {
                    self.literal(inner, depth)
                }
            }
            Ty::Any => self.literal(&Ty::Int, depth),
            Ty::Union(options) => {
                let option = self.rng.pick(options).clone();
                self.literal(&option, depth)
            }
            Ty::Array(inner) => {
                if depth == 0 || self.rng.chance(40) {
                    "[]".to_owned()
                } else {
                    format!("[{}]", self.literal(inner, depth - 1))
                }
            }
            Ty::Hash(inner) => {
                if depth == 0 || self.rng.chance(40) {
                    "{}".to_owned()
                } else {
                    format!("{{ k: {} }}", self.literal(inner, depth - 1))
                }
            }
            Ty::Shape(fields) => {
                let mut parts = Vec::new();
                for field in fields {
                    if !field.optional || self.rng.chance(50) {
                        let value = self.literal(&field.ty, depth.saturating_sub(1));
                        parts.push(format!("{}: {value}", field.name));
                    }
                }
                format!("{{ {} }}", parts.join(", "))
            }
            Ty::Tuple(items) => {
                let parts: Vec<String> = items
                    .iter()
                    .map(|ty| self.literal(ty, depth.saturating_sub(1)))
                    .collect();
                format!("[{}]", parts.join(", "))
            }
            Ty::Enum(index) => {
                let members = MEMBERS[*index];
                let member = members[self.rng.below(members.len())];
                if self.rng.chance(50) {
                    format!(":{}", member.to_lowercase())
                } else {
                    format!("E{index}::{member}")
                }
            }
            Ty::Class(index) => {
                let class = self.classes[*index].clone();
                let args: Vec<String> = class
                    .init
                    .iter()
                    .map(|param| self.literal(&param.ty, depth.saturating_sub(1)))
                    .collect();
                if args.is_empty() {
                    format!("{}.new", class.name)
                } else {
                    format!("{}.new({})", class.name, args.join(", "))
                }
            }
        }
    }

    /// A JSON text of a value of `ty`.
    fn json_of(&mut self, ty: &Ty) -> String {
        match ty {
            Ty::Int => self.rng.range(-5, 50).to_string(),
            Ty::Float => "2.5".to_owned(),
            Ty::Str => "\"j\"".to_owned(),
            Ty::Bool => "true".to_owned(),
            Ty::Opt(inner) => {
                if self.rng.chance(40) {
                    "null".to_owned()
                } else {
                    self.json_of(inner)
                }
            }
            Ty::Array(inner) => {
                let items: Vec<String> = (0..self.rng.below(3))
                    .map(|_| self.json_of(inner))
                    .collect();
                format!("[{}]", items.join(","))
            }
            Ty::Hash(inner) => {
                let items: Vec<String> = (0..self.rng.below(3))
                    .map(|index| format!("\"k{index}\":{}", self.json_of(inner)))
                    .collect();
                format!("{{{}}}", items.join(","))
            }
            Ty::Shape(fields) => {
                let mut items = Vec::new();
                for field in fields {
                    if !field.optional || self.rng.chance(50) {
                        items.push(format!("\"{}\":{}", field.name, self.json_of(&field.ty)));
                    }
                }
                format!("{{{}}}", items.join(","))
            }
            Ty::Tuple(items) => {
                let items: Vec<String> = items.iter().map(|ty| self.json_of(ty)).collect();
                format!("[{}]", items.join(","))
            }
            _ => "1".to_owned(),
        }
    }

    /// Whether `ty` can be written as JSON the generator produces.
    fn jsonable(ty: &Ty) -> bool {
        match ty {
            Ty::Int | Ty::Float | Ty::Str | Ty::Bool => true,
            Ty::Opt(inner) | Ty::Array(inner) | Ty::Hash(inner) => Self::jsonable(inner),
            Ty::Shape(fields) => fields.iter().all(|field| Self::jsonable(&field.ty)),
            Ty::Tuple(items) => items.iter().all(Self::jsonable),
            _ => false,
        }
    }

    /// Readable names whose current type is assignable to `ty`: locals,
    /// and in a method the class's instance variables and getters.
    fn readable(&self, env: &Env, ty: &Ty) -> Vec<String> {
        let mut names: Vec<String> = env
            .locals
            .iter()
            .filter(|local| Self::assignable(ty, &local.current))
            .map(|local| local.name.clone())
            .collect();
        if let Some((class, initializing)) = env.class {
            for field in &self.classes[class].fields {
                if Self::assignable(ty, &field.ty) && (!initializing || field.default.is_some()) {
                    names.push(format!("@{}", field.name));
                    if field.access != Access::Plain {
                        names.push(format!("self.{}", field.name));
                    }
                }
            }
        }
        if let Some(class) = env.class_index().or(env.statics) {
            for (name, var) in &self.classes[class].class_vars {
                if Self::assignable(ty, var) {
                    names.push(name.clone());
                }
            }
        }
        names
    }

    /// An expression of type `ty`. `context` says whether the position
    /// declares the type, which empty collections and `nil` need.
    fn expr(&mut self, env: &mut Env, ty: &Ty, depth: usize, context: bool) -> String {
        let names = self.readable(env, ty);
        if !names.is_empty() && self.rng.chance(if depth == 0 { 60 } else { 30 }) {
            return self.rng.pick(&names).clone();
        }
        if depth > 0 && self.rng.chance(30) {
            if let Some(call) = self.call_of(env, ty, depth) {
                return call;
            }
        }
        if depth > 0 && self.rng.chance(10) {
            let condition = self.expr(env, &Ty::Bool, depth - 1, false);
            let a = self.expr(env, ty, depth - 1, context);
            let b = self.expr(env, ty, depth - 1, context);
            return match self.rng.below(3) {
                0 => format!("({condition} ? {a} : {b})"),
                1 => format!("(if {condition} then ({a}) else ({b}) end)"),
                // A symbol may follow `then` and `else` directly.
                _ => format!("(if {condition} then {a} else {b} end)"),
            };
        }
        if let (true, Ty::Opt(inner)) = (depth > 0 && self.rng.chance(10), ty) {
            // An index abutting a `case` that spans lines indexes its value,
            // an array whose element may be missing.
            let value = self.case_expr(env, &Ty::array((**inner).clone()), depth - 1);
            return format!("{value}[{}]", self.rng.below(2));
        }
        if depth > 0 && self.rng.chance(4) {
            return self.case_expr(env, ty, depth - 1);
        }
        match ty {
            Ty::Int => self.int_expr(env, depth),
            Ty::Float => self.float_expr(env, depth),
            Ty::Str => self.str_expr(env, depth),
            Ty::Sym => self.sym_expr(env, depth),
            Ty::Bool => self.bool_expr(env, depth),
            Ty::Nil => "nil".to_owned(),
            Ty::Any => {
                let inner = self.ty(1);
                if depth > 0 && self.rng.chance(20) && Self::jsonable(&inner) {
                    let json = self.json_of(&inner).replace('"', "\\\"");
                    format!("JSON.parse(\"{json}\")")
                } else {
                    self.expr(env, &inner, depth.saturating_sub(1), true)
                }
            }
            Ty::Opt(inner) => self.opt_expr(env, inner, depth, context),
            Ty::Union(options) => {
                let option = self.rng.pick(options).clone();
                self.expr(env, &option, depth, true)
            }
            Ty::Array(inner) => self.array_expr(env, inner, depth, context),
            Ty::Hash(inner) => self.hash_expr(env, inner, depth, context),
            Ty::Shape(fields) => self.shape_expr(env, fields, depth),
            Ty::Tuple(items) => self.tuple_expr(env, items, depth, context),
            Ty::Enum(index) => {
                let members = MEMBERS[*index];
                let member = members[self.rng.below(members.len())];
                if context && self.rng.chance(40) {
                    format!(":{}", member.to_lowercase())
                } else {
                    format!("E{index}::{member}")
                }
            }
            Ty::Class(index) => self.new_instance(env, *index, depth),
        }
    }

    fn new_instance(&mut self, env: &mut Env, index: usize, depth: usize) -> String {
        if env.class_index() == Some(index) {
            // Constructing its own class could recurse through initialize.
            return "self".to_owned();
        }
        let class = self.classes[index].clone();
        let args: Vec<String> = class
            .init
            .iter()
            .map(|param| self.expr(env, &param.ty, depth.saturating_sub(1), true))
            .collect();
        if args.is_empty() {
            format!("{}.new", class.name)
        } else {
            format!("{}.new({})", class.name, args.join(", "))
        }
    }

    fn int_expr(&mut self, env: &mut Env, depth: usize) -> String {
        if depth == 0 {
            return self.literal(&Ty::Int, 0);
        }
        let d = depth - 1;
        match self.rng.below(22) {
            0 => format!(
                "({} + {})",
                self.expr(env, &Ty::Int, d, false),
                self.expr(env, &Ty::Int, d, false)
            ),
            1 => format!(
                "({} - {})",
                self.expr(env, &Ty::Int, d, false),
                self.expr(env, &Ty::Int, d, false)
            ),
            2 => format!(
                "({} * {})",
                self.expr(env, &Ty::Int, d, false),
                self.expr(env, &Ty::Int, d, false)
            ),
            3 => {
                let ty = Ty::array(self.ty(1));
                format!("{}.length", self.expr(env, &ty, d, false))
            }
            4 => format!("{}.length", self.expr(env, &Ty::Str, d, false)),
            5 => format!("{}.to_i", self.expr(env, &Ty::Str, d, false)),
            6 => format!("{}.sum", self.expr(env, &Ty::array(Ty::Int), d, false)),
            7 => {
                let array = self.expr(env, &Ty::array(Ty::Int), d, false);
                let default = self.expr(env, &Ty::Int, d, true);
                format!("{array}.fetch(0, {default})")
            }
            8 => {
                let array = self.expr(env, &Ty::array(Ty::Int), d, false);
                format!("{array}.count {{ |q| {} }}", self.bool_of("q", &Ty::Int))
            }
            9 => {
                let array = self.expr(env, &Ty::array(Ty::Int), d, false);
                format!("{array}.reduce(0) {{ |acc, q| acc + q }}")
            }
            10 => format!("{}.abs", self.expr(env, &Ty::Int, d, false)),
            11 => {
                let opt = self.expr(env, &Ty::opt(Ty::Int), d, false);
                format!("[{opt}].compact.fetch(0, 0)")
            }
            12 => format!("{}.floor", self.expr(env, &Ty::Float, d, false)),
            13 => {
                let ty = self.scalar_comparable();
                format!(
                    "({} <=> {})",
                    self.expr(env, &ty, d, false),
                    self.expr(env, &ty, d, false)
                )
            }
            14 => format!("loop {{ break {} }}", self.expr(env, &Ty::Int, d, true)),
            15 => format!(
                "{}.fetch(\"k\", 0)",
                self.expr(env, &Ty::hash(Ty::Int), d, false)
            ),
            16 => format!(
                "({} // {})",
                self.expr(env, &Ty::Int, d, false),
                1 + self.rng.below(3)
            ),
            17 => format!(
                "({} % {})",
                self.expr(env, &Ty::Int, d, false),
                1 + self.rng.below(3)
            ),
            18 => {
                let ty = Ty::hash(self.ty(1));
                format!("{}.length", self.expr(env, &ty, d, false))
            }
            19 => format!(
                "{}.index(\"a\", 0).to_s.length",
                self.expr(env, &Ty::Str, d, false)
            ),
            20 => {
                let tuple = self.expr(env, &Ty::Tuple(vec![Ty::Int, Ty::Int]), d, false);
                format!("{tuple}[{}]", self.rng.range(-2, 1))
            }
            _ => format!("{}.count(\"a\")", self.expr(env, &Ty::Str, d, false)),
        }
    }

    fn scalar_comparable(&mut self) -> Ty {
        match self.rng.below(3) {
            0 => Ty::Int,
            1 => Ty::Float,
            _ => Ty::Str,
        }
    }

    fn float_expr(&mut self, env: &mut Env, depth: usize) -> String {
        if depth == 0 {
            return self.literal(&Ty::Float, 0);
        }
        let d = depth - 1;
        match self.rng.below(8) {
            0 => format!(
                "({} / {})",
                self.expr(env, &Ty::Int, d, false),
                1 + self.rng.below(4)
            ),
            1 => format!("{}.to_f", self.expr(env, &Ty::Int, d, false)),
            2 => format!(
                "({} + {})",
                self.expr(env, &Ty::Float, d, false),
                self.expr(env, &Ty::Float, d, false)
            ),
            3 => format!(
                "({} * {})",
                self.expr(env, &Ty::Float, d, false),
                self.expr(env, &Ty::Float, d, false)
            ),
            4 => format!("Math.sqrt({})", self.expr(env, &Ty::Int, d, false)),
            5 => format!("{}.abs", self.expr(env, &Ty::Float, d, false)),
            6 => format!(
                "{}.sum(0.0)",
                self.expr(env, &Ty::array(Ty::Float), d, false)
            ),
            _ => format!("to_float({})", self.expr(env, &Ty::Int, d, false)),
        }
    }

    fn str_expr(&mut self, env: &mut Env, depth: usize) -> String {
        if depth == 0 {
            return self.string_literal();
        }
        let d = depth - 1;
        match self.rng.below(14) {
            0 => {
                let ty = self.ty(1);
                format!("\"v=#{{{}}}\"", self.expr(env, &ty, d, false))
            }
            1 => format!(
                "({} + {})",
                self.expr(env, &Ty::Str, d, false),
                self.expr(env, &Ty::Str, d, false)
            ),
            2 => {
                let ty = self.ty(1);
                let method = if matches!(ty, Ty::Hash(_) | Ty::Shape(_)) {
                    "inspect"
                } else {
                    "to_s"
                };
                format!("{}.{method}", self.expr(env, &ty, d, false))
            }
            3 => format!("{}.upcase", self.expr(env, &Ty::Str, d, false)),
            4 => format!(
                "{}.join(\",\")",
                self.expr(env, &Ty::array(Ty::Str), d, false)
            ),
            5 => format!("{}.strip", self.expr(env, &Ty::Str, d, false)),
            6 => format!("{}.reverse", self.expr(env, &Ty::Str, d, false)),
            7 if self.enums > 0 => {
                let index = self.rng.below(self.enums);
                format!("{}.name", self.expr(env, &Ty::Enum(index), d, false))
            }
            8 => format!(
                "{}.fetch(0, \"\")",
                self.expr(env, &Ty::array(Ty::Str), d, false)
            ),
            9 => format!("format(\"%d\", {})", self.expr(env, &Ty::Int, d, false)),
            10 => format!("{}.keys.join", self.expr(env, &Ty::hash(Ty::Int), d, false)),
            11 => {
                let ty = self.ty(1);
                if matches!(ty, Ty::Class(_)) {
                    return self.string_literal();
                }
                format!("{}.inspect", self.expr(env, &ty, d, false))
            }
            12 => format!("{}.to_s", self.expr(env, &Ty::Sym, d, false)),
            _ => format!("{}.sub(\"a\", \"b\")", self.expr(env, &Ty::Str, d, false)),
        }
    }

    fn sym_expr(&mut self, env: &mut Env, depth: usize) -> String {
        if depth == 0 {
            return self.literal(&Ty::Sym, 0);
        }
        match self.rng.below(3) {
            0 => format!("{}.to_sym", self.expr(env, &Ty::Str, depth - 1, false)),
            1 if self.enums > 0 => {
                let index = self.rng.below(self.enums);
                format!(
                    "{}.symbol",
                    self.expr(env, &Ty::Enum(index), depth - 1, false)
                )
            }
            _ => self.literal(&Ty::Sym, 0),
        }
    }

    /// A condition on a block parameter `name` of type `ty`.
    fn bool_of(&mut self, name: &str, ty: &Ty) -> String {
        match ty {
            Ty::Int => format!("{name} > {}", self.rng.range(-1, 3)),
            Ty::Str => format!("{name}.empty?"),
            Ty::Float => format!("{name} < 1.5"),
            Ty::Bool => name.to_owned(),
            _ => format!("{name} == {name}"),
        }
    }

    fn bool_expr(&mut self, env: &mut Env, depth: usize) -> String {
        if depth == 0 {
            return self.literal(&Ty::Bool, 0);
        }
        let d = depth - 1;
        match self.rng.below(14) {
            0 => {
                let ty = self.scalar_comparable();
                let op = ["<", "<=", ">", ">="][self.rng.below(4)];
                format!(
                    "({} {op} {})",
                    self.expr(env, &ty, d, false),
                    self.expr(env, &ty, d, false)
                )
            }
            1 => {
                let ty = self.ty(1);
                let op = if self.rng.chance(50) { "==" } else { "!=" };
                format!(
                    "({} {op} {})",
                    self.expr(env, &ty, d, false),
                    self.expr(env, &ty, d, false)
                )
            }
            2 => {
                let ty = Ty::opt(self.ty(1).strip());
                let op = if self.rng.chance(50) { "==" } else { "!=" };
                let value = self.expr(env, &ty, d, false);
                if value == "nil" {
                    "true".to_owned()
                } else {
                    format!("({value} {op} nil)")
                }
            }
            3 => format!("!{}", self.expr(env, &Ty::Bool, d, false)),
            4 => format!(
                "({} && {})",
                self.expr(env, &Ty::Bool, d, false),
                self.expr(env, &Ty::Bool, d, false)
            ),
            5 => format!(
                "({} || {})",
                self.expr(env, &Ty::Bool, d, false),
                self.expr(env, &Ty::Bool, d, false)
            ),
            6 => {
                let array = self.expr(env, &Ty::array(Ty::Int), d, false);
                format!("{array}.include?({})", self.expr(env, &Ty::Int, d, false))
            }
            7 => {
                let ty = Ty::array(self.ty(1));
                format!("{}.empty?", self.expr(env, &ty, d, false))
            }
            8 => {
                let hash = self.expr(env, &Ty::hash(Ty::Int), d, false);
                format!("{hash}.key?({})", self.expr(env, &Ty::Str, d, false))
            }
            9 => {
                let ty = self.ty(1);
                let atoms = [
                    "int", "string", "float", "nil", "array", "hash", "bool", "symbol",
                ];
                let atom = atoms[self.rng.below(atoms.len())];
                format!("{}.is_type?(:{atom})", self.expr(env, &ty, d, false))
            }
            10 => format!("{}.even?", self.expr(env, &Ty::Int, d, false)),
            11 => {
                let array = self.expr(env, &Ty::array(Ty::Int), d, false);
                format!("{array}.any? {{ |q| {} }}", self.bool_of("q", &Ty::Int))
            }
            12 => format!("{}.start_with?(\"a\")", self.expr(env, &Ty::Str, d, false)),
            _ => {
                let array = self.expr(env, &Ty::array(Ty::Str), d, false);
                format!("{array}.all? {{ |q| {} }}", self.bool_of("q", &Ty::Str))
            }
        }
    }

    fn opt_expr(&mut self, env: &mut Env, inner: &Ty, depth: usize, context: bool) -> String {
        if depth == 0 || self.rng.chance(25) {
            if context && self.rng.chance(40) {
                return "nil".to_owned();
            }
            return self.expr(env, inner, depth, context);
        }
        let d = depth - 1;
        let array = Ty::array(inner.clone());
        match self.rng.below(9) {
            0 => {
                let receiver = self.expr(env, &array, d, false);
                let index = self.rng.range(-2, 3);
                // A parenthesized receiver cannot be indexed.
                if receiver.starts_with('(') {
                    format!("{receiver}.values_at({index}).fetch(0)")
                } else {
                    format!("{receiver}[{index}]")
                }
            }
            1 => format!("{}.first", self.expr(env, &array, d, false)),
            2 => format!("{}.last", self.expr(env, &array, d, false)),
            3 => {
                let hash = self.expr(env, &Ty::hash(inner.clone()), d, false);
                let key = self.expr(env, &Ty::Str, d, false);
                if hash.starts_with('(') || hash.starts_with('{') {
                    format!("{hash}.values_at({key}).fetch(0)")
                } else {
                    format!("{hash}[{key}]")
                }
            }
            4 => {
                let body = self.bool_of("q", inner);
                format!("{}.find {{ |q| {body} }}", self.expr(env, &array, d, false))
            }
            5 if inner.comparable() => format!("{}.max", self.expr(env, &array, d, false)),
            6 => format!(
                "{}.values_at(0, 5).fetch(1)",
                self.expr(env, &array, d, false)
            ),
            7 => format!("{}.sample", self.expr(env, &array, d, false)),
            _ => self.expr(env, inner, d, context),
        }
    }

    fn array_expr(&mut self, env: &mut Env, inner: &Ty, depth: usize, context: bool) -> String {
        if depth == 0 {
            if context && self.rng.chance(30) {
                return "[]".to_owned();
            }
            let item = self.expr(env, inner, 0, true);
            return format!("[{item}]");
        }
        let d = depth - 1;
        let this = Ty::array(inner.clone());
        match self.rng.below(16) {
            0 | 1 => {
                let items: Vec<String> = (0..1 + self.rng.below(3))
                    .map(|_| self.expr(env, inner, d, true))
                    .collect();
                format!("[{}]", items.join(", "))
            }
            2 => {
                let source = self.ty(1);
                let body = self.expr_in_block(env, "q", &source, inner, d);
                format!(
                    "{}.map {{ |q| {body} }}",
                    self.expr(env, &Ty::array(source), d, false)
                )
            }
            3 => {
                let body = self.bool_of("q", inner);
                format!(
                    "{}.select {{ |q| {body} }}",
                    self.expr(env, &this, d, false)
                )
            }
            4 => format!("{}.reverse", self.expr(env, &this, d, false)),
            5 => format!(
                "({} + {})",
                self.expr(env, &this, d, false),
                self.expr(env, &this, d, false)
            ),
            6 => format!(
                "{}.first({})",
                self.expr(env, &this, d, false),
                self.rng.below(3)
            ),
            7 => format!(
                "{}.compact",
                self.expr(env, &Ty::array(Ty::opt(inner.clone())), d, false)
            ),
            8 if inner.comparable() => format!("{}.sort", self.expr(env, &this, d, false)),
            9 if *inner == Ty::Str => {
                format!("{}.keys", self.expr(env, &Ty::hash(Ty::Int), d, false))
            }
            10 => format!(
                "{}.values",
                self.expr(env, &Ty::hash(inner.clone()), d, false)
            ),
            11 => format!("{}.uniq", self.expr(env, &this, d, false)),
            12 if *inner == Ty::Int => format!("(0..{}).to_a", self.rng.below(4)),
            13 => {
                let source = self.ty(1);
                let body = self.expr_in_block(env, "q", &source, inner, d);
                format!(
                    "{}.flat_map {{ |q| [{body}] }}",
                    self.expr(env, &Ty::array(source), d, false)
                )
            }
            14 if *inner == Ty::Str => format!("{}.chars", self.expr(env, &Ty::Str, d, false)),
            _ => {
                let item = self.expr(env, inner, d, true);
                format!("[{item}]")
            }
        }
    }

    /// An expression of `result` in a block whose parameter `name` has
    /// type `param`.
    fn expr_in_block(
        &mut self,
        env: &mut Env,
        name: &str,
        param: &Ty,
        result: &Ty,
        depth: usize,
    ) -> String {
        let mut inner = env.clone();
        inner
            .locals
            .push(Local::inferred(name.to_owned(), param.clone()));
        inner.block = Some(Some(result.clone()));
        self.expr(&mut inner, result, depth, true)
    }

    fn hash_expr(&mut self, env: &mut Env, inner: &Ty, depth: usize, context: bool) -> String {
        if depth == 0 {
            if context && self.rng.chance(30) {
                return "{}".to_owned();
            }
            let value = self.expr(env, inner, 0, true);
            return format!("{{ k: {value} }}");
        }
        let d = depth - 1;
        let this = Ty::hash(inner.clone());
        match self.rng.below(8) {
            0 | 1 => {
                let items: Vec<String> = (0..1 + self.rng.below(2))
                    .map(|index| format!("k{index}: {}", self.expr(env, inner, d, true)))
                    .collect();
                format!("{{ {} }}", items.join(", "))
            }
            2 => format!(
                "{}.select {{ |k, q| k != \"z\" }}",
                self.expr(env, &this, d, false)
            ),
            3 => format!(
                "{}.merge({})",
                self.expr(env, &this, d, false),
                self.expr(env, &this, d, false)
            ),
            4 => {
                let body = self.expr_in_block(env, "q", &Ty::Int, inner, d);
                let array = self.expr(env, &Ty::array(Ty::Int), d, false);
                format!("{array}.to_h {{ |q| [q.to_s, {body}] }}")
            }
            5 => {
                let source = self.ty(1);
                let body = self.expr_in_block(env, "q", &source, inner, d);
                let hash = self.expr(env, &Ty::hash(source), d, false);
                format!("{hash}.transform_values {{ |q| {body} }}")
            }
            6 => format!("{}.except(\"k0\")", self.expr(env, &this, d, false)),
            _ => {
                let value = self.expr(env, inner, d, true);
                format!("{{ k: {value} }}")
            }
        }
    }

    fn shape_expr(&mut self, env: &mut Env, fields: &[Field], depth: usize) -> String {
        let ty = Ty::Shape(fields.to_vec());
        // A tuple in a type written as a value parses as an array.
        if depth > 0 && Self::jsonable(&ty) && !has_tuple(&ty) && self.rng.chance(25) {
            let json = self.json_of(&ty).replace('"', "\\\"");
            return format!("JSON.parse_as(\"{json}\", {})", self.render(&ty));
        }
        let mut parts = Vec::new();
        for field in fields {
            if !field.optional || self.rng.chance(50) {
                let value = self.expr(env, &field.ty, depth.saturating_sub(1), true);
                parts.push(format!("{}: {value}", field.name));
            }
        }
        format!("{{ {} }}", parts.join(", "))
    }

    fn tuple_expr(&mut self, env: &mut Env, items: &[Ty], depth: usize, context: bool) -> String {
        if !context && items == [Ty::Int, Ty::Int] && depth > 0 {
            // A literal is a tuple only where one is expected.
            let value = self.expr(env, &Ty::Int, depth - 1, false);
            return format!("{value}.divmod({})", 1 + self.rng.below(3));
        }
        let parts: Vec<String> = items
            .iter()
            .map(|ty| self.expr(env, ty, depth.saturating_sub(1), true))
            .collect();
        format!("[{}]", parts.join(", "))
    }

    fn case_expr(&mut self, env: &mut Env, ty: &Ty, depth: usize) -> String {
        if self.enums > 0 && self.rng.chance(50) {
            let index = self.rng.below(self.enums);
            let subject = self.expr(env, &Ty::Enum(index), depth, false);
            let members = MEMBERS[index];
            let complete = self.rng.chance(80);
            let mut arms = Vec::new();
            for (position, member) in members.iter().enumerate() {
                if !complete && position == members.len() - 1 {
                    break;
                }
                let value = self.expr(env, ty, depth, true);
                arms.push(format!("when E{index}::{member} then {}", self.wrap(value)));
            }
            if !complete {
                let value = self.expr(env, ty, depth, true);
                arms.push(format!("else {}", self.wrap(value)));
            }
            return format!("(case {subject}\n{}\nend)", arms.join("\n"));
        }
        let subject = self.expr(env, &Ty::Int, depth, false);
        let a = self.expr(env, ty, depth, true);
        let b = self.expr(env, ty, depth, true);
        let c = self.expr(env, ty, depth, true);
        let (a, b, c) = (self.wrap(a), self.wrap(b), self.wrap(c));
        format!("(case {subject}\nwhen 0 then {a}\nwhen 1..3 then {b}\nelse {c}\nend)")
    }

    /// A `when` or `else` value, in parentheses or not.
    fn wrap(&mut self, value: String) -> String {
        if self.rng.chance(50) {
            format!("({value})")
        } else {
            value
        }
    }

    // ----- Calls -----

    /// A call of a declared function, method, accessor or namespace
    /// function whose result is assignable to `ty`.
    fn call_of(&mut self, env: &mut Env, ty: &Ty, depth: usize) -> Option<String> {
        if self.rng.chance(20) {
            if let Some(call) = self.host_call(env, ty, depth) {
                return Some(call);
            }
        }
        if self.rng.chance(25) {
            if let Some(call) = self.class_expr(env, ty, depth) {
                return Some(call);
            }
        }
        let fits = |result: &Option<Ty>| {
            result
                .as_ref()
                .is_some_and(|result| Self::assignable(ty, result))
        };
        let mut options: Vec<(u8, usize, usize)> = Vec::new();
        for (index, def) in self.functions.iter().enumerate() {
            if def.rank < env.rank && fits(&def.result) {
                options.push((0, index, 0));
            }
        }
        for (index, def) in self.namespace.iter().enumerate() {
            if def.rank < env.rank && fits(&def.result) {
                options.push((1, index, 0));
            }
        }
        for (index, def) in self.inner.iter().enumerate() {
            if def.rank < env.rank && fits(&def.result) {
                options.push((6, index, 0));
            }
        }
        if env.util {
            for (index, def) in self.util.iter().enumerate() {
                if fits(&def.result) {
                    options.push((5, index, 0));
                }
            }
        }
        if env.library {
            for (index, def) in self.library.iter().enumerate() {
                if fits(&def.result) {
                    options.push((2, index, 0));
                }
            }
        }
        for (class, def) in self.classes.iter().enumerate() {
            let own = env.class_index() == Some(class);
            for (index, method) in def.methods.iter().enumerate() {
                if fits(&method.result) && (!own || method.rank < env.rank) {
                    options.push((3, class, index));
                }
            }
            for (index, field) in def.fields.iter().enumerate() {
                if field.access != Access::Plain && Self::assignable(ty, &field.ty) && !own {
                    options.push((4, class, index));
                }
            }
        }
        if options.is_empty() {
            return None;
        }
        let (kind, a, b) = *self.rng.pick(&options);
        Some(match kind {
            0 => {
                let def = self.functions[a].clone();
                self.call_with_block(env, &def, depth, None)
            }
            1 => {
                let def = self.namespace[a].clone();
                format!("N.{}", self.call_with_block(env, &def, depth, None))
            }
            2 => {
                let def = self.library[a].clone();
                let call = self.call_with_block(env, &def, depth, None);
                if self.rng.chance(50) {
                    format!("lib.{call}")
                } else {
                    call
                }
            }
            5 => {
                let def = self.util[a].clone();
                format!("util.{}", self.call_with_block(env, &def, depth, None))
            }
            6 => {
                let def = self.inner[a].clone();
                format!("N::M.{}", self.call_with_block(env, &def, depth, None))
            }
            3 => {
                let receiver = if env.class_index() == Some(a) {
                    "self".to_owned()
                } else {
                    self.expr(env, &Ty::Class(a), depth.saturating_sub(1), false)
                };
                self.method_call_on(env, a, b, &receiver, depth)
            }
            _ => {
                let receiver = self.expr(env, &Ty::Class(a), depth.saturating_sub(1), false);
                format!("{receiver}.{}", self.classes[a].fields[b].name)
            }
        })
    }

    fn method_call_on(
        &mut self,
        env: &mut Env,
        class: usize,
        method: usize,
        receiver: &str,
        depth: usize,
    ) -> String {
        let def = self.classes[class].methods[method].clone();
        let call = self.call_with_block(env, &def, depth, None);
        if receiver == "self" && self.rng.chance(50) {
            call
        } else {
            format!("{receiver}.{call}")
        }
    }

    /// A call of `def` with arguments, keywords and a block as its
    /// signature takes them. `next_other` makes the block's `next` give a
    /// value of another type.
    fn call_with_block(
        &mut self,
        env: &mut Env,
        def: &FnDef,
        depth: usize,
        next_other: Option<bool>,
    ) -> String {
        let d = depth.saturating_sub(1);
        let mut args = Vec::new();
        let mut omitted = false;
        for param in &def.params {
            match &param.kind {
                ParamKind::Required => args.push(self.expr(env, &param.ty, d, true)),
                ParamKind::Default(_) => {
                    if omitted || self.rng.chance(40) {
                        omitted = true;
                    } else {
                        args.push(self.expr(env, &param.ty, d, true));
                    }
                }
                ParamKind::Rest => {
                    let Ty::Array(element) = &param.ty else {
                        unreachable!()
                    };
                    for _ in 0..self.rng.below(3) {
                        args.push(self.expr(env, element, d, true));
                    }
                }
                ParamKind::Keyword(default) => {
                    if default.is_none() || self.rng.chance(50) {
                        let value = self.expr(env, &param.ty, d, true);
                        args.push(format!("{}: {value}", param.name));
                    }
                }
            }
        }
        let mut call = def.name.clone();
        if !args.is_empty() {
            call.push_str(&format!("({})", args.join(", ")));
        }
        let Some(block) = &def.block else { return call };
        if block.optional && self.rng.chance(40) {
            return call;
        }
        let names: Vec<String> = (0..block.params.len())
            .map(|index| format!("y{index}"))
            .collect();
        let mut inner = env.clone();
        for (name, ty) in names.iter().zip(&block.params) {
            inner.locals.push(Local::typed(name.clone(), ty.clone()));
        }
        inner.block = Some(block.result.clone());
        let body = match &block.result {
            Some(result) => {
                let value = self.expr(&mut inner, result, d, true);
                if next_other == Some(true) {
                    let other = self.scalar_ty();
                    let other = self.expr(&mut inner, &other, 0, true);
                    format!("\n  next {other} if y0 == y0\n  {value}\n")
                } else if self.rng.chance(15) {
                    let other = self.ty(1);
                    let other = self.expr(&mut inner, &other, 0, true);
                    format!("\n  break {other} if y0 == y0\n  {value}\n")
                } else {
                    format!(" {value} ")
                }
            }
            None => {
                let ty = self.ty(1);
                format!(" p({}) ", self.expr(&mut inner, &ty, d, false))
            }
        };
        call.push_str(&format!(" {{ |{}|{body}}}", names.join(", ")));
        call
    }

    fn yield_expr(&mut self, env: &mut Env, depth: usize) -> String {
        let block = env.yields.clone().unwrap();
        let args: Vec<String> = block
            .params
            .iter()
            .map(|ty| self.expr(env, ty, depth.saturating_sub(1), true))
            .collect();
        let call = format!("yield({})", args.join(", "));
        let call = match &block.result {
            Some(ty) => format!("{}: {} = {call}", self.name("yv"), self.render(ty)),
            None => call,
        };
        if block.optional && self.rng.chance(85) {
            format!("if block_given?\n  {call}\nend")
        } else {
            call
        }
    }

    /// Prints the locals a program leaves, which both builds must agree on.
    fn observe_all(&mut self, env: &Env) {
        let names: Vec<String> = env.locals.iter().map(|local| local.name.clone()).collect();
        for chunk in names.chunks(4) {
            self.line(format!("p({})", chunk.join(", ")));
        }
    }
}

impl Gen {
    // ----- Classes' operators -----

    /// Writes the class's operator methods, whose bodies see the instance.
    fn operators(&mut self, index: usize) {
        let class = self.classes[index].clone();
        let own = Ty::Class(index);
        let mut env = self.top_env();
        env.function = Some(None);
        env.class = Some((index, false));
        env.rank = 1;
        if class.plus {
            let other = if self.rng.chance(50) { "other" } else { "self" };
            self.line(format!(
                "def +(other: {}) -> {}",
                self.render(&own),
                self.render(&own)
            ));
            self.line(format!("  {other}"));
            self.line("end");
        }
        if class.compare {
            for op in ["==", "<"] {
                let mut inner = env.clone();
                inner.function = Some(Some(Ty::Bool));
                self.line(format!("def {op}(other: {}) -> bool", self.render(&own)));
                self.indent += 1;
                let value = self.expr(&mut inner, &Ty::Bool, 1, false);
                self.line(value);
                self.indent -= 1;
                self.line("end");
            }
        }
        if let Some(element) = &class.element {
            let mut inner = env.clone();
            inner.function = Some(Some(element.clone()));
            inner.locals.push(Local::typed("index".to_owned(), Ty::Int));
            self.line(format!("def [](index: int) -> {}", self.render(element)));
            self.indent += 1;
            let value = self.expr(&mut inner, element, 1, true);
            self.line(value);
            self.indent -= 1;
            self.line("end");
            self.line(format!(
                "def []=(index: int, value: {})",
                self.render(element)
            ));
            let field = class
                .fields
                .iter()
                .find(|field| Self::assignable(&field.ty, element))
                .map(|field| field.name.clone());
            match field {
                Some(field) => self.line(format!("  @{field} = value")),
                None => self.line("  p(value)"),
            }
            self.line("end");
        }
    }

    /// An expression using a class's operators, or calling a class method,
    /// whose type `ty` accepts.
    fn class_expr(&mut self, env: &mut Env, ty: &Ty, depth: usize) -> Option<String> {
        let d = depth.saturating_sub(1);
        let mut options = Vec::new();
        for (index, class) in self.classes.iter().enumerate() {
            if class.plus && Self::assignable(ty, &Ty::Class(index)) {
                options.push((0, index, 0));
            }
            if class.compare && *ty == Ty::Bool {
                options.push((1, index, 0));
            }
            if class
                .element
                .as_ref()
                .is_some_and(|element| Self::assignable(ty, element))
            {
                options.push((2, index, 0));
            }
            for (position, def) in class.statics.iter().enumerate() {
                let own = env.statics == Some(index) || env.class_index() == Some(index);
                let result = def.result.as_ref();
                if result.is_some_and(|result| Self::assignable(ty, result))
                    && (!own || def.rank < env.rank)
                {
                    options.push((3, index, position));
                }
            }
        }
        if options.is_empty() {
            return None;
        }
        let (kind, index, position) = *self.rng.pick(&options);
        let own = Ty::Class(index);
        Some(match kind {
            0 => format!(
                "({} + {})",
                self.expr(env, &own, d, false),
                self.expr(env, &own, d, false)
            ),
            1 => format!(
                "({} {} {})",
                self.expr(env, &own, d, false),
                ["==", "<", "!="][self.rng.below(3)],
                self.expr(env, &own, d, false)
            ),
            2 => format!(
                "{}[{}]",
                self.expr(env, &own, d, false),
                self.expr(env, &Ty::Int, d, false)
            ),
            _ => {
                let def = self.classes[index].statics[position].clone();
                let name = self.classes[index].name.clone();
                format!("{name}.{}", self.call_with_block(env, &def, depth, None))
            }
        })
    }

    /// A `begin` whose `rescue` retries it: narrowing from before it must
    /// not survive the rescue's assignment when it runs again. In the
    /// unsound form the body relies on it.
    fn retry_flow(&mut self, env: &mut Env, unsound: bool) {
        let count = self.name("n");
        let value = self.name("v");
        let inner = match self.rng.below(3) {
            0 => Ty::Int,
            1 => Ty::Str,
            _ => Ty::array(Ty::Int),
        };
        let literal = self.literal(&inner, 1);
        self.line(format!("{count} = 0"));
        self.line(format!("{value}: {}? = {literal}", self.render(&inner)));
        self.line(format!("if {value} != nil"));
        self.indent += 1;
        self.line("begin");
        self.indent += 1;
        self.line(format!("{count} += 1"));
        if unsound {
            let used = self.name("t");
            self.line(format!("{used}: {} = {value}", self.render(&inner)));
            self.line(format!("p({used})"));
        } else {
            self.line(format!("p({value})"));
        }
        self.line(format!("raise \"again\" if {count} < 2"));
        self.indent -= 1;
        self.line("rescue");
        self.indent += 1;
        self.line(format!("{value} = nil"));
        self.line(format!("retry if {count} < 3"));
        self.indent -= 1;
        self.line("end");
        self.indent -= 1;
        self.line("end");
        env.locals.push(Local::typed(count, Ty::Int));
        env.locals.push(Local::typed(value, Ty::opt(inner)));
    }

    /// An optional local an `ensure` narrows or relies on, inside a
    /// `begin` that rescues what it raises. The sound form guards the local
    /// in the ensure, which narrows it after the `begin` however the body
    /// assigned it; the unsound form uses it narrowed in the ensure, which
    /// may start before the body's guard or after the body assigned it.
    fn ensure_flow(&mut self, env: &mut Env, unsound: bool) {
        let value = self.name("v");
        let inner = if self.rng.chance(50) {
            Ty::Int
        } else {
            Ty::Str
        };
        let literal = self.literal(&inner, 1);
        let initial = if self.rng.chance(50) {
            "nil".to_owned()
        } else {
            literal.clone()
        };
        let condition = self.expr(env, &Ty::Bool, 0, false);
        self.line(format!("{value}: {}? = {initial}", self.render(&inner)));
        self.line("begin");
        self.indent += 1;
        self.line("begin");
        self.indent += 1;
        self.line(format!("raise \"early\" if {condition}"));
        let assigned = self.rng.chance(50);
        if assigned {
            let spoil = if self.rng.chance(50) {
                "nil".to_owned()
            } else {
                literal
            };
            self.line(format!("{value} = {spoil}"));
        }
        if unsound && !assigned {
            self.line(format!("raise \"none\" if {value} == nil"));
        }
        self.indent -= 1;
        self.line("ensure");
        self.indent += 1;
        if unsound {
            let use_it = self.use_narrowed(env, &value, &inner);
            self.line(use_it);
        } else {
            self.line(format!("raise \"none\" if {value} == nil"));
        }
        self.indent -= 1;
        self.line("end");
        let use_it = self.use_narrowed(env, &value, &inner);
        self.line(use_it);
        self.indent -= 1;
        self.line("rescue => failure");
        self.indent += 1;
        self.line("p(failure.message)");
        self.indent -= 1;
        self.line("end");
        env.locals.push(Local::typed(value, Ty::opt(inner)));
    }

    // ----- Host -----

    /// A type a host value can have: one JSON writes, which leaves out
    /// symbols, enums and classes.
    fn host_ty(&mut self, depth: usize) -> Ty {
        loop {
            let ty = self.ty(depth);
            if Self::jsonable(&ty) {
                return ty;
            }
        }
    }

    /// Declares the globals the host supplies, typed or `any`, and the
    /// capabilities it grants.
    fn setup_host(&mut self) {
        for index in 0..self.rng.below(4) {
            let name = format!("hv{index}");
            let ty = self.host_ty(2);
            let value = self.json_of(&ty);
            let (model, annotation) = if self.rng.chance(75) {
                (ty.clone(), self.render(&ty))
            } else {
                (Ty::Any, String::new())
            };
            self.host.globals.push(Global {
                name: name.clone(),
                ty: annotation,
                value,
            });
            self.globals.push((name, model));
        }
        for name in super::host::CAPABILITIES {
            if self.rng.chance(50) {
                self.host.capabilities.push(name.to_owned());
            }
        }
    }

    fn granted(&self, name: &str) -> bool {
        self.host.capabilities.iter().any(|granted| granted == name)
    }

    /// A call of a capability's method, a capability's data or a host
    /// function whose result `ty` accepts.
    fn host_call(&mut self, env: &mut Env, ty: &Ty, depth: usize) -> Option<String> {
        let store = self.granted("store");
        let loose = self.granted("loose");
        let results = [
            (store, Ty::opt(Ty::Int)),
            (store, Ty::Int),
            (store, Ty::array(Ty::Str)),
            (store, Ty::Int),
            (store, Ty::Any),
            (store, Ty::Int),
            (store, Ty::array(Ty::Any)),
            (store, Ty::opt(Ty::Int)),
            (store, Ty::Str),
            (store, Ty::Int),
            (store, Ty::Str),
            (loose, Ty::Any),
            (loose, Ty::Any),
            (loose, Ty::Any),
            (true, Ty::Int),
            (true, Ty::Str),
            (true, Ty::Any),
        ];
        let options: Vec<usize> = results
            .iter()
            .enumerate()
            .filter(|(_, (granted, result))| *granted && Self::assignable(ty, result))
            .map(|(index, _)| index)
            .collect();
        if options.is_empty() {
            return None;
        }
        let d = depth.saturating_sub(1);
        let ints = |this: &mut Self, env: &mut Env| this.expr(env, &Ty::array(Ty::Int), d, true);
        Some(match *self.rng.pick(&options) {
            0 => format!("store.get({})", self.expr(env, &Ty::Str, d, false)),
            1 => format!(
                "store.put({}, {})",
                self.expr(env, &Ty::Str, d, false),
                self.expr(env, &Ty::Int, d, false)
            ),
            2 => "store.keys".to_owned(),
            3 => {
                let values = ints(self, env);
                if self.rng.chance(50) {
                    format!("store.total({values})")
                } else {
                    format!(
                        "store.total({values}, {})",
                        self.expr(env, &Ty::Int, d, false)
                    )
                }
            }
            4 => format!(
                "store.lookup({})",
                ["\"n\"", "\"s\"", "\"l\"", "\"x\""][self.rng.below(4)]
            ),
            5 => {
                let values = ints(self, env);
                let block = self.host_block(env, None, Some(Ty::Int), d);
                format!("store.each({values}) {block}")
            }
            6 => {
                let values = ints(self, env);
                let result = self.ty(1);
                let block = self.host_block(env, Some(result), Some(Ty::array(Ty::Any)), d);
                format!("store.collect({values}) {block}")
            }
            7 => {
                let values = ints(self, env);
                let block = self.host_block(env, Some(Ty::Bool), Some(Ty::opt(Ty::Int)), d);
                format!("store.first({values}) {block}")
            }
            8 => "store.meta.version".to_owned(),
            9 => "store.limit".to_owned(),
            10 => "store.label".to_owned(),
            11 => {
                let ty = self.ty(1);
                format!("loose.echo({})", self.expr(env, &ty, d, true))
            }
            12 => {
                let (a, b) = (self.ty(1), self.ty(1));
                format!(
                    "loose.pair({}, {})",
                    self.expr(env, &a, d, true),
                    self.expr(env, &b, d, true)
                )
            }
            13 => {
                let values = ints(self, env);
                let block = self.host_block(env, Some(Ty::Any), None, d);
                format!("loose.visit({values}) {block}")
            }
            14 => format!("twice({})", self.expr(env, &Ty::Int, d, false)),
            15 => {
                let a = self.expr(env, &Ty::Str, d, false);
                if self.rng.chance(50) {
                    format!("joined({a})")
                } else {
                    format!("joined({a}, {})", self.expr(env, &Ty::Str, d, false))
                }
            }
            _ => {
                let value = self.expr(env, &Ty::Int, d, false);
                let block = self.host_block(env, Some(Ty::Any), None, d);
                format!("around({value}) {block}")
            }
        })
    }

    /// A block passed to a host method, whose parameters are `any`: its
    /// value, of type `result` or discarded, and possibly a `break` with a
    /// value of the method's declared result, `breaks`, or of another type
    /// in the unsound form.
    fn host_block(
        &mut self,
        env: &mut Env,
        result: Option<Ty>,
        breaks: Option<Ty>,
        depth: usize,
    ) -> String {
        let param = self.name("e");
        let mut inner = env.clone();
        inner.locals.push(Local::inferred(param.clone(), Ty::Any));
        let value = match &result {
            Some(Ty::Bool) => format!("{param} == {}", self.expr(&mut inner, &Ty::Int, 0, false)),
            Some(Ty::Any) if self.rng.chance(50) => param.clone(),
            Some(ty) => self.expr(&mut inner, ty, depth, true),
            None => format!("p({param})"),
        };
        if self.rng.chance(20) {
            let ty = match &breaks {
                Some(ty) if !self.unsound() => ty.clone(),
                _ => self.ty(1),
            };
            let value_ty = self.expr(&mut inner, &ty, 0, true);
            return format!("{{ |{param}|\n  break {value_ty} if {param} == 1\n  {value}\n}}");
        }
        format!("{{ |{param}| {value} }}")
    }

    /// The script function the host calls with arguments, and the calls:
    /// arguments of the parameters' types, or in a few calls one of
    /// another type, which the call's entry check rejects.
    fn entry(&mut self) -> String {
        let mut params = Vec::new();
        let mut keywords = false;
        for index in 0..1 + self.rng.below(3) {
            let ty = self.host_ty(2);
            let name = format!("a{index}");
            let kind = if keywords || self.rng.chance(20) {
                keywords = true;
                ParamKind::Keyword(self.rng.chance(50).then(|| self.literal(&ty, 1)))
            } else if self.rng.chance(20) {
                ParamKind::Default(self.literal(&ty, 1))
            } else {
                ParamKind::Required
            };
            // A required parameter cannot follow one with a default.
            let kind = match (&kind, params.last()) {
                (
                    ParamKind::Required,
                    Some(Param {
                        kind: ParamKind::Default(_),
                        ..
                    }),
                ) => ParamKind::Default(self.literal(&ty, 1)),
                _ => kind,
            };
            params.push(Param { name, ty, kind });
        }
        let def = FnDef {
            name: "entry".to_owned(),
            params,
            result: self.rng.chance(80).then(|| self.host_ty(1)),
            block: None,
            rank: 40,
        };
        self.function(&def, None, false);
        for _ in 0..1 + self.rng.below(2) {
            let mut args = Vec::new();
            let mut keywords = Vec::new();
            let wrong = self.rng.chance(15);
            // Positional arguments fill parameters in order, so one default
            // left out leaves out the rest.
            let mut omitted = false;
            for (index, param) in def.params.iter().enumerate() {
                let value = if wrong && index == 0 {
                    "[\"wrong\"]".to_owned()
                } else {
                    self.json_of(&param.ty)
                };
                match &param.kind {
                    ParamKind::Required => args.push(value),
                    ParamKind::Default(_) => {
                        if !omitted && self.rng.chance(60) {
                            args.push(value);
                        } else {
                            omitted = true;
                        }
                    }
                    ParamKind::Keyword(default) => {
                        if default.is_none() || self.rng.chance(50) {
                            keywords.push(format!("\"{}\":{value}", param.name));
                        }
                    }
                    ParamKind::Rest => {}
                }
            }
            self.host.calls.push(super::host::Call {
                function: def.name.clone(),
                args: format!(
                    "{{\"args\":[{}],\"keywords\":{{{}}}}}",
                    args.join(","),
                    keywords.join(",")
                ),
            });
        }
        self.entry = Some(def);
        self.take_lines()
    }
}

fn enum_source(index: usize) -> String {
    let mut text = format!("enum E{index}\n");
    for member in MEMBERS[index] {
        text.push_str(&format!("  {member}\n"));
    }
    text.push_str("end\n");
    text
}

fn has_tuple(ty: &Ty) -> bool {
    match ty {
        Ty::Tuple(_) => true,
        Ty::Opt(inner) | Ty::Array(inner) | Ty::Hash(inner) => has_tuple(inner),
        Ty::Union(options) => options.iter().any(has_tuple),
        Ty::Shape(fields) => fields.iter().any(|field| has_tuple(&field.ty)),
        _ => false,
    }
}

/// The `is_type?` atom of a type.
fn atom(ty: &Ty) -> &'static str {
    match ty {
        Ty::Int => "int",
        Ty::Float => "float",
        Ty::Str => "string",
        Ty::Sym => "symbol",
        Ty::Bool => "bool",
        Ty::Nil => "nil",
        Ty::Array(_) | Ty::Tuple(_) => "array",
        Ty::Hash(_) | Ty::Shape(_) => "hash",
        _ => "int",
    }
}
