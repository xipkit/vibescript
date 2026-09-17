use crate::{Error, ErrorKind, Result, types::Type};
use std::sync::Arc;

/// A declarative positional contract for a host method.
///
/// Type spellings use script annotation syntax. Empty spellings leave values
/// unconstrained. Contracts reject keywords and enforce types at runtime;
/// optional parameters remain omitted rather than receiving a default value.
///
/// ```
/// use vibescript::{CallOptions, Engine, HostMethod, Signature, SignatureParam};
/// let echo = HostMethod::new("echo", |_, args, _| Ok(args[0].clone()))
///     .with_signature(Signature {
///         params: vec![SignatureParam {
///             name: "text".into(), ty: "string".into(), optional: false,
///         }],
///         result: "string".into(),
///         accepts_block: false,
///     })?;
/// let mut engine = Engine::new();
/// engine.register_method("echo", echo);
/// let output = engine.compile("echo(\"hello\")")?.run(CallOptions::default())?;
/// assert_eq!(output.value.as_bytes(), Some(b"hello".as_slice()));
/// assert!(engine.compile("echo(7)")?.run(CallOptions::default()).is_err());
/// # Ok::<(), vibescript::Error>(())
/// ```
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Signature {
    /// Positional parameters, with optional parameters after required ones.
    pub params: Vec<SignatureParam>,
    /// The result's annotation spelling, or empty for an unconstrained result.
    pub result: String,
    /// Whether callers may attach a literal or forwarded block.
    pub accepts_block: bool,
}

/// One positional parameter in a host method's signature.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SignatureParam {
    /// A diagnostic label; an empty label uses the one-based parameter index.
    pub name: String,
    /// An annotation spelling, or empty for an unconstrained parameter.
    pub ty: String,
    /// Whether callers may omit this parameter.
    pub optional: bool,
}

pub(crate) struct Compiled {
    pub source: Signature,
    pub params: Vec<Option<Type>>,
    pub result: Option<Type>,
    pub required: usize,
    pub bytes: usize,
}

impl Compiled {
    pub fn new(name: &str, source: Signature) -> Result<Arc<Self>> {
        let mut params = Vec::with_capacity(source.params.len());
        let mut required = 0;
        let mut optional = false;
        for (index, param) in source.params.iter().enumerate() {
            if param.optional {
                optional = true;
            } else if optional {
                return Err(Error::new(
                    ErrorKind::Argument,
                    format!("signature for {name}: optional parameters must trail required ones"),
                ));
            } else {
                required = index + 1;
            }
            params.push(parse(&param.ty).map_err(|error| {
                let label = if param.name.is_empty() {
                    (index + 1).to_string()
                } else {
                    param.name.clone()
                };
                Error::new(
                    error.kind,
                    format!("signature for {name} parameter {label}: {}", error.message),
                )
            })?);
        }
        let result = parse(&source.result).map_err(|error| {
            Error::new(
                error.kind,
                format!("signature for {name} result: {}", error.message),
            )
        })?;
        let bytes = size_of::<Self>()
            + 2 * size_of::<usize>()
            + source.params.capacity() * size_of::<SignatureParam>()
            + source
                .params
                .iter()
                .map(|param| param.name.capacity() + param.ty.capacity())
                .sum::<usize>()
            + source.result.capacity()
            + params.capacity() * size_of::<Option<Type>>()
            + params
                .iter()
                .flatten()
                .map(crate::shapes::retained)
                .sum::<usize>()
            + result.as_ref().map_or(0, crate::shapes::retained);
        Ok(Arc::new(Self {
            source,
            params,
            result,
            required,
            bytes,
        }))
    }
}

fn parse(source: &str) -> Result<Option<Type>> {
    if source.is_empty() {
        Ok(None)
    } else {
        crate::syntax::parse_type(source).map(Some)
    }
}
