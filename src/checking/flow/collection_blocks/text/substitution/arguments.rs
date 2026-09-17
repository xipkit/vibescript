use super::*;

impl Walker<'_> {
    pub(super) fn substitution_arguments(
        &mut self,
        state: &State,
        pc: usize,
        receiver: Fact,
        site: CallSite,
        args: &Arguments,
    ) -> Result<Option<(Buffer<Pattern>, Fact)>> {
        if site.scope
            || args.keywords.data.len() > 1
            || args.positional.data.len() != if args.block.is_some() { 1 } else { 2 }
        {
            self.collection_error(state, pc, receiver, site, args, ErrorClass::Runtime)?;
            return Ok(None);
        }
        let mut modes = Buffer::empty();
        if let Some(keyword) = args.keywords.data.first() {
            let name = self.facts.symbol(self.ctx, b"regex")?;
            if !self.collection_parameter(state, pc, receiver, site, args, (keyword.name, name))?
                || !self.collection_parameter(
                    state,
                    pc,
                    receiver,
                    site,
                    args,
                    (keyword.value, Atom::Bool.fact()),
                )?
            {
                return Ok(None);
            }
            for i in 0..self.facts.arm_count(keyword.value) {
                self.ctx.charge(1)?;
                let arm = self.facts.arm(keyword.value, i);
                if self.facts.overlaps(self.ctx, arm, Atom::Bool.fact())? {
                    if let Node::Boolean(value) = self.facts.node(arm) {
                        modes.push(self.ctx, *value)?;
                    } else {
                        modes.extend(self.ctx, &[false, true])?;
                    }
                }
            }
        } else {
            modes.push(self.ctx, false)?;
        }
        let pattern = args.positional.data[0];
        let expected = self
            .facts
            .union(self.ctx, &[Atom::String.fact(), Atom::Regex.fact()])?;
        let mut patterns = Buffer::empty();
        for i in 0..self.facts.arm_count(pattern) {
            self.ctx.charge(1)?;
            let arm = self.facts.arm(pattern, i);
            if arm == Atom::Never.fact()
                || !self.collection_parameter(state, pc, receiver, site, args, (arm, expected))?
            {
                continue;
            }
            if self.facts.overlaps(self.ctx, arm, Atom::Regex.fact())? {
                if !args.keywords.data.is_empty() {
                    self.collection_error(state, pc, receiver, site, args, ErrorClass::Runtime)?;
                } else {
                    let value = if self.facts.atom(arm) == Some(Atom::Regex) {
                        arm
                    } else {
                        Atom::Regex.fact()
                    };
                    patterns.push(self.ctx, Pattern { value, regex: true })?;
                }
            }
            if self.facts.overlaps(self.ctx, arm, Atom::String.fact())? {
                let value = if self.facts.atom(arm) == Some(Atom::String) {
                    arm
                } else {
                    Atom::String.fact()
                };
                for &regex in &modes.data {
                    patterns.push(self.ctx, Pattern { value, regex })?;
                }
            }
        }
        let mut replacement = Atom::Nil.fact();
        if args.block.is_none() {
            replacement = Atom::Never.fact();
            let input = args.positional.data[1];
            for i in 0..self.facts.arm_count(input) {
                self.ctx.charge(1)?;
                let arm = self.facts.arm(input, i);
                if arm == Atom::Never.fact()
                    || !self.collection_parameter(
                        state,
                        pc,
                        receiver,
                        site,
                        args,
                        (arm, Atom::String.fact()),
                    )?
                {
                    continue;
                }
                let arm = if self.facts.atom(arm) == Some(Atom::String) {
                    arm
                } else {
                    Atom::String.fact()
                };
                replacement = self.facts.union(self.ctx, &[replacement, arm])?;
            }
        }
        Ok(Some((patterns, replacement)))
    }

    pub(super) fn substitution_limits(
        &mut self,
        state: &State,
        pc: usize,
        receiver: Fact,
        pattern: Pattern,
        replacement: Option<Fact>,
    ) -> Result<bool> {
        if !pattern.regex {
            return Ok(true);
        }
        for (value, limit) in [(pattern.value, MAX_PATTERN), (receiver, MAX_TEXT)]
            .into_iter()
            .chain(replacement.map(|value| (value, MAX_TEXT)))
        {
            self.ctx.charge(1)?;
            if self.facts.atom(value) == Some(Atom::Regex) {
                continue;
            }
            if let Node::String(text) = self.facts.node(value) {
                if text.as_bytes().unwrap().len() > limit {
                    self.emit_error(state, pc, handlers::bit(ErrorClass::Limit))?;
                    return Ok(false);
                }
            } else {
                self.emit_error(state, pc, handlers::bit(ErrorClass::Limit))?;
            }
        }
        Ok(true)
    }
}
