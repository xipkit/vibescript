# ADR-005: Dev-mode module reloading via `Config.DevMode`

## Status

Accepted - 2026-07-12

## Decision

We will add an opt-in `Config.DevMode bool` to the embedding engine. When
enabled, every `require` revalidates its cached module against the source
file's mtime+size and recompiles the module when the file changed, and require
misses are re-resolved from disk instead of being negatively cached. The zero
value keeps production behavior: modules compile once and are served from
cache until `ClearModuleCache()`.

Top-level script reloading stays a host responsibility, documented as a
cookbook recipe rather than shipped as API.

## Context

The module cache is keyed by normalized module name only. In a long-running
embedding host — the documented compile-once, `Call`-per-request pattern — an
edited `require`d module is served stale until the host calls
`ClearModuleCache()` or restarts. The docs said exactly that, and the roadmap
carried "module cache invalidation policy for long-running hosts" until
`ClearModuleCache()` landed as the manual answer.

Manual invalidation is the right production posture but a poor development
loop: a host author iterating on `.vibe` modules has to wire cache clearing
into their own file watching, or restart per edit. The CLI already solves this
for file-based iteration (`vibes run -watch` rebuilds a fresh engine per run),
so the gap is specific to embedders. Rails' development-mode code reloading is
the shape developers expect here.

Two facts constrain the design. First, all modules resolve to real files —
there is no in-memory module provider — so file stamps are a complete change
signal. Second, `Engine.Compile` takes source text, not a path: the engine
never learns where the host's top-level script came from, so it cannot detect
top-level changes on its own.

## Semantics

- On a dev-mode cache hit, the engine stats the module's source path and
  compares mtime+size against the stamp recorded at compile time. A changed or
  deleted file evicts the entry and the normal load path recompiles it. The
  stamp is captured from the same open file the source bytes were read from,
  so it always describes the compiled content.
- The derived require caches (search-path hit, negative miss, did-you-mean
  candidates) are bypassed in dev mode. They are perf short-circuits over
  filesystem state dev mode expects to change; the negative-miss bypass is
  what makes a newly created module file loadable without restart.
- Each `Call` still sees one consistent version of every module it requires
  (the per-execution module table already guarantees this), so a mid-call edit
  never mixes versions within a call. Revalidation is skipped for modules the
  execution has already required, so a repeated require in the same call
  neither recompiles nor fails when the file was edited — or broken — mid-call.
- Eviction is guarded: a stale entry is deleted only if the cache still holds
  the exact entry the checker observed, so a concurrent goroutine's fresh
  reload is never evicted. Concurrent requires of a just-edited module may
  duplicate compile work; the first insert wins and all callers converge on
  it. That waste is bounded, dev-only, and accepted.
- `ClearModuleCache()` behaves identically in both modes.

## Non-goals

- Production use. Every require costs a stat, and a reload is not atomic
  across concurrently running calls.
- Watching or reloading the host's top-level script. The engine compiles
  text; file lifecycle belongs to the host (see the host cookbook recipe).
- Detecting content changes that preserve both mtime and size.

## Consequences

Easier: iterating on module code against a running host — edit a `.vibe`
file, call again, see the change. New modules appear without restart. Hosts
no longer wire their own watcher just for development.

Harder / what we now owe: a second module-loading mode to keep correct as the
loader evolves — every future cache added to the require path must decide its
dev-mode behavior, and the dev-mode test suite
(`internal/runtime/modules_devmode_test.go`) is the guard. mtime+size is a
heuristic: an editor that rewrites a file preserving both within filesystem
timestamp granularity can miss a reload (mitigated by size participating in
the stamp). The `Config` field is permanent Tier 1 API surface.

## Alternatives Considered

- **Reload-policy enum instead of a bool.** Only two meaningful policies
  exist today, and `StrictEffects` sets the precedent for boolean modes. An
  enum can arrive later as a new field without breaking the bool.
- **Content hashing instead of mtime+size.** Detects every change, but
  re-reads every module on every require, defeating the cache dev mode exists
  to keep warm.
- **fsnotify watcher inside the engine.** Precise and stat-free, but pushes
  watcher/goroutine lifecycle onto every host engine and adds the dependency
  to the runtime (it is CLI-only today). Hosts that want push-based reload can
  build it on `ClearModuleCache()`.
- **Per-Call revalidation sweep.** `Call` cannot know which modules a call
  will require, so it must stat the entire cache per call, taxing scripts
  that never require.
- **`CompileFile`/`IsStale` helpers for top-level scripts.** Bakes a
  file-storage model into permanent API; hosts load sources from databases
  and request payloads. A cookbook recipe covers the file case in a dozen
  lines.
- **Singleflight dedupe of concurrent recompiles.** More machinery than the
  bounded, dev-only duplicate work justifies.

## Links

- Implementation: `internal/runtime/engine.go` (`Config.DevMode`),
  `internal/runtime/modules.go` (stamp capture and revalidation).
- Docs: `docs/host_cookbook.md` §7, `docs/integration.md`.
- Related: ADR-002 (quota profiles; the `Config` zero-value conventions this
  field follows).
