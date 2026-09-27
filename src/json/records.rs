use crate::{
    CallContext, Result, Value,
    records::Fields,
    types::{Type, TypeKind},
};

struct Record<'a> {
    ty: &'a Type,
    first: usize,
    fields: Fields,
}

const FIELD_CHUNK: usize = 8;

#[derive(Clone, Copy)]
struct Child<'a> {
    parent: &'a Type,
    name: &'a [u8],
    ty: &'a Type,
    ready: bool,
}

/// Schema cursors follow normalization's depth guard; ambiguous types keep
/// the ordinary parser and its late normalization. Cursors never change the
/// order in which fields are inserted into the resulting ordinary hashes.
pub(super) struct Records<'a> {
    path: [Option<&'a Type>; 64],
    ready: [bool; 64],
    children: [Option<Child<'a>>; 4],
    names: u64,
    records: [Option<Record<'a>>; 16],
}

impl Default for Records<'_> {
    fn default() -> Self {
        Self {
            path: [None; 64],
            ready: [true; 64],
            children: [None; 4],
            names: 0,
            records: [const { None }; 16],
        }
    }
}

impl<'a> Records<'a> {
    /// Selects the declared type of a container at its source position.
    #[inline]
    pub fn start(
        &mut self,
        root: Option<&'a Type>,
        depth: usize,
        key: Option<&[u8]>,
        index: usize,
    ) {
        if depth >= self.path.len() {
            return;
        }
        if depth == 0 {
            self.path[depth] = root;
            self.ready[depth] = true;
            return;
        }
        let mut child = None;
        let ty = self.path[depth - 1].and_then(|parent| match &parent.kind {
            TypeKind::Array(element) if key.is_none() => element.as_deref(),
            TypeKind::Tuple(elements) if key.is_none() => elements.get(index),
            TypeKind::Hash(Some(pair)) if key.is_some() => Some(&pair.1),
            TypeKind::Shape(fields, _) => {
                let key = key?;
                let slot = (usize::from(key.first().copied().unwrap_or_default()) ^ key.len()) & 3;
                if let Some(cached) = self.children[slot]
                    .filter(|cached| std::ptr::eq(cached.parent, parent) && cached.name == key)
                {
                    child = Some((slot, cached));
                    return Some(cached.ty);
                }
                let index = fields
                    .binary_search_by(|field| field.name.as_slice().cmp(key))
                    .ok()?;
                let field = &fields[index];
                child = Some((
                    slot,
                    Child {
                        parent,
                        name: &field.name,
                        ty: &field.ty,
                        ready: false,
                    },
                ));
                Some(&field.ty)
            }
            _ => None,
        });
        if match (self.path[depth], ty) {
            (Some(old), Some(new)) => std::ptr::eq(old, new),
            (None, None) => true,
            _ => false,
        } {
            return;
        }
        self.path[depth] = ty;
        self.ready[depth] = child.is_some_and(|(_, cached)| cached.ready)
            || ty.is_none_or(|ty| !matches!(ty.kind, TypeKind::Shape(..)) || self.complete(ty));
        if let Some((slot, mut cached)) = child {
            cached.ready = self.ready[depth];
            self.children[slot] = Some(cached);
        }
    }

    /// Whether cache hits already refer to imported names of this shape.
    pub fn ready(&self, depth: usize) -> bool {
        self.ready.get(depth).copied().unwrap_or(true)
    }

    fn entries(&self) -> impl Iterator<Item = &Record<'a>> {
        self.records.iter().map_while(Option::as_ref)
    }

    fn complete(&self, ty: &Type) -> bool {
        let TypeKind::Shape(fields, _) = &ty.kind else {
            return true;
        };
        self.entries()
            .filter(|record| std::ptr::eq(record.ty, ty) && record.fields.complete())
            .map(|record| record.fields.len())
            .sum::<usize>()
            == fields.len()
    }

    /// The exact declared count for a required, closed shape.
    pub fn capacity(&self, depth: usize) -> Option<usize> {
        let ty = self.path.get(depth).copied().flatten()?;
        match &ty.kind {
            TypeKind::Shape(fields, false) if !fields.iter().any(|field| field.optional) => {
                Some(fields.len())
            }
            _ => None,
        }
    }

    /// Locates a declared name, creating its shape's lazy table on first use.
    pub fn slot(
        &mut self,
        ctx: &mut CallContext,
        depth: usize,
        name: &[u8],
    ) -> Result<Option<(usize, usize)>> {
        // The root is built only once. Its ordinary keys already provide all
        // required ownership; a sharing table there could never be reused.
        if depth == 0 {
            return Ok(None);
        }
        let Some(ty) = self.path.get(depth).copied().flatten() else {
            return Ok(None);
        };
        let TypeKind::Shape(fields, _) = &ty.kind else {
            return Ok(None);
        };
        let Ok(field) = fields.binary_search_by(|field| field.name.as_slice().cmp(name)) else {
            return Ok(None);
        };
        let first = field / FIELD_CHUNK * FIELD_CHUNK;
        for (index, record) in self.records.iter_mut().enumerate() {
            if let Some(record) = record {
                if std::ptr::eq(record.ty, ty) && record.first == first {
                    return Ok(Some((index, field - first)));
                }
            } else {
                *record = Some(Record {
                    ty,
                    first,
                    fields: Fields::lazy(ctx, (fields.len() - first).min(FIELD_CHUNK))?,
                });
                return Ok(Some((index, field - first)));
            }
        }
        // Sharing is an optimization: never allocate a table just to remember
        // names the output already owns. The ordinary key cache handles overflow.
        Ok(None)
    }

    /// Returns a previously imported name.
    pub fn get(&self, slot: (usize, usize)) -> Option<&Value> {
        let record = self.records[slot.0].as_ref().unwrap();
        record.fields.get(slot.1)
    }

    /// Finds a name already imported through another shape.
    pub fn shared(&self, name: &[u8], checked: Option<usize>) -> Option<&Value> {
        if self.names & name_bit(name) == 0 {
            return None;
        }
        self.entries().enumerate().find_map(|(slot, record)| {
            if checked == Some(slot) {
                return None;
            }
            let TypeKind::Shape(fields, _) = &record.ty.kind else {
                unreachable!()
            };
            let index = fields
                .binary_search_by(|field| field.name.as_slice().cmp(name))
                .ok()?;
            let local = index.checked_sub(record.first)?;
            (local < record.fields.len())
                .then(|| record.fields.get(local))
                .flatten()
        })
    }

    /// Shares the builder's already charged key at its declared slot.
    pub fn remember(&mut self, depth: usize, slot: (usize, usize), key: &Value) {
        self.names |= name_bit(key.as_bytes().unwrap());
        let record = self.records[slot.0].as_mut().unwrap();
        record.fields.remember(slot.1, key);
        let ty = record.ty;
        self.ready[depth] = self.complete(ty);
    }
}

fn name_bit(name: &[u8]) -> u64 {
    let first = usize::from(name.first().copied().unwrap_or_default());
    let last = usize::from(name.last().copied().unwrap_or_default());
    1 << ((first + (name.len() & 63) * 13 + last * 7) & 63)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CallOptions, types::Field, value::Kind};

    fn padded(input: impl AsRef<[u8]>) -> Vec<u8> {
        let mut input = input.as_ref().to_vec();
        input.resize(
            input.len().max(super::super::parser::RECORD_MIN_BYTES),
            b' ',
        );
        input
    }

    fn shape(names: &[&str], open: bool) -> Type {
        Type {
            name: String::new(),
            nullable: false,
            kind: TypeKind::Shape(
                names
                    .iter()
                    .map(|name| Field {
                        name: name.as_bytes().to_vec(),
                        ty: Type::named("any".into()),
                        optional: false,
                    })
                    .collect(),
                open,
            ),
        }
    }

    #[test]
    fn field_tables_stop_at_128_names_without_tracked_allocations() {
        let names: Vec<_> = (0..257).map(|i| format!("field_{i:03}")).collect();
        let names: Vec<_> = names.iter().map(String::as_str).collect();
        let ty = Type {
            name: "array".into(),
            nullable: false,
            kind: TypeKind::Array(Some(Box::new(shape(&names, false)))),
        };
        let mut ctx = CallContext::new(CallOptions::default());
        let mut records = Records::default();
        records.start(Some(&ty), 0, None, 0);
        records.start(Some(&ty), 1, None, 0);
        for (index, name) in names.iter().enumerate() {
            let before = ctx.stats();
            let slot = records.slot(&mut ctx, 1, name.as_bytes()).unwrap();
            let after = ctx.stats();
            assert_eq!(before.steps, after.steps);
            assert_eq!(before.peak_memory_bytes, after.peak_memory_bytes);
            assert_eq!(before.retained_memory_bytes, after.retained_memory_bytes);
            assert_eq!(slot.is_some(), index < 128);
            if let Some(slot) = slot {
                let key = ctx.bytes(name.as_bytes()).unwrap();
                records.remember(1, slot, &key);
                assert_eq!(records.get(slot).unwrap().as_bytes(), key.as_bytes());
            }
        }
        drop(records);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }

    #[test]
    fn child_type_cache_distinguishes_the_same_name_in_different_shapes() {
        let nested = |names: &[&str]| Type {
            name: String::new(),
            nullable: false,
            kind: TypeKind::Shape(
                vec![crate::types::Field {
                    name: b"items".to_vec(),
                    ty: Type {
                        name: "array".into(),
                        nullable: false,
                        kind: TypeKind::Array(Some(Box::new(shape(names, false)))),
                    },
                    optional: false,
                }],
                false,
            ),
        };
        let ty = Type {
            name: String::new(),
            nullable: false,
            kind: TypeKind::Tuple(vec![nested(&["a"]), nested(&["b", "c"])]),
        };
        let input =
            padded(br#"[{"items":[{"a":1},{"a":2}]},{"items":[{"b":3,"c":4},{"c":5,"b":6}]}]"#);
        let mut ctx = CallContext::new(CallOptions::default());
        let (value, _) =
            crate::json::parse_typed(&mut ctx, &input, "JSON.parse_as", Some(&ty)).unwrap();
        for (index, parent) in value.as_array().unwrap().iter().enumerate() {
            for record in parent.as_hash().unwrap()[0].1.as_array().unwrap() {
                let Kind::Hash(hash) = &record.0 else {
                    panic!("ordinary hash")
                };
                assert_eq!(hash.buffer.data.capacity(), index + 1);
            }
        }
        drop(value);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }

    #[test]
    fn escaped_names_adopt_keys_cached_through_open_shape_extras() {
        let mut record = shape(&["id"], true);
        let TypeKind::Shape(fields, _) = &mut record.kind else {
            unreachable!()
        };
        fields[0].optional = true;
        let ty = Type {
            name: "array".into(),
            nullable: false,
            kind: TypeKind::Array(Some(Box::new(record))),
        };
        let input = format!(
            r#"[{{"x":{{"id":0}}}},{{"\u0069d":1}},{{"id":2}}]{}"#,
            " ".repeat(super::super::parser::RECORD_MIN_BYTES)
        );
        let mut ctx = CallContext::new(CallOptions::default());
        let (value, _) =
            crate::json::parse_typed(&mut ctx, input.as_bytes(), "JSON.parse_as", Some(&ty))
                .unwrap();
        let rows = value.as_array().unwrap();
        let keys = [
            &rows[0].as_hash().unwrap()[0].1.as_hash().unwrap()[0].0,
            &rows[1].as_hash().unwrap()[0].0,
            &rows[2].as_hash().unwrap()[0].0,
        ];
        for key in keys {
            assert!(std::ptr::eq(
                key.as_bytes().unwrap(),
                keys[0].as_bytes().unwrap()
            ));
        }
    }

    #[test]
    fn more_than_sixteen_shapes_fall_back_to_shared_cached_keys() {
        let types = (0..20)
            .map(|_| Type {
                name: "array".into(),
                nullable: false,
                kind: TypeKind::Array(Some(Box::new(shape(&["id"], false)))),
            })
            .collect();
        let ty = Type {
            name: String::new(),
            nullable: false,
            kind: TypeKind::Tuple(types),
        };
        let input = padded(format!("[{}]", [r#"[{"id":1},{"id":2}]"#; 20].join(",")));
        let mut ctx = CallContext::new(CallOptions::default());
        let (value, _) =
            crate::json::parse_typed(&mut ctx, &input, "JSON.parse_as", Some(&ty)).unwrap();
        let mut first = None;
        for array in value.as_array().unwrap() {
            for row in array.as_array().unwrap() {
                let key = row.as_hash().unwrap()[0].0.as_bytes().unwrap();
                assert!(std::ptr::eq(*first.get_or_insert(key), key));
            }
        }
        drop(value);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }

    #[test]
    fn wide_shapes_share_names_across_field_tables_and_key_cache_evictions() {
        for count in [8, 9, 16, 17, 65] {
            let names: Vec<_> = (0..count)
                .map(|index| format!("field_{index:03}"))
                .collect();
            let names: Vec<_> = names.iter().map(String::as_str).collect();
            let ty = Type {
                name: "array".into(),
                nullable: false,
                kind: TypeKind::Array(Some(Box::new(shape(&names, false)))),
            };
            let fields: Vec<_> = names
                .iter()
                .enumerate()
                .map(|(index, name)| format!("\"{name}\":{index}"))
                .collect();
            let input = padded(format!(
                "[{{{}}},{{{}}}]",
                fields.join(","),
                fields.iter().rev().cloned().collect::<Vec<_>>().join(",")
            ));
            let mut ctx = CallContext::new(CallOptions::default());
            let (value, _) =
                crate::json::parse_typed(&mut ctx, &input, "JSON.parse_as", Some(&ty)).unwrap();
            let rows = value.as_array().unwrap();
            for row in rows {
                let Kind::Hash(hash) = &row.0 else {
                    panic!("ordinary hash")
                };
                assert_eq!(hash.buffer.data.capacity(), count);
            }
            for (first, second) in rows[0]
                .as_hash()
                .unwrap()
                .iter()
                .zip(rows[1].as_hash().unwrap().iter().rev())
            {
                let (Kind::Bytes(first), Kind::Bytes(second)) = (&first.0.0, &second.0.0) else {
                    panic!("ordinary keys")
                };
                assert!(std::sync::Arc::ptr_eq(first, second));
            }
            let mut ordinary = CallContext::new(CallOptions::default());
            let expected = crate::json::parse(&mut ordinary, &input).unwrap();
            assert_eq!(ctx.stats().steps, ordinary.stats().steps);
            assert_eq!(
                crate::json::stringify(&mut ctx, &value).unwrap().as_bytes(),
                crate::json::stringify(&mut ordinary, &expected)
                    .unwrap()
                    .as_bytes()
            );
            drop(value);
            assert_eq!(ctx.stats().retained_memory_bytes, 0);
        }
    }

    #[test]
    fn typed_records_share_names_keep_source_order_and_exact_capacity() {
        let record = shape(
            &[
                "a",
                "b",
                "long_name_over_sixty_four_bytes_abcdefghijklmnopqrstuvwxyz_0123456789",
            ],
            false,
        );
        let ty = Type {
            name: "array".into(),
            nullable: false,
            kind: TypeKind::Array(Some(Box::new(record))),
        };
        let input = padded(br#"[{"b":1,"a":2,"long_name_over_sixty_four_bytes_abcdefghijklmnopqrstuvwxyz_0123456789":3},{"\u0061":4,"b":5,"a":6,"long_name_over_sixty_four_bytes_abcdefghijklmnopqrstuvwxyz_0123456789":7}]"#);
        let mut ctx = CallContext::new(CallOptions::default());
        let (parsed, _) =
            crate::json::parse_typed(&mut ctx, &input, "JSON.parse_as", Some(&ty)).unwrap();
        let rows = parsed.as_array().unwrap();
        for row in rows {
            let Kind::Hash(hash) = &row.0 else {
                panic!("ordinary hash")
            };
            assert_eq!(hash.buffer.data.capacity(), 3);
            for (key, _) in &hash.buffer.data {
                let shared = rows[0]
                    .as_hash()
                    .unwrap()
                    .iter()
                    .find(|(other, _)| key.as_bytes() == other.as_bytes())
                    .unwrap();
                let (Kind::Bytes(key), Kind::Bytes(shared)) = (&key.0, &shared.0.0) else {
                    panic!("ordinary keys")
                };
                assert!(std::sync::Arc::ptr_eq(key, shared));
            }
        }
        let first = rows[0].as_hash().unwrap();
        let second = rows[1].as_hash().unwrap();
        assert_eq!(first[0].0.as_bytes(), Some(b"b".as_slice()));
        assert_eq!(second[0].0.as_bytes(), Some(b"a".as_slice()));
        assert_eq!(second[0].1.as_int(), Some(6));
        let mut ordinary = CallContext::new(CallOptions::default());
        let expected = crate::json::parse(&mut ordinary, &input).unwrap();
        assert_eq!(ctx.stats().steps, ordinary.stats().steps);
        assert_eq!(
            crate::json::stringify(&mut ctx, &parsed)
                .unwrap()
                .as_bytes(),
            crate::json::stringify(&mut ordinary, &expected)
                .unwrap()
                .as_bytes()
        );
        drop(parsed);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
}
