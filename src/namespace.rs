use crate::{CallContext, Result, Value, budget::Charge, hash::Hash, syntax::modules::Visibility};
use std::sync::{Arc, OnceLock, Weak};

#[derive(Debug)]
pub(crate) struct Method {
    pub name: String,
    pub function: usize,
    pub visibility: Visibility,
}

#[derive(Debug)]
pub(crate) struct Definition {
    pub owner: OnceLock<Weak<crate::code::Code>>,
    pub index: usize,
    pub name: String,
    pub methods: Vec<Method>,
    pub instance_methods: Vec<Method>,
    pub constructor: Option<(usize, bool)>,
    pub nested: Vec<(String, usize)>,
    pub body: Option<usize>,
    bytes: usize,
}

impl Definition {
    pub fn new(
        index: usize,
        name: String,
        methods: Vec<Method>,
        instance_methods: Vec<Method>,
        constructor: Option<(usize, bool)>,
        nested: Vec<(String, usize)>,
        body: Option<usize>,
    ) -> Arc<Self> {
        let bytes = size_of::<Self>()
            + 2 * size_of::<usize>()
            + name.capacity()
            + methods.capacity() * size_of::<Method>()
            + methods.iter().map(|m| m.name.capacity()).sum::<usize>()
            + instance_methods.capacity() * size_of::<Method>()
            + instance_methods
                .iter()
                .map(|m| m.name.capacity())
                .sum::<usize>()
            + nested.capacity() * size_of::<(String, usize)>()
            + nested
                .iter()
                .map(|(name, _)| name.capacity())
                .sum::<usize>();
        Arc::new(Self {
            owner: OnceLock::new(),
            index,
            name,
            methods,
            instance_methods,
            constructor,
            nested,
            body,
            bytes,
        })
    }
}

#[derive(Debug)]
pub(crate) struct Namespace {
    pub definition: Arc<Definition>,
    pub owner: Option<Arc<crate::code::Code>>,
    header: Option<Charge>,
    _metadata: Option<Charge>,
}

impl Namespace {
    pub fn untracked(definition: Arc<Definition>) -> Arc<Self> {
        Arc::new(Self {
            definition,
            owner: None,
            header: None,
            _metadata: None,
        })
    }

    pub fn import(ctx: &mut CallContext, value: &Arc<Self>) -> Result<Arc<Self>> {
        if ctx.owns(&value.header) {
            return Ok(value.clone());
        }
        let metadata = ctx.reserve(value.definition.bytes)?;
        let header = ctx.reserve(size_of::<Self>() + 2 * size_of::<usize>())?;
        let owner = value
            .owner
            .clone()
            .or_else(|| value.definition.owner.get().and_then(Weak::upgrade));
        if let Some(owner) = &owner {
            crate::code::Code::retain(ctx, owner)?;
        }
        Ok(Arc::new(Self {
            definition: value.definition.clone(),
            owner,
            header,
            _metadata: metadata,
        }))
    }
}

pub(crate) struct State {
    pub namespace: Arc<Namespace>,
    pub fields: Hash,
    pub initialized: bool,
}
#[derive(Clone, Copy, Debug)]
pub(crate) enum Helper {
    Equality(bool),
    Predicate(crate::members::introspection::Predicate, bool),
}

#[derive(Debug)]
pub(crate) struct Call {
    pub function: usize,
    pub receiver: Option<Value>,
    pub constructor: bool,
    pub ignore_arguments: bool,
}

impl From<usize> for Call {
    fn from(function: usize) -> Self {
        Self {
            function,
            receiver: None,
            constructor: false,
            ignore_arguments: false,
        }
    }
}
