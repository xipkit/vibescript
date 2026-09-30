use super::*;

impl<M: super::recovery::Mode> Parsing<'_, M> {
    pub(super) async fn raise_statement(&self) -> Result<Statement> {
        let work = self.p().work;
        {
            let p = self.p();
            work.charge(1)?;
            let line = p.tokens[p.pos - 1].line;
            if p.ends_after(p.pos - 1, line) || p.modifier_follows(line) {
                return Ok(Statement::Raise(None, None));
            }
        }
        self.p().line_breaks()?;
        let value = self.line_expr(0).await?;
        let separated = {
            let mut p = self.p();
            p.tokens[p.pos].line == p.previous()?.end_line && p.take_p(',')
        };
        let message = if separated {
            self.p().line_breaks()?;
            Some(Boxed::new(work, self.line_expr(0).await?)?)
        } else {
            None
        };
        Ok(Statement::Raise(Some(Boxed::new(work, value)?), message))
    }

    /// Parses the rescue, else and ensure clauses after a begin or function
    /// body, as Go's `parseRescueElseEnsureTail` does, given the offset of the
    /// `def` or `begin` that owns them.
    pub(super) async fn rescue_tail(
        &self,
        body: Buffer<Stmt>,
        function: bool,
        start: u32,
    ) -> Result<Try> {
        let work = self.p().work;
        work.charge(1)?;
        let owner = if function { "function" } else { "begin" };
        let mut rescues = Buffer::new();
        while matches!(self.p().token(), Token::Word(w) if w == "rescue") {
            let (offset, classes, binding, existed, declared_it) = {
                let mut p = self.p();
                let offset = p.tokens[p.pos].offset as u32;
                let line = p.tokens[p.pos].line;
                p.bump()?;
                let (classes, binding) = p.rescue_clause(offset as usize, line)?;
                let existed = match &binding {
                    Some(name) => p.locals.contains(work, name)?,
                    None => false,
                };
                let declared_it = p.declared_it;
                if let Some(name) = &binding {
                    // The binding is the word before the clause's end.
                    let at = p.tokens[p.pos - 1].offset;
                    let id = p.local_id(name, at)?;
                    p.locals.insert(work, name.clone(), id)?;
                    p.declared_it |= name == "it";
                }
                (offset, classes, binding, existed, declared_it)
            };
            let rescue_body = self.block(&["rescue", "else", "ensure", "end"]).await?;
            let mut p = self.p();
            if let Some(name) = &binding {
                if !existed {
                    p.locals.remove(work, name)?;
                }
            }
            if binding.as_deref() == Some("it") {
                p.declared_it = declared_it;
            }
            rescues.push(
                work,
                Rescue {
                    classes,
                    binding,
                    body: rescue_body,
                    offset,
                },
            )?;
        }
        let alternate = if matches!(self.p().token(), Token::Word(w) if w == "else") {
            if rescues.is_empty() {
                return self.p().err(format_args!("{owner} else requires rescue"));
            }
            self.p().bump()?;
            self.block(&["ensure", "end"]).await?
        } else {
            Buffer::new()
        };
        let ensure = if self.p().word("ensure") {
            self.block(&["end"]).await?
        } else {
            Buffer::new()
        };
        let mut p = self.p();
        p.expect_word("end")?;
        if (function || !rescues.is_empty())
            && rescues.iter().all(|r| r.body.is_empty())
            && ensure.is_empty()
        {
            return Err(Error::syntax(
                work,
                start as usize,
                format_args!("{owner} requires rescue and/or ensure"),
            ));
        }
        Ok(Try {
            modifier: false,
            body,
            rescues,
            alternate,
            ensure,
        })
    }

    pub(super) async fn rescue_modifier(&self, body: Expr) -> Result<Expr> {
        let work = self.p().work;
        work.charge(1)?;
        let offset = body.offset;
        let rescue_offset = {
            let p = self.p();
            let rescue = p.pos - 1;
            let next = p.significant(p.pos);
            if p.tokens[next].line != p.tokens[rescue].line || !p.prefix(next) {
                return Err(Error::syntax(
                    work,
                    p.tokens[rescue].offset,
                    "rescue modifier requires fallback expression",
                ));
            }
            p.tokens[rescue].offset as u32
        };
        let fallback = self.line_expr(0).await?;
        let attempt = Try {
            modifier: true,
            body: Buffer::from_array(work, [Statement::Expr(body).at(offset)])?,
            rescues: Buffer::from_array(
                work,
                [Rescue {
                    classes: Buffer::from_array(work, [crate::ErrorClass::Standard])?,
                    binding: None,
                    body: Buffer::from_array(work, [Statement::Expr(fallback).at(rescue_offset)])?,
                    offset: rescue_offset,
                }],
            )?,
            alternate: Buffer::new(),
            ensure: Buffer::new(),
        };
        let depth = attempt.depth();
        // Go locates a rescue modifier at its keyword.
        self.p()
            .make_at(Node::Try(Boxed::new(work, attempt)?), depth, rescue_offset)
    }
}

impl Parser<'_> {
    /// Reports whether a statement modifier follows on `line`.
    pub(super) fn modifier_follows(&self, line: usize) -> bool {
        let next = self.significant(self.pos);
        self.tokens[next].line == line
            && matches!(&self.tokens[next].token, Token::Word(w) if matches!(w.as_str(), "if" | "unless" | "while" | "until"))
    }

    /// Parses a rescue clause's error classes and binding, as Go's
    /// `parseRescueClause` does, after the `rescue` at `rescue`.
    fn rescue_clause(
        &mut self,
        rescue: usize,
        line: usize,
    ) -> Result<(Buffer<crate::ErrorClass>, Option<Name>)> {
        let work = self.work;
        let mut classes = Buffer::from_array(work, [crate::ErrorClass::Standard])?;
        let next = self.significant(self.pos);
        if self.tokens[next].line != line {
            return Ok((classes, None));
        }
        let grouped = self.tokens[next].token == Token::P('(');
        if grouped || self.ident(next) {
            self.pos = next;
            if grouped {
                self.bump()?;
                self.line_breaks()?;
            }
            let mut options = Vec::new();
            loop {
                let offset = self.tokens[self.pos].offset;
                options.push((offset, self.type_atom(1)?));
                let pipe = self.significant(self.pos);
                if self.tokens[pipe].token != Token::P('|') {
                    break;
                }
                self.pos = pipe + 1;
                self.line_breaks()?;
            }
            classes.truncate(0);
            let single = options.len() == 1;
            for (offset, ty) in &options {
                let offset = if single { rescue } else { *offset };
                error_class(ty, &mut classes, work, offset)?;
            }
            if grouped {
                self.line_breaks()?;
                self.expect_p(')')?;
            }
        } else if self.tokens[next].token != Token::Op("=>") {
            if self.tokens[next].token == Token::Op("->") {
                self.pos = next;
                return self.err("rescue binding must use =>");
            }
            return Ok((classes, None));
        }
        let arrow = self.significant(self.pos);
        if self.tokens[arrow].line != line {
            return Ok((classes, None));
        }
        match self.tokens[arrow].token {
            Token::Op("->") => {
                self.pos = arrow;
                self.err("rescue binding must use =>")
            }
            Token::Op("=>") => {
                let name = self.significant(arrow + 1);
                self.pos = name;
                if !self.ident(name) {
                    return self.err("rescue binding must be an identifier");
                }
                Ok((classes, Some(self.name()?)))
            }
            _ => Ok((classes, None)),
        }
    }
}

/// Adds the error class a rescue type names, or reports Go's rejection at `offset`.
fn error_class(
    ty: &crate::compilation::Type,
    out: &mut Buffer<crate::ErrorClass>,
    work: &dyn crate::compilation::Work,
    offset: usize,
) -> Result<()> {
    work.charge(1)?;
    use crate::compilation::TypeKind;
    match &ty.kind {
        TypeKind::Named | TypeKind::Scalar(_) | TypeKind::Array(None) | TypeKind::Hash(None) => {
            if let Some(class) = crate::ErrorClass::from_name(&ty.name) {
                return out.push(work, class);
            }
            let suffix = if ty.nullable && !matches!(ty.kind, TypeKind::Named) {
                "?"
            } else {
                ""
            };
            Err(Error::syntax(
                work,
                offset,
                format_args!(
                    "unknown rescue error type {}",
                    source_text(&format!("{}{suffix}", ty.name))
                ),
            ))
        }
        _ => {
            let mut spelling = Vec::new();
            crate::shapes::format(ty, &mut spelling)?;
            work.bytes(spelling.len())?;
            Err(Error::syntax(
                work,
                offset,
                format_args!(
                    "rescue type must be an error class, got {}",
                    source_text(&String::from_utf8_lossy(&spelling))
                ),
            ))
        }
    }
}
