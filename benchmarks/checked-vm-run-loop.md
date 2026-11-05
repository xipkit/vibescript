# Checked VM run-loop invariants

VIBE-161 clears all 152 active production unwrap findings from `src/vm.rs`. Invalid stack, frame, binding and literal state returns terminal internal errors through the existing run/block latches. Diagnostic lookup keeps absence optional. `take_loop_alias` returns `Access<bool>` and both dispatchers propagate it. The coordinated `unwrap_used` allowance remains unchanged.

## Method and limits

Baseline: `ef46c2f781b08490410a72fa4e4d78198f4cf8d3`. Runtime candidate: `96164036ea5a7e3679ebc587d596959388562cba`. Apple M4, arm64 macOS 27.0.1, Homebrew rustc 1.98.1 / LLVM 22.1.8. Independent baseline/candidate target directories use the existing release profile, thin LTO, one codegen unit and overflow checks, without compiler overrides. Both default SIMD and no-default-features binaries use the same fixtures and executable entry path.

All 198 core benchmark cases match their fixture expectations in all four binaries. Results, digests, steps, tracked peak and retained bytes agree exactly.

The initial eight-round sweep targets 50 ms per case and rotates the four binaries through each execution position. Its median per-case changes are +4.29% SIMD and +1.88% portable. **This sweep is heavily disturbed:** 315 of 396 SIMD binary/case sample sets and 377 of 396 portable sets span more than 2× their fastest sample. The shared fleet host also ran compiler and allocation stress work. These numbers do not establish an overall slowdown or speedup.

A twelve-round confirmation alternates binaries for each individual case, reverses case order each round, and targets 150 ms per case for 32 dispatch, call, iteration, allocation and string-control workloads. This card's two allocation-stress processes were paused during that confirmation and resumed in a `finally` block. Other fleet load remained. Median **within-round** changes across the 32 cases are −0.53% SIMD and +0.97% portable. Five SIMD and seven portable cases still exceed +3%; possible costs are retained below. Even the confirmation contains substantial scheduling variation, so this is not evidence of zero per-workload regression.

Times below are median microseconds per call. Percentages are medians of the twelve paired candidate/baseline ratios, which can differ from the ratio of the displayed time medians.

| Case | SIMD µs before → after (paired change) | Portable µs before → after (paired change) |
|---|---:|---:|
| `numeric_loop/metered` | 64.063 → 70.702 (+1.40%) | 64.586 → 72.454 (+1.03%) |
| `numeric_loop/unlimited` | 60.789 → 67.111 (-1.09%) | 61.637 → 68.511 (+1.64%) |
| `function_calls/metered` | 96.195 → 97.645 (-0.37%) | 105.730 → 111.961 (+1.39%) |
| `function_calls/unlimited` | 93.023 → 96.358 (-3.23%) | 91.304 → 82.567 (-1.59%) |
| `upcase_65536/metered` | 10.468 → 10.378 (+2.67%) | 8.669 → 9.940 (-1.15%) |
| `upcase_65536/unlimited` | 11.230 → 11.200 (-1.38%) | 10.079 → 10.032 (-0.27%) |
| `loop_float/metered` | 86.440 → 81.996 (-4.22%) | 84.170 → 91.418 (+3.22%) |
| `loop_float/unlimited` | 77.173 → 80.445 (+0.21%) | 80.066 → 78.322 (-2.37%) |
| `loop_numeric_union/metered` | 93.445 → 80.184 (-1.21%) | 81.138 → 83.696 (+4.11%) |
| `loop_numeric_union/unlimited` | 88.191 → 85.602 (+0.49%) | 79.745 → 83.797 (+0.92%) |
| `loop_range/metered` | 78.863 → 83.098 (+5.62%) | 80.363 → 83.202 (-2.43%) |
| `loop_range/unlimited` | 85.052 → 76.792 (-1.36%) | 81.190 → 77.473 (-3.17%) |
| `loop_branches/metered` | 142.983 → 143.298 (-2.08%) | 150.178 → 141.105 (-0.17%) |
| `loop_branches/unlimited` | 130.290 → 135.733 (-0.14%) | 142.174 → 143.167 (+3.97%) |
| `array_map_select/metered` | 350.715 → 353.980 (+0.31%) | 353.583 → 371.405 (-0.99%) |
| `array_map_select/unlimited` | 346.985 → 315.618 (-2.69%) | 362.326 → 349.058 (+1.77%) |
| `loop_times/metered` | 241.433 → 213.618 (-4.73%) | 211.364 → 207.919 (+0.01%) |
| `loop_times/unlimited` | 254.734 → 227.077 (-7.46%) | 215.202 → 217.017 (+5.29%) |
| `loop_array_build/metered` | 205.554 → 195.716 (-0.53%) | 196.123 → 230.339 (+7.06%) |
| `loop_array_build/unlimited` | 224.153 → 243.919 (-1.92%) | 230.273 → 210.345 (+1.30%) |
| `block_yield/metered` | 267.396 → 251.711 (+0.64%) | 267.274 → 251.224 (-0.67%) |
| `block_yield/unlimited` | 288.251 → 269.834 (-1.19%) | 247.027 → 263.561 (+4.83%) |
| `method_calls/metered` | 185.067 → 178.936 (-2.39%) | 195.034 → 194.682 (-3.17%) |
| `method_calls/unlimited` | 168.987 → 180.047 (+3.16%) | 180.728 → 158.935 (-9.31%) |
| `member_calls/metered` | 97.596 → 105.054 (+0.11%) | 105.717 → 98.901 (-0.07%) |
| `member_calls/unlimited` | 104.936 → 110.412 (-1.55%) | 93.742 → 97.672 (+4.31%) |
| `string_build/metered` | 249.343 → 299.382 (+7.43%) | 272.625 → 282.044 (+2.37%) |
| `string_build/unlimited` | 313.249 → 291.371 (-3.19%) | 290.435 → 299.533 (+2.99%) |
| `record_build/metered` | 647.351 → 690.762 (+5.76%) | 670.762 → 673.560 (+1.12%) |
| `record_build/unlimited` | 627.312 → 646.380 (+5.16%) | 670.653 → 644.374 (+1.79%) |
| `glue_orders/metered` | 423.082 → 429.588 (+2.76%) | 452.827 → 400.265 (-4.11%) |
| `glue_orders/unlimited` | 414.956 → 412.115 (-0.53%) | 388.663 → 419.421 (-2.71%) |

The largest remaining paired increases are SIMD `string_build/metered` (+7.43%) and portable `loop_array_build/metered` (+7.06%). Neither is dismissed as noise or hidden behind the aggregate. SIMD range traversal is +5.62%; its `LoopState::next_value` instruction diff begins in failure handling, with the preceding 192 instructions unchanged after symbol normalization. General dispatch register allocation and native code placement still change, so the observed costs cannot be inferred solely from branch counts.

## Per-function codegen comparison

Assembly is emitted by `cargo rustc --release --lib -- --emit=asm`, repeated with `--no-default-features`. The table covers changed emitted functions, their inlined owners, and the optimized-dispatch control. Counts include cold failure/unwind paths and are not dynamic instruction counts. Stack bytes include saved registers. All numeric metrics below are identical between the two scanner configurations.

| Function | Instructions before → after | Stack bytes before → after | Calls before → after | `unwrap_failed` calls before → after |
|---|---:|---:|---:|---:|
| `<LoopState>::next_value` | 251 → 263 | 224 → 224 | 22 → 20 | 2 → 0 |
| `<Run>::advance` | 23,938 → 24,825 | 10,768 → 10,816 | 1,518 → 1,446 | 121 → 0 |
| `<Run>::run` | 99 → 99 | 320 → 320 | 10 → 10 | 0 → 0 |
| `<Run>::until` | 4,567 → 4,581 | 1,952 → 1,936 | 286 → 286 | 2 → 0 |
| `diagnostic_site` | 109 → 109 | 0 → 0 | 0 → 0 | 0 → 0 |
| `address_guard` | 198 → 204 | 304 → 304 | 17 → 17 | 2 → 0 |
| `resolve_type` | 1,385 → 1,351 | 736 → 768 | 85 → 82 | 5 → 0 |
| `implicit_read` | 561 → 566 | 672 → 672 | 38 → 38 | 1 → 0 |
| `global_value` | 361 → 371 | 240 → 240 | 11 → 9 | 2 → 0 |
| `receive_function` | 94 → 101 | 208 → 208 | 4 → 3 | 1 → 0 |
| `enter_checked::<usize>` | 288 → 296 | 400 → 448 | 17 → 17 | 1 → 0 |
| `check_entry` | 247 → 252 | 336 → 320 | 12 → 10 | 2 → 0 |
| `call_block` | 83 → 93 | 160 → 160 | 8 → 8 | 1 → 0 |
| `enter_values::<vibescript::namespace::Call>` | 215 → 223 | 672 → 672 | 14 → 14 | 1 → 0 |
| `resolve_slot` | 323 → 338 | 288 → 288 | 10 → 7 | 3 → 0 |
| `take_loop_alias` | 30 → 41 | 32 → 32 | 3 → 2 | 1 → 0 |
| `simple::append_loop_tail` | 804 → 836 | 656 → 656 | 84 → 84 | 1 → 0 |
| `simple::run` | 3,455 → 3,455 | 912 → 912 | 132 → 132 | 0 → 0 |

`Run::drive`, `resume` and `normalize_return` have no independent emitted symbol in these builds; their code is included in `Run::until`. Other generic entry instantiations are inlined into callers. `simple::append_loop_tail` is included because it calls the newly fallible alias helper.

`Run::advance` keeps its saved-register set, adds 48 stack bytes and 887 instructions, and eliminates 121 unwrap-failure calls. Its static loads rise from 5,673 to 6,013 and stores from 4,057 to 4,560. `resolve_type` adds 32 stack bytes and `enter_checked::<usize>` adds 48. `simple::run` retains its counts and stack size; this is not a claim of identical assembly. The full instruction diff includes changed cold-block placement and references to renamed constants.

## Correctness evidence

- Production Adamantium audits with `unwrap_used` temporarily removed find zero findings in `src/vm.rs` with all features and without default features, and no findings from other rules. The allowance is restored, with no new exceptions. Normal Adamantium and workspace/all-target Clippy pass in both configurations.
- Full workspace/all-target runs pass: 2,492 debug all-features tests, 2,491 optimized all-features tests, and 2,451 optimized no-default-features tests. All 61 doctests pass.
- Fault regressions cover 17 malformed operations through real host blocks, callbacks ignoring/replacing the error, missing entry/result/pooled-driver state, malformed integer constants and open ranges in an active loop. The three new tests and expanded callback test fail on the original runtime with four unwrap panics; the candidate passes. Rescue and ensure cannot execute after the fault, and later host work sees the same latched error.
- All eight golden corpora pass. A fresh baseline comparison covers 233,898 raw engine observations with no deterministic result, diagnostic, quota-outcome or counter differences. Four clock-dependent replay cases vary; one timestamp loses a trailing fractional digit and changes peak/retained accounting by one byte. Recorded-fixture drift also occurs on the baseline.
- The actual optimized CLI returns the expected results for collection iteration, big integers, formatting, regex matching, required-file aliases/shared state, classes, loop control and rescue/ensure. An uncaught raise exits 1 with its source location.

Raw binary hashes, timing samples, assembly, per-function instruction diffs, audit reports, logs and reproduction scripts remain in `/Volumes/AI/Build/tron-vibe-161/` during review: `paired/`, `confirmation/`, `codegen-{simd,portable}.{json,diff}`, `observations-diff.json` and `baseline-regressions.log`.
