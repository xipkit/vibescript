# Faster builds, tests and gates

The root integration suites now share one binary, with `footprint` kept separate for its process-wide allocation snapshots. Existing files stay in place. A registration test rejects any unlisted root test file; explicit module declarations preserve rustfmt's traversal. Development builds use line tables, and golden builds resolve workspace features consistently so the harness and CLI share the interpreter library. The CLI retains its original `serde_json` features because the two builds remain separate.

No interpreter source, release-profile setting, observation file or counter file changed. There are no Counter log entries.

## Measurement conditions

Measurements ran on shannon, an Intel Core Ultra 9 285H Linux host, with Rust 1.98.1, six Cargo jobs, affinity to CPUs 4–15 and `nice -n 10`. Queued diagnostic jobs inherited a second niceness increment; the final manual gate was launched directly with the requested prefix. Other agents' benchmark workers used the reserved CPUs, including CPU 2. Actual `~/gate.sh` executions were excluded; similarly named benchmark wrappers were inspected rather than mistaken for shared-directory gates. An interrupted baseline was discarded and its affected manual steps repeated.

The baseline is `f363ac01`. The final code is `2da6897f`, rebased onto `6a7c1a14`; that intervening upstream commit only adds authoring-study prose. Diagnostic after measurements used the same Rust test layout and an equivalent `CARGO_PROFILE_DEV_DEBUG=line-tables-only` override before the setting was committed.

Every measured build used `./scripts/cargo` in offline mode; `CARGO_NET_OFFLINE=true` also covered the baseline Python builder. Bash `time` supplied wall/user/system times and Cargo `--timings` supplied compiler-unit timelines, without unstable flags. A cold build means an empty target directory, with the registry and OS file cache already warm. Incremental measurements touch `src/lib.rs` without changing its contents; actual code edits can invalidate more work. The loop and manual-gate sequences use separate target directories.

## Build and execution breakdown

All entries are wall seconds on shannon. Test builds select the workspace, all targets and all features. Binary execution runs the Cargo-reported test executables directly, sequentially, with each binary's normal test-thread concurrency.

| Measurement | Before | After | Change |
| --- | ---: | ---: | ---: |
| Cold workspace debug build | 13.44 | 13.83 | +2.8% |
| No-op debug build | 0.037 | 0.038 | effectively unchanged |
| Debug build after touching `src/lib.rs` | 1.80 | 1.74 | -3.4% |
| Debug `test --no-run` | 39.47 | 22.43 | -43.2% |
| Debug test rebuild after touching the library | 13.37 | 4.02 | -69.9% |
| Execute debug test binaries | 77.55 | 58.86 | -24.1% |
| Gate-profile `test --no-run` | 75.06 | 60.17 | -19.8% |
| Execute gate-profile test binaries | 11.98 | 9.30 | -22.4% |
| Golden execution without builds | 25.56 | 25.18 | -1.5% |

A separate control kept the consolidated layout and changed only debug information: debug `test --no-run` took **25.98 s with full debug information versus 22.43 s with line tables**, a 13.7% reduction. Cold ordinary builds were essentially identical: 13.88 versus 13.83 s. Set `CARGO_PROFILE_DEV_DEBUG=2` when inspecting local variables in a debugger.

The identical loop sequence occupied **16.98 GB before and 3.52 GB after**, a 79.2% reduction, before additional linker/golden experiments.

Cargo recorded 113 integration compiler units before and two after. Their aggregate, overlapping compiler durations fell from 111.55 to 8.41 s in debug and from 182.63 to 20.00 s in the gate profile. Test execution also benefits from scheduling suites together instead of waiting for each binary to finish.

## Manual gate

The commands from `~/gate.sh` were run by hand in `~/work/build-speed`, using private target directories and `CARGO_INCREMENTAL=0`. The optimized all-features step from `scripts/check` was also included before the portable step. Thus both runs use the same sequence and cache conditions; no shared `~/gates` directory was used. `test-release` names below denote `--profile gate`, not a changed release profile.

| Step | Before | After | Change |
| --- | ---: | ---: | ---: |
| fmt | 1.03 s | 1.09 s | +6.4% |
| clippy-all | 10.81 s | 12.15 s | +12.4% |
| clippy-portable | 8.57 s | 9.08 s | +6.0% |
| test-debug | 113.50 s | 77.77 s | -31.5% |
| test-release | 92.36 s | 64.46 s | -30.2% |
| test-release-portable | 82.90 s | 61.09 s | -26.3% |
| doc | 1.75 s | 1.60 s | -8.4% |
| golden | 70.51 s | 48.25 s | -31.6% |
| **Sum of steps** | **381.42 s** | **275.51 s** | **-27.8%** |

Every final manual-gate step passed: 1,999 debug tests, 1,999 optimized all-feature tests, 1,968 optimized portable tests, 57 doctests and all golden corpora. Clippy is slightly slower with one larger integration crate. The savings in test builds and execution exceed that cost. The final table includes the revised golden build, unlike the earlier diagnostic gate run.

The original golden commands spent 21.31 and 23.78 s building two library variants. Resolving both invocations with `--workspace` makes the second reuse the library. A separate experiment completed the full golden check in 59.55 s versus 70.51 s, with the same observations. In the final gate sequence, the builds took 18.80 and 4.34 s and the complete check took 48.25 s. Cargo artifact records confirmed that the CLI still uses `serde_json` with `default,raw_value,std`, without the harness's `arbitrary_precision` feature.

## Linking and remaining costs

A timed compiler-driver wrapper measured the linker alone:

| Representative integration binary | Debug link | Gate-profile link |
| --- | ---: | ---: |
| Before: `core`, one of 113 binaries | 0.166 s | 0.087 s |
| After: consolidated `all` | 0.235 s | 0.146 s |

These are individual links, not an extrapolated total. Consolidation removes 111 repeated integration links and compiler invocations. On this Rust toolchain the default driver already passes `-fuse-ld=lld`; explicitly requesting it again measured 0.166/0.067 s before and 0.227/0.139 s after. No linker override was added, so Macs retain Apple's linker and WASI retains its normal linker.

The remaining gate compilation is dominated by the interpreter library and its separate unit-test build: 28.07 and 37.45 s in the diagnostic after run. The normal library's baseline frontend accounted for 8.06 s of its 26.46 s. No evidence justified changing shared runtime code or dependency optimization settings for this task.

A larger follow-up could split the compiler/runtime into crates with independent frontend work. Perfectly halving an eight-second frontend would save at most about four seconds on that path; a practical estimate is **2–4 s, roughly 3–7% of the current cold gate test build**, before any separate LLVM benefit. Dependencies and extra cross-crate work could reduce that gain. This remains a proposal, not an implementation or measured promise.

## Coverage, behavior and validation

Normalized executable test inventories contain **no removed tests**. The only addition is `every_root_test_file_is_registered`.

| Scope | Before | After |
| --- | ---: | ---: |
| Root integration tests, both native architectures | 1,154 | 1,155 |
| Linux workspace, all targets/features | 1,998 | 1,999 |
| macOS workspace, all targets/features | 1,999 existing tests | 2,000 |

The Mac existing-test count is the passing post-change inventory minus the one added check; no Mac baseline timing was run. The differing workspace totals reflect platform-specific tests. The guard was also tested with a temporary unregistered `.rs` file: it failed as intended, then passed after that file was removed.

The coupling audit found the global allocator only in `footprint`. Root suites do not mutate the environment, working directory or signal handlers. Fixture counters stay module-local and use distinct directory prefixes. Suite helper modules remain where they were; the harness narrowly allows Clippy's duplicate-module lint for those intentional inclusions.

Local arm64 verification passed formatting, both Clippy modes, 2,022 default workspace tests including doctests, 2,000 all-target/all-feature tests, every golden corpus, portable/SIMD validation on 107,562 shared cases with identical counters, and `check-wasi` with 1,870 tests plus its CLI/filesystem witnesses. A scratch-path Unix-socket limit was resolved by shortening the private cache directory, without changing a test or interpreter code.

A paired old/new harness audit from the same shannon checkout compared **233,847 stable engine cases**, including 127,395 successful calls. It found **zero differences in phase, steps, peak tracked bytes or retained tracked bytes**. Four cases already declared varying were excluded. Both harnesses also passed the golden observations. Existing platform/path/quota drift relative to the checked-in historical counters remains unchanged.

A control build using the complete baseline `Cargo.toml` reused every release compiler artifact and preserved the SIMD comparison executable's SHA-256: `63913a0a55eb9986b0f098fdf74aed1a3349fae016409ee417ee84b1ebfe80c9`. Runtime benchmark rounds were not repeated: this patch changes build/test organization and the golden builder, while the release sources, settings and executable remain unchanged.

## Mac measurements and coordinator gate

Build-time measurements were deliberately not run on the loaded local Mac. The coordinator owns the three-host gate and Mac before/after measurements. The task-specific prohibition on modifying `~/gates`, `~/gate.sh` and the remote main checkouts takes precedence here over the common gate-all instruction; the manual shannon gate above is the completed gate for this branch. **The three-host gate remains pending with the coordinator.**

These are the coordinator-provided M4 baseline figures, not fresh measurements or a cross-host comparison:

| M4 gate step | Supplied before | After |
| --- | ---: | --- |
| Debug tests | 127 s | coordinator pending |
| Optimized tests | 104 s | coordinator pending |
| Optimized portable tests | 93 s | coordinator pending |
| Golden | 78 s | coordinator pending |
| Clippy all features | 9 s | coordinator pending |
| Clippy portable | 6 s | coordinator pending |

## Evidence

Raw logs, timing HTML, inventories, binaries and scripts remain under `shannon:~/work/build-speed-data/`: `before/`, `after/`, `layout-full-debug/`, `final/` and `counter-audit/`. The discarded run's artifacts are in `before/interrupted-gate-target/`. `measure.py` reproduces the loop and gate with a fresh label; `measure-debug.py` isolates debug information and `measure-links.py` records linker calls.

Local verification logs, the inventory comparison, release-reuse proof and copied timing reports are under `/Volumes/AI/Work/xipkit/vibescript.rs/.cache/build-speed/`. Only this summary is committed under `benchmarks/`.
