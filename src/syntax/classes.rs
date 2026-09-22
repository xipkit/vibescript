use super::{
    Definition, Expr, Node, ParamKind, Parameter, Parser, Parsing, Statement, Token, keyword,
    modules::{Module, Visibility},
};
use crate::{
    Result,
    compilation::{Buffer, Name, Table},
};

enum Member {
    Method(Name, bool, u32),
    Statement,
    Declared,
}

impl Parsing<'_> {
    pub(super) async fn class(&self) -> Result<Module> {
        let work = self.p().work;
        let (mut class, outer_locals, outer_it) = {
            let mut p = self.p();
            work.charge(1)?;
            p.enter()?;
            let offset = p.previous()?.offset as u32;
            let name = p.name()?;
            if keyword(&name) || name.starts_with('@') {
                return p.err("expected class name");
            }
            if p.token() == &Token::Op("<") {
                return p.err("class inheritance is not supported; call shared module functions");
            }
            let outer_locals = std::mem::take(&mut p.locals);
            let outer_it = std::mem::replace(&mut p.declared_it, false);
            p.lines()?;
            let class = Module {
                offset,
                is_class: true,
                instance_methods: Buffer::new(),
                name,
                methods: Buffer::new(),
                body: Buffer::new(),
                modules: Buffer::new(),
                directives: Table::new(),
                depth: 1,
            };
            (class, outer_locals, outer_it)
        };
        let mut visibility = Visibility::Public;
        while !matches!(self.p().token(), Token::Word(w) if w == "end") {
            let mut method_visibility = visibility;
            let member = {
                let mut p = self.p();
                if p.token() == &Token::Eof {
                    return p.err("unexpected end of class");
                }
                work.charge(1)?;
                if matches!(p.token(), Token::Word(w) if w == "private")
                    && p.tokens[p.pos + 1].token == Token::P('(')
                {
                    return p.err("private visibility directives do not take parentheses");
                }
                if let Some((word, level)) = p.visibility()? {
                    p.bump()?;
                    class.directives.insert(work, word, ())?;
                    if p.token() == &Token::P(':') {
                        loop {
                            let name = p.class_alias_name(true)?;
                            work.charge(class.instance_methods.len() + class.methods.len())?;
                            let instance = class
                                .instance_methods
                                .iter_mut()
                                .rev()
                                .find(|(m, _)| m.name == name);
                            let target = instance.or_else(|| {
                                class.methods.iter_mut().rev().find(|(m, _)| m.name == name)
                            });
                            let Some((_, current)) = target else {
                                return p.err("visibility directive names an undefined method");
                            };
                            *current = level;
                            if !p.take_p(',') {
                                break;
                            }
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
                if p.word("def") {
                    let offset = p.previous()?.offset as u32;
                    let class_method = p.word("self");
                    if class_method {
                        p.expect_p('.')?;
                    }
                    let name = p.class_method_name(class_method)?;
                    Member::Method(name, class_method, offset)
                } else if matches!(p.token(), Token::Word(w) if matches!(w.as_str(), "property" | "getter" | "setter"))
                {
                    let Token::Word(kind) = p.bump()? else {
                        unreachable!()
                    };
                    p.class_properties(&mut class, &kind, method_visibility)?;
                    Member::Declared
                } else if p.word("alias_method") {
                    let parens = p.take_p('(');
                    let new = p.class_alias_name(true)?;
                    p.expect_p(',')?;
                    let old = p.class_alias_name(true)?;
                    if parens {
                        p.expect_p(')')?;
                    }
                    p.class_alias(&mut class, new, old)?;
                    Member::Declared
                } else if matches!(p.token(), Token::Word(w) if w == "alias")
                    && p.tokens[p.pos].line == p.tokens[p.pos + 1].line
                    && matches!(&p.tokens[p.pos + 1].token, Token::Word(w) if !keyword(w))
                    || matches!(p.token(), Token::Word(w) if w == "alias")
                        && p.tokens[p.pos + 1].token == Token::P(':')
                {
                    p.bump()?;
                    let line = p.previous()?.line;
                    let new = p.class_alias_name(false)?;
                    if p.tokens[p.pos].line != line {
                        return p.err("alias names must be on the same line");
                    }
                    let old = p.class_alias_name(false)?;
                    p.class_alias(&mut class, new, old)?;
                    Member::Declared
                } else if p.removed_mixin()? {
                    return p
                        .err("include and extend are not supported; call shared module functions");
                } else {
                    Member::Statement
                }
            };
            match member {
                Member::Method(name, class_method, offset) => {
                    let definition = self
                        .definition_with_constants(name.clone(), true, offset)
                        .await?;
                    class.depth = class.depth.max(1 + definition.depth());
                    if name == "initialize" {
                        method_visibility = Visibility::Private;
                    }
                    let methods = if class_method {
                        &mut class.methods
                    } else {
                        &mut class.instance_methods
                    };
                    methods.push(work, (definition, method_visibility))?;
                }
                Member::Statement => {
                    let stmt = self.statement().await?;
                    class.depth = class.depth.max(1 + stmt.depth);
                    class.body.push(work, stmt)?;
                }
                Member::Declared => (),
            }
            self.p().lines()?;
        }
        let mut p = self.p();
        p.expect_word("end")?;
        p.check_depth(class.depth)?;
        p.locals = outer_locals;
        p.declared_it = outer_it;
        p.depth -= 1;
        Ok(class)
    }
}

impl Parser<'_> {
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
            let body = || {
                let value = Expr {
                    offset,
                    node: Node::Var(Name::join(self.work, &["@", &name])?),
                    depth: 1,
                };
                Buffer::from_array(self.work, [Statement::Return(Some(value)).at(offset)])
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
                            body: body()?,
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
                            body: body()?,
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
