use super::{Boxed, Buffer, Bytes, Name, Work};
use crate::{Result, types};
use types::description::{Description, FieldDescription, View};

#[derive(Debug)]
pub(crate) struct Type {
    pub name: Name,
    pub kind: TypeKind,
    pub nullable: bool,
}

#[derive(Debug)]
pub(crate) enum TypeKind {
    Scalar(types::Scalar),
    Array(Option<Boxed<Type>>),
    Hash(Option<Boxed<(Type, Type)>>),
    Shape(Buffer<Field>, bool),
    Union(Buffer<Type>),
    /// An array of exactly these elements, in order.
    Tuple(Buffer<Type>),
    /// `type<T>`, a type literal describing `T`.
    Literal(Option<Boxed<Type>>),
    Named,
}

#[derive(Debug)]
pub(crate) struct Field {
    pub name: Bytes,
    pub ty: Type,
    pub optional: bool,
}

impl Type {
    pub fn captures(&self, hash: bool) -> bool {
        match &self.kind {
            TypeKind::Scalar(types::Scalar::Any) => true,
            TypeKind::Array(_) => !hash,
            TypeKind::Hash(_) | TypeKind::Shape(..) => hash,
            TypeKind::Union(options) => options.iter().any(|option| option.captures(hash)),
            TypeKind::Tuple(_) => !hash,
            _ => false,
        }
    }

    /// Classifies a name without allocating an unaccounted folded spelling.
    pub fn named(name: Name) -> Self {
        let kind = match types::builtin_name(&name) {
            Some(types::BuiltinName::Scalar(scalar)) => TypeKind::Scalar(scalar),
            Some(types::BuiltinName::Array) => TypeKind::Array(None),
            Some(types::BuiltinName::Hash) => TypeKind::Hash(None),
            Some(types::BuiltinName::Type) => TypeKind::Literal(None),
            None => TypeKind::Named,
        };
        Self {
            name,
            kind,
            nullable: false,
        }
    }

    /// Copies an alias's type containers, retaining shared immutable name storage.
    pub fn copy(&self, work: &dyn Work) -> Result<Self> {
        work.charge(1)?;
        work.bytes(self.name.len())?;
        let kind = match &self.kind {
            TypeKind::Scalar(scalar) => TypeKind::Scalar(*scalar),
            TypeKind::Array(element) => TypeKind::Array(
                element
                    .as_ref()
                    .map(|ty| Boxed::new(work, ty.copy(work)?))
                    .transpose()?,
            ),
            TypeKind::Hash(pair) => TypeKind::Hash(
                pair.as_ref()
                    .map(|pair| Boxed::new(work, (pair.0.copy(work)?, pair.1.copy(work)?)))
                    .transpose()?,
            ),
            TypeKind::Shape(fields, open) => TypeKind::Shape(
                fields.copy_with(work, |field| {
                    work.bytes(field.name.len())?;
                    Ok(Field {
                        name: field.name.clone(),
                        ty: field.ty.copy(work)?,
                        optional: field.optional,
                    })
                })?,
                *open,
            ),
            TypeKind::Union(options) => {
                TypeKind::Union(options.copy_with(work, |ty| ty.copy(work))?)
            }
            TypeKind::Tuple(elements) => {
                TypeKind::Tuple(elements.copy_with(work, |ty| ty.copy(work))?)
            }
            TypeKind::Literal(described) => TypeKind::Literal(
                described
                    .as_ref()
                    .map(|ty| Boxed::new(work, ty.copy(work)?))
                    .transpose()?,
            ),
            TypeKind::Named => TypeKind::Named,
        };
        Ok(Self {
            name: self.name.clone(),
            kind,
            nullable: self.nullable,
        })
    }

    /// Produces compiled metadata without retaining the originating call's budget.
    pub fn compile(&self, work: &dyn Work) -> Result<types::Type> {
        work.charge(1)?;
        work.bytes(self.name.len())?;
        let kind = match &self.kind {
            TypeKind::Scalar(scalar) => types::TypeKind::Scalar(*scalar),
            TypeKind::Array(element) => types::TypeKind::Array(
                element
                    .as_ref()
                    .map(|ty| Ok(Box::new(ty.compile(work)?)))
                    .transpose()?,
            ),
            TypeKind::Hash(pair) => types::TypeKind::Hash(
                pair.as_ref()
                    .map(|pair| Ok(Box::new((pair.0.compile(work)?, pair.1.compile(work)?))))
                    .transpose()?,
            ),
            TypeKind::Shape(fields, open) => {
                let mut compiled = Vec::with_capacity(fields.len());
                for field in fields {
                    work.bytes(field.name.len())?;
                    compiled.push(types::Field {
                        name: field.name.to_vec(),
                        ty: field.ty.compile(work)?,
                        optional: field.optional,
                    });
                }
                types::TypeKind::Shape(compiled, *open)
            }
            TypeKind::Union(options) => {
                let mut compiled = Vec::with_capacity(options.len());
                for ty in options {
                    compiled.push(ty.compile(work)?);
                }
                types::TypeKind::Union(compiled)
            }
            TypeKind::Tuple(elements) => {
                let mut compiled = Vec::with_capacity(elements.len());
                for ty in elements {
                    compiled.push(ty.compile(work)?);
                }
                types::TypeKind::Tuple(compiled)
            }
            TypeKind::Literal(described) => types::TypeKind::Literal(
                described
                    .as_ref()
                    .map(|ty| Ok(Box::new(ty.compile(work)?)))
                    .transpose()?,
            ),
            TypeKind::Named => types::TypeKind::Named,
        };
        Ok(types::Type {
            name: self.name.as_str().to_owned(),
            kind,
            nullable: self.nullable,
        })
    }
}

impl Description for Type {
    type Field = Field;

    fn name(&self) -> &str {
        &self.name
    }

    fn nullable(&self) -> bool {
        self.nullable
    }

    fn view(&self) -> View<'_, Self> {
        match &self.kind {
            TypeKind::Scalar(scalar) => View::Scalar(*scalar),
            TypeKind::Array(element) => View::Array(element.as_deref()),
            TypeKind::Hash(pair) => View::Hash(pair.as_deref()),
            TypeKind::Shape(fields, open) => View::Shape(fields, *open),
            TypeKind::Union(options) => View::Union(options),
            TypeKind::Tuple(elements) => View::Tuple(elements),
            TypeKind::Literal(described) => View::Literal(described.as_deref()),
            TypeKind::Named => View::Named,
        }
    }
}

impl FieldDescription for Field {
    type Type = Type;

    fn name(&self) -> &[u8] {
        &self.name
    }

    fn ty(&self) -> &Type {
        &self.ty
    }

    fn optional(&self) -> bool {
        self.optional
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CallContext, CallOptions, ErrorKind, compilation::Meter};
    use std::{cell::RefCell, sync::Arc};

    fn parse(context: &mut CallContext, annotation: &str) -> Type {
        let mut parsed = crate::syntax::parse(
            &format!("def take(value:{annotation});value;end"),
            &Meter(RefCell::new(context)),
        )
        .unwrap_or_else(|error| panic!("{annotation}: {error}"));
        parsed.functions.remove(1).params.remove(0).ty.unwrap()
    }

    #[test]
    fn type_aliases_reserve_containers_and_release_partial_copies() {
        let annotation = "array<{id:int, value?:object<symbol, array<float?>> | nil, ...}>";
        for fraction in [0, 1, 2, 3] {
            let mut context = CallContext::new(CallOptions::default());
            let original = parse(&mut context, annotation);
            let before = context.stats().retained_memory_bytes;
            let copy = original.copy(&Meter(RefCell::new(&mut context))).unwrap();
            let additional = context.stats().retained_memory_bytes - before;
            assert!(additional > 0);
            drop(copy);
            let budget = match fraction {
                0 => 0,
                1 => additional / 2,
                2 => additional - 1,
                _ => additional,
            };
            context.options.limits.memory_bytes = Some(before + budget);
            let result = original.copy(&Meter(RefCell::new(&mut context)));
            if fraction == 3 {
                let copy = result.unwrap();
                assert_eq!(copy.name.as_ptr(), original.name.as_ptr());
                drop(original);
                assert!(context.stats().retained_memory_bytes > 0);
                let compiled = copy.compile(&Meter(RefCell::new(&mut context))).unwrap();
                drop(copy);
                assert_eq!(context.stats().retained_memory_bytes, 0);
                assert_eq!(compiled.name, "array");
            } else {
                assert_eq!(result.unwrap_err().kind, ErrorKind::Memory);
                assert_eq!(context.checkpoint().unwrap_err().kind, ErrorKind::Memory);
                assert_eq!(context.stats().retained_memory_bytes, before);
                drop(original);
                assert_eq!(context.stats().retained_memory_bytes, 0);
            }
        }
    }

    #[test]
    fn compiled_types_preserve_formatting_without_retaining_the_call() {
        for (annotation, expected) in [
            (
                "ARRAY<OBJECT<symbol,{z?:string,a:int,...}>>?",
                "array<object<symbol, { a: int, z?: string, ... }>>?",
            ),
            ("int|string|Status.Member?", "int | string | Status.Member?"),
            ("array|hash|object|symbol", "array | hash | object | symbol"),
            ("{:\"ready?\": int}", "{ \"ready?\": int }"),
        ] {
            let mut context = CallContext::new(CallOptions::default());
            let memory = Arc::downgrade(&context.identity());
            let ty = parse(&mut context, annotation);
            let before = context.stats().retained_memory_bytes;
            let mut text = Vec::new();
            crate::shapes::format(&ty, &mut text).unwrap();
            assert_eq!(text, expected.as_bytes());
            let compiled = ty.compile(&Meter(RefCell::new(&mut context))).unwrap();
            assert_eq!(context.stats().retained_memory_bytes, before);
            drop(ty);
            assert_eq!(context.stats().retained_memory_bytes, 0);
            drop(context);
            assert!(memory.upgrade().is_none());
            text.clear();
            crate::shapes::format(&compiled, &mut text).unwrap();
            assert_eq!(text, expected.as_bytes());
        }
    }

    #[test]
    fn builtin_names_preserve_simple_unicode_lowercasing() {
        use types::{BuiltinName, Scalar, builtin_name};
        for (name, expected) in [
            ("any", BuiltinName::Scalar(Scalar::Any)),
            ("int", BuiltinName::Scalar(Scalar::Int)),
            ("float", BuiltinName::Scalar(Scalar::Float)),
            ("number", BuiltinName::Scalar(Scalar::Number)),
            ("string", BuiltinName::Scalar(Scalar::String)),
            ("bool", BuiltinName::Scalar(Scalar::Bool)),
            ("nil", BuiltinName::Scalar(Scalar::Nil)),
            ("duration", BuiltinName::Scalar(Scalar::Duration)),
            ("time", BuiltinName::Scalar(Scalar::Time)),
            ("money", BuiltinName::Scalar(Scalar::Money)),
            ("range", BuiltinName::Scalar(Scalar::Range)),
            ("symbol", BuiltinName::Scalar(Scalar::Symbol)),
            ("array", BuiltinName::Array),
            ("hash", BuiltinName::Hash),
            ("object", BuiltinName::Hash),
        ] {
            assert_eq!(builtin_name(name), Some(expected));
            assert_eq!(builtin_name(&name.to_ascii_uppercase()), Some(expected));
        }
        assert_eq!(builtin_name("İNT"), Some(BuiltinName::Scalar(Scalar::Int)));
        for name in [
            "",
            "integer",
            "durationx",
            "ſtring",
            "ınt",
            "Status",
            "日本語",
        ] {
            assert_eq!(builtin_name(name), None, "{name}");
        }
    }
}
