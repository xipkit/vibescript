# Platform support

## WASI

The library, the `vibes` CLI and the full test suite build and run on `wasm32-wasip1`. [`scripts/check-wasi`](#verification) runs them under Wasmtime; Node's WASI implementation is exercised for comparison. A host must provide preview 1 clocks, randomness and filesystem access.

### Module roots

A WASI host exposes selected directories as preopens; `ModuleConfig::paths` refers to their guest paths. For example, a host mapping a directory to `/scripts` can configure `/scripts` or a directory beneath it as a module root. The ancestors of that guest path need not exist as accessible directories.

Configured roots retain open directory handles. With a host that implements descriptor-relative filesystem operations, renaming or replacing the original path does not redirect a configured engine. Relative imports stay within the root selected for the importing file, including when separate preopens have overlapping guest paths. Walking an outer root follows that directory's contents; a nested preopen must be configured separately to select its different backing directory. Script paths and symlinks are walked component by component without implicitly following links, using the same source limits, filename checks, permissions and accounting as native file loading. A configured root is resolved like a canonical native path: links are expanded and `..` applied component by component through the host's preopens before the resulting directory is opened, so a link that names a nested preopen's guest path selects that preopen. All filesystem access uses safe standard library and Rustix operations.

Host filesystem behavior still matters. The rename witness confirms that Wasmtime 48 preserves directory identity. Node 26's WASI layer resolves child operations through stored path names, so replacing a configured directory can redirect later reads. Node provides functional comparison coverage, but is unsuitable for confinement when filesystem paths may change. This agrees with [Node's documented WASI security limitations](https://nodejs.org/api/wasi.html#security). Wasmtime also rejects reading absolute symlink targets; the engine reports that error. Supported relative links remain confined to the configured module root. Both hosts run the [filesystem witness](../tests/platforms/wasi/README.md), with their different behavior explicitly asserted.

Memory counters include the target's path workspace. WASI uses wasi-libc's 4,096-byte `PATH_MAX`, the same workspace as native Linux; macOS uses 1,024 bytes. Module-loading memory peaks therefore differ slightly between targets.

### The CLI

`vibes.wasm` sees only the directories its host preopens, and every path it receives is a guest path:

```sh
wasmtime run --dir ./app::/app vibes.wasm --module-path /app/lib /app/main.vibe
wasmtime run --dir .::/ vibes.wasm -e 'require("helpers").run'
wasmtime run vibes.wasm -e '1 + 2'
```

Inline source runs without the working-directory root when the host exposes no working directory. See [required modules](cli.md#required-modules).

### Time, threads and panics

WASI local time is UTC unless the host sets `TZ`; named zones use `ZONEINFO` or the bundled IANA database, as described in [timezone sources](timezones.md). Deadlines use the host's monotonic clock.

WASI preview 1 has no threads. Cancellation is still cooperative: a host callback can cancel the running call, but no other thread can. The optional Tokio runner requires OS threads and does not build for WASI; the core and synchronous host capabilities do not require it.

The target aborts on panic instead of unwinding. A panicking host callback therefore ends the WebAssembly instance rather than returning to the embedder.

### Stack use

Parsing and code generation run on a heap task stack, and syntax trees are dropped iteratively. Every form at the syntax nesting limit parses, generates code, runs and drops, and every value at the 10,000-container limit is built, compared, rendered, encoded, imported and dropped, within Rust's default 1 MiB linear stack and Wasmtime's default 512 KiB WebAssembly stack in a debug build. Node's default stack also suffices.

The type checker is the exception. It recurses once per level of syntax, which native targets give a thread with a 64 MiB stack, and a debug build's costliest forms exhaust WASI's default stacks from about 220 levels. WASI has no threads, so there the checker refuses syntax more than 128 levels tall: such a source fails to compile with `V0001`, `syntax nesting too deep to type check`, where native targets accept the parser's 1,024. Nested class and module declarations are walked on the heap, and a top-level declaration taller than the limit is refused the same way.

Host callbacks that re-enter the script through `HostCall::call_block` nest on the native stack, as they do natively, because the host's own frames sit between the two calls. `Limits::recursion`, 256 frames by default, bounds that nesting. A debug build fits about 316 nested re-entries in the default stack. An embedder that raises the limit for deeply re-entrant host blocks should link a larger stack, for example with `-C link-arg=-zstack-size=4194304`.

### Features

Both default features and `--no-default-features` compile for WASI. The explicit SIMD scanners currently target ARM64 and x86_64; other targets use the portable scanners. Disabling `simd` selects those same portable scanners on native builds, although LLVM can still vectorize ordinary Rust code. Collection sizes and memory charges use 32-bit `usize` values, so memory peaks are smaller than on 64-bit targets.

### Verification

`scripts/check-wasi` runs Clippy for `wasm32-wasip1` in both feature configurations, the whole test suite under Wasmtime, CLI smoke tests and the filesystem witness. It uses the Rust toolchain and Wasmtime pinned under `.cache/toolchains` when they are present, and otherwise `rustc` with the `wasm32-wasip1` target and `wasmtime` from `PATH`. `--toolchain` and `--wasmtime` select others, and `--node` repeats the suite and witness under Node. It takes several minutes, so it is not part of `scripts/check`.

The runner maps the repository into the guest at its own path, so tests find their fixtures, and passes the host's local zone as `TZ`, so zone-dependent expectations match native runs. Tests whose subject WASI lacks are skipped there, each with its reason: six that need parallel threads, such as cancellation from another thread; two that catch panics; and the CLI tests that run `vibes` as a subprocess. Checks that concurrent calls stay isolated run the same calls one after another. Under Node, one test is skipped because Node's WASI layer drops the sub-second part of file times that the test restores.

## Other platforms

This support does not provide a browser adapter for `wasm32-unknown-unknown`. Browser clocks, entropy, local-time integration and module provisioning remain separate work. Native macOS is exercised locally; Windows, Android and iOS have cross-compilation evidence but still need execution on those platforms, and native Linux validation remains pending.
