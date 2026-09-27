# Checker differential tests

The [typed VM](vm.md#proven-checks) leaves out the runtime type checks the checker proves, so a program the checker accepts wrongly can store a value of the wrong type where release builds never look again. The differential tests look for such programs: they generate programs, and for each one the checker accepts they compare a release build with a build that keeps every check. The harness is `examples/checker_diff.rs` with its modules in `examples/checker_diff/`; the smoke test and the regression programs are in `tests/checker_diff.rs` and `tests/checker-diff/`.

## Keeping every check

`Engine::set_keep_type_checks(true)` (hidden from the documentation) compiles scripts, and the files they require, with the check at every boundary the checker proves, in the spirit of `debug_assert!`: parameters, including those of functions with a proven start, defaults, results of every function and method, typed locals, destructuring, `yield` arguments, block results and instance variable writes. A program that compiles must behave the same either way, apart from the checks' work, which changes counters. The option changes nothing else: the checker runs as it always does.

## Judging a program

Each program is compiled in both builds. If the checker rejects it, the verdict is the first error's code. Otherwise both builds run it under a step, memory, recursion and time limit, with the same seeded random source, and the harness compares the results, rendered exactly (kinds, float bits, bytes, hash order), and the output:

- **agreed**: the same observations;
- **check-failed**: only the build with checks raised a type error, so the checker proved something false;
- **mismatch**: any other difference;
- **unexpected-error**: both raised an error the checker rules out in a program it accepts, such as a name that does not exist or an operator on operands it does not take; failed casts, `JSON.parse_as`, a `nil` result of an instance method whose class the checker does not prove, hash key types no string satisfies and builtins' range errors are legitimate;
- **panic**, and **compile-mismatch**, when only one build compiles;
- **inconclusive** when a limit stops either run.

An agreed program is judged again with each top-level local declared with the type the checker inferred for it, as `vibes repl` declares a session's variables, so the build with checks verifies every inferred local type as well.

A check of a result or a run that takes longer than two minutes is a hang: the harness writes the program and exits with status 3.

## Programs

- **Generated** (`generate.rs`): a type-directed generator that models each local's declared and narrowed type and builds expressions of a wanted type from literals, locals, operators, builtin members and the functions, methods, classes, enums, namespaces and required files it declares, with keyword and default parameters, blocks and `&block` parameters, early `return`, `break` and `next`, `case` over unions, optionals and enums, and tuples and shapes. Some statements are deliberately risky: narrowing cancelled by a reassignment in a block, closure, loop, rescue or branch, narrowed instance variables across method calls, narrowing of `any`, writes through shapes and tuples, symbols where enums are expected, break values of other types and properties read before `initialize` assigns them. A sound checker rejects the unsound variants; the others must agree.
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

A program with required files lists each under a `#@ file PATH` line, then the script under `#@ main`.

A fixed finding becomes a regression program in `tests/checker-diff`, whose first line says what it must now do: `# expect: rejected CODE`, for a program the checker now rejects, or `# expect: agreed`.

## Known disagreements

These findings are left for a language or runtime decision; long runs count the first two as `known` instead of writing each one out:

- `negative-power`: `int ** int` with a negative exponent is a float, where the checker types an `int`. Typing it `number` would reject ordinary code such as `10 ** digits`, so the runtime should decide, for example by raising.
- `nan-comparison`: `<=>` with a NaN operand gives `nil`, where the checker types an `int`.
- A required file's top-level local that shares a name with a function, as in `helper = helper()`, makes the file's functions read the requiring script's function of that name, or fail, as the golden `same_name_call_*` cases record. The checker checks a file once for every script that requires it and types the local.
- A class with no `initialize` whose properties a setter assigns reads them as `nil` before then. Its methods keep their result check, and a `nil` surfaces as a type error where it is used.
- A string repeated more times than an int holds raises "unsupported multiplication operands", a type error, instead of a range error.

## Not covered yet

The generator does not reach namespaces nested more than one level, class variables, `retry`, host functions, globals and capabilities, the calls a host makes with arguments, required files that require others, operator methods on classes, or programs larger than a few dozen lines. Regular expressions, times and money values come only from the builtin calls, not from typed positions the generator fills. The language has no generic functions of its own; generics with bounds and overloads by arity are reached through the builtin signatures only.
