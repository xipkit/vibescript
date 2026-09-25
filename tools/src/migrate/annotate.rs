//! Type annotations: parameters, results, yielded blocks, locals whose first
//! value does not fix their type, instance variables and properties.

use super::{Code, migrator::Migrator, syntax::*, types::Types};
use std::collections::BTreeMap;

impl<'a> Migrator<'a> {
    /// Whether an existing annotation certainly accepts every observed type.
    pub fn accepts(&self, ty: &TypeExpr, types: &Types) -> bool {
        if types.any {
            return false;
        }
        let arms: Vec<&TypeExpr> = match &ty.kind {
            TypeKind::Union(options) => options.iter().collect(),
            _ => vec![ty],
        };
        let nullable = arms
            .iter()
            .any(|arm| arm.nullable || self.arm_name(arm) == Some("nil"));
        if types.nil && !nullable {
            return false;
        }
        let names: Vec<&str> = arms.iter().filter_map(|arm| self.arm_name(arm)).collect();
        if names.contains(&"any") {
            return true;
        }
        let scalars_ok = types.scalars.iter().all(|scalar| {
            let name = format!("{scalar:?}").to_lowercase();
            names.contains(&name.as_str())
                || (matches!(
                    scalar,
                    super::types::Scalar::Int | super::types::Scalar::Float
                ) && names.contains(&"number"))
        });
        let array_ok = types.array.as_deref().is_none_or(|element| {
            arms.iter().any(|arm| match &arm.kind {
                TypeKind::Named(_, args) if self.arm_name(arm) == Some("array") => {
                    match args.as_slice() {
                        [] => true,
                        [inner] => self.accepts(inner, element),
                        _ => false,
                    }
                }
                _ => false,
            })
        });
        let hash_ok = types.hash.as_deref().is_none_or(|shape| {
            arms.iter().any(|arm| match &arm.kind {
                TypeKind::Named(_, args)
                    if matches!(self.arm_name(arm), Some("hash" | "object")) =>
                {
                    match args.as_slice() {
                        [] => true,
                        [_, value] => shape
                            .fields
                            .values()
                            .all(|(types, _)| self.accepts(value, types)),
                        _ => false,
                    }
                }
                _ => false,
            })
        });
        let named_ok = types
            .classes
            .iter()
            .chain(&types.enums)
            .all(|name| names.contains(&name.as_str()));
        scalars_ok && array_ok && hash_ok && named_ok
    }

    /// The lowercase builtin or declared name of a named type arm.
    fn arm_name(&self, arm: &TypeExpr) -> Option<&'a str> {
        let TypeKind::Named(tok, _) = &arm.kind else {
            return None;
        };
        let written = self.token_text(*tok).trim_end_matches('?');
        Some(match written.to_ascii_lowercase().as_str() {
            "any" => "any",
            "int" => "int",
            "float" => "float",
            "number" => "number",
            "string" => "string",
            "symbol" => "symbol",
            "bool" => "bool",
            "nil" => "nil",
            "duration" => "duration",
            "time" => "time",
            "money" => "money",
            "range" => "range",
            "array" => "array",
            "hash" => "hash",
            "object" => "object",
            _ => written,
        })
    }

    fn render(&self, types: &Types) -> String {
        types.render(&|name| self.known_type(name))
    }

    /// An annotation for observed types, or a fallback from `default` and
    /// then `any`, which is reported.
    fn annotation(
        &mut self,
        types: Option<&Types>,
        default: Option<&Expr>,
        offset: usize,
        what: &str,
    ) -> String {
        let rendered = match types {
            Some(types) if !types.is_empty() => {
                let text = self.render(types);
                if types.has_any() || text.contains("any") {
                    self.report(
                        Code::Any,
                        offset,
                        format!("{what} is annotated with any where its values had no nameable type; narrow it"),
                    );
                }
                return text;
            }
            _ => default.and_then(literal_type),
        };
        match rendered {
            Some(text) => text,
            None => {
                self.report(
                    Code::Any,
                    offset,
                    format!(
                        "{what} is annotated with any because no recorded run reached it; narrow it"
                    ),
                );
                "any".to_owned()
            }
        }
    }

    pub fn annotate_def(&mut self, def: &'a Def, class: Option<&'a Class>) {
        let _ = class;
        let offset = def.offset(self.tokens);
        let facts = self.facts;
        for param in &def.params {
            if param.ty.is_some() {
                continue;
            }
            let observed = facts.and_then(|facts| facts.params.get(&offset, &param.name));
            let name_end = self.tokens[param.name_tok].end;
            let what = format!("parameter {}", param.name);
            match param.kind {
                ParamKind::Positional => {
                    let mut ty = self.annotation(observed, param.default.as_ref(), name_end, &what);
                    // `x: array<int>={}` would lex `>=`.
                    let separator = if self.source[name_end..].starts_with('=') {
                        " "
                    } else {
                        ""
                    };
                    if ty == "nil" {
                        // `name: nil` declares a keyword default. A parameter
                        // that only ever held nil and is never read takes any
                        // value; one that is read keeps its nil type.
                        ty = if self.mentions(def, &param.name) {
                            "nil?".to_owned()
                        } else {
                            "any".to_owned()
                        };
                    }
                    self.edits.insert(name_end, format!(": {ty}{separator}"));
                }
                ParamKind::Rest => {
                    let ty = match observed.and_then(|types| types.array.as_deref()) {
                        Some(element) if !element.is_empty() => {
                            let element = self.annotation(Some(element), None, name_end, &what);
                            format!("array<{element}>")
                        }
                        _ => {
                            let element = self.annotation(None, None, name_end, &what);
                            format!("array<{element}>")
                        }
                    };
                    self.edits.insert(name_end, format!(": {ty}"));
                }
                ParamKind::KeywordRest => {
                    let mut values = Types::default();
                    if let Some(shape) = observed.and_then(|types| types.hash.as_deref()) {
                        for (types, _) in shape.fields.values() {
                            values.join(types);
                        }
                    }
                    let value = self.annotation(
                        Some(&values).filter(|values| !values.is_empty()),
                        None,
                        name_end,
                        &what,
                    );
                    self.edits
                        .insert(name_end, format!(": hash<string, {value}>"));
                }
                ParamKind::Keyword => {
                    // `name:` or `name: default`; the colon follows the name.
                    let colon = self.tokens[param.name_tok + 1..]
                        .iter()
                        .position(|token| token.kind == vibescript::tooling::TokenKind::Punct(':'))
                        .map(|index| param.name_tok + 1 + index);
                    let Some(colon) = colon else {
                        continue;
                    };
                    let colon_end = self.tokens[colon].end;
                    if param.default.is_none() {
                        let ty = self.annotation(observed, None, name_end, &what);
                        self.edits.insert(colon_end, format!(" {ty}:"));
                    } else if self.options.new_syntax {
                        if super::compat::typed_keyword_defaults() {
                            let ty =
                                self.annotation(observed, param.default.as_ref(), name_end, &what);
                            self.edits.insert(colon_end, format!(" {ty}: ="));
                        } else {
                            self.report(
                                Code::Syntax,
                                name_end,
                                format!(
                                    "keyword {} needs a type, written `{}: T: = default`, which this compiler does not accept yet",
                                    param.name, param.name
                                ),
                            );
                        }
                    }
                }
            }
        }
        if self.options.new_syntax && def.block.is_none() {
            self.annotate_block(def);
        }
        self.annotate_result(def);
    }

    /// Whether a function's body names `name` anywhere.
    fn mentions(&self, def: &Def, name: &str) -> bool {
        let start = def
            .parens
            .map_or(def.name_span.end, |(_, close)| self.tokens[close].end);
        self.tokens[..def.end]
            .iter()
            .enumerate()
            .skip_while(|(_, token)| token.start < start)
            .any(|(index, token)| {
                token.kind == vibescript::tooling::TokenKind::Word && self.token_text(index) == name
            })
    }

    fn signature_end(&self, def: &Def) -> usize {
        match (&def.parens, def.params.last()) {
            (Some((_, close)), _) => self.tokens[*close].end,
            (None, Some(last)) => last.span.end,
            (None, None) => def.name_span.end,
        }
    }

    fn annotate_result(&mut self, def: &'a Def) {
        if def.result.is_some() || def.name == "initialize" || def.name.ends_with('=') {
            return;
        }
        let offset = def.offset(self.tokens);
        let end = self.signature_end(def);
        let observed = self.facts.and_then(|facts| facts.returns.get(&offset));
        match observed {
            Some(types) if !types.is_empty() => {
                let only_nil = types.nil && {
                    let mut without = types.clone();
                    without.nil = false;
                    without.is_empty()
                };
                if only_nil && !returns_value(&def.body) {
                    return;
                }
                let ty = self.annotation(
                    Some(types),
                    None,
                    end,
                    &format!("the result of {}", def.name),
                );
                self.edits.insert(end, format!(" -> {ty}"));
            }
            _ => {
                if returns_value(&def.body) || value_body(&def.body) {
                    let ty =
                        self.annotation(None, None, end, &format!("the result of {}", def.name));
                    self.edits.insert(end, format!(" -> {ty}"));
                }
            }
        }
    }

    /// Declares the block of a function that yields, as `&block: A -> R`.
    fn annotate_block(&mut self, def: &'a Def) {
        let mut yields = Vec::new();
        let mut given = false;
        find_yields(&def.body, true, &mut yields, &mut given);
        if let Some(rescued) = &def.rescue {
            for clause in &rescued.rescues {
                find_yields(&clause.body, false, &mut yields, &mut given);
            }
            for body in rescued.alternate.iter().chain(&rescued.ensure) {
                find_yields(body, false, &mut yields, &mut given);
            }
        }
        if yields.is_empty() {
            return;
        }
        let count = yields.iter().map(|(_, count, _)| *count).max().unwrap_or(0);
        let mut args = vec![Types::default(); count];
        let mut result = Types::default();
        let mut used = false;
        for (offset, _, value) in &yields {
            if let Some(observed) = self.facts.and_then(|facts| facts.yields.get(offset)) {
                for (slot, types) in args.iter_mut().zip(observed) {
                    slot.join(types);
                }
            }
            if *value {
                used = true;
                if let Some(types) = self.facts.and_then(|facts| facts.yield_results.get(offset)) {
                    result.join(types);
                }
            }
        }
        let at = self.tokens[def.keyword].start;
        let rendered: Vec<String> = args
            .iter()
            .map(|types| {
                self.annotation(
                    Some(types).filter(|t| !t.is_empty()),
                    None,
                    at,
                    &format!("the block {} yields", def.name),
                )
            })
            .collect();
        let mut ty = match rendered.len() {
            0 => "()".to_owned(),
            1 if !rendered[0].contains(' ')
                || rendered[0].starts_with('{')
                || rendered[0].starts_with("array<") =>
            {
                rendered[0].clone()
            }
            _ => format!("({})", rendered.join(", ")),
        };
        if used {
            let result = self.annotation(
                Some(&result).filter(|t| !t.is_empty()),
                None,
                at,
                &format!("the result of the block {} yields to", def.name),
            );
            ty = format!("{ty} -> {result}");
        }
        let optional = if given { "?" } else { "" };
        let param = format!("&block{optional}: {ty}");
        match (&def.parens, def.params.is_empty()) {
            (Some((_, close)), true) => self.edits.insert(self.tokens[*close].start, param),
            (Some((_, close)), false) => self
                .edits
                .insert(self.tokens[*close].start, format!(", {param}")),
            (None, false) => {
                let end = def.params.last().unwrap().span.end;
                self.edits.insert(end, format!(", {param}"));
            }
            (None, true) => self.edits.insert(def.name_span.end, format!("({param})")),
        }
    }

    /// Gives a local a declared type at its first assignment when its first
    /// value does not decide it.
    pub fn declare_local(&mut self, stmt: &'a Stmt, assign: &'a Assign) {
        if !self.options.new_syntax || self.token_text(assign.op) != "=" {
            return;
        }
        // A typed local is declared already.
        if let [Target::Typed(inner, _)] = assign.targets.as_slice()
            && let Target::Expr(Expr {
                kind: ExprKind::Name(name),
                ..
            }) = &**inner
        {
            let def = self.scope().def.map_or(usize::MAX, |def| def.keyword);
            self.first_assignments.insert((def, name.clone()));
            return;
        }
        let ([Target::Expr(target)], [value]) =
            (assign.targets.as_slice(), assign.values.as_slice())
        else {
            return;
        };
        let ExprKind::Name(name) = &target.kind else {
            return;
        };
        // A capitalized name is a constant, not a local.
        if name.chars().next().is_some_and(char::is_uppercase) {
            return;
        }
        let scope = self.scope();
        if scope
            .def
            .is_some_and(|def| def.params.iter().any(|param| param.name == *name))
        {
            return;
        }
        let key = (
            scope.def.map_or(usize::MAX, |def| def.keyword),
            name.clone(),
        );
        if !self.first_assignments.insert(key) {
            return;
        }
        let empty = matches!(&value.kind, ExprKind::Nil)
            || matches!(&value.kind, ExprKind::Array(items) if items.is_empty())
            || matches!(&value.kind, ExprKind::Hash(entries) if entries.is_empty())
            || matches!(&value.kind, ExprKind::Call(call)
                if call.name == "new" && call.argument_count() == 0 && call.block.is_none()
                    && matches!(call.receiver.as_ref().map(|r| &r.kind), Some(ExprKind::Name(n)) if n == "Hash"));
        let (joined, first) = self.local_types(name, stmt.span.start);
        let differs = match (&first, &joined) {
            (Some(first), Some(joined)) => self.render(first) != self.render(joined),
            _ => false,
        };
        if !empty && !differs {
            return;
        }
        let ty = self.annotation(
            joined.as_ref(),
            None,
            target.span.start,
            &format!("local {name}"),
        );
        // `x: array<int>=[]` would lex `>=`, so the `=` gets a space.
        let spaced = self.source[target.span.end..].starts_with(' ');
        let separator = if spaced { "" } else { " " };
        self.edits
            .insert(target.span.end, format!(": {ty}{separator}"));
    }

    /// The types a local held in the current function, and at one assignment.
    fn local_types(&self, name: &str, assignment: usize) -> (Option<Types>, Option<Types>) {
        let Some(facts) = self.facts else {
            return (None, None);
        };
        let range = self.scope_range();
        let mut joined = Types::default();
        let mut found = false;
        for map in [&facts.stores, &facts.loads] {
            for (offset, local, types) in map.iter() {
                if local == name && range.contains(offset) && !self.nested_def(*offset) {
                    joined.join(types);
                    found = true;
                }
            }
        }
        let first = facts.stores.get(&assignment, name).cloned();
        (found.then_some(joined), first)
    }

    /// The source range of the current function, or the whole source at top level.
    fn scope_range(&self) -> std::ops::Range<usize> {
        match self.scope().def {
            Some(def) => self.tokens[def.keyword].start..self.tokens[def.end].end,
            None => 0..self.source.len(),
        }
    }

    /// Whether `offset` lies in a function nested in the current scope.
    fn nested_def(&self, offset: usize) -> bool {
        let current = self.scope().def.map(|def| self.tokens[def.keyword].start);
        self.def_ranges.iter().any(|range| {
            Some(range.start) != current
                && range.contains(&offset)
                && current.is_none_or(|start| range.start > start)
        })
    }

    /// Declares a class's instance variables and types its properties.
    pub fn annotate_class(&mut self, class: &'a Class, name: &str) {
        let facts = self.facts;
        let mut properties = Vec::new();
        for member in &class.members {
            if let Member::Property(property) = member {
                for (tok, ty) in &property.names {
                    properties.push(self.token_text(*tok).to_owned());
                    if ty.is_some() {
                        continue;
                    }
                    let field = self.token_text(*tok).to_owned();
                    let start = self.tokens[*tok].start;
                    let mut types = Types::default();
                    if let Some(facts) = facts {
                        for source in [
                            facts.instance.get(&name.to_owned(), &field),
                            facts.params.get(&start, "value"),
                            facts.returns.get(&start),
                        ]
                        .into_iter()
                        .flatten()
                        {
                            types.join(source);
                        }
                    }
                    let ty = self.annotation(
                        Some(&types).filter(|t| !t.is_empty()),
                        None,
                        start,
                        &format!("property {field}"),
                    );
                    self.edits.insert(self.tokens[*tok].end, format!(": {ty}"));
                }
            }
        }
        if !self.options.new_syntax {
            return;
        }
        let mut ivars: BTreeMap<String, bool> = BTreeMap::new();
        for member in &class.members {
            let Member::Def(def) = member else {
                continue;
            };
            // A class method's instance variables belong to the class.
            if def.class_method {
                continue;
            }
            for param in &def.params {
                if param.instance {
                    ivars.entry(param.name.clone()).or_insert(false);
                }
            }
            ivar_writes(&def.body, &mut |ivar| {
                ivars.entry(ivar.to_owned()).or_insert(false);
            });
        }
        ivars.retain(|ivar, _| {
            !properties.contains(ivar)
                && !class
                    .members
                    .iter()
                    .any(|member| matches!(member, Member::Ivar(declared, ..) if declared == ivar))
        });
        if ivars.is_empty() {
            return;
        }
        // Assigned on every path through `initialize`: its parameters and
        // its top-level assignments.
        if let Some(initialize) = class.members.iter().find_map(|member| match member {
            Member::Def(def) if def.name == "initialize" => Some(def),
            _ => None,
        }) {
            for param in &initialize.params {
                if param.instance
                    && let Some(set) = ivars.get_mut(&param.name)
                {
                    *set = true;
                }
            }
            for stmt in &initialize.body {
                if let StmtKind::Assign(assign) = &stmt.kind
                    && self.token_text(assign.op) == "="
                {
                    for target in &assign.targets {
                        if let Target::Expr(Expr {
                            kind: ExprKind::Ivar(ivar),
                            ..
                        }) = target
                            && let Some(set) = ivars.get_mut(ivar.trim_start_matches('@'))
                        {
                            *set = true;
                        }
                    }
                }
            }
        }
        // Declarations go on lines of their own after `class Name`.
        let header_end = self.source[self.tokens[class.name_tok].end..]
            .find('\n')
            .map(|index| self.tokens[class.name_tok].end + index)
            .filter(|end| {
                *end < self.tokens[class.end].start
                    && self.source[self.tokens[class.name_tok].end..*end]
                        .trim()
                        .is_empty()
            });
        let Some(header_end) = header_end else {
            self.report(
                Code::Syntax,
                self.tokens[class.keyword].start,
                format!(
                    "{name}'s instance variables need declarations on lines of their own; split the class onto lines to add them"
                ),
            );
            return;
        };
        let indent = format!("{}  ", self.indentation(self.tokens[class.keyword].start));
        let mut lines = String::new();
        for (ivar, set) in ivars {
            let mut types = facts
                .and_then(|facts| facts.instance.get(&name.to_owned(), &ivar))
                .cloned()
                .unwrap_or_default();
            let at = self.tokens[class.name_tok].start;
            if !set {
                types.nil = true;
            }
            let observed = !types.is_empty()
                && !(types.nil && {
                    let mut without = types.clone();
                    without.nil = false;
                    without.is_empty()
                });
            let ty = if observed || types.nil && set {
                self.annotation(
                    Some(&types),
                    None,
                    at,
                    &format!("instance variable @{ivar}"),
                )
            } else {
                let ty = self.annotation(None, None, at, &format!("instance variable @{ivar}"));
                if set {
                    ty
                } else {
                    format!("{ty}?").replace("any?", "any")
                }
            };
            // A default would create the field before the program assigns
            // it, which code that asks for the field could tell apart.
            lines.push_str(&format!("\n{indent}@{ivar}: {ty}"));
            if !set {
                self.report(
                    Code::Initialize,
                    at,
                    format!(
                        "instance variable @{ivar} is not assigned on every path through initialize; assign it there or give it a default"
                    ),
                );
            }
        }
        self.edits.insert(header_end, lines);
    }
}

/// The type a literal default value fixes.
fn literal_type(expr: &Expr) -> Option<String> {
    Some(
        match &expr.kind {
            ExprKind::Integer => "int",
            ExprKind::Float => "float",
            ExprKind::Str | ExprKind::Template(_) => "string",
            ExprKind::Symbol => "symbol",
            ExprKind::True | ExprKind::False => "bool",
            ExprKind::Unary(_, operand) => return literal_type(operand),
            _ => return None,
        }
        .to_owned(),
    )
}

/// Whether a body has an explicit `return` of a value.
fn returns_value(body: &[Stmt]) -> bool {
    let mut found = false;
    visit_stmts(body, &mut |stmt| {
        if let StmtKind::Flow(tok, Some(value)) = &stmt.kind
            && !matches!(value.kind, ExprKind::Nil)
        {
            let _ = tok;
            found = true;
        }
    });
    found
}

/// Whether a body's last statement produces a value a caller could use.
fn value_body(body: &[Stmt]) -> bool {
    let Some(last) = body.last() else {
        return false;
    };
    match &last.kind {
        StmtKind::Expr(expr) => match &expr.kind {
            ExprKind::Nil => false,
            ExprKind::Call(call) => {
                call.receiver.is_some()
                    || !matches!(
                        call.name.as_str(),
                        "puts" | "print" | "p" | "warn" | "raise" | "assert"
                    )
            }
            _ => true,
        },
        _ => false,
    }
}

fn visit_stmts(body: &[Stmt], visit: &mut impl FnMut(&Stmt)) {
    for stmt in body {
        visit(stmt);
        match &stmt.kind {
            StmtKind::If(node) => {
                for (_, body) in &node.branches {
                    visit_stmts(body, visit);
                }
                if let Some((_, body)) = &node.alternate {
                    visit_stmts(body, visit);
                }
            }
            StmtKind::While(node) => visit_stmts(&node.body, visit),
            StmtKind::For(node) => visit_stmts(&node.body, visit),
            StmtKind::Modifier(node) => visit_stmts(std::slice::from_ref(&node.body), visit),
            StmtKind::Expr(expr) => visit_expr_bodies(expr, visit),
            _ => (),
        }
    }
}

fn visit_expr_bodies(expr: &Expr, visit: &mut impl FnMut(&Stmt)) {
    match &expr.kind {
        ExprKind::Begin(node) => {
            visit_stmts(&node.body, visit);
            for clause in &node.rescued.rescues {
                visit_stmts(&clause.body, visit);
            }
        }
        ExprKind::Loop(stmt) => visit_stmts(std::slice::from_ref(stmt), visit),
        ExprKind::Call(call) => {
            if let Some(block) = &call.block {
                visit_stmts(&block.body, visit);
            }
        }
        _ => (),
    }
}

/// Finds each `yield`: its offset, argument count, and whether its value is used.
fn find_yields(
    body: &[Stmt],
    last_used: bool,
    out: &mut Vec<(usize, usize, bool)>,
    given: &mut bool,
) {
    for (index, stmt) in body.iter().enumerate() {
        let statement = index + 1 < body.len() || !last_used;
        find_stmt_yields(stmt, statement, out, given);
    }
}

fn find_stmt_yields(
    stmt: &Stmt,
    discarded: bool,
    out: &mut Vec<(usize, usize, bool)>,
    given: &mut bool,
) {
    match &stmt.kind {
        StmtKind::Expr(expr) => find_expr_yields(expr, !discarded, out, given),
        StmtKind::Assign(assign) => {
            for value in &assign.values {
                find_expr_yields(value, true, out, given);
            }
        }
        StmtKind::If(node) => {
            for (condition, body) in &node.branches {
                find_expr_yields(condition, true, out, given);
                find_yields(body, !discarded, out, given);
            }
            if let Some((_, body)) = &node.alternate {
                find_yields(body, !discarded, out, given);
            }
        }
        StmtKind::While(node) => {
            find_expr_yields(&node.condition, true, out, given);
            find_yields(&node.body, false, out, given);
        }
        StmtKind::For(node) => {
            find_expr_yields(&node.iterable, true, out, given);
            find_yields(&node.body, false, out, given);
        }
        StmtKind::Modifier(node) => {
            find_stmt_yields(&node.body, true, out, given);
            find_expr_yields(&node.condition, true, out, given);
        }
        StmtKind::Flow(_, Some(value)) => find_expr_yields(value, true, out, given),
        StmtKind::Raise(_, value, message) => {
            for expr in value.iter().chain(message) {
                find_expr_yields(expr, true, out, given);
            }
        }
        _ => (),
    }
}

fn find_expr_yields(
    expr: &Expr,
    used: bool,
    out: &mut Vec<(usize, usize, bool)>,
    given: &mut bool,
) {
    match &expr.kind {
        ExprKind::Yield(_, args) => {
            let count = args.as_ref().map_or(0, |args| args.items.len());
            out.push((expr.span.start, count, used));
            for arg in args.iter().flat_map(|args| &args.items) {
                find_expr_yields(&arg.value, true, out, given);
            }
        }
        ExprKind::Name(name) if name == "block_given?" => *given = true,
        ExprKind::Call(call) => {
            if call.name == "block_given?" && call.receiver.is_none() {
                *given = true;
            }
            if let Some(receiver) = &call.receiver {
                find_expr_yields(receiver, true, out, given);
            }
            for arg in call.args.iter().flat_map(|args| &args.items) {
                find_expr_yields(&arg.value, true, out, given);
            }
            if let Some(block) = &call.block {
                find_yields(&block.body, true, out, given);
            }
        }
        ExprKind::Group(_, inner, _) => find_expr_yields(inner, used, out, given),
        ExprKind::Unary(_, operand) => find_expr_yields(operand, true, out, given),
        ExprKind::Binary(_, left, right) => {
            find_expr_yields(left, true, out, given);
            find_expr_yields(right, true, out, given);
        }
        ExprKind::Ternary(condition, _, yes, no) => {
            find_expr_yields(condition, true, out, given);
            find_expr_yields(yes, used, out, given);
            find_expr_yields(no, used, out, given);
        }
        ExprKind::Array(items) => {
            for item in items {
                find_expr_yields(item, true, out, given);
            }
        }
        ExprKind::Hash(entries) => {
            for entry in entries {
                find_expr_yields(&entry.value, true, out, given);
            }
        }
        ExprKind::Index(receiver, _, selectors, _) => {
            find_expr_yields(receiver, true, out, given);
            for selector in selectors {
                find_expr_yields(selector, true, out, given);
            }
        }
        ExprKind::Template(parts) => {
            for part in parts.iter().flatten() {
                find_expr_yields(part, true, out, given);
            }
        }
        ExprKind::If(node) => {
            for (condition, body) in &node.branches {
                find_expr_yields(condition, true, out, given);
                find_yields(body, used, out, given);
            }
            if let Some((_, body)) = &node.alternate {
                find_yields(body, used, out, given);
            }
        }
        ExprKind::Case(node) => {
            if let Some(subject) = &node.subject {
                find_expr_yields(subject, true, out, given);
            }
            for when in &node.whens {
                for (value, _) in &when.values {
                    find_expr_yields(value, true, out, given);
                }
                find_expr_yields(&when.result, used, out, given);
            }
            if let Some((_, alternate)) = &node.alternate {
                find_expr_yields(alternate, used, out, given);
            }
        }
        ExprKind::Begin(node) => {
            find_yields(&node.body, used, out, given);
            for clause in &node.rescued.rescues {
                find_yields(&clause.body, used, out, given);
            }
        }
        ExprKind::Loop(stmt) => find_stmt_yields(stmt, true, out, given),
        ExprKind::Rescue(body, _, fallback) => {
            find_expr_yields(body, used, out, given);
            find_expr_yields(fallback, used, out, given);
        }
        ExprKind::Computed(callee, args) => {
            find_expr_yields(callee, true, out, given);
            for arg in &args.items {
                find_expr_yields(&arg.value, true, out, given);
            }
        }
        ExprKind::BlockCall(callee, block) => {
            find_expr_yields(callee, true, out, given);
            find_yields(&block.body, true, out, given);
        }
        ExprKind::Range(left, _, right) => {
            for operand in left.iter().chain(right) {
                find_expr_yields(operand, true, out, given);
            }
        }
        _ => (),
    }
}

/// Visits the instance variables a body assigns.
fn ivar_writes(body: &[Stmt], visit: &mut impl FnMut(&str)) {
    visit_stmts(body, &mut |stmt| {
        if let StmtKind::Assign(assign) = &stmt.kind {
            for target in &assign.targets {
                if let Target::Expr(Expr {
                    kind: ExprKind::Ivar(ivar),
                    ..
                }) = target
                    && !ivar.starts_with("@@")
                {
                    visit(ivar.trim_start_matches('@'));
                }
            }
        }
    });
}
