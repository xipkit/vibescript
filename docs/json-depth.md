# Deep JSON values

`JSON.parse`, `JSON.stringify` and the Rust `parse_json` and `stringify_json` helpers support 10,000 nested arrays or hashes. Empty containers count as one level; scalars do not. Entering the 10,001st container raises a recoverable `LimitError`.

Valid values retain that depth allowance through host imports, global bindings, instance and module fields, rendering, comparisons and collection traversal. Adding another array or hash around a value already at the limit is rejected. Internal field tables do not consume a data-container level. Typed normalization retains its separate depth rules.

Parsing, encoding and value traversal use explicit frames charged to the invocation. Container destruction uses links reserved in container headers, so deep values and partial results can be released after cancellation or quota exhaustion without allocating a growing cleanup stack. Shared immutable values remain intact until their final owner drops them, including when owners live on different threads. The crate adds no unsafe code for these operations.

The standard limits remain one million logical steps, 16 MiB of tracked memory and 256 script call frames. Traversal frames and the additional container-header storage affect the memory counters. Reaching the supported depth does not guarantee that every operation fits an invocation's budget: full static key-transform analysis can exhaust the memory quota even when runtime execution succeeds. These resource failures remain latched and release partial traversal state.

Script JSON input and output retain the 1 MiB payload cap. Host JSON helpers use their independent call budgets without that payload cap. Syntax, type normalization and function-environment recursion limits are unchanged. Native tests exercise the documented depth using default budgets and include lowered-stack cleanup stress tests; stack settings are not increased.
