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
    /// Any other statement, by index into the entry point's body.
    Statement(usize),
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
    let parsing = Parsing::<super::recovery::FailFast>::new(parser);
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

/// Parses source and lists the tokens the parser finally read, after its
/// regex and percent-literal re-reads.
pub(crate) fn tokens(source: &str) -> Result<Vec<crate::tooling::Token>> {
    tokens_within(source, &())
}

/// Lists the tokens of `source` as [`tokens`] does, charging the parse to
/// `work`.
pub(crate) fn tokens_within(
    source: &str,
    work: &dyn crate::compilation::Work,
) -> Result<Vec<crate::tooling::Token>> {
    let parsing = Parsing::<super::recovery::FailFast>::new(parser(source, work)?);
    parsing.run(Call::Program)?;
    Ok(token_list(source, &parsing.parser.into_inner(), work)?.0)
}

/// Parses source like [`super::parse`], also returning the tokens the parser
/// finally read, as [`tokens`] lists them, and the reservation of their
/// memory in `work`, which is to last as long as they do.
pub(crate) fn parse_with_tokens(
    source: &str,
    work: &dyn crate::compilation::Work,
) -> Result<(
    Declarations,
    Vec<crate::tooling::Token>,
    Option<crate::budget::Charge>,
)> {
    let parsing = Parsing::<super::recovery::FailFast>::new(parser(source, work)?);
    let mut declarations = match parsing.run(Call::Program)? {
        Parsed::Program(declarations) => declarations,
        _ => unreachable!(),
    };
    let parser = parsing.parser.into_inner();
    let (tokens, held) = token_list(source, &parser, work)?;
    declarations.interpolated = interpolated(&parser);
    Ok((declarations, tokens, held))
}

/// What the interpolations of the parser's tokens hold, at every depth.
fn interpolated(parser: &super::Parser<'_>) -> super::Interpolated {
    use super::lexer::{Part, Token};
    /// What is left to read of a string's parts, and whether the string is
    /// in another's interpolation, or of an interpolation's tokens.
    enum Level<'p, 'a> {
        Parts(std::slice::Iter<'p, Part<'a>>, bool),
        Tokens(std::slice::Iter<'p, super::lexer::Lexeme<'a>>),
    }
    let mut found = super::Interpolated::default();
    // One entry for each level of nesting, which the lexer bounds, so a
    // string of many interpolations is read one at a time.
    let mut levels = Vec::new();
    for lexeme in parser.tokens.range(0..parser.tokens.len()) {
        if let Token::Template(parts) = &lexeme.token {
            levels.push(Level::Parts(parts.iter(), false));
        }
        while let Some(level) = levels.last_mut() {
            match level {
                Level::Parts(parts, nested) => {
                    let nested = *nested;
                    match parts.next() {
                        Some(Part::Expr(tokens, _)) => {
                            found.tokens += tokens.len();
                            if nested {
                                found.bytes += std::mem::size_of::<std::ops::Range<usize>>();
                            }
                            levels.push(Level::Tokens(tokens.iter()));
                        }
                        Some(Part::Text(_)) => (),
                        None => {
                            levels.pop();
                        }
                    }
                }
                Level::Tokens(tokens) => match tokens.next().map(|lexeme| &lexeme.token) {
                    Some(Token::Bytes(bytes)) => found.bytes += bytes.len(),
                    Some(Token::Template(parts)) => levels.push(Level::Parts(parts.iter(), true)),
                    Some(_) => (),
                    None => {
                        levels.pop();
                    }
                },
            }
        }
    }
    found
}

/// The tokens the parser finally read, as the tooling lists them, with
/// the reservation of what they hold in `work`, made before they are built.
fn token_list(
    source: &str,
    parser: &super::Parser<'_>,
    work: &dyn crate::compilation::Work,
) -> Result<(Vec<crate::tooling::Token>, Option<crate::budget::Charge>)> {
    use super::lexer::{Part, Token};
    use crate::tooling::TokenKind;
    let payloads: usize = parser
        .tokens
        .range(0..parser.tokens.len())
        .map(|lexeme| match &lexeme.token {
            Token::Symbol(name) => name.len(),
            Token::QuotedSymbol(name) => name.len(),
            Token::Bytes(bytes) => bytes.len(),
            Token::Template(parts) => parts.len() * std::mem::size_of::<std::ops::Range<usize>>(),
            Token::Words(words) => words
                .entries
                .iter()
                .map(|entry| {
                    std::mem::size_of::<Option<Vec<u8>>>()
                        + entry
                            .iter()
                            .map(|part| match part {
                                Part::Text(bytes) => bytes.len(),
                                Part::Expr(..) => 0,
                            })
                            .sum::<usize>()
                })
                .sum(),
            _ => 0,
        })
        .sum();
    let held = work
        .reserve(parser.tokens.len() * std::mem::size_of::<crate::tooling::Token>() + payloads)?;
    // An entry's text, built in place rather than from a copy of each
    // part, so a long entry is held once.
    let text = |parts: &[Part<'_>]| {
        let mut length = 0;
        for part in parts {
            match part {
                Part::Text(bytes) => length += bytes.len(),
                Part::Expr(..) => return None,
            }
        }
        let mut text = Vec::with_capacity(length);
        for part in parts {
            if let Part::Text(bytes) = part {
                text.extend_from_slice(bytes.as_ref());
            }
        }
        Some(text)
    };
    let mut tokens = Vec::with_capacity(parser.tokens.len());
    for lexeme in parser.tokens.range(0..parser.tokens.len()) {
        let kind = match &lexeme.token {
            Token::Word(_) => TokenKind::Word,
            Token::Symbol(name) => TokenKind::Symbol {
                name: name.as_bytes().to_vec(),
                quoted: false,
            },
            Token::QuotedSymbol(name) => TokenKind::Symbol {
                name: name.as_ref().to_vec(),
                quoted: true,
            },
            Token::Int(_) | Token::BigInt(..) => TokenKind::Integer,
            Token::Float(_) => TokenKind::Float,
            Token::Bytes(bytes) => TokenKind::String(bytes.as_ref().to_vec()),
            Token::Template(parts) => TokenKind::Template(
                parts
                    .iter()
                    .filter_map(|part| match part {
                        Part::Expr(_, (start, end)) => Some(*start as usize..*end as usize - 1),
                        Part::Text(_) => None,
                    })
                    .collect(),
            ),
            Token::Words(words) => TokenKind::Words {
                symbols: words.symbol,
                entries: words.entries.iter().map(|entry| text(entry)).collect(),
            },
            Token::Regex(..) => TokenKind::Regex,
            Token::P(c) => TokenKind::Punct(*c),
            Token::Op(op) => TokenKind::Operator(op),
            Token::EndLine if source.as_bytes().get(lexeme.offset) == Some(&b';') => {
                TokenKind::Semicolon
            }
            Token::EndLine => TokenKind::Newline,
            Token::Invalid(_) => TokenKind::Invalid,
            Token::Eof => TokenKind::Eof,
        };
        tokens.push(crate::tooling::Token {
            kind,
            span: lexeme.offset..lexeme.end,
            line: lexeme.line,
        });
    }
    Ok((tokens, held))
}
