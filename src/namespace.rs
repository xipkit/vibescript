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
    pub environment: Option<Arc<crate::objects::Instance>>,
    header: Option<Charge>,
    _metadata: Option<Arc<Charge>>,
}

impl Namespace {
    pub fn untracked(definition: Arc<Definition>) -> Arc<Self> {
        Arc::new(Self {
            definition,
            owner: None,
            environment: None,
            header: None,
            _metadata: None,
        })
    }

    pub fn import(ctx: &mut CallContext, value: &Arc<Self>) -> Result<Arc<Self>> {
        ctx.checkpoint()?;
        let source = match &value.environment {
            Some(environment) => Some(environment.clone()),
            None => Self::snapshot_environment(ctx, value)?,
        };
        let environment = if let Some(environment) = &source {
            if ctx.namespace_depth >= crate::budget::MAX_ENVIRONMENT_DEPTH {
                return ctx.guard(
                    crate::ErrorKind::Recursion,
                    "namespace environment nesting too deep",
                );
            }
            ctx.namespace_depth += 1;
            let environment = crate::objects::import(ctx, environment);
            ctx.namespace_depth -= 1;
            Some(environment?)
        } else {
            None
        };
        ctx.scoped_sources |= environment.is_some();
        if ctx.owns(&value.header)
            && value
                ._metadata
                .as_deref()
                .is_some_and(|charge| ctx.owns_charge(charge))
        {
            return match (&value.environment, environment) {
                (previous, Some(environment))
                    if previous
                        .as_ref()
                        .is_none_or(|previous| !Arc::ptr_eq(previous, &environment)) =>
                {
                    Self::with_environment(ctx, value, environment)
                }
                _ => Ok(value.clone()),
            };
        }
        let metadata = ctx
            .reserve(value.definition.bytes + size_of::<Charge>() + 2 * size_of::<usize>())?
            .map(Arc::new);
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
            environment,
            header,
            _metadata: metadata,
        }))
    }

    fn snapshot_environment(
        ctx: &mut CallContext,
        value: &Self,
    ) -> Result<Option<Arc<crate::objects::Instance>>> {
        if ctx.snapshot_objects.is_none() {
            return Ok(None);
        }
        let Some(owner) = value
            .owner
            .clone()
            .or_else(|| value.definition.owner.get().and_then(Weak::upgrade))
        else {
            return Ok(None);
        };
        let count = ctx
            .snapshot_namespaces
            .as_ref()
            .map_or(0, |bindings| bindings.data.len());
        for index in 0..count {
            ctx.charge(1)?;
            let (code, environment) = &ctx.snapshot_namespaces.as_ref().unwrap().data[index];
            if Arc::ptr_eq(code, &owner) {
                return Ok(Some(environment.clone()));
            }
        }
        Ok(None)
    }

    pub fn with_environment(
        ctx: &mut CallContext,
        value: &Arc<Self>,
        environment: Arc<crate::objects::Instance>,
    ) -> Result<Arc<Self>> {
        let header = ctx.reserve(size_of::<Self>() + 2 * size_of::<usize>())?;
        ctx.scoped_sources = true;
        Ok(Arc::new(Self {
            definition: value.definition.clone(),
            owner: value.owner.clone(),
            environment: Some(environment),
            header,
            _metadata: value._metadata.clone(),
        }))
    }

    pub fn same_binding(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.definition, &other.definition)
            && same_environment(self.environment.as_ref(), other.environment.as_ref())
    }

    /// Compares declaration identity while allowing isolated copies of its state.
    pub fn same_type(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.definition, &other.definition)
            && self
                .environment
                .as_ref()
                .map_or(0, |env| env.nominal_scope())
                == other
                    .environment
                    .as_ref()
                    .map_or(0, |env| env.nominal_scope())
    }
}

pub(crate) fn same_environment(
    left: Option<&Arc<crate::objects::Instance>>,
    right: Option<&Arc<crate::objects::Instance>>,
) -> bool {
    match (left, right) {
        (Some(a), Some(b)) => a.same(b),
        (None, None) => true,
        _ => false,
    }
}

pub(crate) struct State {
    pub program: usize,
    pub namespace: Arc<Namespace>,
    pub fields: Hash,
    pub backing: Option<Arc<crate::objects::Instance>>,
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
