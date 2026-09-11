# Vibescript Roadmap and Implementation Checklist

This is the working implementation TODO for upcoming Vibescript releases.

How to use this file:

- Keep release scope realistic and bias toward shipping.
- Move unfinished items forward instead of silently dropping them.
- Link each checklist item to an issue/PR once created.
- Mark items complete only after tests/docs are updated.

Release status legend:

- `[ ]` not started
- `[~]` in progress (replace manually while working)
- `[x]` complete

---

## Historical Completed Releases

These releases are already shipped and tagged.
Completion dates reflect the corresponding git tag date.

### v0.1.0 (completed 2025-12-15)

- [x] Added GitHub Actions CI workflow.
- [x] Added stack traces to runtime errors.
- [x] Expanded baseline stdlib coverage.
- [x] Added optional-parens syntax support.
- [x] Added initial CLI support.
- [x] Fixed package/versioning setup for release tagging.

### v0.2.0 (completed 2025-12-15)

- [x] Added GoReleaser-based release automation.
- [x] Added recursion depth limit enforcement.
- [x] Added recursion limit test coverage.

### v0.2.1 (completed 2025-12-15)

- [x] Pinned GoReleaser version for stable release builds.

### v0.2.2 (completed 2025-12-15)

- [x] Adjusted GoReleaser configuration.

### v0.3.0 (completed 2025-12-20)

- [x] Added `Number#times`.
- [x] Added `Duration` class modeled after ActiveSupport-style duration semantics.

### v0.4.0 (completed 2025-12-21)

- [x] Added duration arithmetic support.
- [x] Improved optional-parens behavior for zero-arg methods.
- [x] Implemented `Time` class.

### v0.4.1 (completed 2025-12-21)

- [x] Renamed `Time#strftime` to `Time#format` for Go layout alignment.
- [x] Added Duration and Time documentation.

### v0.5.0 (completed 2025-12-26)

- [x] Added first pass of gradual typing.
- [x] Added support for classes.
- [x] Added block arguments in method definitions.
- [x] Added numeric literal underscore separators.
- [x] Expanded complex tests/examples coverage.

### v0.5.1 (completed 2026-02-07)

- [x] Expanded test suite breadth.
- [x] Landed performance optimizations and refactors.
- [x] Exported `CallBlock()` for embedders.

### v0.6.0 (completed 2026-02-11)

- [x] Added runtime memory quota enforcement.
- [x] Enforced strict effects for globals and `require`.
- [x] Isolated `Script.Call` runtime state per invocation.
- [x] Replaced panicking constructors with error-returning APIs.
- [x] Increased CLI/REPL test coverage and added execution benchmarks.

### v0.7.0 (completed 2026-02-12)

- [x] Shipped multi-phase string helper expansion.
- [x] Added regex/byte helper coverage and bang-method parity improvements.

### v0.8.0 (completed 2026-02-12)

- [x] Expanded Hash built-ins and hash manipulation support.
- [x] Refreshed hash docs and examples.

### v0.9.0 (completed 2026-02-12)

- [x] Expanded Array helper surface and enumerable workflows.

### v0.10.0 (completed 2026-02-12)

- [x] Made Time and numeric APIs coherent with documented behavior.

### v0.11.0 (completed 2026-02-12)

- [x] Improved parse/runtime error feedback and debugging quality.

### v0.12.0 (completed 2026-02-13)

- [x] Hardened `require` behavior for safer module composition.
- [x] Improved private helper/module boundary behavior.
- [x] Improved circular dependency diagnostics for modules.

### v0.13.0 (completed 2026-02-17)

- [x] Enforced capability contracts at runtime boundaries.
- [x] Added contract validation paths for capability args and returns.
- [x] Improved capability isolation and contract binding behavior.

---

## v0.14.0 - Capability Foundations (completed 2026-02-18)

Goal: make host integrations first-class and safe enough for production workflows.

### Capability Adapters

- [x] Add first-party `db` capability adapter interface and implementation.
- [x] Add first-party `events` capability adapter interface and implementation.
- [x] Add first-party `ctx` capability adapter for request/user/tenant metadata.
- [x] Define naming conventions for adapter method exposure (`cap.method`).
- [x] Ensure all adapters support context propagation and cancellation.

### Contracts and Safety

- [x] Add capability method contracts for all new adapter methods.
- [x] Validate args/kwargs/returns for `db` adapter methods.
- [x] Validate args/kwargs/returns for `events` adapter methods.
- [x] Add explicit data-only boundary checks for all capability returns.
- [x] Add contract error messages with actionable call-site context.

### Script Surface

- [x] Promote `examples/background/` scenarios to fully supported behavior.
- [x] Convert `examples/future/iteration.vibe` from stretch to supported.
- [x] Add docs for common capability patterns (query + transform + enqueue).
- [x] Add docs for capability failure handling patterns.

### Testing and Hardening

- [x] Add unit tests for each capability adapter method.
- [x] Add integration tests for mixed capability calls in one script.
- [x] Add negative tests for type violations and invalid payload shapes.
- [x] Add quota/recursion interaction tests with capability-heavy scripts.
- [x] Add benchmarks for capability call overhead.

### v0.14.0 Definition of Done

- [x] All new capabilities documented in `docs/integration.md`.
- [x] Background/future examples run in CI examples suite.
- [x] No contract bypasses in capability call boundaries.

---

## v0.15.0 - Type System v2

Goal: make types expressive enough for real workflows while keeping runtime checks predictable.

### Type Features

- [x] Add parametric container types: `array<T>`, `hash<K, V>`.
- [x] Add union types beyond nil: `A | B`.
- [x] Add typed object/hash shape syntax for common payload contracts.
- [x] Add typed block signatures where appropriate.
- [x] Define type display formatting for readable runtime errors.

### Type Semantics

- [x] Specify variance/invariance rules for container assignments.
- [x] Specify nullability interactions with unions (`T?` vs `T | nil`).
- [x] Define coercion policy (no coercion vs explicit coercion helpers).
- [x] Decide strictness for unknown keyword args under typed signatures.

### Runtime and Parser

- [x] Extend parser grammar for generic and union type expressions.
- [x] Extend type resolver and internal type representation.
- [x] Add runtime validators for composite/union types.
- [x] Add contract interop so capability contracts can reuse type validators.

### Testing and Docs

- [x] Add parser tests for all new type syntax forms.
- [x] Add runtime tests for nested composite type checks.
- [x] Add regression tests for existing `any` and nullable behavior.
- [x] Expand `docs/typing.md` with migration examples.

### v0.15.0 Definition of Done

- [x] Existing scripts without annotations remain compatible.
- [x] Type errors include parameter name, expected type, and actual type.
- [x] Capability contract validation can use the same type primitives.

---

## v0.16.0 - Control Flow and Error Handling

Goal: improve language ergonomics for complex script logic and recovery behavior.

### Control Flow

- [x] Add `while` loops.
- [x] Add `until` loops.
- [x] Add loop control keywords: `break` and `next`.
- [x] Add `case/when` expression support (if approved).
- [x] Define behavior for nested loop control and block boundaries.

### Error Handling Constructs

- [x] Add structured error handling syntax (`begin/rescue/ensure` or equivalent).
- [x] Add typed error matching where feasible.
- [x] Define re-raise semantics and stack preservation.
- [x] Ensure runtime errors preserve original position and call frames.

### Runtime Behavior

- [x] Ensure new control flow integrates with step quota accounting.
- [x] Ensure new constructs integrate with recursion/memory quotas.
- [x] Validate behavior inside class methods, blocks, and capability callbacks.

### Testing and Docs

- [x] Add parser/runtime tests for each new control flow construct.
- [x] Add nested control flow tests for edge cases.
- [x] Add docs updates in `docs/control-flow.md` and `docs/errors.md`.
- [x] Add examples under `examples/control_flow/` for each new feature.

### v0.16.0 Definition of Done

- [x] No regressions in existing `if/for/range` behavior.
- [x] Structured error handling works with assertions and runtime errors.
- [x] Coverage includes nested/edge control-flow paths.

---

## v0.17.0 - Modules and Package Ergonomics

Goal: make multi-file script projects easier to compose and maintain.

### Module System

- [x] Add explicit export controls (beyond underscore naming).
- [x] Add import aliasing for module objects.
- [x] Define and enforce module namespace conflict behavior.
- [x] Improve cycle error diagnostics with concise chain rendering.
- [x] Add module cache invalidation policy for long-running hosts.

### Security and Isolation

- [x] Tighten module root boundary checks and path normalization.
- [x] Add test coverage for path traversal attempts.
- [x] Add explicit policy hooks for module allow/deny lists.

### Developer UX

- [x] Add docs for module project layout best practices.
- [x] Add examples for reusable helper modules and namespaced imports.
- [x] Add migration guide for existing `require` users.

### v0.17.0 Definition of Done

- [x] Module APIs are explicit and predictable.
- [x] Error output for cycle/import failures is actionable.
- [x] Security invariants around module paths are fully tested.

---

## v0.18.0 - Standard Library Expansion

Goal: reduce host-side boilerplate for common scripting tasks.

### Core Utilities

- [x] Add JSON parse/stringify built-ins.
- [x] Add regex matching/replacement helpers.
- [x] Add UUID/random identifier utilities with deterministic test hooks.
- [x] Add richer date/time parsing helpers for common layouts.
- [x] Add safer numeric conversions and clamp/round helpers.

### Collections and Strings

- [x] Expand hash helpers for nested transforms and key remapping.
- [x] Expand array helpers for chunking/windowing and stable group operations.
- [x] Add string helpers for common normalization and templating tasks.

### Compatibility and Safety

- [x] Define deterministic behavior for locale-sensitive operations.
- [x] Add quotas/guards around potentially expensive operations.
- [x] Ensure new stdlib functions are capability-safe where required.

### Testing and Docs

- [x] Add comprehensive docs pages and examples for each new family.
- [x] Add negative tests for malformed JSON/regex patterns.
- [x] Add benchmark coverage for hot stdlib paths.

### v0.18.0 Definition of Done

- [x] New stdlib is documented and example-backed.
- [x] Runtime behavior is deterministic across supported OSes.
- [x] Security/performance guardrails are validated by tests.

---

## v0.19.0 - Tooling, Quality, and Performance

Goal: improve day-to-day developer productivity and interpreter robustness.

### Tooling

- [x] Add canonical formatter command and CI check.
- [x] Add language server protocol (LSP) prototype (hover, completion, diagnostics).
- [x] Add static analysis command for script-level linting.
- [x] Improve REPL inspection commands (globals/functions/types).

### Runtime Quality

- [x] Profile evaluator hotspots and optimize dispatch paths.
- [x] Reduce allocations in common value transformations.
- [x] Improve error rendering for deeply nested call stacks.
- [x] Add fuzz tests for parser and runtime edge cases.

### CI and Release Engineering

- [x] Add smoke tests for docs examples to CI.
- [x] Add release checklist automation for changelog/version bumps.
- [x] Add compatibility matrix notes for supported Go versions.

### v0.19.0 Definition of Done

- [x] Tooling commands are documented and stable.
- [x] Performance regressions are tracked with benchmarks.
- [x] CI includes example and fuzz coverage gates.

---

## v1.0.0 - Stabilization and Public API Commitment

Goal: lock the language and embedding API for long-term support.

### Stabilization

- [x] Freeze core syntax and document compatibility guarantees.
- [x] Freeze public Go embedding APIs or publish deprecation policy.
- [x] Publish semantic versioning and compatibility contract.
- [x] Complete migration notes for all pre-1.0 breaking changes.

### Documentation and Adoption

- [x] Publish complete language reference.
- [x] Publish host integration cookbook with production patterns.
- [x] Provide starter templates for common embedding scenarios.

### Final Readiness

- [x] Zero known P0/P1 correctness bugs.
- [x] CI green across supported platforms and Go versions.
- [x] Release process rehearsed and repeatable.

---

## v0.20.0 - Performance and Benchmarking (1.0 Push)

Goal: make performance improvements measurable, repeatable, and protected against regressions.

### Runtime Performance

- [x] Profile evaluator hotspots and prioritize top 3 CPU paths by cumulative time.
- [x] Reduce `Script.Call` overhead for short-running scripts (frame/env setup and teardown).
- [x] Optimize method dispatch and member access fast paths.
- [x] Reduce allocations in common collection transforms (`map`, `select`, `reduce`, `chunk`, `window`).
- [x] Optimize typed argument/return validation for nested composite types.

### Memory and Allocation Discipline

- [x] Reduce transient allocations in stdlib JSON/Regex/String helper paths.
- [x] Reduce temporary map/array churn in module and capability boundary code paths.
- [x] Add per-benchmark allocation targets (`allocs/op`) for hot runtime paths.
- [x] Add focused regression tests for high-allocation call patterns.

### Benchmark Coverage

- [x] Expand benchmark suite for compile, call, control-flow, and typed-runtime workloads.
- [x] Add capability-heavy benchmarks (db/events/context adapters + contract validation).
- [x] Add module-system benchmarks (`require`, cache hits, cache misses, cycle paths).
- [x] Add stdlib benchmarks for JSON/Regex/Time/String/Array/Hash hot operations.
- [x] Add representative end-to-end benchmarks using `tests/complex/*.vibe` workloads.

### Benchmark Tooling and CI

- [x] Add a single benchmark runner command/script with stable flags and output format.
- [x] Persist benchmark baselines in versioned artifacts for release comparison.
- [x] Add PR-time benchmark smoke checks with threshold-based alerts.
- [x] Add scheduled full benchmark runs with trend reporting.
- [x] Document benchmark interpretation and triage workflow.

### Profiling and Diagnostics

- [x] Add reproducible CPU profile capture workflow for compile and runtime benchmarks.
- [x] Add memory profile capture workflow for allocation-heavy scenarios.
- [x] Add flamegraph generation instructions and hotspot triage checklist.
- [x] Add a short "performance playbook" for validating optimizations before merge.

### v0.20.0 Definition of Done

- [x] Benchmarks cover runtime, capability, module, and stdlib hot paths.
- [x] CI reports benchmark deltas for guarded smoke benchmarks.
- [x] Measurable improvements are achieved before the v1.0.0 release tag.
- [x] Performance and benchmarking workflows are documented and maintainable.

---

## v0.21.0 - Nominal Enums and Type Hardening (completed 2026-03-08)

Goal: add first-class nominal enums and harden the typed runtime around enum resolution and coercion.

### Enum Language Support

- [x] Add top-level `enum` declarations with scoped `::` member access.
- [x] Add enum member reflection via `.name`, `.symbol`, and `.enum`.
- [x] Add enum-aware serialization for `JSON.stringify` and `string.template`.
- [x] Add typed enum annotations and matching symbol coercion across function and block boundaries.
- [x] Export top-level enums through `require` alongside module functions.

### Resolution and Correctness

- [x] Resolve enum types case-insensitively while rejecting ambiguous matches.
- [x] Preserve enum-named member access and allow enum labels in typed shape fields.
- [x] Reject enum names that shadow built-in types or use reserved suffix forms.
- [x] Fix enum lookup across shadowed envs, nullable types, block owner resolution, and union/hash-key normalization paths.
- [x] Guard recursive normalization against cyclic values.

### Tooling and Coverage

- [x] Add runnable enum examples and integration coverage aligned with `docs/enums.md`.
- [x] Upgrade the REPL to Bubble Tea v2.
- [x] Add race-detector coverage and tighten fuzz/benchmark quality gates.
- [x] Add a `just install` workflow for the CLI and document editor support integrations.
- [x] Make release workflow reruns idempotent for existing tags.

### v0.21.0 Definition of Done

- [x] Enum syntax and typed-runtime behavior are documented and exercised by runnable examples and tests.
- [x] Enum resolution behaves correctly across modules, blocks, shadowed scopes, and typed normalization paths.
- [x] Tooling and release automation changes are documented and stable for the next release cycle.

---

## v0.26.2 - Rosetta Port Compatibility Patch (completed 2026-03-08)

Goal: remove a handful of parser and runtime edge cases that were blocking direct ports of Ruby-flavored RosettaCode examples.

### Parsing and Control Flow

- [x] Stop `if`, `elsif`, `while`, `until`, `for`, `return`, and `raise` from accidentally consuming next-line literals and indexing expressions.
- [x] Preserve explicit multiline continuations in line-limited headers for chained calls and operators.
- [x] Support bare `return` in line-terminated statement form.

### Runtime Semantics

- [x] Make `&&` and `||` evaluate lazily so guard expressions short-circuit correctly.
- [x] Align signed `int / int` and `int % int` behavior with floor-style Ruby semantics.
- [x] Add `array.length`, `array.empty?`, and `array.fetch` for lower-friction Ruby ports.
- [x] Reject fractional numeric indices in `array.fetch` instead of truncating silently.

### Coverage and Release Confidence

- [x] Add regression tests for multiline control-flow headers, guard short-circuiting, signed integer arithmetic, and array helper behavior.
- [x] Keep integration expectations aligned with the updated integer arithmetic semantics.

### v0.26.2 Definition of Done

- [x] Rosetta-style ports no longer fail on newline-sensitive control-flow parsing, eager boolean guards, or missing array aliases.
- [x] Signed integer arithmetic and `array.fetch` behavior are covered by targeted regressions and the full `go test ./vibes` suite.

---

## v0.27.0 - Engine Containment Hardening (completed 2026-05-04)

Goal: close remaining host-facing containment gaps before the next pre-1.0 release.

### API Boundary Isolation

- [x] Return isolated script inspection snapshots for globals, types, functions, classes, enums, and modules.
- [x] Deep-clone object-valued builtin snapshots so public `Engine.Builtins()` callers cannot mutate stdlib method tables.
- [x] Preserve scalar string conversion and metadata lookup fast paths while adding snapshot hardening.

### Module Containment

- [x] Resolve and store module roots when an engine is constructed.
- [x] Prevent later cwd changes or symlink retargeting from redirecting `require`.
- [x] Reject non-regular module files before source reads.

### Runtime Breakout Guards

- [x] Enforce regex pattern, input, replacement, and output limits for regex-backed string member helpers.
- [x] Reject cyclic host-provided arrays in `array.flatten`.
- [x] Add focused containment regressions for mutable snapshots, module root drift, regex guard bypasses, and cyclic flattening.

### v0.27.0 Definition of Done

- [x] Engine-owned mutable state is not exposed through public snapshot APIs.
- [x] Module resolution remains anchored to engine construction-time roots.
- [x] Security hardening passes full tests, race-detector coverage, and benchmark smoke gates.

---

## v0.28.0 - Fuzz Coverage and Input Hardening (completed 2026-05-15)

Goal: make hostile or malformed user input exercise the same public surfaces users and tools reach in normal Vibescript workflows.

### Fuzz Coverage

- [x] Cover CLI argument/path handling, REPL input flow, LSP payload handling, and formatter input.
- [x] Cover lexer, parser, compiler, generated-script semantics, runtime edge cases, value operations, JSON round trips, module request normalization, module alias validation, module policy validation, capability input validation, and scalar input conversion helpers.
- [x] Keep seed cases focused on real boundary shapes instead of broad random text alone.

### Automation

- [x] Add `just fuzz` with a 10-second default for repeatable local sweeps.
- [x] Add a nightly GitHub Actions fuzz workflow so heavier coverage runs outside normal PR latency.

### Review Follow-ups

- [x] Preserve valid near-1 MiB source support when source text is wrapped in JSON-RPC LSP messages.
- [x] Keep keyword-named hash/object members reachable through dot access after parser access-expression hardening.

### v0.28.0 Definition of Done

- [x] User-input entry points have focused fuzz coverage for panic and invariant regressions.
- [x] Heavy fuzzing runs nightly rather than blocking every PR.
- [x] Codex review follow-ups are fixed, reviewed, and merged.

---

## v0.28.1 - Module Policy Normalization Patch (completed 2026-05-15)

Goal: ship the module policy normalization fix found by the local fuzz corpus after `v0.28.0`.

### Patch Scope

- [x] Make module policy pattern normalization idempotent for whitespace-only path segments.
- [x] Apply the same canonicalization path to policy module names.
- [x] Commit the fuzz-minimized regression case for replay by normal tests and future fuzz runs.

### v0.28.1 Definition of Done

- [x] The minimized `FuzzModulePolicyValidation` input passes.
- [x] Full tests pass.
- [x] Three full local `just fuzz` sweeps pass after the fix.

---

## v0.28.2 - DoS and Module Policy Bypass Patch (completed 2026-05-16)

Goal: close the quadratic `combineErrors` DoS and the empty/dot-only module-policy bypass classes surfaced by local fuzz sweeps after `v0.28.1`.

### Patch Scope

- [x] Replace the quadratic error-message concatenation in `combineErrors` with a linear join so invalid-UTF-8 input cannot drive CPU usage that scales with the square of the parse-error count.
- [x] Fail-close `enforceModulePolicy` on require arguments that normalize to empty whenever any allow- or deny-list is configured.
- [x] Strip at most one implicit `.vibe` from policy keys so allow-lists do not widen to sibling files like `helper.vibe.vibe` or `pkg/..vibe`.
- [x] Pin the bypass classes with new invariant tests and add the fuzz-minimized inputs to the committed corpus.

### v0.28.2 Definition of Done

- [x] Targeted regression tests for the DoS and policy bypasses pass.
- [x] Full tests pass.
- [x] Multiple full local `just fuzz` sweeps pass after the fixes.
- [x] Codex review follow-ups for this patch are fixed, reviewed, and merged.

---

## v0.29.0 - Public API Refactor (completed 2026-05-17)

Goal: finish the pre-1.0 package-boundary cleanup so embedders have a small,
intentional public API and Vibescript internals can keep moving without
becoming accidental contracts.

### Public Package Boundaries

- [x] Move value-system types and constructors to `vibes/value`.
- [x] Move first-party capability adapter contracts to
  `vibes/capability/{contextcap,db,events,jobqueue}`.
- [x] Move public source positions to `vibes/source`.
- [x] Remove the v0.28 deprecation alias bridge from the root `vibes` package.
- [x] Keep the root `vibes` package focused on engine/script execution,
  capability construction, runtime errors, and documented extension points.

### Internal Boundaries

- [x] Hide AST and parser implementation details under `internal/ast` and
  `internal/parser`.
- [x] Hide runtime execution, module loading, builtins, and capability adapters
  under `internal/runtime`.
- [x] Extract CLI analysis support into `internal/tools/analyze`.
- [x] Consolidate parser and runtime files around clearer ownership boundaries.

### Quality and Documentation

- [x] Add public API and value-package Godoc examples.
- [x] Document the CLI package and add a contribution guide.
- [x] Add a `golangci-lint` baseline and opt-in pre-commit hook.
- [x] Modernize runtime, parser, AST, capability, module, and CLI tests with
  table-driven cases, shared helpers, snapshots, and safe parallelization.
- [x] Stabilize benchmark smoke checks by sampling multiple runs and comparing
  the best result.

### v0.29.0 Definition of Done

- [x] Breaking embedder migration notes are documented in `CHANGELOG.md`.
- [x] Full tests pass after the refactor.
- [x] Release checklist passes for `v0.29.0`.

## v0.31.0 - Arithmetic, Parser, and Capability Hardening (completed 2026-05-30)

Goal: close correctness and safety gaps found in follow-up review — overflow in
the money domain type, unbounded parser recursion, and capability contracts that
did not follow builtins captured in closures.

### Hardening

- [x] Reject `int64` overflow in `Money` `Add`/`Sub`/`MulInt`/`DivInt` instead of
  silently wrapping; `MulInt` now returns `(Money, error)`.
- [x] Bound type-annotation recursion in the parser so deeply nested annotations
  fail with a parse error rather than overflowing the host stack.
- [x] Scan script-function and block closure environments when binding capability
  contracts, with a cycle guard and an ambient-global stop.

### v0.31.0 Definition of Done

- [x] Breaking embedder migration note (`Money.MulInt` signature) documented in
  `CHANGELOG.md`.
- [x] Full tests pass.
- [x] Release checklist passes for `v0.31.0`.

## v0.40.0 - Tasks Structured Concurrency (completed 2026-06-06)

Goal: let scripts express bounded concurrent host workflows while preserving
structured lifetimes, host-controlled fanout, task isolation, cancellation, and
runtime quotas.

### Runtime

- [x] Add `Tasks.run` scoped task groups with automatic waiting at block exit.
- [x] Add `tasks.spawn` task handles with `task.value` as the result boundary.
- [x] Add `tasks.wait` as an optional explicit barrier, not a required cleanup
  call.
- [x] Add `Tasks.map` for ordered concurrent mapping over arrays.
- [x] Clone task arguments, keyword arguments, return values, and inherited
  mutable globals across task boundaries.
- [x] Propagate task failures through `task.value` and task scope exit while
  preserving the original failure when enqueue is canceled.
- [x] Account retained task results against the parent memory quota while task
  handles keep completed values alive.

### Host Control and Tooling

- [x] Add `Config.DefaultTaskConcurrency` and `Config.MaxTaskConcurrency`.
- [x] Default task fanout to `4`, or to the lower host cap when
  `MaxTaskConcurrency` is below `4`.
- [x] Reject script `max:` values above the host cap instead of clamping them.
- [x] Add deterministic `testing/synctest` coverage for task scheduling,
  waiting, cancellation, and fanout behavior.
- [x] Add a Go 1.26 goroutine leak profile CI gate for runtime tests.

### Documentation and Examples

- [x] Record the Tasks design in an ADR.
- [x] Document Tasks in the README and host cookbook.
- [x] Add a runnable Tasks example.
- [x] Mark examples and scenario docs with `# vibe: 0.4`.

### v0.40.0 Definition of Done

- [x] Release notes are documented in `CHANGELOG.md`.
- [x] Full tests pass.
- [x] Release checklist passes for `v0.40.0`.

## v0.50.0 - Tooling, Diagnostics, and Runtime Hardening (completed 2026-06-11)

Goal: ship the post-Tasks usability, diagnostics, performance, and bug-fix work
as a coherent pre-1.0 release.

### CLI and LSP

- [x] Add inline evaluation and recursive watch mode to `vibes run`.
- [x] Add the native `vibes test` command.
- [x] Wire `vibes fmt` into LSP document formatting.
- [x] Add context-aware completion, signature help, go-to-definition, and
  document symbols.
- [x] Re-anchor completion, signature, and navigation state against the live
  buffer.

### Diagnostics and Runtime Correctness

- [x] Expose structured parse-error positions to hosts and the LSP.
- [x] Add did-you-mean suggestions for lookup failures.
- [x] Remap inline snippet diagnostics to user source positions.
- [x] Preserve newline statement boundaries, line-ending minus continuations, and
  trailing brace blocks.
- [x] Fix enum value kind names, hash method dispatch collisions, case range
  membership, and scalar-key array set operations.
- [x] Classify sandbox limit terminations distinctly.
- [x] Reject integer, duration, and time arithmetic overflow.

### Performance, Quality, and Documentation

- [x] Cut per-call environment and builtin churn.
- [x] Reduce memory-estimation cost with O(1) static environment accounting.
- [x] Cache compiled regex patterns and stateless builtin member dispatch.
- [x] Throttle step context polling without losing first-step cancellation.
- [x] Add coverage gating and expand capability-contract, public facade, and
  value package tests.
- [x] Expand stdlib, LSP, benchmark, and error-message documentation.
- [x] Centralize input-guard limits and pin module containment edge cases.

### v0.50.0 Definition of Done

- [x] Release notes are documented in `CHANGELOG.md`.
- [x] Full tests pass.
- [x] Release checklist passes for `v0.50.0`.

## v1.0.0-rc1 - Ruby Alignment and 1.0 Hardening (completed 2026-07-06)

Goal: ship the 1.0 release candidate — Ruby-aligned language, class system,
and collection semantics, with the checker, sandbox accounting, tooling, and
embedding API hardened and frozen for GA.

### Language and Semantics

- [x] Modules, mixins, and visibility directives.
- [x] Procs, lambdas, block forwarding, and call splats.
- [x] Beginless/endless ranges and parenless command-argument regex literals.
- [x] Ruby reference semantics for array and hash mutators.
- [x] Hash insertion order and multi-clause rescue.

### Hardening and Performance

- [x] Undefined-name and typed-block-parameter check warnings.
- [x] Linear checker exit analysis with a nesting-depth backstop.
- [x] Estimator base-walk memoization and metered blockless builtins.
- [x] Call-boundary copy reductions and lazy composite globals.
- [x] AST walker completeness gates and a full-budget nightly fuzz job.

### Release Readiness

- [x] Compiled changelog and the migrating-to-1.0 guide.
- [x] Embedding API tiers documented in docs/embedding-api-stability.md.
- [x] tree-sitter grammar and Zed extension synced to 1.0 syntax.

### v1.0.0-rc1 Definition of Done

- [x] Release notes are documented in `CHANGELOG.md`.
- [x] Full tests pass.
- [x] Release checklist passes for `v1.0.0-rc1`.

## v1.0.0-rc2 - Bignums and Candidate Fixes (completed 2026-07-07)

Goal: second release candidate — arbitrary-precision integers with sandbox
accounting, plus the parenless bracket-argument and endless-range first(n)
gaps found smoke-testing the rc1 artifact.

- [x] Arbitrary-precision integers with transparent promotion and quota
  charging (estimator payloads, pow/multiply preflights, step scaling,
  render and literal guards).
- [x] Spaced brackets after command callees parse as array arguments.
- [x] Bounded first(n) on endless ranges.

### v1.0.0-rc2 Definition of Done

- [x] Release notes are documented in `CHANGELOG.md`.
- [x] Full tests pass.
- [x] Release checklist passes for `v1.0.0-rc2`.

## v1.0.0-rc3 - Sandbox Performance and Quota Profiles (completed 2026-07-09)

Goal: third release candidate — a performance and ergonomics pass on the
sandbox: named quota profiles, a much faster memory-quota estimator, and the
zero-value embedding default raised to the `low` profile.

- [x] Named quota profiles (low/medium/high/xhigh) with an `xhigh` CLI default.
- [x] Incremental memory-quota accounting: dormant-frame skipping, block-scope
  prefix memoization, inlined per-value guard, and per-check reallocation
  removal, cross-checked by a differential oracle in CI.
- [x] Zero-value embedding default resolves to the `low` profile.
- [x] ADRs recorded for the quota profiles and arbitrary-precision integers.

### v1.0.0-rc3 Definition of Done

- [x] Release notes are documented in `CHANGELOG.md`.
- [x] Full tests pass.
- [x] Release checklist passes for `v1.0.0-rc3`.

## v1.0.0-rc4 - Static Checking for Typed Boundaries (completed 2026-07-10)

Goal: fourth release candidate — static checking for typed boundaries
(ADR-004): local type inference on the check path, a whole-script
`vibes check` command, and `JSON.parse_as` with first-class shape literals.

- [x] Static local type inference: locals take assigned expression types, and
  known contradictions at typed boundaries error while unknowns stay permitted.
- [x] `vibes check <script>` reports every statically checkable contract issue
  across functions, class methods, and top-level code, exiting non-zero for CI.
- [x] `JSON.parse_as(raw, shape)` with shape literals in expression position,
  shadow resolution matching the runtime, and static schema validation.
- [x] Whole-script checks follow the entrypoint's execution order: requires
  seed exports for later checks, and top-level callees check under call-time
  roots.
- [x] ADR-004 recorded for typed-boundary static checking.

### v1.0.0-rc4 Definition of Done

- [x] Release notes are documented in `CHANGELOG.md`.
- [x] Full tests pass.
- [x] Release checklist passes for `v1.0.0-rc4`.

## v1.0.0-rc5 - Development-Time Module Reloading (completed 2026-07-12)

Goal: fifth release candidate — make embedded development loops faster with
opt-in, production-safe live reloading for required Vibescript modules.

- [x] `Config.DevMode` revalidates cached required modules against their
  mtime+size stamp and recompiles them when the source changes.
- [x] Dev mode bypasses derived require caches, allowing newly created modules
  to resolve without a restart.
- [x] Each `Call` sees one consistent module version, including if a file
  changes or becomes invalid while that call is running.
- [x] ADR-005 and the host cookbook document the development-only behavior and
  the host-owned top-level-script reload pattern.

### v1.0.0-rc5 Definition of Done

- [x] Release notes are documented in `CHANGELOG.md`.
- [x] Full tests pass.
- [x] Release checklist passes for `v1.0.0-rc5`.

## v1.0.0-rc6 - Language Server Documentation (completed 2026-07-15)

Goal: sixth release candidate — remove the accidentally reinstated hash-rocket
syntax and make the language server a real documentation surface.

- [x] Hash rockets in hash literals and shape annotations are parse errors
  again with a single targeted diagnostic; the arbitrary-key runtime model,
  colon shape fields, and rescue bindings are unchanged.
- [x] Hover serves documentation parsed from the embedded references: kernel
  builtins, namespaces and their members through both accessors, keywords and
  contextual words, and the stdlib member surface including merged
  multi-receiver entries and composed bang variants.
- [x] User-defined symbols hover with reconstructed typed signatures and
  leading doc comments, resolved by scope: declaration line, enclosing
  container, qualifier owner and receiver kind, with setters preferred at
  write sites.
- [x] Completions carry the same documentation, and drift gates enforce that
  every engine-registered builtin, parser keyword, and universal member claim
  stays true against the runtime.
- [x] The CLI is rebuilt on urfave/cli with a stable command contract.

### v1.0.0-rc6 Definition of Done

- [x] Release notes are documented in `CHANGELOG.md`.
- [x] Full tests pass.
- [x] Release checklist passes for `v1.0.0-rc6`.

## v1.0.0-rc7 - CLI Reliability and Builtin Discovery (completed 2026-07-16)

Goal: seventh release candidate — finish the urfave CLI migration and remove
manually synchronized builtin metadata from the REPL, checker, and language
server.

- [x] CLI commands bind positional arguments and flags through urfave's typed
  destinations while preserving the existing command contract.
- [x] Command output uses configured streams, propagates write failures, and
  long-running work inherits Ctrl-C cancellation.
- [x] REPL and LSP builtin discovery derives from the runtime registry, parser
  keywords, and embedded documentation instead of parallel manual lists.
- [x] Qualified builtin completion and signature help cover namespace members
  and constants, with bidirectional documentation drift checks.
- [x] Release rehearsal accepts release-candidate versions.

### v1.0.0-rc7 Definition of Done

- [x] Release notes are documented in `CHANGELOG.md`.
- [x] Full tests pass.
- [x] Release checklist passes for `v1.0.0-rc7`.

## v1.0.0-rc8 - Typed Member Contracts and Static Checking (completed 2026-07-26)

Goal: eighth release candidate — turn the gradual checker into a typed member
surface that carries nominal facts through control flow and validates writes.

- [x] Resolve builtin and scalar member contracts from a single runtime-owned
  registry shared by the checker and editor completion.
- [x] Declare typed builtin signatures so provably wrong arguments and misused
  results are reported statically.
- [x] Carry nominal class facts from constructors through locals, branches,
  arguments, and returns.
- [x] Narrow locals across control flow with nullable tests, class predicates,
  and resolved named type comparisons.
- [x] Infer return summaries for unannotated functions and statically resolved
  methods, keeping dynamic receivers and overrides opaque.
- [x] Report incompatible writes to typed arrays, hashes, shapes, and
  accessor-backed instance properties.
- [x] Preserve container facts across member calls the registry proves pure.
- [x] Add optional (`age?`) and open (`...`) shape fields, and the `is_type?`
  predicate for primitives, containers, classes, and enums.
- [x] Validate non-object JSON roots in `JSON.parse_as`.
- [x] Expose the checker to embedders through `Script.CheckedCall`,
  `vibes.CheckWarning`, and published host-callable signatures.
- [x] Seal capability return validation against host adapter bypass.
- [x] Remove quadratic re-measurement from scalar accumulation and hash
  iteration under a memory quota.

### v1.0.0-rc8 Definition of Done

- [x] Release notes are documented in `CHANGELOG.md`.
- [x] Full tests pass.
- [x] Release checklist passes for `v1.0.0-rc8`.

## v1.0.0-rc9 - Linear Hash Iteration Under a Quota (completed 2026-07-27)

Goal: ninth release candidate — finish the memory-quota work rc8 started, so
every block driver iterates linearly whether its receiver came from a script or
from the host.

- [x] Build `Hash#transform_keys` results after the block loop on both the typed
  and legacy branches, removing the last driver that re-measured its receiver on
  every insertion.
- [x] Preserve insertion order, last-writer-wins collisions, unsupported-key
  failure, and array-key identity across the deferred build.
- [x] Charge the deferred key buffer against the quota only when it is
  allocated, and fold it into the build accumulator's baseline.
- [x] Benchmark the block-driver shapes whose absence hid these regressions:
  pure versus accumulating block bodies, host-built versus script-built hash
  receivers, and walking versus result-building drivers.

### v1.0.0-rc9 Definition of Done

- [x] Release notes are documented in `CHANGELOG.md`.
- [x] Full tests pass.
- [x] Release checklist passes for `v1.0.0-rc9`.

## v0.60.0 - Predictable Sandboxing and Lightweight Semantics (completed 2026-08-22)

Goal: ship a smaller language surface whose execution and memory use are easier
for hosts to reason about, while closing the correctness and denial-of-service
gaps found during 1.0 release-candidate hardening.

### Language and Host Semantics

- [x] Remove script-visible Tasks and sleep in favor of synchronous,
  host-controlled capabilities.
- [x] Enforce nonescaping blocks and remove module mixins and instance-style behavior.
- [x] Give hashes one string keyspace and arrays and hashes value semantics.
- [x] Keep values independent across the host boundary.
- [x] Record the lightweight language boundary in ADR-006 and align the public
  documentation with it.

### Sandbox and Tooling Hardening

- [x] Bound pathological parser, checker, formatting, comparison, and
  collection workloads by explicit limits or quota charges.
- [x] Make genuine quota exhaustion uncatchable and close retained-memory,
  scratch-allocation, and backing-storage accounting gaps.
- [x] Improve static diagnostics, runtime error locations, LSP completion, and
  common collection and conversion operations.
- [x] Publish nonmutating and nonretaining contracts for host builtins.

### v0.60.0 Definition of Done

- [x] Release notes are documented in `CHANGELOG.md`.
- [x] Full tests pass.
- [x] Release checklist passes for `v0.60.0`.

## v0.70.0 - Faster Execution and Lower Memory Use (completed 2026-09-10)

Goal: ship the performance and memory audit improvements with measured CPU and
allocation results while preserving sandbox accounting and portable builds.

### Execution, Strings, and Memory

- [x] Accelerate string scans, case conversion and comparison, JSON, whitespace,
  regular-expression quoting, formatting, and string literal compilation.
- [x] Support opt-in Go 1.27.1 SIMD on native ARM64 and x86-64 with guarded CPU
  dispatch and portable fallbacks; keep ordinary releases on Go 1.26.
- [x] Reduce redundant Unicode rune scans and document executable-layout effects
  with native embedding measurements.
- [x] Stream block scans, reuse stable quota estimates, avoid unused declaration
  clones, and make independent declaration checking linear.
- [x] Reduce short-call and serialization allocations and release discarded
  source buffers, deleted keys, and completed runtime scopes.

### Accounting and Delivery

- [x] Bound parser and rendering depth, collection projections, regex and JSON
  materialization, capability cloning, and module request caches.
- [x] Meter assignment and retained-callback accounting walks and enforce module
  policy before filesystem access.
- [x] Document benchmark gains and tradeoffs in `CHANGELOG.md` and the audit reports.
- [x] Pass the full test suite and release checklist for `v0.70.0`.

## Unreleased
