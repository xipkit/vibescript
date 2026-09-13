# Vibescript in Rust

An experimental Rust implementation of the Vibescript core, with a bytecode VM, cooperative cancellation, explicit resource accounting, and optional SIMD scanners. Go v0.70.0 is the compatibility reference. The complete language port is in progress; the supported surface and remaining work are tracked in the [language completion plan](docs/language-port.md).

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
        .ok_or_else(|| vibescript::Error::new(vibescript::ErrorKind::Argument, "expected 64-bit integer"))?;
    let doubled = n.checked_mul(2)
        .ok_or_else(|| vibescript::Error::new(vibescript::ErrorKind::Arithmetic, "overflow"))?;
    Ok(Value::int(doubled))
});
let script = engine.compile("def run(n)\n double(n) + 1\nend")?;
let result = script.call("run", &[Value::int(20)], CallOptions::default())?;
assert_eq!(result.value.as_int(), Some(41));
```

`Script` is immutable and shareable between threads. Each call imports its arguments and owns its budget. `Value` clones share immutable storage; collection writes replace the named local, preserving value semantics. Array updates reuse storage when no aliases exist and copy when another value shares the array. Strings hold arbitrary bytes, and character indexing counts each invalid UTF-8 byte as one rune.

Integers use compact 64-bit storage and promote to shared arbitrary-precision values when needed. Results normalize back to compact integers when they fit. Hosts can construct larger values with `Value::parse_integer(text, radix)`; callbacks use `CallContext::parse_integer` to charge construction to the active call. `Value::is_integer` recognizes both representations, while `as_int` returns a value only within the signed 64-bit range.

Money stores signed 64-bit cents and an uppercase three-letter ASCII currency inline, keeping `Value` at 16 bytes. Hosts use `Value::money(cents, currency)` and `Value::as_money()`. Formatting always uses two decimal places, and arithmetic rejects overflow or currency mismatches. As in Go, JSON cannot encode money; scripts can return `.to_s` for CLI output, while embedded hosts receive typed money values directly.

Durations also stay inline, storing signed 64-bit whole seconds. Hosts use `Value::duration(seconds)` and `Value::as_duration()`. Float scaling rounds to the nearest second with halves away from zero while preserving integer precision above `2^53`. Duration results need an explicit string or numeric conversion for JSON output. Time values and duration methods that return times remain pending.

Enable the `tokio` feature to use `asynchronous::Runner`. It runs calls on the host's Tokio blocking pool with a concurrency limit. Dropping the call future cancels its child token, and the worker retains its permit until it exits. Host callbacks remain synchronous; native async callbacks and suspended script execution are not implemented.

Use `Engine::register_with_keywords` to expose a callback with named arguments, and `Script::call_with_keywords` or `Runner::call_with_keywords` to pass them from Rust. Keyword keys received by callbacks are byte-string values. Registered callbacks validate their own signatures; legacy callbacks reject nonempty keywords. Host-supplied keywords bind by name and do not become a trailing positional options hash.

The [SMS preview example](examples/sms.rs) captures a Rust client in a registered `sms_send` callback. It validates arguments, checks cancellation, and builds a return value through `CallContext`; run it with `./scripts/cargo run --release --example sms`. Host functions become available to scripts through explicit registration, and their results are imported into the call's memory budget. Client-owned buffers and network operations remain the host's responsibility. The site's namespaced `sms.send` syntax is a future addition.

## Implemented subset

- Named functions with positional/default/keyword/rest/keyword-rest parameters, calls with or without parentheses, argument splats and keyword shorthand, implicit returns, explicit `return` of single or multiple values, and top-level statements. Source calls preserve Go's trailing options-hash binding rules. Defaults execute in order after argument validation and use the call's limits.
- Synchronous call-attached `{ ... }` and `do ... end` blocks, `yield`, `block_given?`, explicit/destructured and implicit block parameters, captured local writes, `next`, `break`, and nonlocal method returns. Block arguments and captured scopes remain subject to the call's limits. Rust host block callbacks still need implementation.
- Array/hash/range iteration and block transforms, including `each`, indexed iteration, `map`, `select`, `reject`, reductions, predicates, grouping, adjacent `slice_when`/`chunk_while`, flattening, fallback lookup and hash transforms. Numeric `times`, `upto`, `downto`, and `step` and universal `tap`/`yield_self` run blocks through resumable VM frames. Retained output and pending iteration state count against the call's memory limit.
- Local names become available as statements execute, including declarations after skipped branches and loop control transfers. Zero-parameter script functions can auto-invoke in value positions; other function references and registered host callbacks require explicit calls. Unknown names fail when executed, so unused branches and defaults can compile.
- Arbitrary-precision integers with compact 64-bit storage, floats, booleans, nil, byte strings, symbols including operator names, arrays, insertion-ordered hashes, and integer ranges. Decimal, hexadecimal, binary and octal integer literals support separators and explicit base prefixes, including `0d`.
- Integer arithmetic promotes on overflow, preserves floor division/modulo, and compares large integers exactly with floats. Integer/string/float conversions and numeric inspection preserve large values; finite float-to-integer conversions use the actual binary value. Indexes, counts and range endpoints remain within 64-bit bounds.
- Numeric `round`/`floor`/`ceil` with optional precision, `div`/`divmod`/`fdiv`/`remainder`/`modulo`, `clamp`, `between?`, sign/zero predicates, float finite/NaN/infinity predicates, and integer `next`/`succ`/`pred`. Rounding and division promote integer results; `clamp` compares bounds exactly and preserves the selected value's type. Large rounding buckets and arithmetic scratch use the call's limits.
- `money` and `money_cents`, currency/cent/amount properties, formatting, equality and ordered comparisons, `between?`, checked addition/subtraction, integer multiplication and truncating integer division. Currency codes accept any three ASCII letters and normalize to uppercase. Literal parsing charges bounded work without retaining the source; individual money values require no heap storage.
- Integer duration units, `Duration.build` with seconds or named parts, and `Duration.parse` with Go-style and ISO-8601 strings. Duration properties include whole and fractional unit conversions, `parts`, `iso8601`, `format` and `to_i`; arithmetic includes checked addition/subtraction, integer and float scaling, duration ratios and remainder. Unit multiplication and named/ISO part construction preserve Go's 64-bit wrapping behavior; arithmetic checks its results. Parsing charges bounded work and uses fixed temporary storage.
- Global `to_int` and `to_float`, including strict decimal integers and finite decimal/hexadecimal float strings with Go's separator rules. Fractional floats are rejected by `to_int`; numeric `to_float` preserves IEEE infinity and NaN. Long string conversions charge work and temporary buffers.
- `Math` constants and square/cube roots, trigonometry, exponentials and logarithms. `Math` and `JSON` support normal namespace lookup, aliases, per-execution mutation and `::` access. Go's math algorithms and fused arithmetic preserve the ARM64 reference's float results; x86_64 conformance is not yet verified.
- Single- and double-quoted string escapes, double-quoted `#{expression}` interpolation, and `%w`/`%W` word arrays and `%i`/`%I` symbol arrays. Interpolated values render in order through accounted builders; `%I` preserves symbol values.
- An adjacent minus binds to a numeric literal before member access, so `-5.abs` is `5` and `-1.minutes` is a negative duration. Spaces preserve the outer unary operation, and exponentiation takes precedence over the sign. Unary operators and powers accept the reference's line continuations.
- Arithmetic, floor integer division/modulo, comparisons, short-circuit `&&` and `||`, ternary expressions, assignment and compound assignment, `if`/`elsif`/`else`/`unless`, expression-valued conditionals, `case`/`when`, and statement modifiers.
- Array/hash/finite-range `for` loops, `while`/`until`, expression-valued loops, payloads on `break`/`next`, and nested/rest destructuring. Loop state and captured iteration values are accounted and released on exit.
- Array/hash indexing and writes through nested index and member paths, negative/fractional array indexes, addressable collection mutation, array `sum`, hash `keys`/`values`, collection equality, and substring search.
- Hash field reads and assignment, including compound/logical updates and destructuring. Array `push`/`append`/`<<`, `prepend`/`unshift`, `pop`/`shift`, `delete`, `insert`, `clear`, and value/block `fill`; hash `store`, `delete`, `replace`, and `clear`; array/hash `delete_if`, `keep_if`, and deletion fallback blocks; immutable `dup` views. Removed-value returns preserve their own value when a collection changes again. Mutating blocks stage their changes until iteration finishes and retain their captured receiver address across callbacks.
- Inclusive, exclusive, descending, and open integer ranges, membership, endpoint queries, bounded expansion, array/string slices, raw byte access, and array `first`/`last`. Character indexing and slicing normalize invalid UTF-8; byte slices preserve it.
- String `length`, `bytesize`, single-character indexing, `strip`, `split`, `join`, `to_s`, `to_i`, and ASCII `upcase`/`downcase`. Unicode case mapping requires a later port; use `:ascii` for non-ASCII inputs.
- `JSON.parse` and `JSON.stringify`, including exact large integers, float exponent formatting, escape sequences, surrogate pairs, HTML escaping, duplicate-key replacement, and invalid UTF-8 replacement. Long floating-point tokens use a bounded conversion buffer that preserves rounding.
- Collection transforms (`reverse`, `compact`, `uniq`, `flatten`, `take`/`drop`, `chunk`/`window`, `zip`, `transpose`), nested `dig`, `fetch`, hash membership, and pair-to-hash conversion.
- Hash `merge` with multiple inputs and optional three-argument conflict blocks, `deep_transform_keys` through nested hashes and arrays, `remap_keys`, `except`, and depth-aware `flatten`. Recursive key traversal uses accounted frames, retaining parent keys across callbacks and visiting every shared occurrence.
- Scalar string concatenation, recursive joining, array/range display, character and byte extraction, affix predicates, integer parity, and numeric `abs`.
- String `prepend`, character-position `insert`, `replace`, and `clear` return new strings while preserving the receiver's binding.
- Explicitly registered synchronous host functions.
- Stable array `sort` with comparator blocks, `sort_by`, `min`, `max`, `minmax`, `min_by`, and `max_by`. Array `<=>` compares nested values lexicographically; incomparable pairs return nil. Sort scratch, retained keys and comparison state are accounted, and block exits unwind through VM frames.

Not implemented: classes, modules, imports, enums, host block callbacks, regex, type annotations/checking, full standard-library coverage, or async host functions. `JSON.parse_as`, time values, duration `after`/`since`/`from_now`/`ago`/`before`/`until`, and string/regex block methods remain pending. Like the Go reference, `zip` ignores an attached block. Indexed array assignment past the end is an error; `insert` and `fill` can expand an array. Compilation is bounded by 8 MiB of source, 128 levels of syntax nesting, 64 nested calls without parentheses, and eight nested interpolations; runtime value nesting is also bounded at 128. Large integer literals and string-to-integer conversions follow Go's 100,000-digit guards. JSON integer size and conversion work are governed by the call's limits; JSON floats must be finite. [Known reference differences](docs/compatibility.md) cover integer-boundary loops, hash-loop break results, and collection evaluation during mutation.

The stable-sort driver adapts Go's implementation to preserve comparator call order, and percent-literal delimiters use Go's Unicode 17.0.0 letter and digit tables. The duration parser and math routines also adapt Go code. Their [BSD license](licenses/Go-BSD-3-Clause.txt) and [math notices](licenses/math-NOTICE.txt) accompany the project's MIT license.

## Accounting and cancellation

The VM charges instructions and builtin work. Long scans and copies operate in chunks of at most 4 KiB, and exhaustion stays latched across host callbacks. Memory reservations cover runtime buffer capacity, shared-value headers, integer payloads and arithmetic scratch, imports, locals, operand and frame stacks, and temporary builders. A reservation is made before buffer growth; growth conservatively counts the old and new capacities together. Dropping storage releases its charge, including values retained by the host after a call. Large integer literals are decoded under the call's limits when evaluated, and exponentiation rejects impossible projected growth before constructing the result.

Imported strings share their immutable backing bytes. Each call charges the full retained capacity, including unused capacity in a host buffer, and owns an independent charge that is released with its view. Cloning an imported value shares that charge. Separately importing the same foreign value again conservatively charges another view. Character slices and parsed JSON strings own their output storage, so a small result does not retain its source document.

Hashes keep insertion order and add an index at 16 entries. Index capacity and headers are charged before allocation; hashing, collision probes, comparisons, and resizing consume bounded work. Updates reuse unique storage, while aliases retain snapshots. Removing entries preserves order and rebuilds the index; shrinking collections releases excess capacity at geometric thresholds. Hashing is deterministic for reproducible counters; execution quotas bound collision work rather than relying on secret hash seeds.

This differs from Go's reachable-graph estimator. Step counts and memory thresholds are not interchangeable between implementations, and this prototype's instruction fusion and builtin optimizations can change logical step counts. The budget excludes compiled code, caller-owned storage that the call does not retain, fixed context metadata, allocator bookkeeping, and memory allocated independently by trusted host callbacks. It is not an RSS limit. Unlimited memory disables tracking, so memory counters are zero in that mode. Cancellation is cooperative: host callbacks, allocation, and cleanup must finish or cooperate before a call can exit.

SIMD uses 16-byte NEON operations on ARM64 and SSE2 on x86_64, with portable fallbacks. The `simd` feature is enabled by default; `--no-default-features` selects the portable Rust scanners. LLVM may still auto-vectorize ordinary Rust code. Scanner tests compare every byte value at vector boundaries, and shared fixtures require identical Rust accounting with the feature on and off.

## Compare with Go

```sh
python3 scripts/compare.py --rounds 8
```

The harness downloads the pinned Go v0.70.0 module using an external-volume cache, then builds Go 1.27.1 with and without `GOEXPERIMENT=simd` and Rust with and without explicit SIMD. Rust measurements always use release builds with thin LTO and one codegen unit. It checks shared cases against expected results before measuring compiled calls with accounting enabled and disabled. Generated cases have independently computed expectations; the [site corpus](tests/site/README.md) adds 158 unchanged programs with expected outputs verified against Go. The suite also includes 46 invocations from [ten unchanged upstream files](tests/upstream/README.md), six upstream/site examples in the benchmark suite, and hash/JSON scaling cases up to 2,048 keys. Timing uses uninstrumented Rust binaries; separate release binaries count allocations. Raw results include repeated samples, output digests, toolchains, binary hashes, and process peak RSS.

See the [hash scaling and website results](benchmarks/hash-performance.md), the [first optimization results](benchmarks/performance-followup.md), the [initial comparison](benchmarks/README.md), and the [language completion plan](docs/language-port.md). Native measurements describe the machine recorded in each result directory, and are not a general claim about Rust versus Go.
