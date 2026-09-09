<p align="center">
  <img src="docs/logo-dark.svg" alt="Vibescript" height="60">
</p>

---

As [vibe coding](https://en.wikipedia.org/wiki/Vibe_coding) grows in popularity, there will be many domains where we need to narrow what users can build. Instead of giving them a blank canvas, we can offer an opinionated set of well-defined primitives that combine into predictable, safe applications. Think of it less like traditional software development and more like [HyperCard](https://en.wikipedia.org/wiki/HyperCard): flexible, but within bounds.

Even in these constrained environments, non-technical users still need a way to express custom logic. That’s where Vibescript comes in. It’s a small, Ruby-like workflow language designed to be easy to read and easy for AI to vibe code. The interpreter is written in Go and embeds directly into any Go application; the host keeps control of scheduling, I/O, and authority.

**Key Features**

- Ruby-like syntax: named functions, synchronous blocks, ranges, classes, enums,
  and string-keyed hashes.
- Gradual typing: optional annotations, nullable `?`, enums, positional/keyword args, return checks.
- Time & Duration helpers: literals, math, offsets (`ago`/`after`), Go-layout `format`.
- Money type and helpers.
- Embeddable in Go with typed capabilities and namespace-style modules.
- Interactive REPL with history, autocomplete, help/vars panels.

### Deliberately Small Language Surface

Vibescript is for data-driven business rules inside a host application, not
general-purpose Ruby programs:

- Named functions and methods are called directly. Blocks stay attached to the
  call that runs them synchronously; executable code cannot be stored or passed
  around as a value.
- Arrays and hashes behave as values, so an update never changes a sibling
  alias. Hash labels, string keys, and symbol keys all address one string
  keyspace.
- Modules provide explicit namespaces, not mixins or behavior injection.
- The Go host owns concurrency, delay, scheduling, and external effects. Scripts
  reach approved systems only through capabilities supplied for that call.

See the [language reference](docs/language_reference.md), the
[1.0 migration guide](docs/migrating-to-1.0.md), and
[ADR-006](docs/adr/006-slim-language-for-predictable-sandboxing.md) for the
complete boundary and its rationale.

```vibe
# Quick leaderboard report with typing, time math, and blocks
def leaderboard(players: array, since: time? = nil, limit: int = 5) -> array
  cutoff = since
  if cutoff == nil
    cutoff = 7.days.ago(Time.now)
  end

  recent = players.select do |p|
    Time.parse(p[:last_seen]) >= cutoff
  end

  ranked = recent.map do |p|
    {
      name: p[:name],
      score: p[:score],
      last_seen: Time.parse(p[:last_seen]),
    }
  end

  sorted = ranked.sort do |a, b|
    b[:score] - a[:score]
  end

  top = sorted.first(limit)

  top.map do |entry|
    {
      name: entry[:name],
      score: entry[:score],
      last_seen: entry[:last_seen].format("2006-01-02 15:04:05"),
    }
  end
end
```

### Quick Start Example

> [!WARNING]
> This project is in active development. Expect breaking changes until it reaches a tagged 1.0 release.

```go
package main

import (
    "context"
    "fmt"

    "github.com/mgomes/vibescript/vibes"
    "github.com/mgomes/vibescript/vibes/value"
)

func main() {
    engine, err := vibes.NewEngine(vibes.Config{})
    if err != nil {
        panic(err)
    }

    script, err := engine.Compile(`
    def total_with_fee(amount)
      amount + 1
    end
    `)
    if err != nil {
        panic(err)
    }

    result, err := script.Call(
        context.Background(),
        "total_with_fee",
        []value.Value{value.NewInt(99)},
        vibes.CallOptions{},
    )
    if err != nil {
        panic(err)
    }

    fmt.Println("total:", result.Int())
}
```

Scripts can live in `.vibe` files or be embedded inline. Host applications expose capabilities by seeding `CallOptions.Globals` or registering typed adapters through `CallOptions.Capabilities` before invoking functions.

### Interactive REPL

The `vibes` CLI includes an interactive REPL for experimenting with the language:

```bash
vibes repl
```

The REPL maintains a persistent environment, so variables assigned in one expression are available in subsequent ones. It also provides command history (navigate with up/down arrows) and tab completion for built-in functions, keywords, and defined variables.

#### Commands

| Command  | Description            |
| -------- | ---------------------- |
| `:help`  | Toggle help panel      |
| `:vars`  | Toggle variables panel |
| `:globals` | Print current globals |
| `:functions` | List callable functions |
| `:types` | Show global value types |
| `:last_error` | Show previous error |
| `:clear` | Clear output history   |
| `:reset` | Reset the environment  |
| `:quit`  | Exit the REPL          |

#### Keyboard Shortcuts

| Key      | Action                 |
| -------- | ---------------------- |
| `ctrl+k` | Toggle help            |
| `ctrl+v` | Toggle variables panel |
| `ctrl+l` | Clear history          |
| `ctrl+c` | Quit                   |
| `Tab`    | Autocomplete           |

## Editor Support

Vibescript now has a tree-sitter plugin and an official Zed extension for syntax highlighting and editor support:

- https://github.com/mgomes/tree-sitter-vibescript
- https://zed.dev/extensions/vibescript

A language server ships with the CLI (`vibes lsp`) and provides diagnostics,
hover, and completion in any LSP-capable editor. Setup instructions and the
current feature/limitation list live in [docs/lsp.md](docs/lsp.md).

## Examples

Representative `.vibe` programs are grouped under `examples/`:

- `examples/basics/` – literals, arithmetic, named calls, and explicit module
  namespaces.
- `examples/collections/` – collection value updates and normalized
  string/symbol hash lookups.
- `examples/control_flow/` – conditionals and recursion examples.
- `examples/enums/` – nominal enum values, typed coercion, and serialization.
- `examples/strings/` – string normalization, predicates, and splitting helpers.
- `examples/blocks/` – synchronous, call-attached transformations
  (map/select/reduce) over collections.
- `examples/hashes/` – hash manipulation, merging, and reporting helpers.
- `examples/loops/` – range iteration, collection loops, and accumulation helpers.
- `examples/ranges/` – range literals, ascending/descending iteration, and filtered collection helpers.
- `examples/money/` – exercises for the `money` and `money_cents` built-ins.
- `examples/durations/` – duration literals, math (add/sub/mul/div/mod), and time offsets.
- `examples/time/` – Time creation, formatting (Go layouts), and duration/time math.
- `examples/errors/` – patterns that rely on `assert` for validation.
- `examples/capabilities/` – samples that touch `ctx`, `db`, `events`, and other declared capabilities.
- `examples/background/` – jobs and events workflows delegated to host-provided
  capability adapters.
- `examples/policies/` – authorization helpers consulted by manifest policies.

## Documentation

Long-form guides live in `docs/`:

- `docs/introduction.md` – overview and table of contents.
- `docs/arrays.md` – array helpers including map/select/reduce, first/last, push/pop, sum, and set-like operations.
- `docs/strings.md` – string helpers like strip/upcase/downcase/split and related utilities.
- `docs/hashes.md` – hashes with one string keyspace, merge, and iteration helpers.
- `docs/stdlib_core_utilities.md` – complete method reference for strings, arrays, hashes, numerics, money, durations, times, and builtin functions.
- `docs/errors.md` – parse/runtime error formatting and debugging patterns.
- `docs/control-flow.md` – conditionals, loops, and ranges.
- `docs/blocks.md` – working with block literals for enumerable-style operations.
- `docs/tooling.md` – CLI workflows for running, checking, formatting,
  analyzing, testing, editor integration, and the REPL.
- `docs/architecture.md` – internal runtime/parser/module architecture notes for maintainers.
- `docs/integration.md` – integrating the interpreter in Go applications.
- `docs/host_cookbook.md` – production integration patterns for embedding hosts.
- `docs/starter_templates.md` – starter scaffolds for common embedding scenarios.
- `docs/durations.md` – duration literals, conversions, and arithmetic.
- `docs/time.md` – Time creation, formatting with Go layouts, accessors, and time/duration math.
- `docs/typing.md` – gradual typing: annotations, nullable `?`, positional/keyword binding, and return checks.
- `docs/enums.md` – nominal enums, `::` member access, and typed symbol coercion.
- `docs/language_reference.md` – consolidated language syntax and semantics reference.
- `docs/syntax_compatibility.md` – core syntax freeze baseline and compatibility guarantees.
- `docs/migrating-to-1.0.md` – breaking changes in the 1.0 release with before/after examples and fixes.
- `docs/examples/` – runnable scenario guides (campaign reporting, rewards, notifications, module usage, and more).
- `docs/releasing.md` – GoReleaser workflow for changelog and GitHub release automation.
- `docs/compatibility.md` – supported Go versions and CI coverage notes.
- [Building Vibescript](docs/building.md) – default and optional SIMD builds,
  native validation, and benchmark artifacts.
- `docs/versioning.md` – semantic versioning policy and compatibility contract.
- `docs/deprecation_policy.md` – deprecation lifecycle for public Go embedding APIs.
- `docs/known_issues.md` – tracked P0/P1 correctness bug bar.
- `ROADMAP.md` – versioned implementation checklist and release roadmap.
- `templates/` – copy-friendly starter templates for common host integration patterns.

## Development

This repository uses [Just](https://github.com/casey/just) for common tasks:

- `just test` runs the full Go test suite (`go test ./...`).
- `just test-race` runs the full Go test suite with the race detector (`go test -timeout 30m -race ./...`).
- `just bench` runs the core execution benchmarks (`go test ./vibes -run '^$' -bench '^BenchmarkExecution' -benchmem`).
- `just lint` checks formatting (`gofmt`) and runs `golangci-lint` with a generous timeout.
- `just deadcode` reports functions that neither the CLI nor any test can reach (`deadcode -test ./...`); it is a report, not a CI gate.
- `just install` installs the `vibes` binary to `$GOBIN` (or `$GOPATH/bin` when `GOBIN` is unset); pass a custom directory with `just install /usr/local/bin`.
- `vibes check <script.vibe>` statically checks typed boundaries across the whole script without executing it.
- `vibes fmt <path>` applies canonical formatting to `.vibe` files (`-check` for CI, `-w` to write).
- `vibes analyze <script.vibe>` runs script-level lint checks (e.g., unreachable statements).
- `vibes test [path...]` discovers and runs `*_test.vibe` files (assert-based, `-run` to filter).
- `./scripts/check_ci_green.sh` verifies latest `master` CI run is green.
- `./scripts/release_rehearsal.sh <version>` runs repeatable pre-tag release checks.
- `vibes lsp` starts the language server (hover/completion/diagnostics over stdio); see [docs/lsp.md](docs/lsp.md).
- Add new recipes in the `Justfile` as workflows grow.

CI also publishes benchmark artifacts via `.github/workflows/benchmarks.yml` on
pull requests and pushes to `master`.

Contributions should run `just test` and `just lint` (or the equivalent `go` and `golangci-lint` commands) before submitting patches.

## Runtime Sandbox & Limits

Vibescript runs inside a constrained interpreter to help host applications enforce safety guarantees:

- **Step quota:** Every `Execution` tracks steps (expressions/statements). `Config.StepQuota` caps how much code can run before aborting (default 1M). Useful to prevent unbounded loops; bump for heavy workloads.
- **Recursion limit:** `Config.RecursionLimit` bounds call depth (default 256) to avoid stack blowups from runaway recursion.
- **Memory quota:** `Config.MemoryQuotaBytes` limits interpreter allocations (default 16 MiB). Exceeding the limit raises a runtime error instead of consuming host memory. An unlimited quota (`vibes.Unlimited`) skips the reachable-graph accounting entirely.
- **Quota profiles:** For coherent step/memory/recursion bundles, use the named profiles instead of setting each field by hand — `vibes.ProfileLow` (1M steps / 16 MiB / 256), `ProfileMedium` (20M / 128 MiB / 1,000), `ProfileHigh` (200M / 512 MiB / 4,000), and `ProfileXHigh` (unlimited / unlimited / 10,000). Apply one with `ProfileHigh.ApplyTo(&cfg)`, or look one up by name with `vibes.QuotaProfileByName`. `ApplyTo` writes every quota field, so layer per-quota overrides after applying a profile rather than before. The `vibes` CLI selects them via `-profile` and defaults to `xhigh` (it runs your own scripts, so it is not a sandbox); see [docs/tooling.md](docs/tooling.md#quota-profiles). A zero-value `Config` (no quotas set) resolves to the `low` budget, so `low` reproduces the default embedding sandbox.
- **Effects control:** `Config.StrictEffects` can be set to require explicit capabilities for side-effecting operations (e.g., modules or host adapters), letting embedders keep the sandbox tight.
- **Module search paths:** `Config.ModulePaths` controls where `require` may load modules from. Only approved directories are searched; invalid paths return an error from `NewEngine`.
- **Stdlib input guards:** JSON, Regex, and format helpers enforce fixed caps — 1 MiB for `JSON.parse` input, `JSON.stringify` output, and format output, 10,000 nested JSON containers, 1 MiB for regex text/replacements/output, 16 KiB for regex patterns, and 256 MiB for `scan`'s worst-case match-index table. The canonical values live in `internal/runtime/limits.go`; see [docs/stdlib_core_utilities.md](docs/stdlib_core_utilities.md) for details.
- **Result rendering guard:** The runtime call returns before its result is formatted, so result rendering is outside the step and memory quotas. `Value.StringBounded` renders a value while stopping at a caller-supplied byte budget instead of materializing an unbounded string for a large composite. The `vibes run` CLI uses it with a 1 MiB cap and fails with `result rendering exceeds …` rather than printing a truncated value; see [docs/tooling.md](docs/tooling.md#result-rendering-limit).
- **Capability gating:** Host code injects safe adapters via `CallOptions.Capabilities`, so scripts can only touch what you expose. Globals can be seeded via `CallOptions.Globals` for per-call isolation.

Example with explicit limits:

```go
engine, err := vibes.NewEngine(vibes.Config{
    StepQuota:              10_000,   // abort after 10k steps
    MemoryQuotaBytes:       256 << 10, // 256 KiB heap cap inside the interpreter
    RecursionLimit:         32,       // shallow recursion allowed
    StrictEffects:          true,     // require capabilities for side effects
    ModulePaths:            []string{"/opt/vibes/modules"},
})
if err != nil {
    return err
}

script, _ := engine.Compile(source)
result, err := script.Call(ctx, "run", nil, vibes.CallOptions{
    Capabilities: []vibes.CapabilityAdapter{mySafeAdapter{}},
    Globals:      map[string]value.Value{"tenant": value.NewString("acme")},
})
```

These knobs keep embedded Vibescript code in a defensive sandbox while still allowing host-approved capabilities. Adjust quotas per use case; the defaults favor safety over throughput.
