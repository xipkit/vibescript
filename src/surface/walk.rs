//! The traversal every surface walk shares: which rule runs at each
//! construct, where each expression stands, and which locals are in scope.

use super::{
    Access, Rule,
    checker::Checker,
    context::{Place, Scope, collect_expr, collect_locals, collect_rescued, namespace_name},
    syntax::*,
};

/// Walks a tree, applying every canonical-surface rule at each construct.
impl<'a> Checker<'a> {
    /// Walks a whole program.
    pub(super) fn program(&mut self, body: &'a [Stmt]) {
        let mut scope = Scope::default();
        collect_locals(body, &mut scope, &mut || self.halt());
        self.scopes.push(scope);
        self.statements(body);
        self.scopes.pop();
    }

    /// Walks each statement of a body.
    pub(super) fn statements(&mut self, body: &'a [Stmt]) {
        for stmt in body {
            self.stmt(stmt);
        }
    }

    /// Walks one statement.
    pub(super) fn stmt(&mut self, stmt: &'a Stmt) {
        if self.halt() {
            return;
        }
        match &stmt.kind {
            StmtKind::Expr(expr) => self.expr(expr, Place::Statement),
            StmtKind::Assign(assign) => self.assign(assign),
            StmtKind::If(node) => self.if_node(node),
            StmtKind::While(node) => self.while_node(node),
            StmtKind::For(node) => {
                self.target(&node.target);
                self.expr(&node.iterable, Place::Loose);
                self.statements(&node.body);
            }
            StmtKind::Modifier(node) => self.modifier(node),
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
    pub(super) fn assign(&mut self, assign: &'a Assign) {
        let access = match (assign.targets.len(), self.token_text(assign.op)) {
            (1, "=") => Access::Write,
            (1, _) => Access::Update,
            _ => Access::Destructure,
        };
        // Each target, and each value, is a step the walk counts, as each
        // list it goes over is.
        for target in &assign.targets {
            if self.halt() {
                return;
            }
            self.target_with(target, access);
        }
        for value in &assign.values {
            self.expr(value, Place::Loose);
        }
    }

    /// Walks what a destructured target reads, such as an indexed receiver.
    pub(super) fn target(&mut self, target: &'a Target) {
        self.target_with(target, Access::Destructure);
    }

    /// Walks what an assignment target reads, where a field it names with a
    /// dot is reached as `access` says.
    pub(super) fn target_with(&mut self, target: &'a Target, access: Access) {
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
                        self.walk_receiver(receiver);
                    }
                    self.field_access(expr, call, access);
                }
                _ => (),
            },
            Target::Splat(_, Some(inner)) => self.target(inner),
            Target::Typed(inner, ty) => {
                self.target(inner);
                self.type_names(ty);
            }
            Target::Group(_, parts) => {
                for part in parts {
                    if self.halt() {
                        return;
                    }
                    self.target(part);
                }
            }
            Target::Splat(_, None) => (),
        }
    }

    /// Walks an `if` or `unless`, rewriting `unless` as `if` with the
    /// negated condition.
    pub(super) fn if_node(&mut self, node: &'a If) {
        if node.unless {
            let (condition, _) = &node.branches[0];
            self.negated(Rule::Unless, node.keyword, "if", condition);
        } else {
            for (condition, _) in &node.branches {
                self.condition(condition);
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
    pub(super) fn while_node(&mut self, node: &'a While) {
        if node.until {
            self.negated(Rule::Until, node.keyword, "while", &node.condition);
        } else {
            self.condition(&node.condition);
        }
        self.statements(&node.body);
    }

    /// Walks a statement with an `if`, `unless`, `while` or `until` modifier.
    pub(super) fn modifier(&mut self, node: &'a Modifier) {
        self.stmt(&node.body);
        match node.kind {
            ModifierKind::If | ModifierKind::While => self.condition(&node.condition),
            ModifierKind::Unless => {
                self.negated(Rule::Unless, node.keyword, "if", &node.condition);
            }
            ModifierKind::Until => {
                self.negated(Rule::Until, node.keyword, "while", &node.condition);
            }
        }
    }

    /// Replaces the `unless` or `until` keyword `keyword` with `positive`
    /// and negates its condition, as one rewrite.
    pub(super) fn negated(
        &mut self,
        rule: Rule,
        keyword: Tok,
        positive: &str,
        condition: &'a Expr,
    ) {
        let span = self.token_span(keyword);
        let removed = self.token_text(keyword);
        let advice = format!("write `{positive}` with the negated condition");
        let previous = self.enter(rule, span, removed, advice);
        let group = self.rewrites.len() - 1;
        self.edits.text(span, positive);
        self.leave(previous);
        let outer = self.negation.replace(group);
        self.condition_with(condition, true);
        self.negation = outer;
    }

    /// Walks a function: its parameters' annotations and defaults, then
    /// its body.
    pub(super) fn def(&mut self, def: &'a Def, class: Option<&'a Class>) {
        if self.halt() {
            return;
        }
        let mut scope = Scope {
            def: Some(def),
            class,
            ..Scope::default()
        };
        // Each parameter is a step the pass charged up front, and the walk
        // asks now and then whether the compilation has stopped.
        for param in &def.params {
            if self.halt() {
                return;
            }
            scope.locals.insert(param.name.clone());
            if let Some(default) = &param.default {
                collect_expr(default, &mut scope, &mut || self.halt());
            }
        }
        collect_locals(&def.body, &mut scope, &mut || self.halt());
        if let Some(rescued) = &def.rescue {
            collect_rescued(rescued, &mut scope, &mut || self.halt());
        }
        self.scopes.push(scope);
        self.keyword_params(def);
        for param in &def.params {
            if self.halt() {
                return;
            }
            if let Some(ty) = &param.ty {
                self.type_names(ty);
            }
            if let Some(default) = &param.default {
                self.expr(default, Place::Loose);
            }
        }
        if let Some((_, ty)) = &def.result {
            self.type_names(ty);
        }
        self.statements(&def.body);
        if let Some(rescued) = &def.rescue {
            self.rescued(rescued);
        }
        self.scopes.pop();
    }

    /// Walks rescue, else and ensure clauses.
    pub(super) fn rescued(&mut self, rescued: &'a Rescued) {
        for clause in &rescued.rescues {
            self.statements(&clause.body);
        }
        for body in rescued.alternate.iter().chain(&rescued.ensure) {
            self.statements(body);
        }
    }

    /// Walks a class or module and its members; `prefix` is the dotted name
    /// of the enclosing one.
    pub(super) fn class(&mut self, class: &'a Class, prefix: &str) {
        let name = if prefix.is_empty() {
            class.name.clone()
        } else {
            format!("{prefix}.{}", class.name)
        };
        for member in &class.members {
            if self.halt() {
                return;
            }
            match member {
                Member::Def(def) => self.def(def, Some(class)),
                Member::Property(property) => {
                    for ty in property.names.iter().filter_map(|(_, ty)| ty.as_ref()) {
                        if self.halt() {
                            return;
                        }
                        self.type_names(ty);
                    }
                }
                Member::Class(inner) => self.class(inner, &name),
                Member::Ivar(_, _, default) => {
                    if let Some(default) = default {
                        self.expr(default, Place::Loose);
                    }
                }
                Member::ClassVar(_, _, value) => self.expr(value, Place::Loose),
                Member::Stmt(stmt) => {
                    let mut scope = Scope {
                        class: Some(class),
                        ..Scope::default()
                    };
                    collect_locals(std::slice::from_ref(stmt), &mut scope, &mut || self.halt());
                    self.scopes.push(scope);
                    self.stmt(stmt);
                    self.scopes.pop();
                }
                Member::Other(_) => (),
            }
        }
    }

    /// Walks an expression standing at `place`.
    pub(super) fn expr(&mut self, expr: &'a Expr, place: Place) {
        if self.halt() {
            return;
        }
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
                    if self.halt() {
                        return;
                    }
                    if !entry.shorthand {
                        self.expr(&entry.value, Place::Loose);
                    }
                }
            }
            ExprKind::Unary(_, operand) => self.expr(operand, Place::Tight),
            ExprKind::Binary(_, left, right) => {
                self.expr(left, Place::Tight);
                self.expr(right, Place::Tight);
            }
            ExprKind::Range(left, _, right) => {
                for operand in left.iter().chain(right) {
                    self.expr(operand, Place::Tight);
                }
            }
            ExprKind::Ternary(condition, _, yes, no) => {
                self.condition(condition);
                self.expr(yes, Place::Loose);
                self.expr(no, Place::Loose);
            }
            ExprKind::Call(call) => self.call(expr, call, place),
            ExprKind::Computed(callee, args) => {
                self.expr(callee, Place::Tight);
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
            ExprKind::If(node) => self.if_node(node),
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

    /// Walks a `case`.
    pub(super) fn case(&mut self, node: &'a Case) {
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
    }

    /// Walks a call's arguments.
    pub(super) fn args(&mut self, args: &'a Args) {
        let command = args.parens.is_none();
        for (index, arg) in args.items.iter().enumerate() {
            if self.halt() {
                return;
            }
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
    pub(super) fn call(&mut self, expr: &'a Expr, call: &'a Call, place: Place) {
        if let Some(receiver) = &call.receiver {
            self.walk_receiver(receiver);
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
        if self.field_access(expr, call, Access::Read) || self.rename(expr, call, place) {
            return;
        }
        self.empty_parens(expr, call);
        self.scoped_call(call);
        self.dispatch(expr, call);
        self.hash_new(expr, call, place);
        self.require(call);
    }

    /// Walks a call's receiver. A builtin namespace, such as `Regexp` in
    /// `Regexp.new`, is left to the rules for its member.
    pub(super) fn walk_receiver(&mut self, receiver: &'a Expr) {
        if let ExprKind::Name(name) = &receiver.kind
            && namespace_name(name)
        {
            return;
        }
        self.expr(receiver, Place::Tight);
    }

    /// Walks a block, then converts it to braces when written `do ... end`.
    pub(super) fn block(&mut self, block: &'a Block, owner: Option<&'a Call>, callee_end: usize) {
        let mut scope = Scope {
            def: self.scope().def,
            class: self.scope().class,
            block: true,
            ..Scope::default()
        };
        for param in &block.params {
            param.names(&mut |name, _| {
                if !self.halt() {
                    scope.locals.insert(name.to_owned());
                }
            });
        }
        collect_locals(&block.body, &mut scope, &mut || self.halt());
        self.scopes.push(scope);
        for param in &block.params {
            if self.halt() {
                return;
            }
            self.target(param);
        }
        self.statements(&block.body);
        self.scopes.pop();
        self.braces(block, owner, callee_end);
    }

    /// Walks a condition.
    pub(super) fn condition(&mut self, expr: &'a Expr) {
        self.condition_with(expr, false);
    }

    /// Walks a condition, negating it for `unless` and `until`.
    pub(super) fn condition_with(&mut self, expr: &'a Expr, negate: bool) {
        match &expr.kind {
            ExprKind::Group(_, inner, _) if !negate => self.condition_with(inner, false),
            ExprKind::Binary(op, left, right) if matches!(self.token_text(*op), "&&" | "||") => {
                self.condition_with(left, false);
                if negate {
                    self.expr(right, Place::Loose);
                    let group = self.negation;
                    let previous = self.edits.enter(group);
                    self.edits.wrap(expr.span, "!(", ")");
                    self.edits.enter(previous);
                } else {
                    self.condition_with(right, false);
                }
            }
            ExprKind::Unary(op, operand) if self.token_text(*op) == "!" => {
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
                    self.expr(operand, Place::Loose);
                } else {
                    self.expr(operand, Place::Tight);
                }
            }
            _ => {
                self.expr(expr, Place::Loose);
                if negate {
                    self.negate_condition(expr);
                }
            }
        }
    }
}
