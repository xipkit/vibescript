//! Moves parameters used by name behind the keyword separator.

use super::sites::{self, Node};
use std::collections::HashMap;
use vibescript::surface::{edits::Edits, syntax::*};

pub(super) fn rewrite(source: &str) -> String {
    let Ok(tree) = vibescript::surface::parse::parse(source) else {
        return source.to_owned();
    };
    let definitions = sites::defs(&tree);
    let checked = vibescript::Engine::new().type_check(source).ok();
    let mut calls: HashMap<usize, Vec<&Call>> = HashMap::new();
    for stmt in &tree.body {
        sites::each_stmt(stmt, &mut |node| {
            if let Node::Expr(Expr {
                kind: ExprKind::Call(call),
                ..
            }) = node
            {
                let offset = tree.tokens[call.name_tok].start;
                let enclosing = definitions
                    .iter()
                    .filter(|site| site.span.start <= offset && offset < site.span.end)
                    .min_by_key(|site| site.span.end - site.span.start);
                let receiver = checked
                    .as_ref()
                    .and_then(|checked| checked.calls.receiver_at(offset));
                let target = if call.receiver.is_some() {
                    receiver.and_then(|receiver| {
                        let constructor = receiver.is("namespace") && call.name == "new";
                        let name = if constructor {
                            "initialize"
                        } else {
                            &call.name
                        };
                        definitions
                            .iter()
                            .enumerate()
                            .find(|(_, site)| {
                                site.owner == receiver.name()
                                    && site.def.name == name
                                    && (constructor
                                        || site.def.class_method == receiver.is("namespace"))
                            })
                            .map(|(id, _)| id)
                    })
                } else {
                    let method = enclosing.and_then(|enclosing| {
                        definitions
                            .iter()
                            .enumerate()
                            .find(|(_, site)| {
                                !site.owner.is_empty()
                                    && site.owner == enclosing.owner
                                    && site.def.class_method == enclosing.def.class_method
                                    && site.def.name == call.name
                            })
                            .map(|(id, _)| id)
                    });
                    method.or_else(|| {
                        definitions
                            .iter()
                            .enumerate()
                            .find(|(_, site)| site.owner.is_empty() && site.def.name == call.name)
                            .map(|(id, _)| id)
                    })
                };
                if let Some(target) = target {
                    calls.entry(target).or_default().push(call);
                }
            }
            true
        });
    }
    let mut edits = Edits::default();
    for (id, calls) in calls {
        let def = definitions[id].def;
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
