# Gradual checker implementation

The checker is unfinished. Its type-fact store and boundary relations currently compile only in unit-test builds. There is no public checking API or checked-execution gate yet. Ordinary scripts retain their existing runtime type contracts.

## Type facts

Each analysis owns a metered arena of immutable facts addressed by small IDs. Equivalent facts share storage. Child IDs point to existing facts, so comparison and destruction do not require recursive Rust stacks. Scratch buffers, intern tables, relation caches, field names and enum metadata use the existing call accounting. Failures release temporary storage and preserve latched cancellation or quota errors.

Unknown values and explicit `any` remain separate from known alternatives. Joining an unknown branch with a string branch preserves the string contradiction at an `int` boundary. Facts distinguish empty literal collections from unconstrained collections, retain exact enum symbols, and identify nominal types by their source and declaration rather than by spelling alone.

Boundary relations distinguish acceptance, gradual uncertainty and known rejection. They inspect every known alternative, including container contents and shape fields. Hash key contracts preserve the shared string/symbol keyspace and the distinction between a known literal key representation and an annotation's unspecified representation. Optional fields can be absent; finite shape and literal-array alternatives are split when several declared alternatives collectively cover them. Expansion checks for a counterexample before exploring further combinations and remains subject to normal quotas.

The current reference corpus contains 1,156 boundary pairs generated from 34 type spellings. Go v0.70.0 checks each pair through an annotated producer and return boundary; Rust checks the corresponding facts. The Go test regenerates the decisions from the pinned interpreter. This validates boundary decisions, not whole-script checking, diagnostic wording, control flow or host binding.

## Remaining integration

The existing bytecode retains parameter contracts, local names, call targets, captures and source offsets. The next implementation step is an abstract control-flow walk over that code, without retaining a second syntax tree during ordinary compilation.

Completion still requires:

- Local binding facts, branch joins, loop convergence, nil/type narrowing, reachability and explicit/implicit return summaries. Value-origin facts must preserve correlations without confusing equivalent types with identical values.
- Script, class, module, builtin and host calls, argument evaluation order, positional/keyword/rest binding, defaults, blocks and attached-method restrictions.
- Mutable-container facts, property constraints, selected value semantics and invalidation after unmodeled effects.
- Required-file source discovery and export analysis without executing initializers, plus the selected source and root binding rules.
- Host-value facts, per-call capability descriptors, strict-effects validation and checker accounting across every entry point. Signature metadata must be inspected without invoking callbacks or validators.
- Sorted, deduplicated source diagnostics, whole-file checking, reachable-function checking, exact-call checking, checked invocation and CLI gates. A rejected check must not execute script effects.
- Broader reference fixtures, required-file and capability tests, cancellation/quotas at the public boundary, and the remaining temporary compiler allocation accounting.

The full-language goal remains active. This internal foundation does not establish a deployment gate or complete ADR-004.
