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
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};
use vibescript::{CallOptions, Engine, Error, ErrorKind, Limits, ModuleConfig, Value, parse_json};

#[path = "support/blocks.rs"]
mod blocks;
#[path = "support/declare.rs"]
mod declare;
#[path = "support/probe.rs"]
mod probe;
#[path = "support/signatures.rs"]
mod signatures;
mod support;

/// One output stream of a case: the writer sends each write whole.
type Capture = mpsc::Sender<Vec<u8>>;

#[allow(
    clippy::disallowed_methods,
    reason = "the one thread of a synchronous harness, made for its stack and joined before main returns"
)]
fn main() {
    // Deeply recursive cases need more native stack than the main thread has.
    let result = std::thread::Builder::new()
        .stack_size(1 << 30)
        .spawn(run)
        .map_err(|error| format!("spawn the golden worker: {error}"))
        .and_then(|worker| {
            worker
                .join()
                .map_err(|_| "golden worker panicked".to_owned())
        })
        .and_then(|result| result);
    if let Err(message) = result {
        eprintln!("golden: {message}");
        std::process::exit(2);
    }
}

fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().collect();
    if !(2..=3).contains(&args.len()) {
        return Err("usage: golden CASES.jsonl [START]".into());
    }
    let path = args.get(1).ok_or("missing cases path")?;
    let text = fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
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
                    let fields = case.as_object_mut().ok_or("case must be an object")?;
                    fields.insert(field.into(), value.clone());
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
        let id = case.get("id").cloned().unwrap_or(Json::Null);
        let fields = record
            .as_object_mut()
            .ok_or("observation must be an object")?;
        fields.insert("id".into(), id);
        writeln!(out, "{record}").map_err(|e| e.to_string())?;
        out.flush().map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn observe(case: &Json) -> Json {
    let (stdout, stdout_written) = mpsc::channel();
    let (stderr, stderr_written) = mpsc::channel();
    let mut record = execute(case, &stdout, &stderr);
    for (name, written) in [("stdout", stdout_written), ("stderr", stderr_written)] {
        let bytes: Vec<u8> = written.try_iter().flatten().collect();
        if !bytes.is_empty() {
            if let Some(fields) = record.as_object_mut() {
                fields.insert(name.into(), Json::String(hex(&bytes)));
            }
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
        let Ok(byte) = u8::try_from(byte) else {
            return failure(
                "setup",
                &Error::new(ErrorKind::Argument, "entropy byte out of range"),
            );
        };
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
                for (target, byte) in chunk.iter_mut().zip(splitmix(seed).to_le_bytes()) {
                    *target = byte;
                }
            }
            Ok(output.len())
        });
    }
    // Without a writer, output helpers raise; some cases observe exactly that.
    if flag(case, "stdout") {
        let buffer = stdout.clone();
        engine.set_output_writer(move |_, bytes| {
            if !bytes.is_empty() {
                let _ = buffer.send(bytes.to_vec());
            }
            Ok(())
        });
    }
    if flag(case, "stderr") {
        let buffer = stderr.clone();
        engine.set_error_writer(move |_, bytes| {
            if !bytes.is_empty() {
                let _ = buffer.send(bytes.to_vec());
            }
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
    if let Err(error) = declare::declare(&mut engine, case, &options.globals, signature.as_ref()) {
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
            let value = match typed(&outcome.value, 0) {
                Ok(value) => value,
                Err(error) => return failure("encode", &error),
            };
            let mut record = json!({
                "phase": "ok",
                "value": value,
                "steps": stats.steps,
                "peak": stats.peak_memory_bytes,
                "retained": stats.retained_memory_bytes,
            });
            if flag(case, "json") {
                match support::encode(&outcome.value, "", codec_options()) {
                    Ok(bytes) => {
                        if let Some(fields) = record.as_object_mut() {
                            fields.insert(
                                "json".into(),
                                Json::String(String::from_utf8_lossy(&bytes).into()),
                            );
                        }
                    }
                    Err(error) => {
                        if let Some(fields) = record.as_object_mut() {
                            fields.insert("json_error".into(), Json::String(error.to_string()));
                        }
                    }
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
            let mut item = json!({
                "code": diagnostic.code.to_string(),
                "at": at,
                "file": diagnostic.file.as_deref().map(String::from_utf8_lossy),
                "message": diagnostic.message,
            });
            if diagnostic.code.area() == Some(vibescript::diagnostic::Area::Syntax) {
                if let Some(fields) = item.as_object_mut() {
                    fields.insert("span".into(), json!([diagnostic.span.start, diagnostic.span.end]));
                    let fixes: Vec<_> = diagnostic.fixes.iter().map(|fix| json!({
                        "message": fix.message,
                        "applicability": format!("{:?}", fix.applicability),
                        "edits": fix.edits.iter().map(|edit| json!({"span": [edit.span.start, edit.span.end], "replacement": edit.replacement})).collect::<Vec<_>>(),
                    })).collect();
                    fields.insert("fixes".into(), json!(fixes));
                }
            }
            item
        })
        .collect();
    if let Some(first) = diagnostics.first() {
        if let Some(fields) = record.get_mut("error").and_then(Json::as_object_mut) {
            fields.insert("code".into(), first["code"].clone());
        }
        if let Some(fields) = record.as_object_mut() {
            fields.insert("diagnostics".into(), Json::Array(diagnostics));
        }
    }
    record
}

type Inputs = (CallOptions, Vec<Value>, Vec<(String, Value)>);

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
            memory_bytes: limit("memory", 64 << 20)
                .map(usize::try_from)
                .transpose()
                .map_err(|_| "memory limit out of range")?,
            recursion: case["recursion"]
                .as_u64()
                .map(usize::try_from)
                .transpose()
                .map_err(|_| "recursion limit out of range")?
                .unwrap_or(256),
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
        Ok(text) => {
            if let Some(fields) = detail.as_object_mut() {
                fields.insert("message".into(), Json::String(text.into()));
            }
        }
        Err(_) => {
            if let Some(fields) = detail.as_object_mut() {
                fields.insert(
                    "message_hex".into(),
                    Json::String(hex(error.message_bytes())),
                );
            }
        }
    }
    if let Some(diagnostic) = &error.diagnostic {
        if let Some(fields) = detail.as_object_mut() {
            fields.insert(
                "at".into(),
                json!([diagnostic.position.line, diagnostic.position.column]),
            );
        }
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
fn typed(value: &Value, depth: usize) -> vibescript::Result<Json> {
    if depth > 256 {
        return Ok(json!(["too_deep"]));
    }
    let kind = value.type_name();
    let invalid = || Error::new(ErrorKind::Internal, format!("invalid {kind} value"));
    Ok(match kind {
        "nil" => json!([kind]),
        "bool" => json!([kind, value.truthy()]),
        "int" => json!([kind, value.to_string()]),
        // Scripts cannot observe a NaN's sign or payload, and CPUs produce different ones.
        "float" => match value.as_float().ok_or_else(invalid)? {
            float if float.is_nan() => json!([kind, "nan"]),
            float => json!([kind, format!("{:016x}", float.to_bits())]),
        },
        "string" | "symbol" => json!([kind, hex(value.as_bytes().ok_or_else(invalid)?)]),
        "money" => {
            let (cents, currency) = value.as_money().ok_or_else(invalid)?;
            json!([kind, currency, cents.to_string()])
        }
        "duration" => json!([kind, value.as_duration().ok_or_else(invalid)?.to_string()]),
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
                .ok_or_else(invalid)?
                .iter()
                .map(|item| typed(item, depth + 1))
                .collect::<vibescript::Result<_>>()?;
            json!([kind, items])
        }
        "hash" | "object" => {
            let entries: Vec<_> = value
                .as_hash()
                .ok_or_else(invalid)?
                .iter()
                .map(|(key, item)| {
                    let bytes = key.as_bytes().ok_or_else(|| {
                        Error::new(ErrorKind::Internal, "hash key must contain bytes")
                    })?;
                    Ok(json!([hex(bytes), typed(item, depth + 1)?]))
                })
                .collect::<vibescript::Result<_>>()?;
            json!([kind, entries])
        }
        _ => json!(["opaque", kind, value.to_string()]),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_results_preserve_data_kinds_bits_bytes_and_entry_order() -> Result<(), String> {
        for node in [
            json!(["nil"]),
            json!(["bool", false]),
            json!(["int", "123456789012345678901234567890"]),
            json!(["float", "8000000000000000"]),
            json!(["string", "ff00"]),
            json!(["symbol", "6162"]),
            json!(["money", "USD", "-123"]),
            json!(["duration", "-42"]),
            json!(["time", "1700000000", "123456789"]),
            json!(["range", null, "42", true]),
            json!(["array", [["int", "1"], ["array", [["bool", true]]]]]),
            json!(["hash", [["62", ["int", "2"]], ["61", ["int", "1"]]]]),
            json!(["object", [["ff", ["string", "00"]]]]),
        ] {
            let value = decode(&node)?;
            assert_eq!(typed(&value, 0).map_err(|e| e.to_string())?, node);
        }
        Ok(())
    }

    #[test]
    fn typed_results_normalize_nan_and_bound_nesting() -> vibescript::Result<()> {
        for bits in [0x7ff8_0000_0000_0001, 0xfff8_0000_0000_0042] {
            assert_eq!(
                typed(&Value::float(f64::from_bits(bits)), 0)?,
                json!(["float", "nan"])
            );
        }
        assert_eq!(typed(&Value::nil(), 256)?, json!(["nil"]));
        assert_eq!(typed(&Value::nil(), 257)?, json!(["too_deep"]));
        Ok(())
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
    let digit = |n: u8| char::from(if n < 10 { b'0' + n } else { b'a' + n - 10 });
    bytes
        .iter()
        .flat_map(|byte| [digit(byte >> 4), digit(byte & 15)])
        .collect()
}

fn unhex(text: &str) -> Result<Vec<u8>, String> {
    if text.len() % 2 != 0 {
        return Err("odd hex length".into());
    }
    text.as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let pair = std::str::from_utf8(pair).map_err(|e| e.to_string())?;
            u8::from_str_radix(pair, 16).map_err(|e| e.to_string())
        })
        .collect()
}
