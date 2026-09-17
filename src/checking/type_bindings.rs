use super::facts::{Fact, Facts};
use crate::{CallContext, Result, Value, budget::Buffer, bytecode::Program, types, value::Kind};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Scope(usize);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Binding {
    Type { fact: Fact, enumeration: bool },
    Exports(Scope),
    Other,
    Unknown,
}

impl Binding {
    fn merge(self, other: Self) -> Self {
        if self == other { self } else { Self::Unknown }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Resolution {
    Known(Fact),
    Missing,
    Ambiguous,
    Dynamic,
}

impl Resolution {
    /// Returns a type fact only when lookup has one definite identity.
    pub fn fact(self) -> Option<Fact> {
        if let Self::Known(fact) = self {
            Some(fact)
        } else {
            None
        }
    }

    fn merge(self, other: Self) -> Self {
        match (self, other) {
            (Self::Ambiguous, _) | (_, Self::Ambiguous) => Self::Ambiguous,
            (Self::Dynamic, _) | (_, Self::Dynamic) => Self::Dynamic,
            (Self::Missing, result) | (result, Self::Missing) => result,
            (Self::Known(a), Self::Known(b)) if a == b => Self::Known(a),
            _ => Self::Ambiguous,
        }
    }
}

struct Entry {
    name: Value,
    value: Binding,
    optional: bool,
}

struct Entries {
    values: Buffer<Entry>,
    open: bool,
}

pub(super) struct Bindings {
    // Exported namespaces refer to scope IDs, so copying or dropping cyclic
    // alias graphs never recursively walks a host or script object graph.
    scopes: Buffer<Entries>,
}

impl Bindings {
    /// Creates an empty, accounted binding arena.
    pub fn new() -> Self {
        Self {
            scopes: Buffer::empty(),
        }
    }

    /// Adds a closed scope whose identity remains stable in snapshots.
    pub fn scope(&mut self, ctx: &mut CallContext) -> Result<Scope> {
        ctx.charge(1)?;
        let scope = Scope(self.scopes.data.len());
        self.scopes.push(
            ctx,
            Entries {
                values: Buffer::empty(),
                open: false,
            },
        )?;
        Ok(scope)
    }

    /// Marks a scope as possibly containing additional, unknown names.
    pub fn open(&mut self, ctx: &mut CallContext, scope: Scope) -> Result<()> {
        ctx.charge(1)?;
        ctx.checkpoint()?;
        self.scopes.data[scope.0].open = true;
        Ok(())
    }

    /// Inserts or replaces a definitely present binding by its exact bytes.
    pub fn insert(
        &mut self,
        ctx: &mut CallContext,
        scope: Scope,
        name: &[u8],
        value: Binding,
    ) -> Result<()> {
        self.bind(ctx, scope, name, value, false)
    }

    /// Records the value of a binding that may be absent on some paths.
    pub fn optional(
        &mut self,
        ctx: &mut CallContext,
        scope: Scope,
        name: &[u8],
        value: Binding,
    ) -> Result<()> {
        self.bind(ctx, scope, name, value, true)
    }

    fn bind(
        &mut self,
        ctx: &mut CallContext,
        scope: Scope,
        name: &[u8],
        value: Binding,
        optional: bool,
    ) -> Result<()> {
        ctx.checkpoint()?;
        let entries = &mut self.scopes.data[scope.0].values;
        for entry in &mut entries.data {
            ctx.charge(1)?;
            if types::binding_name_matches(ctx, entry.name.as_bytes().unwrap(), name, false)? {
                entry.value = value;
                entry.optional = optional;
                return Ok(());
            }
        }
        let name = ctx.bytes(name)?;
        entries.push(
            ctx,
            Entry {
                name,
                value,
                optional,
            },
        )
    }

    /// Copies binding state while sharing its immutable name storage.
    pub fn snapshot(&self, ctx: &mut CallContext) -> Result<Self> {
        ctx.checkpoint()?;
        let mut result = Self::new();
        for scope in &self.scopes.data {
            let target = result.scope(ctx)?;
            result.scopes.data[target.0].open = scope.open;
            let entries = &mut result.scopes.data[target.0].values;
            for entry in &scope.values.data {
                ctx.charge(1)?;
                entries.push(
                    ctx,
                    Entry {
                        name: entry.name.clone(),
                        value: entry.value,
                        optional: entry.optional,
                    },
                )?;
            }
        }
        Ok(result)
    }

    /// Replaces existing bindings from the first matching value scope, including
    /// non-type values. Source declarations use their current overlaid values.
    pub fn overlay(
        &mut self,
        ctx: &mut CallContext,
        target: Scope,
        scopes: &[Scope],
    ) -> Result<()> {
        ctx.checkpoint()?;
        for index in 0..self.scopes.data[target.0].values.data.len() {
            ctx.charge(1)?;
            let original = &self.scopes.data[target.0].values.data[index];
            let name = original.name.clone();
            let original = original.value;
            let mut replacement = None;
            let mut fallthrough = true;
            for &scope in scopes {
                ctx.charge(1)?;
                let entries = &self.scopes.data[scope.0];
                let mut candidate = None;
                for entry in &entries.values.data {
                    ctx.charge(1)?;
                    if types::binding_name_matches(
                        ctx,
                        entry.name.as_bytes().unwrap(),
                        name.as_bytes().unwrap(),
                        false,
                    )? {
                        candidate = Some((entry.value, entry.optional));
                        break;
                    }
                }
                if candidate.is_none() && entries.open {
                    candidate = Some((Binding::Unknown, false));
                }
                if let Some((value, optional)) = candidate {
                    replacement =
                        Some(replacement.map_or(value, |previous: Binding| previous.merge(value)));
                    if !optional {
                        fallthrough = false;
                        break;
                    }
                }
            }
            self.scopes.data[target.0].values.data[index].value = if fallthrough {
                replacement.map_or(original, |value| value.merge(original))
            } else {
                replacement.unwrap()
            };
        }
        Ok(())
    }

    /// Admits initial source identities without running declaration bodies.
    /// `owner` identifies this source environment within the analysis arena.
    pub fn source(
        &mut self,
        ctx: &mut CallContext,
        facts: &mut Facts,
        program: &Program,
        owner: usize,
    ) -> Result<Scope> {
        let scope = self.scope(ctx)?;
        for (index, value) in program.declarations.iter().enumerate() {
            ctx.charge(1)?;
            let mut symbols = Buffer::empty();
            let (name, members) = match &value.0 {
                Kind::Namespace(value) => (value.definition.name.as_bytes(), None),
                Kind::Enum(value) => {
                    for member in &value.definition.members {
                        ctx.charge(1)?;
                        symbols.push(ctx, member.symbol.as_bytes())?;
                    }
                    (value.definition.name.as_bytes(), Some(&symbols.data[..]))
                }
                _ => unreachable!(),
            };
            let fact = facts.nominal(ctx, owner, index, name, members)?;
            self.insert(
                ctx,
                scope,
                name,
                Binding::Type {
                    fact,
                    enumeration: members.is_some(),
                },
            )?;
        }
        Ok(scope)
    }

    /// Searches scopes in priority order, checking every exact spelling before
    /// considering folded spellings. Qualified roots always use exact spelling.
    pub fn resolve(
        &self,
        ctx: &mut CallContext,
        scopes: &[Scope],
        name: &str,
        enum_only: bool,
    ) -> Result<Resolution> {
        ctx.checkpoint()?;
        ctx.work_bytes(name.len())?;
        let (binding, member) = name
            .split_once('.')
            .map_or((name, None), |(a, b)| (a, Some(b)));
        for fold in [false, true] {
            if fold && member.is_some() {
                break;
            }
            for &scope in scopes {
                ctx.charge(1)?;
                let mut found = Resolution::Missing;
                let entries = &self.scopes.data[scope.0];
                let mut matched = false;
                for entry in &entries.values.data {
                    ctx.charge(1)?;
                    if !types::binding_name_matches(
                        ctx,
                        entry.name.as_bytes().unwrap(),
                        binding.as_bytes(),
                        fold,
                    )? {
                        continue;
                    }
                    matched = true;
                    let value = match (entry.value, member) {
                        (Binding::Unknown, _) => Resolution::Dynamic,
                        (Binding::Type { fact, .. }, None) => Resolution::Known(fact),
                        (Binding::Exports(exports), Some(member)) => {
                            self.member(ctx, exports, member.as_bytes(), enum_only)?
                        }
                        _ => Resolution::Missing,
                    };
                    found = found.merge(if entry.optional && value != Resolution::Missing {
                        Resolution::Dynamic
                    } else {
                        value
                    });
                    if found == Resolution::Ambiguous {
                        break;
                    }
                }
                if entries.open && (fold || !matched) {
                    found = found.merge(Resolution::Dynamic);
                }
                if found != Resolution::Missing {
                    return Ok(found);
                }
            }
        }
        Ok(Resolution::Missing)
    }

    fn member(
        &self,
        ctx: &mut CallContext,
        scope: Scope,
        name: &[u8],
        enum_only: bool,
    ) -> Result<Resolution> {
        for fold in [false, true] {
            let mut found = Resolution::Missing;
            let entries = &self.scopes.data[scope.0];
            let mut matched = false;
            for entry in &entries.values.data {
                ctx.charge(1)?;
                if !types::binding_name_matches(ctx, entry.name.as_bytes().unwrap(), name, fold)? {
                    continue;
                }
                matched = true;
                let candidate = match entry.value {
                    Binding::Unknown => Resolution::Dynamic,
                    Binding::Type { fact, enumeration } if !enum_only || enumeration => {
                        Resolution::Known(fact)
                    }
                    _ => Resolution::Missing,
                };
                found = found.merge(if entry.optional && candidate != Resolution::Missing {
                    Resolution::Dynamic
                } else {
                    candidate
                });
                if found == Resolution::Ambiguous {
                    break;
                }
            }
            if entries.open && (fold || !matched) {
                found = found.merge(Resolution::Dynamic);
            }
            if found != Resolution::Missing {
                return Ok(found);
            }
        }
        Ok(Resolution::Missing)
    }
}
