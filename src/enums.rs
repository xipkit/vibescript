use crate::{
    CallContext, Error, ErrorKind, Result, Value,
    budget::{Buffer, CHUNK, Charge},
    bytecode::CallSite,
    compilation::{Name, Table},
    syntax::unicode,
    value::Kind,
};
use std::{cmp::Ordering, sync::Arc};

#[derive(Debug)]
pub(crate) struct Definition {
    pub name: String,
    pub members: Vec<MemberDefinition>,
    lookup: Vec<usize>,
    bytes: usize,
}

#[derive(Debug)]
pub(crate) struct MemberDefinition {
    pub name: String,
    pub symbol: String,
}

#[derive(Debug)]
pub(crate) struct Enumeration {
    pub definition: Arc<Definition>,
    // Accounting views may differ while the enum and its members keep one identity.
    identity: Arc<()>,
    header: Option<Charge>,
    _metadata: Option<Charge>,
}

#[derive(Debug)]
pub(crate) struct Member {
    pub enumeration: Arc<Enumeration>,
    pub index: usize,
    header: Option<Charge>,
}

pub(crate) fn compile<I>(
    name: String,
    members: I,
    work: &dyn crate::compilation::Work,
) -> Result<Value>
where
    I: IntoIterator<Item = String>,
    I::IntoIter: ExactSizeIterator,
{
    let members = members.into_iter();
    work.bytes(name.len())?;
    if name.ends_with('?') || crate::types::builtin_name(&name).is_some() {
        return Err(crate::syntax::unsupported(
            "invalid enum name or built-in type conflict",
        ));
    }
    let mut symbols = Table::new();
    let mut values = Vec::with_capacity(members.len());
    for name in members {
        work.bytes(name.len())?;
        let symbol = symbol(&name);
        if symbols
            .insert(work, Name::new(work, &symbol)?, ())?
            .is_some()
        {
            return Err(crate::syntax::unsupported(
                "enum members have the same normalized symbol",
            ));
        }
        values.push(MemberDefinition { name, symbol });
    }
    let mut lookup: Vec<_> = (0..values.len()).collect();
    work.charge(
        lookup
            .len()
            .saturating_mul(lookup.len().max(1).ilog2() as usize + 1),
    )?;
    lookup.sort_unstable_by(|&a, &b| values[a].name.cmp(&values[b].name));
    let bytes = size_of::<Definition>()
        + 2 * size_of::<usize>()
        + name.capacity()
        + values.capacity() * size_of::<MemberDefinition>()
        + lookup.capacity() * size_of::<usize>()
        + values
            .iter()
            .map(|v| v.name.capacity() + v.symbol.capacity())
            .sum::<usize>();
    Ok(Value(Kind::Enum(Arc::new(Enumeration {
        definition: Arc::new(Definition {
            name,
            members: values,
            lookup,
            bytes,
        }),
        identity: Arc::new(()),
        header: None,
        _metadata: None,
    }))))
}

fn symbol(name: &str) -> String {
    let mut output = String::new();
    let mut chars = name.chars().peekable();
    let mut previous = None;
    let mut underscore = false;
    while let Some(c) = chars.next() {
        if c == '_' {
            if !output.is_empty() && !underscore {
                output.push('_');
                underscore = true;
            }
        } else {
            if unicode::upper(c)
                && previous.is_some_and(|p| {
                    p != '_'
                        && (unicode::lower(p)
                            || unicode::digit(p)
                            || chars.peek().is_some_and(|&n| unicode::lower(n)))
                })
            {
                output.push('_');
            }
            output.push(crate::casing::map(c, false));
            underscore = false;
        }
        previous = Some(c);
    }
    output
}

impl Enumeration {
    pub(crate) fn identical(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.identity, &other.identity)
    }

    pub(crate) fn instantiate(ctx: &mut CallContext, value: &Arc<Self>) -> Result<Arc<Self>> {
        let identity = Rebind::resolve(ctx, &value.definition, true)?
            .expect("compiled enum belongs to the invocation");
        Self::view(ctx, value, identity)
    }

    pub(crate) fn fresh(ctx: &mut CallContext, value: &Arc<Self>) -> Result<Arc<Self>> {
        Self::view(ctx, value, Arc::new(()))
    }

    pub fn import(ctx: &mut CallContext, value: &Arc<Self>) -> Result<Arc<Self>> {
        if ctx.owns(&value.header) {
            return Ok(value.clone());
        }
        let identity = Rebind::resolve(ctx, &value.definition, false)?
            .unwrap_or_else(|| value.identity.clone());
        Self::view(ctx, value, identity)
    }

    fn view(ctx: &mut CallContext, value: &Arc<Self>, identity: Arc<()>) -> Result<Arc<Self>> {
        let metadata = ctx.reserve(value.definition.bytes)?;
        let header = ctx.reserve(size_of::<Self>() + 4 * size_of::<usize>())?;
        Ok(Arc::new(Self {
            definition: value.definition.clone(),
            identity,
            header,
            _metadata: metadata,
        }))
    }

    pub(crate) fn lookup_symbol(
        &self,
        ctx: &mut CallContext,
        symbol: &[u8],
    ) -> Result<Option<usize>> {
        for (index, member) in self.definition.members.iter().enumerate() {
            ctx.charge(1)?;
            if compare_names(ctx, member.symbol.as_bytes(), symbol)? == Ordering::Equal {
                return Ok(Some(index));
            }
        }
        Ok(None)
    }

    /// Finds a member by its exact declared name.
    pub(crate) fn lookup(&self, ctx: &mut CallContext, name: &[u8]) -> Result<Option<usize>> {
        let mut lower = 0;
        let mut upper = self.definition.lookup.len();
        while lower < upper {
            ctx.charge(1)?;
            let middle = lower + (upper - lower) / 2;
            let index = self.definition.lookup[middle];
            let candidate = self.definition.members[index].name.as_bytes();
            let order = compare_names(ctx, candidate, name)?;
            match order {
                Ordering::Less => lower = middle + 1,
                Ordering::Greater => upper = middle,
                Ordering::Equal => return Ok(Some(index)),
            }
        }
        Ok(None)
    }
}

// Incoming arguments and globals rebind to this script's enums. Callback results keep
// their source identity; declarations use the same invocation token as arguments.
pub(crate) struct Rebind {
    pub definitions: Option<Arc<[Arc<Definition>]>>,
    pub active: bool,
    identities: Buffer<(Arc<Definition>, Arc<()>)>,
    storage: Option<Charge>,
}

impl Default for Rebind {
    fn default() -> Self {
        Self {
            definitions: None,
            active: false,
            identities: Buffer::empty(),
            storage: None,
        }
    }
}

impl Rebind {
    fn resolve(
        ctx: &mut CallContext,
        definition: &Arc<Definition>,
        declared: bool,
    ) -> Result<Option<Arc<()>>> {
        if !declared && !ctx.enum_rebind.active {
            return Ok(None);
        }
        let mut rebind = std::mem::take(&mut ctx.enum_rebind);
        let result = rebind.identity(ctx, definition, declared);
        ctx.enum_rebind = rebind;
        result
    }

    fn identity(
        &mut self,
        ctx: &mut CallContext,
        definition: &Arc<Definition>,
        declared: bool,
    ) -> Result<Option<Arc<()>>> {
        if !declared {
            let Some(definitions) = &self.definitions else {
                return Ok(None);
            };
            let mut found = false;
            for candidate in definitions.iter() {
                ctx.charge(1)?;
                if Arc::ptr_eq(candidate, definition) {
                    found = true;
                    break;
                }
            }
            if !found {
                return Ok(None);
            }
        }
        for (cached, identity) in &self.identities.data {
            ctx.charge(1)?;
            if Arc::ptr_eq(cached, definition) {
                return Ok(Some(identity.clone()));
            }
        }
        self.identities
            .ensure(ctx, self.identities.data.len() + 1)?;
        let charge = ctx.reserve(2 * size_of::<usize>())?;
        Charge::merge(&mut self.storage, charge);
        let identity = Arc::new(());
        self.identities
            .data
            .push((definition.clone(), identity.clone()));
        Ok(Some(identity))
    }
}

impl Member {
    pub(crate) fn new(
        ctx: &mut CallContext,
        enumeration: Arc<Enumeration>,
        index: usize,
    ) -> Result<Arc<Self>> {
        let header = ctx.reserve(size_of::<Self>() + 2 * size_of::<usize>())?;
        Ok(Arc::new(Self {
            enumeration,
            index,
            header,
        }))
    }

    pub fn import(ctx: &mut CallContext, value: &Arc<Self>) -> Result<Arc<Self>> {
        if ctx.owns(&value.header) {
            return Ok(value.clone());
        }
        let enumeration = Enumeration::import(ctx, &value.enumeration)?;
        Self::new(ctx, enumeration, value.index)
    }

    pub fn definition(&self) -> &MemberDefinition {
        &self.enumeration.definition.members[self.index]
    }
}

pub(crate) fn compare_names(ctx: &mut CallContext, a: &[u8], b: &[u8]) -> Result<Ordering> {
    for (a, b) in a.chunks(CHUNK).zip(b.chunks(CHUNK)) {
        ctx.work_bytes(a.len().min(b.len()))?;
        let order = a.cmp(b);
        if order != Ordering::Equal {
            return Ok(order);
        }
    }
    Ok(a.len().cmp(&b.len()))
}

pub(crate) fn call(
    ctx: &mut CallContext,
    site: CallSite,
    name: &str,
    receiver: &Value,
    args: &[Value],
    keywords: bool,
    block: bool,
) -> Result<Option<Value>> {
    if !matches!(receiver.0, Kind::Enum(_) | Kind::EnumMember(_)) {
        return Ok(None);
    }
    ctx.checkpoint()?;
    if site.scope {
        let Kind::Enum(enumeration) = &receiver.0 else {
            return Err(Error::new(
                ErrorKind::Type,
                "scoped member access requires an enum type or namespace",
            ));
        };
        let index = enumeration
            .lookup(ctx, name.as_bytes())?
            .ok_or_else(|| Error::new(ErrorKind::Name, "unknown enum member"))?;
        if !site.auto {
            return Err(Error::new(
                ErrorKind::Type,
                "attempted to call non-callable enum value",
            ));
        }
        return Ok(Some(Value(Kind::EnumMember(Member::new(
            ctx,
            enumeration.clone(),
            index,
        )?))));
    }
    if matches!(
        name,
        "to_s" | "string" | "inspect" | "nil?" | "itself" | "dup"
    ) {
        crate::ops::arity(args, 0)?;
        if keywords || block {
            return Err(Error::new(
                ErrorKind::Argument,
                "enum conversions do not accept keyword arguments or blocks",
            ));
        }
        return if name == "nil?" {
            Ok(Some(Value::boolean(false)))
        } else if matches!(name, "itself" | "dup") {
            Ok(Some(receiver.clone()))
        } else {
            text(ctx, receiver).map(Some)
        };
    }
    let value = match (&receiver.0, name) {
        (Kind::Enum(e), "name") => ctx.bytes(e.definition.name.as_bytes())?,
        (Kind::EnumMember(m), "name") => ctx.bytes(m.definition().name.as_bytes())?,
        (Kind::EnumMember(m), "symbol") => {
            Value::copy_bytes(ctx, m.definition().symbol.as_bytes(), true)?
        }
        (Kind::EnumMember(m), "enum") => Value(Kind::Enum(m.enumeration.clone())),
        _ => return Ok(None),
    };
    if !site.auto {
        return Err(Error::new(
            ErrorKind::Type,
            "attempted to call non-callable enum property",
        ));
    }
    Ok(Some(value))
}

pub(crate) fn append(ctx: &mut CallContext, value: &Value, output: &mut Buffer<u8>) -> Result<()> {
    let parts: [&[u8]; 3] = match &value.0 {
        Kind::Enum(e) => [b"<Enum ", e.definition.name.as_bytes(), b">"],
        Kind::EnumMember(m) => [
            m.enumeration.definition.name.as_bytes(),
            b"::",
            m.definition().name.as_bytes(),
        ],
        _ => unreachable!(),
    };
    let Some(len) = parts
        .iter()
        .try_fold(output.data.len(), |n, part| n.checked_add(part.len()))
    else {
        return ctx.fail(ErrorKind::Memory, "enum rendering size overflow");
    };
    output.ensure(ctx, len)?;
    for part in parts {
        output.extend(ctx, part)?;
    }
    Ok(())
}

pub(crate) fn text(ctx: &mut CallContext, value: &Value) -> Result<Value> {
    let mut output = Buffer::empty();
    append(ctx, value, &mut output)?;
    Value::from_bytes(ctx, output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CallOptions, Limits};

    #[test]
    fn imports_charge_each_call_and_release_metadata_independently() {
        let original = compile("State".into(), vec!["LongMember".repeat(1024)], &()).unwrap();
        let mut first = CallContext::new(CallOptions::default());
        let imported = first.import(&original).unwrap();
        let retained = first.stats().retained_memory_bytes;
        assert!(retained > 20000);
        let mut second = CallContext::new(CallOptions::default());
        let another = second.import(&imported).unwrap();
        assert_eq!(second.stats().retained_memory_bytes, retained);
        let shared = second.import(&another).unwrap();
        assert_eq!(second.stats().retained_memory_bytes, retained);
        drop(imported);
        assert_eq!(first.stats().retained_memory_bytes, 0);
        drop(another);
        assert_eq!(second.stats().retained_memory_bytes, retained);
        drop(shared);
        assert_eq!(second.stats().retained_memory_bytes, 0);
    }

    #[test]
    fn long_names_are_metered_and_output_capacity_is_reserved_before_writing() {
        let name = "A".repeat(16384);
        let original = compile(name.clone(), vec![name.clone()], &()).unwrap();
        let Kind::Enum(enumeration) = &original.0 else {
            unreachable!()
        };
        let mut ctx = CallContext::new(CallOptions {
            limits: Limits {
                steps: Some(16),
                ..Limits::default()
            },
            ..CallOptions::default()
        });
        assert_eq!(
            enumeration
                .lookup(&mut ctx, name.as_bytes())
                .unwrap_err()
                .kind,
            ErrorKind::Steps
        );
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
        assert_eq!(ctx.checkpoint().unwrap_err().kind, ErrorKind::Steps);

        let mut ctx = CallContext::new(CallOptions {
            limits: Limits {
                memory_bytes: Some(1024),
                ..Limits::default()
            },
            ..CallOptions::default()
        });
        let mut output = Buffer::empty();
        output.extend(&mut ctx, b"prefix").unwrap();
        assert_eq!(
            append(&mut ctx, &original, &mut output).unwrap_err().kind,
            ErrorKind::Memory
        );
        assert_eq!(&output.data, b"prefix");
        assert_eq!(ctx.checkpoint().unwrap_err().kind, ErrorKind::Memory);
    }
}
