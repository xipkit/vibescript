# Copies

`dup` returns a logical copy of a value. Arrays and hashes share accounted storage until a write requires a copy, so a nested update through the copy leaves the original unchanged:

```vibe
a = { items: [1] }
b = a.dup
b["items"].push(2)
[a, b] # [{items: [1]}, {items: [1, 2]}]
```

Collections already have value semantics, so binding one to a second name makes the same independent copy; `dup` makes it explicit. For a class instance `dup` returns the same object, since instances have identity. Scalars, enums, type literals, classes and builtin exports return themselves.

Match data and rescued errors keep their protection and special string rendering through `dup`, including transfer through Rust host values: a nested write such as `m.dup.captures.push("x")` is rejected. Captures assigned to a separate variable are independent collection values and can be updated.

`dup` takes no arguments, keywords or block. User-defined class methods named `dup` take precedence, subject to visibility. It retains the same tracked storage and work counters as an ordinary binding.

`clone`, `freeze` and `frozen?` were removed by [ADR-008](adr/008-canonical-surface-for-ai-authors.md): `clone` was a second spelling of `dup`, and with no mutable freeze flag `freeze` returned its receiver and `frozen?` always answered true. `vibes fix` rewrites them. Until the switchover, a script compiled without static types can still call them.
