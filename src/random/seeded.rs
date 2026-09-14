// Copyright 2009 The Go Authors. All rights reserved.
// Adapted from Go 1.27.1 math/rand; see licenses/Go-BSD-3-Clause.txt.

use super::data::COOKED;
use crate::{CallContext, Result, budget::Buffer};

pub(crate) struct Seeded {
    pub seed: i64,
    tap: usize,
    feed: usize,
    state: Buffer<u64>,
}

impl Seeded {
    pub fn storage() -> usize {
        COOKED.len() * size_of::<u64>()
    }

    pub fn initialize(ctx: &mut CallContext, seed: i64, previous: Option<Self>) -> Result<Self> {
        let mut state = previous.map_or_else(Buffer::empty, |old| old.state);
        state.ensure(ctx, COOKED.len())?;
        state.data.clear();
        let mut x = seed.rem_euclid(i64::from(i32::MAX));
        if x == 0 {
            x = 89482311;
        }
        for _ in 0..20 {
            ctx.charge(1)?;
            x = step(x);
        }
        for &cooked in &COOKED {
            ctx.charge(1)?;
            x = step(x);
            let mut value = (x as u64) << 40;
            x = step(x);
            value ^= (x as u64) << 20;
            x = step(x);
            value ^= x as u64;
            value ^= cooked as u64;
            state.data.push(value);
        }
        Ok(Self {
            seed,
            tap: 0,
            feed: 607 - 273,
            state,
        })
    }

    pub fn next(&mut self) -> u64 {
        self.tap = if self.tap == 0 { 606 } else { self.tap - 1 };
        self.feed = if self.feed == 0 { 606 } else { self.feed - 1 };
        let value = self.state.data[self.feed].wrapping_add(self.state.data[self.tap]);
        self.state.data[self.feed] = value;
        value
    }
}

fn step(x: i64) -> i64 {
    let next = 48271 * (x % 44488) - 3399 * (x / 44488);
    if next < 0 {
        next + i64::from(i32::MAX)
    } else {
        next
    }
}
