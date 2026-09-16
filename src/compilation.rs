use crate::{CallContext, Result};
use std::cell::RefCell;

pub(crate) trait Work {
    fn charge(&self, steps: usize) -> Result<()>;
    fn bytes(&self, bytes: usize) -> Result<()>;
    fn checkpoint(&self) -> Result<()>;

    fn ty(&self, ty: &crate::types::Type) -> Result<()> {
        use crate::types::TypeKind;
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
            TypeKind::Union(options) => {
                for option in options {
                    self.ty(option)?;
                }
            }
            _ => (),
        }
        Ok(())
    }

    fn names(&self, names: &[String]) -> Result<()> {
        for name in names {
            self.bytes(name.len())?;
        }
        Ok(())
    }
}

impl Work for () {
    fn charge(&self, _: usize) -> Result<()> {
        Ok(())
    }
    fn bytes(&self, _: usize) -> Result<()> {
        Ok(())
    }
    fn checkpoint(&self) -> Result<()> {
        Ok(())
    }
}

pub(crate) struct Meter<'a>(pub RefCell<&'a mut CallContext>);

impl Work for Meter<'_> {
    fn charge(&self, steps: usize) -> Result<()> {
        self.0.borrow_mut().charge(steps as u64)
    }
    fn bytes(&self, bytes: usize) -> Result<()> {
        self.0.borrow_mut().work_bytes(bytes)
    }
    fn checkpoint(&self) -> Result<()> {
        self.0.borrow_mut().checkpoint()
    }
}

#[cfg(test)]
mod tests;
