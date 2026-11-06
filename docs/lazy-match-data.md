# Lazy match data

Design handoff for VIBE-196 and VIBE-128, addressing [#1338](https://github.com/xipkit/vibescript/issues/1338).
Audited baseline: `4cb18cb4ab042a0968eed3cc5303b7da9cdbc1b8` (2026-11-06).
Status: proposed implementation contract; this PR changes no runtime behavior.

## Decision and scope

Add an internal, invocation-local `Kind::Match(Arc<Match>)`. Defer the entire
seven-entry hash, its keys, offset arrays and string windows. Keep ordinary
protected `Kind::Hash` values as the representation visible to hosts and inside
stored aggregates. Normalize at publication and at operations requiring hash
storage. Do not change `Value::as_hash`, introduce unmetered materialization,
or make a captures-only change to the eager constructor.

Lazy matches may live in VM operand stacks, ordinary or captured frame locals,
script positional arguments/returns, and explicitly designated VM scratch
buffers. They may not live inside a published array/hash, an instance or module
field, an exported environment, or any host-visible value. Internal scratch is
not a language array just because it uses `Buffer<Value>`.

The exact issue loop stays lazy:

```vibe global=input:array<string>
input.map { |line|
  m = line.match(/user=([a-z]+) id=([0-9]+) status=([a-z]+)/)
  m == nil ? [] : m.captures
}
```

`Store` copies `m` into a frame local; the nil comparison does not force it;
`captures` constructs only the capture array and its windows; the map output
stores that ordinary array. Conversely `input.map { |line| line.match(...) }`
must force every successful match when storing the map's results. That is an
intentional first-implementation boundary, with a performance control below.
`match?` already bypasses match data and must keep doing so. Namespace
`Regex.match`, which returns a substring, and `scan` retain their existing paths.

## Representation and cache ownership

Use the following owned data, with private fields and fallible constructors:

- The originating `MemoryId`, a charged Arc header, the imported subject Value,
  an Arc to the already imported `regex::value::Regex`, and the search result's
  `Buffer<usize>` **moved**, not copied. Retaining the regex keeps its pattern,
  named-group metadata and compiled storage accounted while the match lives.
  Do not retain a borrowed `program::View` or a search scratch reference.
- Seven `OnceLock<Value>` field slots, an initially empty
  `OnceLock<Buffer<OnceLock<Value>>>` for numbered windows, and a
  `OnceLock<Value>` containing the fully normalized protected hash. The numbered
  cache is allocated for `indices.len()/2` slots only on its first needed read.
  Cache slots distinguish “not computed” from a computed nil.
- The cache has no closure, host callback, context reference or pointer back to
  the lazy match. All cached Values are ordinary values. A fully forced hash
  clones the same field Values, including the same Offset objects.

Use `MemoryId`, not `ctx.owns(&header)`, for ownership: unlimited execution can
have a `None` charge. All lazy operations first check ownership and checkpoint
the receiving context. A foreign lazy value is a latched internal invariant
error, not an invitation to charge a different invocation. No process-global
match cache, and no unmetered or ownerless cache entries.

`OnceLock::get`, followed by fallible construction and `set` on success, is
sufficient; no unstable fallible initialization API is needed. Mutation is
serialized by the invocation's exclusive `&mut CallContext`. `OnceLock` keeps
Value's Send/Sync properties across async suspension without unsafe interior
mutability. Do not hold a lock while evaluating script or host code. A competing
initializer is an invariant violation, not an alternate accounting path.

Cloning a lazy Value shares its caches. The first successful read charges and
creates a window once; a later read only clones it. Named reads, captures and
`to_s` reuse numbered windows. A full normalization reuses all completed fields.
The full cache does not point back to Match, so there is no ownership cycle.
Subject, regex, indices and cache headers remain retained until the last lazy
alias is dropped, even after forcing. The emitted Hash retains only the ordinary
fields. This bounded temporary overhead must be measured, especially when a
frame retains both a forced match and its lazy alias; do not claim whole-hash
access is allocation-neutral before measuring it.

## Exact materialization rules

Validate arity, keyword/block restrictions and selector conversions as today.
Invalid access does not build unrelated fields merely to produce an error.
Keep error kinds, wording, field order and source attribution.

| Operation | First access and later accesses |
| --- | --- |
| No match | Return nil; allocate no Match or cache. |
| Truthiness, `== nil`, `!= nil`, nil-safe dispatch | No materialization. Match is truthy and unequal to nil in either operand order. |
| `.as(match_data)`, typed script parameters/results, type predicates | Recognize both the lazy kind and protected Match hash; no force for nominal match-data recognition. Preserve current public type names (`Value::type_name()` reports `object` for the existing match hash). |
| Numeric `[]` and numeric `fetch` | Convert through existing numeric rules (including truncated floats), translate negative indices against group count including zero. Cache only the selected detached window; absent and out-of-bounds `[]` give nil. `fetch` preserves distinct missing-participation/out-of-bounds errors. No hash, offsets or unrelated windows. |
| String `[]` | Seven public field names win over capture names. Otherwise scan regex name metadata in order, charge the scan, select the last participating group; all absent or unknown gives nil. Read the selected numbered cache. Use the existing `as_bytes` string/symbol handling at runtime; preserve checker rejections and other unsupported-selector behavior. |
| String `fetch` | Use existing byte-like selector conversion and select capture names only, including names colliding with fields or `fetch`; last participating duplicate wins. Unknown/all absent raises the existing missing-key error. No named-captures hash required. |
| `to_s`, `pre_match`, `post_match` | Cache only that window, with `to_s` sharing group zero. Use existing `regex::window`: full-subject windows can share the subject; partial windows remain detached copies. |
| `captures` | Construct a normal array of groups 1..N, reusing numbered windows. No keys, named hash, pre/post windows or offsets. Cache the array. |
| `named_captures` | Build the existing ordered hash and key Values, with the current duplicate-name overwrite/absent rule. Reuse numbered windows. Cache the hash. |
| `begin(i)` / `end(i)` and stored field reads | Materialize only that side's offset array and the existing Offset callable; compute character, not byte, offsets with existing `ops::runes`. Cache one Offset per side. Never construct a new callable on repeated reads. Invalid bare method use remains invalid. |
| `dup`, clone/freeze helpers if reachable, general hash reads, enumeration, transforms, merge and conversion | Force the complete root before existing behavior, including existing errors. Do not add methods the checker currently rejects for match_data. Ordinary traversal then sees the same seven entries and protection. |
| Mutation, including through captures/named_captures lvalues | Preserve existing protected-root checks before any write or callback side effect. Force when the existing address/mutation machinery needs hash storage. Reading an array into a separate variable still allows independent COW mutation. |
| Equality except the nil fast path | Force lazy root operands, then use existing iterative structural equality. Distinct matches must not become equal merely because subject/indices agree: Offset equality uses Arc identity. A clone or forced alias must reuse its original Offset identities. Nested operands already obey the aggregate invariant. |
| Interpolation, string conversion, script output using string semantics | Resolve `to_s` through the lazy field path, then use existing metered text handling. |
| Script inspect, diagnostic rendering that traverses fields | Force before entering existing metered renderer. Preserve current rendering; do not expose a new user-visible lazy representation. |
| JSON | Force the root before the existing writer. Whole match JSON still fails on Offset entries: script `JSON.stringify` reports `JSON.stringify key "begin": JSON.stringify unsupported value type builtin`; the lower-level writer rejects builtins too. JSON of `captures`/`named_captures` still works. Do not silently omit methods. |
| Public `Display`, `Debug`, `as_hash`, host serializers | Receive already normalized Values. They never allocate match fields or call a context. Display uses `to_s`; Debug traverses the object fields as today. |
| Host export and later import | Export a normal protected Hash in the producing invocation; import with the existing Hash/Offset accounting in the receiving invocation. No lazy cache crosses calls. |

Field caches are semantic-value caches, not mutation targets. For example,
`a=m.captures; a.push("x")` must leave `m.captures` unchanged, while
`m.captures.push("x")` must still fail. The address machinery must identify
`Kind::Match` as a protected ancestor before resolving a lazy member, rather
than accidentally treating a cached array as a writable root.

## Preserving the borrowed public API

`Value::as_hash(&self) -> Option<&[(Value, Value)]>` remains exactly as it is.
It cannot force, fail, return an empty placeholder, or hide a match by returning
None to host callers. Introduce one internal operation, conceptually
`normalize(ctx, value) -> Result<Value>`, which returns a cached ordinary Match
hash for a lazy root and otherwise shares the root. Separate field access is
also fallible. Normalize before calling APIs that require an ordinary root.

The invariant has an inductive implementation:

1. Public constructors cannot create `Kind::Match`; only metered regex producers
   can. Ordinary leaves and imported host graphs contain no lazy values.
2. Array/hash constructors and mutation paths normalize every inserted direct
   child (including hash keys before key validation). Existing children are
   already ordinary. Normalize before depth computation and before committing a
   mutation; failed normalization cannot leave a partially changed collection.
3. Instance, file, global, module and namespace field stores normalize before
   insertion or heap mapping. Exported Functions hold Instance environments;
   Namespace/Instance values reach the same stored fields, so snapshotting a
   callable cannot conceal a lazy child.
4. Ordinary captured locals reference live VM frame slots (`simple::captured`),
   not host-visible closures. They may stay lazy across nested blocks and async
   suspension. A block result handed to a host, a stored field, or a captured
   root binding is normalized at that boundary. Do not normalize every local
   `Store`, positional-argument scratch insertion, or script return.
5. Every external root is normalized before the first validator or callback
   can observe it, and before successful outcomes/bindings leave the VM.
   Snapshot/import must normalize a same-owner lazy root **before** any
   “already owned” shortcut. A failure runs no receiving host callback.

There is no claim that these properties already hold; they are the mandatory
implementation invariant and must be tested. The public API is preserved by
excluding lazy values from its domain, not by teaching the borrowed getter to
allocate. Add debug assertions at publication points and a test-only iterative
walker that rejects a lazy value anywhere in a published graph, including
instances, namespace/function environments and cyclic/shared object graphs.
Use the existing visited/heap traversal machinery, not recursive Rust walks.

### Boundary checklist from the source audit

| Seam | Required implementation work |
| --- | --- |
| `regex/operations.rs::first`, `Driver::advance` | Move owned indices and retain the Regex Arc at both producers. A script match block receives the lazy root; a host sees only a normalized block result. |
| `value.rs::from_array`, `from_hash`, `push`, `push_exact`, `set_array_index`; `hash.rs::from_entries`, insertion/replacement and `into_arc` | Establish the aggregate invariant, including direct `buffer.data` construction and mutation shortcuts. Avoid adding a new charged recursive walk to every ordinary collection; fuse kind checks with existing loops. |
| `objects.rs::set`, `set_slot`; `objects/fields.rs`; `address.rs`, `mutate.rs`, VM file/global/namespace stores | Normalize before heap locks/mapping and before committing writes; preserve protected ancestors and recovery behavior. Audit direct field-buffer writes as well as these functions. |
| `address.rs::Address.value`, `arguments.rs::Arguments.receiver`, `vm/simple.rs`, `vm/call_targets.rs` | These are not ordinary local slots. Normalize on capture into a pending address or general call receiver, after any lazy fast path has been selected. Protect Match ancestors before field selection. Audit the direct field path (`simple.rs`) and mutating call/driver classification so the new kind cannot silently skip a field or a protection check. |
| `value/import.rs`, `CallContext::import`, `import_rooted`, `snapshot_values` | Normalize same-owner lazy roots before import/snapshot shortcuts. Foreign lazy roots fail closed. Ordinary cross-call graphs use existing import logic. |
| `vm/capabilities.rs`, `capability.rs::begin/invoke_plain/invoke/finish`, registered callbacks | Positional/keyword arguments and receiver snapshots must be normal before argument contracts, not just before callbacks. Preserve existing callback/result-validation order. |
| `vm/host_blocks.rs`, `vm/asynchronous.rs`, `host_call.rs` | Normalize all values visible from receiver(), block calls/results, break/nonlocal-return handling, async callbacks and their contracts. Future suspension may retain lazy internal locals but cannot publish them. |
| `vm/programs/snapshots.rs`, `exports.rs`, namespace/environment snapshots | Stored aggregate/environment children are normal; normalize root values before source discovery/shortcut walks. Keep graph aliases, cycles, initialization seals and depth guards. |
| `vm/execution.rs::complete`, `capture_root_locals`, `run_bindings` | Normalize final result and captured local roots while the producing context and diagnostic frames still exist, before `release_execution` and stats sampling. Ordinary script returns stay lazy until this outer boundary. Cover sync/async, call_with_stats and sessions/REPL bindings. |
| `members.rs`, `members/direct.rs`, `ops.rs`, `types.rs`, `members/names.rs` | Lazy dispatch must run before concrete-Hash checks and before staging a general member receiver; recognize match_data for narrowing/signatures without forcing. Explicit nil fast path precedes equality normalization. `matches::index/fetch` need dedicated lazy paths; they currently read concrete buffers. |
| `collections.rs`, `hash_blocks.rs`, `iteration*`, `sets.rs`, `shapes.rs`, `arguments.rs` | Normalize actual ordinary-hash operands and inserted builtin arguments before direct storage access. Normalize values returned from script blocks into builtin iteration/hash/ordering drivers at their acceptance point, before classification, hashing or storage. In particular audit `iteration/driver.rs::accept`, reductions, uniq/group/chunk accumulators and transform outputs. Keyword hashes and splat aggregates may force; plain positional script scratch must not. Preserve existing rejection of unsupported match-data operations. |
| `text*`, `format*`, `vm/output.rs`, `json/writer.rs`, `value/render.rs`, `types/diagnostics.rs` | Distinguish `to_s` from inspecting fields. Normalize before any context-free formatting of a lazy root, including error construction. Public Rust formatters remain for published values; an internal diagnostic of an invariant failure must not try to force recursively. |
| `value/destroy.rs`, `value.rs::depth`, type/identity switches | Cache children are bounded ordinary values; include the new Arc in destruction; `depth()` is 2, matching the seven-field hash containing capture arrays/named hashes, even before forcing. Keep Value size and Send/Sync unchanged. |

The baseline search finds 74 files matching `Kind::Hash`; several are parser
AST kinds or tests, not runtime Values. Re-run `rg 'Kind::Hash|as_hash\(' src`
and audit wildcard arms and direct buffer writes as well as exhaustive matches.
Compilation alone will not catch an early wildcard treating Match as a scalar.
Maintain a checked-off list in the implementation PR. No separate parallel
representation may be enabled until all publication seams are covered.
`exports::check` is also called internally: recognize Match there as a protected
object without forcing it. Normalize at actual publication, not indiscriminately
in helpers that ordinary script calls use. For builtin arguments, normalize
only operands that enter an ordinary-value consumer or storage, preserving the
explicit index/field/nil fast paths. Add direct-versus-general dispatch tests
for identical cache states: results, errors and charges must agree as required
by `members/direct.rs`. Different first-read/cache-hit states intentionally have
different materialization costs.

## Accounting and failure behavior

Construction retains existing subject/regex charges without charging shared
bytes again. A host subject/regex is imported first under existing rules. Move
the successful indices buffer with its capacity charge. Reserve
`size_of::<Match>() + 2*size_of::<usize>()` for the Arc allocation before creating
it; that includes fixed empty cache slots and the owner id. Search scratch
retains its current accounting and is released normally. Do not preflight the
old seven-field projected footprint at construction.

On first window read, reserve the numbered-cache capacity and charge one step
per initialized slot; reserve/copy the chosen partial window with existing
`window` accounting. Full-subject aliases and nil slots create no string
allocation. Every access checks cancellation and costs one explicit lookup
step, even on a cache hit. Named lookup additionally charges once per metadata
entry visited plus existing byte comparison work. A field getter costs one
lookup step; lazy whole normalization costs one lookup step whether cached
or not. Normalizing a non-Match root adds no new step or allocation. Full
normalization invokes internal field builders without public-getter lookup
fees. Cache helpers used by field construction charge their actual
per-group visits, not fictitious member-dispatch steps. Keep existing numeric
conversion, Hash, Buffer, depth and rune-scanning charges; do not replace them
with guessed flat fees.

Each field preflights its own missing allocations (including newly needed
cache capacity), then constructs a local result using existing metered helpers.
For offset arrays, charge each group visit and the actual prefix rune scans.
For `captures` and `named_captures`, charge traversal and output capacity; reuse
already charged windows. Fixed key bytes, seven hash entries, Hash/index
headers and any still-missing fields are charged only on whole normalization.
Whole normalization preflights its remaining construction, including temporary
capacity, while already cached allocations count as live usage. All arithmetic
must use checked or saturating projections and existing quota guards.

Publish an individual field only after that field succeeds; publish the full
Hash cache only after all seven fields and Hash construction succeed. A later
failure may leave earlier successful caches reachable internally, but no
partial field/hash or successful result escapes. Failed temporary allocations
release normally; spent steps and observed peaks are not rolled back.
Memory/step/deadline/cancellation/internal failures remain latched. A repeated
read checks the latch **before** using a populated cache; rescue, a host callback
that ignores an error, or an async resumption cannot turn exhaustion into
success. A miss/invalid selector is an ordinary existing error, not a cached
resource failure. Cancellation must be observed on cache hits and within long
metadata/offset/window loops.

This deliberately changes when memory and steps are consumed. An unread match
may succeed under a quota where eager construction failed; a first read or host
publication may now fail later. Ordinary no-quota results, errors and side-effect
order must remain unchanged. Output/host publication must finish normalization
before its side effect. Tight-quota replay changes require individual accounting
review, not blanket observation re-recording.

Fresh imports of exported hashes receive the existing destination-owned storage
and Offset arrays. Dropping the original result must release the original
budget, even while an imported copy remains. Re-import in the same call uses
existing ownership rules. Lazy cache storage is never shared across invocations,
including unlimited-to-metered or metered-to-unlimited transitions.

## Architect consultation and rejected alternatives

The required `ask flynn` consultation completed during this run. Flynn confirmed
that captured frame locals can remain lazy and identified additional builtin
block-return, argument, pending-address and receiver-storage seams; those are
included above. Flynn recommends a cheaper eager record first: static keys and
empties, fewer scratch buffers, and lazy Offset computation, followed by a
compiler field-demand analysis only if long-subject measurements justify it.
His allocation estimates were explicitly hand counts against the historical
issue baseline, not measured comparisons; do not report them as achieved gains.

This handoff retains whole-representation laziness because VIBE-196 explicitly
requires deferring fixed construction, including unread pre/post windows, and
specifying every materialization boundary. Cheap eager construction is a useful
alternative scope, but still builds the record on unread matches; lazy Offsets
would also retain a subject after host export and change their Debug storage.
Static untracked keys would need a separate accounting decision. A compile-time
field-demand mask requires escape analysis and would trade a runtime boundary
proof for a compiler proof that a partial record never escapes. Neither is a
smaller implementation of the requested contract as stated.

This is a documented engineering disagreement, not architect approval. The
normalization audit and whole-store regression gate are necessary costs of this
choice. If the owner chooses cheap eager construction instead, rescope VIBE-128
explicitly; do not label it completion of this lazy-match design. Search-scratch
reuse suggested by Flynn is outside this card and unmeasured here.

## Smallest safe implementation PR and estimate

One implementation PR on VIBE-128 must contain the private representation,
whole normalization, all publication/storage boundaries, type/protection
handling, lazy nil/index/field paths, semantic tests and accounting evidence.
Field access can share one implementation rather than exposing a partial kind.
A captures-only eager-constructor change, or a patch enabling Match before host
boundaries exist, is not an acceptable first PR. No new public API, language
methods, search algorithm, zero-copy substring representation, or global cache
is needed. Keep a test-only eager oracle until the differential tests pass.

Estimate: **6–9 engineering days**, plus hosted review/CI turnaround: 2–3 days
for representation and the boundary audit; 1–2 for access/protection/identity;
2 for failure, host/async and semantic regression tests; 1–2 for allocation and
paired timing evidence, counter audit and fixes. This is not the original
“effort 2, land a stalled PR” task. Rescope VIBE-128 to implementation of this
design before assigning its delivery estimate. Any proposal to retain lazy
matches inside containers should be a separate future design; it would widen
the public-graph proof considerably.

## Required regression matrix

Run the existing regex suites unchanged except the eager-allocation-specific
quota assertion, which must become separate construction/first-read/whole-force
quota tests. Add these targeted tests before handoff:

- No match, unread matches, nil on either side, safe navigation, typed narrowing,
  positional script calls, nested/captured block locals and async suspension.
- Numeric zero/positive/negative/out-of-range indices, huge integers, truncated
  floats, absent versus empty captures; existing bad selector behavior and exact
  fetch errors. Duplicate names with later absent groups; collisions with every
  public field and `fetch`; computed names.
- UTF-8 and invalid-byte subjects, zero-width and full-subject matches, negative
  start offsets. Verify detached partial windows, character offsets and released
  subject storage after exported results no longer need it.
- Every field separately, twice, in different orders and followed by force.
  Verify no repeated field allocation and no construction of unrelated fields.
  Compare Offset identities before/after normalization; equal clones versus
  distinct matches. Do not use pointer equality of Match to replace semantics.
- Protected direct and nested lvalue writes, dup paths, pending addresses and
  mutation callbacks; copied-out arrays/hashes remain independently mutable.
  General hash operations must retain current accepted/rejected behavior.
- Script to_s/interpolation/output, inspect and host Display/Debug, seven-entry
  as_hash order/protection, JSON field success and whole-match failure. Exercise
  error-formatting paths, not only successful rendering.
- Direct and nested host arguments, keywords, argument/result validators,
  registered/fast/block/async callbacks, receiver snapshots, block returns,
  break/nonlocal return, final outcomes, run_bindings, module/function exports,
  instance cycles and shared environments. Each host observer asserts a fully
  ordinary graph. Repeat after the producing invocation has ended.
- One-byte-under/exact memory admission for construction, each field and force;
  exhausted steps, cancellation before cache hits and mid-loop, deadline,
  failed normalization before a container/field write or host callback. Latch
  precedence, partial-buffer release, unchanged prior values, final charge
  release, cross-call import and cross-thread result drop.
- Differential observation comparison against the eager baseline with generous
  limits; counter-only and quota-outcome comparison handled separately. Exercise
  SIMD/portable, normal/unlimited budgets, all-feature async and ordinary native
  suites; retain depth/stack-safety tests for published graphs.

## Counter re-record scope

Discover exact affected ids with a paired baseline/candidate golden sweep from
the same checkout path and machine. Candidate corpora are engine cases in
`conformance` (including upstream/site fixtures), `language`, `rejections`,
`compatibility` and `replay`, plus
any CLI witness that reports runtime counters. Parser/compiler/LSP-only
observations should not change. Unrelated runtime counter drift is evidence
of an overly broad normalization charge, not automatically acceptable.

Follow [the golden-counter process](../tests/golden/README.md): save a per-case
before/after counter and quota-outcome audit, check observations and independent
fixture expectations, and record only reviewed affected ids using `--cases`.
Do not replace all observations to hide later quota failures. Record twice,
retain all unselected observations/counters, and add the rationale to the
Counter log. ARM and x86 accounting differences already exist; validate each
against its own baseline. No counters are changed by this design PR.

## Measurements and implementation acceptance

The issue reported 6,782 allocations / about 508 KB with three capture groups,
and 5,634 allocations without groups: about 83% fixed construction. Those are
historical measurements, not numbers for this checkout. A fresh baseline is
recorded in [the benchmark evidence](../benchmarks/lazy-match-design.md).
No candidate runtime exists in this design PR, so there are **no measured after
numbers** and no claimed speedup. The implementation PR must fill that column.

Use the exact 128-line core fixture, with 102 successes and 26 invalid lines,
without precomputing regexes or substituting a simplified loop. Compare:

| Workload | What it isolates | Required candidate result |
| --- | --- | --- |
| Unread, three groups; unread, no groups | Whole fixed-structure deferral | No seven-field/hash/window construction before discard; material allocation reduction in both. |
| `[0]`, `[1]`, repeated `[1]` | Whole/single window and caching | Only selected windows; repeats add lookup work but no more match-field allocations. |
| Original `m.captures` loop | Issue's real use case | No fixed hash, offset arrays or pre/post windows; lower allocations and timing than baseline. |
| Store all matches, then read `[1]` | Mandatory aggregate normalization | Same outputs; quantify eager-equivalent work plus lazy overhead, including peak retention. |
| `match?` | Unchanged control | No new match allocations or meaningful regression. |

Extend these in the implementation benchmark set with named lookup versus
named_captures, begin/end, all fields, `dup`, host export/as_hash and separate
JSON failure validation. Include subjects with nonempty long prefixes/suffixes,
large captures, zero/three/many groups, duplicate names, different hit ratios,
one versus repeated reads, and field-read-then-force. Returned small windows
must not retain a large subject after the lazy local is dropped.

Build separate release binaries for timing and allocation counting. Validate
results before timing. Use at least eight balanced baseline/candidate rounds,
calibrated to >=150 ms per workload, with SIMD/portable and metered/unlimited
modes; use matched ARM/x86 hosts for delivery. Report medians, sample ranges,
allocations, allocated bytes, steps, tracked peak/retained bytes and process RSS.
Archive machine/toolchain, source hashes, fixture hashes and per-round results.
Keep compilation and result serialization outside call timing; preserve the
harness's same warm-up and result-lifetime behavior on both revisions.

Acceptance requires substantial allocation reductions for unread, indexed and
captures cases, including the groupless control. Investigate any reproducible
>3% timing regression in whole-store/general access or unaffected core controls;
do not hide it in an aggregate mean. Check the cache/header overhead before
broadening optimizations. The baseline here establishes reproducibility and
semantics, not a substitute for paired implementation measurements.
