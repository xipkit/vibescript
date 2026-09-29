//! A set of small indices whose copies share their structure: copying one
//! costs nothing, and changing one copies only the path to what changed,
//! so the checker can keep what holds at many points of a long function
//! without copying it at each.

use std::rc::Rc;

/// The indices a leaf holds.
const BITS: usize = 64;
/// The children an inner node holds.
const FAN: usize = 16;

#[derive(Clone)]
enum Node {
    Leaf(u64),
    Inner(Vec<Rc<Node>>),
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
                Node::Inner(children) => {
                    span /= FAN;
                    node = &children[index / span];
                    index %= span;
                }
            }
        }
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
                Node::Inner(children) => {
                    span /= FAN;
                    node = &mut children[index / span];
                    index %= span;
                }
            }
        }
    }

    /// Adds the nodes of this set that `seen`, their addresses, lacks;
    /// returns their bytes and the nodes it looked at. A node `seen` has
    /// is not entered: what it holds was added with it, and nodes another
    /// set shares never change.
    pub fn retain(&self, seen: &mut std::collections::HashSet<usize>) -> (usize, usize) {
        let (mut bytes, mut visited) = (0, 0);
        let mut pending = vec![&self.root];
        while let Some(node) = pending.pop() {
            visited += 1;
            if !seen.insert(Rc::as_ptr(node) as usize) {
                continue;
            }
            bytes += node_bytes(node);
            if let Node::Inner(children) = &**node {
                pending.extend(children);
            }
        }
        (bytes, visited)
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
    let node = Node::Inner(children);
    bytes += node_bytes(&node);
    (Rc::new(node), bytes)
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
        Node::Inner(children) => {
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
            Node::Inner(_) => FAN * std::mem::size_of::<Rc<Node>>(),
        }
}

#[cfg(test)]
mod tests {
    use super::Marks;

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
