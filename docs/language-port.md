# Complete the Vibescript language in Rust

The active objective is to implement the remainder of Vibescript, using Go v0.70.0 at commit `5cba216c33bea8890787d64efb2ab926a761fb1b` as the compatibility reference. This replaces the earlier goal of building a performance experiment around a subset. Performance passes resume after language coverage; existing accounting and cancellation contracts remain required throughout.

The full release source is available locally at `.cache/reference-go-0.70.0` on the external volume. The normative references are its `docs/language_reference.md`, `docs/stdlib_core_utilities.md`, focused language guides, implementation, and tests. The site corpus is useful coverage but is not the complete specification.

## Completion requirements

| Area | Required behavior | Current status |
| --- | --- | --- |
| Values and operators | Arbitrary-precision integers with compact normalization, floats, byte strings, symbols, arrays, hashes, ranges, enum values, class instances, money, duration, and time; complete operators and coercions | Core scalar/collection subset implemented; remainder pending |
| Literals and expressions | Complete numeric/string escapes, interpolation, percent word/symbol arrays, range and ternary expressions, expression-valued conditionals, bracket slices, chained and bare calls | Partial |
| Assignment and control flow | Destructuring/rest assignment, nested addressable writes and mutating receivers, all compound assignments, modifiers, for/while/until, case/when, break/next/return | Loops, modifiers, case, nested/rest destructuring, logical assignment, and nested index/member writes implemented; full operator coverage pending |
| Functions and blocks | Positional, default, keyword, rest and keyword-rest binding; argument splats; synchronous call-attached blocks, yield, block_given?, nonlocal returns, block retirement | Positional/default/keyword/rest/keyword-rest binding and argument splats implemented; blocks pending |
| Classes | Instance/class methods and variables, constructors, operator/index methods, accessors, visibility, constants, introspection, per-call isolation | Pending |
| Modules and enums | Namespace declarations, nested constants, module functions, require/load paths and module initialization/cycles/isolation, enum coercion and serialization | Pending |
| Typed boundaries | Runtime argument/return/accessor normalization and checking; nullable, union, collection, shape, enum and nominal types; optional static checking | Pending |
| Error handling | Raise/rescue/else/ensure, error values and backtraces; unwinding on return/break/next; exhaustion and cancellation remain uncatchable | Host/runtime error categories implemented; language handling pending |
| Standard library | Every documented builtin/member signature and behavior in stdlib_core_utilities.md and focused guides, including block forms, JSON, regex, formatting, numeric/math, money, duration, time and IDs | Partial |
| Host capabilities | Namespaced methods, positional/keyword contracts, synchronous block invocation with retirement, context/deadline propagation, independently accounted results; usable Rust async integration | Flat synchronous positional/keyword callbacks and bounded Tokio runner implemented; namespaced contracts, blocks and native async callbacks pending |
| Limits and value semantics | Account new value storage and temporary allocations; bound work and cancellation latency; enforce recursion and latched exhaustion; preserve aliases and cross-call isolation | Existing core verified; every new feature requires coverage |
| Compatibility evidence | Differential success/error cases covering the full reference, all applicable original examples, input/output and mutation checks, debug/release and portable/SIMD gates | 690 shared success cases, 218 runtime rejection cases, and 134 passing site programs; full coverage pending |

ADR-006 remains part of the target: no inheritance, module mixins, escaping executable values, script-created tasks or sleep. Removed forms must produce the documented errors. These exclusions follow Vibescript's language boundary, not Rust implementation convenience.

## Implementation sequence

1. Complete basic collection, string, numeric and expression behavior; extend the shared harness as each feature lands.
2. Add general addressable writes, richer call binding, control flow and synchronous blocks with explicit VM state.
3. Implement arbitrary precision and the remaining standard-library value families and operations.
4. Add declarations, namespaces, classes, enums, module loading and typed boundaries/checking.
5. Complete error unwinding, capability binding and async integration, then audit every reference surface and gap.

This sequence can change when dependencies require it. Finishing a stage or making all 203 site examples pass does not complete the objective by itself.

## Verification

Use `./scripts/check` for formatting, linting, debug/release, portable/SIMD, documentation and optional Tokio tests. Extend `scripts/fixtures.py` with Go-checked conformance cases and run `scripts/compare.py --validate-only`; do not run performance measurements during this language port. Keep `scripts/audit-site.py` reporting all site successes, missing features and harness gaps. Add independent expected-value and limit tests for new semantics, not only comparisons to the Go implementation.

Before completion, replace every partial/pending status above with specific current evidence, audit all documented signatures and rejection boundaries, and verify the full implemented state. Leave the thread goal active while any required behavior is missing, incomplete or unverified.

## Verified progress

Range values, ternary expressions, array/string slicing, raw byte access, and first/last windows are implemented. Character selections normalize invalid UTF-8 as Go does; byteslice preserves raw bytes. Shared language fixtures cover normal values, empty results, negative/fractional indexes, open and descending ranges, extreme 64-bit endpoints, aliases, and substring selectors. Independent tests cover expansion quotas, cancellation, and per-call range storage accounting.

The first full corpus refresh passes 92 unchanged site programs, up from 74; 96 still need language features and 15 still need harness capabilities or encoding support. These are partial milestones. All pending completion requirements above remain in scope.

The next batch adds collection transforms and projections, fetch/dig and hash queries, scalar string concatenation, nested joining and display, character/byte extraction, affix predicates, and basic numeric helpers. It passes 120 unchanged site programs; 68 remain language gaps and 15 remain harness gaps. `tests/language-errors.json` now also records Go-verified rejection boundaries, exercised in native Rust tests and the shared comparison harness. New allocation/step tests cover expanding windows and zip results, temporary reverse/character buffers, exponential flatten/display graphs, and equality scans. Formatting, Clippy, debug/release, portable/SIMD, optional Tokio, documentation, and shared Go checks pass.

Control flow now includes array/hash/finite-range `for`, expression-valued conditionals and loops, `case`/`when` with ranges and splats, statement modifiers, payloads on `break`/`next`, logical assignment, and nested/rest destructuring. Another 59 shared success cases and nine runtime rejections verify ordering, short-circuiting, binding, and function control boundaries. Independent tests cover cancellation, rest allocation, cleanup on control transfers, parser nesting guards, and integer endpoint termination. The full site audit now matches 125 programs, leaving 63 language gaps and 15 harness gaps. The portable and SIMD Rust builds charge identical counters for all shared cases.

The control-flow audit also found [reference differences](compatibility.md). The Go release has integer-loop wraparound and hash-loop break-result bugs; mutation during collection iteration can depend on aliases and backing allocation. Rust currently retains deterministic collection snapshots. These differences are explicit compatibility evidence, and mutation-during-iteration policy remains an open item for full-port completion.

Nested indexed assignment, compound/logical indexed updates, and `push`/`<<` receivers now use accounted pending-write state. Selectors evaluate once; ordinary writes evaluate the RHS first; nested argument mutations update pending receivers while explicit rebinding leaves the replacement intact. Temporary function and slice results do not write back through the original collection. Another 25 shared success cases and six runtime rejections pass; independent tests cover path allocations, step limits, host-input isolation, and pending-state cleanup on control transfers. The topological-sort site example now passes, bringing the total to 126.

Three further [evaluated-collection differences](compatibility.md#evaluated-collection-values) remain open. `scripts/audit-compatibility.py` runs all seven recorded differences against both Go and both Rust builds and exits unsuccessfully while any remain. Full-port completion must resolve these records or explicitly establish the intended deviations; passing the ordinary shared suite does not settle them.

Hash field reads, writes, and addressable member paths now distinguish stored fields from builtin methods and explicit calls. Collection mutation includes array prepend/pop/shift/delete/insert/clear/fill, hash store/delete/replace/clear, and aliases; string prepend/insert/replace/clear preserve the original binding. Another 61 shared success cases and 34 runtime rejections cover receiver/result separation, nesting, aliases, reserved names, and argument boundaries. An additional 826 window combinations match the Go release. Independent tests verify deletion through index collisions and threshold changes, storage reclamation, nesting-depth reclamation, bounded expansion and equality scans, and pending-member cleanup on control transfers. The full site audit now passes 129 programs, leaving 59 language gaps and 15 harness gaps; the seven recorded compatibility differences remain open. Performance measurements remain deferred while language work continues.

Function calls now support positional defaults, required/defaulted keywords, rest and keyword-rest parameters, positional and hash splats, keyword shorthand, and Go's source-call options-hash rule. Complete argument shapes are validated before defaults execute. Defaults run in the VM with sequential scope, cancellation, memory and recursion limits; pending argument and binding storage is reclaimed on return and loop control transfers. Rust hosts and the Tokio runner can pass keyword arguments, and registered callbacks can opt into receiving them.

The binding audit also corrected local-name declaration timing, function-value rejection, explicit call lookup during assignment, and fallback through mutating/indexed receivers. A skipped default does not create its locals; statement branches and loop control transfers introduce names at the same boundaries as Go. Targeted probes match 1,176 argument-binding combinations and 128 scope cases. Shared validation now checks 690 successful calls and 218 runtime rejections in both Go and both Rust builds, with identical portable/SIMD Rust accounting. Formatting, Clippy, debug/release, optional Tokio, documentation, and Go checks pass. Five more unchanged site programs pass, bringing the total to 134; 54 language gaps, 15 harness gaps, and the seven recorded compatibility differences remain open. General calls without parentheses, typed signatures, synchronous blocks, and the other pending requirements remain in scope.
