# Runtime type annotations

Function arguments, defaults, returns, block bindings and instance-variable writes support runtime type annotations. Nominal class types and typed accessors are also supported. Extended name resolution and static checking remain pending in the [language port](language-port.md).

```vibe
enum Status
  Draft
  Done
end

def accept(packet: {status: Status, attempts?: int, ...}) -> Status
  packet.status
end

accept({status: :draft}).name
```

This returns `"Draft"`. A matching symbol becomes a nominal enum member. A string such as `"draft"` or a member of another enum fails the boundary. Enum definitions retain their identity across calls to the same compiled script; a separate compilation creates distinct definitions.

Annotations support `any`, `int`, `float`, `number`, `string`, `symbol`, `bool`, `nil`, `duration`, `time`, `money` and `range`; bare or parameterized arrays and hashes; shapes; and nominal enums and classes. `object` is a hash-type alias. Scalars are strict: `int` rejects `1.0`, while `number` accepts integers and floats.

Use `T?` for nullable values and `A | B` for unions. Union arms are tried in source order, with `any` last, so `any | Status` still converts `:draft` to an enum member. Every named type must resolve, including names in unused union arms, optional fields and empty collections.

`array<T>` validates elements. `hash<string, T>` and `hash<symbol, T>` share the language's string/symbol keyspace. Shape fields are required by default; `attempts?: int` permits omission, while `attempts: int?` requires a field that may contain nil. A final `...` permits extra fields. Quoted field names preserve a literal trailing question mark.

Typed positional defaults use `state: Status = :draft`; typed required keywords use `state: Status:`. Captures accept collection annotations such as `*states: array<Status>` and `**fields: hash<string, Status>`. `state: nil` remains an optional keyword default; use a union such as `state: nil | Status` for a nil-leading type annotation.

Blocks support simple and destructured bindings:

```vibe
[:draft, :done].map { |state: Status| state.name }
[[:draft, :done]].map { |(first: Status, *rest: array<Status>)| first.name }
```

Type names resolve in a function's declaration environment or a block's captured environment. Earlier argument bindings and the function body's locals do not define types for that function's signature. Return annotations apply to implicit and explicit returns, nonlocal block returns and values returned when a passed block breaks out of a function.

Unchanged arrays and hashes retain their storage. Enum coercions copy changed containers while preserving other values, field order and open-shape extras. Memory charges cover new containers, enum metadata and temporary name-resolution state. Traversal checks work limits, cancellation and deadlines; union fallback cannot absorb exhausted limits. Normalization also enforces the reference's 64-level traversal guard, including union arms.

## Type mismatch diagnostics

Type mismatches identify the boundary, expected annotation and actual value type. They remain rescuable runtime errors, exposed to Rust hosts as `ErrorKind::Type`.

```text
argument payload expected int, got string
return value for typed expected int, got string
instance variable @payload expected array<int>, got array<string>
JSON.parse_as value expected array<int>, got array<int | string>
```

Defaults, keywords, rest captures and typed block bindings use the argument form. Destructured groups retain their pattern label. Generated property setters check their `value` parameter; direct instance-variable assignments and guarded nested mutations name the backing field. Returning through a block or an output helper's `to_s` method preserves the function name. Diagnostics do not call user-defined string methods.

Expected types use the same canonical spelling as type literals. Actual collection types are bounded summaries: arrays sample the first sixteen entries; hashes sample the first sixteen keys in byte order. A hash with at most six fields shows a shape. Larger collections show a sorted, deduplicated union, with `...` when sampling truncates it. Empty collections render as `array<empty>` and `{}`. Nested summaries stop after sixteen levels. Repeated references are summarized independently by path. Raw field-name bytes remain available through `Error::message_bytes()`.

Every scan, comparison and emitted byte consumes work; temporary storage is reserved against the call's memory limit before allocation. Exhaustion, cancellation and deadlines take precedence over constructing a type error. Rescued diagnostics release their scratch storage. Successful normalization creates no diagnostic buffers. Unknown and ambiguous named-type errors still have separate wording; broader diagnostic parity remains pending.

## Type literals and JSON

Type literals are immutable values that can be stored, passed to functions and retained by hosts. `JSON.parse_as` parses a JSON string and validates the result through the same normalization path as annotated parameters:

```vibe
schema = {name: string, age?: int, ...}
packet = JSON.parse_as("{\"name\":\"Ada\",\"active\":true}", schema)
[packet.name, packet.active]
```

This returns `["Ada", true]`. Parenthesized positional arguments also accept non-shape roots, such as `JSON.parse_as("[1,2]", array<int>)` or `JSON.parse_as("null", int?)`. A bare `nil` argument remains nil. Expression type literals recognize only builtin leaf types, matching Go v0.70.0; named enums remain available in annotations.

When the tokens also form a normal value expression, runtime bindings can select that reading. For example, `int = 7; schema = {x: int}` creates a hash containing `7`. A trailing comma keeps `{x: int,}` on the hash path. Closed shapes containing a bare nil field, an empty nested shape or a local value also remain ordinary hashes. Generic and union forms that have no value reading stay type literals.

Type equality compares canonical annotations: field order does not matter, while union order, optional fields and the `object` spelling are preserved. Interpolation renders a value such as `<Shape { name: string }>`. Hosts inspect canonical bytes with `Value::as_type_literal()`; literal field names can contain invalid UTF-8. Type values support `nil?`, `itself`, `dup`, `tap` and `yield_self`, and cannot be JSON-encoded.

Each imported type value charges its retained metadata and wrapper to the receiving call. Clones share that charge; a foreign import receives an independent charge while sharing immutable metadata. Rendering and equality charge bounded byte scans and observe cancellation. Unused compiled literals do not allocate execution storage.

Script `JSON.parse` and `JSON.parse_as` inputs and `JSON.stringify` output have Go's fixed 1 MiB guard. Before writing an ASCII escape, the serializer requires six bytes of headroom even for a two-byte escape. Guard failures return `ErrorKind::OutputLimit` and remain latched. Host `parse_json` and `stringify_json` helpers use their independent `CallOptions` budgets without this builtin payload cap. Runtime value nesting remains bounded at 128.
