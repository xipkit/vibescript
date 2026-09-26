# Golden corpora

The Rust implementation is the reference. These files record what it observably does on every corpus that was once validated against Go v0.70.0, so validation needs no Go toolchain. `scripts/golden.py` runs each case against a build and fails on any observable difference:

```sh
python3 scripts/golden.py                          # build, then check every corpus
python3 scripts/golden.py --corpus replay,cli      # check some corpora; --list names them
python3 scripts/golden.py --record --corpus parse  # accept a deliberate change
```

It builds `examples/golden.rs`, the engine harness, and the `vibes` binary in release mode; `--harness`, `--bin` and `--no-build` check other builds. A full check takes about a minute on ten cores after the build. `./scripts/check` runs it, and `scripts/compare.py --validate-only` runs the engine corpora against its portable and SIMD builds.

| Corpus | Cases | Sources |
| --- | ---: | --- |
| `conformance` | 1,248 | generated cases in `scripts/fixtures.py` and the host-binding, required-file, capability, block and signature generators; the site and upstream programs; the benchmark cases |
| `language` | 107,667 | `tests/language.json` |
| `rejections` | 31,847 | runtime errors in `tests/language-errors.json` and compile errors in `tests/syntax-errors.json`; with static types, the cases that carry a `static_error` |
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

Every source in these corpora is written in the language of [ADR-007](../../docs/adr/007-static-types.md) and [ADR-008](../../docs/adr/008-canonical-surface-for-ai-authors.md). `golden.py --static` compiles every engine case with the static checker, declaring the globals and capabilities the case supplies by their values' types, as a statically typed host would, and checks it against the same goldens: a case must compile and do what its golden records. A case whose purpose is to fail with static types carries `static_error`, the checker's first error as `{"code", "at"}`, and is checked against that instead; its golden still records what it does without static types, which is what the goldens check until the switchover makes static types the default.

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
| `language` |  | 5,653 | 4,069 | 3,062 |  | 4 |  |
| `language -> rejections` | 5,674 |  |  |  |  |  | 98 |
| `rejections` | 12,191 | 1,020 | 216 | 149 | 44 |  | 18 |
| `replay` | 3,301 | 906 | 3,246 | 42 |  |  | 28 |
| `conformance` | 197 |  | 122 |  |  |  | 86 |
| `compatibility` | 171 |  | 9 | 2 |  |  | 20 |
| `tests/site`, `tests/upstream`, `tests/lsp` | 9 |  | 44 |  |  |  |  |

The sources `vibes migrate` rewrote without any of these are not listed. The parse-error programs were migrated with their malformed lines set aside, so each keeps its message and position.
