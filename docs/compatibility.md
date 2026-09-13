# Known differences from Go v0.70.0

The reference is Go Vibescript v0.70.0 at `5cba216c33bea8890787d64efb2ab926a761fb1b`. The shared success and rejection suites require matching results. The cases below are separate, verified differences discovered during the language port; they are not included in the matching-case count. The four-build compatibility audit covers all nineteen recorded cases, and `tests/control.rs` also checks the original loop differences.

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

Rust currently captures an immutable collection snapshot for iteration and for the normal result of a `for` expression. Writes to the surrounding local preserve that snapshot.

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

Matching this allocation-dependent behavior would require a deliberate compatibility policy. It remains an open item in the full-port checklist; the current Rust behavior is recorded explicitly so ordinary success counts cannot conceal the difference.

## Evaluated collection values

The indexed-write audit found three additional differences, recorded with complete source and observed outputs in [compatibility-cases.json](compatibility-cases.json):

- `a = [[1]]; a[0] += a[0].push(2); a` returns `[[1,2,1,2]]` in Go and `[[1,1,2]]` in Rust. Go can expose the mutation through the already-evaluated left operand.
- `a = [1]; x = a + a.push(2); [x,a]` has the same distinction for an ordinary binary operand: Go returns `[[1,2,1,2],[1,2]]`; Rust returns `[[1,1,2],[1,2]]`.
- A pending `a[-1].push(...)` whose argument appends a new array to `a` can still update the previously captured child in Go. Rust currently treats the changed path as a temporary and leaves the original child unchanged.

These are unresolved semantic gaps, not intentional additions to the language. They require further work on evaluated collection views and alias publication. The JSON record also includes the earlier range and loop differences. After rebuilding the comparison binaries, run:

```sh
python3 scripts/audit-compatibility.py --out .cache/compatibility-audit
```

The audit returns a failure status while any recorded case differs, preserves complete results for all four builds, and distinguishes a resolved case from a newly changed result. These cases are separate from the matching conformance count.

## Copy oracle investigation

The reference's `docs/adr/006-slim-language-for-predictable-sandboxing.md` specifies logical collection values and states that sharing is not observable. Its `internal/runtime/collection_values.go` tracks durable script bindings separately from temporary values held by the evaluator. The test-only `VIBES_COW_ALWAYS_COPY` mode forces addressed writes to copy, bypassing the durable-reference optimization. Rust's immutable snapshots remain the working value-semantics policy while the observed Go differences stay open.

The [recorded oracle results](reference-view-results.json) compare both Go modes with the Rust builds. Rust matches Go's copy mode on fifteen of nineteen cases. The compound indexed assignment produces three different results: normal Go returns `[[1,2,1,2]]`, copying Go returns `[[1,2]]`, and Rust returns `[[1,1,2]]`. Integer endpoint wrapping and hash-loop break results are also unaffected by copy mode. These results characterize the source of the differences; they do not establish full language compatibility or resolve the records.

After building the comparison binaries, reproduce the investigation with:

```sh
python3 scripts/audit-reference-views.py --out .cache/reference-view-audit
```

The tool copies the pinned Go module into its output directory, injects a test through a build overlay, and records outputs and binary hashes for normal Go, copying Go, and both Rust builds. It leaves the module cache and Go checkout unchanged. The ordinary compatibility audit continues to report nineteen open cases.
