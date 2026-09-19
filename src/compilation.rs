use crate::{CallContext, Error, ErrorKind, Result, budget::Charge};
use std::cell::RefCell;

mod buffer;
mod name;
mod storage;
mod table;
mod types;
pub(crate) use buffer::Buffer;
pub(crate) use name::Name;
pub(crate) use storage::{Boxed, Bytes, Text};
pub(crate) use table::Table;
pub(crate) use types::{Field, Type, TypeKind};

pub(crate) trait Work {
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
            TypeKind::Union(options) => {
                for option in options {
                    self.ty(option)?;
                }
            }
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
