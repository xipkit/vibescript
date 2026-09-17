# Gradual checker implementation

The checker is unfinished. Its type-fact store, boundary relations, control-flow walker and function-call analysis currently compile only in unit-test builds. There is no public checking API or checked-execution gate yet. Ordinary scripts retain their existing runtime type contracts.

## Type facts

Each analysis owns a metered arena of immutable facts addressed by small IDs. Equivalent facts share storage. Child IDs point to existing facts, so comparison and destruction do not require recursive Rust stacks. Scratch buffers, intern tables, relation caches, field names and enum metadata use the existing call accounting. Failures release temporary storage and preserve latched cancellation or quota errors.

Unknown values and explicit `any` remain separate from known alternatives. Joining an unknown branch with a string branch preserves the string contradiction at an `int` boundary. Facts distinguish empty literal collections from unconstrained collections, retain exact enum symbols, and identify nominal types by their source and declaration rather than by spelling alone.

Boundary relations distinguish acceptance, gradual uncertainty and known rejection. They inspect every known alternative, including container contents and shape fields. Hash key contracts preserve the shared string/symbol keyspace and the distinction between a known literal key representation and an annotation's unspecified representation. Optional fields can be absent; finite shape and literal-array alternatives are split when several declared alternatives collectively cover them. Expansion checks for a counterexample before exploring further combinations and remains subject to normal quotas.

The current reference corpus contains 1,156 boundary pairs generated from 34 type spellings. Go v0.70.0 checks each pair through an annotated producer and return boundary; Rust checks the corresponding facts. The Go test regenerates the decisions from the pinned interpreter. This validates boundary decisions, not whole-script checking, diagnostic wording, control flow or host binding.

## Control flow

The internal walker now builds basic blocks directly from the existing bytecode. It tracks local bindings, scalar expressions, parameter-default branches, nil/truth guards, short-circuit expressions, ordinary branches, nested `while` loops, `break`, `next` and explicit/implicit function returns. Inputs converge before the walker collects diagnostics, which retain their bytecode source positions. It does not retain a second syntax tree or execute script effects.

Local-state snapshots share metered radix-tree nodes. Assignments copy only shared paths; joins skip identical subtrees. Missing local bindings remain distinct from bound `nil` until the compiler's declaration instruction fills the absent path. Operand origins support direct local guards, and writes invalidate older stack predicates, including writes inside loop expressions. These origins do not yet describe stored boolean predicates or container correlations.

The flow corpus contains 60 scripts checked by both implementations. Nine decisions intentionally differ from Go v0.70.0: Rust preserves known default and loop-assignment facts and follows reachable loop exits. Each difference includes a Rust execution witness, including default-quota exhaustion for an unconditional loop whose trailing return is unreachable. These are checker-decision fixtures, separate from the runtime compatibility audit. A further 972 scalar operand/operator combinations compare inferred outcomes with the Rust runtime.

The walker reports incomplete analysis at reachable operations it cannot model. General members, iterable loops, collection mutation/indexing, exception handlers, blocks, required files and namespace scopes remain unfinished. A partial return summary or an empty diagnostic list is not sufficient to approve a script. The implementation remains private until all required paths have analysis and public gates can enforce that distinction.

## Function calls

Plain script calls propagate supplied argument facts and inferred return facts through an iterative work queue. Each function/input combination has a shared summary; changed returns requeue dependent callers, including recursive callers. Script call chains do not grow the Rust analysis stack. Diagnostics come from the final reachable dependencies, so provisional calls and unreachable functions do not contribute warnings. An unfinished callee remains visible even when its current summary has no returning path.

Argument binding follows runtime positional, keyword, options-hash, rest and keyword-rest rules, including duplicate-key order. Supplied arguments skip defaults; missing optional arguments follow the default bytecode. Known boolean and scalar facts survive compatible typed boundaries, while enum conversions produce the declared nominal contract. Literal arrays and hashes retain their element or field facts, supporting exact positional and keyword splats. General array lengths and optional/unknown keyword sets still require analysis.

Call targets are selected before arguments. Nested calls and loop exits preserve the appropriate pending arguments, and missing names stop analysis before argument effects. Explicit root overrides and parameter/local shadowing participate in target selection. Bare reads of overridden function names remain incomplete until root-value analysis can distinguish data reads from forbidden method extraction. Mutable root bindings and source-dependent named types also remain incomplete.

Registered host calls use already compiled declarative signatures to check arity, keyword rejection, arguments and results. Analysis does not invoke callbacks, custom validators or capability factories. Unsigned callbacks retain gradual unknown results. Namespaced capabilities, source-dependent host types and attached blocks still need integration.

The call corpus contains 52 scripts compared with Go v0.70.0 reachable-function checking. Eight decisions intentionally differ: Rust retains known facts through `any`, rejects known noncallables and invalid splats, skips unused defaults, and follows reachable loop/recursive returns. Each difference has a Rust execution witness. Separate tests cover binding against the runtime, a 1,000-function call chain on the default stack, mutual recursion, host-effect isolation, incomplete paths, exact quotas, failed-allocation cleanup and cancellation.

## Remaining integration

Completion still requires:

- Iterable loops, exception/ensure/retry edges, nonlocal block returns, general type narrowing, scalar constant propagation and stored predicate relations. Container loops need deliberate convergence rules, and value-origin facts must preserve correlations without confusing equivalent types with identical values.
- Class, module, builtin and namespaced host calls, variable-size splats, block binding and attached-method restrictions. Plain script calls and registered positional host signatures are implemented internally; public entry binding and whole-program integration remain required.
- Mutable-container facts, property constraints, selected value semantics and invalidation after unmodeled effects.
- Required-file source discovery and export analysis without executing initializers, plus the selected source and root binding rules.
- Host-value facts, per-call capability descriptors, strict-effects validation and checker accounting across every entry point. Signature metadata must be inspected without invoking callbacks or validators.
- Sorted, deduplicated source diagnostics, whole-file checking, reachable-function checking, exact-call checking, checked invocation and CLI gates. A rejected check must not execute script effects.
- Broader reference fixtures, required-file and capability tests, cancellation/quotas at the public boundary, and the remaining temporary compiler allocation accounting.

The full-language goal remains active. This internal foundation does not establish a deployment gate or complete ADR-004.
