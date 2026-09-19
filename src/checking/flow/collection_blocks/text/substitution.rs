use super::*;
use crate::regex::{
    MAX_PATTERN, MAX_TEXT,
    substitute::{self, CallbackPattern},
};

mod arguments;
mod render;
use render::Rendered;

#[derive(Clone, Copy)]
struct Pattern {
    value: Fact,
    regex: bool,
}

impl TextMethod {
    fn replaces_all(self) -> bool {
        matches!(self, Self::Gsub | Self::GsubBang)
    }

    fn bang(self) -> bool {
        matches!(self, Self::SubBang | Self::GsubBang)
    }
}

impl Walker<'_> {
    pub(super) fn text_substitution(
        &mut self,
        state: &State,
        pc: usize,
        receiver: Fact,
        site: MemberSite,
        args: &Arguments,
        method: TextMethod,
    ) -> Result<()> {
        let Some((patterns, replacement)) =
            self.substitution_arguments(state, pc, receiver, site, args)?
        else {
            return Ok(());
        };
        for pattern in patterns.data {
            for i in 0..self.facts.arm_count(replacement) {
                self.ctx.charge(1)?;
                let replacement = self.facts.arm(replacement, i);
                if replacement == Atom::Never.fact() {
                    continue;
                }
                if !self.substitution_limits(
                    state,
                    pc,
                    receiver,
                    pattern,
                    args.block.is_none().then_some(replacement),
                )? {
                    continue;
                }
                let literal = match self.facts.node(pattern.value) {
                    Node::String(value) | Node::Regex(value) => Some(value.clone()),
                    _ => None,
                };
                if args.block.is_none() {
                    if let (Node::String(text), Some(pattern_value), Node::String(replacement)) = (
                        self.facts.node(receiver),
                        &literal,
                        self.facts.node(replacement),
                    ) {
                        let text = text.clone();
                        let input = [pattern_value.clone(), replacement.clone()];
                        let mut keywords = Buffer::empty();
                        if !args.keywords.data.is_empty() {
                            let key = self.ctx.bytes(b"regex")?;
                            keywords.push(self.ctx, (key, Value::boolean(pattern.regex)))?;
                        }
                        match substitute::member(
                            self.ctx,
                            method.iterator(),
                            &text,
                            &input,
                            &keywords.data,
                        ) {
                            Ok(Some(value)) => {
                                let value = self.text_yield_fact(&value)?;
                                let state = state.snapshot(self.ctx)?;
                                self.collection_terminal(state, pc, value)?;
                            }
                            Err(error) => self.text_native_error(state, pc, error)?,
                            Ok(None) => unreachable!(),
                        }
                        continue;
                    }
                }
                let mut matcher = if let Some(literal) = literal {
                    match CallbackPattern::new(self.ctx, literal, pattern.regex) {
                        Ok(matcher) => Some(matcher),
                        Err(error) => {
                            self.text_native_error(state, pc, error)?;
                            continue;
                        }
                    }
                } else {
                    if pattern.regex && self.facts.atom(pattern.value) != Some(Atom::Regex) {
                        self.emit_error(
                            state,
                            pc,
                            handlers::bit(ErrorClass::Runtime) | handlers::bit(ErrorClass::Limit),
                        )?;
                    }
                    None
                };
                if let Some(block) = args.block.as_ref() {
                    let driver = Driver {
                        method: Method::Substitute,
                        mutation: None,
                        callback: Callback::Block(block),
                        pattern: None,
                        count_overflow: false,
                        exact: matcher.is_some()
                            && matches!(self.facts.node(receiver), Node::String(_)),
                        site: Some(site),
                    };
                    if let (Node::String(text), Some(matcher)) =
                        (self.facts.node(receiver), matcher.as_mut())
                    {
                        let text = text.clone();
                        self.substitution_schedule(
                            state,
                            pc,
                            driver,
                            (method, pattern, receiver),
                            &text,
                            matcher,
                        )?;
                    } else {
                        self.substitution_repeat(state, pc, driver, receiver, method, pattern)?;
                    }
                } else {
                    let matched = if let (Node::String(text), Some(matcher)) =
                        (self.facts.node(receiver), matcher.as_mut())
                    {
                        let text = text.clone();
                        match matcher.next(self.ctx, text.as_bytes().unwrap()) {
                            Ok(next) => Some(next.is_some()),
                            Err(error) => {
                                self.text_native_error(state, pc, error)?;
                                continue;
                            }
                        }
                    } else if !pattern.regex
                        && matches!(self.facts.node(pattern.value), Node::String(text) if text.as_bytes().unwrap().is_empty())
                    {
                        Some(true)
                    } else {
                        None
                    };
                    let value = if matched == Some(false) {
                        if method.bang() {
                            Atom::Nil.fact()
                        } else {
                            receiver
                        }
                    } else {
                        self.emit_error(
                            state,
                            pc,
                            handlers::bit(ErrorClass::Limit)
                                | if pattern.regex {
                                    handlers::bit(ErrorClass::Runtime)
                                } else {
                                    0
                                },
                        )?;
                        if method.bang() && matched.is_none() {
                            self.facts.nullable(self.ctx, Atom::String.fact())?
                        } else {
                            Atom::String.fact()
                        }
                    };
                    let state = state.snapshot(self.ctx)?;
                    self.collection_terminal(state, pc, value)?;
                }
            }
        }
        Ok(())
    }

    fn substitution_initial(&mut self, state: &State) -> Result<IterationState> {
        let empty = self.facts.string(self.ctx, b"")?;
        Ok(IterationState {
            state: state.snapshot(self.ctx)?,
            output: empty,
            auxiliary: empty,
            previous: Atom::Never.fact(),
        })
    }

    fn substitution_value(&mut self, state: &State, pc: usize, value: Rendered) -> Result<Fact> {
        if value.limited {
            self.emit_error(state, pc, handlers::bit(ErrorClass::Limit))?;
        }
        Ok(value.value)
    }

    fn substitution_append(
        &mut self,
        state: &State,
        pc: usize,
        output: Fact,
        piece: Rendered,
    ) -> Result<Fact> {
        let output = self.substitution_string(output)?;
        let output = self.substitution_concat(output, piece)?;
        self.substitution_value(state, pc, output)
    }

    pub(in crate::checking::flow::collection_blocks) fn substitution_result(
        &mut self,
        mut current: IterationState,
        pc: usize,
        value: Fact,
    ) -> Result<Option<IterationState>> {
        let mut rendered = self.substitution_render(value)?;
        let value = self.substitution_value(&current.state, pc, rendered)?;
        if value == Atom::Never.fact() {
            return Ok(None);
        }
        rendered.limited = false;
        let prefix = self.substitution_string(current.auxiliary)?;
        current.output = self.substitution_append(&current.state, pc, current.output, prefix)?;
        if current.output == Atom::Never.fact() {
            return Ok(None);
        }
        current.output = self.substitution_append(&current.state, pc, current.output, rendered)?;
        if current.output == Atom::Never.fact() {
            return Ok(None);
        }
        current.auxiliary = self.facts.string(self.ctx, b"")?;
        Ok(Some(current))
    }

    fn substitution_schedule(
        &mut self,
        state: &State,
        pc: usize,
        driver: Driver<'_>,
        call: (TextMethod, Pattern, Fact),
        text: &Value,
        matcher: &mut CallbackPattern,
    ) -> Result<()> {
        let (method, pattern, receiver) = call;
        let initial = self.substitution_initial(state)?;
        let depth = self.collection_depth(&initial, driver, receiver)?;
        let mut current = initial.alternatives(self.ctx)?;
        let mut appended = 0;
        let mut matched = false;
        loop {
            self.ctx.charge(1)?;
            if current.data.is_empty() {
                return Ok(());
            }
            let location = match matcher.next(self.ctx, text.as_bytes().unwrap()) {
                Ok(location) => location,
                Err(error) => return self.text_alternative_error(&current.data, pc, error),
            };
            let Some([start, end]) = location else { break };
            matched = true;
            let prefix = self.substitution_bytes(&text.as_bytes().unwrap()[appended..start])?;
            let mut ready = Buffer::empty();
            for mut current in current.data {
                if method.replaces_all() || !pattern.regex {
                    current.output =
                        self.substitution_append(&current.state, pc, current.output, prefix)?;
                    if current.output == Atom::Never.fact() {
                        continue;
                    }
                } else {
                    current.auxiliary = prefix.value;
                }
                ready.push(self.ctx, current)?;
            }
            let element = if !pattern.regex {
                pattern.value
            } else if start == 0 && end == text.as_bytes().unwrap().len() {
                receiver
            } else {
                self.facts
                    .string(self.ctx, &text.as_bytes().unwrap()[start..end])?
            };
            current = self.iteration_next(ready, pc, driver, Self::text_item(element), depth)?;
            if current.data.is_empty() {
                return Ok(());
            }
            appended = end;
            if !method.replaces_all() {
                break;
            }
        }
        let suffix = self.substitution_bytes(&text.as_bytes().unwrap()[appended..])?;
        for current in current.data {
            let output = self.substitution_append(&current.state, pc, current.output, suffix)?;
            if output != Atom::Never.fact() {
                self.collection_terminal(
                    current.state,
                    pc,
                    if !matched && method.bang() {
                        Atom::Nil.fact()
                    } else {
                        output
                    },
                )?;
            }
        }
        Ok(())
    }

    fn substitution_repeat(
        &mut self,
        state: &State,
        pc: usize,
        driver: Driver<'_>,
        receiver: Fact,
        method: TextMethod,
        pattern: Pattern,
    ) -> Result<()> {
        let empty_pattern = !pattern.regex
            && matches!(self.facts.node(pattern.value), Node::String(text) if text.as_bytes().unwrap().is_empty());
        let mut initial = self.substitution_initial(state)?;
        if !empty_pattern {
            let suffix = self.substitution_string(receiver)?;
            let output = self.substitution_append(state, pc, initial.output, suffix)?;
            if output != Atom::Never.fact() {
                let state = state.snapshot(self.ctx)?;
                self.collection_terminal(
                    state,
                    pc,
                    if method.bang() {
                        Atom::Nil.fact()
                    } else {
                        receiver
                    },
                )?;
            }
            if !matches!(self.facts.node(receiver), Node::String(_)) {
                self.emit_error(state, pc, handlers::bit(ErrorClass::Limit))?;
            }
        }
        let prefix = if empty_pattern {
            self.facts.string(self.ctx, b"")?
        } else {
            Atom::String.fact()
        };
        if !method.replaces_all() && pattern.regex {
            initial.auxiliary = prefix;
        } else {
            if !pattern.regex && !empty_pattern {
                self.emit_error(state, pc, handlers::bit(ErrorClass::Limit))?;
            }
            initial.output = prefix;
        }
        let element = if !pattern.regex {
            pattern.value
        } else {
            Atom::String.fact()
        };
        let item = Self::text_item(element);
        let depth = self.collection_depth(&initial, driver, receiver)?;
        let first = self.collection_step(initial, pc, driver, item, depth)?;
        self.iteration_loop(
            first,
            driver,
            receiver,
            |walker, mut current, depth| {
                let prefix = walker.substitution_string(Atom::String.fact())?;
                current.output =
                    walker.substitution_append(&current.state, pc, current.output, prefix)?;
                walker.collection_step(current, pc, driver, item, depth)
            },
            |walker, current| {
                walker.emit_error(&current.state, pc, handlers::bit(ErrorClass::Limit))?;
                walker.collection_terminal(current.state, pc, Atom::String.fact())?;
                Ok(method.replaces_all())
            },
        )?;
        Ok(())
    }
}
