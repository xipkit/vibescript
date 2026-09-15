# Value lifecycle helpers

`clone` returns a logical data copy, like `dup`. Arrays and hashes share accounted storage until a write requires a copy. Nested updates preserve the original value:

```vibescript
a = {items: [1]}
b = a.clone
b.items.push(2)
[a, b] # [{items: [1]}, {items: [1, 2]}]
```

`freeze` returns its receiver, and `frozen?` returns true for every value. Vibescript has no mutable freeze flag; ordinary collection operations can still update their bindings. Scalars, enums, type literals, classes, instances, builtin exports and regex-offset callables expose the same helpers. Cloning a class or instance preserves its runtime identity.

The three helpers accept no positional arguments, keywords or blocks. User-defined class methods and callable namespace exports take precedence, subject to visibility. A direct hash member read resolves the helper even when a key has the same name. Brackets read the stored data. Existing addressable field-write rules still apply to mutation chains: for `h = {clone: [1]}`, `h.clone.clear` clears the stored array, while `h.clone().clear` clears a temporary copy of the hash. Use parentheses to select the helper explicitly in such a chain. Scoped namespace lookup still reads exports and constants.

These helpers take their receiver at call time. Direct `value.clone` uses `value`; wrapping the member in a computed target, such as `(value.clone rescue fallback)()`, calls it with nil. Builtin exports and regex-offset callables remain callable after a direct clone or freeze.

Match data and rescued errors retain protection and their special string rendering through clones and freeze calls, including transfer through Rust host values. A nested write such as `match.clone.captures.push("x")` is rejected. Captures assigned to a separate variable are independent collection values and can be updated. These are the selected protected-object semantics where Go's behavior is inconsistent.

Native tests cover nested copy isolation, reserved data and callable exports, protected paths, host transfer, cancellation, storage reclamation and exact work and memory limits. `clone` and `freeze` retain the same tracked storage and work counters as `dup` in the focused import test. General stored equality predicates and the rest of the unfinished standard library remain part of [the language port](language-port.md).
