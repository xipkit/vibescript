# Calls to a member named call

An explicit `.call(...)` resolves its member before evaluating arguments. Missing and inaccessible members stop positional arguments, keyword arguments, splats and attached blocks from running. Safe navigation on nil skips the whole call. An existing non-callable data field keeps ordinary argument evaluation before the invocation error.

Script-defined `call` methods retain positional and keyword binding, method options rules and block control flow. Callable hash fields are selected before argument mutations: `h={call:Math::sqrt}; h.call(h.clear.length)` invokes the selected square-root helper with zero. The selected target remains accounted for until invocation finishes or unwinds.

Four native tests cover callback order, binding, nonlocal block exits, repeated cleanup, memory exhaustion and cancellation. The shared suite adds 179 reference-checked evaluations and eleven uncaught runtime rejections. Fifty-five cases agree on error class and output order while retaining differing diagnostic wording, recorded in [the diagnostic observations](call-member-differences.json). Broader member lookup and diagnostic coverage remain part of the language port.
