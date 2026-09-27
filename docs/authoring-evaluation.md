# AI authoring evaluation

This study used one Codex session to author 40 glue programs from the public
language documentation and CLI. The baseline is `a23d4ece7968c6cbdaa572b2e898929ec0c4193d`.
The authoring machine was an Apple M4 with 16 GiB of RAM. This is a small,
non-blind case study, not an estimate of how all models perform.

## Results

An attempt is one saved source revision, including its test and required files.
It passes only when `vibes check --json` accepts both the program and test and
`vibes test --profile low` passes the behavioral assertions. Warnings do not
fail a check. The 40 initial drafts were all written before receiving feedback.

| Measure | Before | After, informed rewrite |
| --- | ---: | ---: |
| Programs passing on first attempt | 37/40 (92.5%) | 40/40 (100%) |
| Mean attempts per program | 1.075 | 1.000 |
| Programs passing within two attempts | 40/40 | 40/40 |
| First drafts passing both compile checks | 40/40 | 40/40 |
| Unchanged original drafts passing on the new compiler | — | 37/40 (92.5%) |

The after pass starts from the saved original drafts, rewrites the three that
failed, and leaves the other 37 unchanged. The author already knew the failures;
it is a repair/rewrite measurement, not a fresh independent author sample.
Two of the three failures came from the Python source writer consuming string
escapes. They remain in the headline numbers. Excluding those two programs
entirely gives a descriptive before rate of 37/38 (97.4%), not a corrected rate
for all 40. The one remaining failure was a wrong rescue class for JSON input
validation. Documentation changes do not make the unchanged sources pass.

The corpus covers typed JSON projections, numbered and cursor pagination,
webhook validation, retries and per-item error recovery, aggregation, templates,
CSV and percent formatting, UTC time and durations, money, exhaustive enum
cases, typed class fields, required modules, regex logs, and dictionaries and
arrays of records. Capability workflows use typed blocks and deterministic
script doubles. They do not claim to exercise actual network adapters or the
host's capability registration. The corrected Rust capability examples were
separately compiled and executed.

## Causes and repairs

The operational programs incurred three extra attempts, ranked by frequency:

| Cause | Programs | Extra attempts | Response |
| --- | ---: | ---: | --- |
| Source-writer escaping | 2 | 2 | Preserve Vibescript escapes with raw strings; freeze exact source bytes in the dataset. This is an author tooling mistake. |
| JSON validation rescue class absent from author docs | 1 | 1 | Document `JSON.parse_as` schema/syntax failures as `RuntimeError`, separately from `.as(T)` failures (`TypeError`) and Rust `ErrorKind::Type`. |

No original program used a removed spelling. No observed runtime failure was a
missing static type check: dynamic JSON validation is necessarily a runtime
boundary, and the two misquoted strings were valid programs with the wrong
behavior. The dataset does not manufacture first-draft spelling mistakes.

A separate set of 20 deliberately seeded repair exercises tests habits from
Ruby, Crystal, Python, JavaScript and Go, plus common typing mistakes. They are
not included in the 40-program pass rate. Every initial exercise is intentionally
invalid; the relevant measure is whether its first repair succeeds.

| Repair exercise measure | Before | After |
| --- | ---: | ---: |
| First repair compiles and runs | 18/20 (90%) | 20/20 (100%) |
| Mean attempts, including deliberately invalid draft | 2.10 | 2.00 |
| Diagnostic or fix directly supplies the repair | 12/20 (60%) | 17/20 (85%) |
| V04xx exercises with a correct canonical spelling and automatic edit | 9/9 | 9/9 |

The nine V04xx exercises produced ten diagnostics because the Crystal `String`
exercise used it in both a parameter and result annotation. All their automatic
edits worked. Foreign spellings that never existed in Vibescript appropriately
use V0201/V0203 rather than pretending to be removed aliases.

### Diagnostics that performed worst

| Code | Evidence before | Repair |
| --- | --- | --- |
| V0107, optional use | `optional_capture`: an `always` edit changed `m[1]` into `m.fetch(1)`. Match data has no `fetch`, so this introduced V0203 and cost a third attempt. | Only offer `fetch` when the indexed receiver and fetched element type support it. Do not offer it for strings, match data, custom indexers, range slices or nullable stored elements. An indexed optional member call without a safe edit explains how to bind and nil-test a local. |
| V0114, non-exhaustive case | `exhaustive_case`: the message listed `:closed`. Writing `when :closed` then failed with V0101 and needed `State::Closed`, costing a third attempt. | List qualified enum members in their actual declaration spelling, including names such as `InReview`. |
| V0203, unknown member | Three JavaScript exercises (`filter`, `trim`, `includes`) gave no canonical equivalent; the bad V0107 edit also produced a fourth V0203. | Offer `select`, `strip`, and `include?` as suggestions on the applicable builtin receiver types. Preserve rejection and do not rewrite user-defined methods or automatically guess argument/block semantics. |
| V0201, undefined name | `len`, `fmt.Sprintf`, and `strings.ToLower` required prelude lookup. No diagnostic named the equivalent operation. | Add a quick lookup to the prelude. Keep name resolution unchanged; host globals and capabilities may legitimately use those namespace names. These three still do not get direct repair advice from their diagnostic. |

The remaining exercises (`JSON.parse` indexing, a dynamic key on a record,
and an untyped empty array) were repaired directly from their diagnostics.
The generated edits are regression-tested by applying them, recompiling and
running the resulting code, with negative coverage for invalid `fetch` repairs.

## Documentation and prelude

The prelude now starts with common glue operations, distinguishes signature-only
notation from script declarations, and explains optional reads, regex captures,
exact/open shapes and validation error classes. Builtin signatures are unchanged.

The author docs now distinguish static diagnostics from runtime errors and
explain labels and fix applicability. They include webhook and regex capture
recipes. The initializer shorthand assigns an already declared field; it does
not declare one. A new documentation example exposed that ambiguity during
Phase 2, and V0204 directly identified its repair. It is not counted as a
Phase 1 program failure.

The capability guide now declares templates before compiling, declares data
fields before publication, and narrows unsigned block values. Executing all
four Rust examples caught another stale `config` example with V0201; all four
corrected examples pass. The CLI guide describes the current commands rather
than comparing each to the retired Go implementation. Computed calls and
callable hash fields are no longer described as supported. Required files are
checked before execution; runtime loading failures are described separately.
Topic guides describe current behavior. The compatibility page explicitly marks
its examples as historical; compatibility records and ADRs retain their history.

## Proposals, not language changes

1. **Use one script exception class for typed input mismatches.** A webhook
   author selected `TypeError | ArgumentError`, but `JSON.parse_as` raised
   `RuntimeError`. A separate dynamic `.as(int)` probe raised `TypeError` for
   the analogous mismatch. This inconsistency cost one operational attempt and
   makes a broad rescue tempting. Consider standardizing both on `TypeError`,
   with explicit migration treatment for existing rescue clauses. Syntax
   failures should remain distinguishable. This study changes documentation,
   not exception semantics.
2. **Consider required capture access using the existing `fetch` operation.**
   An optional match capture led the existing repair system to emit a method
   that does not exist. A `match_data.fetch(group)` operation could provide the
   same missing-value policy as array/hash `fetch`, while preserving optional
   `[]` reads. Evidence is one seeded exercise; the operational log parser
   already succeeds with checked casts, and a local nil test also works.
   Keep this lower priority than fixing incorrect diagnostic edits.
3. **Publish a typed contract independently of a capability factory.** CLI
   tests cannot grant host capabilities, and the old factory-based guide
   examples did not provide a statically callable service. A host API for a
   declared factory namespace could combine per-call state with published
   method/data contracts. Assess this separately from adding dynamic dispatch;
   no language or host API change is made here. The evidence is the documented
   examples and the testability limitation, not a failed real network call.

There is no evidence here to justify changing condition truthiness, collection
value semantics, enum exhaustiveness, or canonical spellings.

## Blind replication with a second model

GLM 5.3 wrote the same 40 programs blind, at `4b8c9aef`.
- **What it had:** only `README.md`, the user guides in `docs/` (without this
  report, `vm.md`, `compiler-accounting.md`, `language-port.md` and
  `compatibility.md`), the `vibes prelude` output, and each task's
  `*_test.vibe` as its acceptance test.
- **What it couldn't do:** run or check code, or see the implementation, the
  corpus or these solutions. An audit of its file accesses confirmed it read
  nothing outside the trial directory.
- **Result:** all 40 programs passed `vibes check` for both program and test,
  and passed `vibes test --profile low`, on the single attempt. No diagnostics.
- **Scoring check:** the scorer was confirmed to fail both a behavioural change
  and a type error, and no solution hard-codes an asserted output.
- **Caveat:** the guides it read include the recipes this study added. The
  replication shows those guides suffice for a model that has never seen the
  language; it isn't an independent measure of the original docs.

The model's notes name the places where it had to guess, all of which it
guessed right:
- float rendering in `format`;
- `Duration#iso8601` and `Time#iso8601` (trailing `Z`) output;
- the `strftime` directives;
- the money rounding mode;
- CSV quoting;
- the type of a `begin` expression whose `rescue` ends in `retry`;
- zero-argument block types (`&block: () -> any`);
- `...` inside a nested shape field;
- `sort` with a two-parameter comparator.

Each of these deserves an explicit example in the guides. The prompt, the
solutions, the model's notes and the scores are under `.cache/blind-trial/` on
the external volume.

## Evidence and reproduction

[The JSON Lines dataset](../corpus/glue/evaluation.jsonl) contains every source
revision, SHA-256 digest, diagnostic (including fixes), command status, failed
behavioral output, and qualitative assessment. It also records the unchanged
first-draft replay and all seeded exercises separately. Raw command streams,
source snapshots, baseline/updated binaries and exploratory doc checks are kept
under `/Volumes/AI/Work/xipkit/vibescript.rs/.cache/authoring-eval/evidence/`.
The baseline executable is `vibes-before`; the updated one is `vibes-after`.
No raw benchmark data or binary is committed.

```sh
python3 scripts/authoring-eval.py summary
python3 scripts/authoring-eval.py replay --bin target/debug/vibes \
  --drafts original --out .cache/authoring-eval/replay-original
python3 scripts/authoring-eval.py replay --bin target/debug/vibes \
  --drafts revised --out .cache/authoring-eval/replay-revised
```

Replay directories must be fresh. Original-draft replay is expected to return
failure for the three recorded programs; revised replay expects 40 passes.
Replaying is a reproducibility check, not another authoring trial.

The maintained [glue corpus](../corpus/glue/README.md) runs through `vibes test`
in the normal workspace tests. It lives outside `examples/` so adding it does
not extend the historical CLI golden sweep with unrecorded cases. The existing
golden corpora and counter files are unchanged. No runtime code was changed,
so performance benchmarks and Counter log entries are not required. Full local
and remote validation logs live under `.cache/authoring-eval/verification/`;
the completion report records their results and the checked branch head.

The baseline already differs from one saved replay counter: `call2881` records
54 steps in its golden but both preserved baseline and updated CLIs report
56 steps, 4,611 peak bytes and zero retained bytes. That existing discrepancy
was verified on both binaries and was not re-recorded. The golden runner also
reports its existing non-blocking quota-outcome drift separately from failures.
