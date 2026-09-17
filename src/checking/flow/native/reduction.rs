use super::*;
use crate::checking::facts::Node;

struct Request {
    state: State,
    pc: usize,
    site: MemberSite,
    values: [Fact; 3],
}

struct Child {
    values: [Fact; 3],
    output: Option<(State, Fact)>,
}

pub(in crate::checking::flow) struct NativeFrame {
    children: Buffer<Child>,
    cursor: usize,
    pending: Option<Request>,
}

impl NativeFrame {
    fn new() -> Self {
        Self {
            children: Buffer::empty(),
            cursor: 0,
            pending: None,
        }
    }
}

struct Task {
    request: Request,
    frame: NativeFrame,
}

impl Walker<'_> {
    pub(in crate::checking::flow) fn native_reduction(
        &mut self,
        state: &State,
        pc: usize,
        site: MemberSite,
        values: [Fact; 3],
    ) -> Result<Option<(State, Fact)>> {
        self.ctx.charge(1)?;
        if let Some(frame) = &mut self.native_frame {
            let index = frame.cursor;
            frame.cursor += 1;
            if let Some(child) = frame.children.data.get(index) {
                assert_eq!(child.values, values);
                return child
                    .output
                    .as_ref()
                    .map(|(state, value)| Ok((state.snapshot(self.ctx)?, *value)))
                    .transpose();
            }
            if frame.pending.is_none() {
                frame.pending = Some(Request {
                    state: state.snapshot(self.ctx)?,
                    pc,
                    site,
                    values,
                });
            }
            return Ok(None);
        }

        let mut tasks = Buffer::empty();
        let state = state.snapshot(self.ctx)?;
        tasks.push(
            self.ctx,
            Task {
                request: Request {
                    state,
                    pc,
                    site,
                    values,
                },
                frame: NativeFrame::new(),
            },
        )?;
        while let Some(mut task) = tasks.data.pop() {
            self.ctx.charge(1)?;
            task.frame.cursor = 0;
            self.native_frame = Some(task.frame);
            let request = task.request;
            let output = self.evaluate_native_reduction(
                &request.state,
                request.pc,
                request.site,
                request.values,
            );
            let mut frame = self.native_frame.take().unwrap();
            let output = output?;
            if let Some(pending) = frame.pending.take() {
                // Resume the same deterministic call after its next child completes.
                // Only completed children are replayed; nested calls never recurse here.
                tasks.push(self.ctx, Task { request, frame })?;
                tasks.push(
                    self.ctx,
                    Task {
                        request: pending,
                        frame: NativeFrame::new(),
                    },
                )?;
            } else if let Some(parent) = tasks.data.last_mut() {
                parent.frame.children.push(
                    self.ctx,
                    Child {
                        values: request.values,
                        output,
                    },
                )?;
            } else {
                return Ok(output);
            }
        }
        unreachable!()
    }

    fn evaluate_native_reduction(
        &mut self,
        state: &State,
        pc: usize,
        site: MemberSite,
        values: [Fact; 3],
    ) -> Result<Option<(State, Fact)>> {
        let [receiver, operation, argument] = values;
        let selected = match self.facts.node(operation) {
            Node::String(value) | Node::Symbol(value) => value.clone(),
            _ => {
                self.incomplete(pc)?;
                return Ok(None);
            }
        };
        let name =
            crate::members::introspection::method_name(self.ctx, selected.as_bytes().unwrap())?;
        let site = MemberSite {
            call: CallSite {
                method: name.and_then(crate::bytecode::Method::parse),
                auto: false,
                parenthesized: false,
                scope: false,
                ..site.call
            },
            selected: Some(operation),
        };
        let mut detached = state.snapshot(self.ctx)?;
        detached
            .addresses
            .push(self.ctx, Address::new(None, receiver))?;
        let mut args = Arguments::new();
        args.positional.push(self.ctx, argument)?;
        let outer = self.native_results.replace(Buffer::empty());
        let result = self.forwarded_member(&detached, pc, receiver, site, &args);
        let results = std::mem::replace(&mut self.native_results, outer).unwrap();
        result?;
        let mut joined: Option<State> = None;
        let mut value = Atom::Never.fact();
        for mut next in results.data {
            self.ctx.charge(1)?;
            let output = next.stack.data.pop().unwrap().value;
            assert_eq!(next.stack.data.len(), state.stack.data.len());
            assert_eq!(next.addresses.data.len(), state.addresses.data.len());
            value = self.facts.union(self.ctx, &[value, output])?;
            if let Some(joined) = &mut joined {
                joined.join(self.ctx, self.facts, &next, false)?;
            } else {
                joined = Some(next);
            }
        }
        Ok(joined.map(|state| (state, value)))
    }
}
