use super::{
    files::{Root, Stamp},
    resolver::Origin,
};
use crate::{CallContext, Error, ErrorKind, Result, actor::Actor};
use std::{collections::HashMap, sync::Arc};

pub(super) struct Entry<T> {
    pub origin: Origin,
    pub stamp: Stamp,
    pub code: T,
}

#[derive(Clone)]
pub(super) struct Epoch(Arc<()>);

type Modules<T> = HashMap<Root, HashMap<Arc<[u8]>, Arc<Entry<T>>>>;

struct State<T> {
    modules: Modules<T>,
    count: usize,
    epoch: Epoch,
}

impl<T> State<T> {
    fn new() -> Self {
        Self {
            modules: HashMap::new(),
            count: 0,
            epoch: Epoch(Arc::new(())),
        }
    }

    fn get(&self, root: &Root, name: &[u8]) -> Option<&Arc<Entry<T>>> {
        self.modules.get(root)?.get(name)
    }
}

pub(super) struct Cache<T> {
    state: Actor<State<T>>,
    limit: usize,
}

impl<T> Cache<T> {
    pub fn new(limit: usize) -> Self {
        Self {
            state: Actor::new(State::new()),
            limit,
        }
    }

    pub fn lookup(
        &self,
        ctx: &mut CallContext,
        root: &Root,
        name: &[u8],
    ) -> Result<(Epoch, Option<Arc<Entry<T>>>)> {
        key_work(ctx, root, name)?;
        self.with_state(ctx, |state| {
            Ok((state.epoch.clone(), state.get(root, name).cloned()))
        })
    }

    pub fn insert(
        &self,
        ctx: &mut CallContext,
        epoch: &Epoch,
        origin: Origin,
        stamp: Stamp,
        code: T,
    ) -> Result<Arc<Entry<T>>> {
        key_work(ctx, &origin.root, &origin.relative)?;
        let entry = Arc::new(Entry {
            origin,
            stamp,
            code,
        });
        self.with_state(ctx, |state| {
            if !Arc::ptr_eq(&state.epoch.0, &epoch.0) {
                // A clear must not be undone by a compilation that began before it.
                Ok(entry.clone())
            } else if let Some(existing) = state.get(&entry.origin.root, &entry.origin.relative) {
                Ok(existing.clone())
            } else if state.count >= self.limit {
                Err(Error::new(
                    ErrorKind::Runtime,
                    format!(
                        "require: module cache limit reached ({} modules)",
                        self.limit
                    ),
                ))
            } else {
                state
                    .modules
                    .entry(entry.origin.root.clone())
                    .or_default()
                    .insert(entry.origin.relative.clone(), entry.clone());
                state.count += 1;
                Ok(entry.clone())
            }
        })
    }

    pub fn invalidate(&self, ctx: &mut CallContext, entry: &Arc<Entry<T>>) -> Result<()> {
        key_work(ctx, &entry.origin.root, &entry.origin.relative)?;
        let removed = self.with_state(ctx, |state| {
            let Some(existing) = state.get(&entry.origin.root, &entry.origin.relative) else {
                return Ok(None);
            };
            if !Arc::ptr_eq(existing, entry) {
                return Ok(None);
            }
            let modules = state.modules.get_mut(&entry.origin.root).unwrap();
            let removed = modules.remove(entry.origin.relative.as_ref());
            if modules.is_empty() {
                state.modules.remove(&entry.origin.root);
            }
            state.count -= 1;
            Ok(removed)
        })?;
        // Code can retain host callbacks whose destructors reenter the engine.
        drop(removed);
        Ok(())
    }

    pub fn clear(&self) {
        let old = self
            .state
            .with(|state| std::mem::replace(state, State::new()));
        // Code can retain host callbacks whose destructors reenter the engine.
        drop(old);
    }

    /// Runs `f` on the cache's state, checking the call's budget before
    /// waiting its turn and again once it has it.
    fn with_state<R>(
        &self,
        ctx: &mut CallContext,
        f: impl FnOnce(&mut State<T>) -> Result<R>,
    ) -> Result<R> {
        ctx.checkpoint()?;
        self.state.with(|state| {
            ctx.checkpoint()?;
            f(state)
        })
    }
}

fn key_work(ctx: &mut CallContext, root: &Root, name: &[u8]) -> Result<()> {
    ctx.charge(
        (root.path().as_os_str().as_encoded_bytes().len() as u64)
            .saturating_add(name.len() as u64)
            .saturating_add(1),
    )
}

#[cfg(test)]
mod tests;
