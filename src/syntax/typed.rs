//! The declarations ADR-007 adds to annotations.

use super::{Label, Parser, Token};
use crate::{
    Result,
    compilation::{Buffer, Name, Type, TypeKind},
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
