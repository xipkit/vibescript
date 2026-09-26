# Golden corpora

The Rust implementation is the reference. These files record what it observably does on every corpus that was once validated against Go v0.70.0, so validation needs no Go toolchain. `scripts/golden.py` runs each case against a build and fails on any observable difference:

```sh
python3 scripts/golden.py                          # build, then check every corpus
python3 scripts/golden.py --corpus replay,cli      # check some corpora; --list names them
python3 scripts/golden.py --record --corpus parse  # accept a deliberate change
```

It builds `examples/golden.rs`, the engine harness, and the `vibes` binary with the `gate` profile (release optimizations with parallel code generation); `--harness`, `--bin` and `--no-build` check other builds. A full check takes about a minute on ten cores after the build. `./scripts/check` runs it, and `scripts/compare.py --validate-only` runs the engine corpora against its portable and SIMD builds.

To check or re-record only affected cases, pass `--cases FILE`, where the JSON file maps corpus names to lists of exact case ids, for example `{"conformance": ["case_id"]}`. Use it with `--record --corpus conformance` to preserve every unselected observation and counter. Selected recordings still run twice, validate independent fixture expectations, and preserve the contents of unselected LSP replies when their shared table is renumbered.

| Corpus | Cases | Sources |
| --- | ---: | --- |
| `conformance` | 1,248 | generated cases in `scripts/fixtures.py` and the host-binding, required-file, capability, block and signature generators; the site and upstream programs; the benchmark cases |
| `language` | 106,416 | `tests/language.json` |
| `rejections` | 33,098 | runtime errors in `tests/language-errors.json` and compile errors in `tests/syntax-errors.json`, and the static rejections among them that carry a `static_error` |
| `compatibility` | 219 | the selected differences from Go: `docs/compatibility-cases.json`, the generators' policy cases and the sources in `docs/*-differences.json` and `docs/computed-call-gaps.json` |
| `replay` | 56,827 | the calls and compiles Go v0.70.0's test suite made, in `replay/` |
| `parse` | 35,965 | the site and upstream programs with a token deleted, duplicated or inserted, or cut after a line, as `scripts/mutations.py` makes them; parsed only |
| `cli` | 2,859 | `vibes` help and flag errors, `run`, `analyze` and `fmt` on each program in `tests/site`, `tests/upstream` and `examples`, `fmt` on generated whitespace files and whole trees, and `test` on a small suite |
| `lsp` | 216 sessions, 291,062 messages | `vibes lsp` over `tests/site`, `tests/upstream/examples` and `tests/lsp`, in the sessions of `scripts/lsp_sessions.py`, plus its malformed-message session |

## Format

Each `<corpus>.jsonl` or `.jsonl.gz` file has one JSON object per line, sorted by case id. The id is stable, and the source lives elsewhere, so a rewritten source is checked against the same observation. An engine case records one of:

- `"ok"`: the result as a typed-v1 node, which keeps value kinds, exact float bits, raw bytes and hash order (see [the site corpus](../site/README.md)). A NaN is `["float", "nan"]`, since scripts cannot observe its sign or payload and CPUs differ in them. Times and ranges have their own nodes; other values are `["opaque", kind, rendering]`.
- `"compiled": true` for a compile-only case that compiles.
- `"error"`: `phase` (`compile`, `call` or `setup`), `kind`, the script-visible `class`, `message`, and `at`, the one-based line and character column.

Output a case wrote is `"stdout"` and `"stderr"`, as text, or `{"hex": ...}` when it is not UTF-8. Values, messages and output longer than 4 KiB are recorded as `{"sha256", "bytes"}`. A command records `status`, `stdout`, `stderr` and, for `fmt -w`, a digest of the rewritten tree; paths under its scratch directory read `$TREE`. A language-server session records one index per message into `lsp.replies.jsonl.gz`, the distinct replies with request ids removed.

The runner fixes what a result could otherwise take from the host. Scripts draw entropy from a seeded generator unless a case fixes it. Every process takes its local zone, America/Detroit where the Go results were recorded, and the zones scripts name from the bundled tz database. `--record` runs every case twice; a case whose observation still differs between the runs, because it reads the clock, records only its outcome, such as `{"varies": "ok"}`, and is checked only for that. The goldens pass on macOS arm64, where they were recorded, and on Linux x86_64.

`language` keeps its goldens where they already were: the expected values and output in `tests/language.json`. Cases that carry an independent expectation, in `tests/language.json` or a fixture generator, are also checked against it, and `--record` refuses a build that misses one or crashes.

Accounting counters are separate, in `<corpus>.counters.jsonl.gz`: `[id, steps, peak bytes, retained bytes]` for each case that returned, or `[id]` when they varied. Changes are reported as counter drift and fail only with `--strict-counters`, since removing proven runtime checks will change step counts. Counters also differ slightly between platforms: on Linux x86_64 about 4,800 cases report drift, most of them eight bytes of peak memory. A `replay` case recorded under Go's tight quota whose outcome changes to or from a step or memory quota error is reported as accounting drift too, and fails only with `--strict-quota`.

## The replay corpus

`replay/` holds what a Rust-only replay needs from a recording of every `Script.Call` and `Engine.Compile` that Go v0.70.0's test suite made: `programs.jsonl.gz` has the 7,452 distinct sources, `inputs.jsonl.gz` the 1,314 distinct argument sets as typed-v1 nodes, and `cases.jsonl.gz` one line per case with its Go test, program, entry point, arguments and limits. 15,969 calls run with generous limits, 38,393 under the quota Go's test set, and 2,465 sources are only compiled. A one-off importer, since removed, built these files from the recorder's fixtures; the Go outcomes are not kept.

## Static types

Every source in these corpora is written in the language of [ADR-007](../../docs/adr/007-static-types.md) and [ADR-008](../../docs/adr/008-canonical-surface-for-ai-authors.md), and `golden.py` compiles every engine case with static types, declaring the globals and capabilities the case supplies by their values' types, as a statically typed host would: a case must compile and do what its golden records. A case whose purpose is to fail with static types carries `static_error`, the checker's first error as `{"code", "at"}`, which is checked as an independent expectation; its golden records the compile error.

The `parse` corpus records only whether each source parses: its token mutations deliberately produce malformed or partially valid programs, so a case records `compiled` when its source parses and otherwise the syntax error `Engine::compile` reports, whatever it would report about types. Static rejections belong in the semantic corpora, where their first diagnostic is recorded explicitly.

## History

The corpora were validated against Go v0.70.0 until the Rust implementation became the reference, and moved to the static language when static types became the only mode (2026-09-26). The migration rewrote their sources, turned the cases that tested removed features into static rejections and recorded each non-mechanical decision, one per case, in `migration-decisions.jsonl`; that file and the migration tooling are in the repository's history. Until the ADR-004 escape hatch and the runtime support for removed spellings were deleted, a static rejection's golden kept the outcome it had in the ADR-004 language; those goldens now record the compile error, and 611 more cases whose removed spellings the checker had missed became static rejections. Accounting counters were not re-recorded with either change, so they drift as described above.
