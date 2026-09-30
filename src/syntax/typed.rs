//! The declarations ADR-007 adds: typed locals, typed block parameters,
//! instance-variable declarations, type aliases and the types they annotate.

use super::{
    BlockParam, Expr, Label, Node, Parser, Parsing, Statement, Target, Token, TypeAlias,
    source_text,
};
use crate::{
    Error, Result,
    compilation::{Boxed, Buffer, Name, Type, TypeKind},
};

/// An instance or class variable a class body declares, such as
/// `@count: int = 0` or `@@total: int = 0`.
#[derive(Debug)]
pub(crate) struct Ivar {
    pub name: Name,
    pub ty: Type,
    pub offset: u32,
}

/// The typed declarations that live beside the syntax tree, keyed by the
/// source offset of the function or class that makes them, so the tree's
/// nodes keep their size and accounting.
#[derive(Debug, Default)]
pub(crate) struct Additions {
    /// Typed block parameters, by the offset of their function's `def`.
    pub blocks: Buffer<(u32, BlockParam)>,
    /// Type aliases, by the offset of the declaring module or class, or with
    /// none at the top level.
    pub aliases: Buffer<(Option<u32>, TypeAlias)>,
    /// Instance-variable declarations, by the declaring class's offset.
    pub ivars: Buffer<(u32, Ivar)>,
    /// Class-variable declarations, `@@name: T = value`, by the declaring
    /// class or module's offset. The name keeps its `@@`; the value's
    /// assignment stays in the body, where it runs in order.
    pub class_vars: Buffer<(u32, Ivar)>,
    /// The assignments of their defaults, by the declaring class's offset.
    pub defaults: Buffer<(u32, super::Stmt)>,
}

impl Parser<'_> {
    /// Records the type aliases, classes and enums declared anywhere in the
    /// source. The scan is linear in tokens the lexer already charged for;
    /// the names it keeps, and their tables, are charged to the parse's
    /// work, as the parser's other tables are.
    pub(super) fn with_type_names(mut self) -> Result<Self> {
        for index in 0..self.tokens.len().saturating_sub(2) {
            let Token::Word(word) = &self.tokens[index].token else {
                continue;
            };
            if !matches!(word.as_str(), "type" | "class" | "enum") || !self.ident(index + 1) {
                continue;
            }
            let Token::Word(name) = &self.tokens[index + 1].token else {
                unreachable!()
            };
            let alias = *word == "type";
            if alias && self.tokens[index + 2].token != Token::Op("=") {
                continue;
            }
            let name = Name::new(self.work, name)?;
            if alias {
                self.alias_names.insert(self.work, name.clone(), ())?;
            }
            self.type_names.insert(self.work, name, ())?;
        }
        Ok(self)
    }

    /// Whether `name` is a type alias the source declares.
    pub(super) fn is_alias(&self, name: &str) -> bool {
        self.alias_names.contains(&(), name).unwrap_or(false)
    }

    /// Whether `name` is a type alias, class or enum the source declares.
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

    /// Whether a statement starts a typed local or constant declaration,
    /// `name: T = value`. A statement that only resembles one, such as a
    /// stray hash entry, keeps the error it had before typed locals existed:
    /// the declaration needs a type followed by `=`, or a builtin or declared
    /// type ending its line.
    pub(super) fn typed_local_ahead(&mut self) -> Result<bool> {
        if !matches!(self.token(), Token::Word(_))
            || !self.ident(self.pos)
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

    /// Whether a class body member declares an instance variable, `@name: T`.
    pub(super) fn ivar_ahead(&self) -> bool {
        matches!(self.token(), Token::Word(w) if w.starts_with('@') && !w.starts_with("@@"))
            && self.annotation_colon(self.pos)
    }

    /// Whether a class or module body member declares a class variable,
    /// `@@name: T = value`. As for a typed local, a member that only
    /// resembles one keeps the error it had before: the declaration needs a
    /// type followed by `=`, or a builtin or declared type ending its line.
    pub(super) fn class_var_ahead(&mut self) -> Result<bool> {
        if !matches!(self.token(), Token::Word(w) if w.starts_with("@@"))
            || !self.annotation_colon(self.pos)
        {
            return Ok(false);
        }
        let (start, structural) = (self.pos, self.type_structural_error);
        self.pos += 2;
        let parsed = self.type_expr(1, false);
        self.work.checkpoint()?;
        let result = match parsed {
            Ok(_) if self.token() == &Token::Op("=") => true,
            Ok(ty) => {
                matches!(self.token(), Token::EndLine | Token::Eof) && self.declared_leaves(&ty)
            }
            Err(_) => false,
        };
        self.pos = start;
        self.type_structural_error = structural;
        Ok(result)
    }

    /// Whether `type` starts a type alias, `type Name = T`, on its line.
    pub(super) fn type_alias_ahead(&self) -> bool {
        matches!(self.token(), Token::Word(w) if w == "type")
            && self.ident(self.pos + 1)
            && self.tokens[self.pos + 2].token == Token::Op("=")
            && self.tokens[self.pos + 1].line == self.tokens[self.pos].line
    }

    /// Parses a type alias from its `type` keyword.
    pub(super) fn type_alias(&mut self) -> Result<TypeAlias> {
        self.work.charge(1)?;
        self.bump()?;
        let offset = self.tokens[self.pos].offset as u32;
        let name = self.name()?;
        // An alias's name is bound where it is declared, like a class's, and
        // read in types.
        self.namespace_entered(&name, offset as usize)?;
        if crate::types::builtin_name(&name).is_some() {
            return Err(Error::syntax(
                self.work,
                offset as usize,
                format_args!(
                    "type alias {} conflicts with a built-in type",
                    source_text(&name)
                ),
            ));
        }
        self.bump()?;
        self.line_breaks()?;
        let ty = self.type_expr(1, false)?;
        Ok(TypeAlias { name, ty, offset })
    }

    /// Whether the parameter list continues with a typed block parameter,
    /// `&name: T` or `&name?: T`.
    pub(super) fn block_param_ahead(&self) -> bool {
        self.token() == &Token::Op("&")
            && matches!(&self.tokens[self.pos + 1].token, Token::Word(w) if !w.starts_with('@'))
            && self.annotation_colon(self.pos + 1)
    }

    /// Parses a typed block parameter from its `&`: one argument type, a
    /// parenthesized list of them, and an optional `-> R` result type.
    pub(super) fn block_param(&mut self) -> Result<BlockParam> {
        let work = self.work;
        work.charge(1)?;
        let offset = self.tokens[self.pos].offset as u32;
        self.bump()?;
        let at = self.tokens[self.pos].offset;
        let Token::Word(word) = self.bump()? else {
            unreachable!()
        };
        let written = word.strip_suffix('?').unwrap_or(&word);
        self.binding_name(written, at)?;
        if written.is_empty() || super::keyword(written) || written.ends_with(['?', '!']) {
            return Err(Error::syntax(work, at, "expected block parameter name"));
        }
        let name = Name::new(work, written)?;
        self.bump()?;
        self.line_breaks()?;
        let mut params = Buffer::new();
        if self.take_p('(') {
            self.line_breaks()?;
            if !self.take_p(')') {
                loop {
                    params.push(work, self.type_expr(1, false)?)?;
                    self.line_breaks()?;
                    if self.take_p(')') {
                        break;
                    }
                    if self.token() != &Token::P(',') {
                        return self.expected(Label::Char(')'));
                    }
                    self.bump()?;
                    self.line_breaks()?;
                }
            }
        } else {
            params.push(work, self.type_expr(1, false)?)?;
        }
        let arrow = self.significant(self.pos);
        let result = if self.tokens[arrow].token == Token::Op("->") {
            self.pos = arrow + 1;
            self.line_breaks()?;
            Some(self.type_expr(1, false)?)
        } else {
            None
        };
        Ok(BlockParam {
            name,
            params,
            result,
            offset,
        })
    }

    /// Whether the token at `index` can start a tuple type's first element:
    /// a builtin type name or a type the source declares, optional or not,
    /// or a nested tuple or shape, whose leaves decide.
    pub(super) fn tuple_start(&self, index: usize) -> bool {
        match &self.tokens[index].token {
            Token::Word(name) => self.type_name(name.trim_end_matches('?')),
            Token::P('[' | '{') => true,
            _ => false,
        }
    }

    /// Whether `name` names a builtin type, one of the signature table's
    /// aliases or a type the source declares.
    fn type_name(&self, name: &str) -> bool {
        crate::types::builtin_name(name).is_some()
            || crate::signatures::alias_type(name).is_some()
            || self.declared_type(name)
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
            TypeKind::Named => self.type_name(&ty.name),
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

    /// Refuses a reference to the enclosing function's block parameter, which
    /// is not a value.
    pub(super) fn block_reference(&self, name: &str, offset: usize) -> Result<()> {
        let Some(block) = &self.block_name else {
            return Ok(());
        };
        if **block != *name || self.locals.contains(self.work, name)? {
            return Ok(());
        }
        Err(Error::syntax(
            self.work,
            offset,
            format_args!(
                "block parameter {} is not a value; run the block with `yield`, and ask `block_given?` when it is optional",
                source_text(name)
            ),
        ))
    }
}

impl<M: super::recovery::Mode> Parsing<'_, M> {
    /// Parses a typed local or constant declaration, `name: T = value`.
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

    /// Parses an instance-variable declaration in a class body, `@name: T`
    /// with an optional `= default`, returning it and the default's assignment.
    pub(super) async fn ivar(&self) -> Result<(Ivar, Option<super::Stmt>)> {
        let work = self.p().work;
        let (ivar, variable, equals) = {
            let mut p = self.p();
            work.charge(1)?;
            let offset = p.tokens[p.pos].offset;
            let Token::Word(word) = p.bump()? else {
                unreachable!()
            };
            p.binding_name(&word, offset)?;
            let name = &word[1..];
            if name.is_empty() {
                return Err(Error::syntax(
                    work,
                    offset,
                    "expected instance variable name, got instance variable",
                ));
            }
            let variable = p.make_at(Node::Var(Name::new(work, &word)?), 1, offset as u32)?;
            let name = Name::new(work, name)?;
            p.bump()?;
            p.line_breaks()?;
            let ty = p.type_expr(1, false)?;
            // A default starts on the declaration's line.
            let equals = p.token() == &Token::Op("=") && {
                p.bump()?;
                p.line_breaks()?;
                true
            };
            let ivar = Ivar {
                name,
                ty,
                offset: offset as u32,
            };
            (ivar, variable, equals)
        };
        if !equals {
            return Ok((ivar, None));
        }
        let value = self.block_line_expr().await?;
        let offset = ivar.offset;
        let assignment = Statement::Assign(Target::Value(variable), "=", value).at(offset);
        Ok((ivar, Some(assignment)))
    }
}

impl<M: super::recovery::Mode> Parsing<'_, M> {
    /// Parses a class-variable declaration in a class or module body,
    /// `@@name: T = value`, returning it and its value's assignment.
    pub(super) async fn class_var(&self) -> Result<(Ivar, super::Stmt)> {
        let work = self.p().work;
        let (declared, variable) = {
            let mut p = self.p();
            work.charge(1)?;
            let offset = p.tokens[p.pos].offset;
            let Token::Word(word) = p.bump()? else {
                unreachable!()
            };
            p.binding_name(&word, offset)?;
            if word.len() == 2 {
                return Err(Error::syntax(
                    work,
                    offset,
                    "expected class variable name, got class variable",
                ));
            }
            let name = Name::new(work, &word)?;
            let variable = p.make_at(Node::Var(name.clone()), 1, offset as u32)?;
            p.bump()?;
            p.line_breaks()?;
            let ty = p.type_expr(1, false)?;
            if p.token() != &Token::Op("=") {
                let at = p.tokens[p.pos - 1].end;
                return Err(Error::syntax(
                    work,
                    at,
                    format_args!(
                        "class variable {} needs a value; write {}: T = value",
                        source_text(&name),
                        source_text(&name)
                    ),
                ));
            }
            p.bump()?;
            p.line_breaks()?;
            let declared = Ivar {
                name,
                ty,
                offset: offset as u32,
            };
            (declared, variable)
        };
        let value = self.block_line_expr().await?;
        let offset = declared.offset;
        let assignment = Statement::Assign(Target::Value(variable), "=", value).at(offset);
        Ok((declared, assignment))
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
