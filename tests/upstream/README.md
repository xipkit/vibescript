# Upstream Vibescript programs

These 48 files come from Go Vibescript v0.70.0 under its MIT license: nine `examples/` programs and all 39 programs under Go's `tests/` tree, rewritten into the statically typed language of ADR-007 and ADR-008 with the same results. `sources.json` pins the repository, commit and paths, and the SHA-256 hashes of the rewritten files. `tests/errors/{arguments,attributes,classes,runtime,types}.vibe` and `tests/blocks/{block_arity,block_error_propagation,error_cases}.vibe` keep the calls that exist to fail, so they do not compile with static types. Passing any of the checks below does not establish full language compatibility.

## Shared comparison corpus

`cases.json` selects 46 named-function invocations across the nine `examples/` files and `tests/complex/operators.vibe`, with independently computed expected results in plain JSON. `tests/upstream.rs` runs them natively, and `scripts/fixtures.py` feeds the same cases to the Go/Rust comparison harness. This corpus is unchanged by the driver harness below and remains the only part of this directory that the Go binary also executes.

## Native driver harness

The native harness mirrors the 105 invocations of these programs in Go's `internal/runtime/integration_test.go`. `driver.json` records each invocation with a stable identifier and the driver lines it comes from:

| Go test | Invocations | Native assertion |
| --- | --- | --- |
| `TestComplexExamplesCompile` | 9 | compile succeeds |
| `TestComplexExamplesRun` | 12 | exact value, or the driver's predicates for `durations` and `chudnovsky` |
| `TestProgramFixtures` | 15 | exact value; `classes/counter` is asserted on two calls of one script |
| `TestEnumFixtureTypedCalls` | 4 | enum member triples, a host symbol argument, and a live member passed back into `block_names` |
| `TestBlockErrorCases`, `TestBlockErrorPropagation`, `Test{Runtime,Type,Attribute,Yield,Argument}ErrorCases` | 20 rejections, 4 successes | compile succeeds, then the call fails with a runtime `ErrorKind` derived from the Rust source, or succeeds with the driver's value |
| `TestComplexExamplesStress` | 2 | `massive` at 5,000,000 steps; `pi_approx_precise(5000)` fifty times on one script within `1e-6` of pi |
| `TestAllVibeFilesCompileAndRun` | 39 | every program compiles; the 35 that declare `run` execute it at 5,000,000 steps; the four without `run` are compile-only |

`tests/upstream_driver.rs` implements these natively. Expected values are the driver's literals in a small JSON notation with `$symbol`, `$money`, `$time` and `$enum` tags, compared against typed `Value` accessors so that integers, floats, symbols, strings, money, times and enum members are distinguished. Hash comparison ignores insertion order but enforces the key set, as Go's `assertValueEqual` does. Rejections assert the Rust error kind, not Go's message text. Default-limit cases use `CallOptions::default()`; only the two Go tests that set `StepQuota: 5_000_000` use a raised step limit.

The harness records the identifier of every assertion it makes; each group test checks that it covered exactly its declared identifiers, and `driver_manifest_is_fully_covered` checks that the groups together cover all 105 identifiers and that every pinned `tests/` program has a walk entry. It is native-only: it does not extend the typed-v1 encoders or the Go comparison binary, and the driver's `run` values for `hash_edges`, `string_edges`, `array_edges` and `errors/classes` remain unpinned because the Go driver only requires that they run.
