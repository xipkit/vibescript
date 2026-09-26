# Calls to a member named call

With static types, `.call(...)` is an ordinary method call: a class may define a `call` method, while a function or builtin name is never a value, so `helper.call(1)` is a compile error. The rules below describe how the runtime selects the target.

An explicit `.call(...)` resolves its member before evaluating arguments. Missing and inaccessible members stop positional arguments, keyword arguments, splats and attached blocks from running. Safe navigation on nil skips the whole call. An existing non-callable data field keeps ordinary argument evaluation before the invocation error.

A bare name receiving `call` follows Go's auto-invocation rules. `helper.call`, `helper&.call` and, for a zero-parameter function, `helper.call()` or `helper.call { }` keep the function as a value and fail with `a function has no member call; call helper(...) directly`; `helper.call(1)` runs the function first. Builtins and implicit methods behave alike, so `puts.call` reports `a method has no member call; call puts(...) directly` and a method `helper` of class `K` reports `K#helper`.

Script-defined `call` methods retain positional and keyword binding, method options rules and block control flow. Callable hash fields are selected before argument mutations: `h={call:Math::sqrt}; h.call(h.clear.length)` invokes the selected square-root helper with zero. The selected target remains accounted for until invocation finishes or unwinds.

Four native tests cover callback order, binding, nonlocal block exits, repeated cleanup, memory exhaustion and cancellation. The shared suite adds 179 reference-checked evaluations and eleven uncaught runtime rejections. Their diagnostics also match the reference wording: a value without a `call` member reports Go's lookup failure for its kind, such as `unknown int method call`, and a method kept as a value reports `a method has no member call; call Math.sqrt(...) directly`. Broader member lookup and diagnostic coverage remain part of the language port.
