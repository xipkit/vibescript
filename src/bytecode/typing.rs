//! The typed declarations a program makes beside its syntax tree: type
//! aliases, typed block parameters and instance-variable declarations.

use super::aliases::Aliases;
use crate::{
    Result,
    compilation::{Buffer, Work},
    syntax::{BlockParam, Stmt, modules::Module, typed::Additions, typed::Ivar},
};
use std::collections::HashMap;

pub(super) struct Typing {
    pub aliases: Aliases,
    /// Typed block parameters, by the offset of their function's `def`.
    blocks: Vec<(u32, BlockParam)>,
    /// Instance-variable declarations, by the declaring class's offset.
    ivars: Vec<(u32, Ivar)>,
    /// Their default assignments, by the declaring class's offset.
    defaults: Vec<(u32, Stmt)>,
    /// The default assignments each constructor runs before its parameters
    /// bind, by function index.
    pub prologues: HashMap<usize, Vec<Stmt>>,
}

impl Typing {
    /// Collects a program's typed declarations. `declared` reports whether
    /// the program declares a class, module or enum of a name.
    pub fn new(
        additions: Additions,
        modules: &Buffer<Module>,
        declared: impl Fn(&str) -> bool,
        work: &dyn Work,
    ) -> Result<Self> {
        let Additions {
            blocks,
            aliases,
            ivars,
            defaults,
        } = additions;
        let mut blocks: Vec<_> = blocks.into_iter().collect();
        work.charge(blocks.len())?;
        blocks.sort_by_key(|(offset, _)| *offset);
        Ok(Self {
            aliases: Aliases::new(aliases, modules, declared, work)?,
            blocks,
            ivars: ivars.into_iter().collect(),
            defaults: defaults.into_iter().collect(),
            prologues: HashMap::new(),
        })
    }

    /// The typed block parameter of the function whose `def` is at `offset`.
    pub fn block(&self, offset: u32) -> Option<&BlockParam> {
        let index = self
            .blocks
            .binary_search_by_key(&offset, |(offset, _)| *offset)
            .ok()?;
        Some(&self.blocks[index].1)
    }

    /// Takes the instance variables the class at `offset` declares and the
    /// assignments of their defaults.
    pub fn class(&mut self, offset: u32) -> (Vec<Ivar>, Vec<Stmt>) {
        if self.ivars.is_empty() {
            return (Vec::new(), Vec::new());
        }
        let (ivars, rest): (Vec<_>, Vec<_>) = std::mem::take(&mut self.ivars)
            .into_iter()
            .partition(|(owner, _)| *owner == offset);
        self.ivars = rest;
        let (defaults, rest): (Vec<_>, Vec<_>) = std::mem::take(&mut self.defaults)
            .into_iter()
            .partition(|(owner, _)| *owner == offset);
        self.defaults = rest;
        (
            ivars.into_iter().map(|(_, ivar)| ivar).collect(),
            defaults.into_iter().map(|(_, stmt)| stmt).collect(),
        )
    }
}
