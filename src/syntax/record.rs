//! Declaration facts that compilation discards, recorded only for editor tooling.
//!
//! Ordinary parses carry no record, so compilation and its accounting are
//! unchanged. A recorded parse is unmetered and bounded by the usual source and
//! syntax-depth guards.

use super::{Call, Declarations, Expr, Node, Parameter, Parsed, Parsing, parser};
use crate::{Result, compilation::TypeKind, types::Scalar, value::Kind};

/// What one top-level statement declares, by index into the parsed declarations.
#[derive(Debug)]
pub(crate) enum Top {
    /// A function, by index into the functions without the entry point.
    Function(usize),
    /// An alias copy of the named function, indexed like [`Top::Function`].
    Alias(usize, String),
    Enum(usize),
    /// A class or module declaration.
    Module(usize),
    /// Any other statement.
    Statement,
}

/// A class alias, which compiles to a copy of its target among the class's
/// instance methods.
#[derive(Debug)]
pub(crate) struct ClassAlias {
    /// The declaring class's offset.
    pub class: u32,
    /// The copy's index among the class's instance methods.
    pub index: usize,
    pub offset: u32,
    pub target: String,
}

/// A member access whose receiver the caller wants classified.
#[derive(Debug, Default)]
pub(crate) struct Probe {
    pub name: String,
    /// Declared parameter kinds of the functions whose bodies are being parsed.
    pub params: Vec<Vec<(String, Option<&'static str>)>>,
    /// The first matching access's receiver kind, once one has been parsed.
    pub receiver: Option<Option<&'static str>>,
}

#[derive(Debug, Default)]
pub(crate) struct Record {
    /// Top-level statements in source order, with their starting offsets.
    pub top: Vec<(u32, Top)>,
    /// Each enum's member offsets, in declaration order.
    pub enums: Vec<Vec<u32>>,
    pub aliases: Vec<ClassAlias>,
    pub probe: Option<Probe>,
}

impl Record {
    /// Starts a function body, exposing its declared parameter kinds to the probe.
    pub(super) fn enter_function(&mut self, params: &[Parameter]) {
        if let Some(probe) = &mut self.probe {
            probe.params.push(
                params
                    .iter()
                    .map(|param| (param.name.to_string(), declared_kind(param)))
                    .collect(),
            );
        }
    }

    pub(super) fn leave_function(&mut self) {
        if let Some(probe) = &mut self.probe {
            probe.params.pop();
        }
    }

    /// Classifies the receiver of the first member access named like the probe.
    pub(super) fn member(&mut self, name: &str, receiver: &Expr) {
        let Some(probe) = &mut self.probe else {
            return;
        };
        if probe.receiver.is_some() || probe.name != name {
            return;
        }
        let params = probe.params.last().map(Vec::as_slice).unwrap_or_default();
        probe.receiver = Some(receiver_kind(receiver, params));
    }
}

/// The member-table receiver kind of an annotated parameter. Nullable, union,
/// named and unannotated parameters are not one kind.
fn declared_kind(param: &Parameter) -> Option<&'static str> {
    let ty = param.ty.as_ref()?;
    if ty.nullable {
        return None;
    }
    Some(match ty.kind {
        TypeKind::Scalar(Scalar::String) => "string",
        TypeKind::Scalar(Scalar::Int) => "int",
        TypeKind::Scalar(Scalar::Float) => "float",
        TypeKind::Scalar(Scalar::Bool) => "bool",
        TypeKind::Scalar(Scalar::Symbol) => "symbol",
        TypeKind::Scalar(Scalar::Money) => "money",
        TypeKind::Scalar(Scalar::Duration) => "duration",
        TypeKind::Scalar(Scalar::Time) => "time",
        TypeKind::Scalar(Scalar::Range) => "range",
        TypeKind::Array(_) => "array",
        TypeKind::Hash(_) => "hash",
        _ => return None,
    })
}

/// The receiver kind a literal or annotated parameter fixes from syntax alone.
fn receiver_kind(
    receiver: &Expr,
    params: &[(String, Option<&'static str>)],
) -> Option<&'static str> {
    match &receiver.node {
        Node::Integer(_) | Node::BigInteger(..) => Some("int"),
        Node::Literal(value) => match &value.0 {
            Kind::Bytes(_) => Some("string"),
            Kind::Int(_) | Kind::Big(_) => Some("int"),
            Kind::Float(_) => Some("float"),
            Kind::Bool(_) => Some("bool"),
            Kind::Symbol(_) => Some("symbol"),
            _ => None,
        },
        Node::Template(_, symbol) => Some(if *symbol { "symbol" } else { "string" }),
        Node::Array(_) => Some("array"),
        Node::Hash(_) => Some("hash"),
        Node::Shape(_, Some(fallback), _) => receiver_kind(fallback, params),
        Node::Regex(..) => Some("regex"),
        Node::Var(name) => params
            .iter()
            .rev()
            .find(|(param, _)| param.as_str() == &**name)
            .and_then(|(_, kind)| *kind),
        _ => None,
    }
}

/// Parses source while recording declaration facts, and classifies the first
/// member access named `probe` when one is given.
///
/// The record keeps what was observed before a syntax error, so a probe
/// parsed ahead of an error elsewhere is still classified.
pub(crate) fn parse(source: &str, probe: Option<&str>) -> (Result<Declarations>, Record) {
    let record = Record {
        probe: probe.map(|name| Probe {
            name: name.to_owned(),
            ..Probe::default()
        }),
        ..Record::default()
    };
    let mut parser = match parser(source, &()) {
        Ok(parser) => parser,
        Err(error) => return (Err(error), record),
    };
    parser.record = Some(Box::new(record));
    let parsing = Parsing::new(parser);
    let result = parsing.run(Call::Program).map(|parsed| match parsed {
        Parsed::Program(declarations) => declarations,
        _ => unreachable!(),
    });
    let record = parsing
        .parser
        .borrow_mut()
        .record
        .take()
        .map(|record| *record)
        .unwrap_or_default();
    (result, record)
}
