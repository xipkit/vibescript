//! The traversal every surface walk shares: which rule runs at each
//! construct, where each expression stands, and which locals are in scope.

use super::{
    Rule,
    context::{Place, Scope, collect_expr, collect_locals, collect_rescued},
    hooks::{Annotation, Probe},
    rules::Rules,
    syntax::*,
};

/// Walks a tree, applying every canonical-surface rule and the
/// implementor's own [`Hooks`](super::Hooks) at each construct.
pub trait Walk<'a>: Rules<'a> {
    /// Walks a whole program.
    fn program(&mut self, body: &'a [Stmt]) {
        let mut scope = Scope::default();
        collect_locals(body, &mut scope);
        self.scopes.push(scope);
        self.statements(body);
        self.scopes.pop();
    }

    /// Walks each statement of a body.
    fn statements(&mut self, body: &'a [Stmt]) {
        for stmt in body {
            self.stmt(stmt);
        }
    }

    /// Walks one statement.
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

    /// Walks an assignment's targets and values.
    fn assign(&mut self, stmt: &'a Stmt, assign: &'a Assign) {
        for target in &assign.targets {
            self.target(target);
        }
        for value in &assign.values {
            self.expr(value, Place::Loose);
        }
        self.after_assign(stmt, assign);
    }

    /// Walks what an assignment target reads, such as an indexed receiver.
    fn target(&mut self, target: &'a Target) {
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
                let passed = self.annotation_passed(Annotation::BlockParameter);
                self.type_names_checked(ty, passed);
            }
            Target::Group(_, parts) => {
                for part in parts {
                    self.target(part);
                }
            }
            Target::Splat(_, None) => (),
        }
    }

    /// Walks an `if` or `unless`, rewriting `unless` as `if` with the
    /// negated condition.
    fn if_node(&mut self, node: &'a If, report: usize) {
        if node.unless {
            let (condition, _) = &node.branches[0];
            self.negated(Rule::Unless, node.keyword, "if", condition, report);
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

    /// Walks a `while` or `until`, rewriting `until` as `while` with the
    /// negated condition.
    fn while_node(&mut self, node: &'a While) {
        let report = self.tokens[node.keyword].start;
        if node.until {
            self.negated(Rule::Until, node.keyword, "while", &node.condition, report);
        } else {
            self.condition(&node.condition, report);
        }
        self.statements(&node.body);
    }

    /// Walks a statement with an `if`, `unless`, `while` or `until` modifier.
    fn modifier(&mut self, stmt: &'a Stmt, node: &'a Modifier) {
        self.stmt(&node.body);
        let report = stmt.span.start;
        match node.kind {
            ModifierKind::If | ModifierKind::While => self.condition(&node.condition, report),
            ModifierKind::Unless => {
                self.negated(Rule::Unless, node.keyword, "if", &node.condition, report);
            }
            ModifierKind::Until => {
                self.negated(Rule::Until, node.keyword, "while", &node.condition, report);
            }
        }
    }

    /// Replaces the `unless` or `until` keyword `keyword` with `positive`
    /// and negates its condition, as one rewrite.
    fn negated(
        &mut self,
        rule: Rule,
        keyword: Tok,
        positive: &str,
        condition: &'a Expr,
        report: usize,
    ) {
        let span = self.token_span(keyword);
        let removed = self.token_text(keyword);
        let advice = format!("write `{positive}` with the negated condition");
        let previous = self.enter(rule, span, removed, advice);
        let group = self.rewrites.len() - 1;
        self.edits.text(span, positive);
        self.leave(previous);
        let outer = self.negation.replace(group);
        self.condition_with(condition, report, true);
        self.negation = outer;
    }

    /// Walks a function: its parameters' annotations and defaults, then
    /// its body.
    fn def(&mut self, def: &'a Def, class: Option<&'a Class>) {
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
        self.keyword_params(def);
        let bound = self.annotation_passed(Annotation::Parameter(def));
        for param in &def.params {
            if let Some(ty) = &param.ty {
                self.type_names_checked(ty, bound);
            }
            if let Some(default) = &param.default {
                self.expr(default, Place::Loose);
            }
        }
        if let Some((_, ty)) = &def.result {
            let returned = self.annotation_passed(Annotation::Result(def, ty));
            self.type_names_checked(ty, returned);
        }
        self.before_body(def, class);
        self.statements(&def.body);
        if let Some(rescued) = &def.rescue {
            self.rescued(rescued);
        }
        self.scopes.pop();
    }

    /// Walks rescue, else and ensure clauses.
    fn rescued(&mut self, rescued: &'a Rescued) {
        for clause in &rescued.rescues {
            self.statements(&clause.body);
        }
        for body in rescued.alternate.iter().chain(&rescued.ensure) {
            self.statements(body);
        }
    }

    /// Walks a class or module and its members; `prefix` is the dotted name
    /// of the enclosing one.
    fn class(&mut self, class: &'a Class, prefix: &str) {
        let name = if prefix.is_empty() {
            class.name.clone()
        } else {
            format!("{prefix}.{}", class.name)
        };
        self.before_class(class, &name);
        for member in &class.members {
            match member {
                Member::Def(def) => self.def(def, Some(class)),
                Member::Property(property) => {
                    for (tok, ty) in &property.names {
                        if let Some(ty) = ty {
                            let passed =
                                self.annotation_passed(Annotation::Property(&name, *tok, ty));
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

    /// Walks an expression standing at `place`.
    fn expr(&mut self, expr: &'a Expr, place: Place) {
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
            ExprKind::Binary(op, left, right) => {
                self.expr(left, Place::Tight);
                self.expr(right, Place::Tight);
                self.after_binary(expr, *op, left, right, place);
            }
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

    /// `!x` outside a condition.
    fn unary(&mut self, expr: &'a Expr, op: Tok, operand: &'a Expr) {
        self.expr(operand, Place::Tight);
        if self.token_text(op) != "!" {
            return;
        }
        let test = self.test(operand, Probe::Negation(self.tokens[op].start));
        self.negation(expr, operand, test);
    }

    /// Walks a `case`.
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
        self.after_case(node);
    }

    /// Walks a call's arguments.
    fn args(&mut self, args: &'a Args) {
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

    /// Walks a named call, then applies the call rules to it.
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
                let group = self.words.get(&first.value.span).copied();
                let previous = self.edits.enter(group);
                self.edits.wrap(first.value.span, "(", ")");
                self.edits.enter(previous);
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

    /// Walks a block, then converts it to braces when written `do ... end`.
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
        self.braces(block, owner, callee_end);
    }

    /// Walks a condition and makes it test a `bool`.
    fn condition(&mut self, expr: &'a Expr, report: usize) {
        self.condition_with(expr, report, false);
    }

    /// Walks a condition, making it test a `bool` and, for `unless` and
    /// `until`, negating it.
    fn condition_with(&mut self, expr: &'a Expr, report: usize, negate: bool) {
        match &expr.kind {
            ExprKind::Group(_, inner, _) if !negate => {
                self.condition_with(inner, report, false);
                return;
            }
            ExprKind::Binary(op, left, right) if matches!(self.token_text(*op), "&&" | "||") => {
                let inner = self.operator_offset(*op);
                self.condition_with(left, inner, false);
                if negate {
                    // The compiler negates the whole test, so the negation
                    // saw the right operand's values, or the left's.
                    let test = self.test(right, Probe::Negation(inner));
                    self.condition_typed(right, test);
                    let group = self.negation;
                    let previous = self.edits.enter(group);
                    self.edits.wrap(expr.span, "!(", ")");
                    self.edits.enter(previous);
                } else {
                    self.condition_with(right, report, false);
                }
                return;
            }
            ExprKind::Unary(op, operand) if self.token_text(*op) == "!" => {
                let test = self.test(operand, Probe::Negation(self.tokens[*op].start));
                if negate {
                    // `unless !x` tests `x`.
                    let span = Span {
                        start: self.tokens[*op].start,
                        end: operand.span.start,
                    };
                    let group = self.negation;
                    let previous = self.edits.enter(group);
                    self.edits.text(span, "");
                    self.edits.enter(previous);
                    self.condition_typed(operand, test);
                } else {
                    self.expr(operand, Place::Tight);
                    self.negation(expr, operand, test);
                }
                return;
            }
            _ => (),
        }
        // For `unless` and `until`, the compiler tests the negation, and the
        // negated value is what `!` saw.
        let probe = if negate {
            Probe::Negation(self.compiler_offset(expr))
        } else {
            Probe::Condition(report)
        };
        let test = self.test(expr, probe);
        self.expr(expr, Place::Loose);
        self.apply_test(expr, test, negate);
    }

    /// Walks a condition whose test is already known.
    fn condition_typed(&mut self, expr: &'a Expr, test: super::hooks::Test) {
        self.expr(expr, Place::Loose);
        self.apply_test(expr, test, false);
    }
}

impl<'a, T: Rules<'a> + ?Sized> Walk<'a> for T {}
