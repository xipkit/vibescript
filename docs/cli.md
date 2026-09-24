# Command line

`vibes FILE` compiles one source file, runs its top-level statements and prints the final value as JSON on stdout. `puts`, `print` and `p` write to stdout and `warn` writes to stderr before that value. `--function NAME` calls one function instead, with `--arg JSON` positional values in order and `--kwarg NAME=JSON` keyword values. Options may appear anywhere around FILE, option values are taken verbatim, and `--` ends option parsing so a file name may start with `-`.

```sh
./scripts/cargo run --release -p vibes -- examples/total.vibe --function total --arg '[10,20,30]' --stats
```

Keyword values follow `Script::call_with_keywords`: they bind by name rather than forming a trailing options hash, and a repeated name binds its last value. Every command-line value is parsed and validated before the file is read, so malformed JSON, numbers or option combinations never execute anything.

The binary offers these ways to analyze or run a file:

| Invocation | Library entry | Scope | Executes |
| --- | --- | --- | --- |
| `vibes check FILE` | `Script::check` | The whole file: top-level statements, then every function and method declaration, including unused ones, for its declared parameter types and defaults. | Nothing. |
| `vibes check --function NAME FILE` | `Script::check_function` | One declaration and whatever it reaches, for its declared parameter types and defaults; no concrete argument values. | Nothing. |
| `vibes FILE --function NAME [--arg JSON]... --check` | `Script::check_call_with_keywords` | One concrete call with the supplied values and whatever it reaches. | Nothing. |
| `vibes FILE --function NAME [--arg JSON]... --checked` | `Script::checked_call_with_keywords` | The same concrete call. | The call, only when its check is clean. |
| `vibes -e SOURCE --check` | `Script::check` | The whole inline snippet, like `vibes check -e SOURCE`. | Nothing. |

Analysis never executes script code, host callbacks, defaults or initializers. A clean `vibes check` or `--check` prints nothing unless `--stats` is requested and exits with status 0; a clean `--checked` proceeds to execution and prints its result. Unsupported analysis is reported as `incomplete`. Dynamic values retain their runtime contracts, so a clean report can still be followed by an execution error.

## Inline source

`-e SOURCE` or `--eval SOURCE` supplies the source on the command line instead of FILE, in both command forms. The value is taken verbatim, so `-e -7` prints `-7` and `vibes -- -e` runs a file named `-e`. It may be given once, never together with FILE, and must be valid UTF-8; empty source is valid and evaluates to `null`. These rules are checked with the rest of the command line, before any file or module directory is read. No temporary file is written.

```sh
vibes -e 'x = 2
y = 3
x * y'                                                  # 6
vibes -e 'def run(x:int) -> int; x + 1; end' --function run --arg 41 --checked
vibes check -e 'def unused(n:string) -> int; n; end'    # rejects the unused declaration
```

Reports and parse errors name inline source `<eval>`; diagnostics from required modules keep their own filenames. A whole-snippet summary reads `check of the whole snippet`.

Every scope is available for inline source. `--function NAME` with `--check` or `--checked` keeps its exact-call meaning, and `vibes check -e SOURCE --function NAME` checks one declaration. Without `--function`, `vibes -e SOURCE --check` checks the whole snippet, exactly as `vibes check -e SOURCE` does, so unused functions and methods are covered as ADR-004 of the reference implementation requires for snippets; `vibes FILE --check` still requires `--function`, because `vibes check FILE` already covers the whole file. `--checked` requires `--function` in every case.

## Required modules

The CLI searches the input file's directory first for calls such as `require(:helpers)`; for inline source, the process working directory is searched first instead. Repeatable `--module-path DIR` options append search roots in the order supplied. Relative option paths are resolved from the process working directory; the script directory remains first even when the command runs elsewhere. Duplicate directory paths are collapsed, and missing paths or ordinary files are rejected before execution.

```sh
vibes --module-path shared --module-path vendor app/main.vibe
vibes check --module-path shared app/main.vibe
vibes app/main.vibe --module-path shared --function run --checked
```

All execution and checking modes use the same configured roots and the engine's directory-handle confinement. Required files can make relative imports within their root, such as `require('./helpers')`; a relative import from the main script still requires a module caller, as in the Go reference. Checking reads and analyzes resolved modules without executing their initializers or output helpers.

Under WASI, `vibes.wasm` sees only the directories its host preopens, and every path is a guest path. Duplicate module paths are collapsed by their absolute spelling, because WASI cannot canonicalize a path beneath a preopen whose ancestors are hidden; the engine still resolves links when it opens each root. Inline source runs without the working-directory root when the host exposes no working directory. See [platform support](platforms.md) for an example.

## Whole-file checking

`vibes check FILE` analyzes the whole file through `Script::check`. The top-level statements are checked in source order, then every effective function and method declaration is checked against its declared parameter types and defaults, including declarations that nothing calls. Declarations use namespace state from the top-level analysis, so a module constant assigned from a top-level variable keeps its known type. No result value is printed, and `puts`, `print`, `p` and `warn` never run.

```sh
cat > unused.vibe <<'EOF'
7
def unused(n:string) -> int
  n
end
EOF
./scripts/cargo run --release -p vibes -- check unused.vibe
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
./scripts/cargo run --release -p vibes -- check --function 'C#read' methods.vibe
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
./scripts/cargo run --release -p vibes -- examples/total.vibe --function total --arg '[10,20,30]' --checked
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

The summary reads `check of the whole file` for `vibes check FILE`, `check of the whole snippet` for a whole `-e` check, `check of NAME for its declared parameter types` for `vibes check --function NAME FILE` and `check of NAME` for an exact call; a rejected `--checked` call adds `; nothing was executed`. Incomplete analysis is never treated as clean and never causes the checker to execute source code to discover types; a `require` the checker cannot analyze stays an `incomplete` entry in every mode. Both kinds exit with status 1.

## Limits and counters

`--steps N` and `--memory N` set the step and tracked-memory quotas (zero disables one), `--recursion N` sets the execution call-depth limit and `--timeout-ms N` sets an absolute deadline measured from option parsing. The defaults are one million steps, 16 MiB and 256 frames. Both command forms accept these options. Checking summarizes recursive calls under the step and memory limits; it does not create execution frames or enforce the execution call-depth limit. Analysis and execution each receive the supplied quotas and share the deadline. Exhausted quotas, deadlines, unknown functions or selectors, read failures and parse errors print their message on stderr and exit with status 1.

`--stats` prints `steps=N peak_bytes=N retained_bytes=N` on stderr: analysis counters after `vibes check`, `--check` or a rejected `--checked` call, and execution counters after ordinary or accepted execution.

## Exit status

| Status | Meaning |
| --- | --- |
| 0 | Execution finished, or the check was clean. |
| 1 | Reading, parsing, checking or execution failed, including rejected and incomplete checks. |
| 2 | Usage error; nothing was read or executed. |

## Interactive REPL

`vibes repl` starts the interactive REPL, a port of the Go CLI's. It takes no positional arguments. As in the Go CLI, it defaults to the `xhigh` quota profile: unlimited steps and memory with a 10,000-frame recursion cap, because it runs your own code on your own machine. The flags follow Go's spelling, with one or two dashes and the value after a space or `=`:

| Flag | Meaning |
| --- | --- |
| `-profile NAME` | `low` (1,000,000 steps, 16 MiB, 256 frames), `medium` (20,000,000, 128 MiB, 1,000), `high` (200,000,000, 512 MiB, 4,000) or `xhigh` (unlimited, unlimited, 10,000). |
| `-step-quota N` | Overrides the profile's step quota. |
| `-memory-quota N` | Overrides the profile's memory quota in bytes. |
| `-recursion-limit N` | Overrides the profile's recursion limit. |

For each override, `-1` removes the limit, `0` selects the library default and a positive value is the limit. An unknown profile, flag or positional argument prints a message on stderr and exits with status 1 before any input is read; `-h` prints the usage. A runaway loop under a finite profile fails with `step quota exceeded` instead of freezing the session.

```sh
vibes repl
vibes repl -profile low
printf 'x = 20\nx * 2 + 2\n' | vibes repl
```

### The session

Each complete input runs as a top-level snippet. Top-level variables it assigns, including destructuring and compound assignments and loop variables, stay available to later inputs, as do changes to existing variables such as `items.push(2)`. `_` holds the last result. Functions, classes, modules and enums declared at the top level stay available too, and a new declaration with the same name replaces the old one. Classes, modules and enums are kept as values, so instances and enum members made earlier still match them in `is_a?`, comparisons and type annotations; an instance made before a class was redefined keeps its original class. Class variables and module state start afresh for each input, while instances keep their fields. Functions are kept as source and compiled into each later input. An input that fails to compile or run leaves the variables and declarations as they were.

An input that ends inside an unfinished construct continues on the next line under a `...>` prompt: a `def`, `class` or block without its `end`, an open bracket, a trailing operator or an unterminated string. Blank lines inside it are kept. Ctrl-C discards the unfinished input.

The result shows anything the input printed with `puts`, `print`, `p` or `warn`, then the result unless it is nil; a nil result with no output shows `nil`. Values are rendered as the Go REPL renders them: strings and symbols without quotes, nil inside a collection as an empty string, floats in Go's shortest form (`1e+21`, `Infinity`) and collections as `[1, 2]` and `{a: 1}`. Failures start with `compile error:` or `runtime error:` and carry the library's code frame and call trace. Positions refer to the text as typed: frames in the input are named `<repl>`, a parse error at the end of the input reads `unexpected end of snippet`, and an error inside a carried function points at the line where that function was typed.

### Commands and keys

| Command | Effect |
| --- | --- |
| `:help`, `:h` | Show or hide the help panel. |
| `:vars`, `:v` | Show or hide the variables panel. |
| `:globals`, `:g` | List variables as `name = value`. |
| `:functions`, `:f` | List callable builtins, the session's functions and callable variables. |
| `:types`, `:t` | List variables as `name: type`. |
| `:clear`, `:c` | Clear the transcript. |
| `:reset`, `:r` | Forget every variable and declaration. |
| `:last_error`, `:le` | Show the most recent error. |
| `:quit`, `:q` | Exit. |

With a terminal on both stdin and stdout, the REPL runs full screen in the alternate screen, with the Go REPL's layout and colors: a header, the transcript of inputs (`›`), results (`→`) and failures (`✗`), the panels, the input line and a key hint footer. Older transcript lines scroll away so the input stays visible. Colors follow the terminal: true color when `COLORTERM` says so, 256 or 16 colors otherwise, and none under `NO_COLOR` or a dumb terminal.

| Key | Effect |
| --- | --- |
| Enter | Submit the line. |
| Up, Down | Recall earlier and later inputs; a multi-line input comes back whole. |
| Tab | Complete the last word: `:` commands, builtins such as `JSON.parse_as` after a dot, keywords, variables and the session's declarations. Several matches are listed in the transcript. |
| Ctrl-C | Interrupt a running evaluation, discard an unfinished input, or exit. |
| Ctrl-D | Exit. |
| Ctrl-L | Clear the transcript. |
| Ctrl-V, Ctrl-K | Show or hide the variables or help panel. |
| Left, Right, Home, End, Ctrl-A, Ctrl-E, Ctrl-B, Ctrl-F | Move the cursor. |
| Alt-Left, Alt-Right, Ctrl-Left, Ctrl-Right, Alt-B, Alt-F | Move by word. |
| Backspace, Delete, Ctrl-H, Ctrl-W, Alt-Backspace, Alt-D, Ctrl-U | Delete a character, a word, or everything before the cursor. |

An input line holds at most 500 characters and scrolls horizontally when it is wider than the terminal. Pasted text is inserted as typed, and each line break in it submits a line. Keys typed while an evaluation runs are applied after it finishes.

### Piped input

When stdin or stdout is not a terminal, and always under WASI, the REPL reads one line per input, ending at `\n`, `\r\n` or `\r`, and prints each transcript entry as the full-screen transcript shows it, followed by a blank line. `:vars` and `:help` print their panel. It stops at `:quit`, a Ctrl-D byte or the end of input, and exits with status 0; an input still unfinished at the end is evaluated so its error is reported. Colors are used only when stdout is a terminal.

```sh
$ printf 'def sq(n)\n  n * n\nend\nsq(7)\n' | vibes repl
  › def sq(n)
      n * n
    end
  → nil

  › sq(7)
  → 49

```

### Embedding

The session is the `vibescript_tools::repl::ReplSession` library type; the CLI only adds the terminal. `feed_line` takes one line and reports whether it needs more input, ran as an evaluation with its rendered output and its value or structured error, or ran a command. The session also offers completion, the transcript and history navigation, and is built on the library's `Script::run_bindings`, `Script::declarations` and `vibescript::builtins` (see [interactive sessions](sessions.md)).

### Differences from the Go REPL

The Go REPL wraps each input in a function, so it keeps only the variables of a single assignment statement and cannot define functions, classes, modules or enums; this REPL keeps every top-level variable, including changes made through methods such as `push`, and carries declarations. Go's input line is single-line; this REPL continues unfinished input. Go shows an input's call frames twice (`at <repl> (1:1)` for both the function and its call) and reports some parse errors differently because of its wrapper; here each frame appears once and a lone `end` is `expected expression`. `:functions` omits `proc`, `lambda` and `Proc`, which the Rust library removed. With piped input, Go's program reads the pipe as keystrokes, requires carriage returns, renders nothing when stdout is not a terminal and waits forever at the end of input; this REPL's line mode prints the transcript and exits. Ctrl-C interrupts a running evaluation instead of waiting for it, a long transcript scrolls by lines so the input line never leaves the screen, the terminal's own cursor replaces the Go input's blinking block, and the header shows this package's version.
