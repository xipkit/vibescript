use super::{Definition, Parser, Stmt, Token, keyword};
use crate::{
    Result,
    compilation::{Boxed, Buffer, Name},
};
use std::collections::HashSet;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Visibility {
    #[default]
    Public,
    Private,
    Protected,
}

#[derive(Debug)]
pub(crate) struct Module {
    pub offset: u32,
    pub is_class: bool,
    pub instance_methods: Buffer<(Definition, Visibility)>,
    pub name: Name,
    pub methods: Buffer<(Definition, Visibility)>,
    pub body: Buffer<Stmt>,
    pub modules: Buffer<Module>,
    pub directives: HashSet<Name>,
}

impl Parser<'_> {
    pub(super) fn module_ahead(&self) -> bool {
        matches!(self.token(), Token::Word(w) if w == "module")
            && matches!(&self.tokens[self.pos + 1].token, Token::Word(w) if !keyword(w))
            && self.tokens[self.pos].line == self.tokens[self.pos + 1].line
    }

    pub(super) fn definition(&mut self, name: Name, offset: u32) -> Result<Definition> {
        self.work.charge(1)?;
        self.definition_with_constants(name, false, offset)
    }

    pub(super) fn definition_with_constants(
        &mut self,
        name: Name,
        module: bool,
        offset: u32,
    ) -> Result<Definition> {
        self.work.charge(1)?;
        #[expect(
            clippy::mutable_key_type,
            reason = "Name hashes immutable text; budget counters do not affect key identity."
        )]
        let outer_locals = std::mem::take(&mut self.locals);
        if module {
            self.locals.extend(
                outer_locals
                    .iter()
                    .filter(|name| name.chars().next().is_some_and(super::unicode::upper))
                    .cloned(),
            );
        }
        let outer_it = std::mem::replace(&mut self.declared_it, false);
        let parenthesized = self.take_p('(');
        let params = self.parameters(parenthesized)?;
        self.line_breaks()?;
        let return_type = if self.token() == &Token::Op("->") {
            self.bump()?;
            Some(self.type_expr(1, false)?)
        } else {
            None
        };
        self.lines()?;
        let body = self.block(&["rescue", "else", "ensure", "end"])?;
        let body = if matches!(self.token(), Token::Word(w) if w != "end") {
            let attempt = self.rescue_tail(body, true)?;
            let depth = attempt.depth();
            Buffer::from_array(
                self.work,
                [super::Statement::Expr(self.make_at(
                    super::Node::Try(Boxed::new(self.work, attempt)?),
                    depth,
                    offset,
                )?)
                .at(offset)],
            )?
        } else {
            self.expect_word("end")?;
            body
        };
        self.locals = outer_locals;
        self.declared_it = outer_it;
        Ok(Definition {
            private: false,
            offset,
            accessor: None,
            name,
            params,
            body,
            return_type,
        })
    }

    pub(super) fn module(&mut self) -> Result<Module> {
        self.work.charge(1)?;
        self.enter()?;
        let offset = self.tokens[self.pos].offset as u32;
        self.expect_word("module")?;
        let name = self.name()?;
        if !name.as_bytes().first().is_some_and(u8::is_ascii_uppercase) {
            return self.err("module name must start with an uppercase letter");
        }
        #[expect(
            clippy::mutable_key_type,
            reason = "Name hashes immutable text; budget counters do not affect key identity."
        )]
        let outer_locals = std::mem::take(&mut self.locals);
        let outer_it = std::mem::replace(&mut self.declared_it, false);
        let mut module = Module {
            offset,
            is_class: false,
            instance_methods: Buffer::new(),
            name,
            methods: Buffer::new(),
            body: Buffer::new(),
            modules: Buffer::new(),
            directives: HashSet::new(),
        };
        let mut visibility = Visibility::Public;
        self.lines()?;
        while !matches!(self.token(), Token::Word(w) if w == "end") {
            if self.token() == &Token::Eof {
                return self.err("unexpected end of module");
            }
            let mut method_visibility = visibility;
            if matches!(self.token(), Token::Word(w) if w == "private")
                && self.tokens[self.pos + 1].token == Token::P('(')
            {
                return self.err("private visibility directives do not take parentheses");
            }
            if let Some((word, level)) = self.visibility()? {
                self.bump()?;
                module.directives.insert(word);
                if self.token() == &Token::P(':') {
                    loop {
                        self.expect_p(':')?;
                        let name = self.name()?;
                        let Some((_, current)) = module
                            .methods
                            .iter_mut()
                            .rev()
                            .find(|(m, _)| m.name == name)
                        else {
                            return self.err("visibility directive names an undefined method");
                        };
                        *current = level;
                        if !self.take_p(',') {
                            break;
                        }
                        self.line_breaks()?;
                    }
                    self.lines()?;
                    continue;
                }
                if self.token() == &Token::EndLine
                    || matches!(self.token(), Token::Word(w) if w == "end")
                {
                    visibility = level;
                    self.lines()?;
                    continue;
                }
                method_visibility = level;
            }
            if self.module_ahead() {
                module.modules.push(self.work, self.module()?)?;
            } else if self.word("def") {
                let offset = self.previous()?.offset as u32;
                self.expect_word("self")?;
                self.expect_p('.')?;
                let mut name = self.name()?;
                if keyword(&name) || name.starts_with('@') {
                    return self.err("expected module method name");
                }
                if self.token() == &Token::Op("=") {
                    self.bump()?;
                    name = Name::join(self.work, &[&name, "="])?;
                }
                let definition = self.definition_with_constants(name, true, offset)?;
                module
                    .methods
                    .push(self.work, (definition, method_visibility))?;
            } else if matches!(self.token(), Token::Word(w) if matches!(w.as_str(), "class" | "enum" | "property" | "getter" | "setter" | "alias" | "include" | "extend"))
            {
                return self.err("modules declare methods with def self.name and do not support classes, enums, accessors, aliases, or mixins");
            } else {
                module.body.push(self.work, self.statement()?)?;
            }
            self.lines()?;
        }
        self.expect_word("end")?;
        self.locals = outer_locals;
        self.declared_it = outer_it;
        self.depth -= 1;
        Ok(module)
    }

    pub(super) fn visibility(&self) -> Result<Option<(Name, Visibility)>> {
        let Token::Word(word) = self.token() else {
            return Ok(None);
        };
        if self.tokens[self.pos + 1].token == Token::P('(') {
            return Ok(None);
        }
        let level = match word.as_str() {
            "public" => Visibility::Public,
            "private" => Visibility::Private,
            "protected" => Visibility::Protected,
            _ => return Ok(None),
        };
        let next = &self.tokens[self.pos + 1];
        let section = (word == "private" || !self.locals.contains(word.as_str()))
            && (matches!(next.token, Token::EndLine | Token::Eof)
                || matches!(&next.token, Token::Word(w) if w == "end"));
        let inline = next.line == self.tokens[self.pos].line
            && (next.token == Token::P(':')
                || matches!(&next.token, Token::Word(w) if matches!(w.as_str(), "def" | "property" | "getter" | "setter")));
        if section || inline {
            Ok(Some((Name::new(self.work, word)?, level)))
        } else {
            Ok(None)
        }
    }
}
