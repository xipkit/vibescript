# Gradual checker implementation

The checker is unfinished. Its type-fact store, boundary relations, control-flow walker, function-call analysis, collection inference and exception flow currently compile only in unit-test builds. There is no public checking API or checked-execution gate yet. Ordinary scripts retain their existing runtime type contracts.

## Type facts

Each analysis owns a metered arena of immutable facts addressed by small IDs. Equivalent facts share storage. Child IDs point to existing facts, so comparison and destruction do not require recursive Rust stacks. Scratch buffers, intern tables, relation caches, field names and enum metadata use the existing call accounting. Failures release temporary storage and preserve latched cancellation or quota errors.

Unknown values and explicit `any` remain separate from known alternatives. Joining an unknown branch with a string branch preserves the string contradiction at an `int` boundary. Facts distinguish empty literal collections from unconstrained collections, retain compact integers, float bit patterns, raw strings, enum symbols, range bounds and compiled regexes, and identify nominal types by their source and declaration rather than by spelling alone. General scalar facts absorb their literal alternatives during joins; compatible typed boundaries preserve known literal values. Float fact identity does not establish value equality: mixed numeric comparisons, signed zeros and NaN need their runtime matching rules, including inside collections.

Boundary relations distinguish acceptance, gradual uncertainty and known rejection. They inspect every known alternative, including container contents and shape fields. Hash key contracts preserve the shared string/symbol keyspace and the distinction between a known literal key representation and an annotation's unspecified representation. Optional fields can be absent; finite shape and literal-array alternatives are split when several declared alternatives collectively cover them. Expansion checks for a counterexample before exploring further combinations and remains subject to normal quotas.

The current reference corpus contains 1,156 boundary pairs generated from 34 type spellings. Go v0.70.0 checks each pair through an annotated producer and return boundary; Rust checks the corresponding facts. The Go test regenerates the decisions from the pinned interpreter. This validates boundary decisions, not whole-script checking, diagnostic wording, control flow or host binding.

## Control flow

The internal walker now builds basic blocks directly from the existing bytecode. It tracks local bindings, scalar expressions, parameter-default branches, nil/truth guards, short-circuit expressions, ordinary branches, nested `while` and `for` loops, `break`, `next` and explicit/implicit function returns. Inputs converge before the walker collects diagnostics, which retain their bytecode source positions. It does not retain a second syntax tree or execute script effects.

Local-state snapshots share metered radix-tree nodes. Assignments copy only shared paths; joins skip identical subtrees. Missing local bindings remain distinct from bound `nil` until the compiler's declaration instruction fills the absent path. Operand origins support direct local guards, and writes invalidate older stack predicates, including writes inside loop expressions. These origins do not yet describe stored boolean predicates or container correlations.

The flow corpus contains 60 scripts checked by both implementations. Nine decisions intentionally differ from Go v0.70.0: Rust preserves known default and loop-assignment facts and follows reachable loop exits. Each difference includes a Rust execution witness, including default-quota exhaustion for an unconditional loop whose trailing return is unreachable. These are checker-decision fixtures, separate from the runtime compatibility audit. A further 972 scalar operand/operator combinations compare inferred outcomes with the Rust runtime.

The walker reports incomplete analysis at reachable operations it cannot model. General members, opaque iterable dispatch, blocks, required files and namespace scopes remain unfinished. A partial return summary or an empty diagnostic list is not sufficient to approve a script. The implementation remains private until all required paths have analysis and public gates can enforce that distinction.

## Function calls

Plain script calls propagate supplied argument facts and inferred return facts through an iterative work queue. Each function/input combination has a shared summary; changed returns requeue dependent callers, including recursive callers. Script call chains do not grow the Rust analysis stack. Diagnostics come from the final reachable dependencies, so provisional calls and unreachable functions do not contribute warnings. An unfinished callee remains visible even when its current summary has no returning path.

Argument binding follows runtime positional, keyword, options-hash, rest and keyword-rest rules, including duplicate-key order. Supplied arguments skip defaults; missing optional arguments follow the default bytecode. Known boolean and scalar facts survive compatible typed boundaries, while enum conversions produce the declared nominal contract. Literal arrays and hashes retain their element or field facts, supporting exact positional and keyword splats. General array lengths and optional/unknown keyword sets still require analysis.

Call targets are selected before arguments. Nested calls and loop exits preserve the appropriate pending arguments, and missing names stop analysis before argument effects. Explicit root overrides and parameter/local shadowing participate in target selection. Bare reads of overridden function names remain incomplete until root-value analysis can distinguish data reads from forbidden method extraction. Mutable root bindings and source-dependent named types also remain incomplete.

Registered host calls use already compiled declarative signatures to check arity, keyword rejection, arguments and results. Analysis does not invoke callbacks, custom validators or capability factories. Unsigned callbacks retain gradual unknown results. Namespaced capabilities, source-dependent host types and attached blocks still need integration.

The call corpus contains 52 scripts compared with Go v0.70.0 reachable-function checking. Eight decisions intentionally differ: Rust retains known facts through `any`, rejects known noncallables and invalid splats, skips unused defaults, and follows reachable loop/recursive returns. Each difference has a Rust execution witness. Separate tests cover binding against the runtime, a 1,000-function call chain on the default stack, mutual recursion, host-effect isolation, incomplete paths, exact quotas, failed-allocation cleanup and cancellation.

## Recursive call convergence

Recursive calls now reuse and widen an ancestor context when their arguments change, preserving the distinction between supplied values and defaults. Recursive return summaries also widen as their collection structure grows. Both operations freeze a structural depth at their first widening; later growth becomes gradual beyond that envelope. Known scalar alternatives remain visible, and declared contracts participate in the initial facts. Separate ordinary calls retain their exact cache keys and specialized summaries.

Cycle discovery walks the recorded call graph iteratively with metered scratch storage. New contexts record their first caller directly; ordinary call chains avoid unnecessary graph searches. Shared cached contexts can close a cycle, and changed recursive inputs requeue the owning analysis even when its return has not changed yet. Final diagnostics still follow reachable dependencies. Unmodeled callees remain incomplete, and functions without normal returns do not make trailing statements reachable.

Twelve tests cover growing arrays and hashes in arguments and results, mutual recursion, shared contexts, positional/keyword defaults, rest arguments, preserved known contradictions, separate call specializations, incomplete paths, exact quotas, sampled allocation failures, cleanup and cancellation. A 200-function recursive cycle and the existing 1,000-function ordinary chain run on the default Rust stack under normal limits. Three initial regression tests exhausted the default checker step quota before this change; they now converge.

The [recursion reference corpus](../tests/checker-recursion.json) retains all 23 comparison scripts. Go v0.70.0 completed 21, with six decisions that differ from Rust and have explicit runtime witnesses. Standalone checks of two recursively nested rest-argument cases did not finish within five seconds; their Go decisions remain unresolved, and both remain Rust execution and inference regressions. Widened recursive facts can still lose correlations and cause conservative diagnostics. This work does not expose the checker publicly or complete the remaining analysis paths.

## Collection reads

Array, string and hash indexing now preserves element and field facts, including known negative indexes, missing values and optional shape fields. Literal keys and indexes survive bindings and script calls. Array slices with known start/count retain selected elements; range and unknown numeric selectors retain conservative collection/nullability facts. Raw string indexing uses the runtime's metered Unicode and invalid-byte handling without executing script code.

The walker models `length`, `size`, `bytesize`, `empty?`, `keys`, `values`, `reverse`, `first`, `last`, `take`, `drop`, `at`, `slice`, `getbyte`, `itself`, `dup` and `nil?` for their supported data receivers. Exact positional splats work for these members. Hash projections retain possible contents without inventing an iteration order from sorted shape fields. Pure reads preserve the original local facts. Unknown methods, keyword binding and block effects still leave analysis incomplete; modeled addressed mutations are described below.

Known ordinary hashes carry a separate representation flag. Structural annotations can also describe capability objects or protected match data, so they do not imply ordinary hash member/index dispatch. Unresolved object overrides and special capture indexing remain explicitly incomplete. Known hash fields keep their normal lookup and builtin-name precedence; missing members fail before argument analysis.

Ninety-two reference scripts compare collection decisions with Go v0.70.0. Twenty-six intentional differences have Rust execution witnesses: retained literal facts exclude unreachable warnings, and known invalid operations or incompatible collection contents produce diagnostics. Independent inference checks compare 1,080 index cases and 2,786 pure-member cases with actual runtime results. Tests also cover raw bytes, shape optionality, call propagation, value snapshots, match-data inputs, exact memory/work limits, failed allocation cleanup and cached-path cancellation.

## Collection mutation facts

Independent mutation inference now separates an operation's updated receiver from its expression result. It models array indexed writes, push/prepend/insert, pop/shift, delete, clear and fill; ordinary hash writes, store/delete/replace/clear; and string prepend/insert/replace/clear. String methods preserve the original receiver. Literal tuples retain exact positions and bounded windows, while dynamic selectors and expanding windows produce conservative collection facts without allocating a runtime-sized array.

Dynamic hash writes retain ordinary-data provenance separately from structural contracts, including through generalized hashes and subsequent reads. Protected or overridden object mutation remains explicitly incomplete. Deletion uses exact value facts where equality is known; sharing an abstract type fact never proves shared runtime storage. Known invalid alternatives survive joins with unknown inputs.

Twelve unit tests cover 5,580 mutation and indexed-write cases against runtime outcomes, exact receiver/result facts, snapshots, optional fields, generalized collections, large windows, quotas, failed-allocation cleanup and cancellation. These are inference tests, separate from the addressed-flow and Go fixtures below. Property guards and public checking remain unfinished.

## Addressed mutation and collection loops

The walker now publishes modeled array/hash mutations and indexed, compound and logical writes into local facts. Metered pending addresses preserve selected positions while argument expressions edit parents, detach fresh replacements, and keep copies and temporary results separate. Negative array selectors capture their selected absolute position. Literal value equality and shared type-fact IDs never establish shared runtime storage. Uncertain attachment retains both possible outcomes; paths whose dispatch or origin cannot be modeled remain explicitly incomplete.

Mutation results remain distinct from updated receivers, including popped values and unchanged string receivers. Hash fields keep their lookup precedence, including names that resemble mutators. Safe navigation skips nil-receiver arguments. `begin ... end` expressions preserve pending outer calls and writes, declare missing body locals on exit, and clean up abandoned state on loop control transfers. The exception flow below restores pending state before rescue and cleanup paths resume.

Backward control-flow edges widen growing collection facts until they converge. Equal-length tuples retain positions; differently sized tuples become general element facts. Hash joins retain optional fields and ordinary-data provenance. Recursive collection growth becomes gradual beyond the fact depth present at the first backward join, including inputs and declared contracts. The work queue, widening memo and temporary buffers are metered and use the default Rust stack. Runtime limits are unchanged.

Precision is still limited: generalizing differently sized arrays loses prefix positions and minimum lengths, and uncertain attachment can include impossible combinations. These can produce conservative diagnostics for safe programs. The checker remains private while these limits and the other integration requirements are addressed.

Nine addressed-flow tests compare 268 parent-mutation executions, twelve branch/result executions and ten selected-position witnesses with inferred results. They also cover exact quotas, sampled allocation-failure boundaries, cleanup and cancellation. Six widening tests cover recursive growth, optional hash fields, preserved scalar contradictions, shared 2,000-level facts, quotas and the default stack. The reference corpus contains 66 Go v0.70.0 decisions: fifteen differences diagnose runtime-invalid code that Go accepts, while two reflect Go's separate temporary-update lint warnings. All seventeen have Rust runtime witnesses. These checks do not establish a public deployment gate.

## Iterable loops and destructuring

The walker models `for` over literal and inferred arrays, ordinary hashes and integer ranges. It distinguishes zero iterations from the first body, so known nonempty collections do not invent an unassigned binding or nil loop result. Singleton collections cannot repeat. Later iterations use joined element facts and the existing metered widening; nested loops converge without executing the script or materializing range elements.

The iteration source remains an immutable value snapshot when the body changes its binding, collection or nested values. Hash iterations yield key/value pairs without treating sorted type fields as insertion order. Destructuring keeps literal positions, rest windows and trailing nil padding; non-array values occupy one position. These operations also support ordinary destructuring assignments. Break payloads, bare break, next, return and normal statement/expression results follow the selected runtime semantics, including hash-loop break values.

Thirteen tests include 200 destructuring and 192 loop-control comparisons with runtime values, source/binding mutations, optional hash fields, nested growth, integer endpoints, exact quotas, failure cleanup and cancellation. Fifty reference scripts retain 49 Go checker decisions, with 22 explained differences, plus one separate Go parser rejection for a nested binding. Every reference script has a Rust execution witness. One checker difference retains the existing type-changing reassignment warning for a script that succeeds at runtime; it is not described as a runtime type error.

Iteration order and exact trip counts beyond a singleton are not tracked. Compact literal range bounds distinguish empty, singleton and repeated iteration, including descending ranges and integer endpoints, without expanding the range. Known open-ended ranges report the runtime's iteration error; dynamic endpoints retain general integer range facts. Generalized array lengths, optional fields and joined iteration states can lose correlations and produce conservative diagnostics. Opaque hash/capability iteration and unsupported body operations remain explicitly incomplete. Public checking is still unavailable.

## Case matching and conditional narrowing

The walker models `case` with or without a target, ordered `when` alternatives, matcher splats and `===`. A successful earlier matcher skips later expressions. Ranges use the runtime's inclusive, exclusive, descending and open-ended matching rules; regexes retain flags and anchors and match only strings. Regex compilation and matching use the existing metered implementation without invoking script code. Known invalid patterns and non-array matcher splats produce diagnostics, while latched cancellation and quota failures propagate.

Direct target bindings narrow on successful and failed matches. Matching retains possible integer and float kinds instead of treating mixed numeric equality as a type conversion, including large integer comparisons, fractional floats, signed zeros and NaN. Writes during matcher evaluation invalidate the old binding's predicate; immutable target facts still describe the value selected before the write. Copied bindings and stored booleans do not yet retain those correlations.

Short-circuit flow keeps at most three truth partitions for each compatible handler continuation at a basic-block entry, separated by whether the top operand is known false, known true or unresolved. This prevents a skipped right operand from erasing facts required on the evaluated path. Joins and widening remain metered. Refining a retained operand requires bytecode that preserves or duplicates that same value; equal abstract facts cannot prove runtime identity.

Fourteen focused tests include 816 runtime matcher comparisons, checking the actual branch against literal, general-kind, unknown and `any` inputs. Further witnesses cover reassignment, pending writes and calls, nested loop exits, targetless conditions, short circuits and unrelated boolean values. Exact quotas, sampled allocation failures, cleanup, cancellation and deep shared facts run under normal limits on the default Rust stack. The signed-zero regression first reproduced a discarded valid branch before the fix.

The [case reference corpus](../tests/checker-case.json) retains 53 Go v0.70.0 checker decisions with 29 explained differences, each supported by a Rust execution witness. Known impossible or matched branches avoid spurious warnings; invalid splats, open-ended iteration and incompatible result kinds remain diagnosed. Copy correlations and structural numeric matches can still produce conservative results. This is private analysis; public checking and diagnostic sorting/deduplication remain unfinished.

## Exceptions and cleanup

The walker now models ordered rescue selection, successful-body `else`, `ensure`, explicit raises and retry. An empty matching rescue clause propagates the error after cleanup. Rescue bindings shadow their outer names only within the clause; skipped declarations become nil at the same boundaries as execution. Bound error facts describe the class, message, source frame and backtrace fields without constructing runtime diagnostics or executing callbacks.

Cleanup entries keep pending values, returns, loop transfers, retries and errors separate. An ordinary cleanup result leaves the pending outcome intact, while a new error or transfer replaces it. Writes invalidate saved predicates without changing captured values. Retry preserves completed writes and skips its own handler's ensure between attempts; nested ensures run when retry exits their regions. Cross-call retry runs the callee's cleanup before reporting LocalJumpError to its caller.

Script-call summaries propagate possible escaping exception classes and requeue callers when those classes change. Bare raise inherits the currently rescued error through helper calls. Call contexts retain the absence of an ambient error separately when recursive inputs widen, so it cannot disappear beside known exception classes. Known missing-name reads and modeled operation failures enter the appropriate handler with earlier writes preserved and rejected native updates unpublished. Known type contradictions remain diagnostics even when rescue catches their runtime failures.

Handler state, saved outcomes, work queues, call contexts and error-value facts use normal accounting and the default Rust stack. Actual checker cancellation, deadlines and quota failures propagate directly; they do not become abstract catchable errors. Registered host calls describe possible ordinary errors using signatures without invoking callbacks or validators.

Eighteen focused tests cover 225 combinations of errors, returns, loop exits and cleanup, plus scope, retry, call summaries, inherited rethrows, saved predicates, receiver preservation and host-effect isolation. Exact quotas, sampled allocation failures, reclamation, cancellation and deadlines are checked. An out-of-range float-index regression first demonstrated a missing rescue path before its correction. Three previously incomplete syntax fixtures now have runtime witnesses; tests still retain reachable unmodeled handler bodies as incomplete.

The [exception reference corpus](../tests/checker-exceptions.json) records 43 Go v0.70.0 checker decisions with five explained differences and Rust execution witnesses. One avoids an unreachable rescue after a callee returns from ensure; four retain known-invalid operation diagnostics that Go omits. This remains gradual analysis: generalized native inputs can conservatively reach extra error paths, and invalid typed returns can retain provisional success facts alongside their diagnostics. Precise error-message values, protected error-object mutation and rendering, block/nonlocal-call contexts, general dispatch and public checking remain unfinished.

## Remaining integration

Completion still requires:

- Opaque iterable dispatch, nonlocal block returns, general type narrowing, scalar constant propagation and stored predicate relations. Collection-loop, recursive-call and exception-effect precision need further work; value-origin facts must preserve correlations without confusing equivalent types with identical values.
- Class, module, builtin and namespaced host calls, variable-size splats, block binding and attached-method restrictions. Plain script calls and registered positional host signatures are implemented internally; public entry binding and whole-program integration remain required.
- Property constraints, broader mutable-container dispatch, precise attachment correlations and invalidation after unmodeled effects. Ordinary addressed writes and collection-loop widening are integrated internally.
- Required-file source discovery and export analysis without executing initializers, plus the selected source and root binding rules.
- Host-value facts, per-call capability descriptors, strict-effects validation and checker accounting across every entry point. Signature metadata must be inspected without invoking callbacks or validators.
- Sorted, deduplicated source diagnostics, whole-file checking, reachable-function checking, exact-call checking, checked invocation and CLI gates. A rejected check must not execute script effects.
- Broader reference fixtures, required-file and capability tests, cancellation/quotas at the public boundary, and the remaining temporary compiler allocation accounting.

The full-language goal remains active. This internal foundation does not establish a deployment gate or complete ADR-004.
