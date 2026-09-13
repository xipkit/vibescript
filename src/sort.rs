// Copyright 2022 The Go Authors. All rights reserved.
// Adapted from Go 1.27.1 src/slices/zsortanyfunc.go into a resumable driver.
// The original BSD license is reproduced in licenses/Go-BSD-3-Clause.txt.

use crate::{CallContext, Result, budget::Buffer};
use std::cmp::Ordering;

pub(crate) enum Action {
    Compare(usize, usize),
    Swap(usize, usize),
    Done,
}

#[derive(Clone, Copy)]
enum SearchKind {
    Left,
    Right,
    Middle,
}

#[derive(Clone, Copy)]
struct Search {
    a: usize,
    m: usize,
    b: usize,
    lo: usize,
    hi: usize,
    kind: SearchKind,
}

enum Task {
    Insert {
        a: usize,
        b: usize,
        i: usize,
        j: usize,
    },
    InsertResult {
        a: usize,
        b: usize,
        i: usize,
        j: usize,
    },
    Merge {
        a: usize,
        m: usize,
        b: usize,
    },
    Search(Search),
    SearchResult(Search, usize),
    Slide {
        from: usize,
        to: usize,
    },
    Rotate {
        m: usize,
        left: usize,
        right: usize,
    },
    SwapRange {
        a: usize,
        b: usize,
        len: usize,
    },
}

pub(crate) struct Sort {
    len: usize,
    base: usize,
    width: usize,
    inserting: bool,
    tasks: Buffer<Task>,
}

impl Sort {
    pub fn new(len: usize) -> Self {
        Self {
            len,
            base: 0,
            width: 20,
            inserting: true,
            tasks: Buffer::empty(),
        }
    }

    pub fn advance(
        &mut self,
        ctx: &mut CallContext,
        mut result: Option<Ordering>,
    ) -> Result<Action> {
        loop {
            ctx.charge(1)?;
            let Some(task) = self.tasks.data.pop() else {
                if self.inserting {
                    if self.base < self.len {
                        let a = self.base;
                        let b = self.len.min(a + self.width);
                        self.base = b;
                        self.tasks.push(
                            ctx,
                            Task::Insert {
                                a,
                                b,
                                i: a + 1,
                                j: a + 1,
                            },
                        )?;
                        continue;
                    }
                    self.inserting = false;
                    self.base = 0;
                }
                if self.width >= self.len {
                    return Ok(Action::Done);
                }
                let a = self.base;
                let m = a + self.width;
                if m < self.len {
                    let b = self.len.min(m + self.width);
                    self.base = b;
                    self.tasks.push(ctx, Task::Merge { a, m, b })?;
                } else {
                    self.width *= 2;
                    self.base = 0;
                }
                continue;
            };
            match task {
                Task::Insert { a, b, i, j } => {
                    if i < b {
                        if j > a {
                            self.tasks.push(ctx, Task::InsertResult { a, b, i, j })?;
                            return Ok(Action::Compare(j, j - 1));
                        }
                        self.tasks.push(
                            ctx,
                            Task::Insert {
                                a,
                                b,
                                i: i + 1,
                                j: i + 1,
                            },
                        )?;
                    }
                }
                Task::InsertResult { a, b, i, j } => {
                    if result.take().unwrap() == Ordering::Less {
                        self.tasks.push(ctx, Task::Insert { a, b, i, j: j - 1 })?;
                        return Ok(Action::Swap(j, j - 1));
                    }
                    self.tasks.push(
                        ctx,
                        Task::Insert {
                            a,
                            b,
                            i: i + 1,
                            j: i + 1,
                        },
                    )?;
                }
                Task::Merge { a, m, b } => {
                    let mid = a + (b - a) / 2;
                    let (lo, hi, kind) = if m - a == 1 {
                        (m, b, SearchKind::Left)
                    } else if b - m == 1 {
                        (a, m, SearchKind::Right)
                    } else if m > mid {
                        (mid + m - b, mid, SearchKind::Middle)
                    } else {
                        (a, m, SearchKind::Middle)
                    };
                    self.tasks.push(
                        ctx,
                        Task::Search(Search {
                            a,
                            m,
                            b,
                            lo,
                            hi,
                            kind,
                        }),
                    )?;
                }
                Task::Search(search) => {
                    let Search {
                        a,
                        m,
                        b,
                        lo,
                        hi,
                        kind,
                    } = search;
                    if lo < hi {
                        let h = lo + (hi - lo) / 2;
                        self.tasks.push(ctx, Task::SearchResult(search, h))?;
                        return Ok(match kind {
                            SearchKind::Left => Action::Compare(h, a),
                            SearchKind::Right => Action::Compare(m, h),
                            SearchKind::Middle => Action::Compare(a + (b - a) / 2 + m - 1 - h, h),
                        });
                    }
                    match kind {
                        SearchKind::Left => self.tasks.push(
                            ctx,
                            Task::Slide {
                                from: a,
                                to: lo - 1,
                            },
                        )?,
                        SearchKind::Right => {
                            self.tasks.push(ctx, Task::Slide { from: m, to: lo })?
                        }
                        SearchKind::Middle => {
                            let mid = a + (b - a) / 2;
                            let end = mid + m - lo;
                            // LIFO tasks retain Go's left-before-right comparator order.
                            if mid < end && end < b {
                                self.tasks.push(ctx, Task::Merge { a: mid, m: end, b })?;
                            }
                            if a < lo && lo < mid {
                                self.tasks.push(ctx, Task::Merge { a, m: lo, b: mid })?;
                            }
                            if lo < m && m < end {
                                self.tasks.push(
                                    ctx,
                                    Task::Rotate {
                                        m,
                                        left: m - lo,
                                        right: end - m,
                                    },
                                )?;
                            }
                        }
                    }
                }
                Task::SearchResult(mut search, h) => {
                    let less = result.take().unwrap() == Ordering::Less;
                    if less == matches!(search.kind, SearchKind::Left) {
                        search.lo = h + 1;
                    } else {
                        search.hi = h;
                    }
                    self.tasks.push(ctx, Task::Search(search))?;
                }
                Task::Slide { from, to } => {
                    if from != to {
                        let next = if from < to { from + 1 } else { from - 1 };
                        self.tasks.push(ctx, Task::Slide { from: next, to })?;
                        return Ok(Action::Swap(from, next));
                    }
                }
                Task::Rotate { m, left, right } => {
                    let (a, b, len) = if left > right {
                        self.tasks.push(
                            ctx,
                            Task::Rotate {
                                m,
                                left: left - right,
                                right,
                            },
                        )?;
                        (m - left, m, right)
                    } else if left < right {
                        self.tasks.push(
                            ctx,
                            Task::Rotate {
                                m,
                                left,
                                right: right - left,
                            },
                        )?;
                        (m - left, m + right - left, left)
                    } else {
                        (m - left, m, left)
                    };
                    self.tasks.push(ctx, Task::SwapRange { a, b, len })?;
                }
                Task::SwapRange { a, b, len } => {
                    if len > 1 {
                        self.tasks.push(
                            ctx,
                            Task::SwapRange {
                                a: a + 1,
                                b: b + 1,
                                len: len - 1,
                            },
                        )?;
                    }
                    return Ok(Action::Swap(a, b));
                }
            }
        }
    }
}
