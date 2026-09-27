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
| `conformance` | 1,326 | generated cases in `scripts/fixtures.py` and the host-binding, required-file, capability, block and signature generators; the site and upstream programs; the benchmark cases |
| `language` | 106,389 | `tests/language.json` |
| `rejections` | 33,125 | runtime errors in `tests/language-errors.json` and compile errors in `tests/syntax-errors.json`, and the static rejections among them that carry a `static_error` |
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

Accounting counters are separate, in `<corpus>.counters.jsonl.gz`: `[id, steps, peak bytes, retained bytes]` for each case that returned, or `[id]` when they varied. Changes are reported as counter drift and fail only with `--strict-counters`. A change that alters accounting on purpose, such as removing a runtime check the checker proves, re-records the counters of the affected corpora with `--record` and keeps every observation; its commit says why the counters moved, and [the counter log](#counter-log) lists it. Counters also differ slightly between platforms: on Linux x86_64 about 4,800 cases report drift, most of them eight bytes of peak memory. A `replay` case recorded under Go's tight quota whose runtime outcome changes to or from a step or memory quota error is reported as accounting drift too, and fails only with `--strict-quota`. A compile failure is always an observable difference, even when the other outcome is a quota error.

## The replay corpus

`replay/` holds what a Rust-only replay needs from a recording of every `Script.Call` and `Engine.Compile` that Go v0.70.0's test suite made: `programs.jsonl.gz` has the 7,452 distinct sources, `inputs.jsonl.gz` the 1,314 distinct argument sets as typed-v1 nodes, and `cases.jsonl.gz` one line per case with its Go test, program, entry point, arguments and limits. 15,969 calls run with generous limits, 38,393 under the quota Go's test set, and 2,465 sources are only compiled. A one-off importer, since removed, built these files from the recorder's fixtures; the Go outcomes are not kept.

## Static types

Every source in these corpora is written in the language of [ADR-007](../../docs/adr/007-static-types.md) and [ADR-008](../../docs/adr/008-canonical-surface-for-ai-authors.md), and `golden.py` compiles every engine case with static types, declaring the globals and capabilities the case supplies by their values' types, as a statically typed host would: a case must compile and do what its golden records. A case whose purpose is to fail with static types carries `static_error`, the checker's first error as `{"code", "at"}`, which is checked as an independent expectation; its golden records the compile error.

The `parse` corpus records only whether each source parses: its token mutations deliberately produce malformed or partially valid programs, so a case records `compiled` when its source parses and otherwise the syntax error `Engine::compile` reports, whatever it would report about types. Static rejections belong in the semantic corpora, where their first diagnostic is recorded explicitly.

## History

The corpora were validated against Go v0.70.0 until the Rust implementation became the reference, and moved to the static language when static types became the only mode (2026-09-26). The migration rewrote their sources, turned the cases that tested removed features into static rejections and recorded each non-mechanical decision, one per case, in `migration-decisions.jsonl`; that file and the migration tooling are in the repository's history. Until the ADR-004 escape hatch and the runtime support for removed spellings were deleted, a static rejection's golden kept the outcome it had in the ADR-004 language; those goldens now record the compile error, and 611 more cases whose removed spellings the checker had missed became static rejections. Accounting counters were not re-recorded with either change; they were brought up to date afterwards, as the counter log records.


## Soundness audit after legacy deletion

The soundness changes were rebased onto the legacy deletion at `bab6498`. Its
fixtures and goldens were kept, then the intentional source changes and static
expectations were reapplied. Its broader removal rules handle item 8; a typing
fallback covers scoped removed calls on literal values that would otherwise
fail silently. Ambiguous call shapes have no automatic fix.

Relative to that baseline, 28 more language cases become static rejections:
compound class-constant writes and removed or invalid scoped member calls.
`error_handling_109_binding_implicit_it` returns to the language corpus because a
rescue binding has its own error-typed scope. `iteration_to_hash_duplicates`
uses a string key in its checked pair, preserving the duplicate-key result.
There are 176 corrected static expectations among existing rejection fixtures;
their goldens have 193 changed observations, principally earlier type errors,
accurate spans (including interpolations and instance parameters), and the eight
`operator_visibility_*` cast failures that now raise `TypeError`.

The five replay cases `call2512`, `call2515`, `call2518`, `call2521` and
`call2524` are static rejections, each expecting `V0101` at 4:19: their
`fetch_values` splat supplies symbols where string keys are required. Their old
step-quota outcomes no longer conceal the compile failure. Positive checker and
runtime tests cover a string splat. The replay corpus has 64 changed static
expectations and 85 changed observations, including the restored rescue cases
`call15776` and `compile1153`, invalid module annotations, checked block results,
unsupported conversions and selectors, tuple mutation, and module-resolution
reasons.

Eleven replay sources are rewritten while preserving their runtime purpose:
`call41858` declares its returned `to_h` pair as `[string, int]`; `call41862`
uses `.fetch(0)` after `flat_map` so the retained hash is non-nullable.
`call41884`–`call41887` and `call54673`–`call54677` use `.new` instead of `.new()`
inside interpolation. Correct interpolation spans now let the surface rules
recognize these constructors; the rewrites keep their string-conversion tests.

The conformance corpus has 86 changed observations for invalid `require` calls,
alias conflicts, declared data called as a function, and module-resolution
reasons. One CLI observation adds the invalid string-selector diagnostic.
All 216 LSP sessions retain their reply indexes; 615 shared replies change:
613 syntax diagnostics retain `V0001`, and two completion documents describe
`flat_map` accepting scalar or array block results. Compatibility and parse
observations are unchanged. Only affected cases were recorded; accounting
counters were kept.

## Checker differential fixes

The [differential tests](../../docs/checker-diff.md) found programs the checker accepted although the runtime, with the checks it leaves out, could give a value of another type. Their fixes reject such programs, and some corpus programs with them. A program whose purpose does not depend on the construct now rejected was rewritten, keeping its observation; one that tests the construct itself became a static rejection, its golden the compile error. Only the affected cases were recorded.

Rewritten sources, with unchanged observations:

- Records whose keys a program removes or replaces (V0123) are declared as dictionaries, or with the removed fields optional: language `member_hash_delete`, `member_hash_replace`, `rendering_inspect_composites_3`, the eight `mutable_block_hash_{delete_if,keep_if}_*` and six `mutable_block_hash_filter_*` cases; compatibility `mutable_block_hash_filter_changes_noop`; and the replay programs of `call1362`, `call1682`, `call1874`, `call2018`, `call33281`, `call41645`, `call41857`, `call45778`, `call4886`, `call52168`, `call52170` and `call54594`, which 98 cases run.
- `deep_transform_keys` renames keys, so its result is a dictionary: the result types of 30 language `collection_blocks_deep_*` cases, compatibility `collection_blocks_deep_mutation_3_False`, and the replay programs of `call41657` and `call7200`, which 20 cases run.
- A `break` forwarded through a `yield` inside a block of the callee has the callee's result type: `block_forwarded_break_boundary` and `iteration_break_forward_yield` declare `any` results.
- An empty literal's element type no longer accepts every value: `iteration_empty_fetch` and the four `mutable_block_delete_*` cases declare what their `fetch` or `delete` can give, and `compile1306`'s `delete` block gives an int.
- The parts of an array destructured into an element or field may be missing: the ten replay programs `call53723` to `call53737` declare their array as a tuple, and `call40515`'s `bump` returns one.
- A loop that ends a function gives the value its body had last, or `nil` when it never ran: `call54843` and `call55016` admit `nil` in their result.
- An instance has `inspect` only when its class defines it: the classes of `call49100` and `call49148`, whose `break` makes `new` give another value that they inspect, define it.

The typed locals of the memory threshold programs `call1682`, `call1874` and `call2018` build their record with less memory, so 20 of their quota cases now exceed the quota at a later line; `golden.py` reports them among the quota outcomes that follow accounting drift.

New static rejections:

- `lifecycle_chained_dup_bare` moves to the rejections (V0415): the runtime updates the field `dup` of `h.dup.clear`, where the checker typed a copy of `h`.
- `module_surface_39` and `module_binding_12` move to the rejections (V0203): a compound assignment to a module constant needs a setter, as a plain one does.
- Rejections that failed when called now fail to compile: `module_surface_13` and `class_surface_24` (V0203, `inspect` and `to_s` of a module and an instance that do not define them), and `module_setters_19`, `module_setters_27` and `module_setters_35` (V0208, a compound assignment calls the setter, which is not visible there).
- Replay: `call54753` to `call54759`, `compile628`, `compile634` and `compile648` (V0205, `initialize` reads a property, or lets `self` escape to code that may, before assigning it), `compile1029` (V0101, a symbol assigned to an enum parameter stays a symbol), and `compile459` and `compile1356` (V0123).

`control_called_function_own_loop` and `destructure_groups_each_bind_a_level` move from the rejections to the language corpus: a `for` loop over a literal with elements always runs its body, and a nested array literal destructured by a nested pattern is a tuple, so both now compile, and the differential tests confirm their values.

Changed first errors: 30 `class_accessors_*` rejections report the setter whose type differs from its variable's at the declaration, `sum_initial` reports `[].sum(nil)`, and the replay expectations of `call2528`, `call3017`, `call41693`, `call41961`, `call54722`, `call54723`, `call54725`, `compile408`, `compile533` and `compile884` name the earlier error the checker now finds. Seven rejection messages say `array<never>` instead of `array<unknown>` for the result of a call whose block always breaks. Two LSP completion replies, which 197 sessions give, describe `array<T: int>.sum` as returning `int`.
## Required captures and foreign-name advice

The 2026-09-27 language changes add `match_data.fetch` and V0201 advice for
familiar foreign names. Re-recorded `lsp.replies.jsonl.gz`: exactly three shared
completion replies append `match_data.fetch(group: number | string) -> string`
to the documentation for `fetch`. These replies occur 11,125 times across 197
selected sessions. Every other reply field is unchanged. `lsp.jsonl.gz` keeps
all 216 sessions and their reply indexes byte-for-byte; no engine, parser or CLI
observation changed. No counters were re-recorded, so this adds no Counter log
entry. The reply audit is in `.cache/language-1/lsp-audit.json` on the main
checkout's external volume.

## Counter log

Each re-recording of the counters, and why. Observations stay as recorded, including the `replay` outcomes that follow accounting drift into or out of a quota error. Cases that load required files charge work for the paths of their scratch files, so about 150 `conformance` and `compatibility` cases drift by a few steps and bytes in a checkout at another path; these counters were recorded in a checkout at `/private/tmp/vibescript-typed-vm`.

- Re-recorded all engine corpora on macOS arm64 at the start of the typed VM work, so that later changes show only their own drift. The static-language migration had rewritten the sources without re-recording their counters. 322 `replay` cases keep their recorded quota outcomes.
- Removed the runtime checks the checker proves: parameters of calls from script code, results of functions other than instance methods, typed locals, `yield` arguments and block results, when the type has no named part and every hash key type admits strings. Steps drop by the checks' work in about 106,000 language, 13,000 replay and 1,000 conformance cases, and peak bytes drop by a few words where a call no longer builds its argument binding. 53 more `replay` cases change quota outcome with the lower counts.
- Kept block arguments on the operand stack instead of in a per-frame list, which made each frame 32 bytes smaller. Peak bytes drop in every case that runs code; steps are unchanged.
- Bound builtin members to receivers whose static base type is known ([`src/members/direct.rs`](../../src/members/direct.rs)): such a call no longer builds an argument list or prepares a non-hash receiver. Steps drop by one or two instructions per call in about 1,050 cases, and peak bytes drop where the argument list was the largest allocation.
- Shared string and symbol literals within a call ([`Op::Shared`](../../src/bytecode.rs)): each distinct literal is imported once per call and every record key or string built from it shares that copy. Peak bytes drop sharply where literals repeat, such as records built in a loop, and rise by 16 bytes per distinct literal the call evaluates, plus any literal kept until the call returns; steps are unchanged.
- Method calls on instances and namespaces pass their arguments from the operand stack to a callee that binds them directly, instead of copying them into a list. Steps drop by the list's copy in about 280 cases.
- Reserved four frames on a call's first frame rather than eight. Peak bytes drop by 1,248 in almost every case, and rise in about 80 whose frames outgrow four while the old and new capacities are both charged.
- Started every buffer of elements over 128 bytes at four rather than eight: pending argument lists, loop and iteration states, addresses and handlers. Peak bytes drop in about 84,000 cases by a median of 1,440 and rise in none; steps are unchanged.
- Checked a host entry call's arguments without building an argument binding, for a function whose parameters are required positional ones. Steps drop by the binding's and prologue's work in almost every case. With the lower peak bytes of the changes above, 4,993 `replay` cases under Go's tight memory quotas now finish, or fail at another point, and keep their recorded outcomes.
- Copied an instance or class variable's name into a key only when the variable is new, not on every write. Steps drop by the copy's work in about 400 cases.
- Called script functions with a block and plain arguments from the operand stack (`Op::CallBlock`), without an argument list or its instructions. Steps drop by those instructions in about 190 cases.
- Rebased JSON SIMD onto `6bad12f`, retaining upstream counters and observations. Existing JSON key sharing and string-decoding optimizations reduce peak bytes in eight mixed-depth JSON cases and both `json_transform` modes; only those ten conformance counters were re-recorded on macOS arm64. Steps and retained bytes are unchanged. Paired upstream/rebased runs from the same checkout exclude path-dependent drift; all other counters and every observation remain byte-for-byte upstream. See the [counter audit](../../benchmarks/results/json-simd-rebase/counter-changes.json). This pass adds no accounting changes and leaves `records::Fields` for the next task.
- Skipped the scan for host methods and exported functions wherever the checker proves the value plain: indexes, member and function calls, arguments, block parameters, `for` elements, iteration and returns, when their static type holds no `any`, capability or module. Only calls with a capability or required module scan at all, so steps drop in about 500 `conformance` and `compatibility` cases; the `language` and `replay` corpora bind none.
- Shared the keys of the records a host passes in: one import copies each distinct short key of its small hashes once, and the other records reuse it. Peak and retained bytes drop in the few cases whose inputs hold several records; steps are unchanged.
- Skipped the runtime check of an instance variable write whose type the checker proves, deciding at compile time from the class's declarations, as the runtime would find them, instead of scanning the class's methods on every write. Steps drop in about 570 cases that write instance variables.
- Called an instance method directly when the checker proves the receiver is an instance of one class the program declares (`Op::MethodOf`), instead of looking the method up by name. Steps drop by the lookup's comparisons in about 670 cases that call methods.
- Moved each frame's loops, pending argument lists and parameter binding into stacks shared by the call, which made each frame 104 bytes smaller and removed their allocations per frame. Peak bytes drop by 416 in most cases and rise by up to 704 in 46 whose pending argument lists nest more than four deep across frames; steps are unchanged. 1,709 more `replay` cases change quota outcome with the lower peak bytes.
- Kept each frame's stack indexes, function and enclosing frames in 32 bits, which made each frame 72 bytes smaller. Peak bytes drop by 288 or more in every case that runs code; steps are unchanged. 649 more `replay` cases change quota outcome with the lower peak bytes.
- Skipped the result check of instance methods of classes the checker proves assign every property before a method can read it ([`src/typing/construction.rs`](../../src/typing/construction.rs)). Steps drop by the check's work in about 1,500 cases that call methods; peak bytes and observations are unchanged.
- Boxed active iteration drivers instead of reserving four copies of the largest state, and compacted the common driver's range and aggregation fields. The first collection iteration reserves 608 rather than 2,176 bytes; `array_each` peaks at 18,440 rather than 20,008 bytes, and `nested_blocks` at 4,536 rather than 5,624. Steps and retained bytes are unchanged.
- Compiled declared instance variables, accessors and instance parameters to fixed field slots. Slot links preserve first-write order and absent fields through import and traversal; internal environments keep named fields. `method_calls` falls from 19,068 to 15,059 steps and from 5,023 to 4,690 peak bytes.
- Fused literal string indexing of plain hashes, borrowing the compiled key instead of importing and pushing it. `record_fields` falls from 21,149 to 20,381 steps; `glue_orders` falls from 29,338 to 27,802 steps. Together with iteration storage, glue peak bytes fall from 232,416 to 229,848 (with a capability: 237,242 to 234,674); retained bytes stay 4,192. Paired runs of the baseline and new harness from this checkout found no counter increases. Re-recorded only affected counters: 125 conformance, 9,761 language, 22 compatibility and 9,334 replay cases, including newly completed calls; selected clock-dependent cases retain their prior counters. All observations remain unchanged, including the recorded outcomes of 1,664 replay cases that move under their step or memory quotas.
- Reused compact array and numeric iteration drivers in a two-slot arena, charged for its full 320-byte allocation on 64-bit targets until its last active driver finishes or unwinds. Deeper drivers retain separate compact boxes. The first common iteration previously reserved 608 bytes; `array_each` peaks at 18,152 instead of 18,440 bytes, `nested_blocks` at 3,768 instead of 4,536, and `glue_orders` at 229,720 instead of 229,848. Direct binding of plain block arguments also removes temporary operands. Comparison/branch and block-prologue fusion retain each original instruction's charge and checkpoint position: steps and retained bytes are unchanged. Paired runs from the same checkout found no counter increases; the clock-dependent cases keep their prior counters. Re-recorded only affected counters: 83 conformance, 3,775 language, 6 compatibility and 767 replay cases, including eight newly completed calls. Every existing observation remains unchanged, including the recorded outcomes of replay calls affected by lower memory use. Added 30 independently checked loop benchmark observations.
- Rebased JSON record sharing onto `3161370a`, taking upstream's counter files first. Exact record capacities, bounded inline field tables and repeated scalar-string sharing in large indexed documents reduce peak bytes by 78,304 in both modes of `glue_orders` and `glue_orders_cap`; record capacity also reduces replay `call536` and `call538` by 192 bytes. Only these six counters were re-recorded. Every step count, retained byte count and stable observation is unchanged in paired runs from the same checkout. Small documents use ordinary hashes and direct accounting, and field-table metadata adds no tracked heap allocation. See the [counter audit](../../benchmarks/json-records.md#counter-audit-and-verification).
- Extended exact JSON record capacities to documents from 2 KiB while keeping accounting reservations at 8 KiB. Both modes of `json_transform` and `json_transform_cap` now peak 8,192 bytes lower because their four-field rows reserve four pairs instead of eight. Only these four additional conformance counters were re-recorded. Paired runs retain every step count, retained-byte count and stable observation; the bounded field tables add no tracked heap allocation.
- Rebased JSON records and accounting onto `a23d4ece`, retaining upstream counter files before re-recording the same ten intentional peak reductions: 78,304 bytes in both modes of `glue_orders` and `glue_orders_cap`, 8,192 bytes in both modes of `json_transform` and `json_transform_cap`, and 192 bytes in replay `call536` and `call538`. Exact record capacities and shared strings account for these savings. Wide field tables now use bounded stack storage and add no tracked heap allocation; discarded records release their unused cached names. Paired runs from the same checkout preserve all steps, retained bytes and 233,817 stable engine/parser observations, excluding four existing clock-dependent cases. See the [counter audit](../../benchmarks/json-records.md#counter-audit-and-verification). The subsequent rebases onto `7d309479` and `2d42a247` add benchmark tooling, footprint examples, reports and one integration test; upstream library runtime source and counter files are identical, so these ten audited reductions remain unchanged.
- Rebased JSON records onto perf-batch `f363ac01`, preserving the VM round 4 counter files and Counter log before recording only the same ten JSON peak reductions. `glue_orders` now peaks at 151,416 rather than 229,720 bytes, and its capability variant at 156,242 rather than 234,546; both modes retain the 78,304-byte saving. Both transform modes save 8,192 bytes and replay `call536`/`call538` save 192. Steps, retained bytes and all 233,847 stable paired observations are unchanged. The fresh audit and prior evidence live outside Git under `.cache/mgomes/json-records/`; see the [JSON summary](../../benchmarks/json-records.md#counter-audit-and-verification).
- Compiled regex literals once with their script and imported independently charged views of their immutable code. Pattern errors remain deferred until evaluation. Compilation inside an invocation stays at the first evaluation under that call's limits; cached code retains no invocation budget, and quota or interruption failures are not cached. Steps drop only by removed compilation work, and temporary compilation peaks disappear; retained bytes are unchanged. Paired runs from the same checkout found no counter increases. Re-recorded six conformance, 722 language, three compatibility and 199 replay counters, preserving every observation and the recorded replay quota outcomes. `regex_record_fields` falls from 69,947 to 41,915 steps and from 60,214 to 59,898 peak bytes, retaining 2,144 bytes.
- Consumed the previous array result at an unused loop-tail append, retaining other aliases and the exact-capacity growth the old copy allocated. Removed element-copy work lowers `loop_array_build` from 76,297 to 20,686 steps; eliminating the copied header lowers peak bytes from 23,664 to 23,568, while retained bytes remain 10,752. Re-recorded only its two conformance modes and replay `call52200`; all observations are unchanged.
- Rebased VM round 5 onto JSON integration `a4cc97a1`, taking upstream counter files first and re-recording only the same 933 regex/array cases (eight conformance, 722 language, three compatibility and 200 replay). A fresh paired audit of 164,767 engine cases found the same reductions and no increases or newly completed calls. All observation files and the ten upstream JSON peak reductions remain unchanged; four clock-dependent calls and 25 quota-dependent error outcomes keep their recorded observations.
- Rewrote the corpus sources above for the checker differential fixes and recorded only their counters. Steps drop where a parameter's type is simpler to check when the host calls: `call1362` from 224 to 199, `call41650` from 51 to 46, `call45778` and `call45780` from 67 to 65, and `call4886` from 88 to 86. Peak bytes rise by 7 in `call49100` and 23 in `call49148`, whose class now defines `inspect`. Checker work charged to cold `require`s is unchanged in every case. The two cases that moved to the language corpus have new counters, and the three that moved out, and the replay cases that no longer compile, lost theirs. Rebased onto VM round 5 `32ae7e77`, taking upstream's counter files and re-recording only these cases.
