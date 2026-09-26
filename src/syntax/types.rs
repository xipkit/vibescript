use super::{Expr, Label, Node, Parser, Parsing, Token};
use crate::{
    Result,
    compilation::{Boxed, Buffer, Bytes, Field, Name, Type, TypeKind},
    types::Scalar,
};

impl Parsing<'_> {
    pub(super) async fn hash_expr(&self) -> Result<Expr> {
        let (candidate, end, malformed) = {
            let mut p = self.p();
            p.work.charge(1)?;
            let start = p.pos - 1;
            let classes = std::mem::take(&mut p.type_argument);
            let (candidate, end, malformed) = p.hash_type_candidate(classes)?;
            p.pos = start + 1;
            (candidate, end, malformed)
        };
        if candidate.is_none() && !malformed {
            return self.hash_group().await;
        }
        self.typed_hash_group(candidate, end).await
    }

    /// Parses a braced group that also reads as a shape type. Like Go, a group
    /// that reads only as a malformed shape reports the shape's error.
    async fn typed_hash_group(&self, candidate: Option<Type>, end: usize) -> Result<Expr> {
        let work = self.p().work;
        // Type-only tokens cannot alter locals or re-lex percent expressions.
        let (structural, start, state) = {
            let p = self.p();
            let start = p.pos;
            let state = (
                p.depth,
                p.groups,
                p.line_exprs,
                p.command_depth,
                p.ternaries.copy_with(work, |n| Ok(*n))?,
                p.command_group,
                p.loop_condition,
                p.then_stop,
                p.declared_it,
            );
            (p.type_structural_error, start, state)
        };
        match self.hash_group().await {
            Ok(fallback) => {
                let Some(ty) = candidate else {
                    return Ok(fallback);
                };
                let mut names = Buffer::new();
                work.ty(&ty)?;
                literal_names(&ty, &mut names, work)?;
                // A class or enum is bound as a declaration, which cannot
                // turn the group back into a hash.
                let names = {
                    let p = self.p();
                    let mut kept = Buffer::new();
                    for name in names {
                        let last = name.rsplit("::").next().unwrap_or(&name);
                        if !p.declared_type(last) || p.is_alias(last) {
                            kept.push(work, name)?;
                        }
                    }
                    kept
                };
                let mut names = names;
                work.charge(
                    names
                        .len()
                        .saturating_mul(names.len().max(1).ilog2() as usize + 1),
                )?;
                names.sort_unstable();
                work.charge(names.len())?;
                names.dedup();
                let depth = fallback.depth;
                self.p().make(
                    Node::Shape(
                        Boxed::new(work, ty)?,
                        Some(Boxed::new(work, fallback)?),
                        names,
                    ),
                    depth,
                )
            }
            Err(error) => {
                work.checkpoint()?;
                let mut p = self.p();
                (
                    p.depth,
                    p.groups,
                    p.line_exprs,
                    p.command_depth,
                    p.ternaries,
                    p.command_group,
                    p.loop_condition,
                    p.then_stop,
                    p.declared_it,
                ) = state;
                p.type_structural_error = structural;
                let Some(ty) = candidate else {
                    if error.kind == crate::ErrorKind::Syntax {
                        p.pos = start;
                        p.type_shape(0)?;
                    }
                    return Err(error);
                };
                p.pos = end;
                p.make(Node::Shape(Boxed::new(work, ty)?, None, Buffer::new()), 1)
            }
        }
    }
}

impl Parser<'_> {
    /// Parses an argument that is a type literal, such as `int` or
    /// `array<string>`, and where the call takes `types`, a tuple type.
    pub(super) fn argument_type_literal(&mut self, types: bool) -> Result<Option<Expr>> {
        self.work.charge(1)?;
        if self.token() == &Token::P('[') {
            return if types {
                self.argument_tuple_literal()
            } else {
                Ok(None)
            };
        }
        if !matches!(self.token(), Token::Word(_)) {
            return Ok(None);
        }
        let start = self.pos;
        let offset = self.tokens[start].offset as u32;
        let structural = self.type_structural_error;
        let candidate = self.type_expr(1, false);
        self.work.checkpoint()?;
        let end = self.pos;
        self.line_breaks()?;
        let boundary = matches!(self.token(), Token::P(',' | ')'));
        self.pos = start;
        self.type_structural_error = structural;
        let Ok(ty) = candidate else {
            return Ok(None);
        };
        self.work.ty(&ty)?;
        // A call that takes types may name the source's classes and enums
        // inside a type; a bare name is the class or enum's own value.
        let classes = types && !(matches!(ty.kind, TypeKind::Named) && !ty.nullable);
        if !boundary
            || matches!(ty.kind, TypeKind::Scalar(Scalar::Nil) | TypeKind::Shape(..))
            || !self.literal_leaves(&ty, classes)
        {
            return Ok(None);
        }
        let mut names = Buffer::new();
        // A local named like an ADR-007 type, such as a rescued `error`, is
        // the value; the runtime's guard does not see every such binding. In
        // a call that takes types, a builtin function named like a builtin
        // type, such as `money`, is the type, since a function is not a value.
        let (local, function) = match &self.tokens[start].token {
            Token::Word(name) if end == start + 1 => {
                let newer = matches!(
                    name.as_str(),
                    "regex" | "match_data" | "error" | "enum_value" | "enum_type"
                ) || crate::signatures::alias_type(name).is_some();
                (
                    newer && self.locals.contains(self.work, name.as_str())?,
                    types
                        && name.bytes().all(|byte| !byte.is_ascii_uppercase())
                        && crate::builtin::Global::parse(name).is_some(),
                )
            }
            _ => (false, false),
        };
        if local {
            return Ok(None);
        }
        let fallback = if end == start + 1 && !function {
            if let Token::Word(name) = &self.tokens[start].token {
                let name = Name::new(self.work, name)?;
                names.push(self.work, name.clone())?;
                Some(Boxed::new(
                    self.work,
                    self.make_at(Node::Var(name), 1, offset)?,
                )?)
            } else {
                None
            }
        } else {
            None
        };
        self.pos = end;
        Ok(Some(self.make_at(
            Node::Shape(Boxed::new(self.work, ty)?, fallback, names),
            1,
            offset,
        )?))
    }

    /// Reads the one `nil` argument of a cast, `value.as(nil)`, as the nil
    /// type, which every other argument position reads as the value.
    pub(super) fn nil_type_argument(&mut self, args: &mut Buffer<super::Argument>) -> Result<()> {
        let [arg] = &mut args[..] else {
            return Ok(());
        };
        let nil = matches!(arg.kind, super::ArgumentKind::Positional)
            && matches!(&arg.value.node, Node::Literal(value) if matches!(value.0, crate::value::Kind::Nil));
        if !nil {
            return Ok(());
        }
        let ty = Type {
            name: Name::new(self.work, "nil")?,
            kind: TypeKind::Scalar(Scalar::Nil),
            nullable: false,
        };
        let offset = arg.value.offset;
        arg.value = self.make_at(
            Node::Shape(Boxed::new(self.work, ty)?, None, Buffer::new()),
            1,
            offset,
        )?;
        Ok(())
    }

    /// A tuple type as an argument of a call that takes types, `[int,
    /// string]`, where every element is a builtin type, an alias, or a class
    /// or enum the source declares, and none names a local; an array of
    /// values otherwise.
    fn argument_tuple_literal(&mut self) -> Result<Option<Expr>> {
        if !self.tuple_start(self.significant(self.pos + 1)) {
            return Ok(None);
        }
        let start = self.pos;
        let offset = self.tokens[start].offset as u32;
        let structural = self.type_structural_error;
        let candidate = self.type_expr(1, false);
        self.work.checkpoint()?;
        let end = self.pos;
        self.line_breaks()?;
        let boundary = matches!(self.token(), Token::P(',' | ')'));
        self.pos = start;
        self.type_structural_error = structural;
        let Ok(ty) = candidate else {
            return Ok(None);
        };
        self.work.ty(&ty)?;
        let TypeKind::Tuple(elements) = &ty.kind else {
            return Ok(None);
        };
        if !boundary
            || !elements
                .iter()
                .all(|element| self.literal_leaves(element, true))
        {
            return Ok(None);
        }
        let mut names = Buffer::new();
        literal_names(&ty, &mut names, self.work)?;
        for name in &names {
            if self.locals.contains(self.work, name.as_str())? {
                return Ok(None);
            }
        }
        self.pos = end;
        Ok(Some(self.make_at(
            Node::Shape(Boxed::new(self.work, ty)?, None, Buffer::new()),
            1,
            offset,
        )?))
    }

    /// Reads the braced group at the current token as a shape type, whose
    /// leaves may name the source's classes and enums when `classes`.
    pub(super) fn hash_type_candidate(
        &mut self,
        classes: bool,
    ) -> Result<(Option<Type>, usize, bool)> {
        let structural = self.type_structural_error;
        self.type_structural_error = false;
        let candidate = self.type_shape(0);
        self.work.checkpoint()?;
        let end = self.pos;
        let malformed = self.type_structural_error;
        self.type_structural_error = structural;
        if let Ok(ty) = &candidate {
            self.work.ty(ty)?;
        }
        let candidate = match candidate {
            Ok(ty)
                if self.token() != &Token::P('?')
                    && !self.default_field(&ty)?
                    && self.literal_leaves(&ty, classes) =>
            {
                Some(ty)
            }
            _ => None,
        };
        Ok((candidate, end, malformed))
    }

    /// Parses a type annotation, as Go's `parseTypeExpr` does. A block
    /// parameter's union continues only while another option and a boundary follow.
    pub(super) fn type_expr(&mut self, depth: usize, block: bool) -> Result<Type> {
        self.work.charge(1)?;
        if depth > 64 {
            return self.err("type annotation nesting too deep");
        }
        self.line_breaks()?;
        let first = self.type_atom(depth)?;
        self.type_union(first, depth, block)
    }

    // Keep union assembly off the stack while parsing the first atom.
    fn type_union(&mut self, first: Type, depth: usize, block: bool) -> Result<Type> {
        let mut options = Buffer::from_array(self.work, [first])?;
        loop {
            let boundary = self.pos;
            self.line_breaks()?;
            if self.token() != &Token::P('|') || (block && !self.block_type_continues(depth)?) {
                self.pos = boundary;
                break;
            }
            self.bump()?;
            self.line_breaks()?;
            options.push(self.work, self.type_atom(depth)?)?;
        }
        if options.len() == 1 {
            return Ok(options.pop().unwrap());
        }
        Ok(Type {
            name: Name::default(),
            kind: TypeKind::Union(options),
            nullable: false,
        })
    }

    /// Parses a type option with its nullable suffix, as Go's `parseTypeAtom` does.
    pub(super) fn type_atom(&mut self, depth: usize) -> Result<Type> {
        self.work.charge(1)?;
        let mut ty = if self.take_p('{') {
            self.type_shape(depth)?
        } else if self.token() == &Token::P('[') {
            self.type_tuple(depth)?
        } else {
            self.named_type(depth)?
        };
        let question = self.significant(self.pos);
        if self.tokens[question].token == Token::P('?') {
            self.pos = question;
            if ty.nullable {
                return self.err(format_args!(
                    "duplicate nullable suffix on type {}",
                    type_text(&ty, self.work)?
                ));
            }
            self.bump()?;
            ty.nullable = true;
            let second = self.significant(self.pos);
            if self.tokens[second].token == Token::P('?') {
                self.pos = second;
                return self.err(format_args!(
                    "duplicate nullable suffix on type {}",
                    type_text(&ty, self.work)?
                ));
            }
        }
        Ok(ty)
    }

    fn named_type(&mut self, depth: usize) -> Result<Type> {
        if !self.ident(self.pos) && !matches!(self.token(), Token::Word(w) if w == "nil") {
            return self.expected(Label::Text("type name"));
        }
        let mut index = self.pos;
        let Token::Word(written) = self.bump()? else {
            unreachable!()
        };
        let written = written.as_str();
        let name = written.strip_suffix('?').unwrap_or(written);
        if name.ends_with('?') {
            self.pos = index;
            return self.err(format_args!(
                "duplicate nullable suffix on type {}",
                super::source_text(written)
            ));
        }
        let mut ty = Type::named(Name::new(self.work, name)?);
        ty.nullable = name.len() < written.len();
        // A class or module nested in another is named through each, as
        // `Outer::Inner`, as a value is.
        loop {
            let scope = self.significant(self.pos);
            if !matches!(ty.kind, TypeKind::Named) || self.tokens[scope].token != Token::Op("::") {
                break;
            }
            if ty.nullable {
                self.pos = index;
                return self.err(format_args!(
                    "nullable suffix on {name} is misplaced; write {name}::Name? instead",
                    name = super::source_text(&ty.name)
                ));
            }
            self.pos = self.significant(scope + 1);
            if !self.ident(self.pos) {
                return self.expected(Label::Text("identifier"));
            }
            index = self.pos;
            let Token::Word(member) = self.bump()? else {
                unreachable!()
            };
            let member = member.as_str();
            ty.nullable = member.ends_with('?');
            let member = member.strip_suffix('?').unwrap_or(member);
            ty.name = Name::join(self.work, &[&ty.name, "::", member])?;
        }
        let dot = self.significant(self.pos);
        if matches!(ty.kind, TypeKind::Named)
            && !ty.name.contains("::")
            && self.tokens[dot].token == Token::P('.')
        {
            if ty.nullable {
                self.pos = index;
                return self.err(format_args!(
                    "nullable suffix on {name} is misplaced; write {name}.Name? instead",
                    name = super::source_text(name)
                ));
            }
            self.pos = self.significant(dot + 1);
            if !self.ident(self.pos) {
                return self.expected(Label::Text("identifier"));
            }
            index = self.pos;
            let Token::Word(member) = self.bump()? else {
                unreachable!()
            };
            let member = member.as_str();
            ty.nullable = member.ends_with('?');
            let member = member.strip_suffix('?').unwrap_or(member);
            ty.name = Name::join(self.work, &[&ty.name, ".", member])?;
        }
        let open = self.significant(self.pos);
        if self.tokens[open].token != Token::Op("<") {
            return Ok(ty);
        }
        if !matches!(
            ty.kind,
            TypeKind::Array(_) | TypeKind::Hash(_) | TypeKind::Literal(_)
        ) {
            self.pos = index;
            return self.err(format_args!(
                "type {} does not accept type arguments",
                super::source_text(&ty.name)
            ));
        }
        if ty.nullable {
            self.pos = index;
            let base = super::source_text(&ty.name);
            return self.err(format_args!(
                "nullable suffix on {base} is misplaced; write the nullable container after its type arguments, e.g. {base}<...>?, instead of {base}?<...>"
            ));
        }
        self.pos = open + 1;
        let mut arguments = Buffer::new();
        loop {
            self.line_breaks()?;
            arguments.push(self.work, self.type_expr(depth + 1, false)?)?;
            let next = self.significant(self.pos);
            self.pos = next;
            if self.take_p(',') {
                continue;
            }
            if self.token() == &Token::Op(">=") {
                self.split_closing_angle()?;
            }
            if self.token() != &Token::Op(">") {
                return self.expected(Label::Text(">"));
            }
            self.bump()?;
            break;
        }
        if matches!(ty.kind, TypeKind::Literal(_)) {
            if arguments.len() != 1 {
                return Err(crate::Error::syntax(
                    self.work,
                    self.tokens[index].offset,
                    "type expects exactly 1 type argument",
                ));
            }
            ty.kind = TypeKind::Literal(Some(Boxed::new(self.work, arguments.pop().unwrap())?));
            return Ok(ty);
        }
        let array = matches!(ty.kind, TypeKind::Array(_));
        let expected = if array { 1 } else { 2 };
        if arguments.len() != expected {
            return Err(crate::Error::syntax(
                self.work,
                self.tokens[index].offset,
                if array {
                    "array type expects exactly 1 type argument"
                } else {
                    "hash type expects exactly 2 type arguments"
                },
            ));
        }
        ty.kind = if array {
            TypeKind::Array(Some(Boxed::new(self.work, arguments.pop().unwrap())?))
        } else {
            let second = arguments.pop().unwrap();
            let first = arguments.pop().unwrap();
            TypeKind::Hash(Some(Boxed::new(self.work, (first, second))?))
        };
        Ok(ty)
    }

    /// Splits the `>=` that closes type arguments followed by a value, as in
    /// `names: array<string>=[]`, into its `>` and `=`.
    fn split_closing_angle(&mut self) -> Result<()> {
        let lexeme = &self.tokens[self.pos];
        let (offset, end, line) = (lexeme.offset, lexeme.end, lexeme.line);
        let at = |token, offset, end| super::lexer::Lexeme {
            token,
            offset,
            end,
            line,
            end_line: line,
        };
        let pair = Buffer::from_array(
            self.work,
            [
                at(Token::Op(">"), offset, offset + 1),
                at(Token::Op("="), offset + 1, end),
            ],
        )?;
        self.tokens.replace(self.pos..self.pos + 1, pair, self.work)
    }

    /// Parses a shape type after its `{`, as Go's `parseTypeShape` does.
    fn type_shape(&mut self, depth: usize) -> Result<Type> {
        self.work.charge(1)?;
        let mut fields: Buffer<Field> = Buffer::new();
        let mut open = false;
        self.line_breaks()?;
        if !self.take_p('}') {
            loop {
                if self.token() == &Token::Op("...") {
                    self.bump()?;
                    self.line_breaks()?;
                    if !self.take_p('}') {
                        return self.expected(Label::Text("}"));
                    }
                    open = true;
                    break;
                }
                let (name, optional) = self.shape_field_name()?;
                self.line_breaks()?;
                let ty = self.type_expr(depth + 1, false)?;
                self.work.charge(fields.len())?;
                if let Some(prior) = fields.iter().find(|field| field.name == name) {
                    self.type_structural_error =
                        !self.default_field(&prior.ty)? && !self.default_field(&ty)?;
                    self.pos -= 1;
                    return self.err(format_args!(
                        "duplicate shape field {}",
                        super::source_text(&String::from_utf8_lossy(&name))
                    ));
                }
                fields.push(self.work, Field { name, ty, optional })?;
                self.line_breaks()?;
                if self.take_p('}') {
                    break;
                }
                if !self.take_p(',') {
                    self.type_structural_error = self.shape_field_start(self.pos)
                        && self.tokens[self.significant(self.pos + 1)].token == Token::P(':');
                    return self.expected(Label::Text("}"));
                }
                self.line_breaks()?;
            }
        }
        fields.sort_unstable_by(|a, b| a.name.cmp(&b.name));
        Ok(Type {
            name: Name::default(),
            kind: TypeKind::Shape(fields, open),
            nullable: false,
        })
    }

    /// Go's `tokenStartsShapeFieldName`: a label, plain string or symbol.
    fn shape_field_start(&self, index: usize) -> bool {
        match &self.tokens[index].token {
            Token::Word(word) => !word.starts_with('@'),
            Token::Bytes(_) | Token::Symbol(_) | Token::QuotedSymbol(_) => true,
            _ => false,
        }
    }

    fn shape_field_name(&mut self) -> Result<(Bytes, bool)> {
        // Like Go, a field is named by a label, a plain string or a symbol.
        let (name, optional) = match self.token() {
            Token::Word(word) if !word.starts_with('@') => {
                let raw = word.as_str();
                let name = raw.strip_suffix('?').unwrap_or(raw);
                if name.ends_with('?') {
                    return self.err(format_args!(
                        "duplicate optional suffix on shape field {}",
                        super::source_text(raw)
                    ));
                }
                (
                    Bytes::from_slice(self.work, name.as_bytes())?,
                    name.len() < raw.len(),
                )
            }
            Token::Bytes(bytes) | Token::QuotedSymbol(bytes) => (bytes.clone(), false),
            Token::Symbol(name) => (Bytes::from_slice(self.work, name.as_bytes())?, false),
            _ => return self.expected(super::Label::Text("shape field name")),
        };
        self.bump()?;
        self.line_breaks()?;
        if !self.take_p(':') {
            return self.expected(super::Label::Text(":"));
        }
        Ok((name, optional))
    }

    fn block_type_continues(&mut self, depth: usize) -> Result<bool> {
        let saved = self.pos;
        self.bump()?;
        self.line_breaks()?;
        let result = self.type_atom(depth).is_ok()
            && matches!(
                self.tokens[self.significant(self.pos)].token,
                Token::P(',' | '|')
            );
        self.pos = saved;
        self.work.checkpoint()?;
        Ok(result)
    }

    /// Go's `typeAnnotationBoundaryFollows` for the token after the one at `last`.
    fn type_boundary(&self, last: usize, parenthesized: bool) -> bool {
        let next = self.significant(last + 1);
        match &self.tokens[next].token {
            Token::P(',' | ')' | ':' | '|') | Token::Op("=") => true,
            Token::Op("->") | Token::EndLine | Token::Eof => !parenthesized,
            _ => !parenthesized && self.tokens[next].line != self.tokens[last].line,
        }
    }

    /// Go's `colonIntroducesKeywordDefault`: whether what follows a
    /// parameter's colon is a default value rather than a type.
    pub(super) fn keyword_default(&mut self, parenthesized: bool) -> Result<bool> {
        let peek = self.significant(self.pos);
        let result = match &self.tokens[peek].token {
            // `name: nil` declares a parameter of type nil, as `name: T`
            // does for any type; a nil keyword default is `*, name: T? = nil`.
            Token::Word(name) if *name == "nil" => false,
            Token::P('{') => {
                let saved = self.pos;
                let structural = self.type_structural_error;
                self.type_structural_error = false;
                self.pos = peek;
                let annotation = match self.type_expr(1, false) {
                    Ok(ty) => {
                        !self.default_field(&ty)? && self.type_boundary(self.pos - 1, parenthesized)
                    }
                    Err(_) => self.type_structural_error,
                };
                self.pos = saved;
                self.type_structural_error = structural;
                !annotation
            }
            // A bracket reads as a tuple type only when every leaf names a type.
            Token::P('[') if !self.tuple_start(peek + 1) => true,
            Token::P('[') => {
                let saved = self.pos;
                let structural = self.type_structural_error;
                self.pos = peek;
                let annotation = match self.type_expr(1, false) {
                    Ok(ty) => {
                        self.declared_leaves(&ty)
                            && !self.default_field(&ty)?
                            && self.type_boundary(self.pos - 1, parenthesized)
                    }
                    Err(_) => false,
                };
                self.pos = saved;
                self.type_structural_error = structural;
                !annotation
            }
            _ if self.ident(peek) => self.name_starts_default(peek, parenthesized)?,
            _ => self.prefix(peek),
        };
        self.work.checkpoint()?;
        Ok(result)
    }

    /// Go's `identAfterColonStartsExpression` for the identifier at `peek`.
    fn name_starts_default(&self, peek: usize, parenthesized: bool) -> Result<bool> {
        let Token::Word(name) = &self.tokens[peek].token else {
            unreachable!()
        };
        let next = self.significant(peek + 1);
        Ok(match &self.tokens[next].token {
            Token::P(',' | ')' | ':' | '|') | Token::Op("=") => false,
            Token::Op("<") => {
                !matches!(
                    crate::types::builtin_name(name),
                    Some(
                        crate::types::BuiltinName::Array
                            | crate::types::BuiltinName::Hash
                            | crate::types::BuiltinName::Type
                    )
                ) && self.locals.contains(self.work, name.as_str())?
            }
            Token::P('.') => !self.dotted_type_follows(peek, next, parenthesized)?,
            Token::Op("::") => !self.scoped_type_follows(peek, parenthesized)?,
            Token::Op("->") | Token::EndLine | Token::Eof => parenthesized,
            _ => parenthesized || self.tokens[next].line == self.tokens[peek].line,
        })
    }

    /// Whether `Outer::Inner` after a parameter's colon reads as a nested
    /// class's type rather than a default.
    fn scoped_type_follows(&self, peek: usize, parenthesized: bool) -> Result<bool> {
        let mut last = peek;
        loop {
            let scope = self.significant(last + 1);
            if self.tokens[scope].token != Token::Op("::") {
                break;
            }
            let member = self.significant(scope + 1);
            if !self.ident(member) {
                return Ok(false);
            }
            last = member;
        }
        let question = self.significant(last + 1);
        if self.tokens[question].token == Token::P('?') {
            last = question;
        }
        Ok(self.type_boundary(last, parenthesized))
    }

    /// Go's `dottedTypeAnnotationFollows`: whether `Namespace.Type` reads as a
    /// qualified type annotation.
    fn dotted_type_follows(&self, peek: usize, dot: usize, parenthesized: bool) -> Result<bool> {
        let Token::Word(namespace) = &self.tokens[peek].token else {
            unreachable!()
        };
        if self.locals.contains(self.work, namespace.as_str())? {
            return Ok(false);
        }
        let member = self.significant(dot + 1);
        if !self.ident(member) {
            return Ok(false);
        }
        let Token::Word(written) = &self.tokens[member].token else {
            unreachable!()
        };
        let name = written.trim_end_matches('?');
        let looks_like_type = name.as_bytes().first().is_some_and(u8::is_ascii_uppercase)
            && name.as_bytes().iter().skip(1).any(u8::is_ascii_lowercase);
        if !looks_like_type || (*namespace == "Math" && matches!(name, "PI" | "E")) {
            return Ok(false);
        }
        if written.ends_with('?') {
            return Ok(self.type_boundary(member, parenthesized));
        }
        let question = self.significant(member + 1);
        let last = if self.tokens[question].token == Token::P('?') {
            question
        } else {
            member
        };
        Ok(self.type_boundary(last, parenthesized))
    }

    fn default_field(&self, ty: &Type) -> Result<bool> {
        self.work.charge(1)?;
        Ok(match &ty.kind {
            TypeKind::Shape(fields, false) => {
                if fields.is_empty() {
                    return Ok(true);
                }
                for field in fields {
                    if self.default_field(&field.ty)? {
                        return Ok(true);
                    }
                }
                false
            }
            TypeKind::Scalar(Scalar::Nil) => true,
            TypeKind::Scalar(_)
            | TypeKind::Named
            | TypeKind::Array(None)
            | TypeKind::Hash(None) => !ty.nullable && self.locals.contains(self.work, &ty.name)?,
            TypeKind::Tuple(elements) => {
                for element in elements {
                    if self.default_field(element)? {
                        return Ok(true);
                    }
                }
                false
            }
            _ => false,
        })
    }
}

impl Parser<'_> {
    /// Whether every leaf of a type an expression could also spell reads as
    /// a type literal: a builtin type name, or a type alias, and with
    /// `classes` a class or enum the source declares, also through its
    /// scope as `Outer::Inner`. A single name that a local shadows, such as
    /// a rescued `error`, reads as the local through the literal's fallback.
    fn literal_leaves(&self, ty: &Type, classes: bool) -> bool {
        match &ty.kind {
            TypeKind::Named => {
                self.is_alias(&ty.name)
                    || crate::signatures::alias_type(&ty.name).is_some()
                    || (classes
                        && ty
                            .name
                            .rsplit("::")
                            .next()
                            .is_some_and(|name| self.declared_type(name)))
            }
            TypeKind::Tuple(_) => false,
            TypeKind::Literal(Some(described)) => self.literal_leaves(described, classes),
            TypeKind::Array(Some(element)) => self.literal_leaves(element, classes),
            TypeKind::Hash(Some(pair)) => {
                self.literal_leaves(&pair.0, classes) && self.literal_leaves(&pair.1, classes)
            }
            TypeKind::Shape(fields, _) => fields
                .iter()
                .all(|field| self.literal_leaves(&field.ty, classes)),
            TypeKind::Union(options) => options
                .iter()
                .all(|option| self.literal_leaves(option, classes)),
            _ => true,
        }
    }
}

fn literal_names(
    ty: &Type,
    names: &mut Buffer<Name>,
    work: &dyn crate::compilation::Work,
) -> Result<()> {
    match &ty.kind {
        TypeKind::Shape(fields, _) => {
            for field in fields {
                literal_names(&field.ty, names, work)?;
            }
        }
        TypeKind::Union(options) | TypeKind::Tuple(options) => {
            for option in options {
                literal_names(option, names, work)?;
            }
        }
        TypeKind::Scalar(Scalar::Nil) => (),
        _ => {
            names.push(work, ty.name.clone())?;
            match &ty.kind {
                TypeKind::Array(Some(element)) | TypeKind::Literal(Some(element)) => {
                    literal_names(element, names, work)?
                }
                TypeKind::Hash(Some(pair)) => {
                    literal_names(&pair.0, names, work)?;
                    literal_names(&pair.1, names, work)?;
                }
                _ => (),
            }
        }
    }
    Ok(())
}

/// Spells a type for a diagnostic, as Go's `FormatTypeExpr` does, within
/// Go's bound on quoted source.
fn type_text(ty: &Type, work: &dyn crate::compilation::Work) -> Result<String> {
    let mut text = Vec::new();
    crate::shapes::format(ty, &mut text)?;
    work.bytes(text.len())?;
    Ok(super::source_text(&String::from_utf8_lossy(&text)).to_string())
}
