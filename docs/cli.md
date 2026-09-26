# Command line

`vibes` provides these commands. Their syntax, output, error texts and exit codes began as those of the Go CLI's; the [differences](#differences-from-the-go-reference) are listed at the end.

| Command | Purpose |
| --- | --- |
| `vibes run [options] <script> [args...]`, `vibes run [options] -e SNIPPET` | Execute a script file or inline snippet. |
| `vibes check [options] <script>` | Type check a whole script without executing it. |
| `vibes fmt [-w] [-check] <path>...` | Canonically format `.vibe` files. |
| `vibes analyze <script>` | Report lint findings: statements that can never run. |
| `vibes test [options] [path...]` | Discover `*_test.vibe` files and run their `test_` functions. |
| `vibes help [command]`, `--help`, `-h` | Print the command list or one command's help. |
| `vibes repl [options]` | Start the interactive REPL. |
| `vibes lsp` | Serve the language server over stdin and stdout for editors. |
| `vibes prelude` | Print every builtin signature as Vibescript declarations. |
| `vibes fix [--dry-run] <file or directory>...` | Apply the machine-applicable fixes of the static language's diagnostics. |

It also keeps the flat form that predates these commands, `vibes [OPTIONS] FILE` and `vibes [OPTIONS] -e SOURCE`, which prints results as JSON (see [the flat form](#the-flat-form)), and it prints its version with `vibes --version`.

Every command that compiles a script type checks it (ADR-007): a script with type errors does not run, and the command prints its diagnostics instead.

The formatter, analyzer, fixer, test runner, REPL session and language server are also libraries in the `vibescript-tools` crate (`vibescript_tools::format`, `::analyze`, `::fix`, `::test_runner`, `::repl` and `::lsp`), so other programs can embed them; the CLI is a front end that parses arguments, finds and writes files, and renders results.

```sh
./scripts/cargo run --release -p vibes -- check examples/total.vibe
./scripts/cargo run --release -p vibes -- test ./tests
```

## Command selection

The first argument alone decides what runs, in this order:

1. `-h` or `--help` prints the root help. A first argument the reference rejects outright, `--`, one with leading or trailing whitespace, `-help`, `--h` or a help flag with a value such as `--help=false`, prints the root help and `unknown command "..."` on stderr.
2. A command name (`run`, `check`, `fmt`, `analyze`, `test`, `lsp`, `repl`, `prelude`, `fix`, `help` or `h`) runs that command. A file named after a command therefore needs `vibes run check`, or a flat-form option before it.
3. `--version` prints `vibescript.rs VERSION`.
4. A flat-form option (`-e`, `--eval`, `--function`, `--module-path`, `--arg`, `--kwarg`, `--steps`, `--memory`, `--recursion`, `--timeout-ms` or `--stats`), or a script path, runs the flat form. A script path is an existing file, or a spelling that contains a path separator or ends in `.vibe`.
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

`run` compiles the script with static types and refuses it, with its diagnostics, when it has type errors. Without `-function`, `run` executes the script's top-level statements when it has any, and otherwise calls its `run` function. A top-level statement is anything other than a function, class, module or enum declaration or an alias. `-function NAME` calls another function, and `-function '<script>'` selects the top-level statements explicitly. Every argument after the script path is passed to the function as a string, so the called function's parameters must accept `string`, or a rest parameter `array<string>`; otherwise the script is refused before it runs. The script's directory is the first module root, and repeatable `-module-path DIR` options add more; the paths are made absolute and deduplicated by spelling, and a missing path or a file fails with the reference's message.

A non-nil result prints on stdout in the reference's string form: strings and symbols without quotes, `nil` as nothing, floats in Go's shortest form (`2`, `1e+20`, `Infinity`), arrays as `[a, b]` and hashes as `{key: value}`. A rendering over 1 MiB fails with `result rendering exceeds 1048576 bytes; reduce the returned value or stream it from the script`. `puts`, `print` and `p` write to stdout and `warn` to stderr.

A script larger than 1 MiB is refused before it is read with `source exceeds maximum size (SIZE > 1048576 bytes)`, as are directories and other non-regular files. Invalid UTF-8 in a script is decoded with replacement characters, as the reference's lexer does. Failures are prefixed by their stage: `read script:`, `compile failed:` and `execution failed:`.

`-e SNIPPET` evaluates inline source with the working directory as its first module root. `-e` cannot be combined with `-watch`, `-function` or positional arguments, and an empty snippet is an error. Frames of the snippet's top-level code are named `<snippet>`, and a parse error at the end of the snippet reads `unexpected end of snippet`.

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

`check` compiles the script with static types without running anything and prints every diagnostic with its code, source line and fixes, then fails with `check failed with N error(s)`; a script without errors prints its warnings, if any, or `No issues found`. The [type checker](checker.md) lists the codes. Unused declarations are checked like the rest of the script, and required files are resolved and checked too.

`-module-path DIR` adds module search roots, as for `run`. These flags extend the reference's:

| Flag | Meaning |
| --- | --- |
| `-e SOURCE`, `-eval SOURCE` | Check inline source instead of a file; diagnostics name it `<eval>`, and the working directory is the first module root. |
| `-json` | Print each diagnostic as one JSON object per line (`Diagnostic::to_json`): its code, name, severity, spans with byte offsets and one-based lines and columns, message, expected and found types, labels and fixes. A syntax error is a `V0001` diagnostic, or `V0002` for a hash literal passed to a call without parentheses, which has a fix. A script that compiles prints its warnings, if any, and succeeds. |

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

It checks every document with static types as it changes and answers hover, completion, signature help, definition, document symbol and formatting requests as the reference does. Its diagnostics carry their codes, their fixes are offered as quick fixes, and required files resolve from the document's directory as `vibes check` resolves them from the script's. See [the language server](lsp.md) for its features, limits and differences from the reference.

## `vibes prelude`

```sh
vibes prelude
```

`vibes prelude` prints the builtin signature table: every builtin function, namespace and member of every value type, under its one canonical name, as Vibescript declarations with typed and generic signatures (ADR-007 and ADR-008). The text is `vibescript::signatures::prelude()`, printed from the same table the compiler checks against, in a stable order; its header explains the notation. A host that registers functions or grants capabilities gets the same text extended with its own declarations from `Engine::prelude`. It takes no positional arguments; an argument fails with `vibes prelude: does not accept positional arguments` and status 1.

## `vibes fix`

```sh
vibes fix [--dry-run] <file or directory>...
```

`vibes fix` checks each `.vibe` file in the static language of ADR-007 and ADR-008 and applies every fix whose applicability is machine-applicable, then checks again, until no such fix remains. Each round applies the innermost fixes first and only those whose edits neither overlap nor touch another's, so nested rewrites such as `unless x.nil?` land in turn; a suggestion, which a person should confirm, is never applied. Running it again changes nothing.

Each applied fix prints on stdout as `path:line:column: fixed V0401: message`, at its position in the text of the round that fixed it, and each diagnostic left afterwards as `path:line:column: error[V0405]: message`. A summary goes to stderr, and the command fails with `vibes fix: N error(s) remain` when errors remain, including a file that does not parse. `--dry-run` writes nothing and prints the changes as a unified diff before the report.

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

Each complete input runs as a top-level snippet. Top-level variables it assigns, including destructuring and compound assignments and loop variables, stay available to later inputs, as do changes to existing variables such as `items.push(2)`. `_` holds the last result. Functions, classes, modules and enums declared at the top level stay available too, and a new declaration with the same name replaces the old one. Classes, modules and enums are kept as values, so instances and enum members made earlier still match them in `is_type?`, comparisons and type annotations; an instance made before a class was redefined keeps its original class. Class variables and module state start afresh for each input, while instances keep their fields. Functions are kept as source and compiled into each later input. An input that fails to compile or run leaves the variables and declarations as they were.

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
$ printf 'def sq(n: int) -> int\n  n * n\nend\nsq(7)\n' | vibes repl
  › def sq(n: int) -> int
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

`vibes FILE` compiles one source file with static types, runs its top-level statements and prints the final value as JSON on stdout; a file with type errors prints its diagnostics on stderr instead, as `vibes run` does. `puts`, `print` and `p` write to stdout and `warn` writes to stderr before that value. `--function NAME` calls one function instead, with `--arg JSON` positional values in order and `--kwarg NAME=JSON` keyword values. Options may appear anywhere around FILE, option values are taken verbatim, and `--` ends option parsing so a file name may start with `-`. `vibes help flat` prints this form's usage.

```sh
./scripts/cargo run --release -p vibes -- examples/total.vibe --function total --arg '[10,20,30]' --stats
```

Keyword values follow `Script::call_with_keywords`: they bind by name rather than forming a trailing options hash, and a repeated name binds its last value. Every command-line value is parsed and validated before the file is read, so malformed JSON, numbers or option combinations never execute anything.

### Inline source

`-e SOURCE` or `--eval SOURCE` supplies the source on the command line instead of FILE. The value is taken verbatim, so `-e -7` prints `-7`. It may be given once, never together with FILE, and must be valid UTF-8; empty source is valid and evaluates to `null`. These rules are checked with the rest of the command line, before any file or module directory is read. No temporary file is written.

```sh
vibes -e 'x = 2
y = 3
x * y'                                                  # 6
vibes -e 'def run(x: int) -> int; x + 1; end' --function run --arg 41   # 42
```

Diagnostics and parse errors name inline source `<eval>`; diagnostics from required modules keep their own filenames.

### Required modules

The flat form searches the input file's directory first for calls such as `require(:helpers)`; for inline source, the process working directory is searched first instead. Repeatable `--module-path DIR` options append search roots in the order supplied. Relative option paths are resolved from the process working directory; the script directory remains first even when the command runs elsewhere. Duplicate directories are collapsed after resolving links, and missing paths or ordinary files are rejected before execution.

```sh
vibes --module-path shared --module-path vendor app/main.vibe
vibes app/main.vibe --module-path shared --function run
```

Compilation and execution use the same configured roots and the engine's directory-handle confinement. Required files can make relative imports within their root, such as `require("./helpers")`; a relative import from the main script still requires a module caller, as in the Go reference. Type checking reads and checks resolved modules without executing their initializers or output helpers.

Under WASI, `vibes.wasm` sees only the directories its host preopens, and every path is a guest path. Duplicate module paths are collapsed by their absolute spelling, because WASI cannot canonicalize a path beneath a preopen whose ancestors are hidden; the engine still resolves links when it opens each root. Inline source runs without the working-directory root when the host exposes no working directory. See [platform support](platforms.md) for an example.

### Limits and counters

`--steps N` and `--memory N` set the step and tracked-memory quotas (zero disables one), `--recursion N` sets the execution call-depth limit and `--timeout-ms N` sets an absolute deadline measured from option parsing. The defaults are one million steps, 16 MiB and 256 frames, the reference's `low` profile. Exhausted quotas, deadlines, unknown functions, read failures and parse errors print their message on stderr and exit with status 1.

`--stats` prints the execution counters `steps=N peak_bytes=N retained_bytes=N` on stderr.

## Exit status

| Status | Meaning |
| --- | --- |
| 0 | The command succeeded, the check was clean, or watch mode stopped after an interrupt. |
| 1 | Any failure of the Go-style commands, including usage errors; in the flat form, reading, compiling or execution failed. |
| 2 | A flat-form usage error; nothing was read or executed. |

## Differences from the Go reference

Before the Rust implementation became the reference, a comparison script ran the Go CLI and this one on the help and error paths, `fmt` over every `.vibe` file in both trees plus generated whitespace cases, `run` and `analyze` over the script corpus, and a `test` suite, and all of them were identical. The analyzer was also compared on about 430,000 sources from the fixtures and generated programs with injected terminators; only three differed, where the two parsers disagree about a call on a parenthesized `begin` block. The `cli` [golden corpus](../tests/golden/README.md) records this CLI's results on the same kinds of invocations. These differences are intentional:

- `vibes --version` prints the version; the reference reports an undefined flag.
- The flat form, `vibes help flat` and the `check` flags `-e`/`-eval` and `-json` are extensions, and `vibes check --help` lists them. Each applies only where the reference reports an error.
- `vibes lsp` adds this library's checker findings to its diagnostics and reports only the first parse error; its other differences are listed [with the language server](lsp.md#differences-from-the-reference). The REPL's own differences are listed [with the REPL](#differences-from-the-go-repl).
- Every command compiles the static language, which the reference does not have: `check` reports the static type checker's diagnostics rather than the reference checker's findings, and the reference's `run -check`, which checked one invocation, is gone with that checker.
- Engine messages are the library's: the `require` not-found message, step accounting under small quotas, and stack traces, which omit the reference's final frame for the entry function of a script.
- Watch mode always polls, as the reference does when file notifications are unavailable, so a new module file that nothing edits is noticed by the periodic scan within five seconds rather than immediately.
- Under WASI, interrupts are not observed, and `fmt` opens files by path within the host's preopened directories instead of through root handles.
- Quoted values in error messages treat a few rare Unicode format characters as printable where Go escapes them.
