//! Static inference with the ADR-007 checker as the oracle.
//!
//! The checker never infers a function's result or parameter types; it
//! checks values against declared ones and reports each mismatch with the
//! type it found. A probe declares the types to infer as fresh classes no
//! value belongs to, so every exit of a probed function, and every argument
//! a call passes to a probed parameter, is reported with its own type.

use super::{
    sites::{DefSite, def_containing, defs},
    ty::Ty,
};
use std::collections::HashMap;
use vibescript::{Engine, diagnostic::Diagnostic, surface::syntax::*};

/// What to infer: results and parameters, by function index in
/// [`defs`] order.
#[derive(Default)]
pub(crate) struct Request {
    pub results: Vec<usize>,
    pub params: Vec<(usize, String)>,
}

/// The types a probe found. A site absent from a map was never reached by
/// a typed value; a value of another probed site counts as the type that
/// site declared.
#[derive(Default, Debug)]
pub(crate) struct Found {
    pub results: HashMap<usize, Vec<Ty>>,
    pub params: HashMap<(usize, String), Vec<Ty>>,
}

/// Runs one probe of `text`, whose syntax is `tree`.
pub(crate) fn probe(engine: &Engine, text: &str, tree: &Tree, request: &Request) -> Found {
    let sites = defs(tree);
    let base = unused_name(text, "MigrateProbe");
    let mut edits: Vec<(Span, String)> = Vec::new();
    let mut names: Vec<String> = Vec::new();
    // What each probed annotation said, for values that come from another
    // probed function or parameter.
    let mut declared: HashMap<String, Ty> = HashMap::new();
    let mut result_of: HashMap<String, usize> = HashMap::new();
    let mut param_of: HashMap<String, (usize, String)> = HashMap::new();
    for &index in &request.results {
        let Some((_, ty)) = &sites[index].def.result else {
            continue;
        };
        let name = format!("{base}{}", names.len());
        edits.push((ty.span, name.clone()));
        result_of.insert(name.clone(), index);
        declared.insert(
            name.clone(),
            Ty::parse(&text[ty.span.range()]).unwrap_or(Ty::Any),
        );
        names.push(name);
    }
    for (index, param) in &request.params {
        let Some(ty) = sites[*index]
            .def
            .params
            .iter()
            .find(|p| p.name == *param)
            .and_then(|p| p.ty.as_ref())
        else {
            continue;
        };
        let name = format!("{base}{}", names.len());
        edits.push((ty.span, name.clone()));
        param_of.insert(name.clone(), (*index, param.clone()));
        declared.insert(
            name.clone(),
            Ty::parse(&text[ty.span.range()]).unwrap_or(Ty::Any),
        );
        names.push(name);
    }
    if edits.is_empty() {
        return Found::default();
    }
    edits.sort_by_key(|(span, _)| span.start);
    let mut probe = String::with_capacity(text.len() + 64 * names.len());
    let mut cursor = 0;
    for (span, name) in &edits {
        if span.start < cursor {
            return Found::default();
        }
        probe.push_str(&text[cursor..span.start]);
        probe.push_str(name);
        cursor = span.end;
    }
    probe.push_str(&text[cursor..]);
    probe.push('\n');
    for name in &names {
        probe.push_str(&format!("class {name}\nend\n"));
    }
    let Ok(checked) = engine.type_check(&probe) else {
        return Found::default();
    };
    let Ok(probe_tree) = vibescript::surface::parse::parse(&probe) else {
        return Found::default();
    };
    let probe_sites = defs(&probe_tree);
    if probe_sites.len() < sites.len() {
        return Found::default();
    }
    let mut found = Found::default();
    let mut unresolved_results = Vec::new();
    let mut unresolved_params = Vec::new();
    for diagnostic in &checked.diagnostics {
        let (Some(expected), Some(text)) = (&diagnostic.expected, &diagnostic.found) else {
            continue;
        };
        if diagnostic.file.is_some() {
            continue;
        }
        // A value of another probed site has the type it was declared.
        let ty = Ty::parse(text).map(|ty| {
            names
                .iter()
                .filter(|name| *name != expected)
                .fold(ty, |ty, name| ty.substitute(name, &declared[name]))
        });
        if let Some(&index) = result_of.get(expected.as_str()) {
            if !returns(diagnostic, &sites[index]) {
                continue;
            }
            // A value inside the probed function, as a nested function's.
            let inside = def_containing(&probe_sites, diagnostic.span.start)
                .is_some_and(|site| std::ptr::eq(site.def, probe_sites[index].def));
            if !inside {
                continue;
            }
            match ty {
                // A recursive call's result adds nothing.
                Some(ty) if ty.mentions(expected) => (),
                Some(ty) => found.results.entry(index).or_default().push(ty),
                None => unresolved_results.push(index),
            }
        } else if let Some(site) = param_of.get(expected.as_str()) {
            if !argument(diagnostic, &site.1) {
                continue;
            }
            match ty {
                Some(ty) if !ty.mentions(expected) => {
                    found.params.entry(site.clone()).or_default().push(ty)
                }
                _ => unresolved_params.push(site.clone()),
            }
        }
    }
    for index in unresolved_results {
        found.results.remove(&index);
    }
    for site in unresolved_params {
        found.params.remove(&site);
    }
    found
}

/// Whether a diagnostic reports a value a function returns. Methods are
/// named with their class, as `Invoice#total`.
fn returns(diagnostic: &Diagnostic, site: &DefSite<'_>) -> bool {
    let Some(rest) = diagnostic.message.strip_prefix('`') else {
        return false;
    };
    let Some((name, _)) = rest.split_once("` returns ") else {
        return false;
    };
    name.rsplit(['#', '.']).next() == Some(site.def.name.as_str())
}

/// Whether a diagnostic reports an argument passed to the parameter `name`.
fn argument(diagnostic: &Diagnostic, name: &str) -> bool {
    let message = &diagnostic.message;
    (message.starts_with("argument ") && message.contains(&format!(" (`{name}`) of ")))
        || message.starts_with(&format!("keyword `{name}:` of "))
}

/// A class name that `text` does not use.
fn unused_name(text: &str, base: &str) -> String {
    let mut name = base.to_owned();
    while text.contains(&name) {
        name.push('X');
    }
    name
}
