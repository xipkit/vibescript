# Checker differential tests

The [typed VM](vm.md#proven-checks) leaves out the runtime type checks the checker proves, so a program the checker accepts wrongly can store a value of the wrong type where release builds never look again. The differential tests look for such programs: they generate programs, and for each one the checker accepts they compare a release build with a build that keeps every check. The harness is `examples/checker_diff.rs` with its modules in `examples/checker_diff/`; the smoke test and the regression programs are in `tests/checker_diff.rs` and `tests/checker-diff/`.

## Keeping every check

`Engine::set_keep_type_checks(true)` (hidden from the documentation) compiles scripts, and the files they require, with the check at every boundary the checker proves, in the spirit of `debug_assert!`: parameters, including those of functions with a proven start, defaults, results of every function and method, typed locals, destructuring, `yield` arguments, block results and instance variable writes. A program that compiles must behave the same either way, apart from the checks' work, which changes counters. The option changes nothing else: the checker runs as it always does.

## Judging a program

Each program is compiled in both builds, against the same host (`host.rs`): the globals and capabilities the program declares, and three host functions every engine registers. If the checker rejects it, the verdict is the first error's code. Otherwise both builds run it under a step, memory, recursion and time limit, with the same seeded random source and the host's globals and capabilities, then make each call the host makes into it with arguments, and the harness compares the results, rendered exactly (kinds, float bits, bytes, hash order), and the output:

- **agreed**: the same observations;
- **check-failed**: only the build with checks raised a type error, so the checker proved something false;
- **mismatch**: any other difference;
- **unexpected-error**: both raised an error the checker rules out in a program it accepts, such as a name that does not exist or an operator on operands it does not take; failed casts, `JSON.parse_as`, a `nil` result of an instance method whose class the checker does not prove, hash key types no string satisfies, builtins' range errors and a host call's arguments that its function's parameters refuse are legitimate;
- **panic**, and **compile-mismatch**, when only one build compiles;
- **inconclusive** when a limit stops either run.

An agreed program is judged again with each top-level local declared with the type the checker inferred for it, as `vibes repl` declares a session's variables, so the build with checks verifies every inferred local type as well.

A check of a result or a run that takes longer than two minutes is a hang: the harness writes the program and exits with status 3.

## Programs

- **Generated** (`generate.rs`): a type-directed generator that models each local's declared and narrowed type and builds expressions of a wanted type from literals, locals, operators, builtin members and what it declares: functions, classes with class variables, class methods and operator methods (`+`, `==`, `<`, `[]` and `[]=`), enums, a module with a nested module, and a required file that may require another, or, in an unsound form, require it back. A third of the programs run against a host: globals of JSON types, typed or `any`, capabilities whose methods take arguments and blocks, with and without signatures, host functions, and calls from the host into a function with positional and keyword arguments, a few of the wrong type. Keyword and default parameters, blocks and `&block` parameters, early `return`, `break` and `next`, `retry`, `case` over unions, optionals and enums, and tuples and shapes all appear, and one program in eleven is large, with tens of statements and hundreds of lines. Some statements are deliberately risky: narrowing cancelled by a reassignment in a block, closure, loop, rescue or branch, or before a `retry`, narrowed instance variables across method calls, narrowing of `any`, writes through shapes and tuples, symbols where enums are expected, break values of other types, including out of a host method's block, and properties read before `initialize` assigns them. A sound checker rejects the unsound variants; the others must agree.
- **Builtins** (`builtins.rs`), a quarter of the generated seeds: calls of every signature in `src/signatures/builtins.vibe` with arguments of its parameter types, so a runtime result outside the declared return type fails the build with checks.
- **Corpus edits** (`mutate.rs`): type-changing edits of the language corpus cases and the site, upstream and glue programs, such as widening a declared type, flipping a nil test or replacing a literal with one of another type.

Every program comes from its seed alone.

## Commands

The smoke test judges a few hundred fixed seeds and every regression program in `tests/checker-diff`, in the normal test suite:

```sh
./scripts/cargo test --offline --test all checker_diff
```

Long runs use the example, built with the `gate` profile:

```sh
./scripts/cargo build --offline --profile gate --example checker_diff
target/gate/examples/checker_diff run --from 0 --count 1000000 --jobs 12 --out DIR --source mixed
```

`--source` is `generated`, `corpus` or `mixed`, where a fifth of the seeds are corpus edits. `run` prints totals and writes each finding, up to 200 of each kind, to `DIR` as a program headed by comments that describe it; after a hang, continue from the seed after the one in `DIR/hang-SEED.vibe`. Other commands:

```sh
checker_diff judge FILE...     # judge programs, such as findings
checker_diff minimize FILE...  # remove lines while the finding stays, into FILE.min.vibe
checker_diff generate SEED     # print a seed's program; add `corpus` for an edit
```

A program's host comes first, one directive a line: `#@ global NAME: TYPE = JSON`, or `#@ global NAME = JSON` for an `any` global; `#@ capability NAME`, `store` or `loose`; and `#@ call FUNCTION {"args": [...], "keywords": {...}}`. Each required file follows under a `#@ file PATH` line, then the script under `#@ main`.

A fixed finding becomes a regression program in `tests/checker-diff`, whose first line says what it must now do: `# expect: rejected CODE`, for a program the checker now rejects, or `# expect: agreed`.

## Known disagreements

ADR-008's 2026-09-27 addenda resolved the numeric and required-file findings: negative integer powers raise `ArgumentError`, float `<=>` orders NaN first, and a required file's calls resolve within it. The numeric integration tests also run with every type check kept. The harness no longer counts any finding as known. Two remain, which it avoids generating:

- A class with no `initialize` whose properties a setter assigns reads them as `nil` before then. Its methods keep their result check, and a `nil` surfaces as a type error where it is used.
- A string repeated a float number of times beyond the int range raises "unsupported multiplication operands", a type error, instead of a range error.

## Syntax

The generator once avoided syntax the parser read another way. Each is now a parser fix or a rule of the language:

- Fixed in the parser: a symbol after a keyword such as `then`, `else` or `rescue`, which read as a keyword label; a symbol statement on the line after a call without parentheses, which read as that call's label; a tuple parameter type whose first element is optional, a shape or a tuple, which read as a removed keyword default; and an index that abuts the end of an expression spanning lines, as in `(case x ... end)[0]`, which read as a second statement. Adjacent expressions are now an error, so none of these can hide a statement.
- Rules of the language: an assignment is a statement, not a value, so it cannot appear in parentheses or as a `when` branch; a `begin` whose rescues are all empty needs an `ensure`; and a nested tuple in a type written as a call's argument, as in `JSON.parse_as(text, [[int, int], int])`, is an array of values, so such a type is named through an alias.

## Not covered yet

The generator does not reach async host methods, capabilities built per call by a factory, host methods whose signatures name the script's classes or enums, host values of script classes, keyword arguments to host methods, REPL sessions, or regular expressions, times and money values in the typed positions it fills, which come only from the builtin calls. The language has no generic functions of its own; generics with bounds and overloads by arity are reached through the builtin signatures only.
