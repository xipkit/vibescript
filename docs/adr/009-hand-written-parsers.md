# ADR-009: Keep the hand-written parsers

## Status

Proposed - 2026-11-02

Answers VIBE-130, which asked whether [chumsky](https://codeberg.org/zesterer/chumsky)
would be worth adopting. Relates to [ADR-006](006-slim-language-for-predictable-sandboxing.md),
whose budgets the parsers are measured against, and [ADR-008](008-canonical-surface-for-ai-authors.md),
whose rules have a parser of their own.

## Decision

We will keep the three hand-written parsers of the Vibescript grammar: the
compiler's in `src/syntax`, the ADR-008 rules' in `src/surface/parse.rs` and
the signature files' in `src/signatures/parse.rs`. We will not adopt chumsky,
or another parser-combinator library, for any of them. A combinator library
remains an option for a new, small, context-free grammar off the compile
path; none is planned.

## Context

Vibescript is parsed by three hand-written parsers in this crate, all ported
decision for decision from the Go reference implementation, which `src/syntax`
cites 77 times:

| Parser | Lines | Role |
| --- | ---: | --- |
| `src/syntax` | 15,316, of which about 8,100 are grammar; the rest is the lexer (1,981), Unicode tables (1,096), the tree, charged copies and the V0003 rename reach | Compiles every script and required file; backs `tooling::outline`, `unreachable`, `member_receiver` and `tokens` |
| `src/surface/parse.rs` | 4,448 | Reads the compiler's tokens into a tree that keeps every construct's span, so the ADR-008 rules can rewrite spellings; runs only in the checker path, from `typing.rs` |
| `src/signatures/parse.rs` | 990 | The declaration subset the builtin table is written in; on the first-compile path |

The compiler's parser does more than recognise the grammar, and each of these
is a property a replacement must keep:

- **Metering.** 253 call sites charge steps, bytes and checkpoints to the
  compilation's `Work`; every tree node is allocated through the charged
  containers `Boxed`, `Buffer` and `Table`. Cancellation and the language
  server's two-second deadline reach the parser the same way.
- **Stack.** Recursion runs as async tasks on a heap stack
  (`compilation/tasks.rs`), bounded by a syntax depth of 1,024 and charged
  frame by frame. The WASI playground has no threads to move deep sources to.
- **Context.** The parser owns the locals table, the declared type names,
  whether `it` is declared, the command-call depth, line-expression counts
  and loop-condition state, and consults them to decide command arguments,
  block attachment and line continuation. A declared local turns `%w` into a
  modulo, and the parser then re-lexes the tokens after it: the token stream
  is mutated during the parse (`syntax/tokens.rs`, `expand_modulo`).
- **Errors.** The parse golden corpus holds 35,965 mutated programs, 29,029 of
  them rejected with an exact message, line and column, in 335 distinct
  message shapes such as `invalid hash pair: expected key like name: or "name":`
  and the adjacent-expressions advice. The parser also emits coded
  diagnostics with fixes: V0001, V0002 with its parenthesis fix, V0003 and the
  duplicate and reserved name codes.
- **Recovery.** A second specialisation of the same parse functions
  (`syntax/recovery.rs`), zero-sized on the success path, checkpoints 16
  parser fields and copies the scope's locals, synchronises on per-construct
  stop sets, reports at most 100 diagnostics without duplicates or cascades,
  bounds its work at 128 steps per source byte plus 16 KiB, keeps the first
  error identical to the fail-fast parse, and never publishes a partial tree.
  Runtime compilation of required files stays fail-fast.

These parsers are held to measured budgets:

- First compilation takes 746 µs on arm64 and 935 µs on x86_64 with 9,867
  allocations; `scripts/footprint.py --check` and `tests/footprint.rs` cap it
  at 16,000 allocations and engine construction at 16 allocations and 4 KiB
  ([startup parser report](../../benchmarks/startup-parser.md)).
- Clean parses must stay within 3% of the previous release; the 9.2 KB
  `massive` program parses in 540 µs and a 100 KB source with 10,000 errors
  recovers in 5.5 ms ([recovery report](../../benchmarks/parse-recovery.md)).
- The library's dependency graph on macOS is 16 crates besides itself, five of
  them direct: `memchr`, `getrandom`, `cap-std`, `cap-fs-ext` and `libc`.
- The playground is a 3.82 MB wasm (905 KB Brotli) built for WASI preview 1.

The parsers are checked by a nightly fuzz target over the `tooling` API,
233,865 paired engine and parser golden cases, and a 42,440-case parse sweep
that preserves every first error, applies every V0003 fix and asserts that the
rules' parser reads every source the compiler accepts.

Nothing outside the crate reads a syntax tree. The language server consumes
`Engine::compile_with_options`, `tooling`, `signatures` and
`Script::declarations`; the Zed, VS Code and Sublime packages highlight with
`tree-sitter-vibescript` or their own grammar files and launch `vibes lsp`;
Rip uses `tree-sitter-vibescript`. The `surface` module is private.

### Chumsky, as measured on 2026-11-02

Version 0.13.0 was released on 2026-05-06; 0.14 is on `main`.

- **Dependencies.** Without default features it adds seven crates: `chumsky`,
  `hashbrown`, `allocator-api2`, `equivalent`, `foldhash`, `unicode-ident`
  and `unicode-segmentation`. The default `stacker` feature adds `stacker`,
  `psm` (C and assembly, built with `cc`), `cfg-if` and `libc`. The library's
  graph would grow from 16 dependency crates to 23, or 25 with the default
  features, since `cfg-if` and `libc` are already present.
- **Code.** 19,552 lines with 135 `unsafe` sites; its `Input` trait has an
  `unsafe fn next_maybe`. The 0.13 changelog removes an unstable API "due to
  soundness issues".
- **Releases.** 0.9 (2023-02), a 1.0.0-alpha series, 0.10 (2025-03), 0.11
  (2025-09), 0.12 (2025-12), 0.13 (2026-05): a breaking release every three to
  six months. crates.io lists 1.0.0-alpha.8 as the latest version because of
  pre-release ordering.
- **Recovery.** Four strategies: `skip_then_retry_until`, `skip_until`,
  `nested_delimiters` and `via_parser`; the `Strategy` trait is sealed as an
  "unstable API". Backtracking restores the input cursor and the error count
  plus whatever the state type records (`Checkpoint { cursor, err_count,
  inspector }`): state added with `with_state` is rolled back only through the
  `Inspector` trait, where `RollbackState` clones the whole state at every save
  point and `SimpleState` is never rolled back. Errors collect in an unbounded
  `Vec`; there is no diagnostic-count or work bound.
- **Errors.** `Rich` carries an `expected: Vec<RichPattern>` built for each
  failed alternative and merged across `choice`, producing "expected X, found
  Y" at the furthest failure. Other wording comes from `map_err` on each rule.
- **Context.** `with_ctx`, `then_with_ctx` and `configure` pass a value
  forward; `with_state` holds mutable state; `custom` runs arbitrary code
  against the input. The input itself, `&[Token]` or a `Stream`, cannot be
  changed during the parse.
- **Stack.** `recursive` grows the stack through `stacker::maybe_grow`, which
  switches stacks through `psm`, whose documentation says it is not applicable
  to wasm. On the playground, and anywhere without the feature, recursion is
  native.
- **Compile time** on an Apple Silicon host, Rust stable, cold target
  directories: the `nano_rust` example (622 lines: lexer, parser and
  interpreter) takes 2.65 s wall and 9.2 CPU-seconds to recompile in release;
  the `json` example (157 lines) 1.1 s and 4.9 CPU-seconds; the library alone
  1.3 s. The vibescript library (143,484 lines) builds in release in 37 s wall
  and 39 CPU-seconds, and in debug in 10 s. A comment-only edit rebuilds the
  debug example in 0.25 s through incremental compilation; the gate and the
  release build use optimised profiles.
- **Speed.** Chumsky's own JSON benchmark on a Ryzen 7 3700X: chumsky 210 µs,
  the hand-written `sn` 238 µs, winnow 179 µs. That grammar is context-free,
  LL(1) and unmetered.

## Consequences

What stays easy:

- The startup, recovery, dependency and wasm budgets, the goldens and the
  parse sweep are untouched. No re-acceptance run on two hosts, no re-recording
  of 29,029 first errors, no new unsafe surface, no new release cadence to
  follow.
- The compiler's hot path keeps its tree without spans or tokens it never
  reads, and WASI keeps a parser whose depth does not depend on the native
  stack.

What we owe:

- Three grammars to keep aligned. A syntax change is made in the compiler, in
  the rules' mirror and, when it touches declarations, in the signature
  parser, and `scripts/parse-sweep.py` is what proves the mirror still reads
  everything the compiler accepts. That is the real cost of the current design
  and it stays.
- Recovery stays hand-synchronised. Each new construct chooses its stop set
  and the checkpoint fields it restores.
- The language server keeps re-anchoring outlines by section for documents
  that do not parse; a partial tree after recovery is still not published.

## Alternatives considered

### Chumsky for the compiler's parser

Each property above would live in `custom`, `try_map` or `with_state`
closures: charging and cancelling through a flag that turns every step into a
parse error, keeping the locals table in a `RollbackState` that clones it at
every save point where the current parser copies it only when recovering, and
re-lexing `%` tokens with a `custom` parser that reads ahead of the token
input it cannot change. The 335 message shapes would be reproduced with
`map_err` on nearly every rule. That is the hand-written parser again,
written inside combinators, for 7 to 9 more crates, more unsafe code, a
breaking release every few months and an optimised compile cost per grammar
line one to two orders of magnitude above this crate's. The 3% timing and
16,000-allocation budgets leave no room for `Rich` errors allocated per failed
alternative or for the backtracking the current parser avoids by consulting
its state, and neither can be sized without writing the whole grammar. The one
capability chumsky would add, a partial tree after recovery, is withheld on
purpose, so that no syntax failure reaches the checker; the existing `Recover`
mode already synchronises and could emit one if that decision changes.

### Chumsky for the rules' parser only

The mirror has to make the compiler's decisions, decision for decision, from
the compiler's tokens, under the same `stop` polling and the 48-level nesting
limit before it moves to a 256 MiB thread. Writing it in a second idiom makes
it harder to keep aligned, and command calls, line continuation and block
attachment would still be `custom` code.

### Chumsky for the signature parser

The only grammar of the three that combinators fit, and the one on the
first-compile path measured to the allocation. 990 lines of a stable grammar
are not worth a dependency on that path.

### Fold the rules' parser into the compiler's parser

A compiler tree that kept every span and token would remove the 4,448-line
mirror, which is the duplication that costs upkeep. It would also put spans
and tokens the compiler never reads on every node of the runtime compile path,
against the first-compile and retained-storage budgets, to serve a pass that
runs only under the checker. Worth measuring if the mirror's upkeep becomes
the dominant parser cost; it does not involve chumsky.

### Other libraries

`winnow` and `nom` share the metering, state and token-mutation mismatch and
offer weaker recovery; `pest` and `lalrpop` generate parsers from grammar
files that cannot express the context the parser consults.
