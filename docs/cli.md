# Command line

`vibes` provides the Go reference's commands with the reference's syntax, output, error texts and exit codes:

| Command | Purpose |
| --- | --- |
| `vibes run [options] <script> [args...]`, `vibes run [options] -e SNIPPET` | Execute a script file or inline snippet. |
| `vibes check [options] <script>` | Statically check a whole script without executing it. |
| `vibes fmt [-w] [-check] <path>...` | Canonically format `.vibe` files. |
| `vibes analyze <script>` | Report lint findings: statements that can never run. |
| `vibes test [options] [path...]` | Discover `*_test.vibe` files and run their `test_` functions. |
| `vibes help [command]`, `--help`, `-h` | Print the command list or one command's help. |
| `vibes repl [options]` | Start the interactive REPL. |
| `vibes lsp` | Serve the language server over stdin and stdout for editors. |

It also keeps the flat form that predates these commands, `vibes [OPTIONS] FILE` and `vibes [OPTIONS] -e SOURCE`, which prints results as JSON and checks exact calls (see [the flat form](#the-flat-form)), and it prints its version with `vibes --version`.

The formatter, analyzer, test runner, REPL session and language server are also libraries in the `vibescript-tools` crate (`vibescript_tools::format`, `::analyze`, `::test_runner`, `::repl` and `::lsp`), so other programs can embed them; the CLI is a front end that parses arguments, finds and writes files, and renders results.

```sh
./scripts/cargo run --release -p vibes -- run examples/total.vibe
./scripts/cargo run --release -p vibes -- test ./tests
```

## Command selection

The first argument alone decides what runs, in this order:

1. `-h` or `--help` prints the root help. A first argument the reference rejects outright, `--`, one with leading or trailing whitespace, `-help`, `--h` or a help flag with a value such as `--help=false`, prints the root help and `unknown command "..."` on stderr.
2. A command name (`run`, `check`, `fmt`, `analyze`, `test`, `lsp`, `repl`, `help` or `h`) runs that command. A file named after a command therefore needs `vibes run check`, or a flat-form option before it.
3. `--version` prints `vibescript.rs VERSION`.
4. A flat-form option (`-e`, `--eval`, `--function`, `--module-path`, `--arg`, `--kwarg`, `--check`, `--checked`, `--steps`, `--memory`, `--recursion`, `--timeout-ms` or `--stats`), or a script path, runs the flat form. A script path is an existing file, or a spelling that contains a path separator or ends in `.vibe`.
5. Anything else fails as in the reference: `vibes` alone reports `command required` after the root help, a flag such as `-x` or `--bogus` reports `flag provided but not defined: -x`, and any other word reports `unknown command "word"` after the root help.

As in the reference, `--` does not escape command selection: `vibes -- run` is an unknown command. Use `vibes run -- FILE`, or a flat-form option such as `vibes --stats -- FILE`, for a file whose name starts with `-`.

## Command syntax

After a command name, flags take one or two leading hyphens, `-name` and `--name` alike. A value follows as `-name value` or `-name=value`; boolean flags accept `-name`, or `-name=` with `1`, `t`, `T`, `TRUE`, `true`, `True` or their false counterparts. Integers use Go syntax, so `0x10`, `0o17`, `1_000` and `-1` are valid. A repeated flag keeps its last value, except `-module-path`, which accumulates.

Flags must come before the first positional argument; every later token is an argument, even when it starts with `-`. `--` ends flags explicitly, and a `--` after the first positional argument is itself an argument. `-h` or `--help` prints the command's help and ignores later tokens, but earlier flag errors are still reported.

Errors print one line on stderr and exit with status 1, with the reference's wording:

```text
flag provided but not defined: -unknown
flag needs an argument: -e
bad flag syntax: ---w
invalid boolean value "yes" for -check: parse error
invalid value "nope" for flag -step-quota: parse error
help flag does not accept a value
```

## `vibes run`

```sh
vibes run [options] <script> [args...]
vibes run [options] -e SNIPPET
```

Without `-function`, `run` executes the script's top-level statements when it has any, and otherwise calls its `run` function. A top-level statement is anything other than a function, class, module or enum declaration or an alias. `-function NAME` calls another function, and `-function '<script>'` selects the top-level statements explicitly. Every argument after the script path is passed to the function as a string. The script's directory is the first module root, and repeatable `-module-path DIR` options add more; the paths are made absolute and deduplicated by spelling, and a missing path or a file fails with the reference's message.

A non-nil result prints on stdout in the reference's string form: strings and symbols without quotes, `nil` as nothing, floats in Go's shortest form (`2`, `1e+20`, `Infinity`), arrays as `[a, b]` and hashes as `{key: value}`. A rendering over 1 MiB fails with `result rendering exceeds 1048576 bytes; reduce the returned value or stream it from the script`. `puts`, `print` and `p` write to stdout and `warn` to stderr.

A script larger than 1 MiB is refused before it is read with `source exceeds maximum size (SIZE > 1048576 bytes)`, as are directories and other non-regular files. Invalid UTF-8 in a script is decoded with replacement characters, as the reference's lexer does. Failures are prefixed by their stage: `read script:`, `compile failed:` and `execution failed:`.

`-check` checks the invocation `run` would make, with the same function and arguments, without executing anything. A clean check prints nothing; issues fail with `check failed: LINE:COLUMN: MESSAGE (FUNCTION)`, or `check failed with N issue(s):` followed by one indented line per issue. `-e SNIPPET` evaluates inline source with the working directory as its first module root; `-check -e` checks the whole snippet, including unused declarations, and sorts its issues by position. `-e` cannot be combined with `-watch`, `-function` or positional arguments, and an empty snippet is an error. Frames of the snippet's top-level code are named `<snippet>`, and a parse error at the end of the snippet reads `unexpected end of snippet`.

An interrupt (ctrl-c) cancels the running script, which then fails; a second interrupt terminates the process.

### Quota profiles

`run`, `test` and `repl` run under one of the reference's quota profiles, selected with `-profile` (case and surrounding spaces are ignored):

| Profile | Step quota | Memory quota | Recursion limit |
| --- | --- | --- | --- |
| `low` | 1,000,000 | 16 MiB | 256 |
| `medium` | 20,000,000 | 128 MiB | 1,000 |
| `high` | 200,000,000 | 512 MiB | 4,000 |
| `xhigh` (default) | unlimited | unlimited | 10,000 |

`-step-quota`, `-memory-quota` and `-recursion-limit` override one quota of the selected profile: a positive value is the limit, `-1` or any negative value removes it, and `0` selects the engine default, which is the `low` value. An unknown profile fails with `unknown quota profile "NAME" (choose one of: low, medium, high, xhigh)`. The values map directly onto `vibescript::Limits`; an unlimited recursion limit is `usize::MAX`.

### Watch mode

`vibes run -watch SCRIPT` runs the script, then runs it again whenever the script or a `.vibe` file under its module roots changes, with a fresh engine so modules load again. Status lines go to stderr: `watching N file(s); press ctrl-c to stop`, `change detected, re-running NAME` and, after an interrupt, `watch stopped`, which exits with status 0. Compile and runtime errors are printed without ending the watch.

Changes are found by the reference's polling method: every 300 ms the size and modification time of every known file are compared, and every 5 seconds the module roots are walked again for added and deleted files. Linked directories are not descended; linked files are followed, so a dangling link whose target appears counts as a change.

## `vibes check`

```sh
vibes check [options] <script>
```

`check` analyzes the whole script without executing anything: the top-level statements in source order, then every function and method declaration, including unused ones, for its declared parameter types and defaults. It prints one issue per line on stdout, `PATH:LINE:COLUMN: MESSAGE (FUNCTION)`, then fails with `check failed with N issue(s)`; a clean check prints `No issues found`. PATH is the absolute script path, or the resolved path of the required module that owns the issue. Analysis the checker cannot finish is an issue too, marked `incomplete:`, and is never reported as clean. The issues come from this library's checker, which is [stricter than the reference's](compatibility.md) and words its messages differently.

`-module-path DIR` adds module search roots, as for `run`. These flags extend the reference's:

| Flag | Meaning |
| --- | --- |
| `-function NAME` | Check one declaration instead of the whole file: a function, `Class#method`, `Namespace.method`, `Class.new` or `__main__`, for its declared parameter types and defaults. |
| `-e SOURCE`, `-eval SOURCE` | Check inline source instead of a file; issues name it `<eval>`, and the working directory is the first module root. |
| `-steps N`, `-memory N` | Analysis step and memory quotas; 0 disables one. Like the reference, analysis has neither by default. |
| `-recursion N` | The call-depth setting, 256 by default. |
| `-timeout-ms N` | An analysis deadline. |
| `-stats` | Print `steps=N peak_bytes=N retained_bytes=N` on stderr. |

## `vibes fmt`

```sh
vibes fmt [-w] [-check] <path>...
```

The canonical form normalizes `\r\n` and lone `\r` to `\n`, strips trailing spaces and tabs, drops trailing blank lines and ends with one newline; everything else, including invalid UTF-8, is kept. Without flags, the formatted files are printed on stdout. `-w` rewrites files that change, and `-check` fails with `vibes fmt: N file(s) need formatting` when any would; with both, changed files are rewritten and the command still fails.

Directory operands are walked recursively without following links: only regular `.vibe` files are formatted, and linked files, linked directories and other entries are skipped. An explicit file operand ending in `.vibe` is formatted where it points, so a linked operand formats its target and keeps the link; other explicit files are ignored, and a non-regular `.vibe` operand is an error. Files are processed once each, in byte order of their absolute paths. Directory operands are opened as root handles that refuse paths escaping them, with at most eight open at once; a root reopened after eviction must still be the same directory. Every read and write checks that the file is still the regular file found during discovery, and a rewrite happens in place, keeping the file's identity, permissions and hard links.

## `vibes analyze`

```sh
vibes analyze <script>
```

`analyze` reports statements that can never run because an earlier statement in the same body always leaves it: `return`, `raise`, `break`, `next`, `retry`, or a compound statement whose every path ends in one. Each finding prints as `PATH:LINE:COLUMN: unreachable statement (SCOPE)`, then the command fails with `analysis found N issue(s)`; a clean script prints `No issues found`. SCOPE is a function name, `<script>` for top-level code, `Class#method`, `Class.method` or `Class.<class body>`, with ` block at LINE:COLUMN` for each enclosing block. Scopes, positions and ordering match the reference's linter, including its conventions for operators, modifiers and positions inside string interpolation. A compile error fails with `analysis compile failed:`.

## `vibes test`

```sh
vibes test [-run REGEXP] [-module-path DIR]... [quota flags] [path...]
```

`test` finds `*_test.vibe` files under the paths, `.` by default, recursively and without following linked directories; an explicit file must follow the naming convention. A test is a top-level function whose name starts with `test_`; it passes when it returns and fails when it raises, including a failed `assert`, and it must not require arguments. Tests run in name order, each as its own call, under the quota profile flags described for `run`. Each file's directory is its first module root. Test files may only declare functions, classes, modules, enums and aliases, as in the reference, whose compiler rejects other top-level statements.

The report goes to stdout, with the tests' own output interleaved: `--- FAIL: FILE :: NAME` and the indented failure for each failing test, or a pseudo-test such as `(compile)` when a file cannot run, then `ok   FILE (N test(s))` for a clean file, and finally `N test(s) across N file(s): N passed, N failed`. A failure exits with `vibes test: N test(s) failed`. `-run` selects tests whose names match a regular expression in the engine's Go-compatible syntax; an invalid pattern fails with the reference's message.

## `vibes lsp`

```sh
vibes lsp
```

`vibes lsp` starts the language server that editors launch for `*.vibe` files, speaking the Language Server Protocol over stdin and stdout, as the reference's does. It takes no positional arguments; as with the other commands, `-h` prints its help and an argument fails with `vibes lsp: does not accept positional arguments` and status 1. It exits with status 0 after the client sends `exit` or closes its input, or after an interrupt, and with status 1 when the input's framing is corrupt.

It publishes compile errors on every change and answers hover, completion, signature help, definition, document symbol and formatting requests as the reference does. Its diagnostics add this library's checker findings, with required files resolved from the document's directory as `vibes check` resolves them from the script's. See [the language server](lsp.md) for its features, limits and differences from the reference.

## `vibes repl`

```sh
vibes repl [options]
```

`vibes repl` starts the interactive REPL, a port of the Go CLI's. It takes no positional arguments and accepts the [quota profile flags](#quota-profiles) described for `vibes run`, with the same syntax and defaults: `xhigh`, unlimited steps and memory with a 10,000-frame recursion cap, because it runs your own code on your own machine. An unknown profile, flag or positional argument prints a message on stderr and exits with status 1 before any input is read, and `-h` prints the usage. A runaway loop under a finite profile fails with `step quota exceeded` instead of freezing the session.

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

The Go REPL wraps each input in a function, so it keeps only the variables of a single assignment statement and cannot define functions, classes, modules or enums; this REPL keeps every top-level variable, including changes made through methods such as `push`, and carries declarations. Go's input line is single-line; this REPL continues unfinished input. Go shows an input's call frames twice (`at <repl> (1:1)` for both the function and its call) and reports some parse errors differently because of its wrapper; here each frame appears once and a lone `end` is `unexpected token 'end'`, as in a Go script. `:functions` omits `proc`, `lambda` and `Proc`, which the Rust library removed. With piped input, Go's program reads the pipe as keystrokes, requires carriage returns, renders nothing when stdout is not a terminal and waits forever at the end of input; this REPL's line mode prints the transcript and exits. Ctrl-C interrupts a running evaluation instead of waiting for it, a long transcript scrolls by lines so the input line never leaves the screen, the terminal's own cursor replaces the Go input's blinking block, and the header shows this package's version.

## The flat form

`vibes FILE` compiles one source file, runs its top-level statements and prints the final value as JSON on stdout. `puts`, `print` and `p` write to stdout and `warn` writes to stderr before that value. `--function NAME` calls one function instead, with `--arg JSON` positional values in order and `--kwarg NAME=JSON` keyword values. Options may appear anywhere around FILE, option values are taken verbatim, and `--` ends option parsing so a file name may start with `-`. `vibes help flat` prints this form's usage.

```sh
./scripts/cargo run --release -p vibes -- examples/total.vibe --function total --arg '[10,20,30]' --stats
```

Keyword values follow `Script::call_with_keywords`: they bind by name rather than forming a trailing options hash, and a repeated name binds its last value. Every command-line value is parsed and validated before the file is read, so malformed JSON, numbers or option combinations never execute anything.

| Invocation | Library entry | Scope | Executes |
| --- | --- | --- | --- |
| `vibes FILE --function NAME [--arg JSON]... --check` | `Script::check_call_with_keywords` | One concrete call with the supplied values and whatever it reaches. | Nothing. |
| `vibes FILE --function NAME [--arg JSON]... --checked` | `Script::checked_call_with_keywords` | The same concrete call. | The call, only when its check is clean. |
| `vibes -e SOURCE --check` | `Script::check` | The whole inline snippet, like `vibes check -e SOURCE`. | Nothing. |

Analysis never executes script code, host callbacks, defaults or initializers. A clean `--check` prints nothing unless `--stats` is requested and exits with status 0; a clean `--checked` proceeds to execution and prints its result. Unsupported analysis is reported as `incomplete`. Dynamic values retain their runtime contracts, so a clean report can still be followed by an execution error.

### Inline source

`-e SOURCE` or `--eval SOURCE` supplies the source on the command line instead of FILE. The value is taken verbatim, so `-e -7` prints `-7`. It may be given once, never together with FILE, and must be valid UTF-8; empty source is valid and evaluates to `null`. These rules are checked with the rest of the command line, before any file or module directory is read. No temporary file is written.

```sh
vibes -e 'x = 2
y = 3
x * y'                                                  # 6
vibes -e 'def run(x:int) -> int; x + 1; end' --function run --arg 41 --checked
```

Reports and parse errors name inline source `<eval>`; diagnostics from required modules keep their own filenames. A whole-snippet summary reads `check of the whole snippet`. `--function NAME` with `--check` or `--checked` keeps its exact-call meaning. Without `--function`, `vibes -e SOURCE --check` checks the whole snippet, so unused functions and methods are covered as ADR-004 of the reference implementation requires for snippets; `vibes FILE --check` still requires `--function`, because `vibes check FILE` covers the whole file. `--checked` requires `--function` in every case.

### Required modules

The flat form searches the input file's directory first for calls such as `require(:helpers)`; for inline source, the process working directory is searched first instead. Repeatable `--module-path DIR` options append search roots in the order supplied. Relative option paths are resolved from the process working directory; the script directory remains first even when the command runs elsewhere. Duplicate directories are collapsed after resolving links, and missing paths or ordinary files are rejected before execution.

```sh
vibes --module-path shared --module-path vendor app/main.vibe
vibes app/main.vibe --module-path shared --function run --checked
```

All execution and checking modes use the same configured roots and the engine's directory-handle confinement. Required files can make relative imports within their root, such as `require('./helpers')`; a relative import from the main script still requires a module caller, as in the Go reference. Checking reads and analyzes resolved modules without executing their initializers or output helpers.

Under WASI, `vibes.wasm` sees only the directories its host preopens, and every path is a guest path. Duplicate module paths are collapsed by their absolute spelling, because WASI cannot canonicalize a path beneath a preopen whose ancestors are hidden; the engine still resolves links when it opens each root. Inline source runs without the working-directory root when the host exposes no working directory. See [platform support](platforms.md) for an example.

### Exact-call checking

`--check` analyzes the call selected by `--function`, `--arg` and `--kwarg` through `Script::check_call_with_keywords` without executing script code, host callbacks, defaults or initializers. `--checked` runs the same analysis through `Script::checked_call_with_keywords` and executes the call only when the report is clean; a rejected call prints the report instead of a result and runs no script code. Both modes require `--function` and exclude each other, because they cover one concrete call rather than a file.

The scope is exactly one call: the named function, the supplied values and whatever that call reaches. Selecting `__main__` checks the top-level entrypoint in source order. Other named calls omit ordinary top-level statements, and unused functions and methods are outside either scope. Use `vibes check` for the file as a whole or `vibes check -function NAME` for a declaration without supplied values. Method and constructor selectors such as `C#read` belong to `vibes check`; the flat form rejects them as unknown functions. See the [checker notes](checker.md) for the analysis itself.

### Reports

Each flat-form report entry names the input file, the one-based line and column, the containing function and the library's code frame. Diagnostics that belong to a required module name that module instead. Known contradictions are rendered as `error` entries and analysis the checker cannot finish as `incomplete` entries, followed by a summary that names the checked scope and counts both kinds separately:

```text
add.vibe:1:1: error in run: "run": argument "x": expected int, got string
  --> line 1, column 1
 1 | def run(x:int) -> int
   | ^
add.vibe: check of run found 1 error; nothing was executed
```

The summary reads `check of the whole snippet` for a whole `-e` check and `check of NAME` for an exact call; a rejected `--checked` call adds `; nothing was executed`. Both kinds exit with status 1.

### Limits and counters

`--steps N` and `--memory N` set the step and tracked-memory quotas (zero disables one), `--recursion N` sets the execution call-depth limit and `--timeout-ms N` sets an absolute deadline measured from option parsing. The defaults are one million steps, 16 MiB and 256 frames, the reference's `low` profile. Checking summarizes recursive calls under the step and memory limits; it does not create execution frames or enforce the execution call-depth limit. Exhausted quotas, deadlines, unknown functions, read failures and parse errors print their message on stderr and exit with status 1.

`--stats` prints `steps=N peak_bytes=N retained_bytes=N` on stderr: analysis counters after `--check` or a rejected `--checked` call, and execution counters after ordinary or accepted execution.

## Exit status

| Status | Meaning |
| --- | --- |
| 0 | The command succeeded, the check was clean, or watch mode stopped after an interrupt. |
| 1 | Any failure of the Go-style commands, including usage errors; in the flat form, reading, parsing, checking or execution failed. |
| 2 | A flat-form usage error; nothing was read or executed. |

## Differences from the Go reference

`scripts/compare-cli.py` builds the reference CLI and compares exit status, stdout and stderr on the help and error paths, `fmt` over every `.vibe` file in both trees plus generated whitespace cases, `run` and `analyze` over the script corpus, and a `test` suite; all of them are identical. The analyzer was also compared on about 430,000 sources from the fixtures and generated programs with injected terminators; only three differ, where the two parsers disagree about a call on a parenthesized `begin` block. These differences are intentional:

- `vibes --version` prints the version; the reference reports an undefined flag.
- The flat form, `vibes help flat` and the `check` flags `-function`, `-e`/`-eval`, `-steps`, `-memory`, `-recursion`, `-timeout-ms` and `-stats` are extensions, and `vibes check --help` lists them. Each applies only where the reference reports an error.
- `vibes lsp` adds this library's checker findings to its diagnostics and reports only the first parse error; its other differences are listed [with the language server](lsp.md#differences-from-the-reference). The REPL's own differences are listed [with the REPL](#differences-from-the-go-repl).
- `check` and `run -check` report this library's checker findings, marking unfinished analysis `incomplete:`. Their wording, positions and scope names (`bad` rather than the reference's `helpers.bad`) differ from the reference's checker, which also accepts some scripts this checker rejects.
- Engine messages are the library's: the `require` not-found message, step accounting under small quotas, and stack traces, which omit the reference's final frame for the entry function of a script.
- Watch mode always polls, as the reference does when file notifications are unavailable, so a new module file that nothing edits is noticed by the periodic scan within five seconds rather than immediately.
- Under WASI, interrupts are not observed, and `fmt` opens files by path within the host's preopened directories instead of through root handles.
- Quoted values in error messages treat a few rare Unicode format characters as printable where Go escapes them.
