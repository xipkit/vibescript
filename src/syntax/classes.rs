use super::{
    Definition, Expr, Node, ParamKind, Parameter, Parser, Statement, Token, keyword,
    modules::{Module, Visibility},
};
use crate::{
    Result,
    compilation::{Buffer, Name, Table},
};

impl Parser<'_> {
    pub(super) fn class(&mut self) -> Result<Module> {
        self.work.charge(1)?;
        self.enter()?;
        let offset = self.previous()?.offset as u32;
        let name = self.name()?;
        if keyword(&name) || name.starts_with('@') {
            return self.err("expected class name");
        }
        if self.token() == &Token::Op("<") {
            return self.err("class inheritance is not supported; call shared module functions");
        }
        let outer_locals = std::mem::take(&mut self.locals);
        let outer_it = std::mem::replace(&mut self.declared_it, false);
        let mut class = Module {
            offset,
            is_class: true,
            instance_methods: Buffer::new(),
            name,
            methods: Buffer::new(),
            body: Buffer::new(),
            modules: Buffer::new(),
            directives: Table::new(),
        };
        let mut visibility = Visibility::Public;
        self.lines()?;
        while !matches!(self.token(), Token::Word(w) if w == "end") {
            if self.token() == &Token::Eof {
                return self.err("unexpected end of class");
            }
            self.work.charge(1)?;
            let mut method_visibility = visibility;
            if matches!(self.token(), Token::Word(w) if w == "private")
                && self.tokens[self.pos + 1].token == Token::P('(')
            {
                return self.err("private visibility directives do not take parentheses");
            }
            if let Some((word, level)) = self.visibility()? {
                self.bump()?;
                class.directives.insert(self.work, word, ())?;
                if self.token() == &Token::P(':') {
                    loop {
                        let name = self.class_alias_name(true)?;
                        self.work
                            .charge(class.instance_methods.len() + class.methods.len())?;
                        let instance = class
                            .instance_methods
                            .iter_mut()
                            .rev()
                            .find(|(m, _)| m.name == name);
                        let target = instance.or_else(|| {
                            class.methods.iter_mut().rev().find(|(m, _)| m.name == name)
                        });
                        let Some((_, current)) = target else {
                            return self.err("visibility directive names an undefined method");
                        };
                        *current = level;
                        if !self.take_p(',') {
                            break;
                        }
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
            if self.word("def") {
                let offset = self.previous()?.offset as u32;
                let class_method = self.word("self");
                if class_method {
                    self.expect_p('.')?;
                }
                let name = self.class_method_name(class_method)?;
                let definition = self.definition_with_constants(name.clone(), true, offset)?;
                if name == "initialize" {
                    method_visibility = Visibility::Private;
                }
                let methods = if class_method {
                    &mut class.methods
                } else {
                    &mut class.instance_methods
                };
                methods.push(self.work, (definition, method_visibility))?;
            } else if matches!(self.token(), Token::Word(w) if matches!(w.as_str(), "property" | "getter" | "setter"))
            {
                let Token::Word(kind) = self.bump()? else {
                    unreachable!()
                };
                self.class_properties(&mut class, &kind, method_visibility)?;
            } else if self.word("alias_method") {
                let parens = self.take_p('(');
                let new = self.class_alias_name(true)?;
                self.expect_p(',')?;
                let old = self.class_alias_name(true)?;
                if parens {
                    self.expect_p(')')?;
                }
                self.class_alias(&mut class, new, old)?;
            } else if matches!(self.token(), Token::Word(w) if w == "alias")
                && self.tokens[self.pos].line == self.tokens[self.pos + 1].line
                && matches!(&self.tokens[self.pos + 1].token, Token::Word(w) if !keyword(w))
                || matches!(self.token(), Token::Word(w) if w == "alias")
                    && self.tokens[self.pos + 1].token == Token::P(':')
            {
                self.bump()?;
                let line = self.previous()?.line;
                let new = self.class_alias_name(false)?;
                if self.tokens[self.pos].line != line {
                    return self.err("alias names must be on the same line");
                }
                let old = self.class_alias_name(false)?;
                self.class_alias(&mut class, new, old)?;
            } else if self.removed_mixin()? {
                return self
                    .err("include and extend are not supported; call shared module functions");
            } else {
                class.body.push(self.work, self.statement()?)?;
            }
            self.lines()?;
        }
        self.expect_word("end")?;
        self.locals = outer_locals;
        self.declared_it = outer_it;
        self.depth -= 1;
        Ok(class)
    }

    fn removed_mixin(&self) -> Result<bool> {
        let Token::Word(word) = self.token() else {
            return Ok(false);
        };
        if !matches!(word.as_str(), "include" | "extend") {
            return Ok(false);
        }
        let next = &self.tokens[self.pos + 1];
        let same_line = next.line == self.tokens[self.pos].line;
        Ok((same_line
            && (next.token == Token::P('(')
                || matches!(&next.token, Token::Word(w) if !keyword(w) || w == "self")))
            || (!self.locals.contains(self.work, word.as_str())?
                && (!same_line
                    || matches!(next.token, Token::EndLine | Token::Eof | Token::P('}'))
                    || matches!(&next.token, Token::Word(w) if matches!(w.as_str(), "end" | "else" | "elsif" | "ensure" | "rescue")))))
    }

    fn class_method_name(&mut self, class: bool) -> Result<Name> {
        self.work.charge(1)?;
        let (mut name, operator) = if !class && self.take_p('[') {
            self.expect_p(']')?;
            (Name::new(self.work, "[]")?, true)
        } else if !class
            && matches!(
                self.token(),
                Token::Op(
                    "+" | "-"
                        | "*"
                        | "/"
                        | "%"
                        | "**"
                        | "<<"
                        | "&"
                        | "=="
                        | "!="
                        | "<"
                        | "<="
                        | ">"
                        | ">="
                        | "<=>"
                )
            )
        {
            let Token::Op(op) = self.bump()? else {
                unreachable!()
            };
            (Name::new(self.work, op)?, true)
        } else {
            let name = self.name()?;
            if keyword(&name) || name.starts_with('@') {
                return self.err("expected method name");
            }
            (name, false)
        };
        if (!operator || name == "[]") && self.token() == &Token::Op("=") {
            self.bump()?;
            name = Name::join(self.work, &[&name, "="])?;
        }
        Ok(name)
    }

    fn class_alias_name(&mut self, symbol: bool) -> Result<Name> {
        self.work.charge(1)?;
        if self.take_p(':') {
            let Expr {
                node: Node::Literal(value),
                ..
            } = self.symbol()?
            else {
                return self.err("expected method symbol");
            };
            let bytes = value.as_bytes().unwrap();
            self.work.bytes(bytes.len())?;
            let name = std::str::from_utf8(bytes)
                .map_err(|_| super::unsupported(self.work, "method names must be UTF-8"))?;
            return Name::new(self.work, name);
        }
        if symbol {
            return self.err("expected method name symbol");
        }
        let name = self.name()?;
        if keyword(&name) || name.starts_with('@') {
            return self.err("expected method alias name");
        }
        Ok(name)
    }

    fn class_alias(&self, class: &mut Module, new: Name, old: Name) -> Result<()> {
        self.work.charge(1)?;
        self.work.charge(class.instance_methods.len())?;
        let Some((target, visibility)) = class
            .instance_methods
            .iter()
            .rev()
            .find(|(m, _)| m.name == old)
        else {
            return self.err("alias target method is not defined on class");
        };
        let mut definition = super::work::definition(self.work, target)?;
        self.work.checkpoint()?;
        let visibility = *visibility;
        definition.name = new;
        class
            .instance_methods
            .push(self.work, (definition, visibility))?;
        Ok(())
    }

    fn class_properties(
        &mut self,
        class: &mut Module,
        kind: &str,
        visibility: Visibility,
    ) -> Result<()> {
        self.work.charge(1)?;
        loop {
            let offset = self.tokens[self.pos].offset as u32;
            let name = self.name()?;
            if keyword(&name) || name.starts_with('@') {
                return self.err("expected property name");
            }
            let ty = if self.take_p(':') {
                Some(self.type_expr(1, false)?)
            } else {
                None
            };
            if kind != "setter" {
                class.instance_methods.push(
                    self.work,
                    (
                        Definition {
                            private: false,
                            offset,
                            accessor: Some((name.clone(), false)),
                            name: name.clone(),
                            params: Buffer::new(),
                            body: Buffer::from_array(
                                self.work,
                                [Statement::Return(Some(Expr {
                                    offset,
                                    node: Node::Var(Name::join(self.work, &["@", &name])?),
                                    depth: 1,
                                }))
                                .at(offset)],
                            )?,
                            return_type: ty.as_ref().map(|ty| ty.copy(self.work)).transpose()?,
                        },
                        visibility,
                    ),
                )?;
            }
            if kind != "getter" {
                class.instance_methods.push(
                    self.work,
                    (
                        Definition {
                            private: false,
                            offset,
                            accessor: Some((name.clone(), true)),
                            name: Name::join(self.work, &[&name, "="])?,
                            params: Buffer::from_array(
                                self.work,
                                [Parameter {
                                    ivar: Some(name.clone()),
                                    name: Name::new(self.work, "value")?,
                                    kind: ParamKind::Positional,
                                    default: None,
                                    ty,
                                }],
                            )?,
                            body: Buffer::from_array(
                                self.work,
                                [Statement::Return(Some(Expr {
                                    offset,
                                    node: Node::Var(Name::join(self.work, &["@", &name])?),
                                    depth: 1,
                                }))
                                .at(offset)],
                            )?,
                            return_type: None,
                        },
                        visibility,
                    ),
                )?;
            }
            if !self.take_p(',') {
                break;
            }
        }
        Ok(())
    }
}
