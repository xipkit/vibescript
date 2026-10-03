//! A set of small indices whose copies share their structure: copying one
//! costs nothing, and changing one copies only the path to what changed,
//! so the checker can keep what holds at many points of a long function
//! without copying it at each.

use super::counted::{CountedSet, Ledger};
use std::rc::Rc;

/// The indices a leaf holds.
const BITS: usize = 64;
/// The children an inner node holds.
const FAN: usize = 16;

#[derive(Clone)]
enum Node {
    Leaf(u64),
    /// Children, each covering an equal part of the span, and how many
    /// indices they hold together.
    Inner(Vec<Rc<Node>>, usize),
}

/// A set of indices below the count it was made with.
#[derive(Clone)]
pub(crate) struct Marks {
    root: Rc<Node>,
    /// The indices the root covers.
    span: usize,
    /// The indices the set may hold.
    count: usize,
    len: usize,
    bytes: usize,
}

impl Marks {
    /// The set of every index below `count`.
    pub fn all(count: usize) -> Self {
        let mut span = BITS;
        while span < count {
            span *= FAN;
        }
        let (root, bytes) = full(span, count);
        Self {
            root,
            span,
            count,
            len: count,
            bytes,
        }
    }

    /// The bytes of the nodes this set allocated, whether or not it still
    /// shares them.
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn contains(&self, index: usize) -> bool {
        if index >= self.count {
            return false;
        }
        let (mut node, mut index, mut span) = (&self.root, index, self.span);
        loop {
            match &**node {
                Node::Leaf(bits) => return bits & (1u64 << index) != 0,
                Node::Inner(children, _) => {
                    span /= FAN;
                    node = &children[index / span];
                    index %= span;
                }
            }
        }
    }

    /// The bytes [`Self::set`] would copy to add `index` or take it out:
    /// those of the nodes on its path from the first another set shares,
    /// whose children copying it shares in turn.
    pub fn cost(&self, index: usize, present: bool) -> usize {
        if index >= self.count || self.contains(index) == present {
            return 0;
        }
        let (mut node, mut index, mut span) = (&self.root, index, self.span);
        let (mut shared, mut copied) = (false, 0);
        loop {
            shared = shared || Rc::strong_count(node) > 1;
            if shared {
                copied += node_bytes(node);
            }
            match &**node {
                Node::Leaf(_) => return copied,
                Node::Inner(children, _) => {
                    span /= FAN;
                    node = &children[index / span];
                    index %= span;
                }
            }
        }
    }

    /// The bytes [`Self::all`] of `count` allocates, found as it builds
    /// the set, without building it.
    pub fn most(count: usize) -> usize {
        let mut span = BITS;
        while span < count {
            span *= FAN;
        }
        full_bytes(span, count)
    }

    /// Adds `index`, or takes it out; returns the bytes of the nodes that
    /// had to be copied, since another set shared them.
    pub fn set(&mut self, index: usize, present: bool) -> usize {
        if index >= self.count || self.contains(index) == present {
            return 0;
        }
        if present {
            self.len += 1;
        } else {
            self.len -= 1;
        }
        let (mut node, mut index, mut span) = (&mut self.root, index, self.span);
        let mut copied = 0;
        loop {
            if Rc::strong_count(node) > 1 {
                copied += node_bytes(node);
            }
            match Rc::make_mut(node) {
                Node::Leaf(bits) => {
                    if present {
                        *bits |= 1u64 << index;
                    } else {
                        *bits &= !(1u64 << index);
                    }
                    self.bytes += copied;
                    return copied;
                }
                Node::Inner(children, held) => {
                    if present {
                        *held += 1;
                    } else {
                        *held -= 1;
                    }
                    span /= FAN;
                    node = &mut children[index / span];
                    index %= span;
                }
            }
        }
    }

    /// Adds the nodes of this set that `seen`, their addresses, lacks,
    /// each counted to `ledger` before it is; returns their bytes and the
    /// nodes it looked at, or `None` once the budget refuses one, which
    /// stops the check. A node `seen` has is not entered: what it holds was
    /// added with it, and nodes another set shares never change.
    pub fn retain(
        &self,
        seen: &mut CountedSet<usize>,
        ledger: Ledger<'_>,
    ) -> Option<(usize, usize)> {
        let (mut bytes, mut visited) = (0, 0);
        let mut pending = vec![&self.root];
        while let Some(node) = pending.pop() {
            visited += 1;
            if !seen.insert(ledger, Rc::as_ptr(node) as usize).ok()? {
                continue;
            }
            bytes += node_bytes(node);
            if let Node::Inner(children, _) = &**node {
                pending.extend(children);
            }
        }
        Some((bytes, visited))
    }

    /// The indices in the set, in order.
    pub fn indices(&self) -> Vec<usize> {
        let mut found = Vec::with_capacity(self.len);
        collect(&self.root, 0, self.span, &mut found);
        found
    }
}

/// A node covering `span` indices from 0, holding those below `count`,
/// and the bytes of the nodes it made. Nodes wholly full or wholly empty
/// are shared.
fn full(span: usize, count: usize) -> (Rc<Node>, usize) {
    if span == BITS {
        let bits = if count >= BITS {
            u64::MAX
        } else {
            (1u64 << count) - 1
        };
        let node = Node::Leaf(bits);
        let bytes = node_bytes(&node);
        return (Rc::new(node), bytes);
    }
    let child = span / FAN;
    let (whole, mut bytes) = full(child, child);
    let (empty, more) = full(child, 0);
    bytes += more;
    let children = (0..FAN)
        .map(|index| {
            let start = index * child;
            if start + child <= count {
                Rc::clone(&whole)
            } else if start >= count {
                Rc::clone(&empty)
            } else {
                let (node, more) = full(child, count - start);
                bytes += more;
                node
            }
        })
        .collect();
    let node = Node::Inner(children, count.min(span));
    bytes += node_bytes(&node);
    (Rc::new(node), bytes)
}

/// The bytes [`full`] of `span` and `count` allocates.
fn full_bytes(span: usize, count: usize) -> usize {
    if span == BITS {
        return node_bytes(&Node::Leaf(0));
    }
    let child = span / FAN;
    let partial = (count % child != 0 && count < span).then(|| full_bytes(child, count % child));
    full_bytes(child, child)
        + full_bytes(child, 0)
        + partial.unwrap_or(0)
        + node_bytes(&Node::Inner(Vec::new(), 0))
}

fn collect(node: &Node, start: usize, span: usize, found: &mut Vec<usize>) {
    match node {
        Node::Leaf(bits) => {
            let mut bits = *bits;
            while bits != 0 {
                found.push(start + bits.trailing_zeros() as usize);
                bits &= bits - 1;
            }
        }
        // A part holding none is not entered, so a sparse set's indices
        // are found along their paths alone.
        Node::Inner(_, 0) => (),
        Node::Inner(children, _) => {
            let child = span / FAN;
            for (index, node) in children.iter().enumerate() {
                collect(node, start + index * child, child, found);
            }
        }
    }
}

/// The bytes copying `node` allocates.
fn node_bytes(node: &Node) -> usize {
    std::mem::size_of::<Node>()
        + 16
        + match node {
            Node::Leaf(_) => 0,
            Node::Inner(..) => FAN * std::mem::size_of::<Rc<Node>>(),
        }
}

#[cfg(test)]
mod tests {
    use super::Marks;

    #[test]
    fn a_sparse_sets_indices_are_found_along_their_paths() {
        let count = 20_000;
        let mut marks = Marks::all(count);
        for index in (0..count).filter(|&index| index != 12_345) {
            marks.set(index, false);
        }
        assert_eq!(marks.indices(), [12_345]);
        marks.set(7, true);
        assert_eq!(marks.indices(), [7, 12_345]);
        assert_eq!(Marks::all(count).indices().len(), count);
    }

    #[test]
    fn a_change_costs_what_it_copies() {
        for count in [1, 64, 1_000, 20_000] {
            let mut marks = Marks::all(count);
            assert_eq!(marks.bytes(), Marks::most(count));
            let shared = marks.clone();
            for index in [0, count / 2, count - 1] {
                let cost = marks.cost(index, false);
                assert_eq!(marks.set(index, false), cost);
                assert_eq!(marks.cost(index, false), 0);
            }
            drop(shared);
        }
    }

    #[test]
    fn copies_keep_what_they_held() {
        for count in [0, 1, 63, 64, 65, 1_000, 20_000] {
            let mut marks = Marks::all(count);
            assert_eq!(marks.len(), count);
            assert_eq!(marks.indices(), (0..count).collect::<Vec<_>>());
            let before = marks.clone();
            for index in (0..count).step_by(3) {
                marks.set(index, false);
            }
            assert_eq!(before.indices(), (0..count).collect::<Vec<_>>());
            let expected: Vec<usize> = (0..count).filter(|index| index % 3 != 0).collect();
            assert_eq!(marks.indices(), expected);
            assert_eq!(marks.len(), expected.len());
            assert!(!marks.contains(0));
            marks.set(0, true);
            assert_eq!(marks.contains(0), count > 0);
            marks.set(count, true);
            assert!(!marks.contains(count));
        }
    }
}
