use crate::{CallContext, Error, ErrorKind, Result, budget::Charge};
use std::cell::RefCell;

mod buffer;
mod diagnostics;
mod name;
mod storage;
mod table;
mod tasks;
mod types;
pub(crate) use buffer::{Buffer, IntoIter};
pub(crate) use diagnostics::{error, formatted};
pub(crate) use name::Name;
pub(crate) use storage::{Boxed, Bytes, Text};
pub(crate) use table::Table;
pub(crate) use tasks::{Frame, Tasks, framed, task};
pub(crate) use types::{Field, Type, TypeKind};

/// What compilation may still spend, as a copy another thread can check
/// without the context it came from: the type checker runs on a thread of
/// its own for long sources, and stops when it passes these.
#[derive(Clone, Debug, Default)]
pub(crate) struct Budget {
    /// The steps left before the step quota, if there is one.
    pub steps: Option<u64>,
    /// The bytes left before the memory quota, if there is one.
    pub memory: Option<usize>,
    pub deadline: Option<std::time::Instant>,
    pub cancellation: Option<crate::CancellationToken>,
}

impl Budget {
    /// The budget left after `steps` more steps.
    pub fn less(&self, steps: u64) -> Self {
        Self {
            steps: self.steps.map(|left| left.saturating_sub(steps)),
            ..self.clone()
        }
    }

    /// Whether the deadline has passed or the host cancelled.
    pub fn interrupted(&self) -> bool {
        self.cancellation
            .as_ref()
            .is_some_and(crate::CancellationToken::is_cancelled)
            || self
                .deadline
                .is_some_and(|deadline| std::time::Instant::now() >= deadline)
    }
}

pub(crate) trait Work {
    /// Whether compilation runs outside an invocation's budget.
    fn unmetered(&self) -> bool {
        false
    }

    /// What the work may still spend; unlimited unless metered.
    fn budget(&self) -> Budget {
        Budget::default()
    }

    fn charge(&self, steps: usize) -> Result<()>;
    fn bytes(&self, bytes: usize) -> Result<()>;
    fn checkpoint(&self) -> Result<()>;
    fn reserve(&self, bytes: usize) -> Result<Option<Charge>>;
    fn allocation_error(&self, message: &str) -> Error;

    fn ty(&self, ty: &Type) -> Result<()> {
        self.charge(1)?;
        self.bytes(ty.name.len())?;
        match &ty.kind {
            TypeKind::Array(Some(element)) => self.ty(element)?,
            TypeKind::Hash(Some(pair)) => {
                self.ty(&pair.0)?;
                self.ty(&pair.1)?;
            }
            TypeKind::Shape(fields, _) => {
                for field in fields {
                    self.bytes(field.name.len())?;
                    self.ty(&field.ty)?;
                }
            }
            TypeKind::Union(options) | TypeKind::Tuple(options) => {
                for option in options {
                    self.ty(option)?;
                }
            }
            TypeKind::Literal(Some(described)) => self.ty(described)?,
            _ => (),
        }
        Ok(())
    }

    fn names(&self, names: &[Name]) -> Result<()> {
        for name in names {
            self.bytes(name.len())?;
        }
        Ok(())
    }
}

impl Work for () {
    fn unmetered(&self) -> bool {
        true
    }

    fn charge(&self, _: usize) -> Result<()> {
        Ok(())
    }
    fn bytes(&self, _: usize) -> Result<()> {
        Ok(())
    }
    fn checkpoint(&self) -> Result<()> {
        Ok(())
    }
    fn reserve(&self, _: usize) -> Result<Option<Charge>> {
        Ok(None)
    }
    fn allocation_error(&self, message: &str) -> Error {
        Error::new(ErrorKind::Memory, message)
    }
}

pub(crate) struct Meter<'a>(pub RefCell<&'a mut CallContext>);

impl Work for Meter<'_> {
    fn budget(&self) -> Budget {
        self.0.borrow().budget()
    }
    fn charge(&self, steps: usize) -> Result<()> {
        self.0.borrow_mut().charge(steps as u64)
    }
    fn bytes(&self, bytes: usize) -> Result<()> {
        self.0.borrow_mut().work_bytes(bytes)
    }
    fn checkpoint(&self) -> Result<()> {
        self.0.borrow_mut().checkpoint()
    }
    fn reserve(&self, bytes: usize) -> Result<Option<Charge>> {
        self.0.borrow_mut().reserve(bytes)
    }
    fn allocation_error(&self, message: &str) -> Error {
        let mut context = self.0.borrow_mut();
        context
            .checkpoint()
            .err()
            .unwrap_or_else(|| context.fail::<()>(ErrorKind::Memory, message).unwrap_err())
    }
}

#[cfg(test)]
mod tests;
