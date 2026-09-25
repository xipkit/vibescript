# Types

Every expression has a static type, and a program that does not type check does not compile ([ADR-007](adr/007-static-types.md)). The [language guide](language.md) explains how types are declared, inferred and narrowed; this page lists the type syntax and the runtime checks that remain at the edges where dynamic data enters.

## Type syntax

| Type | Meaning |
| --- | --- |
| `int`, `float`, `string`, `symbol`, `bool`, `nil` | Scalars. `int` is arbitrary precision and does not accept `1.0`. |
| `number` | `int \| float` |
| `duration`, `time`, `money`, `range` | Built-in value types. |
| `regex`, `match_data`, `error` | A compiled pattern, a successful match, and what `rescue => error` binds. |
| `any` | A value whose type is not known statically; it must be narrowed before use. |
| `T?` | `T` or `nil`. |
| `A \| B` | Either type. |
| `array<T>` | An array of `T`. |
| `hash<string, V>` | A dictionary with string keys and values of type `V`. |
| `{ name: string, age?: int }` | A shape: a hash with exactly these string keys. `age?:` may be absent, while `age: int?` must be present and may hold nil. A final `...` permits extra keys. |
| `[A, B]` | A tuple: an array of exactly these elements, in order. |
| `Status`, `Account`, `Outer::Inner` | An enum or class, named through its scope. |
| `type<T>` | A type literal describing `T`, such as the second argument of `JSON.parse_as`. |

Builtin type names are lowercase; `Int` and `object` are removed spellings of `int` and `hash` (V0413). `type Reward = { id: string, points: int }` names a type at top level or in a module or class body; aliases are transparent, and may refer to other aliases but not to themselves.

Parameters, results, properties, instance variables and typed locals declare their types. Positional defaults, keyword parameters after a bare `*`, and rest captures are typed the same way:

```vibe
enum Status
  Draft
  Done
end

def publish(id: string, *, state: Status = :draft, note: string) -> string
  "#{id} #{state.name} #{note}"
end

def tag_count(*names: array<string>, **extra: hash<string, int>) -> int
  names.length + extra.length
end

publish("a", note: "x")    # "a Draft x"
tag_count("a", "b", c: 1)  # 3
```

A symbol literal naming an enum member is accepted wherever that enum is expected, and becomes the member: `state: Status = :draft` above, or `states: array<Status> = [:draft, :done]`. A string such as `"draft"`, or a member of another enum, is not. Enum definitions retain their identity across calls to the same compiled script; a separate compilation creates distinct definitions.

Block parameters take their types from the called function's signature. An annotation on a block parameter is optional and must match it.

## Runtime checks

A typed boundary between two well-typed parts of a program is proven when it compiles. Values are still checked at runtime where dynamic data enters:

- arguments a host passes to `Script::call` and the functions it names, and the host's declared globals and capabilities;
- `JSON.parse_as(text, T)` and the checked cast `value.as(T)`, which both have type `T` and raise the typed boundary error on a mismatch;
- the results of host capabilities with declared contracts.

These checks follow the declared type exactly. Union arms are tried in source order, with `any` last, and a matching symbol becomes a nominal enum member:

```vibe
enum Status
  Draft
  Done
end

def accept(packet: { status: Status, attempts?: int, ... }) -> Status
  packet["status"]
end

accept({ status: :draft }).name # "Draft"
```

A host that calls `accept` with a hash whose `status` is `:draft` gets `Status::Draft`; a string such as `"draft"`, a missing `status` or a member of another enum fails the boundary.

Unchanged arrays and hashes retain their storage. Enum coercions copy changed containers while preserving other values, field order and open-shape extras. Memory charges cover new containers, enum metadata and temporary name-resolution state. Traversal checks work limits, cancellation and deadlines; union fallback cannot absorb exhausted limits. Normalization also enforces a 64-level traversal guard, including union arms.

## Type mismatch diagnostics

Type mismatches found at runtime identify the boundary, expected annotation and actual value type. They are rescuable runtime errors, exposed to Rust hosts as `ErrorKind::Type`.

```text
argument payload expected int, got string
return value for typed expected int, got string
instance variable @payload expected array<int>, got array<string>
JSON.parse_as value expected array<int>, got array<int | string>
cast value expected array<int>, got array<int | string>
```

Defaults, keywords, rest captures and block parameters use the argument form. Destructured groups retain their pattern label. Generated property setters check their `value` parameter; direct instance-variable assignments and guarded nested updates name the backing field. Diagnostics do not call user-defined string methods.

Expected types use the same canonical spelling as type literals. Actual collection types are bounded summaries: arrays sample the first sixteen entries; hashes sample the first sixteen keys in byte order. A hash with at most six fields shows a shape. Larger collections show a sorted, deduplicated union, with `...` when sampling truncates it. Empty collections render as `array<empty>` and `{}`. Nested summaries stop after sixteen levels. Repeated references are summarized independently by path. Raw field-name bytes remain available through `Error::message_bytes()`.

Every scan, comparison and emitted byte consumes work; temporary storage is reserved against the call's memory limit before allocation. Exhaustion, cancellation and deadlines take precedence over constructing a type error. Rescued diagnostics release their scratch storage. Successful normalization creates no diagnostic buffers.

Compile-time type errors are separate: they are [diagnostics](diagnostics.md) with stable codes, reported before any code runs.

## Type literals and JSON

A type literal describes a type as a value. `JSON.parse_as` parses a JSON string and validates the result through the same normalization path as a typed parameter, and its result has the literal's type:

```vibe
schema = { name: string, age?: int, ... }
packet = JSON.parse_as("{\"name\":\"Ada\",\"active\":true}", schema)
packet["name"].upcase # "ADA"
```

The second argument can be any type an annotation can name: `JSON.parse_as("[1,2]", array<int>)` has type `array<int>` and `JSON.parse_as("null", int?)` has type `int?`. A shape literal can be stored in a local and passed later, as `schema` is above.

Type equality compares canonical annotations: field order does not matter, while union order and optional fields are preserved. Interpolation renders a value such as `<Shape { name: string }>`. Hosts inspect canonical bytes with `Value::as_type_literal()`; literal field names can contain invalid UTF-8. Type values cannot be JSON-encoded.

Each imported type value charges its retained metadata and wrapper to the receiving call. Clones share that charge; a foreign import receives an independent charge while sharing immutable metadata. Rendering and equality charge bounded byte scans and observe cancellation. Unused compiled literals do not allocate execution storage.

Script `JSON.parse` and `JSON.parse_as` inputs and `JSON.stringify` output have a fixed 1 MiB guard. Before writing an ASCII escape, the serializer requires six bytes of headroom even for a two-byte escape. Guard failures return `ErrorKind::OutputLimit` and remain latched. Host `parse_json` and `stringify_json` helpers use their independent `CallOptions` budgets without this builtin payload cap. Runtime value nesting is bounded at 10,000 levels; see [JSON depth](json-depth.md).
