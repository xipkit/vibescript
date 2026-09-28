use crate::{
    CallContext, Error, ErrorKind, Result, Value, budget::Buffer, hash::Hash, value::Kind,
};

pub(crate) mod description;
mod diagnostics;
pub(crate) use diagnostics::Context;
pub(crate) use diagnostics::host_resolution;

/// Compares nominal binding spellings using the runtime's exact or folded lookup.
pub(crate) fn binding_name_matches(
    ctx: &mut CallContext,
    candidate: &[u8],
    binding: &[u8],
    fold: bool,
) -> Result<bool> {
    if fold {
        crate::text::case::equal(ctx, candidate, binding)
    } else {
        Ok(crate::enums::compare_names(ctx, candidate, binding)? == std::cmp::Ordering::Equal)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Scalar {
    Any,
    Int,
    Float,
    Number,
    String,
    Bool,
    Nil,
    Duration,
    Time,
    Money,
    Range,
    Symbol,
    Regex,
    /// A successful regex match.
    MatchData,
    /// What `rescue => error` binds.
    Error,
    /// A member of any enum, as a value from another script can be.
    EnumValue,
    /// Any enum itself.
    EnumType,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BuiltinName {
    Scalar(Scalar),
    Array,
    Hash,
    /// `type<T>`, a type literal.
    Type,
}

/// Classifies a builtin type name. The older names match in any case; the
/// names ADR-007 adds are lowercase only, so a class or enum spelled `Error`
/// or `Regex` keeps naming itself.
pub(crate) fn builtin_name(name: &str) -> Option<BuiltinName> {
    match name {
        "regex" => return Some(BuiltinName::Scalar(Scalar::Regex)),
        "match_data" => return Some(BuiltinName::Scalar(Scalar::MatchData)),
        "error" => return Some(BuiltinName::Scalar(Scalar::Error)),
        "enum_value" => return Some(BuiltinName::Scalar(Scalar::EnumValue)),
        "enum_type" => return Some(BuiltinName::Scalar(Scalar::EnumType)),
        "type" => return Some(BuiltinName::Type),
        _ => (),
    }
    let mut folded = [0; 8];
    let mut length = 0;
    for c in name.chars() {
        if length == folded.len() {
            return None;
        }
        let c = crate::casing::map(c, false);
        if !c.is_ascii() {
            return None;
        }
        folded[length] = c as u8;
        length += 1;
    }
    Some(match &folded[..length] {
        b"any" => BuiltinName::Scalar(Scalar::Any),
        b"int" => BuiltinName::Scalar(Scalar::Int),
        b"float" => BuiltinName::Scalar(Scalar::Float),
        b"number" => BuiltinName::Scalar(Scalar::Number),
        b"string" => BuiltinName::Scalar(Scalar::String),
        b"bool" => BuiltinName::Scalar(Scalar::Bool),
        b"nil" => BuiltinName::Scalar(Scalar::Nil),
        b"duration" => BuiltinName::Scalar(Scalar::Duration),
        b"time" => BuiltinName::Scalar(Scalar::Time),
        b"money" => BuiltinName::Scalar(Scalar::Money),
        b"range" => BuiltinName::Scalar(Scalar::Range),
        b"symbol" => BuiltinName::Scalar(Scalar::Symbol),
        b"array" => BuiltinName::Array,
        b"hash" | b"object" => BuiltinName::Hash,
        _ => return None,
    })
}

#[derive(Clone, Debug)]
pub(crate) struct Type {
    pub name: String,
    pub kind: TypeKind,
    pub nullable: bool,
}

#[derive(Clone, Debug)]
pub(crate) enum TypeKind {
    Scalar(Scalar),
    Array(Option<Box<Type>>),
    Hash(Option<Box<(Type, Type)>>),
    Shape(Vec<Field>, bool),
    Union(Vec<Type>),
    /// An array of exactly these elements, in order.
    Tuple(Vec<Type>),
    /// A type literal, such as `JSON.parse_as`'s schema, describing the type.
    Literal(Option<Box<Type>>),
    Named,
}

#[derive(Clone, Debug)]
pub(crate) struct Field {
    pub name: Vec<u8>,
    pub ty: Type,
    pub optional: bool,
}

impl Type {
    /// Admits nil, spelling a union's nil as one of its options.
    pub fn make_nullable(&mut self) {
        match &mut self.kind {
            TypeKind::Union(options) => {
                if !options.iter().any(|option| {
                    option.nullable || matches!(option.kind, TypeKind::Scalar(Scalar::Nil))
                }) {
                    options.push(Type {
                        name: "nil".into(),
                        kind: TypeKind::Scalar(Scalar::Nil),
                        nullable: false,
                    });
                }
            }
            TypeKind::Scalar(Scalar::Nil | Scalar::Any) => (),
            _ => self.nullable = true,
        }
    }

    /// The height of the type's tree.
    pub fn height(&self) -> usize {
        1 + match &self.kind {
            TypeKind::Array(Some(element)) | TypeKind::Literal(Some(element)) => element.height(),
            TypeKind::Hash(Some(pair)) => pair.0.height().max(pair.1.height()),
            TypeKind::Shape(fields, _) => fields
                .iter()
                .map(|field| field.ty.height())
                .max()
                .unwrap_or(0),
            TypeKind::Union(options) | TypeKind::Tuple(options) => {
                options.iter().map(Type::height).max().unwrap_or(0)
            }
            _ => 0,
        }
    }

    /// Whether a check against this type does more than the checker
    /// proves, so the runtime keeps it even between two well-typed parts of
    /// a program. A named type resolves at runtime and an enum turns a
    /// symbol into its member, and the checker admits hash key types that
    /// no string key satisfies, which only the runtime rejects.
    pub fn unproven(&self) -> bool {
        match &self.kind {
            TypeKind::Named => true,
            TypeKind::Scalar(_) | TypeKind::Literal(_) => false,
            TypeKind::Array(element) => element.as_ref().is_some_and(|element| element.unproven()),
            TypeKind::Hash(pair) => pair.as_ref().is_some_and(|pair| {
                pair.0.unproven() || !pair.0.admits_keys() || pair.1.unproven()
            }),
            TypeKind::Shape(fields, _) => fields.iter().any(|field| field.ty.unproven()),
            TypeKind::Union(options) | TypeKind::Tuple(options) => {
                options.iter().any(Type::unproven)
            }
        }
    }

    /// Whether hash keys, which are strings, satisfy this key type, as
    /// [`hash_keys`] decides it for a type without names.
    fn admits_keys(&self) -> bool {
        match &self.kind {
            TypeKind::Scalar(Scalar::Any | Scalar::String | Scalar::Symbol) => true,
            TypeKind::Union(options) => options.iter().any(Type::admits_keys),
            _ => false,
        }
    }

    /// The number of type nodes, which bounds what substituting it costs.
    pub fn nodes(&self) -> usize {
        1 + match &self.kind {
            TypeKind::Array(Some(element)) | TypeKind::Literal(Some(element)) => element.nodes(),
            TypeKind::Hash(Some(pair)) => pair.0.nodes() + pair.1.nodes(),
            TypeKind::Shape(fields, _) => fields.iter().map(|field| field.ty.nodes()).sum(),
            TypeKind::Union(options) | TypeKind::Tuple(options) => {
                options.iter().map(Type::nodes).sum()
            }
            _ => 0,
        }
    }
}

#[cfg(test)]
impl Type {
    pub fn named(name: String) -> Self {
        let kind = match builtin_name(&name) {
            Some(BuiltinName::Scalar(scalar)) => TypeKind::Scalar(scalar),
            Some(BuiltinName::Array) => TypeKind::Array(None),
            Some(BuiltinName::Hash) => TypeKind::Hash(None),
            Some(BuiltinName::Type) => TypeKind::Literal(None),
            None => TypeKind::Named,
        };
        Self {
            name,
            kind,
            nullable: false,
        }
    }
}

#[cfg(test)]
fn normalize(
    ctx: &mut CallContext,
    ty: &Type,
    value: Value,
    resolve: impl FnMut(&mut CallContext, &str) -> Result<Value>,
) -> Result<Value> {
    prepare(ctx, ty, resolve)?.normalize(ctx, value)
}

pub(crate) struct Prepared<'a> {
    ty: &'a Type,
    names: Buffer<(usize, Value)>,
}

impl Prepared<'_> {
    #[cfg(test)]
    pub fn normalize(&self, ctx: &mut CallContext, value: Value) -> Result<Value> {
        self.normalize_with(ctx, value, Context::Value)
    }

    pub fn normalize_with(
        &self,
        ctx: &mut CallContext,
        value: Value,
        context: Context<'_>,
    ) -> Result<Value> {
        let json = matches!(context, Context::Json);
        if let Some((value, _)) = visit(ctx, self.ty, value.clone(), &self.names, 0, json)? {
            return Ok(value);
        }
        Err(diagnostics::mismatch(ctx, self.ty, &value, context)?)
    }
}

pub(crate) fn prepare<'a>(
    ctx: &mut CallContext,
    ty: &'a Type,
    mut resolve: impl FnMut(&mut CallContext, &str) -> Result<Value>,
) -> Result<Prepared<'a>> {
    let mut names = Buffer::empty();
    resolve_names(ctx, ty, &mut names, &mut resolve, 0)?;
    Ok(Prepared { ty, names })
}

fn resolve_names(
    ctx: &mut CallContext,
    ty: &Type,
    names: &mut Buffer<(usize, Value)>,
    resolve: &mut impl FnMut(&mut CallContext, &str) -> Result<Value>,
    depth: usize,
) -> Result<()> {
    ctx.charge(1)?;
    // A union adds an AST level without increasing the parser's type depth.
    if depth > 130 {
        return ctx.guard(
            ErrorKind::Recursion,
            "type normalization exceeded maximum depth",
        );
    }
    match &ty.kind {
        TypeKind::Named => {
            let value = resolve(ctx, &ty.name)?;
            names.push(ctx, (std::ptr::from_ref(ty) as usize, value))?;
        }
        TypeKind::Array(Some(element)) => resolve_names(ctx, element, names, resolve, depth + 1)?,
        TypeKind::Hash(Some(pair)) => {
            resolve_names(ctx, &pair.0, names, resolve, depth + 1)?;
            resolve_names(ctx, &pair.1, names, resolve, depth + 1)?;
        }
        TypeKind::Shape(fields, _) => {
            for field in fields {
                resolve_names(ctx, &field.ty, names, resolve, depth + 1)?;
            }
        }
        TypeKind::Union(options) | TypeKind::Tuple(options) => {
            for option in options {
                resolve_names(ctx, option, names, resolve, depth + 1)?;
            }
        }
        _ => (),
    }
    Ok(())
}

/// Normalizes `value` against `ty`, returning the value and whether it
/// changed, or none on a mismatch. In `json`, a string names an enum member
/// as a symbol does, since JSON has no symbols.
fn visit(
    ctx: &mut CallContext,
    ty: &Type,
    value: Value,
    names: &Buffer<(usize, Value)>,
    depth: usize,
    json: bool,
) -> Result<Option<(Value, bool)>> {
    ctx.charge(1)?;
    if depth >= 64 {
        return ctx.guard(
            ErrorKind::Recursion,
            "type normalization exceeded maximum depth",
        );
    }
    if ty.nullable && matches!(value.0, Kind::Nil) {
        return Ok(Some((value, false)));
    }
    match &ty.kind {
        TypeKind::Scalar(scalar) => {
            let matches = match scalar {
                Scalar::Any => true,
                Scalar::Int => matches!(value.0, Kind::Int(_) | Kind::Big(_)),
                Scalar::Float => matches!(value.0, Kind::Float(_)),
                Scalar::Number => matches!(value.0, Kind::Int(_) | Kind::Big(_) | Kind::Float(_)),
                Scalar::String => matches!(value.0, Kind::Bytes(_)),
                Scalar::Symbol => matches!(value.0, Kind::Symbol(_)),
                Scalar::Bool => matches!(value.0, Kind::Bool(_)),
                Scalar::Nil => matches!(value.0, Kind::Nil),
                Scalar::Duration => matches!(value.0, Kind::Duration(_)),
                Scalar::Time => matches!(value.0, Kind::Time(_) | Kind::Zoned(_)),
                Scalar::Money => matches!(value.0, Kind::Money(_)),
                Scalar::Range => matches!(value.0, Kind::Range(_)),
                Scalar::Regex => matches!(value.0, Kind::Regex(_)),
                Scalar::MatchData => {
                    matches!(&value.0, Kind::Hash(hash) if hash.tag == crate::hash::Tag::Match)
                }
                Scalar::Error => {
                    matches!(&value.0, Kind::Hash(hash) if hash.tag == crate::hash::Tag::Error)
                }
                Scalar::EnumValue => matches!(value.0, Kind::EnumMember(_)),
                Scalar::EnumType => matches!(value.0, Kind::Enum(_)),
            };
            Ok(matches.then_some((value, false)))
        }
        TypeKind::Literal(_) => Ok(matches!(value.0, Kind::Shape(_)).then_some((value, false))),
        TypeKind::Tuple(elements) => {
            let Some(items) = value.as_array() else {
                return Ok(None);
            };
            if items.len() != elements.len() {
                return Ok(None);
            }
            let mut output = None;
            for (index, (item, element)) in items.iter().zip(elements).enumerate() {
                let Some((normalized, changed)) =
                    visit(ctx, element, item.clone(), names, depth + 1, json)?
                else {
                    return Ok(None);
                };
                if changed && output.is_none() {
                    let mut buffer = Buffer::with_capacity(ctx, items.len())?;
                    buffer.extend(ctx, &items[..index])?;
                    output = Some(buffer);
                }
                if let Some(output) = &mut output {
                    output.push(ctx, normalized)?;
                }
            }
            match output {
                Some(output) => Ok(Some((Value::from_array(ctx, output)?, true))),
                None => Ok(Some((value, false))),
            }
        }
        TypeKind::Named => {
            let id = std::ptr::from_ref(ty) as usize;
            let mut found = None;
            for (node, enumeration) in &names.data {
                ctx.charge(1)?;
                if *node == id {
                    found = Some(enumeration);
                    break;
                }
            }
            let found = found.unwrap();
            if let Kind::Namespace(class) = &found.0 {
                let matches = matches!(&value.0, Kind::Instance(instance) if instance.class().same_type(class));
                return Ok(matches.then_some((value, false)));
            }
            let Kind::Enum(enumeration) = &found.0 else {
                return Err(Error::new(
                    ErrorKind::Type,
                    "named annotation does not refer to a type",
                ));
            };
            match &value.0 {
                Kind::EnumMember(member)
                    if std::sync::Arc::ptr_eq(
                        &member.enumeration.definition,
                        &enumeration.definition,
                    ) =>
                {
                    Ok(Some((value, false)))
                }
                Kind::Symbol(name) | Kind::Bytes(name)
                    if json || matches!(value.0, Kind::Symbol(_)) =>
                {
                    if let Some(index) = enumeration.lookup_symbol(ctx, &name.data)? {
                        let value = crate::enums::Member::new(ctx, enumeration.clone(), index)?;
                        Ok(Some((Value(Kind::EnumMember(value)), true)))
                    } else {
                        Ok(None)
                    }
                }
                _ => Ok(None),
            }
        }
        TypeKind::Union(options) => {
            for any in [false, true] {
                for option in options {
                    ctx.charge(1)?;
                    if matches!(option.kind, TypeKind::Scalar(Scalar::Any)) != any {
                        continue;
                    }
                    if let Some(result) = visit(ctx, option, value.clone(), names, depth + 1, json)?
                    {
                        return Ok(Some(result));
                    }
                }
            }
            Ok(None)
        }
        TypeKind::Array(element) => {
            let Some(items) = value.as_array() else {
                return Ok(None);
            };
            let Some(element) = element else {
                return Ok(Some((value, false)));
            };
            let mut output = None;
            for (index, item) in items.iter().enumerate() {
                let Some((normalized, changed)) =
                    visit(ctx, element, item.clone(), names, depth + 1, json)?
                else {
                    return Ok(None);
                };
                if changed && output.is_none() {
                    let mut buffer = Buffer::with_capacity(ctx, items.len())?;
                    buffer.extend(ctx, &items[..index])?;
                    output = Some(buffer);
                }
                if let Some(output) = &mut output {
                    output.push(ctx, normalized)?;
                }
            }
            match output {
                Some(output) => Ok(Some((Value::from_array(ctx, output)?, true))),
                None => Ok(Some((value, false))),
            }
        }
        TypeKind::Hash(pair) => {
            let Kind::Hash(hash) = &value.0 else {
                return Ok(None);
            };
            let Some(pair) = pair else {
                return Ok(Some((value, false)));
            };
            match hash_keys(ctx, &pair.0)? {
                Some(false) => return Ok(None),
                Some(true) => (),
                None => {
                    for (key, _) in &hash.buffer.data {
                        if visit(ctx, &pair.0, key.clone(), names, depth + 1, json)?.is_none() {
                            return Ok(None);
                        }
                    }
                }
            }
            let mut output = None;
            for (index, (key, item)) in hash.buffer.data.iter().enumerate() {
                let Some((normalized, changed)) =
                    visit(ctx, &pair.1, item.clone(), names, depth + 1, json)?
                else {
                    return Ok(None);
                };
                append_hash(ctx, hash, &mut output, index, key, normalized, changed)?;
            }
            finish_hash(ctx, value, output)
        }
        TypeKind::Shape(fields, open) => {
            let Kind::Hash(hash) = &value.0 else {
                return Ok(None);
            };
            if !open && hash.buffer.data.len() > fields.len() {
                return Ok(None);
            }
            let mut output: Option<Buffer<(Value, Value)>> = None;
            for (index, (key, item)) in hash.buffer.data.iter().enumerate() {
                let Some(field) = shape_field(ctx, fields, key.require_bytes()?)? else {
                    if !open {
                        return Ok(None);
                    }
                    if let Some(output) = &mut output {
                        output.push(ctx, (key.clone(), item.clone()))?;
                    }
                    continue;
                };
                let Some((normalized, changed)) =
                    visit(ctx, &field.ty, item.clone(), names, depth + 1, json)?
                else {
                    return Ok(None);
                };
                append_hash(ctx, hash, &mut output, index, key, normalized, changed)?;
            }
            for field in fields {
                ctx.charge(1)?;
                if !field.optional && hash.find(ctx, &field.name)?.is_none() {
                    return Ok(None);
                }
            }
            finish_hash(ctx, value, output)
        }
    }
}

fn hash_keys(ctx: &mut CallContext, ty: &Type) -> Result<Option<bool>> {
    ctx.charge(1)?;
    Ok(match &ty.kind {
        TypeKind::Named => None,
        TypeKind::Scalar(Scalar::Any | Scalar::String | Scalar::Symbol) => Some(true),
        TypeKind::Union(options) => {
            let mut matches = false;
            for option in options {
                let Some(allowed) = hash_keys(ctx, option)? else {
                    return Ok(None);
                };
                matches |= allowed;
            }
            Some(matches)
        }
        _ => Some(false),
    })
}

fn shape_field<'a>(
    ctx: &mut CallContext,
    fields: &'a [Field],
    key: &[u8],
) -> Result<Option<&'a Field>> {
    let mut lower = 0;
    let mut upper = fields.len();
    while lower < upper {
        ctx.charge(1)?;
        let middle = lower + (upper - lower) / 2;
        match crate::enums::compare_names(ctx, &fields[middle].name, key)? {
            std::cmp::Ordering::Less => lower = middle + 1,
            std::cmp::Ordering::Greater => upper = middle,
            std::cmp::Ordering::Equal => return Ok(Some(&fields[middle])),
        }
    }
    Ok(None)
}

fn append_hash(
    ctx: &mut CallContext,
    source: &Hash,
    output: &mut Option<Buffer<(Value, Value)>>,
    index: usize,
    key: &Value,
    value: Value,
    changed: bool,
) -> Result<()> {
    if changed && output.is_none() {
        let mut buffer = Buffer::with_capacity(ctx, source.buffer.data.len())?;
        buffer.extend(ctx, &source.buffer.data[..index])?;
        *output = Some(buffer);
    }
    if let Some(output) = output {
        output.push(ctx, (key.clone(), value))?;
    }
    Ok(())
}

fn finish_hash(
    ctx: &mut CallContext,
    original: Value,
    output: Option<Buffer<(Value, Value)>>,
) -> Result<Option<(Value, bool)>> {
    let Some(output) = output else {
        return Ok(Some((original, false)));
    };
    let Kind::Hash(source) = &original.0 else {
        unreachable!()
    };
    let mut hash = Hash::from_entries(ctx, output)?;
    hash.object = source.object;
    Ok(Some((Value::from_hash(ctx, hash)?, true)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CallOptions, Limits};
    use std::sync::Arc;

    fn array(element: Type) -> Type {
        Type {
            name: "array".into(),
            kind: TypeKind::Array(Some(Box::new(element))),
            nullable: false,
        }
    }

    fn union(options: Vec<Type>) -> Type {
        Type {
            name: String::new(),
            kind: TypeKind::Union(options),
            nullable: false,
        }
    }

    fn enum_type(ctx: &mut CallContext) -> Value {
        ctx.import(&crate::enums::compile("Status".into(), vec!["Draft".into()], &()).unwrap())
            .unwrap()
    }

    fn same_storage(left: &Value, right: &Value) -> bool {
        match (&left.0, &right.0) {
            (Kind::Array(a), Kind::Array(b)) => Arc::ptr_eq(a, b),
            (Kind::Hash(a), Kind::Hash(b)) => Arc::ptr_eq(a, b),
            (Kind::EnumMember(a), Kind::EnumMember(b)) => Arc::ptr_eq(a, b),
            _ => panic!("expected heap values"),
        }
    }

    #[test]
    fn unchanged_collections_reuse_storage_without_allocating() {
        let mut ctx = CallContext::new(CallOptions::default());
        let items = ctx.array(&[Value::int(1), Value::int(2)]).unwrap();
        let hash = ctx
            .import(&Value::hash(vec![(b"x".to_vec(), Value::int(1))]))
            .unwrap();
        let hash_type = Type {
            name: "hash".into(),
            kind: TypeKind::Hash(Some(Box::new((
                Type::named("string".into()),
                Type::named("int".into()),
            )))),
            nullable: false,
        };
        let shape = Type {
            name: String::new(),
            kind: TypeKind::Shape(
                vec![Field {
                    name: b"x".to_vec(),
                    ty: Type::named("int".into()),
                    optional: false,
                }],
                false,
            ),
            nullable: false,
        };
        let before = ctx.stats();
        for (ty, value) in [
            (array(Type::named("int".into())), &items),
            (hash_type, &hash),
            (shape, &hash),
        ] {
            let output = normalize(&mut ctx, &ty, value.clone(), |_, _| unreachable!()).unwrap();
            assert!(same_storage(value, &output));
            assert_eq!(ctx.stats().peak_memory_bytes, before.peak_memory_bytes);
            assert_eq!(
                ctx.stats().retained_memory_bytes,
                before.retained_memory_bytes
            );
        }
        drop(items);
        drop(hash);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }

    #[test]
    fn coercion_copies_only_changed_collections_and_preserves_siblings() {
        let mut ctx = CallContext::new(CallOptions::default());
        let enumeration = enum_type(&mut ctx);
        let named = Type::named("Status".into());
        let member = normalize(
            &mut ctx,
            &named,
            Value::symbol(b"draft".to_vec()),
            |_, _| Ok(enumeration.clone()),
        )
        .unwrap();
        let source = ctx
            .array(&[
                Value::int(1),
                member.clone(),
                Value::symbol(b"draft".to_vec()),
            ])
            .unwrap();
        let ty = array(union(vec![Type::named("int".into()), named.clone()]));
        let before = ctx.stats().retained_memory_bytes;
        let output = normalize(
            &mut ctx,
            &ty,
            source.clone(),
            |_, _| Ok(enumeration.clone()),
        )
        .unwrap();
        assert!(!same_storage(&source, &output));
        let values = output.as_array().unwrap();
        assert_eq!(values[0].as_int(), Some(1));
        assert!(same_storage(&source.as_array().unwrap()[1], &values[1]));
        assert_eq!(
            values[2].as_enum_member(),
            Some(("Status", "Draft", "draft"))
        );
        assert_eq!(source.as_array().unwrap()[2].type_name(), "symbol");
        drop(output);
        assert_eq!(ctx.stats().retained_memory_bytes, before);

        let extra = Value::array(vec![Value::int(1)]);
        let source = ctx
            .import(&Value::hash(vec![
                (b"before".to_vec(), extra.clone()),
                (b"state".to_vec(), Value::symbol(b"draft".to_vec())),
                (b"after".to_vec(), extra),
            ]))
            .unwrap();
        let ty = Type {
            name: String::new(),
            kind: TypeKind::Shape(
                vec![Field {
                    name: b"state".to_vec(),
                    ty: named,
                    optional: false,
                }],
                true,
            ),
            nullable: false,
        };
        let before = ctx.stats().retained_memory_bytes;
        let output = normalize(
            &mut ctx,
            &ty,
            source.clone(),
            |_, _| Ok(enumeration.clone()),
        )
        .unwrap();
        let (Kind::Hash(original), Kind::Hash(changed)) = (&source.0, &output.0) else {
            unreachable!()
        };
        assert!(!Arc::ptr_eq(original, changed));
        for index in [0, 2] {
            assert!(same_storage(
                &original.buffer.data[index].1,
                &changed.buffer.data[index].1
            ));
            assert_eq!(
                original.buffer.data[index].0.as_bytes(),
                changed.buffer.data[index].0.as_bytes()
            );
        }
        assert_eq!(
            changed.buffer.data[1].1.as_enum_member(),
            Some(("Status", "Draft", "draft"))
        );
        assert_eq!(original.buffer.data[1].1.type_name(), "symbol");
        drop(output);
        assert_eq!(ctx.stats().retained_memory_bytes, before);
    }

    #[test]
    fn allocation_failures_are_preflighted_latched_and_not_swallowed_by_unions() {
        let mut ctx = CallContext::new(CallOptions::default());
        let enumeration = enum_type(&mut ctx);
        let source = ctx
            .array(&vec![Value::symbol(b"draft".to_vec()); 128])
            .unwrap();
        let before = ctx.stats().retained_memory_bytes;
        let limit = before + 512;
        ctx.options.limits.memory_bytes = Some(limit);
        let ty = union(vec![
            array(Type::named("Status".into())),
            Type::named("any".into()),
        ]);
        assert_eq!(
            normalize(
                &mut ctx,
                &ty,
                source.clone(),
                |_, _| Ok(enumeration.clone())
            )
            .unwrap_err()
            .kind,
            ErrorKind::Memory
        );
        assert!(ctx.stats().peak_memory_bytes <= limit);
        assert_eq!(ctx.stats().retained_memory_bytes, before);
        assert_eq!(ctx.checkpoint().unwrap_err().kind, ErrorKind::Memory);
        assert!(
            source
                .as_array()
                .unwrap()
                .iter()
                .all(|x| x.type_name() == "symbol")
        );
        drop(source);
        drop(enumeration);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }

    #[test]
    fn scans_and_resolution_observe_work_cancellation_and_deadlines() {
        for kind in [ErrorKind::Steps, ErrorKind::Cancelled, ErrorKind::Deadline] {
            let mut ctx = CallContext::new(CallOptions::default());
            let value = ctx.array(&vec![Value::int(1); 4096]).unwrap();
            let before = ctx.stats();
            match kind {
                ErrorKind::Steps => ctx.options.limits.steps = Some(before.steps + 40),
                ErrorKind::Cancelled => ctx.options.cancellation.cancel(),
                ErrorKind::Deadline => ctx.options.deadline = Some(std::time::Instant::now()),
                _ => unreachable!(),
            }
            let ty = union(vec![
                array(Type::named("int".into())),
                Type::named("any".into()),
            ]);
            assert_eq!(
                normalize(&mut ctx, &ty, value.clone(), |_, _| unreachable!())
                    .unwrap_err()
                    .kind,
                kind
            );
            assert!(ctx.stats().steps - before.steps <= 41);
            assert_eq!(
                ctx.stats().retained_memory_bytes,
                before.retained_memory_bytes
            );
            assert_eq!(ctx.checkpoint().unwrap_err().kind, kind);
        }
        let mut ctx = CallContext::new(CallOptions {
            limits: Limits {
                steps: Some(20),
                ..Limits::default()
            },
            ..CallOptions::default()
        });
        let ty = union(vec![Type::named("int".into()); 1000]);
        assert_eq!(
            normalize(&mut ctx, &ty, Value::int(1), |_, _| unreachable!())
                .unwrap_err()
                .kind,
            ErrorKind::Steps
        );
        assert_eq!(ctx.stats().retained_memory_bytes, 0);

        let mut ctx = CallContext::new(CallOptions::default());
        let ty = union(vec![
            Type::named("any".into()),
            Type::named("Missing".into()),
        ]);
        let error = normalize(&mut ctx, &ty, Value::nil(), |_, _| {
            Err(Error::new(ErrorKind::Type, "unknown type"))
        })
        .unwrap_err();
        assert_eq!(error.message, "unknown type");
    }

    #[test]
    fn normalization_depth_counts_visited_values_and_union_arms() {
        let mut ty = Type::named("int".into());
        let mut value = Value::int(1);
        for _ in 0..32 {
            ty = array(union(vec![ty, Type::named("string".into())]));
            value = Value::array(vec![value]);
        }
        let mut ctx = CallContext::new(CallOptions::default());
        let empty = normalize(&mut ctx, &ty, Value::array(vec![]), |_, _| unreachable!()).unwrap();
        assert!(empty.as_array().unwrap().is_empty());
        assert_eq!(
            normalize(&mut ctx, &ty, value, |_, _| unreachable!())
                .unwrap_err()
                .kind,
            ErrorKind::Recursion
        );
        ctx.charge(1).unwrap();
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }

    #[test]
    fn long_shape_keys_and_enum_symbols_check_work_in_bounded_chunks() {
        let name = "a".repeat(32768);
        let mut ctx = CallContext::new(CallOptions::default());
        let value = ctx
            .import(&Value::hash(vec![(
                name.as_bytes().to_vec(),
                Value::int(1),
            )]))
            .unwrap();
        let before = ctx.stats();
        ctx.options.limits.steps = Some(before.steps + 20);
        let ty = Type {
            name: String::new(),
            kind: TypeKind::Shape(
                vec![Field {
                    name: name.as_bytes().to_vec(),
                    ty: Type::named("int".into()),
                    optional: false,
                }],
                false,
            ),
            nullable: false,
        };
        assert_eq!(
            normalize(&mut ctx, &ty, value, |_, _| unreachable!())
                .unwrap_err()
                .kind,
            ErrorKind::Steps
        );
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
        assert_eq!(ctx.checkpoint().unwrap_err().kind, ErrorKind::Steps);

        let mut ctx = CallContext::new(CallOptions::default());
        let enumeration = ctx
            .import(&crate::enums::compile("Status".into(), vec![name.clone()], &()).unwrap())
            .unwrap();
        let before = ctx.stats();
        ctx.options.limits.steps = Some(before.steps + 20);
        assert_eq!(
            normalize(
                &mut ctx,
                &Type::named("Status".into()),
                Value::symbol(name.into_bytes()),
                |_, _| Ok(enumeration.clone())
            )
            .unwrap_err()
            .kind,
            ErrorKind::Steps
        );
        assert_eq!(
            ctx.stats().retained_memory_bytes,
            before.retained_memory_bytes
        );
        assert_eq!(ctx.checkpoint().unwrap_err().kind, ErrorKind::Steps);
    }
}
