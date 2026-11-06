use super::*;

struct Key {
    bytes: [u8; 22],
    start: usize,
}

impl Key {
    fn new(kind: u8, mut index: usize) -> Access<Self> {
        let mut key = Self {
            bytes: [0; 22],
            start: 22,
        };
        // Twenty digits hold any usize, with room for the kind and a lead byte.
        loop {
            key.start -= 1;
            if let Some(byte) = key.bytes.get_mut(key.start) {
                *byte = b'0' + crate::checked::convert::<u8, _>(index % 10)?;
            }
            index /= 10;
            if index == 0 {
                break;
            }
        }
        key.start -= 1;
        if let Some(byte) = key.bytes.get_mut(key.start) {
            *byte = kind;
        }
        key.start -= 1;
        Ok(key)
    }

    fn text(&self) -> Access<&str> {
        std::str::from_utf8(self.bytes.tail(self.start)?)
            .map_err(|_| violation("scope key must be UTF-8"))
    }
}

pub(super) struct NamespaceState {
    pub fields: Arc<crate::objects::Instance>,
    pub initialized: bool,
    pub fresh: bool,
}

pub(super) fn namespace(
    ctx: &mut CallContext,
    environment: &Arc<crate::objects::Instance>,
    index: usize,
) -> Result<NamespaceState> {
    let key = Key::new(b'n', index)?;
    let (fields, fresh) = match crate::objects::field(ctx, environment, key.text()?)? {
        Some(Value(Kind::Instance(fields))) => (fields, false),
        None => {
            let fields = crate::objects::environment(ctx)?;
            crate::objects::set(
                ctx,
                environment,
                key.text()?,
                &Value(Kind::Instance(fields.clone())),
            )?;
            (fields, true)
        }
        _ => return Err(Error::new(ErrorKind::Type, "invalid namespace environment")),
    };
    let initialized = crate::objects::field(ctx, environment, Key::new(b'i', index)?.text()?)?
        .is_some_and(|value| matches!(value.0, Kind::Bool(true)));
    Ok(NamespaceState {
        fields,
        initialized,
        fresh,
    })
}

pub(super) fn initialized(
    ctx: &mut CallContext,
    environment: &Arc<crate::objects::Instance>,
    index: usize,
) -> Result<()> {
    crate::objects::set(
        ctx,
        environment,
        Key::new(b'i', index)?.text()?,
        &Value::boolean(true),
    )
}
