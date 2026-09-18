use super::*;
use crate::{Error, Value, iteration::Progress};

mod literal;
mod matching;
mod substitution;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(in crate::checking::flow) enum TextMethod {
    Char,
    Byte,
    Codepoint,
    Line,
    Chars,
    Bytes,
    Codepoints,
    Lines,
    Match,
    Scan,
    Sub,
    SubBang,
    Gsub,
    GsubBang,
}

impl TextMethod {
    pub fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "each_char" => Self::Char,
            "each_byte" => Self::Byte,
            "each_codepoint" => Self::Codepoint,
            "each_line" => Self::Line,
            "chars" => Self::Chars,
            "bytes" => Self::Bytes,
            "codepoints" => Self::Codepoints,
            "lines" => Self::Lines,
            "match" => Self::Match,
            "scan" => Self::Scan,
            "sub" => Self::Sub,
            "sub!" => Self::SubBang,
            "gsub" => Self::Gsub,
            "gsub!" => Self::GsubBang,
            _ => return None,
        })
    }

    fn substitutes(self) -> bool {
        matches!(
            self,
            Self::Sub | Self::SubBang | Self::Gsub | Self::GsubBang
        )
    }

    pub(in crate::checking::flow) fn materializes(self) -> bool {
        matches!(
            self,
            Self::Chars | Self::Bytes | Self::Codepoints | Self::Lines
        )
    }

    fn iterator(self) -> &'static str {
        match self {
            Self::Char | Self::Chars => "each_char",
            Self::Byte | Self::Bytes => "each_byte",
            Self::Codepoint | Self::Codepoints => "each_codepoint",
            Self::Line | Self::Lines => "each_line",
            Self::Match => "match",
            Self::Scan => "scan",
            Self::Sub => "sub",
            Self::SubBang => "sub!",
            Self::Gsub => "gsub",
            Self::GsubBang => "gsub!",
        }
    }
}

enum Schedule<'a> {
    Text(&'a mut crate::text::iteration::Driver),
    Regex(&'a mut crate::regex::operations::Driver),
}

impl Schedule<'_> {
    fn advance(&mut self, ctx: &mut CallContext) -> Result<Progress> {
        match self {
            Self::Text(driver) => driver.advance(ctx, Some(Value::nil())),
            Self::Regex(driver) => driver.advance(ctx, Some(Value::nil())),
        }
    }
}

impl Walker<'_> {
    pub(in crate::checking::flow) fn text_block(
        &mut self,
        state: &State,
        pc: usize,
        receiver: Fact,
        site: MemberSite,
        args: Arguments,
        method: TextMethod,
    ) -> Result<()> {
        for i in 0..self.facts.arm_count(receiver) {
            self.ctx.charge(1)?;
            let arm = self.facts.arm(receiver, i);
            if arm == Atom::Never.fact() {
                continue;
            }
            if method.materializes() || self.facts.atom(arm) != Some(Atom::String) {
                self.text_fallback(state, pc, arm, site, &args, method)?;
                continue;
            }
            if matches!(method, TextMethod::Match | TextMethod::Scan) {
                self.text_matching(state, pc, arm, site, &args, method)?;
                continue;
            }
            if method.substitutes() {
                self.text_substitution(state, pc, arm, site, &args, method)?;
                continue;
            }
            if site.scope
                || !args.positional.data.is_empty()
                || !args.keywords.data.is_empty()
                || args.block.is_none()
            {
                self.collection_error(state, pc, arm, site, &args, ErrorClass::Runtime)?;
                continue;
            }
            let driver = Driver {
                method: Method::Each,
                mutation: None,
                callback: Callback::Block(args.block.as_ref().unwrap()),
                pattern: None,
                count_overflow: false,
                exact: matches!(self.facts.node(arm), Node::String(_)),
                site: Some(site),
            };
            if let Node::String(value) = self.facts.node(arm) {
                let value = value.clone();
                let mut schedule = crate::text::iteration::Driver::new(
                    self.ctx,
                    method.iterator(),
                    &value,
                    &[],
                    false,
                    true,
                )?
                .unwrap();
                self.text_schedule(state, pc, arm, driver, Schedule::Text(&mut schedule))?;
            } else {
                let element = if matches!(method, TextMethod::Byte | TextMethod::Codepoint) {
                    Atom::Int.fact()
                } else {
                    Atom::String.fact()
                };
                self.text_repeat(state, pc, arm, driver, element, false)?;
            }
        }
        Ok(())
    }

    fn text_fallback(
        &mut self,
        state: &State,
        pc: usize,
        receiver: Fact,
        site: MemberSite,
        args: &Arguments,
        method: TextMethod,
    ) -> Result<()> {
        let regex = method == TextMethod::Match && self.facts.atom(receiver) == Some(Atom::Regex);
        let materializer = method.materializes() && self.facts.atom(receiver) == Some(Atom::String);
        let fields = matches!(
            self.facts.node(receiver),
            Node::Shape(..) | Node::Hash(..) | Node::Protected(..)
        );
        if matches!(
            self.facts.node(receiver),
            Node::Named(_)
                | Node::Nominal { .. }
                | Node::Instance { .. }
                | Node::Atom(Atom::Unknown | Atom::Any)
        ) {
            self.incomplete(pc)?;
            return Ok(());
        }
        if !regex && !fields && !materializer {
            return self.collection_error(state, pc, receiver, site, args, ErrorClass::Runtime);
        }
        let selected = site.text(self.program, self.facts);
        let name = selected.as_str();
        if args.block.is_some() && builtins::namespace_call(self.ctx, self.facts, receiver, name)? {
            return self.collection_error(state, pc, receiver, site, args, ErrorClass::Runtime);
        }
        let mut args = args.snapshot(self.ctx)?;
        if regex || materializer {
            // Regex value match and text materializers leave attached blocks unused.
            args.block = None;
        }
        let mut next = state.snapshot(self.ctx)?;
        if let Some(edges) = self.member_without_collection(&mut next, pc, receiver, site, &args)? {
            for edge in edges.into_iter().flatten() {
                self.extra.push(self.ctx, edge)?;
            }
        } else {
            self.native_continue(pc, next)?;
        }
        Ok(())
    }

    fn text_initial(&mut self, state: &State) -> Result<IterationState> {
        Ok(IterationState {
            state: state.snapshot(self.ctx)?,
            output: Atom::Nil.fact(),
            auxiliary: Atom::Never.fact(),
            previous: Atom::Never.fact(),
        })
    }

    fn text_item(element: Fact) -> Item {
        Item {
            arguments: [element, Atom::Nil.fact()],
            count: 1,
            element,
            index: Atom::Int.fact(),
            pair: None,
        }
    }

    fn text_schedule(
        &mut self,
        state: &State,
        pc: usize,
        receiver: Fact,
        driver: Driver<'_>,
        mut schedule: Schedule<'_>,
    ) -> Result<()> {
        let mut current = self.text_initial(state)?;
        let depth = self.collection_depth(&current, driver, receiver)?;
        loop {
            self.ctx.charge(1)?;
            let progress = match schedule.advance(self.ctx) {
                Ok(progress) => progress,
                Err(error) => return self.text_native_error(&current.state, pc, error),
            };
            let Progress::Yield([value, _, _], 1) = progress else {
                assert!(matches!(progress, Progress::Done(_)));
                return self.collection_done(current, pc, driver.method, receiver);
            };
            let element = self.text_yield_fact(&value)?;
            drop(value);
            let Some(next) =
                self.collection_step(current, pc, driver, Self::text_item(element), depth)?
            else {
                return Ok(());
            };
            current = next;
        }
    }

    fn text_repeat(
        &mut self,
        state: &State,
        pc: usize,
        receiver: Fact,
        driver: Driver<'_>,
        element: Fact,
        single: bool,
    ) -> Result<()> {
        let initial = self.text_initial(state)?;
        let empty = initial.snapshot(self.ctx)?;
        self.collection_done(empty, pc, driver.method, receiver)?;
        let depth = self.collection_depth(&initial, driver, receiver)?;
        let item = Self::text_item(element);
        let Some(mut current) = self.collection_step(initial, pc, driver, item, depth)? else {
            return Ok(());
        };
        let depth = self.collection_depth(&current, driver, receiver)?;
        current.state.widening.get_or_insert(depth);
        loop {
            self.ctx.charge(1)?;
            let done = current.snapshot(self.ctx)?;
            self.collection_done(done, pc, driver.method, receiver)?;
            if single {
                break;
            }
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

    fn text_native_error(&mut self, state: &State, pc: usize, error: Error) -> Result<()> {
        if self.ctx.exhausted() {
            return Err(error);
        }
        let Some(class) = error.class() else {
            return Err(error);
        };
        self.emit_error(state, pc, handlers::bit(class))
    }
}
