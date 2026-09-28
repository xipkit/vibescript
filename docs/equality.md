# Equality

`==` and `!=` compare values by content and return `bool`. Integers and floats compare exactly: `9007199254740993 == 9007199254740992.0` is false, because converting the integer to a float first would erase the difference. The same rule applies inside arrays and hashes, so `[1] == [1.0]` is true while that large pair is not. Arrays compare element by element; hashes compare their entries, and insertion order does not change equality. NaN is unequal to everything under `==`, itself included.

Class instances compare by identity unless the class defines `==`. Enum members compare nominally: `Status::Draft` equals only itself, and repeated reads of a member share its identity. Incoming enum arguments from the same compiled script resolve to that invocation's enum identity, including references inside collections and instance fields; foreign enum aliases preserve their shared identity through independent accounting views.

An operand is evaluated before the next one runs, so a later update cannot change a value already compared:

```vibe
a = [1]
result = a == a.push(2)
[result, a] # [false, [1, 2]]
```

`eql?` and `equal?` were removed by [ADR-008](adr/008-canonical-surface-for-ai-authors.md); `==` is the one equality. `vibes fix` offers `==` as a suggestion, since `eql?` also required both sides to have the same kind at every depth and `equal?` compared other values than collections by identity.

Comparisons charge visited values, string bytes and hash lookups. Within one comparison, `==`, `<=>` and sorting walk each pair of shared arrays or hashes once: a pair reached again along another path reuses its recorded result, so structures built from shared parts compare in time proportional to their distinct pairs rather than their unfolded size. Recorded pairs are charged and reserved against the memory limit, and are released when the comparison finishes. Enum tokens and their temporary lookup cache are reserved before allocation; imports charge metadata independently. Exact step and memory limits, cancellation and uncatchable invocation exhaustion remain enforced.

Array `uniq`, `union`, `difference`, `&` and `-` index their keys in a hash table reserved against the call's memory limit, so they take time and steps proportional to their inputs. Keys hash consistently with equality: numbers by exact value, times by instant and hashes independently of insertion order. Long strings, arrays and large integers hash bounded samples; equal hashes still compare by ordinary equality, and every probe and comparison is charged.

## Float ordering

Float `<=>` returns `-1`, `0` or `1`, always an `int`, using the order of
[Go's `cmp.Compare`](https://pkg.go.dev/cmp#Compare). Every NaN precedes every
non-NaN, two NaNs compare as zero, and negative and positive zero compare as
zero. Infinities retain their numerical order. NaN sign and payload do not
break ties.

`sort`, `sort_by`, `min`, `max`, `minmax`, `min_by`, `max_by` and float `clamp`
use that order. Sorting is stable: equal keys, including NaNs and signed zeros,
keep their input order. Extrema keep the first value among ties. Empty arrays
retain their existing nil results.

```vibe
nan = 0.0 / 0.0
order: int = nan <=> 1.0             # -1
same: int = nan <=> nan              # 0
values = [1.0, nan, -2.0].sort        # [NaN, -2, 1]
clamped = nan.clamp(0.0, 1.0)         # 0.0
```

Equality and relational predicates keep IEEE semantics: NaN is unequal even
to itself, and `<`, `<=`, `>` and `>=` with NaN are false. `between?` remains
a relational predicate and is false for NaN. Choose `<=>` when an ordering
must include every float; a sort comparator should return that integer.
