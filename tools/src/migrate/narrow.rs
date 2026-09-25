//! Repairs that narrow a value where it is used, leaving annotations alone:
//! `fetch` for an index or `first` the recorded runs never found missing, a
//! checked cast where a value is `any` or optional, `to_s` where `+` joined
//! a string with another value, and a string for a symbol key.
//!
//! Each one can change what a program does when the value is not what the
//! runs saw, so the repair loop proposes them only where it can run the
//! recorded invocations to confirm that nothing changed.

use super::{
    observe::Facts,
    repair::Proposal,
    sites::{Node, each_node, expr_at},
    ty::Ty,
};
use vibescript::{
    diagnostic::{Applicability, Code, Diagnostic},
    surface::{self, Surface, syntax::*},
};

/// Proposes narrowing repairs for `errors` in `text`: for each error, the
/// repairs to try in order.
pub(crate) fn narrowings(
    text: &str,
    tree: &Tree,
    errors: &[Diagnostic],
    facts: Option<&Facts>,
) -> Vec<Vec<Proposal>> {
    let surface = Surface::new(text, tree);
    let mut groups = Vec::new();
    for error in errors {
        let optional_found = error
            .found
            .as_deref()
            .and_then(Ty::parse)
            .is_some_and(|ty| ty.has_nil() && ty.without_nil() != Ty::Never);
        let mut candidates = match error.code {
            Code::OPTIONAL_USE => optional(text, tree, error),
            Code::CONDITION_NOT_BOOL | Code::LOGICAL_NOT_BOOL | Code::LOCAL_TYPE_CHANGED
                if optional_found =>
            {
                optional(text, tree, error)
            }
            Code::ANY_USE => any(text, tree, &surface, error, facts),
            Code::NO_OPERATOR => joined_string(text, tree, error).into_iter().collect(),
            Code::TYPE_MISMATCH => symbol_key(text, tree, error).into_iter().collect(),
            _ => Vec::new(),
        };
        if matches!(
            error.code,
            Code::TYPE_MISMATCH | Code::LOCAL_TYPE_CHANGED | Code::OPTIONAL_USE
        ) && nested_nil(error)
        {
            candidates.extend(inner_fetches(text, tree, error));
        }
        if candidates.is_empty() {
            candidates.extend(checker_fix(error));
        }
        if !candidates.is_empty() {
            groups.push(candidates);
        }
    }
    groups
}

/// Whether the type a diagnostic found allows `nil` inside a collection
/// where the expected type does not.
fn nested_nil(error: &Diagnostic) -> bool {
    let (Some(expected), Some(found)) = (&error.expected, &error.found) else {
        return false;
    };
    let nils = |text: &str| text.matches('?').count() + text.matches("nil").count();
    nils(found) > nils(expected) && found.contains('<')
}

/// `fetch` for every index, `first` and `last` a value reads, and for those
/// the locals it reads were assigned from.
fn inner_fetches(text: &str, tree: &Tree, error: &Diagnostic) -> Vec<Proposal> {
    let Some(value) = expr_at(tree, error.span.start, error.span.end) else {
        return Vec::new();
    };
    let mut edits = Vec::new();
    let mut names = Vec::new();
    super::sites::each_expr(value, &mut |node| {
        let Node::Expr(expr) = node else {
            return false;
        };
        if let Some(edit) = fetch(text, expr) {
            edits.push(edit);
            return false;
        }
        if let ExprKind::Name(name) = &expr.kind {
            names.push(name.clone());
        }
        true
    });
    for name in names {
        for edit in origins(text, tree, &name, value.span.start) {
            if !edits.contains(&edit) {
                edits.push(edit);
            }
        }
    }
    let mut kept: Vec<(Span, String)> = Vec::new();
    for edit in edits {
        if kept
            .iter()
            .all(|other| edit.0.end <= other.0.start || other.0.end <= edit.0.start)
        {
            kept.push(edit);
        }
    }
    if kept.is_empty() {
        Vec::new()
    } else {
        vec![proposal(kept)]
    }
}

fn proposal(edits: Vec<(Span, String)>) -> Proposal {
    Proposal {
        edits,
        tightens: false,
        safe: false,
    }
}

/// The checker's own fix, such as `fetch` for an index that must not be nil.
fn checker_fix(error: &Diagnostic) -> Option<Proposal> {
    let fix = error
        .fixes
        .iter()
        .find(|fix| fix.applicability == Applicability::Always)?;
    Some(proposal(
        fix.edits
            .iter()
            .map(|edit| {
                (
                    Span {
                        start: edit.span.start,
                        end: edit.span.end,
                    },
                    edit.replacement.clone(),
                )
            })
            .collect(),
    ))
}

/// The expression a diagnostic about a value points at: the value itself,
/// or the receiver of the member call whose name it points at.
fn value_at<'t>(tree: &'t Tree, error: &Diagnostic) -> Option<&'t Expr> {
    let span = error.span;
    let mut receiver = None;
    each_node(tree, &mut |node| {
        if let Node::Expr(Expr {
            kind: ExprKind::Call(call),
            ..
        }) = node
            && tree.tokens[call.name_tok].start == span.start
            && tree.tokens[call.name_tok].end == span.end
        {
            receiver = call.receiver.as_ref();
        }
        true
    });
    if receiver.is_some() {
        return receiver;
    }
    expr_at(tree, span.start, span.end)
        .filter(|expr| expr.span.start == span.start && expr.span.end == span.end)
}

/// The expressions a diagnostic about a value may mean: [`value_at`]'s,
/// then each member read without arguments that chains on it. The checker's
/// span of `x.last` covers only `x`, so both are candidates.
fn values_at<'t>(tree: &'t Tree, error: &Diagnostic) -> Vec<&'t Expr> {
    let Some(first) = value_at(tree, error) else {
        return Vec::new();
    };
    let mut values = vec![first];
    loop {
        let inner = *values.last().unwrap();
        let mut outer = None;
        each_node(tree, &mut |node| {
            if let Node::Expr(expr) = node
                && let ExprKind::Call(call) = &expr.kind
                && call.args.is_none()
                && call.block.is_none()
                && call
                    .receiver
                    .as_ref()
                    .is_some_and(|receiver| std::ptr::eq(receiver, inner))
            {
                outer = Some(expr);
            }
            outer.is_none()
        });
        match outer {
            Some(expr) => values.push(expr),
            None => break,
        }
    }
    values
}

/// `x.as(T)`, parenthesizing `x` when it binds looser than a member call.
fn cast(text: &str, expr: &Expr, ty: &str) -> Proposal {
    proposal(vec![(
        expr.span,
        format!("{}.as({ty})", receiver(text, expr)),
    )])
}

/// An expression's text as the receiver of a member call.
fn receiver(text: &str, expr: &Expr) -> String {
    let source = &text[expr.span.range()];
    if surface::primary(expr) {
        source.to_owned()
    } else {
        format!("({source})")
    }
}

/// The span of the function around `offset`, or of the whole source.
fn scope(text: &str, tree: &Tree, offset: usize) -> Span {
    let sites = super::sites::defs(tree);
    super::sites::def_containing(&sites, offset).map_or(
        Span {
            start: 0,
            end: text.len(),
        },
        |site| site.span,
    )
}

/// A value that may be nil where it must not be: `fetch` for an index,
/// `first` or `last`, or for the ones a local was assigned from, and
/// otherwise a cast to its type without `nil`.
fn optional(text: &str, tree: &Tree, error: &Diagnostic) -> Vec<Proposal> {
    let mut candidates = Vec::new();
    let target = fetch_fix_target(tree, error);
    let values = match target {
        Some(index) => vec![index],
        None => values_at(tree, error),
    };
    let ty = optional_type(error)
        .as_deref()
        .and_then(Ty::parse)
        .map(|ty| ty.without_nil())
        .filter(|ty| *ty != Ty::Never && !ty.vague())
        .and_then(|ty| ty.render());
    for expr in values {
        if let Some(edit) = fetch(text, expr) {
            candidates.push(proposal(vec![edit]));
        }
        if let ExprKind::Name(name) = &expr.kind {
            let origins = origins(text, tree, name, expr.span.start);
            if !origins.is_empty() {
                candidates.push(proposal(origins));
            }
        }
        if let Some(rendered) = &ty {
            candidates.push(cast(text, expr, rendered));
        }
    }
    candidates
}

/// The index the checker offers to read with `fetch`: the one whose
/// brackets its fix replaces. The fix is rendered from this tree's spans.
fn fetch_fix_target<'t>(tree: &'t Tree, error: &Diagnostic) -> Option<&'t Expr> {
    let fix = error
        .fixes
        .iter()
        .find(|fix| fix.applicability == Applicability::Always)?;
    let [edit] = fix.edits.as_slice() else {
        return None;
    };
    if !edit.replacement.starts_with(".fetch(") {
        return None;
    }
    let mut found = None;
    each_node(tree, &mut |node| {
        if let Node::Expr(expr) = node
            && let ExprKind::Index(_, open, ..) = &expr.kind
            && tree.tokens[*open].start == edit.span.start
        {
            found = Some(expr);
        }
        true
    });
    found
}

/// `x.fetch(i)` for `x[i]`, `x.fetch(0)` for `x.first` and `x.fetch(-1)`
/// for `x.last`.
fn fetch(text: &str, expr: &Expr) -> Option<(Span, String)> {
    let (receiver, selector) = match &expr.kind {
        ExprKind::Index(receiver, _, selectors, _)
            if selectors.len() == 1 && !matches!(selectors[0].kind, ExprKind::Range(..)) =>
        {
            (&**receiver, text[selectors[0].span.range()].to_owned())
        }
        ExprKind::Call(call)
            if call.args.is_none()
                && call.block.is_none()
                && matches!(call.name.as_str(), "first" | "last") =>
        {
            let index = if call.name == "first" { "0" } else { "-1" };
            (call.receiver.as_ref()?, index.to_owned())
        }
        _ => return None,
    };
    Some((
        expr.span,
        format!("{}.fetch({selector})", self::receiver(text, receiver)),
    ))
}

/// `fetch` for every index, `first` or `last` assigned to the local
/// `name` in the function around `offset`.
fn origins(text: &str, tree: &Tree, name: &str, offset: usize) -> Vec<(Span, String)> {
    let scope = scope(text, tree, offset);
    let mut edits = Vec::new();
    each_node(tree, &mut |node| {
        if let Node::Stmt(stmt) = node
            && scope.start <= stmt.span.start
            && stmt.span.end <= scope.end
            && let StmtKind::Assign(assign) = &stmt.kind
            && let ([target], [value]) = (assign.targets.as_slice(), assign.values.as_slice())
            && &text[tree.tokens[assign.op].start..tree.tokens[assign.op].end] == "="
        {
            let assigned = match target {
                Target::Expr(expr) => matches!(&expr.kind, ExprKind::Name(own) if own == name),
                Target::Typed(inner, _) => {
                    matches!(&**inner, Target::Expr(Expr { kind: ExprKind::Name(own), .. }) if own == name)
                }
                _ => false,
            };
            if assigned && let Some(edit) = fetch(text, value) {
                edits.push(edit);
            }
        }
        true
    });
    edits
}

/// The optional type a diagnostic found, from its types or its message.
fn optional_type(error: &Diagnostic) -> Option<String> {
    if let Some(found) = &error.found {
        return Some(found.clone());
    }
    let message = &error.message;
    if let Some(rest) = message.strip_prefix("this value may be nil (") {
        return Some(rest[..rest.find("); ")?].to_owned());
    }
    let start = message.find("and this value is ")? + "and this value is ".len();
    let rest = &message[start..];
    Some(rest[..rest.find("; ")?].to_owned())
}

/// An `any` value used where a type is needed: a cast to the type its
/// position expects, or to the one type the recorded runs saw there.
fn any(
    text: &str,
    tree: &Tree,
    surface: &Surface<'_>,
    error: &Diagnostic,
    facts: Option<&Facts>,
) -> Vec<Proposal> {
    let values = values_at(tree, error);
    let Some(&expr) = values.first() else {
        return Vec::new();
    };
    let usable = |ty: &Ty| !ty.vague() && *ty != Ty::Nil && *ty != Ty::Never;
    let mut candidates = Vec::new();
    // A local that is `any` because of what it was assigned: cast where it
    // is assigned, so every use has the type.
    if let ExprKind::Name(name) = &expr.kind
        && let Some(facts) = facts
        && let Some(ty) = stored(tree, surface, facts, name, expr.span.start).filter(usable)
        && let Some(rendered) = ty.render()
    {
        let edits: Vec<(Span, String)> = assignments(text, tree, name, expr.span.start)
            .into_iter()
            .flat_map(|value| cast(text, value, &rendered).edits)
            .collect();
        if !edits.is_empty() {
            candidates.push(proposal(edits));
        }
    }
    let expected = error.expected.as_deref().and_then(Ty::parse).filter(usable);
    let ty = match expected {
        Some(ty) => Some(ty),
        None => facts.and_then(|facts| observed(tree, surface, expr, error, facts)),
    };
    if let Some(ty) = ty.filter(usable)
        && let Some(rendered) = ty.render()
    {
        for expr in values {
            candidates.push(cast(text, expr, &rendered));
        }
    }
    candidates
}

/// The types the recorded runs stored in the local `name` in the function
/// around `offset`.
fn stored(
    tree: &Tree,
    surface: &Surface<'_>,
    facts: &Facts,
    name: &str,
    offset: usize,
) -> Option<Ty> {
    let sites = super::sites::defs(tree);
    let site = super::sites::def_containing(&sites, offset);
    let scope = site.map_or(0..surface.source.len(), |site| {
        site.span.start..site.span.end
    });
    // Offsets inside a nested function belong to it.
    let nested: Vec<Span> = sites
        .iter()
        .filter(|other| {
            site.is_none_or(|site| !std::ptr::eq(site.def, other.def))
                && scope.contains(&other.span.start)
        })
        .map(|other| other.span)
        .collect();
    let mut types = super::types::Types::default();
    let mut found = false;
    for (at, local, observed) in facts.stores.iter() {
        if local == name
            && scope.contains(at)
            && !nested
                .iter()
                .any(|span| span.start <= *at && *at < span.end)
        {
            types.join(observed);
            found = true;
        }
    }
    found.then(|| Ty::observed(&types, &|name| surface.known_type(name)))
}

/// The values assigned with `=` to the local `name` in the function around
/// `offset`.
fn assignments<'t>(text: &str, tree: &'t Tree, name: &str, offset: usize) -> Vec<&'t Expr> {
    let scope = scope(text, tree, offset);
    let mut values = Vec::new();
    each_node(tree, &mut |node| {
        if let Node::Stmt(stmt) = node
            && scope.start <= stmt.span.start
            && stmt.span.end <= scope.end
            && let StmtKind::Assign(assign) = &stmt.kind
            && let ([target], [value]) = (assign.targets.as_slice(), assign.values.as_slice())
            && &text[tree.tokens[assign.op].start..tree.tokens[assign.op].end] == "="
        {
            let assigned = match target {
                Target::Expr(expr) => matches!(&expr.kind, ExprKind::Name(own) if own == name),
                _ => false,
            };
            if assigned {
                values.push(value);
            }
        }
        true
    });
    values
}

/// The types the recorded runs saw for the value an `any` diagnostic is
/// about: a member call's receiver, an indexed value or an operand.
fn observed(
    tree: &Tree,
    surface: &Surface<'_>,
    expr: &Expr,
    error: &Diagnostic,
    facts: &Facts,
) -> Option<Ty> {
    let message = &error.message;
    let types = if let Some(member) = message
        .strip_suffix('`')
        .and_then(|rest| rest.rsplit_once("before calling `"))
        .map(|(_, member)| member)
    {
        let mut found = None;
        each_node(tree, &mut |node| {
            if let Node::Expr(outer) = node
                && let ExprKind::Call(call) = &outer.kind
                && call.name == member
                && call
                    .receiver
                    .as_ref()
                    .is_some_and(|receiver| receiver.span == expr.span)
            {
                found = facts
                    .receivers
                    .get(&surface.compiler_offset(outer), member)
                    .cloned();
            }
            true
        });
        found?
    } else if message.ends_with("before indexing it") {
        let mut found = None;
        each_node(tree, &mut |node| {
            if let Node::Expr(outer) = node
                && let ExprKind::Index(receiver, open, ..) = &outer.kind
                && receiver.span == expr.span
            {
                found = facts
                    .indexes
                    .get(&tree.tokens[*open].start)
                    .map(|(receiver, _)| receiver.clone());
            }
            true
        });
        found?
    } else if message.ends_with("before using an operator on it") {
        let mut found = None;
        each_node(tree, &mut |node| {
            if let Node::Expr(outer) = node
                && let ExprKind::Binary(op, left, right) = &outer.kind
            {
                let binary = facts.binaries.get(&surface.operator_offset(*op));
                if left.span == expr.span {
                    found = binary.map(|binary| binary.left.clone());
                } else if right.span == expr.span {
                    found = binary.map(|binary| binary.right.clone());
                }
            }
            true
        });
        found?
    } else {
        return None;
    };
    Some(Ty::observed(&types, &|name| surface.known_type(name)))
}

/// `"text" + value` or `value + "text"`, which today's runtime converts:
/// `value.to_s`.
fn joined_string(text: &str, tree: &Tree, error: &Diagnostic) -> Option<Proposal> {
    let rest = error.message.strip_prefix("`+` is not defined for ")?;
    let rest = rest.split(';').next()?.trim();
    let (left_type, right_type) = rest.split_once(" and ")?;
    let converts = |ty: &str| matches!(ty, "int" | "float" | "number" | "symbol" | "bool");
    let left_converts = right_type == "string" && converts(left_type);
    if !left_converts && !(left_type == "string" && converts(right_type)) {
        return None;
    }
    let mut binary = None;
    each_node(tree, &mut |node| {
        if let Node::Expr(outer) = node
            && let ExprKind::Binary(op, _, _) = &outer.kind
            && &text[tree.tokens[*op].start..tree.tokens[*op].end] == "+"
            && outer.span.start <= error.span.start
            && error.span.end <= outer.span.end
        {
            binary = Some(outer);
        }
        true
    });
    let ExprKind::Binary(_, left, right) = &binary?.kind else {
        return None;
    };
    let operand = if left_converts { left } else { right };
    Some(proposal(vec![(
        operand.span,
        format!("{}.to_s", receiver(text, operand)),
    )]))
}

/// A symbol passed where a hash member takes a string key: the string.
fn symbol_key(text: &str, tree: &Tree, error: &Diagnostic) -> Option<Proposal> {
    if error.found.as_deref() != Some("symbol") || error.expected.as_deref() != Some("string") {
        return None;
    }
    if !error.message.starts_with("argument ") {
        return None;
    }
    let expr = expr_at(tree, error.span.start, error.span.end)?;
    if !matches!(expr.kind, ExprKind::Symbol) {
        return None;
    }
    let symbol = &text[expr.span.range()];
    let name = symbol.strip_prefix(':')?;
    let literal = if name.starts_with('"') {
        name.to_owned()
    } else {
        format!("\"{name}\"")
    };
    Some(proposal(vec![(expr.span, literal)]))
}
