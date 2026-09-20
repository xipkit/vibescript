# Command line

`vibes FILE` compiles one source file, runs its top-level statements and prints the final value as JSON on stdout. `puts`, `print` and `p` write to stdout and `warn` writes to stderr before that value. `--function NAME` calls one function instead, with `--arg JSON` positional values in order and `--kwarg NAME=JSON` keyword values. Options may appear anywhere around FILE, option values are taken verbatim, and `--` ends option parsing so a file name may start with `-`.

```sh
./scripts/cargo run --release -- examples/total.vibe --function total --arg '[10,20,30]' --stats
```

Keyword values follow `Script::call_with_keywords`: they bind by name rather than forming a trailing options hash, and a repeated name binds its last value. Every command-line value is parsed and validated before the file is read, so malformed JSON, numbers or option combinations never execute anything.

The binary offers four ways to analyze or run a file:

| Invocation | Library entry | Scope | Executes |
| --- | --- | --- | --- |
| `vibes check FILE` | `Script::check` | The whole file: top-level statements, then every function and method declaration, including unused ones, for its declared parameter types and defaults. | Nothing. |
| `vibes check --function NAME FILE` | `Script::check_function` | One declaration and whatever it reaches, for its declared parameter types and defaults; no concrete argument values. | Nothing. |
| `vibes FILE --function NAME [--arg JSON]... --check` | `Script::check_call_with_keywords` | One concrete call with the supplied values and whatever it reaches. | Nothing. |
| `vibes FILE --function NAME [--arg JSON]... --checked` | `Script::checked_call_with_keywords` | The same concrete call. | The call, only when its check is clean. |

Analysis never executes script code, host callbacks, defaults or initializers. A clean `vibes check` or `--check` prints nothing unless `--stats` is requested and exits with status 0; a clean `--checked` proceeds to execution and prints its result. Unsupported analysis is reported as `incomplete`. Dynamic values retain their runtime contracts, so a clean report can still be followed by an execution error.

## Required modules

The CLI searches the input file's directory first for calls such as `require(:helpers)`. Repeatable `--module-path DIR` options append search roots in the order supplied. Relative option paths are resolved from the process working directory; the script directory remains first even when the command runs elsewhere. Duplicate directory paths are collapsed, and missing paths or ordinary files are rejected before execution.

```sh
vibes --module-path shared --module-path vendor app/main.vibe
vibes check --module-path shared app/main.vibe
vibes app/main.vibe --module-path shared --function run --checked
```

All execution and checking modes use the same configured roots and the engine's directory-handle confinement. Required files can make relative imports within their root, such as `require('./helpers')`; a relative import from the main script still requires a module caller, as in the Go reference. Checking reads and analyzes resolved modules without executing their initializers or output helpers.

## Whole-file checking

`vibes check FILE` analyzes the whole file through `Script::check`. The top-level statements are checked in source order, then every effective function and method declaration is checked against its declared parameter types and defaults, including declarations that nothing calls. Declarations use namespace state from the top-level analysis, so a module constant assigned from a top-level variable keeps its known type. No result value is printed, and `puts`, `print`, `p` and `warn` never run.

```sh
cat > unused.vibe <<'EOF'
7
def unused(n:string) -> int
  n
end
EOF
./scripts/cargo run --release -- check unused.vibe
# unused.vibe:3:3: error in unused: Return value: expected int, got string
#   --> line 3, column 3
#  3 |   n
#    |   ^
# unused.vibe: check of the whole file found 1 error
```

The same file is clean under `vibes unused.vibe --function __main__ --check`, because the exact-call scope never reaches `unused`. Whole-file checking includes every effective callable declaration; overwritten definitions and generated accessors are excluded.

## Checking one declaration

`vibes check --function NAME FILE` analyzes one declaration through `Script::check_function`. Annotated parameters enter with their declared types, unannotated parameters stay dynamic and optional parameters include both supplied and default paths. For example, `def run(x:int=false) -> int` reports its bad default even though `--function run --arg 7 --check` is clean for the supplied value. Namespace analysis follows ordinary named-call semantics, without the top-level statements, so a declaration that depends on top-level state may report an error that the whole-file check does not.

NAME may select any declaration the library exposes:

| Selector | Declaration |
| --- | --- |
| `run` | A top-level function. |
| `__main__` | The top-level entrypoint. |
| `Class#method` | An instance method, starting from unknown receiver state without running a constructor. |
| `Namespace.method` | A static or module method, including nested names such as `Outer::Inner.method`. |
| `Class.new` | A constructor. |

```sh
./scripts/cargo run --release -- check --function 'C#read' methods.vibe
# methods.vibe:6:5: error in read: Return value: expected int, got string
#   --> line 6, column 5
#  6 |     "bad"
#    |     ^
# methods.vibe: check of C#read for its declared parameter types found 1 error
```

Method and constructor selectors belong to `vibes check`. `vibes FILE --function 'C#read' --check` and the other flat forms reject those selectors with `unknown function`. Top-level names such as `run` and `__main__` work in both forms.

`vibes check` takes no concrete arguments: `--arg`, `--kwarg`, `--check` and `--checked` are usage errors under the command, reported before the file is read. The command is recognized only as the first argument. `vibes -- check` runs a file named `check`, `vibes check -- check` checks it, and `vibes check --help` prints the command's own usage.

## Exact-call checking

`--check` analyzes the call selected by `--function`, `--arg` and `--kwarg` through `Script::check_call_with_keywords` without executing script code, host callbacks, defaults or initializers. `--checked` runs the same analysis through `Script::checked_call_with_keywords` and executes the call only when the report is clean; a rejected call prints the report instead of a result and runs no script code. Both modes require `--function` and exclude each other, because they cover one concrete call rather than a file.

The scope is exactly one call: the named function, the supplied values and whatever that call reaches. Selecting `__main__` checks the top-level entrypoint in source order. Other named calls omit ordinary top-level statements, and unused functions and methods are outside either scope. Use `vibes check` for the file as a whole or for a declaration without supplied values. See the [checker notes](checker.md) for the analysis itself.

For example, the supplied array length makes the indexed loop in `total.vibe` checkable:

```sh
./scripts/cargo run --release -- examples/total.vibe --function total --arg '[10,20,30]' --checked
# {"total":60,"count":3}
```

## Reports

Each report entry names the input file, the one-based line and column, the containing function and the library's code frame. Diagnostics that belong to a required module name that module instead. Known contradictions are rendered as `error` entries and analysis the checker cannot finish as `incomplete` entries, followed by a summary that names the checked scope and counts both kinds separately:

```text
add.vibe:1:1: error in run: "run": argument "x": expected int, got string
  --> line 1, column 1
 1 | def run(x:int) -> int
   | ^
add.vibe: check of run found 1 error; nothing was executed
```

The summary reads `check of the whole file` for `vibes check FILE`, `check of NAME for its declared parameter types` for `vibes check --function NAME FILE` and `check of NAME` for an exact call; a rejected `--checked` call adds `; nothing was executed`. Incomplete analysis is never treated as clean and never causes the checker to execute source code to discover types; a `require` the checker cannot analyze stays an `incomplete` entry in every mode. Both kinds exit with status 1.

## Limits and counters

`--steps N` and `--memory N` set the step and tracked-memory quotas (zero disables one), `--recursion N` sets the execution call-depth limit and `--timeout-ms N` sets an absolute deadline measured from option parsing. The defaults are one million steps, 16 MiB and 256 frames. Both command forms accept these options. Checking summarizes recursive calls under the step and memory limits; it does not create execution frames or enforce the execution call-depth limit. Analysis and execution each receive the supplied quotas and share the deadline. Exhausted quotas, deadlines, unknown functions or selectors, read failures and parse errors print their message on stderr and exit with status 1.

`--stats` prints `steps=N peak_bytes=N retained_bytes=N` on stderr: analysis counters after `vibes check`, `--check` or a rejected `--checked` call, and execution counters after ordinary or accepted execution.

## Exit status

| Status | Meaning |
| --- | --- |
| 0 | Execution finished, or the check was clean. |
| 1 | Reading, parsing, checking or execution failed, including rejected and incomplete checks. |
| 2 | Usage error; nothing was read or executed. |
