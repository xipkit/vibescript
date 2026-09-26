# The type checker

Every Vibescript program is type checked before it runs, and a program with a type error does not compile ([ADR-007](adr/007-static-types.md)). The checker also reports the spellings [ADR-008](adr/008-canonical-surface-for-ai-authors.md) removed. The [language guide](language.md) describes the rules from an author's side; this page describes the checker, its entry points and its diagnostics.

Every engine, the command line, the REPL, the language server and the test runner type check every compile; there is no mode without static types.

## What it proves

A program that compiles has no type errors except at the edges where dynamic data enters. The checker proves that:

- every call names a function, method, namespace member, host function or capability that is in scope, and its positional arguments, keywords and block match one of the callee's signatures;
- every value has the type its position expects: arguments, results, fields, collection elements, block results and assignments;
- optional values are tested before use and `any` values are narrowed;
- locals keep the type of their first assignment and are assigned on every path before they are read;
- instance variables are declared and assigned by `initialize`;
- shapes are read and written only at keys they declare;
- conditions and logical operands are `bool`;
- `case` over an enum or `bool` handles every value;
- every `require` names a literal module that resolves, and calls into it match its declarations.

At runtime the remaining checks are those at the edges: arguments a host passes to `Script::call` and its declared globals and capabilities, `JSON.parse_as`, checked casts with `as`, and the results of host capabilities with contracts. Builtins still raise on invalid values, such as a missing key passed to `fetch`, and every call still runs under its step, memory, recursion and time limits. Method visibility is checked statically too: calling a private method through a receiver, or a protected one from outside its class, is a compile error (V0208).

## How it works

The checker reads the parsed program once, after the parser. It first collects every declaration, so the order of declarations does not matter: functions, classes with their properties and instance variables, enums, modules, type aliases, host functions with their published signatures, and the globals and capabilities the host declares. It resolves every signature and alias, then checks each function body once, from its own signature and the signatures of what it calls; it never reads a callee's body. Required files named by literal `require` calls are checked the same way, and their exports' declarations type the calls into them.

Types are interned, so comparing two types compares identifiers. A local's type and its narrowing live on one flow state with a trail of changes, so branches and loops join in time proportional to what they changed rather than to the number of locals. Builtin members come from the signature table, [`src/signatures/builtins.vibe`](../src/signatures/builtins.vibe), whose generic members are instantiated at each call: `array<T>#map` binds `T` to the receiver's element type and infers `U` from the block. An overloaded name selects its signature by the number of positional arguments, the keyword names and the presence and arity of a block, never by argument types.

The work is linear in the program. The checker counts it deterministically, and compilation charges that count to the step quota when it runs under limits, as a cold `require` inside a call does. Except on WASI, sources longer than 1 KiB are checked on a thread with a 64 MiB stack, because the checker recurses once per level of syntax, which the parser bounds to 1,024. WASI has no threads, so there the checker refuses syntax more than 128 levels tall with `V0001` instead of exhausting the stack (see [platform support](platforms.md#stack-use)).

The same pass also records the static type of every member call's receiver. The removed-spelling rules use it, so that `size` on an array is reported and rewritten to `length` while a class's own `size` method is left alone.

## Entry points

- `Engine::compile` reports every diagnostic as a compile error; `Error::diagnostics()` returns them.
- `Engine::type_check(source)` checks without compiling and returns every diagnostic, warnings included, with the receiver types, and the types of the top-level locals and result, which a host continuing a session declares for its next script (see [sessions](sessions.md)).
- `Engine::check_entry_arguments(source, function, count)` checks that a command line can call `function` with `count` string arguments.
- `vibes check FILE` prints the diagnostics with their source lines; `--json` prints one JSON object per diagnostic. See [the command line](cli.md#vibes-check).
- `vibes fix` applies every machine-applicable fix and checks again until none applies.
- `vibes lsp` publishes the diagnostics as the document changes and offers the fixes as quick fixes.

## Diagnostics

Each diagnostic has a stable code, a severity, a primary span, optional labelled spans such as the declaration a value is checked against, the expected and found types where they apply, and zero or more fixes. A fix is a set of text edits valid on its own, such as inserting the annotation the checker inferred, rewriting a removed spelling, or replacing `x[i]` with `x.fetch(i)`; a diagnostic with more than one plausible repair offers none rather than guessing. A fix marked `always` may be applied without asking, and `vibes fix` applies only those. Codes keep their meaning across releases; the message text may improve.

Two diagnostics are warnings, which do not stop compilation: `V0121`, a nil test whose result the value's type already decides, and `V0120` when an `is_type?` test can never be true. Every other diagnostic is an error.

| Code | Name | Meaning |
| --- | --- | --- |
| `V0001` | `syntax` | The source does not parse. |
| `V0002` | `hash-argument` | A hash literal is passed to a call without parentheses, where `{` after the call starts a block; the call's arguments need parentheses. |
| `V0101` | `type-mismatch` | A value's type is not assignable to the type its position expects. |
| `V0102` | `local-type-changed` | A local is assigned a value of a type other than the one its first assignment fixed. |
| `V0103` | `needs-type` | `nil`, `[]` or `{}` appears where no declared type gives it one. |
| `V0104` | `condition-not-bool` | A condition is not a `bool`. |
| `V0105` | `logical-not-bool` | An operand of `!`, `&&` or `\|\|` is not a `bool`. |
| `V0106` | `any-use` | A value of type `any` is used before it is narrowed. |
| `V0107` | `optional-use` | A value that may be `nil` is used where `nil` is not accepted. |
| `V0108` | `no-operator` | An operator is not defined for its operand types. |
| `V0109` | `integer-division` | Retired: it rejected `/` on two ints before `/` became true division. No diagnostic reports it, and the number is not reused. |
| `V0110` | `unknown-field` | A shape is read or written at a key it does not declare. |
| `V0111` | `dynamic-key` | A shape is indexed with a key known only at runtime. |
| `V0112` | `not-indexable` | A value of this type cannot be indexed this way. |
| `V0113` | `tuple-index` | A tuple is indexed outside its elements. |
| `V0114` | `non-exhaustive-case` | A `case` over an enum or `bool` neither names every value nor has an `else`. |
| `V0115` | `bound` | A generic member's bound is not met, such as `sort` on a union element type. |
| `V0116` | `unknown-type` | An annotation names a type that does not exist. |
| `V0117` | `return-without-type` | A function without `-> T` returns a value. |
| `V0118` | `missing-parameter-type` | A parameter does not declare its type. |
| `V0119` | `yield-value` | The value of `yield` is used, but the block declares no result type. |
| `V0120` | `cast` | A checked cast or `is_type?` can never succeed for the value's type. |
| `V0121` | `unreachable-narrowing` | A nil test or type test on a value whose type already decides it. |
| `V0201` | `undefined-name` | A name does not refer to a local, function, constant or type in scope. |
| `V0202` | `unassigned-local` | A local is read where it is not assigned on every path. |
| `V0203` | `unknown-member` | A type has no member with this name. |
| `V0204` | `undeclared-ivar` | An instance variable is read or assigned without a declaration. |
| `V0205` | `uninitialized-ivar` | An instance variable without a default is not assigned on every path through `initialize`. |
| `V0206` | `unknown-enum-member` | A symbol or constant does not name a member of the enum. |
| `V0207` | `block-not-value` | A block parameter is used as a value. |
| `V0208` | `visibility` | A private method is called with a receiver, or a protected one from outside its class's own methods. |
| `V0209` | `duplicate-name` | A function, method or alias takes a name its scope already defines. |
| `V0210` | `reserved-name` | A function or alias takes a reserved name: `require`, which the compiler resolves statically, or `__main__`. |
| `V0301` | `no-overload` | No signature accepts the call's positional arguments, keywords and block. |
| `V0302` | `unknown-keyword` | A call passes a keyword the signature does not declare. |
| `V0303` | `missing-keyword` | A call omits a required keyword argument. |
| `V0304` | `missing-block` | A call omits a required block. |
| `V0305` | `unexpected-block` | A call passes a block to a function that takes none. |
| `V0306` | `block-parameters` | A block declares parameters the signature does not provide. |
| `V0307` | `unguarded-yield` | A `yield` to an optional block is not guarded by `block_given?`. |
| `V0308` | `undeclared-block` | A function yields but does not declare its block as a typed `&` parameter. |
| `V0309` | `dynamic-require` | A `require` names its module or alias with something other than a string literal. |
| `V0310` | `not-callable` | A value is called but is not a function. |
| `V0401` | `removed-name` | A builtin member, function or namespace member is called by a removed name, such as `size` for `length`. |
| `V0402` | `nil-predicate` | `nil?` is removed; `x == nil` tests for nil. |
| `V0403` | `identity-equality` | `eql?` and `equal?` are removed; `==` compares values. |
| `V0404` | `identity-call` | `itself`, `tap` and `yield_self` are removed; the expression itself replaces them. |
| `V0405` | `dispatch-by-name` | `send`, `public_send` and `respond_to?` are removed; call the member directly. |
| `V0406` | `do-block` | A block is written `do ... end`; blocks are written with braces. |
| `V0407` | `unless` | `unless` is removed; `if` takes the negated condition. |
| `V0408` | `until` | `until` is removed; `while` takes the negated condition. |
| `V0409` | `symbol-key` | A hash is indexed with a symbol; hash keys are strings. |
| `V0410` | `percent-literal` | A percent literal such as `%w[a b]`; arrays are written as array literals. |
| `V0411` | `hash-new` | `Hash.new` is removed; `{}` with a declared type makes a hash. |
| `V0412` | `empty-parentheses` | A call without arguments is written with `()`; it takes no parentheses. |
| `V0413` | `type-name` | A builtin type name is spelled other than in lowercase, or as `object` for `hash`. |
| `V0414` | `keyword-parameter` | A keyword parameter is declared as `name:`, `name: default` or `name: T:`; keyword parameters follow a bare `*`. |
| `V0415` | `field-access` | A hash or shape field is read or written with a dot; dot calls methods, and a field is indexed as `h["name"]`. |
| `V0416` | `scoped-call` | A function or method is called with `::`, as in `JSON::parse(x)`; `::` names only constants, nested types and enum members, and a dot calls. |

`vibescript::diagnostic::codes()` returns this registry, retired codes included and marked by `CodeInfo::retired`, and `Diagnostic::to_json` gives the JSON form `vibes check --json` prints:

```text
{"code":"V0107","name":"optional-use","severity":"error","file":null,"span":{"start":24,"end":31,"line":2,"column":1,"end_line":2,"end_column":8},"message":"this value may be nil (array<int>?); test it with `!= nil` before indexing it","expected":null,"found":null,"labels":[],"fixes":[...]}
```

A deliberately invalid example in this documentation is fenced as ```` ```vibe error=V0107 ````, and `tests/docs.rs` checks that it fails with exactly those codes while every other example compiles cleanly.
