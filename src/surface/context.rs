//! What the rules know about a source while they walk it: its tokens, its
//! declarations, the locals in scope, and the edits made so far.

use super::{Rewrite, Rule, edits::Edits, parse, syntax::*};
use crate::tooling::TokenKind;
use std::collections::{HashMap, HashSet};

/// Where an expression stands, which decides whether a replacement with a
/// low-precedence operator needs parentheses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Place {
    /// A whole statement, whose value may be discarded.
    Statement,
    /// Anywhere a whole expression may stand: an argument, a value being
    /// assigned, returned or tested.
    Loose,
    /// An operand, a receiver or an indexed value.
    Tight,
}

/// The type of a literal, such as `int` for `3` or `-3`, or none for any
/// other expression, `nil` included.
pub fn literal_type(surface: &Surface<'_>, expr: &Expr) -> Option<&'static str> {
    Some(match &expr.kind {
        ExprKind::Integer => "int",
        ExprKind::Float => "float",
        ExprKind::Str | ExprKind::Template(_) => "string",
        ExprKind::True | ExprKind::False => "bool",
        ExprKind::Symbol => "symbol",
        ExprKind::Unary(op, operand) if matches!(surface.token_text(*op), "-" | "+") => {
            return literal_type(surface, operand).filter(|ty| matches!(*ty, "int" | "float"));
        }
        ExprKind::Group(_, inner, _) => return literal_type(surface, inner),
        _ => return None,
    })
}

/// Whether `h.name` on a hash calls a method rather than reading a field:
/// a member the signature table or the rename table has on hashes or on
/// every value, except `string`, which a hash always read as a field.
pub fn hash_method(name: &str) -> bool {
    static TABLE: std::sync::OnceLock<HashSet<&'static str>> = std::sync::OnceLock::new();
    let table = TABLE.get_or_init(|| {
        crate::signatures::table()
            .items
            .iter()
            .filter_map(|item| match item {
                crate::signatures::Item::Class(class) if matches!(class.base(), "hash" | "T") => {
                    Some(class.members.iter().map(|member| member.name()))
                }
                _ => None,
            })
            .flatten()
            .collect()
    });
    matches!(name, "to_s" | "as")
        || table.contains(name)
        || name != "string"
            && super::patterns::patterns().iter().any(|pattern| {
                pattern.callee == super::patterns::Callee::Member
                    && matches!(pattern.receiver.as_str(), "hash" | "T")
                    && pattern.name == name
            })
}

/// Declarations in the source, gathered before the walk.
#[derive(Default)]
pub struct Declared<'a> {
    /// Top-level functions by name.
    pub functions: HashMap<String, &'a Def>,
    /// Classes by their dotted name, and the methods each defines.
    pub classes: HashMap<String, &'a Class>,
    /// Enums by name, with their members.
    pub enums: HashMap<String, Vec<String>>,
    /// Every method name a class or top-level function defines.
    pub methods: HashSet<String>,
    /// Methods a direct call from outside their class cannot reach.
    pub private_methods: HashSet<String>,
}

/// What a function body binds, for the names that could be locals.
#[derive(Default)]
pub struct Scope<'a> {
    /// The function the scope belongs to, if any.
    pub def: Option<&'a Def>,
    /// The class the scope is in, if any.
    pub class: Option<&'a Class>,
    /// Locals, parameters and rescue bindings.
    pub locals: HashSet<String>,
    /// The names `rescue => name` binds.
    pub rescues: HashSet<String>,
    /// Whether the scope is a block's, whose collection stops at the blocks
    /// nested in it: they collect their own, and the enclosing function's
    /// scope already holds what they assign.
    pub block: bool,
}

/// The state the canonical-surface rules share while they walk one source.
pub struct Surface<'a> {
    /// The source the tree was parsed from.
    pub source: &'a str,
    /// Every token of the tree, interpolations' after the source's own.
    pub tokens: &'a [Token],
    /// The edits made so far.
    pub edits: Edits,
    /// The source's classes, functions and enums.
    pub declared: Declared<'a>,
    /// The scopes enclosing the walk, innermost last.
    pub scopes: Vec<Scope<'a>>,
    /// Expressions a rename rewrote as an operator, such as `x.nil?` as `x == nil`.
    pub operator_rewrites: HashSet<Span>,
    /// The offsets of every `&&` and `||`, where they test their left operand.
    pub short_circuits: HashSet<usize>,
    /// The source range of every function.
    pub def_ranges: Vec<std::ops::Range<usize>>,
    /// Each removed spelling the walk rewrote, whose edits form a group.
    pub rewrites: Vec<Rewrite>,
    /// The rewrite of each percent literal, by its span.
    pub(super) words: HashMap<Span, usize>,
    /// The rewrite of the `unless` or `until` whose condition the walk is
    /// negating.
    pub(super) negation: Option<usize>,
}

impl<'a> Surface<'a> {
    /// Prepares to walk `tree`, parsed from `source`.
    pub fn new(source: &'a str, tree: &'a Tree) -> Self {
        let mut surface = Self {
            source,
            tokens: &tree.tokens,
            edits: Edits::default(),
            declared: Declared::default(),
            scopes: Vec::new(),
            operator_rewrites: HashSet::new(),
            short_circuits: HashSet::new(),
            def_ranges: Vec::new(),
            rewrites: Vec::new(),
            words: HashMap::new(),
            negation: None,
        };
        for (index, token) in tree.tokens.iter().enumerate() {
            if matches!(token.kind, TokenKind::Operator("&&" | "||")) {
                let offset = surface.operator_offset(index);
                surface.short_circuits.insert(offset);
            }
        }
        surface.declare(&tree.body, "");
        surface
    }

    /// The source text of `span`.
    pub fn text(&self, span: Span) -> &'a str {
        &self.source[span.start..span.end]
    }

    /// The span of token `tok`.
    pub fn token_span(&self, tok: Tok) -> Span {
        Span {
            start: self.tokens[tok].start,
            end: self.tokens[tok].end,
        }
    }

    /// The source text of token `tok`.
    pub fn token_text(&self, tok: Tok) -> &'a str {
        let token = &self.tokens[tok];
        &self.source[token.start..token.end]
    }

    /// The token that starts at `offset`.
    pub fn token_at(&self, offset: usize) -> Tok {
        parse::token_at(self.tokens, offset)
    }

    /// Starts a rewrite of a removed spelling at `span`: edits made until
    /// [`Self::leave`] belong to it. `removed` names the spelling and
    /// `advice` says what replaces it.
    pub fn enter(
        &mut self,
        rule: Rule,
        span: Span,
        removed: impl Into<String>,
        advice: impl Into<String>,
    ) -> Option<usize> {
        let group = self.rewrites.len();
        self.rewrites.push(Rewrite {
            rule,
            span,
            removed: removed.into(),
            advice: advice.into(),
        });
        self.edits.enter(Some(group))
    }

    /// Ends the rewrite [`Self::enter`] started, restoring `previous`.
    pub fn leave(&mut self, previous: Option<usize>) {
        self.edits.enter(previous);
    }

    fn declare(&mut self, body: &'a [Stmt], prefix: &str) {
        for stmt in body {
            match &stmt.kind {
                StmtKind::Def(def) => {
                    self.declared.methods.insert(def.name.clone());
                    self.def_ranges
                        .push(self.tokens[def.keyword].start..self.tokens[def.end].end);
                    if prefix.is_empty() {
                        self.declared.functions.insert(def.name.clone(), def);
                    }
                }
                StmtKind::Class(class) => self.declare_class(class, prefix),
                StmtKind::Enum(declared) => {
                    self.declared
                        .enums
                        .insert(declared.name.clone(), declared.members.clone());
                }
                _ => (),
            }
        }
    }

    fn declare_class(&mut self, class: &'a Class, prefix: &str) {
        let name = if prefix.is_empty() {
            class.name.clone()
        } else {
            format!("{prefix}.{}", class.name)
        };
        self.declared.classes.insert(name.clone(), class);
        let mut private = false;
        for member in &class.members {
            match member {
                Member::Def(def) => {
                    self.declared.methods.insert(def.name.clone());
                    self.def_ranges
                        .push(self.tokens[def.keyword].start..self.tokens[def.end].end);
                    if private || def.name == "initialize" {
                        self.declared.private_methods.insert(def.name.clone());
                    }
                }
                Member::Property(property) => {
                    for (tok, _) in &property.names {
                        let name = self.token_text(*tok);
                        self.declared.methods.insert(name.to_owned());
                        self.declared.methods.insert(format!("{name}="));
                    }
                }
                Member::Class(inner) => self.declare_class(inner, &name),
                Member::Ivar(..) | Member::ClassVar(..) => (),
                Member::Other(span) => {
                    // `send` reaches private and protected methods, which a
                    // direct call from outside the class does not.
                    let text = self.text(*span).trim();
                    if text == "private" || text == "protected" {
                        private = true;
                    } else if text == "public" {
                        private = false;
                    } else if let Some(names) = text
                        .strip_prefix("private ")
                        .or_else(|| text.strip_prefix("protected "))
                    {
                        for name in names.split(',') {
                            let name = name.trim().trim_start_matches(':');
                            self.declared.private_methods.insert(name.to_owned());
                        }
                    }
                }
                Member::Stmt(_) => (),
            }
        }
    }

    /// Whether a class or enum of that dotted name is declared here.
    pub fn known_type(&self, name: &str) -> bool {
        self.declared.classes.contains_key(name) || self.declared.enums.contains_key(name)
    }

    /// The innermost scope.
    pub fn scope(&self) -> &Scope<'a> {
        self.scopes.last().expect("a scope")
    }

    /// Whether `name` is a local, parameter or rescue binding in scope.
    pub fn local(&self, name: &str) -> bool {
        self.scopes
            .iter()
            .rev()
            .take_while(|_| true)
            .any(|scope| scope.locals.contains(name))
    }

    /// A receiver kind the syntax decides: a literal, a builtin namespace,
    /// a rescued error or an annotated parameter.
    pub fn static_kind(&self, receiver: &Expr) -> Option<String> {
        Some(
            match &receiver.kind {
                ExprKind::Str | ExprKind::Template(_) => "string",
                ExprKind::Symbol => "symbol",
                ExprKind::Array(_) | ExprKind::Words => "array",
                ExprKind::Hash(_) => "hash",
                ExprKind::Integer => "int",
                ExprKind::Float => "float",
                ExprKind::Range(..) => "range",
                ExprKind::Group(_, inner, _) => return self.static_kind(inner),
                ExprKind::Name(name) => {
                    if self.local(name) {
                        let scope = self.scope();
                        if scope.rescues.contains(name) {
                            return Some("error".to_owned());
                        }
                        let def = scope.def?;
                        let param = def.params.iter().find(|param| param.name == *name)?;
                        let ty = param.ty.as_ref()?;
                        if ty.nullable {
                            return None;
                        }
                        let TypeKind::Named(tok, _) = &ty.kind else {
                            return None;
                        };
                        let written = self.token_text(*tok).to_ascii_lowercase();
                        return matches!(
                            written.as_str(),
                            "string"
                                | "symbol"
                                | "array"
                                | "hash"
                                | "int"
                                | "float"
                                | "money"
                                | "duration"
                                | "time"
                                | "range"
                        )
                        .then(|| written.replace("object", "hash"));
                    }
                    if namespace_name(name) && !self.declared.classes.contains_key(name.as_str()) {
                        return Some(name.clone());
                    }
                    return None;
                }
                _ => return None,
            }
            .to_owned(),
        )
    }

    /// Whether an expression starts from a name the script never binds,
    /// which the host supplies, such as a capability.
    pub fn host_rooted(&self, expr: &Expr) -> bool {
        match &expr.kind {
            ExprKind::Name(name) => {
                !self.local(name)
                    && !self.declared.functions.contains_key(name.as_str())
                    && !self.known_type(name)
                    && !namespace_name(name)
                    && !parse::keyword(name)
            }
            ExprKind::Call(call) => match &call.receiver {
                Some(receiver) => self.host_rooted(receiver),
                None => {
                    call.args.is_none()
                        && call.block.is_none()
                        && !self.local(&call.name)
                        && !self.declared.functions.contains_key(call.name.as_str())
                }
            },
            ExprKind::Index(receiver, ..) | ExprKind::Group(_, receiver, _) => {
                self.host_rooted(receiver)
            }
            _ => false,
        }
    }

    /// The argument of `call` whose value spans `span`.
    pub fn expr_at(&self, call: Option<&'a Call>, span: Span) -> Option<&'a Expr> {
        let args = call?.args.as_ref()?;
        args.items
            .iter()
            .map(|arg| &arg.value)
            .find(|value| value.span == span)
    }

    /// Where the compiler reports an operator: its last character, except
    /// for `<=>` and `===`.
    pub fn operator_offset(&self, op: Tok) -> usize {
        let token = &self.tokens[op];
        match self.token_text(op) {
            "<=>" | "===" => token.start,
            text => token.start + text.len() - 1,
        }
    }

    /// Whether an expression binds tighter than any binary operator after
    /// the rewrites made to it.
    pub fn primary(&self, expr: &Expr) -> bool {
        primary(expr) && !self.operator_rewrites.contains(&expr.span)
    }
}

/// The builtin namespaces a capitalized name can refer to.
pub fn namespace_name(name: &str) -> bool {
    matches!(
        name,
        "Time" | "Duration" | "JSON" | "Math" | "Regex" | "Regexp" | "Hash"
    )
}

/// Whether a builtin namespace member can be called without arguments.
pub fn namespace_member_takes_no_arguments(namespace: &str, member: &str) -> bool {
    let table = crate::signatures::table();
    let namespace = if namespace == "Regexp" {
        "Regex"
    } else {
        namespace
    };
    table.module(namespace).is_some_and(|module| {
        module.named(member).any(|member| match member {
            crate::signatures::Member::Function(function) => function.params.iter().all(|param| {
                param.optional || param.kind != crate::signatures::ParamKind::Positional
            }),
            _ => false,
        })
    })
}

/// An expression that is safe to repeat: a local, instance variable, `self`
/// or literal.
pub fn simple(expr: &Expr) -> bool {
    matches!(
        expr.kind,
        ExprKind::Name(_)
            | ExprKind::Ivar(_)
            | ExprKind::SelfRef
            | ExprKind::Nil
            | ExprKind::True
            | ExprKind::False
            | ExprKind::Integer
            | ExprKind::Float
            | ExprKind::Str
            | ExprKind::Symbol
    )
}

/// An expression that binds tighter than any binary operator.
pub fn primary(expr: &Expr) -> bool {
    match &expr.kind {
        ExprKind::Binary(..)
        | ExprKind::Range(..)
        | ExprKind::Ternary(..)
        | ExprKind::Rescue(..)
        | ExprKind::Unary(..) => false,
        ExprKind::Call(call) => call.args.as_ref().is_none_or(|args| args.parens.is_some()),
        _ => true,
    }
}

/// Whether `name` can be called as a method: an identifier, possibly ending
/// in `?` or `!`, that is not a keyword.
pub fn method_name(name: &str) -> bool {
    let body = name.trim_end_matches(['?', '!']);
    body.len() + 1 >= name.len()
        && body
            .chars()
            .next()
            .is_some_and(|c| c == '_' || c.is_alphabetic())
        && body.chars().all(|c| c == '_' || c.is_alphanumeric())
        && !parse::keyword(body)
}

/// Spells bytes as a double-quoted string literal, when every byte can be.
pub fn string_literal(bytes: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(bytes).ok()?;
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            '#' if chars.peek() == Some(&'{') => out.push_str("\\#"),
            c if c.is_control() => return None,
            c => out.push(c),
        }
    }
    out.push('"');
    Some(out)
}

/// Spells bytes as a symbol literal, quoted when they are not a name.
pub fn symbol_literal(bytes: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(bytes).ok()?;
    if method_name(text) {
        return Some(format!(":{text}"));
    }
    string_literal(bytes).map(|quoted| format!(":{quoted}"))
}

/// Gathers the names a body binds, without entering nested functions.
pub fn collect_locals(body: &[Stmt], scope: &mut Scope<'_>) {
    for stmt in body {
        collect_stmt(stmt, scope);
    }
}

/// Gathers the names a function's rescue clauses bind.
pub fn collect_rescued(rescued: &Rescued, scope: &mut Scope<'_>) {
    for clause in &rescued.rescues {
        if let Some(name) = &clause.binding {
            scope.locals.insert(name.clone());
            scope.rescues.insert(name.clone());
        }
        collect_locals(&clause.body, scope);
    }
    for body in rescued.alternate.iter().chain(&rescued.ensure) {
        collect_locals(body, scope);
    }
}

fn collect_stmt(stmt: &Stmt, scope: &mut Scope<'_>) {
    match &stmt.kind {
        StmtKind::Assign(assign) => {
            for target in &assign.targets {
                target.names(&mut |name, _| {
                    scope.locals.insert(name.to_owned());
                });
            }
            for value in &assign.values {
                collect_expr(value, scope);
            }
        }
        StmtKind::Expr(expr) => collect_expr(expr, scope),
        StmtKind::If(node) => collect_if(node, scope),
        StmtKind::While(node) => {
            collect_expr(&node.condition, scope);
            collect_locals(&node.body, scope);
        }
        StmtKind::For(node) => {
            node.target.names(&mut |name, _| {
                scope.locals.insert(name.to_owned());
            });
            collect_expr(&node.iterable, scope);
            collect_locals(&node.body, scope);
        }
        StmtKind::Modifier(node) => {
            collect_stmt(&node.body, scope);
            collect_expr(&node.condition, scope);
        }
        StmtKind::Flow(_, Some(value)) => collect_expr(value, scope),
        StmtKind::Raise(_, value, message) => {
            for expr in value.iter().chain(message) {
                collect_expr(expr, scope);
            }
        }
        _ => (),
    }
}

fn collect_if(node: &If, scope: &mut Scope<'_>) {
    for (condition, body) in &node.branches {
        collect_expr(condition, scope);
        collect_locals(body, scope);
    }
    if let Some((_, body)) = &node.alternate {
        collect_locals(body, scope);
    }
}

/// Gathers the names an expression binds, such as locals assigned inside
/// an `if` expression or a block.
pub fn collect_expr(expr: &Expr, scope: &mut Scope<'_>) {
    match &expr.kind {
        ExprKind::If(node) => collect_if(node, scope),
        ExprKind::Loop(stmt) => collect_stmt(stmt, scope),
        ExprKind::Begin(node) => {
            collect_locals(&node.body, scope);
            collect_rescued(&node.rescued, scope);
        }
        ExprKind::Case(node) => {
            if let Some(subject) = &node.subject {
                collect_expr(subject, scope);
            }
            for when in &node.whens {
                collect_expr(&when.result, scope);
            }
            if let Some((_, alternate)) = &node.alternate {
                collect_expr(alternate, scope);
            }
        }
        ExprKind::Group(_, inner, _) => collect_expr(inner, scope),
        ExprKind::Binary(_, left, right) => {
            collect_expr(left, scope);
            collect_expr(right, scope);
        }
        ExprKind::Call(call) => {
            if let Some(receiver) = &call.receiver {
                collect_expr(receiver, scope);
            }
            for arg in call.args.iter().flat_map(|args| &args.items) {
                collect_expr(&arg.value, scope);
            }
            // Blocks see the enclosing locals and may assign them.
            if let Some(block) = &call.block
                && !scope.block
            {
                collect_locals(&block.body, scope);
            }
        }
        _ => (),
    }
}
