# OTIS JSON writer and helper placement measurement

Completed the physical OTIS M4 comparison of base `7ab5ea12a4b87a438789a2a347b4a0d399a05f4d` and head `a7daf27b8386d2565fce32252a3390d811e64a11`, with the explicit Rust 1.98.1 shims and unchanged release profile. All 256 isolated placements passed the symbol audits and all six full-output/accounting cases. All four sets of three differential instruction estimates and both complete timing seeds finished. Each of the eight sweeps contains a warmup and 15 measured rounds, 480 matched groups, zero rejected groups, matching accounting and peak accepted load at most 3.355.

Lone-escape placement minima, median (range), in microseconds; seeds 187 / 188 respectively:

| Axis | Base | Head |
|---|---|---|
| SIMD writer | 8.170 (7.997–8.395) / 8.186 (7.984–8.405) | 5.154 (5.108–5.209) / 5.157 (5.130–5.208) |
| Portable writer | 10.324 (9.935–10.819) / 10.357 (9.901–10.797) | 5.177 (5.144–5.226) / 5.182 (5.134–5.218) |
| SIMD helper | 8.170 (8.093–8.269) / 8.186 (8.053–8.358) | 5.152 (5.087–5.191) / 5.162 (5.104–5.190) |
| Portable helper | 10.107 (9.967–10.242) / 10.079 (9.914–10.275) | 5.178 (5.130–5.219) / 5.184 (5.148–5.215) |

Across all eight sweeps ASCII-line median minima change by -0.009 to +0.011 µs and Unicode by -0.015 to +0.007 µs; the full per-payload minima, ranges and paired same-binary controls are in the eight `*-summary.txt` files. Paired control medians range from -0.28% to +0.23%. All 32 phase means and paired changes are retained separately for each axis and seed in `*-phases.json`, alongside raw rounds, warmups and rejected-group arrays. The candidate helper minima do not exhibit the large new helper variation seen in b9380440. These isolated sweeps do not prove robustness across arbitrary joint layouts or identify a predictor mechanism; no crossed control was run.

The untrimmed SIMD helper head phase means range from 5.150964 to 5.291120 µs in seed 187 and from 5.157204 to 5.594416 µs in seed 188. The latter maximum is at `local/09`, influenced by an accepted round 11 head observation of 10.677917 µs (base 8.305035 µs, control 8.540972 µs, load 3.117676). The observation remains in the raw data; its cause is unknown.

Writer-axis median differential instructions/call (all 96 estimates per side), base → head: SIMD lines 50,095 → 50,178, Unicode 112,310 → 112,296, lone escapes 254,102 → 135,305; portable lines 65,385 → 65,572, Unicode 112,285 → 112,334, lone escapes 282,599 → 136,335. All raw process counts and the separate helper-axis instruction estimates are archived.

A native sample of the exact SIMD head binary shows lone escapes executing the expanded `put_run` including short-span source lines; its checksum matched after the subsequent full run. In both modes the helper has the same 14 stack references as baseline (prologue/epilogue, buffer initialization and slow path), with no portable mask reload in its escape/copy loops. Prefix and Unicode helpers have only entry/exit stack references in all four builds. Writer frames match base (SIMD 0x120; portable 0x160), eliminating b9380440's additional portable frame allocation and x28 stack slot. Both save x25 in the same slot; head has an additional x25 reload on a new path to `write_text`, and deferred-step/error stack traffic persists. This is not a blanket no-spill assertion. Full assembly and address-bearing extracts are included.

Evidence on Shannon: `/home/mgomes/Fleet/Build/dumont-vibe-187/otis-a7daf27b-evidence.tar.gz`; OTIS: `/Volumes/AI/Build/dumont-vibe-187/share/otis-a7daf27b-evidence.tar.gz`. SHA-256 `a5f4826d07ea20f8563315164e7e4c37eb70406e18b9c6a067c0e0593090c066`, verified on Shannon over host-key-verified SSH. The archive includes environment, validation, all raw counts/timings, phase curves, placement and symbol manifests, four full disassemblies, extracts, sample and logs. All binaries remain in `/Volumes/AI/Build/dumont-vibe-187/measurement-a7daf27b/`; earlier evidence is preserved.

## Complete sweep summaries

Each file below reports the median and range of the 32 placement minima in microseconds; paired percentages are medians of per-group ratios across all measured groups. The archived phase JSON retains all phase curves and raw samples.

### simd-string-187

```text
local: 480 matched groups; time is median of placement minima (range), microseconds
lines_4k
  base            2.096 (2.067..2.135) paired +0.00%
  head            2.095 (2.076..2.148) paired -0.11%
  control         2.099 (2.072..2.128) paired +0.12%
unicode_4k
  base            4.746 (4.703..4.790) paired +0.00%
  head            4.741 (4.704..4.780) paired -0.02%
  control         4.756 (4.698..4.805) paired +0.23%
lone_escapes_4k
  base            8.170 (7.997..8.395) paired +0.00%
  head            5.154 (5.108..5.209) paired -37.85%
  control         8.160 (7.986..8.470) paired +0.02%

0 rejected groups; accounting matched for 3 cases
peak accepted load: 2.8388671875
```

### simd-string-188

```text
local: 480 matched groups; time is median of placement minima (range), microseconds
lines_4k
  base            2.105 (2.075..2.135) paired +0.00%
  head            2.096 (2.065..2.139) paired -0.31%
  control         2.100 (2.073..2.136) paired -0.07%
unicode_4k
  base            4.762 (4.702..4.798) paired +0.00%
  head            4.748 (4.702..4.781) paired -0.24%
  control         4.767 (4.708..4.822) paired +0.09%
lone_escapes_4k
  base            8.186 (7.984..8.405) paired +0.00%
  head            5.157 (5.130..5.208) paired -37.99%
  control         8.185 (7.993..8.388) paired +0.16%

0 rejected groups; accounting matched for 3 cases
peak accepted load: 2.4931640625
```

### portable-string-187

```text
local: 480 matched groups; time is median of placement minima (range), microseconds
lines_4k
  base            2.702 (2.680..2.725) paired +0.00%
  head            2.713 (2.674..2.741) paired +0.35%
  control         2.699 (2.674..2.747) paired +0.01%
unicode_4k
  base            4.749 (4.686..4.799) paired +0.00%
  head            4.748 (4.710..4.785) paired +0.28%
  control         4.743 (4.684..4.799) paired -0.11%
lone_escapes_4k
  base           10.324 (9.935..10.819) paired +0.00%
  head            5.177 (5.144..5.226) paired -51.40%
  control        10.355 (9.978..10.915) paired +0.19%

0 rejected groups; accounting matched for 3 cases
peak accepted load: 2.87744140625
```

### portable-string-188

```text
local: 480 matched groups; time is median of placement minima (range), microseconds
lines_4k
  base            2.711 (2.684..2.742) paired +0.00%
  head            2.714 (2.688..2.749) paired +0.28%
  control         2.709 (2.678..2.753) paired +0.02%
unicode_4k
  base            4.760 (4.694..4.819) paired +0.00%
  head            4.767 (4.728..4.812) paired +0.20%
  control         4.766 (4.698..4.806) paired +0.09%
lone_escapes_4k
  base           10.357 (9.901..10.797) paired +0.00%
  head            5.182 (5.134..5.218) paired -51.43%
  control        10.351 (9.904..10.834) paired -0.28%

0 rejected groups; accounting matched for 3 cases
peak accepted load: 2.97265625
```

### simd-run-187

```text
local: 480 matched groups; time is median of placement minima (range), microseconds
lines_4k
  base            2.096 (2.065..2.136) paired +0.00%
  head            2.104 (2.066..2.115) paired +0.33%
  control         2.098 (2.068..2.134) paired +0.15%
unicode_4k
  base            4.746 (4.712..4.805) paired +0.00%
  head            4.752 (4.681..4.801) paired -0.10%
  control         4.747 (4.711..4.800) paired +0.04%
lone_escapes_4k
  base            8.170 (8.093..8.269) paired +0.00%
  head            5.152 (5.087..5.191) paired -38.16%
  control         8.187 (8.091..8.291) paired +0.01%

0 rejected groups; accounting matched for 3 cases
peak accepted load: 2.75244140625
```

### simd-run-188

```text
local: 480 matched groups; time is median of placement minima (range), microseconds
lines_4k
  base            2.100 (2.083..2.140) paired +0.00%
  head            2.107 (2.072..2.125) paired +0.33%
  control         2.103 (2.074..2.135) paired +0.15%
unicode_4k
  base            4.763 (4.725..4.819) paired +0.00%
  head            4.766 (4.704..4.803) paired +0.02%
  control         4.765 (4.718..4.820) paired +0.18%
lone_escapes_4k
  base            8.186 (8.053..8.358) paired +0.00%
  head            5.162 (5.104..5.190) paired -38.22%
  control         8.200 (8.059..8.344) paired -0.06%

0 rejected groups; accounting matched for 3 cases
peak accepted load: 3.35498046875
```

### portable-run-187

```text
local: 480 matched groups; time is median of placement minima (range), microseconds
lines_4k
  base            2.712 (2.690..2.735) paired +0.00%
  head            2.706 (2.679..2.738) paired -0.28%
  control         2.710 (2.676..2.752) paired -0.08%
unicode_4k
  base            4.763 (4.718..4.804) paired +0.00%
  head            4.754 (4.678..4.794) paired -0.18%
  control         4.766 (4.720..4.804) paired -0.09%
lone_escapes_4k
  base           10.107 (9.967..10.242) paired +0.00%
  head            5.178 (5.130..5.219) paired -49.92%
  control        10.098 (9.950..10.282) paired -0.00%

0 rejected groups; accounting matched for 3 cases
peak accepted load: 2.68505859375
```

### portable-run-188

```text
local: 480 matched groups; time is median of placement minima (range), microseconds
lines_4k
  base            2.716 (2.692..2.746) paired +0.00%
  head            2.715 (2.685..2.752) paired -0.05%
  control         2.721 (2.686..2.751) paired +0.18%
unicode_4k
  base            4.778 (4.716..4.833) paired +0.00%
  head            4.763 (4.681..4.800) paired -0.37%
  control         4.776 (4.730..4.819) paired +0.01%
lone_escapes_4k
  base           10.079 (9.914..10.275) paired +0.00%
  head            5.184 (5.148..5.215) paired -49.93%
  control        10.119 (9.972..10.332) paired +0.09%

0 rejected groups; accounting matched for 3 cases
peak accepted load: 3.35498046875
```
