//! Types as the static checker writes them in diagnostics, such as
//! `array<int | string>` or `{ name: string, age?: int }`, parsed so they can
//! be joined with each other and with observed [`Types`] and written back as
//! annotations.

use super::types::{Scalar, Types};

/// A type an annotation can name, or [`Ty::Opaque`] for one it cannot.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Ty {
    /// No value, such as the elements of `[]`.
    Never,
    Any,
    Nil,
    /// A builtin scalar, or a declared class or enum.
    Named(String),
    Array(Box<Ty>),
    /// `hash<string, V>`.
    Hash(Box<Ty>),
    /// Fields sorted by name, and whether other keys may appear.
    Shape(Vec<Field>, bool),
    Tuple(Vec<Ty>),
    /// Two or more alternatives, flattened, sorted and distinct.
    Union(Vec<Ty>),
    /// Something no annotation names, such as `type<int>` or a builtin
    /// namespace.
    Opaque(String),
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Field {
    pub name: String,
    pub optional: bool,
    pub ty: Ty,
}

/// Builtin scalar names an annotation may use.
const SCALARS: [&str; 15] = [
    "bool",
    "int",
    "float",
    "string",
    "symbol",
    "money",
    "duration",
    "time",
    "range",
    "regex",
    "match_data",
    "error",
    "enum_value",
    "enum_type",
    "number",
];

impl Ty {
    /// Parses a type as the checker displays it, or `None` when the text is
    /// malformed.
    pub fn parse(text: &str) -> Option<Ty> {
        let mut parser = Parser {
            text: text.as_bytes(),
            at: 0,
        };
        let ty = parser.union()?;
        parser.space();
        (parser.at == parser.text.len()).then_some(ty)
    }

    /// The union of `types`, simplified: collections of the same kind merge
    /// element by element, `never` disappears and `any` absorbs the rest.
    pub fn union(types: impl IntoIterator<Item = Ty>) -> Ty {
        let mut arms: Vec<Ty> = Vec::new();
        for ty in types {
            match ty {
                Ty::Union(inner) => arms.extend(inner),
                Ty::Never => (),
                other => arms.push(other),
            }
        }
        if arms.contains(&Ty::Any) {
            return Ty::Any;
        }
        let mut merged: Vec<Ty> = Vec::new();
        for arm in arms {
            if merged.contains(&arm) {
                continue;
            }
            match merged.iter().position(|kept| same_kind(kept, &arm)) {
                Some(index) => {
                    let kept = merged.remove(index);
                    merged.push(join_collections(kept, arm));
                }
                None => merged.push(arm),
            }
        }
        merged.sort();
        merged.dedup();
        match merged.len() {
            0 => Ty::Never,
            1 => merged.pop().unwrap(),
            _ => Ty::Union(merged),
        }
    }

    /// Joins two types.
    pub fn join(self, other: Ty) -> Ty {
        Ty::union([self, other])
    }

    /// The type without `nil`.
    pub fn without_nil(&self) -> Ty {
        match self {
            Ty::Nil => Ty::Never,
            Ty::Union(arms) => Ty::union(arms.iter().filter(|arm| **arm != Ty::Nil).cloned()),
            other => other.clone(),
        }
    }

    pub fn has_nil(&self) -> bool {
        match self {
            Ty::Nil => true,
            Ty::Union(arms) => arms.contains(&Ty::Nil),
            _ => false,
        }
    }

    /// Whether the type mentions `any`, `never` inside a collection, or
    /// something no annotation names.
    pub fn vague(&self) -> bool {
        match self {
            Ty::Any | Ty::Opaque(_) => true,
            Ty::Never | Ty::Nil | Ty::Named(_) => false,
            Ty::Array(element) | Ty::Hash(element) => **element == Ty::Never || element.vague(),
            Ty::Shape(fields, open) => *open || fields.iter().any(|field| field.ty.vague()),
            Ty::Tuple(items) | Ty::Union(items) => items.iter().any(Ty::vague),
        }
    }

    /// Whether the type names the class `name`.
    pub fn mentions(&self, name: &str) -> bool {
        match self {
            Ty::Named(own) => own == name,
            Ty::Array(element) | Ty::Hash(element) => element.mentions(name),
            Ty::Shape(fields, _) => fields.iter().any(|field| field.ty.mentions(name)),
            Ty::Tuple(items) | Ty::Union(items) => items.iter().any(|item| item.mentions(name)),
            _ => false,
        }
    }

    /// The type with every mention of the class `name` replaced by `by`.
    pub fn substitute(self, name: &str, by: &Ty) -> Ty {
        match self {
            Ty::Named(own) if own == name => by.clone(),
            Ty::Array(element) => Ty::Array(Box::new(element.substitute(name, by))),
            Ty::Hash(value) => Ty::Hash(Box::new(value.substitute(name, by))),
            Ty::Shape(fields, open) => Ty::Shape(
                fields
                    .into_iter()
                    .map(|field| Field {
                        ty: field.ty.substitute(name, by),
                        ..field
                    })
                    .collect(),
                open,
            ),
            Ty::Tuple(items) => Ty::Tuple(
                items
                    .into_iter()
                    .map(|item| item.substitute(name, by))
                    .collect(),
            ),
            Ty::Union(arms) => Ty::union(arms.into_iter().map(|arm| arm.substitute(name, by))),
            other => other,
        }
    }

    /// The type as an annotation writes it, or `None` when it names
    /// something no annotation can, such as a builtin namespace, a type
    /// literal or a class nested in another.
    pub fn render(&self) -> Option<String> {
        Some(match self {
            Ty::Never | Ty::Opaque(_) => return None,
            Ty::Any => "any".to_owned(),
            Ty::Nil => "nil".to_owned(),
            Ty::Named(name) if name.contains("::") || name.contains('.') => return None,
            Ty::Named(name) => name.clone(),
            Ty::Array(element) => match **element {
                Ty::Never => "array<any>".to_owned(),
                _ => format!("array<{}>", element.render()?),
            },
            Ty::Hash(value) => match **value {
                Ty::Never => "hash<string, any>".to_owned(),
                _ => format!("hash<string, {}>", value.render()?),
            },
            Ty::Shape(fields, open) => {
                if fields.is_empty() {
                    return Some(if *open { "hash<string, any>" } else { "{}" }.to_owned());
                }
                if *open {
                    return None;
                }
                let mut parts = Vec::new();
                for field in fields {
                    if !label(&field.name) {
                        return None;
                    }
                    let optional = if field.optional { "?" } else { "" };
                    // A field typed `nil` would read as a default value.
                    let ty = match &field.ty {
                        Ty::Nil => "any".to_owned(),
                        ty => ty.render()?,
                    };
                    parts.push(format!("{}{optional}: {ty}", field.name));
                }
                format!("{{ {} }}", parts.join(", "))
            }
            Ty::Tuple(items) => {
                let items: Option<Vec<String>> = items.iter().map(Ty::render).collect();
                format!("[{}]", items?.join(", "))
            }
            Ty::Union(arms) => {
                let nil = arms.contains(&Ty::Nil);
                let others: Vec<&Ty> = arms.iter().filter(|arm| **arm != Ty::Nil).collect();
                let int = Ty::Named("int".to_owned());
                let float = Ty::Named("float".to_owned());
                let number = others.contains(&&int) && others.contains(&&float);
                let mut parts: Vec<String> = Vec::new();
                for arm in &others {
                    if number && (**arm == int || **arm == float) {
                        continue;
                    }
                    parts.push(arm.render()?);
                }
                if number {
                    parts.push("number".to_owned());
                }
                parts.sort();
                match (parts.len(), nil) {
                    (1, true) => format!("{}?", parts[0]),
                    (_, true) => format!("{} | nil", parts.join(" | ")),
                    _ => parts.join(" | "),
                }
            }
        })
    }

    /// The types observed at a site, naming only the classes and enums
    /// `known` declares.
    pub fn observed(types: &Types, known: &dyn Fn(&str) -> bool) -> Ty {
        if types.any {
            return Ty::Any;
        }
        let mut arms = Vec::new();
        if types.nil {
            arms.push(Ty::Nil);
        }
        for scalar in &types.scalars {
            arms.push(Ty::Named(scalar_name(*scalar).to_owned()));
        }
        if let Some(element) = &types.array {
            arms.push(Ty::Array(Box::new(Ty::observed(element, known))));
        }
        if let Some(shape) = &types.hash {
            let labels = shape
                .fields
                .keys()
                .all(|key| std::str::from_utf8(key).is_ok_and(label));
            if shape.dictionary || !labels {
                let values = Ty::union(
                    shape
                        .fields
                        .values()
                        .map(|(types, _)| Ty::observed(types, known)),
                );
                arms.push(Ty::Hash(Box::new(values)));
            } else {
                let fields = shape
                    .fields
                    .iter()
                    .map(|(key, (types, seen))| Field {
                        name: String::from_utf8_lossy(key).into_owned(),
                        optional: *seen < shape.count,
                        ty: Ty::observed(types, known),
                    })
                    .collect();
                arms.push(Ty::Shape(fields, false));
            }
        }
        for name in types.classes.iter().chain(&types.enums) {
            if !known(name) {
                return Ty::Any;
            }
            arms.push(Ty::Named(name.clone()));
        }
        Ty::union(arms)
    }
}

fn scalar_name(scalar: Scalar) -> &'static str {
    match scalar {
        Scalar::Bool => "bool",
        Scalar::Int => "int",
        Scalar::Float => "float",
        Scalar::String => "string",
        Scalar::Symbol => "symbol",
        Scalar::Money => "money",
        Scalar::Duration => "duration",
        Scalar::Time => "time",
        Scalar::Range => "range",
        Scalar::Regex => "regex",
    }
}

/// Whether two types are collections whose union merges into one.
fn same_kind(a: &Ty, b: &Ty) -> bool {
    let collection = |ty: &Ty| match ty {
        Ty::Array(_) | Ty::Tuple(_) => Some(0),
        Ty::Hash(_) | Ty::Shape(..) => Some(1),
        _ => None,
    };
    collection(a).is_some() && collection(a) == collection(b)
}

fn join_collections(a: Ty, b: Ty) -> Ty {
    match (a, b) {
        (Ty::Array(a), Ty::Array(b)) => Ty::Array(Box::new(a.join(*b))),
        (Ty::Tuple(a), Ty::Tuple(b)) if a.len() == b.len() => {
            Ty::Tuple(a.into_iter().zip(b).map(|(a, b)| a.join(b)).collect())
        }
        (Ty::Tuple(items), Ty::Array(element)) | (Ty::Array(element), Ty::Tuple(items)) => {
            Ty::Array(Box::new(Ty::union(items.into_iter().chain([*element]))))
        }
        (Ty::Tuple(a), Ty::Tuple(b)) => Ty::Array(Box::new(Ty::union(a.into_iter().chain(b)))),
        (Ty::Hash(a), Ty::Hash(b)) => Ty::Hash(Box::new(a.join(*b))),
        (Ty::Shape(fields, _), Ty::Hash(value)) | (Ty::Hash(value), Ty::Shape(fields, _)) => {
            Ty::Hash(Box::new(Ty::union(
                fields.into_iter().map(|field| field.ty).chain([*value]),
            )))
        }
        (Ty::Shape(a, a_open), Ty::Shape(b, b_open)) => {
            let mut fields: Vec<Field> = Vec::new();
            for field in &a {
                match b.iter().find(|other| other.name == field.name) {
                    Some(other) => fields.push(Field {
                        name: field.name.clone(),
                        optional: field.optional || other.optional,
                        ty: field.ty.clone().join(other.ty.clone()),
                    }),
                    None => fields.push(Field {
                        optional: true,
                        ..field.clone()
                    }),
                }
            }
            for field in &b {
                if !a.iter().any(|other| other.name == field.name) {
                    fields.push(Field {
                        optional: true,
                        ..field.clone()
                    });
                }
            }
            fields.sort();
            Ty::Shape(fields, a_open || b_open)
        }
        (a, b) => Ty::Union(vec![a, b]),
    }
}

/// Whether a key can be written as a shape field label.
fn label(key: &str) -> bool {
    let mut chars = key.chars();
    chars
        .next()
        .is_some_and(|c| c == '_' || c.is_ascii_alphabetic())
        && chars.all(|c| c == '_' || c.is_ascii_alphanumeric())
        && !vibescript::surface::parse::keyword(key)
}

struct Parser<'t> {
    text: &'t [u8],
    at: usize,
}

impl Parser<'_> {
    fn space(&mut self) {
        while self.text.get(self.at) == Some(&b' ') {
            self.at += 1;
        }
    }

    fn eat(&mut self, byte: u8) -> bool {
        self.space();
        if self.text.get(self.at) == Some(&byte) {
            self.at += 1;
            true
        } else {
            false
        }
    }

    fn eat_str(&mut self, text: &str) -> bool {
        self.space();
        if self.text[self.at..].starts_with(text.as_bytes()) {
            self.at += text.len();
            true
        } else {
            false
        }
    }

    fn union(&mut self) -> Option<Ty> {
        let mut arms = vec![self.optional()?];
        while self.eat(b'|') {
            arms.push(self.optional()?);
        }
        Some(if arms.len() == 1 {
            arms.pop().unwrap()
        } else {
            Ty::union(arms)
        })
    }

    fn optional(&mut self) -> Option<Ty> {
        let ty = self.primary()?;
        if self.eat(b'?') {
            return Some(ty.join(Ty::Nil));
        }
        Some(ty)
    }

    fn word(&mut self) -> Option<String> {
        self.space();
        let start = self.at;
        while let Some(&byte) = self.text.get(self.at) {
            let colons = byte == b':' && self.text.get(self.at + 1) == Some(&b':');
            if byte.is_ascii_alphanumeric() || byte == b'_' {
                self.at += 1;
            } else if colons && self.at > start {
                self.at += 2;
            } else {
                break;
            }
        }
        (self.at > start).then(|| String::from_utf8_lossy(&self.text[start..self.at]).into_owned())
    }

    fn primary(&mut self) -> Option<Ty> {
        self.space();
        match self.text.get(self.at)? {
            b'{' => {
                self.at += 1;
                self.shape()
            }
            b'[' => {
                self.at += 1;
                let mut items = Vec::new();
                if !self.eat(b']') {
                    loop {
                        items.push(self.union()?);
                        if self.eat(b']') {
                            break;
                        }
                        if !self.eat(b',') {
                            return None;
                        }
                    }
                }
                Some(Ty::Tuple(items))
            }
            b':' => {
                self.at += 1;
                let name = self.word()?;
                Some(Ty::Opaque(format!(":{name}")))
            }
            _ => {
                let name = self.word()?;
                if self.eat(b'<') {
                    let mut args = vec![self.union()?];
                    while self.eat(b',') {
                        args.push(self.union()?);
                    }
                    if !self.eat(b'>') {
                        return None;
                    }
                    return Some(match (name.as_str(), args.as_slice()) {
                        ("array", [element]) => Ty::Array(Box::new(element.clone())),
                        ("hash", [_, value]) => Ty::Hash(Box::new(value.clone())),
                        _ => Ty::Opaque(name),
                    });
                }
                Some(match name.as_str() {
                    "any" => Ty::Any,
                    "nil" => Ty::Nil,
                    "never" => Ty::Never,
                    "number" => {
                        Ty::union([Ty::Named("int".to_owned()), Ty::Named("float".to_owned())])
                    }
                    "unknown" | "module" => Ty::Opaque(name),
                    "array" => Ty::Array(Box::new(Ty::Any)),
                    "hash" => Ty::Hash(Box::new(Ty::Any)),
                    scalar if SCALARS.contains(&scalar) => Ty::Named(name),
                    // A signature's type variable.
                    var if var.starts_with('T')
                        && var[1..].bytes().all(|b| b.is_ascii_digit())
                        && var.len() > 1 =>
                    {
                        Ty::Opaque(name)
                    }
                    _ => Ty::Named(name),
                })
            }
        }
    }

    fn shape(&mut self) -> Option<Ty> {
        let mut fields = Vec::new();
        let mut open = false;
        if self.eat(b'}') {
            return Some(Ty::Shape(fields, false));
        }
        loop {
            if self.eat_str("...") {
                open = true;
            } else {
                self.space();
                let name = if self.text.get(self.at) == Some(&b'"') {
                    self.quoted()?
                } else {
                    self.word()?
                };
                let optional = self.eat(b'?');
                if !self.eat(b':') {
                    return None;
                }
                let ty = self.union()?;
                fields.push(Field { name, optional, ty });
            }
            if self.eat(b'}') {
                break;
            }
            if !self.eat(b',') {
                return None;
            }
        }
        fields.sort();
        Some(Ty::Shape(fields, open))
    }

    fn quoted(&mut self) -> Option<String> {
        self.at += 1;
        let mut out = Vec::new();
        loop {
            match *self.text.get(self.at)? {
                b'"' => {
                    self.at += 1;
                    return Some(String::from_utf8_lossy(&out).into_owned());
                }
                b'\\' => {
                    out.push(*self.text.get(self.at + 1)?);
                    self.at += 2;
                }
                byte => {
                    out.push(byte);
                    self.at += 1;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn joined(types: &[&str]) -> String {
        Ty::union(types.iter().map(|text| Ty::parse(text).unwrap()))
            .render()
            .unwrap_or_else(|| "<none>".to_owned())
    }

    #[test]
    fn round_trips_the_checker_spelling() {
        for text in [
            "int",
            "int?",
            "array<int | string>",
            "hash<string, array<int>>",
            "{ age?: int, name: string }",
            "[int, string]",
            "array<string> | int | nil",
            "number",
            "Invoice?",
        ] {
            assert_eq!(Ty::parse(text).unwrap().render().unwrap(), text);
        }
        assert_eq!(Ty::parse("type<int>").unwrap().render(), None);
        assert_eq!(Ty::parse("Outer::Inner").unwrap().render(), None);
        assert!(Ty::parse("array<int").is_none());
    }

    #[test]
    fn joins_collections_element_by_element() {
        assert_eq!(
            joined(&["array<string>", "array<array<int> | string>"]),
            "array<array<int> | string>"
        );
        assert_eq!(joined(&["int", "nil"]), "int?");
        assert_eq!(joined(&["int", "float"]), "number");
        assert_eq!(joined(&["array<never>", "array<int>"]), "array<int>");
        assert_eq!(
            joined(&["[int, int]", "array<string>"]),
            "array<int | string>"
        );
        assert_eq!(
            joined(&["{ a: int }", "{ a: string, b: int }"]),
            "{ a: int | string, b?: int }"
        );
        assert_eq!(
            joined(&["{ a: int }", "hash<string, string>"]),
            "hash<string, int | string>"
        );
        assert_eq!(joined(&["{}", "{ a: int }"]), "{ a?: int }");
        assert_eq!(joined(&["int", "any"]), "any");
    }
}
