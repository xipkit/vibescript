# Checked VM helper access

VIBE-162 converts the 131 production unwrap sites in the synchronous VM helpers to checked invariant errors. The 31 additional all-features-only sites in the async ownership bridge are tracked separately as VIBE-179. `unwrap_used` stays allowed for the coordinated rule rollout.

## Measurement

Baseline: `3dd337e072e157453ee211bfcfad8929888550f8`. Apple M4, arm64 macOS 27.0.1, Homebrew rustc 1.98.1 / LLVM 22.1.8. Both revisions use the existing release profile: thin LTO, one codegen unit, overflow checks, no compiler-environment overrides. SIMD and portable binaries use the same fixtures and executable entry path.

The 182 core cases pass their independent fixture expectations. Results, digests, logical steps and tracked peak/retained bytes are identical across both revisions and scanner builds.

Eight rounds rotate all four binaries through every execution position, with pilot-selected common iteration counts targeting 30 ms per case. The median of per-case candidate/baseline timing ratios is **−0.06% SIMD** and **−1.10% portable**. This shared fleet machine also ran allocation/compilation checks, so these are regression measurements, not evidence of an overall speedup.

A sixteen-round confirmation targets 75 ms per case for the twelve largest initial increases in each scanner build, the main dispatch loops and string controls (36 distinct cases). Times below are median microseconds per call, before → after:

| Case | SIMD | Portable |
|---|---:|---:|
| `loop_float/metered` | 64.403 → 60.238 (-6.47%) | 64.411 → 60.851 (-5.53%) |
| `loop_numeric_union/unlimited` | 67.310 → 63.974 (-4.96%) | 68.184 → 63.293 (-7.17%) |
| `loop_range/metered` | 60.009 → 54.992 (-8.36%) | 59.717 → 54.987 (-7.92%) |
| `loop_branches/metered` | 103.247 → 99.845 (-3.30%) | 102.693 → 98.016 (-4.55%) |
| `loop_times/metered` | 167.322 → 158.828 (-5.08%) | 162.660 → 161.755 (-0.56%) |
| `block_yield/metered` | 197.523 → 188.989 (-4.32%) | 195.087 → 191.430 (-1.87%) |
| `array_map_select/unlimited` | 256.273 → 250.048 (-2.43%) | 251.773 → 259.062 (+2.89%) |
| `upcase_65536/metered` | 8.447 → 9.030 (+6.90%) | 7.496 → 7.316 (-2.40%) |

The initial portable `array_map_select/unlimited` increase falls from 12.42% to 2.89% in the confirmation. The SIMD upcase control remains +6.90%; its source is untouched, and `text::case::call` (1,275 instructions, including its inlined walks) and `Transform::ascii` (114 instructions) have identical normalized instructions after resolving renamed constants to their contents. Placement and shared-host effects have not been isolated; the measured upcase increase is retained here.

## `simple::run` codegen

Assembly comes from `cargo rustc --release --lib -- --emit=asm` in each revision. Counts cover the entire function, including cold failure paths, and are not dynamic instruction counts.

| Static property | Before | After |
|---|---:|---:|
| Instructions | 3,335 | 3,455 |
| Stack frame, including saved registers | 880 bytes | 912 bytes |
| Calls | 147 | 132 |
| Calls to `Option::unwrap_failed` | 19 | 0 |
| Calls to cold `Error::from(Violation)` | 10 | 13 |
| Load instructions (`ldr`, `ldp`, `ldur`) | 719 | 678 |
| Store instructions (`str`, `stp`, `stur`) | 362 | 430 |

The same registers are saved in both prologues; the local stack allocation grows from 768 to 800 bytes. Register allocation and block layout change, so the additional cold error handling is not claimed to be codegen-neutral. The before/after timing above measures its effect.

Raw samples, binary hashes, assembly, the normalized per-function diff and scripts are retained under `/Volumes/AI/Build/beck-vibe-162/`: `paired/`, `confirmation/`, `simple-codegen.diff`, `codegen-summary.json` and `upcase-resolved-codegen.json`.
