use super::*;
use crate::checking::{
    calls::{Require, require_failure},
    facts::Node,
};
use crate::{Error, ErrorKind, Value};

impl Walker<'_> {
    fn require_error(&mut self, state: &State, pc: usize, error: Error) -> Result<()> {
        let outcome = require_failure(self.ctx, self.facts, error)?;
        self.call_effects(
            state,
            pc,
            Target::Builtin(crate::builtin::Builtin::Require),
            &outcome,
        )
    }

    pub(super) fn require_file(&mut self, state: &State, pc: usize, args: Arguments) -> Result<()> {
        self.ctx.checkpoint()?;
        if self.ctx.strict_effects && !self.ctx.options.allow_require {
            return self.require_error(
                state,
                pc,
                Error::new(
                    ErrorKind::Runtime,
                    "strict effects: require is disabled without CallOptions.allow_require",
                ),
            );
        }
        if args.positional.data.len() != 1 {
            return self.require_error(
                state,
                pc,
                Error::new(
                    ErrorKind::Argument,
                    "require expects a single module name argument",
                ),
            );
        }
        if args.block.is_some() {
            return self.require_error(
                state,
                pc,
                Error::new(ErrorKind::Argument, "require does not accept blocks"),
            );
        }
        let mut aliases = Buffer::empty();
        aliases.push(self.ctx, Alias::Absent)?;
        for keyword in &args.keywords.data {
            self.ctx.charge(1)?;
            let key = match self.facts.node(keyword.name) {
                Node::String(name) | Node::Symbol(name) => name.as_bytes().unwrap(),
                _ => {
                    self.incomplete(pc)?;
                    return Ok(());
                }
            };
            if key != b"as" {
                return self.require_error(
                    state,
                    pc,
                    Error::new(ErrorKind::Argument, "require: unknown keyword argument"),
                );
            }
            aliases.data.clear();
            for index in 0..self.facts.arm_count(keyword.value) {
                self.ctx.charge(1)?;
                let arm = self.facts.arm(keyword.value, index);
                let value = match self.facts.node(arm) {
                    Node::String(name) | Node::Symbol(name) => name.clone(),
                    Node::Atom(Atom::Unknown | Atom::Any | Atom::String | Atom::Symbol) => {
                        if !aliases
                            .data
                            .iter()
                            .any(|alias| matches!(alias, Alias::Unknown))
                        {
                            aliases.push(self.ctx, Alias::Unknown)?;
                        }
                        continue;
                    }
                    _ => {
                        self.require_error(
                            state,
                            pc,
                            Error::new(
                                ErrorKind::Argument,
                                "require: alias must be a string or symbol",
                            ),
                        )?;
                        continue;
                    }
                };
                match alias(self.ctx, &value)? {
                    Some(name) => aliases.push(self.ctx, Alias::Named(name))?,
                    None => self.require_error(
                        state,
                        pc,
                        Error::new(ErrorKind::Argument, "require: invalid alias"),
                    )?,
                }
            }
        }
        let input = args.positional.data[0];
        let mut unknown = false;
        for alias in aliases.data {
            for index in 0..self.facts.arm_count(input) {
                self.ctx.charge(1)?;
                let arm = self.facts.arm(input, index);
                let name = match self.facts.node(arm) {
                    Node::String(name) | Node::Symbol(name) => name.clone(),
                    Node::Atom(Atom::Unknown | Atom::Any | Atom::String | Atom::Symbol) => {
                        if !unknown {
                            unknown = true;
                            self.require_unknown(state, pc)?;
                        }
                        continue;
                    }
                    _ => {
                        self.require_error(
                            state,
                            pc,
                            Error::new(
                                ErrorKind::Argument,
                                "require expects a string or symbol module name",
                            ),
                        )?;
                        continue;
                    }
                };
                let next = state.snapshot(self.ctx)?;
                self.require_named(next, pc, name, &alias)?;
            }
        }
        Ok(())
    }

    /// Requires a file whose name is known only at runtime.
    ///
    /// Any file may fail, or run code and publish exports that this analysis cannot list,
    /// so the call behaves like an unknown call that also marks those publications.
    fn require_unknown(&mut self, state: &State, pc: usize) -> Result<()> {
        let mut next = state.snapshot(self.ctx)?;
        self.unknown_call_effects(&mut next, pc)?;
        self.emit_error(&next, pc, u8::MAX)?;
        self.publish_unknown(&mut next, pc)?;
        next.stack
            .push(self.ctx, Operand::new(Atom::Unknown.fact()))?;
        self.native_continue(pc, next)
    }

    fn exports_slot(&mut self, state: &State) -> Result<Option<usize>> {
        let receiving = state
            .global_layout
            .source(self.ctx, state.source_slots.receiving)?;
        Ok(receiving
            .exports
            .filter(|&slot| slot < state.global_count)
            .map(|slot| state.global_base + slot))
    }

    /// Reports whether a `require` may have published names that the analysis cannot list,
    /// so that a name missing from every known scope may still resolve at runtime.
    pub(super) fn unknown_exports(&mut self, state: &State) -> Result<bool> {
        let Some(slot) = self.exports_slot(state)? else {
            return Ok(false);
        };
        let value = state.locals.get(self.ctx, slot)?.value;
        Ok(self.facts.filter(self.ctx, value, Test::Truth, true)? != Atom::Never.fact())
    }

    fn publish_unknown(&mut self, state: &mut State, pc: usize) -> Result<()> {
        if let Some(slot) = self.exports_slot(state)? {
            let published = self.facts.boolean(self.ctx, true)?;
            self.store(state, pc, slot, Operand::new(published))?;
        }
        Ok(())
    }

    fn require_named(
        &mut self,
        mut next: State,
        pc: usize,
        name: Value,
        alias: &Alias,
    ) -> Result<()> {
        let mut bindings = Buffer::empty();
        if let Alias::Named(value) = alias {
            let alias_name = std::str::from_utf8(value.as_bytes().unwrap()).unwrap();
            if let Some(slot) = self.root_index(&next, alias_name)? {
                let slot = next.global_base + slot;
                let Some(alternatives) = self.import_root_branches(&mut next, pc, slot)? else {
                    return Ok(());
                };
                for alternative in alternatives.data {
                    self.require_named(alternative, pc, name.clone(), alias)?;
                }
            }
            if !self.require_bindings(&next, pc, alias_name, &mut bindings)? {
                return Ok(());
            }
        }
        let request = Require {
            name,
            alias: match alias {
                Alias::Named(value) => Some(value.clone()),
                Alias::Absent | Alias::Unknown => None,
            },
            bindings,
        };
        let current_error = next.current_error(self.ctx, self.current_error)?;
        let globals = next.global_call(self.ctx)?;
        let result = self
            .calls
            .require(self.ctx, self.facts, &request, current_error, &globals)?;
        self.call_effects(
            &next,
            pc,
            Target::Builtin(crate::builtin::Builtin::Require),
            &result,
        )?;
        if result.incomplete {
            self.incomplete(pc)?;
        }
        // An alias known only at runtime can be invalid or already bound, which fails before
        // the file runs. Otherwise it publishes the exports under a name the analysis cannot
        // list, and the file's own exports publish as usual.
        let unknown = matches!(alias, Alias::Unknown) && !result.exits.data.is_empty();
        if unknown {
            self.emit_error(&next, pc, handlers::bit(ErrorClass::Runtime))?;
        }
        if !result.exits.data.is_empty() && self.global_exits(&mut next, pc, result.exits)? {
            if unknown {
                self.publish_unknown(&mut next, pc)?;
            }
            self.native_continue(pc, next)?;
        }
        Ok(())
    }

    fn require_bindings(
        &mut self,
        state: &State,
        pc: usize,
        name: &str,
        bindings: &mut Buffer<Fact>,
    ) -> Result<bool> {
        for (slot, local) in self.function.local_names.iter().enumerate() {
            self.ctx.work_bytes(local.len().max(name.len()))?;
            if local == name {
                let binding = state.locals.get(self.ctx, slot)?;
                if binding.value != Atom::Never.fact() {
                    if binding.missing {
                        self.incomplete(pc)?;
                        return Ok(false);
                    }
                    bindings.push(self.ctx, binding.value)?;
                }
            }
        }
        if let Some((_, binding)) = self.file_binding(state, name)? {
            if binding.missing {
                self.incomplete(pc)?;
                return Ok(false);
            }
            bindings.push(self.ctx, binding.value)?;
        }
        if let Some(field) = self.namespace_constant(state, name, false)? {
            if field.missing || field.incomplete {
                self.incomplete(pc)?;
                return Ok(false);
            }
            bindings.push(self.ctx, field.value)?;
        }
        Ok(true)
    }
}

fn alias(ctx: &mut CallContext, value: &Value) -> Result<Option<Value>> {
    let bytes = value.as_bytes().unwrap();
    ctx.work_bytes(bytes.len())?;
    let Ok(name) = std::str::from_utf8(bytes) else {
        return Ok(None);
    };
    let name = name.trim();
    let mut chars = name.chars();
    if !chars
        .next()
        .is_some_and(|c| c == '_' || crate::syntax::unicode::letter(c))
        || !chars.all(|c| {
            matches!(c, '_' | '?' | '!')
                || crate::syntax::unicode::letter(c)
                || crate::syntax::unicode::digit(c)
        })
        || crate::syntax::keyword(name)
    {
        return Ok(None);
    }
    ctx.bytes(name.as_bytes()).map(Some)
}

/// The `as:` alias of a `require` call.
enum Alias {
    Absent,
    Named(Value),
    /// A string or symbol known only at runtime.
    Unknown,
}
