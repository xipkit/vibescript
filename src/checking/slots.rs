use crate::{CallContext, Result, budget::Charge};
use std::{mem::size_of, sync::Arc};

const WIDTH: usize = 16;
const BITS: u32 = 4;

#[derive(Debug)]
enum Data<T> {
    Leaf([T; WIDTH]),
    Branch([Option<Arc<Node<T>>>; WIDTH]),
}

#[derive(Debug)]
struct Node<T> {
    data: Data<T>,
    _charge: Option<Charge>,
}

#[derive(Debug)]
pub(super) struct Slots<T> {
    len: usize,
    shift: u32,
    empty: T,
    root: Option<Arc<Node<T>>>,
}

impl<T: Copy + Eq> Slots<T> {
    pub fn new(len: usize, empty: T) -> Self {
        let bits = usize::BITS - len.saturating_sub(1).leading_zeros();
        Self {
            len,
            shift: bits.saturating_sub(1) / BITS * BITS,
            empty,
            root: None,
        }
    }

    pub fn snapshot(&self, ctx: &mut CallContext) -> Result<Self> {
        ctx.checkpoint()?;
        ctx.charge(1)?;
        Ok(Self {
            len: self.len,
            shift: self.shift,
            empty: self.empty,
            root: self.root.clone(),
        })
    }

    pub fn get(&self, ctx: &mut CallContext, index: usize) -> Result<T> {
        ctx.checkpoint()?;
        assert!(index < self.len);
        let mut node = &self.root;
        let mut shift = self.shift;
        loop {
            ctx.charge(1)?;
            match node.as_deref().map(|node| &node.data) {
                None => return Ok(self.empty),
                Some(Data::Leaf(values)) => return Ok(values[index & (WIDTH - 1)]),
                Some(Data::Branch(children)) => {
                    node = &children[(index >> shift) & (WIDTH - 1)];
                    shift -= BITS;
                }
            }
        }
    }

    pub fn set(&mut self, ctx: &mut CallContext, index: usize, value: T) -> Result<()> {
        assert!(index < self.len);
        if self.get(ctx, index)? != value {
            Self::write(ctx, &mut self.root, self.shift, index, self.empty, value)?;
        }
        Ok(())
    }

    fn allocate(ctx: &mut CallContext, data: Data<T>) -> Result<Arc<Node<T>>> {
        let charge = ctx.reserve(size_of::<Node<T>>() + 2 * size_of::<usize>())?;
        Ok(Arc::new(Node {
            data,
            _charge: charge,
        }))
    }

    // Recursion is bounded by the number of radix digits in a machine index.
    fn write(
        ctx: &mut CallContext,
        root: &mut Option<Arc<Node<T>>>,
        shift: u32,
        index: usize,
        empty: T,
        value: T,
    ) -> Result<()> {
        ctx.charge(WIDTH as u64)?;
        if root
            .as_ref()
            .is_none_or(|node| Arc::strong_count(node) != 1)
        {
            let data = match root.as_deref().map(|node| &node.data) {
                Some(Data::Leaf(values)) => Data::Leaf(*values),
                Some(Data::Branch(children)) => Data::Branch(children.clone()),
                None if shift == 0 => Data::Leaf([empty; WIDTH]),
                None => Data::Branch(std::array::from_fn(|_| None)),
            };
            *root = Some(Self::allocate(ctx, data)?);
        }
        let node = Arc::get_mut(root.as_mut().unwrap()).unwrap();
        match &mut node.data {
            Data::Leaf(values) => values[index & (WIDTH - 1)] = value,
            Data::Branch(children) => Self::write(
                ctx,
                &mut children[(index >> shift) & (WIDTH - 1)],
                shift - BITS,
                index,
                empty,
                value,
            )?,
        }
        Ok(())
    }

    pub fn merge(
        &mut self,
        ctx: &mut CallContext,
        other: &Self,
        mut join: impl FnMut(&mut CallContext, T, T) -> Result<T>,
    ) -> Result<bool> {
        ctx.checkpoint()?;
        assert!(self.len == other.len && self.empty == other.empty);
        let next = Self::join(
            ctx,
            &self.root,
            &other.root,
            self.shift,
            self.empty,
            &mut join,
        )?;
        let changed = !same(&self.root, &next);
        self.root = next;
        Ok(changed)
    }

    pub fn equal(&self, ctx: &mut CallContext, other: &Self) -> Result<bool> {
        ctx.charge(1)?;
        if self.len != other.len || self.empty != other.empty {
            return Ok(false);
        }
        Self::equal_nodes(ctx, &self.root, &other.root, self.shift, self.empty)
    }

    fn equal_nodes(
        ctx: &mut CallContext,
        left: &Option<Arc<Node<T>>>,
        right: &Option<Arc<Node<T>>>,
        shift: u32,
        empty: T,
    ) -> Result<bool> {
        ctx.charge(1)?;
        if same(left, right) {
            return Ok(true);
        }
        if shift == 0 {
            let leaf = |node: &Option<Arc<Node<T>>>| match node.as_deref().map(|n| &n.data) {
                Some(Data::Leaf(values)) => *values,
                None => [empty; WIDTH],
                _ => unreachable!(),
            };
            ctx.charge(WIDTH as u64)?;
            return Ok(leaf(left) == leaf(right));
        }
        for i in 0..WIDTH {
            let child = |node: &Option<Arc<Node<T>>>| match node.as_deref().map(|n| &n.data) {
                Some(Data::Branch(children)) => children[i].clone(),
                None => None,
                _ => unreachable!(),
            };
            if !Self::equal_nodes(ctx, &child(left), &child(right), shift - BITS, empty)? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn join(
        ctx: &mut CallContext,
        left: &Option<Arc<Node<T>>>,
        right: &Option<Arc<Node<T>>>,
        shift: u32,
        empty: T,
        join: &mut impl FnMut(&mut CallContext, T, T) -> Result<T>,
    ) -> Result<Option<Arc<Node<T>>>> {
        ctx.charge(1)?;
        if same(left, right) {
            return Ok(left.clone());
        }
        let mut changed = false;
        let data = if shift == 0 {
            let leaf = |node: &Option<Arc<Node<T>>>| match node.as_deref() {
                Some(Node {
                    data: Data::Leaf(values),
                    ..
                }) => *values,
                None => [empty; WIDTH],
                _ => unreachable!(),
            };
            let mut values = leaf(left);
            for (value, incoming) in values.iter_mut().zip(leaf(right)) {
                ctx.charge(1)?;
                if *value != incoming {
                    let merged = join(ctx, *value, incoming)?;
                    changed |= *value != merged;
                    *value = merged;
                }
            }
            Data::Leaf(values)
        } else {
            let children = |node: &Option<Arc<Node<T>>>| match node.as_deref() {
                Some(Node {
                    data: Data::Branch(children),
                    ..
                }) => children.clone(),
                None => std::array::from_fn(|_| None),
                _ => unreachable!(),
            };
            ctx.charge((2 * WIDTH) as u64)?;
            let mut children_left = children(left);
            for (child, incoming) in children_left.iter_mut().zip(children(right)) {
                let merged = Self::join(ctx, child, &incoming, shift - BITS, empty, join)?;
                changed |= !same(child, &merged);
                *child = merged;
            }
            Data::Branch(children_left)
        };
        if changed {
            Ok(Some(Self::allocate(ctx, data)?))
        } else {
            Ok(left.clone())
        }
    }
}

fn same<T>(left: &Option<Arc<Node<T>>>, right: &Option<Arc<Node<T>>>) -> bool {
    match (left, right) {
        (Some(left), Some(right)) => Arc::ptr_eq(left, right),
        (None, None) => true,
        _ => false,
    }
}
