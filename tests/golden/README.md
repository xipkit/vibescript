# Golden corpora

The Rust implementation is the reference. These files record what it observably does on every corpus that was once validated against Go v0.70.0, so validation needs no Go toolchain. `scripts/golden.py` runs each case against a build and fails on any observable difference:

```sh
python3 scripts/golden.py                          # build, then check every corpus
python3 scripts/golden.py --corpus replay,cli      # check some corpora; --list names them
python3 scripts/golden.py --record --corpus parse  # accept a deliberate change
```

It builds `examples/golden.rs`, the engine harness, and the `vibes` binary in release mode; `--harness`, `--bin` and `--no-build` check other builds. A full check takes about a minute on ten cores after the build. `./scripts/check` runs it, and `scripts/compare.py --validate-only` runs the engine corpora against its portable and SIMD builds.

To check or re-record only affected cases, pass `--cases FILE`, where the JSON file maps corpus names to lists of exact case ids, for example `{"conformance": ["case_id"]}`. Use it with `--record --corpus conformance` to preserve every unselected observation and counter. Selected recordings still run twice, validate independent fixture expectations, and preserve the contents of unselected LSP replies when their shared table is renumbered.

| Corpus | Cases | Sources |
| --- | ---: | --- |
| `conformance` | 1,248 | generated cases in `scripts/fixtures.py` and the host-binding, required-file, capability, block and signature generators; the site and upstream programs; the benchmark cases |
| `language` | 106,791 | `tests/language.json` |
| `rejections` | 32,723 | runtime errors in `tests/language-errors.json` and compile errors in `tests/syntax-errors.json`, and the static rejections among them that carry a `static_error` |
| `compatibility` | 219 | the selected differences from Go: `docs/compatibility-cases.json`, the generators' policy cases and the sources in `docs/*-differences.json` and `docs/computed-call-gaps.json` |
| `replay` | 56,827 | the calls and compiles Go v0.70.0's test suite made, in `replay/` |
| `parse` | 35,965 | the site and upstream programs with a token deleted, duplicated or inserted, or cut after a line, as `scripts/parse-sweep.py` makes them; compiled only |
| `cli` | 2,859 | `vibes` help and flag errors, `run`, `analyze` and `fmt` on each program in `tests/site`, `tests/upstream` and `examples`, `fmt` on generated whitespace files and whole trees, and `test` on a small suite |
| `lsp` | 216 sessions, 291,062 messages | `vibes lsp` over `tests/site`, `tests/upstream/examples` and `tests/lsp`, as `scripts/lsp-transcripts.py` drives it, plus its malformed-message session |

## Format

Each `<corpus>.jsonl` or `.jsonl.gz` file has one JSON object per line, sorted by case id. The id is stable, and the source lives elsewhere, so a rewritten source is checked against the same observation. An engine case records one of:

- `"ok"`: the result as a typed-v1 node, which keeps value kinds, exact float bits, raw bytes and hash order (see [the site corpus](../site/README.md)). A NaN is `["float", "nan"]`, since scripts cannot observe its sign or payload and CPUs differ in them. Times and ranges have their own nodes; other values are `["opaque", kind, rendering]`.
- `"compiled": true` for a compile-only case that compiles.
- `"error"`: `phase` (`compile`, `call` or `setup`), `kind`, the script-visible `class`, `message`, and `at`, the one-based line and character column.

Output a case wrote is `"stdout"` and `"stderr"`, as text, or `{"hex": ...}` when it is not UTF-8. Values, messages and output longer than 4 KiB are recorded as `{"sha256", "bytes"}`. A command records `status`, `stdout`, `stderr` and, for `fmt -w`, a digest of the rewritten tree; paths under its scratch directory read `$TREE`. A language-server session records one index per message into `lsp.replies.jsonl.gz`, the distinct replies with request ids removed.

The runner fixes what a result could otherwise take from the host. Scripts draw entropy from a seeded generator unless a case fixes it. Every process takes its local zone, America/Detroit where the Go results were recorded, and the zones scripts name from the bundled tz database. `--record` runs every case twice; a case whose observation still differs between the runs, because it reads the clock, records only its outcome, such as `{"varies": "ok"}`, and is checked only for that. The goldens pass on macOS arm64, where they were recorded, and on Linux x86_64.

`language` keeps its goldens where they already were: the expected values and output in `tests/language.json`. Cases that carry an independent expectation, in `tests/language.json` or a fixture generator, are also checked against it, and `--record` refuses a build that misses one or crashes.

Accounting counters are separate, in `<corpus>.counters.jsonl.gz`: `[id, steps, peak bytes, retained bytes]` for each case that returned, or `[id]` when they varied. Changes are reported as counter drift and fail only with `--strict-counters`, since removing proven runtime checks will change step counts. Counters also differ slightly between platforms: on Linux x86_64 about 4,800 cases report drift, most of them eight bytes of peak memory. A `replay` case recorded under Go's tight quota whose outcome changes to or from a step or memory quota error is reported as accounting drift too, and fails only with `--strict-quota`.

## Migrated sources

`--export DIR` writes every source to `DIR/<corpus>/`, with an `index.json` from source keys to files. A source key is the case id, `<case id>::<file>` for a file the case requires, or a program id for a `replay` program many cases share. After rewriting the files in place, `--sources DIR` runs them against the originals' goldens. `--override FILE` does the same from a JSON map `{"corpus": {"source key": "source"}}`. An error whose position alone moved in a rewritten source is reported as position drift and does not fail; a value that embeds a position, such as a rescued backtrace, still does. `parse`, `cli` and `lsp` observe today's syntax and tools; they are re-recorded when those change, not migrated.

## The replay corpus

`replay/` holds what a Rust-only replay needs from a recording of every `Script.Call` and `Engine.Compile` that Go v0.70.0's test suite made: `programs.jsonl.gz` has the 7,452 distinct sources, `inputs.jsonl.gz` the 1,314 distinct argument sets as typed-v1 nodes, and `cases.jsonl.gz` one line per case with its Go test, program, entry point, arguments and limits. 15,969 calls run with generous limits, 38,393 under the quota Go's test set, and 2,465 sources are only compiled. `scripts/import-replay.py` built these files from the recorder's fixtures; the Go outcomes are not kept.

## Static types

Every source in these corpora is written in the language of [ADR-007](../../docs/adr/007-static-types.md) and [ADR-008](../../docs/adr/008-canonical-surface-for-ai-authors.md), and `golden.py` compiles every engine case with static types, declaring the globals and capabilities the case supplies by their values' types, as a statically typed host would: a case must compile and do what its golden records. A case whose purpose is to fail with static types carries `static_error`, the checker's first error as `{"code", "at"}`, and is checked against that instead. Its golden keeps the outcome it had in the ADR-004 language before the switchover, and recording keeps it too; a case that became a static rejection with the switchover records its compile error.

The `parse` corpus runs without static types, through the ADR-004 engine that `vibes migrate` uses (`Engine::legacy_unchecked`).
Its token mutations deliberately produce malformed or partially valid programs;
it records parser acceptance and syntax errors, not semantic validity. Static
rejections belong in the semantic corpora, where their first diagnostic is
recorded explicitly.

### The switchover

When static types became the only mode (2026-09-26), these goldens were re-recorded, and only for these rules:

| Rule | `language` | `rejections` | `replay` | Total |
| --- | ---: | ---: | ---: | ---: |
| `/` is true division | 0 | 0 | 0 | 0 |
| `fill` and `insert` raise past the end | 6 | | 16 | 22 |
| a function or method without `-> T` returns `nil` | 129 | 32 | 2 | 163 |
| a class's `==` is typed by its declared result | 2 | | | 2 |
| removed syntax stops parsing | | 83 | 1 | 84 |

- True division changed no case: the migration rewrote every `/` between ints as `//`.
- Of the `language` cases, 51 took a new expected value, 12 now raise at runtime and 74 fail to compile; the last two groups moved to `tests/language-errors.json`. Of the 32 `rejections` cases, 10 raised at runtime and now fail to compile, and 22 whose private `==` declares no result now report `V0101` before `V0208`; each carries its new `static_error` and keeps its golden.
- Most of the `nil` returns are operator methods without `-> T`, such as `def ==(other: int); false; end`. Since `==` and `!=` of a class give what its method returns, several now fail to compile where the method's result reaches a typed position. Two `language` cases declare `==` with a non-bool result, which the checker used to type as `bool`.
- Of the 44 `surface-at-flip` cases, 14 changed; the other 30 report the same first error in the canonical grammar. 69 more parse-error cases changed for the same rule: malformed parameter lists that the removed `name: default` keyword form used to read. `tests/syntax-errors.json` gives each its new message in `error`.
- The `cli` corpus changed in 62 cases: the help and flag errors of the commands whose `--static` and `--check` flags were removed (30), and `run` and `analyze` of the 16 programs that do not type check without their host's capabilities or are deliberately ill-typed, which now fail to compile. The `test` suite is written in the static language and records the same reports.
- 214 of the 216 `lsp` sessions changed: the server advertises quick fixes, and the diagnostics of 55 documents are the type checker's instead of the gradual checker's findings.
- Accounting was not re-recorded. As with `--static` before, a statically typed host checks each call's declared globals and capabilities at entry, so counters drift and 330 `replay` cases recorded under Go's tight quota change quota outcome; all of it is reported as accounting drift and fails only with `--strict-counters` or `--strict-quota`.

The migration's non-mechanical decisions are in [migration-decisions.jsonl](migration-decisions.jsonl), one per case, sorted by corpus and id, each with a short reason:

- `converted-to-rejection`: the case tests a feature ADR-007 or ADR-008 removed, or deliberately contradicts a type, and now carries the `static_error` recorded with it. Language cases moved to `tests/language-errors.json` (`language -> rejections`).
- `deleted-redundant`: the case tested only a removed feature, and `equivalent` names the rejection case that remains for its diagnostic code and spelling.
- `rewritten`: rewritten by hand into its canonical equivalent. `outcome_changed` marks the few whose expected result changed on purpose (a message naming a renamed type, a dropped element that used a removed spelling); `expected` gives a language case's new value.
- `sloppiness-removed`: blocks, keywords, arguments or block parameters the runtime ignored are gone.
- `surface-at-flip`: a parse-error case whose malformation is itself removed syntax (a percent literal, `do`, `unless`, `until`, a `name:` keyword parameter); its expected message changes when that syntax stops parsing.
- `restored`: a case once converted to a rejection because the checker refused what it does, restored as written after a checker fix made it type check.

`checker_issue` marks decisions that depend on a known checker or runtime issue, such as a conversion that records the error the checker reports today; they are the ones to revisit when the checker changes.

| Corpus | converted | deleted | rewritten | sloppiness | at the flip | restored | checker issue |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| `language` |  | 5,653 | 3,938 | 3,058 |  | 88 |  |
| `language -> rejections` | 6,476 |  |  |  |  |  | 12 |
| `rejections` | 12,695 | 1,020 | 42 | 148 | 44 |  |  |
| `replay` | 3,356 | 906 | 3,207 | 40 |  | 13 |  |
| `conformance` | 187 |  | 78 |  |  | 54 |  |
| `compatibility` | 143 |  | 9 | 2 |  | 28 |  |
| `tests/site`, `tests/upstream`, `tests/lsp` | 10 |  | 44 |  |  |  |  |

The sources `vibes migrate` rewrote without any of these are not listed. The parse-error programs were migrated with their malformed lines set aside, so each keeps its message and position.
