use super::{Field, Scalar, Type, TypeKind};

pub(crate) trait Description: Sized {
    type Field: FieldDescription<Type = Self>;

    fn name(&self) -> &str;
    fn nullable(&self) -> bool;
    fn view(&self) -> View<'_, Self>;
}

pub(crate) trait FieldDescription {
    type Type: Description;

    fn name(&self) -> &[u8];
    fn ty(&self) -> &Self::Type;
    fn optional(&self) -> bool;
}

// Formatting borrows either syntax or compiled descriptors without allocating a copy.
pub(crate) enum View<'a, T: Description> {
    Scalar(Scalar),
    Array(Option<&'a T>),
    Hash(Option<&'a (T, T)>),
    Shape(&'a [T::Field], bool),
    Union(&'a [T]),
    Named,
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
