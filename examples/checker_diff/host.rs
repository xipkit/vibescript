//! The host a generated program runs against: the globals it declares, the
//! capabilities and host functions it grants, and the calls it makes into
//! the script with arguments. Every build of a program declares the same
//! host, so the build that keeps every check sees the same values.

use vibescript::{
    CallContext, CallOptions, Capability, Engine, HostCall, HostMethod, Result, Signature,
    SignatureParam, Value,
};

/// A global the host declares with a type, empty for `any`, and supplies
/// with a value, written as JSON.
#[derive(Clone, Debug, PartialEq)]
pub struct Global {
    pub name: String,
    pub ty: String,
    pub value: String,
}

/// A call the host makes into the script after running it: a function and
/// its positional and keyword arguments, as JSON.
#[derive(Clone, Debug, PartialEq)]
pub struct Call {
    pub function: String,
    pub args: String,
}

/// What a program's host declares and does.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Host {
    pub globals: Vec<Global>,
    pub capabilities: Vec<String>,
    pub calls: Vec<Call>,
}

impl Host {
    pub fn is_empty(&self) -> bool {
        self.globals.is_empty() && self.capabilities.is_empty() && self.calls.is_empty()
    }

    /// The directives [`super::harness::Case::render`] writes before the
    /// program's files.
    pub fn render(&self) -> String {
        let mut text = String::new();
        for global in &self.globals {
            if global.ty.is_empty() {
                text.push_str(&format!("#@ global {} = {}\n", global.name, global.value));
            } else {
                text.push_str(&format!(
                    "#@ global {}: {} = {}\n",
                    global.name, global.ty, global.value
                ));
            }
        }
        for name in &self.capabilities {
            text.push_str(&format!("#@ capability {name}\n"));
        }
        for call in &self.calls {
            text.push_str(&format!("#@ call {} {}\n", call.function, call.args));
        }
        text
    }

    /// Reads one directive line, without its `#@ ` prefix; `false` when it
    /// is not one of these.
    pub fn parse_directive(&mut self, line: &str) -> bool {
        if let Some(rest) = line.strip_prefix("global ") {
            let (head, value) = rest.split_once(" = ").unwrap_or((rest, "null"));
            let (name, ty) = head.split_once(": ").unwrap_or((head, ""));
            self.globals.push(Global {
                name: name.trim().to_owned(),
                ty: ty.trim().to_owned(),
                value: value.trim().to_owned(),
            });
            true
        } else if let Some(name) = line.strip_prefix("capability ") {
            self.capabilities.push(name.trim().to_owned());
            true
        } else if let Some(rest) = line.strip_prefix("call ") {
            let (function, args) = rest.split_once(' ').unwrap_or((rest, "{}"));
            self.calls.push(Call {
                function: function.trim().to_owned(),
                args: args.trim().to_owned(),
            });
            true
        } else {
            false
        }
    }

    /// Declares the globals and capabilities on `engine`, and registers the
    /// host functions.
    pub fn declare(&self, engine: &mut Engine) -> Result<()> {
        register(engine)?;
        for global in &self.globals {
            engine.declare_global(global.name.clone(), &global.ty)?;
        }
        for name in &self.capabilities {
            if let Some(capability) = capability(name) {
                engine.declare_capability(&capability)?;
            }
        }
        Ok(())
    }

    /// Supplies the globals and grants the capabilities to one call.
    pub fn grant(&self, options: &mut CallOptions) {
        for global in &self.globals {
            options
                .globals
                .insert(global.name.clone(), json(&global.value));
        }
        for name in &self.capabilities {
            if let Some(capability) = capability(name) {
                options.capabilities.push(capability);
            }
        }
    }
}

/// The value JSON text spells, or `nil` when it does not parse.
pub fn json(text: &str) -> Value {
    vibescript::parse_json(text.as_bytes(), CallOptions::default())
        .map(|outcome| outcome.value)
        .unwrap_or_else(|_| Value::nil())
}

/// A call's positional arguments and keywords from `{"args": [...],
/// "keywords": {...}}`.
pub fn arguments(text: &str) -> (Vec<Value>, Vec<(String, Value)>) {
    let parsed = json(text);
    let mut args = Vec::new();
    let mut keywords = Vec::new();
    for (key, value) in parsed.as_hash().unwrap_or_default() {
        match key.as_bytes() {
            Some(b"args") => args.extend(value.as_array().unwrap_or_default().iter().cloned()),
            Some(b"keywords") => {
                for (name, value) in value.as_hash().unwrap_or_default() {
                    let name = String::from_utf8_lossy(name.as_bytes().unwrap_or_default());
                    keywords.push((name.into_owned(), value.clone()));
                }
            }
            _ => {}
        }
    }
    (args, keywords)
}

fn signature(params: &[(&str, &str, bool)], result: &str, accepts_block: bool) -> Signature {
    Signature {
        params: params
            .iter()
            .map(|&(name, ty, optional)| SignatureParam {
                name: name.to_owned(),
                ty: ty.to_owned(),
                optional,
            })
            .collect(),
        result: result.to_owned(),
        accepts_block,
    }
}

fn int(value: Option<&Value>) -> i64 {
    value.and_then(Value::as_int).unwrap_or(0)
}

fn ints(value: Option<&Value>) -> Vec<Value> {
    value
        .and_then(Value::as_array)
        .map(<[Value]>::to_vec)
        .unwrap_or_default()
}

fn method(
    name: &str,
    params: &[(&str, &str, bool)],
    result: &str,
    callback: impl Fn(&mut CallContext, &[Value]) -> Result<Value> + Send + Sync + 'static,
) -> Value {
    HostMethod::new(name, move |ctx, args, _| callback(ctx, args))
        .with_signature(signature(params, result, false))
        .expect("a valid signature")
        .value()
}

fn block_method(
    name: &str,
    params: &[(&str, &str, bool)],
    result: &str,
    callback: impl Fn(&mut HostCall<'_>, &[Value]) -> Result<Value> + Send + Sync + 'static,
) -> Value {
    HostMethod::new_with_block(name, move |call, args, _| callback(call, args))
        .with_signature(signature(params, result, true))
        .expect("a valid signature")
        .value()
}

/// A capability of the catalog, by name.
pub fn capability(name: &str) -> Option<Capability> {
    let value = match name {
        "store" => store(),
        "loose" => loose(),
        _ => return None,
    };
    Some(Capability::from_value(name, value))
}

/// Methods with published signatures, data and a nested namespace.
fn store() -> Value {
    let get = method(
        "store.get",
        &[("key", "string", false)],
        "int?",
        |_, args| {
            Ok(match args.first().and_then(Value::as_bytes) {
                Some(b"a") => Value::int(1),
                Some(b"b") => Value::int(2),
                _ => Value::nil(),
            })
        },
    );
    let put = method(
        "store.put",
        &[("key", "string", false), ("value", "int", false)],
        "int",
        |_, args| Ok(Value::int(int(args.get(1)))),
    );
    let keys = method("store.keys", &[], "array<string>", |_, _| {
        Ok(Value::array(vec![Value::bytes("a"), Value::bytes("b")]))
    });
    let total = method(
        "store.total",
        &[("values", "array<int>", false), ("scale", "int", true)],
        "int",
        |_, args| {
            let sum: i64 = ints(args.first()).iter().filter_map(Value::as_int).sum();
            let scale = args.get(1).and_then(Value::as_int).unwrap_or(1);
            Ok(Value::int(sum.wrapping_mul(scale)))
        },
    );
    let lookup = method(
        "store.lookup",
        &[("key", "string", false)],
        "",
        |_, args| {
            Ok(match args.first().and_then(Value::as_bytes) {
                Some(b"n") => Value::int(7),
                Some(b"s") => Value::bytes("seven"),
                Some(b"l") => Value::array(vec![Value::int(1), Value::bytes("x")]),
                _ => Value::nil(),
            })
        },
    );
    let each = block_method(
        "store.each",
        &[("items", "array<int>", false)],
        "int",
        |call, args| {
            let items = ints(args.first());
            for item in &items {
                if call.block_given() {
                    call.call_block(std::slice::from_ref(item))?;
                }
            }
            Ok(Value::int(items.len() as i64))
        },
    );
    let collect = block_method(
        "store.collect",
        &[("items", "array<int>", false)],
        "array<any>",
        |call, args| {
            let mut out = Vec::new();
            for item in ints(args.first()) {
                out.push(if call.block_given() {
                    call.call_block(&[item])?
                } else {
                    item
                });
            }
            Ok(Value::array(out))
        },
    );
    let first = block_method(
        "store.first",
        &[("items", "array<int>", false)],
        "int?",
        |call, args| {
            for item in ints(args.first()) {
                if !call.block_given() || call.call_block(std::slice::from_ref(&item))?.truthy() {
                    return Ok(item);
                }
            }
            Ok(Value::nil())
        },
    );
    let version = method("store.meta.version", &[], "string", |_, _| {
        Ok(Value::bytes("1.2"))
    });
    Value::object(vec![
        (b"get".to_vec(), get),
        (b"put".to_vec(), put),
        (b"keys".to_vec(), keys),
        (b"total".to_vec(), total),
        (b"lookup".to_vec(), lookup),
        (b"each".to_vec(), each),
        (b"collect".to_vec(), collect),
        (b"first".to_vec(), first),
        (b"limit".to_vec(), Value::int(3)),
        (b"label".to_vec(), Value::bytes("main")),
        (
            b"meta".to_vec(),
            Value::object(vec![(b"version".to_vec(), version)]),
        ),
    ])
}

/// Methods without signatures, which take and return `any`.
fn loose() -> Value {
    let echo = HostMethod::new("loose.echo", |_, args, _| {
        Ok(args.first().cloned().unwrap_or_else(Value::nil))
    })
    .value();
    let pair = HostMethod::new("loose.pair", |_, args, _| {
        Ok(Value::array(args.iter().take(2).cloned().collect()))
    })
    .value();
    let visit = HostMethod::new_with_block("loose.visit", |call, args, _| {
        let mut last = Value::nil();
        for item in ints(args.first()) {
            if call.block_given() {
                last = call.call_block(&[item])?;
            }
        }
        Ok(last)
    })
    .value();
    Value::object(vec![
        (b"echo".to_vec(), echo),
        (b"pair".to_vec(), pair),
        (b"visit".to_vec(), visit),
    ])
}

/// Registers the host functions scripts call by name.
fn register(engine: &mut Engine) -> Result<()> {
    engine.register_method(
        "twice",
        HostMethod::new("twice", |_, args, _| {
            Ok(Value::int(int(args.first()).wrapping_mul(2)))
        })
        .with_signature(signature(&[("n", "int", false)], "int", false))?,
    );
    engine.register_method(
        "joined",
        HostMethod::new("joined", |ctx, args, _| {
            let mut text = Vec::new();
            for arg in args {
                text.extend_from_slice(arg.as_bytes().unwrap_or_default());
            }
            ctx.bytes(&text)
        })
        .with_signature(signature(
            &[("a", "string", false), ("b", "string", true)],
            "string",
            false,
        ))?,
    );
    engine.register_method(
        "around",
        HostMethod::new_with_block("around", |call, args, _| {
            let value = args.first().cloned().unwrap_or_else(Value::nil);
            if call.block_given() {
                call.call_block(&[value])
            } else {
                Ok(value)
            }
        })
        .with_signature(signature(&[("value", "int", false)], "", true))?,
    );
    Ok(())
}
