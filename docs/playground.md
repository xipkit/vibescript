# Browser playground runner

`playground` is a WASI preview 1 command for the static website's Web Worker.
Each instance reads **one JSON object through EOF** on stdin and writes one JSON
object followed by a newline on stdout. Script output is captured inside that
response. Requests get fresh engines, module caches and capability grants.
Protocol, compile and execution failures return `ok: false` and exit successfully;
only stream I/O failures exit unsuccessfully.

## Build

`scripts/build-playground` uses `./scripts/cargo --offline`, four build jobs, the
local WASI toolchain when available, and the `playground` profile: `opt-level="z"`,
`panic="abort"`, stripping, no debug information, thin LTO and one codegen unit.
Override the toolchain with `--toolchain PREFIX`. A rustc wrapper (`scripts/rustc-remap`) maps
the checkout and Cargo home out of embedded source paths, so the artifact names no
local directories and is the same for every checkout. The build uses a target
directory named for the wrapper's hash, so artifacts compiled without it are never
reused.

Artifacts are `target/playground/playground.wasm` and `manifest.json`. The manifest
records the Rust repository commit (`rust_commit`), working-tree dirty flag,
compiler version, target, profile, raw byte count and SHA-256 of the wasm. When
`brotli` is installed, the script also writes `playground.wasm.br` at quality 11
and records its byte count. Ship an artifact with `dirty: false` and retain its
manifest. Wasm size is independent of the native host architecture.

Measured with Rust 1.98.1 on macOS ARM64: **3,822,420 bytes raw** and
**904,866 bytes Brotli** (quality 11). The earlier engine-only measurement was
approximately 3.8 MB raw and 0.97 MB Brotli; it is a sizing reference, not a
performance benchmark or a paired comparison of identical binaries.

## Request

```json
{
  "op": "run",
  "source": "def greet(name: string) -> string\n puts name\n name.upcase\nend\n",
  "entry": "greet",
  "args": ["Ada"],
  "files": {},
  "limits": { "steps": 250000, "memory_bytes": 262144, "recursion": 32 },
  "capabilities": []
}
```

| Field | Type | Default / meaning |
| --- | --- | --- |
| `op` | `"check"`, `"run"`, `"format"`, `"fix"` | Required. |
| `source` | string | Required UTF-8 Vibescript source. |
| `files` | object mapping filenames to source strings | `{}`. At most 64 in-memory files. |
| `entry` | string or null | Null/omitted runs top-level statements (`__main__`). A name calls that function directly without first running top-level statements, as `Script::call` does. |
| `args` | array of JSON values | `[]`. Positional entry arguments, checked against parameter types. |
| `limits` | object | Omitted fields use the website defaults shown above. |
| `capabilities` | array of capability declarations | `[]`. Preview methods only; schema below. |

Unknown fields and operations are rejected. The encoded request is limited to
1 MiB. Limits must be positive integers: at most 10,000,000 steps, 16 MiB of
tracked invocation memory and 128 recursive frames. Zero and null do not disable
quotas. `entry` and `args` are used only by `run`.

`run` compiles, then executes the selected entry under those limits. `check`
compiles and returns diagnostics without executing. `format` returns canonical
formatting (including normalized line endings and a final newline) for the source
and every supplied file, even when they do not compile. `fix` applies complete
machine-applicable fixes and recompiles the whole project after each fix. It
returns edited source and files, count of applied fixes and remaining diagnostics.
Suggestions stay unapplied. Repeated edits and a 256-fix ceiling stop repair;
remaining errors still make `ok` false. Nothing is written to disk.

Compilation and JSON conversion have independent guards of 5,000,000 steps and
16 MiB. The compiler's checker charges work after analysis, so this is **not a
wall-clock deadline**. Formatting and repair are bounded by input size and repair
count. Use a disposable Worker and terminate superseded checks or requests that
exceed the website's wall-clock deadline. Debouncing checks belongs to the site.

### In-memory modules

```json
{
  "op": "run",
  "source": "require(\"lib/helper\").answer",
  "files": {
    "lib/helper.vibe": "def answer -> int\n require('../numbers').value\nend\n",
    "numbers.vibe": "def value -> int\n 42\nend\n"
  }
}
```

Keys are canonical root-relative filenames with extensions. Absolute names, drive
prefixes, backslashes, NUL, `.` and `..` components are rejected. Import requests
use ordinary resolution: `require("lib/helper")` adds `.vibe`; `./` and `../`
resolve from a required file's directory and cannot escape the virtual root. The
unnamed main source uses root-relative imports without `./`. Only supplied files
resolve. Files initialize once per invocation; later requests start fresh.
Module diagnostics refer to their own source; `fix` returns module edits under
their corresponding key in `files`.

### Preview capabilities

```json
{
  "op": "run",
  "source": "email.send(\"ada@example.org\", \"Hello\", \"Your order shipped.\")",
  "capabilities": [{
    "name": "email",
    "members": [{
      "name": "send",
      "behavior": "preview",
      "signature": {
        "params": [
          { "name": "to", "type": "string" },
          { "name": "subject", "type": "string" },
          { "name": "body", "type": "string" }
        ],
        "result": "{ to: string, subject: string, body: string, status: string }"
      }
    }]
  }]
}
```

This returns the three argument fields plus `status: "preview"`. It follows the
site's Go email and SMS adapters in `internal/notifications`: no message is sent.
For SMS, omit `subject` and name the capability `sms`. Rust uses a record result
as above or `hash<string, any>` instead of the old Go adapter's bare `hash`.

Each capability requires `name` and `members`. Each member requires `name`,
`behavior: "preview"`, and `signature` with `params` and `result`. Each parameter
requires `name` and `type` strings, with optional `optional: boolean` (default
false). Types use host signature syntax and are checked statically and at dynamic
boundaries. The result type must accept the preview record; a mismatch produces
a type error. Omitted optional arguments are absent from the record. Keywords and
blocks are unsupported.

At most 64 capabilities, 64 members per capability and 64 parameters per member
are allowed. Names are ASCII identifiers, unique within their scope. `status` is
reserved in parameter lists. Methods return arguments under their parameter names
plus the fixed preview status; requests cannot supply executable host behavior.

## Response

| Field | Type | Meaning |
| --- | --- | --- |
| `ok` | boolean | Whether the operation succeeded. |
| `output` | array of strings | Captured `puts`, `print` and `p` lines. |
| `stderr` | array of strings | Captured `warn` lines. |
| `result` | JSON value or null | Run result; null for other operations or errors (also for successful `nil`). |
| `error` | object or null | `{kind, message, location}`; null on success. |
| `diagnostics` | array of objects | Same objects as `vibes check --json`, including warnings. |
| `stats` | object | `{steps, peak_memory_bytes, retained_memory_bytes}`. |
| `source` | string | Present for `format` and `fix`. |
| `files` | object | Present for `format` and `fix`, including unchanged files. |
| `applied` | integer | Present for `fix`; number of fixes applied. |

All fields except the last three are always present. `error.kind` is the engine
category: `Syntax`, `Type`, `Name`, `Argument`, `Arithmetic`, `Json`, `OutputLimit`,
`Steps`, `Memory`, `Recursion`, `Runtime`, `Host`, `Cancelled`, `Deadline` or
`ControlFlow`. Invalid requests use `Argument`. `location` is null when unavailable,
otherwise `{file: string|null, line: integer, column: integer, offset: integer|null}`.
Lines and Unicode character columns are one-based; byte offsets are zero-based.
The main source has `file: null`.

Diagnostics carry `code`, `name`, `severity`, `file`, `span`, `message`, `expected`,
`found`, `labels` and `fixes`. Notes are labeled secondary spans in `labels`, exactly
as in the CLI. Spans contain zero-based UTF-8 byte `start`/`end` (end-exclusive),
and one-based `line`, `column`, `end_line`, `end_column`. Fixes have `message`,
`applicability` (`always` or `suggestion`) and `edits` with `span` and `replacement`.
An uncoded syntax error becomes `V0001`. See [diagnostics](diagnostics.md).

Output preserves order within each stream; adjacent `print` writes join the same
line. A final newline terminates the last line without adding an empty element.
An unterminated final line is included; invalid UTF-8 becomes U+FFFD. Each stream
is capped at 64 KiB; a refused write reports `OutputLimit` while preserving earlier
output. JSON results are capped at 1 MiB. Non-JSON values (such as money, time and
duration) produce serialization errors; scripts should explicitly convert them.
Integer tokens remain exact in the runner; JavaScript consumers need lossless JSON
parsing to retain integers beyond the safe `Number` range.

Stats describe invocation work, including cold runtime module loading. They exclude
request parsing, initial compilation, JSON conversion and host-owned captured
output. Non-running operations and compile failures report zeros. Runtime failures
keep counters and locations, including quota exhaustion. Retained bytes are sampled
after execution storage is released and belong to the result or error. Tracked
bytes are not Wasm linear memory or process RSS; WASI and native byte counts differ.

## Worker host contract and verification

Supply the complete request followed by EOF on stdin, buffer stdout, call WASI
`_start` once per fresh instance and decode its single JSON response. Cache the
compiled `WebAssembly.Module` between requests, but instantiate fresh WASI state.
Serve wasm as `application/wasm`; deliver `.br` with `Content-Encoding: br` only
when the deployment is configured for that precompressed artifact.

Use **no preopened directories, inherited environment or network imports**. The
runner configures only memory module sources. A browser WASI shim should deny
filesystem/socket operations and provide stdio, lifecycle and the clock/entropy
operations required by ordinary language builtins (`Time.now`, random values and
UUIDs). Without `TZ`, WASI local time is UTC; named zones can use the bundled
database. Preview capabilities grant no external effects. Node WASI is not itself
a security boundary; browser confinement comes from the Worker's imports.

`scripts/check-playground` builds and exercises every operation under Wasmtime and
`node:wasi`, using `tests/platforms/wasi/node.mjs` without preopens. It covers typed
previews, relative imports, machine fixes, three quotas, an infinite loop,
filesystem rejection and identical counters across both hosts. `--no-build` tests
an existing artifact. Native tests live in the tools library; core loader and
failure-counter tests are registered in `tests/all.rs`. The general
`scripts/check-wasi` gate remains required.
