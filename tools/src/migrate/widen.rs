//! Repairs that widen an annotation the migration wrote to the types the
//! checker found for it: a typed local another assignment gives a new
//! type, a parameter a caller passes something else, an instance variable
//! or property assigned another type, and a record whose keys turn out to
//! be computed, which becomes a dictionary.
//!
//! An annotation built from what the recorded runs observed accepts every
//! value they produced, and a wider one accepts them too, so these cannot
//! change what a recorded run does.

use super::{
    repair::Proposal,
    sites::{DefSite, Node, each_node},
    ty::Ty,
};
use vibescript::{
    diagnostic::{Code, Diagnostic},
    surface::syntax::*,
};

/// Whether an annotation of `name` as written was the author's: the
/// original source spells the same declaration.
pub(crate) type Authored<'o> = dyn Fn(&str, &str) -> bool + 'o;

/// Proposes widening repairs for `errors` in `text`. `params` tells, for a
/// function and parameter, whether the migration wrote its annotation.
pub(crate) fn widenings(
    text: &str,
    tree: &Tree,
    sites: &[DefSite<'_>],
    errors: &[Diagnostic],
    written_param: &dyn Fn(usize, &str) -> bool,
    authored: &Authored<'_>,
) -> Vec<Proposal> {
    let mut proposals = Vec::new();
    for error in errors {
        let proposal = match error.code {
            Code::LOCAL_TYPE_CHANGED => local(text, tree, error, authored),
            Code::DYNAMIC_KEY => dictionary(text, tree, sites, error, written_param, authored),
            Code::TYPE_MISMATCH | Code::OPTIONAL_USE => argument(text, sites, error, written_param)
                .or_else(|| ivar(text, tree, error, authored)),
            Code::UNINITIALIZED_IVAR => unassigned(text, tree, error, authored),
            _ => None,
        };
        proposals.extend(proposal);
    }
    proposals
}

fn proposal(span: Span, text: String) -> Proposal {
    Proposal {
        edits: vec![(span, text)],
        tightens: false,
        safe: true,
    }
}

/// The join of a diagnostic's expected and found types, as an annotation.
fn joined(error: &Diagnostic) -> Option<(Ty, String)> {
    let expected = Ty::parse(error.expected.as_deref()?)?;
    let found = Ty::parse(error.found.as_deref()?)?;
    if found == Ty::Any || expected.vague() {
        return None;
    }
    let ty = expected.join(found);
    let rendered = ty.render()?;
    Some((ty, rendered))
}

/// A local assigned a value its first assignment's type excludes: declare
/// it with both.
fn local(text: &str, tree: &Tree, error: &Diagnostic, authored: &Authored<'_>) -> Option<Proposal> {
    if !error.message.contains(", fixed by its first assignment; ")
        && !error.message.contains(", declared; ")
    {
        return None;
    }
    let declared = error.labels.first()?.span;
    let (_, rendered) = joined(error)?;
    let mut found = None;
    each_node(tree, &mut |node| {
        if let Node::Stmt(stmt) = node
            && let StmtKind::Assign(assign) = &stmt.kind
            && let [target] = assign.targets.as_slice()
            && &text[tree.tokens[assign.op].start..tree.tokens[assign.op].end] == "="
        {
            match target {
                Target::Expr(
                    expr @ Expr {
                        kind: ExprKind::Name(name),
                        ..
                    },
                ) if expr.span.start == declared.start => {
                    found = Some((name.clone(), expr.span, None));
                }
                Target::Typed(inner, ty) => {
                    if let Target::Expr(
                        expr @ Expr {
                            kind: ExprKind::Name(name),
                            ..
                        },
                    ) = &**inner
                        && expr.span.start == declared.start
                    {
                        found = Some((name.clone(), expr.span, Some(ty.span)));
                    }
                }
                _ => (),
            }
        }
        true
    });
    let (name, target, annotation) = found?;
    match annotation {
        Some(span) => {
            if authored(&name, &text[span.range()]) {
                return None;
            }
            Some(proposal(span, rendered))
        }
        None => {
            // `x: array<int>=[]` would lex `>=`.
            let spaced = text[target.end..].starts_with(' ');
            let separator = if spaced { "" } else { " " };
            Some(proposal(
                Span {
                    start: target.end,
                    end: target.end,
                },
                format!(": {rendered}{separator}"),
            ))
        }
    }
}

/// An argument a caller passes to a parameter the migration annotated:
/// widen the parameter to take it.
fn argument(
    text: &str,
    sites: &[DefSite<'_>],
    error: &Diagnostic,
    written_param: &dyn Fn(usize, &str) -> bool,
) -> Option<Proposal> {
    let message = &error.message;
    let (param, function) = if let Some(rest) = message.strip_prefix("argument ") {
        let start = rest.find(" (`")? + 3;
        let end = start + rest[start..].find("`) of `")?;
        let function_start = end + "`) of `".len();
        let function_end = function_start + rest[function_start..].find('`')?;
        (&rest[start..end], &rest[function_start..function_end])
    } else {
        let rest = message.strip_prefix("keyword `")?;
        let end = rest.find(":` of `")?;
        let function_start = end + ":` of `".len();
        let function_end = function_start + rest[function_start..].find('`')?;
        (&rest[..end], &rest[function_start..function_end])
    };
    let found = Ty::parse(error.found.as_deref()?)?;
    if found == Ty::Any || found.vague() {
        return None;
    }
    let name = function.rsplit(['#', '.']).next()?;
    let mut candidates = sites
        .iter()
        .enumerate()
        .filter(|(_, site)| site.def.name == name);
    let (index, site) = candidates.next()?;
    if candidates.next().is_some() {
        return None;
    }
    let annotation = site
        .def
        .params
        .iter()
        .find(|p| p.name == param)?
        .ty
        .as_ref()?;
    if !written_param(index, param) {
        return None;
    }
    let current = Ty::parse(&text[annotation.span.range()])?;
    let widened = current.clone().join(found);
    if widened == current {
        return None;
    }
    Some(proposal(annotation.span, widened.render()?))
}

/// An instance variable or property the migration declared, assigned a
/// value of another type.
fn ivar(text: &str, tree: &Tree, error: &Diagnostic, authored: &Authored<'_>) -> Option<Proposal> {
    let rest = error.message.strip_prefix("`@")?;
    let name = &rest[..rest.find("` is ")?];
    let (_, rendered) = joined(error)?;
    let span = declaration(text, tree, error, name)?;
    if authored(name, &text[span.range()]) {
        return None;
    }
    Some(proposal(span, rendered))
}

/// Instance variables the migration declared that `initialize` leaves
/// unassigned on some path: declare them optional.
fn unassigned(
    text: &str,
    tree: &Tree,
    error: &Diagnostic,
    authored: &Authored<'_>,
) -> Option<Proposal> {
    let rest = error
        .message
        .strip_prefix("`initialize` does not assign ")?;
    let names = &rest[..rest.find(" on every path")?];
    let mut edits = Vec::new();
    for name in names.split(", ") {
        let name = name.strip_prefix('@')?;
        let span = declaration(text, tree, error, name)?;
        let declared = &text[span.range()];
        if authored(name, declared) {
            return None;
        }
        let optional = Ty::parse(declared)?.join(Ty::Nil).render()?;
        edits.push((span, optional));
    }
    Some(Proposal {
        edits,
        tightens: false,
        safe: true,
    })
}

/// The type of the declaration of the instance variable or property `name`
/// in the class around `error`.
fn declaration(text: &str, tree: &Tree, error: &Diagnostic, name: &str) -> Option<Span> {
    let mut found = None;
    each_node(tree, &mut |node| {
        if let Node::Stmt(Stmt {
            kind: StmtKind::Class(class),
            span,
        }) = node
            && span.start <= error.span.start
            && error.span.start < span.end
        {
            for member in &class.members {
                match member {
                    Member::Ivar(ivar, ty, _) if ivar.trim_start_matches('@') == name => {
                        found = Some(ty.span);
                    }
                    Member::Property(property) => {
                        for (tok, ty) in &property.names {
                            if &text[tree.tokens[*tok].start..tree.tokens[*tok].end] == name
                                && let Some(ty) = ty
                            {
                                found = Some(ty.span);
                            }
                        }
                    }
                    _ => (),
                }
            }
        }
        true
    });
    found
}

/// A record the migration declared, indexed with a computed key: declare it
/// a dictionary of its fields' types.
fn dictionary(
    text: &str,
    tree: &Tree,
    sites: &[DefSite<'_>],
    error: &Diagnostic,
    written_param: &dyn Fn(usize, &str) -> bool,
    authored: &Authored<'_>,
) -> Option<Proposal> {
    let mut receiver = None;
    each_node(tree, &mut |node| {
        if let Node::Expr(expr) = node
            && let ExprKind::Index(inner, ..) = &expr.kind
            && expr.span.start <= error.span.start
            && error.span.end <= expr.span.end
        {
            receiver = Some(&**inner);
        }
        true
    });
    let ExprKind::Name(name) = &receiver?.kind else {
        return None;
    };
    // The local's typed declaration, or the parameter, in the function
    // around the index.
    let site = super::sites::def_containing(sites, error.span.start);
    let mut annotation = None;
    if let Some(site) = site
        && let Some(param) = site.def.params.iter().find(|p| p.name == *name)
    {
        let index = sites
            .iter()
            .position(|other| std::ptr::eq(other.def, site.def))?;
        if !written_param(index, name) {
            return None;
        }
        annotation = param.ty.as_ref().map(|ty| ty.span);
    } else {
        let scope = site.map_or(
            Span {
                start: 0,
                end: text.len(),
            },
            |site| site.span,
        );
        each_node(tree, &mut |node| {
            if let Node::Stmt(stmt) = node
                && let StmtKind::Assign(assign) = &stmt.kind
                && let [Target::Typed(inner, ty)] = assign.targets.as_slice()
                && let Target::Expr(Expr {
                    kind: ExprKind::Name(own),
                    ..
                }) = &**inner
                && own == name
                && scope.start <= stmt.span.start
                && stmt.span.end <= scope.end
                && annotation.is_none()
            {
                annotation = Some(ty.span);
            }
            true
        });
        let span = annotation?;
        if authored(name, &text[span.range()]) {
            return None;
        }
    }
    let span = annotation?;
    let Ty::Shape(fields, _) = Ty::parse(&text[span.range()])? else {
        return None;
    };
    let value = Ty::union(fields.into_iter().map(|field| field.ty));
    let value = if value == Ty::Never { Ty::Any } else { value };
    let rendered = Ty::Hash(Box::new(value)).render()?;
    Some(proposal(span, rendered))
}
