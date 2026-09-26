//! Runs golden corpus cases and prints one observation per line.
//!
//! Usage: `golden CASES.jsonl [START]`
//!
//! Each input line is one case: its source, entry point, arguments, host
//! configuration and limits. START skips that many cases. Each output line
//! records what the case observably did (its value or error, and its output
//! streams) and its accounting counters. `scripts/golden.py` drives this binary
//! and compares the records with the committed goldens. Records are flushed one
//! at a time so the driver can resume after the process dies.
//!
//! A case compiles with static types, declaring the globals and capabilities
//! it supplies by their values' types, as a statically typed host would. A
//! compilation that fails records its diagnostics' codes. A `parse` case,
//! such as a parse sweep's, records only whether its source parses.
use serde_json::{Value as Json, json};
use std::{
    collections::{BTreeMap, HashMap},
    fs,
    io::Write,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use vibescript::{CallOptions, Capability, Engine, Error, Limits, ModuleConfig, Value, parse_json};

#[path = "support/blocks.rs"]
mod blocks;
#[path = "support/probe.rs"]
mod probe;
#[path = "support/signatures.rs"]
mod signatures;
mod support;

type Capture = Arc<Mutex<Vec<u8>>>;

fn main() {
    // Deeply recursive cases need more native stack than the main thread has.
    let worker = std::thread::Builder::new()
        .stack_size(1 << 30)
        .spawn(run)
        .expect("spawn the golden worker");
    if let Err(message) = worker.join().expect("join the golden worker") {
        eprintln!("golden: {message}");
        std::process::exit(2);
    }
}

fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().collect();
    if !(2..=3).contains(&args.len()) {
        return Err("usage: golden CASES.jsonl [START]".into());
    }
    let text = fs::read_to_string(&args[1]).map_err(|e| format!("{}: {e}", args[1]))?;
    let start = match args.get(2) {
        Some(start) => start.parse().map_err(|e| format!("start: {e}"))?,
        None => 0,
    };
    std::panic::set_hook(Box::new(|_| {}));
    let mut out = std::io::stdout().lock();
    // Lines naming an `input` define argument sets that later cases share by name.
    let mut inputs = HashMap::new();
    let mut seen = 0;
    for line in text.lines() {
        let mut case: Json = serde_json::from_str(line).map_err(|e| format!("case: {e}"))?;
        if let Some(name) = case.get("input").and_then(Json::as_str) {
            inputs.insert(name.to_owned(), case);
            continue;
        }
        seen += 1;
        if seen <= start {
            continue;
        }
        if let Some(name) = case.get("inputs").and_then(Json::as_str) {
            let given: &Json = inputs
                .get(name)
                .ok_or_else(|| format!("undefined input {name}"))?;
            for field in ["typed_args", "typed_kwargs", "typed_globals"] {
                if let Some(value) = given.get(field) {
                    case[field] = value.clone();
                }
            }
        }
        let mut record =
            catch_unwind(AssertUnwindSafe(|| observe(&case))).unwrap_or_else(|payload| {
                let message = payload
                    .downcast_ref::<String>()
                    .cloned()
                    .or_else(|| payload.downcast_ref::<&str>().map(|s| s.to_string()))
                    .unwrap_or_default();
                json!({"phase": "panic", "error": {"message": message}})
            });
        record["id"] = case["id"].clone();
        writeln!(out, "{record}").map_err(|e| e.to_string())?;
        out.flush().map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn observe(case: &Json) -> Json {
    let stdout = Capture::default();
    let stderr = Capture::default();
    let mut record = execute(case, &stdout, &stderr);
    for (name, buffer) in [("stdout", &stdout), ("stderr", &stderr)] {
        let bytes = buffer.lock().unwrap();
        if !bytes.is_empty() {
            record[name] = Json::String(hex(&bytes));
        }
    }
    record
}

fn execute(case: &Json, stdout: &Capture, stderr: &Capture) -> Json {
    let source = case["source"].as_str().unwrap_or_default();
    if flag(case, "parse") {
        // Type checking fails only on a syntax error.
        return match Engine::new().type_check(source) {
            Ok(_) => json!({"phase": "compiled"}),
            Err(error) => compile_failure(source, &error),
        };
    }
    let mut engine = Engine::new();
    engine.set_strict_effects(flag(case, "strict_effects"));
    if case.get("module_paths").is_some() {
        let config = ModuleConfig {
            paths: strings(&case["module_paths"])
                .into_iter()
                .map(Into::into)
                .collect(),
            allow: strings(&case["module_allow"]),
            deny: strings(&case["module_deny"]),
            development: flag(case, "module_development"),
            ..ModuleConfig::default()
        };
        if let Err(error) = engine.set_module_config(config) {
            return failure("setup", &error);
        }
    }
    if let Some(byte) = case.get("entropy_byte").and_then(Json::as_u64) {
        let byte = byte as u8;
        engine.set_random_source(move |_, output| {
            output.fill(byte);
            Ok(output.len())
        });
    } else {
        // Seeded entropy makes random results, and what they cost, repeatable.
        let state = AtomicU64::new(0);
        engine.set_random_source(move |_, output| {
            for chunk in output.chunks_mut(8) {
                let seed = state.fetch_add(1, Ordering::Relaxed);
                chunk.copy_from_slice(&splitmix(seed).to_le_bytes()[..chunk.len()]);
            }
            Ok(output.len())
        });
    }
    // Without a writer, output helpers raise; some cases observe exactly that.
    if flag(case, "stdout") {
        let buffer = stdout.clone();
        engine.set_output_writer(move |_, bytes| {
            buffer.lock().unwrap().extend_from_slice(bytes);
            Ok(())
        });
    }
    if flag(case, "stderr") {
        let buffer = stderr.clone();
        engine.set_error_writer(move |_, bytes| {
            buffer.lock().unwrap().extend_from_slice(bytes);
            Ok(())
        });
    }
    let signature = match case
        .get("signature_probe")
        .map(|probe| signatures::configure(&mut engine, probe))
        .transpose()
    {
        Ok(method) => method,
        Err(error) => return failure("setup", &error),
    };
    // A statically typed host declares what each call supplies before compiling.
    let (options, args, keywords) = match inputs(case, signature.clone()) {
        Ok(inputs) => inputs,
        Err(message) => return json!({"phase": "setup", "error": {"message": message}}),
    };
    if let Err(error) = declare(&mut engine, case, &options, signature.as_ref()) {
        return failure("setup", &error);
    }
    let script = match engine.compile(source) {
        Ok(script) => script,
        Err(error) => return compile_failure(source, &error),
    };
    let Some(function) = case["function"].as_str() else {
        return json!({"phase": "compiled"});
    };
    match script.call_with_keywords(function, &args, &keywords, options) {
        Ok(outcome) => {
            let stats = outcome.stats;
            let mut record = json!({
                "phase": "ok",
                "value": typed(&outcome.value, 0),
                "steps": stats.steps,
                "peak": stats.peak_memory_bytes,
                "retained": stats.retained_memory_bytes,
            });
            if flag(case, "json") {
                match support::encode(&outcome.value, "", codec_options()) {
                    Ok(bytes) => {
                        record["json"] = Json::String(String::from_utf8_lossy(&bytes).into());
                    }
                    Err(error) => record["json_error"] = Json::String(error.to_string()),
                }
            }
            record
        }
        Err(error) => failure("call", &error),
    }
}

/// A failed compilation, with the codes of its error diagnostics.
fn compile_failure(source: &str, error: &Error) -> Json {
    let mut record = failure("compile", error);
    let diagnostics: Vec<Json> = error
        .diagnostics()
        .iter()
        .filter(|diagnostic| diagnostic.is_error())
        .map(|diagnostic| {
            // A diagnostic in a required file has no position in this source.
            let at = diagnostic.file.is_none().then(|| {
                let at = diagnostic.span.position(source);
                [at.line, at.column]
            });
            json!({
                "code": diagnostic.code.to_string(),
                "at": at,
                "file": diagnostic.file.as_deref().map(String::from_utf8_lossy),
                "message": diagnostic.message,
            })
        })
        .collect();
    if let Some(first) = diagnostics.first() {
        record["error"]["code"] = first["code"].clone();
        record["diagnostics"] = Json::Array(diagnostics);
    }
    record
}

type Inputs = (CallOptions, Vec<Value>, Vec<(String, Value)>);

/// Declares the globals and capabilities a call supplies, each typed by its
/// value, as a statically typed host would. A capability built when a call
/// starts is declared by a fresh value of the same kind.
fn declare(
    engine: &mut Engine,
    case: &Json,
    options: &CallOptions,
    signature: Option<&vibescript::HostMethod>,
) -> vibescript::Result<()> {
    let mut declared = Vec::new();
    if flag(case, "capability_probe") {
        declared.push(Capability::from_value("host", probe::template()));
    }
    if flag(case, "block_probe") {
        declared.push(Capability::from_value("blocks", blocks::template()));
    }
    for name in strings(&case["notifications"]) {
        declared.push(support::notification(&name)?);
    }
    if let Some(method) = signature
        && case["signature_probe"]["registration"]
            .as_str()
            .is_none_or(|registration| registration == "capability")
    {
        declared.push(Capability::from_value(
            "typed",
            Value::object(vec![(b"echo".to_vec(), method.value())]),
        ));
    }
    // A global of a capability's name overrides it, as it does at runtime.
    for (name, value) in &options.globals {
        declared.push(Capability::from_value(name.clone(), value.clone()));
    }
    for capability in &declared {
        engine.declare_capability(capability)?;
    }
    Ok(())
}

fn inputs(case: &Json, signature: Option<vibescript::HostMethod>) -> Result<Inputs, String> {
    let mut args = Vec::new();
    for arg in case["args"].as_array().into_iter().flatten() {
        args.push(json_value(arg)?);
    }
    for node in case["typed_args"].as_array().into_iter().flatten() {
        args.push(decode(node)?);
    }
    let mut keywords = Vec::new();
    for pair in case["typed_kwargs"].as_array().into_iter().flatten() {
        keywords.push((
            pair[0].as_str().unwrap_or_default().to_owned(),
            decode(&pair[1])?,
        ));
    }
    let mut globals = BTreeMap::new();
    for (name, value) in case["globals"].as_object().into_iter().flatten() {
        globals.insert(name.clone(), json_value(value)?);
    }
    for pair in case["typed_globals"].as_array().into_iter().flatten() {
        let name = pair[0].as_str().unwrap_or_default().to_owned();
        globals.insert(name, decode(&pair[1])?);
    }
    let mut capabilities = Vec::new();
    if flag(case, "capability_probe") {
        capabilities.push(probe::capability());
    }
    if flag(case, "block_probe") {
        capabilities.push(blocks::capability());
    }
    for name in strings(&case["notifications"]) {
        capabilities.push(support::notification(&name).map_err(|e| e.to_string())?);
    }
    let metered = case["accounting"].as_bool().unwrap_or(true);
    let limit = |name: &str, default: u64| match case.get(name) {
        Some(Json::Null) => None,
        Some(value) => value.as_u64(),
        None => metered.then_some(default),
    };
    let timeout = case["timeout_ms"].as_u64().unwrap_or(60_000);
    let mut options = CallOptions {
        globals,
        capabilities,
        allow_require: flag(case, "allow_require"),
        limits: Limits {
            steps: limit("steps", 5_000_000),
            memory_bytes: limit("memory", 64 << 20).map(|bytes| bytes as usize),
            recursion: case["recursion"]
                .as_u64()
                .map_or(256, |depth| depth as usize),
        },
        deadline: Some(Instant::now() + Duration::from_millis(timeout)),
        ..CallOptions::default()
    };
    if let Some(method) = signature {
        signatures::bind(&mut options, &case["signature_probe"], method);
    }
    Ok((options, args, keywords))
}

fn failure(phase: &str, error: &Error) -> Json {
    let mut detail = json!({
        "kind": format!("{:?}", error.kind),
        "class": error.class().map(|class| class.name()),
    });
    match std::str::from_utf8(error.message_bytes()) {
        Ok(text) => detail["message"] = Json::String(text.into()),
        Err(_) => detail["message_hex"] = Json::String(hex(error.message_bytes())),
    }
    if let Some(diagnostic) = &error.diagnostic {
        detail["at"] = json!([diagnostic.position.line, diagnostic.position.column]);
    }
    json!({"phase": phase, "error": detail})
}

fn json_value(value: &Json) -> Result<Value, String> {
    let bytes = serde_json::to_vec(value).map_err(|e| e.to_string())?;
    parse_json(&bytes, codec_options())
        .map(|outcome| outcome.value)
        .map_err(|e| e.to_string())
}

fn codec_options() -> CallOptions {
    CallOptions {
        limits: Limits {
            steps: Some(5_000_000),
            memory_bytes: Some(64 << 20),
            ..Limits::default()
        },
        ..CallOptions::default()
    }
}

fn flag(case: &Json, name: &str) -> bool {
    case[name].as_bool().unwrap_or(false)
}

fn strings(value: &Json) -> Vec<String> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| item.as_str().map(str::to_owned))
        .collect()
}

/// Decodes a typed-v1 node into a host value.
fn decode(node: &Json) -> Result<Value, String> {
    let kind = node[0].as_str().ok_or("typed node without a kind")?;
    let text = |index: usize| {
        node[index]
            .as_str()
            .ok_or_else(|| format!("malformed {kind} node"))
    };
    let number = |index: usize| {
        text(index)?
            .parse::<i64>()
            .map_err(|e| format!("{kind}: {e}"))
    };
    Ok(match kind {
        "nil" => Value::nil(),
        "bool" => Value::boolean(node[1].as_bool().ok_or("malformed bool node")?),
        "int" => match text(1)?.parse::<i64>() {
            Ok(value) => Value::int(value),
            Err(_) => Value::parse_integer(text(1)?, 10).map_err(|e| e.to_string())?,
        },
        "float" => Value::float(f64::from_bits(
            u64::from_str_radix(text(1)?, 16).map_err(|e| e.to_string())?,
        )),
        "string" => Value::bytes(unhex(text(1)?)?),
        "symbol" => Value::symbol(unhex(text(1)?)?),
        "money" => Value::money(number(2)?, text(1)?).map_err(|e| e.to_string())?,
        "duration" => Value::duration(number(1)?),
        "time" => {
            let nanos = text(2)?.parse::<u32>().map_err(|e| e.to_string())?;
            Value::time(number(1)?, nanos).map_err(|e| e.to_string())?
        }
        "range" => {
            let bound = |index: usize| match &node[index] {
                Json::Null => Ok(None),
                Json::String(text) => text.parse::<i64>().map(Some).map_err(|e| e.to_string()),
                _ => Err("malformed range node".to_owned()),
            };
            Value::range(bound(1)?, bound(2)?, node[3].as_bool().unwrap_or(false))
        }
        "array" => Value::array(
            node[1]
                .as_array()
                .ok_or("malformed array node")?
                .iter()
                .map(decode)
                .collect::<Result<_, _>>()?,
        ),
        "hash" | "object" => {
            let mut entries = Vec::new();
            for entry in node[1].as_array().ok_or("malformed hash node")? {
                let key = unhex(entry[0].as_str().ok_or("malformed hash key")?)?;
                entries.push((key, decode(&entry[1])?));
            }
            if kind == "hash" {
                Value::hash(entries)
            } else {
                Value::object(entries)
            }
        }
        other => return Err(format!("unsupported typed node {other}")),
    })
}

/// Encodes a result as a typed-v1 node, which keeps value kinds, exact float
/// bits, raw bytes and hash order. Values without a data form are rendered.
fn typed(value: &Value, depth: usize) -> Json {
    if depth > 256 {
        return json!(["too_deep"]);
    }
    let kind = value.type_name();
    match kind {
        "nil" => json!([kind]),
        "bool" => json!([kind, value.truthy()]),
        "int" => json!([kind, value.to_string()]),
        // Scripts cannot observe a NaN's sign or payload, and CPUs produce different ones.
        "float" => match value.as_float().unwrap() {
            float if float.is_nan() => json!([kind, "nan"]),
            float => json!([kind, format!("{:016x}", float.to_bits())]),
        },
        "string" | "symbol" => json!([kind, hex(value.as_bytes().unwrap())]),
        "money" => {
            let (cents, currency) = value.as_money().unwrap();
            json!([kind, currency, cents.to_string()])
        }
        "duration" => json!([kind, value.as_duration().unwrap().to_string()]),
        "time" => match value.as_time() {
            Some((seconds, nanos)) => json!([kind, seconds.to_string(), nanos.to_string()]),
            None => json!(["opaque", kind, value.to_string()]),
        },
        "range" => match value.as_range() {
            Some((start, end, exclusive)) => json!([
                kind,
                start.map(|v| v.to_string()),
                end.map(|v| v.to_string()),
                exclusive
            ]),
            None => json!(["opaque", kind, value.to_string()]),
        },
        "array" => {
            let items: Vec<_> = value
                .as_array()
                .unwrap()
                .iter()
                .map(|item| typed(item, depth + 1))
                .collect();
            json!([kind, items])
        }
        "hash" | "object" => {
            let entries: Vec<_> = value
                .as_hash()
                .unwrap()
                .iter()
                .map(|(key, item)| json!([hex(key.as_bytes().unwrap()), typed(item, depth + 1)]))
                .collect();
            json!([kind, entries])
        }
        _ => json!(["opaque", kind, value.to_string()]),
    }
}

/// The SplitMix64 output for one counter value.
fn splitmix(counter: u64) -> u64 {
    let mut z = counter.wrapping_add(1).wrapping_mul(0x9e37_79b9_7f4a_7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(DIGITS[usize::from(byte >> 4)] as char);
        output.push(DIGITS[usize::from(byte & 15)] as char);
    }
    output
}

fn unhex(text: &str) -> Result<Vec<u8>, String> {
    if text.len() % 2 != 0 {
        return Err("odd hex length".into());
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).map_err(|e| e.to_string()))
        .collect()
}
