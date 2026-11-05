# Linker comparison

Mold improved warm development rebuilds by 3-15% on these workloads. The optimized gate profile saved only 19-53 ms in linking, with little effect on total rebuild time. Keep the current CI defaults; mold is an optional development convenience.

Measured on November 5, 2026, against Vibescript `043955369fab4eff50ec9c02be706ef25c28fe13` (0.90.0), using Rust 1.98.1 and mold `331ac663668dc0b1f4949403854613b9b1913025`. This mold commit includes the newer macOS implementation; it is later than the released 3.0.0 tag.

## Results

Medians of five measured rebuilds per linker, following warmup. Link time includes the Clang driver. Full rebuild time includes Cargo, rustc, linking and the common measurement wrapper.

| Platform | Profile | Target | Default link -> mold | Link speedup | Full rebuild default -> mold |
|---|---|---|---:|---:|---:|
| macOS ARM64 | dev | cli | 186 -> 134 ms | 1.39x | 0.691 -> 0.616 s |
| macOS ARM64 | dev | integration | 361 -> 163 ms | 2.22x | 3.140 -> 2.848 s |
| macOS ARM64 | gate | cli | 77 -> 52 ms | 1.47x | 3.637 -> 3.676 s |
| macOS ARM64 | gate | integration | 131 -> 111 ms | 1.19x | 38.241 -> 38.300 s |
| Linux x86-64 | dev | cli | 233 -> 146 ms | 1.59x | 0.583 -> 0.493 s |
| Linux x86-64 | dev | integration | 343 -> 199 ms | 1.72x | 3.653 -> 3.545 s |
| Linux x86-64 | gate | cli | 82 -> 63 ms | 1.30x | 4.303 -> 4.276 s |
| Linux x86-64 | gate | integration | 159 -> 106 ms | 1.50x | 46.030 -> 45.565 s |

Development CLI rebuilds saved about 75 ms on macOS and 89 ms on Linux. Development integration rebuilds saved about 292 ms and 108 ms respectively. These are favorable cases for a faster linker: dependencies are cached and only the root target is rebuilt. Editing the interpreter library would also incur its compilation cost.

Gate integration linking took about 0.1 s inside a 38-46 s rebuild. The observed whole-build differences are larger than the link-time savings and overlap the sample ranges; they should not be attributed entirely to mold. Release-profile builds and complete CI wall time were not benchmarked.

## Environment and method

- macOS 27.2, Apple M1 Max, 10 logical CPUs, 32 GiB RAM; Apple Clang 21 and `ld-27106.1` versus mold Mach-O. The local Rust distribution is Homebrew.
- Native GitHub Linux runner, Ubuntu 26.04.1, AMD EPYC 7763 with four visible logical CPUs; Clang 21.1.8 and Rust's bundled LLD 22.1.8 versus mold ELF. Compare linkers within each machine; the hosts differ.
- Both machines used three Cargo jobs and all features. The existing development and gate profiles were unchanged. Gate has no LTO, 16 codegen units and no debug information.
- The CLI target is `vibes`; the integration target is the combined `all` test binary. Touching `cli/src/main.rs` or `tests/all.rs` changes only its modification time. Dependencies remain cached, and the harness asserts that each measured build invokes exactly one linker.
- Linker choice is read by a common driver wrapper at execution time. Neither Rust flags nor the configured wrapper path change between variants, preventing dependency rebuilds from contaminating the comparison. Each pair alternates linker order.
- One warmup per linker and workload is excluded. CLI evaluation and integration tests run outside the timers. Source contents and the Cargo lock file match between platforms; the two recorded harness commits differ only in the workflow build-output path.

## Correctness

All 80 measured rebuilds and 16 warmups linked successfully. Every CLI binary evaluated `6 * 7` as `42`. On macOS, all 1,279 integration tests passed with both linkers in both profiles.

The original Linux workflow omitted the normal CI setting `TZ=America/Detroit`. In all four Linux integration validations, 1,278 tests passed and the same site-example test failed: its maintenance-window fixture expected Detroit local time but the runner used UTC. Both linkers produced the identical five-hour discrepancy. The benchmark harness now supplies the CI timezone.

The targeted follow-up passed the previously failing site test with both linkers in both profiles under `TZ=America/Detroit`. The timing measurements above are preserved from the original run; no Rust source or profile change was needed.

## Evidence

- [Harness](../scripts/benchmark-linkers.py) and [Linux benchmark workflow](../.github/workflows/benchmark-linkers.yml).
- [macOS measurements](results/linkers-2026-11-05/macos.json) and [Linux measurements](results/linkers-2026-11-05/linux.json), including all samples, ranges, commands, linker events and initial validation outcomes. macOS CPU and mold-version metadata were verified separately after the original probes were unavailable or used the ELF version flag.
- [Linux timing run](https://github.com/xipkit/vibescript/actions/runs/42117069274), including logs for every build and test validation. Its failed conclusion reflects the timezone fixture, not a compile or link failure.
- [Successful targeted timezone validation run](https://github.com/xipkit/vibescript/actions/runs/42121512964) and [its four saved test results](results/linkers-2026-11-05/linux-timezone-validation.json).
