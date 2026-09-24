use super::*;
use crate::{
    Value,
    checking::facts::{Computation, Node},
    syntax::ParamKind,
};

struct Converted {
    state: State,
    value: Fact,
}

impl Walker<'_> {
    pub(super) fn render_call(
        &mut self,
        state: &mut State,
        pc: usize,
        builtin: crate::builtin::Builtin,
        mut args: Arguments,
    ) -> Result<Option<Edges>> {
        use crate::{builtin::Builtin, output::Kind as Output};
        let target = Target::Builtin(builtin);
        let mut admission = crate::checking::calls::Outcome::empty();
        let admitted = args.admit(self.ctx, self.facts, &mut admission.failures)?;
        self.call_effects(state, pc, target, &admission)?;
        if !admitted {
            return Ok(Some([None, None]));
        }
        let output = match builtin {
            Builtin::Output(kind) => Some(kind),
            Builtin::Format(_) => None,
            _ => unreachable!(),
        };
        let failure = if !args.keywords.data.is_empty() {
            Some(Failure::BuiltinKeywords)
        } else if args.block.is_some() {
            Some(Failure::BuiltinBlock)
        } else if output.is_none() && args.positional.data.is_empty() {
            Some(Failure::BuiltinArity)
        } else {
            None
        };
        if let Some(failure) = failure {
            self.issue(pc, IssueKind::Call { target, failure })?;
            self.emit_error(state, pc, handlers::bit(ErrorClass::Runtime))?;
            return Ok(Some([None, None]));
        }
        if output.is_none() {
            let pattern = args.positional.data[0];
            let relation = self
                .facts
                .relation(self.ctx, pattern, Atom::String.fact())?;
            if relation != Relation::Accepted {
                self.emit_error(state, pc, handlers::bit(ErrorClass::Runtime))?;
            }
            if relation == Relation::Rejected {
                self.issue(
                    pc,
                    IssueKind::Call {
                        target,
                        failure: Failure::BuiltinDomain(pattern),
                    },
                )?;
                return Ok(Some([None, None]));
            }
        } else if let Some(kind) = output {
            // Missing writers fail before conversion; configured writers may fail
            // after any operand. Analysis never invokes either writer.
            let writer = self.calls.writer(self.ctx, kind)?;
            if writer != Some(true) {
                self.emit_error(state, pc, handlers::bit(ErrorClass::Runtime))?;
            }
            if writer == Some(false) {
                return Ok(Some([None, None]));
            }
        }
        struct Pending {
            state: State,
            values: Buffer<Fact>,
        }
        let count = args.positional.data.len();
        let mut pending = Buffer::empty();
        let initial = state.snapshot(self.ctx)?;
        pending.push(
            self.ctx,
            Pending {
                state: initial,
                values: args.positional,
            },
        )?;
        for index in usize::from(output.is_none())..count {
            let mut following = Buffer::empty();
            for entry in pending.data {
                self.ctx.charge(1)?;
                let original = entry.values.data[index];
                let conversions = if output == Some(Output::Inspect) {
                    let mut conversions = Buffer::empty();
                    let state = entry.state.snapshot(self.ctx)?;
                    conversions.push(
                        self.ctx,
                        Converted {
                            state,
                            value: original,
                        },
                    )?;
                    conversions
                } else {
                    self.string_conversion(&entry.state, pc, original)?
                };
                // The last conversion takes the operands; only alternatives before it copy them.
                let mut operands = Some(entry.values);
                let mut conversions = conversions.data.into_iter().peekable();
                while let Some(Converted { state, value }) = conversions.next() {
                    let mut values = if conversions.peek().is_some() {
                        let mut values = Buffer::empty();
                        values.extend(self.ctx, &operands.as_ref().unwrap().data)?;
                        values
                    } else {
                        operands.take().unwrap()
                    };
                    values.data[index] = value;
                    if output.is_some() {
                        self.emit_error(&state, pc, u8::MAX)?;
                    }
                    following.push(self.ctx, Pending { state, values })?;
                }
            }
            pending = following;
        }
        for Pending { mut state, values } in pending.data {
            self.ctx.charge(1)?;
            let value = if let Some(kind) = output {
                if count == 0 && kind == Output::Puts {
                    self.emit_error(&state, pc, u8::MAX)?;
                }
                if kind == Output::Inspect {
                    match values.data.as_slice() {
                        [] => Atom::Nil.fact(),
                        [value] => *value,
                        values => self.facts.tuple(self.ctx, values)?,
                    }
                } else {
                    Atom::Nil.fact()
                }
            } else {
                self.emit_error(
                    &state,
                    pc,
                    handlers::bit(ErrorClass::Runtime) | handlers::bit(ErrorClass::Limit),
                )?;
                Atom::String.fact()
            };
            state.stack.push(self.ctx, Operand::new(value))?;
            self.native_continue(pc, state)?;
        }
        Ok(Some([None, None]))
    }

    fn string_conversion(
        &mut self,
        state: &State,
        pc: usize,
        value: Fact,
    ) -> Result<Buffer<Converted>> {
        let mut converted = Buffer::empty();
        for i in 0..self.facts.arm_count(value) {
            self.ctx.charge(1)?;
            let original = self.facts.arm(value, i);
            if original == Atom::Never.fact() {
                continue;
            }
            if matches!(
                self.facts.node(original),
                Node::Named(_) | Node::Nominal { symbols: None, .. }
            ) {
                self.incomplete(pc)?;
                continue;
            }
            let mut next = state.snapshot(self.ctx)?;
            if matches!(
                self.facts.node(original),
                Node::Atom(Atom::Unknown | Atom::Any)
            ) {
                // A gradual value may be a source instance whose eligible `to_s`
                // runs with the effects and errors of an unknown call. Every other
                // value renders natively, so the rendered text is still a string.
                self.unknown_call_effects(&mut next, pc)?;
                self.emit_error(&next, pc, u8::MAX)?;
                converted.push(
                    self.ctx,
                    Converted {
                        state: next,
                        value: original,
                    },
                )?;
                continue;
            }
            let mut value = original;
            if matches!(self.facts.node(original), Node::Instance { .. }) {
                let Some(module) = self.namespace(state, original)? else {
                    self.incomplete(pc)?;
                    continue;
                };
                let mut function = None;
                for method in &module.program().namespaces[module.index].instance_methods {
                    self.ctx.work_bytes(method.name.len())?;
                    if method.name == "to_s" {
                        function = Some(method.function);
                        break;
                    }
                }
                if let Some(candidate) = function {
                    for parameter in &module.program().functions[candidate].params {
                        self.ctx.charge(1)?;
                        if matches!(parameter.kind, ParamKind::Positional | ParamKind::Keyword)
                            && !parameter.default
                        {
                            function = None;
                            break;
                        }
                    }
                }
                if let Some(function) = function {
                    let target = Target::Method {
                        function: module.source.callable(function),
                        receiver: original,
                        constructor: false,
                    };
                    if let Some(edges) = self.invoke(&mut next, pc, target, Arguments::new())? {
                        for edge in edges.into_iter().flatten() {
                            self.extra.push(self.ctx, edge)?;
                        }
                        continue;
                    }
                    let returned = next.stack.data.pop().unwrap().value;
                    let mut values = Buffer::empty();
                    for j in 0..self.facts.arm_count(returned) {
                        self.ctx.charge(1)?;
                        let arm = self.facts.arm(returned, j);
                        if arm == Atom::Never.fact() {
                            continue;
                        }
                        let value = match self.facts.node(arm) {
                            Node::String(_) | Node::Atom(Atom::String) => arm,
                            Node::Named(_) | Node::Atom(Atom::Unknown | Atom::Any) => self
                                .facts
                                .union(self.ctx, &[original, Atom::String.fact()])?,
                            _ => original,
                        };
                        values.push(self.ctx, value)?;
                    }
                    value = self.facts.union(self.ctx, &values.data)?;
                }
            }
            converted.push(self.ctx, Converted { state: next, value })?;
        }
        Ok(converted)
    }

    fn rendered_text(&mut self, state: &State, pc: usize, value: Fact) -> Result<Fact> {
        let mut values = Buffer::empty();
        for i in 0..self.facts.arm_count(value) {
            self.ctx.charge(1)?;
            let arm = self.facts.arm(value, i);
            let scalar = match *self.facts.node(arm) {
                Node::Boolean(value) => Some(Value::boolean(value)),
                Node::Integer(value) => Some(Value::int(value)),
                Node::Float(bits) => Some(Value::float(f64::from_bits(bits))),
                Node::Atom(Atom::Nil) => Some(Value::nil()),
                _ => None,
            };
            let value = if let Some(value) = scalar {
                let text = crate::text::display(self.ctx, &value)?;
                self.facts.string(self.ctx, text.as_bytes().unwrap())?
            } else {
                match self.facts.node(arm) {
                    Node::Atom(Atom::Never) => continue,
                    Node::String(_) => arm,
                    Node::Symbol(value) => {
                        let value = value.clone();
                        self.facts.string(self.ctx, value.as_bytes().unwrap())?
                    }
                    Node::Instance { class, .. } | Node::TypeValue(class) => {
                        if let Node::Nominal {
                            name,
                            symbols: None,
                            ..
                        } = self.facts.node(*class)
                        {
                            let instance = matches!(self.facts.node(arm), Node::Instance { .. });
                            let name = name.clone();
                            let mut text = Buffer::empty();
                            text.extend(self.ctx, if instance { b"<" } else { b"<Class " })?;
                            text.extend(self.ctx, name.as_bytes().unwrap())?;
                            text.extend(self.ctx, if instance { b" instance>" } else { b">" })?;
                            self.facts.string(self.ctx, &text.data)?
                        } else {
                            Atom::String.fact()
                        }
                    }
                    Node::Tuple(_) | Node::Array(_) | Node::Shape(..) | Node::Hash(..) => {
                        self.emit_error(state, pc, handlers::bit(ErrorClass::Limit))?;
                        Atom::String.fact()
                    }
                    _ => Atom::String.fact(),
                }
            };
            values.push(self.ctx, value)?;
        }
        self.facts.union(self.ctx, &values.data)
    }

    fn append_text(&mut self, before: Fact, value: Fact) -> Result<Fact> {
        // Loops interpolate the same text alternatives on every walk.
        let key = Computation::Append(before, value);
        if let Some((text, _)) = self.facts.remembered(self.ctx, key)? {
            return Ok(text);
        }
        let mut joined = Buffer::empty();
        for i in 0..self.facts.arm_count(before) {
            for j in 0..self.facts.arm_count(value) {
                self.ctx.charge(1)?;
                let a = self.facts.arm(before, i);
                let b = self.facts.arm(value, j);
                if a == Atom::Never.fact() || b == Atom::Never.fact() {
                    continue;
                }
                let value = match (self.facts.node(a), self.facts.node(b)) {
                    (Node::String(a), Node::String(b)) => {
                        let (a, b) = (a.clone(), b.clone());
                        let mut text = Buffer::empty();
                        text.extend(self.ctx, a.as_bytes().unwrap())?;
                        text.extend(self.ctx, b.as_bytes().unwrap())?;
                        self.facts.string(self.ctx, &text.data)?
                    }
                    _ => Atom::String.fact(),
                };
                joined.push(self.ctx, value)?;
            }
        }
        let text = self.facts.union(self.ctx, &joined.data)?;
        self.facts.remember(self.ctx, key, (text, 0))?;
        Ok(text)
    }

    pub(super) fn text_part(&mut self, mut state: State, pc: usize) -> Result<Edges> {
        let original = state.stack.data.pop().unwrap().value;
        for Converted { mut state, value } in self.string_conversion(&state, pc, original)?.data {
            let text = self.rendered_text(&state, pc, value)?;
            let before = *state.texts.data.last().unwrap();
            *state.texts.data.last_mut().unwrap() = self.append_text(before, text)?;
            self.extra.push(self.ctx, (pc + 1, state))?;
        }
        Ok([None, None])
    }

    pub(super) fn text_end(&mut self, state: &mut State, symbol: bool) -> Result<()> {
        let mut value = state.texts.data.pop().unwrap();
        if symbol {
            let mut symbols = Buffer::empty();
            for i in 0..self.facts.arm_count(value) {
                self.ctx.charge(1)?;
                let arm = self.facts.arm(value, i);
                let symbol = if let Node::String(value) = self.facts.node(arm) {
                    let value = value.clone();
                    self.facts.symbol(self.ctx, value.as_bytes().unwrap())?
                } else {
                    Atom::Symbol.fact()
                };
                symbols.push(self.ctx, symbol)?;
            }
            value = self.facts.union(self.ctx, &symbols.data)?;
        }
        state.stack.push(self.ctx, Operand::new(value))
    }
}
