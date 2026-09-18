use super::*;

impl Walker<'_> {
    pub(in crate::checking::flow) fn native_loop(
        &mut self,
        state: &State,
        pc: usize,
        args: Arguments,
    ) -> Result<()> {
        let target = Target::Builtin(crate::builtin::Builtin::Loop);
        let failure = if !args.positional.data.is_empty() {
            Some(IssueKind::Call {
                target,
                failure: Failure::BuiltinArity,
            })
        } else if !args.keywords.data.is_empty() {
            Some(IssueKind::Call {
                target,
                failure: Failure::BuiltinKeywords,
            })
        } else if args.block.is_none() {
            Some(IssueKind::MissingBlock)
        } else {
            None
        };
        if let Some(failure) = failure {
            self.emit_error(state, pc, handlers::bit(ErrorClass::Runtime))?;
            return self.issue(pc, failure);
        }
        let driver = Driver {
            mutation: None,
            method: Method::Loop,
            callback: Callback::Block(args.block.as_ref().unwrap()),
            pattern: None,
            count_overflow: false,
            exact: false,
            site: None,
        };
        let initial = IterationState {
            state: state.snapshot(self.ctx)?,
            output: Atom::Nil.fact(),
            auxiliary: Atom::Never.fact(),
            previous: Atom::Never.fact(),
        };
        let item = Item {
            arguments: [Atom::Nil.fact(); 2],
            count: 0,
            element: Atom::Nil.fact(),
            index: Atom::Int.fact(),
            pair: None,
        };
        let depth = self.collection_depth(&initial, driver, Atom::Nil.fact())?;
        let Some(mut current) = self.collection_step(initial, pc, driver, item, depth)? else {
            return Ok(());
        };
        let depth = self.collection_depth(&current, driver, Atom::Nil.fact())?;
        current.state.widening.get_or_insert(depth);
        loop {
            self.ctx.charge(1)?;
            let before = current.snapshot(self.ctx)?;
            let Some(next) = self.collection_step(before, pc, driver, item, depth)? else {
                break;
            };
            if !current.join(self.ctx, self.facts, &next, true, depth, self.program)? {
                break;
            }
        }
        Ok(())
    }
}
