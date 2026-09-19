# WASI conformance

This consumer crate runs the core library on `wasm32-wasip1` without the native Tokio runner. It shares the language, syntax-rejection and collection-boundary tests with native builds. The filesystem witness covers configured roots, deep virtual preopens, links, escaping paths, exact filename spelling, overlapping preopens, renamed directories, permissions, source limits, cache refresh, call isolation and execution limits.

The full gate currently exposes normal-stack overflows in Node 26's debug bare-call nesting and deeply nested type syntax tests. Both remain enabled and pass under Wasmtime. WASI support is experimental until the target gate passes.

Install a Rust toolchain with the matching `wasm32-wasip1` standard library, Node and Wasmtime, then run from the repository root:

```sh
python3 tests/platforms/wasi/check.py --wasmtime /path/to/wasmtime
```

The script uses `scripts/cargo`, one build job and one WASI test thread. Logs, generated fixtures and a JSON report stay under `.cache/wasi-tests`. It uses normal stack limits and checks that module-loading counters agree between hosts. Select a Rust toolchain by putting its binaries first on `PATH`; compiler and standard-library builds must match.

The verified host configurations are Node 26 and Wasmtime 48. Wasmtime rejects reading absolute symlink targets, including targets within the configured root. Its witness checks that rejection; Node also exercises supported absolute links. The rename test checks descriptor retention on Wasmtime and explicitly records Node's different path-based behavior. Node is used for functional comparison, not a confinement guarantee. Neither host restriction is bypassed; see [platform support](../../../docs/platforms.md).

The broader comparison harness also runs host registrations, capabilities, required modules and the unchanged example corpus through `vibescript-wasi-compare`. WASI's local timezone is UTC, so timezone-dependent expectations must come from native Go and Rust runs with `TZ=UTC`.
