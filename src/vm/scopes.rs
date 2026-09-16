use super::*;

struct Key {
    bytes: [u8; 22],
    start: usize,
}

impl Key {
    fn new(kind: u8, mut index: usize) -> Self {
        let mut key = Self {
            bytes: [0; 22],
            start: 22,
        };
        loop {
            key.start -= 1;
            key.bytes[key.start] = b'0' + (index % 10) as u8;
            index /= 10;
            if index == 0 {
                break;
            }
        }
        key.start -= 1;
        key.bytes[key.start] = kind;
        key.start -= 1;
        key
    }

    fn text(&self) -> &str {
        std::str::from_utf8(&self.bytes[self.start..]).unwrap()
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
    let key = Key::new(b'n', index);
    let (fields, fresh) = match crate::objects::field(ctx, environment, key.text())? {
        Some(Value(Kind::Instance(fields))) => (fields, false),
        None => {
            let fields = crate::objects::environment(ctx)?;
            crate::objects::set(
                ctx,
                environment,
                key.text(),
                &Value(Kind::Instance(fields.clone())),
            )?;
            (fields, true)
        }
        _ => return Err(Error::new(ErrorKind::Type, "invalid namespace environment")),
    };
    let initialized = crate::objects::field(ctx, environment, Key::new(b'i', index).text())?
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
        Key::new(b'i', index).text(),
        &Value::boolean(true),
    )
}
