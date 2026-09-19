use super::{Expr, Node, Parser, Token, keyword};
use crate::{
    Result,
    compilation::{Boxed, Buffer},
    types::{Field, Scalar, Type, TypeKind},
};

impl Parser<'_> {
    pub(super) fn argument_type_literal(&mut self) -> Result<Option<Expr>> {
        self.work.charge(1)?;
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
        if !boundary
            || matches!(ty.kind, TypeKind::Scalar(Scalar::Nil) | TypeKind::Shape(..))
            || !builtin_leaves(&ty)
        {
            return Ok(None);
        }
        let mut names = Buffer::new();
        let fallback = if end == start + 1 {
            if let Token::Word(name) = &self.tokens[start].token {
                names.push(self.work, name.as_str().to_owned())?;
                Some(Boxed::new(
                    self.work,
                    self.make_at(Node::Var(name.as_str().to_owned()), 1, offset)?,
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

    pub(super) fn hash_expr(&mut self) -> Result<Expr> {
        self.work.charge(1)?;
        let start = self.pos - 1;
        let (candidate, end, malformed) = self.hash_type_candidate()?;
        self.pos = start + 1;
        if candidate.is_none() && !malformed {
            return self.hash_group();
        }
        self.typed_hash_group(candidate, end)
    }

    fn hash_type_candidate(&mut self) -> Result<(Option<Type>, usize, bool)> {
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
        let candidate = candidate.ok().filter(|ty| {
            self.token() != &Token::P('?') && !self.default_field(ty) && builtin_leaves(ty)
        });
        Ok((candidate, end, malformed))
    }

    fn typed_hash_group(&mut self, candidate: Option<Type>, end: usize) -> Result<Expr> {
        let structural = self.type_structural_error;
        // Type-only tokens cannot alter locals or re-lex percent expressions.
        let state = (
            self.depth,
            self.groups,
            self.line_exprs,
            self.command_depth,
            self.ternaries.copy_with(self.work, |n| Ok(*n))?,
            self.command_group,
            self.loop_condition,
            self.declared_it,
        );
        match self.hash_group() {
            Ok(fallback) => {
                let Some(ty) = candidate else {
                    return Ok(fallback);
                };
                let mut names = Buffer::new();
                self.work.ty(&ty)?;
                literal_names(&ty, &mut names, self.work)?;
                names.sort();
                names.dedup();
                let depth = fallback.depth;
                self.make(
                    Node::Shape(
                        Boxed::new(self.work, ty)?,
                        Some(Boxed::new(self.work, fallback)?),
                        names,
                    ),
                    depth,
                )
            }
            Err(error) => {
                self.work.checkpoint()?;
                (
                    self.depth,
                    self.groups,
                    self.line_exprs,
                    self.command_depth,
                    self.ternaries,
                    self.command_group,
                    self.loop_condition,
                    self.declared_it,
                ) = state;
                self.type_structural_error = structural;
                let Some(ty) = candidate else {
                    return Err(error);
                };
                self.pos = end;
                self.make(
                    Node::Shape(Boxed::new(self.work, ty)?, None, Buffer::new()),
                    1,
                )
            }
        }
    }

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
        let mut options = vec![first];
        loop {
            let boundary = self.pos;
            self.line_breaks()?;
            if self.token() != &Token::P('|') || (block && !self.block_type_continues(depth)?) {
                self.pos = boundary;
                break;
            }
            self.bump()?;
            self.line_breaks()?;
            options.push(self.type_atom(depth)?);
        }
        if options.len() == 1 {
            return Ok(options.pop().unwrap());
        }
        Ok(Type {
            name: String::new(),
            kind: TypeKind::Union(options),
            nullable: false,
        })
    }

    fn type_atom(&mut self, depth: usize) -> Result<Type> {
        self.work.charge(1)?;
        let mut ty = if self.take_p('{') {
            self.type_shape(depth)?
        } else {
            self.named_type(depth)?
        };
        let boundary = self.pos;
        self.line_breaks()?;
        if self.take_p('?') {
            if ty.nullable {
                return self.err("duplicate nullable suffix");
            }
            ty.nullable = true;
            if self.token() == &Token::P('?') {
                return self.err("duplicate nullable suffix");
            }
        } else {
            self.pos = boundary;
        }
        Ok(ty)
    }

    fn named_type(&mut self, depth: usize) -> Result<Type> {
        let Token::Word(name) = self.bump()? else {
            return self.err("expected type name");
        };
        let mut name = name.as_str().to_owned();
        if keyword(&name) && name != "nil" {
            return self.err("expected type name");
        }
        let nullable = name.ends_with('?');
        if nullable {
            name.pop();
        }
        if name.ends_with('?') {
            return self.err("duplicate nullable suffix");
        }
        let mut ty = Type::named(name);
        ty.nullable = nullable;
        if matches!(ty.kind, TypeKind::Named) && self.take_p('.') {
            if ty.nullable {
                return self.err("nullable suffix belongs on qualified member");
            }
            self.line_breaks()?;
            let Token::Word(member) = self.bump()? else {
                return self.err("expected qualified type name");
            };
            let mut member = member.as_str().to_owned();
            if keyword(&member) {
                return self.err("expected qualified type name");
            }
            if member.ends_with('?') {
                member.pop();
                ty.nullable = true;
            }
            if member.ends_with('?') {
                return self.err("duplicate nullable suffix");
            }
            ty.name.push('.');
            ty.name.push_str(&member);
        }
        let boundary = self.pos;
        self.line_breaks()?;
        if self.token() == &Token::Op("<") {
            if ty.nullable {
                return self.err("nullable suffix belongs after type arguments");
            }
            if !matches!(ty.kind, TypeKind::Array(_) | TypeKind::Hash(_)) {
                return self.err("type does not accept type arguments");
            }
            self.bump()?;
            let first = self.type_expr(depth + 1, false)?;
            self.line_breaks()?;
            ty.kind = if matches!(ty.kind, TypeKind::Array(_)) {
                TypeKind::Array(Some(Box::new(first)))
            } else {
                self.expect_p(',')?;
                let second = self.type_expr(depth + 1, false)?;
                self.line_breaks()?;
                TypeKind::Hash(Some(Box::new((first, second))))
            };
            if self.token() != &Token::Op(">") {
                return self.err("expected closing type argument bracket");
            }
            self.bump()?;
        } else {
            self.pos = boundary;
        }
        Ok(ty)
    }

    fn type_shape(&mut self, depth: usize) -> Result<Type> {
        self.work.charge(1)?;
        let mut fields: Vec<Field> = Vec::new();
        let mut open = false;
        self.line_breaks()?;
        if !self.take_p('}') {
            loop {
                if self.token() == &Token::Op("...") {
                    self.bump()?;
                    self.line_breaks()?;
                    self.expect_p('}')?;
                    open = true;
                    break;
                }
                let (mut name, optional) = self.shape_field_name()?;
                let ty = self.type_expr(depth + 1, false)?;
                self.work.charge(fields.len())?;
                if let Some(prior) = fields.iter().find(|field| field.name == name) {
                    self.type_structural_error =
                        !self.default_field(&prior.ty) && !self.default_field(&ty);
                    return self.err("duplicate shape field");
                }
                fields.push(Field {
                    name: std::mem::take(&mut name),
                    ty,
                    optional,
                });
                self.line_breaks()?;
                if self.take_p('}') {
                    break;
                }
                if !self.take_p(',') {
                    self.type_structural_error =
                        matches!(self.token(), Token::Word(_) | Token::Bytes(_))
                            && self.tokens[self.pos + 1].token == Token::P(':');
                    return self.err("expected shape field separator");
                }
                self.line_breaks()?;
            }
        }
        fields.sort_unstable_by(|a, b| a.name.cmp(&b.name));
        Ok(Type {
            name: String::new(),
            kind: TypeKind::Shape(fields, open),
            nullable: false,
        })
    }

    fn shape_field_name(&mut self) -> Result<(Vec<u8>, bool)> {
        let symbol = self.token() == &Token::P(':');
        let (name, optional) = match self.bump()? {
            Token::Word(name) => {
                let mut name = name.as_str().to_owned();
                let optional = name.ends_with('?');
                if optional {
                    name.pop();
                }
                if name.ends_with('?') {
                    return self.err("duplicate optional shape field suffix");
                }
                (name.into_bytes(), optional)
            }
            Token::Bytes(bytes) => (bytes.to_vec(), false),
            Token::P(':') => {
                if !self.symbol_start(self.pos - 1) {
                    return self.err("expected symbol shape field");
                }
                let name = match self.bump()? {
                    Token::Word(name) => name.as_bytes().to_vec(),
                    Token::Bytes(bytes) => bytes.to_vec(),
                    Token::Op(op) => op.as_bytes().to_vec(),
                    _ => return self.err("expected symbol shape field"),
                };
                (name, false)
            }
            _ => return self.err("expected shape field name"),
        };
        self.line_breaks()?;
        self.expect_p(':')?;
        // An adjacent identifier after a symbol key starts another symbol.
        if symbol
            && matches!(self.token(), Token::Word(_))
            && self.previous()?.end == self.tokens[self.pos].offset
        {
            return self.err("expected shape field separator");
        }
        Ok((name, optional))
    }

    fn block_type_continues(&mut self, depth: usize) -> Result<bool> {
        let saved = self.pos;
        self.bump()?;
        self.line_breaks()?;
        let result = self.type_atom(depth).is_ok() && matches!(self.token(), Token::P(',' | '|'));
        self.pos = saved;
        self.work.checkpoint()?;
        Ok(result)
    }

    fn type_boundary(&self, parenthesized: bool) -> Result<bool> {
        Ok(matches!(
            self.token(),
            Token::P(',' | ')' | ':' | '|') | Token::Op("=")
        ) || (!parenthesized
            && (matches!(self.token(), Token::EndLine | Token::Eof | Token::Op("->"))
                || self.tokens[self.pos].line != self.previous()?.end_line)))
    }

    pub(super) fn keyword_default(&mut self, parenthesized: bool) -> Result<bool> {
        let result = match self.token() {
            Token::Word(name) if name == "nil" => self.tokens[self.pos + 1].token != Token::P('|'),
            Token::P('{') => {
                let saved = self.pos;
                let structural = self.type_structural_error;
                self.type_structural_error = false;
                let annotation = match self.type_expr(1, false) {
                    Ok(ty) => !self.default_field(&ty) && self.type_boundary(parenthesized)?,
                    Err(_) => self.type_structural_error,
                };
                self.pos = saved;
                self.type_structural_error = structural;
                !annotation
            }
            Token::Word(name) if !keyword(name) => {
                let next = &self.tokens[self.pos + 1];
                match &next.token {
                    Token::P(',' | ')' | ':' | '|') | Token::Op("=") => false,
                    Token::Op("<") => {
                        !matches!(
                            Type::named(name.as_str().to_owned()).kind,
                            TypeKind::Array(_) | TypeKind::Hash(_)
                        ) && self.locals.contains(name.as_str())
                    }
                    Token::P('.') => {
                        if self.locals.contains(name.as_str()) {
                            return Ok(true);
                        }
                        let saved = self.pos;
                        let namespace = *name;
                        self.bump()?;
                        self.bump()?;
                        let annotation = if let Token::Word(member) = self.bump()? {
                            let member = member.trim_end_matches('?');
                            let looks_like_type = member
                                .as_bytes()
                                .first()
                                .is_some_and(u8::is_ascii_uppercase)
                                && member.as_bytes().iter().skip(1).any(u8::is_ascii_lowercase);
                            let constant = namespace == "Math" && matches!(member, "PI" | "E");
                            self.take_p('?');
                            looks_like_type && !constant && self.type_boundary(parenthesized)?
                        } else {
                            false
                        };
                        self.pos = saved;
                        !annotation
                    }
                    Token::EndLine | Token::Eof | Token::Op("->") => parenthesized,
                    _ => true,
                }
            }
            _ => true,
        };
        self.work.checkpoint()?;
        Ok(result)
    }

    fn default_field(&self, ty: &Type) -> bool {
        match &ty.kind {
            TypeKind::Shape(fields, false) => {
                fields.is_empty() || fields.iter().any(|field| self.default_field(&field.ty))
            }
            TypeKind::Scalar(Scalar::Nil) => true,
            TypeKind::Scalar(_)
            | TypeKind::Named
            | TypeKind::Array(None)
            | TypeKind::Hash(None) => !ty.nullable && self.locals.contains(&ty.name),
            _ => false,
        }
    }
}

fn builtin_leaves(ty: &Type) -> bool {
    match &ty.kind {
        TypeKind::Named => false,
        TypeKind::Array(Some(element)) => builtin_leaves(element),
        TypeKind::Hash(Some(pair)) => builtin_leaves(&pair.0) && builtin_leaves(&pair.1),
        TypeKind::Shape(fields, _) => fields.iter().all(|field| builtin_leaves(&field.ty)),
        TypeKind::Union(options) => options.iter().all(builtin_leaves),
        _ => true,
    }
}

fn literal_names(
    ty: &Type,
    names: &mut Buffer<String>,
    work: &dyn crate::compilation::Work,
) -> Result<()> {
    match &ty.kind {
        TypeKind::Shape(fields, _) => {
            for field in fields {
                literal_names(&field.ty, names, work)?;
            }
        }
        TypeKind::Union(options) => {
            for option in options {
                literal_names(option, names, work)?;
            }
        }
        TypeKind::Scalar(Scalar::Nil) => (),
        _ => {
            names.push(work, ty.name.clone())?;
            match &ty.kind {
                TypeKind::Array(Some(element)) => literal_names(element, names, work)?,
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
