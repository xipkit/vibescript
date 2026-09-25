//! The rewrite of each removed spelling. Each rule reads one construct,
//! records its edits as a group, or reports a [`Finding`] when the rewrite
//! is not safe where the construct stands.

use super::{
    Access, Finding, Reason, Rule,
    context::{
        Place, namespace_member_takes_no_arguments, namespace_name, simple, string_literal,
        symbol_literal,
    },
    edits::Piece,
    hooks::{Hooks, Test},
    patterns::{ArgPattern, Callee, Change, KeywordValue, Pattern, TemplatePiece, patterns},
    probe::{member_without_parens, namespace_without_parens},
    syntax::*,
};
use crate::tooling::TokenKind;
use std::collections::{HashMap, HashSet};

/// The arguments a rename pattern captured.
#[derive(Default)]
pub struct Captures {
    /// Each `$name` and the span of the argument it matched.
    pub values: HashMap<String, Span>,
    /// The spans of the arguments `...` matched.
    pub rest: Vec<Span>,
}

/// The canonical surface's rules, for every [`Hooks`] implementor.
pub trait Rules<'a>: Hooks<'a> {
    /// A global function renamed without parentheses, such as `now`.
    fn bare_name(&mut self, expr: &'a Expr, name: &str, place: Place) {
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

    /// Rewrites a percent literal as an array literal.
    fn words(&mut self, expr: &'a Expr, place: Place) {
        let tok = self.token_at(expr.span.start);
        let TokenKind::Words { symbols, entries } = &self.tokens[tok].kind else {
            return;
        };
        let removed = excerpt(self.text(expr.span));
        let mut items = Vec::new();
        for entry in entries {
            let Some(bytes) = entry else {
                let finding = Finding::new(
                    Reason::Syntax,
                    expr.span,
                    "an interpolated percent literal needs rewriting as an array literal by hand",
                )
                .spelling(
                    Rule::PercentLiteral,
                    removed,
                    "write an array literal, interpolating in its strings",
                );
                self.report(finding);
                return;
            };
            let Some(item) = (if *symbols {
                symbol_literal(bytes)
            } else {
                string_literal(bytes)
            }) else {
                let finding = Finding::new(
                    Reason::Syntax,
                    expr.span,
                    "a percent literal entry has bytes a string literal cannot spell",
                )
                .spelling(Rule::PercentLiteral, removed, "write an array literal");
                self.report(finding);
                return;
            };
            items.push(item);
        }
        // A percent literal after a command name is an argument, which an
        // array literal after a space is as well.
        let _ = place;
        let literal = format!("[{}]", items.join(", "));
        let previous = self.enter(
            Rule::PercentLiteral,
            expr.span,
            removed,
            format!("use the array literal `{}`", excerpt(&literal)),
        );
        let group = self.rewrites.len() - 1;
        self.words.insert(expr.span, group);
        self.edits.text(expr.span, literal);
        self.leave(previous);
    }

    /// Rewrites `h[:name]` as `h["name"]` when the receiver is a hash.
    fn symbol_keys(&mut self, expr: &'a Expr, selectors: &'a [Expr]) {
        let [selector] = selectors else {
            return;
        };
        if !matches!(selector.kind, ExprKind::Symbol) {
            return;
        }
        let ExprKind::Index(_, open, ..) = &expr.kind else {
            return;
        };
        let offset = self.tokens[*open].start;
        match self.index_is_hash(offset) {
            Some(false) => return,
            None if self.declared.methods.contains("[]")
                || self.declared.methods.contains("[]=") =>
            {
                return;
            }
            _ => (),
        }
        let TokenKind::Symbol { name, .. } = &self.tokens[self.token_at(selector.span.start)].kind
        else {
            return;
        };
        let removed = self.text(selector.span);
        match string_literal(name) {
            Some(literal) => {
                let advice = format!("hash keys are strings: `{literal}`");
                let previous = self.enter(Rule::SymbolKey, selector.span, removed, advice);
                self.edits.text(selector.span, literal);
                self.leave(previous);
            }
            None => {
                let finding = Finding::removed(
                    Rule::SymbolKey,
                    selector.span,
                    removed,
                    "hash keys are strings",
                );
                self.report(finding);
            }
        }
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
        self.receiver_plain(expr, call)
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
        let Change::Template(pieces) = &pattern.rewrite else {
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
            Callee::Member => pattern
                .target_member()
                .is_none_or(|member| member_without_parens(&pattern.receiver, member)),
            Callee::Namespace(namespace) => {
                let Some((target_namespace, member)) = text.split_once('.') else {
                    return true;
                };
                let parenthesized = call.args.is_some();
                parenthesized
                    || (namespace_without_parens(namespace, &pattern.name)
                        && namespace_without_parens(target_namespace, member))
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
            return namespace_name(&kind) || member_without_parens(&kind, &call.name);
        }
        let Some(kinds) = self.bare_call_kinds(expr, call) else {
            return false;
        };
        kinds
            .iter()
            .all(|kind| member_without_parens(kind, &call.name))
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
            } else if self.new_syntax() {
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
                || (!self.new_syntax() && !namespace_without_parens(namespace, &call.name))
            {
                return;
            }
        } else if !self.parens_optional(expr, call) {
            return;
        }
        if !self.call_returned(expr, call) {
            return;
        }
        if !self.new_syntax() && !self.bare_call_works(expr, call) {
            return;
        }
        let span = Span {
            start: self.tokens[*open].start,
            end: self.tokens[*close].end,
        };
        let previous = self.enter(
            Rule::EmptyParentheses,
            span,
            format!("{}()", call.name),
            format!(
                "a call without arguments has no parentheses: `{}`",
                call.name
            ),
        );
        self.edits.text(span, "");
        self.leave(previous);
    }

    /// Replaces `send(:name, ...)` and `public_send(:name, ...)` with a direct call.
    fn dispatch(&mut self, expr: &'a Expr, call: &'a Call) {
        if !matches!(call.name.as_str(), "send" | "public_send" | "respond_to?") {
            return;
        }
        let Some(receiver) = &call.receiver else {
            return;
        };
        let observed = self.receiver_dynamic(expr, call);
        if observed == Some(true) {
            // A host object's own `send`, such as a capability's.
            return;
        }
        let span = self.token_span(call.name_tok);
        let direct = "call the member directly, or use `case` over the name";
        if call.name == "respond_to?" {
            let finding = Finding::new(
                Reason::Dispatch,
                span,
                "respond_to? is removed; use case over the name or is_type?",
            )
            .spelling(
                Rule::Dispatch,
                "respond_to?",
                "use `case` over the name, or `is_type?`",
            );
            self.report(finding);
            return;
        }
        // A method the source defines under the name is its own.
        let removed = !self.declared.methods.contains(&call.name);
        let Some(args) = &call.args else {
            if removed {
                self.report(Finding::removed(Rule::Dispatch, span, &call.name, direct));
            }
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
            super::context::method_name(name)
                && !matches!(name.as_str(), "send" | "public_send" | "respond_to?")
        };
        let Some(name) = symbol.filter(dispatch_name) else {
            if observed.is_some() || first.is_some_and(|arg| arg.kind == ArgKind::Positional) {
                let finding = Finding::new(
                    Reason::Dispatch,
                    span,
                    format!(
                        "{} with a name known only at runtime is removed; call the member, or use case over the name",
                        call.name
                    ),
                )
                .spelling(Rule::Dispatch, &call.name, direct);
                self.report(finding);
            } else if removed {
                self.report(Finding::removed(Rule::Dispatch, span, &call.name, direct));
            }
            return;
        };
        let call_directly = format!("call `{name}` directly");
        if !self.call_returned(expr, call) {
            let finding = Finding::new(
                Reason::Dispatch,
                span,
                format!(
                    "{} raised in a recorded run, and a direct call can report the error differently; call {name} directly by hand",
                    call.name
                ),
            )
            .spelling(Rule::Dispatch, &call.name, call_directly);
            self.report(finding);
            return;
        }
        if self.declared.private_methods.contains(&name) {
            let finding = Finding::new(
                Reason::Dispatch,
                span,
                format!(
                    "{} reaches the private method {name}; call it from inside its class",
                    call.name
                ),
            )
            .spelling(
                Rule::Dispatch,
                &call.name,
                format!("{name} is private; call it from inside its class"),
            );
            self.report(finding);
            return;
        }
        let rest: Vec<&Arg> = args.items.iter().skip(1).collect();
        if rest.iter().any(|arg| arg.kind != ArgKind::Positional) {
            // Dispatch passes keywords as an options hash, and a direct call does not.
            let finding = Finding::new(
                Reason::Dispatch,
                span,
                format!("{} with keyword or splat arguments binds them differently from a direct call; call {name} by hand", call.name),
            )
            .spelling(Rule::Dispatch, &call.name, call_directly);
            self.report(finding);
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
        let previous = self.enter(Rule::Dispatch, span, &call.name, call_directly);
        self.edits.replace(
            Span {
                start: expr.span.start,
                end,
            },
            pieces,
        );
        self.leave(previous);
    }

    /// Rewrites `Hash.new` as `{}`.
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
        let advice = "write `{}` with a declared type, such as `counts: hash<string, int> = {}`";
        if call.argument_count() > 0 || call.block.is_some() {
            let finding = Finding::new(
                Reason::HashNew,
                expr.span,
                "Hash.new with a default is removed; write {} with a declared type and handle missing keys with fetch",
            )
            .spelling(
                Rule::HashNew,
                "Hash.new",
                "write `{}` with a declared type, and read missing keys with `fetch`",
            );
            self.report(finding);
            return;
        }
        // Without parentheses, `Hash.new` as a receiver is not a call.
        if place == Place::Tight && call.args.is_none() {
            self.report(Finding::removed(
                Rule::HashNew,
                expr.span,
                "Hash.new",
                advice,
            ));
            return;
        }
        // `Hash.new` may read a field a script stored on the namespace.
        if self.namespace_field(expr) {
            return;
        }
        let text = if place == Place::Tight { "({})" } else { "{}" };
        let previous = self.enter(Rule::HashNew, expr.span, "Hash.new", advice);
        self.edits.text(expr.span, text);
        self.leave(previous);
    }

    /// Rewrites symbols naming a required module as strings, and reports a
    /// `require` whose names are not literals.
    fn require(&mut self, call: &'a Call) {
        if call.receiver.is_some() || call.name != "require" {
            return;
        }
        let Some(args) = &call.args else {
            return;
        };
        let span = self.token_span(call.name_tok);
        for arg in &args.items {
            if matches!(arg.value.kind, ExprKind::Symbol)
                && let TokenKind::Symbol { name, .. } =
                    &self.tokens[self.token_at(arg.value.span.start)].kind
                && let Some(literal) = string_literal(name)
            {
                let removed = self.text(arg.value.span);
                let what = match &arg.kind {
                    ArgKind::Keyword(name) if name == "as" => "alias",
                    _ => "module",
                };
                let advice = format!("name the {what} with the string `{literal}`");
                let previous = self.enter(Rule::Require, arg.value.span, removed, advice);
                self.edits.text(arg.value.span, literal);
                self.leave(previous);
                continue;
            }
            let literal = matches!(arg.value.kind, ExprKind::Str);
            match &arg.kind {
                ArgKind::Positional | ArgKind::Keyword(_) if literal => (),
                ArgKind::Keyword(name) if name != "as" => (),
                _ => {
                    self.report(Finding::new(
                        Reason::Require,
                        span,
                        "require takes string literals; write the module name and alias as literals",
                    ));
                    return;
                }
            }
        }
    }

    /// Rewrites a hash field read or written with a dot, `h.name`, as the
    /// index `h["name"]`: dot calls methods only. A read, or an update that
    /// reads first, applies where the receiver is a hash, the name is not a
    /// hash method and every hash held data under it; a write, which always
    /// sets a field, wherever the receiver is a hash. Returns whether it
    /// reported the access.
    fn field_access(&mut self, expr: &'a Expr, call: &'a Call, access: Access) -> bool {
        let (Some(receiver), Some(operator)) = (&call.receiver, call.operator) else {
            return false;
        };
        let reads = matches!(access, Access::Read | Access::Update);
        if call.args.is_some()
            || call.block.is_some()
            || call.scoped(self.tokens)
            || (reads && super::context::hash_method(&call.name))
        {
            return false;
        }
        // A braced literal of names, such as `{ x: int }`, may be a type.
        let typed_literal = |mut literal: &Expr| loop {
            match &literal.kind {
                ExprKind::Group(_, inner, _) => literal = inner,
                ExprKind::Hash(entries) => {
                    break entries.iter().all(|entry| {
                        entry.shorthand || matches!(entry.value.kind, ExprKind::Name(_))
                    });
                }
                _ => break false,
            }
        };
        let kinds = match self.receiver_kinds(expr, call) {
            Some(kinds) if !kinds.is_empty() => kinds,
            _ if typed_literal(receiver) => Vec::new(),
            _ => self.static_kind(receiver).into_iter().collect(),
        };
        let hashes = kinds.iter().filter(|kind| *kind == "hash").count();
        // A host or module object answers a dot with its own members.
        let object = kinds.iter().any(|kind| kind == "object");
        let Some(key) = string_literal(call.name.as_bytes()).filter(|_| hashes > 0 && !object)
        else {
            return false;
        };
        let span = Span {
            start: self.tokens[operator].start,
            end: self.tokens[call.name_tok].end,
        };
        let removed = self.text(span).to_owned();
        let receiver_text = excerpt(self.text(receiver.span));
        let indexed = format!("`{receiver_text}[{key}]`");
        // The index joins its receiver, so nothing but space may part them.
        let between = self.text(Span {
            start: receiver.span.end,
            end: span.start,
        });
        // A brace or keyword expression followed by `[` can read as a
        // separate statement, as `{ a: 1 }` at the start of one does, so it
        // is parenthesized; a parenthesized group that spans lines is
        // followed by a new expression, not an index.
        let wrap = matches!(
            receiver.kind,
            ExprKind::Hash(_)
                | ExprKind::If(_)
                | ExprKind::Case(_)
                | ExprKind::Begin(_)
                | ExprKind::Loop(_)
                | ExprKind::BlockCall(..)
        );
        let multiline_group = (wrap || matches!(receiver.kind, ExprKind::Group(..)))
            && self.text(receiver.span).contains('\n');
        let by_hand = if hashes < kinds.len() {
            Some(format!(
                "{removed} reaches a hash field on a value that is not always a hash; index the field by hand where it is one"
            ))
        } else if call.safe(self.tokens) {
            Some(format!(
                "{removed} reaches a hash field through `&.`; test for nil and index the field by hand"
            ))
        } else if access == Access::Destructure {
            Some(format!(
                "{removed} is destructured into, which an index cannot be; assign the field by hand"
            ))
        } else if multiline_group {
            Some(format!(
                "{removed} follows a receiver that spans lines, after which `[` starts a new expression; index the field by hand"
            ))
        } else if between.contains('#') {
            Some(format!(
                "a comment parts {removed} from its receiver; index the field by hand"
            ))
        } else if reads && !self.field_holds_data(expr, call) {
            // A dot read raises at a missing field and calls a function,
            // where the index reads nil or the function.
            Some(format!(
                "{removed} read a missing field or a function in a recorded run, which the index reads differently; index the field by hand"
            ))
        } else {
            None
        };
        if let Some(message) = by_hand {
            let finding = Finding::new(Reason::Receiver, span, message).spelling(
                Rule::FieldAccess,
                removed,
                format!("hash fields are indexed, as in {indexed}, once the value is a hash"),
            );
            self.report(finding);
            return true;
        }
        let advice = format!("hash fields are indexed: {indexed}");
        let previous = self.enter(Rule::FieldAccess, span, removed, advice);
        if wrap {
            self.edits.wrap(receiver.span, "(", ")");
        }
        self.edits.text(
            Span {
                start: receiver.span.end,
                end: span.end,
            },
            format!("[{key}]"),
        );
        self.leave(previous);
        true
    }

    /// Moves keyword parameters declared in a removed form after a bare
    /// `*`: `retries: 3` becomes `*, retries: int = 3`, `name:` becomes
    /// `*, name: T` and `name: T:` becomes `*, name: T`, with the type the
    /// hooks give for an untyped one.
    fn keyword_params(&mut self, def: &'a Def) {
        if !self.new_syntax() {
            return;
        }
        let old: Vec<&'a Param> = def
            .params
            .iter()
            .filter(|param| param.keyword_colon.is_some())
            .collect();
        let (Some(first), Some(last)) = (old.first(), old.last()) else {
            return;
        };
        let span = Span {
            start: first.span.start,
            end: last.span.end,
        };
        let types: Vec<Option<String>> = old
            .iter()
            .map(|param| match param.ty {
                Some(_) => None,
                None => self.keyword_type(def, param),
            })
            .collect();
        let index = def
            .params
            .iter()
            .position(|param| param.kind == ParamKind::Keyword)
            .unwrap_or(0);
        let star = def.star.is_none()
            && !def.params[..index]
                .iter()
                .any(|param| param.kind == ParamKind::Rest);
        let mut canonical = Vec::new();
        for (param, ty) in old.iter().zip(&types) {
            let ty = match &param.ty {
                Some(ty) => Some(self.text(ty.span)),
                None => ty.as_deref(),
            };
            let mut text = param.name.clone();
            if let Some(ty) = ty {
                text.push_str(": ");
                text.push_str(ty);
            }
            if let Some(default) = &param.default {
                text.push_str(" = ");
                text.push_str(self.text(default.span));
            }
            canonical.push(text);
        }
        let canonical = format!("{}{}", if star { "*, " } else { "" }, canonical.join(", "));
        let removed = excerpt(self.text(span));
        let advice = format!(
            "keyword parameters follow a bare `*`: `{}`",
            excerpt(&canonical)
        );
        let previous = self.enter(Rule::KeywordParameter, span, removed, advice);
        if star {
            self.edits.insert(def.params[index].span.start, "*, ");
        }
        for (param, ty) in old.iter().zip(types) {
            let colon = self.token_span(param.keyword_colon.expect("a removed keyword form"));
            let name_end = self.tokens[param.name_tok].end;
            let declared = ty.map(|ty| format!(": {ty}")).unwrap_or_default();
            match (&param.ty, &param.default) {
                (Some(ty), _) => self.edits.text(
                    Span {
                        start: ty.span.end,
                        end: colon.end,
                    },
                    "",
                ),
                (None, Some(default)) => self.edits.text(
                    Span {
                        start: name_end,
                        end: default.span.start,
                    },
                    format!("{declared} = "),
                ),
                (None, None) => self.edits.text(
                    Span {
                        start: name_end,
                        end: colon.end,
                    },
                    declared,
                ),
            }
        }
        self.leave(previous);
    }

    /// Lowercases builtin type names and spells `object` as `hash`, unless
    /// a check of the annotation failed in a recorded run, whose error
    /// message quotes its spelling.
    fn type_names_checked(&mut self, ty: &'a TypeExpr, passed: bool) {
        if passed {
            self.type_names(ty);
        } else if self.spelled_differently(ty) {
            let finding = Finding::new(
                Reason::Rename,
                ty.span,
                "this annotation failed a check in a recorded run, and its error quotes the old type spelling; respell it by hand",
            )
            .spelling(
                Rule::TypeName,
                self.text(ty.span),
                "type names are lowercase, and `object` is `hash`",
            );
            self.report(finding);
        }
    }

    /// Whether a builtin type name in the annotation is spelled other than
    /// canonically.
    fn spelled_differently(&self, ty: &TypeExpr) -> bool {
        match &ty.kind {
            TypeKind::Named(tok, args) => {
                let written = self.token_text(*tok).trim_end_matches('?');
                let lower = written.to_ascii_lowercase();
                (super::parse::respelled_type(&lower) && (lower != written || lower == "object"))
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

    /// Lowercases builtin type names and spells `object` as `hash`.
    fn type_names(&mut self, ty: &'a TypeExpr) {
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
                    let span = self.token_span(*tok);
                    let finding = Finding::new(
                        Reason::Rename,
                        span,
                        format!("type names are lowercase, but {name} as `nil` would read as a keyword default here; respell it by hand"),
                    )
                    .spelling(
                        Rule::TypeName,
                        name,
                        "type names are lowercase; `nil` here would read as a keyword default",
                    );
                    self.report(finding);
                }
                if super::parse::respelled_type(&lower) && lower != "nil" {
                    let canonical = if lower == "object" { "hash" } else { &lower };
                    if canonical != name {
                        let span = self.token_span(*tok);
                        let advice = if lower == "object" {
                            "use `hash`".to_owned()
                        } else {
                            format!("type names are lowercase: `{canonical}`")
                        };
                        let previous = self.enter(Rule::TypeName, span, name, advice);
                        self.edits.text(span, format!("{canonical}{suffix}"));
                        self.leave(previous);
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

    /// The kinds a receiver's values have, as the rename table names them:
    /// from the syntax when it decides, and otherwise from the hooks.
    fn kinds_of(&self, expr: &'a Expr, call: &'a Call) -> Option<Vec<String>> {
        let receiver = call.receiver.as_ref()?;
        if let Some(kind) = self.static_kind(receiver) {
            return Some(vec![kind]);
        }
        self.receiver_kinds(expr, call)
    }

    /// Applies the rename table to a call; returns whether it rewrote it.
    fn rename(&mut self, expr: &'a Expr, call: &'a Call, place: Place) -> bool {
        if matches!(call.name.as_str(), "eql?" | "equal?")
            && let Some(receiver) = &call.receiver
        {
            let span = self.token_span(call.name_tok);
            let mut finding = Finding::new(
                Reason::Rename,
                span,
                format!(
                    "{} is removed; == compares values, which differs where {} compared types or identity, so choose by hand",
                    call.name, call.name
                ),
            )
            .spelling(
                Rule::Equality,
                &call.name,
                "use `==`, which compares values; check that no comparison of types or identity was meant",
            );
            if let Some(args) = &call.args
                && let [arg] = args.items.as_slice()
                && arg.kind == ArgKind::Positional
                && call.block.is_none()
                && !call.safe(self.tokens)
            {
                let wrap = |text: &str, expr: &Expr| {
                    if super::context::primary(expr) {
                        text.to_owned()
                    } else {
                        format!("({text})")
                    }
                };
                let mut text = format!(
                    "{} == {}",
                    wrap(self.text(receiver.span), receiver),
                    wrap(self.text(arg.value.span), &arg.value)
                );
                if place == Place::Tight {
                    text = format!("({text})");
                }
                finding.suggestion = vec![(expr.span, text)];
            }
            self.report(finding);
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
        let span = self.token_span(call.name_tok);
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
            if !self.call_returned(expr, call) {
                return false;
            }
            return self.apply_pattern(expr, Some(call), pattern, &captures, place);
        }
        let members: Vec<&Pattern> = candidates
            .into_iter()
            .filter(|p| p.callee == Callee::Member)
            .collect();
        if members.is_empty() {
            return false;
        }
        let choose = |kind: &str, rules: &Self| -> Option<(&'static Pattern, Captures)> {
            patterns()
                .iter()
                .filter(|p| p.callee == Callee::Member && p.name == call.name)
                .filter(|p| p.receiver == kind || p.receiver == "T")
                .find_map(|p| rules.match_args(call, p).map(|c| (p, c)))
        };
        // A hash field of the member's name answers the call instead. A
        // rescued error's fields are its members.
        let error = self.static_kind(receiver).as_deref() == Some("error");
        if !error && self.receiver_field(expr, call) {
            let finding = Finding::new(
                Reason::Receiver,
                span,
                format!(
                    "{} reads a hash field of that name here; rewrite it by hand",
                    call.name
                ),
            );
            self.report(finding);
            return false;
        }
        let kinds = self.kinds_of(expr, call);
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
                        let finding = Finding::new(
                            Reason::Receiver,
                            span,
                            format!(
                                "{} is renamed for some of its receivers' types ({}) but not others; rewrite it by hand",
                                call.name,
                                kinds.join(", ")
                            ),
                        )
                        .spelling(
                            Rule::Name,
                            &call.name,
                            format!(
                                "it is removed for some of the receiver's types ({}) but not others; call a member every type has",
                                kinds.join(", ")
                            ),
                        );
                        self.report(finding);
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
                    let choices: Vec<String> = matched
                        .iter()
                        .map(|(p, _)| format!("{} on {}", p.advice(), p.receiver))
                        .collect();
                    let finding = Finding::new(
                        Reason::Receiver,
                        span,
                        format!(
                            "{} is renamed depending on its receiver's type ({}), which no recorded run observed; rewrite it by hand",
                            call.name,
                            receivers.join(", ")
                        ),
                    )
                    .spelling(Rule::Name, &call.name, choices.join(", "));
                    self.report(finding);
                    return false;
                }
                Some(matched.into_iter().next().unwrap())
            }
        };
        let Some((pattern, captures)) = decision else {
            return false;
        };
        if !self.call_returned(expr, call) && matches!(pattern.rewrite, Change::Template(_)) {
            let finding = Finding::new(
                Reason::Rename,
                span,
                format!(
                    "{} is removed, and it raised in a recorded run, where its replacement would report a different error; rewrite it by hand",
                    call.name
                ),
            )
            .spelling(rule_of(pattern), &call.name, pattern.advice());
            self.report(finding);
            return false;
        }
        self.apply_pattern(expr, Some(call), pattern, &captures, place)
    }

    /// The arguments of `call` that `pattern` captures, if it matches.
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
    fn apply_pattern(
        &mut self,
        expr: &'a Expr,
        call: Option<&'a Call>,
        pattern: &Pattern,
        captures: &Captures,
        place: Place,
    ) -> bool {
        let span = call.map_or(expr.span, |call| self.token_span(call.name_tok));
        let rule = rule_of(pattern);
        let advice = pattern.advice();
        let pieces = match &pattern.rewrite {
            Change::Manual(hint) => {
                let finding = Finding::new(
                    Reason::Rename,
                    span,
                    format!("{} is removed; {hint}", pattern.name),
                )
                .spelling(rule, &pattern.name, hint);
                self.report(finding);
                return false;
            }
            Change::Template(pieces) => pieces,
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
            let finding = Finding::new(
                Reason::Rename,
                span,
                format!(
                    "{} is removed, and its replacement differs here; rewrite it by hand",
                    pattern.name
                ),
            )
            .spelling(
                rule,
                &pattern.name,
                format!("{advice}, where it behaves the same"),
            );
            self.report(finding);
            return false;
        }
        if pattern.receiver_uses() > 1 && !receiver.is_some_and(simple) {
            let finding = Finding::new(
                Reason::Rename,
                span,
                format!(
                    "{} is removed; its replacement repeats the receiver, so bind it to a local first",
                    pattern.name
                ),
            )
            .spelling(
                rule,
                &pattern.name,
                format!("{advice}, binding the receiver to a local first"),
            );
            self.report(finding);
            return false;
        }
        if !self.accepts_rewrite(pattern, call) {
            return false;
        }
        if !self.bare_replacement_works(pattern, call) {
            self.report(Finding::removed(rule, span, &pattern.name, advice));
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
            let finding = Finding::new(
                Reason::Rename,
                span,
                format!("{} is removed, and here its replacement would behave differently; rewrite it by hand", pattern.name),
            )
            .spelling(rule, &pattern.name, format!("{advice}, where it behaves the same"));
            self.report(finding);
            return false;
        }
        let safe = call.is_some_and(|call| call.safe(self.tokens));
        let plain_member = matches!(pieces.as_slice(), [TemplatePiece::Receiver, TemplatePiece::Text(text), ..] if text.starts_with('.'));
        if safe && !plain_member {
            // `x&.m` skips nil, which an operator replacement would not.
            self.report(Finding::removed(rule, span, &pattern.name, advice));
            return false;
        }
        if pattern.name == "is_a?" || pattern.name == "kind_of?" || pattern.name == "instance_of?" {
            let Some(type_span) = captures.values.get("type") else {
                return false;
            };
            let text = self.text(*type_span);
            if !text.chars().next().is_some_and(char::is_uppercase)
                || !text.chars().all(|c| c.is_alphanumeric() || c == '_')
            {
                let finding = Finding::new(
                    Reason::Rename,
                    span,
                    format!(
                        "{} is removed; use is_type? with the type's name as a symbol",
                        pattern.name
                    ),
                )
                .spelling(
                    rule,
                    &pattern.name,
                    "use `is_type?` with the type's name as a symbol",
                );
                self.report(finding);
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
                    let wrap =
                        operator && value.is_some_and(|value| !super::context::primary(value));
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
        let previous = self.enter(rule, span, &pattern.name, advice);
        self.edits.replace(expr.span, pieces);
        self.leave(previous);
        true
    }

    /// Converts a `do ... end` block to braces, keeping the call it attaches to.
    fn braces(&mut self, block: &'a Block, owner: Option<&'a Call>, callee_end: usize) {
        if block.brace {
            return;
        }
        let open = &self.tokens[block.open];
        let open_span = self.token_span(block.open);
        let advice = "write the block with braces";
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
            let finding = Finding::new(
                Reason::Syntax,
                open_span,
                "this do block's call passes bare keywords, which parentheses would bind differently; convert it to braces by hand",
            )
            .spelling(
                Rule::DoBlock,
                "do ... end",
                "write the block with braces, giving its call parentheses; its bare keywords then bind as keyword arguments",
            );
            self.report(finding);
            return;
        }
        let previous_group = self.enter(Rule::DoBlock, open_span, "do ... end", advice);
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
            self.edits.text(open_span, "{");
        }
        let close = self.token_span(block.close);
        self.edits.text(close, "}");
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
        self.leave(previous_group);
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
                let span = self.token_span(*op);
                self.edits.text(span, text);
            }
            ExprKind::Unary(op, operand)
                if self.token_text(*op) == "!" && self.primary(operand) =>
            {
                let span = Span {
                    start: self.tokens[*op].start,
                    end: operand.span.start,
                };
                self.edits.text(span, "");
            }
            _ if self.primary(expr) => self.edits.insert(expr.span.start, "!"),
            _ => self.edits.wrap(expr.span, "!(", ")"),
        }
    }

    /// Appends a comparison, such as ` != nil`, to a condition.
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

    /// Makes a condition test a `bool`, negating it for `unless` and
    /// `until` into the rewrite of their keyword.
    fn apply_test(&mut self, expr: &'a Expr, test: Test, negate: bool) {
        let tight = !self.primary(expr);
        let previous = negate.then(|| {
            let group = self.negation;
            self.edits.enter(group)
        });
        match (test, negate) {
            (Test::Bool, false) => (),
            (Test::Bool, true) => self.negate_bool(expr),
            (Test::Present, false) => self.compare(expr, tight, " != nil"),
            (Test::Present, true) => self.compare(expr, tight, " == nil"),
            (Test::True, false) => self.compare(expr, tight, " == true"),
            (Test::True, true) => self.compare(expr, tight, " != true"),
            (Test::Unknown(why), _) => {
                self.report(Finding::new(
                    Reason::Condition,
                    expr.span,
                    format!("this condition must be a bool, and {why}; compare it explicitly"),
                ));
                if negate {
                    self.negate_bool(expr);
                }
            }
        }
        if let Some(previous) = previous {
            self.edits.enter(previous);
        }
    }

    /// `!x` on a value that is not a bool.
    fn negation(&mut self, expr: &'a Expr, operand: &'a Expr, test: Test) {
        let tight = !self.primary(operand);
        let replace = |rules: &mut Self, suffix: &str| {
            let mut pieces = Vec::new();
            if tight {
                pieces.push(Piece::Text("(".into()));
            }
            pieces.push(Piece::Source(operand.span));
            if tight {
                pieces.push(Piece::Text(")".into()));
            }
            pieces.push(Piece::Text(suffix.into()));
            rules.edits.replace(expr.span, pieces);
        };
        match test {
            Test::Bool => (),
            Test::Present => replace(self, " == nil"),
            Test::True => replace(self, " != true"),
            Test::Unknown(why) => self.report(Finding::new(
                Reason::Condition,
                expr.span,
                format!("! takes a bool, and {why}; compare the value explicitly"),
            )),
        }
    }
}

impl<'a, T: Hooks<'a> + ?Sized> Rules<'a> for T {}

/// The rule a rename pattern's removed spelling belongs to.
fn rule_of(pattern: &Pattern) -> Rule {
    match pattern.name.as_str() {
        "nil?" => Rule::NilPredicate,
        "eql?" | "equal?" => Rule::Equality,
        "itself" | "tap" | "yield_self" => Rule::Identity,
        "send" | "public_send" | "respond_to?" => Rule::Dispatch,
        "new" if pattern.receiver == "Hash" => Rule::HashNew,
        _ => Rule::Name,
    }
}

/// Source text short enough to quote in a message: its first line, cut at
/// forty characters.
fn excerpt(text: &str) -> String {
    const LIMIT: usize = 40;
    let line = text.lines().next().unwrap_or_default();
    match line.char_indices().nth(LIMIT) {
        Some((end, _)) => format!("{} ...", &line[..end]),
        None if line.len() < text.len() => format!("{line} ..."),
        None => line.to_owned(),
    }
}

fn template_text(rewrite: &Change) -> String {
    match rewrite {
        Change::Template(pieces) => format!("{pieces:?}"),
        Change::Manual(hint) => hint.clone(),
    }
}

/// Whether some builtin type spells `name` canonically, beside the types
/// whose patterns matched.
fn canonical_member(name: &str, matched: &[(&Pattern, Captures)]) -> bool {
    crate::tooling::member_names()
        .iter()
        .filter(|(kind, _)| {
            !matched
                .iter()
                .any(|(p, _)| p.receiver == *kind || p.receiver == "T")
        })
        .any(|(kind, names)| {
            names.contains(&name)
                && !matches!(*kind, "nil" | "bool")
                && !patterns()
                    .iter()
                    .any(|p| p.receiver == *kind && p.name == name)
                && !universal(name)
        })
}

fn universal(name: &str) -> bool {
    crate::tooling::member_names()
        .iter()
        .all(|(_, names)| names.contains(&name))
}

fn defines(class: &Class, name: &str) -> bool {
    class.members.iter().any(|member| match member {
        Member::Def(def) => def.name == name,
        _ => false,
    })
}
