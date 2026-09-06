# Integrating Vibescript in Go

The interpreter runs entirely in Go. Create an engine, compile scripts, and
call functions like so. For API lifecycle guarantees, also see
`docs/versioning.md` and `docs/deprecation_policy.md`. For production
integration playbooks, see `docs/host_cookbook.md`. For copy-friendly starter
scaffolds, see `docs/starter_templates.md` and `templates/`.

```go
package main

import (
    "context"
    "fmt"

    "github.com/mgomes/vibescript/vibes"
    "github.com/mgomes/vibescript/vibes/value"
)

func main() {
    engine, err := vibes.NewEngine(vibes.Config{})
    if err != nil {
        panic(err)
    }

    scriptSource := `
    def total_with_bonus(base, bonus)
      base + bonus
    end
    `

    script, err := engine.Compile(scriptSource)
    if err != nil {
        panic(err)
    }

    result, err := script.Call(
        context.Background(),
        "total_with_bonus",
        []value.Value{value.NewInt(100), value.NewInt(25)},
        vibes.CallOptions{},
    )
    if err != nil {
        panic(err)
    }

    fmt.Println("result:", result.Int())
}
```

Host applications can expose capabilities by seeding `CallOptions.Globals` with
values (hashes, builtins, arrays) or, for richer integrations, by supplying
typed adapters via `CallOptions.Capabilities`. Review
`examples/capabilities/` and the test harness in `vibes/examples_test.go` for
mocks you can repurpose.

### Quota Profiles

Each execution runs under three quotas set on `Config`: `StepQuota` (aborts
runaway loops), `MemoryQuotaBytes` (bounds retained heap, enforced by the
reachable-graph accounting), and `RecursionLimit` (bounds call depth). For each
field a **positive** value is an explicit limit, `vibes.Unlimited` disables that
quota, and a **zero** value selects the engine's conservative built-in default —
the `low` profile (1,000,000 steps / 16 MiB / 256), so `low` is the reproducible
name for the default sandbox budget. An unlimited memory quota skips the
accounting walk entirely.

Compilation separately bounds parser recursion and expression/statement tree
depth to 1,024 levels. Excessive nesting returns `syntax nesting too deep` before
execution starts. This fixed limit also applies to snippets, modules, and editor
tooling; increasing `RecursionLimit` or `MaxSourceBytes` does not raise it. Wide
arrays, argument lists, and sequences of shallow statements do not consume extra
nesting levels. Type annotations, parenless calls, and string interpolation keep
their existing, smaller nesting limits.

Rather than tune the three fields by hand, select a coherent bundle with a named
profile:

| Profile              | Step quota  | Memory quota | Recursion |
| -------------------- | ----------- | ------------ | --------- |
| `vibes.ProfileLow`   | 1,000,000   | 16 MiB       | 256       |
| `vibes.ProfileMedium`| 20,000,000  | 128 MiB      | 1,000     |
| `vibes.ProfileHigh`  | 200,000,000 | 512 MiB      | 4,000     |
| `vibes.ProfileXHigh` | unlimited   | unlimited    | 10,000    |

Apply one to a `Config` with `ApplyTo` (it writes every quota field, leaving the
rest untouched), or resolve one from a user-supplied name. Because `ApplyTo`
overwrites all three, set any per-quota override **after** applying the profile,
not before:

```go
cfg := vibes.Config{StrictEffects: true}
vibes.ProfileHigh.ApplyTo(&cfg)
engine, err := vibes.NewEngine(cfg)

// Or select by name (e.g. from a config file or flag):
if p, ok := vibes.QuotaProfileByName(userChoice); ok {
    p.ApplyTo(&cfg)
}
```

`vibes.QuotaProfileNames()` returns the names in ascending order of generosity
(`low`, `medium`, `high`, `xhigh`) for building help text or validation. The
`vibes` CLI is built on the same profiles and defaults to `xhigh`; see
[Tooling Commands](tooling.md#quota-profiles).

### Module Search Paths

Set `Config.ModulePaths` to the directories that contain re-usable `.vibe`
files. Scripts can then call `require("module_name")` to load another file.
The `require` builtin returns a namespace object containing the module's public
function names and also defines any non-conflicting public names on the global
scope for convenient direct calls. Required functions are not values: call
them through the namespace or injected name rather than trying to store or pass
them.

```go
engine, err := vibes.NewEngine(vibes.Config{ModulePaths: []string{"/app/workflows"}})
if err != nil {
    panic(err)
}

script, err := engine.Compile(`def total(amount)
  require("fees", as: "helpers")
  helpers.apply_fee(amount)
end`)
```

The interpreter searches each configured directory for `<module>.vibe` in order
and caches compiled modules so subsequent calls to `require` are inexpensive.
Parsed module requests, search results, and suggestion text share an 8 MiB
cache text limit in addition to their `Config.MaxCachedModules` entry limits.
Requests beyond that text limit still resolve normally without being cached.
`ClearModuleCache` also clears this text and resets its byte accounting.
Executable top-level statements in a required module run as a module initializer
before its exports are returned.
For long-running hosts, call `engine.ClearModuleCache()` between runs when
module sources can change. During development, set `Config.DevMode: true`
instead to revalidate modules on every `require` and reload edited files
automatically; keep it off in production.
Use `Config.ModuleAllowList` / `Config.ModuleDenyList` for policy hooks over
which modules may be loaded (`*` glob patterns against normalized module names,
with deny-list rules taking precedence).
Policy matching preserves whitespace within filename and directory components
and distinguishes extra extensions: `helper` and `helper.vibe` are equivalent,
while `helper .vibe` and `helper.json.vibe` are separate module names.
When a circular module dependency is detected, the runtime reports a concise
chain (for example `a -> b -> a`).
Use the optional `as:` keyword to bind the loaded module namespace to a global
alias.
Inside a module, use explicit relative paths (`./` or `../`) to load siblings
or parent-local helpers. Relative requires are resolved from the calling
module's directory and are rejected if they escape the module root. Functions
are exported by default; use `private def ...` for module-local helpers.
Exported names are only injected into globals when no binding
already exists, so existing host/script globals keep precedence.
Import paths are normalized across slash styles, and traversal/symlink escapes
outside configured module roots are blocked.

### Capability Adapters

Use `CallOptions.Capabilities` to install typed host integrations. The
`vibes.NewJobQueueCapability` helper wraps a host
`jobqueue.JobQueue` implementation and exposes `enqueue` (and `retry` when
supported) with automatic argument parsing and context propagation.

```go
type jobQueue struct{}

func (jobQueue) Enqueue(ctx context.Context, job jobqueue.JobQueueJob) (value.Value, error) {
    log.Printf("queue %s with payload %+v", job.Name, job.Payload)
    return value.NewString("queued"), nil
}

cap, err := vibes.NewJobQueueCapability("jobs", jobQueue{})
if err != nil {
    panic(err)
}

result, err := script.Call(ctx, "queue_recalc", nil, vibes.CallOptions{
    Capabilities: []vibes.CapabilityAdapter{cap},
})

```

Adapters receive the invocation `context.Context`, making it straightforward to
apply deadlines, tracing spans, or other host-specific policy without hand
wiring builtins.

Embedders that parse enqueue keywords directly should call
`jobqueue.ParseEnqueueOptions`. It validates `delay`/`key` and rejects any extra
keyword that is not data-only or that contains cyclic references, then
deep-clones the survivors into `JobQueueEnqueueOptions.Kwargs`. The runtime
adapter, which already enforces the data-only contract before parsing, uses the
`jobqueue.ParseEnqueueOptionsValidated` fast path so the option graph is not
walked twice; direct callers should prefer the safe `ParseEnqueueOptions`.

### First-Party Capability Helpers

Vibescript ships capability helpers for common integration points:

- `NewDBCapability(name, db)` for `find/query/update/sum/each` with
  `db.Database`.
- `NewEventsCapability(name, publisher)` for `publish` with `events.Publisher`.
- `NewJobQueueCapability(name, queue)` for `enqueue/retry` with
  `jobqueue.JobQueue`.
- `NewContextCapability(name, resolver)` for data-only request metadata with
  `contextcap.Resolver`.

```go
dbCap := vibes.MustNewDBCapability("db", myDB)
eventsCap := vibes.MustNewEventsCapability("events", myEvents)
jobsCap := vibes.MustNewJobQueueCapability("jobs", myJobs)
ctxCap := vibes.MustNewContextCapability("ctx", func(ctx context.Context) (value.Value, error) {
    userID, _ := ctx.Value("user_id").(string)
    role, _ := ctx.Value("role").(string)
    return value.NewObject(map[string]value.Value{
        "user": value.NewObject(map[string]value.Value{
            "id":   value.NewString(userID),
            "role": value.NewString(role),
        }),
    }), nil
})

result, err := script.Call(ctx, "run", args, vibes.CallOptions{
    Capabilities: []vibes.CapabilityAdapter{dbCap, eventsCap, jobsCap, ctxCap},
})
```

Capability method names follow `capability.method` naming (`db.find`,
`events.publish`, `jobs.enqueue`) so contracts and runtime errors are explicit
about the boundary being enforced.

### Capability Workflow Pattern

A practical pattern is `query -> transform -> publish/enqueue` in one script
call:

1. Query records through `db.each` or `db.query`.
2. Build a data-only payload in script code.
3. Publish notifications via `events.publish` or queue work via `jobs.enqueue`.

This keeps business logic in Vibescript while side effects stay behind typed
host adapters.

### Capability Failure Handling

Capability adapters enforce data-only boundaries and argument shapes at runtime:

- If script args/kwargs are invalid, the call fails before host code executes.
- If host returns callable values, return contracts reject them.
- Adapter errors are surfaced as runtime errors with call-site stack frames.

In host code, handle script call errors the same way as other runtime failures
and log adapter-specific method names from the error text (for example
`db.update attributes must be data-only`).

### Value Independence at the Boundary

Every collection crossing between adapter Go code and script state is
independent: arguments arrive isolated from script slots, and returns --
including values exchanged through `Execution.CallBlock` -- are detached from
any backing the adapter retains. The one deliberate exception is the
capability object itself: it is the host's live state for the duration of a
`Script.Call`, writing into it is the sanctioned factory channel, and script
reads of it see the host's current truth mid-call. Everything reachable from
it is made independent when the Call returns, so nothing host-held ever
crosses out live. Do not defensively deep-clone a return before handing it
back; the boundary already copies it, and a pre-clone just pays twice. An adapter that keeps no reference to anything it receives,
returns, or yields can declare `vibes.DeclareNonRetaining` on its builtins to
skip the boundary copies entirely, and one that never writes a script
container can declare `vibes.DeclareNonMutating` to skip argument isolation;
both are safety promises, so declare only what is true.

The DB, events, jobqueue, and context adapters preserve shared children within
each copied data graph. Positional arguments and keyword options share one
request copy. Returns and individual `db.each` rows use fresh snapshots, so a
host callback or an earlier row cannot leave a stale copy in a later result.
Jobqueue payloads retain object provenance; extra enqueue options keep their
existing behavior of stripping it.

These adapters bound copy work even when used without an interpreter. One
operation permits at most 262,144 composite-node visits, 1,048,576 value visits,
64 MiB of cumulative allocation reservations, and 67,108,864 units of traversal
and byte work. The existing nesting limit remains 256. Runtime calls also
charge their configured step and memory quotas and check cancellation while
copying. All rows in one `db.each` call share the operation budget.

### Handling Dynamic Types

Every call returns a `value.Value`. Inspect the `Kind()` before consuming it:

```go
result, err := script.Call(ctx, "handler", args, vibes.CallOptions{})
if err != nil {
    return err
}

switch result.Kind() {
case value.KindInt:
    fmt.Println("int:", result.Int())
case value.KindHash:
    fmt.Println("hash keys:", result.Hash())
case value.KindNil:
    // nothing returned
default:
    return fmt.Errorf("unexpected return type: %v", result.Kind())
}
```

Because the interpreter is dynamic, there is no compile-time guarantee about
return values—always branch on `Kind()` when you need type safety.

### Error Handling and Stack Traces

Runtime errors arrive as `*vibes.RuntimeError`, which includes a stack trace
with line and column information for debugging. Use `errors.As()` to check for
runtime errors:

```go
result, err := script.Call(ctx, "process", args, opts)
if err != nil {
    var rtErr *vibes.RuntimeError
    if errors.As(err, &rtErr) {
        // Runtime error with stack trace
        log.Printf("Script error: %v", rtErr)
        // Access individual frames if needed
        for _, frame := range rtErr.Frames {
            log.Printf("  %s at %d:%d", frame.Function, frame.Pos.Line, frame.Pos.Column)
        }
    } else {
        // Other error (compilation, etc.)
        return err
    }
}
```

Branch on `rtErr.Type` for stable programmatic handling. `LimitError`
identifies step quota, memory quota, and recursion-limit terminations without
scraping message text. Step- and memory-quota exhaustion cannot be rescued by
script code, so a quota-killed script is guaranteed to surface here rather
than looping inside a `rescue`.

Example error output:

```
assertion failed: amount must be positive
  at validate_amount (3:7)
  at validate_amount (8:3)
  at process_payment (8:3)
  at process_payment (12:5)
```

The first frame shows where the error occurred, followed by the call stack
showing where each function was called from.
