//! The declarations ADR-007 adds: typed locals and the types they annotate.

use super::{Expr, Label, Node, Parser, Parsing, Statement, Target, Token, source_text, unicode};
use crate::{
    Error, Result,
    compilation::{Boxed, Buffer, Name, Type, TypeKind},
};

impl Parser<'_> {
    /// Records the classes and enums declared anywhere in the source. The
    /// scan is linear in tokens the lexer already charged for.
    pub(super) fn with_type_names(mut self) -> Self {
        for index in 0..self.tokens.len().saturating_sub(1) {
            let Token::Word(word) = &self.tokens[index].token else {
                continue;
            };
            if !matches!(word.as_str(), "class" | "enum") || !self.ident(index + 1) {
                continue;
            }
            let Token::Word(name) = &self.tokens[index + 1].token else {
                unreachable!()
            };
            if let Ok(name) = Name::new(&(), name) {
                let _ = self.type_names.insert(&(), name, ());
            }
        }
        self
    }

    /// Whether `name` is a class or enum the source declares.
    pub(super) fn declared_type(&self, name: &str) -> bool {
        self.type_names.contains(&(), name).unwrap_or(false)
    }

    /// Whether the name at `index` is followed by a colon written as an
    /// annotation's, `name: T`: attached to the name and followed by a space.
    fn annotation_colon(&self, index: usize) -> bool {
        let colon = &self.tokens[index + 1];
        colon.token == Token::P(':')
            && colon.offset == self.tokens[index].end
            && matches!(self.source.as_bytes().get(colon.end), Some(b' ' | b'\t'))
    }

    /// Whether a statement starts a typed local declaration, `name: T = value`.
    /// A statement that only resembles one, such as a stray hash entry, keeps
    /// the error it had before typed locals existed: the declaration needs a
    /// type followed by `=`, or a builtin or declared type ending its line.
    pub(super) fn typed_local_ahead(&mut self) -> Result<bool> {
        let Token::Word(word) = self.token() else {
            return Ok(false);
        };
        if !self.ident(self.pos)
            || word.chars().next().is_some_and(unicode::upper)
            || !self.annotation_colon(self.pos)
        {
            return Ok(false);
        }
        let (start, structural) = (self.pos, self.type_structural_error);
        self.pos += 2;
        let parsed = self.type_expr(1, false);
        self.work.checkpoint()?;
        let next = self.significant(self.pos);
        let result = match parsed {
            Ok(_) if self.tokens[next].token == Token::Op("=") => true,
            Ok(ty) => {
                matches!(self.token(), Token::EndLine | Token::Eof) && self.declared_leaves(&ty)
            }
            // A malformed type still declares when `=` follows on its line.
            Err(_) => {
                self.tokens
                    .from(start + 2)
                    .try_fold(false, |_, lexeme| match lexeme.token {
                        Token::Op("=") => Err(true),
                        Token::EndLine | Token::Eof => Err(false),
                        _ => Ok(false),
                    })
                    == Err(true)
            }
        };
        self.pos = start;
        self.type_structural_error = structural;
        Ok(result)
    }

    /// Whether the token at `index` can start a tuple type's first element:
    /// a builtin or declared type name.
    pub(super) fn tuple_start(&self, index: usize) -> bool {
        matches!(&self.tokens[index].token, Token::Word(name)
            if crate::types::builtin_name(name).is_some() || self.declared_type(name))
    }

    /// Parses a tuple type from its `[`: an array of exactly these elements.
    pub(super) fn type_tuple(&mut self, depth: usize) -> Result<Type> {
        self.work.charge(1)?;
        let open = self.pos;
        self.bump()?;
        self.line_breaks()?;
        if self.token() == &Token::P(']') {
            self.pos = open;
            return self.err("a tuple type needs at least one element type, as in [int, string]");
        }
        let mut elements = Buffer::new();
        loop {
            elements.push(self.work, self.type_expr(depth + 1, false)?)?;
            self.line_breaks()?;
            if self.take_p(']') {
                break;
            }
            if self.token() != &Token::P(',') {
                return self.expected(Label::Char(']'));
            }
            self.bump()?;
            self.line_breaks()?;
        }
        Ok(Type {
            name: Name::default(),
            kind: TypeKind::Tuple(elements),
            nullable: false,
        })
    }

    /// Whether every leaf of a type an expression could also spell names a
    /// builtin type or one the source declares, so it reads as a type.
    pub(super) fn declared_leaves(&self, ty: &Type) -> bool {
        match &ty.kind {
            TypeKind::Named => self.declared_type(&ty.name),
            TypeKind::Array(Some(element)) => self.declared_leaves(element),
            TypeKind::Hash(Some(pair)) => {
                self.declared_leaves(&pair.0) && self.declared_leaves(&pair.1)
            }
            TypeKind::Shape(fields, _) => {
                fields.iter().all(|field| self.declared_leaves(&field.ty))
            }
            TypeKind::Union(options) | TypeKind::Tuple(options) => {
                options.iter().all(|option| self.declared_leaves(option))
            }
            _ => true,
        }
    }
}

impl Parsing<'_> {
    /// Parses a typed local declaration, `name: T = value`.
    pub(super) async fn typed_local(&self) -> Result<Statement> {
        let work = self.p().work;
        let target = {
            let mut p = self.p();
            work.charge(1)?;
            let offset = p.tokens[p.pos].offset as u32;
            let name = p.name()?;
            p.bump()?;
            p.line_breaks()?;
            let ty = p.type_expr(1, false)?;
            let equals = p.significant(p.pos);
            if p.tokens[equals].token != Token::Op("=") {
                let at = p.tokens[p.pos - 1].end;
                return Err(Error::syntax(
                    work,
                    at,
                    format_args!(
                        "typed local {} needs a value; write {}: T = value",
                        source_text(&name),
                        source_text(&name)
                    ),
                ));
            }
            p.pos = equals + 1;
            p.line_breaks()?;
            let var = p.make_at(Node::Var(name), 1, offset)?;
            Target::Typed(Boxed::new(work, Target::Value(var))?, ty)
        };
        let value = self.block_line_expr().await?;
        self.p().declare_target(&target)?;
        Ok(Statement::Assign(target, "=", value))
    }
}

/// The name and type a statement-level typed declaration declares, if the
/// target is one.
pub(crate) fn declared_local(target: &Target) -> Option<(&Name, &Type)> {
    match target {
        Target::Typed(inner, ty) => match &**inner {
            Target::Value(Expr {
                node: Node::Var(name),
                ..
            }) => Some((name, ty)),
            _ => None,
        },
        _ => None,
    }
}
