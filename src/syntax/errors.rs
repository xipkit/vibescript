use super::*;

impl Parser<'_> {
    pub(super) fn raise_statement(&mut self) -> Result<Statement> {
        self.work.charge(1)?;
        if !self.starts_expression()
            || matches!(self.token(), Token::Word(w) if matches!(w.as_str(), "if"|"unless"|"while"|"until"))
        {
            return Ok(Statement::Raise(None, None));
        }
        let value = self.line_expr(0)?;
        let message = if self.tokens[self.pos].line == self.previous()?.end_line && self.take_p(',')
        {
            Some(Box::new(self.line_expr(0)?))
        } else {
            None
        };
        Ok(Statement::Raise(Some(Box::new(value)), message))
    }

    pub(super) fn rescue_tail(&mut self, body: Vec<Stmt>, function: bool) -> Result<Try> {
        self.work.charge(1)?;
        let mut rescues = Vec::new();
        while self.word("rescue") {
            let token = &self.tokens[self.pos - 1];
            let offset = token.offset as u32;
            let line = token.line;
            let mut classes = vec![crate::ErrorClass::Standard];
            let mut binding = None;
            if self.tokens[self.pos].line == line {
                if matches!(self.token(), Token::Word(w) if !reserved(w))
                    || self.token() == &Token::P('(')
                {
                    let grouped = self.take_p('(');
                    let ty = self.type_expr(1, false)?;
                    if grouped {
                        self.expect_p(')')?;
                    }
                    classes.clear();
                    error_classes(&ty, &mut classes).map_err(|_| {
                        Error::syntax(offset as usize, "invalid rescue exception type")
                    })?;
                }
                if self.tokens[self.pos].line == line && self.token() == &Token::Op("=>") {
                    self.bump()?;
                    if self.tokens[self.pos].line != line {
                        return self.err("rescue binding must be an identifier");
                    }
                    let name = self.name()?;
                    if name.starts_with('@') {
                        return self.err("rescue binding must be an identifier");
                    }
                    binding = Some(name);
                }
                if self.token() == &Token::Op("->") {
                    return self.err("rescue binding must use =>");
                }
            }
            let existed = binding
                .as_ref()
                .is_some_and(|name| self.locals.contains(name));
            let declared_it = self.declared_it;
            if let Some(name) = &binding {
                self.locals.insert(name.clone());
                self.declared_it |= name == "it";
            }
            let rescue_body = self.block(&["rescue", "else", "ensure", "end"])?;
            if let Some(name) = &binding {
                if !existed {
                    self.locals.remove(name);
                }
            }
            if binding.as_deref() == Some("it") {
                self.declared_it = declared_it;
            }
            rescues.push(Rescue {
                classes,
                binding,
                body: rescue_body,
                offset,
            });
        }
        let alternate = if self.word("else") {
            if rescues.is_empty() {
                return self.err("else requires rescue");
            }
            self.block(&["ensure", "end"])?
        } else {
            Vec::new()
        };
        let ensure = if self.word("ensure") {
            self.block(&["end"])?
        } else {
            Vec::new()
        };
        if (function || !rescues.is_empty())
            && rescues.iter().all(|r| r.body.is_empty())
            && ensure.is_empty()
        {
            return self.err("begin requires rescue and/or ensure");
        }
        self.expect_word("end")?;
        Ok(Try {
            modifier: false,
            body,
            rescues,
            alternate,
            ensure,
        })
    }

    pub(super) fn rescue_modifier(&mut self, body: Expr) -> Result<Expr> {
        self.work.charge(1)?;
        let offset = body.offset;
        let rescue_offset = self.tokens[self.pos - 1].offset as u32;
        let fallback = self.line_expr(0)?;
        let attempt = Try {
            modifier: true,
            body: vec![Statement::Expr(body).at(offset)],
            rescues: vec![Rescue {
                classes: vec![crate::ErrorClass::Standard],
                binding: None,
                body: vec![Statement::Expr(fallback).at(rescue_offset)],
                offset: rescue_offset,
            }],
            alternate: Vec::new(),
            ensure: Vec::new(),
        };
        let depth = attempt.depth();
        self.make_at(Node::Try(Box::new(attempt)), depth, offset)
    }
}

fn error_classes(
    ty: &crate::types::Type,
    out: &mut Vec<crate::ErrorClass>,
) -> std::result::Result<(), ()> {
    match &ty.kind {
        crate::types::TypeKind::Named => {
            out.push(crate::ErrorClass::from_name(&ty.name).ok_or(())?)
        }
        crate::types::TypeKind::Union(options) => {
            for ty in options {
                error_classes(ty, out)?;
            }
        }
        _ => return Err(()),
    }
    Ok(())
}
