# Known differences from Go v0.70.0

The reference is Go Vibescript v0.70.0 at `5cba216c33bea8890787d64efb2ab926a761fb1b`. The shared success and rejection suites require matching results. The cases below are separate, verified differences discovered during the control-flow port; they are not included in the matching-case count. Rust expectations are covered by `tests/control.rs`.

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
