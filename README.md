# Vibescript

Vibescript is a small, statically typed language for workflow scripts that a host program embeds and runs under explicit step, memory, recursion and time limits. This is its reference implementation, in Rust: a bytecode VM with cooperative cancellation, explicit resource accounting and optional SIMD scanners, a type checker, and the `vibes` command-line tools. It also runs on WASI; see [platforms](docs/platforms.md).

Start with the [language guide](docs/language.md), which covers the whole language with examples that compile. Together with `vibes prelude`, which prints every builtin signature, it is the context to give a model that writes Vibescript.

The [glue corpus](corpus/glue/README.md) provides 40 tested workflows. The [AI authoring study](docs/authoring-evaluation.md) records their first drafts, diagnostics, repairs and before/after results.

```vibe
def total(items: array<{ price: int, qty: int }>) -> int
  items.sum { |item| item["price"] * item["qty"] }
end

total([{ price: 250, qty: 2 }, { price: 100, qty: 1 }]) # 600
```

## Try it

This checkout lives on `/Volumes/AI/Work/xipkit/vibescript.rs`. The original sibling path links here. Use the wrapper so Cargo downloads, build artifacts, and temporary files stay on the external drive:

```sh
./scripts/cargo run --release -p vibes -- examples/total.vibe --function total --arg '[10,20,30]' --stats
./scripts/cargo run --release -p vibes -- check examples/total.vibe
./scripts/cargo test --workspace --all-features
```

The [CLI](docs/cli.md) provides `vibes run`, `check`, `fix`, `prelude`, `fmt`, `analyze`, `test`, `repl` and `lsp`. Every command type checks the scripts it compiles. `vibes check FILE` compiles a script without running it and reports every diagnostic with its code; `--json` prints them as JSON Lines, and `vibes fix` applies their machine-applicable fixes. `vibes prelude` prints every builtin function, namespace and member under its one canonical name with its typed signature; `Engine::prelude` extends it with a host's functions, capabilities and globals. The flat form, `vibes FILE`, prints the final result as JSON and accepts `--function`, `--arg` and `--kwarg`; `vibes help flat` lists its limits, whose defaults are one million logical steps, 16 MiB of tracked memory, and 256 call frames. Scripts write `puts`, `print`, and `p` to stdout and `warn` to stderr. The formatter, analyzer, fixer, test runner, REPL and language server are also libraries in the `vibescript-tools` crate. `./scripts/check` runs the full local validation gate.

The CLI searches the input file's directory for required modules. Add other roots with repeatable `--module-path DIR` options, before the script path.

`vibes lsp` is the language server editors launch for `*.vibe` files; it reports the type checker's diagnostics and offers their fixes as quick fixes. Hover and completion documentation comes from the [builtin reference](tools/src/lsp/reference). See [the language server](docs/lsp.md). `vibes repl` starts the interactive REPL; see [the REPL section](docs/cli.md#vibes-repl).

## Embed it

```rust
use vibescript::{CallOptions, Engine, HostMethod, Signature, SignatureParam, Value};

let double = HostMethod::new("double", |ctx, args, _| {
    ctx.charge(1)?;
    let n = args[0].as_int()
        .ok_or_else(|| vibescript::Error::new(vibescript::ErrorKind::Argument, "expected a 64-bit integer"))?;
    let doubled = n.checked_mul(2)
        .ok_or_else(|| vibescript::Error::new(vibescript::ErrorKind::Arithmetic, "overflow"))?;
    Ok(Value::int(doubled))
})
.with_signature(Signature {
    params: vec![SignatureParam { name: "n".into(), ty: "int".into(), optional: false }],
    result: "int".into(),
    accepts_block: false,
})?;
let mut engine = Engine::new();
engine.register_method("double", double);
let script = engine.compile("def run(n: int) -> int\n  double(n) + 1\nend")?;
let result = script.call("run", &[Value::int(20)], CallOptions::default())?;
assert_eq!(result.value.as_int(), Some(41));
```

`Script` is immutable and shareable between threads. Each call imports its arguments, checks them against the called function's parameter types, and owns its budget. `Value` clones share immutable storage; collection writes replace the named local, preserving value semantics. Array updates reuse storage when no aliases exist and copy when another value shares the array. Strings hold arbitrary bytes, and character indexing counts each invalid UTF-8 byte as one rune.

A host function or capability method with a `Signature` is typed by it, and one without takes and returns `any`, which scripts must narrow. `Engine::declare_global` and `Engine::declare_capability` declare the globals and capabilities every call supplies, so scripts can use them with their types; a name that is neither in scope nor declared is a compile error. See [host globals](docs/globals.md) and [host capabilities](docs/capabilities.md).

Interactive hosts can carry state between snippets: `Script::run_bindings` returns the top-level variables, classes, modules and enums a run leaves, `Script::declarations` lists top-level declarations with their source spans, and `vibescript::builtins()` lists the core builtins. `vibes repl` is built on them; see [interactive sessions](docs/sessions.md).

Integers use compact 64-bit storage and promote to shared arbitrary-precision values when needed. Results normalize back to compact integers when they fit. Hosts can construct larger values with `Value::parse_integer(text, radix)`; callbacks use `CallContext::parse_integer` to charge construction to the active call. `Value::is_integer` recognizes both representations, while `as_int` returns a value only within the signed 64-bit range.

Money stores signed 64-bit cents and an uppercase three-letter ASCII currency inline, keeping `Value` at 16 bytes. Hosts use `Value::money(cents, currency)` and `Value::as_money()`. Formatting always uses two decimal places, and arithmetic rejects overflow or currency mismatches. JSON cannot encode money; scripts can return `.to_s` for CLI output, while embedded hosts receive typed money values directly.

Durations also stay inline, storing signed 64-bit whole seconds. Hosts use `Value::duration(seconds)` and `Value::as_duration()`. Float scaling rounds to the nearest second with halves away from zero while preserving integer precision above `2^53`. Duration results need an explicit string or numeric conversion for JSON output.

UTC timestamps store Unix seconds and nanoseconds inline, retaining the 16-byte `Value` representation. Hosts use `Value::time(seconds, nanoseconds)` and `Value::as_time()`. Zoned timestamps share immutable timezone data with independent memory charges on import. Time results also need an explicit string or numeric conversion for JSON output.

Enable the `tokio` feature to use `asynchronous::Runner` and `HostMethod::new_async`. Script work runs on bounded Tokio blocking workers; native host futures can await I/O without occupying a worker. Their scoped `AsyncHostCall` can await attached script blocks under the same accounting, cancellation and control-flow rules. Dropping a call cancels its child token. Synchronous host callbacks keep their worker reservation until they return, including nested async block calls. See [async capabilities](docs/capabilities.md#async-methods).

Use `Engine::register_with_keywords` to expose a callback with named arguments, and `Script::call_with_keywords` or `Runner::call_with_keywords` to pass them from Rust. Keyword keys received by callbacks are byte-string values. Plain registered callbacks validate their own arguments; legacy callbacks reject nonempty keywords. `HostMethod::with_signature` adds a declared positional type contract, and `Engine::register_method` registers that descriptor with its validators and optional block driver.

The [SMS preview example](examples/sms.rs) exposes a Rust client as `sms.send` through a `Capability` template whose method publishes its signature and keeps its argument and return contracts; the engine declares the capability and compiles a typed script against it. It checks cancellation and builds its preview through `CallContext`; run it with `./scripts/cargo run --release --example sms`. Capability results are imported into the call's memory budget. Client-owned buffers and network operations remain the host's responsibility.

## The language

The [language guide](docs/language.md) is the complete, compact description. The topic pages go deeper into runtime semantics:

- [Types](docs/types.md): type syntax, and the runtime checks that remain where dynamic data enters.
- [The type checker](docs/checker.md): what it proves, and its diagnostics.
- [Classes](docs/classes.md), [modules](docs/modules.md), [required files](docs/require.md) and [error handling](docs/errors.md).
- [Strings](docs/strings.md), [string iteration](docs/string-iteration.md), [regular expressions](docs/regex.md), [inspection and templates](docs/rendering.md), [output](docs/output.md), [random values](docs/random.md), [equality](docs/equality.md), [copies](docs/value-helpers.md), [type tests](docs/introspection.md), [safe navigation](docs/safe-navigation.md), [keyword arguments](docs/options-hash.md) and [loops](docs/loop.md).
- [Numeric bounds](docs/numeric-guards.md), [JSON depth](docs/json-depth.md), [formatting](docs/formatting.md) and [timezone sources](docs/timezones.md).
- [Source diagnostics](docs/diagnostics.md), [host globals](docs/globals.md) and [host capabilities](docs/capabilities.md).
- The builtin reference in [`tools/src/lsp/reference`](tools/src/lsp/reference), which also feeds the language server's hover and completion.

Language decisions are recorded as [architecture decision records](docs/adr/README.md); [ADR-007](docs/adr/007-static-types.md) and [ADR-008](docs/adr/008-canonical-surface-for-ai-authors.md) define the current language. `tests/docs.rs` compiles every example in this README, the topic docs and the builtin reference with static types, so the documentation cannot drift from the compiler; the ADRs keep the language they were written in.

Compilation is bounded by 8 MiB of source, 1,024 levels of syntax nesting, 64 nested calls without parentheses, and eight nested interpolations; arrays and hashes support [10,000 nested containers](docs/json-depth.md), including JSON values. Environment imports retain their separate 128-level guard. Large integer literals and string-to-integer conversions are limited to 100,000 digits. JSON integer size and conversion work are governed by the call's limits; JSON floats must be finite. Windows, Android and iOS integration has been cross-compiled but not tested on those operating systems; the browser-Wasm local-time adapter remains pending.

The stable-sort driver adapts Go's implementation to preserve comparator call order. Identifiers, enum symbols, timezone case conversion and [string casing](licenses/casing-NOTICE.txt) use Go's Unicode 17.0.0 tables. The duration parser, time calendar, layout and timezone routines, math routines and [regex implementation](licenses/regex-NOTICE.txt) also adapt Go code. Their [BSD license](licenses/Go-BSD-3-Clause.txt) and [math notices](licenses/math-NOTICE.txt) accompany the project's MIT license.

## Accounting and cancellation

The VM charges instructions and builtin work. Cold required-file compilation also shares the call's work limit, cancellation and deadline through parsing and bytecode generation; cache hits avoid compilation charges. Long scans and copies operate in chunks of at most 4 KiB, and exhaustion stays latched across host callbacks. Memory reservations cover runtime buffer capacity, shared-value headers, integer payloads and arithmetic scratch, imports, locals, operand and frame stacks, and temporary builders. A reservation is made before buffer growth; growth conservatively counts the old and new capacities together. Dropping storage releases its charge, including values retained by the host after a call. Large integer literals are decoded under the call's limits when evaluated, and exponentiation rejects impossible projected growth before constructing the result.

Fixed formatting guards return `ErrorKind::OutputLimit` and a recoverable script `LimitError`. These include `strftime`'s 1 MiB output cap and the 100-digit precision cap on ISO8601 time serializers. Actual invocation exhaustion remains uncatchable. Go-layout `Time#format` uses the call's memory and work limits without the `strftime` output cap.

Script JSON inputs and serialized output have a 1 MiB guard. ASCII escaping keeps six bytes of headroom, including for shorter escapes. Host `parse_json` and `stringify_json` helpers use their independent execution budgets without the builtin payload cap. [Type literals](docs/types.md#type-literals-and-json) share immutable metadata with independent charges on import; hosts inspect them through `Value::as_type_literal()`.

Imported strings share their immutable backing bytes. Each call charges the full retained capacity, including unused capacity in a host buffer, and owns an independent charge that is released with its view. Cloning an imported value shares that charge. Separately importing the same foreign value again conservatively charges another view. Character slices and parsed JSON strings own their output storage, so a small result does not retain its source document.

Hashes keep insertion order and add an index at 16 entries. Index capacity and headers are charged before allocation; hashing, collision probes, comparisons, and resizing consume bounded work. Updates reuse unique storage, while aliases retain snapshots. Removing entries preserves order and rebuilds the index; shrinking collections releases excess capacity at geometric thresholds. Hashing is deterministic for reproducible counters; execution quotas bound collision work rather than relying on secret hash seeds.

The VM relies on static types to skip checks the checker proves, call common builtins directly and share record keys, without changing behavior; see [the typed VM](docs/vm.md). String literals are imported once per call and shared by everything built from them.

The budget excludes compiled code, caller-owned storage that the call does not retain, fixed context metadata, allocator bookkeeping, and memory allocated independently by trusted host callbacks. Instruction fusion and builtin optimizations can change logical step counts between versions. Cold compilation charges token storage, literal payloads, interpolation containers and token-stream copies. Identifier tokens borrow the source; owned literals stay charged through syntax-tree use, and cached constants do not retain the compiling call's budget. Syntax-tree boxes and containers, including method-alias copies, remain charged until compilation releases them. Owned syntax names, generated names, type field bytes and type containers are charged for their full lifetimes. Aliases share immutable names and reserve separate type containers; cached metadata does not retain the compiling call's budget. Parser tables charge local bindings, enum duplicate checks, visibility directives and copied scopes, including overlapping storage during growth. Bytecode generation also accounts binding tables, copied outer scopes, declaration contexts, loop and assignment scratch, case jump lists, normalized enum-symbol checks and type-label builders. Compiler error messages, diagnostic headers and source snippets reserve storage before construction and retain their charges through rescue handling. Formatting checks work and interruption while counting and writing text. Cold loading borrows host registrations, and builtin namespaces build retained entries directly, avoiding separate staging maps and lists. The [compiler allocation audit](docs/compiler-accounting.md) records working-storage lifetimes and the compiled-metadata exclusion. It is not an RSS limit. Unlimited memory disables tracking, so memory counters are zero in that mode. Cancellation is cooperative: host callbacks, allocation, and cleanup must finish or cooperate before a call can exit.

SIMD uses 16-byte NEON operations on ARM64 and SSE2 on x86_64, with portable fallbacks. The `simd` feature is enabled by default; `--no-default-features` selects the portable Rust scanners. LLVM may still auto-vectorize ordinary Rust code. Scanner tests compare every byte value at vector boundaries, and shared fixtures require identical Rust accounting with the feature on and off.

The library, the `vibes` CLI and the test suite [support `wasm32-wasip1`](docs/platforms.md), including module loading from host-provided directories. `./scripts/check-wasi` runs the suite and a filesystem witness under Wasmtime with normal stack limits, and `--node` repeats them under Node. Browser integration and execution on the remaining native platforms are still pending.

## Validation

```sh
python3 scripts/golden.py
python3 scripts/compare.py --rounds 8
```

This implementation is the reference. [The golden corpora](tests/golden/README.md) record its observable behavior, and `scripts/golden.py` checks a build against them. `scripts/compare.py` validates the portable and SIMD builds against the goldens and measures them, always with release builds using thin LTO and one codegen unit; see [the benchmarks](benchmarks/README.md).

Integration suites share `tests/all.rs` so each build links the interpreter fewer times. Run one suite with `./scripts/cargo test --offline --test all require::`. Register new root test files as modules there, or as explicit `[[test]]` targets; a test checks that none are omitted. The allocation-sensitive `footprint` suite keeps its own process.

Debug builds retain line numbers and backtraces without full variable and type information. Set `CARGO_PROFILE_DEV_DEBUG=2` when you need to inspect locals in a debugger. The release profile is unchanged.

## History

This implementation began in 2026 as a port of Go Vibescript v0.70.0, which was its compatibility reference: shared fixtures, a replay of Go's whole test suite and differential comparisons held it to Go's results, apart from [deliberate differences](docs/compatibility.md) where Go contradicted Vibescript's documented value semantics or behaved inconsistently. [The port history](docs/language-port.md) summarizes that work. On 2026-09-24 the Rust implementation became the reference and the language moved to static types and a canonical surface (ADR-007 and ADR-008); the Go implementation is deprecated and keeps the earlier, dynamically typed language.
