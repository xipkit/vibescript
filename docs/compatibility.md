# Differences from Go v0.70.0

This page records historical port decisions. Its examples include earlier spellings and dynamic call forms that the current checker rejects; use the [language guide](language.md) when writing new scripts.

This implementation began as a port of Go Vibescript v0.70.0 (`5cba216c33bea8890787d64efb2ab926a761fb1b`) and matched it except for the differences below, each selected deliberately where Go contradicted Vibescript's documented value semantics or behaved inconsistently. Since 2026-09-24 this implementation is the reference: the language moved to the static types of [ADR-007](adr/007-static-types.md) and the canonical surface of [ADR-008](adr/008-canonical-surface-for-ai-authors.md), and the Go implementation is deprecated and keeps the ADR-004 language. The `compatibility` [golden corpus](../tests/golden/README.md) retains the recorded cases, including their current static rejections, from [compatibility-cases.json](compatibility-cases.json), the fixture generators' policy cases and the difference records next to the topic pages.

## Documented value semantics take precedence

[ADR-006](https://github.com/xipkit/vibescript/blob/v0.70.0/docs/adr/006-slim-language-for-predictable-sandboxing.md) defines arrays and hashes as logical values whose shared storage is not observable, and the [standard-library contract](https://github.com/xipkit/vibescript/blob/v0.70.0/docs/stdlib_core_utilities.md) says mutations update the named binding or path while other values remain unchanged. Where Go v0.70.0 exposed shared storage, this implementation follows the documentation, as selected on 2026-09-13.

An evaluated operand or argument retains its value, and adding an unused alias cannot change a result. For `a=[1]; x=a+a.push(2); [x,a]` the result is `[[1,1,2],[1,2]]`: the left operand stays `[1]` while the right one runs, where Go returned `[[1,2,1,2],[1,2]]`. Likewise `a = [[1]]; a[0] += a[0].push(2); a` returns `[[1,1,2]]`, not Go's `[[1,2,1,2]]`. [value_semantics.rs](../tests/value_semantics.rs) checks these cases with and without an extra alias.

## Captured array positions

When an argument or block appends to an array, a pending mutation through a valid negative index updates the originally selected element: `a=[[1]]; x=a[-1].push((while true; a.push([9]); break 2; end)); [x,a]` returns `[[1,2],[[1,2],[9]]]`. The block-fill variant likewise updates the original child while preserving appended siblings.

Compound and logical indexed assignments retain the position selected by their initial read. Plain assignment still evaluates its right-hand side before selecting the target, and custom index methods receive the original selector values. Replacing the parent binding or selected child detaches a pending mutator from that binding. Slices remain temporary collection values. Nested property types still apply, and rejected writes preserve mutations already completed by an argument or block.

## Mutation during collection iteration

Iteration and the normal result of a `for` expression use an immutable snapshot of the collection, so writes to the surrounding local during the loop leave it unchanged. This returns `[[1,2],[1,2,3,3]]`:

```vibe
a = [1, 2]
x = for value in a
  a.push(3)
end
[x, a]
```

In Go the result depended on which writes detached its shared storage: with no alias this returned `[[1,2,3,3],[1,2,3,3]]`, and adding `b = a` before the loop returned the snapshot.

Builtin block iteration traverses the same snapshot: `a=[1,2,3]; a.map {|v| a[1]=9; v}` returns `[1,2,3]`, where Go returned `[1,9,3]`. The same holds for hash `each`, whose result is the original snapshot, for filters, for the key blocks of `sort_by`, `min_by` and `max_by`, for `slice_when` and `chunk_while`, for `merge` conflict blocks and for `deep_transform_keys`: callback writes never reach values already captured. Hash filters remove the selected keys from the current captured hash, preserving callback writes to other keys, and comparator-form `sort` copies its input before calling the block.

## Module collection values

Module aliases share state within an invocation, but their array and hash fields remain logical values. If a method returns a module array and a later call appends to that module variable, the earlier result stays unchanged. If a local array is assigned to a module field, a later indexed field write leaves the local array unchanged. Go v0.70.0 exposed both writes through the earlier values.

## Collections returned by index methods

An index getter returns a logical collection value, so a nested write to that temporary leaves both the stored collection and an earlier snapshot unchanged. In the `class_index_getter_snapshot` case, a grid backed by a hash stores `[1]`, saves the getter result and writes `grid[0,0][0] = 8`, and returns `[[1],[1]]`; Go returned `[[8],[8]]`.

## Writes through bare field names

In the ADR-004 language, `rows[0] = 9` in a class method with no local or method named `rows` wrote the `@rows` field, as in Go v0.70.0, but isolated the write as `@rows[0] = 9` does, so a snapshot saved earlier with `saved = @rows` keeps its value; Go changed the snapshot too. With static types a bare name must be a local or a function in scope (V0201), so the field is always written as `@rows[0] = 9`, which [value_semantics.rs](../tests/value_semantics.rs) checks.

## Inclusive range endpoints

An inclusive `for` range stops at its endpoint, even at the largest or smallest 64-bit integer: `for n in max..max` visits `max` once. The iteration position and length are kept in a wider integer so the endpoint cannot wrap. Go's counter wrapped past the endpoint and kept going.

## Hash-loop break results

An expression-valued hash loop returns its break value, as array, range and while loops do:

```vibe
counts: hash<string, int> = { a: 1 }
x = for key, value in counts
  break 7
end
```

`x` is `7`, and a bare `break` gives `nil`. Go returned the hash in both cases.

## State isolation across host calls

Mutable class and module state is isolated at every `Script.call` boundary, including calls into another compiled script or engine, as selected on 2026-09-15. Go v0.70.0 isolated a returned source class or module when it reentered its original compiled script, but let a different script mutate the original live state.

Imported instance fields preserve their values, shared references and cycles within the receiving call, while class and source-module declarations initialize fresh invocation state. Mutations cannot change the source value or another concurrent call, and foreign code uses the receiving call's accounting, cancellation and module policy. Cross-script namespace and instance dispatch run the original compiled code and host callbacks on the receiving VM stack. Source-program globals and class and module initializers start fresh, while imported instance graphs keep their contents. Required-file exports keep their private state, which is copied at each receiving call boundary.

## Host binding precedence

Lookup and assignment go through the nearest existing binding, as selected on 2026-09-16. Parameters, module constants and enclosing initializer locals take precedence over root data bindings, including host globals, classes and builtins, and named calls keep their declared script-function and module-method dispatch when a constant has the same name. For example, `module M; Parser=JSON[:parse]; def self.apply; Parser("3"); end; end` returns `3` through `M.apply` even when a host global named `Parser` contains `nil`; Go tried to call that `nil`.

A block assignment also updates an existing host binding unless a nearer local shadows it. With a host global `count=9`, `[1].each { count += 1 }; count` returns `10`, for compound and ordinary assignments alike; Go created an uninitialized block local and raised an addition error. Explicit block parameters keep their separate scope. The host-global generator in [fixtures.py](../scripts/fixtures.py) records these rules across named, computed, splat and bare calls, with strict effects enabled and disabled.

## Required-file values and lookup errors

When a required file's private array is returned and later mutated by another call, the earlier result stays unchanged; Go exposed the mutation through both results. A lookup error, such as accessing an unexported module member, can be rescued at the expression that raised it, where Go deferred it past that handler to a caller's. The [file-module fixture generator](../scripts/module_fixtures.py) records both across production and development mode and ordinary and strict effects.

## Attached capability methods and host blocks

Capability methods stay attached, following ADR-006, as selected on 2026-09-16. `sms.send(...)`, `sms::send(...)` and `sms[:send](...)` are calls; reading, storing, passing or returning the method by itself is rejected. Capability namespaces can still be copied or aliased within their invocation, but a saved namespace cannot grant its methods to a later invocation. Go permitted indexed and scoped extraction. See [host capabilities](capabilities.md).

A pending `break` or nonlocal `return` from a host block survives a callback that ignores `ErrorKind::ControlFlow`, and no later invocation of that block can execute script. Go let the callback swallow the signal, rerun the block and replace its result. Ordinary block exceptions remain available for host recovery and repeated invocation.

## Capability receiver publication

A builtin callback that receives a capability publishes into the binding that holds it, following ADR-006's collection value semantics, as selected on 2026-09-22. `HostCall::set_receiver_field` writes to the capability or method-bearing global binding that holds the receiver, so that name and its nested hashes observe the write, while copies the script took earlier stay unchanged. Only block-capable and async methods receive the handle, publication cannot replace a method field, and fields cannot be deleted. Go passed the capability object as a live `receiver` map that every alias observed. The documented pattern, where a factory method installs data and the script then reads it through the capability, behaves the same in both.

## Builtin descriptors

Required script functions remain callable through their module but cannot be extracted, stored, passed or returned as executable values, following ADR-006: indexed and scoped export reads such as `m[:fn]` and `m::fn` are rejected outside immediate call-target syntax, where Go permitted them. Collections containing extracted functions are rejected before computed-call arguments can produce effects. See [required files](require.md).

A stateless builtin descriptor, which captures no script frame, can still be obtained through indexed or scoped namespace access, as in Go: `f = Math::sqrt; f(9)` or `f = Math["sqrt"]; f(9)`. Ordinary reads of non-auto-invoking builtin bindings are rejected, and JSON cannot encode a builtin descriptor.

## Compiling top-level statements

`Engine::compile` accepts a source with top-level statements, and `Script::run` executes them as the script's entrypoint. Go's `Engine.Compile` rejected them (`unsupported top-level statement`), leaving them to `CompileSnippet`; the `vibes` commands behaved the same in both, because the Go CLI compiled scripts with `CompileSnippet`.

## Out-of-range float calendar fields

A float calendar field or `Time.at` argument that does not fit a 64-bit integer saturates on every platform, so `Time.utc(2024, 1e100, 1)` normalizes from the largest month and a script's result does not depend on the host CPU, as selected on 2026-09-22. Go's conversion depended on the CPU: arm64 saturated, while amd64 wrapped to the smallest integer and `Time.at(9.223372036854776e18)` raised.

## Host signature boundaries

Published signatures follow the documented runtime type contract and the binding rules above. Named types resolve through the active source before the call root, including required-file defaults and qualified file aliases; Go could substitute a same-named root enum or class and fail an otherwise valid call, or fail to find a file's type alias.

An absorbed block `break` is validated against the host signature's result type, as for custom capability return contracts, so `break "bad"` cannot escape a declared `int` result as it could in Go. Nonlocal returns continue to validate at their defining script function.

## Regex namespace anchors

`Regex.match`, `Regex.replace` and `Regex.replace_all` honor anchors: `Regex.match("(?:^a$)", "ab")` returns nil, as `"ab".match?("(?:^a$)")` is false, and the replacement helpers leave text that the complete expression does not match. Go's helpers took a literal-prefix shortcut that dropped the anchors, returning `"a"`.

Global substitutions keep the original subject as the context for assertions, so `"a b aa".gsub(/\b/, "X")` returns `"XaX XbX XaaX"`, with templates and blocks alike. Go repaired suffix searches with a one-byte preceding window, which skipped some word boundaries and returned `"Xa Xb XaaX"`.

## Match-data protection

Match data's protected fields survive nested writes, duplication and temporary results: `m.captures.push("x")` and clearing `m.dup` are rejected, block mutators reject protected paths before running callbacks, and a duplicate keeps the original match's string rendering. Copying the captures into a separate variable produces an ordinary array that can be changed independently. A replacement block that returns match data renders the whole match. Go allowed some of these writes, dropped the protection and the rendering in its deep clone, and rendered `<object>` in both places.
