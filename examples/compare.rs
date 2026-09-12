use serde_json::{Value as Json, json};
use std::{fs, hint::black_box, time::Instant};
use vibescript::{CallOptions, Engine, Limits, parse_json, stringify_json};

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
        let script = Engine::new().compile(source)?;
        let function = case["function"].as_str().unwrap_or("run");
        let mut input = Vec::new();
        for arg in case["args"].as_array().ok_or("missing args")? {
            input.push(parse_json(&serde_json::to_vec(arg)?, codec_options())?.value);
        }
        let metered = case["accounting"].as_bool().unwrap_or(true);
        let options = CallOptions {
            limits: Limits {
                steps: if metered { Some(5_000_000) } else { None },
                memory_bytes: if metered { Some(64 << 20) } else { None },
                recursion: 256,
            },
            ..CallOptions::default()
        };
        let result = script.call(function, &input, options.clone())?;
        let encoded = stringify_json(&result.value, codec_options())?;
        let output = encoded.value.as_bytes().unwrap();
        let hash = output.iter().fold(0xcbf29ce484222325u64, |h, b| {
            (h ^ *b as u64).wrapping_mul(0x100000001b3)
        });
        let mut record = json!({"name":name,"digest":format!("{hash:016x}"),"output_bytes":output.len(),"steps":result.stats.steps,"tracked_peak_bytes":result.stats.peak_memory_bytes,"tracked_retained_bytes":result.stats.retained_memory_bytes});
        if mode == "validate" {
            record["result_json"] = Json::String(String::from_utf8(output.to_vec())?);
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
            let final_output = stringify_json(&last.unwrap().value, codec_options())?;
            if final_output.value.as_bytes() != Some(output) {
                return Err(format!("{name}: timed output differs from validation").into());
            }
            record["iterations"] = json!(n);
            record["ns_per_call"] = json!(elapsed.as_nanos() as f64 / n as f64);
        }
        println!("{}", serde_json::to_string(&record)?);
    }
    Ok(())
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
