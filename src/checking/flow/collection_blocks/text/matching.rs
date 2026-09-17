use super::*;
use crate::regex::{MAX_PATTERN, MAX_TEXT, operations, value::Regex};

impl Walker<'_> {
    pub(super) fn text_matching(
        &mut self,
        state: &State,
        pc: usize,
        receiver: Fact,
        site: CallSite,
        args: &Arguments,
        method: TextMethod,
    ) -> Result<()> {
        let single = method == TextMethod::Match;
        let count = args.positional.data.len();
        if site.scope || !args.keywords.data.is_empty() || !(count == 1 || (single && count == 2)) {
            return self.collection_error(state, pc, receiver, site, args, ErrorClass::Runtime);
        }
        let expected = self
            .facts
            .union(self.ctx, &[Atom::String.fact(), Atom::Regex.fact()])?;
        let pattern = args.positional.data[0];
        for i in 0..self.facts.arm_count(pattern) {
            self.ctx.charge(1)?;
            let pattern = self.facts.arm(pattern, i);
            if pattern == Atom::Never.fact() {
                continue;
            }
            if !self.collection_parameter(state, pc, receiver, site, args, (pattern, expected))? {
                continue;
            }
            if let Node::String(text) = self.facts.node(receiver) {
                if text.as_bytes().unwrap().len() > MAX_TEXT {
                    self.emit_error(state, pc, handlers::bit(ErrorClass::Limit))?;
                    continue;
                }
            } else {
                self.emit_error(state, pc, handlers::bit(ErrorClass::Limit))?;
            }
            if let Node::String(text) = self.facts.node(pattern) {
                if text.as_bytes().unwrap().len() > MAX_PATTERN {
                    self.emit_error(state, pc, handlers::bit(ErrorClass::Limit))?;
                    continue;
                }
            } else if self.facts.atom(pattern) != Some(Atom::Regex) {
                self.emit_error(state, pc, handlers::bit(ErrorClass::Limit))?;
            }
            let offsets = self.text_offsets(state, pc, receiver, site, args)?;
            if offsets.data.is_empty() {
                continue;
            }
            let regex = match self.facts.node(pattern) {
                Node::Regex(_) => pattern,
                Node::String(value) => {
                    let value = value.clone();
                    match Regex::compile(self.ctx, value, 0) {
                        Ok(regex) => self.facts.regex(self.ctx, regex)?,
                        Err(error) => {
                            self.text_native_error(state, pc, error)?;
                            continue;
                        }
                    }
                }
                _ => {
                    if self.facts.atom(pattern) != Some(Atom::Regex) {
                        self.emit_error(
                            state,
                            pc,
                            handlers::bit(ErrorClass::Runtime) | handlers::bit(ErrorClass::Limit),
                        )?;
                    }
                    Atom::Regex.fact()
                }
            };
            let driver = Driver {
                method: if single {
                    Method::YieldSelf
                } else {
                    Method::Each
                },
                mutation: None,
                callback: args
                    .block
                    .as_ref()
                    .map_or(Callback::Identity, Callback::Block),
                pattern: None,
                count_overflow: false,
                exact: true,
                site: Some(site),
            };
            for &offset in &offsets.data {
                self.ctx.charge(1)?;
                if let (Node::String(text), Node::Regex(regex), Node::Integer(offset)) = (
                    self.facts.node(receiver),
                    self.facts.node(regex),
                    self.facts.node(offset),
                ) {
                    let text = text.clone();
                    let input = [regex.clone(), Value::int(*offset)];
                    let input = &input[..if single { 2 } else { 1 }];
                    if args.block.is_none() {
                        match operations::member(self.ctx, method.iterator(), &text, input, false) {
                            Ok(Some(value)) => {
                                let fact = if single {
                                    self.text_yield_fact(&value)?
                                } else {
                                    self.text_scan_fact(&value)?
                                };
                                let state = state.snapshot(self.ctx)?;
                                self.collection_terminal(state, pc, fact)?;
                            }
                            Err(error) => self.text_native_error(state, pc, error)?,
                            Ok(None) => unreachable!(),
                        }
                    } else {
                        match operations::Driver::new(
                            self.ctx,
                            method.iterator(),
                            &text,
                            input,
                            false,
                            true,
                        ) {
                            Ok(Some(mut schedule)) => self.text_schedule(
                                state,
                                pc,
                                receiver,
                                driver,
                                Schedule::Regex(&mut schedule),
                            )?,
                            Err(error) => self.text_native_error(state, pc, error)?,
                            Ok(None) => unreachable!(),
                        }
                    }
                } else {
                    let element = if single {
                        builtins::protected::match_data(self.ctx, self.facts, regex)?
                    } else {
                        self.text_scan_element(regex)?
                    };
                    if args.block.is_none() {
                        let value = if single {
                            self.facts.nullable(self.ctx, element)?
                        } else {
                            self.emit_error(state, pc, handlers::bit(ErrorClass::Limit))?;
                            self.facts.array(self.ctx, element)?
                        };
                        let state = state.snapshot(self.ctx)?;
                        self.collection_terminal(state, pc, value)?;
                    } else {
                        self.text_repeat(
                            state,
                            pc,
                            receiver,
                            Driver {
                                exact: false,
                                ..driver
                            },
                            element,
                            single,
                        )?;
                    }
                }
            }
        }
        Ok(())
    }

    fn text_offsets(
        &mut self,
        state: &State,
        pc: usize,
        receiver: Fact,
        site: CallSite,
        args: &Arguments,
    ) -> Result<Buffer<Fact>> {
        let mut offsets = Buffer::empty();
        let Some(&offset) = args.positional.data.get(1) else {
            let zero = self.facts.integer(self.ctx, 0)?;
            offsets.push(self.ctx, zero)?;
            return Ok(offsets);
        };
        let number = self
            .facts
            .union(self.ctx, &[Atom::Int.fact(), Atom::Float.fact()])?;
        for i in 0..self.facts.arm_count(offset) {
            self.ctx.charge(1)?;
            let arm = self.facts.arm(offset, i);
            if arm == Atom::Never.fact()
                || !self.collection_parameter(state, pc, receiver, site, args, (arm, number))?
            {
                continue;
            }
            let value = match self.facts.node(arm) {
                Node::Integer(_) => arm,
                Node::Float(bits) => {
                    let n = f64::from_bits(*bits);
                    if !n.is_finite()
                        || !(-9_223_372_036_854_775_808.0..9_223_372_036_854_775_808.0).contains(&n)
                    {
                        self.collection_error(
                            state,
                            pc,
                            receiver,
                            site,
                            args,
                            ErrorClass::Runtime,
                        )?;
                        continue;
                    }
                    self.facts.integer(self.ctx, n as i64)?
                }
                _ => {
                    self.emit_error(state, pc, handlers::bit(ErrorClass::Runtime))?;
                    Atom::Int.fact()
                }
            };
            offsets.push(self.ctx, value)?;
        }
        Ok(offsets)
    }

    fn text_scan_element(&mut self, regex: Fact) -> Result<Fact> {
        let capture = self.facts.nullable(self.ctx, Atom::String.fact())?;
        if let Node::Regex(value) = self.facts.node(regex) {
            let Kind::Regex(regex) = &value.0 else {
                unreachable!()
            };
            let count = regex.capture_names().1.len() - 1;
            if count == 0 {
                return Ok(Atom::String.fact());
            }
            let mut captures = Buffer::with_capacity(self.ctx, count)?;
            for _ in 0..count {
                captures.push(self.ctx, capture)?;
            }
            self.facts.tuple(self.ctx, &captures.data)
        } else {
            let captures = self.facts.array(self.ctx, capture)?;
            self.facts.union(self.ctx, &[Atom::String.fact(), captures])
        }
    }
}
