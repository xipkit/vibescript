# The typed VM

Static types are always on (ADR-007), so the compiler knows the type of every value, and the VM uses that knowledge: it skips runtime checks the checker has proven, binds builtin members to receivers whose type is known, runs blocks without allocating, and shares the keys of records. Behavior is unchanged in every case: the golden corpora record the same observations before and after. Step counts drop only where the VM no longer executes an instruction or a check, and each such change re-recorded the counters (see [the counter log](../tests/golden/README.md#counter-log)).

This page describes the design. The implementation is in `src/bytecode.rs`, `src/vm.rs`, `src/vm/simple.rs`, `src/members/direct.rs` and `src/records.rs`.

## Proven checks

A typed boundary between two well-typed parts of a program is proven when it compiles (ADR-007), so the VM does not check it again:

- the arguments of a call from script code to a script function, method, block or required file's function;
- the result of a function or class method;
- the value stored in a typed local, a `yield` argument and a block's result.

A function whose parameters are all required and positional has a *proven start*: the instruction after its prologue's parameter checks. A call from script code binds its arguments straight into the parameter slots and starts there, without building an argument list; this is most of what made calls three times faster.

The runtime keeps a check where it does more than the checker proves (`Type::unproven`):

- **Host entry.** Arguments to `Script::call`, declared globals and capabilities, `JSON.parse_as`, `as` casts and capability results are dynamic data. The entry call runs its function's prologue, which checks every argument.
- **Named types.** A type that names a class or enum resolves at runtime, and an enum parameter turns a symbol literal into its member, so the check is a conversion.
- **Hash key types.** The checker admits a hash type whose key type no string satisfies, such as `hash<int, any>`, which only the runtime rejects.
- **Instance method results.** A class property that no path assigns reads as `nil` whatever its declared type; the checker does not report it. The result check of instance methods and accessors keeps turning that `nil` into a type error where it surfaces.

Instance variable writes and property setters keep their checks.

## Direct builtin calls

A member call dispatched by name at runtime: calls to members that can iterate, such as `fetch` and `include?`, built an argument list and passed through the iteration, capability, export and keyword checks, and every call then probed about twenty member tables, twice, before reaching the builtin.

The checker records the static base type of each member call's receiver when it has exactly one: `hash` (dictionaries and shapes), `array` (arrays and tuples), `string`, `int` or `float`. When that base serves the member called, with no block, splat or keywords, the compiler emits `Op::Direct` instead of the general call sequence. `members::direct::serves` lists the members: `length`, `empty?`, `fetch`, `key?`, `value?`, `keys`, `values`, `first`, `last`, `sum`, `join`, `include?`, `start_with?`, `end_with?`, `bytesize`, `abs`, `even?` and `odd?`.

`Op::Direct` checks the receiver's runtime kind first. A big integer, a host object, whose fields take precedence over hash members, or a rescued error or match data takes the dynamic path exactly as before. A unit test compares every direct member with dynamic dispatch on well-typed arguments, including the steps and bytes each charges.

Receivers typed `any` or a union keep the dynamic path, and so do calls of script methods: their resolution by name also enforces visibility and nominal receivers, and it is not yet a measured cost.

## Blocks

A block's arguments stay on the operand stack, below its own values, instead of in a list the block's frame owned; `BlockArg` reads them there. The simple loop runs block prologues (`Shadow`, `BlockArg`) and follows captured locals out to the frame that binds them, charging each frame it passes as the general path does. Frames are 32 bytes smaller as a result.

## Records

A shape's runtime value, a record, is an ordinary hash. It keeps insertion order, and converts to `hash<string, V>`, crosses host boundaries, serializes to JSON, compares, iterates and prints as any hash does, with the same accounting. Records are compact because their keys share storage:

- Every string and symbol literal of a program takes a slot, one per distinct text (`Program::shared`). A call imports a slot on its first use and shares that value afterwards (`Op::Shared`), charging the step an import charges each time. Every record a literal builds, and every key a literal indexes it with, holds the same strings. The table costs 16 bytes per distinct literal a call evaluates.
- A record `{ id: i, name: "row", active: b }` built in a loop took about 630 bytes, most of them copies of its keys; it now takes about 225. Records hold no per-record hash table below 16 fields, as before.

Field positions are not fixed at compile time. Shape types are structural and order-free, so the checker interns their fields sorted by name, while a record's insertion order is observable and depends on how it was built: a literal's order, a JSON document's, or a host's. A lookup with a literal key compares the record's keys in order, and with shared keys the comparisons are short.

`records::Fields` is the hook for building records outside the VM. `JSON.parse_as(raw, shape)` can import the shape's field names once per parse and key every object it builds with them, instead of copying each key of each object; that follow-up belongs to the JSON implementation.

## Typed arithmetic

The simple loop already applies arithmetic and comparisons to two compact integers or two floats inline, checking their tags, and falls back to the general operator for big integers, instances and other operands; integer overflow promotes to a big integer there. Measurement found no cost left to remove: carrying the operator as an enum instead of its spelling made the numeric loops 5 to 10 percent slower on an Apple M4, and the tag checks cost less than the dispatch around them. Inlining the operand stack's push in the simple loop made them 6 to 15 percent faster.

## Accounting

Steps and tracked bytes stay exact and deterministic, and the portable and SIMD builds report the same counters. Limits, cancellation and latched exhaustion behave as before. Counters change only in these ways:

- Removed checks no longer charge their work, and removed instructions (argument lists, prologue checks, receiver preparation of non-hash receivers) no longer charge their steps.
- Frames are smaller and direct calls build no argument list, so peak bytes drop.
- Shared literals lower peak and retained bytes wherever a literal is evaluated more than once, and add 16 bytes per distinct literal.

The golden README's counter log records each re-recording.
