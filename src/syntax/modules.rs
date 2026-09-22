use super::{Definition, Parser, Parsing, Stmt, Token, keyword};
use crate::{
    Result,
    compilation::{Boxed, Buffer, Name, Table},
};

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
    pub directives: Table<()>,
    pub(super) depth: u32,
}

impl Parser<'_> {
    pub(super) fn module_ahead(&self) -> bool {
        matches!(self.token(), Token::Word(w) if w == "module")
            && matches!(&self.tokens[self.pos + 1].token, Token::Word(w) if !keyword(w))
            && self.tokens[self.pos].line == self.tokens[self.pos + 1].line
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
        let section = (word == "private" || !self.locals.contains(self.work, word.as_str())?)
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

enum Member {
    Module,
    Method(Name, u32),
    Statement,
}

impl Parsing<'_> {
    pub(super) async fn definition(&self, name: Name, offset: u32) -> Result<Definition> {
        self.p().work.charge(1)?;
        self.definition_with_constants(name, false, offset).await
    }

    pub(super) async fn definition_with_constants(
        &self,
        name: Name,
        module: bool,
        offset: u32,
    ) -> Result<Definition> {
        let work = self.p().work;
        let (outer_locals, outer_it, parenthesized) = {
            let mut p = self.p();
            work.charge(1)?;
            let outer_locals = std::mem::take(&mut p.locals);
            if module {
                for (name, _) in outer_locals.iter(work)? {
                    if name.chars().next().is_some_and(super::unicode::upper) {
                        p.locals.insert(work, name.clone(), ())?;
                    }
                }
            }
            let outer_it = std::mem::replace(&mut p.declared_it, false);
            (outer_locals, outer_it, p.take_p('('))
        };
        let params = self.parameters(parenthesized).await?;
        let return_type = {
            let mut p = self.p();
            p.line_breaks()?;
            let return_type = if p.token() == &Token::Op("->") {
                p.bump()?;
                Some(p.type_expr(1, false)?)
            } else {
                None
            };
            p.lines()?;
            return_type
        };
        let body = self.block(&["rescue", "else", "ensure", "end"]).await?;
        let rescued = matches!(self.p().token(), Token::Word(w) if w != "end");
        let body = if rescued {
            let attempt = self.rescue_tail(body, true).await?;
            let depth = attempt.depth();
            let p = self.p();
            Buffer::from_array(
                work,
                [super::Statement::Expr(p.make_at(
                    super::Node::Try(Boxed::new(work, attempt)?),
                    depth,
                    offset,
                )?)
                .at(offset)],
            )?
        } else {
            self.p().expect_word("end")?;
            body
        };
        let mut p = self.p();
        p.locals = outer_locals;
        p.declared_it = outer_it;
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

    pub(super) async fn module(&self) -> Result<Module> {
        let work = self.p().work;
        let (mut module, outer_locals, outer_it) = {
            let mut p = self.p();
            work.charge(1)?;
            p.enter()?;
            let offset = p.tokens[p.pos].offset as u32;
            p.expect_word("module")?;
            let name = p.name()?;
            if !name.as_bytes().first().is_some_and(u8::is_ascii_uppercase) {
                return p.err("module name must start with an uppercase letter");
            }
            let outer_locals = std::mem::take(&mut p.locals);
            let outer_it = std::mem::replace(&mut p.declared_it, false);
            p.lines()?;
            let module = Module {
                offset,
                is_class: false,
                instance_methods: Buffer::new(),
                name,
                methods: Buffer::new(),
                body: Buffer::new(),
                modules: Buffer::new(),
                directives: Table::new(),
                depth: 1,
            };
            (module, outer_locals, outer_it)
        };
        let mut visibility = Visibility::Public;
        while !matches!(self.p().token(), Token::Word(w) if w == "end") {
            let mut method_visibility = visibility;
            let member = {
                let mut p = self.p();
                if p.token() == &Token::Eof {
                    return p.err("unexpected end of module");
                }
                if matches!(p.token(), Token::Word(w) if w == "private")
                    && p.tokens[p.pos + 1].token == Token::P('(')
                {
                    return p.err("private visibility directives do not take parentheses");
                }
                if let Some((word, level)) = p.visibility()? {
                    p.bump()?;
                    module.directives.insert(work, word, ())?;
                    if p.token() == &Token::P(':') {
                        loop {
                            p.expect_p(':')?;
                            let name = p.name()?;
                            let Some((_, current)) = module
                                .methods
                                .iter_mut()
                                .rev()
                                .find(|(m, _)| m.name == name)
                            else {
                                return p.err("visibility directive names an undefined method");
                            };
                            *current = level;
                            if !p.take_p(',') {
                                break;
                            }
                            p.line_breaks()?;
                        }
                        p.lines()?;
                        continue;
                    }
                    if p.token() == &Token::EndLine
                        || matches!(p.token(), Token::Word(w) if w == "end")
                    {
                        visibility = level;
                        p.lines()?;
                        continue;
                    }
                    method_visibility = level;
                }
                if p.module_ahead() {
                    Member::Module
                } else if p.word("def") {
                    let offset = p.previous()?.offset as u32;
                    p.expect_word("self")?;
                    p.expect_p('.')?;
                    let mut name = p.name()?;
                    if keyword(&name) || name.starts_with('@') {
                        return p.err("expected module method name");
                    }
                    if p.token() == &Token::Op("=") {
                        p.bump()?;
                        name = Name::join(work, &[&name, "="])?;
                    }
                    Member::Method(name, offset)
                } else if p.alias_ahead()
                    || matches!(p.token(), Token::Word(w) if w == "alias_method")
                {
                    let Token::Word(word) = p.token() else {
                        unreachable!()
                    };
                    return p.err(format_args!(
                        "{} in module {} is not supported; a module has no instance methods to rename, so define module functions with def self.name and call them on the module (Naming.display_name(person))",
                        word.as_str(),
                        module.name
                    ));
                } else if matches!(p.token(), Token::Word(w) if matches!(w.as_str(), "class" | "enum" | "property" | "getter" | "setter" | "include" | "extend"))
                {
                    return p.err("modules declare methods with def self.name and do not support classes, enums, accessors, aliases, or mixins");
                } else {
                    Member::Statement
                }
            };
            match member {
                Member::Module => {
                    let nested = self.nested_module().await?;
                    module.depth = module.depth.max(1 + nested.depth);
                    module.modules.push(work, nested)?;
                }
                Member::Method(name, offset) => {
                    let definition = self.definition_with_constants(name, true, offset).await?;
                    module.depth = module.depth.max(1 + definition.depth());
                    module.methods.push(work, (definition, method_visibility))?;
                }
                Member::Statement => {
                    let stmt = self.statement().await?;
                    module.depth = module.depth.max(1 + stmt.depth);
                    module.body.push(work, stmt)?;
                }
            }
            self.p().lines()?;
        }
        let mut p = self.p();
        p.expect_word("end")?;
        p.check_depth(module.depth)?;
        p.locals = outer_locals;
        p.declared_it = outer_it;
        p.depth -= 1;
        Ok(module)
    }
}
