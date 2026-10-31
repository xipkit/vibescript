use super::*;

impl<M: super::recovery::Mode> Parsing<'_, M> {
    // A condition ends at `then`, which otherwise names a local like any
    // identifier, as in Go.
    async fn condition(&self) -> Result<Expr> {
        let previous = {
            let mut p = self.p();
            p.line_breaks()?;
            let groups = p.groups;
            p.then_stop.replace(groups)
        };
        let condition = self.line_expr(0).await;
        self.p().then_stop = previous;
        condition
    }

    pub(super) async fn while_stmt(&self, until: bool) -> Result<Statement> {
        let previous = {
            let mut p = self.p();
            p.work.charge(1)?;
            p.line_breaks()?;
            let groups = p.groups;
            p.loop_condition.replace(groups)
        };
        let cond = self.line_expr(0).await;
        self.p().loop_condition = previous;
        let mut cond = cond?;
        {
            let mut p = self.p();
            if until {
                cond = p.negate(cond)?;
            }
            p.word("do");
        }
        let body = self.block(&["end"]).await?;
        self.p().expect_word("end")?;
        Ok(Statement::While(cond, body, None))
    }

    pub(super) async fn for_stmt(&self) -> Result<Statement> {
        self.p().work.charge(1)?;
        let (target, _) = self.target(Place::For).await?;
        let previous = {
            let mut p = self.p();
            if !target.is_binding(p.work)? {
                let offset = target
                    .offset()
                    .map_or(p.position(p.pos), |offset| offset as usize);
                return Err(Error::syntax(p.work, offset, "invalid for loop target"));
            }
            p.line_breaks()?;
            p.expect_word("in")?;
            p.line_breaks()?;
            let groups = p.groups;
            p.loop_condition.replace(groups)
        };
        let iterable = self.line_expr(0).await;
        self.p().loop_condition = previous;
        let iterable = iterable?;
        {
            let mut p = self.p();
            p.word("do");
            p.declare_target(&target)?;
        }
        let body = self.block(&["end"]).await?;
        self.p().expect_word("end")?;
        Ok(Statement::For(target, iterable, body))
    }

    pub(super) async fn if_stmt(&self, unless: bool) -> Result<Statement> {
        let work = self.p().work;
        work.charge(1)?;
        let mut branches = Buffer::new();
        let alternate = loop {
            let mut cond = self.condition().await?;
            {
                let mut p = self.p();
                if unless {
                    cond = p.negate(cond)?;
                }
                p.word("then");
                p.lines()?;
            }
            let body = self.block(&["else", "elsif", "end"]).await?;
            branches.push(work, (cond, body))?;
            let alternate = {
                let mut p = self.p();
                if p.word("elsif") {
                    if unless {
                        return p.err("unless does not support elsif");
                    }
                    continue;
                }
                let alternate = p.word("else");
                if alternate {
                    p.lines()?;
                } else {
                    p.expect_word("end")?;
                }
                alternate
            };
            if !alternate {
                break Buffer::new();
            }
            let alternate = self.block(&["end"]).await?;
            self.p().expect_word("end")?;
            break alternate;
        };
        Ok(Statement::If(branches, alternate, None))
    }

    pub(super) async fn if_expr(&self, unless: bool) -> Result<Expr> {
        let work = self.p().work;
        work.charge(1)?;
        let mut branches = Buffer::new();
        let alternate = loop {
            let mut cond = self.condition().await?;
            {
                let mut p = self.p();
                if unless {
                    cond = p.negate(cond)?;
                }
                p.word("then");
                p.lines()?;
            }
            let yes = self.expr(0).await?;
            let yes = self.trailing_block(yes).await?;
            branches.push(work, (cond, yes))?;
            let alternate = {
                let mut p = self.p();
                p.lines()?;
                if p.word("elsif") {
                    if unless {
                        return p.err("unless does not support elsif");
                    }
                    continue;
                }
                let alternate = p.word("else");
                if alternate {
                    p.lines()?;
                } else {
                    p.expect_word("end")?;
                }
                alternate
            };
            if !alternate {
                break self.p().make(Node::Literal(Value::nil()), 1)?;
            }
            let no = self.expr(0).await?;
            let no = self.trailing_block(no).await?;
            let mut p = self.p();
            p.lines()?;
            p.expect_word("end")?;
            break no;
        };
        let depth = 1 + branches
            .iter()
            .map(|(cond, yes)| cond.depth.max(yes.depth))
            .max()
            .unwrap_or(0)
            .max(alternate.depth);
        self.p().make(
            Node::Conditional(branches, Boxed::new(work, alternate)?),
            depth,
        )
    }

    pub(super) async fn case_expr(&self) -> Result<Expr> {
        let work = self.p().work;
        work.charge(1)?;
        self.p().lines()?;
        let target = if matches!(self.p().token(), Token::Word(w) if w=="when") {
            None
        } else {
            Some(Boxed::new(work, self.line_expr(0).await?)?)
        };
        self.p().lines()?;
        let mut clauses = Buffer::new();
        while self.p().word("when") {
            let mut values = Buffer::new();
            loop {
                let splat = {
                    let mut p = self.p();
                    let splat = p.token() == &Token::Op("*");
                    if splat {
                        p.bump()?;
                    }
                    splat
                };
                values.push(work, (self.condition().await?, splat))?;
                // Like Go, a comma on the next line continues the values.
                let mut p = self.p();
                let comma = p.significant(p.pos);
                if p.tokens[comma].token != Token::P(',') {
                    break;
                }
                p.pos = comma + 1;
                p.lines()?;
            }
            {
                let mut p = self.p();
                p.word("then");
                p.lines()?;
            }
            let result = self.expr(0).await?;
            let result = self.trailing_block(result).await?;
            clauses.push(work, When { values, result })?;
            self.p().lines()?;
        }
        if clauses.is_empty() {
            return self.p().expected(Label::Text("when"));
        }
        let alternate = if self.p().word("else") {
            self.p().lines()?;
            let alternate = self.expr(0).await?;
            Some(Boxed::new(work, self.trailing_block(alternate).await?)?)
        } else {
            None
        };
        let mut p = self.p();
        p.lines()?;
        p.expect_word("end")?;
        let depth = 1 + target
            .as_ref()
            .map_or(0, |e| e.depth)
            .max(alternate.as_ref().map_or(0, |e| e.depth))
            .max(
                clauses
                    .iter()
                    .map(|c| {
                        c.result
                            .depth
                            .max(c.values.iter().map(|(v, _)| v.depth).max().unwrap_or(0))
                    })
                    .max()
                    .unwrap_or(0),
            );
        p.make(Node::Case(target, clauses, alternate), depth)
    }

    pub(super) async fn loop_expression(&self, word: &str, offset: u32) -> Result<Expr> {
        let stmt = if word == "for" {
            self.for_stmt().await?
        } else {
            self.while_stmt(word == "until").await?
        };
        let stmt = stmt.at(self.work, offset)?;
        let depth = stmt.depth;
        let p = self.p();
        p.make(Node::Compound(Boxed::new(p.work, stmt)?), depth)
    }
}
