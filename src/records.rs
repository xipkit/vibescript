//! Field names that records of one shape share, for code that builds records
//! outside the VM.
//!
//! A record, the runtime value of a shape type, is an ordinary hash: it
//! keeps insertion order, converts, compares, iterates and serializes like
//! any hash, and its accounting is a hash's. What makes records compact is
//! that their keys share storage: the VM imports each string literal once
//! per call ([`crate::bytecode::Op::Shared`]), so every record a literal
//! builds holds the same key strings. [`Fields`] gives other builders the
//! same sharing. `JSON.parse_as(raw, shape)` is its intended user: it can
//! import a shape's field names once per parse and key every object it
//! builds with them, instead of copying each key of each object.
//!
//! See `docs/vm.md` for the design.

use crate::checked::Checked;
use crate::{
    CallContext, Result, Value,
    budget::Buffer,
    types::{Type, TypeKind},
};

/// The field names of a shape type, imported once into a call so that the
/// records built with them share their keys.
pub(crate) struct Fields {
    /// Each field's key, in the type's order.
    keys: Buffer<Value>,
    inline: Option<([Value; 8], usize)>,
    imported: usize,
}

impl Fields {
    /// Imports the field names of `ty`, or returns `None` when it is not a
    /// shape type. The import charges each name as a string of its own.
    #[allow(dead_code)]
    pub(crate) fn of(ctx: &mut CallContext, ty: &Type) -> Result<Option<Self>> {
        let TypeKind::Shape(fields, _) = &ty.kind else {
            return Ok(None);
        };
        let mut keys = Buffer::with_capacity(ctx, fields.len())?;
        for field in fields {
            let key = ctx.bytes(&field.name)?;
            keys.data.push(key);
        }
        Ok(Some(Self {
            imported: keys.data.len(),
            keys,
            inline: None,
        }))
    }

    /// The shared key for the field named `name`, or `None` for a name the
    /// shape does not declare. Each field passed charges a step, as a hash
    /// lookup's comparison does.
    #[allow(dead_code)]
    pub(crate) fn key(&self, ctx: &mut CallContext, name: &[u8]) -> Result<Option<Value>> {
        for key in self.values() {
            ctx.charge(1)?;
            if key.require_bytes()? == name {
                return Ok(Some(key.clone()));
            }
        }
        Ok(None)
    }

    /// The number of fields.
    #[allow(dead_code)]
    pub(crate) fn len(&self) -> usize {
        self.inline
            .as_ref()
            .map_or(self.keys.data.len(), |(_, len)| *len)
    }

    /// Creates slots for lazily imported names. Importing at the original
    /// materialization point preserves the builder's work and failure order.
    pub(crate) fn lazy(ctx: &mut CallContext, count: usize) -> Result<Self> {
        if count <= 8 {
            return Ok(Self {
                keys: Buffer::empty(),
                inline: Some(([const { Value::nil() }; 8], count)),
                imported: 0,
            });
        }
        let mut keys = Buffer::with_capacity(ctx, count)?;
        keys.data.resize(count, Value::nil());
        Ok(Self {
            keys,
            inline: None,
            imported: 0,
        })
    }

    /// Returns an already imported name by its position in the shape.
    pub(crate) fn get(&self, index: usize) -> Option<&Value> {
        let key = match &self.inline {
            Some((keys, len)) => keys.get(index).filter(|_| index < *len)?,
            None => self.keys.data.get(index)?,
        };
        key.as_bytes().map(|_| key)
    }

    /// Retains a name the builder imported with its own accounting.
    pub(crate) fn remember(&mut self, index: usize, key: &Value) -> crate::checked::Access<()> {
        let slot = match &mut self.inline {
            Some((keys, len)) => keys.span_mut(0, *len)?.at_mut(index)?,
            None => self.keys.at_mut(index)?,
        };
        self.imported += usize::from(slot.as_bytes().is_none());
        *slot = key.clone();
        Ok(())
    }

    /// Releases a cached name that no completed record still owns.
    pub(crate) fn forget(&mut self, index: usize) {
        let slot = match &mut self.inline {
            Some((keys, len)) => keys.get_mut(..*len).and_then(|keys| keys.get_mut(index)),
            None => self.keys.data.get_mut(index),
        };
        if let Some(slot) = slot {
            self.imported -= usize::from(slot.as_bytes().is_some());
            *slot = Value::nil();
        }
    }

    /// Whether every declared name has been imported.
    pub(crate) fn complete(&self) -> bool {
        self.imported == self.len()
    }

    fn values(&self) -> impl Iterator<Item = &Value> {
        self.keys.data.iter().chain(
            self.inline
                .iter()
                .flat_map(|(keys, len)| keys.iter().take(*len)),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CallOptions, value::Kind};

    #[test]
    fn absent_fields_do_not_change_the_imported_count() {
        let mut ctx = CallContext::new(CallOptions::default());
        let key = Value::bytes(b"name");
        for count in [0, 1, 8, 9, 20] {
            let mut fields = Fields::lazy(&mut ctx, count).unwrap();
            for index in [count, usize::MAX] {
                assert!(fields.get(index).is_none());
                let error = crate::Error::from(fields.remember(index, &key).unwrap_err());
                assert_eq!(error.kind, crate::ErrorKind::Internal);
                fields.forget(index);
                assert_eq!(fields.imported, 0);
            }
            if count > 0 {
                fields.remember(0, &key).unwrap();
                fields.remember(0, &key).unwrap();
                assert_eq!(fields.imported, 1);
                fields.forget(0);
                fields.forget(0);
                assert_eq!(fields.imported, 0);
            }
        }
    }

    fn shape(names: &[&str]) -> Type {
        Type {
            name: String::new(),
            kind: TypeKind::Shape(
                names
                    .iter()
                    .map(|name| crate::types::Field {
                        name: name.as_bytes().to_vec(),
                        ty: Type::named("int".into()),
                        optional: false,
                    })
                    .collect(),
                false,
            ),
            nullable: false,
        }
    }

    #[test]
    fn records_built_with_fields_share_their_keys() {
        let mut ctx = CallContext::new(CallOptions::default());
        let fields = Fields::of(&mut ctx, &shape(&["id", "name"]))
            .unwrap()
            .unwrap();
        assert_eq!(fields.len(), 2);
        let first = fields.key(&mut ctx, b"name").unwrap().unwrap();
        let second = fields.key(&mut ctx, b"name").unwrap().unwrap();
        let (Kind::Bytes(a), Kind::Bytes(b)) = (&first.0, &second.0) else {
            panic!("keys are strings");
        };
        assert!(a.ptr_eq(b));
        assert!(fields.key(&mut ctx, b"missing").unwrap().is_none());
        assert!(
            Fields::of(&mut ctx, &Type::named("int".into()))
                .unwrap()
                .is_none()
        );
    }
}
