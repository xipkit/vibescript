# ADR-007: Static types with local inference

## Status

Accepted - 2026-09-24

This ADR supersedes [ADR-004](004-static-checking-for-typed-boundaries.md) and
builds on [ADR-006](006-slim-language-for-predictable-sandboxing.md). It is the
first language decision made for the Rust implementation as the reference; the
Go implementation will be deprecated and keeps the ADR-004 language.

## Decision

Every Vibescript expression has a static type, and a program that does not type
check does not compile. `Engine::compile`, `vibes run`, `vibes check`, the REPL
and the language server all report type errors as compile errors, before any
script code runs.

Types come from four places only:

1. **Declarations.** Every function parameter and every function that returns a
   value declares its type, and so does the block parameter of every function
   that yields. Properties, instance variables and typed locals declare theirs.
2. **Inference from the expression.** A local takes the type of its first
   assignment and keeps it. Literals, operators and calls have types determined
   by their operands and signatures.
3. **Signatures of builtins and host capabilities.** Every builtin function and
   member has a typed, possibly generic, signature. Host capabilities contribute
   their declared contracts.
4. **Narrowing of dynamic values.** Values whose type is not known statically,
   such as `JSON.parse` results and results of unsigned host functions, have type
   `any` and must be narrowed before use.

The governing principle is: **every value's type is known where it is used, and
the only runtime type checks are at the edges where dynamic data enters.**

## Resulting language shape

```vibe
def total(items: array<{ price: int, qty: int }>) -> int
  sum = 0
  items.each { |item|           # item: { price: int, qty: int }
    sum = sum + item["price"] * item["qty"]
  }
  sum
end

count = 1
count = "one"                   # compile error: count is int, got string

names: array<string> = []       # empty literals need a declared type
names << "Ada"

label: string? = nil            # so does nil
label = "ready"

body = JSON.parse(raw)          # any
body["name"].upcase             # compile error: body is any; narrow it first

user = JSON.parse_as(raw, { name: string, age?: int })
user["name"].upcase             # user["name"] is string
```

## Context

ADR-004 chose gradual typing: unannotated code stays dynamic, the checker infers
facts where it can, and unknown values defer to runtime contracts. Implementing
that contract faithfully made the checker the largest and least predictable part
of the system. In the Rust implementation it is about 52,000 lines, plus 46,000
lines of tests, half of `src/`. To stay useful on unannotated code it performs
flow-sensitive abstract interpretation: literal and union facts, per-call-site
specialization of function bodies, loop fixpoints with widening, object-heap and
recursion summaries, and an explicit "incomplete" result for anything it cannot
yet model.

Each of those mechanisms has had to be hardened separately. One sweep over the
reference test suite's 7,890 programs found 320 reporting incomplete analysis,
four that never converged, and program shapes whose checking cost grew
cubically. All of these were fixed. None of them would exist in a checker that
knows every value's type from declarations and local inference.

Vibescript is written mostly by AI. Rules an author can apply locally, such as
"annotate every parameter" and "a variable keeps its first type", are easier for
a model to follow than rules about which facts a gradual checker will manage to
prove. A mandatory type check also turns every mistake into a compile-time
diagnostic instead of a runtime failure in production.

ADR-004 rejected mandatory annotations for making small scripts heavier. With AI
writing most scripts that cost is small, and the benefit is a smaller, faster
checker with predictable results.

## Design

### Functions

- Every parameter, including optional, keyword and rest parameters, declares a
  type: `def greet(name: string, times: int = 1, **opts: hash<string, any>)`.
- Keyword parameters follow a bare `*` or a rest parameter and are declared
  like positional ones: `def send_email(to: string, *, cc: string? = nil,
  retries: int = 3)` and `def join(*items: array<int>, sep: string = ",")`.
  Calls pass them by name, as in `send_email("a@b.c", retries: 5)`. The forms
  `name:`, `name: default` and `name: T:` are removed (ADR-008): `name: 2`
  read as a keyword default while `name: int` read as a typed positional
  parameter, and a typed keyword could not have a default.
- A function that returns a value declares `-> T`. A function without `->`
  returns `nil`: its final expression is evaluated for effect only, and
  `return value` inside it is an error.
- A function is checked once, from its own signature and the signatures of what
  it calls. Callers never look at bodies. Checking is linear in the program and
  independent of declaration order.
- Functions are not generic; builtins are. A helper that would need a type
  parameter takes `any` and narrows, or is written once per type. User generics
  may be proposed later in a separate ADR.

### Locals

- A local is declared by its first assignment and keeps that type:
  `x = 1; x = "a"` is an error, whether the second assignment is in the same
  block, a branch, a loop or a block body.
- `name: T = value` declares a local with an explicit type. It is required when
  the initial value does not determine the intended type: `nil`, `[]`, `{}`, or
  a value that should be stored as a wider union.
- A local must be definitely assigned before it is read. A local first assigned
  in only one branch cannot be read after the branch.
- Assignment never converts. `int` does not widen to `float`; `number` is the
  union `int | float`.

### Unions, nil and narrowing

- Unions and nullability exist only where they are declared (`T?`, `A | B`) or
  produced by an operation whose signature includes `nil`: `array[i]` and
  `hash[key]` are `T?`, `find` is `T?`, and `x&.m` adds `nil`.
- `fetch(i)` and `fetch(key)` return `T` and raise when the element is missing.
- `x == nil`, `x != nil`, `x.is_type?(:atom)` and early returns narrow a local
  or parameter in the branches they guard. Conditions are strictly `bool`
  (ADR-008). Narrowing does
  not apply to member reads or index expressions: bind the value to a local
  first.

### Collections

- An array literal's element type is the union of its elements' types:
  `[1, 2]` is `array<int>`, `[1, "a"]` is `array<int | string>`.
- A hash literal is an exact shape: `{ name: "Ada", age: 3 }` is
  `{ name: string, age: int }`. Labels are string keys (ADR-006), so the fields
  are read as `h["name"]` and `h["age"]`. Reading a declared field with a
  literal key yields the field type, not `T?`. Reading or writing an undeclared
  key is an error, and so is indexing a shape with a key known only at runtime:
  a record is not a dictionary. The diagnostic's fix declares the dictionary
  type when every field has the same type.
- A dictionary is declared as `hash<string, V>`:
  `counts: hash<string, int> = {}`. Keys are strings (ADR-006). A shape whose
  fields all have type `V` is assignable to `hash<string, V>`.
- Empty literals take their type from context: a declared local, a typed
  parameter, return, property or field, or an element of a typed collection.
- A tuple type `[A, B]` is an array of exactly those elements, in order.
  Tuples exist only at compile time; their values are arrays. An array literal
  of matching length and element types is assignable to a tuple type, and
  indexing a tuple with an integer literal yields that element's type.
  Builtins use tuples for fixed-length results and pairs: `partition` returns
  `[array<T>, array<T>]`, `divmod` returns `[int, int]`, and hash `to_a`
  returns `array<[string, V]>`.

### Dynamic values

- `any` is the type of values the program cannot know statically:
  `JSON.parse` results, host globals declared without a type, results of host
  functions and capability methods without signatures, and values stored in
  `any`-typed containers.
- An `any` value may be compared with `==`, tested with `== nil` and `is_type?`,
  passed or stored where `any` is accepted, and narrowed. Every other use is a
  compile error: calling a member, indexing, using an operator, or passing it to
  a typed parameter.
- Narrowing happens through `is_type?` in a condition, through
  `JSON.parse_as(raw, T)`, and through the checked cast `value.as(T)`. Both of
  the latter validate at runtime like a typed parameter, raise the same boundary
  error on a mismatch, and have type `T`. A cast also narrows a declared union.

### Blocks

- Builtin members declare block signatures, generic where needed:
  `array<T>#map` takes a block `(T) -> U` and returns `array<U>`. Block
  parameters take their types from the signature. Annotations on block
  parameters are optional and must match.
- A block's result is checked against the signature's result type.
- A script function that uses `yield` declares its block as a typed parameter,
  last in the parameter list:

  ```vibe
  def keep(items: array<Item>, &block: Item -> bool) -> array<Item>
    kept: array<Item> = []
    items.each { |item|
      kept << item if yield(item)
    }
    kept
  end

  def each_pair(h: hash<string, int>, &block: (string, int))
    h.keys.each { |k| yield k, h.fetch(k) }
  end

  def maybe_log(msg: string, &block?: string -> nil)
    yield msg if block_given?
  end
  ```

  A single argument type needs no parentheses; several are parenthesized.
  Without `-> R` the block's value is discarded, and using `yield` as a value is
  an error.
- The block parameter's name is a declaration only. Calling, storing, returning
  or passing it is a compile error; `yield` and `block_given?` are the only ways
  to reach the block, so it still cannot escape (ADR-006).
- Each `yield` is checked against the declared argument types and has the
  declared result type. Callers' blocks are checked against the declaration like
  builtin blocks, and calling a function whose block is required without one is
  a compile error.
- `&block?:` makes the block optional. Every `yield` must then be guarded by
  `block_given?`, which narrows like a nil check.

### Builtin signatures

- `src/signatures/builtins.vibe` is the signature table: every builtin
  function, namespace member and member of every value type, written as
  declarations and printed by `vibes prelude` (ADR-008).
- Builtin signatures may be generic. `class array<T>` binds `T` to the
  receiver's element type, `def map<U>` introduces `U`, and `T: B` requires `T`
  to be a single type assignable to `B`, so `array<int | string>` has no `sort`
  or `sum`.
- A builtin name may have several signatures. A call selects one by its number
  of positional arguments, its keyword names, and whether it passes a block and
  how many parameters the block declares, never by the types of its arguments:
  `first` returns `T?` and `first(n)` returns `array<T>`, and a hash's
  `each { |key, value| }` and `each { |pair| }` bind `(string, V)` and
  `[string, V]`. The table refuses an overload set in which one call could
  match two signatures. Script functions are not overloaded.
- `regex`, `match_data` (a successful match), `error` (what
  `rescue => error` binds) and `type<T>` (a type literal, such as
  `JSON.parse_as`'s second argument) are type names in annotations too.

### Classes, enums and namespaces

- Properties, getters and setters declare their types. Other instance
  variables are declared in the class body: `@count: int = 0` gives each
  instance that default before `initialize` runs, and `@name: string` without a
  default must be assigned on every path through `initialize`. Reading or
  assigning an undeclared instance variable is an error. Class variables are
  declared the same way, with a value: `@@count: int = 0`.
- Methods follow the function rules. `initialize` declares its parameter types.
- Classes are nominal and have no inheritance (ADR-006), so there is no subtype
  relation beyond unions, `nil` and `any`.
- Each enum is a type. A symbol literal naming a member is accepted where the
  enum is expected; any other symbol is an error.

### Host boundaries

- `Script::call(name, args)` validates the host's argument values against the
  function's declared parameter types when the call starts, since host values
  are dynamic. The result has the declared return type.
- Host functions and capabilities with signatures are typed by them. Without a
  signature they accept `any` arguments and return `any`.
- A host declares the globals and capabilities each call supplies:
  `Engine::declare_global(name, type)`, and `Engine::declare_capability`, which
  types a capability's methods by their published signatures and its data by
  its template's values. A global declared without a type, and a capability
  built by a factory, are `any`. A bare name that is neither in scope nor
  declared is a compile error (V0201), not `any`. `Engine::prelude` lists the
  declarations, and each call checks at entry that its globals and
  capabilities match them, as it checks arguments.
- The CLI passes arguments as strings. `vibes run script.vibe a b` requires the
  entry function's parameters to accept `string`, or a rest parameter to accept
  `array<string>`, and reports a type error before running otherwise.

### Runtime and accounting

- A typed boundary between two well-typed parts of a program is proven at
  compile time and is not rechecked at runtime. Runtime type checks remain at
  host entry, including declared globals and capabilities, `JSON.parse_as`,
  checked casts, and capability results.
- Step, memory and recursion accounting are unchanged. Sizes, loop counts and
  arbitrary-precision arithmetic stay dynamic, so every existing charge remains.

### What goes away

- The gradual checker (`src/checking`), its incomplete results, and its
  per-call-site analysis.
- `vibes check` as a separate semantic gate: it becomes "compile and report".
  `run -check`, the flat `--check`/`--checked` flags and `Script::check*` become
  compile-time type checking, and `checked_call` becomes ordinary `call`.
- Runtime contract checks on proven internal boundaries.

### Diagnostics

Type errors are reported in Rust-owned, stable wording with source positions,
several per compilation where recovery is cheap. There is no Go compatibility
constraint on type diagnostics.

## Migration

- This is a breaking change, permitted before 1.0. Every script with an
  unannotated parameter, a type-changing assignment or an untyped empty literal
  must change.
- A `vibes migrate` command will propose annotations: parameter and return types
  observed while running a script's tests or supplied inputs, plus the types the
  compiler infers for locals. The migration must cover the repository's own
  corpus, tests, fixtures, examples and documentation.
- The Go implementation stays on the ADR-004 language. Most annotations already
  parse and run there, so the Go differential tools remain usable for runtime
  semantics on well-typed programs Go can parse. Typed local declarations,
  instance-variable declarations, block type parameters and `as` casts are new
  syntax that Go rejects.

Planned order: this ADR and the specification; parser support for typed locals,
instance-variable declarations and block type parameters; the new type checker with builtin signatures; compile
integration and removal of proven runtime checks; migration of the corpus;
removal of the gradual checker; CLI, REPL, language server and documentation.

## Consequences

Easier:

- Type checking is one linear, modular pass: no fixpoints, widening, summaries,
  heaps or incomplete results. The checker becomes a small fraction of its
  current size and runs in time proportional to the program.
- A program that compiles has no type errors except at explicit narrowing
  points and host entry.
- Authors, human or AI, follow local rules with local diagnostics.
- The runtime can skip checks the compiler has proven, and the language server
  gets exact types for hover and completion.

Harder, and what we now owe:

- Scripts carry more annotations, and dynamic JSON needs `JSON.parse_as` or
  explicit narrowing.
- `array[i]` and `hash[key]` require nil handling or `fetch`.
- Helpers that are naturally generic need `any` or per-type copies until user
  generics exist.
- Every builtin member needs a complete typed signature, including generic
  block signatures, and that table becomes part of the language contract.
- The migration touches almost every test and example in the repository, and
  the REPL must keep binding types across lines.

## Alternatives considered

### Keep ADR-004's gradual checker

Rejected. It works, but at the cost described under Context, and a clean result
still does not prove the absence of type errors.

### Crystal-style whole-program inference

Rejected. Crystal infers parameter types per call site and turns
`x = 1; x = "a"` into a union. That is the per-call-site analysis this ADR
removes, and it keeps a function's types dependent on its callers.

### Keep `any` gradual

Rejected. Allowing operations on `any` and checking them at runtime keeps the
checker simple, but it leaves runtime type errors possible wherever dynamic data
flows, and those are the paths that matter most in workflow scripts.

### Declare block types after the return type

Rejected. A clause such as `-> array<Item> yields (Item) -> bool` puts a second
arrow after the return type and a new keyword in every yielding signature.
Declaring the block as a typed `&` parameter, as Crystal does, keeps the whole
contract in the parameter list; making its name a declaration only preserves
ADR-006's rule that blocks never become values.

### Infer block types

Rejected. Argument types could come from the `yield` sites, but the block's
result type depends on each caller's block, so the body would have to be
checked per call site. Crystal can do this because it inlines every yielding
method; checking each function once from its signature cannot.

### Infer return types

Rejected. It saves one annotation per function, but makes checking depend on
callee bodies and declaration order, and makes a function's contract implicit.

## Links

- Superseded: [ADR-004: Infer local types and check typed boundaries
  statically](004-static-checking-for-typed-boundaries.md)
- Builds on: [ADR-006: Slim the language for predictable
  sandboxing](006-slim-language-for-predictable-sandboxing.md)
- Current type syntax and runtime contracts: [types](../types.md)
- Current checker, to be replaced: [checker](../checker.md)
