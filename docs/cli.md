# Command line

`vibes FILE` compiles one source file, runs its top-level statements and prints the final value as JSON on stdout. `puts`, `print` and `p` write to stdout and `warn` writes to stderr before that value. `--function NAME` calls one function instead, with `--arg JSON` positional values in order and `--kwarg NAME=JSON` keyword values. Options may appear anywhere around FILE, option values are taken verbatim, and `--` ends option parsing so a file name may start with `-`.

```sh
./scripts/cargo run --release -- examples/total.vibe --function total --arg '[10,20,30]' --stats
```

Keyword values follow `Script::call_with_keywords`: they bind by name rather than forming a trailing options hash, and a repeated name binds its last value. Every command-line value is parsed and validated before the file is read, so malformed JSON, numbers or option combinations never execute anything.

## Exact-call checking

`--check` analyzes the call selected by `--function`, `--arg` and `--kwarg` through `Script::check_call_with_keywords` without executing script code, host callbacks, defaults or initializers. A clean check prints nothing and exits with status 0. `--checked` runs the same analysis through `Script::checked_call_with_keywords` and executes the call only when the report is clean; a rejected call prints the report instead of a result and runs no script code. Both modes require `--function` and exclude each other, because the current checker covers one concrete call rather than a file.

Each report entry names the input file, the one-based line and column, the containing function and the library's code frame. Known contradictions are rendered as `error` entries and analysis the checker cannot finish as `incomplete` entries, followed by a summary that counts both separately:

```text
add.vibe:1:1: error in run: "run": argument "x": expected int, got string
  --> line 1, column 1
 1 | def run(x:int) -> int
   | ^
add.vibe: check of run found 1 error; nothing was executed
```

Incomplete analysis is never treated as clean and never causes the checker to execute source code to discover types. Both kinds exit with status 1.

The scope is exactly one call: the named function, the supplied values and whatever that call reaches. Unused functions, top-level statements and the file as a whole are not checked, and a clean result does not prove that the script is type safe; dynamic values keep their runtime contracts, so a `--checked` execution can still fail with an ordinary error. See the [checker notes](checker.md) for the analysis itself.

For example, the supplied array length makes the indexed loop in `total.vibe` checkable:

```sh
./scripts/cargo run --release -- examples/total.vibe --function total --arg '[10,20,30]' --checked
# {"total":60,"count":3}
```

## Limits and counters

`--steps N` and `--memory N` set the step and tracked-memory quotas (zero disables one), `--recursion N` sets the call-depth limit and `--timeout-ms N` sets an absolute deadline measured from option parsing. The defaults are one million steps, 16 MiB and 256 frames. Analysis and execution each receive the supplied quotas and share the deadline. Exhausted quotas, deadlines, unknown functions, read failures and parse errors print their message on stderr and exit with status 1.

`--stats` prints `steps=N peak_bytes=N retained_bytes=N` on stderr: analysis counters after `--check` or a rejected `--checked` call, and execution counters after ordinary or accepted execution.

## Exit status

| Status | Meaning |
| --- | --- |
| 0 | Execution finished, or the check was clean. |
| 1 | Reading, parsing, checking or execution failed, including rejected and incomplete checks. |
| 2 | Usage error; nothing was read or executed. |
