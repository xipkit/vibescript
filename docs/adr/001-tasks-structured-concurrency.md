# ADR-001: Add Tasks for bounded structured concurrency

## Status

Superseded by
[ADR-006](006-slim-language-for-predictable-sandboxing.md) - 2026-08-15
(previously accepted - 2026-06-06)

## Decision

We will add a built-in `Tasks` namespace for bounded structured concurrency in Vibescript. `Tasks.run` will create a scoped task manager with automatic waiting at block exit, and `Tasks.map` will provide ordered concurrent mapping over collections.

Hosts will control task fanout through engine configuration. `Config.DefaultTaskConcurrency` will default to `4`, or to `Config.MaxTaskConcurrency` when the host cap is lower, and `Config.MaxTaskConcurrency` will cap script-provided `max:` values; requests above the cap will fail instead of being silently clamped.

## Context

Vibescript scripts increasingly need to express independent host operations, such as fetching data, sending notifications, publishing events, or scoring multiple items. Today those operations are sequential unless the host moves the concurrency outside the script.

The runtime already supports concurrent `Script.Call` invocations by constructing a fresh root environment and a fresh `Execution` for each call. A single `Execution`, however, is mutable interpreter state: it tracks stacks, receiver context, module state, quotas, capability contracts, and the active environment. Sharing one live `Execution` across concurrent script bodies would make correctness, quota accounting, cancellation, and value ownership significantly harder.

Vibescript also exposes host-controlled execution bounds through `Config`: step quota, memory quota, recursion limit, module policy, and related settings. Task concurrency should fit that model so embedders can tune scripts against their database pools, API limits, and operational budgets.

## API Shape

`Tasks.run` creates a scoped task manager:

```vibe
Tasks.run(max: 8) do |tasks|
  profile = tasks.spawn(:load_profile, user_id)
  orders = tasks.spawn(:load_orders, user_id)

  render(profile.value, orders.value)
end
```

`max:` is optional:

```vibe
Tasks.run do |tasks|
  tasks.spawn(:send_email, user)
  tasks.spawn(:record_metric, user)
end
```

The default is `Config.DefaultTaskConcurrency`, which defaults to `4` unless the host cap is lower.

`tasks.spawn(:function_name, arg1, arg2, key: value)` starts an isolated function
call and returns a task handle. `task.value` waits for that task if needed,
returning its value or raising its error. Call splats (`*args`) and keyword
splats (`**kwargs`) are not supported; pass task arguments explicitly.

`Tasks.run` always waits for all spawned tasks before returning, even when the script never calls `tasks.wait`. `tasks.wait` remains available as an explicit barrier for tasks spawned so far:

```vibe
Tasks.run(max: 4) do |tasks|
  tasks.spawn(:warm_cache)
  tasks.wait

  use_cache()
end
```

`Tasks.run` returns the block's normal value. It does not auto-collect unclaimed task values.

`Tasks.map` is the result-collecting helper:

```vibe
scores = Tasks.map(users, with: :score_user)
```

This is equivalent to spawning one task per item, waiting for all tasks, and returning task values in input order. `Tasks.map` accepts the same optional `max:` override:

```vibe
scores = Tasks.map(users, max: 8, with: :score_user)
```

## Semantics

Task groups are structured:

- Tasks cannot detach from the enclosing `Tasks.run` or `Tasks.map` scope.
- Task handles cannot be used after their scope exits.
- Exiting a task scope waits for every spawned task.
- If the task scope body raises, outstanding tasks are canceled and awaited before the original error is re-raised.
- If a task fails, queued tasks are canceled, running sibling tasks receive cancellation through the parent context, and the task error is reported through `task.value` or at scope exit.
- `Tasks.map` returns results in input order, not completion order.

Task execution is isolated:

- Each spawned task runs a named function through a fresh per-call execution state.
- Spawned tasks inherit the parent call's capabilities, globals, strict-effects policy, module policy, and cancellation context.
- Spawned tasks do not share mutable local variables or captured block state with the parent execution.
- V1 does not support arbitrary block fibers such as `tasks.spawn do ... end`.

Task concurrency is host-controlled:

- `DefaultTaskConcurrency <= 0` means use the runtime default of `4`, or `MaxTaskConcurrency` when the host cap is lower.
- `MaxTaskConcurrency <= 0` means use the runtime default cap of `64`.
- Script-provided `max:` must be an integer greater than or equal to `1`.
- Script-provided `max:` greater than `Config.MaxTaskConcurrency` raises a runtime error instead of being clamped.

## Non-goals

- No detached background tasks.
- No shared-execution fibers.
- No block-capturing task bodies in V1.
- No silent clamping of script-provided concurrency.
- No scheduler or runtime abstraction exposed as `Fiber`.

## Consequences

This makes common concurrent host workflows ergonomic while keeping concurrency bounded and scoped. Scripts can express fanout locally, hosts retain operational control, and tests can use deterministic synchronization for task scheduling and cancellation behavior.

The design also keeps the first implementation aligned with current runtime boundaries: a task is an isolated function call, not a concurrent mutation of one `Execution`.

The tradeoff is that V1 is less expressive than arbitrary fibers. Scripts cannot spawn closures that mutate local variables, and functions intended for task execution must be named. Hosts and runtime maintainers also need to define precise error aggregation, cancellation, task-handle lifetime checks, and quota behavior across child executions before implementation.

Rejecting over-cap `max:` values may surprise users who expect clamping, but it preserves explicit performance behavior and avoids hiding misconfigured scripts.

## Alternatives Considered

### Require explicit `tasks.wait`

Rejected. Requiring `wait` makes leaked or forgotten work easy. Automatic waiting at scope exit is the core structured-concurrency guarantee.

### Auto-collect task values from `Tasks.run`

Rejected. `Tasks.run` should return the block value so side-effect and value-producing task groups behave predictably. `Tasks.map` is the explicit result-collecting API.

### Use `tasks.call` for value-producing work

Rejected. Having both `spawn` and `call` makes the task API harder to learn. `spawn` always starts work and returns a handle; `task.value` is the value boundary.

### Implement arbitrary fibers first

Rejected for V1. Arbitrary fibers would require sharing or cloning live execution state, captured locals, block environments, and mutable values across concurrent execution. That is a larger runtime design than bounded structured task calls.

### Silently clamp script-provided `max:`

Rejected. Clamping hides script behavior changes and makes performance harder to reason about. A runtime error gives the script author and host an explicit configuration mismatch to resolve.
