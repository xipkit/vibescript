# ADR-004: Infer local types and check typed boundaries statically

## Status

Superseded by [ADR-007](007-static-types.md) - 2026-09-24. Accepted - 2026-07-09

## Decision

Vibescript uses implicit local type inference on the check path. Locals take
the types of the expressions assigned to them, annotations are compile-time
facts as well as runtime contracts, and the checker reports an error wherever
known types contradict. The CLI exposes a whole-file gate through `vibes
check`, an exact invocation gate through `vibes run -check`, and a whole-snippet
gate through `vibes run -check -e`.

The governing principle is: **error on known contradictions, permit unknowns,
and make unknowns easy to validate at boundaries.**

To make boundary validation ergonomic, Vibescript provides
`JSON.parse_as(raw, type)`: it parses JSON and validates the result against a
shape, container, scalar, nullable, or union contract in one step, and the
checker treats its result as that declared type.

This is still gradual typing, with a substantially better inference engine. It
is not a switch to a whole-program proof system.

## Context

Vibescript supports optional type annotations and enforces them at runtime. The
pre-ADR checker caught direct contract violations such as a string literal
passed to an `int` parameter:

```vibe
def takes_int(value: int)
  value
end

takes_int("1")
```

It did not carry local type facts through bindings:

```vibe
def takes_int(value: int)
  value
end

value = "1"
takes_int(value)
```

The shipped checker now rejects this program before execution. That is the
contract required by deployment gates and AI-generated code: if a host can
prove a typed-boundary error before deployment, it should reject it before
runtime.

Crystal proves that a Ruby-shaped language can infer nearly everything, but its
inference works because every expression must ultimately have a compile-time
type. That requirement is exactly what makes JSON painful in Crystal: dynamic
payloads become `JSON::Any`, unions, casts, or declared serializable structs.
Adopting that model wholesale would reintroduce the same complexity precisely
where Vibescript's main use case lives — host payloads, loose JSON, and
capability data flowing into short scripts. Vibescript also keeps Ruby-like
runtime semantics: values are dynamically tagged, hosts inject globals and
capabilities, and dispatch can be dynamic.

The problem, then, is to get Crystal's useful inference for local code without
inheriting its requirement that every value be statically typed.

## Design

Locals receive inferred types, and known contradictions are static errors:

```vibe
name = "Mauricio"   # inferred string
count = 1           # inferred int
count = name        # check error: count is int, name is string
```

Typed boundaries remain optional:

```vibe
def create_user(name: string) -> User
  User.new(name)
end
```

Dynamic inputs remain allowed:

```vibe
body = JSON.parse(raw)
create_user(body["name"])
```

Here `body["name"]` is unknown statically, so `vibes check` accepts the call and
`create_user` validates the value at runtime, exactly as it does today. Unknown
values are never rejected merely because the checker cannot prove their type.

Apps wanting stronger guarantees validate once at the edge:

```vibe
body = JSON.parse_as(raw, {
  name: string,
  email: string
})

create_user(body["name"])
```

After that validation, `body` is a known shape, `body["name"]` is a known
`string`, and everything downstream is inferred and checked. Shape-annotated
parameters already provide the same edge validation at function boundaries;
`JSON.parse_as` covers the common inline case where a payload is parsed and
consumed in the same scope.

Concretely, the checker maintains a local type environment while walking code:

- Assignments bind the inferred type of the right-hand side to the local.
- Sequential reassignment to a conflicting type is a static error; assignments
  in sibling branches merge into unions.
- Annotated parameters enter the function body with their declared type.
- Known function calls check argument types and expose annotated or inferred
  return facts to the caller.
- Annotated returns check the inferred type of explicit and implicit return
  expressions.
- Operators reject operands known to be invalid.
- Shape-typed values carry field-level facts, so indexing with a known key
  yields the field's type.
- `nil` checks and supported `is_type?` predicates narrow union types. Every
  finite known arm must satisfy a typed boundary; one compatible arm cannot
  hide another known mismatch. `any` and unknown arms remain gradual and are
  checked at runtime, but they do not hide incompatible known arms.
- Constructors and resolved class values carry nominal facts. Unknown or
  overrideable dynamic dispatch remains unknown.
- Known pure member calls and compatible modeled writes preserve
  mutable-container facts. Other known mutation, unregistered members, blocks,
  impure arguments, dynamic dispatch, and aliases to nested mutable values
  discard facts that the checker can no longer prove.

When the checker proves a violation, a check reports an error such as:

```text
call to takes_int argument value expected int, got string
```

This design takes the useful parts of Crystal:

- Locals receive inferred types.
- Operators reject known-invalid operands.
- Calls and returns enforce declared contracts.
- AI-generated code gets concrete diagnostics from `vibes check`.

And it preserves the useful parts of a scripting language:

- Function annotations remain optional.
- Untyped JSON and host values can flow dynamically.
- Unknown values are not rejected merely because the checker cannot prove their
  type.
- Runtime checks cover the uncertainty.

`any` remains an explicit escape hatch: it tells the checker to stop proving
facts about a value. The core rule does not change: known mismatches are static
errors; unknowns defer to runtime contracts.

## Checking scopes

The same gradual rule is available at several scopes:

- `vibes check script.vibe` and `Script.CheckWarnings` check top-level code,
  functions, and class methods across the compiled script. This is the
  whole-file deployment and CI gate.
- `vibes run -check [-function name] script.vibe [args...]`,
  `CheckWarningsForFunction`, and `CheckWarningsForCall` check the execution
  path of one function call. `CheckWarningsForCall` includes the supplied
  positional arguments, keywords, globals, and capability contracts.
- `vibes run -check -e 'source'` checks the inline entrypoint and every function
  or method declared by the snippet, including declarations the entrypoint
  never calls.
- `CheckedCall` applies the exact-call check and executes only when it produces
  no diagnostics. Ordinary `Call` still executes and relies on runtime
  contracts.

A clean result means only that the selected scope contains no contradiction the
checker can currently prove. It is not proof of full type safety: unknown JSON,
host values, opaque `any` or unknown union arms, and dynamic dispatch can still
fail at a runtime contract.

## Non-goals

- No Crystal-style requirement that every expression in every script be
  statically typed before execution.
- No change to runtime value representation or Ruby-like dynamic dispatch.
- No removal of runtime boundary checks; the runtime remains the final guard for
  dynamic host data and unchecked paths.
- No whole-program inference across unknown host globals, arbitrary dynamic
  dispatch, or capability implementations.

## Consequences

Typed annotations serve as checker facts while remaining runtime guards. Local
contradictions that a user reasonably expects a type checker to catch are caught
before execution, and AI-generated scripts get a tighter correction loop: wrong
operators, wrong argument types, wrong return types, and shape mismatches produce
concrete diagnostics before a host deploys the script.

The JSON path gets a one-step validated entry. A script that calls
`JSON.parse_as` at the edge gets full static checking downstream without any
other annotations.

The implementation is more complex. The checker maintains a local type
environment, joins branch facts, represents unknown values, and takes care not
to guess: when host data or dynamic dispatch cannot be proven, it defers to
runtime checks rather than rejecting the script. `vibes check` is a semantic
pass with its own compatibility surface.

`JSON.parse_as` carries specific costs: braced shape literals are legal as
first-class expressions when unshadowed, while non-shape type literals are
recognized only in parenthesized call arguments. Those paths add parser and
runtime surface, and validation failures must keep the same semantics as
existing typed-boundary errors.

## Alternatives Considered

### Keep the current gradual contract checker

Rejected. Runtime-only enforcement catches errors too late for deployment gates,
and the pre-ADR check path missed simple local contradictions such as
`value = "1"; takes_int(value)`.

### Adopt Crystal's static model wholesale

Rejected. Full inference requires every expression to have a compile-time type,
which forces `JSON::Any`-style wrappers, casts, and declared structs onto
dynamic payloads — the exact data Vibescript scripts exist to handle. We want
Crystal's inference for local code, not its obligations for dynamic data.

### Require annotations everywhere

Rejected. Mandatory annotations make small scripts heavier and do not solve
dynamic host data by themselves. Local inference gives most of the benefit for
typed code while keeping scripts readable.

### Lean on `any` for gradual typing

Rejected as the primary model. `any` is useful as an escape hatch, but making it
central would hide exactly the class of mistakes `vibes check` should catch, and
its contagious propagation rules complicate the language.

### Treat unknown values as static errors

Rejected for the base checker. That would make gradual adoption and
host-provided data painful. Deployment policies may require stricter annotations
at entrypoints, but the language-level checker must distinguish "known wrong"
from "not statically known."
