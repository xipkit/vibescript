//! Moves parameters used by name behind the keyword separator.

use super::sites::{self, Node};
use std::collections::HashMap;
use vibescript::surface::{edits::Edits, syntax::*};

pub(super) fn rewrite(source: &str) -> String {
    let Ok(tree) = vibescript::surface::parse::parse(source) else {
        return source.to_owned();
    };
    let definitions = sites::defs(&tree);
    let functions: HashMap<_, _> = definitions
        .iter()
        .filter(|site| site.owner.is_empty())
        .map(|site| (site.def.name.as_str(), site.def))
        .collect();
    let mut calls: HashMap<&str, Vec<&Call>> = HashMap::new();
    for stmt in &tree.body {
        sites::each_stmt(stmt, &mut |node| {
            if let Node::Expr(Expr {
                kind: ExprKind::Call(call),
                ..
            }) = node
            {
                if call.receiver.is_none() && functions.contains_key(call.name.as_str()) {
                    calls.entry(call.name.as_str()).or_default().push(call);
                }
            }
            true
        });
    }
    let mut edits = Edits::default();
    for (name, calls) in calls {
        let def = functions[name];
        if def.params.iter().any(|p| p.kind == ParamKind::Rest)
            || calls
                .iter()
                .flat_map(|call| call.args.iter())
                .flat_map(|args| &args.items)
                .any(|arg| matches!(arg.kind, ArgKind::Splat | ArgKind::KeywordSplat))
        {
            continue;
        }
        let positional: Vec<_> = def
            .params
            .iter()
            .filter(|p| p.kind == ParamKind::Positional)
            .collect();
        let first = calls
            .iter()
            .flat_map(|call| call.args.iter())
            .flat_map(|args| &args.items)
            .filter_map(|arg| match &arg.kind {
                ArgKind::Keyword(name) => positional.iter().position(|p| p.name == *name),
                _ => None,
            })
            .min();
        let Some(first) = first else { continue };
        let Some((open, close)) = def.parens else {
            continue;
        };
        let mut params: Vec<String> = positional[..first]
            .iter()
            .map(|p| source[p.span.range()].to_owned())
            .collect();
        params.push("*".to_owned());
        params.extend(
            positional[first..]
                .iter()
                .map(|p| source[p.span.range()].to_owned()),
        );
        params.extend(
            def.params
                .iter()
                .filter(|p| p.kind != ParamKind::Positional)
                .map(|p| source[p.span.range()].to_owned()),
        );
        if let Some(block) = def.block {
            params.push(source[block.range()].to_owned());
        }
        edits.text(
            Span {
                start: tree.tokens[open].end,
                end: tree.tokens[close].start,
            },
            params.join(", "),
        );
        for call in calls {
            let Some(args) = &call.args else { continue };
            for (index, arg) in args
                .items
                .iter()
                .filter(|arg| arg.kind == ArgKind::Positional)
                .enumerate()
            {
                if index >= first {
                    if let Some(param) = positional.get(index) {
                        edits.insert(arg.span.start, format!("{}: ", param.name));
                    }
                }
            }
        }
    }
    edits.apply(source)
}
