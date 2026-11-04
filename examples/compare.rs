use serde_json::{Value as Json, json};
use std::{
    collections::BTreeMap,
    fs,
    hint::black_box,
    sync::{Arc, Mutex},
    time::Instant,
};
use vibescript::{CallOptions, Engine, Limits, ModuleConfig, parse_json};

#[path = "support/blocks.rs"]
mod blocks;
#[path = "support/declare.rs"]
mod declare;
#[path = "support/probe.rs"]
mod probe;
#[path = "support/signatures.rs"]
mod signatures;
mod support;

#[cfg(feature = "allocation-stats")]
mod allocations {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::sync::atomic::{AtomicU64, Ordering};
    pub struct Counting;
    pub static BYTES: AtomicU64 = AtomicU64::new(0);
    pub static COUNT: AtomicU64 = AtomicU64::new(0);
    // SAFETY: all operations delegate to System with the original pointer and layout;
    // counters neither allocate nor alter allocation lifetimes.
    unsafe impl GlobalAlloc for Counting {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            // SAFETY: the GlobalAlloc caller supplies a valid allocation layout.
            let ptr = unsafe { System.alloc(layout) };
            if !ptr.is_null() {
                BYTES.fetch_add(layout.size() as u64, Ordering::Relaxed);
                COUNT.fetch_add(1, Ordering::Relaxed);
            }
            ptr
        }
        unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
            // SAFETY: the GlobalAlloc caller supplies a valid allocation layout.
            let ptr = unsafe { System.alloc_zeroed(layout) };
            if !ptr.is_null() {
                BYTES.fetch_add(layout.size() as u64, Ordering::Relaxed);
                COUNT.fetch_add(1, Ordering::Relaxed);
            }
            ptr
        }
        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            // SAFETY: pointer and layout are passed unchanged to their original allocator.
            unsafe { System.dealloc(ptr, layout) }
        }
        unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
            // SAFETY: the allocation and new size satisfy GlobalAlloc's realloc contract.
            let ptr = unsafe { System.realloc(ptr, layout, new_size) };
            if !ptr.is_null() {
                BYTES.fetch_add(new_size as u64, Ordering::Relaxed);
                COUNT.fetch_add(1, Ordering::Relaxed);
            }
            ptr
        }
    }
    pub fn snapshot() -> (u64, u64) {
        (BYTES.load(Ordering::Relaxed), COUNT.load(Ordering::Relaxed))
    }
}
#[cfg(feature = "allocation-stats")]
#[global_allocator]
static ALLOCATOR: allocations::Counting = allocations::Counting;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().collect();
    let [_, fixtures, iterations, mode] = args.as_slice() else {
        return Err("usage: compare FIXTURES ITERATIONS MODE".into());
    };
    let cases: Vec<Json> = serde_json::from_slice(&fs::read(fixtures)?)?;
    let fixed: usize = iterations.parse()?;
    if !matches!(mode.as_str(), "timing" | "validate" | "alloc") {
        return Err("invalid mode".into());
    }
    if mode == "alloc" && !cfg!(feature = "allocation-stats") {
        return Err("alloc mode requires allocation-stats feature".into());
    }
    for case in cases {
        let name = case
            .get("name")
            .and_then(Json::as_str)
            .ok_or("missing name")?;
        let source = case
            .get("source")
            .and_then(Json::as_str)
            .ok_or("missing source")?;
        if case.get("parse").and_then(Json::as_bool).unwrap_or(false) {
            println!("{}", measure_parse(&case, source, fixed, mode)?);
            continue;
        }
        let mut engine = Engine::new();
        engine.set_strict_effects(
            case.get("strict_effects")
                .and_then(Json::as_bool)
                .unwrap_or(false),
        );
        if case.get("module_paths").is_some() {
            engine.set_module_config(ModuleConfig {
                paths: strings(&case, "module_paths")?
                    .into_iter()
                    .map(Into::into)
                    .collect(),
                allow: strings(&case, "module_allow")?,
                deny: strings(&case, "module_deny")?,
                development: case
                    .get("module_development")
                    .and_then(Json::as_bool)
                    .unwrap_or(false),
                ..ModuleConfig::default()
            })?;
        }
        if let Some(byte) = case.get("entropy_byte") {
            let byte = u8::try_from(byte.as_u64().ok_or("invalid entropy byte")?)?;
            engine.set_random_source(move |_, output| {
                output.fill(byte);
                Ok(output.len())
            });
        }
        let stdout = case
            .get("stdout")
            .and_then(Json::as_bool)
            .unwrap_or(false)
            .then(|| Arc::new(Mutex::new(Vec::new())));
        let stderr = case
            .get("stderr")
            .and_then(Json::as_bool)
            .unwrap_or(false)
            .then(|| Arc::new(Mutex::new(Vec::new())));
        let capture = mode == "validate";
        if let Some(buffer) = &stdout {
            let buffer = buffer.clone();
            engine.set_output_writer(move |_, bytes| {
                if capture {
                    buffer.lock().unwrap().extend_from_slice(bytes);
                }
                Ok(())
            });
        }
        if let Some(buffer) = &stderr {
            let buffer = buffer.clone();
            engine.set_error_writer(move |_, bytes| {
                if capture {
                    buffer.lock().unwrap().extend_from_slice(bytes);
                }
                Ok(())
            });
        }
        let signature_method = case
            .get("signature_probe")
            .map(|probe| signatures::configure(&mut engine, probe))
            .transpose()?;
        let function = case.get("function").and_then(Json::as_str).unwrap_or("run");
        let mut input = Vec::new();
        for arg in case
            .get("args")
            .and_then(Json::as_array)
            .ok_or("missing args")?
        {
            input.push(parse_json(&serde_json::to_vec(arg)?, codec_options())?.value);
        }
        let mut globals = BTreeMap::new();
        if let Some(values) = case.get("globals") {
            for (name, value) in values.as_object().ok_or("globals must be an object")? {
                globals.insert(
                    name.clone(),
                    parse_json(&serde_json::to_vec(value)?, codec_options())?.value,
                );
            }
        }
        let metered = case
            .get("accounting")
            .and_then(Json::as_bool)
            .unwrap_or(true);
        let mut capabilities = Vec::new();
        if case
            .get("capability_probe")
            .and_then(Json::as_bool)
            .unwrap_or(false)
        {
            capabilities.push(probe::capability());
        }
        if case
            .get("block_probe")
            .and_then(Json::as_bool)
            .unwrap_or(false)
        {
            capabilities.push(blocks::capability());
        }
        for name in strings(&case, "notifications")? {
            capabilities.push(support::notification(&name)?);
        }
        let mut options = CallOptions {
            globals,
            capabilities,
            allow_require: case
                .get("allow_require")
                .and_then(Json::as_bool)
                .unwrap_or(false),
            limits: Limits {
                steps: if metered { Some(5_000_000) } else { None },
                memory_bytes: if metered { Some(64 << 20) } else { None },
                recursion: 256,
            },
            ..CallOptions::default()
        };
        if let Some(method) = signature_method.clone() {
            signatures::bind(
                &mut options,
                case.get("signature_probe")
                    .ok_or("missing signature probe")?,
                method,
            );
        }
        declare::declare(
            &mut engine,
            &case,
            &options.globals,
            signature_method.as_ref(),
        )?;
        let script = engine.compile(source)?;
        let result = script.call(function, &input, options.clone())?;
        let encoding = case
            .get("result_encoding")
            .and_then(Json::as_str)
            .unwrap_or("");
        let output = support::encode(&result.value, encoding, codec_options())?;
        let hash = output.iter().fold(0xcbf29ce484222325u64, |h, b| {
            (h ^ *b as u64).wrapping_mul(0x100000001b3)
        });
        let mut record = serde_json::Map::from_iter([
            ("name".into(), json!(name)),
            ("digest".into(), json!(format!("{hash:016x}"))),
            ("output_bytes".into(), json!(output.len())),
            ("steps".into(), json!(result.stats.steps)),
            (
                "tracked_peak_bytes".into(),
                json!(result.stats.peak_memory_bytes),
            ),
            (
                "tracked_retained_bytes".into(),
                json!(result.stats.retained_memory_bytes),
            ),
        ]);
        if mode == "validate" {
            record.insert(
                "result_json".into(),
                Json::String(String::from_utf8(output.to_vec())?),
            );
            for (name, buffer) in [("stdout_hex", &stdout), ("stderr_hex", &stderr)] {
                if let Some(buffer) = buffer {
                    record.insert(name.into(), Json::String(hex(&buffer.lock().unwrap())));
                }
            }
        } else {
            let n = if fixed > 0 {
                fixed
            } else {
                case.get("iterations")
                    .and_then(Json::as_u64)
                    .ok_or("missing iterations")? as usize
            };
            if n == 0 {
                return Err("zero iterations".into());
            }
            for _ in 0..n.min(32) {
                black_box(script.call(function, &input, options.clone())?);
            }
            #[cfg(feature = "allocation-stats")]
            let before = allocations::snapshot();
            let start = Instant::now();
            let mut last = None;
            for _ in 0..n {
                last = Some(black_box(script.call(
                    function,
                    black_box(&input),
                    options.clone(),
                )?));
            }
            let elapsed = start.elapsed();
            #[cfg(feature = "allocation-stats")]
            {
                let after = allocations::snapshot();
                record.insert(
                    "alloc_bytes".into(),
                    json!((after.0 - before.0) as f64 / n as f64),
                );
                record.insert(
                    "allocations".into(),
                    json!((after.1 - before.1) as f64 / n as f64),
                );
            }
            let final_output = support::encode(&last.unwrap().value, encoding, codec_options())?;
            if final_output != output {
                return Err(format!("{name}: timed output differs from validation").into());
            }
            record.insert("iterations".into(), json!(n));
            record.insert(
                "ns_per_call".into(),
                json!(elapsed.as_nanos() as f64 / n as f64),
            );
        }
        println!("{}", serde_json::to_string(&record)?);
    }
    Ok(())
}

fn measure_parse(
    case: &Json,
    source: &str,
    fixed: usize,
    mode: &str,
) -> Result<Json, Box<dyn std::error::Error>> {
    let malformed = case
        .get("syntax_error")
        .and_then(Json::as_bool)
        .unwrap_or(false);
    let parse = || -> Result<(), Box<dyn std::error::Error>> {
        if malformed {
            let error = Engine::new()
                .type_check(black_box(source))
                .err()
                .ok_or("expected syntax error")?;
            if error.kind != vibescript::ErrorKind::Syntax {
                return Err(error.into());
            }
            black_box(error);
        } else {
            black_box(vibescript::tooling::tokens(black_box(source))?);
        }
        Ok(())
    };
    parse()?;
    let mut record = serde_json::Map::from_iter([
        (
            "name".into(),
            case.get("name").cloned().unwrap_or(Json::Null),
        ),
        ("digest".into(), json!("parse")),
        ("output_bytes".into(), json!(4)),
        ("steps".into(), json!(0)),
        ("tracked_peak_bytes".into(), json!(0)),
        ("tracked_retained_bytes".into(), json!(0)),
    ]);
    if mode == "validate" {
        record.insert("result_json".into(), json!("true"));
        return Ok(Json::Object(record));
    }
    let n = if fixed > 0 {
        fixed
    } else {
        case.get("iterations")
            .and_then(Json::as_u64)
            .ok_or("missing iterations")? as usize
    };
    if n == 0 {
        return Err("zero iterations".into());
    }
    for _ in 0..n.min(32) {
        parse()?;
    }
    #[cfg(feature = "allocation-stats")]
    let before = allocations::snapshot();
    let start = Instant::now();
    for _ in 0..n {
        parse()?;
    }
    let elapsed = start.elapsed();
    #[cfg(feature = "allocation-stats")]
    {
        let after = allocations::snapshot();
        record.insert(
            "alloc_bytes".into(),
            json!((after.0 - before.0) as f64 / n as f64),
        );
        record.insert(
            "allocations".into(),
            json!((after.1 - before.1) as f64 / n as f64),
        );
    }
    record.insert("iterations".into(), json!(n));
    record.insert(
        "ns_per_call".into(),
        json!(elapsed.as_nanos() as f64 / n as f64),
    );
    Ok(Json::Object(record))
}

fn strings(case: &Json, name: &str) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let Some(value) = case.get(name) else {
        return Ok(Vec::new());
    };
    let values = value
        .as_array()
        .ok_or_else(|| format!("{name} must be an array"))?;
    values
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| format!("{name} entries must be strings").into())
        })
        .collect()
}

fn hex(bytes: &[u8]) -> String {
    let digit = |n: u8| char::from(if n < 10 { b'0' + n } else { b'a' + n - 10 });
    bytes
        .iter()
        .flat_map(|byte| [digit(byte >> 4), digit(byte & 15)])
        .collect()
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
