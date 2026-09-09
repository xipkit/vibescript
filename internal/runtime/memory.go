package runtime

import (
	"fmt"
	"math"
	"math/big"
	"reflect"
	"sync/atomic"
	"unsafe"

	"github.com/mgomes/vibescript/vibes/value"
)

const (
	estimatedValueBytes        = int(unsafe.Sizeof(Value{}))
	estimatedIntBytes          = int(unsafe.Sizeof(int(0)))
	estimatedRuneBytes         = int(unsafe.Sizeof(rune(0)))
	estimatedStringHeaderBytes = 16
	estimatedSliceBaseBytes    = int(unsafe.Sizeof([]Value{}))
	estimatedMapBaseBytes      = 48
	estimatedMapEntryBytes     = 32
	estimatedEnvBytes          = int(unsafe.Sizeof(Env{}))
	estimatedInstanceBytes     = 16
	estimatedBlockBytes        = 24
	estimatedBuiltinBytes      = int(unsafe.Sizeof(Builtin{}))
	estimatedCallFrameBytes    = 48
	estimatedModuleContextSize = 24
)

// estimatedHashDataBytes is the heap footprint of the hashData wrapper every
// KindHash value allocates around its entry map to carry Ruby-style default
// metadata (a default value and/or a default proc). The entry map and default
// payloads are charged separately; this is the wrapper struct itself, which the
// estimate would otherwise miss for workloads that retain many small hashes (an
// array of Hash.new or empty literals).
const estimatedHashDataBytes = value.HashDataBytes

// estimatedObjectDataBytes is the heap footprint of the objectData wrapper
// every KindObject value allocates around its entry map.
const estimatedObjectDataBytes = value.ObjectDataBytes

// estimatedArrayWrapperExtraBytes is the part of the arrayData wrapper every
// KindArray value allocates that the array's own slice accounting does not
// already cover. sliceStructuralBytes charges estimatedSliceBaseBytes for the
// element slice header, and for an array that header is a field of the wrapper
// rather than an allocation of its own, so what is missing is only what the
// wrapper carries beyond it. Charging value.ArrayDataBytes whole on top would
// bill the header twice.
//
// It is derived from both sides so that neither can drift: a field added to
// arrayData is charged by the commit that adds it, and a change to what the
// slice base models is subtracted from it. head was added without either, which
// took the wrapper from 24 bytes to 32 and left 8 unmetered per array -- an
// under-count introduced by a fix for an under-count.
//
// Drift is not only over time. Both sides are word-sized, so this is 8 bytes on
// a 64-bit target and 4 on a 32-bit one, and the assertion below found that out:
// while the slice base was the hand-written 24 it stayed 24 on 386, where a
// slice header is 12 and the wrapper is 16, and the remainder went negative. A
// stated size is not merely stale-prone, it is blind to the target it is
// compiled for.
const estimatedArrayWrapperExtraBytes = value.ArrayDataBytes - estimatedSliceBaseBytes

// A wrapper smaller than the slice header the estimator already bills would
// make the remainder negative and quietly subtract from every array's charge,
// which is the direction that escapes a quota. An array length may not be
// negative, so this stops compiling before it can -- on every architecture the
// release matrix builds, which is what caught the hand-written slice base.
var _ [estimatedArrayWrapperExtraBytes]struct{}

// estimatedBigIntStructBytes is the heap footprint of the big.Int struct a
// big-integer payload allocates around its word backing (sign flag plus the
// word slice header). The words themselves are charged per allocated slot on
// top of this.
const estimatedBigIntStructBytes = int(unsafe.Sizeof(big.Int{}))

// estimatedBigIntWordBytes is the size of one big.Word in a big-integer
// payload's backing array.
const estimatedBigIntWordBytes = int(unsafe.Sizeof(big.Word(0)))

const inlineSeenEnvs = 8

// estimatedMapEntryStructuralBytes is the per-entry structural footprint a
// map[string]Value reserves for one slot regardless of what its key and value
// point at: the bucket overhead, the key string header, and the value slot. It
// is what make(map[string]Value, n) charges per capacity slot beyond the empty
// map's base overhead; the key bytes and value payloads are charged on top by
// whoever inserts them. The hash projections and the incremental build
// accumulator share this constant so the up-front and running budgets reserve
// the same per-entry structure.
const estimatedMapEntryStructuralBytes = estimatedMapEntryBytes + estimatedStringHeaderBytes + estimatedValueBytes

type memoryEstimator struct {
	seenFrozen       *Env
	seenEnvInline    [inlineSeenEnvs]*Env
	seenEnvVersions  [inlineSeenEnvs]uint64
	seenEnvInlineLen int
	seenEnvs         map[*Env]uint64
	seenMaps         map[uintptr]struct{}
	seenHashData     map[uintptr]struct{}
	seenObjectData   map[uintptr]struct{}
	seenSlices       map[uintptr]struct{}
	seenStrings      map[stringIdentity]struct{}
	seenClasses      map[*ClassDef]struct{}
	seenInstances    map[*Instance]struct{}
	seenBlocks       map[*Block]struct{}
	seenBuiltins     map[*Builtin]struct{}

	// journal, when non-nil, records every seen-set entry a walk newly inserts so
	// the walk can be rolled back to the estimator's prior state. It backs the
	// per-call charged-root probe (see blockBindCharge.begin), which measures a
	// transient root's marginal footprint against the persistent call-root estimator
	// without permanently committing that root: committing it would let a later
	// root that reuses the transient's freed backing be falsely deduplicated, an
	// under-count that could escape the memory quota. A walk records nothing when
	// journal is nil, which is the common path.
	journal *estimatorJournal

	// dormant, when non-nil, is the execution's committed dormant-frame set (see
	// memory_dormant.go). env returns zero for any env in it: those frames are
	// already charged in exec.dormantBytes, so this deduplicates them out of the
	// root walk, module walk, and extras walk without double counting. It is a
	// borrowed pointer to exec.dormantSet, so it stays current as the set is
	// reconciled; reset clears it.
	dormant map[*Env]struct{}

	// walked counts the graph nodes this estimator has visited over its whole
	// lifetime, so a caller that drives a walk from script code can charge the
	// step quota for the work it caused (see chargeEstimatorWalk). It is
	// monotonic and survives reset: callers read differences around their own
	// walk rather than an absolute, and a reset mid-walk would make a
	// difference negative.
	//
	// Both of the estimator's recursive descents increment it: value for the
	// reachable-value graph and env for the environment chain. Counting only
	// values made the charge track a fraction of the walk rather than the walk,
	// because a frame that binds nothing (or binds only lazy values) calls
	// value zero times while still costing a seen-set probe and a parent-chain
	// step. A literal under 64 nested zero-parameter block drivers walked
	// 13,923,112 env frames against 38,431 values, so the counter saw 0.3% of
	// the work (#1). Everything else the walk does is downstream of one of
	// these two: the journal rollback and the seen-set reset are proportional
	// to the identities a counted visit inserted, and the per-check scalar base
	// is fixed overhead the walk does not drive.
	walked int
}

// estimatorJournal records the seen-set keys a single probe walk inserts so the
// probe can be undone, restoring the estimator to its pre-probe state. Each slice
// holds only the keys this probe added (entries the estimator already contained
// before the probe are not recorded and stay committed), so rollback removes
// exactly the probe's own additions and is O(values walked by the probe). The
// single-slot frozen cache is not journaled per-write; probe captures its
// pre-probe value and rollback restores it directly.
type estimatorJournal struct {
	envs       []*Env
	maps       []uintptr
	hashData   []uintptr
	objectData []uintptr
	slices     []uintptr
	strings    []stringIdentity
	classes    []*ClassDef
	instances  []*Instance
	blocks     []*Block
	builtins   []*Builtin

	// limit, when positive, bounds how many insertions this journal records;
	// entries counts them and overflowed reports that the limit was hit. A
	// memoized base-walk session uses a bounded journal (see sessionJournalBudget)
	// because extras that dedup against the base record almost nothing, while a
	// large fresh extra graph (a capability result not yet bound anywhere)
	// would make the journal — and its entry-by-entry rollback — cost more
	// than simply re-walking on the next check. Once overflowed the walk stops
	// recording and the session's close invalidates the memo instead of
	// rolling back: a partial rollback would leave unrecorded extras committed,
	// letting a later value falsely deduplicate against a freed backing.
	// probe's journals stay unbounded (limit 0) so probes always roll back
	// exactly.
	limit      int
	entries    int
	overflowed bool
}

// minSessionJournalLimit floors a memoized session's journal budget. Hit-path
// extras (call roots that alias the environment, literals, small fresh values)
// insert at most a few dozen identities, so this floor keeps them journaled
// even over a near-empty environment.
const minSessionJournalLimit = 256

// sessionJournalBudget sizes a session's journal from the committed base
// walk's deduplicated identity count. Journaling extras of E identities costs
// roughly one append plus one rollback delete per identity, while losing the
// memo costs a full base re-walk (B identities) on the next check — usually
// several checks. Extras comparable to the base are therefore still worth
// journaling, but a graph much larger than the base (a huge capability return
// not yet bound anywhere) is cheaper to re-walk than to journal and roll back
// at every check site it passes through.
func sessionJournalBudget(baseIdentities int) int {
	if baseIdentities < minSessionJournalLimit {
		return minSessionJournalLimit
	}
	return baseIdentities
}

// identityCount reports how many deduplicated identities the estimator has
// committed across its seen-sets. Used to size a session's journal budget
// relative to the base walk it protects.
func (est *memoryEstimator) identityCount() int {
	return est.seenEnvInlineLen + len(est.seenEnvs) + len(est.seenMaps) +
		len(est.seenHashData) + len(est.seenObjectData) + len(est.seenSlices) + len(est.seenStrings) +
		len(est.seenClasses) + len(est.seenInstances) + len(est.seenBlocks) +
		len(est.seenBuiltins)
}

// record reports whether the journal should record one more insertion,
// tripping overflowed once a bounded journal fills.
func (j *estimatorJournal) record() bool {
	if j.limit > 0 && j.entries >= j.limit {
		j.overflowed = true
		return false
	}
	j.entries++
	return true
}

type stringIdentity struct {
	ptr uintptr
	len int
}

func newMemoryEstimator() *memoryEstimator {
	return &memoryEstimator{}
}

func (est *memoryEstimator) reset() {
	est.seenFrozen = nil
	est.journal = nil
	est.dormant = nil
	clear(est.seenEnvInline[:est.seenEnvInlineLen])
	est.seenEnvInlineLen = 0
	clear(est.seenEnvs)
	clear(est.seenMaps)
	clear(est.seenHashData)
	clear(est.seenObjectData)
	clear(est.seenSlices)
	clear(est.seenStrings)
	clear(est.seenClasses)
	clear(est.seenInstances)
	clear(est.seenBlocks)
	clear(est.seenBuiltins)
}

// probe walks val against the estimator's current seen-set to measure val's
// marginal footprint (the bytes not already deduplicated against everything the
// estimator has committed), then rolls the estimator back to its pre-probe state
// so nothing val touched is retained. It backs the per-call charged-root probe in
// blockBindCharge.begin, where a transient root (a reduce accumulator that changes
// every call) must deduplicate against the committed call roots (the receiver)
// without being committed itself -- so the NEXT call's accumulator cannot falsely
// deduplicate against this one's possibly-freed backing.
//
// probe saves and restores any journal already open on the estimator, so it can
// run inside a memoized base-walk session (see beginBaseWalk): the probe's own
// insertions are rolled back here and never leak into the session's journal,
// while insertions the session recorded before the probe stay journaled for the
// session's own rollback.
func (est *memoryEstimator) probe(val Value) int {
	prevFrozen := est.seenFrozen
	prevJournal := est.journal
	journal := &estimatorJournal{}
	est.journal = journal
	size := est.value(val)
	est.journal = prevJournal
	journal.rollback(est, prevFrozen)
	return size
}

// rollback removes from est exactly the seen-set entries the journal recorded,
// restoring the single-slot frozen cache to the value it held before the probe.
func (j *estimatorJournal) rollback(est *memoryEstimator, prevFrozen *Env) {
	est.seenFrozen = prevFrozen
	for _, env := range j.envs {
		est.forgetEnv(env)
	}
	for _, id := range j.maps {
		delete(est.seenMaps, id)
	}
	for _, id := range j.objectData {
		delete(est.seenObjectData, id)
	}
	for _, id := range j.hashData {
		delete(est.seenHashData, id)
	}
	for _, id := range j.slices {
		delete(est.seenSlices, id)
	}
	for _, key := range j.strings {
		delete(est.seenStrings, key)
	}
	for _, cl := range j.classes {
		delete(est.seenClasses, cl)
	}
	for _, inst := range j.instances {
		delete(est.seenInstances, inst)
	}
	for _, blk := range j.blocks {
		delete(est.seenBlocks, blk)
	}
	for _, builtin := range j.builtins {
		delete(est.seenBuiltins, builtin)
	}
}

// clear resets the journal to empty while keeping its backing capacity so a
// memoized base-walk session can reuse one journal across checks without
// re-allocating. The insertion count and overflow flag are the journal's
// per-session budget accounting (see record and sessionJournalBudget), so they
// reset alongside the records: without this the count would accumulate across
// every check in a call and spuriously trip the limit — permanently disabling
// the memo for the rest of the call once tripped. Pointer-typed records are
// zeroed first so a rolled-back probe value cannot be retained by the truncated
// backing array.
func (j *estimatorJournal) clear() {
	clear(j.envs)
	j.envs = j.envs[:0]
	j.maps = j.maps[:0]
	j.hashData = j.hashData[:0]
	j.objectData = j.objectData[:0]
	j.slices = j.slices[:0]
	j.strings = j.strings[:0]
	clear(j.classes)
	j.classes = j.classes[:0]
	clear(j.instances)
	j.instances = j.instances[:0]
	clear(j.blocks)
	j.blocks = j.blocks[:0]
	clear(j.builtins)
	j.builtins = j.builtins[:0]
	j.entries = 0
	j.overflowed = false
}

// memoryEstimatorForCheck returns the shared per-execution estimator reset to
// an empty seen-state, invalidating any memoized base walk because the caller
// is about to overwrite the committed state the memo points at. Checks
// themselves go through beginBaseWalk; this remains for probes (tests and
// diagnostics) that need a bare estimator with the execution's identity.
func (exec *Execution) memoryEstimatorForCheck() *memoryEstimator {
	if c := exec.baseWalkCache; c != nil {
		c.valid = false
		c.unmemoizedPrefix = false
	}
	est := &exec.memoryEst
	est.journal = nil
	est.reset()
	return est
}

// bumpMutationEpoch advances the process-wide mutation epoch that invalidates
// every memoized estimator base walk. Runtime code must call it before any
// write that changes state reachable from an execution's roots outside the
// value package's own wrapper mutators: environment container writes go through
// Env's setters (which bump), and array/hash wrapper mutations go through the
// value package (which bumps), so the direct call sites are the raw
// slice/map writes (index assignment, ivar and class-var stores) plus builtin
// dispatch, which wholesale covers every Go builtin's internal writes.
func bumpMutationEpoch() { value.BumpMutationEpoch() }

// baseWalkCacheDisabled turns off base-walk memoization so tests can assert
// that memoized and unmemoized estimates are byte-identical. Never set outside
// tests: disabling costs a full graph re-walk per check but changes no result.
var baseWalkCacheDisabled atomic.Bool

// estimatorVisitCounting and estimatorVisits let a test count the nodes the
// estimator walks, which is the work a complexity assertion actually cares
// about. Allocation totals only approximate it -- a change in how the seen
// maps grow would decouple the two -- and wall-clock folds in scheduling and
// instrumentation. Never set outside tests; when off this costs one relaxed
// load per node on a path that already does map work.
var (
	estimatorVisitCounting atomic.Bool
	estimatorVisits        atomic.Uint64
)

// baseWalkCache memoizes the reachable-graph portion of the estimator's base
// walk between checks. The memo is the pair (exec.memoryEst's committed
// seen-state, graphBytes): a later check whose graph provably cannot have
// changed -- its reachable environment versions and wrapper mutation history
// are still current, and its root-set topology matches -- resumes from that state instead of
// re-walking the graph, then walks only its extra roots. Because the resumed
// computation is literally the suffix of the walk the check would have
// performed from scratch (the estimator's total is order-independent: it is a
// deduplicated union over reachable identities), a memoized check returns
// exactly the bytes an unmemoized one would. A relevant mutation discards the
// whole memo; unrelated lexical writes and journaled wrapper writes preserve it.
// Opaque writes and incomplete mutation history conservatively discard it too.
//
// Extra roots walked on top of the memo are recorded in journal and rolled
// back when the session closes, restoring the committed base-only state (and
// the single-slot frozen-env cache via prevFrozen) so the next check resumes
// from an uncontaminated memo. A stale rollback would let a later extra
// falsely deduplicate against a freed backing, so the journal is cleared and
// re-armed on every session.
type baseWalkCache struct {
	journal    estimatorJournal
	prevFrozen *Env
	graphBytes int
	// An uncached region check keeps only its prefix identities for assigning
	// work to later writes. Its byte total must never serve a memo hit.
	unmemoizedPrefix bool
	// outputBytes is what the registered driver outputs contribute on top of
	// graphBytes, deduplicated against it and committed into the same
	// seen-state (see memory_output.go). It is memoized rather than re-walked
	// per check because a driver performs many checks between changes to its
	// output: re-walking an n-element output at every one of them made
	// array.map quadratic (4000 elements went from 3.4ms to 216ms). A driver
	// commits each retained result's marginal through addRetainedOutput, and
	// any mutation a callback performs bumps the mutation epoch, which discards
	// the whole memo -- so a result a callback grew or detached is re-derived
	// before the next check reads it.
	outputBytes int
	// journalBudget is the per-session journal limit derived from the
	// committed walk's identity count (see sessionJournalBudget), refreshed
	// whenever the graph walk is re-memoized.
	journalBudget int
	epoch         uint64
	opaqueEpoch   uint64
	wrapperEpoch  uint64
	topo          uint64
	// regionBoundary records which walk shape the memoized graphBytes holds: a
	// block-iteration region's prefix boundary (see memory_blockregion.go), or
	// noBlockRegion for the ordinary whole-stack walk. A check whose boundary
	// differs from the cached one re-walks, so the memo never serves a
	// prefix-only total to a whole-stack check or vice versa.
	regionBoundary int
	valid          bool
}

// noBlockRegion is the regionBoundary sentinel for a memo recorded by an
// ordinary (non-region) base walk, whose graphBytes covers the entire env
// stack rather than a block-iteration prefix.
const noBlockRegion = -1

// releaseBaseWalkCache parks the execution's memo struct in the owning
// engine's single spare slot once the call that owned it finishes, so the next
// Script.Call on the engine reuses it instead of allocating: without this,
// every quota-enforced call — including the shortest scripts — would pay one
// cache allocation on its first memoizable check. The memo is invalidated and
// its env reference dropped before parking, so nothing from this execution
// survives into the next borrower (beginBaseWalk additionally re-invalidates
// on acquisition). A cache with a session still open (which cannot happen on
// the call-completion path) is left in place rather than parked.
//
// Only the struct is recycled; the journal's backing arrays are dropped for
// the GC. That is deliberate twice over. Reusing the backing across calls
// reproducibly doubled the per-op cost of high-frequency large-payload call
// loops on darwin/arm64 (a GC/runtime interaction the allocation and hit-rate
// counters rule out being algorithmic — walk counts, GC cycle counts, and
// bytes allocated were all equal), and keeping arrays grown by one call's
// large journal budget would otherwise be retained on the engine indefinitely.
// The backing regrows lazily only in calls whose sessions actually record
// insertions, which short calls do not.
//
// A single atomic slot is used instead of a sync.Pool deliberately: an atomic
// swap costs a few nanoseconds, has no per-GC-cycle machinery, and captures
// the dominant reuse pattern (sequential calls on one engine). Concurrent
// calls beyond the slot simply allocate fresh.
func (exec *Execution) releaseBaseWalkCache() {
	c := exec.baseWalkCache
	if c == nil || exec.baseWalkOpen || exec.engine == nil {
		return
	}
	exec.baseWalkCache = nil
	c.valid = false
	c.unmemoizedPrefix = false
	c.prevFrozen = nil
	c.journal = estimatorJournal{}
	exec.engine.spareBaseWalkCache.Store(c)
}

// baseWalkSession is one memory check's view of the base walk: est is
// positioned exactly after the base walk (committed seen-state loaded), and
// base is the full base total (reachable graph plus the cheap scalar state
// recomputed every check). The caller walks its extra roots through est --
// they deduplicate against the base and against each other, exactly as in an
// unmemoized walk -- and must call close before the next check runs.
type baseWalkSession struct {
	exec *Execution
	est  *memoryEstimator
	base int
	// walked0 is est.walked as the session opened, so nodes can report the
	// graph nodes this session visited no matter which estimator beginBaseWalk
	// selected.
	walked0     int
	cached      bool
	region      bool
	prefixNodes int
	walkBilled  bool
}

// nodes reports how many graph nodes this session has walked so far, base walk
// included: a memo hit walks almost none, a miss walks the whole graph. Callers
// that drive sessions from script code charge the step quota for it (#1).
//
// Callers billing the whole walk close it with closeCharged. Other sessions
// queue only the work attributable to this execution's committed assignments;
// unrelated invalidations do not become script work merely by causing a miss.
func (s *baseWalkSession) nodes() int {
	return s.est.walked - s.walked0
}

// estimatorNodesPerStep is the number of graph nodes one sandbox step covers
// when script code drives an estimator walk.
//
// Visiting a node is a switch plus a seen-set map probe, so it is dominated by
// the same hashing an array element comparison pays; the rate is amortized for
// the same reason chargeStringScan's is, so that ordinary code whose accounting
// walks a handful of nodes is not charged for the runtime's bookkeeping while a
// walk that scales with host-supplied data cannot stay free. A wide hash literal
// with duplicate keys re-walked a 10k-element host array once per pair, 51.7M
// estimator visits for a few thousand steps, and nothing charged for it (#1).
const estimatorNodesPerStep = 64

// chargeEstimatorWalk charges the step quota for an estimator walk of n nodes.
// Walking nothing costs nothing, and a walk short enough to round down to zero
// steps is already bounded by the caller's own per-expression charge.
func (exec *Execution) chargeEstimatorWalk(n int) error {
	steps := n / estimatorNodesPerStep
	if steps <= 0 {
		return nil
	}
	return exec.stepN(steps)
}

// beginBaseWalk opens a base-walk session, reusing the memoized graph walk
// when it is provably current and re-walking (and re-memoizing) otherwise.
//
// The memo is bypassed entirely -- neither used nor refreshed -- while a Go
// builtin that has not declared non-mutation is on the call stack
// (undeclaredBuiltinDepth > 0), because such code can mutate reachable
// containers through raw slice/map writes between its own checks without
// bumping the epoch; and under the test-only kill switch. Bypass walks run on
// the same shared estimator, so they invalidate the memo (valid = false) rather
// than leave stamps pointing at a clobbered seen-state.
// baseWalkSessionsAreCheap reports whether beginBaseWalk would resume a
// memoized or region-prefix walk rather than re-walk the reachable graph, so
// an incremental accounting caller can afford one session per increment
// instead of snapshotting a reference walk up front.
func (exec *Execution) baseWalkSessionsAreCheap() bool {
	if exec.baseWalkOpen {
		return false
	}
	if exec.blockRegionBaseWalkEngaged() {
		return true
	}
	return exec.undeclaredBuiltinDepth == 0 && !baseWalkCacheDisabled.Load()
}

func (exec *Execution) beginBaseWalk() baseWalkSession {
	if exec.baseWalkOpen {
		// Sessions never nest (estimator walks do not re-enter evaluation); if
		// one ever does, a throwaway estimator keeps the open session's
		// committed state intact at the cost of one uncached walk.
		est := newMemoryEstimator()
		return baseWalkSession{exec: exec, est: est, base: exec.estimateMemoryUsageBase(est)}
	}
	scalars := exec.estimateScalarBase()
	est := &exec.memoryEst
	walked0 := est.walked
	if exec.blockRegionBaseWalkEngaged() {
		return exec.beginRegionBaseWalk(est, scalars)
	}
	if exec.undeclaredBuiltinDepth > 0 || baseWalkCacheDisabled.Load() {
		if exec.blockRegionActive && exec.blockRegionBoundary >= 0 && exec.blockRegionBoundary <= len(exec.envStack) {
			return exec.beginUncachedRegionBaseWalk(est, scalars)
		}
		// The bypass walk clobbers whatever committed state the shared
		// estimator held, so an existing memo is discarded; a cache that was
		// never allocated stays unallocated and the bypass costs nothing.
		if c := exec.baseWalkCache; c != nil {
			c.valid = false
			c.unmemoizedPrefix = false
		}
		exec.baseWalkOpen = true
		est.journal = nil
		est.reset()
		// The bypass path runs while builtin Go code can mutate reachable state,
		// so it uses the full reference walk rather than the dormant
		// optimization: the fast path is reserved for the memoized path below,
		// where the graph is stable. popEnv still keeps the committed prefix consistent here
		// (it retracts on every pop), so the next memoized check resumes correctly.
		//
		// Registered driver outputs are walked last, so they deduplicate against
		// everything the graph walk committed (see memory_output.go). This is the
		// branch a builtin's own callbacks run on, so it is where the accounting
		// matters most.
		base := scalars + exec.estimateGraphBase(est)
		base = saturatingAdd(base, exec.outputWalkBytes(est))
		return baseWalkSession{
			exec: exec, est: est, base: base, walked0: walked0,
		}
	}
	c := exec.baseWalkCache
	if c == nil {
		if exec.engine != nil {
			c = exec.engine.spareBaseWalkCache.Swap(nil)
		}
		if c == nil {
			c = new(baseWalkCache)
		}
		// A recycled cache may carry another execution's stamps; its memo is
		// meaningless against this execution's empty estimator, so it starts
		// invalid and the first check commits fresh.
		c.valid = false
		c.unmemoizedPrefix = false
		exec.baseWalkCache = c
	}
	// Snapshot before walking so the next check considers mutations that arrive
	// during the walk instead of treating their sequence as already observed.
	epoch := value.MutationEpoch()
	if !c.valid || !c.mutationsCurrent(est, epoch) || c.topo != exec.baseTopoVersion || c.regionBoundary != noBlockRegion {
		est.reset()
		c.captureMutationEpochs(epoch)
		c.topo = exec.baseTopoVersion
		c.regionBoundary = noBlockRegion
		c.graphBytes = exec.estimateGraphBaseFast(est)
		// Walked after the graph and committed alongside it, so a driver's later
		// results deduplicate against everything already counted and a check
		// between two of them pays nothing (see memory_output.go).
		c.outputBytes = exec.outputWalkBytes(est)
		c.journalBudget = sessionJournalBudget(est.identityCount())
		c.valid = true
	}
	exec.baseWalkOpen = true
	c.prevFrozen = est.seenFrozen
	c.journal.clear()
	c.journal.limit = c.journalBudget
	est.journal = &c.journal
	// Point the estimator at the committed dormant set so the session's extras
	// walk deduplicates against it. On a memo hit estimateGraphBaseFast did not run
	// this check, but the memo is valid only while the topology is unchanged, so
	// the set still matches the stack. See memory_dormant.go.
	est.dormant = exec.currentDormantSet()
	return baseWalkSession{
		exec: exec, est: est, base: scalars + c.graphBytes + c.outputBytes,
		walked0: walked0, cached: true,
	}
}

// close ends a base-walk session. A memoized session rolls back every
// insertion its extra roots made, restoring the committed base-only state the
// next check resumes from; a bypass session leaves the shared estimator dirty
// exactly as pre-memo checks did (the next session resets it).
func (s *baseWalkSession) close() {
	if s.est != &s.exec.memoryEst {
		return
	}
	s.collectAssignmentWork()
	s.exec.baseWalkOpen = false
	if !s.cached {
		return
	}
	c := s.exec.baseWalkCache
	s.est.journal = nil
	if c.journal.overflowed {
		// The extras walk committed more identities than the journal recorded;
		// a partial rollback would leave the memo claiming a base-only state
		// it no longer holds, so discard the memo and let the next check
		// re-walk from scratch.
		c.valid = false
		c.unmemoizedPrefix = false
		c.journal.clear()
		return
	}
	c.journal.rollback(s.est, c.prevFrozen)
	c.journal.clear()
}

// closeCharged ends a session whose caller already bills its complete walk.
func (s *baseWalkSession) closeCharged() {
	s.walkBilled = true
	s.close()
}

// memoryQuotaExceededError builds the canonical memory-quota failure for this
// execution and latches it: unlike the step quota, the memory measurement is a
// live reachable-graph walk that would legitimately recover once the offending
// value goes out of scope, so without the latch a rescuing loop could exceed
// the quota forever one allocation at a time.
func (exec *Execution) memoryQuotaExceededError() error {
	return exec.latchExhaustion(fmt.Errorf("%w (%d bytes)", errMemoryQuotaExceeded, exec.memoryQuota))
}

// memoryBudgetBytes reports the largest total graph this execution may hold
// before a check would refuse it.
//
// This is what a caller needs when it is about to *size* an allocation rather
// than admit one already made: a scratch reservation, a projected entry cap, a
// match table built before any check can see it. Sizing against anything looser
// allocates the buffer first and refuses it afterwards, which is exactly the
// spike the bound exists to stop.
//
// An unlimited quota answers with the largest int rather than with zero, so a
// caller dividing this figure is not told it has no room at all.
func (exec *Execution) memoryBudgetBytes() int {
	if exec.memoryQuota <= 0 {
		return math.MaxInt
	}
	return exec.memoryQuota
}

// memoryExceeded reports whether a just-measured graph estimate breaches this
// execution's memory quota.
//
// It is the single place the bound is applied, so a check site cannot compare
// against something looser and admit an allocation the quota refuses.
//
// The comparison is guarded on a positive quota rather than relying on callers
// to have guarded it. Reached with the unlimited quota of zero, a bare
// `used > exec.memoryQuota` refuses every non-empty graph.
func (exec *Execution) memoryExceeded(used int) bool {
	return exec.memoryQuota > 0 && used > exec.memoryQuota
}

func (exec *Execution) checkMemory() error {
	if exec.memoryQuota <= 0 {
		return nil
	}
	return exec.checkMemoryMetered()
}

// checkMemoryValue charges a single just-produced value against the memory
// quota. The quota guard is kept small and inlinable so unlimited-quota
// execution (the CLI default) pays only a field load and branch rather than a
// variadic call into the estimator.
func (exec *Execution) checkMemoryValue(val Value) error {
	if exec.memoryQuota <= 0 {
		return nil
	}
	return exec.checkMemoryWith(val)
}

// checkMemoryMetered runs the no-extras estimator check. Callers reach it only
// after the inlinable quota guard has confirmed metering is active.
func (exec *Execution) checkMemoryMetered() error {
	used := exec.estimateMemoryUsage()
	if exec.memoryExceeded(used) {
		return exec.memoryQuotaExceededError()
	}
	return exec.chargeAssignmentWalk()
}

// checkMemoryWith charges current usage plus extras, and is the hard check the
// per-value sites reach.
func (exec *Execution) checkMemoryWith(extras ...Value) error {
	if exec.memoryQuota <= 0 {
		return nil
	}
	if exec.memoryExceeded(exec.estimateMemoryUsage(extras...)) {
		return exec.memoryQuotaExceededError()
	}
	return exec.chargeAssignmentWalk()
}

// memoryFitsWith reports whether current usage plus extras stays within the
// quota without building (and latching) a quota error. Soft probes that have a
// cheaper fallback — the comparison memo, the merge projections — ask this
// instead of a check function: not fitting is a capacity answer for them, not
// budget exhaustion.
//
// It compares against memoryBudgetBytes, the same figure the sizing callers
// use, because two of its callers allocate on the answer -- an output map, a
// comparison memo -- so it must not answer yes to anything a refusal would
// reject.
func (exec *Execution) memoryFitsWith(extras ...Value) bool {
	if exec.memoryQuota <= 0 {
		return true
	}
	return exec.estimateMemoryUsage(extras...) <= exec.memoryBudgetBytes()
}

func (exec *Execution) checkMemoryWithCallRoots(callee, receiver Value, args []Value, kwargs map[string]Value, block Value) error {
	if exec.memoryQuota <= 0 {
		return nil
	}

	used := exec.estimateMemoryUsageForCallRoots(callee, receiver, args, kwargs, block)
	if exec.memoryExceeded(used) {
		return exec.memoryQuotaExceededError()
	}
	return nil
}

func (exec *Execution) checkMemoryWithPositionalCallRoots(receiver, arg0, arg1 Value, argCount int) error {
	if exec.memoryQuota <= 0 {
		return nil
	}

	used := exec.estimateMemoryUsageForPositionalCallRoots(NewNil(), receiver, arg0, arg1, argCount, NewNil())
	if exec.memoryExceeded(used) {
		return exec.memoryQuotaExceededError()
	}
	return nil
}

// checkAccumulatorWithCallRoots rejects a fold step whose running accumulator,
// together with the builtin's live call roots and any extra Go-local values that
// coexist with it at the step's peak, would exceed the memory quota. Builtins
// that grow a single Go-local accumulator value from a non-rooted receiver —
// Array#sum building a string or array total — use it instead of the plain
// checkMemoryWith(accumulator). The receiver, args, and block stay live on the Go
// call stack for the builtin's whole run yet are invisible to
// estimateMemoryUsageBase, so a check that charged only the accumulator could
// admit a peak (call roots + accumulator) that exceeds the quota until the
// builtin returns. Charging the accumulator and the call roots through one
// deduplicating estimator keeps the running check consistent with the pre-call
// checkCallMemoryRoots: an accumulator that aliases the receiver or an argument
// is counted once, matching the real shared backing.
//
// liveExtras are additional values that are live on the Go call stack alongside
// the new accumulator at the step's allocation peak but are not reachable from
// any call root. Array#sum passes both the prior total and the contribution it
// just produced: arraySumAdd builds the next accumulator from a fresh copy of the
// old total and the contribution, so the old total, the contribution, and the new
// accumulator all coexist at the peak. The prior total is the critical case once
// it has grown across iterations into a large string or array reachable only from
// that Go-local — the base walk never sees it. Without charging both extras, a
// quota above call roots + new accumulator but below call roots + old total +
// contribution + new accumulator would admit a step whose true peak exceeds the
// limit. Each extra is charged through the same deduplicating estimator, so an
// extra that aliases a receiver element or the accumulator itself is counted once,
// matching the real shared backing.
func (exec *Execution) checkAccumulatorWithCallRoots(accumulator, receiver Value, args []Value, kwargs map[string]Value, block Value, liveExtras ...Value) error {
	if exec.memoryQuota <= 0 {
		return nil
	}

	s := exec.beginBaseWalk()
	used := s.base
	used = saturatingAdd(used, s.est.value(accumulator))
	if receiver.Kind() != KindNil {
		used = saturatingAdd(used, s.est.value(receiver))
	}
	for _, arg := range args {
		used = saturatingAdd(used, s.est.value(arg))
	}
	for _, kwarg := range kwargs {
		used = saturatingAdd(used, s.est.value(kwarg))
	}
	if !block.IsNil() {
		used = saturatingAdd(used, s.est.value(block))
	}
	for _, extra := range liveExtras {
		used = saturatingAdd(used, s.est.value(extra))
	}
	s.close()

	if exec.memoryExceeded(used) {
		return exec.memoryQuotaExceededError()
	}
	return nil
}

// checkProjectedStringBytes rejects allocations that would exceed the memory
// quota before the string is built. Builtins that grow a string by a
// user-controlled amount (such as the padding helpers) use it to fail fast
// instead of materializing a huge buffer that the post-call check would only
// catch after the allocation already happened. payloadBytes is the byte length
// of the string that would be produced.
func (exec *Execution) checkProjectedStringBytes(payloadBytes int) error {
	if exec.memoryQuota <= 0 {
		return nil
	}

	s := exec.beginBaseWalk()
	used := s.base
	s.close()

	used = saturatingAdd(used, estimatedValueBytes+estimatedStringHeaderBytes)
	used = saturatingAdd(used, payloadBytes)
	if exec.memoryExceeded(used) {
		return exec.memoryQuotaExceededError()
	}
	return nil
}

func (exec *Execution) checkProjectedStringBytesWithCallRoots(payloadBytes int, receiver Value, args []Value, kwargs map[string]Value, block Value) error {
	if exec.memoryQuota <= 0 {
		return nil
	}

	used := exec.estimateMemoryUsageForCallRoots(NewNil(), receiver, args, kwargs, block)
	used = saturatingAdd(used, estimatedValueBytes+estimatedStringHeaderBytes)
	used = saturatingAdd(used, payloadBytes)
	if exec.memoryExceeded(used) {
		return exec.memoryQuotaExceededError()
	}
	return nil
}

func (exec *Execution) checkProjectedStringBytesWithPositionalCallRoots(payloadBytes int, receiver, arg0, arg1 Value, argCount int) error {
	if exec.memoryQuota <= 0 {
		return nil
	}

	used := exec.estimateMemoryUsageForPositionalCallRoots(NewNil(), receiver, arg0, arg1, argCount, NewNil())
	used = saturatingAdd(used, estimatedValueBytes+estimatedStringHeaderBytes)
	used = saturatingAdd(used, payloadBytes)
	if exec.memoryExceeded(used) {
		return exec.memoryQuotaExceededError()
	}
	return nil
}

func (exec *Execution) checkProjectedStringBytesAndScratchWithCallRoots(payloadBytes, scratchBytes int, receiver Value, args []Value, kwargs map[string]Value, block Value) error {
	if exec.memoryQuota <= 0 {
		return nil
	}

	used := exec.estimateMemoryUsageForCallRoots(NewNil(), receiver, args, kwargs, block)
	used = saturatingAdd(used, scratchBytes)
	used = saturatingAdd(used, estimatedValueBytes+estimatedStringHeaderBytes)
	used = saturatingAdd(used, payloadBytes)
	if exec.memoryExceeded(used) {
		return exec.memoryQuotaExceededError()
	}
	return nil
}

// checkProjectedBigIntBytes rejects a big-integer allocation of the given word
// count before it happens, following the #604 preflight convention the string
// builders use: integer exponentiation and large multiplications project their
// result's word count and fail fast instead of materializing a payload the
// post-op check would only catch after the allocation.
func (exec *Execution) checkProjectedBigIntBytes(words int) error {
	if exec.memoryQuota <= 0 {
		return nil
	}

	s := exec.beginBaseWalk()
	used := s.base
	s.close()

	used = saturatingAdd(used, estimatedValueBytes+estimatedBigIntStructBytes)
	used = saturatingAdd(used, saturatingMul(words, estimatedBigIntWordBytes))
	if exec.memoryExceeded(used) {
		return exec.memoryQuotaExceededError()
	}
	return nil
}

// checkProjectedValueRendering rejects a step that renders a value into a fresh
// string (or streams it into a builder) before that string is built, when the
// peak would exceed the memory quota. It charges three things that are all live at
// the peak of the write: the execution's reachable roots (the base), the rendered
// value's own footprint, and the result string (its header plus payloadBytes of
// rendered output). It backs both string interpolation and the `inspect` builtin,
// which share the shape "value stays live while its rendering materializes."
//
// The value's footprint matters because the rendered expression may produce a
// temporary that is not reachable from any environment — a function return, an
// array/hash literal constructed inline, or a receiver like `[big].inspect`. Such
// a temporary lives only on the Go call stack while its rendering is copied, so
// estimateMemoryUsageBase never sees it, yet it is real memory held alongside the
// new string. Without charging it, base+output and base+value could each fit the
// quota while base+value+output exceeds it, letting a huge temporary render past
// the limit.
//
// val is charged through the same estimator that walks the base, so a value that
// IS reachable from an environment (an existing variable rendered directly) is
// deduplicated against the base and contributes only its already-counted footprint
// once, leaving the small-render fast path unchanged.
func (exec *Execution) checkProjectedValueRendering(val Value, payloadBytes int) error {
	if exec.memoryQuota <= 0 {
		return nil
	}

	s := exec.beginBaseWalk()
	used := s.base
	used = saturatingAdd(used, s.est.value(val))
	s.close()

	used = saturatingAdd(used, estimatedValueBytes+estimatedStringHeaderBytes)
	used = saturatingAdd(used, payloadBytes)
	used = saturatingAdd(used, renderingScratchBytes(val))
	if exec.memoryExceeded(used) {
		return exec.memoryQuotaExceededError()
	}
	return nil
}

// renderingScratchBytes is the peak heap inspect/to_s holds for iteration
// buffers while val stays live. A recorded-order hash walks in place; a
// fallback hash or any object larger than the inline buffer allocates a
// []HashEntry that remains live while nested values render, so the peak is
// this node's buffer plus the largest child path rather than the root only.
func renderingScratchBytes(val Value) int {
	return walkRenderingScratch(val, make(map[uintptr]struct{}))
}

func walkRenderingScratch(val Value, seen map[uintptr]struct{}) int {
	switch val.Kind() {
	case KindArray:
		id := arrayIdentity(val)
		if id != 0 {
			if _, ok := seen[id]; ok {
				return 0
			}
			seen[id] = struct{}{}
		}
		maxChild := 0
		for _, elem := range val.Array() {
			if n := walkRenderingScratch(elem, seen); n > maxChild {
				maxChild = n
			}
		}
		return maxChild
	case KindHash, KindObject:
		id := hashScanIdentity(val)
		if id != 0 {
			if _, ok := seen[id]; ok {
				return 0
			}
			seen[id] = struct{}{}
		}
		own := 0
		if !val.HashUsesRecordedOrder() {
			own = sortedHashEntryBufferBytes(val.HashLen())
		}
		maxChild := 0
		for _, item := range val.HashEntryMap() {
			if n := walkRenderingScratch(item, seen); n > maxChild {
				maxChild = n
			}
		}
		return saturatingAdd(own, maxChild)
	default:
		return 0
	}
}

// checkProjectedIntArrayBytes rejects allocations that would exceed the memory
// quota before the array is built. Builtins that preallocate an array of int
// values sized by a user-controlled count (such as the range materialization
// helpers) use it to fail fast instead of reserving a huge backing array that
// the per-element check would only catch after the allocation already happened.
// count is the number of int values the array would hold; each int value
// contributes only the base Value size.
func (exec *Execution) checkProjectedIntArrayBytes(count int) error {
	return exec.checkProjectedIntArrayBytesWithLive(count, 0, NewNil())
}

// checkProjectedIntArrayBytesWithLive is checkProjectedIntArrayBytes for a
// projection that must also account for memory that is already allocated but not
// reachable from any environment root, so estimateMemoryUsageBase cannot see it.
// Destructuring assignment uses it: while assignDestructure runs, the evaluated
// right-hand side (liveRoot) is held only on the Go call stack — a function or
// capability return, or an array literal — and a defensive snapshot of it
// (liveSlots) may have been copied into another Go-local slice. Both are live at
// the peak of the array this check guards (a named rest's captured window), yet
// neither is reachable from an environment, so the base walk misses them.
//
// liveRoot is the live right-hand-side value, charged through the same estimator
// that walks the base so a right-hand side that IS reachable from an environment
// (an existing variable destructured directly) deduplicates against the base and
// contributes only its already-counted footprint once. A nil liveRoot (the
// builtin callers, which have no off-stack right-hand side) charges nothing.
// liveSlots is the snapshot's slot count; its slot array is charged structurally
// because the snapshot shares element payloads with liveRoot, which already
// charged them. Charging all three projects the true peak (base + right-hand side
// + snapshot + array), which the per-statement check would otherwise miss because
// the right-hand side and snapshot are gone by the time control returns from the
// assignment.
func (exec *Execution) checkProjectedIntArrayBytesWithLive(count, liveSlots int, liveRoot Value) error {
	if exec.memoryQuota <= 0 {
		return nil
	}

	s := exec.beginBaseWalk()
	used := s.base
	if liveRoot.Kind() != KindNil {
		used = saturatingAdd(used, s.est.value(liveRoot))
	}
	s.close()

	if liveSlots > 0 {
		used = saturatingAdd(used, liveValueSliceBytes(liveSlots))
	}
	used = saturatingAdd(used, arraySlotBackingBytes(count))
	if exec.memoryExceeded(used) {
		return exec.memoryQuotaExceededError()
	}
	return nil
}

// checkProjectedArrayBytesWithCallRoots rejects an array result whose slot
// backing, precomputed retained element payload, and still-live scratch would
// exceed the quota while the builtin's receiver, arguments, keyword arguments,
// and block are live.
//
// liveScratchBytes is what the caller is still holding while the result is
// built and which the root walk cannot see: an index table the engine returned,
// a temporary slice the elements are copied out of. It is a required argument
// rather than an optional one because omitting it is invisible. A preflight
// that priced only the result read as complete, and twice on one change it was
// not: String#lines held a []string of every line beside the result it copied
// into, 81,920 bytes unpriced against 200,056 charged at 4,000 lines, and
// String#scan held an index table whose spare capacity went uncounted. Both
// projections fit a quota the real peak exceeded. Passing an explicit zero is a
// claim that nothing else is live, and is worth stating.
func (exec *Execution) checkProjectedArrayBytesWithCallRoots(slotCount, payloadBytes, liveScratchBytes int, receiver Value, args []Value, kwargs map[string]Value, block Value) error {
	if exec.memoryQuota <= 0 {
		return nil
	}
	used := exec.estimateMemoryUsageForCallRoots(NewNil(), receiver, args, kwargs, block)
	used = saturatingAdd(used, arraySlotBackingBytes(slotCount))
	used = saturatingAdd(used, payloadBytes)
	used = saturatingAdd(used, liveScratchBytes)
	if exec.memoryExceeded(used) {
		return exec.memoryQuotaExceededError()
	}
	return nil
}

// checkProjectedArrayBytesWithPositionalCallRoots is the positional-root form of
// checkProjectedArrayBytesWithCallRoots; liveScratchBytes means the same thing.
func (exec *Execution) checkProjectedArrayBytesWithPositionalCallRoots(slotCount, payloadBytes, liveScratchBytes int, receiver, arg0, arg1 Value, argCount int) error {
	if exec.memoryQuota <= 0 {
		return nil
	}

	used := exec.estimateMemoryUsageForPositionalCallRoots(NewNil(), receiver, arg0, arg1, argCount, NewNil())
	used = saturatingAdd(used, arraySlotBackingBytes(slotCount))
	used = saturatingAdd(used, payloadBytes)
	used = saturatingAdd(used, liveScratchBytes)
	if exec.memoryExceeded(used) {
		return exec.memoryQuotaExceededError()
	}
	return nil
}

// checkProjectedHashBytes rejects allocations that would exceed the memory quota
// before a derived map is built. Hash transform helpers (such as merge, except,
// compact, and remap_keys) materialize an output map sized to their inputs; for
// large receivers that backing map can dwarf the quota, and the statement-level
// check would only observe it after the allocation already happened. count is the
// number of entries the output map would hold. Each entry contributes the map's
// per-entry overhead plus a key header and a value slot; the keys and values are
// references shared with the receiver and arguments, whose payloads are already
// counted in the call-root usage below, so only the new map's structural
// footprint is projected here.
//
// The receiver, arguments, and block are the call roots that hold the transform's
// inputs alive while the output map is built. When the transform runs on an
// ephemeral receiver or argument (for example a hash literal or capability return
// invoked immediately, `{ ... }.compact` or `h.merge(load_defaults)`), those
// inputs are reachable only through these roots and are not part of
// estimateMemoryUsageBase, so the projection must count them or it would
// under-report the peak and admit a transform that doubles the live footprint.
// The estimator de-duplicates by backing pointer, so a root that overlaps the
// base (a named local) or another root (`h.merge(h)`) is counted once.
func (exec *Execution) checkProjectedHashBytes(count int, receiver Value, args []Value, kwargs map[string]Value, block Value) error {
	return exec.checkProjectedHashTransformBytes(count, 0, receiver, args, kwargs, block)
}

// checkProjectedHashTransformBytes rejects a map-producing hash transform before
// it allocates either its output map or the sorted-key scratch buffer(s) that
// drive an ordered walk. outputEntries is the number of entries the result map
// would hold; scratchBytes is the heap footprint of the scratch key slices (see
// sortedHashEntryBufferBytes), which the caller sums per-buffer so the inline-stack-
// buffer threshold is applied to each independently.
//
// The output map's fixed overhead is always charged, even when outputEntries is
// zero: a transform that produces a hash (merge, select, transform_values, ...)
// allocates a real empty map for an empty result. Pure iterators that build no
// map (each, each_key, each_value) use checkProjectedHashWalkBytes instead, which
// omits that overhead.
//
// Both allocations coexist at peak: the scratch list of every key is live while
// the output map fills, so they are charged together against the same call-root
// baseline. The buffered keys alias the receiver's map keys, already counted in
// the call-root usage, so only the scratch slices' headers (not the key bytes)
// are added here. Without this the scratch list -- one header per entry, outside
// the output-map projection -- could allocate past the quota on a large receiver
// before any later check observed it.
func (exec *Execution) checkProjectedHashTransformBytes(outputEntries, scratchBytes int, receiver Value, args []Value, kwargs map[string]Value, block Value) error {
	if exec.memoryQuota <= 0 {
		return nil
	}
	// The caller allocates the output map on the strength of this, so it is a
	// final admission and goes through the check rather than through the probe
	// beside it.
	if exec.memoryExceeded(exec.projectedHashTransformBytes(outputEntries, scratchBytes, receiver, args, kwargs, block)) {
		return exec.memoryQuotaExceededError()
	}
	return nil
}

// projectedHashTransformBytes is the figure both the admission above and the
// probe below are asking about, so the two cannot answer different questions.
func (exec *Execution) projectedHashTransformBytes(outputEntries, scratchBytes int, receiver Value, args []Value, kwargs map[string]Value, block Value) int {
	used := exec.projectedHashBaseBytes(receiver, args, kwargs, block)
	used = saturatingAdd(used, saturatingMul(outputEntries, estimatedMapEntryStructuralBytes))
	// The output hash grows an insertion-order slot per entry beside its map,
	// which the reservation helpers charge; this admission must agree with them
	// or it admits an output the reservation then refuses.
	used = saturatingAdd(used, hashOrderBackingBytes(outputEntries))
	return saturatingAdd(used, scratchBytes)
}

// projectedHashTransformFits is checkProjectedHashTransformBytes's quota test
// without the error: the merge projection probes a loose upper bound with it
// and falls back to the exact union count when the bound does not fit, so the
// probe must not latch the execution as exhausted.
func (exec *Execution) projectedHashTransformFits(outputEntries, scratchBytes int, receiver Value, args []Value, kwargs map[string]Value, block Value) bool {
	if exec.memoryQuota <= 0 {
		return true
	}

	return exec.projectedHashTransformBytes(outputEntries, scratchBytes, receiver, args, kwargs, block) <= exec.memoryBudgetBytes()
}

// hashTransformBufferBytes returns the Go-local heap footprint a block-driven hash
// transform (select, reject, transform_keys, transform_values, and the
// block-conflict merge) holds for its whole walk: the output map it fills plus any
// sorted-key scratch slices. outputEntries is the largest entry count the result
// map could reach; scratchBytes is the scratch slices' footprint (see
// sortedHashEntryBufferBytes). The empty-map base overhead is always included because the
// transform allocates a real map even for an empty result.
//
// These buffers live only on the Go call stack while the block runs, so they are
// invisible to estimateMemoryUsageBase. Callers reserve this through
// reserveLoopScratch BEFORE building the block-call runner so the runner's
// bind-charge baseline already includes them: otherwise a rest-collecting
// destructure block (|k, (head, *tail)|) would charge its fresh tail backing
// against a baseline that omits the output map and scratch, letting
// receiver+out+scratch and receiver+tail each fit the quota while the real peak
// receiver+out+scratch+tail exceeds it. It mirrors what checkProjectedHashTransformBytes
// charges so the up-front reservation and the projection agree byte-for-byte.
func hashTransformBufferBytes(outputEntries, scratchBytes int) int {
	bytes := estimatedValueBytes + estimatedMapBaseBytes + estimatedHashDataBytes
	bytes = saturatingAdd(bytes, saturatingMul(outputEntries, estimatedMapEntryStructuralBytes))
	bytes = saturatingAdd(bytes, hashOrderBackingBytes(outputEntries))
	return saturatingAdd(bytes, scratchBytes)
}

// checkProjectedHashWalkBytes rejects a pure hash iterator (each, each_key,
// each_value, and the `for k, v in hash` statement) before it walks the receiver.
// These iterators return the receiver and build no derived map, so they must not
// be charged an output map they never create; charging a phantom empty map here
// would falsely reject a quota that exactly fits the receiver and block.
//
// The scratch these iterators materialize stays live while their body runs, so
// callers reserve it through reserveLoopScratch before this check rather than
// passing it here: the reservation folds the scratch into the live baseline
// (hashCallRootBytes measures it), which both charges it at this preflight and
// keeps every memory check inside the body aware of it. A collapsed-pair walk
// reserves its largest transient [key, value] pair (maxCollapsedPairBytes) the
// same way, so the symbol-key payload the estimator bills is captured exactly
// without re-walking the receiver per entry (the walk stays O(n) in the receiver
// size).
func (exec *Execution) checkProjectedHashWalkBytes(receiver Value, args []Value, kwargs map[string]Value, block Value) error {
	if exec.memoryQuota <= 0 {
		return nil
	}

	if used := exec.hashCallRootBytes(receiver, args, kwargs, block); exec.memoryExceeded(used) {
		return exec.memoryQuotaExceededError()
	}
	return nil
}

// checkReservedLoopScratch rejects a hash walk or transform after it has folded
// Go-local buffers into reservedScratchBytes, but before the caller allocates or
// walks those buffers. It charges the current reserved scratch together with the
// call roots, matching the baseline every later memory check will observe.
func (exec *Execution) checkReservedLoopScratch(receiver Value, args []Value, kwargs map[string]Value, block Value) error {
	if exec.memoryQuota <= 0 {
		return nil
	}

	if used := exec.hashCallRootBytes(receiver, args, kwargs, block); exec.memoryExceeded(used) {
		return exec.memoryQuotaExceededError()
	}
	return nil
}

// maxCollapsedPairBytes returns the heap footprint of the largest transient
// two-element [key, value] array a collapsed-pair hash walk allocates over
// entries (Hash#each or `for pair in hash` with a single-parameter block). Only
// one pair is live at a time -- the previous entry's pair is unreferenced once the
// next overwrites the slot it is bound to -- so reserving the largest one for the
// walk's lifetime conservatively bounds the transient when body checks cannot see
// the Go-stack receiver.
//
// Each pair is PROBED against a single estimator seeded once with the receiver, so
// the value the pair references deduplicates against the receiver (it aliases the
// receiver's own entry) and is charged only its reference slot, while the array
// structure and the symbol key's string payload -- which the estimator bills on top
// of the structure -- are charged in full. probe rolls the estimator back after each
// pair, so the receiver is walked exactly once and every pair is measured against the
// same baseline: the scan is O(n) in the number of entries, not O(n^2).
//
// Reserving this exact maximum (rather than a fixed structural constant) closes the
// escape where a constant omitting the symbol payload let a quota between
// receiver+structure and receiver+full-pair pass the preflight while the body check,
// blind to the Go-stack receiver, also passed -- letting the true peak exceed the
// quota. Because the reservation is folded into the live baseline, it also keeps the
// pair charged alongside any fresh rest backing a destructuring block binds, so the
// combined peak (receiver + pair + rest) is bounded, not just each term alone.
//
// It returns 0 when no memory quota is enforced, skipping the per-entry probe scan
// the reservation would otherwise pay for a budget nothing checks.
func (exec *Execution) maxCollapsedPairBytes(receiver Value) int {
	if exec.memoryQuota <= 0 {
		return 0
	}
	est := newMemoryEstimator()
	est.value(receiver)
	return maxCollapsedPairBytesWithEstimator(receiver, est)
}

func maxCollapsedPairBytesWithEstimator(receiver Value, est *memoryEstimator) int {
	if receiver.HashLen() == 0 {
		return 0
	}
	maxBytes := 0
	var entryBuf [smallHashKeyBufferSize]HashEntry
	for _, entry := range receiver.HashEntriesInto(entryBuf[:]) {
		pair := NewArray([]Value{entry.Key, entry.Value})
		if bytes := est.probe(pair); bytes > maxBytes {
			maxBytes = bytes
		}
	}
	return maxBytes
}

func (exec *Execution) valueReachableFromLiveBase(value, block Value) bool {
	if exec.memoryQuota <= 0 || value.Kind() == KindNil {
		return false
	}
	s := exec.beginBaseWalk()
	if !block.IsNil() {
		s.est.value(block)
	}
	reachable := s.est.probe(value) <= estimatedValueBytes
	s.close()
	return reachable
}

func (exec *Execution) checkCollapsedPairBytesWithLiveBase(receiver, block Value) error {
	if exec.memoryQuota <= 0 {
		return nil
	}
	s := exec.beginBaseWalk()
	used := s.base
	if !block.IsNil() {
		used = saturatingAdd(used, s.est.value(block))
	}
	used = saturatingAdd(used, maxCollapsedPairBytesWithEstimator(receiver, s.est))
	s.close()
	if exec.memoryExceeded(used) {
		return exec.memoryQuotaExceededError()
	}
	return nil
}

// beginAccumulatorMeteredSection opens an accumulator-metered section and
// returns the closure that ends it; callers invoke it as
// `defer exec.beginAccumulatorMeteredSection()()` around a blockless native
// build loop. While a section is active, step()'s periodic slow path skips the
// full reachable-graph memory walk and keeps only the step-quota and
// context-cancellation checks.
//
// A loop may open a section only when both invariants hold for everything the
// section spans:
//
//  1. No script re-entry: the loop never yields to a block, invokes a hash
//     default proc, calls a capability callback, or otherwise evaluates script
//     code, and it does not mutate any container reachable from an execution
//     root. The reachable graph the periodic walk measures therefore cannot
//     change while the section is active — the walk would re-measure the same
//     answer every period.
//  2. Pre-charged allocation: every allocation the loop performs is charged
//     against the memory quota before it happens, through an
//     arrayBuildAccumulator/hashBuildAccumulator reservation
//     (reserveSlots/reserveScratch/checkSlotArrays/add) or a projected-growth
//     charge. The accumulator baselines include the same scalar-plus-graph
//     base the periodic walk measures (estimateMemoryUsageBase) plus the call
//     roots, so any quota the skipped walk would have rejected is rejected by
//     the loop's own accumulator checks at the same threshold.
//
// Together the invariants make the skipped walk provably redundant, so
// removing it must not move any acceptance threshold; it only removes the
// O(reachable graph) re-walk the slow path paid every stepSlowPathMask+1
// steps, which made large blockless materializations quadratic.
//
// The section must never remain in force across code that does not satisfy
// the invariants. Rather than trusting every future call path, the runtime
// auto-degrades: block invocation (callBlock), script function invocation
// (callFunctionWithBoundEnv), and builtin dispatch (invokeCallable) all
// suspend the counter for the duration of the nested call, restoring it on
// return. A section leaked across a re-entry point therefore never weakens
// the nested code's checks — the degradation is strictly conservative, which
// is why this is a silent suspend rather than a dev-build assertion.
//
// Sections nest (the counter increments) and the returned end closure is
// safe under defer-based unwinding. The #903 base-walk memo is unaffected:
// inside builtins the memo stays bypassed exactly as before, this only skips
// the redundant bypass walks themselves.
func (exec *Execution) beginAccumulatorMeteredSection() func() {
	exec.accumMeteredSections++
	return func() { exec.accumMeteredSections-- }
}

// arrayBuildAccumulator charges the memory of an array assembled element by
// element against the quota without re-walking the whole prefix on each append.
// Builtins that grow a Go-local result slice from values they cannot bound up
// front (such as Array#fill's block form, where each block call can return an
// arbitrarily large value, or Array#filter_map, which keeps arbitrary truthy
// block results) use it so accumulated payloads count toward the quota during
// construction, not only after the builtin returns.
//
// checkProjectedIntArrayBytes is enough for results whose every element is an
// inlined scalar, because there the slot array is the entire allocation. It does
// not account for the payloads reachable from each element (string bytes, nested
// collections), so a fill block returning many quota-sized strings would slip
// past it until the post-call check. This accumulator closes that gap: it keeps
// a baseline of everything live at the start of the build plus a running total of
// each kept element's payload, walking only the new element on each append.
//
// The baseline includes the builtin's live call roots (receiver, args, kwargs,
// block), not just exec's reachable roots. While a builtin runs, those roots are
// held on the Go call stack and are invisible to estimateMemoryUsageBase, yet
// they are still live memory the result accumulates on top of — exactly what the
// pre-call checkCallMemoryRoots charges. Seeding them here keeps the incremental
// check consistent with that pre-call check, so a transform whose receiver or
// captured block already nears the quota cannot accumulate an unbounded result
// that only the post-call check would catch.
type arrayBuildAccumulator struct {
	exec    *Execution
	est     *memoryEstimator
	result  *memoryEstimator
	base    int
	payload int

	// Call roots retained so checkTransient can re-seed a throwaway estimator
	// with the same baseline the build was snapshotted against, deduplicating a
	// transient yielded value against memory already charged in base.
	receiver Value
	args     []Value
	kwargs   map[string]Value
	block    Value
}

type arraySortByItem struct {
	item  Value
	key   Value
	index int
}

func arraySortByDecoratedBufferBytes(count int) int {
	if count <= 0 {
		return 0
	}
	return saturatingAdd(estimatedSliceBaseBytes, saturatingMul(count, int(unsafe.Sizeof(arraySortByItem{}))))
}

// newArrayBuildAccumulator snapshots the execution's current live memory —
// exec's reachable roots plus the builtin's live call roots (receiver, args,
// kwargs, block) — as the baseline for an incremental array build. It uses its
// own estimator rather than the execution's shared one so nested evaluation (a
// block call, say) cannot reset the seen-set mid-build; that estimator persists
// across add calls so a value aliased by an earlier element or already reachable
// from the baseline is counted once, matching the real shared backing.
//
// Pass the same receiver/args/kwargs/block the builtin received so the baseline
// reflects what checkCallMemoryRoots charged before the call: a nil receiver,
// nil kwargs, and nil block are skipped, mirroring estimateMemoryUsageForCallRoots.
func newArrayBuildAccumulator(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) *arrayBuildAccumulator {
	acc := &arrayBuildAccumulator{
		exec:     exec,
		receiver: receiver,
		args:     args,
		kwargs:   kwargs,
		block:    block,
	}
	if exec.memoryQuota <= 0 {
		return acc
	}
	acc.est = newMemoryEstimator()
	acc.base = exec.estimateMemoryUsageBase(acc.est)

	if receiver.Kind() != KindNil {
		acc.base = saturatingAdd(acc.base, acc.est.value(receiver))
	}
	for _, arg := range args {
		acc.base = saturatingAdd(acc.base, acc.est.value(arg))
	}
	for _, kwarg := range kwargs {
		acc.base = saturatingAdd(acc.base, acc.est.value(kwarg))
	}
	if !block.IsNil() {
		acc.base = saturatingAdd(acc.base, acc.est.value(block))
	}
	return acc
}

// add charges a newly appended element and rejects the build if the result's
// projected memory exceeds the quota. backingCap is the capacity of the result's
// backing slice after the append; its slot array is charged from that capacity
// while only the element's payload beyond its slot is added to the running total,
// so the slot is never double counted.
//
// Elements aliased by a baseline root (for example filter_map returning an
// element of its receiver unchanged) are deduplicated by the persistent
// estimator, so their backing is charged once, exactly as the post-call check
// would. Scratch buffers held live for the build's duration are charged via
// reserveScratch, which folds them into the baseline so they are counted
// alongside the growing result rather than separately.
func (acc *arrayBuildAccumulator) add(val Value, backingCap int) error {
	if acc.exec.memoryQuota <= 0 {
		return nil
	}

	acc.payload = saturatingAdd(acc.payload, acc.est.valuePayload(val))

	if used := acc.projected(backingCap); acc.exec.memoryExceeded(used) {
		return acc.exec.memoryQuotaExceededError()
	}
	return nil
}

// addToReservedBacking charges an appended element when the caller has already
// reserved the result array's value and backing slice through reserveLoopScratch.
// The reservation is part of acc.base, so this method adds only retained element
// payloads and avoids charging an empty array backing a second time.
func (acc *arrayBuildAccumulator) addToReservedBacking(val Value) error {
	if acc.exec.memoryQuota <= 0 {
		return nil
	}

	acc.payload = saturatingAdd(acc.payload, acc.est.valuePayload(val))
	if used := saturatingAdd(acc.base, acc.payload); acc.exec.memoryExceeded(used) {
		return acc.exec.memoryQuotaExceededError()
	}
	return nil
}

// addConservative charges a block-produced result without deduplicating it
// against the build baseline. That keeps in-place mutations of receiver-owned
// containers visible to the quota while still deduplicating shared backings
// across retained results.
func (acc *arrayBuildAccumulator) addConservative(val Value, backingCap int) error {
	if acc.exec.memoryQuota <= 0 {
		return nil
	}

	if acc.result == nil {
		acc.result = newMemoryEstimator()
	}
	acc.payload = saturatingAdd(acc.payload, acc.result.valuePayload(val))

	if used := acc.projected(backingCap); acc.exec.memoryExceeded(used) {
		return acc.exec.memoryQuotaExceededError()
	}
	return nil
}

func (acc *arrayBuildAccumulator) addConservativeToReservedBacking(val Value) error {
	if acc.exec.memoryQuota <= 0 {
		return nil
	}

	if acc.result == nil {
		acc.result = newMemoryEstimator()
	}
	acc.payload = saturatingAdd(acc.payload, acc.result.valuePayload(val))
	if used := saturatingAdd(acc.base, acc.payload); acc.exec.memoryExceeded(used) {
		return acc.exec.memoryQuotaExceededError()
	}
	return nil
}

// checkTransient rejects the build when a freshly allocated value yielded to the
// block (and live only for that call) would push the peak footprint over the
// quota. Builders that synthesize a per-iteration argument the result does not
// retain — hash.map_with_index allocates a fresh [key, value] pair to yield —
// use it so the live pair is charged alongside the accumulating result, matching
// how each_with_index charges its yielded pair before invoking the block.
//
// The transient is measured against a throwaway estimator re-seeded with the
// build's call roots, so memory already counted in base (the receiver value the
// pair wraps) deduplicates away and only the transient's fresh allocation is
// added. Using a throwaway estimator rather than the persistent results-only one
// keeps the transient's backing out of the seen-set: it is freed before the next
// iteration, and recording it could let a later value reusing that address be
// dedup'd to nothing. backingCap is the result backing's current capacity so the
// peak charges base, the result slots, the payloads so far, and the live
// transient together.
func (acc *arrayBuildAccumulator) checkTransient(transient Value, backingCap int) error {
	if acc.exec.memoryQuota <= 0 {
		return nil
	}

	s := acc.exec.beginBaseWalk()
	defer s.close()
	if acc.receiver.Kind() != KindNil {
		s.est.value(acc.receiver)
	}
	for _, arg := range acc.args {
		s.est.value(arg)
	}
	for _, kwarg := range acc.kwargs {
		s.est.value(kwarg)
	}
	if !acc.block.IsNil() {
		s.est.value(acc.block)
	}

	transientBytes := s.est.value(transient)
	if used := saturatingAdd(acc.projected(backingCap), transientBytes); acc.exec.memoryExceeded(used) {
		return acc.exec.memoryQuotaExceededError()
	}
	return nil
}

// reserveSlots rejects the build up front when a backing slice of slotCount Value
// slots would already overflow the quota on top of the baseline (exec's reachable
// roots plus the call roots) and the payload accumulated so far. Builtins that can
// derive a large lower bound on the result length before emitting it — such as a
// range selector in Array#values_at expanding to a billion padded positions — use
// it to fail fast instead of charging the same slots one append at a time. It
// charges only the slot array, not per-element payloads (those are added by add as
// each element is appended), so it never rejects a result add would accept.
func (acc *arrayBuildAccumulator) reserveSlots(slotCount int) error {
	return acc.reserveSlotArrays(slotCount)
}

func (acc *arrayBuildAccumulator) checkRetainedPayloadBytes(slotCount, payloadBytes int) error {
	if acc.exec.memoryQuota <= 0 {
		return nil
	}
	used := saturatingAdd(acc.projected(slotCount), payloadBytes)
	if acc.exec.memoryExceeded(used) {
		return acc.exec.memoryQuotaExceededError()
	}
	return nil
}

// reserveSlotArrays rejects a build when several result arrays will be live
// together, such as Array#pop returning both the remaining and removed arrays.
// checkSlotReservationWithCallRoots rejects a slot-array reservation whose
// backing, on top of the reachable base and the caller's Go-frame roots, would
// overflow the quota. It prices exactly what a fresh build accumulator's
// reserveSlotArrays prices — base + call roots + slot backing — but resumes
// the memoized base walk when one is available instead of snapshotting a
// reference walk, so an amortized growth check inside a loop does not re-walk
// the whole graph at every capacity doubling (#1129).
func (exec *Execution) checkSlotReservationWithCallRoots(slotCount int, receiver Value, args []Value, kwargs map[string]Value, block Value) error {
	if exec.memoryQuota <= 0 {
		return nil
	}
	s := exec.beginBaseWalk()
	used := s.base
	if receiver.Kind() != KindNil {
		used = saturatingAdd(used, s.est.value(receiver))
	}
	for _, arg := range args {
		used = saturatingAdd(used, s.est.value(arg))
	}
	for _, kwarg := range kwargs {
		used = saturatingAdd(used, s.est.value(kwarg))
	}
	if !block.IsNil() {
		used = saturatingAdd(used, s.est.value(block))
	}
	s.close()
	used = saturatingAdd(used, arraySlotBackingBytes(slotCount))
	if exec.memoryExceeded(used) {
		return exec.memoryQuotaExceededError()
	}
	return nil
}

func (acc *arrayBuildAccumulator) reserveSlotArrays(slotCounts ...int) error {
	if acc.exec.memoryQuota <= 0 {
		return nil
	}
	return acc.checkSlotArrays(slotCounts...)
}

func (acc *arrayBuildAccumulator) checkSlotArrays(slotCounts ...int) error {
	if acc.exec.memoryQuota <= 0 {
		return nil
	}

	used := saturatingAdd(acc.base, acc.payload)
	for _, slotCount := range slotCounts {
		used = saturatingAdd(used, arraySlotBackingBytes(slotCount))
	}
	if acc.exec.memoryExceeded(used) {
		return acc.exec.memoryQuotaExceededError()
	}
	return nil
}

// projected returns the build's live footprint if its backing slice held slotCount
// Value slots: the baseline (exec's reachable roots plus the call roots), the slot
// array sized to slotCount, and the payloads accumulated so far. add and
// reserveSlots share it so the per-append and up-front checks charge the slot array
// identically.
func (acc *arrayBuildAccumulator) projected(slotCount int) int {
	return saturatingAdd(saturatingAdd(acc.base, arraySlotBackingBytes(slotCount)), acc.payload)
}

// arraySlotBackingBytes is what one new array of slotCount slots costs a
// projection standing on its own: the Value that names it, plus everything the
// array itself allocates.
func arraySlotBackingBytes(slotCount int) int {
	return saturatingAdd(estimatedValueBytes, nestedArrayBackingBytes(slotCount))
}

// nestedArrayBackingBytes is what one new array of slotCount slots costs when
// something else already prices the Value that names it -- an inner array in a
// row of tuples, a capture list inside a match element, a group inside a pair.
// Its Value occupies a slot of the enclosing backing, which that backing's own
// projection charges, so charging it again here would bill every inner array's
// Value twice.
//
// It is the wrapper remainder plus the slot backing. Pricing a wrapped array
// with valueSliceBackingBytes alone is the mistake this exists to make hard to
// write: the two differ by exactly what arrayData carries beyond a slice
// header, which is invisible at a call site and multiplies by the row count.
// Array#zip over a wide receiver allocated one such wrapper per row against a
// quota that had admitted none of them.
func nestedArrayBackingBytes(slotCount int) int {
	return saturatingAdd(estimatedArrayWrapperExtraBytes, valueSliceBackingBytes(slotCount))
}

// liveValueSliceBytes is what a slot array held only on a Go stack costs a
// projection: the backing, plus one Value beyond it.
//
// It is deliberately not arraySlotBackingBytes. Nothing wrapped this slice --
// a destructure's defensive snapshot is a bare append([]Value(nil), ...) that
// no array value ever boxes -- so there is no arrayData to charge, and adding
// one would reserve for a wrapper that is never allocated. The Value beyond the
// backing is the conservative margin this projection has always carried.
func liveValueSliceBytes(slotCount int) int {
	return saturatingAdd(estimatedValueBytes, valueSliceBackingBytes(slotCount))
}

// nestedArrayWrapperBytes is the wrapper cost alone for count arrays whose Value
// slots and slot backings some other reservation already covers -- a group whose
// Value is a hash entry and whose backing grew through the loop scratch, a
// partition side whose Value is a slot of the returned pair.
//
// It is the residue left when a projection prices an array in two pieces and
// neither piece is the wrapper. That is not a wrong formula and not a wrong
// value, so neither the spelling gate nor the compile-time assertion sees it;
// it is a charge that is simply absent, and naming it is what makes its absence
// legible at a call site.
func nestedArrayWrapperBytes(count int) int {
	return saturatingMul(count, estimatedArrayWrapperExtraBytes)
}

func valueSliceBackingBytes(slotCount int) int {
	return saturatingAdd(estimatedSliceBaseBytes, saturatingMul(slotCount, estimatedValueBytes))
}

func projectedAppendCap(length, capacity int) int {
	if length < capacity {
		return capacity
	}
	if capacity == 0 {
		return 1
	}
	next := saturatingMul(capacity, 2)
	if needed := length + 1; next < needed {
		return needed
	}
	return next
}

func (acc *arrayBuildAccumulator) retainedPayloadBytes() int {
	return acc.payload
}

// hashBuildAccumulator charges the memory of an output map assembled entry by
// entry against the quota without re-walking the whole map on each insertion.
// Hash transforms whose block returns fresh heap values (transform_values and
// transform_keys, where each block call can yield an arbitrarily large string or
// nested collection, and the merge conflict block) use it so accumulated
// payloads count toward the quota during construction, not only after the
// builtin returns.
//
// checkProjectedHashBytes alone is enough for blockless transforms whose values
// are references shared with the receiver, because there the output map's
// payloads are already resident and the projection only needs the new map's
// structural slots. It cannot bound a block that synthesizes new values: those
// live solely in the Go-local result map, unreachable from any execution root
// until the builtin returns, so many individually-under-quota results could
// accumulate well past the quota before the post-call check observes them.
//
// The accumulator charges block results conservatively. It keeps a results-only
// estimator that is NOT seeded with the build's base or call roots, so each
// inserted entry is charged its full current footprint as the estimator would
// measure it. Two results that share a backing are still deduplicated against
// each other (the estimator's seen-sets persist across add calls), but a result
// is never deduplicated against the base, so a block that mutates a
// receiver-owned container in place and returns it is charged at its full
// current size rather than dedup'd to nothing against the backing the baseline
// already saw. This can only over-count -- a block returning an unchanged value
// shared with the base or another result is counted again -- and so never
// under-counts the live result footprint, which keeps the sandbox bound sound by
// construction even under in-place mutation. The over-count is intentional and
// documented (see changelog.d/608 and docs/hashes.md); the array-side equivalent
// is tracked in #787.
//
// The output map is preallocated with make(map[string]Value, n), so its full
// n-slot backing -- and its empty-map overhead -- is live from the first block
// call, before any result has been charged. The caller therefore reserves that
// whole output map plus any sorted-key scratch through reserveLoopScratch
// (hashTransformBufferBytes) BEFORE building the accumulator, so the reservation is
// folded into the live baseline every check reads (estimateMemoryUsageBase),
// including the accumulator's own base measured here. add then charges each entry
// only the key/value PAYLOAD beyond the structural slot already reserved. Routing
// the reservation through reserveLoopScratch (rather than a private accumulator
// field) is what also lets the block-call runner's bind-charge baseline see the
// output map and scratch, so a rest-collecting destructure block cannot charge its
// fresh backing against a baseline that omits them.
//
// Each add is O(size of the inserted value) and the total is O(sum of inserted
// result sizes), not O(n^2): the estimator walks only the newly inserted value,
// never the accumulated prefix.
type hashBuildAccumulator struct {
	exec *Execution
	// est is a results-only estimator: it is never seeded with the base or call
	// roots, so it deduplicates backings shared across block results but charges a
	// result's full footprint even when it aliases a baseline container. base is the
	// live footprint snapshotted when the build started: exec's reachable roots, the
	// call roots, and the output map plus scratch the caller reserved through
	// reserveLoopScratch before the accumulator was built. built is the running byte
	// total charged for the per-entry key/value payloads as the output is assembled.
	est     *memoryEstimator
	baseEst *memoryEstimator
	base    int
	built   int

	// Call roots retained so checkTransient can re-seed a throwaway estimator
	// with the same baseline the build was snapshotted against, deduplicating a
	// transient block result against memory already charged in base.
	receiver Value
	args     []Value
	kwargs   map[string]Value
	block    Value
}

// newHashBuildAccumulator snapshots the execution's current live memory plus the
// transform's call roots as the baseline for an incremental hash build. Callers
// reserve the preallocated output map (its empty overhead plus every capacity slot)
// and any sorted-key scratch through reserveLoopScratch BEFORE building the
// accumulator, so those bytes are already folded into the live baseline this
// snapshot reads; add then charges only the per-entry payloads beyond the reserved
// structural slots. The accumulator's results-only estimator is a fresh estimator
// that is deliberately NOT seeded with the base or call roots: it deduplicates
// backings shared across block results but never against the baseline, so an
// in-place-mutated receiver container returned by a block is charged at its full
// current size rather than treated as already accounted.
func newHashBuildAccumulator(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) *hashBuildAccumulator {
	acc := &hashBuildAccumulator{
		exec:     exec,
		receiver: receiver,
		args:     args,
		kwargs:   kwargs,
		block:    block,
	}
	if exec.memoryQuota <= 0 {
		return acc
	}
	// Measure the baseline through a dedicated estimator so the results-only
	// estimator stays empty: the base must be counted, but the results estimator
	// must not dedup later block results against the call roots it walked. The
	// baseline estimator is retained for recursive transforms whose results are
	// runtime-built containers that may still share unchanged leaves with the
	// receiver. The baseline already includes the output map and scratch the caller
	// reserved via reserveLoopScratch (estimateMemoryUsageBase reads the
	// reservation), so the empty-map overhead is not folded in again here.
	acc.baseEst = newMemoryEstimator()
	acc.base = exec.estimateMemoryUsageBase(acc.baseEst)
	if receiver.Kind() != KindNil {
		acc.base = saturatingAdd(acc.base, acc.baseEst.value(receiver))
	}
	for _, arg := range args {
		acc.base = saturatingAdd(acc.base, acc.baseEst.value(arg))
	}
	for _, kwarg := range kwargs {
		acc.base = saturatingAdd(acc.base, acc.baseEst.value(kwarg))
	}
	if !block.IsNil() {
		acc.base = saturatingAdd(acc.base, acc.baseEst.value(block))
	}
	acc.est = newMemoryEstimator()
	return acc
}

type hashLiteralBuildAccumulator struct {
	exec *Execution
	// est is the snapshot mode's private estimator, seeded with a reference
	// walk at construction; nil in sessions mode.
	est *memoryEstimator
	// base holds the structural constants (wrapper, map base, backing slots)
	// in sessions mode, plus the construction-time reference walk in snapshot
	// mode.
	base     int
	retained int
	// sessions selects per-check base-walk sessions over the construction
	// snapshot: each check resumes the memoized base and measures only this
	// entry, so a literal built in a loop costs its own size rather than a
	// whole-graph walk (#1129). Snapshot mode remains for contexts where a
	// session cannot resume a memo (builtin depth) and would otherwise re-walk
	// the graph once per entry.
	sessions      bool
	replacing     bool
	keyPayloads   map[string]int
	valuePayloads map[string]int
}

type hashLiteralEntry struct {
	key   string
	value Value
}

// newHashLiteralBuildAccumulator snapshots the current execution roots for a
// hash literal. Literal values are plain expression results, not block callbacks
// that may mutate baseline containers in place, so an alias such as
// `big = ...; {a: big}` should be charged like the final hash: the new map
// structure and key bytes are fresh, while big's backing remains counted once.
//
// Sessions mode is taken at every literal width. It was once capped at 64 pairs
// because the union replay makes a k-pair literal do O(k²) estimator probes the
// step quota never saw, but the snapshot mode wide literals fell back to is the
// worse trade: it walks the whole reachable graph once per literal, and once a
// duplicate key switches it to replacement accounting, once per pair. Where the
// replay is bounded by the literal's own source width, that walk is bounded by
// whatever the host handed the script. Looping a 256-pair duplicate-key literal
// over a 10k-element host array did 51.7M estimator visits against 233k for a
// 32-pair one, on a step count that barely moved (#1). The replay is metered
// instead (see chargeEstimatorWalk), which is what the cap was standing in for.
func newHashLiteralBuildAccumulator(exec *Execution) (*hashLiteralBuildAccumulator, error) {
	acc := &hashLiteralBuildAccumulator{exec: exec}
	if exec.memoryQuota <= 0 {
		return acc, nil
	}

	acc.base = estimatedValueBytes + estimatedMapBaseBytes + estimatedHashDataBytes
	if exec.baseWalkSessionsAreCheap() {
		acc.sessions = true
		return acc, nil
	}
	acc.est = newMemoryEstimator()
	acc.base = saturatingAdd(exec.estimateMemoryUsageBase(acc.est), acc.base)
	if err := exec.chargeEstimatorWalk(acc.est.walked); err != nil {
		return nil, err
	}
	return acc, nil
}

func (acc *hashLiteralBuildAccumulator) reserveBacking(capacity int) error {
	if acc.exec.memoryQuota <= 0 {
		return nil
	}
	// The literal allocates its entry map and pre-sizes its insertion-order
	// backing to the pair count before the first pair runs, so both are fixed
	// structure rather than something an entry grows.
	acc.base = saturatingAdd(acc.base, saturatingMul(capacity, estimatedMapEntryStructuralBytes))
	acc.base = saturatingAdd(acc.base, hashOrderBackingBytes(capacity))
	// No entries exist yet, so the replay set the sessions path measures is
	// empty.
	return acc.checkQuota(nil)
}

// hashOrderBackingBytes is the heap footprint of the insertion-order backing a
// hash of capacity slots retains: the slice base plus one key Value per slot.
// The slots hold the key Values iteration hands out, so walking a hash boxes
// nothing per entry; their string payloads alias the entry map's own keys,
// which the entry accounting already charges.
func hashOrderBackingBytes(capacity int) int {
	if capacity <= 0 {
		return 0
	}
	return saturatingAdd(estimatedSliceBaseBytes, saturatingMul(capacity, estimatedValueBytes))
}

func (acc *hashLiteralBuildAccumulator) addDistinctEntry(current map[string]hashLiteralEntry, key string, val Value) error {
	if acc.exec.memoryQuota <= 0 {
		return nil
	}

	if acc.sessions {
		used, nodes := acc.sessionUsedBytes(current, func(est *memoryEstimator) int {
			return saturatingAdd(est.stringPayloadSize(key), est.valuePayload(val))
		})
		if acc.exec.memoryExceeded(used) {
			return acc.exec.memoryQuotaExceededError()
		}
		return acc.exec.chargeEstimatorWalk(nodes)
	}

	before := acc.est.walked
	acc.retained = saturatingAdd(acc.retained, acc.est.stringPayloadSize(key))
	acc.retained = saturatingAdd(acc.retained, acc.est.valuePayload(val))
	if err := acc.checkQuota(current); err != nil {
		return err
	}
	return acc.exec.chargeEstimatorWalk(acc.est.walked - before)
}

// replaceEntry switches duplicate-key literals to per-key retained accounting.
// Distinct-key literals stay on the seeded-estimator fast path; once a duplicate
// appears, retained charges must become subtractable so overwritten values stop
// contributing after the replacement.
func (acc *hashLiteralBuildAccumulator) replaceEntry(
	key string,
	val Value,
	current map[string]hashLiteralEntry,
) error {
	if acc.exec.memoryQuota <= 0 {
		return nil
	}
	if acc.sessions {
		// Sessions mode keeps its identity-based accounting through
		// replacements: the replay set becomes the literal's
		// post-replacement entries and every payload is re-measured against
		// the live base per check, so neither the replaced value nor a
		// payload the base has started counting is retained. A frozen
		// per-key byte total went stale as soon as a later value expression
		// published a shared payload into a root. Admission runs first,
		// against the PRE-replacement set plus the candidate: the old value
		// stays in the unpublished hash until the write lands, so the
		// transient peak holds both allocations.
		used, nodes := acc.sessionUsedBytes(current, func(est *memoryEstimator) int {
			return saturatingAdd(est.stringPayloadSize(key), est.valuePayload(val))
		})
		if acc.exec.memoryExceeded(used) {
			return acc.exec.memoryQuotaExceededError()
		}
		if err := acc.exec.chargeEstimatorWalk(nodes); err != nil {
			return err
		}
		// The post-replacement set never exceeds the admitted peak, so no
		// second check is needed here; the caller's write makes current that
		// set and later entries re-measure it anyway.
		acc.replacing = true
		return nil
	}
	nodes := 0
	if !acc.replacing {
		nodes = acc.rebuildRetainedEntries(current)
	}

	keyPayload, valuePayload, entryNodes := acc.entryPayloads(key, val)
	nodes += entryNodes
	incoming := saturatingAdd(keyPayload, valuePayload)
	base, baseNodes := acc.liveBase()
	nodes += baseNodes
	if used := saturatingAdd(saturatingAdd(base, acc.retained), incoming); acc.exec.memoryExceeded(used) {
		return acc.exec.memoryQuotaExceededError()
	}
	if err := acc.exec.chargeEstimatorWalk(nodes); err != nil {
		return err
	}

	prior := saturatingAdd(acc.keyPayloads[key], acc.valuePayloads[key])
	acc.retained -= prior
	if acc.retained < 0 {
		acc.retained = 0
	}
	acc.retained = saturatingAdd(acc.retained, incoming)
	if acc.keyPayloads == nil {
		acc.keyPayloads = make(map[string]int)
		acc.valuePayloads = make(map[string]int)
	}
	// keyPayloads and valuePayloads are the subtractable per-key totals a later
	// replacement of this same key retracts.
	acc.keyPayloads[key] = keyPayload
	acc.valuePayloads[key] = valuePayload
	return acc.checkQuota(current)
}

// rebuildRetainedEntries re-measures every entry retained so far and reports the
// graph nodes doing so walked, so the caller can charge for them: one walk per
// entry is the widest single burst of estimator work a literal can drive.
func (acc *hashLiteralBuildAccumulator) rebuildRetainedEntries(current map[string]hashLiteralEntry) int {
	acc.retained = 0
	acc.keyPayloads = make(map[string]int, len(current))
	acc.valuePayloads = make(map[string]int, len(current))
	nodes := 0
	for key, entry := range current {
		keyPayload, valuePayload, entryNodes := acc.entryPayloads(entry.key, entry.value)
		nodes += entryNodes
		acc.keyPayloads[key] = keyPayload
		acc.valuePayloads[key] = valuePayload
		acc.retained = saturatingAdd(acc.retained, saturatingAdd(keyPayload, valuePayload))
	}
	acc.est = nil
	acc.replacing = true
	return nodes
}

// entryPayloads measures one entry's key and value payloads and reports the
// graph nodes doing so walked.
func (acc *hashLiteralBuildAccumulator) entryPayloads(key string, val Value) (int, int, int) {
	if acc.sessions {
		s := acc.exec.beginBaseWalk()
		defer s.closeCharged()
		valuePayload := s.est.valuePayload(val)
		keyPayload := s.est.stringPayloadSize(key)
		return keyPayload, valuePayload, s.nodes()
	}
	est := newMemoryEstimator()
	acc.exec.estimateMemoryUsageBase(est)
	valuePayload := est.valuePayload(val)
	keyPayload := est.stringPayloadSize(key)
	return keyPayload, valuePayload, est.walked
}

// sessionUsedBytes measures the literal's retained set against the live base
// inside one session. The prior entries' payloads are re-measured rather than
// carried as a running total: each walk deduplicates against the current
// reachable graph and the earlier entries, so a payload that a later value
// expression published into a root, making the base start counting it, stops
// being counted in the retained side too, exactly as the reference walk's union
// dedup behaves. measure, when non-nil, prices the candidate entry against the
// same union. In sessions mode acc.retained holds only the arithmetic
// structural bytes.
//
// current is the builder's own canonical-key entry map, borrowed rather than
// mirrored into the accumulator. Keeping a second copy cost 128 bytes a pair
// that no quota projection could see, bounded only by the source while the
// width cap stood and unbounded once it went, plus a whole second copy while a
// replacement rebuilt it (#1). The replay set is order independent: the
// estimator's union total does not depend on which entry it reaches an identity
// through.
func (acc *hashLiteralBuildAccumulator) sessionUsedBytes(current map[string]hashLiteralEntry, measure func(est *memoryEstimator) int) (int, int) {
	s := acc.exec.beginBaseWalk()
	defer s.closeCharged()
	retained := 0
	for _, prior := range current {
		retained = saturatingAdd(retained, s.est.stringPayloadSize(prior.key))
		retained = saturatingAdd(retained, s.est.valuePayload(prior.value))
	}
	extra := 0
	if measure != nil {
		extra = measure(s.est)
	}
	used := saturatingAdd(saturatingAdd(s.base, acc.base), saturatingAdd(acc.retained, saturatingAdd(retained, extra)))
	return used, s.nodes()
}

// liveBase returns the reachable-graph base the next quota comparison should
// use: the memoized base in sessions mode (the construction snapshot in base
// already covers it otherwise), so checks track the graph as it actually is
// mid-literal rather than as it was at construction. It also reports the graph
// nodes the session it opened walked, which is zero in snapshot mode because
// the construction walk already paid for that base.
func (acc *hashLiteralBuildAccumulator) liveBase() (int, int) {
	if !acc.sessions {
		return acc.base, 0
	}
	s := acc.exec.beginBaseWalk()
	defer s.closeCharged()
	return saturatingAdd(s.base, acc.base), s.nodes()
}

// checkQuota measures the literal so far and charges the walk doing so drove.
// Sessions mode must not reach liveBase on the way: opening a session there and
// then discarding it for sessionUsedBytes wasted the whole-graph walk a memo
// miss pays AND hid it from the charge, because the discarded session left the
// memo warm so the second one reported only its cheap replay. A loop whose
// literals each invalidate the memo therefore re-walked an arbitrarily large
// host graph for a flat step count (#1).
func (acc *hashLiteralBuildAccumulator) checkQuota(current map[string]hashLiteralEntry) error {
	var used, nodes int
	if acc.sessions {
		used, nodes = acc.sessionUsedBytes(current, nil)
	} else {
		var base int
		base, nodes = acc.liveBase()
		used = saturatingAdd(base, acc.retained)
	}
	if acc.exec.memoryExceeded(used) {
		return acc.exec.memoryQuotaExceededError()
	}
	return acc.exec.chargeEstimatorWalk(nodes)
}

// reserveBacking folds the structural footprint of a preallocated output map
// into the baseline so the whole backing is held against the quota from the
// first block call, and rejects the build if the reservation alone already
// overflows. Use it for hash builds that have not already reserved their output
// map through reserveLoopScratch. Hash transforms reserve output and scratch
// before constructing the accumulator so the block bind-charge baseline can see
// those Go-local buffers; array.to_h's block form has no transform scratch and
// reserves its output map here instead.
//
// capacity is the slot count passed to make. The key and value PAYLOADS are
// charged incrementally by add/addSynthesizedKey, which after this reservation
// add only the payload beyond the structural slot already counted, so nothing is
// double counted: base ends at call roots + empty map + capacity*slot, and built
// ends at the sum of per-entry payloads.
func (acc *hashBuildAccumulator) reserveBacking(capacity int) error {
	if acc.exec.memoryQuota <= 0 {
		return nil
	}
	acc.base = saturatingAdd(acc.base, estimatedValueBytes+estimatedMapBaseBytes+estimatedHashDataBytes)
	acc.base = saturatingAdd(acc.base, saturatingMul(capacity, estimatedMapEntryStructuralBytes))
	acc.base = saturatingAdd(acc.base, hashOrderBackingBytes(capacity))
	return acc.checkQuota()
}

// add charges a write whose VALUE is a block result to the output map and rejects
// the build if the projected map memory exceeds the quota. Use it where the block
// produces the VALUE (transform_values, the merge conflict block) and the key is a
// receiver or argument key kept unchanged; transform_keys, whose block produces
// the KEY while the value stays a receiver value, uses addSynthesizedKey instead.
//
// The entry's structural slot (map bucket, key header, value slot) is already
// held in the baseline by the caller's reserveLoopScratch reservation, so add
// charges only PAYLOADS beyond it.
// The key is a receiver/argument key whose payload is already counted in the
// baseline via the call roots, so it contributes nothing further. Only the
// block-returned value's payload goes through the results-only estimator, so a
// backing shared across two block results is counted once but a result that
// aliases a baseline container is counted at full size rather than deduplicated to
// nothing. Routing the key through the estimator would record its backing in the
// seen-set and risk a later block result that aliases it being dedup'd away.
//
// Charging per write (rather than per distinct key) means a key overwritten by a
// later write -- a merge conflict block folding several colliding arguments -- is
// counted once per occurrence. That is a conservative over-count of the final
// map's footprint, the intentional tradeoff that makes the bound sound under
// in-place mutation: built only ever grows, so it can never drop below the live
// footprint and let a later insert materialize past the quota.
func (acc *hashBuildAccumulator) add(val Value) error {
	if acc.exec.memoryQuota <= 0 {
		return nil
	}

	acc.built = saturatingAdd(acc.built, acc.est.valuePayload(val))
	return acc.checkQuota()
}

// addBaselineDeduped charges a runtime-built value while deduplicating unchanged
// leaves already reachable from the transform's call roots. Use it for recursive
// transforms such as deep_transform_keys, where the runtime synthesizes fresh
// container structure but carries original leaf values through unchanged. Do not
// use it for arbitrary block results: a block can mutate a baseline container in
// place and return it, and those results need add's conservative unseeded walk.
func (acc *hashBuildAccumulator) addBaselineDeduped(val Value) error {
	if acc.exec.memoryQuota <= 0 {
		return nil
	}

	acc.built = saturatingAdd(acc.built, acc.baseEst.valuePayload(val))
	return acc.checkQuota()
}

// addSynthesizedKey charges a key write whose KEY is a fresh block-synthesized
// string but whose VALUE is a receiver value already counted in the baseline
// (transform_keys yields a new key per entry while keeping the original value).
// The entry's structural slot (map bucket, key header, value slot) is already held
// in the baseline by the caller's reserveLoopScratch reservation, and the value's
// reachable payload is already folded into acc.base via the call roots, so the only
// fresh allocation to charge is the synthesized key's PAYLOAD beyond its header. It
// goes through the
// results-only estimator so a key string shared across block results is counted
// once; the value never goes through the estimator, since routing it would record
// its backing in the seen-set and let a later block result aliasing it be dedup'd
// to nothing -- the exact under-count the results-only estimator exists to
// prevent. The synthesized key payload is the only unbounded fresh allocation the
// up-front structural projection cannot see, so charging it incrementally keeps
// the build within the quota during the loop.
func (acc *hashBuildAccumulator) addSynthesizedKey(key string) error {
	if acc.exec.memoryQuota <= 0 {
		return nil
	}

	acc.built = saturatingAdd(acc.built, acc.est.stringPayloadSize(key))
	return acc.checkQuota()
}

// checkTransient rejects the build when a freshly allocated value returned by a
// block, but not retained as part of the output map, would push the peak
// footprint over the quota. Array#to_h's block form uses it for the temporary
// two-element pair returned by each block call: the output map backing is already
// live and reserved in base, while the pair array itself is only live until its
// key and value are extracted.
func (acc *hashBuildAccumulator) checkTransient(transient Value) error {
	if acc.exec.memoryQuota <= 0 {
		return nil
	}

	s := acc.exec.beginBaseWalk()
	defer s.close()
	if acc.receiver.Kind() != KindNil {
		s.est.value(acc.receiver)
	}
	for _, arg := range acc.args {
		s.est.value(arg)
	}
	for _, kwarg := range acc.kwargs {
		s.est.value(kwarg)
	}
	if !acc.block.IsNil() {
		s.est.value(acc.block)
	}

	used := saturatingAdd(saturatingAdd(acc.base, acc.built), s.est.value(transient))
	if acc.exec.memoryExceeded(used) {
		return acc.exec.memoryQuotaExceededError()
	}
	return nil
}

// checkQuota rejects the build when the live baseline plus the accumulated output
// exceeds the quota.
// reserveScratch folds a fixed scratch allocation into the baseline so it is held
// against the quota for the build's entire lifetime, and rejects the build if the
// reservation alone already overflows. Builders that keep a Go-local scratch
// buffer live while the result accumulates (String#scan holds the engine's whole
// [][]int match table the entire time it materializes per-match result elements
// from it) reserve that buffer here so its bytes coexist with every accumulated
// element at peak. Without the reservation a build could keep both the scratch and
// the growing result live and exceed the quota by the scratch size before the
// per-element check observed it. scratchBytes is the heap footprint of that live
// buffer.
func (acc *arrayBuildAccumulator) reserveScratch(scratchBytes int) error {
	if acc.exec.memoryQuota <= 0 {
		return nil
	}
	acc.base = saturatingAdd(acc.base, scratchBytes)
	if acc.exec.memoryExceeded(acc.base) {
		return acc.exec.memoryQuotaExceededError()
	}
	return nil
}

func (acc *hashBuildAccumulator) checkQuota() error {
	used := saturatingAdd(acc.base, acc.built)
	if acc.exec.memoryExceeded(used) {
		return acc.exec.memoryQuotaExceededError()
	}
	return nil
}

func (acc *hashBuildAccumulator) retainedPayloadBytes() int {
	return acc.built
}

// blockBindCharge charges the fresh memory a block's destructuring parameters
// allocate when they are bound. A destructuring parameter that collects a rest
// (|(k, *tail)|, or the nested |(k, (head, *tail))|) makes AssignDestructure copy
// the collected window into a fresh backing slice (make+copy) before binding it.
// That copy is sized to the SOURCE, not to anything the iterator preflighted: over
// a hash whose values are arrays, |(k, (head, *tail))| binds tail to a fresh copy
// of the whole value array, a backing the per-entry [key, value] pair reservation
// does not bound. With an empty (or trivial) block body the body's own memory
// checks never observe that backing, and the iterator's receiver lives only on the
// Go call stack (invisible to estimateMemoryUsageBase), so the fresh copy could
// escape the sandbox quota entirely. This charge closes that gap in the one place
// every block iterator shares (the param-binding path), so it covers Hash#each,
// array.each/map/select, reduce, and any other block call alike.
//
// rootEst is a PERSISTENT estimator seeded ONCE (at construction) with the live call
// roots: exec's reachable roots (the env stack and exec.root) plus the receiver,
// kwargs, callArgs, and block. It is the same single walk estimateMemoryUsageForCallRoots
// performs, so the receiver deduplicates against the env (a named local passed to
// arr.each is counted once, not twice) and baseline records that one walk's total.
// Crucially it is NOT re-walked per entry: re-walking the receiver or the env on
// every iteration is the O(n^2) trap that previously dominated CI. The env held in
// rootEst stays live for the whole loop (the iterator runs mid-expression), so the
// committed backings are never freed and a later probe cannot falsely deduplicate
// against a reused address.
//
// Per-entry growth in outer scope is still caught by the body's own per-statement
// checks, which walk the live env (the bound rest included) on each statement.
// Those walks cannot see Go-frame-only roots, so callBlock reserves
// ephemeralRootBytes into the live baseline while the body runs: the snapshot
// baseline bounds the bind-time peak (receiver plus fresh rest) and the body
// checks, reading env plus the reservation, bound the combined live footprint
// (receiver plus whatever the body retains) -- closing the ~2x transient a
// retained accumulator of rest copies could otherwise reach (issue #835).
//
// Each call resets a SEPARATE per-call estimator (est) and seeds it with that call's
// arguments (the [key, value] pair for Hash#each, the destructured element for array
// iterators, the (acc, item) pair for reduce), so a fresh rest backing whose ELEMENTS
// alias the yielded data deduplicates those payloads to zero and only the backing's
// genuinely new slots are charged. est is reset every call so its seen-set never
// grows across a long loop, keeping the charge O(the data bound this entry).
type blockBindCharge struct {
	exec     *Execution
	est      *memoryEstimator
	rootEst  *memoryEstimator
	baseline int
	built    int

	// ephemeralRootBytes is the marginal footprint of the call roots that live only
	// on the iterator's Go frame: what the receiver, callArgs, kwargs, and block
	// added to the baseline beyond exec's reachable roots. While a block body runs,
	// callBlock folds these bytes into the live baseline via reserveLoopScratch so
	// the body's own checks -- per-statement walks and mutator growth preflights,
	// which cannot see Go-frame values -- bound the COMBINED peak of the ephemeral
	// receiver plus whatever the body accumulates. Without it, the receiver is
	// counted only in this charge's one-time snapshot while the accumulator is
	// counted only by the body checks, and each view fits a quota the combined live
	// footprint exceeds by up to 2x (issue #835). A receiver that is reachable from
	// the environment deduplicated against the base walk at construction, so its
	// marginal here is ~0 and env-rooted iteration is unaffected.
	ephemeralRootBytes int

	// reservedAtStart is exec.reservedScratchBytes when the charge was built,
	// and selfReserved is what callBlock reserved for this call's ephemeral
	// roots. liveBaseline adds the growth between them so a driver that raises
	// its reservation while iterating -- an incremental result backing, say --
	// is weighed by every later bind charge, not just by the ones built after
	// it. Subtracting selfReserved keeps the ephemeral roots, which baseline
	// already carries, from being counted twice.
	reservedAtStart int
	selfReserved    int
	// retainedAtStart is the registered outputs' marginal over the reachable graph
	// when the charge was built, which baseline already carries. liveBaseline adds
	// only the growth beyond it, the same way it treats the scratch reservation: a
	// charge built inside an outer driver's callback would otherwise count that
	// driver's retained results twice. Both readings are on the marginal basis --
	// this one from the walk above, the later ones from
	// retainedOutputMarginalBytes -- because subtracting one basis from the other
	// silently zeroed the growth (see memory_output.go).
	retainedAtStart int
}

// liveBaseline is the construction-time baseline plus everything the driver has
// accumulated since, which is what the call is really being weighed against:
// scratch it has reserved, and results it has retained into a registered output
// root (see memory_output.go). Both were the same quantity while the drivers
// reserved their results as scratch; a registered output does not move the
// reservation counter, so a charge reading only that counter weighed a rest
// window against a baseline missing every result the loop had kept.
//
// Both are counted as growth beyond what the snapshot already carried, because
// this charge can be built inside another driver's callback, where the baseline
// already includes that driver's retained results.
func (c *blockBindCharge) liveBaseline() int {
	baseline := c.baseline
	if growth := c.exec.reservedScratchBytes - c.reservedAtStart - c.selfReserved; growth > 0 {
		baseline = saturatingAdd(baseline, growth)
	}
	if growth := c.exec.retainedOutputMarginalBytes() - c.retainedAtStart; growth > 0 {
		baseline = saturatingAdd(baseline, growth)
	}
	return baseline
}

// noteSelfReservation records the scratch callBlock reserved for this call's
// ephemeral roots, which the baseline already includes.
func (c *blockBindCharge) noteSelfReservation(bytes int) {
	if c == nil {
		return
	}
	c.selfReserved = bytes
}

// newBlockBindCharge snapshots the live call roots as the baseline for charging a
// block's destructured bindings, or returns nil when no charge is needed: either no
// memory quota is enforced or the block has no rest-collecting destructure parameter
// (the only binding shape AssignDestructure materializes into a fresh, source-sized
// backing slice). A plain or non-rest destructure parameter binds references already
// counted in the call roots, so it allocates nothing fresh and needs no charge.
//
// callArgs are the iterator's POSITIONAL call roots (the other hashes a block-driven
// hash.merge folds in, a grep pattern, the host arguments a capability CallBlock
// drives a block with). Like the receiver they live only on the Go call stack during
// the loop, invisible to estimateMemoryUsageBase, so they are walked into the
// persistent rootEst here: charging only the fresh rest copy against a baseline that
// omits them lets a quota fit (roots + rest) and (receiver + rest) separately while
// the real peak (receiver + args + rest) exceeds it. They are the FIXED backings held
// for the whole loop; the per-entry yielded values and the per-call charged roots are
// handled by begin instead.
func newBlockBindCharge(exec *Execution, blk *Block, receiver Value, callArgs []Value, kwargs map[string]Value, block Value) *blockBindCharge {
	if exec.memoryQuota <= 0 || !blockBindsRest(blk) {
		return nil
	}
	rootEst := newMemoryEstimator()
	// estimateMemoryUsageBase's parts, inlined so the registered outputs' share of
	// it can be kept: liveBaseline prices their growth against this start value, and
	// taking it from this walk puts both on the marginal basis for free. Asking
	// retainedOutputMarginalBytes instead would fall back to a second graph walk
	// here, because a nested driver has just invalidated the memo by registering.
	base := exec.estimateScalarBase()
	base = saturatingAdd(base, exec.estimateGraphBase(rootEst))
	// Metered for the same reason the retained-output fallback's basis walk is,
	// and only while a driver output is registered: that is exactly when this
	// construction is script-repeatable, because a lookup builds its runner inside
	// its own loop. A driver with no registered output -- every rest-binding block
	// driver outside the hash lookups -- has nothing that would drain the counter,
	// so charging it here would leave the nodes to be billed to whichever lookup
	// ran next (see chargeRetainedOutputWalk).
	if len(exec.outputWalkRoots) > 0 {
		exec.outputWalkNodes += rootEst.walked
	}
	retained := exec.outputWalkBytes(rootEst)
	base = saturatingAdd(base, retained)
	baseline := base
	if receiver.Kind() != KindNil {
		baseline = saturatingAdd(baseline, rootEst.value(receiver))
	}
	for _, arg := range callArgs {
		baseline = saturatingAdd(baseline, rootEst.value(arg))
	}
	for _, kwarg := range kwargs {
		baseline = saturatingAdd(baseline, rootEst.value(kwarg))
	}
	if !block.IsNil() {
		baseline = saturatingAdd(baseline, rootEst.value(block))
	}
	return &blockBindCharge{
		exec:               exec,
		est:                newMemoryEstimator(),
		rootEst:            rootEst,
		baseline:           baseline,
		ephemeralRootBytes: baseline - base,
		reservedAtStart:    exec.reservedScratchBytes,
		retainedAtStart:    retained,
	}
}

// ephemeralRootScratch returns the bytes callBlock reserves into the live
// baseline while the block body runs: the marginal footprint of the Go-frame-only
// call roots the body's own checks cannot see. Zero for a nil charge and for
// iteration over environment-reachable roots, whose marginal deduplicated to ~0
// against the base walk at construction.
func (c *blockBindCharge) ephemeralRootScratch() int {
	if c == nil {
		return 0
	}
	return c.ephemeralRootBytes
}

// begin prepares the charge for one block call: it resets the per-call estimator and
// seeds it with the call's arguments so any payload a freshly bound rest backing
// shares with them (the receiver's own data, reached through the yielded pair or
// element) deduplicates to zero. Only the backing's new slots remain to be charged.
// Seeding walks just this call's arguments, never the whole receiver, so it stays
// O(the data this entry yields).
//
// chargedRoots are per-call values that live only in the iterator's Go frame and
// evolve every call, so they cannot be folded into the one-time baseline -- the
// reduce accumulator (acc_0 is the seed, acc_n is the previous call's block result).
// Each is PROBED against the persistent rootEst, which already holds the receiver, so
// a no-seed accumulator that is the receiver's first element deduplicates to its
// structural slots only and is NOT charged a second copy of the receiver's data --
// the double-charge this guards against. The probe walks only the charged root
// (bounded by its size) and rolls back, so the rootEst is not permanently grown and
// the next call's accumulator cannot falsely deduplicate against this one's freed
// backing. begin returns the quota error if the charged roots alone exceed the
// budget, so a reduce whose live peak is receiver + accumulator is rejected before
// the block body runs. The charged roots are also seeded into the per-call estimator
// so a rest backing that copies part of the accumulator deduplicates against it.
func (c *blockBindCharge) begin(args []Value, chargedRoots ...Value) error {
	if c == nil {
		return nil
	}
	c.est.reset()
	c.built = 0
	for _, root := range chargedRoots {
		c.built = saturatingAdd(c.built, c.rootEst.probe(root))
		if c.exec.memoryExceeded(saturatingAdd(c.liveBaseline(), c.built)) {
			return c.exec.memoryQuotaExceededError()
		}
		c.est.value(root)
	}
	for _, arg := range args {
		c.est.value(arg)
	}
	return nil
}

// charge adds a freshly bound leaf value to the running estimate and rejects the
// call when the live baseline plus every value bound so far this call exceeds the
// quota. The estimator returns each leaf's marginal footprint -- a value that
// aliases the seeded arguments contributes only its structural slots, since its
// payload deduplicates against the seed -- so a rest backing is charged its real
// fresh footprint while a pass-through binding charges essentially nothing.
func (c *blockBindCharge) charge(value Value) error {
	if c == nil {
		return nil
	}
	c.built = saturatingAdd(c.built, c.est.value(value))
	if c.exec.memoryExceeded(saturatingAdd(c.liveBaseline(), c.built)) {
		return c.exec.memoryQuotaExceededError()
	}
	return nil
}

// projectRestWindow rejects a destructure rest backing of count Value slots
// BEFORE assignDestructure allocates it. assignDestructure copies the collected
// window into a fresh make([]Value, count) backing before binding it; without this
// preflight a quota smaller than one such backing would let the copy materialize
// (a single huge tail, |(head, *tail)| over a [[huge...]], with an empty block
// body whose own checks never run) and the post-bind charge would only observe the
// over-budget array after it already escaped. Projecting the window's structural
// footprint -- a slice header plus count Value slots, the same shape the estimator
// charges a fresh slice backing -- on top of this call's baseline and everything
// already bound this call mirrors the check-before-materialize discipline the
// standalone assignDestructure path uses (checkProjectedIntArrayBytesWithLive).
//
// The window's elements alias the yielded value (already seeded into the per-call
// estimator and counted in the baseline through the receiver and call roots), so
// only the backing's genuinely new slots are projected, not a second copy of the
// element payloads. The post-bind charge still runs on the bound array afterward to
// account for its real, dedup-aware footprint; this is the pre-allocation gate.
func (c *blockBindCharge) projectRestWindow(count int) error {
	if c == nil {
		return nil
	}
	window := arraySlotBackingBytes(count)
	if c.exec.memoryExceeded(saturatingAdd(saturatingAdd(c.liveBaseline(), c.built), window)) {
		return c.exec.memoryQuotaExceededError()
	}
	return nil
}

// destructureCharge returns the rest-window preflight assignDestructure consults
// before allocating a named rest's backing slice. A nil charge (no memory quota or
// a block with no rest-collecting parameter) admits every window, matching the host
// AssignDestructure path that runs outside a quota. The liveSlots and liveRoot
// arguments the destructurer threads are unused here: block parameters bind only
// identifiers and nested destructures (never an index or member write), so the
// snapshot path that produces a live Go-stack slot array never runs during block
// binding -- the only off-baseline memory is the rest backing this preflight gates.
//
// The step charge comes from exec rather than from c because the two quotas are
// independent: newBlockBindCharge returns nil whenever memory is unlimited or
// the block binds no rest, so taking the CPU metering from c left a block's
// target walk and window copies free under the memory-unlimited, steps-finite
// configuration the CLI runs by default (#49).
func (c *blockBindCharge) destructureCharge(exec *Execution) destructureCharge {
	charge := destructureCharge{check: noopDestructureCheck, step: exec.chargeDestructureScan}
	if c != nil {
		charge.check = func(count, _ int, _ Value) error {
			return c.projectRestWindow(count)
		}
	}
	return charge
}

// blockBindsRest reports whether any of the block's parameters destructure a value
// and collect a named rest, the only binding shape AssignDestructure materializes
// into a fresh, source-sized backing slice. Used to skip the per-call bind charge
// for the common parameter shapes that allocate nothing fresh.
func blockBindsRest(blk *Block) bool {
	for i := range blk.Params {
		if targetCollectsRest(blk.Params[i].Target) {
			return true
		}
	}
	return false
}

// targetCollectsRest reports whether a destructure target collects a rest into a
// fresh backing slice. An anonymous rest (a bare "*", whose element Target is nil)
// is skipped here: assignDestructure discards its window without materializing an
// array, so charging its bindings would seed the estimator with the whole yielded
// value for a backing that never exists -- regressing the |(head, *)| fast path
// over large nested rows. Only a rest element with a non-nil target, or a nested
// destructure that itself collects one, allocates the slice this charge gates.
func targetCollectsRest(target Expression) bool {
	destructure, ok := target.(*DestructureTarget)
	if !ok {
		return false
	}
	for _, element := range destructure.Elements {
		if element.Rest && element.Target != nil {
			return true
		}
		if targetCollectsRest(element.Target) {
			return true
		}
	}
	return false
}

// hashCallRootBytes estimates the live footprint a hash transform holds before it
// reserves any output: exec's reachable roots plus the call roots (receiver,
// args, kwargs, block). It excludes the output map's overhead so callers that
// build no derived map (the pure iterators) are not charged a map they never
// allocate; callers that do build one fold the empty-map overhead in themselves.
func (exec *Execution) hashCallRootBytes(receiver Value, args []Value, kwargs map[string]Value, block Value) int {
	used, _ := exec.hashCallRootUsageWithBilling(receiver, args, kwargs, block, false)
	return used
}

func (exec *Execution) hashCallRootUsage(receiver Value, args []Value, kwargs map[string]Value, block Value) (int, int) {
	return exec.hashCallRootUsageWithBilling(receiver, args, kwargs, block, true)
}

func (exec *Execution) hashCallRootUsageWithBilling(receiver Value, args []Value, kwargs map[string]Value, block Value, billed bool) (int, int) {
	s := exec.beginBaseWalk()
	used := s.base
	if receiver.Kind() != KindNil {
		used = saturatingAdd(used, s.est.value(receiver))
	}
	for _, arg := range args {
		used = saturatingAdd(used, s.est.value(arg))
	}
	for _, kwarg := range kwargs {
		used = saturatingAdd(used, s.est.value(kwarg))
	}
	if !block.IsNil() {
		used = saturatingAdd(used, s.est.value(block))
	}
	walked := s.nodes()
	s.walkBilled = billed
	s.close()
	return used, walked
}

// projectedHashBaseBytes estimates the live footprint a hash transform holds
// before it reserves its output map: the call-root usage plus the empty hash's
// structural overhead (the empty map plus the hashData wrapper every KindHash
// allocates). maxProjectedHashEntries builds on it so the entry budget and the
// byte check agree on the same baseline. Callers that build no output map (the
// pure iterators) use hashCallRootBytes directly instead.
func (exec *Execution) projectedHashBaseBytes(receiver Value, args []Value, kwargs map[string]Value, block Value) int {
	return saturatingAdd(exec.hashCallRootBytes(receiver, args, kwargs, block), estimatedValueBytes+estimatedMapBaseBytes+estimatedHashDataBytes)
}

// maxProjectedHashEntries returns the largest output-map entry count that
// checkProjectedHashTransformBytes would still accept for the given call roots
// and scratch budget, or math.MaxInt when no memory quota is enforced. Counting
// helpers that must deduplicate keys across inputs (such as the merge union
// count) use it to cap their tracking set at the quota-derived budget: once the
// distinct-key total passes this ceiling the transform is certain to be
// rejected, so they can stop allocating and report an over-budget count instead
// of building a tracking table sized to the rejected result.
//
// scratchBytes is the heap footprint of any sorted-key scratch buffers the
// transform materializes alongside its output (see sortedHashEntryBufferBytes). It is
// subtracted from the byte budget before deriving the entry cap so this ceiling
// agrees with the final checkProjectedHashTransformBytes, which charges the same
// scratch: without it the cap would admit entries the projection's actual budget
// (quota minus base minus scratch) cannot, letting a doomed merge grow its
// dedup set past the bytes the quota allows.
func (exec *Execution) maxProjectedHashEntries(scratchBytes int, receiver Value, args []Value, kwargs map[string]Value, block Value) int {
	if exec.memoryQuota <= 0 {
		return math.MaxInt
	}
	// The budget a refusal would use, since the dedup set is grown before any
	// check sees it.
	budget := exec.memoryBudgetBytes()
	used := saturatingAdd(exec.projectedHashBaseBytes(receiver, args, kwargs, block), scratchBytes)
	used = saturatingAdd(used, estimatedSliceBaseBytes)
	if used >= budget {
		return 0
	}
	// Each admitted entry costs a map slot and an insertion-order slot.
	return (budget - used) / (estimatedMapEntryStructuralBytes + estimatedValueBytes)
}

func (exec *Execution) estimateMemoryUsage(extras ...Value) int {
	s := exec.beginBaseWalk()

	total := s.base
	for _, extra := range extras {
		total += s.est.value(extra)
	}

	s.close()
	return total
}

func (exec *Execution) estimateMemoryUsageForCallRoots(callee, receiver Value, args []Value, kwargs map[string]Value, block Value) int {
	s := exec.beginBaseWalk()

	total := s.base

	if callee.Kind() != KindNil {
		total += s.est.value(callee)
	}
	if receiver.Kind() != KindNil {
		total += s.est.value(receiver)
	}
	for _, arg := range args {
		total += s.est.value(arg)
	}
	for _, kwarg := range kwargs {
		total += s.est.value(kwarg)
	}
	if !block.IsNil() {
		total += s.est.value(block)
	}

	s.close()
	return total
}

func (exec *Execution) estimateMemoryUsageForPositionalCallRoots(callee, receiver, arg0, arg1 Value, argCount int, block Value) int {
	s := exec.beginBaseWalk()

	total := s.base

	if callee.Kind() != KindNil {
		total += s.est.value(callee)
	}
	if receiver.Kind() != KindNil {
		total += s.est.value(receiver)
	}
	if argCount > 0 {
		total += s.est.value(arg0)
	}
	if argCount > 1 {
		total += s.est.value(arg1)
	}
	if !block.IsNil() {
		total += s.est.value(block)
	}

	s.close()
	return total
}

// reserveLoopScratch folds a Go-local scratch allocation into the live memory
// baseline so it is charged by every memory check for as long as it is held,
// then returns the bytes actually reserved so releaseLoopScratch can subtract
// exactly that amount. A hash-walking loop (the `for k, v in hash` statement and
// Hash#each / each_key / each_value) materializes a sorted-key scratch slice that
// stays live on the Go stack while its body runs. The body executes arbitrary
// code with its own memory checks, but those checks measure only exec's reachable
// roots and so never see this scratch slice; without reserving it, a body that
// allocates near the quota could pass its own checks while the true peak (roots +
// scratch + body allocation) exceeds the quota by the scratch size. Folding the
// scratch into estimateMemoryUsageBase for the loop's duration makes every check
// inside the body account for it.
//
// The return value is the delta applied, which equals scratchBytes unless the
// reservation saturates at math.MaxInt; releaseLoopScratch must be passed that
// delta so nested reservations stay perfectly balanced even under saturation.
func (exec *Execution) reserveLoopScratch(scratchBytes int) int {
	reserved := saturatingAdd(exec.reservedScratchBytes, scratchBytes)
	delta := reserved - exec.reservedScratchBytes
	exec.reservedScratchBytes = reserved
	return delta
}

// releaseLoopScratch returns reserved scratch bytes to the baseline once the loop
// that held them has finished. delta is the value reserveLoopScratch returned.
func (exec *Execution) releaseLoopScratch(delta int) {
	exec.reservedScratchBytes -= delta
}

type loopScratchReservation struct {
	exec     *Execution
	baseline int
	delta    int
}

func newLoopScratchReservation(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (loopScratchReservation, error) {
	reservation := loopScratchReservation{exec: exec}
	if exec.memoryQuota <= 0 {
		return reservation, nil
	}
	reservation.baseline = exec.hashCallRootBytes(receiver, args, kwargs, block)
	if exec.memoryExceeded(reservation.baseline) {
		return loopScratchReservation{}, exec.memoryQuotaExceededError()
	}
	return reservation, nil
}

// reserveIfFits charges scratchBytes against this execution and reports whether
// the reservation may stand.
//
// It compares against the budget, which is the same figure a refusal would use,
// so a reservation cannot be accepted against room a check would not grant.
func (r *loopScratchReservation) reserveIfFits(scratchBytes int) bool {
	if r.exec.memoryQuota <= 0 || scratchBytes <= 0 {
		return true
	}

	delta := r.exec.reserveLoopScratch(scratchBytes)
	nextDelta := saturatingAdd(r.delta, delta)
	if saturatingAdd(r.baseline, nextDelta) > r.exec.memoryBudgetBytes() {
		r.exec.releaseLoopScratch(delta)
		return false
	}
	r.delta = nextDelta
	return true
}

func (r *loopScratchReservation) reserve(scratchBytes int) error {
	if r.reserveIfFits(scratchBytes) {
		return nil
	}
	return r.exec.memoryQuotaExceededError()
}

func (r *loopScratchReservation) release() {
	if r == nil || r.delta == 0 {
		return
	}
	r.exec.releaseLoopScratch(r.delta)
	r.delta = 0
}

// estimateMemoryUsageBase is the live base a snapshot is taken against: the
// scalar state, the reachable graph, and the Go-local outputs registered as walk
// roots (see memory_output.go). The roots belong here for the same reason the
// reserved scratch does -- they are live bytes a builtin is holding -- and
// putting them here rather than at each snapshot site means an accumulator built
// inside a driver's callback sees what that driver has already retained, which
// is what the drivers used to get for free while they reserved their results as
// scratch.
//
// The one rule a caller must respect: this is a SNAPSHOT, so a builder that also
// tracks its own retained output incrementally must take it before registering
// that output, or price only the growth since. Otherwise its own results are
// counted twice, once in the snapshot and once in its running total. Every
// driver takes its snapshot while its output is still empty; blockBindCharge,
// which re-reads the roots as its loop proceeds, subtracts what the snapshot
// already carried (see liveBaseline).
//
// The per-check walks do not come through here: they compose the same parts
// themselves in beginBaseWalk, where the roots are memoized rather than
// re-walked.
func (exec *Execution) estimateMemoryUsageBase(est *memoryEstimator) int {
	total := exec.estimateScalarBase()
	total += exec.estimateGraphBase(est)
	return saturatingAdd(total, exec.outputWalkBytes(est))
}

// estimateGraphBase is the reference walk: the root, every env-stack frame, and
// the graph tail, with no dormant-frame optimization. It is used by the one-shot
// and persistent-estimator base computations (the accumulator and charged-root
// probes), which retain their estimator across env-stack changes and so must see
// every frame committed in their seen-set, and by estimateGraphBaseFast as the
// oracle's reference. The per-check memoized and bypass walks use the faster
// estimateGraphBaseFast instead.
func (exec *Execution) estimateGraphBase(est *memoryEstimator) int {
	total := est.env(exec.root)
	for _, env := range exec.envStack {
		total += est.env(env)
	}
	total += exec.estimateGraphTail(est)
	return total
}

// estimateGraphBaseFast is the dormant-optimized base walk: it charges the
// committed dormant prefix from a running byte sum and walks only the root, the
// active suffix, and the tail (see memory_dormant.go). It is sound only for a
// fresh-each-check computation that never retains the estimator across stack
// changes — the per-check memoized and bypass walks. Under the oracle it recomputes
// the reference and panics on any divergence.
func (exec *Execution) estimateGraphBaseFast(est *memoryEstimator) int {
	total := est.env(exec.root)
	total += exec.envStackGraphBytes(est)
	total += exec.estimateGraphTail(est)
	if estimatorVerify {
		// The oracle compares this fast walk against a second, sequential reference
		// walk, so it is meaningful only while the graph is stable between them.
		// The caller (the memoized base walk) guarantees that: an Execution runs on
		// one goroutine, so nothing can mutate the tail between the two walks.
		refEst := newMemoryEstimator()
		if ref := exec.estimateGraphBase(refEst); ref != total {
			panic(fmt.Sprintf(
				"vibescript: dormant-frame estimator mismatch: fast=%d reference=%d "+
					"(stackDepth=%d dormantSlots=%d dormantBytes=%d nonBaseParentDepth=%d)",
				total, ref, len(exec.envStack), exec.dormantSlots, exec.dormantBytes,
				exec.nonBaseParentDepth,
			))
		}
	}
	return total
}

// estimateGraphTail charges the reachable graph beyond the root and env stack:
// loaded modules and the array backings this execution still retains. It is
// shared by the fast walk and the differential-verification reference walk so
// the two can never drift on anything but the env-stack portion they are meant
// to compare.
func (exec *Execution) estimateGraphTail(est *memoryEstimator) int {
	total := 0
	for _, mod := range exec.modules {
		total += est.value(mod)
	}
	// A module still initializing is not in exec.modules yet, and its env hangs
	// off no root the walk reaches; without charging it here the checks running
	// inside its own initialization would measure a graph missing everything it
	// has built (see pushInitializingModule). Envs deduplicate by pointer, so a
	// module env that is also on the env stack is charged once.
	for _, env := range exec.initializingModules {
		total += est.env(env)
	}
	total += exec.detachedArrayBackingBytes(est)
	total += exec.retainedArrayBackingBytes(est)
	return total
}

// estimateScalarBase sums the base-walk contributions that need no estimator:
// per-stack slot overheads, reserved scratch, and small bookkeeping maps. It is
// recomputed on every check (it is O(small) and changes with plain stack
// pushes), so the memoized graph walk never has to be invalidated for it.
func (exec *Execution) estimateScalarBase() int {
	total := exec.reservedScratchBytes

	total += len(exec.callStack) * estimatedCallFrameBytes
	total += len(exec.receiverStack) * estimatedValueBytes
	total += exec.claimStackBytes()
	total += len(exec.validatedCapabilityArgs) * estimatedStringHeaderBytes
	for _, method := range exec.validatedCapabilityArgs {
		total += len(method)
	}
	if exec.moduleLoading != nil {
		total += estimatedMapBaseBytes + len(exec.moduleLoading)*estimatedMapEntryBytes
		for name := range exec.moduleLoading {
			total += estimatedStringHeaderBytes + len(name)
		}
	}
	if exec.capabilityContracts != nil {
		total += estimatedMapBaseBytes + len(exec.capabilityContracts)*estimatedMapEntryBytes
	}
	if exec.capabilityContractScopes != nil {
		total += estimatedMapBaseBytes + len(exec.capabilityContractScopes)*estimatedMapEntryBytes
		total += capabilityContractScopeMemory(exec.capabilityContractScopes)
	}
	if exec.capabilityContractsByName != nil {
		total += estimatedMapBaseBytes + len(exec.capabilityContractsByName)*estimatedMapEntryBytes
		for name := range exec.capabilityContractsByName {
			total += estimatedStringHeaderBytes + len(name)
		}
	}
	total += estimatedSliceBaseBytes + len(exec.moduleLoadStack)*estimatedStringHeaderBytes
	for _, key := range exec.moduleLoadStack {
		total += len(key)
	}
	total += estimatedSliceBaseBytes + len(exec.moduleStack)*estimatedModuleContextSize
	for _, ctx := range exec.moduleStack {
		total += estimatedStringHeaderBytes*3 + len(ctx.key) + len(ctx.path) + len(ctx.root)
	}

	return total
}

func capabilityContractScopeMemory(scopes map[*Builtin]*capabilityContractScope) int {
	const inlineSeenScopes = 8

	total := 0
	var seen [inlineSeenScopes]*capabilityContractScope
	seenLen := 0
	var overflow map[*capabilityContractScope]struct{}
	for _, scope := range scopes {
		if scope == nil {
			continue
		}
		duplicate := false
		for i := range seenLen {
			if seen[i] == scope {
				duplicate = true
				break
			}
		}
		if duplicate {
			continue
		}
		if overflow != nil {
			if _, ok := overflow[scope]; ok {
				continue
			}
			overflow[scope] = struct{}{}
		} else if seenLen < len(seen) {
			seen[seenLen] = scope
			seenLen++
		} else {
			overflow = make(map[*capabilityContractScope]struct{}, len(scopes))
			for _, item := range seen {
				overflow[item] = struct{}{}
			}
			overflow[scope] = struct{}{}
		}
		total += estimatedMapBaseBytes + len(scope.knownBuiltins)*estimatedMapEntryBytes
	}
	return total
}

func (est *memoryEstimator) env(env *Env) int {
	est.walked++
	if env == nil {
		return 0
	}
	if env.frozen {
		// The engine's frozen builtin proto terminates every env chain,
		// so it is revisited on each walk; a single-slot cache replaces
		// the map insert the seen-set would pay per estimate. Frozen
		// envs hold only statically accounted bindings and no parent.
		if est.seenFrozen == env {
			return 0
		}
		est.seenFrozen = env
		return estimatedEnvBytes + staticBindingsBytes(env)
	}
	if est.dormant != nil {
		if _, ok := est.dormant[env]; ok {
			// Already charged in exec.dormantBytes; skipping the recursion here is
			// exact because a committed dormant frame's parent is a base env the
			// root walk always charges. See memory_dormant.go.
			return 0
		}
	}
	if est.rememberEnv(env) {
		return 0
	}
	if est.journal != nil && est.journal.record() {
		est.journal.envs = append(est.journal.envs, env)
	}

	size := estimatedEnvBytes + staticBindingsBytes(env)
	if env.values != nil {
		size += estimatedMapBaseBytes + len(env.values)*estimatedMapEntryBytes
	}
	if len(env.arrayAppendBuffers) > 0 {
		size += estimatedMapBaseBytes + len(env.arrayAppendBuffers)*estimatedMapEntryBytes
		for name, buffer := range env.arrayAppendBuffers {
			size += estimatedStringHeaderBytes + len(name)
			size += est.slice(buffer)
		}
	}
	for i := range int(env.inlineLen) {
		binding := env.inline[i]
		size += len(binding.name)
		size += est.inlineBindingValue(binding.value)
	}
	for name, val := range env.values {
		size += estimatedStringHeaderBytes + len(name)
		size += est.mapBindingValue(val)
	}
	if env.hasCallBlock {
		// A call frame's supplied block lives in a hidden slot rather than a
		// named binding, but for an escaped closure or default proc it can be
		// the only reference to a block that closes over large data. Charge its
		// payload (the block struct and its captured env) so the quota still
		// accounts for it; the value header is already part of estimatedEnvBytes,
		// and a frame that received no block charges nothing.
		size += est.valuePayload(env.callBlock)
	}
	size += est.env(env.parent)
	return size
}

func (est *memoryEstimator) rememberEnv(env *Env) bool {
	for i := range est.seenEnvInlineLen {
		if est.seenEnvInline[i] == env {
			return true
		}
	}
	if est.seenEnvs != nil {
		if _, seen := est.seenEnvs[env]; seen {
			return true
		}
		est.seenEnvs[env] = env.mutationVersion
		return false
	}
	if est.seenEnvInlineLen < len(est.seenEnvInline) {
		est.seenEnvInline[est.seenEnvInlineLen] = env
		est.seenEnvVersions[est.seenEnvInlineLen] = env.mutationVersion
		est.seenEnvInlineLen++
		return false
	}
	est.seenEnvs = make(map[*Env]uint64, len(est.seenEnvInline)+1)
	for _, seenEnv := range est.seenEnvInline {
		est.seenEnvs[seenEnv] = seenEnv.mutationVersion
	}
	est.seenEnvs[env] = env.mutationVersion
	return false
}

func (est *memoryEstimator) forgetEnv(env *Env) {
	for i := range est.seenEnvInlineLen {
		if est.seenEnvInline[i] != env {
			continue
		}
		last := est.seenEnvInlineLen - 1
		est.seenEnvInline[i] = est.seenEnvInline[last]
		est.seenEnvVersions[i] = est.seenEnvVersions[last]
		est.seenEnvInline[last] = nil
		est.seenEnvInlineLen--
		break
	}
	if est.seenEnvs != nil {
		delete(est.seenEnvs, env)
	}
}

func staticBindingsBytes(env *Env) int {
	if env.statics == nil {
		return 0
	}
	return estimatedMapBaseBytes + int(env.staticBytes)
}

func (est *memoryEstimator) inlineBindingValue(val Value) int {
	if _, ok := lazyValue(val); ok {
		return 0
	}
	return est.valuePayload(val)
}

func (est *memoryEstimator) mapBindingValue(val Value) int {
	if _, ok := lazyValue(val); ok {
		return estimatedValueBytes
	}
	return est.value(val)
}

func (est *memoryEstimator) valuePayload(val Value) int {
	size := est.value(val) - estimatedValueBytes
	if size < 0 {
		return 0
	}
	return size
}

func (est *memoryEstimator) value(val Value) int {
	est.walked++
	if estimatorVisitCounting.Load() {
		estimatorVisits.Add(1)
	}
	size := estimatedValueBytes

	switch val.Kind() {
	case KindInt:
		// Compact integers live entirely in the Value struct; only a
		// big-integer payload adds heap footprint, deduplicated by payload
		// identity so aliased copies of one bignum are charged once.
		if bi, ok := value.BigIntPayload(val); ok {
			size += est.bigIntPayloadSize(bi)
		}
	case KindString, KindSymbol:
		str := val.String()
		size += estimatedStringHeaderBytes
		size += est.stringPayloadSize(str)
	case KindRegex:
		// A regex value retains its pattern source and flag strings plus a
		// compiled RE2 program. The program's exact footprint is not cheaply
		// knowable, so it is approximated by the source it was compiled from,
		// which the pattern-size guard bounds at compile time.
		regex := val.Regex()
		size += 2 * estimatedStringHeaderBytes
		size += est.stringPayloadSize(regex.Source)
		size += est.stringPayloadSize(regex.Flags)
		size += len(regex.Source)
	case KindArray:
		size += est.slice(val.Array())
		// Charged per occurrence, not per distinct wrapper, so that it behaves
		// exactly like the slice base it completes: sliceStructuralBytes bills
		// estimatedSliceBaseBytes every time it is reached and deduplicates only
		// the element backing, and a zero-capacity backing has no identity to
		// deduplicate on at all. A charge that deduplicated where its other half
		// does not made the memoized total drift 8 bytes from the reference walk
		// it must equal, because a probe that rolls its seen-set back charges
		// what it inserted and a later walk charges it again. Two aliases of one
		// array are billed the wrapper twice, which is the same conservative
		// over-count the slice base already makes for them.
		size += estimatedArrayWrapperExtraBytes
	case KindHash:
		size += est.hash(val.HashEntryMap())
		// A KindHash wraps its entry map in a hashData struct that also holds the
		// insertion order; that wrapper is a real per-hash heap allocation
		// outside the entry map, so it counts toward the quota too. Charged once
		// per distinct wrapper identity so two values sharing the same hashData
		// are not double counted. Objects use a bare map with no wrapper, so
		// hashWrapperBytes returns 0 for them.
		size += est.hashWrapperBytes(val)
	case KindObject:
		size += est.objectWrapperBytes(val)
		// A tagged bag's published rendering is retained by the wrapper and can
		// outlive the entry it was taken from: a host may remove or replace
		// to_s through the live map while the rendering stays. It goes through
		// the estimator's string accounting so it still deduplicates while it
		// does alias an entry.
		if form, ok := val.ObjectStringForm(); ok {
			size += estimatedStringHeaderBytes
			size += est.stringPayloadSize(form)
		}
		size += est.hash(val.HashEntryMap())
	case KindClass:
		cl := valueClass(val)
		if cl == nil {
			return size
		}
		if _, seen := est.seenClasses[cl]; seen {
			return size
		}
		if est.seenClasses == nil {
			est.seenClasses = make(map[*ClassDef]struct{})
		}
		est.seenClasses[cl] = struct{}{}
		if est.journal != nil && est.journal.record() {
			est.journal.classes = append(est.journal.classes, cl)
		}
		size += est.hash(cl.ClassVars)
	case KindInstance:
		inst := valueInstance(val)
		if inst == nil {
			return size
		}
		if _, seen := est.seenInstances[inst]; seen {
			return size
		}
		if est.seenInstances == nil {
			est.seenInstances = make(map[*Instance]struct{})
		}
		est.seenInstances[inst] = struct{}{}
		if est.journal != nil && est.journal.record() {
			est.journal.instances = append(est.journal.instances, inst)
		}
		size += estimatedInstanceBytes
		size += est.hash(inst.Ivars)
	case KindBlock:
		blk := valueBlock(val)
		if blk == nil {
			return size
		}
		if _, seen := est.seenBlocks[blk]; seen {
			return size
		}
		if est.seenBlocks == nil {
			est.seenBlocks = make(map[*Block]struct{})
		}
		est.seenBlocks[blk] = struct{}{}
		if est.journal != nil && est.journal.record() {
			est.journal.blocks = append(est.journal.blocks, blk)
		}
		size += estimatedBlockBytes + estimatedSliceBaseBytes + len(blk.Params)*estimatedStringHeaderBytes
		for _, param := range blk.Params {
			size += estimatedParamBytes(param)
		}
		size += estimatedSliceBaseBytes + len(blk.ImplicitParams)*estimatedStringHeaderBytes
		for _, param := range blk.ImplicitParams {
			size += len(param)
		}
		size += estimatedStringHeaderBytes*3 + len(blk.moduleKey) + len(blk.modulePath) + len(blk.moduleRoot)
		size += est.env(blk.Env)
	case KindFunction:
		// The compiled body is a static artifact, but the captured environment
		// is not. A module that exports any function retains its whole module
		// env through that closure, and a closure minted in a loop retains its
		// frame; treating the whole value as static let those drop out of the
		// quota once initialization returned, so requiring many modules
		// accumulated unbounded memory (#48). The block arm above charges
		// est.env for exactly this reason. Env dedup keeps an env reachable
		// from both the env stack and a function value charged once.
		if fn := valueFunction(val); fn != nil {
			size += est.env(fn.Env)
		}
	case KindBuiltin:
		// Static stdlib builtins are singletons reachable once, so they stay
		// free. A builtin that closes over runtime values, though, is a
		// dynamically allocated probe a script can mint in a loop (for example
		// pushing `1.eql?` or `obj.equal?` into an array): each member access
		// allocates a fresh *Builtin plus its CapturedValues backing. Charge that
		// per-probe structure — the Builtin struct and the slice backing — so the
		// quota accounts for the allocation itself, not just its captured
		// payloads (which are effectively zero for scalar receivers). Then charge
		// the captured payloads on top. Dedup by builtin pointer guards against
		// revisiting the same builtin; recursing through est.value dedups each
		// captured value against any independently reachable copy via the existing
		// seen* maps, so a receiver that is also reachable elsewhere is charged
		// only once.
		builtin := valueBuiltin(val)
		if builtin == nil || len(builtin.CapturedValues) == 0 {
			return size
		}
		if _, seen := est.seenBuiltins[builtin]; seen {
			return size
		}
		if est.seenBuiltins == nil {
			est.seenBuiltins = make(map[*Builtin]struct{})
		}
		est.seenBuiltins[builtin] = struct{}{}
		if est.journal != nil && est.journal.record() {
			est.journal.builtins = append(est.journal.builtins, builtin)
		}
		size = saturatingAdd(size, estimatedBuiltinBytes)
		size = saturatingAdd(size, sliceStructuralBytes(builtin.CapturedValues))
		for _, captured := range builtin.CapturedValues {
			size = saturatingAdd(size, est.valuePayload(captured))
		}
	}

	return size
}

func estimatedParamBytes(param Param) int {
	size := len(param.Name)
	if param.Target != nil {
		size += estimatedParamTargetBytes(param.Target)
	}
	return size
}

func estimatedParamTargetBytes(target Expression) int {
	switch t := target.(type) {
	case *Identifier:
		return len(t.Name)
	case *DestructureTarget:
		size := 0
		for _, element := range t.Elements {
			size += estimatedParamTargetBytes(element.Target)
		}
		return size
	default:
		return 0
	}
}

// bigIntPayloadSize charges a big-integer payload's heap footprint: the
// big.Int struct plus its allocated word backing (capacity, not length, since
// arithmetic may leave spare allocated words). Payloads are deduplicated by
// pointer identity through the shared seenSlices identity space (an address of
// a live heap object never collides with a distinct live slice backing, and the
// walk-local stability caveat matches ArrayIdentity's). Reusing that set keeps
// the estimator and journal structs at their pre-bignum sizes, which keeps the
// per-check journal clear/rollback on the memoized walk path inlinable.
func (est *memoryEstimator) bigIntPayloadSize(bi *big.Int) int {
	id := uintptr(unsafe.Pointer(bi))
	if _, seen := est.seenSlices[id]; seen {
		return 0
	}
	if est.seenSlices == nil {
		est.seenSlices = make(map[uintptr]struct{})
	}
	est.seenSlices[id] = struct{}{}
	if est.journal != nil && est.journal.record() {
		est.journal.slices = append(est.journal.slices, id)
	}
	return saturatingAdd(estimatedBigIntStructBytes, saturatingMul(cap(bi.Bits()), estimatedBigIntWordBytes))
}

func (est *memoryEstimator) stringPayloadSize(str string) int {
	if len(str) == 0 {
		return 0
	}

	key := stringIdentity{
		ptr: uintptr(unsafe.Pointer(unsafe.StringData(str))),
		len: len(str),
	}
	if _, seen := est.seenStrings[key]; seen {
		return 0
	}
	if est.seenStrings == nil {
		est.seenStrings = make(map[stringIdentity]struct{})
	}
	est.seenStrings[key] = struct{}{}
	if est.journal != nil && est.journal.record() {
		est.journal.strings = append(est.journal.strings, key)
	}
	return len(str)
}

// sliceStructuralBytes is the heap footprint of a slice backing excluding the
// payloads reachable from its elements: the slice base plus one Value slot per
// capacity slot. The element payloads are added on top by recursing into each
// element.
func sliceStructuralBytes(values []Value) int {
	return saturatingAdd(estimatedSliceBaseBytes, saturatingMul(cap(values), estimatedValueBytes))
}

// mapStructuralBytes is the heap footprint of a map backing excluding the
// payloads reachable from its values: the map base, one bucket per entry, a key
// header and key bytes per entry, and one Value slot per entry. The value
// payloads are added on top by recursing into each value.
//
// The per-entry Value slot is part of the structural cost (it exists for every
// entry regardless of what the value points at), so a map of scalar values is
// charged its full slot footprint here even though the recursion contributes no
// further payload for those scalars.
func mapStructuralBytes(values map[string]Value) int {
	size := estimatedMapBaseBytes + len(values)*(estimatedMapEntryBytes+estimatedValueBytes)
	for key := range values {
		size += estimatedStringHeaderBytes + len(key)
	}
	return size
}

func (est *memoryEstimator) slice(values []Value) int {
	size := sliceStructuralBytes(values)
	if cap(values) == 0 {
		return size
	}

	// Deduplicate aliased backings — including empty slices that retained
	// capacity, whose cap*estimatedValueBytes backing is real memory worth
	// counting only once across aliases (e.g. a partition's empty result side
	// shared with another binding). This dedup is best-effort: a non-empty
	// slice's backing pointer (via sliceBackingIdentity/unsafe.SliceData) is
	// stable and reliably deduplicates, but a ZERO-LENGTH slice's backing
	// identity is not reproducible across Go build configurations (observed
	// flaking under coverage, race, and goroutine-leak-profile builds). When an
	// empty slice's identity is unstable the dedup simply does not fire and the
	// backing is counted again — an over-count that is conservative and
	// sandbox-safe (it never under-counts, so the memory bound still holds).
	id := sliceBackingIdentity(values)
	if id != 0 {
		if _, seen := est.seenSlices[id]; seen {
			return 0
		}
		if est.seenSlices == nil {
			est.seenSlices = make(map[uintptr]struct{})
		}
		est.seenSlices[id] = struct{}{}
		if est.journal != nil && est.journal.record() {
			est.journal.slices = append(est.journal.slices, id)
		}
	}

	if len(values) == 0 {
		return size
	}

	for _, val := range values {
		size += est.value(val)
	}
	return size
}

func sliceBackingIdentity(values []Value) uintptr {
	if cap(values) == 0 {
		return 0
	}
	return uintptr(unsafe.Pointer(unsafe.SliceData(values)))
}

// hashWrapperBytes charges the hashData wrapper a KindHash value allocates
// around its entry map, plus any typed-key entry map retained by that wrapper.
// It is deduplicated on the wrapper's identity so aliases count the extra hash
// state once. KindObject allocates its own smaller wrapper, charged by
// objectWrapperBytes.
func (est *memoryEstimator) hashWrapperBytes(val Value) int {
	if val.Kind() != KindHash {
		return 0
	}
	id := hashIdentity(val)
	if id == 0 {
		return saturatingAdd(estimatedHashDataBytes, est.hashReservedBytes(val))
	}
	if _, seen := est.seenHashData[id]; seen {
		return 0
	}
	if est.seenHashData == nil {
		est.seenHashData = make(map[uintptr]struct{})
	}
	est.seenHashData[id] = struct{}{}
	if est.journal != nil && est.journal.record() {
		est.journal.hashData = append(est.journal.hashData, id)
	}
	return saturatingAdd(estimatedHashDataBytes, est.hashReservedBytes(val))
}

// hashReservedBytes charges the storage a hash retains beyond the entries the
// entry-map walk already prices: buckets reserved past the live entry count and
// the insertion-order backing. The order slots hold the entry map's own key
// strings, so only their headers are new.
func (est *memoryEstimator) hashReservedBytes(val Value) int {
	if val.Kind() != KindHash {
		return 0
	}
	size := hashOrderBackingBytes(value.HashOrderCapacity(val))
	if extra := value.HashEntryCapacity(val) - val.HashLen(); extra > 0 {
		size = saturatingAdd(size, saturatingMul(extra, estimatedMapEntryStructuralBytes))
	}
	return size
}

func (est *memoryEstimator) hash(values map[string]Value) int {
	id := reflect.ValueOf(values).Pointer()
	if id != 0 {
		if _, seen := est.seenMaps[id]; seen {
			return 0
		}
		if est.seenMaps == nil {
			est.seenMaps = make(map[uintptr]struct{})
		}
		est.seenMaps[id] = struct{}{}
		if est.journal != nil && est.journal.record() {
			est.journal.maps = append(est.journal.maps, id)
		}
	}

	size := mapStructuralBytes(values)
	for _, val := range values {
		size += est.valuePayload(val)
	}
	return size
}

// accumulatedBytes reports the payload the build has charged so far.
//
// A build accumulator records the growing result privately, so memory checks
// performed inside a block call cannot see it: a block allocating a large
// temporary is measured against a baseline that omits everything the loop has
// already retained, and the two can pass separately even though they coexist.
// Exposing the running total lets the loop reserve it as releasable scratch,
// which is what makes the retained output visible to those checks.
func (acc *arrayBuildAccumulator) accumulatedBytes(backingCap int) int {
	if acc == nil {
		return 0
	}
	// The element payload alone is not what stays live: the preallocated
	// result backing and the scratch reserved for the walk do too, and for a
	// block returning scalars the payload stays zero -- so reserving only that
	// never grew the reservation at all while the backing could still fill the
	// quota alongside an in-block temporary.
	return saturatingAdd(acc.payload, arraySlotBackingBytes(backingCap))
}

// retainedOutputScratch keeps a loop's accumulated output reserved as scratch,
// raising the reservation as the output grows and releasing all of it when the
// loop ends. A driver whose output is a base-walk root (see memory_output.go)
// needs neither, since the estimator reaches its results directly; this remains
// for the builders that cannot afford the walk a root costs them, which
// format's per-operand to_s conversions measured as quadratic.
type retainedOutputScratch struct {
	exec     *Execution
	reserved int
}

func newRetainedOutputScratch(exec *Execution) *retainedOutputScratch {
	return &retainedOutputScratch{exec: exec}
}

// reserve raises the reservation to total, charging only the increase.
func (r *retainedOutputScratch) reserve(total int) {
	if r == nil || r.exec == nil || total <= r.reserved {
		return
	}
	r.exec.reserveLoopScratch(total - r.reserved)
	r.reserved = total
}

// release returns the whole reservation once the loop is done with it.
func (r *retainedOutputScratch) release() {
	if r == nil || r.exec == nil {
		return
	}
	r.exec.releaseLoopScratch(r.reserved)
	r.reserved = 0
}

// objectWrapperBytes charges the objectData wrapper a KindObject value
// allocates around its entry map. Like hashWrapperBytes it deduplicates on the
// entry map's identity, so wrappers sharing one map count the extra state
// once. Without it a workload holding many small objects -- a JSON array of
// empty ones, say -- was undercounted by a wrapper apiece.
func (est *memoryEstimator) objectWrapperBytes(val Value) int {
	if val.Kind() != KindObject {
		return 0
	}
	// Keyed on the wrapper, not the entry map: several wrappers can share one
	// map -- a host passing an array of NewObject over the same entries makes
	// exactly that -- and each is its own allocation.
	id := value.ObjectIdentity(val)
	if id == 0 {
		return estimatedObjectDataBytes
	}
	if _, seen := est.seenObjectData[id]; seen {
		return 0
	}
	if est.seenObjectData == nil {
		est.seenObjectData = make(map[uintptr]struct{})
	}
	est.seenObjectData[id] = struct{}{}
	if est.journal != nil && est.journal.record() {
		est.journal.objectData = append(est.journal.objectData, id)
	}
	return estimatedObjectDataBytes
}

// reserveCallerRetainedRoots folds the innermost builtin frame's receiver and
// arguments into the reserved scratch, returning the delta to release when the
// block it drives has finished.
//
// Those values live on that frame's Go stack, which no walk reaches, so the
// body's own checks -- per-statement walks and mutator preflights -- cannot see
// them. Reserving them makes those checks bound the combined peak of what the
// caller holds and what the body builds, which is the same job blockBindCharge
// does for the values it is built from.
//
// What the caller passes to the block is excluded, by seeding the estimator
// with it before measuring: those values are bound into the block's own scope
// and counted by every walk while the body runs, so charging them here as well
// doubled them. `proc.call`, whose arguments are exactly the block's, went from
// charging a 2,000,000-byte argument once to charging it twice.
func (exec *Execution) reserveCallerRetainedRoots(blockArgs []Value) (int, bool) {
	if exec == nil || exec.memoryQuota <= 0 {
		return 0, false
	}
	// Nothing held means nothing to weigh, and the walk below is not free: it is
	// a full base walk, and a yield inside a script function reaches here with no
	// builtin frame in scope at all. Walking anyway made a loop of n yields over
	// an n-element collection cost 1,957,647 estimator nodes against master's
	// 1,310,447 -- a 1.49x constant on a path that is already quadratic, bought
	// for a measurement of nothing.
	if exec.builtinFrameReceiver.Kind() == KindNil && len(exec.builtinFrameArgs) == 0 &&
		len(exec.builtinFrameKwargs) == 0 {
		return 0, false
	}
	// Already folded in by an outer CallBlock under this same frame. A block that
	// enters a script function which yields arrives here again holding exactly
	// what the outer call reserved, and reserving it twice charges it twice.
	if exec.builtinFrameRootsReserved {
		return 0, false
	}
	est := newMemoryEstimator()
	base := exec.estimateMemoryUsageBase(est)
	for _, arg := range blockArgs {
		base = saturatingAdd(base, est.value(arg))
	}
	total := base
	if exec.builtinFrameReceiver.Kind() != KindNil {
		total = saturatingAdd(total, est.value(exec.builtinFrameReceiver))
	}
	for _, arg := range exec.builtinFrameArgs {
		total = saturatingAdd(total, est.value(arg))
	}
	for _, kwarg := range exec.builtinFrameKwargs {
		total = saturatingAdd(total, est.value(kwarg))
	}
	exec.builtinFrameRootsReserved = true
	if total <= base {
		return 0, true
	}
	return exec.reserveLoopScratch(total - base), true
}
