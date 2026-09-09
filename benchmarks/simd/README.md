# Native SIMD benchmark profiles

The SIMD workflow always measures the eight existing ASCII and Unicode string
loop controls. On pull requests it also selects a profile when its fixture is
present in the head and its JSON definition, fixture bytes, or watched production
files differ from the base. File additions and deletions count; `_test.go` files
are excluded from production globs. Push and manual runs use only the controls.

Each JSON file names one reviewed standalone benchmark fixture, its top-level
benchmark expression, expected case count, and relevant production paths.
Selected fixtures are copied to the base before building, so both revisions run
the same benchmarks. Fixtures must use APIs and test helpers available before
the optimization. Keep unrelated tests and new implementation helpers out of
these fixtures.

The artifact's `profiles.json` records the selection and fixture hashes. Result
validation requires the expected unique cases in each profile, six samples per
case, and identical case names across all build variants. Define new profiles
here without changing the workflow; updates are checked on their own PR once
the fixture is present.
