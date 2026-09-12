# Vibescript in Rust

An experimental Rust implementation of the Vibescript core, with a bytecode VM, cooperative cancellation, explicit resource accounting, and optional SIMD scanners. Go v0.70.0 is the compatibility reference. This is a prototype for measuring architecture and performance; it is not a replacement for the full Go runtime.

## Try it

This checkout lives on `/Volumes/m2/Work/xipkit/vibescript.rs`. The original sibling path links here. Use the wrapper so Cargo downloads, build artifacts, and temporary files stay on the external drive:

```sh
./scripts/cargo run --release -- examples/total.vibe --function total --arg '[10,20,30]' --stats
./scripts/cargo test --all-features
```

The CLI prints results as JSON. Run `--help` for step, memory, recursion, and deadline options. Defaults are one million logical steps, 16 MiB of tracked memory, and 256 call frames. `./scripts/check` runs the full local validation gate.

## Embed it

```rust
use vibescript::{CallOptions, Engine, Value};

let mut engine = Engine::new();
engine.register("double", |ctx, args| {
    ctx.charge(1)?;
    let n = args.first().and_then(Value::as_int)
        .ok_or_else(|| vibescript::Error::new(vibescript::ErrorKind::Argument, "expected integer"))?;
    let doubled = n.checked_mul(2)
        .ok_or_else(|| vibescript::Error::new(vibescript::ErrorKind::Arithmetic, "overflow"))?;
    Ok(Value::int(doubled))
});
let script = engine.compile("def run(n)\n double(n) + 1\nend")?;
let result = script.call("run", &[Value::int(20)], CallOptions::default())?;
assert_eq!(result.value.as_int(), Some(41));
```

`Script` is immutable and shareable between threads. Each call imports its arguments and owns its budget. `Value` clones share immutable storage; collection writes replace the named local, preserving value semantics. Array updates reuse storage when no aliases exist and copy when another value shares the array. Strings hold arbitrary bytes, and character indexing counts each invalid UTF-8 byte as one rune.

Enable the `tokio` feature to use `asynchronous::Runner`. It runs calls on the host's Tokio blocking pool with a concurrency limit. Dropping the call future cancels its child token, and the worker retains its permit until it exits. Host callbacks remain synchronous; native async callbacks and suspended script execution are not implemented.

## Implemented subset

- Named functions with positional arguments, implicit returns, explicit `return`, and top-level statements.
- Signed 64-bit integers, floats, booleans, nil, byte strings, symbols, arrays, and insertion-ordered hashes.
- Arithmetic, floor integer division/modulo, comparisons, short-circuit `&&` and `||`, assignment and compound assignment, `if`/`elsif`/`else`, and `while`/`until` with `break`/`next`.
- Array/hash indexing and writes through a local, negative array indexes, local `push` and `<<`, array `sum`, hash `keys`/`values`, collection equality, and substring search.
- String `length`, `bytesize`, single-character indexing, `strip`, `split`, `join`, `to_s`, `to_i`, and ASCII `upcase`/`downcase`. Unicode case mapping requires a later port; use `:ascii` for non-ASCII inputs.
- `JSON.parse` and `JSON.stringify`, including escape sequences, surrogate pairs, HTML escaping, duplicate-key replacement, and invalid UTF-8 replacement.
- Explicitly registered synchronous host functions.

Not implemented: classes, modules, imports, enums, blocks/iterators, keyword/default parameters, interpolation, ranges, regex, arbitrary-precision integers, typed checking, full standard-library coverage, nested mutation paths, or async host functions. Integer overflow is an error. JSON numbers are limited to i64/finite f64 and 1,024 digits; float JSON spelling can differ from Go. Array writes past the end are errors. Compilation is bounded by 8 MiB of source and 128 levels of syntax nesting; runtime value nesting is also bounded at 128.

## Accounting and cancellation

The VM charges instructions and builtin work. Long scans and copies operate in chunks of at most 4 KiB, and exhaustion stays latched across host callbacks. Memory reservations cover runtime buffer capacity, shared-value headers, imports, locals, operand and frame stacks, and temporary builders. A reservation is made before buffer growth; growth conservatively counts the old and new capacities together. Dropping storage releases its charge, including values retained by the host after a call.

Imported strings share their immutable backing bytes. Each call charges the full retained capacity, including unused capacity in a host buffer, and owns an independent charge that is released with its view. Cloning an imported value shares that charge. Separately importing the same foreign value again conservatively charges another view. Character slices and parsed JSON strings own their output storage, so a small result does not retain its source document.

This differs from Go's reachable-graph estimator. Step counts and memory thresholds are not interchangeable between implementations, and this prototype's instruction fusion and builtin optimizations can change logical step counts. The budget excludes compiled code, caller-owned storage that the call does not retain, fixed context metadata, allocator bookkeeping, and memory allocated independently by trusted host callbacks. It is not an RSS limit. Unlimited memory disables tracking, so memory counters are zero in that mode. Cancellation is cooperative: host callbacks, allocation, and cleanup must finish or cooperate before a call can exit.

SIMD uses 16-byte NEON operations on ARM64 and SSE2 on x86_64, with portable fallbacks. The `simd` feature is enabled by default; `--no-default-features` selects the portable Rust scanners. LLVM may still auto-vectorize ordinary Rust code. Scanner tests compare every byte value at vector boundaries, and shared fixtures require identical Rust accounting with the feature on and off.

## Compare with Go

```sh
python3 scripts/compare.py --rounds 8
```

The harness downloads the pinned Go v0.70.0 module using an external-volume cache, then builds Go 1.27.1 with and without `GOEXPERIMENT=simd` and Rust with and without explicit SIMD. It checks shared cases against independently computed expected results before measuring compiled calls with accounting enabled and disabled. This includes 46 invocations from [ten unchanged upstream files](tests/upstream/README.md), and three original examples in the benchmark suite. Timing uses uninstrumented Rust binaries; separate binaries count allocations. Raw results include repeated samples, output digests, toolchains, binary hashes, and process peak RSS.

See the [optimization results](benchmarks/performance-followup.md), the [initial comparison](benchmarks/README.md), and the [implementation plan](docs/implementation-plan.md). Native measurements describe the machine recorded in each result directory, and are not a general claim about Rust versus Go.
