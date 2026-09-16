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
    if args.len() != 4 {
        return Err("usage: compare FIXTURES ITERATIONS MODE".into());
    }
    let cases: Vec<Json> = serde_json::from_slice(&fs::read(&args[1])?)?;
    let fixed: usize = args[2].parse()?;
    let mode = &args[3];
    if !matches!(mode.as_str(), "timing" | "validate" | "alloc") {
        return Err("invalid mode".into());
    }
    if mode == "alloc" && !cfg!(feature = "allocation-stats") {
        return Err("alloc mode requires allocation-stats feature".into());
    }
    for case in cases {
        let name = case["name"].as_str().ok_or("missing name")?;
        let source = case["source"].as_str().ok_or("missing source")?;
        let mut engine = Engine::new();
        engine.set_strict_effects(case["strict_effects"].as_bool().unwrap_or(false));
        if case.get("module_paths").is_some() {
            engine.set_module_config(ModuleConfig {
                paths: strings(&case, "module_paths")?
                    .into_iter()
                    .map(Into::into)
                    .collect(),
                allow: strings(&case, "module_allow")?,
                deny: strings(&case, "module_deny")?,
                development: case["module_development"].as_bool().unwrap_or(false),
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
        let stdout = case["stdout"]
            .as_bool()
            .unwrap_or(false)
            .then(|| Arc::new(Mutex::new(Vec::new())));
        let stderr = case["stderr"]
            .as_bool()
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
        let script = engine.compile(source)?;
        let function = case["function"].as_str().unwrap_or("run");
        let mut input = Vec::new();
        for arg in case["args"].as_array().ok_or("missing args")? {
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
        let metered = case["accounting"].as_bool().unwrap_or(true);
        let mut capabilities = Vec::new();
        if case["capability_probe"].as_bool().unwrap_or(false) {
            capabilities.push(probe_capability());
        }
        if case["block_probe"].as_bool().unwrap_or(false) {
            capabilities.push(blocks::capability());
        }
        for name in strings(&case, "notifications")? {
            capabilities.push(support::notification(&name)?);
        }
        let options = CallOptions {
            globals,
            capabilities,
            allow_require: case["allow_require"].as_bool().unwrap_or(false),
            limits: Limits {
                steps: if metered { Some(5_000_000) } else { None },
                memory_bytes: if metered { Some(64 << 20) } else { None },
                recursion: 256,
            },
            ..CallOptions::default()
        };
        let result = script.call(function, &input, options.clone())?;
        let encoding = case["result_encoding"].as_str().unwrap_or("");
        let output = support::encode(&result.value, encoding, codec_options())?;
        let hash = output.iter().fold(0xcbf29ce484222325u64, |h, b| {
            (h ^ *b as u64).wrapping_mul(0x100000001b3)
        });
        let mut record = json!({"name":name,"digest":format!("{hash:016x}"),"output_bytes":output.len(),"steps":result.stats.steps,"tracked_peak_bytes":result.stats.peak_memory_bytes,"tracked_retained_bytes":result.stats.retained_memory_bytes});
        if mode == "validate" {
            record["result_json"] = Json::String(String::from_utf8(output.to_vec())?);
            for (name, buffer) in [("stdout_hex", &stdout), ("stderr_hex", &stderr)] {
                if let Some(buffer) = buffer {
                    record[name] = Json::String(hex(&buffer.lock().unwrap()));
                }
            }
        } else {
            let n = if fixed > 0 {
                fixed
            } else {
                case["iterations"].as_u64().ok_or("missing iterations")? as usize
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
                record["alloc_bytes"] = json!((after.0 - before.0) as f64 / n as f64);
                record["allocations"] = json!((after.1 - before.1) as f64 / n as f64);
            }
            let final_output = support::encode(&last.unwrap().value, encoding, codec_options())?;
            if final_output != output {
                return Err(format!("{name}: timed output differs from validation").into());
            }
            record["iterations"] = json!(n);
            record["ns_per_call"] = json!(elapsed.as_nanos() as f64 / n as f64);
        }
        println!("{}", serde_json::to_string(&record)?);
    }
    Ok(())
}

fn probe_capability() -> vibescript::Capability {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use vibescript::{Capability, Error, ErrorKind, HostMethod, Value};
    Capability::new("host", |_| {
        let count = Arc::new(AtomicUsize::new(0));
        let next_count = count.clone();
        let next = HostMethod::new("host.next", move |_, _, _| {
            Ok(Value::int(
                next_count.fetch_add(1, Ordering::Relaxed) as i64 + 1,
            ))
        });
        let checked = HostMethod::new("host.checked", move |_, args, _| {
            count.fetch_add(1, Ordering::Relaxed);
            Ok(if args[0].as_int() == Some(0) {
                Value::bytes("invalid result")
            } else {
                args[0].clone()
            })
        })
        .with_contract(
            |_, args, keywords| {
                if args.len() != 1 || args[0].as_int().is_none() || !keywords.is_empty() {
                    return Err(Error::new(
                        ErrorKind::Runtime,
                        "host.checked expects one integer",
                    ));
                }
                Ok(())
            },
            |_, value| {
                if value.as_int().is_none() {
                    return Err(Error::new(
                        ErrorKind::Runtime,
                        "host.checked must return an integer",
                    ));
                }
                Ok(())
            },
        );
        let nested = checked.clone();
        let factory = HostMethod::new("host.factory", move |_, _, _| {
            Ok(Value::object(vec![(b"checked".to_vec(), nested.value())]))
        });
        let echo = HostMethod::new("host.echo", |ctx, args, keywords| {
            let options = Value::hash(
                keywords
                    .iter()
                    .map(|(key, value)| (key.as_bytes().unwrap().to_vec(), value.clone()))
                    .collect(),
            );
            let args = ctx.array(args)?;
            ctx.array(&[args, options])
        });
        let fail = HostMethod::new("host.fail", |_, _, _| {
            Err(Error::new(ErrorKind::Runtime, "host failure"))
        });
        Ok(Value::object(vec![
            (b"next".to_vec(), next.value()),
            (b"checked".to_vec(), checked.value()),
            (b"factory".to_vec(), factory.value()),
            (b"echo".to_vec(), echo.value()),
            (b"map".to_vec(), echo.value()),
            (b"fail".to_vec(), fail.value()),
            (b"items".to_vec(), Value::array(vec![Value::int(1)])),
        ]))
    })
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
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    bytes
        .iter()
        .flat_map(|byte| {
            [
                char::from(DIGITS[usize::from(byte >> 4)]),
                char::from(DIGITS[usize::from(byte & 15)]),
            ]
        })
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
