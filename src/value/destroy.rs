//! Deep destruction reuses a parent link reserved in each container header.
//! Only uniquely owned containers are rewired; no reachable value changes.
//! This needs neither a growing worklist nor a live invocation budget, so it
//! also works when the final owner drops a value after quota exhaustion.

use super::{Kind, Value};
use crate::budget::Buffer;
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

fn discard(value: Value) {
    if value.depth() <= SHALLOW {
        return;
    }
    let Some(mut current) = unique(value) else {
        return;
    };
    loop {
        let child = match &mut current.0 {
            Kind::Array(array) => Arc::get_mut(array).unwrap().buffer.data.pop(),
            Kind::Hash(hash) => {
                let entries = &mut Arc::get_mut(hash).unwrap().buffer.data;
                match entries.last_mut() {
                    // Leave a nil marker while visiting a value, then visit its
                    // key on the next pass. Even a malformed internal key is
                    // therefore destroyed without recursive container glue.
                    Some((_, value)) if !matches!(value.0, Kind::Nil) => Some(mem::take(value)),
                    Some(_) => entries.pop().map(|(key, _)| key),
                    None => None,
                }
            }
            _ => unreachable!(),
        };
        if let Some(child) = child {
            if child.depth() > SHALLOW {
                if let Some(mut child) = unique(child) {
                    *parent(&mut child) = Some(current);
                    current = child;
                }
            }
        } else {
            let previous = parent(&mut current).take();
            // The container is empty and unlinked, so its normal destructor
            // releases its allocation and original charge without descending.
            drop(current);
            match previous {
                Some(value) => current = value,
                None => return,
            }
        }
    }
}

fn parent(value: &mut Value) -> &mut Option<Value> {
    match &mut value.0 {
        Kind::Array(array) => &mut Arc::get_mut(array).unwrap().drop_parent,
        Kind::Hash(hash) => &mut Arc::get_mut(hash).unwrap().drop_parent,
        _ => unreachable!(),
    }
}

fn unique(value: Value) -> Option<Value> {
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
            debug_assert!(Arc::get_mut(&mut array).unwrap().drop_parent.is_none());
            Some(Value(Kind::Array(array)))
        }
        Kind::Hash(mut hash) => {
            if Arc::get_mut(&mut hash).is_none() {
                hash = Arc::new(Arc::into_inner(hash)?);
            }
            debug_assert!(Arc::get_mut(&mut hash).unwrap().drop_parent.is_none());
            Some(Value(Kind::Hash(hash)))
        }
        _ => unreachable!(),
    }
}
