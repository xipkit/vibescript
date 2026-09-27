# The typed VM

Static types are always on (ADR-007), so the compiler knows the type of every value, and the VM uses that knowledge: it skips runtime checks the checker has proven, binds builtin members and script methods to receivers whose type is known, runs blocks without allocating, and shares the keys of records. Behavior is unchanged in every case: the golden corpora record the same observations before and after. Step counts drop only where the VM no longer executes an instruction or a check, and each such change re-recorded the counters (see [the counter log](../tests/golden/README.md#counter-log)).

This page describes the design. The implementation is in `src/bytecode.rs`, `src/vm.rs`, `src/vm/simple.rs`, `src/members/direct.rs`, `src/records.rs` and `src/value/import.rs`, with the checker's side in `src/typing.rs` (`Facts`) and `src/typing/construction.rs`.

## Proven checks

A typed boundary between two well-typed parts of a program is proven when it compiles (ADR-007), so the VM does not check it again:

- the arguments of a call from script code to a script function, method, block or required file's function;
- the result of a function or class method;
- the value stored in a typed local, a `yield` argument and a block's result.

A function whose parameters are all required and positional has a *proven start*: the instruction after its prologue's parameter checks. A call from script code binds its arguments straight into the parameter slots and starts there, without building an argument list; this is most of what made calls three times faster.

The runtime keeps a check where it does more than the checker proves (`Type::unproven`):

- **Host entry.** Arguments to `Script::call`, declared globals and capabilities, `JSON.parse_as`, `as` casts and capability results are dynamic data. The entry call checks every argument against its parameter's type. When the function has a proven start, it binds the host's arguments directly and checks each as its prologue's `Bind` instruction would, reporting a mismatch at the same place; otherwise it runs the prologue.
- **Named types.** A type that names a class or enum resolves at runtime, and an enum parameter turns a symbol literal into its member, so the check is a conversion.
- **Hash key types.** The checker admits a hash type whose key type no string satisfies, such as `hash<int, any>`, which only the runtime rejects.
- **Instance method results, in some classes.** A property that is not assigned yet reads as `nil` whatever its declared type. The checker proves a class safe when `initialize` assigns every property without a default before any method can read it: no call on `self` while a property is unassigned reaches a method that reads it, directly or through the methods it calls on `self`, and `self` is not passed on until they are all assigned (`src/typing/construction.rs`). The checker does not report the other classes, such as one with no `initialize` whose properties a setter assigns; their instance methods and accessors keep the result check, which turns the `nil` into a type error where it surfaces.

An instance variable write is proven too when the class declares the variable's type, as the runtime would find it, and the type names no class or enum (`Program::prove_instance_variables`); the runtime then neither looks the type up by scanning the class's methods nor checks the value. Property setters keep their parameter check.

## Capabilities

A glue service binds capabilities on every call, and a value that holds a host method or an exported function must not escape as data. With a capability or required module bound, the VM scanned every index, member and function call result, argument, block parameter, `for` element, iterated element and return value for them, walking the whole value each time: reading a record of a JSON document scanned the record, iterating an array scanned each element, and returning a document scanned it again.

Only a value whose static type involves `any`, a capability or a required module can hold one. The checker records per expression and per block whether the type is *plain* (`Facts::plain`), and the compiler marks the instructions whose value is (`Function::plain_values`, `Function::plain_inputs`), so the VM skips their scan. The scan stays at host entry, on capability results and wherever the type could hold a callable.

Binding a capability also creates the call's root bindings, which can stand for a local no slot binds. The simple loop used to decline every instruction once any existed; it now declines only a local whose resolved slot is unbound while bindings or host globals exist, the one case they can affect.

## Direct builtin calls

A member call dispatched by name at runtime: calls to members that can iterate, such as `fetch` and `include?`, built an argument list and passed through the iteration, capability, export and keyword checks, and every call then probed about twenty member tables, twice, before reaching the builtin.

The checker records the static base type of each member call's receiver when it has exactly one: `hash` (dictionaries and shapes), `array` (arrays and tuples), `string`, `int` or `float`. When that base serves the member called, with no block, splat or keywords, the compiler emits `Op::Direct` instead of the general call sequence. `members::direct::serves` lists the members: `length`, `empty?`, `fetch`, `key?`, `value?`, `keys`, `values`, `first`, `last`, `sum`, `join`, `include?`, `start_with?`, `end_with?`, `bytesize`, `abs`, `even?` and `odd?`.

`Op::Direct` checks the receiver's runtime kind first. A big integer, a host object, whose fields take precedence over hash members, or a rescued error or match data takes the dynamic path exactly as before. A unit test compares every direct member with dynamic dispatch on well-typed arguments, including the steps and bytes each charges.

Receivers typed `any` or a union keep the dynamic path.

Array updates, `push`, `pop`, `shift`, `prepend`, `insert` and `<<`, go straight to the update (`members::direct::update`) instead of probing every member table by name, and the simple loop runs them, with index stores, for arrays and hashes whose root needs no type guard.

A call of a script method whose receiver the checker proves is always an instance of one class the program declares compiles to `Op::MethodOf` ahead of the dynamic call. It checks that the receiver is an instance of that class, as every method call does, and calls the method without looking it up by name; any other receiver falls through to the dynamic call, which also enforces visibility.

## Calls and blocks

Calls pass their arguments without temporary lists wherever the callee binds them directly: a call to a script function (`Op::Call`), a call with a block and plain arguments (`Op::CallBlock`), and a method call on an instance or namespace all enter from the operand stack. Only a callee with defaults, keywords or a rest parameter, or a call with a splat or keywords, builds an argument list.

A block's arguments stay on the operand stack, below its own values, instead of in a list the block's frame owned; `BlockArg` reads them there. The simple loop runs block prologues (`Shadow`, `BlockArg`) and follows captured locals out to the frame that binds them, charging each frame it passes as the general path does. Frames are 32 bytes smaller as a result.

Class variable and internal environment writes copy the variable's name into a field key only when the variable is new.

## Instance field slots

Each class has a compiled slot layout (`Program::field_layouts`) covering typed instance variables, properties, getters, setters and instance parameters. `InstanceField`, `InstanceStore`, `InstanceAddress` and `BindField` carry slot numbers. Reads, writes and addressed collection updates use those slots directly; named types and setters retain their existing boundary checks.

An instance stores values in a metered slot buffer that grows through its highest assigned slot; gaps are initialized in bounded, charged chunks. Its slots also link the fields in first-write order: declaration order does not determine export or traversal order. An unassigned slot is distinct from an assigned `nil`, reads as `nil`, and stays absent from dynamic field enumeration. Replacing a value leaves its position unchanged. Imports, snapshots, equality, printing and inspection retain their previous behavior, and GC follows the same ordered field values. Internal environment objects continue to use ordinary named fields. The class layout stays with compiled code, alongside its existing field types.

## Records

A shape's runtime value, a record, is an ordinary hash. It keeps insertion order, and converts to `hash<string, V>`, crosses host boundaries, serializes to JSON, compares, iterates and prints as any hash does, with the same accounting. Records are compact because their keys share storage:

- Every string and symbol literal of a program takes a slot, one per distinct text (`Program::shared`). A call imports a slot on its first use and shares that value afterwards (`Op::Shared`), charging the step an import charges each time. Every record a literal builds, and every key a literal indexes it with, holds the same strings. The table costs 16 bytes per distinct literal a call evaluates.
- A record `{ id: i, name: "row", active: b }` built in a loop took about 630 bytes, most of them copies of its keys; it now takes about 225. Records hold no per-record hash table below 16 fields, as before.

Field positions are not fixed at compile time. Shape types are structural and order-free, so the checker interns their fields sorted by name, while a record's insertion order is observable and depends on how it was built: a literal's order, a JSON document's, or a host's. A lookup with a literal key compares the record's keys in order, and with shared keys the comparisons are short.

Literal string reads use `IndexLiteral`: a plain hash borrows the key's compiled bytes, avoiding the shared-literal import, temporary operand and separate instruction. Lookup still respects the actual record's order. Instances, tagged hashes and host objects keep general indexing, and results whose types can contain callables keep the export check.

Records a host passes in share their keys as well. An import keeps the keys it copies from small hashes, under 16 entries, in a table of 64 slots by a hash of their bytes, and the records after the first reuse them; a JSON-shaped argument of 256 records of four fields held 1,024 key copies, half of the call's tracked memory. Dictionaries do not use the table, since their keys do not repeat.

`records::Fields` is the hook for building records outside the VM. `JSON.parse_as(raw, shape)` can import the shape's field names once per parse and key every object it builds with them, instead of copying each key of each object; that follow-up belongs to the JSON implementation.

## Fixed memory

A call's fixed memory was mostly the VM's control stacks, since a buffer's first growth reserves eight elements: 2,496 bytes of frames, 1,664 of pending argument lists and 4,352 for a first iteration. A buffer of elements over 128 bytes now starts at four, which covers almost every call.

A frame was 312 bytes and carried three buffers of its own: its loops, its pending calls' arguments and its parameter binding. They are now stacks shared by the call (`Storage::loops`, `Storage::arguments`, `Storage::parameters`), and a frame records where its part starts; its indexes and enclosing frames are 32 bits. A frame is 136 bytes, so a call's four reserved frames take 544 bytes, and a trivial call peaks at 968 bytes instead of about 1,700.

A compiled script keeps its instructions between calls. An instruction is 16 bytes instead of 40: sources are at most 8 MiB, so every slot, jump target and table index fits in 32 bits, an operator is a byte naming its spelling, a receiver rule packs into its member's name index, and the rare `raise` class and destructuring selections sit in tables on the program. A script such as the `glue_orders` benchmark keeps about 11 KB after compiling instead of 14 KB. The first compilation in a process also builds the builtin signature tables, about 470 KB that every later compilation shares.

A trivial call's memory is now mostly its four frames. Iteration previously reserved four 544-byte states, or 2,176 bytes, regardless of which driver was active. Round 3 boxed individual drivers, with a 480-byte common driver and eight 16-byte handles. Common array and numeric methods now use a compact driver without the hash, window and grouping buffers. The first two nesting levels share one charged, two-slot arena; deeper levels use individual boxes. Finishing an inner iteration drops its values and output buffers immediately, leaving its arena slot available to the next iteration. Finishing or unwinding the last arena driver releases the arena itself. Text, sorting, hash and regex drivers reserve their own boxed state, and `loop` needs only its inline waiting flag.

The common driver keeps endpoints and strides in 64 bits, but computes range lengths and stepping arithmetic in 128 bits, preserving the full integer range. Pattern matching and aggregation share one value slot because no method uses both.

## Typed arithmetic

The simple loop applies arithmetic and comparisons to two compact integers or two floats inline, checking their tags, and falls back to the general operator for big integers, instances and other operands; integer overflow promotes to a big integer there. Scalar operations keep the bytecode operator code through execution, and only overloads and general operations recover its spelling. Operand pushes stay inline, while literal hashing and rare argument/context errors stay outside instruction dispatch. These choices reduce dispatch work, but do not guarantee that unrelated changes leave the interpreter's machine-code layout or timing unchanged.

The simple loop consumes a boolean comparison directly in a following conditional branch or loop test. It also binds plain block arguments straight to their shadowed slots when the prologue has no intervening check or destructuring. Both paths charge each original instruction separately, preserving its location, quota boundary and checkpoint cadence. Big integers, overflow promotion, floating-point arithmetic and comparisons still use the existing operator implementation. `Op` remains 16 bytes.

## Accounting

Steps and tracked bytes stay exact and deterministic, and the portable and SIMD builds report the same counters. Limits, cancellation and latched exhaustion behave as before. Counters change only in these ways:

- Removed checks no longer charge their work, and removed instructions (argument lists, prologue checks, receiver preparation of non-hash receivers) and copies (field names) no longer charge their steps.
- Frames are smaller, control stacks start at four elements and calls build fewer argument lists, so peak bytes drop.
- Records a host passes in share their keys, so peak and retained bytes drop where an argument holds several.
- Shared literals lower peak and retained bytes wherever a literal is evaluated more than once, and add 16 bytes per distinct literal.
- Active iteration drivers replace capacity reserved for inactive drivers; nested collection iteration also uses less memory.
- Declared instance fields skip name searches and per-instance key copies. Slot links preserve first-write order and distinguish absent fields from assigned nil.
- Literal record reads remove the key-push instruction and its import work, with no change to lookup order or dynamic-boundary checks.

The golden README's counter log records each re-recording.
