use super::*;
use crate::{checking::facts::Field, hash::Tag};

impl Walker<'_> {
    fn text_scalar_fact(&mut self, value: &Value) -> Result<Fact> {
        self.ctx.charge(1)?;
        match &value.0 {
            Kind::Nil => Ok(Atom::Nil.fact()),
            Kind::Int(n) => self.facts.integer(self.ctx, *n),
            Kind::Bytes(bytes) => self.facts.string(self.ctx, &bytes.data),
            _ => unreachable!("text iterators yield only strings, integers and capture data"),
        }
    }

    fn text_list_fact(&mut self, values: &[Value]) -> Result<Fact> {
        let mut items = Buffer::with_capacity(self.ctx, values.len())?;
        for value in values {
            let fact = self.text_scalar_fact(value)?;
            items.push(self.ctx, fact)?;
        }
        self.facts.tuple(self.ctx, &items.data)
    }

    fn text_named_fact(&mut self, entries: &[(Value, Value)]) -> Result<Fact> {
        let mut fields = Buffer::with_capacity(self.ctx, entries.len())?;
        for (name, value) in entries {
            let value = self.text_scalar_fact(value)?;
            fields.push(
                self.ctx,
                Field {
                    name: name.clone(),
                    value,
                    optional: false,
                },
            )?;
        }
        self.facts
            .shape_fields(self.ctx, fields, false, Atom::String.fact(), true)
    }

    pub(super) fn text_yield_fact(&mut self, value: &Value) -> Result<Fact> {
        self.ctx.charge(1)?;
        match &value.0 {
            Kind::Array(array) => self.text_list_fact(&array.buffer.data),
            Kind::Hash(hash) => {
                assert_eq!(hash.tag, Tag::Match);
                let captures = hash.find(self.ctx, b"captures")?.unwrap();
                let count = hash.buffer.data[captures].1.as_array().unwrap().len() + 1;
                let mut fields = Buffer::with_capacity(self.ctx, hash.buffer.data.len())?;
                for (name, value) in &hash.buffer.data {
                    self.ctx.charge(1)?;
                    let value = match &value.0 {
                        Kind::Offset(offset) => {
                            let mut values = Buffer::with_capacity(self.ctx, count)?;
                            for index in 0..count {
                                let value = offset.call(
                                    self.ctx,
                                    &[Value::int(index as i64)],
                                    &[],
                                    false,
                                )?;
                                let value = self.text_scalar_fact(&value)?;
                                values.push(self.ctx, value)?;
                            }
                            let values = self.facts.tuple(self.ctx, &values.data)?;
                            self.facts.offset(self.ctx, values)?
                        }
                        Kind::Array(array) => self.text_list_fact(&array.buffer.data)?,
                        Kind::Hash(named) => self.text_named_fact(&named.buffer.data)?,
                        _ => self.text_scalar_fact(value)?,
                    };
                    fields.push(
                        self.ctx,
                        Field {
                            name: name.clone(),
                            value,
                            optional: false,
                        },
                    )?;
                }
                let shape =
                    self.facts
                        .shape_fields(self.ctx, fields, false, Atom::String.fact(), true)?;
                self.facts.protected(self.ctx, shape, Tag::Match)
            }
            _ => self.text_scalar_fact(value),
        }
    }

    pub(super) fn text_scan_fact(&mut self, value: &Value) -> Result<Fact> {
        let values = value.as_array().unwrap();
        let mut items = Buffer::with_capacity(self.ctx, values.len())?;
        for value in values {
            let fact = self.text_yield_fact(value)?;
            items.push(self.ctx, fact)?;
        }
        self.facts.tuple(self.ctx, &items.data)
    }
}
