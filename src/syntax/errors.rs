use super::*;

impl Parsing<'_> {
    pub(super) async fn raise_statement(&self) -> Result<Statement> {
        let work = self.p().work;
        {
            let p = self.p();
            work.charge(1)?;
            if !p.starts_expression()
                || matches!(p.token(), Token::Word(w) if matches!(w.as_str(), "if"|"unless"|"while"|"until"))
            {
                return Ok(Statement::Raise(None, None));
            }
        }
        let value = self.line_expr(0).await?;
        let separated = {
            let mut p = self.p();
            p.tokens[p.pos].line == p.previous()?.end_line && p.take_p(',')
        };
        let message = if separated {
            Some(Boxed::new(work, self.line_expr(0).await?)?)
        } else {
            None
        };
        Ok(Statement::Raise(Some(Boxed::new(work, value)?), message))
    }

    pub(super) async fn rescue_tail(&self, body: Buffer<Stmt>, function: bool) -> Result<Try> {
        let work = self.p().work;
        work.charge(1)?;
        let mut rescues = Buffer::new();
        while self.p().word("rescue") {
            let (offset, classes, binding, existed, declared_it) = {
                let mut p = self.p();
                let token = &p.tokens[p.pos - 1];
                let offset = token.offset as u32;
                let line = token.line;
                let mut classes = Buffer::from_array(work, [crate::ErrorClass::Standard])?;
                let mut binding = None;
                if p.tokens[p.pos].line == line {
                    if matches!(p.token(), Token::Word(w) if !reserved(w))
                        || p.token() == &Token::P('(')
                    {
                        let grouped = p.take_p('(');
                        let ty = p.type_expr(1, false)?;
                        if grouped {
                            p.expect_p(')')?;
                        }
                        classes.truncate(0);
                        error_classes(&ty, &mut classes, work, offset as usize)?;
                    }
                    if p.tokens[p.pos].line == line && p.token() == &Token::Op("=>") {
                        p.bump()?;
                        if p.tokens[p.pos].line != line {
                            return p.err("rescue binding must be an identifier");
                        }
                        let name = p.name()?;
                        if name.starts_with('@') {
                            return p.err("rescue binding must be an identifier");
                        }
                        binding = Some(name);
                    }
                    if p.token() == &Token::Op("->") {
                        return p.err("rescue binding must use =>");
                    }
                }
                let existed = match &binding {
                    Some(name) => p.locals.contains(work, name)?,
                    None => false,
                };
                let declared_it = p.declared_it;
                if let Some(name) = &binding {
                    p.locals.insert(work, name.clone(), ())?;
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
        let alternate = if self.p().word("else") {
            if rescues.is_empty() {
                return self.p().err("else requires rescue");
            }
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
        if (function || !rescues.is_empty())
            && rescues.iter().all(|r| r.body.is_empty())
            && ensure.is_empty()
        {
            return p.err("begin requires rescue and/or ensure");
        }
        p.expect_word("end")?;
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
            p.tokens[p.pos - 1].offset as u32
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
        self.p()
            .make_at(Node::Try(Boxed::new(work, attempt)?), depth, offset)
    }
}

fn error_classes(
    ty: &crate::compilation::Type,
    out: &mut Buffer<crate::ErrorClass>,
    work: &dyn crate::compilation::Work,
    offset: usize,
) -> Result<()> {
    work.charge(1)?;
    let invalid = || Error::syntax(work, offset, "invalid rescue exception type");
    match &ty.kind {
        crate::compilation::TypeKind::Named => {
            let class = crate::ErrorClass::from_name(&ty.name).ok_or_else(invalid)?;
            out.push(work, class)?;
        }
        crate::compilation::TypeKind::Union(options) => {
            for ty in options {
                error_classes(ty, out, work, offset)?;
            }
        }
        _ => return Err(invalid()),
    }
    Ok(())
}
