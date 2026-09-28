# Empty hash construction

`Hash.new` was removed by [ADR-008](adr/008-canonical-surface-for-ai-authors.md) (V0411). An empty hash is the literal `{}`, which takes its type from a declaration, a typed parameter, return or field, or an element of a typed collection:

```vibe
counts: hash<string, int> = {}
counts["draft"] = 2
counts.fetch("done", 0) # 0
```

Hashes carry no default value: a missing key reads as `nil`, so `counts["done"]` has type `int?`, and `fetch(key, fallback)` or its block form supplies a fallback for one lookup. A new hash has the same logical value semantics as any other: changing one binding leaves an earlier copy unchanged, and each empty hash's storage is accounted before allocation.

`Hash.new` is removed (V0411), and `vibes fix` rewrites it as `{}`. Its [difference record](hash-new-differences.json) remains part of the `compatibility` golden corpus.
