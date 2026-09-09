# Native SIMD benchmark profiles

The SIMD workflow always measures the eight existing ASCII and Unicode string
loop controls. On pull requests it also selects a profile when its fixture is
present in the head and its JSON definition, fixture or input bytes, or watched
production files differ from the base. File additions and deletions count; `_test.go` files
are excluded from production globs. Removed profiles use their base definition
when their fixture remains in the head. Renamed profiles run once using the head
definition. Push and manual runs use only the controls.
Each fixture has one profile owner. When a profile moves its fixture or a declared
input and the old file is absent from the head, its superseded base copy is removed
unless another head profile still declares it.

Each JSON file names one reviewed standalone benchmark fixture, its top-level
benchmark expression, expected case count, and relevant production paths.
Selected fixtures are copied to the base before building, so both revisions run
the same benchmarks. Fixtures must use APIs and test helpers available before
the optimization. Keep unrelated tests and new implementation helpers out of
these fixtures.

A fixture lists its reviewed shared Go test helpers and workload files in
`inputs`. Those inputs also participate in selection and are copied with the
fixture, so base and head benchmark identical programs. The lexer profile adds
three source files from `tests/complex`. The controls always share their three
Go test files, even when no profile is selected. Selection compares all original
bytes before copying. Inputs cannot replace production Go files.

The artifact's `profiles.json` records selection, fixture hashes, and input hashes. Result
validation requires the expected unique cases in each profile, six samples per
case, and identical case names across all build variants. Define new profiles
here without changing the workflow; updates are checked on their own PR once
the fixture is present.

Compare `base-nosimd.txt` with `head-nosimd.txt`, and `base-simd.txt` with
`head-simd.txt`, to measure the change under identical compiler settings.
Linux also produces `base-simd-avx2-disabled.txt` and
`head-simd-avx2-disabled.txt` for a direct fallback comparison with the same
SIMD experiment and CPU settings. Comparing different experiment or CPU settings
also includes their effects on Go's runtime and standard library.
