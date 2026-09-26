use crate::{
    Value,
    types::{Scalar, Type, TypeKind},
    value::Kind,
};

/// A successful, allocation-free check and its ordinary normalization charges.
/// Unsupported schemas and mismatches use the full normalizer after parsing,
/// preserving syntax-error precedence, enum conversion and diagnostic paths.
pub(super) fn check(
    ty: &Type,
    value: &Value,
    depth: usize,
    saved: Option<(&Type, u64)>,
) -> Option<u64> {
    visit(ty, value, depth, saved, &mut 4096)
}

fn visit(
    ty: &Type,
    value: &Value,
    depth: usize,
    saved: Option<(&Type, u64)>,
    fuel: &mut usize,
) -> Option<u64> {
    *fuel = fuel.checked_sub(1)?;
    if depth >= 64 {
        return None;
    }
    if let Some((known, steps)) = saved {
        if std::ptr::eq(known, ty) && depth <= 1 {
            return Some(steps);
        }
    }
    if ty.nullable && matches!(value.0, Kind::Nil) {
        return Some(1);
    }
    let mut steps = 1;
    match &ty.kind {
        TypeKind::Scalar(scalar) => {
            let valid = match scalar {
                Scalar::Any => true,
                Scalar::Int => matches!(value.0, Kind::Int(_) | Kind::Big(_)),
                Scalar::Float => matches!(value.0, Kind::Float(_)),
                Scalar::Number => matches!(value.0, Kind::Int(_) | Kind::Big(_) | Kind::Float(_)),
                Scalar::String => matches!(value.0, Kind::Bytes(_)),
                Scalar::Bool => matches!(value.0, Kind::Bool(_)),
                Scalar::Nil => matches!(value.0, Kind::Nil),
                _ => false,
            };
            if !valid {
                return None;
            }
        }
        TypeKind::Array(element) => {
            let items = value.as_array()?;
            if let Some(element) = element {
                for item in items {
                    steps += visit(element, item, depth + 1, None, fuel)?;
                }
            }
        }
        TypeKind::Shape(fields, open) => {
            let Kind::Hash(hash) = &value.0 else {
                return None;
            };
            let entries = &hash.buffer.data;
            // Small records use linear hash lookup. Keep large/indexed hashes
            // on the normalizer, including its collision accounting.
            if entries.len() >= 16
                || fields.len() > 16
                || (!open && entries.len() > fields.len())
                || fields.iter().any(|f| f.name.len() > 64)
                || entries
                    .iter()
                    .any(|(k, _)| k.as_bytes().is_none_or(|k| k.len() > 64))
            {
                return None;
            }
            for (key, item) in entries {
                let key = key.as_bytes()?;
                let (mut lower, mut upper) = (0, fields.len());
                let mut field = None;
                while lower < upper {
                    *fuel = fuel.checked_sub(1)?;
                    steps += 1;
                    let middle = lower + (upper - lower) / 2;
                    let name = &fields[middle].name;
                    steps += u64::from(!name.is_empty() && !key.is_empty());
                    match name.as_slice().cmp(key) {
                        std::cmp::Ordering::Less => lower = middle + 1,
                        std::cmp::Ordering::Greater => upper = middle,
                        std::cmp::Ordering::Equal => {
                            field = Some(&fields[middle]);
                            break;
                        }
                    }
                }
                if let Some(field) = field {
                    steps += visit(&field.ty, item, depth + 1, saved, fuel)?;
                } else if !open {
                    return None;
                }
            }
            for field in fields {
                steps += 1;
                if field.optional {
                    continue;
                }
                let mut found = false;
                for (key, _) in entries {
                    *fuel = fuel.checked_sub(1)?;
                    steps += 1;
                    let key = key.as_bytes()?;
                    if key.len() == field.name.len() {
                        steps += u64::from(!key.is_empty());
                        if key == field.name {
                            found = true;
                            break;
                        }
                    }
                }
                if !found {
                    return None;
                }
            }
        }
        _ => return None,
    }
    Some(steps)
}

#[derive(Default)]
pub(super) struct Stream<'a> {
    pub ty: Option<&'a Type>,
    array: Option<(&'a Type, usize)>,
    steps: Option<u64>,
    saved: Option<(&'a Type, u64)>,
}

impl<'a> Stream<'a> {
    /// Begins a root or direct shape-field value, invalidating duplicate fields.
    pub fn start(&mut self, depth: usize, key: Option<&[u8]>, array: bool) {
        let Some(mut ty) = self.ty else {
            return;
        };
        if depth == 1 {
            let (Some(key), TypeKind::Shape(fields, _)) = (key, &ty.kind) else {
                return;
            };
            let Ok(index) = fields.binary_search_by(|f| f.name.as_slice().cmp(key)) else {
                return;
            };
            ty = &fields[index].ty;
        } else if depth != 0 {
            return;
        }
        if self.saved.is_some_and(|(old, _)| std::ptr::eq(old, ty)) {
            self.saved = None;
        }
        if array && matches!(ty.kind, TypeKind::Array(Some(_))) {
            self.array = Some((ty, depth + 1));
            self.steps = Some(1);
        }
    }

    /// Checks each completed array item while its record is still hot.
    pub fn complete(&mut self, depth: usize, value: &Value) {
        let Some((ty, array_depth)) = self.array else {
            return;
        };
        if depth == array_depth {
            let TypeKind::Array(Some(element)) = &ty.kind else {
                unreachable!();
            };
            self.steps = self
                .steps
                .and_then(|steps| check(element, value, depth, None).map(|n| steps + n));
        } else if depth + 1 == array_depth {
            self.saved = self.steps.map(|steps| (ty, steps));
            self.array = None;
        }
    }

    /// Returns the proof only after the document has parsed successfully.
    pub fn finish(&self, value: &Value) -> Option<u64> {
        check(self.ty?, value, 0, self.saved)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        CallContext, CallOptions, Limits,
        types::{self, Field},
    };

    fn array(ty: Type) -> Type {
        Type {
            name: "array".into(),
            kind: TypeKind::Array(Some(Box::new(ty))),
            nullable: false,
        }
    }

    fn shape(fields: Vec<Field>, open: bool) -> Type {
        Type {
            name: String::new(),
            kind: TypeKind::Shape(fields, open),
            nullable: false,
        }
    }

    #[test]
    fn streaming_proofs_match_normalization_and_quota_boundaries() {
        let field = |name: &str, ty, optional| Field {
            name: name.as_bytes().to_vec(),
            ty,
            optional,
        };
        let record = shape(
            vec![
                field("", Type::named("string".into()), true),
                field("id", Type::named("int".into()), false),
                field("name", Type::named("string".into()), true),
            ],
            false,
        );
        let packet = shape(vec![field("rows", array(record.clone()), false)], false);
        let tests = [
            (
                array(record.clone()),
                r#"[{"id":1,"name":"ok"},{"id":2},{"id":"bad","id":3}]"#,
            ),
            (array(record.clone()), r#"[{"id":1},{"id":"bad"}]"#),
            (array(record.clone()), r#"[{"id":1},{"id":"bad"}] trailing"#),
            (array(record.clone()), r#"[{"id":1,"extra":2}]"#),
            (array(record.clone()), r#"[{"name":"missing"}]"#),
            (array(record.clone()), r#"[{"": "", "id":1}]"#),
            (array(record.clone()), "[]"),
            (packet.clone(), r#"{"rows":[{"id":1},{"id":2}]}"#),
            (
                packet.clone(),
                r#"{"rows":[{"id":"bad"}],"rows":[{"id":1}]}"#,
            ),
            (
                packet.clone(),
                r#"{"rows":[{"id":1}],"rows":[{"id":"bad"}]}"#,
            ),
            (packet.clone(), r#"{"rows":[{"id":1}],"rows":null}"#),
            (packet, r#"{"rows":[]}"#),
            (record, r#"{"id":1,"name":"ok"}"#),
        ];
        for (ty, text) in tests {
            let run = |streaming: bool, limit| {
                let mut ctx = CallContext::new(CallOptions {
                    limits: Limits {
                        steps: limit,
                        ..Limits::default()
                    },
                    ..CallOptions::default()
                });
                let result = (|| {
                    let (value, proof) = crate::json::parse_typed(
                        &mut ctx,
                        text.as_bytes(),
                        "JSON.parse_as",
                        streaming.then_some(&ty),
                    )?;
                    let prepared = types::prepare(&mut ctx, &ty, |_, _| unreachable!())?;
                    if let Some(steps) = proof {
                        ctx.charge_each(steps)?;
                        Ok(value)
                    } else {
                        prepared.normalize_with(&mut ctx, value, types::Context::Json)
                    }
                })();
                let stats = ctx.stats();
                let result = result
                    .map(|v| format!("{v:?}"))
                    .map_err(|e: crate::Error| (e.kind, e.message));
                assert_eq!(ctx.stats().retained_memory_bytes, 0);
                (
                    result,
                    stats.steps,
                    stats.peak_memory_bytes,
                    stats.retained_memory_bytes,
                )
            };
            let baseline = run(false, None);
            assert_eq!(run(true, None), baseline, "{text}");
            for limit in 0..=baseline.1 {
                assert_eq!(
                    run(true, Some(limit)),
                    run(false, Some(limit)),
                    "{text}: {limit}"
                );
            }
        }
    }
}
