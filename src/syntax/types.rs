use super::{Parser, Token, keyword};
use crate::{
    Result,
    types::{Field, Scalar, Type, TypeKind},
};

impl Parser<'_> {
    pub(super) fn type_expr(&mut self, depth: usize, block: bool) -> Result<Type> {
        if depth > 64 {
            return self.err("type annotation nesting too deep");
        }
        self.line_breaks();
        let first = self.type_atom(depth)?;
        let mut options = vec![first];
        loop {
            let boundary = self.pos;
            self.line_breaks();
            if self.token() != &Token::P('|') || (block && !self.block_type_continues(depth)) {
                self.pos = boundary;
                break;
            }
            self.bump();
            self.line_breaks();
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
        let mut ty = if self.take_p('{') {
            self.type_shape(depth)?
        } else {
            let Token::Word(mut name) = self.bump() else {
                return self.err("expected type name");
            };
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
                self.line_breaks();
                let Token::Word(mut member) = self.bump() else {
                    return self.err("expected qualified type name");
                };
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
            self.line_breaks();
            if self.token() == &Token::Op("<") {
                if ty.nullable {
                    return self.err("nullable suffix belongs after type arguments");
                }
                if !matches!(ty.kind, TypeKind::Array(_) | TypeKind::Hash(_)) {
                    return self.err("type does not accept type arguments");
                }
                self.bump();
                let first = self.type_expr(depth + 1, false)?;
                self.line_breaks();
                ty.kind = if matches!(ty.kind, TypeKind::Array(_)) {
                    TypeKind::Array(Some(Box::new(first)))
                } else {
                    self.expect_p(',')?;
                    let second = self.type_expr(depth + 1, false)?;
                    self.line_breaks();
                    TypeKind::Hash(Some(Box::new((first, second))))
                };
                if self.token() != &Token::Op(">") {
                    return self.err("expected closing type argument bracket");
                }
                self.bump();
            } else {
                self.pos = boundary;
            }
            ty
        };
        let boundary = self.pos;
        self.line_breaks();
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

    fn type_shape(&mut self, depth: usize) -> Result<Type> {
        let mut fields: Vec<Field> = Vec::new();
        let mut open = false;
        self.line_breaks();
        if !self.take_p('}') {
            loop {
                if self.token() == &Token::Op("...") {
                    self.bump();
                    self.line_breaks();
                    self.expect_p('}')?;
                    open = true;
                    break;
                }
                let symbol = self.token() == &Token::P(':');
                let (mut name, optional) = match self.bump() {
                    Token::Word(mut name) => {
                        let optional = name.ends_with('?');
                        if optional {
                            name.pop();
                        }
                        if name.ends_with('?') {
                            return self.err("duplicate optional shape field suffix");
                        }
                        (name.into_bytes(), optional)
                    }
                    Token::Bytes(bytes) => (bytes, false),
                    Token::P(':') => {
                        if !self.symbol_start(self.pos - 1) {
                            return self.err("expected symbol shape field");
                        }
                        let name = match self.bump() {
                            Token::Word(name) => name.into_bytes(),
                            Token::Bytes(bytes) => bytes,
                            Token::Op(op) => op.as_bytes().to_vec(),
                            _ => return self.err("expected symbol shape field"),
                        };
                        (name, false)
                    }
                    _ => return self.err("expected shape field name"),
                };
                self.line_breaks();
                self.expect_p(':')?;
                // An adjacent identifier after a symbol key starts another symbol.
                if symbol
                    && matches!(self.token(), Token::Word(_))
                    && self.previous().end == self.tokens[self.pos].offset
                {
                    return self.err("expected shape field separator");
                }
                let ty = self.type_expr(depth + 1, false)?;
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
                self.line_breaks();
                if self.take_p('}') {
                    break;
                }
                if !self.take_p(',') {
                    self.type_structural_error =
                        matches!(self.token(), Token::Word(_) | Token::Bytes(_))
                            && self.tokens[self.pos + 1].token == Token::P(':');
                    return self.err("expected shape field separator");
                }
                self.line_breaks();
                if self.take_p('}') {
                    break;
                }
            }
        }
        fields.sort_unstable_by(|a, b| a.name.cmp(&b.name));
        Ok(Type {
            name: String::new(),
            kind: TypeKind::Shape(fields, open),
            nullable: false,
        })
    }

    fn block_type_continues(&mut self, depth: usize) -> bool {
        let saved = self.pos;
        self.bump();
        self.line_breaks();
        let result = self.type_atom(depth).is_ok() && matches!(self.token(), Token::P(',' | '|'));
        self.pos = saved;
        result
    }

    fn type_boundary(&self, parenthesized: bool) -> bool {
        matches!(
            self.token(),
            Token::P(',' | ')' | ':' | '|') | Token::Op("=")
        ) || (!parenthesized
            && (matches!(self.token(), Token::EndLine | Token::Eof | Token::Op("->"))
                || self.tokens[self.pos].line != self.previous().end_line))
    }

    pub(super) fn keyword_default(&mut self, parenthesized: bool) -> bool {
        match self.token() {
            Token::Word(name) if name == "nil" => self.tokens[self.pos + 1].token != Token::P('|'),
            Token::P('{') => {
                let saved = self.pos;
                let structural = self.type_structural_error;
                self.type_structural_error = false;
                let annotation = match self.type_expr(1, false) {
                    Ok(ty) => !self.default_field(&ty) && self.type_boundary(parenthesized),
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
                            Type::named(name.clone()).kind,
                            TypeKind::Array(_) | TypeKind::Hash(_)
                        ) && self.locals.contains(name)
                    }
                    Token::P('.') => {
                        if self.locals.contains(name) {
                            return true;
                        }
                        let saved = self.pos;
                        let namespace = name.clone();
                        self.bump();
                        self.bump();
                        let annotation = if let Token::Word(member) = self.bump() {
                            let member = member.trim_end_matches('?');
                            let looks_like_type = member
                                .as_bytes()
                                .first()
                                .is_some_and(u8::is_ascii_uppercase)
                                && member.as_bytes().iter().skip(1).any(u8::is_ascii_lowercase);
                            let constant = namespace == "Math" && matches!(member, "PI" | "E");
                            self.take_p('?');
                            looks_like_type && !constant && self.type_boundary(parenthesized)
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
        }
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
