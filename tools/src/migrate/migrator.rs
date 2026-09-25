//! Walks a parsed source and records every rewrite as an edit.

use super::{
    Code, Diagnostic, Migration, Options,
    observe::Facts,
    parse,
    renames::{ArgPattern, Callee, KeywordValue, Pattern, Rewrite, TemplatePiece, patterns},
    rewrite::{Edits, Piece},
    syntax::*,
    types::Types,
};
use std::collections::{HashMap, HashSet};
use vibescript::tooling::TokenKind;

/// Nesting deeper than the parser's default stack allows runs on a larger one.
const STACK: usize = 256 << 20;

pub(crate) fn migrate(source: &str, facts: Option<&Facts>, options: &Options) -> Migration {
    std::thread::scope(|scope| {
        std::thread::Builder::new()
            .stack_size(STACK)
            .spawn_scoped(scope, || migrate_on_stack(source, facts, options))
            .expect("spawn the migration thread")
            .join()
            .unwrap_or_else(|_| Migration {
                source: source.to_owned(),
                changed: false,
                diagnostics: vec![diagnostic(
                    source,
                    Code::Internal,
                    0,
                    "the migration failed on this source; it was left unchanged".to_owned(),
                )],
            })
    })
}

fn unchanged(source: &str, diagnostics: Vec<Diagnostic>) -> Migration {
    Migration {
        source: source.to_owned(),
        changed: false,
        diagnostics,
    }
}

fn migrate_on_stack(source: &str, facts: Option<&Facts>, options: &Options) -> Migration {
    let script = match vibescript::Engine::new().compile(source) {
        Ok(script) => script,
        Err(error) => {
            let offset = error.offset.unwrap_or(0);
            let message = format!(
                "does not compile, so it was left unchanged: {}",
                error.message
            );
            return unchanged(
                source,
                vec![diagnostic(source, Code::Unparsed, offset, message)],
            );
        }
    };
    let tree = match parse::parse(source) {
        Ok(tree) => tree,
        Err(fail) => {
            let message = format!("could not be parsed for migration: {}", fail.message);
            return unchanged(
                source,
                vec![diagnostic(source, Code::Internal, fail.offset, message)],
            );
        }
    };
    let mut migrator = Migrator::new(source, &tree, facts, options);
    migrator.script = Some(&script);
    migrator.program(&tree.body);
    let mut diagnostics = std::mem::take(&mut migrator.diagnostics);
    diagnostics.sort_by_key(|d| (d.offset, d.code));
    diagnostics.dedup();
    if migrator.edits.is_empty() {
        return unchanged(source, diagnostics);
    }
    let output = migrator.edits.apply(source);
    if let Some(span) = migrator.edits.conflicts.first() {
        diagnostics.push(diagnostic(
            source,
            Code::Internal,
            span.start,
            "overlapping rewrites; the source was left unchanged".to_owned(),
        ));
        return unchanged(source, diagnostics);
    }
    // Formatting normalizes line ends and trailing spaces, which would change
    // a string literal that spans lines with them. Rewrites never add such
    // literals, so the original source decides.
    let output = if literals_survive_formatting(source) {
        crate::format::format(&output)
    } else {
        output
    };
    if !options.new_syntax
        && let Err(error) = vibescript::Engine::new().compile(&output)
    {
        diagnostics.push(diagnostic(
            source,
            Code::Internal,
            0,
            format!(
                "the migrated source did not compile ({}); the source was left unchanged",
                error.message
            ),
        ));
        return unchanged(source, diagnostics);
    }
    Migration {
        changed: output != source,
        source: output,
        diagnostics,
    }
}

fn literals_survive_formatting(source: &str) -> bool {
    let Ok(tokens) = vibescript::tooling::tokens(source) else {
        return false;
    };
    tokens.iter().all(|token| {
        let literal = matches!(
            token.kind,
            TokenKind::String(_)
                | TokenKind::Template(_)
                | TokenKind::Regex
                | TokenKind::Words { .. }
                | TokenKind::Symbol { quoted: true, .. }
        );
        let text = &source[token.span.clone()];
        !literal || (!text.contains('\r') && !text.contains(" \n") && !text.contains("\t\n"))
    })
}

pub(crate) fn diagnostic(source: &str, code: Code, offset: usize, message: String) -> Diagnostic {
    let offset = offset.min(source.len());
    let before = &source[..offset];
    let line = before.matches('\n').count() + 1;
    let column = before[before.rfind('\n').map_or(0, |i| i + 1)..]
        .chars()
        .count()
        + 1;
    Diagnostic {
        code,
        offset,
        line,
        column,
        message,
    }
}

/// Where an expression stands, which decides whether a replacement with a
/// low-precedence operator needs parentheses.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Place {
    /// A whole statement, whose value may be discarded.
    Statement,
    /// Anywhere a whole expression may stand: an argument, a value being
    /// assigned, returned or tested.
    Loose,
    /// An operand, a receiver or an indexed value.
    Tight,
}

/// Declarations in the source, gathered before the walk.
#[derive(Default)]
pub(crate) struct Declared<'a> {
    pub functions: HashMap<String, &'a Def>,
    /// Classes by their dotted name, and the methods each defines.
    pub classes: HashMap<String, &'a Class>,
    pub enums: HashMap<String, Vec<String>>,
    /// Every method name a class or top-level function defines.
    pub methods: HashSet<String>,
    pub private_methods: HashSet<String>,
}

/// What a function body binds, for the names that could be locals.
#[derive(Default)]
pub(crate) struct Scope<'a> {
    pub def: Option<&'a Def>,
    pub class: Option<&'a Class>,
    pub locals: HashSet<String>,
    pub rescues: HashSet<String>,
}

pub(crate) struct Migrator<'a> {
    pub source: &'a str,
    pub tokens: &'a [Token],
    pub facts: Option<&'a Facts>,
    pub options: &'a Options,
    pub edits: Edits,
    pub diagnostics: Vec<Diagnostic>,
    pub declared: Declared<'a>,
    pub scopes: Vec<Scope<'a>>,
    /// Conditions by the offset of their branch.
    pub conditions: HashMap<usize, Vec<(usize, &'a Types)>>,
    /// The compiled source, for the checker's inferences.
    pub script: Option<&'a vibescript::Script>,
    /// Expressions a rename rewrote as an operator, such as `x.nil?` as `x == nil`.
    pub operator_rewrites: HashSet<Span>,
    /// The offsets of every `&&` and `||`, where they test their left operand.
    pub short_circuits: HashSet<usize>,
    /// How many computed callees enclose the walk: `(x.m rescue y)()` calls
    /// what `x.m` evaluates to, so its call forms stay exactly as written.
    pub frozen: usize,
    /// Locals whose first assignment has been seen, by function.
    pub first_assignments: HashSet<(Tok, String)>,
    /// The source range of every function.
    pub def_ranges: Vec<std::ops::Range<usize>>,
}

impl<'a> Migrator<'a> {
    fn new(
        source: &'a str,
        tree: &'a Tree,
        facts: Option<&'a Facts>,
        options: &'a Options,
    ) -> Self {
        let mut conditions: HashMap<usize, Vec<(usize, &Types)>> = HashMap::new();
        if let Some(facts) = facts {
            for ((report, origin), types) in &facts.conditions {
                conditions
                    .entry(*report)
                    .or_default()
                    .push((*origin, types));
            }
        }
        let mut migrator = Self {
            source,
            tokens: &tree.tokens,
            facts,
            options,
            edits: Edits::default(),
            diagnostics: Vec::new(),
            declared: Declared::default(),
            scopes: Vec::new(),
            conditions,
            script: None,
            operator_rewrites: HashSet::new(),
            first_assignments: HashSet::new(),
            def_ranges: Vec::new(),
            short_circuits: HashSet::new(),
            frozen: 0,
        };
        for (index, token) in tree.tokens.iter().enumerate() {
            if matches!(token.kind, TokenKind::Operator("&&" | "||")) {
                let offset = migrator.operator_offset(index);
                migrator.short_circuits.insert(offset);
            }
        }
        migrator.declare(&tree.body, "");
        migrator
    }

    pub fn text(&self, span: Span) -> &'a str {
        &self.source[span.start..span.end]
    }

    pub fn token_span(&self, tok: Tok) -> Span {
        Span {
            start: self.tokens[tok].start,
            end: self.tokens[tok].end,
        }
    }

    pub fn token_text(&self, tok: Tok) -> &'a str {
        let token = &self.tokens[tok];
        &self.source[token.start..token.end]
    }

    pub fn report(&mut self, code: Code, offset: usize, message: impl Into<String>) {
        self.diagnostics
            .push(diagnostic(self.source, code, offset, message.into()));
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
                Member::Ivar(..) => (),
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

    // Walk ----------------------------------------------------------------

    fn program(&mut self, body: &'a [Stmt]) {
        let mut scope = Scope::default();
        collect_locals(body, &mut scope);
        self.scopes.push(scope);
        self.statements(body);
        self.scopes.pop();
    }

    pub fn statements(&mut self, body: &'a [Stmt]) {
        for stmt in body {
            self.stmt(stmt);
        }
    }

    fn stmt(&mut self, stmt: &'a Stmt) {
        match &stmt.kind {
            StmtKind::Expr(expr) => self.expr(expr, Place::Statement),
            StmtKind::Assign(assign) => self.assign(stmt, assign),
            StmtKind::If(node) => self.if_node(node, self.tokens[node.keyword].start),
            StmtKind::While(node) => self.while_node(node),
            StmtKind::For(node) => {
                self.target(&node.target);
                self.expr(&node.iterable, Place::Loose);
                self.statements(&node.body);
            }
            StmtKind::Modifier(node) => self.modifier(stmt, node),
            StmtKind::Flow(_, value) => {
                if let Some(value) = value {
                    self.expr(value, Place::Loose);
                }
            }
            StmtKind::Raise(_, value, message) => {
                for expr in value.iter().chain(message) {
                    self.expr(expr, Place::Loose);
                }
            }
            StmtKind::Retry(_) | StmtKind::Enum(_) | StmtKind::Other => (),
            StmtKind::Def(def) => self.def(def, None),
            StmtKind::Class(class) => self.class(class, ""),
        }
    }

    fn assign(&mut self, stmt: &'a Stmt, assign: &'a Assign) {
        for target in &assign.targets {
            self.target(target);
        }
        for value in &assign.values {
            self.expr(value, Place::Loose);
        }
        self.logical_assignment(stmt, assign);
        self.declare_local(stmt, assign);
    }

    pub fn target(&mut self, target: &'a Target) {
        match target {
            Target::Expr(expr) => match &expr.kind {
                ExprKind::Index(receiver, _, selectors, _) => {
                    self.expr(receiver, Place::Tight);
                    self.symbol_keys(expr, selectors);
                    for selector in selectors {
                        self.expr(selector, Place::Loose);
                    }
                }
                ExprKind::Call(call) => {
                    if let Some(receiver) = &call.receiver {
                        self.expr(receiver, Place::Tight);
                    }
                }
                _ => (),
            },
            Target::Splat(_, Some(inner)) => self.target(inner),
            Target::Typed(inner, ty) => {
                self.target(inner);
                // A block parameter's check is not observed, so a failure
                // quoting the old spelling cannot be ruled out.
                self.type_names_checked(ty, false);
            }
            Target::Group(_, parts) => {
                for part in parts {
                    self.target(part);
                }
            }
            Target::Splat(_, None) => (),
        }
    }

    fn if_node(&mut self, node: &'a If, report: usize) {
        if node.unless {
            let (condition, _) = &node.branches[0];
            self.edits.text(self.token_span(node.keyword), "if");
            self.negated_condition(condition, report);
        } else {
            for (condition, _) in &node.branches {
                self.condition(condition, report);
            }
        }
        for (_, body) in &node.branches {
            self.statements(body);
        }
        if let Some((_, body)) = &node.alternate {
            self.statements(body);
        }
    }

    fn while_node(&mut self, node: &'a While) {
        let report = self.tokens[node.keyword].start;
        if node.until {
            self.edits.text(self.token_span(node.keyword), "while");
            self.negated_condition(&node.condition, report);
        } else {
            self.condition(&node.condition, report);
        }
        self.statements(&node.body);
    }

    fn modifier(&mut self, stmt: &'a Stmt, node: &'a Modifier) {
        self.stmt(&node.body);
        let report = stmt.span.start;
        match node.kind {
            ModifierKind::If | ModifierKind::While => self.condition(&node.condition, report),
            ModifierKind::Unless | ModifierKind::Until => {
                let keyword = if node.kind == ModifierKind::Unless {
                    "if"
                } else {
                    "while"
                };
                self.edits.text(self.token_span(node.keyword), keyword);
                self.negated_condition(&node.condition, report);
            }
        }
    }

    pub fn def(&mut self, def: &'a Def, class: Option<&'a Class>) {
        let mut scope = Scope {
            def: Some(def),
            class,
            ..Scope::default()
        };
        for param in &def.params {
            scope.locals.insert(param.name.clone());
            if let Some(default) = &param.default {
                collect_expr(default, &mut scope);
            }
        }
        collect_locals(&def.body, &mut scope);
        if let Some(rescued) = &def.rescue {
            collect_rescued(rescued, &mut scope);
        }
        self.scopes.push(scope);
        let offset = def.offset(self.tokens);
        let bound = self.bindings_passed(offset);
        for param in &def.params {
            if let Some(ty) = &param.ty {
                self.type_names_checked(ty, bound);
            }
            if let Some(default) = &param.default {
                self.expr(default, Place::Loose);
            }
        }
        if let Some((_, ty)) = &def.result {
            let returned = self
                .facts
                .and_then(|facts| facts.returns.get(&offset))
                .is_none_or(|types| self.accepts(ty, types));
            self.type_names_checked(ty, returned);
        }
        self.annotate_def(def, class);
        self.statements(&def.body);
        if let Some(rescued) = &def.rescue {
            self.rescued(rescued);
        }
        self.scopes.pop();
    }

    fn rescued(&mut self, rescued: &'a Rescued) {
        for clause in &rescued.rescues {
            self.statements(&clause.body);
        }
        for body in rescued.alternate.iter().chain(&rescued.ensure) {
            self.statements(body);
        }
    }

    fn class(&mut self, class: &'a Class, prefix: &str) {
        let name = if prefix.is_empty() {
            class.name.clone()
        } else {
            format!("{prefix}.{}", class.name)
        };
        self.annotate_class(class, &name);
        for member in &class.members {
            match member {
                Member::Def(def) => self.def(def, Some(class)),
                Member::Property(property) => {
                    for (tok, ty) in &property.names {
                        if let Some(ty) = ty {
                            let offset = self.tokens[*tok].start;
                            let field = self.token_text(*tok);
                            let accepts = |types: Option<&Types>| {
                                types.is_none_or(|types| self.accepts(ty, types))
                            };
                            let passed = self.bindings_passed(offset)
                                && accepts(self.facts.and_then(|facts| facts.returns.get(&offset)))
                                && accepts(
                                    self.facts
                                        .and_then(|facts| facts.instance.get(&name, field)),
                                );
                            self.type_names_checked(ty, passed);
                        }
                    }
                }
                Member::Class(inner) => self.class(inner, &name),
                Member::Ivar(_, _, default) => {
                    if let Some(default) = default {
                        self.expr(default, Place::Loose);
                    }
                }
                Member::Stmt(stmt) => {
                    let mut scope = Scope {
                        class: Some(class),
                        ..Scope::default()
                    };
                    collect_locals(std::slice::from_ref(stmt), &mut scope);
                    self.scopes.push(scope);
                    self.stmt(stmt);
                    self.scopes.pop();
                }
                Member::Other(_) => (),
            }
        }
    }

    pub fn expr(&mut self, expr: &'a Expr, place: Place) {
        match &expr.kind {
            ExprKind::Nil
            | ExprKind::True
            | ExprKind::False
            | ExprKind::SelfRef
            | ExprKind::Integer
            | ExprKind::Float
            | ExprKind::Str
            | ExprKind::Symbol
            | ExprKind::Regex
            | ExprKind::Ivar(_)
            | ExprKind::TypeLiteral => (),
            ExprKind::Name(name) => self.bare_name(expr, name, place),
            ExprKind::Words => self.words(expr, place),
            ExprKind::Template(parts) => {
                for part in parts.iter().flatten() {
                    self.expr(part, Place::Loose);
                }
            }
            ExprKind::Array(items) => {
                for item in items {
                    self.expr(item, Place::Loose);
                }
            }
            ExprKind::Hash(entries) => {
                for entry in entries {
                    if !entry.shorthand {
                        self.expr(&entry.value, Place::Loose);
                    }
                }
            }
            ExprKind::Unary(op, operand) => self.unary(expr, *op, operand),
            ExprKind::Binary(op, left, right) => self.binary(expr, *op, left, right, place),
            ExprKind::Range(left, _, right) => {
                for operand in left.iter().chain(right) {
                    self.expr(operand, Place::Tight);
                }
            }
            ExprKind::Ternary(condition, question, yes, no) => {
                let report = self.tokens[*question].start;
                self.condition(condition, report);
                self.expr(yes, Place::Loose);
                self.expr(no, Place::Loose);
            }
            ExprKind::Call(call) => self.call(expr, call, place),
            ExprKind::Computed(callee, args) => {
                self.frozen += 1;
                self.expr(callee, Place::Tight);
                self.frozen -= 1;
                self.args(args);
            }
            ExprKind::BlockCall(callee, block) => {
                self.expr(callee, Place::Tight);
                self.block(block, None, callee.span.end);
            }
            ExprKind::Index(receiver, _, selectors, _) => {
                self.expr(receiver, Place::Tight);
                self.symbol_keys(expr, selectors);
                for selector in selectors {
                    self.expr(selector, Place::Loose);
                }
            }
            ExprKind::Yield(_, args) => {
                if let Some(args) = args {
                    self.args(args);
                }
            }
            ExprKind::Group(_, inner, _) => self.expr(inner, Place::Loose),
            ExprKind::If(node) => self.if_node(node, self.tokens[node.keyword].start),
            ExprKind::Case(node) => self.case(node),
            ExprKind::Loop(stmt) => self.stmt(stmt),
            ExprKind::Begin(node) => {
                self.statements(&node.body);
                self.rescued(&node.rescued);
            }
            ExprKind::Rescue(body, _, fallback) => {
                self.expr(body, Place::Tight);
                self.expr(fallback, Place::Tight);
            }
        }
    }

    fn case(&mut self, node: &'a Case) {
        if let Some(subject) = &node.subject {
            self.expr(subject, Place::Loose);
        }
        for when in &node.whens {
            for (value, _) in &when.values {
                self.expr(value, Place::Loose);
            }
            self.expr(&when.result, Place::Loose);
        }
        if let Some((_, alternate)) = &node.alternate {
            self.expr(alternate, Place::Loose);
        }
        self.enum_case(node);
    }

    pub fn args(&mut self, args: &'a Args) {
        let command = args.parens.is_none();
        for (index, arg) in args.items.iter().enumerate() {
            let place = if command && index == 0 {
                Place::Tight
            } else {
                Place::Loose
            };
            if matches!(arg.kind, ArgKind::Keyword(_)) && arg.value.span == arg.span {
                continue;
            }
            self.expr(&arg.value, place);
        }
    }

    fn call(&mut self, expr: &'a Expr, call: &'a Call, place: Place) {
        if let Some(receiver) = &call.receiver {
            self.expr(receiver, Place::Tight);
        }
        if let Some(args) = &call.args {
            self.args(args);
            // After a local, such as a block's implicit `it`, a percent
            // literal is a command argument where an array would index.
            if args.parens.is_none()
                && call.receiver.is_none()
                && (self.local(&call.name) || call.name == "it")
                && let Some(first) = args.items.first()
                && matches!(first.value.kind, ExprKind::Words)
            {
                self.edits.wrap(first.value.span, "(", ")");
            }
        }
        if let Some(block) = &call.block {
            let end = call
                .args
                .as_ref()
                .and_then(|args| args.items.last().map(|arg| arg.span.end))
                .unwrap_or(self.token_span(call.name_tok).end);
            self.block(block, Some(call), end);
        }
        if self.frozen > 0 {
            return;
        }
        if self.rename(expr, call, place) {
            return;
        }
        self.empty_parens(expr, call);
        self.dispatch(expr, call);
        self.hash_new(expr, call, place);
        self.require(call);
    }

    /// Converts a `do ... end` block to braces, keeping the call it attaches to.
    fn block(&mut self, block: &'a Block, owner: Option<&'a Call>, callee_end: usize) {
        let mut scope = Scope {
            def: self.scope().def,
            class: self.scope().class,
            ..Scope::default()
        };
        for param in &block.params {
            param.names(&mut |name, _| {
                scope.locals.insert(name.to_owned());
            });
        }
        collect_locals(&block.body, &mut scope);
        self.scopes.push(scope);
        for param in &block.params {
            self.target(param);
        }
        self.statements(&block.body);
        self.scopes.pop();
        if block.brace {
            return;
        }
        // Parentheses would turn a command's bare keywords from an options
        // hash into keyword arguments, so such a block stays as written.
        if let Some(call) = owner
            && let Some(args) = &call.args
            && args.parens.is_none()
            && args
                .items
                .iter()
                .any(|arg| matches!(arg.kind, ArgKind::Keyword(_) | ArgKind::KeywordSplat))
        {
            self.report(
                Code::Syntax,
                self.tokens[block.open].start,
                "this do block's call passes bare keywords, which parentheses would bind differently; convert it to braces by hand",
            );
            return;
        }
        let open = &self.tokens[block.open];
        let previous = self.tokens[..block.open]
            .iter()
            .rev()
            .find(|token| !matches!(token.kind, TokenKind::Newline | TokenKind::Semicolon))
            .map_or(callee_end, |token| token.end);
        if self.source[previous..open.start].contains('\n') {
            // A brace must follow its call on the same line, with its
            // parameters, while comments stay where they are.
            let moved_end = block
                .pipes
                .map_or(open.end, |(_, close)| self.tokens[close].end);
            let line_end = self.source[moved_end..]
                .find('\n')
                .map_or(self.source.len(), |index| moved_end + index);
            let rest_blank = self.source[moved_end..line_end].trim().is_empty();
            let mut pieces = vec![Piece::Text(" {".into())];
            if let Some((open_pipe, close_pipe)) = block.pipes {
                pieces.push(Piece::Text(" ".into()));
                pieces.push(Piece::Source(Span {
                    start: self.tokens[open_pipe].start,
                    end: self.tokens[close_pipe].end,
                }));
            }
            self.edits.replace(
                Span {
                    start: previous,
                    end: previous,
                },
                Vec::new(),
            );
            let text: String = pieces
                .iter()
                .map(|piece| match piece {
                    Piece::Text(text) => text.clone(),
                    Piece::Source(span) => self.text(*span).to_owned(),
                })
                .collect();
            self.edits.insert(previous, text);
            // Drop `do |params|` and, when nothing else is on its line, the line.
            let line_start = self.source[..open.start].rfind('\n').map_or(0, |i| i + 1);
            let alone = self.source[line_start..open.start].trim().is_empty();
            let removed = if alone && rest_blank {
                Span {
                    start: line_start.saturating_sub(1),
                    end: line_end,
                }
            } else {
                Span {
                    start: open.start,
                    end: moved_end,
                }
            };
            self.edits.text(removed, "");
        } else {
            self.edits.text(self.token_span(block.open), "{");
        }
        self.edits.text(self.token_span(block.close), "}");
        // Braces bind to the nearest call, so a command call's arguments
        // get parentheses to keep the block on the call it belonged to.
        if let Some(call) = owner
            && let Some(args) = &call.args
            && args.parens.is_none()
            && let (Some(first), Some(last)) = (args.items.first(), args.items.last())
        {
            let name_end = self.tokens[call.name_tok].end;
            self.edits.text(
                Span {
                    start: name_end,
                    end: first.span.start,
                },
                "(",
            );
            self.edits.insert(last.span.end, ")");
        }
    }

    fn bare_name(&mut self, expr: &'a Expr, name: &str, place: Place) {
        // A global function renamed without parentheses, such as `now`.
        if self.local(name) || self.declared.methods.contains(name) || self.known_type(name) {
            return;
        }
        let Some(pattern) = patterns()
            .iter()
            .find(|p| p.callee == Callee::Global && p.name == name && p.args.is_none())
        else {
            return;
        };
        self.apply_pattern(expr, None, pattern, &Captures::default(), place);
    }

    fn words(&mut self, expr: &'a Expr, place: Place) {
        let tok = self.token_at(expr.span.start);
        let TokenKind::Words { symbols, entries } = &self.tokens[tok].kind else {
            return;
        };
        let mut items = Vec::new();
        for entry in entries {
            let Some(bytes) = entry else {
                self.report(
                    Code::Syntax,
                    expr.span.start,
                    "an interpolated percent literal needs rewriting as an array literal by hand",
                );
                return;
            };
            let Some(item) = (if *symbols {
                symbol_literal(bytes)
            } else {
                string_literal(bytes)
            }) else {
                self.report(
                    Code::Syntax,
                    expr.span.start,
                    "a percent literal entry has bytes a string literal cannot spell",
                );
                return;
            };
            items.push(item);
        }
        // A percent literal after a command name is an argument, which an
        // array literal after a space is as well.
        let _ = place;
        self.edits
            .text(expr.span, format!("[{}]", items.join(", ")));
    }

    /// Rewrites `h[:name]` as `h["name"]` when the receiver is a hash.
    fn symbol_keys(&mut self, expr: &'a Expr, selectors: &'a [Expr]) {
        let [selector] = selectors else {
            return;
        };
        if !matches!(selector.kind, ExprKind::Symbol) {
            return;
        }
        let ExprKind::Index(receiver, open, ..) = &expr.kind else {
            return;
        };
        let _ = receiver;
        let offset = self.tokens[*open].start;
        let observed = self
            .facts
            .and_then(|facts| facts.indexes.get(&offset))
            .map(|(receiver, _)| receiver);
        if let Some(receiver) = observed
            && (receiver.nil
                || receiver.any
                || receiver.array.is_some()
                || !receiver.scalars.is_empty()
                || !receiver.classes.is_empty()
                || !receiver.enums.is_empty())
        {
            return;
        }
        if observed.is_none()
            && (self.declared.methods.contains("[]") || self.declared.methods.contains("[]="))
        {
            return;
        }
        let TokenKind::Symbol { name, .. } = &self.tokens[self.token_at(selector.span.start)].kind
        else {
            return;
        };
        if let Some(literal) = string_literal(name) {
            self.edits.text(selector.span, literal);
        }
    }

    pub fn token_at(&self, offset: usize) -> Tok {
        parse::token_at(self.tokens, offset)
    }

    /// Whether `x.name()` and `x.name` do the same: a hash receiver may hold
    /// a function under the name, which only the parentheses call.
    fn parens_optional(&self, expr: &'a Expr, call: &'a Call) -> bool {
        let Some(receiver) = &call.receiver else {
            return true;
        };
        if let Some(kind) = self.static_kind(receiver) {
            return kind != "hash";
        }
        self.receiver_types(expr, call).is_some_and(|types| {
            !types.is_empty() && !types.any && types.hash.is_none() && !types.nil
        })
    }

    /// Whether a replacement written without parentheses calls, where the
    /// call had no arguments: `x.after()` must not become a bare `x.from_now`
    /// that reads a method, and a namespace member read bare must stay a
    /// read of one on both sides.
    fn bare_replacement_works(&self, pattern: &Pattern, call: Option<&'a Call>) -> bool {
        let Some(call) = call else {
            return true;
        };
        if call.argument_count() > 0 || call.block.is_some() {
            return true;
        }
        let Rewrite::Template(pieces) = &pattern.rewrite else {
            return true;
        };
        let text: String = pieces
            .iter()
            .map(|piece| match piece {
                TemplatePiece::Text(text) => text.as_str(),
                _ => "",
            })
            .collect();
        if text.contains('(') || text.contains(' ') {
            return true;
        }
        match &pattern.callee {
            Callee::Member => pattern.target_member().is_none_or(|member| {
                super::compat::member_without_parens(&pattern.receiver, member)
            }),
            Callee::Namespace(namespace) => {
                let Some((target_namespace, member)) = text.split_once('.') else {
                    return true;
                };
                let parenthesized = call.args.is_some();
                parenthesized
                    || (super::compat::namespace_without_parens(namespace, &pattern.name)
                        && super::compat::namespace_without_parens(target_namespace, member))
            }
            Callee::Global => true,
        }
    }

    /// Whether today's runtime calls a member written without parentheses
    /// for every receiver type observed.
    fn bare_call_works(&self, expr: &'a Expr, call: &'a Call) -> bool {
        let Some(receiver) = &call.receiver else {
            return true;
        };
        if let Some(kind) = self.static_kind(receiver) {
            return namespace_name(&kind)
                || super::compat::member_without_parens(&kind, &call.name);
        }
        let Some(types) = self.receiver_types(expr, call) else {
            return false;
        };
        let mut kinds: Vec<String> = types
            .scalars
            .iter()
            .map(|scalar| format!("{scalar:?}").to_lowercase())
            .collect();
        if types.array.is_some() {
            kinds.push("array".to_owned());
        }
        kinds
            .iter()
            .all(|kind| super::compat::member_without_parens(kind, &call.name))
    }

    /// Drops the parentheses of a call without arguments.
    fn empty_parens(&mut self, expr: &'a Expr, call: &'a Call) {
        let Some(Args {
            parens: Some((open, close)),
            items,
        }) = &call.args
        else {
            return;
        };
        if !items.is_empty() || call.name == "call" || call.scoped(self.tokens) {
            return;
        }
        if call.receiver.is_none() {
            let name = call.name.as_str();
            // A bare capitalized name reads a constant.
            if self.local(name) || name.chars().next().is_some_and(char::is_uppercase) {
                return;
            }
            // A bare name calls a function only when it takes no parameters.
            let allowed = if let Some(def) = self.declared.functions.get(name) {
                def.params.is_empty()
            } else if self.options.new_syntax {
                matches!(
                    name,
                    "now"
                        | "p"
                        | "print"
                        | "puts"
                        | "rand"
                        | "random_id"
                        | "srand"
                        | "uuid"
                        | "warn"
                )
            } else {
                matches!(name, "now" | "rand" | "uuid")
            };
            if !allowed {
                return;
            }
        } else if let Some(receiver) = &call.receiver
            && let ExprKind::Name(namespace) = &receiver.kind
            && namespace_name(namespace)
            && !self.local(namespace)
            && !self.declared.classes.contains_key(namespace.as_str())
        {
            if !namespace_member_takes_no_arguments(namespace, &call.name)
                || (!self.options.new_syntax
                    && !super::compat::namespace_without_parens(namespace, &call.name))
            {
                return;
            }
        } else if !self.parens_optional(expr, call) {
            return;
        }
        if !self.calls_returned(expr, call) {
            return;
        }
        if !self.options.new_syntax && !self.bare_call_works(expr, call) {
            return;
        }
        self.edits.text(
            Span {
                start: self.tokens[*open].start,
                end: self.tokens[*close].end,
            },
            "",
        );
    }

    /// Replaces `send(:name, ...)` and `public_send(:name, ...)` with a direct call.
    fn dispatch(&mut self, expr: &'a Expr, call: &'a Call) {
        if !matches!(call.name.as_str(), "send" | "public_send" | "respond_to?") {
            return;
        }
        let Some(receiver) = &call.receiver else {
            return;
        };
        let observed = self.receiver_types(expr, call);
        if observed.is_some_and(|types| types.any) {
            // A host object's own `send`, such as a capability's.
            return;
        }
        let offset = self.token_span(call.name_tok).start;
        if call.name == "respond_to?" {
            self.report(
                Code::Dispatch,
                offset,
                "respond_to? is removed; use case over the name or is_type?",
            );
            return;
        }
        let Some(args) = &call.args else {
            return;
        };
        let first = args.items.first();
        let symbol = first.and_then(|arg| match (&arg.kind, &arg.value.kind) {
            (ArgKind::Positional, ExprKind::Symbol) => {
                match &self.tokens[self.token_at(arg.value.span.start)].kind {
                    TokenKind::Symbol {
                        name,
                        quoted: false,
                    } => std::str::from_utf8(name).ok().map(str::to_owned),
                    _ => None,
                }
            }
            _ => None,
        });
        let dispatch_name = |name: &String| {
            method_name(name) && !matches!(name.as_str(), "send" | "public_send" | "respond_to?")
        };
        let Some(name) = symbol.filter(dispatch_name) else {
            if observed.is_some() || first.is_some_and(|arg| arg.kind == ArgKind::Positional) {
                self.report(
                    Code::Dispatch,
                    offset,
                    format!(
                        "{} with a name known only at runtime is removed; call the member, or use case over the name",
                        call.name
                    ),
                );
            }
            return;
        };
        let completed = self.facts.is_none_or(|facts| {
            facts
                .calls
                .get(&self.compiler_offset(expr), &call.name)
                .is_none_or(|(started, returned)| started == returned)
        });
        if !completed {
            self.report(
                Code::Dispatch,
                offset,
                format!(
                    "{} raised in a recorded run, and a direct call can report the error differently; call {name} directly by hand",
                    call.name
                ),
            );
            return;
        }
        if self.declared.private_methods.contains(&name) {
            self.report(
                Code::Dispatch,
                offset,
                format!(
                    "{} reaches the private method {name}; call it from inside its class",
                    call.name
                ),
            );
            return;
        }
        let rest: Vec<&Arg> = args.items.iter().skip(1).collect();
        if rest.iter().any(|arg| arg.kind != ArgKind::Positional) {
            // Dispatch passes keywords as an options hash, and a direct call does not.
            self.report(
                Code::Dispatch,
                offset,
                format!("{} with keyword or splat arguments binds them differently from a direct call; call {name} by hand", call.name),
            );
            return;
        }
        let mut pieces = vec![
            Piece::Source(receiver.span),
            Piece::Text(
                self.text(Span {
                    start: receiver.span.end,
                    end: self.tokens[call.name_tok].start,
                })
                .to_owned(),
            ),
            Piece::Text(name),
        ];
        if !rest.is_empty() {
            pieces.push(Piece::Text("(".into()));
            for (index, arg) in rest.iter().enumerate() {
                if index > 0 {
                    pieces.push(Piece::Text(", ".into()));
                }
                pieces.push(Piece::Source(arg.span));
            }
            pieces.push(Piece::Text(")".into()));
        }
        let end = match &call.block {
            Some(block) => {
                pieces.push(Piece::Text(" ".into()));
                pieces.push(Piece::Source(Span {
                    start: self.tokens[block.open].start,
                    end: self.tokens[block.close].end,
                }));
                expr.span.end
            }
            None => expr.span.end,
        };
        self.edits.replace(
            Span {
                start: expr.span.start,
                end,
            },
            pieces,
        );
    }

    fn hash_new(&mut self, expr: &'a Expr, call: &'a Call, place: Place) {
        let Some(receiver) = &call.receiver else {
            return;
        };
        if call.name != "new"
            || !matches!(&receiver.kind, ExprKind::Name(name) if name == "Hash")
            || self.local("Hash")
            || self.declared.classes.contains_key("Hash")
        {
            return;
        }
        if call.argument_count() > 0 || call.block.is_some() {
            self.report(
                Code::HashNew,
                expr.span.start,
                "Hash.new with a default is removed; write {} with a declared type and handle missing keys with fetch",
            );
            return;
        }
        // Without parentheses, `Hash.new` as a receiver is not a call.
        if place == Place::Tight && call.args.is_none() {
            return;
        }
        // `Hash.new` may read a field a script stored on the namespace.
        if self
            .facts
            .and_then(|facts| facts.results.get(&self.compiler_offset(expr), "new"))
            .is_some_and(|types| {
                types.scalars.len() + usize::from(types.array.is_some()) > 0 || types.any
            })
        {
            return;
        }
        let text = if place == Place::Tight { "({})" } else { "{}" };
        self.edits.text(expr.span, text);
    }

    fn require(&mut self, call: &'a Call) {
        if call.receiver.is_some() || call.name != "require" {
            return;
        }
        let Some(args) = &call.args else {
            return;
        };
        let offset = self.token_span(call.name_tok).start;
        for arg in &args.items {
            if matches!(arg.value.kind, ExprKind::Symbol)
                && let TokenKind::Symbol { name, .. } =
                    &self.tokens[self.token_at(arg.value.span.start)].kind
                && let Some(literal) = string_literal(name)
            {
                self.edits.text(arg.value.span, literal);
                continue;
            }
            let literal = matches!(arg.value.kind, ExprKind::Str);
            match &arg.kind {
                ArgKind::Positional | ArgKind::Keyword(_) if literal => (),
                ArgKind::Keyword(name) if name != "as" => (),
                _ => {
                    self.report(
                        Code::Require,
                        offset,
                        "require takes string literals; write the module name and alias as literals",
                    );
                    return;
                }
            }
        }
    }

    /// Lowercases builtin type names and spells `object` as `hash`, unless
    /// a check of the annotation failed in a recorded run, whose error
    /// message quotes its spelling.
    pub fn type_names_checked(&mut self, ty: &'a TypeExpr, passed: bool) {
        if passed {
            self.type_names(ty);
        } else if self.spelled_differently(ty) {
            self.report(
                Code::Rename,
                ty.span.start,
                "this annotation failed a check in a recorded run, and its error quotes the old type spelling; respell it by hand",
            );
        }
    }

    fn spelled_differently(&self, ty: &TypeExpr) -> bool {
        match &ty.kind {
            TypeKind::Named(tok, args) => {
                let written = self.token_text(*tok).trim_end_matches('?');
                let lower = written.to_ascii_lowercase();
                (parse::respelled_type(&lower) && (lower != written || lower == "object"))
                    || args.iter().any(|arg| self.spelled_differently(arg))
            }
            TypeKind::Shape(fields, _) => fields
                .iter()
                .any(|(_, field)| self.spelled_differently(field)),
            TypeKind::Union(options) | TypeKind::Tuple(options) => options
                .iter()
                .any(|option| self.spelled_differently(option)),
            TypeKind::Qualified(_) => false,
        }
    }

    /// Whether every recorded start of the function at `offset` bound its
    /// arguments, so no parameter check failed.
    pub fn bindings_passed(&self, offset: usize) -> bool {
        self.facts.is_none_or(|facts| {
            facts.starts.get(&offset).copied().unwrap_or(0)
                == facts.entered.get(&offset).copied().unwrap_or(0)
                || !facts.starts.contains_key(&offset)
        })
    }

    /// Lowercases builtin type names and spells `object` as `hash`.
    pub fn type_names(&mut self, ty: &'a TypeExpr) {
        match &ty.kind {
            TypeKind::Named(tok, args) => {
                let written = self.token_text(*tok);
                let (name, suffix) = match written.strip_suffix('?') {
                    Some(name) => (name, "?"),
                    None => (written, ""),
                };
                let lower = name.to_ascii_lowercase();
                // `name: nil` would declare a keyword default.
                if lower == "nil" && name != "nil" {
                    self.report(
                        Code::Rename,
                        self.tokens[*tok].start,
                        format!("type names are lowercase, but {name} as `nil` would read as a keyword default here; respell it by hand"),
                    );
                }
                if parse::respelled_type(&lower) && lower != "nil" {
                    let canonical = if lower == "object" { "hash" } else { &lower };
                    if canonical != name {
                        self.edits
                            .text(self.token_span(*tok), format!("{canonical}{suffix}"));
                    }
                }
                for arg in args {
                    self.type_names(arg);
                }
            }
            TypeKind::Shape(fields, _) => {
                for (_, field) in fields {
                    self.type_names(field);
                }
            }
            TypeKind::Union(options) | TypeKind::Tuple(options) => {
                for option in options {
                    self.type_names(option);
                }
            }
            TypeKind::Qualified(_) => (),
        }
    }

    // Renames -------------------------------------------------------------

    /// Whether every recorded call at this site returned. A rewrite of a call
    /// that raised could change the error it reports.
    pub fn calls_returned(&self, expr: &Expr, call: &Call) -> bool {
        self.facts.is_none_or(|facts| {
            facts
                .calls
                .get(&self.compiler_offset(expr), &call.name)
                .is_none_or(|(started, returned)| started == returned)
        })
    }

    /// The observed receiver types of a member call.
    pub fn receiver_types(&self, expr: &Expr, call: &Call) -> Option<&'a Types> {
        let facts = self.facts?;
        let offset = self.compiler_offset(expr);
        facts.receivers.get(&offset, &call.name)
    }

    /// The kinds a receiver's values had, as the rename table names them.
    fn receiver_kinds(&self, expr: &'a Expr, call: &'a Call) -> Option<Vec<String>> {
        let receiver = call.receiver.as_ref()?;
        if let Some(kind) = self.static_kind(receiver) {
            return Some(vec![kind]);
        }
        let types = self.receiver_types(expr, call)?;
        if types.is_empty() {
            return None;
        }
        let mut kinds = Vec::new();
        if types.nil {
            kinds.push("nil".to_owned());
        }
        for scalar in &types.scalars {
            kinds.push(format!("{scalar:?}").to_lowercase());
        }
        if types.array.is_some() {
            kinds.push("array".to_owned());
        }
        if types.hash.is_some() {
            kinds.push("hash".to_owned());
        }
        if types.object {
            kinds.push("object".to_owned());
        }
        for class in &types.classes {
            kinds.push(format!("class {class}"));
        }
        if !types.enums.is_empty() {
            kinds.push("enum".to_owned());
        }
        if types.any {
            kinds.push("any".to_owned());
        }
        Some(kinds)
    }

    /// A receiver kind the syntax decides: a literal, a builtin namespace,
    /// a rescued error or an annotated parameter.
    fn static_kind(&self, receiver: &Expr) -> Option<String> {
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

    /// Applies the rename table to a call; returns whether it rewrote it.
    fn rename(&mut self, expr: &'a Expr, call: &'a Call, place: Place) -> bool {
        if matches!(call.name.as_str(), "eql?" | "equal?") && call.receiver.is_some() {
            self.report(
                Code::Rename,
                self.token_span(call.name_tok).start,
                format!(
                    "{} is removed; == compares values, which differs where {} compared types or identity, so choose by hand",
                    call.name, call.name
                ),
            );
            return false;
        }
        // Dispatch by name and `Hash.new` have rules of their own.
        if matches!(call.name.as_str(), "send" | "public_send" | "respond_to?")
            || (call.name == "new"
                && matches!(call.receiver.as_ref().map(|r| &r.kind), Some(ExprKind::Name(n)) if n == "Hash"))
        {
            return false;
        }
        let candidates: Vec<&Pattern> = patterns()
            .iter()
            .filter(|pattern| pattern.name == call.name)
            .collect();
        if candidates.is_empty() {
            return false;
        }
        let offset = self.token_span(call.name_tok).start;
        if call.scoped(self.tokens) {
            return false;
        }
        if call.receiver.is_none() {
            if self.local(&call.name) || self.declared.methods.contains(&call.name) {
                return false;
            }
            let Some((pattern, captures)) = candidates
                .iter()
                .filter(|p| p.callee == Callee::Global)
                .find_map(|p| self.match_args(call, p).map(|c| (*p, c)))
            else {
                return false;
            };
            return self.apply_pattern(expr, Some(call), pattern, &captures, place);
        }
        let receiver = call.receiver.as_ref().unwrap();
        // A namespace member, such as `Time.gm`.
        if let ExprKind::Name(name) = &receiver.kind
            && namespace_name(name)
            && !self.local(name)
            && !self.declared.classes.contains_key(name.as_str())
        {
            let Some((pattern, captures)) = candidates
                .iter()
                .filter(|p| p.callee == Callee::Namespace(name.clone()))
                .find_map(|p| self.match_args(call, p).map(|c| (*p, c)))
            else {
                return false;
            };
            if !self.calls_returned(expr, call) {
                return false;
            }
            return self.apply_pattern(expr, Some(call), pattern, &captures, place);
        }
        if call.scoped(self.tokens) {
            return false;
        }
        let members: Vec<&Pattern> = candidates
            .into_iter()
            .filter(|p| p.callee == Callee::Member)
            .collect();
        if members.is_empty() {
            return false;
        }
        let choose = |kind: &str, migrator: &Self| -> Option<(&'static Pattern, Captures)> {
            patterns()
                .iter()
                .filter(|p| p.callee == Callee::Member && p.name == call.name)
                .filter(|p| p.receiver == kind || p.receiver == "T")
                .find_map(|p| migrator.match_args(call, p).map(|c| (p, c)))
        };
        // A hash field of the member's name answers the call instead. A
        // rescued error's fields are its members.
        let error = self.static_kind(receiver).as_deref() == Some("error");
        if !error
            && self
                .receiver_types(expr, call)
                .and_then(|types| types.hash.as_deref())
                .is_some_and(|shape| shape.fields.contains_key(call.name.as_bytes()))
        {
            self.report(
                Code::Receiver,
                offset,
                format!(
                    "{} reads a hash field of that name here; rewrite it by hand",
                    call.name
                ),
            );
            return false;
        }
        let kinds = self.receiver_kinds(expr, call);
        let decision = match &kinds {
            Some(kinds) if !kinds.is_empty() => {
                let mut chosen: Option<(&Pattern, Captures)> = None;
                let mut mixed = false;
                let mut unmatched = false;
                for kind in kinds {
                    let found = if kind == "any" {
                        mixed = true;
                        None
                    } else if let Some(class) = kind.strip_prefix("class ") {
                        let own = self
                            .declared
                            .classes
                            .get(class)
                            .is_some_and(|c| defines(c, &call.name));
                        if own { None } else { choose("instance", self) }
                    } else {
                        choose(kind, self)
                    };
                    match (found, &chosen) {
                        (Some(found), Some(existing)) => {
                            if !std::ptr::eq(found.0, existing.0) {
                                mixed = true;
                            }
                        }
                        (Some(found), None) => {
                            if unmatched {
                                mixed = true;
                            }
                            chosen = Some(found);
                        }
                        (None, Some(_)) => mixed = true,
                        (None, None) => unmatched = true,
                    }
                }
                if mixed {
                    if chosen.is_some() {
                        self.report(
                            Code::Receiver,
                            offset,
                            format!(
                                "{} is renamed for some of its receivers' types ({}) but not others; rewrite it by hand",
                                call.name,
                                kinds.join(", ")
                            ),
                        );
                    }
                    return false;
                }
                chosen
            }
            _ => {
                // Unobserved: rename only when every builtin type agrees and
                // nothing in this source defines the name, and not on a host
                // global, whose members the host names.
                if self.declared.methods.contains(&call.name) || self.host_rooted(receiver) {
                    return false;
                }
                let matched: Vec<(&'static Pattern, Captures)> = members
                    .iter()
                    .filter_map(|p| self.match_args(call, p).map(|c| (*p, c)))
                    .collect();
                let Some(first) = matched.first() else {
                    return false;
                };
                let same = matched
                    .iter()
                    .all(|(p, _)| template_text(&p.rewrite) == template_text(&first.0.rewrite));
                // A name some type spells canonically, such as `to_s`, is
                // most likely that; the compiler's fixes catch the rest.
                if canonical_member(&call.name, &matched) {
                    return false;
                }
                if !same {
                    let receivers: Vec<&str> =
                        matched.iter().map(|(p, _)| p.receiver.as_str()).collect();
                    self.report(
                        Code::Receiver,
                        offset,
                        format!(
                            "{} is renamed depending on its receiver's type ({}), which no recorded run observed; rewrite it by hand",
                            call.name,
                            receivers.join(", ")
                        ),
                    );
                    return false;
                }
                Some(matched.into_iter().next().unwrap())
            }
        };
        let Some((pattern, captures)) = decision else {
            return false;
        };
        if !self.calls_returned(expr, call) && matches!(pattern.rewrite, Rewrite::Template(_)) {
            self.report(
                Code::Rename,
                offset,
                format!(
                    "{} is removed, and it raised in a recorded run, where its replacement would report a different error; rewrite it by hand",
                    call.name
                ),
            );
            return false;
        }
        self.apply_pattern(expr, Some(call), pattern, &captures, place)
    }

    /// Whether an expression starts from a name the script never binds,
    /// which the host supplies, such as a capability.
    fn host_rooted(&self, expr: &Expr) -> bool {
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

    fn match_args(&self, call: &'a Call, pattern: &Pattern) -> Option<Captures> {
        let mut captures = Captures::default();
        let (positional, keywords, splats): (Vec<&Arg>, Vec<&Arg>, bool) = match &call.args {
            None => (Vec::new(), Vec::new(), false),
            Some(args) => {
                let positional = args
                    .items
                    .iter()
                    .filter(|arg| arg.kind == ArgKind::Positional)
                    .collect();
                let keywords = args
                    .items
                    .iter()
                    .filter(|arg| matches!(arg.kind, ArgKind::Keyword(_)))
                    .collect();
                let splats = args
                    .items
                    .iter()
                    .any(|arg| matches!(arg.kind, ArgKind::Splat | ArgKind::KeywordSplat));
                (positional, keywords, splats)
            }
        };
        let Some(pattern_args) = &pattern.args else {
            let empty = positional.is_empty() && keywords.is_empty() && !splats;
            return (empty && call.block.is_none()).then_some(captures);
        };
        let rest = pattern_args.contains(&ArgPattern::Rest);
        let mut next = 0;
        let mut used_keywords = HashSet::new();
        for arg in pattern_args {
            match arg {
                ArgPattern::Rest => (),
                ArgPattern::Capture(name) => {
                    let value = positional.get(next)?;
                    next += 1;
                    captures.values.insert(name.clone(), value.value.span);
                }
                ArgPattern::Symbol(symbol) => {
                    let value = positional.get(next)?;
                    next += 1;
                    if !matches!(value.value.kind, ExprKind::Symbol)
                        || self.text(value.value.span) != format!(":{symbol}")
                    {
                        return None;
                    }
                }
                ArgPattern::Keyword(name, expected) => {
                    let (index, arg) = keywords
                        .iter()
                        .enumerate()
                        .find(|(_, arg)| arg.kind == ArgKind::Keyword(name.clone()))?;
                    used_keywords.insert(index);
                    match expected {
                        KeywordValue::Literal(literal) => {
                            if self.text(arg.value.span) != literal {
                                return None;
                            }
                        }
                        KeywordValue::Capture(capture) => {
                            captures.values.insert(capture.clone(), arg.value.span);
                        }
                    }
                }
            }
        }
        let remaining: Vec<Span> = match &call.args {
            None => Vec::new(),
            Some(args) => {
                let mut positional_seen = 0;
                let mut keyword_seen = 0;
                args.items
                    .iter()
                    .filter(|arg| match arg.kind {
                        ArgKind::Positional => {
                            positional_seen += 1;
                            positional_seen > next
                        }
                        ArgKind::Keyword(_) => {
                            keyword_seen += 1;
                            !used_keywords.contains(&(keyword_seen - 1))
                        }
                        _ => true,
                    })
                    .map(|arg| arg.span)
                    .collect()
            }
        };
        if !rest && (!remaining.is_empty() || call.block.is_some()) {
            return None;
        }
        captures.rest = remaining;
        Some(captures)
    }

    /// Replaces a call with a rename's template; returns whether it did.
    pub fn apply_pattern(
        &mut self,
        expr: &'a Expr,
        call: Option<&'a Call>,
        pattern: &Pattern,
        captures: &Captures,
        place: Place,
    ) -> bool {
        let offset = call.map_or(expr.span.start, |call| self.token_span(call.name_tok).start);
        let pieces = match &pattern.rewrite {
            Rewrite::Manual(hint) => {
                self.report(
                    Code::Rename,
                    offset,
                    format!("{} is removed; {hint}", pattern.name),
                );
                return false;
            }
            Rewrite::Template(pieces) => pieces,
        };
        let receiver = call.and_then(|call| call.receiver.as_ref());
        // `time.hash` wraps where its replacement does not, float modulo
        // refuses operands `%` takes, and `h[k] = v` is only a statement.
        // `%` refuses a float operand that `modulo` takes.
        let integer_divisor = captures
            .values
            .get("divisor")
            .and_then(|span| self.expr_at(call, *span))
            .is_some_and(|divisor| matches!(divisor.kind, ExprKind::Integer));
        let exact = !(pattern.receiver == "time" && pattern.name == "hash")
            && !(pattern.name == "modulo" && (pattern.receiver == "float" || !integer_divisor))
            && (pattern.name != "store" || place == Place::Statement);
        let assignable = pattern.name != "store"
            || receiver.is_some_and(|r| match &r.kind {
                ExprKind::Name(name) => self.local(name),
                ExprKind::Ivar(_) => true,
                _ => false,
            });
        if !exact || !assignable {
            self.report(
                Code::Rename,
                offset,
                format!(
                    "{} is removed, and its replacement differs here; rewrite it by hand",
                    pattern.name
                ),
            );
            return false;
        }
        if pattern.receiver_uses() > 1 && !receiver.is_some_and(simple) {
            self.report(
                Code::Rename,
                offset,
                format!(
                    "{} is removed; its replacement repeats the receiver, so bind it to a local first",
                    pattern.name
                ),
            );
            return false;
        }
        if !self.options.new_syntax && !self.old_runtime_accepts(pattern, call) {
            return false;
        }
        if !self.bare_replacement_works(pattern, call) {
            return false;
        }
        // An index takes no block, and dropping `itself` or `freeze` from a
        // receiver would let a mutation reach the original.
        let blocked = call.is_some_and(|call| call.block.is_some())
            && pieces
                .iter()
                .any(|piece| matches!(piece, TemplatePiece::Text(text) if text.starts_with(']')));
        let identity =
            matches!(pattern.name.as_str(), "itself" | "freeze") && place == Place::Tight;
        if blocked || identity {
            self.report(
                Code::Rename,
                offset,
                format!("{} is removed, and here its replacement would behave differently; rewrite it by hand", pattern.name),
            );
            return false;
        }
        let safe = call.is_some_and(|call| call.safe(self.tokens));
        let plain_member = matches!(pieces.as_slice(), [TemplatePiece::Receiver, TemplatePiece::Text(text), ..] if text.starts_with('.'));
        if safe && !plain_member {
            // `x&.m` skips nil, which an operator replacement would not.
            return false;
        }
        if pattern.name == "is_a?" || pattern.name == "kind_of?" || pattern.name == "instance_of?" {
            let Some(span) = captures.values.get("type") else {
                return false;
            };
            let text = self.text(*span);
            if !text.chars().next().is_some_and(char::is_uppercase)
                || !text.chars().all(|c| c.is_alphanumeric() || c == '_')
            {
                self.report(
                    Code::Rename,
                    offset,
                    format!(
                        "{} is removed; use is_type? with the type's name as a symbol",
                        pattern.name
                    ),
                );
                return false;
            }
        }
        let operator = pattern.operator();
        let mut out = Vec::new();
        // An empty `...` takes the separator next to it along.
        let mut strip_separator = false;
        for piece in pieces {
            match piece {
                TemplatePiece::Text(text) => {
                    let text = match text.strip_prefix(", ") {
                        Some(rest) if strip_separator => rest,
                        _ => text,
                    };
                    strip_separator = false;
                    out.push(Piece::Text(text.to_owned()));
                }
                TemplatePiece::Receiver => {
                    let receiver = receiver.expect("a member pattern has a receiver");
                    out.push(Piece::Source(receiver.span));
                    if safe {
                        out.push(Piece::Text("&".into()));
                    }
                }
                TemplatePiece::Capture(name) => {
                    let Some(span) = captures.values.get(name) else {
                        return false;
                    };
                    let value = self.expr_at(call, *span);
                    let wrap = operator && value.is_some_and(|value| !primary(value));
                    if wrap {
                        out.push(Piece::Text("(".into()));
                    }
                    out.push(Piece::Source(*span));
                    if wrap {
                        out.push(Piece::Text(")".into()));
                    }
                }
                TemplatePiece::Rest if captures.rest.is_empty() => match out.last_mut() {
                    Some(Piece::Text(text)) if text.ends_with(", ") => {
                        text.truncate(text.len() - 2);
                    }
                    _ => strip_separator = true,
                },
                TemplatePiece::Rest => {
                    for (index, span) in captures.rest.iter().enumerate() {
                        let separated = matches!(out.last(), Some(Piece::Text(t))
                            if t.ends_with('(') || t.ends_with('[') || t.ends_with(", "));
                        if index > 0 || !separated {
                            out.push(Piece::Text(", ".into()));
                        }
                        out.push(Piece::Source(*span));
                    }
                }
            }
        }
        let mut text_only = String::new();
        let mut pieces = Vec::new();
        // Tidy `name()` left by an empty `...` into `name`, and `(, ` into `(`.
        for piece in out {
            match piece {
                Piece::Text(text) => text_only.push_str(&text),
                Piece::Source(span) => {
                    if !text_only.is_empty() {
                        pieces.push(Piece::Text(std::mem::take(&mut text_only)));
                    }
                    pieces.push(Piece::Source(span));
                }
            }
        }
        if !text_only.is_empty() {
            pieces.push(Piece::Text(text_only));
        }
        for piece in &mut pieces {
            if let Piece::Text(text) = piece {
                *text = text.replace("(, ", "(").replace("()", "");
            }
        }
        if let Some(block) = call.and_then(|call| call.block.as_ref()) {
            pieces.push(Piece::Text(" ".into()));
            pieces.push(Piece::Source(Span {
                start: self.tokens[block.open].start,
                end: self.tokens[block.close].end,
            }));
        }
        if operator && place == Place::Tight {
            pieces.insert(0, Piece::Text("(".into()));
            pieces.push(Piece::Text(")".into()));
        } else if operator {
            self.operator_rewrites.insert(expr.span);
        }
        self.edits.replace(expr.span, pieces);
        true
    }

    /// Whether today's runtime has a rename's replacement: its member for
    /// the receiver's type, and every namespace member it calls.
    fn old_runtime_accepts(&self, pattern: &Pattern, call: Option<&Call>) -> bool {
        let Rewrite::Template(pieces) = &pattern.rewrite else {
            return true;
        };
        // Replacements that today's runtime spells but runs differently.
        let differs = match (pattern.receiver.as_str(), pattern.name.as_str()) {
            // `%` refuses a float operand today, and `Time.local` takes no zone.
            (_, "modulo") | ("Time", "new") => true,
            _ => pieces
                .iter()
                .any(|piece| matches!(piece, TemplatePiece::Text(text) if text.contains("in: "))),
        };
        if differs {
            return false;
        }
        // A member written without parentheses must call on today's runtime.
        if let Some(member) = pattern.target_member()
            && call.is_some_and(|call| call.args.is_none())
            && !matches!(pieces.get(1), Some(TemplatePiece::Text(text)) if text.contains('('))
            && !super::compat::member_without_parens(&pattern.receiver, member)
        {
            return false;
        }
        let text: String = pieces
            .iter()
            .map(|piece| match piece {
                TemplatePiece::Text(text) => text.as_str(),
                _ => " ",
            })
            .collect();
        let builtins = vibescript::builtins();
        let mut rest = text.as_str();
        while let Some(start) = rest.find(|c: char| c.is_ascii_uppercase()) {
            let word: String = rest[start..]
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric())
                .collect();
            rest = &rest[start + word.len()..];
            let Some(member) = rest.strip_prefix('.') else {
                continue;
            };
            let member: String = member
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '?' | '!'))
                .collect();
            let present = builtins
                .get(&word)
                .and_then(|value| value.as_hash())
                .is_some_and(|fields| {
                    fields
                        .iter()
                        .any(|(key, _)| key.as_bytes() == Some(member.as_bytes()))
                });
            if !present {
                return false;
            }
        }
        if pattern.callee == Callee::Global
            && let Some(TemplatePiece::Text(text)) = pieces.first()
        {
            let name: String = text
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect();
            if !name.is_empty()
                && name.chars().next().is_some_and(char::is_lowercase)
                && !builtins.contains_key(&name)
            {
                return false;
            }
        }
        let Some(member) = pattern.target_member() else {
            return true;
        };
        if matches!(pattern.receiver.as_str(), "T" | "error") {
            return true;
        }
        vibescript::tooling::member_names()
            .iter()
            .find(|(kind, _)| *kind == pattern.receiver)
            .is_none_or(|(_, names)| names.contains(&member))
    }

    fn expr_at(&self, call: Option<&'a Call>, span: Span) -> Option<&'a Expr> {
        let args = call?.args.as_ref()?;
        args.items
            .iter()
            .map(|arg| &arg.value)
            .find(|value| value.span == span)
    }

    /// The offset the compiler attributes an expression to, as observation
    /// events report it.
    pub fn compiler_offset(&self, expr: &Expr) -> usize {
        match &expr.kind {
            ExprKind::Binary(op, ..) | ExprKind::Range(Some(_), op, _) => self.operator_offset(*op),
            ExprKind::Index(_, open, ..) => self.tokens[*open].start,
            ExprKind::Ternary(_, question, ..) => self.tokens[*question].start,
            ExprKind::Rescue(_, keyword, _) => self.tokens[*keyword].start,
            ExprKind::Group(_, inner, _) => self.compiler_offset(inner),
            ExprKind::Call(call) => match &call.receiver {
                Some(receiver) => self.compiler_offset(receiver),
                None => expr.span.start,
            },
            ExprKind::Computed(callee, _) | ExprKind::BlockCall(callee, _) => {
                self.compiler_offset(callee)
            }
            ExprKind::Unary(op, operand)
                if self.token_text(*op) == "-"
                    && matches!(operand.kind, ExprKind::Integer | ExprKind::Float)
                    && self.tokens[*op].end == operand.span.start =>
            {
                operand.span.start
            }
            _ => expr.span.start,
        }
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
}

/// The arguments a rename pattern captured.
#[derive(Default)]
pub(crate) struct Captures {
    pub values: HashMap<String, Span>,
    pub rest: Vec<Span>,
}

fn template_text(rewrite: &Rewrite) -> String {
    match rewrite {
        Rewrite::Template(pieces) => format!("{pieces:?}"),
        Rewrite::Manual(hint) => hint.clone(),
    }
}

/// Whether some builtin type spells `name` canonically, beside the types
/// whose patterns matched.
fn canonical_member(name: &str, matched: &[(&Pattern, Captures)]) -> bool {
    vibescript::tooling::member_names()
        .iter()
        .filter(|(kind, _)| {
            !matched
                .iter()
                .any(|(p, _)| p.receiver == *kind || p.receiver == "T")
        })
        .any(|(kind, names)| {
            names.contains(&name)
                && !matches!(*kind, "nil" | "bool")
                && !super::renames::patterns()
                    .iter()
                    .any(|p| p.receiver == *kind && p.name == name)
                && !universal(name)
        })
}

fn universal(name: &str) -> bool {
    vibescript::tooling::member_names()
        .iter()
        .all(|(_, names)| names.contains(&name))
}

fn defines(class: &Class, name: &str) -> bool {
    class.members.iter().any(|member| match member {
        Member::Def(def) => def.name == name,
        _ => false,
    })
}

/// The builtin namespaces a capitalized name can refer to.
pub(crate) fn namespace_name(name: &str) -> bool {
    matches!(
        name,
        "Time" | "Duration" | "JSON" | "Math" | "Regex" | "Regexp" | "Hash"
    )
}

fn namespace_member_takes_no_arguments(namespace: &str, member: &str) -> bool {
    let table = vibescript::signatures::table();
    let namespace = if namespace == "Regexp" {
        "Regex"
    } else {
        namespace
    };
    table.module(namespace).is_some_and(|module| {
        module.named(member).any(|member| match member {
            vibescript::signatures::Member::Function(function) => {
                function.params.iter().all(|param| {
                    param.optional || param.kind != vibescript::signatures::ParamKind::Positional
                })
            }
            _ => false,
        })
    })
}

/// An expression that is safe to repeat: a local, instance variable, `self`
/// or literal.
pub(crate) fn simple(expr: &Expr) -> bool {
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
pub(crate) fn primary(expr: &Expr) -> bool {
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

fn method_name(name: &str) -> bool {
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
pub(crate) fn string_literal(bytes: &[u8]) -> Option<String> {
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

fn symbol_literal(bytes: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(bytes).ok()?;
    if method_name(text) {
        return Some(format!(":{text}"));
    }
    string_literal(bytes).map(|quoted| format!(":{quoted}"))
}

/// Gathers the names a body binds, without entering nested functions.
pub(crate) fn collect_locals(body: &[Stmt], scope: &mut Scope<'_>) {
    for stmt in body {
        collect_stmt(stmt, scope);
    }
}

fn collect_rescued(rescued: &Rescued, scope: &mut Scope<'_>) {
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

fn collect_expr(expr: &Expr, scope: &mut Scope<'_>) {
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
            if let Some(block) = &call.block {
                collect_locals(&block.body, scope);
            }
        }
        _ => (),
    }
}
