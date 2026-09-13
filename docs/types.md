# Runtime type annotations

Function arguments, defaults, returns and block bindings support runtime type annotations. Static checking, class types and typed accessors remain pending in the [language port](language-port.md).

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

Annotations support `any`, `int`, `float`, `number`, `string`, `symbol`, `bool`, `nil`, `duration`, `time`, `money` and `range`; bare or parameterized arrays and hashes; shapes; and named enums. `object` is a hash-type alias. Scalars are strict: `int` rejects `1.0`, while `number` accepts integers and floats.

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
