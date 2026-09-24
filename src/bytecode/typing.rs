//! The typed declarations a program makes beside its syntax tree.

use crate::{
    Result,
    compilation::Work,
    syntax::{BlockParam, typed::Additions},
};

pub(super) struct Typing {
    /// Typed block parameters, by the offset of their function's `def`.
    blocks: Vec<(u32, BlockParam)>,
}

impl Typing {
    pub fn new(additions: Additions, work: &dyn Work) -> Result<Self> {
        let Additions { blocks } = additions;
        let mut blocks: Vec<_> = blocks.into_iter().collect();
        work.charge(blocks.len())?;
        blocks.sort_by_key(|(offset, _)| *offset);
        Ok(Self { blocks })
    }

    /// The typed block parameter of the function whose `def` is at `offset`.
    pub fn block(&self, offset: u32) -> Option<&BlockParam> {
        let index = self
            .blocks
            .binary_search_by_key(&offset, |(offset, _)| *offset)
            .ok()?;
        Some(&self.blocks[index].1)
    }
}
