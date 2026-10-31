use super::*;

/// Where a destructuring target list appears.
#[derive(Clone, Copy, PartialEq)]
pub(super) enum Place {
    Statement,
    For,
    /// A parenthesized or bracketed group, and whether its names take types.
    Group(bool),
}

/// The prefix needed by `source_text`, including enough bytes to finish a
/// UTF-8 character at its truncation boundary.
struct TargetText<'w> {
    work: &'w dyn Work,
    bytes: [u8; 68],
    len: usize,
    clipped: bool,
}

impl TargetText<'_> {
    fn text(&self) -> &str {
        let bytes = &self.bytes[..self.len];
        match std::str::from_utf8(bytes) {
            Ok(text) => text,
            Err(error) => std::str::from_utf8(&bytes[..error.valid_up_to()]).unwrap(),
        }
    }
}

impl crate::shapes::TypeWriter for TargetText<'_> {
    fn node(&mut self) -> Result<()> {
        self.work.checkpoint()?;
        self.work.charge(1)?;
        if self.len == self.bytes.len() {
            self.clipped = true;
            return Err(Error::new(crate::ErrorKind::Syntax, ""));
        }
        Ok(())
    }

    fn write(&mut self, bytes: &[u8]) -> Result<()> {
        self.node()?;
        let count = bytes.len().min(self.bytes.len() - self.len);
        self.work.bytes(count)?;
        self.bytes[self.len..self.len + count].copy_from_slice(&bytes[..count]);
        self.len += count;
        if count < bytes.len() {
            self.clipped = true;
            return Err(Error::new(crate::ErrorKind::Syntax, ""));
        }
        Ok(())
    }
}

/// Writes only the diagnostic prefix of a destructuring target.
fn target_text(target: &Target, out: &mut TargetText<'_>) -> Result<()> {
    use crate::shapes::TypeWriter;
    out.node()?;
    match target {
        Target::Value(Expr {
            node: Node::Var(name),
            ..
        }) => out.write(name.as_bytes())?,
        Target::Tuple(parts) => {
            out.write(b"(")?;
            for (index, (part, rest)) in parts.iter().enumerate() {
                out.node()?;
                if index > 0 {
                    out.write(b", ")?;
                }
                if *rest {
                    out.write(b"*")?;
                }
                if let Some(part) = part {
                    target_text(part, out)?;
                }
            }
            out.write(b")")?;
        }
        Target::Typed(target, ty) => {
            target_text(target, out)?;
            out.write(b": ")?;
            crate::shapes::format(ty, out)?;
        }
        Target::Value(_) => (),
    }
    Ok(())
}

fn target_excerpt<'w>(target: &Target, work: &'w dyn Work) -> Result<TargetText<'w>> {
    let mut text = TargetText {
        work,
        bytes: [0; 68],
        len: 0,
        clipped: false,
    };
    let result = target_text(target, &mut text);
    if !text.clipped {
        result?;
    }
    Ok(text)
}

impl<M: super::recovery::Mode> Parsing<'_, M> {
    /// Parses a destructuring target list, as Go's `parseDestructureTargetList`
    /// does, and reports whether commas at its own level made it a tuple.
    pub(super) async fn target(&self, place: Place) -> Result<(Target, bool)> {
        let work = self.p().work;
        work.charge(1)?;
        let typed = place == Place::Group(true);
        let mut parts = Buffer::new();
        let mut tuple = false;
        let mut has_rest = false;
        loop {
            let (rest, anonymous, group, star) = {
                let mut p = self.p();
                let star = p.tokens[p.pos].offset as u32;
                let rest = p.token() == &Token::Op("*");
                let mut anonymous = false;
                if rest {
                    let next = p.significant(p.pos + 1);
                    anonymous = match &p.tokens[next].token {
                        Token::P(',' | ')' | ']') => true,
                        Token::Op(op) => assignment(op),
                        Token::Word(w) => place == Place::For && *w == "in",
                        _ => false,
                    };
                    p.bump()?;
                    if !anonymous {
                        p.line_breaks()?;
                    }
                    tuple = true;
                }
                // Go reads a statement's first target as an expression, so it
                // cannot open a group.
                let grouped = place != Place::Statement || !parts.is_empty() || rest;
                let group = match p.token() {
                    Token::P('(') if grouped && !anonymous => Some(')'),
                    Token::P('[') if grouped && !anonymous => Some(']'),
                    _ => None,
                };
                (rest, anonymous, group, star)
            };
            let (mut value, offset) = if anonymous {
                (None, star)
            } else if let Some(close) = group {
                // Go counts each nested destructuring group against the syntax limit.
                let open = {
                    let mut p = self.p();
                    p.enter()?;
                    let open = p.tokens[p.pos].offset as u32;
                    p.bump()?;
                    let next = p.significant(p.pos);
                    if p.tokens[next].token == Token::P(close) {
                        p.pos = next;
                        return p.expected(Label::Text("destructuring assignment target"));
                    }
                    p.line_breaks()?;
                    open
                };
                let (inner, tuple) = self.nested_target(typed).await?;
                let mut p = self.p();
                p.line_breaks()?;
                p.expect_p(close)?;
                p.depth -= 1;
                // Like Go, every group destructures one level; commas inside
                // it list that level's parts.
                let inner = if tuple {
                    inner
                } else {
                    Target::Tuple(Buffer::from_array(work, [(Some(inner), false)])?)
                };
                (Some(inner), open)
            } else {
                let outer = {
                    let mut p = self.p();
                    let start = p.pos;
                    p.target_start.replace(start)
                };
                let expression = self.line_expr(0).await;
                let mut p = self.p();
                p.target_start = outer;
                let expression = expression?;
                // Go checks a statement's lone target only once an operator follows.
                let lone = place == Place::Statement && parts.is_empty() && !rest;
                let listed = p.tokens[p.significant(p.pos)].token == Token::P(',');
                if !lone || listed {
                    p.assignment_member(&expression)?;
                    if let Some(offset) = expression.safe_navigation() {
                        return Err(Error::syntax(
                            p.work,
                            offset,
                            "safe navigation cannot be used as an assignment target",
                        ));
                    }
                    if !expression.assignable() {
                        return Err(Error::syntax(
                            p.work,
                            expression.offset as usize,
                            "invalid destructuring assignment target",
                        ));
                    }
                }
                let offset = expression.offset;
                (Some(Target::Value(expression)), offset)
            };
            let mut p = self.p();
            let colon = p.significant(p.pos);
            if typed && value.is_some() && p.tokens[colon].token == Token::P(':') {
                p.pos = colon + 1;
                p.line_breaks()?;
                let start = p.tokens[p.pos].offset;
                let ty = p.type_expr(1, false)?;
                if rest && !ty.captures(false) {
                    let excerpt = target_excerpt(value.as_ref().unwrap(), work)?;
                    let text = excerpt.text();
                    return Err(Error::syntax(
                        work,
                        start,
                        format_args!(
                            "rest destructuring target {} captures an array; annotate it as array<...> or any",
                            source_text(if text.is_empty() { "*" } else { text })
                        ),
                    ));
                }
                value = Some(Target::Typed(Boxed::new(work, value.take().unwrap())?, ty));
            }
            if rest {
                if has_rest {
                    return Err(Error::syntax(
                        work,
                        offset as usize,
                        "duplicate rest assignment target",
                    ));
                }
                has_rest = true;
            }
            parts.push(work, (value, rest))?;
            let comma = p.significant(p.pos);
            if p.tokens[comma].token != Token::P(',') {
                break;
            }
            p.pos = comma + 1;
            tuple = true;
            p.line_breaks()?;
        }
        let target = if tuple {
            Target::Tuple(parts)
        } else {
            parts.pop().unwrap().0.unwrap()
        };
        let p = self.p();
        let offset = target.offset().unwrap_or(p.tokens[p.pos].offset as u32);
        p.check_depth(target.depth(p.work)?, offset)?;
        Ok((target, tuple))
    }
}

impl Parser<'_> {
    pub(super) fn declare_target(&mut self, target: &Target) -> Result<()> {
        self.work.charge(1)?;
        let mut names = Buffer::new();
        target.parts(self.work, |part, _| {
            if let Target::Value(Expr {
                node: Node::Var(name),
                offset,
                ..
            }) = part
            {
                self.binding_name(name, *offset as usize)?;
                names.push(self.work, (name.clone(), *offset))?;
            }
            Ok(true)
        })?;
        for (name, offset) in names {
            self.work.charge(1)?;
            self.declared_it |= name == "it";
            let id = self.local_id(&name, offset as usize)?;
            self.locals.insert(self.work, name, id)?;
        }
        Ok(())
    }
    /// Declares the names a block parameter binds, as
    /// [`Self::declare_target`] does, and fails with V0209 at one that it
    /// or an `earlier` parameter of the same list binds already. A block's
    /// parameters may shadow the enclosing locals, so only a name the
    /// locals already have is looked for among them.
    pub(super) fn declare_block_parameter(
        &mut self,
        target: &Target,
        earlier: &[Target],
    ) -> Result<()> {
        self.work.charge(1)?;
        let mut names = Buffer::new();
        target.parts(self.work, |part, _| {
            if let Target::Value(Expr {
                node: Node::Var(name),
                offset,
                ..
            }) = part
            {
                self.binding_name(name, *offset as usize)?;
                names.push(self.work, (name.clone(), *offset))?;
            }
            Ok(true)
        })?;
        for (index, (name, offset)) in names.iter().enumerate() {
            self.work.charge(1)?;
            self.declared_it |= name == "it";
            let id = self.local_id(name, *offset as usize)?;
            if self.locals.insert(self.work, name.clone(), id)?.is_none() {
                continue;
            }
            let mut repeated = false;
            for (other, _) in &names[..index] {
                if same_binding(self.work, other, name)? {
                    repeated = true;
                    break;
                }
            }
            for other in earlier {
                if repeated {
                    break;
                }
                repeated = binds(other, name, self.work)?;
            }
            if repeated {
                return Err(duplicate_parameter(
                    self.work,
                    name,
                    *offset as usize,
                    name.len(),
                ));
            }
        }
        Ok(())
    }
}

/// The error for a parameter named like an earlier one of its list: a
/// syntax error with V0209 at the name, which starts at `at` and is `width`
/// bytes long.
pub(super) fn duplicate_parameter(work: &dyn Work, name: &str, at: usize, width: usize) -> Error {
    let error = Error::syntax(
        work,
        at,
        format_args!("duplicate parameter {}", source_text(name)),
    );
    if error.kind != crate::ErrorKind::Syntax {
        return error;
    }
    let build = || {
        let mut error = error;
        let (message, mut held) =
            crate::compilation::formatted(work, format_args!("{}", error.message))?;
        // The diagnostic's box and Arc<Extra>: its slice pointer, enum
        // discriminant, and two reference counts.
        crate::budget::Charge::merge(
            &mut held,
            work.reserve(size_of::<crate::diagnostic::Diagnostic>() + 5 * size_of::<usize>())?,
        );
        error.retain(work, held)?;
        let diagnostic = crate::diagnostic::Diagnostic::error(
            crate::diagnostic::Code::DUPLICATE_NAME,
            crate::diagnostic::Span::new(at, at + width),
            message,
        );
        Ok::<_, Error>(error.with_diagnostic(diagnostic))
    };
    build().unwrap_or_else(|error| error)
}

/// Whether a block parameter binds `name`, charging a step for each part of
/// it looked at.
fn binds(target: &Target, name: &str, work: &dyn Work) -> Result<bool> {
    let mut found = false;
    target.parts(work, |part, _| {
        if let Target::Value(Expr {
            node: Node::Var(bound),
            ..
        }) = part
        {
            found = same_binding(work, bound, name)?;
        }
        Ok(!found)
    })?;
    Ok(found)
}

fn same_binding(work: &dyn Work, left: &str, right: &str) -> Result<bool> {
    work.checkpoint()?;
    work.charge(1)?;
    if left.len() != right.len() {
        return Ok(false);
    }
    for (left, right) in left
        .as_bytes()
        .chunks(4096)
        .zip(right.as_bytes().chunks(4096))
    {
        work.checkpoint()?;
        work.bytes(left.len())?;
        if left != right {
            return Ok(false);
        }
    }
    Ok(true)
}

#[cfg(test)]
mod walk_tests {
    use super::{Statement, Target};
    use crate::{CallContext, CallOptions, compilation::Meter};
    use std::cell::RefCell;

    #[test]
    fn duplicate_parameter_keeps_a_budget_refusal_unchanged() {
        let mut context = CallContext::new(CallOptions::default());
        context.options.limits.memory_bytes = Some(0);
        let error = super::duplicate_parameter(&Meter(RefCell::new(&mut context)), "x", 0, 1);
        assert_eq!(error.kind, crate::ErrorKind::Memory);
        assert!(error.diagnostics().is_empty());
    }

    #[test]
    fn duplicate_parameter_owns_and_releases_its_diagnostic_storage() {
        let mut context = CallContext::new(CallOptions::default());
        let error = super::duplicate_parameter(&Meter(RefCell::new(&mut context)), "x", 0, 1);
        let diagnostic = &error.diagnostics()[0];
        assert_eq!(diagnostic.message, "duplicate parameter x");
        assert!(
            context.stats().retained_memory_bytes
                >= 2 * error.message.len() + size_of::<crate::diagnostic::Diagnostic>()
        );
        drop(error);
        assert_eq!(context.stats().retained_memory_bytes, 0);
    }

    #[test]
    fn a_rest_target_excerpt_never_builds_its_full_spelling() {
        let source = format!("{} = 1", "x".repeat(1 << 20));
        let parsed = super::parse(&source, &()).unwrap();
        let Statement::Assign(target, _, _) = &parsed.functions[0].body[0].node else {
            panic!("assignment")
        };
        let mut context = CallContext::new(CallOptions::default());
        context.options.limits.steps = Some(8);
        let work = Meter(RefCell::new(&mut context));
        let text = super::target_excerpt(target, &work).unwrap();
        assert_eq!(text.text().len(), 68);
        assert_eq!(work.0.borrow().stats().peak_memory_bytes, 0);
    }

    #[test]
    fn a_target_walk_stops_before_visiting_a_refused_part() {
        let parsed = super::parse("a, b = [1, 2]", &()).unwrap();
        let Statement::Assign(target, _, _) = &parsed.functions[0].body[0].node else {
            panic!("assignment")
        };
        let mut context = CallContext::new(CallOptions::default());
        context.options.limits.steps = Some(0);
        let mut visited = 0;
        let result = target.parts(&Meter(RefCell::new(&mut context)), |_, _| {
            visited += 1;
            Ok(true)
        });
        assert_eq!(result.unwrap_err().kind, crate::ErrorKind::Steps);
        assert_eq!(visited, 0);
    }

    #[test]
    fn a_walk_over_a_deep_target_reserves_its_levels() {
        // A target of pairs nested as deep as the parser allows, whose
        // walk keeps a level for each.
        let depth = 1_000;
        let mut target = format!("a{}", depth - 1);
        for i in (0..depth - 1).rev() {
            target = format!("a{i}, ({target})");
        }
        let source = format!("def f(x: any)\n  {target} = x\nend\n");
        let parsed = super::parse(&source, &()).unwrap();
        let target = parsed
            .functions
            .iter()
            .flat_map(|function| function.body.iter())
            .find_map(|stmt| match &stmt.node {
                Statement::Assign(target, _, _) => Some(target),
                _ => None,
            })
            .unwrap();
        let mut context = CallContext::new(CallOptions::default());
        let found = super::binds(target, "absent", &Meter(RefCell::new(&mut context))).unwrap();
        assert!(!found);
        let level = std::mem::size_of::<(std::slice::Iter<'static, (Option<Target>, bool)>, u32)>();
        let peak = context.stats().peak_memory_bytes;
        assert!(
            peak >= (depth - 1) * level,
            "{peak} bytes reserved for {depth} levels of {level}"
        );
    }
}
