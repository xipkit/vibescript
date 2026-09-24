# Known differences from Go v0.70.0

The reference is Go Vibescript v0.70.0 at `5cba216c33bea8890787d64efb2ab926a761fb1b`. The shared success and rejection suites require matching results. The separate compatibility audit covers thirty-three collection, regex and control-flow differences, forty-two host-binding differences, eight required-file differences, sixteen attached-capability-method differences, eight host-block control-flow differences, twenty-eight host-signature differences, and two previously different mutation cases that now agree. It retains the observed Go outputs and checks each Rust result against its port contract.

## Attached capability methods and host blocks

Both ordinary and block-capable host methods follow the selected ADR-006 restriction: indexed and scoped members remain attached, while immediate calls work. The capability and block fixture generators each cover two detached forms with strict effects and accounting enabled or disabled. Go permits extracting those methods.

The host-block audit records the explicitly selected control-flow preservation rule. Rust preserves a pending `break` or nonlocal `return` when a callback ignores `ErrorKind::ControlFlow`, and prevents another invocation of that block from executing script. Go allows the callback to swallow the signal, rerun the block and replace its result. The two examples are checked in all four strict-effects/accounting combinations. Ordinary block exceptions remain available for host recovery and repeated invocation.

## Capability receiver publication

Go's builtin callbacks receive the capability object as a live `receiver` map: a write into it is visible to the script, and every script alias of the object observes it, because the object keeps a shared mutable identity for the call. Rust follows ADR-006's collection value semantics and publishes into the binding instead, as selected on 2026-09-22. `HostCall::set_receiver_field` writes to the capability or method-bearing global binding that holds the receiver, so that name and its nested hashes observe the write, while copies the script took earlier stay unchanged. Only block-capable and async methods receive the handle, publication cannot replace a method field, and fields cannot be deleted. The documented pattern, where a factory method installs data and the script then reads it through the capability, behaves the same in both implementations.

## Static checker strictness

Rust's `vibes check` is deliberately stricter than Go's, as selected on 2026-09-22. Both report a typed boundary when any known alternative of the value fails it, such as passing an `int?` to an `int` parameter. For operators and member calls, Go v0.70.0 reports only when every alternative fails: `nil + 1` is an error, while `v + 1` with `v: int?`, `"x" + items[i]` and `v.upcase` with `v: string?` pass. Rust also reports these whenever a finite known alternative, including `nil` from an index or an empty array before a loop fills it, cannot succeed. Unknown and `any` values stay gradual in both.

Rust also fails the gate when it reaches an expression it does not yet analyze, reporting it as incomplete; Go treats such a value as unknown. Of the 277 site, example and upstream test programs, 196 are clean and 16 are rejected in both checkers. The other 65 are rejected only by Rust: its union-alternative errors, and known failures in deliberate error fixtures that Go's checker does not report. None report incomplete analysis.

## Language server diagnostics

The Go reference's `vibes lsp` publishes compile errors only. This port's server also publishes the static checker's findings for the whole document, so documents the checker rejects show errors in the editor, and the stricter checker described above applies there too: 73 of the 241 documents in the [language server comparison](lsp.md#comparison-with-the-reference) get findings the reference does not report. The port's parser reports only its first error, which matches the reference's first error in text and position (see [source diagnostics](diagnostics.md)); hover, completion, signature help, definitions, symbols and formatting match the reference across the comparison.

## Out-of-range float calendar fields

Go converts a float calendar field or `Time.at` argument that does not fit a 64-bit integer with its implementation-defined conversion, which differs by CPU. On arm64 it saturates, so `Time.utc(2024, 1e100, 1)` normalizes from the largest month; on amd64 it wraps to the smallest integer, giving another date, and `Time.at(9.223372036854776e18)` raises. The Rust port uses the arm64 result on every platform, as selected on 2026-09-22, so a script's result does not depend on the host CPU. The shared corpus was recorded on arm64; on amd64 Go differs from it in six such cases.

## Host signature boundaries

Published signatures follow the documented runtime type contract and the selected consistent binding rules. Named types resolve through the active source before the call root, including required-file defaults and qualified file aliases. Go v0.70.0 can substitute a same-named root enum or class and fail an otherwise valid call, or fail to find a file's type alias. The signature audit records twenty-four such cases across registered methods, capabilities and ordinary globals, where allowed by strict effects.

Rust also validates an absorbed block `break` against the host signature's result type, as it does for custom capability return contracts. Go's signature wrapper skips validation when the callback returns the block's control signal; `break "bad"` can therefore escape a declared `int` result. Four audit cases preserve that Go observation while requiring Rust to reject it. Nonlocal returns continue to validate at their defining script function.

## Host binding precedence

Consistent lookup and assignment through the nearest existing binding was explicitly selected on 2026-09-16.

Rust resolves named and computed calls through the nearest existing binding. Parameters, module constants and enclosing initializer locals take precedence over root data bindings, including host globals, classes and builtins. Named calls retain their existing declared script-function and module-method dispatch when a constant has the same name. In Go v0.70.0, a root data binding can win over a module constant when that constant is called. For example, `module M; Parser=JSON[:parse]; def self.apply; Parser("3"); end; end` returns `3` through `M.apply` in Rust even when a host global named `Parser` contains `nil`; Go tries to call that `nil`. A root class or builtin with the same name causes the same inconsistency.

A block assignment also preserves an existing host binding unless a nearer local shadows it. With a host global `count=9`, `[1].each { count += 1 }; count` returns `10` in Rust. Go creates an uninitialized block local and raises an addition error, although a direct `count += 1` outside the block succeeds. Rust keeps the existing binding for both ordinary and compound assignments. Explicit block parameters retain their separate scope.

The host-global generator in [fixtures.py](../scripts/fixtures.py) records these contracts across named, computed, splat and bare calls, with strict effects enabled and disabled. Thirty additional host-global cases belong to the shared success suite. Forty-two differing cases retain Go's rescued error result in the compatibility audit; they are not counted as matching evaluations.

## Required-file values and lookup errors

The [file-module fixture generator](../scripts/module_fixtures.py) separately records eight observations across production/development mode and ordinary/strict effects. Four preserve the selected collection value semantics: when a required file's private array is returned and later mutated by another call, the earlier result stays unchanged. Go exposes the later mutation through both results in this case.

Four preserve the selected rule that a lookup error can be rescued at the expression that raised it. Accessing an unexported module member raises inside the nearest `rescue` in Rust. Go defers the error past that handler to a caller's handler. The audit records both results explicitly. These cases do not count toward the shared file-module successes.

## State isolation across host calls

The selected contract isolates mutable class and module state at every `Script.call` boundary, including calls into another compiled script or engine. This decision was explicitly selected on 2026-09-15. Go v0.70.0 isolates a returned source class or module when it reenters its original compiled script, but lets a different script mutate the original live state. Rust consistently gives each call independent state.

Existing same-script behavior remains the baseline: imported instance fields preserve their values, shared references and cycles within the receiving call, while class and source-module declarations initialize fresh invocation state. Mutations must not change the source value or another concurrent call. Foreign code must still use the receiving call's accounting, cancellation and module policy.

Cross-script source namespace and instance dispatch now use the original compiled code and host callbacks on the receiving VM stack. Source-program globals and class/module initializers start fresh, while imported instance graphs preserve their contents. Required-file exports retain their private state, which is copied at each receiving call boundary. Independent native tests cover the selected cross-script behavior; these tests are separate from the thirty-three intentional and two resolved cases initially recorded by the compatibility audit.

## Documented value semantics take precedence

The Rust port follows Vibescript's documented collection value semantics when they conflict with Go v0.70.0. This policy was explicitly selected on 2026-09-13. [ADR-006](https://github.com/xipkit/vibescript/blob/v0.70.0/docs/adr/006-slim-language-for-predictable-sandboxing.md) defines arrays and hashes as logical values whose shared storage is not observable. The [standard-library contract](https://github.com/xipkit/vibescript/blob/v0.70.0/docs/stdlib_core_utilities.md) says mutations update the named binding or path while other values remain unchanged.

An evaluated operand or argument retains its value. Iteration and transformation traverse captured values, so callback writes to a surrounding binding cannot change already captured elements. Adding an unused alias cannot change the result. For `a=[1]; x=a+a.push(2); [x,a]`, the selected result is `[[1,1,2],[1,2]]`: the left operand remains `[1]`, the right operand is `[1,2]`, and the local `a` contains `[1,2]`.

[compatibility-cases.json](compatibility-cases.json) records each policy and its reason. Twenty-four collection cases have explicit selected expectations, exercised by [value_semantics.rs](../tests/value_semantics.rs) both with and without an extra alias. Twenty-two remain intentional reference differences; the two negative-index mutation cases now agree with Go. The separately selected control-flow contracts are recorded below.

## Captured array positions

When an argument or block appends to an array, a pending mutation through a valid negative index updates the originally selected element. For `a=[[1]]; x=a[-1].push((while true; a.push([9]); break 2; end)); [x,a]`, both implementations now return `[[1,2],[[1,2],[9]]]`. The block-fill variant likewise updates the original child while preserving appended siblings.

Compound and logical indexed assignments retain the position selected by their initial read. Plain assignment still evaluates its right-hand side before selecting the target, and custom index methods receive the original selector values. Replacing the parent binding or selected child detaches a pending mutator from that binding. Slices remain temporary collection values. Nested property types still apply, and rejected writes preserve mutations already completed by an argument or block.

## Inclusive range endpoints

Go's `for` range counters can wrap when an inclusive loop reaches the maximum or minimum 64-bit integer. For example, `for n in max..max` visits `max`, wraps to `min`, and continues. A probe with `break if n < 0` returned `[max, min]` from Go. The selected contract stops at the endpoint: Rust visits `[max]` and terminates. Rust keeps the iteration position and length in a wider integer so the endpoint cannot wrap. This is an intentional difference.

## Hash-loop break results

In Go, an expression-valued hash loop loses its break result when the hash helper returns to the outer loop evaluator:

```vibescript
x = for key, value in {a: 1}
  break 7
end
x
```

Go returns `{a: 1}`; Rust returns the selected break value, `7`. With a bare `break`, Go still returns the hash and Rust returns `nil`. Rust applies the same break-result rules to array, range, hash, and while loops. Both hash-loop cases are intentional differences under the selected contract.

## Mutation during collection iteration

Rust captures an immutable collection snapshot for iteration and for the normal result of a `for` expression. Writes to the surrounding local preserve that snapshot.

Go's result can depend on whether another script local aliases the collection. With no alias, this returns `[[1,2,3,3],[1,2,3,3]]` in Go and `[[1,2],[1,2,3,3]]` in Rust:

```vibescript
a = [1, 2]
x = for value in a
  a.push(3)
end
[x, a]
```

Adding `b = a` before the loop makes Go return the original snapshot for `x` too. Reassigning `a` to a new array or using `a += [3]` also leaves Go's captured result unchanged. The difference comes from which writes detach Go's shared collection storage.

Go array iteration can also observe index writes through its captured backing, and a preceding append can change that behavior by replacing the backing. Hash entries are captured before iteration, but the hash returned by a normal loop expression can include writes performed during the loop. Rust's captured values remain unchanged in both cases.

Builtin block iteration exposes the same gap. For `a=[1,2,3]; a.map {|v| a[1]=9; v}`, Go returns `[1,9,3]` and Rust returns `[1,2,3]`. Separate audit records cover array `each`, `map`, `select`, and `each_with_index`, including the yielded sequence, the mutated local and the method result. For hash `each`, both implementations yield the original entries, but Go's returned receiver includes the write while Rust's result remains the original snapshot. Hash `map`, `select`, and `transform_values` mutation cases that agree are in the shared success suite.

Mutating blocks originally added five observations: filling a prefix after a callback edits the captured tail, filtering after an index write, the receiver returned by a filter that removes nothing, and filling a captured negative-index child after its parent grows. The negative-index case now agrees with Go; the other four remain intentional differences. Hash filters also have a distinct commit rule that both Go modes agree on: they remove the selected keys from the current captured hash, preserving callback writes to other keys. Rust implements and tests that rule.

Key blocks in `sort_by`, `min_by`, and `max_by` also traverse the captured array. Two ordering audit records show how a callback writing the last element affects Go's sorted output and selected maximum while Rust retains the original values. Creating an alias before the call makes the examined mutation cases agree; these matching cases also record the key callback sequence. Comparator-form `sort` copies its input before calling the block and agrees in the examined mutation case.

Adjacent grouping and recursive hash transforms add five records of the same distinction. `slice_when` and `chunk_while` can observe index writes through Go's captured array, changing later pairs and groups. A `merge` conflict block can change a later hash argument before Go visits it, while Rust retains the evaluated argument. Two `deep_transform_keys` cases show callback writes becoming visible in nested arrays or hashes in Go. Rust traverses the captured snapshots; the corresponding cases with explicit aliases agree and remain in the shared success suite.

The selected policy preserves these captured logical values. These differences stay visible in the audit as intentional results. Negative-index parent growth follows the selected captured-position contract above.

## Evaluated collection values

The indexed-write audit originally found three additional differences, recorded with complete source and observed outputs in [compatibility-cases.json](compatibility-cases.json):

- `a = [[1]]; a[0] += a[0].push(2); a` returns `[[1,2,1,2]]` in Go and `[[1,1,2]]` in Rust. Go can expose the mutation through the already-evaluated left operand.
- `a = [1]; x = a + a.push(2); [x,a]` has the same distinction for an ordinary binary operand: Go returns `[[1,2,1,2],[1,2]]`; Rust returns `[[1,1,2],[1,2]]`.
- A pending `a[-1].push(...)` whose argument appends a new array to `a` now updates the previously captured child in both Go and Rust.

The first two results follow the selected value semantics: an already evaluated left operand cannot change while its right operand runs. The third and the block-fill variant are resolved. The selected endpoint and hash-break contracts make the three control-flow records intentional differences. After rebuilding the comparison binaries, run:

```sh
python3 scripts/audit-compatibility.py --out .cache/compatibility-audit
```

The audit accepts selected results only when Rust matches their documented expectations, and marks them resolved when Go matches too. It also checks identical portable/SIMD Rust work, peak-memory and retained-memory counters for successful cases. It fails on unresolved cases, changed Rust behavior, changed reference output or mismatched accounting, and preserves results for all four builds. All intentional cases remain separate from the matching conformance count.

## Copy oracle investigation

The reference's `internal/runtime/collection_values.go` tracks durable script bindings separately from temporary values held by the evaluator. The test-only `VIBES_COW_ALWAYS_COPY` mode forces addressed writes to copy, bypassing that optimization. This mode helps explain the implementation differences; the documented semantics determine the Rust contract.

The [recorded oracle results](reference-view-results.json) compare both Go modes with the Rust builds. Rust matches Go's copy mode on twenty-one of thirty-one cases, including all five adjacent-grouping, merge and deep-transform records. The compound indexed assignment produces three different results: normal Go returns `[[1,2,1,2]]`, copying Go returns `[[1,2]]`, and Rust returns the selected `[[1,1,2]]`. Integer endpoint wrapping and hash-loop break results are also unaffected by copy mode. Copy mode preserves a returned module array's snapshot, but an indexed write to a module field still changes the original local array in both Go modes. The index getter case also changes both saved and stored arrays in both Go modes. Agreement with either Go mode is evidence about the implementation, not a substitute for the language contract.

After building the comparison binaries, reproduce the investigation with:

```sh
python3 scripts/audit-reference-views.py --out .cache/reference-view-audit
```

The tool copies the pinned Go module into its output directory, injects a test through a build overlay, and records outputs and binary hashes for normal Go, copying Go, and both Rust builds. It leaves the module cache and Go checkout unchanged. These recorded copy-oracle results are historical; the ordinary compatibility audit at that milestone distinguished thirty-three intentional cases and two resolved cases.


## Builtin descriptors

The reference permits a stateless builtin descriptor to be obtained through indexed or scoped namespace access, for example `f = Math::sqrt; f(9)` or `f = Math["sqrt"]; f(9)`. The Rust port currently preserves that observed behavior. Ordinary reads of non-auto-invoking builtin bindings are rejected, and JSON cannot encode a builtin descriptor. These descriptors contain no captured script frame; general script-function values and escaping blocks remain outside the implemented surface. The final ADR-006 boundary audit must account for this reference behavior alongside its stated restrictions on executable values. The collection value-semantics decision does not settle this separate boundary.

The user selected ADR-006's restriction for required script functions: they remain callable through their module but cannot be extracted, stored, passed or returned as executable values. Go v0.70.0 permits this through indexed and scoped export reads such as `m[:fn]` and `m::fn`; Rust rejects those reads outside immediate call-target syntax. This decision applies to required script functions and preserves the existing stateless builtin-descriptor behavior. [Required file exports](require.md) are implemented, with integration tests covering direct calls and rejected extraction through collections, iteration and host transfers. Collections containing extracted functions are rejected before computed-call arguments can produce effects. Broader module conformance remains part of the unfinished language port.

## Capability methods

The user selected ADR-006's attached-method restriction for host capabilities on 2026-09-16. `sms.send(...)`, `sms::send(...)` and `sms[:send](...)` remain calls; reading, storing, passing or returning the method by itself is rejected. Capability namespaces can still be copied or aliased within their invocation. A saved namespace cannot grant its methods to a later invocation.

Go v0.70.0 permits indexed and scoped extraction, including assigning a method to a local and calling it. Eight policy evaluations cover both forms, strict and ordinary effects, and enabled or disabled accounting. These intentional differences stay separate from matching conformance cases. Stateless core builtin descriptors retain their existing separate behavior. See [host capabilities](capabilities.md).

## Regex namespace anchors

Go's `Regex.match`, `Regex.replace` and `Regex.replace_all` helpers can take a literal-prefix shortcut for anchored patterns. The shortcut omits the anchors: `Regex.match("(?:^a$)", "ab")` returns `"a"`, while `"ab".match?("(?:^a$)")` correctly returns false. Replacement helpers can likewise replace text that the complete expression does not match. Rust's state machine retains the anchor assertions, returning nil and preserving the input in those cases.

One aggregate audit case records all three helpers alongside the predicate. Fifty-one focused probes reproduce this discrepancy across grouping, flags and subjects; they are excluded from matching conformance counts. The selected regex policy honors anchors. This aggregate case is an intentional difference, with explicit expected results and native regression coverage.

String global substitutions expose another assertion-context issue. Go repairs suffix searches with a one-byte preceding window, which can skip valid word boundaries and non-boundaries. For example, `"a b aa".gsub(/\b/, "X")` returns `"Xa Xb XaaX"` in Go and `"XaX XbX XaaX"` in Rust. Rust retains the original subject context for both templates and blocks. One aggregate audit case and sixty focused comparisons record this difference; the Rust expectations are also checked by an independent character-boundary oracle.

## Match-data protection

The selected policy preserves match data's protected fields through nested writes and duplication. Copying its captures into a separate variable produces an ordinary array that can be changed independently. Direct writes such as `m.captures.push("x")` and clearing `m.dup` are rejected. A duplicate retains the original match's string rendering.

Go's behavior varies by operation: `m.captures.push("x")` succeeds, while `m.captures.clear` fails when its write reaches the protected parent. Go's deep clone also drops the protection and published rendering, making `m.dup.clear` succeed and interpolation render `<object>`. Three audit records preserve these observations and verify Rust's chosen rejection or value. Ten focused comparisons cover nested captures, named captures, duplicate containers and rendering. These intentional differences are excluded from matching conformance totals.

Go's bounded replacement-block conversion also emits `<object>` when the block returns match data. Rust keeps its whole-match rendering in this context. A fourth match-data audit record and four focused substitution comparisons preserve that selected behavior.

## Module collection values

Module aliases share state within an invocation, but their array and hash fields remain logical values. If a method returns a module array and a later call appends to that module variable, the earlier result stays unchanged. If a local array is assigned to a module field, a later indexed field write leaves the local array unchanged. Go v0.70.0 can expose both writes through the earlier values. Two complete-source cases record those Go results and the selected Rust expectations, with and without an extra alias. They are excluded from the shared matching count.

## Nested match-data protection

Match-data protection survives temporary results and duplication, including nested capture writes. Block mutators reject protected paths before running callbacks. Explicit copies of the capture array itself remain independent mutable values. Two additional compatibility cases record the selected rejection behavior.

## Collections returned by index methods

An index getter returns a logical collection value. A nested write to that temporary leaves both the stored collection and an earlier snapshot unchanged. The `class_index_getter_snapshot` record exercises a grid backed by a hash: after storing `[1]`, saving the getter result and writing `grid[0,0][0] = 8`, Rust returns `[[1],[1]]`. Go v0.70.0 returns `[[8],[8]]`. The Rust expectation follows the selected value-semantics policy and is checked with and without an extra alias.

## Writes through bare field names

In a class, `rows[0] = 9` with no local or method named `rows` writes the `@rows` field, as in Go v0.70.0. Go writes the stored collection without isolating it first, so a snapshot saved from the field earlier, such as `saved = @rows`, changes too. Rust isolates the write as it does for `@rows[0] = 9`, and the snapshot keeps its value, following the selected value-semantics policy. [value_semantics.rs](../tests/value_semantics.rs) checks this.
