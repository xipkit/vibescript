# SIMD accounting analysis

Inspected current master e6f1fca9 in /tmp/vibescript-simd-assessment-20260908. Read-only analysis; no tests or implementation changes. Go performance and defensive skills applied. Toolchain/SIMD package availability is handled separately by the root agent.

## Conclusion

SIMD does not inherently break accounting. The safest useful boundary is an allocation-free, read-only byte classification/search kernel returning an index, mask, or count, called by the current interpreter wrapper. Keep current logical work charges, pre-allocation checks, immutable output ownership, and scalar fallbacks. CPU instruction count is not the quota unit. Vector registers add no script heap; an input-sized unpacked numeric array, index table, staging buffer, or pooled buffer does.

## Existing accounting contract

- internal/runtime/errors.go:292-322: step increments logical steps, latches quota exhaustion, and polls reachable memory/context every 16 steps (mask at 106). Step failures remain sticky through latchExhaustion at 280-289.
- errors.go:337-363: stepN bulk-charges the same logical count and catches crossing a slow-path boundary even when the final count does not land on it. It executes the slow checks once, so it is not a substitute for checkpoints during an arbitrarily long kernel.
- errors.go:366-390: checkStepBudgetFor rejects known-size work before doing it, while checking cancellation. Useful before pure bulk work; do not use to reorder observable partial mutation or callbacks.
- members_string.go:2844-2874: byte scans use floor(n/64) steps. members_string.go:2897-2907 carries charset-probe residue. Chunking must preserve the cumulative count; rounding every short chunk independently can make work uncharged, while stepN(0) itself charges one step.
- String#count differs: members_string.go:2910-2944 charges one step per rune plus actual charset probes. A SIMD ASCII fast path must preserve those charges, or explicitly change public quota semantics separately. All-ASCII detection and tail handling cannot erase malformed UTF-8 behavior.
- memory.go:2807-2833: Go-local scratch is invisible to the graph walker unless reserveLoopScratch adds it to the live baseline for its whole lifetime. Reserve first, check roots plus scratch before allocation, and defer release using the exact returned delta. Reservation alone does not enforce quota.
- memory.go:1348-1393: accumulator-metered sections may omit periodic graph walks only if no script/capability/block reentry or mutation of reachable containers occurs and every allocation is precharged. Cancellation and step limits remain. This is a good fit for pure native kernels but not arbitrary array map/reduce callbacks.
- json_memory.go:5-20: JSON parsing already has a stable call-root baseline plus partial-container/output accumulator. A new SIMD scratch allocation must be added to this peak, not measured after materialization.
- json.go:369-425: unescaped tokens reserve then strings.Clone; escaped strings preflight builder capacity plus final owned clone. Returning a substring after fast token discovery would reintroduce source-document retention, even though the estimator sees only token length.
- json.go:993-1035: stringify charges emitted bytes, not input bytes, because control-character escaping expands up to six bytes. SIMD escape detection can find safe spans but must retain output-growth checks and charges.

## Mutation and representation constraints

- vibes/value/value.go:70-75 defines Value as kind + any + uint64; int/float scalar constructors are inline (value_constructors.go:21-25). On 64-bit Go this is a 32-byte heterogeneous tagged element, not a packed float64/int64 vector. Reinterpreting []Value as []float64 is invalid; unboxing all values adds allocation, traversal, and peak live memory. Favor byte strings or bounded stack tiles over whole-array conversion.
- vibes/value/epoch.go:106-127 records a collection write before backing changes; arrays use old backing identity. Raw writes to an Array() slice bypass this unless the caller explicitly follows mutation/copy-on-write contracts. SetArrayElems invokes the epoch hook (value_constructors.go:114-129).
- value_constructors.go:77-104 says NewArray owns its backing and publishes contained collections; AdoptArray is only for already-published elements. Unsafe vector stores cannot bypass Go pointer write barriers or overwrite kind/data lanes. Read-only byte kernels avoid all of this.
- collection_values.go:38-58 documents the always-copy oracle. SIMD mutators would need the existing exclusive-ownership path and publication bookkeeping in addition to epoch invalidation.
- members_array.go:2545-2611 shows sum is a sequential fold with per-element steps, big-integer operation guards, and old_total + contribution + next all charged together. Tree reduction changes float association and may remove intermediate big-int promotions/allocations or move errors. This is not a safe first SIMD target. Block forms have observable callbacks and must remain ordered.

## Concrete safe kernel contract

1. Input is a bounded read-only byte span; bounds-safe vector loads and scalar tail, no reading past len, no retained pointer or shared mutable state.
2. Return only scalar metadata (first special-byte index/count/mask). Keep UTF-8, escape, malformed-input handling in scalar code reached at that index. Example: JSON parse ASCII run ends at quote, backslash, control byte <0x20, or high bit >=0x80.
3. Preserve the wrapper's exact logical charges; before each bounded pure chunk check its logical quota and poll cancellation at a documented bound. Avoid charging the same prefix again on scalar fallback.
4. Use only fixed bounded local/register scratch, or preflight and reserve any dynamic scratch at its actual concurrent peak with receiver, arguments, partial output, and prior buffers.
5. Allocate and publish outputs through existing ownership and accounting helpers; do not turn owned token strings into views or keep full input alive through pooled scratch.
6. No script reentry or mutation inside the kernel. Architecture feature dispatch chooses SIMD or scalar with identical observable results and thresholds.

Unsafe examples: one step per 32 vector lanes instead of per rune; scanning an entire large input between cancellation polls; building a full structural-index table before quota validation; unboxing an entire []Value to []float64 with no scratch reservation; SIMD stores into existing collections without copy-on-write/epoch/publication/write-barrier handling; replacing strings.Clone with substring; charging JSON escape work only by source bytes; reassociating array.sum.

## Validation

Force scalar and SIMD dispatch, then compare results, errors, minimum passing step quota and memory quota, not just speed. Include vector-boundary lengths, short tails, malformed UTF-8, escaped JSON, short-circuit/early-return cases, tight quotas, and cancellation mid-work. Allocation-free scans should keep end-to-end B/op and allocs/op unchanged; parser retained-heap tests must still release source documents.

Existing useful gates:
- step_bulk_charge_test.go:15,38: crossing periodic boundaries and fast path inside one period.
- string_scan_metering_test.go:15 binary-search minimum quota helper; tests at 42,83,113 for scaling/exemptions/short-input charges.
- json_materialization_test.go:85,101,113,128,173,188: elements, live unfinished parents, duplicate release, parser charge/reference estimate, cancellation, fuzz semantics.
- json_retention_test.go:11: retained JSON values/keys release source documents.
- memory_estimator_cache_test.go:653: memo-enabled/disabled exact minimum passing memory thresholds; array_uniq_accounting_test.go:18 extends this to retained scratch and nested callbacks.
- memory_mutations_test.go:14-197: unrelated mutations, shared host maps/arrays, journal overflow, independent concurrent calls, raw host mutations, captured environments.
- env_recycle_test.go:22-42: VIBES_ESTIMATOR_VERIFY=1 full/reference accounting and VIBES_BUILTIN_CONTRACT_VERIFY=1 epoch contract checks.
- collection_values.go:52-58: VIBES_COW_ALWAYS_COPY=1 reference value semantics for mutators; cost-specific tests intentionally skip under this oracle.

The estimator oracle checks bookkeeping equality; both walks can omit the same new Go-local scratch. It therefore does not replace explicit peak-allocation tests and reservation checks.

## Go 1.27 upgrade concerns separate from SIMD

- go.mod:3 currently declares Go 1.26.
- sizeclass.go:3-23,25-57,65-86 mirrors allocator size tables, 8192-byte pages, and strings.Builder.Grow's noscan rounding. sizeclass_test.go:17 compares actual builder capacities across starting capacities and growth sizes, and :68/:93 cover boundaries/projections. Run these under the exact 1.27 toolchain on each target used. Do not assume a package compiles implies preflights still match.
- The file comment mentions roundupsize_exact_test.go, but that file is absent on this tree; the actual guard is sizeclass_test.go. Do not cite a nonexistent full-range test.
- memory.go:15-28 derives Value/int/rune/slice/Env/Builtin sizes via unsafe.Sizeof but retains fixed string-header/map/instance/block/frame estimates. mapStructuralBytes at 3452-3466 is a model using len and fixed costs, not inspection of Go's map layout. Recheck actual heap/capacity boundaries on 1.27 (especially map changes); successful estimator parity alone cannot prove real allocator parity.
- .github/workflows/test.yml:227-237 also compiles linux/386, windows/386, linux/arm, linux/mips. SIMD must have supported-architecture/feature dispatch and buildable scalar fallback; importing an architecture-only SIMD package into generic runtime code could break this matrix.

## Measurement-only ASCII overlay review

Reviewed benchmarks/audits/2026-09-08-simd/probe_test.go.txt and prepare.py after the bounded-load revision. No correctness or accounting defect found in this limited overlay.

- SIMD copies exactly text[:16] into a fixed [16]byte and loads that array; all 16 lanes are initialized. Unsigned ReduceMax >=128 is equivalent to the scalar predicate. The scalar tail is bounded and preserves all byte patterns.
- The word loop's endian choice is immaterial to the repeated high-bit mask. It checks the same eight byte high bits and delegates the tail.
- The overlay replaces only stringIsASCII (members_string.go:1099); callers at 1178,1188,1243,1348 still take identical ASCII/UTF-8 branches. chargeStringCall/chargeStringScanBeforeCall at 160-194 remain unchanged, so logical scan charges, input guards, errors, and output allocation sites are unchanged.
- Exhaustive single-byte placements for every byte at lengths 0..129 establish lane/tail behavior. AllocsPerRun at selected boundary and large lengths is a meaningful regression guard against string-to-byte conversion copies; root reports this passes. The prior LoadUint8x16([]byte(text)) variant allocated input-sized suffixes repeatedly, which would have bypassed graph-based scratch accounting. It must not be used.
- Fixed 16-byte scratch does not grow with input. Zero heap allocation does not mean zero stack/register usage; this distinction is compatible with the existing script heap estimator.
- The existing scalar helper does not poll cancellation inside its full-input loop, and the overlay preserves that. This assessment establishes no regression in cancellation behavior, not a new tighter latency bound.
- All overlay variants compile the same probe importing simd/archsimd. This provides a controlled Go1.27 SIMD-enabled comparison but does not itself verify Go1.26 or other-architecture portability of a selected word implementation. Production SIMD also needs feature/architecture fallback absent from this local measurement fixture.
- prepare.py computes the repository root correctly, finds the exact helper body, and writes overlays outside production sources. No byte-array reinterpreting or unsafe pointer conversion is introduced.
