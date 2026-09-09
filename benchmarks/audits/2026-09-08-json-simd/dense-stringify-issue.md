`JSON.stringify` repeatedly walks the unchanged execution graph while escaping a string when a memory quota is enabled. Each escaped character triggers a projected-output check, and the builtin currently disables the estimator's memoization.

On `e6f1fca90c012885453a6c413c0826f6abff1f32`, Go 1.27.1, Apple M4, a `Script.Call` rendering an object with a 64 KiB string took approximately **9.0 ms** for the repeated pattern `a\n\t"\\`. A plain ASCII string of the same input length took approximately **77.9 µs** in the final baseline benchmark. These are diagnostic measurements, not a measured fix. The dense case's CPU profile attributed **58.01% cumulative CPU to `memoryEstimator.env`**; the output is only about 1.8 times larger.

The source path is explicit:

- [`engine.go:691`](https://github.com/xipkit/vibescript/blob/e6f1fca90c012885453a6c413c0826f6abff1f32/internal/runtime/engine.go#L691) registers `JSON.stringify` without a nonmutation declaration.
- [`call.go:321`](https://github.com/xipkit/vibescript/blob/e6f1fca90c012885453a6c413c0826f6abff1f32/internal/runtime/call.go#L321) therefore counts the call in `undeclaredBuiltinDepth`; [`memory.go:566`](https://github.com/xipkit/vibescript/blob/e6f1fca90c012885453a6c413c0826f6abff1f32/internal/runtime/memory.go#L566) bypasses the cached graph walk.
- [`appendJSONString`](https://github.com/xipkit/vibescript/blob/e6f1fca90c012885453a6c413c0826f6abff1f32/internal/runtime/json.go#L923) calls `checkOutputBytes` for each escape, which calls `checkProjectedStringBytes` and opens another graph walk.

Audit whether `JSON.stringify` can declare nonmutation using the existing contract, keeping every output projection and step charge in place. Its current rendering paths read the input and build fresh output; they do not invoke user callbacks. This would let ordinary calls reuse the existing estimator cache. Calls beneath an undeclared outer builtin would still need the conservative fallback.

Acceptance: benchmark dense escapes with memory quotas enabled, verify estimator visits no longer multiply by the number of escapes for ordinary calls, and preserve exact output, step charges, tight memory thresholds, error precedence, and cancellation. Run the builtin-contract and accounting oracles. This is separate from the closed allocation-churn issue #499 and containment issue #680; it is a specific remaining estimator hot path after #197.
