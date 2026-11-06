//! Deep destruction reuses a parent link reserved in each container header.
//! Only uniquely owned containers are rewired; no reachable value changes.
//! This needs neither a growing worklist nor a live invocation budget, so it
//! also works when the final owner drops a value after quota exhaustion.

use super::{Heap, Kind, Value};
use crate::budget::Buffer;
use crate::hash::Hash;
use std::{mem, sync::Arc};

pub(crate) const SHALLOW: usize = 8;

pub(crate) fn values(mut buffer: Buffer<Value>) {
    while let Some(value) = buffer.data.pop() {
        discard(value);
    }
}

pub(crate) fn pairs(mut buffer: Buffer<(Value, Value)>) {
    while let Some((key, value)) = buffer.data.pop() {
        discard(key);
        discard(value);
    }
}

enum Container {
    Array(Arc<Heap<Value>>),
    Hash(Arc<Hash>),
}

impl Container {
    fn into_value(self) -> Value {
        Value(match self {
            Self::Array(array) => Kind::Array(array),
            Self::Hash(hash) => Kind::Hash(hash),
        })
    }
}

fn discard(value: Value) {
    if value.depth() <= SHALLOW {
        return;
    }
    let Some(mut current) = unique(value) else {
        return;
    };
    loop {
        let child = match &mut current {
            Container::Array(array) => {
                Arc::get_mut(array).and_then(|array| array.buffer.data.pop())
            }
            Container::Hash(hash) => {
                Arc::get_mut(hash).and_then(|hash| {
                    let entries = &mut hash.buffer.data;
                    match entries.last_mut() {
                        // Leave a nil marker while visiting a value, then visit its
                        // key on the next pass. Even a malformed internal key is
                        // therefore destroyed without recursive container glue.
                        Some((_, value)) if !matches!(value.0, Kind::Nil) => Some(mem::take(value)),
                        Some(_) => entries.pop().map(|(key, _)| key),
                        None => None,
                    }
                })
            }
        };
        if let Some(child) = child {
            if child.depth() > SHALLOW {
                if let Some(mut child) = unique(child) {
                    if let Some(parent) = parent(&mut child) {
                        *parent = Some(current.into_value());
                        current = child;
                    }
                }
            }
        } else {
            let previous = parent(&mut current).and_then(Option::take);
            // The container is empty and unlinked, so its normal destructor
            // releases its allocation and original charge without descending.
            drop(current);
            match previous {
                Some(value) => match value.0 {
                    Kind::Array(array) => current = Container::Array(array),
                    Kind::Hash(hash) => current = Container::Hash(hash),
                    _ => return,
                },
                None => return,
            }
        }
    }
}

fn parent(value: &mut Container) -> Option<&mut Option<Value>> {
    match value {
        Container::Array(array) => Arc::get_mut(array).map(|array| &mut array.drop_parent),
        Container::Hash(hash) => Arc::get_mut(hash).map(|hash| &mut hash.drop_parent),
    }
}

fn unique(value: Value) -> Option<Container> {
    match value.0 {
        Kind::Array(mut array) => {
            if Arc::get_mut(&mut array).is_none() {
                // Consuming the reference atomically avoids racing another
                // owner's drop into a recursive last-owner destructor. In the
                // rare case we obtain the payload, its retained header charge
                // covers the replacement allocation. Container Arcs expose no
                // Weak references, so the old allocation has already been freed.
                array = Arc::new(Arc::into_inner(array)?);
            }
            debug_assert!(array.drop_parent.is_none());
            Some(Container::Array(array))
        }
        Kind::Hash(mut hash) => {
            if Arc::get_mut(&mut hash).is_none() {
                hash = Arc::new(Arc::into_inner(hash)?);
            }
            debug_assert!(hash.drop_parent.is_none());
            Some(Container::Hash(hash))
        }
        _ => None,
    }
}
