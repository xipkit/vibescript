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

1. **Diagnostics carry codes and fixes.** Every compile error has a stable code,
   an exact span, the expected and found types where they apply, and, when the
   repair is unambiguous, a machine-applicable edit. `vibes check --json` emits
   them, `vibes fix` applies the edits, and the language server offers the same
   edits as code actions.
2. **One name per operation.** Every builtin function and member has exactly one
   spelling. A removed synonym is a compile error whose fix renames it.
3. **Conditions are booleans.** `if`, `unless`, `while`, `until`, the ternary,
   `!`, `&&` and `||` take `bool`, or an optional `T?` tested for `nil`. There is
   no other truthiness.
4. **`case` over an enum is exhaustive.** It must name every member or have an
   `else`.
5. **Type aliases.** `type Name = T` names a type.
6. **A typed prelude.** `vibes prelude` prints every builtin signature, and the
   embedding API prints a host's capabilities and globals the same way, as text a
   host can give a model as context.
7. **`/` divides, `//` floors.** `7 / 2` is `3.5` and `7 // 2` is `3`.
8. **`require` is static.** Module names and aliases are string literals.
9. **No dispatch by name.** `send`, `public_send` and `respond_to?` are removed.

Parenless calls stay. Profiling on quota exhaustion is deferred until after 1.0.

## Context

Two properties decide whether a model writes working code in a language: how
many valid ways there are to say the same thing, and how directly a mistake can
be repaired from the error it produces. Vibescript inherited Ruby's generous
answers to both. It has 441 builtin member names, many of them synonyms:
`size`, `length` and `count`; `key?`, `has_key?`, `member?` and `include?`;
`to_s` and `string` on every type; singular and plural duration units; and 13
synonym pairs on `Time` alone, such as `tv_usec`, `gmtoff`, `isdst` and
`xmlschema`, which come from C. Each synonym is one more name to learn,
type-sign, test and document, and a source of inconsistent generated code.

Other inherited rules surprise models trained mostly on Python and
JavaScript. Every value except `nil` and `false` is truthy, so `if count`
succeeds for `0`. Integer `/` floors, so `7 / 2` is `3`. Dispatch by a runtime
name defeats static typing (ADR-007) and reaches private methods: a script that
runs `account.send(payload["action"].to_sym)` on untrusted JSON can be made to
call any method on the account. Nothing in the 286-program corpus dispatches by
name; its only `.send(...)` calls are capability methods that happen to be
named `send`.

## Design

### Diagnostics

- Codes are stable identifiers such as `V0101`, grouped by area (syntax, types,
  names, calls). A code keeps its meaning across releases; the message text may
  improve.
- Each diagnostic has a primary span, optional secondary spans ("declared
  here"), and for type errors the expected and found types.
- A fix is a set of text edits that is valid on its own, such as inserting an
  annotation the compiler inferred, renaming a synonym, or replacing `x[i]` with
  `x.fetch(i)` where the result must not be `nil`. A diagnostic with more than
  one plausible repair offers none rather than guessing.
- `vibes check --json` prints one JSON object per diagnostic. `vibes fix FILE`
  applies every fix and rechecks, repeating until no fix applies. The library
  returns the same data from compilation errors.

### Canonical names

The rule is: keep the spelling most models already know, prefer the
`to_*` conversion family, and keep one form per concept.

| Removed | Canonical |
| --- | --- |
| `size`, and `count` without an argument or block | `length` |
| `has_key?`, `member?`, `include?` on hashes | `key?` |
| `has_value?` | `value?` |
| `member?` on ranges | `include?` |
| `string` | `to_s` |
| singular units: `second`, `minute`, `hour`, `day`, `week` | plural: `seconds`, `minutes`, ... |
| `find_index` | `index` |
| `mon`, `mday` | `month`, `day` |
| `tv_sec`, `tv_usec`, `tv_nsec` | `to_i`, `usec`, `nsec` |
| `gmt_offset`, `gmtoff` | `utc_offset` |
| `gmtime`, `getutc`, `getgm` | `utc` |
| `gmt?`, `isdst` | `utc?`, `dst?` |
| `xmlschema`, `rfc3339` | `iso8601` |
| `rfc822` | `rfc2822` |

`count` keeps its counting forms, `count(value)` and `count { ... }`. The
prelude is the complete, authoritative list; any synonym found while writing it
is resolved by the same rule.

### Conditions

- A condition is `bool`, or `T?`, which is true when the value is not `nil` and
  narrows it to `T` in the guarded branch.
- `!`, `&&` and `||` take `bool` operands. The one exception is `a || b` with
  `a: T?` and `b: T`, which has type `T` and supplies a default.
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
renames synonyms and rewrites integer `/` to `//`, using the operand types the
compiler has at that point, so existing arithmetic keeps its results. Truthiness
tests on non-optional values, dynamic `require` and dispatch by name need
rewriting by hand; each has a diagnostic that says so.

## Consequences

Easier:

- A model has one name to produce per operation, and one meaning per operator
  and condition.
- A failed compile can usually be repaired mechanically, and hosts can put the
  exact available API in a model's context.
- The builtin surface to type, test and document shrinks.
- Dispatch is always visible in source, so private methods and capabilities are
  reachable only as written.

Harder:

- Scripts written from Ruby habits hit more compile errors at first: synonyms,
  truthiness and integer `/`. Every one comes with a precise diagnostic, and
  most with a fix.
- Diagnostic codes and the prelude format become compatibility surfaces.

## Alternatives considered

### Keep synonyms as deprecated aliases

Rejected. Aliases that still compile keep the surface as large as before, and
models keep emitting them.

### Require parentheses on every call with arguments

Not chosen. Parenless calls are part of the language's character, and the
remaining ambiguities are handled by the parser's diagnostics.

### Allow `send` only with a literal name

Rejected. A literal `send` is a direct call written less clearly.

## Links

- [ADR-006: Slim the language for predictable sandboxing](006-slim-language-for-predictable-sandboxing.md)
- [ADR-007: Static types with local inference](007-static-types.md)
