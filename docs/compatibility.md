# Known differences from Go v0.70.0

The reference is Go Vibescript v0.70.0 at `5cba216c33bea8890787d64efb2ab926a761fb1b`. The shared success and rejection suites require matching results. The cases below are separate from that count: twenty-five intentional differences follow the selected collection and regex semantics, and five remain unresolved. The four-build compatibility audit retains the observed Go outputs and checks each Rust result against its selected policy.

## Documented value semantics take precedence

The Rust port follows Vibescript's documented collection value semantics when they conflict with Go v0.70.0. This policy was explicitly selected on 2026-09-13. [ADR-006](https://github.com/xipkit/vibescript/blob/v0.70.0/docs/adr/006-slim-language-for-predictable-sandboxing.md) defines arrays and hashes as logical values whose shared storage is not observable. The [standard-library contract](https://github.com/xipkit/vibescript/blob/v0.70.0/docs/stdlib_core_utilities.md) says mutations update the named binding or path while other values remain unchanged.

An evaluated operand or argument retains its value. Iteration and transformation traverse captured values, so callback writes to a surrounding binding cannot change already captured elements. Adding an unused alias cannot change the result. For `a=[1]; x=a+a.push(2); [x,a]`, the selected result is `[[1,1,2],[1,2]]`: the left operand remains `[1]`, the right operand is `[1,2]`, and the local `a` contains `[1,2]`.

[compatibility-cases.json](compatibility-cases.json) records each policy and its reason. Nineteen cases have explicit documented expectations, exercised by [value_semantics.rs](../tests/value_semantics.rs) both with and without an extra alias. They are intentional reference differences, not missing language features. This decision does not by itself settle control flow or publication through a nested path that changes during argument or callback evaluation.

## Inclusive range endpoints

Go's `for` range counters can wrap when an inclusive loop reaches the maximum or minimum 64-bit integer. For example, `for n in max..max` visits `max`, wraps to `min`, and continues. A probe with `break if n < 0` returned `[max, min]` from Go. Rust visits `[max]` and terminates. Rust keeps the iteration position and length in a wider integer so the endpoint cannot wrap.

## Hash-loop break results

In Go, an expression-valued hash loop loses its break result when the hash helper returns to the outer loop evaluator:

```vibescript
x = for key, value in {a: 1}
  break 7
end
x
```

Go returns `{a: 1}`; Rust returns `7`. With a bare `break`, Go still returns the hash and Rust returns `nil`. Rust applies the same break-result rules to array, range, hash, and while loops.

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

Mutating blocks add five observations of the same distinction: filling a prefix after a callback edits the captured tail, filtering after an index write, the receiver returned by a filter that removes nothing, and filling a captured negative-index child after its parent grows. These remain separate from the matching cases. Hash filters also have a distinct commit rule that both Go modes agree on: they remove the selected keys from the current captured hash, preserving callback writes to other keys. Rust implements and tests that rule.

Key blocks in `sort_by`, `min_by`, and `max_by` also traverse the captured array. Two ordering audit records show how a callback writing the last element affects Go's sorted output and selected maximum while Rust retains the original values. Creating an alias before the call makes the examined mutation cases agree; these matching cases also record the key callback sequence. Comparator-form `sort` copies its input before calling the block and agrees in the examined mutation case.

Adjacent grouping and recursive hash transforms add five records of the same distinction. `slice_when` and `chunk_while` can observe index writes through Go's captured array, changing later pairs and groups. A `merge` conflict block can change a later hash argument before Go visits it, while Rust retains the evaluated argument. Two `deep_transform_keys` cases show callback writes becoming visible in nested arrays or hashes in Go. Rust traverses the captured snapshots; the corresponding cases with explicit aliases agree and remain in the shared success suite.

The selected policy preserves these captured logical values. These differences stay visible in the audit as intentional results. The negative-index parent-growth case remains open because the documentation does not specify which changed path should receive the final write.

## Evaluated collection values

The indexed-write audit found three additional differences, recorded with complete source and observed outputs in [compatibility-cases.json](compatibility-cases.json):

- `a = [[1]]; a[0] += a[0].push(2); a` returns `[[1,2,1,2]]` in Go and `[[1,1,2]]` in Rust. Go can expose the mutation through the already-evaluated left operand.
- `a = [1]; x = a + a.push(2); [x,a]` has the same distinction for an ordinary binary operand: Go returns `[[1,2,1,2],[1,2]]`; Rust returns `[[1,1,2],[1,2]]`.
- A pending `a[-1].push(...)` whose argument appends a new array to `a` can still update the previously captured child in Go. Rust currently treats the changed path as a temporary and leaves the original child unchanged.

The first two results follow the selected value semantics: an already evaluated left operand cannot change while its right operand runs. The third remains open because publication through the changed negative-index path needs a separate decision based on the language contract. Together with the block-fill variant and the three control-flow records, that leaves five unresolved differences. After rebuilding the comparison binaries, run:

```sh
python3 scripts/audit-compatibility.py --out .cache/compatibility-audit
```

The audit accepts intentional results only when Rust matches their documented expectations. It fails on unresolved cases, changed Rust behavior, or changed reference output, and preserves results for all four builds. All intentional cases remain separate from the matching conformance count.

## Copy oracle investigation

The reference's `internal/runtime/collection_values.go` tracks durable script bindings separately from temporary values held by the evaluator. The test-only `VIBES_COW_ALWAYS_COPY` mode forces addressed writes to copy, bypassing that optimization. This mode helps explain the implementation differences; the documented semantics determine the Rust contract.

The [recorded oracle results](reference-view-results.json) compare both Go modes with the Rust builds. Rust matches Go's copy mode on twenty of twenty-four cases, including all five adjacent-grouping, merge and deep-transform records. The compound indexed assignment produces three different results: normal Go returns `[[1,2,1,2]]`, copying Go returns `[[1,2]]`, and Rust returns the selected `[[1,1,2]]`. Integer endpoint wrapping and hash-loop break results are also unaffected by copy mode. Agreement with either Go mode is evidence about the implementation, not a substitute for the language contract.

After building the comparison binaries, reproduce the investigation with:

```sh
python3 scripts/audit-reference-views.py --out .cache/reference-view-audit
```

The tool copies the pinned Go module into its output directory, injects a test through a build overlay, and records outputs and binary hashes for normal Go, copying Go, and both Rust builds. It leaves the module cache and Go checkout unchanged. The ordinary compatibility audit distinguishes twenty-five intentional cases from five open cases.


## Builtin descriptors

The reference permits a stateless builtin descriptor to be obtained through indexed or scoped namespace access, for example `f = Math::sqrt; f(9)` or `f = Math["sqrt"]; f(9)`. The Rust port currently preserves that observed behavior. Ordinary reads of non-auto-invoking builtin bindings are rejected, and JSON cannot encode a builtin descriptor. These descriptors contain no captured script frame; general script-function values and escaping blocks remain outside the implemented surface. The final ADR-006 boundary audit must account for this reference behavior alongside its stated restrictions on executable values. The collection value-semantics decision does not settle this separate boundary.

## Regex namespace anchors

Go's `Regex.match`, `Regex.replace` and `Regex.replace_all` helpers can take a literal-prefix shortcut for anchored patterns. The shortcut omits the anchors: `Regex.match("(?:^a$)", "ab")` returns `"a"`, while `"ab".match?("(?:^a$)")` correctly returns false. Replacement helpers can likewise replace text that the complete expression does not match. Rust's state machine retains the anchor assertions, returning nil and preserving the input in those cases.

One aggregate audit case records all three helpers alongside the predicate. Fifty-one focused probes reproduce this discrepancy across grouping, flags and subjects; they are excluded from matching conformance counts. The selected regex policy honors anchors. This aggregate case is an intentional difference, with explicit expected results and native regression coverage.

String global substitutions expose another assertion-context issue. Go repairs suffix searches with a one-byte preceding window, which can skip valid word boundaries and non-boundaries. For example, `"a b aa".gsub(/\b/, "X")` returns `"Xa Xb XaaX"` in Go and `"XaX XbX XaaX"` in Rust. Rust retains the original subject context for both templates and blocks. One aggregate audit case and sixty focused comparisons record this difference; the Rust expectations are also checked by an independent character-boundary oracle.

## Match-data protection

The selected policy preserves match data's protected fields through nested writes and duplication. Copying its captures into a separate variable produces an ordinary array that can be changed independently. Direct writes such as `m.captures.push("x")` and clearing `m.dup` are rejected. A duplicate retains the original match's string rendering.

Go's behavior varies by operation: `m.captures.push("x")` succeeds, while `m.captures.clear` fails when its write reaches the protected parent. Go's deep clone also drops the protection and published rendering, making `m.dup.clear` succeed and interpolation render `<object>`. Three audit records preserve these observations and verify Rust's chosen rejection or value. Ten focused comparisons cover nested captures, named captures, duplicate containers and rendering. These intentional differences are excluded from matching conformance totals.

Go's bounded replacement-block conversion also emits `<object>` when the block returns match data. Rust keeps its whole-match rendering in this context. A fourth match-data audit record and four focused substitution comparisons preserve that selected behavior.
