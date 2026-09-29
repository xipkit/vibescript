# ADR-008: A canonical surface for AI authors

## Status

Accepted - 2026-09-24

Builds on [ADR-006](006-slim-language-for-predictable-sandboxing.md) and
[ADR-007](007-static-types.md). The changes ship in the same release, and are
applied by the same migration, as ADR-007.

## Decision

Vibescript is written mostly by AI and checked by machines. Before 1.0 we make
its surface smaller and more regular, and make its diagnostics repairable
without interpretation:

1. **One way to write each thing.** Every operation and construct has exactly
   one spelling: builtin functions, namespaces and members, type names, and
   syntax. A removed spelling is a compile error whose fix rewrites it.
2. **Diagnostics carry codes and fixes.** Every compile error has a stable code,
   an exact span, the expected and found types where they apply, and, when the
   repair is unambiguous, a machine-applicable edit. `vibes check --json` emits
   them, `vibes fix` applies the edits, and the language server offers the same
   edits as code actions.
3. **Conditions are `bool`.** `if`, `while`, the ternary, `!`, `&&` and `||`
   take `bool` and nothing else.
4. **`case` over an enum is exhaustive.** It must name every member or have an
   `else`.
5. **Type aliases.** `type Name = T` names a type.
6. **A typed prelude.** `vibes prelude` prints every builtin signature, and the
   embedding API prints a host's capabilities and globals the same way, as text a
   host can give a model as context.
7. **`/` divides, `//` floors.** `7 / 2` is `3.5` and `7 // 2` is `3`.
8. **`require` is static.** Module names and aliases are string literals.
9. **No dispatch by name.** `send`, `public_send` and `respond_to?` are removed.

This is a standing rule, not a one-time cleanup. A future proposal may not add a
second spelling for an existing operation or construct. Familiarity from Ruby,
Crystal or any other language is not a reason to add one.

Parenless calls, statement modifiers (`x if cond`) and `loop` stay. Profiling on
quota exhaustion is deferred until after 1.0.

## Context

Two properties decide whether a model writes working code in a language: how
many valid ways there are to say the same thing, and how directly a mistake can
be repaired from the error it produces. Vibescript inherited Ruby's generous
answers to both. It has 441 builtin member names, many of them synonyms:
`size`, `length` and `count`; `key?`, `has_key?`, `member?` and `include?`;
`to_s` and `string` on every type; `is_a?`, `kind_of?` and `instance_of?`, which
are identical in a language without inheritance; singular and plural duration
units; and 13 synonym pairs on `Time` alone, such as `tv_usec`, `gmtoff`,
`isdst` and `xmlschema`, which come from C. The same pattern repeats in the
globals (`format` and `sprintf`, `Time.gm` and `Time.utc`, two regex
namespaces), in type names (`hash` and `object`, and case-insensitive spelling)
and in syntax (`unless` and `until`, brace and `do...end` blocks, symbol and
string hash keys). Each synonym is one more name to learn, type-sign, test and
document, and a source of inconsistent generated code. Go shows that a language
with one spelling per operation is easier to read, write and generate.

Other inherited rules surprise models trained mostly on Python and JavaScript.
Every value except `nil` and `false` is truthy, so `if count` succeeds for `0`.
Integer `/` floors, so `7 / 2` is `3`. A `do...end` block after a parenless call
attaches to the outer call, so `puts items.map do ... end` passes the block to
`puts`. Dispatch by a runtime name defeats static typing (ADR-007) and reaches
private methods: a script that runs `account.send(payload["action"].to_sym)` on
untrusted JSON can be made to call any method on the account. Nothing in the
286-program corpus dispatches by name; its only `.send(...)` calls are
capability methods that happen to be named `send`.

## Design

### Canonical names

The rule is: keep the spelling most models already know, prefer the `to_*`
conversion family, and keep one form per concept.

| Removed | Canonical |
| --- | --- |
| `size`, and `count` without an argument or block | `length` |
| `has_key?`, `member?`, `include?` on hashes | `key?` |
| `has_value?` | `value?` |
| `member?` on ranges | `include?` |
| `string`, `id2name` | `to_s` |
| `nil?` | `x == nil` |
| `is_a?`, `kind_of?`, `instance_of?` | `is_type?` |
| `clone` | `dup` |
| `eql?`, `equal?` | `==` |
| `itself`, `tap`, `yield_self` | the expression itself |
| singular units: `second`, `minute`, `hour`, `day`, `week` | plural: `seconds`, `minutes`, ... |
| duration `since`, `until`, `after` and `before` without a time, `ago` and `from_now` with one | `ago` and `from_now` count from now; `before(time)` and `after(time)` from a given time |
| `find_index` | `index` |
| `mon`, `mday` | `month`, `day` |
| `tv_sec`, `tv_usec`, `tv_nsec` | `to_i`, `usec`, `nsec` |
| `gmt_offset`, `gmtoff` | `utc_offset` |
| `gmtime`, `getutc`, `getgm` | `utc` |
| `gmt?`, `isdst` | `utc?`, `dst?` |
| `xmlschema`, `rfc3339` | `iso8601` |
| `rfc822` | `rfc2822` |
| `Time.gm` | `Time.utc` |
| `Time.mktime`, `Time.new` | `Time.local`, which takes `in:` |
| `Regexp.*` | `Regex.*` (one namespace) |
| `Regexp.quote` | `Regex.escape` |
| `sprintf` | `format` |
| global `now` | `Time.now`, with `.iso8601` for the string |
| type name `object` | `hash` |
| type names in any other case, such as `Int` | lowercase `int` |

`count` keeps its counting forms, `count(value)` and `count { ... }`. The
prelude lists every canonical name, and `src/signatures/renames.txt` is the
authoritative list of removed spellings and their rewrites, shared by the
migration and the compiler's fixes. Writing the signature table resolved more
synonyms by the same rule, among them `append` → `push`, `unshift` →
`prepend`, `collect_concat` → `flat_map`, `take(n)` → `first(n)`, `at` and
`slice` on arrays → `[]`, `store` → `[]=`, `modulo` → `%`, `cover?` →
`include?`, `intern` → `to_sym`, `next` → `succ` and `reduce(:op)` → a block.
Members that always answer the same are removed: `frozen?`,
`Regex.last_match`, string `clear` and `replace`, and time `hash`.

### Canonical syntax

| Removed | Canonical |
| --- | --- |
| `unless cond` | `if !cond` |
| `until cond` | `while !cond` |
| `do \|x\| ... end` blocks | `{ \|x\| ... }`, on one line or several |
| symbol hash keys, `h[:name]` | string keys, `h["name"]`; `name:` labels remain in literals and keyword arguments |
| a hash field read or written with a dot, `h.name` | `h["name"]`; dot calls methods only |
| keyword parameters `name:`, `name: default` and `name: T:` | `*, name: T` and `*, name: T = default` (ADR-007) |
| percent literals, `%w[a b]` | `["a", "b"]` |
| `Hash.new` | `{}` with a declared type |
| `x.length()`, `uuid()`, `Time.now()` | `x.length`, `uuid`, `Time.now`: a call without arguments has no parentheses |
| `JSON::parse(x)`, `Pricing::with_tax(1)` | `JSON.parse(x)`, `Pricing.with_tax(1)`: `::` names only constants, nested types and enum members |

Symbols remain for enum members.

Braces bind to the nearest call, so a block always attaches to the call it
follows. A `{` starts a block when it follows a call on the same line: a
function or method name (`loop {`, `items.each {`), a `)` (`reduce(0) {`), or the last argument of a call without parentheses
(`each_slice 2 {`), where the block belongs to that call. Anywhere else, `{`
starts a hash literal. A block's statements therefore never read as hash
entries: `loop { break :done }` breaks with a symbol. A hash literal passed to a
call without parentheses needs them, `log({ id: 1 })` and
`log("sent", { id: 1 })`, and `log { id: 1 }` is a syntax error (V0002) whose
fix adds them.

Hashes are read by index only. A field never answers a dot, so it cannot
shadow a member, and `h.as(T)` is always the cast. `h.name` on a hash or shape
is reported with the fix `h["name"]` wherever the receiver's static type is a
hash. The runtime reads a field with a dot only on a namespace, such as a
builtin module, a required module's exports, a rescued error, match data or a
host capability, and never writes one.

### Diagnostics

- Codes are stable identifiers such as `V0101`, grouped by area (syntax, types,
  names, calls). A code keeps its meaning across releases; the message text may
  improve.
- Each diagnostic has a primary span, optional secondary spans ("declared
  here"), and for type errors the expected and found types.
- A fix is a set of text edits that is valid on its own, such as inserting an
  annotation the compiler inferred, rewriting a removed spelling, or replacing
  `x[i]` with `x.fetch(i)` where the result must not be `nil`. A diagnostic with
  more than one plausible repair offers none rather than guessing.
- `vibes check --json` prints one JSON object per diagnostic. `vibes fix FILE`
  applies every fix and rechecks, repeating until no fix applies. The library
  returns the same data from compilation errors.

### Conditions

- A condition is `bool`. Optional values are tested with `x == nil` or
  `x != nil`, which narrow `x` in the guarded branch.
- `!`, `&&` and `||` take and return `bool`. A default for an optional value is
  written as a conditional.
- `x&.m` remains the way to call through an optional value.

### Exhaustive `case`

`case` on an enum value must have a `when` for every member or an `else`, so
adding a member makes the compiler list every `case` that needs it. The same
applies to `bool`. Other `case` subjects keep ordinary matching.

### Type aliases

- `type Reward = { id: string, points: int }` at top level, or in a module or
  class body, names a type. Aliases are transparent: `Reward` and the shape it
  names are the same type.
- An alias may refer to other aliases but not to itself. Recursive types may be
  proposed separately.

### Prelude

- `vibes prelude` prints the builtin globals, namespaces and members of every
  type as Vibescript declarations with full signatures, generic block types
  included, in a stable order.
- `Engine::prelude()` returns the same text extended with the host's registered
  functions, capabilities and globals.
- The prelude is generated from the signature table ADR-007 requires, so it
  cannot drift from what the compiler checks.

### Division

- `/` is true division. For two integers it returns a `float` and raises when
  the result is out of `float` range.
- `//` is floor division. It returns an `int` for integers of any size and a
  floored `float` otherwise.
- `%` is the floored remainder, consistent with `//`.
- Money and durations keep their own division rules.
- After an operand, `//` lexes as the operator. The empty regex literal `//` is
  removed.

### No nil padding

`fill` and `insert` past the end of an array raise instead of padding the gap
with nil, so an `array<T>` never gains elements its type excludes.

### Static `require`

`require("reports/format", as: "fmt")` takes string literals. A module's
exports have the types their declarations give them, and the compiler resolves
and checks them before the requiring script runs.

### Removed dispatch by name

`send`, `public_send` and `respond_to?` are removed. Code that chose a member
from data uses `case`, exhaustive over an enum where possible. Capability
methods named `send` are unaffected.

## Migration

The ADR-007 migration applies these changes in the same pass. `vibes fix`
rewrites removed spellings, drops the parentheses of calls without arguments,
converts `do...end` blocks to braces and symbol keys to strings, indexes hash
fields read with a dot, moves keyword parameters after a bare `*` with the type
of a literal default, and rewrites integer `/` to `//` using the operand types
the compiler has at that point, so existing arithmetic keeps its results.
Truthiness tests on non-`bool` values, dynamic `require` and dispatch by name
need rewriting by hand; each has a diagnostic that says so.

## Consequences

Easier:

- A model has one spelling to produce for every operation and construct, and
  one meaning per operator and condition.
- A failed compile can usually be repaired mechanically, and hosts can put the
  exact available API in a model's context.
- The builtin surface to type, test and document shrinks, and the lexer and
  parser lose their percent-literal, `unless`/`until` and block-binding cases.
- Dispatch is always visible in source, so private methods and capabilities are
  reachable only as written.

Harder:

- Scripts written from Ruby habits hit more compile errors at first: synonyms,
  truthiness, `do...end`, symbol keys and integer `/`. Every one comes with a
  precise diagnostic, and most with a fix.
- Diagnostic codes and the prelude format become compatibility surfaces.

## Alternatives considered

### Keep synonyms as deprecated aliases

Rejected. Aliases that still compile keep the surface as large as before, and
models keep emitting them.

### Require parentheses on every call with arguments

Not chosen. Parenless calls are part of the language's character. Removing
`do...end` blocks removes their worst interaction.

### Allow `if x` as a nil test

Rejected. It would be a second nil test beside `x == nil`, and a truthiness rule
models from other languages misread.

### Allow `send` only with a literal name

Rejected. A literal `send` is a direct call written less clearly.

## Addendum: required regex captures (2026-09-27)

Accepted after the [AI authoring evaluation](../authoring-evaluation.md):
`match_data.fetch(group: number | string) -> string` extends the existing
required-read operation to captures. Missing and non-participating groups raise
`RuntimeError` with `ErrorKind::Argument`, as array/hash `fetch` does; optional
`[]` reads are unchanged. Numeric indices preserve `[]` semantics, including
negative indices and truncated floats. Names select captures, including names
that collide with public fields; no default or block is accepted. V0107 may
therefore rewrite an optional capture read to `fetch` when a string is required.

## Addendum: typed JSON failures (2026-09-27)

Accepted after the same evaluation: a valid JSON value that does not fit
`JSON.parse_as`'s requested type raises `TypeError`, matching `.as(T)`.
Its Rust kind remains `ErrorKind::Type`, and existing expected/actual type
details remain intact. Malformed JSON keeps `RuntimeError` and
`ErrorKind::Json`, so a `rescue TypeError` handles validation failures alone.
This applies to scalar, collection, shape and nominal type mismatches.

## Links

- [ADR-006: Slim the language for predictable sandboxing](006-slim-language-for-predictable-sandboxing.md)
- [ADR-007: Static types with local inference](007-static-types.md)

## Addendum: integer powers (2026-09-27)

`int ** int` keeps type `int`. Negative integer exponents raise `ArgumentError`, including through `**=`, with a hint to use a float base (`2.0 ** -1`). This removes an implicit float result that violated the static type. Non-negative powers, bigint promotion and floating-point powers retain their behavior; there is no separate `pow` builtin.

## Addendum: total float ordering (2026-09-27)

Float `<=>` always returns `int`, ordered like Go's `cmp.Compare`: NaNs first,
NaNs equal to each other, and signed zeros equal. Stable sorting preserves
input order within those ties, and extrema keep the first tied value. All
ordering builtins, including key-based selection and float `clamp`, share
this rule. Equality and relational predicates retain IEEE semantics; NaN
remains unequal to itself and `between?` remains false for NaN. This makes
float selection deterministic without weakening the declared result type.

## Addendum: expression separators (2026-09-27)

Two expressions cannot form adjacent statements on the same line. `x = 1"0"`
reports V0001 at their gap, suggesting an operator, a comma between arguments,
or a newline or `;` between statements. The parser offers no automatic fix
because those repairs mean different things. Parenless calls keep their
existing grammar. A suffix that the multiline expression grammar leaves as a
separate expression also requires a separator; it is no longer silently run.

## Addendum: required-file function scope (2026-09-27)

A required file's function calls are lexically scoped to that file. During
`helper = helper(...)`, bypass the local being assigned, but keep the file's
function declarations in scope. The requiring script's `helper`, and exports
from other files, cannot replace the function the checker resolved. The rule
is independent of call syntax, visibility and nesting. Historical
`same_name_call_*` golden observations intentionally change where they
recorded lookup in the requiring script or an undefined-name error.

## Addendum: method name suffixes (2026-09-28)

Only method names may end in one `?` or `!`: definitions, calls (including
safe navigation and parenless calls), aliases and symbols naming methods.
Bindings, constants, nominal declarations and variables never take a suffix;
V0003 offers its removal. Immediately before a single `=`, neither character
is a suffix: `a!=b` compares, and `x?=y` is rejected. Before `==` or `=~`,
the suffix remains part of the method name.

Hash and keyword labels keep suffixes because they spell string keys, not
bindings. A suffixed shorthand label calls the method with that name;
an explicit value can read a local instead. Optional type, shape-field and
block markers keep their existing meaning and do not become part of a name.
This removes the whitespace-dependent accidental assignment in `a!=b`
without changing method calls or string keys.

Callable publication uses the parser's shared method-spelling validator:
host registrations, capability templates and factory results, callable globals,
and methods added by host callbacks follow the same rule. Infallible host
constructors remain infallible; validation happens at declaration, compilation
or import, before the name reaches scripts or the generated prelude. Qualified
host diagnostic labels are separate from published names. Opaque factory roots
may declare one terminal suffix because they can return a callable descriptor;
binding validates the name against the actual result and rejects data under a
suffixed root. Such roots have an unsigned function declaration until binding.

Bare aliases and both quoted and unquoted symbol aliases validate the new
name and target before lookup. Operators and ordinary setters remain valid;
constructing a setter may not turn a suffixed method into `ok?=`. Exported
module functions use the same definition parser. There is no dynamic
`define_method` API. Quoted-symbol repairs replace the whole literal so decoded
escapes cannot produce an edit at the wrong source byte.
