//! Types inferred from observed values: each site accumulates the values it
//! saw into a [`Types`] set, which renders as the narrowest annotation that
//! accepts all of them.

use std::collections::{BTreeMap, BTreeSet};
use vibescript::Value;

/// Collections nest at most this deep before their contents become `any`.
const MAX_DEPTH: usize = 6;
/// A hash with more distinct keys than this is a dictionary, not a record.
const MAX_FIELDS: usize = 12;

/// Scalar types, in the order unions list them. `symbol` precedes every enum,
/// so a symbol argument is not converted to an enum member.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum Scalar {
    Bool,
    Int,
    Float,
    String,
    Symbol,
    Money,
    Duration,
    Time,
    Range,
    Regex,
}

impl Scalar {
    fn name(self) -> &'static str {
        match self {
            Self::Bool => "bool",
            Self::Int => "int",
            Self::Float => "float",
            Self::String => "string",
            Self::Symbol => "symbol",
            Self::Money => "money",
            Self::Duration => "duration",
            Self::Time => "time",
            Self::Range => "range",
            Self::Regex => "regex",
        }
    }
}

/// The union of the types of the values a site produced.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Types {
    pub nil: bool,
    pub scalars: BTreeSet<Scalar>,
    /// The element types of the arrays seen, when any were.
    pub array: Option<Box<Types>>,
    pub hash: Option<Box<Shape>>,
    pub classes: BTreeSet<String>,
    pub enums: BTreeSet<String>,
    /// A value no annotation names precisely, such as a regex or a class.
    pub any: bool,
    /// Whether a boolean seen was ever `false`.
    pub falsy: bool,
    /// Whether a hash seen was a host object, whose fields can shadow members.
    pub object: bool,
}

/// The fields of the hashes seen.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Shape {
    /// How many hashes were joined.
    pub count: usize,
    /// Each key, the types stored under it and how many hashes had it.
    pub fields: BTreeMap<Vec<u8>, (Types, usize)>,
    /// Too many or unnamed keys: a dictionary of the joined field types.
    pub dictionary: bool,
}

/// Collections at least this long are typed once and remembered.
const CACHED: usize = 32;
/// Elements typed for one value before the rest count as `any`.
const BUDGET: usize = 200_000;
/// Remembered collections before the cache starts over.
const CACHE_ENTRIES: usize = 4096;

/// Types values, remembering large collections by their storage. The cache
/// holds a clone of each, so the program copies rather than changes it in
/// place, and a remembered type stays exact.
#[derive(Default)]
pub(crate) struct Typer {
    cache: std::collections::HashMap<(usize, usize), (Value, Types)>,
    budget: usize,
}

impl Typer {
    /// Adds a value's type to `types`.
    pub fn add(&mut self, types: &mut Types, value: &Value) {
        self.budget = BUDGET;
        self.add_at(types, value, 0);
    }

    fn add_at(&mut self, types: &mut Types, value: &Value, depth: usize) {
        let key = match (value.as_array(), value.as_hash()) {
            (Some(items), _) if items.len() >= CACHED => {
                Some((items.as_ptr() as usize, items.len()))
            }
            (_, Some(entries)) if entries.len() >= CACHED => {
                Some((entries.as_ptr() as usize, entries.len()))
            }
            _ => None,
        };
        let Some(key) = key else {
            types.add_with(value, depth, self);
            return;
        };
        if let Some((_, cached)) = self.cache.get(&key) {
            types.join(cached);
            return;
        }
        let mut own = Types::default();
        own.add_with(value, depth, self);
        types.join(&own);
        if self.cache.len() >= CACHE_ENTRIES {
            self.cache.clear();
        }
        self.cache.insert(key, (value.clone(), own));
    }
}

impl Types {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// Adds a value's type.
    #[cfg(test)]
    pub fn add(&mut self, value: &Value) {
        Typer::default().add(self, value);
    }

    fn add_with(&mut self, value: &Value, depth: usize, typer: &mut Typer) {
        if depth > MAX_DEPTH || typer.budget == 0 {
            self.any = true;
            return;
        }
        typer.budget -= 1;
        let scalar = match value.type_name() {
            "nil" => {
                self.nil = true;
                return;
            }
            "bool" => {
                self.falsy |= !value.truthy();
                Scalar::Bool
            }
            "int" => Scalar::Int,
            "float" => Scalar::Float,
            "string" => Scalar::String,
            "symbol" => Scalar::Symbol,
            "money" => Scalar::Money,
            "duration" => Scalar::Duration,
            "time" => Scalar::Time,
            "range" => Scalar::Range,
            "regex" => Scalar::Regex,
            "array" => {
                let element = self.array.get_or_insert_with(Default::default);
                for item in value.as_array().unwrap_or_default() {
                    typer.add_at(element, item, depth + 1);
                }
                return;
            }
            "hash" | "object" => {
                self.object |= value.type_name() == "object";
                let shape = self.hash.get_or_insert_with(Default::default);
                shape.count += 1;
                for (key, item) in value.as_hash().unwrap_or_default() {
                    let key = key.as_bytes().unwrap_or_default();
                    if !shape.fields.contains_key(key) {
                        shape.fields.insert(key.to_vec(), Default::default());
                    }
                    let (types, seen) = shape.fields.get_mut(key).unwrap();
                    typer.add_at(types, item, depth + 1);
                    *seen += 1;
                }
                if shape.fields.len() > MAX_FIELDS {
                    shape.dictionary = true;
                }
                return;
            }
            "instance" => {
                match vibescript::observe::class_name(value) {
                    Some(name) => {
                        self.classes.insert(name.replace("::", "."));
                    }
                    None => self.any = true,
                }
                return;
            }
            "enum value" => {
                match value.as_enum_member() {
                    Some((name, _, _)) => {
                        self.enums.insert(name.to_owned());
                    }
                    None => self.any = true,
                }
                return;
            }
            _ => {
                self.any = true;
                return;
            }
        };
        self.scalars.insert(scalar);
    }

    /// Adds every type of `other`.
    pub fn join(&mut self, other: &Types) {
        self.nil |= other.nil;
        self.any |= other.any;
        self.falsy |= other.falsy;
        self.object |= other.object;
        self.scalars.extend(other.scalars.iter().copied());
        self.classes.extend(other.classes.iter().cloned());
        self.enums.extend(other.enums.iter().cloned());
        if let Some(element) = &other.array {
            self.array
                .get_or_insert_with(Default::default)
                .join(element);
        }
        if let Some(shape) = &other.hash {
            let mine = self.hash.get_or_insert_with(Default::default);
            mine.count += shape.count;
            mine.dictionary |= shape.dictionary;
            for (key, (types, seen)) in &shape.fields {
                let (joined, count) = mine.fields.entry(key.clone()).or_default();
                joined.join(types);
                *count += seen;
            }
            if mine.fields.len() > MAX_FIELDS {
                mine.dictionary = true;
            }
        }
    }

    /// Whether every value was a boolean.
    pub fn only_bool(&self) -> bool {
        !self.nil
            && !self.any
            && self.array.is_none()
            && self.hash.is_none()
            && self.classes.is_empty()
            && self.enums.is_empty()
            && self.scalars.iter().all(|s| *s == Scalar::Bool)
            && !self.scalars.is_empty()
    }

    /// Whether a value other than nil can be falsy: a boolean `false`.
    pub fn can_be_false(&self) -> bool {
        self.scalars.contains(&Scalar::Bool)
    }

    /// Whether the set contains `any`, anywhere.
    pub fn has_any(&self) -> bool {
        self.any
            || self
                .array
                .as_ref()
                .is_some_and(|element| element.has_any() || element.is_empty())
            || self.hash.as_ref().is_some_and(|shape| {
                shape.fields.values().any(|(types, _)| types.has_any())
                    || (shape.dictionary && shape.fields.is_empty())
            })
    }

    /// Renders the annotation that accepts every value seen, naming only the
    /// classes and enums `known` declares; others become `any`.
    pub fn render(&self, known: &dyn Fn(&str) -> bool) -> String {
        let mut arms = Vec::new();
        let mut any = self.any;
        let scalars: Vec<Scalar> = self.scalars.iter().copied().collect();
        let number = scalars.contains(&Scalar::Int) && scalars.contains(&Scalar::Float);
        for scalar in scalars {
            match scalar {
                Scalar::Int if number => arms.push("number".to_owned()),
                Scalar::Float if number => (),
                // Only the ADR-007 compiler names regexes.
                Scalar::Regex if !super::compat::regex_type() => any = true,
                _ => arms.push(scalar.name().to_owned()),
            }
        }
        if let Some(element) = &self.array {
            let element = if element.is_empty() {
                "any".to_owned()
            } else {
                element.render(known)
            };
            arms.push(format!("array<{element}>"));
        }
        if let Some(shape) = &self.hash {
            arms.push(shape.render(known));
        }
        for name in self.enums.iter().chain(&self.classes) {
            if known(name) {
                arms.push(name.clone());
            } else {
                any = true;
            }
        }
        if any {
            return "any".to_owned();
        }
        match (arms.len(), self.nil) {
            (0, true) => "nil".to_owned(),
            (0, false) => "any".to_owned(),
            (1, true) => format!("{}?", arms[0]),
            (_, true) => {
                arms.push("nil".to_owned());
                arms.join(" | ")
            }
            _ => arms.join(" | "),
        }
    }
}

impl Shape {
    fn render(&self, known: &dyn Fn(&str) -> bool) -> String {
        let labels = self.fields.keys().all(|key| label(key));
        if self.dictionary || !labels || self.fields.is_empty() {
            let mut values = Types::default();
            for (types, _) in self.fields.values() {
                values.join(types);
            }
            let values = if values.is_empty() {
                "any".to_owned()
            } else {
                values.render(known)
            };
            return format!("hash<string, {values}>");
        }
        let fields: Vec<String> = self
            .fields
            .iter()
            .map(|(key, (types, seen))| {
                let optional = if *seen < self.count { "?" } else { "" };
                let name = String::from_utf8_lossy(key);
                // A field typed `nil` makes a parameter's shape read as a
                // default hash value, so it takes any value instead.
                let ty = match types.render(known) {
                    ty if ty == "nil" => "any".to_owned(),
                    ty => ty,
                };
                format!("{name}{optional}: {ty}")
            })
            .collect();
        format!("{{ {} }}", fields.join(", "))
    }
}

/// Whether a key can be written as a shape field label.
fn label(key: &[u8]) -> bool {
    let Ok(key) = std::str::from_utf8(key) else {
        return false;
    };
    let mut chars = key.chars();
    chars
        .next()
        .is_some_and(|c| c == '_' || c.is_ascii_alphabetic())
        && chars.all(|c| c == '_' || c.is_ascii_alphanumeric())
        && !vibescript::surface::parse::keyword(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(values: &[Value]) -> String {
        let mut types = Types::default();
        for value in values {
            types.add(value);
        }
        types.render(&|_| true)
    }

    #[test]
    fn joins_scalars_into_the_narrowest_union() {
        assert_eq!(render(&[Value::int(1)]), "int");
        assert_eq!(render(&[Value::int(1), Value::nil()]), "int?");
        assert_eq!(render(&[Value::int(1), Value::float(1.5)]), "number");
        assert_eq!(
            render(&[Value::int(1), Value::bytes("a"), Value::nil()]),
            "int | string | nil"
        );
        assert_eq!(render(&[Value::nil()]), "nil");
    }

    #[test]
    fn joins_collections_by_element_and_field() {
        let array = Value::array(vec![Value::int(1), Value::bytes("x")]);
        assert_eq!(render(&[array]), "array<int | string>");
        assert_eq!(render(&[Value::array(Vec::new())]), "array<any>");
        let a = Value::hash(vec![(b"id".to_vec(), Value::int(1))]);
        let b = Value::hash(vec![
            (b"id".to_vec(), Value::int(2)),
            (b"name".to_vec(), Value::bytes("x")),
        ]);
        assert_eq!(render(&[a, b]), "{ id: int, name?: string }");
        let odd = Value::hash(vec![(b"a b".to_vec(), Value::int(1))]);
        assert_eq!(render(&[odd]), "hash<string, int>");
    }
}
