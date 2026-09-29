use super::{
    Definition, Expr, Label, Node, ParamKind, Parameter, Parser, Statement, Token,
    modules::{Module, Visibility},
    source_text,
};
use crate::{
    Result,
    compilation::{Buffer, Name, Text},
};

impl Parser<'_> {
    /// Reports whether `alias` starts a declaration. Go requires a name or
    /// symbol on the same line and otherwise reads `alias` as an identifier.
    pub(super) fn alias_ahead(&self) -> bool {
        let next = &self.tokens[self.pos + 1];
        matches!(self.token(), Token::Word(w) if w == "alias")
            && next.line == self.tokens[self.pos].line
            && (self.ident(self.pos + 1)
                || matches!(next.token, Token::Symbol(_) | Token::QuotedSymbol(_)))
    }

    /// Reads the two names of an alias declaration, which Go requires on the
    /// line of `alias`.
    pub(super) fn alias_names(&mut self) -> Result<(Name, Name)> {
        self.work.charge(1)?;
        let line = self.tokens[self.pos].line;
        self.bump()?;
        let new = self.alias_name(line)?;
        let old = self.alias_name(line)?;
        Ok((new, old))
    }

    fn alias_name(&mut self, line: usize) -> Result<Name> {
        self.pos = self.significant(self.pos);
        if self.tokens[self.pos].line == line {
            if self.ident(self.pos) {
                return self.method_name();
            }
            if let Some(name) = self.symbol_name()? {
                return Ok(name);
            }
        }
        self.expected(Label::Text("alias name"))
    }

    /// Reads `alias_method :new, :old`, optionally parenthesized, as Go's
    /// `parseAliasMethodStatement` does.
    pub(super) fn alias_method(&mut self) -> Result<(Name, Name)> {
        self.work.charge(1)?;
        self.bump()?;
        let parenthesized = self.tokens[self.significant(self.pos)].token == Token::P('(');
        if parenthesized {
            self.line_breaks()?;
            self.bump()?;
        }
        let new = self.expect_symbol()?;
        self.line_breaks()?;
        self.expect_p(',')?;
        let old = self.expect_symbol()?;
        if parenthesized {
            self.line_breaks()?;
            self.expect_p(')')?;
        }
        Ok((new, old))
    }

    fn expect_symbol(&mut self) -> Result<Name> {
        self.line_breaks()?;
        match self.symbol_name()? {
            Some(name) => Ok(name),
            None => self.expected(Label::Text("symbol")),
        }
    }

    /// Copies the aliased instance method, or records Go's compile error when
    /// it is not defined yet.
    pub(super) fn class_alias(
        &mut self,
        class: &mut Module,
        new: Name,
        old: Name,
        offset: u32,
    ) -> Result<()> {
        self.work.charge(1)?;
        self.work.charge(class.instance_methods.len())?;
        let Some((target, visibility)) = class
            .instance_methods
            .iter()
            .rev()
            .find(|(m, _)| m.name == old)
        else {
            if class.missing.is_none() {
                let message = format!(
                    "alias target method {old} is not defined on class {}",
                    class.name
                );
                class.missing = Some(Text::new(self.work, &message)?);
            }
            return Ok(());
        };
        let mut definition = super::work::definition(self.work, target)?;
        self.work.checkpoint()?;
        let visibility = *visibility;
        definition.name = new;
        class
            .instance_methods
            .push(self.work, (definition, visibility))?;
        let index = class.instance_methods.len() - 1;
        class.aliases.push(self.work, (index, offset))?;
        self.note(|record| {
            record.aliases.push(super::record::ClassAlias {
                class: class.offset,
                index,
                offset,
                target: old.to_string(),
            });
        });
        Ok(())
    }

    /// Reads an accessor declaration from its keyword, as Go's
    /// `parsePropertyDecl` does.
    pub(super) fn class_properties(
        &mut self,
        class: &mut Module,
        kind: &str,
        visibility: Visibility,
    ) -> Result<()> {
        self.work.charge(1)?;
        self.bump()?;
        self.line_breaks()?;
        loop {
            let offset = self.tokens[self.pos].offset as u32;
            if !self.ident(self.pos) {
                if let Token::Symbol(name) = self.token() {
                    let name = source_text(name);
                    return self.err(format_args!(
                        "property takes a bare name: property {name}, not property :{name}"
                    ));
                }
                if let Token::QuotedSymbol(name) = self.token() {
                    let name = String::from_utf8_lossy(name);
                    let name = source_text(&name);
                    return self.err(format_args!(
                        "property takes a bare name: property {name}, not property :{name}"
                    ));
                }
                return self.expected(Label::Text("property name"));
            }
            let name = self.name()?;
            let colon = self.significant(self.pos);
            let ty = if self.tokens[colon].token == Token::P(':') {
                self.pos = colon + 1;
                self.line_breaks()?;
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
            let comma = self.significant(self.pos);
            if self.tokens[comma].token != Token::P(',') {
                break;
            }
            self.pos = comma + 1;
            self.line_breaks()?;
        }
        Ok(())
    }
}
