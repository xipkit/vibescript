# WASI filesystem witness

The main test suite runs on `wasm32-wasip1` with the repository mapped into the guest at its own path. This consumer crate covers what that layout cannot: configured roots beneath virtual preopen ancestors, links, escaping paths, exact filename spelling, overlapping preopens, renamed directories, permissions, source limits, cache refresh, call isolation and execution limits. It builds the core library without the native Tokio runner.

Run it with the rest of the WASI checks from the repository root:

```sh
scripts/check-wasi          # Wasmtime
scripts/check-wasi --node   # Wasmtime, then Node
```

The script builds fixtures for each host, maps them at `/sandbox` and `/nested/sandbox`, and checks that module-loading counters agree between hosts. It uses normal stack limits. `node.mjs` runs any WASI command under Node with the same `--dir HOST::GUEST` mappings as Wasmtime.

The verified host configurations are Node 26 and Wasmtime 48. Wasmtime rejects reading absolute symlink targets, including targets within the configured root. Its witness checks that rejection; Node also exercises supported absolute links. The rename test checks descriptor retention on Wasmtime and explicitly records Node's different path-based behavior. Node is used for functional comparison, not a confinement guarantee. Neither host restriction is bypassed; see [platform support](../../../docs/platforms.md).

The `vibescript-wasi-compare` binary runs host registrations, capabilities, required modules and the example corpus for broader comparisons. WASI's local timezone is UTC unless the host sets `TZ`, so timezone-dependent expectations must come from native Go and Rust runs in the same zone.
