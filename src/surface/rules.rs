//! The rewrite of each removed spelling. Each rule reads one construct,
//! records its edits as a group, or reports a [`Finding`] when the rewrite
//! is not safe where the construct stands.

use super::{
    Access, Finding, Rule,
    checker::Checker,
    context::{
        Place, literal_type, namespace_member_takes_no_arguments, namespace_name, simple,
        string_literal, symbol_literal,
    },
    edits::Piece,
    patterns::{ArgPattern, Callee, Change, KeywordValue, Pattern, TemplatePiece, patterns},
    syntax::*,
};
use crate::tooling::TokenKind;
use std::collections::{HashMap, HashSet};

/// The arguments a rename pattern captured.
#[derive(Default)]
pub struct Captures<'r> {
    /// Each `$name` and the span of the argument it matched.
    pub values: HashMap<String, Span>,
    /// The spans of the arguments `...` matched.
    pub rest: Vec<Span>,
    room: Option<&'r super::edits::Room<'r>>,
}

impl Drop for Captures<'_> {
    fn drop(&mut self) {
        if let Some(room) = self.room {
            room.give_back(self.rest.capacity() * size_of::<Span>());
        }
    }
}

/// The canonical surface's rules.
impl<'a> Checker<'a> {
    /// A global function renamed without parentheses, such as `now`.
    pub(super) fn bare_name(&mut self, expr: &'a Expr, name: &str, place: Place) {
        if self.local(name) || self.declared.methods.contains(name) || self.known_type(name) {
            return;
        }
        let globals = || {
            patterns()
                .iter()
                .filter(|p| p.callee == Callee::Global && p.name == name)
        };
        let Some(pattern) = globals().find(|p| p.args.is_none()) else {
            // A removed global named without its arguments, such as
            // `sprintf`, or a removed namespace, such as `Regexp`.
            let table = crate::signatures::table();
            let removed = if table.functions(name).next().is_none() {
                globals().next()
            } else {
                None
            }
            .or_else(|| {
                patterns()
                    .iter()
                    .find(
                        |p| matches!(&p.callee, Callee::Namespace(namespace) if namespace == name),
                    )
                    .filter(|_| table.module(name).is_none())
            });
            if let Some(pattern) = removed {
                self.unmatched_at(expr.span, name, pattern);
                return;
            }
            // A method body calls a member of every value on `self`; a
            // type name, as in the shape `{ name: string }`, calls nothing.
            let on_self = patterns()
                .iter()
                .find(|p| p.callee == Callee::Member && p.receiver == "T" && p.name == name);
            if let Some(pattern) = on_self
                && self.scope().class.is_some()
                && self.scope().def.is_some()
                && !super::parse::builtin_type(name)
            {
                self.unmatched_at(expr.span, name, pattern);
            }
            return;
        };
        self.apply_pattern(expr, None, pattern, &Captures::default(), place);
    }

    /// Rewrites a percent literal as an array literal.
    pub(super) fn words(&mut self, expr: &'a Expr, place: Place) {
        let tok = self.token_at(expr.span.start);
        let TokenKind::Words { symbols, entries } = &self.tokens[tok].kind else {
            return;
        };
        let removed = excerpt(self.text(expr.span));
        let mut literal = self.writer();
        literal.push_str("[");
        for (index, entry) in entries.iter().enumerate() {
            // Each entry is a step the pass charged up front, and the walk
            // asks now and then whether the compilation has stopped.
            if self.halt() {
                return;
            }
            let Some(bytes) = entry else {
                self.report(Finding::new(
                    Rule::PercentLiteral,
                    expr.span,
                    removed,
                    "write an array literal, interpolating in its strings",
                ));
                return;
            };
            let Some(item) = (if *symbols {
                symbol_literal(bytes, self.room)
            } else {
                string_literal(bytes, self.room)
            }) else {
                self.report(Finding::new(
                    Rule::PercentLiteral,
                    expr.span,
                    removed,
                    "write an array literal",
                ));
                return;
            };
            if index > 0 {
                literal.push_str(", ");
            }
            literal.push_str(&item);
            let capacity = item.capacity();
            drop(item);
            self.room.give_back(capacity);
        }
        literal.push_str("]");
        let Some(literal) = literal.finish() else {
            return;
        };
        // A percent literal after a command name is an argument, which an
        // array literal after a space is as well.
        let _ = place;
        let advice = written!(self, "use the array literal `{}`", excerpt(&literal));
        let previous = self.enter(Rule::PercentLiteral, expr.span, removed, advice);
        let group = self.rewrites.len() - 1;
        self.words.insert(expr.span, group);
        self.edits.text(expr.span, literal);
        self.leave(previous);
    }

    /// Rewrites `h[:name]` as `h["name"]` when the receiver is a hash.
    pub(super) fn symbol_keys(&mut self, expr: &'a Expr, selectors: &'a [Expr]) {
        let [selector] = selectors else {
            return;
        };
        if !matches!(selector.kind, ExprKind::Symbol) {
            return;
        }
        if !matches!(expr.kind, ExprKind::Index(..))
            || self.declared.methods.contains("[]")
            || self.declared.methods.contains("[]=")
        {
            return;
        }
        let TokenKind::Symbol { name, .. } = &self.tokens[self.token_at(selector.span.start)].kind
        else {
            return;
        };
        let removed = self.text(selector.span);
        match string_literal(name, self.room) {
            Some(literal) => {
                let advice = written!(self, "hash keys are strings: `{literal}`");
                let previous = self.enter(Rule::SymbolKey, selector.span, removed, advice);
                self.edits.text(selector.span, literal);
                self.leave(previous);
            }
            None => self.report(Finding::new(
                Rule::SymbolKey,
                selector.span,
                removed,
                "hash keys are strings",
            )),
        }
    }

    /// Whether `x.name()` and `x.name` do the same: a hash receiver may hold
    /// a function under the name, which only the parentheses call.
    pub(super) fn parens_optional(&self, expr: &'a Expr, call: &'a Call) -> bool {
        let Some(receiver) = &call.receiver else {
            return true;
        };
        if let Some(kind) = self.static_kind(receiver) {
            return kind != "hash";
        }
        self.receiver_plain(expr, call)
    }

    /// Drops the parentheses of a call without arguments.
    pub(super) fn empty_parens(&mut self, expr: &'a Expr, call: &'a Call) {
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
            } else {
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
            if !namespace_member_takes_no_arguments(namespace, &call.name) {
                return;
            }
        } else if !self.parens_optional(expr, call) {
            return;
        }
        let span = Span {
            start: self.tokens[*open].start,
            end: self.tokens[*close].end,
        };
        let removed = written!(self, "{}()", call.name);
        let advice = written!(
            self,
            "a call without arguments has no parentheses: `{}`",
            call.name
        );
        let previous = self.enter(Rule::EmptyParentheses, span, removed, advice);
        self.edits.text(span, "");
        self.leave(previous);
    }

    /// Replaces `send(:name, ...)` and `public_send(:name, ...)` with a direct call.
    pub(super) fn dispatch(&mut self, expr: &'a Expr, call: &'a Call) {
        if !matches!(call.name.as_str(), "send" | "public_send" | "respond_to?") {
            return;
        }
        let Some(receiver) = &call.receiver else {
            return;
        };
        if self.receiver_owns_method(expr, call) {
            return;
        }
        let observed = self.receiver_dynamic(expr, call);
        if observed == Some(true) {
            // A host object's own `send`, such as a capability's.
            return;
        }
        let span = self.token_span(call.name_tok);
        let direct = "call the member directly, or use `case` over the name";
        if call.name == "respond_to?" {
            self.report(Finding::new(
                Rule::Dispatch,
                span,
                "respond_to?",
                "use `case` over the name, or `is_type?`",
            ));
            return;
        }
        let Some(args) = &call.args else {
            self.report(Finding::new(Rule::Dispatch, span, &call.name, direct));
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
            self.report(Finding::new(Rule::Dispatch, span, &call.name, direct));
            return;
        };
        let call_directly = written!(self, "call `{name}` directly");
        if self.declared.private_methods.contains(&name) {
            self.report(Finding::new(
                Rule::Dispatch,
                span,
                &call.name,
                written!(self, "{name} is private; call it from inside its class"),
            ));
            return;
        }
        let rest = &args.items[1..];
        if rest
            .iter()
            .any(|arg| !self.room.charge(1) || arg.kind != ArgKind::Positional)
        {
            if self.halt() {
                return;
            }
            // Dispatch passes keywords as an options hash, and a direct call does not.
            self.report(Finding::new(
                Rule::Dispatch,
                span,
                &call.name,
                call_directly,
            ));
            return;
        }
        let mut pieces = vec![
            Piece::Source(receiver.span),
            Piece::Text(self.copied(self.text(Span {
                start: receiver.span.end,
                end: self.tokens[call.name_tok].start,
            }))),
            Piece::Text(name),
        ];
        if !rest.is_empty() {
            pieces.push(Piece::Text("(".into()));
            for (index, arg) in rest.iter().enumerate() {
                if self.halt() {
                    return;
                }
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
    pub(super) fn hash_new(&mut self, expr: &'a Expr, call: &'a Call, place: Place) {
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
            self.report(Finding::new(
                Rule::HashNew,
                expr.span,
                "Hash.new",
                "write `{}` with a declared type, and read missing keys with `fetch`",
            ));
            return;
        }
        // Without parentheses, `Hash.new` as a receiver is not a call.
        if place == Place::Tight && call.args.is_none() {
            self.report(Finding::new(Rule::HashNew, expr.span, "Hash.new", advice));
            return;
        }
        let text = if place == Place::Tight { "({})" } else { "{}" };
        let previous = self.enter(Rule::HashNew, expr.span, "Hash.new", advice);
        self.edits.text(expr.span, text);
        self.leave(previous);
    }

    /// Rewrites a call written with `::`, such as `JSON::parse(x)` or
    /// `Pricing::with_tax(1)`, with a dot: `::` names only constants,
    /// nested types and enum members, which take no arguments.
    pub(super) fn scoped_call(&mut self, call: &'a Call) {
        let (Some(receiver), Some(operator)) = (&call.receiver, call.operator) else {
            return;
        };
        if !call.scoped(self.tokens) {
            return;
        }
        let called = call.args.is_some() || call.block.is_some();
        let lowercase = call
            .name
            .chars()
            .next()
            .is_some_and(|c| c == '_' || c.is_lowercase());
        // A lowercase name may still be an enum member, and an enum member
        // written with arguments is refused as it is, so only a
        // namespace's or a value's name is a call. `Hash::new` has a rule of
        // its own.
        let hash_new =
            call.name == "new" && matches!(&receiver.kind, ExprKind::Name(name) if name == "Hash");
        if hash_new || !((called || lowercase) && self.scopes_functions(receiver)) {
            return;
        }
        let span = self.token_span(operator);
        let receiver = excerpt(self.text(receiver.span));
        let advice = written!(
            self,
            "use `{receiver}.{}`; `::` names only constants, nested types and enum members",
            call.name
        );
        let removed = written!(self, "{receiver}::{}", call.name);
        let previous = self.enter(Rule::ScopedCall, span, removed, advice);
        self.edits.text(span, ".");
        self.leave(previous);
    }

    /// Whether a name after `receiver::` can only be a function: the receiver
    /// is a builtin namespace, a class or module the source declares, or a
    /// local or call that may hold one, but not an enum, which may be
    /// lowercase, a type the source does not declare or a literal.
    pub(super) fn scopes_functions(&self, receiver: &'a Expr) -> bool {
        let capitalized = |name: &str| name.chars().next().is_some_and(char::is_uppercase);
        match &receiver.kind {
            ExprKind::Name(name) if capitalized(name) => {
                namespace_name(name) || self.declared.classes.contains_key(name.as_str())
            }
            ExprKind::Name(name) => !self.declared.enums.contains_key(name.as_str()),
            ExprKind::Call(call) => !capitalized(&call.name),
            _ => false,
        }
    }

    /// Rewrites symbols naming a required module as strings, and reports a
    /// `require` whose names are not literals.
    pub(super) fn require(&mut self, call: &'a Call) {
        if call.receiver.is_some() || call.name != "require" {
            return;
        }
        let Some(args) = &call.args else {
            return;
        };
        for arg in &args.items {
            if self.halt() {
                return;
            }
            if matches!(arg.value.kind, ExprKind::Symbol)
                && let TokenKind::Symbol { name, .. } =
                    &self.tokens[self.token_at(arg.value.span.start)].kind
                && let Some(literal) = string_literal(name, self.room)
            {
                let removed = self.text(arg.value.span);
                let what = match &arg.kind {
                    ArgKind::Keyword(name) if name == "as" => "alias",
                    _ => "module",
                };
                let advice = written!(self, "name the {what} with the string `{literal}`");
                let previous = self.enter(Rule::Require, arg.value.span, removed, advice);
                self.edits.text(arg.value.span, literal);
                self.leave(previous);
                continue;
            }
            // The checker reports any other name that is not a literal.
            let literal = matches!(arg.value.kind, ExprKind::Str);
            match &arg.kind {
                ArgKind::Positional | ArgKind::Keyword(_) if literal => (),
                ArgKind::Keyword(name) if name != "as" => (),
                _ => return,
            }
        }
    }

    /// Rewrites a hash field read or written with a dot, `h.name`, as the
    /// index `h["name"]`: dot calls methods only. A read, or an update that
    /// reads first, applies where the receiver is a hash, the name is not a
    /// hash method and every hash held data under it; a write, which always
    /// sets a field, wherever the receiver is a hash. Returns whether it
    /// reported the access.
    pub(super) fn field_access(&mut self, expr: &'a Expr, call: &'a Call, access: Access) -> bool {
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
        let Some(key) =
            string_literal(call.name.as_bytes(), self.room).filter(|_| hashes > 0 && !object)
        else {
            return false;
        };
        let span = Span {
            start: self.tokens[operator].start,
            end: self.tokens[call.name_tok].end,
        };
        let removed = self.copied(self.text(span));
        let receiver_text = excerpt(self.text(receiver.span));
        let indexed = written!(self, "`{receiver_text}[{key}]`");
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
        // A value that is not always a hash, `&.`, destructuring, a receiver
        // spanning lines, after which `[` starts a new expression, and a
        // comment between the receiver and the dot leave it to a person.
        if hashes < kinds.len()
            || call.safe(self.tokens)
            || access == Access::Destructure
            || multiline_group
            || between.contains('#')
        {
            self.report(Finding::new(
                Rule::FieldAccess,
                span,
                removed,
                written!(
                    self,
                    "hash fields are indexed, as in {indexed}, once the value is a hash"
                ),
            ));
            return true;
        }
        let advice = written!(self, "hash fields are indexed: {indexed}");
        let previous = self.enter(Rule::FieldAccess, span, removed, advice);
        if wrap {
            self.edits.wrap(receiver.span, "(", ")");
        }
        let index = written!(self, "[{key}]");
        self.edits.text(
            Span {
                start: receiver.span.end,
                end: span.end,
            },
            index,
        );
        self.leave(previous);
        true
    }

    /// Moves keyword parameters declared in a removed form after a bare
    /// `*`: `retries: 3` becomes `*, retries: int = 3`, `name:` becomes
    /// `*, name: T` and `name: T:` becomes `*, name: T`, declaring an
    /// untyped one with the type of its literal default, if it has one.
    pub(super) fn keyword_params(&mut self, def: &'a Def) {
        let old: Vec<&'a Param> = def
            .params
            .iter()
            .take_while(|_| self.room.within())
            .filter(|param| param.keyword_colon.is_some())
            .collect();
        if self.halt() {
            return;
        }
        let (Some(first), Some(last)) = (old.first(), old.last()) else {
            return;
        };
        let span = Span {
            start: first.span.start,
            end: last.span.end,
        };
        // Each parameter is a step the pass charged up front, and the walk
        // asks now and then whether the compilation has stopped, in each
        // pass over them.
        let mut types: Vec<Option<String>> = Vec::with_capacity(old.len());
        for param in &old {
            if self.halt() {
                return;
            }
            types.push(match (&param.ty, &param.default) {
                (None, Some(default)) => literal_type(self, default).map(str::to_owned),
                _ => None,
            });
        }
        let index = def
            .params
            .iter()
            .position(|param| param.kind == ParamKind::Keyword)
            .unwrap_or(0);
        let star = def.star.is_none()
            && !def.params[..index]
                .iter()
                .any(|param| param.kind == ParamKind::Rest);
        // The parameters in their new form, whose defaults the source
        // sizes, written through the room.
        let mut canonical = self.writer();
        if star {
            canonical.push_str("*, ");
        }
        for (at, (param, ty)) in old.iter().zip(&types).enumerate() {
            if self.halt() {
                return;
            }
            if at > 0 {
                canonical.push_str(", ");
            }
            canonical.push_str(&param.name);
            let ty = match &param.ty {
                Some(ty) => Some(self.text(ty.span)),
                None => ty.as_deref(),
            };
            if let Some(ty) = ty {
                canonical.push_str(": ");
                canonical.push_str(ty);
            }
            if let Some(default) = &param.default {
                canonical.push_str(" = ");
                canonical.push_str(self.text(default.span));
            }
        }
        // A refusal leaves the room full, which stops the walk.
        let canonical = canonical.finish().unwrap_or_default();
        let removed = excerpt(self.text(span));
        let advice = written!(
            self,
            "keyword parameters follow a bare `*`: `{}`",
            excerpt(&canonical)
        );
        let previous = self.enter(Rule::KeywordParameter, span, removed, advice);
        if star {
            self.edits.insert(def.params[index].span.start, "*, ");
        }
        for (param, ty) in old.iter().zip(types) {
            if self.halt() {
                break;
            }
            let colon = self.token_span(param.keyword_colon.expect("a removed keyword form"));
            let name_end = self.tokens[param.name_tok].end;
            let declared = ty.map(|ty| written!(self, ": {ty}")).unwrap_or_default();
            match (&param.ty, &param.default) {
                (Some(ty), _) => self.edits.text(
                    Span {
                        start: ty.span.end,
                        end: colon.end,
                    },
                    "",
                ),
                (None, Some(default)) => {
                    let text = written!(self, "{declared} = ");
                    self.edits.text(
                        Span {
                            start: name_end,
                            end: default.span.start,
                        },
                        text,
                    );
                }
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

    /// Lowercases builtin type names and spells `object` as `hash`.
    pub(super) fn type_names(&mut self, ty: &'a TypeExpr) {
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
                    self.report(Finding::new(
                        Rule::TypeName,
                        self.token_span(*tok),
                        name,
                        "type names are lowercase; `nil` here would read as a keyword default",
                    ));
                }
                if super::parse::respelled_type(&lower) && lower != "nil" {
                    let canonical = if lower == "object" { "hash" } else { &lower };
                    if canonical != name {
                        let span = self.token_span(*tok);
                        let advice = if lower == "object" {
                            "use `hash`".to_owned()
                        } else {
                            written!(self, "type names are lowercase: `{canonical}`")
                        };
                        let previous = self.enter(Rule::TypeName, span, name, advice);
                        let text = written!(self, "{canonical}{suffix}");
                        self.edits.text(span, text);
                        self.leave(previous);
                    }
                }
                for arg in args {
                    if self.halt() {
                        return;
                    }
                    self.type_names(arg);
                }
            }
            TypeKind::Shape(fields, _) => {
                for (_, field) in fields {
                    if self.halt() {
                        return;
                    }
                    self.type_names(field);
                }
            }
            TypeKind::Union(options) | TypeKind::Tuple(options) => {
                for option in options {
                    if self.halt() {
                        return;
                    }
                    self.type_names(option);
                }
            }
            TypeKind::Qualified(_) => (),
        }
    }

    /// The kinds a receiver's values have, as the rename table names them:
    /// from the syntax when it decides, and otherwise from the hooks.
    pub(super) fn kinds_of(&self, expr: &'a Expr, call: &'a Call) -> Option<Vec<String>> {
        let receiver = call.receiver.as_ref()?;
        if let Some(kind) = self.static_kind(receiver) {
            return Some(vec![kind]);
        }
        self.receiver_kinds(expr, call)
    }

    /// Applies the rename table to a call; returns whether it rewrote it.
    pub(super) fn rename(&mut self, expr: &'a Expr, call: &'a Call, place: Place) -> bool {
        if matches!(call.name.as_str(), "eql?" | "equal?")
            && let Some(receiver) = &call.receiver
        {
            let span = self.token_span(call.name_tok);
            let mut finding = Finding::new(
                Rule::Equality,
                span,
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
                        self.copied(text)
                    } else {
                        written!(self, "({text})")
                    }
                };
                let mut text = written!(
                    self,
                    "{} == {}",
                    wrap(self.text(receiver.span), receiver),
                    wrap(self.text(arg.value.span), &arg.value)
                );
                if place == Place::Tight {
                    text = written!(self, "({text})");
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
                let global = candidates
                    .iter()
                    .find(|p| p.callee == Callee::Global)
                    .filter(|_| {
                        crate::signatures::table()
                            .functions(&call.name)
                            .next()
                            .is_none()
                    });
                // A method body calls a member of every value on `self`.
                let on_self = candidates
                    .iter()
                    .find(|p| p.callee == Callee::Member && p.receiver == "T")
                    .filter(|_| self.scope().class.is_some() && self.scope().def.is_some());
                if let Some(pattern) = global.or(on_self) {
                    self.unmatched(call, pattern);
                }
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
            // Otherwise a member of every value, such as `Math.itself`, is
            // renamed below.
            if let Some((pattern, captures)) = candidates
                .iter()
                .filter(|p| matches!(&p.callee, Callee::Namespace(namespace) if namespace == name))
                .find_map(|p| self.match_args(call, p).map(|c| (*p, c)))
            {
                return self.apply_pattern(expr, Some(call), pattern, &captures, place);
            }
        }
        let members: Vec<&Pattern> = candidates
            .into_iter()
            .filter(|p| p.callee == Callee::Member)
            .collect();
        if members.is_empty() {
            return false;
        }
        let choose = |kind: &str, rules: &Self| -> Option<(&'static Pattern, Captures<'a>)> {
            patterns()
                .iter()
                .filter(|p| p.callee == Callee::Member && p.name == call.name)
                .filter(|p| p.receiver == kind || p.receiver == "T")
                .find_map(|p| rules.match_args(call, p).map(|c| (p, c)))
        };
        let kinds = self.kinds_of(expr, call);
        let decision = match &kinds {
            Some(kinds) if !kinds.is_empty() => {
                let mut chosen: Option<(&Pattern, Captures<'a>)> = None;
                let mut mixed = false;
                let mut unmatched = false;
                for kind in kinds {
                    if !self.room.charge(1) {
                        return false;
                    }
                    let found = if kind == "any" {
                        mixed = true;
                        None
                    } else if let Some(class) = kind.strip_prefix("class ") {
                        let own = self
                            .declared
                            .classes
                            .get(class)
                            .is_some_and(|c| defines(c, &call.name, self.room));
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
                // A removed member is removed however it is called, such as
                // `nil?` with arguments or `itself` with a splat. The static
                // checker reports a plain rename's spelling, such as `size`.
                if chosen.is_none() && !mixed {
                    let first = |receiver: &str| {
                        patterns().iter().find(|p| {
                            p.callee == Callee::Member
                                && p.name == call.name
                                && p.receiver == receiver
                        })
                    };
                    let removed =
                        kinds
                            .iter()
                            .take_while(|_| self.room.charge(1))
                            .find_map(|kind| {
                                let own = match kind.strip_prefix("class ") {
                                    Some(class) => self
                                        .declared
                                        .classes
                                        .get(class)
                                        .is_some_and(|c| defines(c, &call.name, self.room)),
                                    None => kind == "any" || declares(kind, &call.name),
                                };
                                if own {
                                    return None;
                                }
                                let receiver = if kind.starts_with("class ") {
                                    "instance"
                                } else {
                                    kind
                                };
                                first(receiver)
                                    .or_else(|| first("T"))
                                    .filter(|p| p.canonical.is_none())
                            });
                    if let Some(pattern) = removed {
                        self.unmatched(call, pattern);
                    }
                }
                if mixed {
                    if chosen.is_some() {
                        self.report(Finding::new(
                            Rule::Name,
                            span,
                            &call.name,
                            written!(self,
                                "it is removed for some of the receiver's types ({}) but not others; call a member every type has",
                                kinds.join(", ")
                            ),
                        ));
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
                let matched: Vec<(&'static Pattern, Captures<'a>)> = members
                    .iter()
                    .filter_map(|p| self.match_args(call, p).map(|c| (*p, c)))
                    .collect();
                let Some(first) = matched.first() else {
                    // A name no builtin type has, called as no rewrite
                    // takes it, is removed whatever the receiver is.
                    if let Some(pattern) = members.first()
                        && !declared_anywhere(&call.name)
                    {
                        self.unmatched(call, pattern);
                    }
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
                    let choices: Vec<String> = matched
                        .iter()
                        .map(|(p, _)| written!(self, "{} on {}", p.advice(), p.receiver))
                        .collect();
                    self.report(Finding::new(
                        Rule::Name,
                        span,
                        &call.name,
                        choices.join(", "),
                    ));
                    return false;
                }
                Some(matched.into_iter().next().unwrap())
            }
        };
        let Some((pattern, captures)) = decision else {
            return false;
        };
        if self.halt() {
            return false;
        }
        self.apply_pattern(expr, Some(call), pattern, &captures, place)
    }

    /// The arguments of `call` that `pattern` captures, if it matches.
    pub(super) fn match_args(&self, call: &'a Call, pattern: &Pattern) -> Option<Captures<'a>> {
        let mut captures = Captures::default();
        let args = call
            .args
            .as_ref()
            .map_or(&[][..], |args| args.items.as_slice());
        let Some(pattern_args) = &pattern.args else {
            return (args.is_empty() && call.block.is_none()).then_some(captures);
        };
        let rest = pattern_args.contains(&ArgPattern::Rest);
        let mut positional = args
            .iter()
            .take_while(|_| self.room.charge(1))
            .filter(|arg| arg.kind == ArgKind::Positional);
        let mut next = 0;
        let mut used_keywords = HashSet::new();
        for arg in pattern_args {
            if !self.room.charge(1) {
                return None;
            }
            match arg {
                ArgPattern::Rest => (),
                ArgPattern::Capture(name) => {
                    let value = positional.next()?;
                    next += 1;
                    captures.values.insert(name.clone(), value.value.span);
                }
                ArgPattern::Symbol(symbol) => {
                    let value = positional.next()?;
                    next += 1;
                    if !matches!(value.value.kind, ExprKind::Symbol)
                        || self.text(value.value.span) != written!(self, ":{symbol}")
                    {
                        return None;
                    }
                }
                ArgPattern::Keyword(name, expected) => {
                    let mut found = None;
                    for (index, arg) in args.iter().enumerate() {
                        if !self.room.charge(1) {
                            return None;
                        }
                        if let ArgKind::Keyword(key) = &arg.kind {
                            if self.room.same_text(key, name)? {
                                found = Some((index, arg));
                                break;
                            }
                        }
                    }
                    let (index, arg) = found?;
                    used_keywords.insert(index);
                    match expected {
                        KeywordValue::Literal(literal) => {
                            if !self
                                .room
                                .charge(1 + arg.value.span.range().len().div_ceil(64) as u64)
                                || self.text(arg.value.span) != literal
                            {
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
        if rest {
            if !self.room.take(args.len() * size_of::<Span>()) {
                return None;
            }
            captures.rest = Vec::with_capacity(args.len());
            captures.room = Some(self.room);
        }
        let mut positional_seen = 0;
        for (index, arg) in args.iter().enumerate() {
            if !self.room.charge(1) {
                return None;
            }
            let remaining = match arg.kind {
                ArgKind::Positional => {
                    positional_seen += 1;
                    positional_seen > next
                }
                ArgKind::Keyword(_) => !used_keywords.contains(&index),
                _ => true,
            };
            if remaining {
                if !rest {
                    return None;
                }
                captures.rest.push(arg.span);
            }
        }
        if !rest && call.block.is_some() {
            return None;
        }
        self.room.within().then_some(captures)
    }

    /// Reports a removed spelling that no rewrite takes as it is called,
    /// with arguments or a block its replacement has no place for, or on
    /// the implicit `self` of a method, leaving the rewrite to a person.
    pub(super) fn unmatched(&mut self, call: &'a Call, pattern: &Pattern) {
        let span = self.token_span(call.name_tok);
        self.unmatched_at(span, &call.name, pattern);
    }

    /// Reports removed spelling `name` at `span`, as [`Self::unmatched`] does.
    pub(super) fn unmatched_at(&mut self, span: Span, name: &str, pattern: &Pattern) {
        self.report(Finding::new(rule_of(pattern), span, name, pattern.advice()));
    }

    /// Whether a call rewrites to an index its arguments can fill, as
    /// `$x[...]` does `slice(i)`: an index takes one or more plain values,
    /// and no block, splat or keyword.
    pub(super) fn index_rewrites(
        &self,
        pieces: &[TemplatePiece],
        call: Option<&'a Call>,
        captures: &Captures<'_>,
    ) -> bool {
        let indexes = pieces.windows(2).any(|pair| {
            matches!(pair, [TemplatePiece::Text(text), TemplatePiece::Rest] if text.ends_with('['))
        });
        let Some(call) = call.filter(|_| indexes) else {
            return true;
        };
        let plain = call
            .args
            .as_ref()
            .is_none_or(|args| args.items.iter().all(|arg| arg.kind == ArgKind::Positional));
        plain && call.block.is_none() && !captures.rest.is_empty()
    }

    /// Replaces a call with a rename's template; returns whether it did.
    pub(super) fn apply_pattern(
        &mut self,
        expr: &'a Expr,
        call: Option<&'a Call>,
        pattern: &Pattern,
        captures: &Captures<'_>,
        place: Place,
    ) -> bool {
        let span = call.map_or(expr.span, |call| self.token_span(call.name_tok));
        let rule = rule_of(pattern);
        let advice = pattern.advice();
        let pieces = match &pattern.rewrite {
            Change::Manual(hint) => {
                self.report(Finding::new(rule, span, &pattern.name, hint));
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
            self.report(Finding::new(
                rule,
                span,
                &pattern.name,
                written!(self, "{advice}, where it behaves the same"),
            ));
            return false;
        }
        if !self.index_rewrites(pieces, call, captures) {
            self.report(Finding::new(
                rule,
                span,
                &pattern.name,
                written!(
                    self,
                    "{advice} with an index, a start and a length, or a range"
                ),
            ));
            return false;
        }
        if pattern.receiver_uses() > 1 && !receiver.is_some_and(simple) {
            self.report(Finding::new(
                rule,
                span,
                &pattern.name,
                written!(self, "{advice}, binding the receiver to a local first"),
            ));
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
            self.report(Finding::new(
                rule,
                span,
                &pattern.name,
                written!(self, "{advice}, where it behaves the same"),
            ));
            return false;
        }
        let safe = call.is_some_and(|call| call.safe(self.tokens));
        let plain_member = matches!(pieces.as_slice(), [TemplatePiece::Receiver, TemplatePiece::Text(text), ..] if text.starts_with('.'));
        if safe && !plain_member {
            // `x&.m` skips nil, which an operator replacement would not.
            self.report(Finding::new(rule, span, &pattern.name, advice));
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
                self.report(Finding::new(
                    rule,
                    span,
                    &pattern.name,
                    "use `is_type?` with the type's name as a symbol",
                ));
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
                        if self.halt() {
                            return true;
                        }
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
            if self.halt() {
                return true;
            }
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
            if self.halt() {
                return true;
            }
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
    pub(super) fn braces(&mut self, block: &'a Block, owner: Option<&'a Call>, callee_end: usize) {
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
            self.report(Finding::new(
                Rule::DoBlock,
                open_span,
                "do ... end",
                "write the block with braces, giving its call parentheses; its bare keywords then bind as keyword arguments",
            ));
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
                    Piece::Source(span) => self.copied(self.text(*span)),
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
    pub(super) fn negate_bool(&mut self, expr: &'a Expr) {
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
                    written!(self, " {flipped}")
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

    /// Negates the condition of `unless` or `until` into the rewrite of
    /// their keyword.
    pub(super) fn negate_condition(&mut self, expr: &'a Expr) {
        let group = self.negation;
        let previous = self.edits.enter(group);
        self.negate_bool(expr);
        self.edits.enter(previous);
    }
}

/// Whether the signature table declares member `name` on values of `kind`,
/// or on every value.
fn declares(kind: &str, name: &str) -> bool {
    crate::signatures::table()
        .items
        .iter()
        .any(|item| match item {
            crate::signatures::Item::Class(class) => {
                (class.base() == kind || class.base() == "T") && class.named(name).next().is_some()
            }
            _ => false,
        })
}

/// Whether the signature table declares member `name` on any type.
fn declared_anywhere(name: &str) -> bool {
    crate::signatures::table()
        .items
        .iter()
        .any(|item| match item {
            crate::signatures::Item::Class(class) => class.named(name).next().is_some(),
            _ => false,
        })
}

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
fn canonical_member(name: &str, matched: &[(&Pattern, Captures<'_>)]) -> bool {
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

fn defines(class: &Class, name: &str, room: &super::edits::Room<'_>) -> bool {
    for member in &class.members {
        if !room.charge(1) {
            return false;
        }
        if let Member::Def(def) = member {
            match room.same_text(&def.name, name) {
                Some(true) => return true,
                Some(false) => (),
                None => return false,
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn class_method_search_stops_before_the_last_member() {
        let methods = (0..1000)
            .map(|i| format!("def f{i}; end;"))
            .collect::<String>();
        let source = format!("class C; {methods} end");
        let tree = super::super::parse::parse(&source).unwrap();
        let StmtKind::Class(class) = &tree.body[0].kind else {
            panic!("class")
        };
        let room = super::super::edits::Room::new(None, &|steps| steps <= 8, 0);
        assert!(!defines(class, "f999", &room));
        assert!(!room.within());
    }
}
