//! Rewrites whose meaning depends on observed types: conditions on values
//! that are not `bool`, integer division, and `case` over enums.

use super::{
    Code,
    migrator::{Migrator, Place, simple},
    rewrite::Piece,
    syntax::*,
    types::Types,
};

/// What a condition needs so it tests a `bool`.
enum Test {
    /// It is a `bool` already, or nothing observed it and its syntax is boolean.
    Bool,
    /// It is `nil` or a value that is never `false`: test `!= nil`.
    Present,
    /// It is `nil`, `true` or `false`: test `== true`.
    True,
    /// Something else, or unknown.
    Unknown(String),
}

impl<'a> Migrator<'a> {
    /// The types a condition's value had where `report` tested it. `&&` and
    /// `||` test their left operand's value from the operator itself.
    fn tested(&self, expr: &Expr, report: usize) -> Option<Types> {
        let entries = self.conditions.get(&report)?;
        let mut types = Types::default();
        let mut found = false;
        let operator = self.short_circuits.contains(&report);
        for (origin, observed) in entries {
            let inside =
                *origin >= expr.span.start && *origin < expr.span.end.max(expr.span.start + 1);
            if inside || (operator && *origin == report) {
                types.join(observed);
                found = true;
            }
        }
        found.then_some(types)
    }

    /// Whether an expression's value is a `bool` by its syntax alone.
    fn boolean_syntax(&self, expr: &Expr) -> bool {
        match &expr.kind {
            ExprKind::True | ExprKind::False => true,
            ExprKind::Group(_, inner, _) => self.boolean_syntax(inner),
            ExprKind::Unary(op, _) => self.token_text(*op) == "!",
            ExprKind::Binary(op, left, right) => match self.token_text(*op) {
                "==" | "!=" | "<" | "<=" | ">" | ">=" | "===" | "=~" | "!~" => true,
                "&&" | "||" => self.boolean_syntax(left) && self.boolean_syntax(right),
                _ => false,
            },
            ExprKind::Call(call) => {
                call.name.ends_with('?') && call.name != "nonzero?" && builtin_predicate(&call.name)
            }
            _ => false,
        }
    }

    fn classify(&self, expr: &Expr, types: Option<Types>) -> Test {
        if self.boolean_syntax(expr) {
            return Test::Bool;
        }
        let Some(types) = types else {
            return Test::Unknown("no recorded run reached it".to_owned());
        };
        if types.only_bool() {
            return Test::Bool;
        }
        let bool_seen = types.can_be_false();
        let other = types.any
            || types.array.is_some()
            || types.hash.is_some()
            || !types.classes.is_empty()
            || !types.enums.is_empty()
            || types
                .scalars
                .iter()
                .any(|s| *s != super::types::Scalar::Bool);
        if !bool_seen {
            return Test::Present;
        }
        if !other {
            return Test::True;
        }
        Test::Unknown("it is sometimes a bool and sometimes another value".to_owned())
    }

    /// Makes a condition test a `bool`.
    pub fn condition(&mut self, expr: &'a Expr, report: usize) {
        self.condition_with(expr, report, false);
    }

    /// Negates a condition for `unless` and `until`, making it test a `bool`.
    pub fn negated_condition(&mut self, expr: &'a Expr, report: usize) {
        self.condition_with(expr, report, true);
    }

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
                    let types = self.unary_types(inner);
                    self.condition_typed(right, types);
                    self.edits.wrap(expr.span, "!(", ")");
                } else {
                    self.condition_with(right, report, false);
                }
                return;
            }
            ExprKind::Unary(op, operand) if self.token_text(*op) == "!" => {
                let types = self.unary_types(self.tokens[*op].start);
                if negate {
                    // `unless !x` tests `x`.
                    self.edits.text(
                        Span {
                            start: self.tokens[*op].start,
                            end: operand.span.start,
                        },
                        "",
                    );
                    self.condition_typed(operand, types);
                } else {
                    self.expr(operand, Place::Tight);
                    self.negation(expr, operand, types);
                }
                return;
            }
            _ => (),
        }
        // For `unless` and `until`, the compiler tests the negation, and the
        // negated value is what `!` saw.
        let types = if negate {
            self.unary_types(self.compiler_offset(expr))
        } else {
            self.tested(expr, report)
        };
        self.expr(expr, Place::Loose);
        self.apply_test(expr, types, negate);
    }

    fn unary_types(&self, offset: usize) -> Option<Types> {
        self.facts
            .and_then(|facts| facts.unaries.get(&offset).cloned())
    }

    fn condition_typed(&mut self, expr: &'a Expr, types: Option<Types>) {
        self.expr(expr, Place::Loose);
        self.apply_test(expr, types, false);
    }

    fn apply_test(&mut self, expr: &'a Expr, types: Option<Types>, negate: bool) {
        let test = self.classify(expr, types);
        let tight = !self.primary(expr);
        match (test, negate) {
            (Test::Bool, false) => (),
            (Test::Bool, true) => self.negate_bool(expr),
            (Test::Present, false) => self.compare(expr, tight, " != nil"),
            (Test::Present, true) => self.compare(expr, tight, " == nil"),
            (Test::True, false) => self.compare(expr, tight, " == true"),
            (Test::True, true) => self.compare(expr, tight, " != true"),
            (Test::Unknown(why), _) => {
                self.report(
                    Code::Condition,
                    expr.span.start,
                    format!("this condition must be a bool, and {why}; compare it explicitly"),
                );
                if negate {
                    self.negate_bool(expr);
                }
            }
        }
    }

    /// Whether an expression binds tighter than any binary operator after
    /// the rewrites made to it.
    fn primary(&self, expr: &Expr) -> bool {
        super::migrator::primary(expr) && !self.operator_rewrites.contains(&expr.span)
    }

    /// `!x` on a value that is not a bool.
    fn negation(&mut self, expr: &'a Expr, operand: &'a Expr, types: Option<Types>) {
        let test = self.classify(operand, types);
        let tight = !self.primary(operand);
        let replace = |migrator: &mut Self, suffix: &str| {
            let mut pieces = Vec::new();
            if tight {
                pieces.push(Piece::Text("(".into()));
            }
            pieces.push(Piece::Source(operand.span));
            if tight {
                pieces.push(Piece::Text(")".into()));
            }
            pieces.push(Piece::Text(suffix.into()));
            migrator.edits.replace(expr.span, pieces);
        };
        match test {
            Test::Bool => (),
            Test::Present => replace(self, " == nil"),
            Test::True => replace(self, " != true"),
            Test::Unknown(why) => self.report(
                Code::Condition,
                expr.span.start,
                format!("! takes a bool, and {why}; compare the value explicitly"),
            ),
        }
    }

    fn compare(&mut self, expr: &'a Expr, tight: bool, suffix: &str) {
        // `x != nil? 1 : 2` would lex `nil?` as a name.
        let glued = self.source[expr.span.end..]
            .chars()
            .next()
            .is_some_and(|c| c.is_alphanumeric() || matches!(c, '_' | '?' | '!'));
        let suffix = if glued {
            format!("{suffix} ")
        } else {
            suffix.to_owned()
        };
        if tight {
            self.edits.wrap(expr.span, "(", &format!("){suffix}"));
        } else {
            self.edits.wrap(expr.span, "", &suffix);
        }
    }

    /// Negates a boolean condition: flips a comparison, drops a `!`, or adds one.
    fn negate_bool(&mut self, expr: &'a Expr) {
        if let ExprKind::Call(call) = &expr.kind
            && call.name == "nil?"
            && self.operator_rewrites.contains(&expr.span)
            && let Some(receiver) = &call.receiver
        {
            self.edits.replace(
                expr.span,
                vec![Piece::Source(receiver.span), Piece::Text(" != nil".into())],
            );
            return;
        }
        match &expr.kind {
            ExprKind::Binary(op, ..) if matches!(self.token_text(*op), "==" | "!=") => {
                let flipped = if self.token_text(*op) == "==" {
                    "!="
                } else {
                    "=="
                };
                // `i==3` flipped as `i!=3` would lex `i!` as a name.
                let start = self.tokens[*op].start;
                let glued = self.source[..start]
                    .chars()
                    .next_back()
                    .is_some_and(|c| c.is_alphanumeric() || matches!(c, '_' | '?' | '!'));
                let text = if glued {
                    format!(" {flipped}")
                } else {
                    flipped.to_owned()
                };
                self.edits.text(self.token_span(*op), text);
            }
            ExprKind::Unary(op, operand)
                if self.token_text(*op) == "!" && self.primary(operand) =>
            {
                self.edits.text(
                    Span {
                        start: self.tokens[*op].start,
                        end: operand.span.start,
                    },
                    "",
                );
            }
            _ if self.primary(expr) => self.edits.insert(expr.span.start, "!"),
            _ => self.edits.wrap(expr.span, "!(", ")"),
        }
    }

    /// `!x` outside a condition.
    pub fn unary(&mut self, expr: &'a Expr, op: Tok, operand: &'a Expr) {
        self.expr(operand, Place::Tight);
        if self.token_text(op) != "!" {
            return;
        }
        let types = self
            .facts
            .and_then(|facts| facts.unaries.get(&self.tokens[op].start).cloned());
        self.negation(expr, operand, types);
    }

    /// A binary operator outside a condition: division, and `&&` or `||` on
    /// values.
    pub fn binary(
        &mut self,
        expr: &'a Expr,
        op: Tok,
        left: &'a Expr,
        right: &'a Expr,
        place: Place,
    ) {
        let text = self.token_text(op);
        match text {
            "&&" | "||" => {
                self.expr(left, Place::Tight);
                self.expr(right, Place::Tight);
                self.logical_value(expr, op, left, right, place);
            }
            "/" => {
                self.expr(left, Place::Tight);
                self.expr(right, Place::Tight);
                self.division(op, left, right);
            }
            _ => {
                self.expr(left, Place::Tight);
                self.expr(right, Place::Tight);
            }
        }
    }

    /// `a || b` and `a && b` used for their value.
    fn logical_value(
        &mut self,
        expr: &'a Expr,
        op: Tok,
        left: &'a Expr,
        right: &'a Expr,
        place: Place,
    ) {
        let report = self.operator_offset(op);
        let types = self.tested(left, report);
        let test = self.classify(left, types);
        let or = self.token_text(op) == "||";
        match test {
            Test::Bool => (),
            Test::Present if or && simple(left) => {
                // A default for an optional value is a conditional.
                let mut pieces = vec![
                    Piece::Source(left.span),
                    Piece::Text(" != nil ? ".into()),
                    Piece::Source(left.span),
                    Piece::Text(" : ".into()),
                    Piece::Source(right.span),
                ];
                if place == Place::Tight {
                    pieces.insert(0, Piece::Text("(".into()));
                    pieces.push(Piece::Text(")".into()));
                }
                self.edits.replace(expr.span, pieces);
            }
            Test::Present | Test::True | Test::Unknown(_) => {
                let _ = right;
                self.report(
                    Code::Condition,
                    expr.span.start,
                    format!(
                        "{} takes bools; write the default as a conditional, such as `x != nil ? x : y`",
                        if or { "||" } else { "&&" }
                    ),
                );
            }
        }
    }

    /// `x ||= v` and `x &&= v` on values that are not bools.
    pub fn logical_assignment(&mut self, stmt: &'a Stmt, assign: &'a Assign) {
        let op = self.token_text(assign.op);
        if !matches!(op, "||=" | "&&=") {
            return;
        }
        let [Target::Expr(target)] = assign.targets.as_slice() else {
            return;
        };
        let report = stmt.span.start;
        let types = self.tested(
            &Expr {
                span: stmt.span,
                kind: ExprKind::Nil,
            },
            report,
        );
        let test = self.classify(target, types);
        match test {
            Test::Bool => (),
            Test::Present
                if op == "||="
                    && assign.values.len() == 1
                    && self.readable_before(target, stmt) =>
            {
                let target_text = self.text(target.span).to_owned();
                let pieces = vec![
                    Piece::Text(format!("{target_text} = ")),
                    Piece::Source(assign.values[0].span),
                    Piece::Text(format!(" if {target_text} == nil")),
                ];
                self.edits.replace(stmt.span, pieces);
                // The statement's value was the variable's.
                if self.last_statement(stmt) {
                    let indent = self.indentation(stmt.span.start);
                    self.edits
                        .insert(stmt.span.end, format!("\n{indent}{target_text}"));
                }
            }
            _ => self.report(
                Code::Condition,
                stmt.span.start,
                format!("{op} tests a value that is not a bool; assign under an explicit nil test"),
            ),
        }
    }

    /// Whether a target reads as nil rather than failing before it is first
    /// assigned: an instance variable, or a local assigned earlier.
    fn readable_before(&self, target: &Expr, stmt: &Stmt) -> bool {
        match &target.kind {
            ExprKind::Ivar(name) => !name.starts_with("@@"),
            ExprKind::Name(name) => {
                !name.chars().next().is_some_and(char::is_uppercase)
                    && self
                        .scope()
                        .def
                        .map_or(0, |def| self.tokens[def.keyword].start)
                        < stmt.span.start
                    && self.assigned_before(name, stmt.span.start)
            }
            _ => false,
        }
    }

    /// Whether the local `name` is a parameter or assigned before `offset`
    /// in the current function.
    fn assigned_before(&self, name: &str, offset: usize) -> bool {
        let scope = self.scope();
        if scope
            .def
            .is_some_and(|def| def.params.iter().any(|param| param.name == name))
        {
            return true;
        }
        let start = scope.def.map_or(0, |def| self.tokens[def.keyword].start);
        let first = self.token_at(start);
        let last = self.token_at(offset);
        (first..last).any(|index| {
            self.tokens[index].kind == vibescript::tooling::TokenKind::Word
                && self.token_text(index) == name
                && matches!(
                    self.tokens.get(index + 1).map(|t| &t.kind),
                    Some(vibescript::tooling::TokenKind::Operator(
                        "=" | "||=" | "+=" | "-="
                    ))
                )
        })
    }

    /// Whether `stmt` ends a body, where its value may be used.
    fn last_statement(&self, stmt: &Stmt) -> bool {
        let rest = &self.source[stmt.span.end..];
        let next = rest.trim_start_matches([' ', '\t', '\r', '\n', ';']);
        next.is_empty()
            || next.starts_with("end")
            || next.starts_with('}')
            || next.starts_with("else")
            || next.starts_with("rescue")
            || next.starts_with("ensure")
            || next.starts_with("elsif")
            || next.starts_with("when")
    }

    pub fn indentation(&self, offset: usize) -> String {
        let line_start = self.source[..offset].rfind('\n').map_or(0, |i| i + 1);
        self.source[line_start..]
            .chars()
            .take_while(|c| *c == ' ' || *c == '\t')
            .collect()
    }

    /// Integer `/` becomes `//`; mixed or unknown operands are reported.
    fn division(&mut self, op: Tok, left: &'a Expr, right: &'a Expr) {
        let offset = self.tokens[op].start;
        let observed = self.facts.and_then(|facts| facts.binaries.get(&offset));
        let integers = match observed {
            Some(binary) if binary.integers > 0 && binary.others == 0 => true,
            Some(binary) if binary.integers == 0 => false,
            Some(_) => {
                self.report(
                    Code::Division,
                    offset,
                    "/ divided integers in some runs and other numbers in others; use // where both are integers",
                );
                return;
            }
            None => {
                let int = |e: &Expr| matches!(e.kind, ExprKind::Integer);
                let float = |e: &Expr| matches!(e.kind, ExprKind::Float);
                if int(left) && int(right) {
                    true
                } else if float(left) || float(right) {
                    false
                } else {
                    self.report(
                        Code::Division,
                        offset,
                        "/ now divides integers exactly; no recorded run reached this one, so use // if both operands are integers",
                    );
                    return;
                }
            }
        };
        if !integers {
            return;
        }
        if self.options.new_syntax {
            self.edits.text(self.token_span(op), "//");
        } else {
            self.report(
                Code::Syntax,
                offset,
                "integer / becomes //, which needs the new syntax",
            );
        }
    }

    /// Reports a `case` over an enum that misses members and has no `else`.
    pub fn enum_case(&mut self, node: &'a Case) {
        if node.alternate.is_some() {
            return;
        }
        let offset = self.tokens[node.keyword].start;
        let Some(types) = self.facts.and_then(|facts| facts.cases.get(&offset)) else {
            return;
        };
        let [name] = types.enums.iter().collect::<Vec<_>>()[..] else {
            if types.only_bool() {
                let mut named = std::collections::BTreeSet::new();
                for when in &node.whens {
                    for (value, _) in &when.values {
                        match value.kind {
                            ExprKind::True => {
                                named.insert("true");
                            }
                            ExprKind::False => {
                                named.insert("false");
                            }
                            _ => (),
                        }
                    }
                }
                if named.len() < 2 {
                    self.report(
                        Code::Case,
                        offset,
                        "a case over a bool must name true and false or have an else",
                    );
                }
            }
            return;
        };
        let Some(members) = self.declared.enums.get(name) else {
            return;
        };
        let mut missing: Vec<&String> = members.iter().collect();
        for when in &node.whens {
            for (value, _) in &when.values {
                let text = self.text(value.span);
                missing.retain(|member| {
                    let symbol = format!(":{}", enum_symbol(member));
                    let qualified = format!("{name}::{member}");
                    text != symbol && text != qualified
                });
            }
        }
        if !missing.is_empty() {
            let missing: Vec<&str> = missing.iter().map(|m| m.as_str()).collect();
            self.report(
                Code::Case,
                offset,
                format!(
                    "a case over the enum {name} must name every member or have an else; missing {}",
                    missing.join(", ")
                ),
            );
        }
    }
}

/// The symbol an enum member matches, as `Status::InReview` matches `:in_review`.
fn enum_symbol(member: &str) -> String {
    let mut out = String::new();
    for (index, c) in member.chars().enumerate() {
        if c.is_uppercase() {
            if index > 0 {
                out.push('_');
            }
            out.extend(c.to_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

/// Whether every builtin member of this name returns a bool.
fn builtin_predicate(name: &str) -> bool {
    use vibescript::signatures::{Item, Member, Type};
    let table = vibescript::signatures::table();
    let mut found = false;
    for item in &table.items {
        let members: Vec<&Member> = match item {
            Item::Class(class) => class.named(name).collect(),
            Item::Module(module) => module.named(name).collect(),
            _ => Vec::new(),
        };
        for member in members {
            if let Member::Function(function) = member {
                found = true;
                if function.result != Some(Type::name("bool")) {
                    return false;
                }
            }
        }
    }
    found
        || matches!(
            name,
            "nil?"
                | "is_a?"
                | "kind_of?"
                | "instance_of?"
                | "respond_to?"
                | "eql?"
                | "equal?"
                | "frozen?"
                | "block_given?"
        )
}
