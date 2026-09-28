//! Calls of every builtin signature in `src/signatures/builtins.vibe` with
//! arguments of its parameter types, each result bound to a local declared
//! with the signature's result type. The checker accepts the binding from
//! the signature, and the build that keeps every check verifies the value
//! the runtime returns against it, so a wrong signature is a finding.

use super::{harness::Case, rng::Rng};
use std::sync::OnceLock;

/// A type as the signature table writes it.
#[derive(Clone, Debug, PartialEq)]
pub enum T {
    Named(String),
    Var(String),
    Opt(Box<T>),
    Union(Vec<T>),
    Array(Box<T>),
    Hash(Box<T>),
    Shape(Vec<(String, T, bool)>, bool),
    Tuple(Vec<T>),
    Sym(String),
    Type(Box<T>),
}

#[derive(Clone, Debug, PartialEq)]
enum Kind {
    Required,
    /// `name: T = value` or `name?: T`.
    Optional,
    Rest,
    Keyword {
        required: bool,
    },
    KeywordRest,
}

#[derive(Clone, Debug)]
struct Param {
    name: String,
    ty: T,
    kind: Kind,
}

#[derive(Clone, Debug)]
struct Block {
    params: Vec<T>,
    result: Option<T>,
    optional: bool,
}

#[derive(Clone, Debug)]
enum Owner {
    Function,
    Module(String),
    /// A receiver pattern and the bounds of its variables.
    Class(T, Vec<(String, Option<T>)>),
}

/// One signature of the table.
#[derive(Clone, Debug)]
pub struct Sig {
    owner: Owner,
    name: String,
    generics: Vec<(String, Option<T>)>,
    params: Vec<Param>,
    block: Option<Block>,
    result: Option<T>,
}

impl Sig {
    /// How the signature reads in a finding.
    pub fn describe(&self) -> String {
        let owner = match &self.owner {
            Owner::Function => String::new(),
            Owner::Module(name) => format!("{name}."),
            Owner::Class(pattern, _) => format!("{}#", render(pattern)),
        };
        format!("{owner}{}", self.name)
    }
}

const SOURCE: &str = include_str!("../../src/signatures/builtins.vibe");

/// The table's signatures, parsed once.
pub fn table() -> &'static [Sig] {
    static TABLE: OnceLock<Vec<Sig>> = OnceLock::new();
    TABLE.get_or_init(|| parse_table(SOURCE))
}

fn parse_table(source: &str) -> Vec<Sig> {
    let mut sigs = Vec::new();
    let mut owner = Owner::Function;
    for line in source.lines() {
        let line = line.split(" #").next().unwrap_or(line);
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("module ") {
            owner = Owner::Module(rest.trim().to_owned());
        } else if let Some(rest) = trimmed.strip_prefix("class ") {
            let mut parser = Parser::new(rest.trim());
            let (pattern, bounds) = parser.class_header();
            owner = Owner::Class(pattern, bounds);
        } else if trimmed == "end" {
            owner = Owner::Function;
        } else if let Some(rest) = trimmed.strip_prefix("def ") {
            if let Some(sig) = Parser::new(rest).def(owner.clone()) {
                sigs.push(sig);
            }
        }
    }
    sigs
}

struct Parser<'a> {
    text: &'a str,
    at: usize,
}

impl<'a> Parser<'a> {
    fn new(text: &'a str) -> Self {
        Self { text, at: 0 }
    }

    fn rest(&self) -> &'a str {
        &self.text[self.at..]
    }

    fn skip(&mut self) {
        while self.rest().starts_with(' ') {
            self.at += 1;
        }
    }

    fn eat(&mut self, token: &str) -> bool {
        self.skip();
        if self.rest().starts_with(token) {
            self.at += token.len();
            true
        } else {
            false
        }
    }

    fn word(&mut self) -> String {
        self.skip();
        let length = self
            .rest()
            .find(|c: char| !(c.is_alphanumeric() || c == '_' || c == '?' || c == '!'))
            .unwrap_or(self.rest().len());
        let word = self.rest()[..length].to_owned();
        self.at += length;
        word
    }

    /// `array<T: comparable>`, `hash<string, V?>`, `T`, `int`.
    fn class_header(&mut self) -> (T, Vec<(String, Option<T>)>) {
        let mut bounds = Vec::new();
        let pattern = self.bounded(&mut bounds);
        (pattern, bounds)
    }

    /// A type in a class header, whose variables may carry bounds.
    fn bounded(&mut self, bounds: &mut Vec<(String, Option<T>)>) -> T {
        self.skip();
        if self.eat("[") {
            let mut items = vec![self.bounded(bounds)];
            while self.eat(",") {
                items.push(self.bounded(bounds));
            }
            self.eat("]");
            return T::Tuple(items);
        }
        let word = self.word();
        if word == "array" && self.eat("<") {
            let inner = self.bounded(bounds);
            self.eat(">");
            return T::Array(Box::new(inner));
        }
        if word == "hash" && self.eat("<") {
            self.word();
            self.eat(",");
            let inner = self.bounded(bounds);
            self.eat(">");
            return T::Hash(Box::new(inner));
        }
        let optional = word.ends_with('?');
        let name = word.trim_end_matches('?').to_owned();
        let ty = if is_var(&name) {
            let bound = self.eat(":").then(|| self.union());
            bounds.push((name.clone(), bound));
            T::Var(name)
        } else {
            T::Named(name)
        };
        if optional { T::Opt(Box::new(ty)) } else { ty }
    }

    fn def(&mut self, owner: Owner) -> Option<Sig> {
        let name = self.word();
        let mut generics = Vec::new();
        if self.eat("<") {
            loop {
                let var = self.word();
                let bound = self.eat(":").then(|| self.union());
                generics.push((var, bound));
                if !self.eat(",") {
                    break;
                }
            }
            self.eat(">");
        }
        let mut params = Vec::new();
        let mut block = None;
        if self.eat("(") {
            let mut keywords = false;
            loop {
                self.skip();
                if self.eat(")") {
                    break;
                }
                if self.eat("**") {
                    let name = self.word();
                    self.eat(":");
                    let ty = self.union();
                    params.push(Param {
                        name,
                        ty,
                        kind: Kind::KeywordRest,
                    });
                } else if self.eat("*,") {
                    keywords = true;
                    continue;
                } else if self.eat("*") {
                    let name = self.word();
                    self.eat(":");
                    let ty = self.union();
                    params.push(Param {
                        name,
                        ty,
                        kind: Kind::Rest,
                    });
                    keywords = true;
                } else if self.eat("&") {
                    let word = self.word();
                    self.eat(":");
                    block = Some(self.block(word.ends_with('?')));
                } else {
                    let word = self.word();
                    let maybe = word.ends_with('?');
                    self.eat(":");
                    let ty = self.union();
                    let default = self.eat("=");
                    if default {
                        self.value();
                    }
                    let kind = if keywords {
                        Kind::Keyword {
                            required: !default && !maybe,
                        }
                    } else if default || maybe {
                        Kind::Optional
                    } else {
                        Kind::Required
                    };
                    params.push(Param {
                        name: word.trim_end_matches('?').to_owned(),
                        ty,
                        kind,
                    });
                }
                self.eat(",");
            }
        }
        let result = self.eat("->").then(|| self.union());
        Some(Sig {
            owner,
            name,
            generics,
            params,
            block,
            result,
        })
    }

    /// Skips a default value.
    fn value(&mut self) {
        self.skip();
        let mut depth = 0i32;
        while let Some(c) = self.rest().chars().next() {
            match c {
                '(' | '[' | '{' => depth += 1,
                ')' | ']' | '}' if depth == 0 => return,
                ')' | ']' | '}' => depth -= 1,
                ',' if depth == 0 => return,
                _ => {}
            }
            self.at += c.len_utf8();
        }
    }

    fn block(&mut self, optional: bool) -> Block {
        self.skip();
        let params = if self.rest().starts_with('(') {
            self.eat("(");
            let mut params = Vec::new();
            if !self.eat(")") {
                loop {
                    params.push(self.union());
                    if !self.eat(",") {
                        break;
                    }
                }
                self.eat(")");
            }
            params
        } else {
            vec![self.union()]
        };
        let result = self.eat("->").then(|| self.union());
        Block {
            params,
            result,
            optional,
        }
    }

    fn union(&mut self) -> T {
        let mut options = vec![self.postfix()];
        while self.eat("|") {
            options.push(self.postfix());
        }
        if options.len() == 1 {
            options.pop().unwrap()
        } else {
            T::Union(options)
        }
    }

    fn postfix(&mut self) -> T {
        let mut ty = self.primary();
        while self.rest().starts_with('?') {
            self.at += 1;
            ty = T::Opt(Box::new(ty));
        }
        ty
    }

    fn primary(&mut self) -> T {
        self.skip();
        if self.eat("(") {
            let ty = self.union();
            self.eat(")");
            return ty;
        }
        if self.eat("[") {
            let mut items = vec![self.union()];
            while self.eat(",") {
                items.push(self.union());
            }
            self.eat("]");
            return T::Tuple(items);
        }
        if self.eat("{") {
            let mut fields = Vec::new();
            loop {
                self.skip();
                if self.eat("}") {
                    break;
                }
                let word = self.word();
                self.eat(":");
                let ty = self.union();
                fields.push((
                    word.trim_end_matches('?').to_owned(),
                    ty,
                    word.ends_with('?'),
                ));
                self.eat(",");
            }
            return T::Shape(fields, false);
        }
        if self.eat(":") {
            return T::Sym(self.word());
        }
        let word = self.word();
        match word.as_str() {
            "array" if self.eat("<") => {
                let inner = self.union();
                self.eat(">");
                T::Array(Box::new(inner))
            }
            "hash" if self.eat("<") => {
                self.word();
                self.eat(",");
                let inner = self.union();
                self.eat(">");
                T::Hash(Box::new(inner))
            }
            "type" if self.eat("<") => {
                let inner = self.union();
                self.eat(">");
                T::Type(Box::new(inner))
            }
            _ if is_var(&word) => T::Var(word),
            // A trailing `?` belongs to the type, not the word.
            _ => match word.strip_suffix('?') {
                Some(base) if is_var(base) => T::Opt(Box::new(T::Var(base.to_owned()))),
                Some(base) => T::Opt(Box::new(T::Named(base.to_owned()))),
                None => T::Named(word),
            },
        }
    }
}

fn is_var(name: &str) -> bool {
    matches!(name, "T" | "U" | "V" | "K" | "A")
}

/// Renders a concrete type as an annotation.
pub fn render(ty: &T) -> String {
    match ty {
        T::Named(name) | T::Var(name) => name.clone(),
        T::Opt(inner) => match &**inner {
            T::Union(_) => format!("{} | nil", render(inner)),
            other => format!("{}?", render(other)),
        },
        T::Union(options) => options.iter().map(render).collect::<Vec<_>>().join(" | "),
        T::Array(inner) => format!("array<{}>", render(inner)),
        T::Hash(inner) => format!("hash<string, {}>", render(inner)),
        T::Shape(fields, open) => {
            let mut parts: Vec<String> = fields
                .iter()
                .map(|(name, ty, optional)| {
                    format!("{name}{}: {}", if *optional { "?" } else { "" }, render(ty))
                })
                .collect();
            if *open {
                parts.push("...".to_owned());
            }
            format!("{{ {} }}", parts.join(", "))
        }
        T::Tuple(items) => format!(
            "[{}]",
            items.iter().map(render).collect::<Vec<_>>().join(", ")
        ),
        T::Sym(name) => format!(":{name}"),
        T::Type(inner) => format!("type<{}>", render(inner)),
    }
}

/// Replaces type variables by their bindings.
fn subst(ty: &T, bindings: &[(String, T)]) -> T {
    match ty {
        T::Var(name) => bindings
            .iter()
            .find(|(var, _)| var == name)
            .map_or_else(|| T::Named("any".into()), |(_, ty)| ty.clone()),
        T::Opt(inner) => opt(subst(inner, bindings)),
        T::Union(options) => union(options.iter().map(|ty| subst(ty, bindings)).collect()),
        T::Array(inner) => T::Array(Box::new(subst(inner, bindings))),
        T::Hash(inner) => T::Hash(Box::new(subst(inner, bindings))),
        T::Shape(fields, open) => T::Shape(
            fields
                .iter()
                .map(|(name, ty, optional)| (name.clone(), subst(ty, bindings), *optional))
                .collect(),
            *open,
        ),
        T::Tuple(items) => T::Tuple(items.iter().map(|ty| subst(ty, bindings)).collect()),
        T::Type(inner) => T::Type(Box::new(subst(inner, bindings))),
        other => other.clone(),
    }
}

fn opt(ty: T) -> T {
    match ty {
        T::Opt(_) => ty,
        T::Named(ref name) if name == "nil" || name == "any" => ty,
        other => T::Opt(Box::new(other)),
    }
}

/// A union without repeats, flattened; `nil` makes it optional.
fn union(options: Vec<T>) -> T {
    let mut flat: Vec<T> = Vec::new();
    let mut nil = false;
    for option in options {
        let parts = match option {
            T::Union(parts) => parts,
            T::Opt(inner) => {
                nil = true;
                vec![*inner]
            }
            other => vec![other],
        };
        for part in parts {
            if part == T::Named("nil".into()) {
                nil = true;
            } else if !flat.contains(&part) {
                flat.push(part);
            }
        }
    }
    let ty = match flat.len() {
        0 => return T::Named("nil".into()),
        1 => flat.pop().unwrap(),
        _ => T::Union(flat),
    };
    if nil { opt(ty) } else { ty }
}

/// Generates one program of builtin calls from `seed`, or in a fifth of
/// seeds, of operators applied to values of random types.
pub fn program(seed: u64) -> Case {
    let mut rng = Rng::new(seed ^ 0xb111_7115);
    if rng.chance(20) {
        return operators(&mut rng);
    }
    let mut out = Out {
        rng: &mut rng,
        lines: Vec::new(),
        fresh: 0,
        small: false,
    };
    out.lines.push("enum E\n  A\n  B\nend".to_owned());
    out.lines.push(
        "class K\n  getter v: int\n  def initialize(v: int)\n    @v = v\n  end\nend".to_owned(),
    );
    let table = table();
    for _ in 0..1 + out.rng.below(4) {
        let sig = &table[out.rng.below(table.len())];
        out.call(sig);
    }
    let mut main = out.lines.join("\n");
    main.push('\n');
    Case::new(main)
}

struct Out<'r> {
    rng: &'r mut Rng,
    lines: Vec<String>,
    fresh: usize,
    /// Whether integers stay within 64 bits, as counts such as a string
    /// repetition's must.
    small: bool,
}

/// Types the generator can bind a type variable to.
const SIMPLE: [&str; 5] = ["int", "string", "float", "symbol", "bool"];

impl Out<'_> {
    fn name(&mut self, prefix: &str) -> String {
        self.fresh += 1;
        format!("{prefix}{}", self.fresh)
    }

    /// A concrete type satisfying `bound`.
    fn pick(&mut self, bound: Option<&T>) -> T {
        let named = |name: &str| T::Named(name.to_owned());
        match bound {
            Some(T::Named(name)) => match name.as_str() {
                "comparable" => named(
                    ["int", "float", "string", "symbol", "duration", "money"][self.rng.below(6)],
                ),
                "number" => named(["int", "float"][self.rng.below(2)]),
                other => named(other),
            },
            Some(T::Union(options)) => {
                let option = options[self.rng.below(options.len())].clone();
                self.pick(Some(&option))
            }
            Some(other) => other.clone(),
            None => match self.rng.below(10) {
                0..=4 => named(SIMPLE[self.rng.below(SIMPLE.len())]),
                5 => T::Array(Box::new(named("int"))),
                6 => opt(named(["int", "string"][self.rng.below(2)])),
                7 => T::Tuple(vec![named("string"), named("int")]),
                8 => named("E"),
                _ => T::Hash(Box::new(named("int"))),
            },
        }
    }

    /// Emits a call of `sig` with a receiver and arguments of its types,
    /// binding the result to a local of the result type.
    fn call(&mut self, sig: &Sig) {
        if skipped(sig) {
            return;
        }
        let mut bindings: Vec<(String, T)> = Vec::new();
        let receiver = match &sig.owner {
            Owner::Function => None,
            Owner::Module(name) => Some(name.clone()),
            Owner::Class(pattern, bounds) => {
                for (var, bound) in bounds {
                    let ty = self.pick(bound.as_ref());
                    bindings.push((var.clone(), ty));
                }
                let receiver = match pattern {
                    // Every type's members bind `T` to the receiver's.
                    T::Var(var) => {
                        let receiver = self.any_receiver();
                        bindings.retain(|(name, _)| name != var);
                        bindings.push((var.clone(), receiver.clone()));
                        receiver
                    }
                    _ => subst(pattern, &bindings),
                };
                Some(self.bound_value(&receiver))
            }
        };
        for (var, bound) in &sig.generics {
            let ty = self.pick(bound.as_ref());
            bindings.push((var.clone(), ty));
        }
        let mut args = Vec::new();
        for param in &sig.params {
            let ty = subst(&param.ty, &bindings);
            match &param.kind {
                Kind::Required => args.push(self.value(&ty, 1)),
                Kind::Optional => {
                    if self.rng.chance(50) {
                        args.push(self.value(&ty, 1));
                    } else {
                        // Positional arguments bind in order.
                        break;
                    }
                }
                Kind::Rest => {
                    let T::Array(element) = &ty else { continue };
                    for _ in 0..self.rng.below(3) {
                        args.push(self.value(element, 1));
                    }
                }
                Kind::Keyword { required } => {
                    if *required || self.rng.chance(40) {
                        let value = self.value(&ty, 1);
                        args.push(format!("{}: {value}", param.name));
                    }
                }
                Kind::KeywordRest => {}
            }
        }
        // Keywords follow positionals; a skipped optional stops the rest.
        let keyword_index = sig
            .params
            .iter()
            .position(|param| matches!(param.kind, Kind::Keyword { .. }));
        let _ = keyword_index;
        let mut call = match &receiver {
            Some(receiver) => format!("{receiver}.{}", sig.name),
            None => sig.name.clone(),
        };
        if !args.is_empty() {
            call.push_str(&format!("({})", args.join(", ")));
        }
        if let Some(block) = &sig.block {
            if !block.optional || self.rng.chance(60) {
                let params: Vec<T> = block.params.iter().map(|ty| subst(ty, &bindings)).collect();
                let names: Vec<String> =
                    (0..params.len()).map(|index| format!("b{index}")).collect();
                let body = match &block.result {
                    Some(result) => {
                        let result = subst(result, &bindings);
                        let reuse = params.iter().position(|param| *param == result);
                        match reuse {
                            Some(index) if self.rng.chance(50) => names[index].clone(),
                            _ => self.value(&result, 1),
                        }
                    }
                    None => "nil".to_owned(),
                };
                let params = if names.is_empty() {
                    String::new()
                } else {
                    format!("|{}| ", names.join(", "))
                };
                call.push_str(&format!(" {{ {params}{body} }}"));
            }
        }
        let result = match &sig.result {
            Some(result) => subst(result, &bindings),
            None => T::Named("nil".into()),
        };
        let local = self.name("r");
        self.lines.push(format!("# {}", sig.describe()));
        let line = format!("{local}: {} = {call}", render(&result));
        self.lines.push(line);
        self.lines.push(format!("p({local})"));
    }

    /// A receiver type for a member every type has.
    fn any_receiver(&mut self) -> T {
        let named = |name: &str| T::Named(name.to_owned());
        match self.rng.below(12) {
            0 => named("int"),
            1 => named("string"),
            2 => T::Array(Box::new(named("int"))),
            3 => T::Hash(Box::new(named("string"))),
            4 => named("range"),
            5 => named("duration"),
            6 => named("money"),
            7 => named("time"),
            8 => named("E"),
            9 => named("K"),
            10 => named("symbol"),
            _ => T::Shape(vec![("id".into(), named("int"), false)], false),
        }
    }

    /// A receiver expression of type `ty`, bound to a local first when it
    /// needs a declared type.
    fn bound_value(&mut self, ty: &T) -> String {
        let value = self.value(ty, 1);
        match ty {
            T::Named(name) if name == "match_data" => value,
            T::Named(name) if name == "error" => value,
            _ => {
                let local = self.name("v");
                self.lines
                    .push(format!("{local}: {} = {value}", render(ty)));
                local
            }
        }
    }

    /// A value of concrete type `ty`.
    fn value(&mut self, ty: &T, depth: usize) -> String {
        match ty {
            T::Named(name) => self.named(name),
            T::Var(_) => self.named("int"),
            T::Opt(inner) => {
                if self.rng.chance(30) {
                    "nil".to_owned()
                } else {
                    self.value(inner, depth)
                }
            }
            T::Union(options) => {
                let option = options[self.rng.below(options.len())].clone();
                self.value(&option, depth)
            }
            T::Array(inner) => {
                if depth > 2 || self.rng.chance(15) {
                    return "[]".to_owned();
                }
                let items: Vec<String> = (0..1 + self.rng.below(4))
                    .map(|_| self.value(inner, depth + 1))
                    .collect();
                format!("[{}]", items.join(", "))
            }
            T::Hash(inner) => {
                if depth > 2 || self.rng.chance(15) {
                    return "{}".to_owned();
                }
                let items: Vec<String> = (0..1 + self.rng.below(3))
                    .map(|index| {
                        format!(
                            "{}: {}",
                            ["a", "b", "c"][index],
                            self.value(inner, depth + 1)
                        )
                    })
                    .collect();
                format!("{{ {} }}", items.join(", "))
            }
            T::Shape(fields, _) => {
                let mut parts = Vec::new();
                for (name, ty, optional) in fields {
                    if !*optional || self.rng.chance(50) {
                        parts.push(format!("{name}: {}", self.value(ty, depth + 1)));
                    }
                }
                format!("{{ {} }}", parts.join(", "))
            }
            T::Tuple(items) => {
                let items: Vec<String> = items.iter().map(|ty| self.value(ty, depth + 1)).collect();
                format!("[{}]", items.join(", "))
            }
            T::Sym(name) => format!(":{name}"),
            T::Type(inner) => render(inner),
        }
    }

    fn named(&mut self, name: &str) -> String {
        let pick = |rng: &mut Rng, items: &[&str]| items[rng.below(items.len())].to_owned();
        match name {
            "int" if self.small => pick(
                self.rng,
                &["0", "1", "2", "3", "-1", "7", "-4", "10", "100"],
            ),
            "int" => pick(
                self.rng,
                &[
                    "0",
                    "1",
                    "2",
                    "3",
                    "-1",
                    "7",
                    "-4",
                    "10",
                    "100",
                    "9223372036854775808",
                ],
            ),
            "float" => pick(
                self.rng,
                &["0.5", "-1.5", "2.0", "0.0", "3.75", "-0.25", "1.0e20"],
            ),
            "number" => {
                let kind = pick(self.rng, &["int", "float"]);
                self.named(&kind)
            }
            "string" => pick(
                self.rng,
                &[
                    "\"\"",
                    "\"a\"",
                    "\"abc\"",
                    "\"Hello World\"",
                    "\" pad \"",
                    "\"a,b,c\"",
                    "\"ünï\"",
                    "\"42\"",
                    "\"3.5\"",
                    "\"x\\ny\"",
                    "\"aAbB\"",
                ],
            ),
            "symbol" => pick(self.rng, &[":a", ":ascii", ":fold", ":id"]),
            "bool" => pick(self.rng, &["true", "false"]),
            "nil" => "nil".to_owned(),
            "any" => {
                let kind = pick(self.rng, &["int", "string", "float", "bool", "nil"]);
                self.named(&kind)
            }
            "comparable" => {
                let kind = pick(self.rng, &["int", "string", "float", "symbol"]);
                self.named(&kind)
            }
            "range" => pick(
                self.rng,
                &["(1..3)", "(0...4)", "(-2..2)", "(3..1)", "(0..0)"],
            ),
            "money" => pick(
                self.rng,
                &[
                    "money(\"12.50 USD\")",
                    "money_cents(-5, \"USD\")",
                    "money(\"0.00 USD\")",
                ],
            ),
            "duration" => pick(
                self.rng,
                &[
                    "90.seconds",
                    "2.hours",
                    "Duration.parse(\"1h30m\")",
                    "0.seconds",
                    "3.days",
                ],
            ),
            "time" => pick(
                self.rng,
                &[
                    "Time.at(0)",
                    "Time.utc(2024, 2, 29, 12, 30, 5)",
                    "Time.at(1700000000, in: \"America/Detroit\")",
                ],
            ),
            "regex" => pick(
                self.rng,
                &["/a+/", "/(\\d+)-(\\w+)/", "Regex.new(\"b\")", "/x/i"],
            ),
            "match_data" => {
                let text = pick(self.rng, &["\"id-42-x\"", "\"42-ab\""]);
                format!("{text}.match(/(\\d+)-(\\w+)/).as(match_data)")
            }
            "error" => "(begin\n  raise \"boom\"\nrescue => e\n  e\nend)".to_owned(),
            "enum_value" | "E" => pick(self.rng, &["E::A", "E::B"]),
            "enum_type" => "E".to_owned(),
            "K" => format!("K.new({})", self.named("int")),
            _ => self.named("int"),
        }
    }
}

/// Signatures whose calls cannot be compared between two runs, as the
/// clock and time-ordered identifiers differ, or cannot be generated from
/// their types alone.
fn skipped(sig: &Sig) -> bool {
    let module = match &sig.owner {
        Owner::Module(name) => name.as_str(),
        _ => "",
    };
    matches!(
        (module, sig.name.as_str()),
        ("Time", "now")
            | ("", "loop")
            | ("", "require")
            | ("", "p")
            | ("JSON", "parse_as")
            | ("", "format")
    ) || matches!(
        sig.name.as_str(),
        "from_now" | "ago" | "cycle" | "as" | "uuid"
    )
}

/// Operand types for operators.
const OPERANDS: [&str; 13] = [
    "int",
    "float",
    "number",
    "string",
    "symbol",
    "bool",
    "time",
    "duration",
    "money",
    "range",
    "array<int>",
    "array<string>",
    "int?",
];

const OPERATORS: [&str; 17] = [
    "+", "-", "*", "/", "//", "%", "**", "<", "<=", ">", ">=", "<=>", "==", "!=", "<<", "=~", "!~",
];

/// A program of binary operators on values of random types, each result a
/// top-level local whose inferred type the annotation pass declares.
fn operators(rng: &mut Rng) -> Case {
    let mut out = Out {
        rng,
        lines: Vec::new(),
        fresh: 0,
        small: true,
    };
    for index in 0..1 + out.rng.below(3) {
        let op = OPERATORS[out.rng.below(OPERATORS.len())];
        let left = OPERANDS[out.rng.below(OPERANDS.len())];
        let right = if out.rng.chance(50) {
            left
        } else {
            OPERANDS[out.rng.below(OPERANDS.len())]
        };
        let a = out.name("a");
        let b = out.name("b");
        let value = out.value(&parse_type(left), 1);
        out.lines.push(format!("{a}: {left} = {value}"));
        let counts = right.contains("int") || right.contains("number") || right.contains("any");
        let value = if op == "=~" || op == "!~" {
            "/a/".to_owned()
        } else if op == "*" && left == "string" && counts {
            // A string repeated more times than an int holds raises an
            // operand error, a known difference, not a checker bug.
            ["0", "1", "2", "3"][out.rng.below(4)].to_owned()
        } else if op == "*" && left == "string" {
            ["0.0", "1.5", "2.5"][out.rng.below(3)].to_owned()
        } else {
            out.value(&parse_type(right), 1)
        };
        let right = if op == "=~" || op == "!~" {
            "regex"
        } else {
            right
        };
        out.lines.push(format!("{b}: {right} = {value}"));
        out.lines.push(format!("o{index} = {a} {op} {b}"));
        out.lines.push(format!("p(o{index})"));
    }
    let mut main = out.lines.join("\n");
    main.push('\n');
    Case::new(main)
}

fn parse_type(text: &str) -> T {
    Parser::new(text).union()
}
