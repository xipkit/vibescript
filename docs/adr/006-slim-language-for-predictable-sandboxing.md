# ADR-006: Slim the language for predictable sandboxing

## Status

Accepted - 2026-08-15

## Decision

Before 1.0, we will remove script-visible features whose primary benefit is
general-purpose Ruby ergonomics but whose implementation requires hidden
scheduling, escaping executable state, shared mutable collection identity, or
implicit behavior injection. A host-controlled `Script.Call` will remain the
unit at which script execution is scheduled and budgeted; script code will not
create child executions or request idle delays. Script dataflow and authority
will remain explicit in source.

Specifically, Vibescript will make these changes:

1. **Hosts own concurrency and delay.** Remove the `Tasks` namespace, task
   managers and handles, `Tasks.run`, `Tasks.map`, `tasks.spawn`, `tasks.wait`,
   and `task.value`. Remove script-visible `sleep` and the configuration and
   quota-profile fields dedicated to tasks and sleeping. A host may run
   independent `Script.Call` invocations concurrently, expose a bounded batch
   operation as a capability, or schedule delayed work in its job system.
2. **Arrays and hashes have value semantics.** Binding or passing a collection
   creates another logical value. Updating one binding or path cannot change a
   sibling alias. An operation may update an addressable receiver such as a
   local, instance variable, or nested path by rebinding that root; it cannot
   mutate a temporary or another alias. Non-mutating transformations return new
   values. Bang-named variants that duplicate a non-bang transformation will be
   removed. The runtime may use persistent storage or copy-on-write internally,
   but sharing is not observable and does not create collection identity in the
   language. `==` remains content equality for collections.
3. **Hashes use one string keyspace.** Strings and symbols are the only accepted
   hash-key inputs, and symbols normalize to strings. `hash["name"]`,
   `hash[:name]`, and the literal label `name:` address the same entry; `keys`
   returns strings. Other key types are rejected. Hashes preserve insertion
   order, missing `[]` lookups return `nil`, and explicit `fetch` forms provide
   fallbacks. Per-hash default values and default procs are removed. Hash values
   may still be any supported Vibescript value; this decision does not require
   every hash to be JSON-serializable.
4. **Executable code does not escape.** Remove proc and lambda constructors,
   stabby-lambda literals, first-class function and bound-method values,
   callable `.call`, block capture and forwarding with `&`, and symbol-to-proc.
   Named functions and methods remain directly callable. A block remains legal
   as syntax attached to a call and may be invoked with `yield`, but it must run
   synchronously and cannot be stored, returned, or invoked after its receiving
   call returns. Capability methods may be called directly but cannot be
   detached as callable values.
5. **Modules are namespaces, not behavior injection.** A source module may hold
   constants, nested modules, and `def self.name` functions. Remove `include`
   and `extend`, instance-style module methods, copied methods/accessors/
   visibility/constants, and the type and introspection relationships created
   by module inclusion. Classes otherwise remain in the language and continue
   to have no inheritance.

Ruby compatibility is not sufficient reason to restore one of these features.
A future proposal that introduces another scheduler, independently retained
execution state, shared mutable identity, implicit dispatch source, or dedicated
quota family must demonstrate a Vibescript application need and define its
sandbox and accounting model in a separate ADR.

This ADR supersedes [ADR-001](001-tasks-structured-concurrency.md).

## Resulting Language Shape

Collection updates affect the addressed value, not its aliases:

```vibe
first = [1, 2]
second = first
first[0] = 9

first  # [9, 2]
second # [1, 2]
```

Hash label and symbol syntax are conveniences over one string keyspace:

```vibe
person = { name: "Ada" }

person["name"] == person[:name] # true
person.keys                     # ["name"]
```

Reusable developer behavior stays in its namespace rather than becoming an
implicit method source:

```vibe
module Naming
  def self.display_name(person)
    "I am " + person.name
  end
end

Naming.display_name(person)
```

Blocks remain concise but do not become values:

```vibe
names = people.map { |person| person.name } # valid and synchronous
mapper = ->(person) { person.name }          # compile error
```

## Context

Vibescript is an embedded workflow language. Its primary programs receive host
and JSON-shaped data, query through capabilities, transform values, and publish
or enqueue results. It is not intended to reproduce every facility of a
general-purpose Ruby runtime.

The host is Go and already controls process-wide concurrency. Concurrent
`Script.Call` invocations use separate execution state, so a host can place
parallelism where its goroutine, pool, tracing, cancellation, and rate-limit
tooling already applies. Script-visible tasks instead create nested execution
trees inside one sandbox boundary. The runtime must then define sibling failure,
inline execution when a pool is exhausted, nested cancellation, task-handle
lifetime, capability inheritance, and quota ownership across goroutines. The
central task implementation is roughly 1,700 lines before its dedicated tests
and has required repeated hardening for nested-task races and live-memory
accounting.

Current repository usage does not demonstrate broader demand for that surface.
`Tasks` appears in the dedicated task example, while the other shipped `.vibe`
examples do not use mixin directives or first-class callable constructors. The
in-repository evidence is feature demonstration rather than an application
requirement.

`sleep` has a similarly small language benefit and a large accounting contract.
A bounded sandbox must charge wall-clock delay across nested calls and tasks,
prevent fanout from multiplying the allowance, propagate parent limits, handle
refunds, and add another limit to every quota profile. During that time a script
holds a host goroutine while doing no work. Workflow hosts already have timers,
queues, and delayed jobs with lifecycle outside an interpreter call.

Ruby-style mutable collection identity makes the memory boundary graph-based
rather than value-based. Aliases must observe mutation, so the runtime boxes
every array and hash, tracks identity and mutation epochs, deduplicates shared
backing storage, detects cycles, accounts retained capacity after shrinking,
and detaches projections that would otherwise keep large receivers alive. Every
new mutator must also declare and implement the right invalidation behavior.

Arbitrary hash keys multiply that graph problem. Composite keys require
recursive canonicalization, equality, hashing, cycle detection, and accounting;
separate string and symbol keyspaces also make ordinary JSON round-trips change
lookup behavior. Most application hashes are records or JSON-shaped objects,
where one ordered string keyspace is the expected model.

First-class callables retain code together with lexical environments, receiver
state, control-flow semantics, and potentially host capabilities. Bound methods,
block forwarding, proc/lambda differences, and callbacks invoked after their
creating frame returns make lifetime, static call resolution, and boundary
accounting materially harder. Synchronous nonescaping blocks provide the useful
enumeration syntax without creating that retained executable graph.

Mixins are expanded at compile time rather than through runtime inheritance,
but they still introduce hidden method and constant sources, collision order,
transitive membership, visibility copying, nominal type relationships, and
accounting for adopted state. Explicit namespace calls provide the same code
reuse while keeping the dependency visible.

Vibescript is still pre-1.0. Its versioning contract permits breaking changes in
minor releases while the language and embedding API are being finalized. This
is the least expensive point to choose a smaller semantic contract; after 1.0,
removing these features would require a major release and a substantially longer
migration period.

## Consequences

Easier:

- A call has one execution and quota tree. Cancellation, memory settlement, and
  capability lifetime no longer cross script-created goroutine boundaries.
- Step, memory, and recursion remain the core sandbox budgets; sleeping and task
  fanout no longer require independent or inherited allowances.
- Arrays and hashes can be accounted as logical values without preserving
  alias-visible collection mutation or script-visible collection identity.
  Persistent or copy-on-write storage can optimize snapshots without becoming
  language semantics; representation-level sharing may still be measured
  internally.
- Hash lookup, equality, estimation, JSON interoperability, and host-boundary
  conversion operate on one canonical key representation.
- Call targets, capability use, and module dependencies remain visible at the
  source location where they occur. The checker does not need to model escaping
  closures or injected module membership.
- The runtime and standard library have fewer combinations of mutation,
  callbacks, task failure, cancellation, and quota inheritance to test and
  secure.

Harder, and what we now owe:

- Scripts cannot dynamically fan out arbitrary function calls. Hosts that need
  this must parallelize calls or expose a purpose-built bounded batch
  capability, including its own aggregate rate and resource limits.
- Scripts cannot pause an invocation to model delayed workflow steps. Hosts must
  persist and schedule those steps outside the interpreter.
- Ruby ports using aliases, in-place mutators, arbitrary hash keys, hash
  defaults, callbacks, callable values, or mixins require mechanical rewrites.
  Code that relied on mutation crossing an alias or function boundary must pass
  or return the updated collection, and injected methods become namespace
  calls.
- Efficient value semantics require an immutable, persistent, or copy-on-write
  collection representation. A naive deep copy on every binding would be
  correct but could make common transformations unnecessarily expensive.
- Nonescaping blocks need an enforceable host contract. A capability that
  accepts a block must finish every invocation before returning and may not
  retain the block or captured execution state.
- Classes and host objects can still carry identity and cycles. This decision
  substantially reduces reachable-graph complexity but does not eliminate the
  need for bounded traversal or identity handling everywhere in the runtime.
- Concurrent host calls can still exceed aggregate host resources if the host
  launches them without bounds. Moving scheduling out of the language makes
  that policy explicit; it does not provide the policy automatically.
- The implementation must remove syntax, runtime behavior, public `Config`
  fields, checker rules, LSP support, examples, and documentation together. The
  migration guide and release notes must describe replacements before the first
  release containing the removals.

## Non-goals

- Removing classes, enums, shapes, named functions, direct method calls, or
  ordinary control flow.
- Removing synchronous blocks used by enumeration helpers or capability methods.
- Requiring hash values, arrays, classes, `Money`, `Time`, or other values to be
  JSON-serializable. "JSON-shaped" applies only to the hash key model.
- Prohibiting hosts from using goroutines, concurrent `Script.Call` invocations,
  batch capabilities, timers, or durable job systems.
- Mandating a particular internal collection representation.
- Changing the gradual-typing rule established by
  [ADR-004](004-static-checking-for-typed-boundaries.md): known contradictions
  remain errors and unknown host or JSON data remains runtime-checked.

## Alternatives Considered

### Keep the surface and continue hardening it

Rejected. The difficult cases are consequences of the public semantics, not
isolated implementation mistakes. More tests and accounting patches can harden
individual paths, but every combination of nested tasks, aliases, callbacks,
mutators, and injected methods remains part of the permanent sandbox contract.
The application value observed so far does not justify that continuing cost.

### Keep only `Tasks.map`

Rejected. Ordered concurrent mapping is the most attractive task operation, but
it still creates sibling executions with inherited capabilities, cancellation,
failure, and quotas. A host-side bounded batch operation can provide the same
application behavior with a domain-specific resource contract.

### Preserve Ruby reference semantics behind copy-on-write storage

Rejected as a semantic compromise. Copy-on-write is useful only when mutation
through one binding does not affect another. If aliases must observe the same
mutation, storage remains shared and the runtime still owes identity,
invalidation, retention, and cycle accounting. Copy-on-write remains available
as an implementation of the chosen value semantics.

### Canonicalize hash keys only during JSON conversion

Rejected. String/symbol collisions and arbitrary composite-key accounting would
remain everywhere else, and parsing then stringifying ordinary application data
would still cross between two key models. One language-wide keyspace is simpler
than a special JSON exception.

### Keep callable values but prohibit capability capture

Rejected. Capture restrictions would add an effect and escape-analysis system
while bound receivers, stale frames, indirect targets, and callable containers
would remain. Nonescaping blocks preserve the common ergonomic use without
requiring executable values.

### Remove classes as part of this decision

Not chosen. Mutable class instances retain some identity and graph complexity,
but removing classes is a separate product and language decision. This ADR does
not need that larger cut to remove script scheduling, shared collection
mutation, escaping callables, or mixin injection.

## Links

- Superseded decision:
  [ADR-001: Add Tasks for bounded structured concurrency](001-tasks-structured-concurrency.md)
- Related quotas:
  [ADR-002: Named quota profiles and an `xhigh` CLI default](002-cli-quota-profiles.md)
- Related checking rule:
  [ADR-004: Infer local types and check typed boundaries
  statically](004-static-checking-for-typed-boundaries.md)
- Current language surfaces: [Blocks](../blocks.md), [Classes](../classes.md),
  [Hashes](../hashes.md), and [Integration](../integration.md)
- Compatibility policy: [Versioning and Compatibility Contract](../versioning.md)
