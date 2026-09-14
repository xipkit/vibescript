use crate::{CallContext, Result, budget::Charge, hash::Hash, syntax::modules::Visibility};
use std::sync::Arc;

#[derive(Debug)]
pub(crate) struct Method {
    pub name: String,
    pub function: usize,
    pub visibility: Visibility,
}

#[derive(Debug)]
pub(crate) struct Definition {
    pub index: usize,
    pub name: String,
    pub methods: Vec<Method>,
    pub nested: Vec<(String, usize)>,
    pub body: Option<usize>,
    bytes: usize,
}

impl Definition {
    pub fn new(
        index: usize,
        name: String,
        methods: Vec<Method>,
        nested: Vec<(String, usize)>,
        body: Option<usize>,
    ) -> Arc<Self> {
        let bytes = size_of::<Self>()
            + 2 * size_of::<usize>()
            + name.capacity()
            + methods.capacity() * size_of::<Method>()
            + methods.iter().map(|m| m.name.capacity()).sum::<usize>()
            + nested.capacity() * size_of::<(String, usize)>()
            + nested
                .iter()
                .map(|(name, _)| name.capacity())
                .sum::<usize>();
        Arc::new(Self {
            index,
            name,
            methods,
            nested,
            body,
            bytes,
        })
    }
}

#[derive(Debug)]
pub(crate) struct Namespace {
    pub definition: Arc<Definition>,
    header: Option<Charge>,
    _metadata: Option<Charge>,
}

impl Namespace {
    pub fn untracked(definition: Arc<Definition>) -> Arc<Self> {
        Arc::new(Self {
            definition,
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
        Ok(Arc::new(Self {
            definition: value.definition.clone(),
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
    Equality,
    Class,
    Respond(bool),
}
