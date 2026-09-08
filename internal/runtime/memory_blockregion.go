package runtime

import (
	"fmt"

	"github.com/mgomes/vibescript/vibes/value"
)

// Block-iteration region accounting makes the memory-quota estimator's per-check
// base walk incremental across a block-driving builtin's iteration.
//
// A pure block driver — Array#map/select/group_by/reduce/each and their kin —
// holds a receiver collection on its Go stack and yields each element to a
// script block. While the block body runs, step() performs its periodic
// reachable-graph memory walk (an accumulator-metered section cannot vouch for
// arbitrary script, so it is suspended inside a block body). That walk re-charges
// every env-stack frame, and the frame that owns the receiver is one of them, so
// iterating an n-element collection under a memory quota re-walked the whole
// collection O(n) times — quadratic. The base-walk memo (see beginBaseWalk)
// could collapse those repeats to O(1) per check, but two things defeat it every
// iteration: the driver is a Go builtin (an undeclared one forces the memo
// bypass), and binding the block parameter bumps the mutation epoch (which
// invalidates the memo).
//
// A region lifts both. When a driver opens a region it records the env-stack
// depth beneath its block scopes as the region boundary. From then until the
// region closes:
//
//   - The base walk memoizes the reachable graph of the stable prefix
//     (everything at a depth below the boundary: the receiver-owning frame, its
//     ancestors, the root, and the module tail) exactly as the ordinary memo
//     does, and re-walks only the active suffix (the block's own parameter and
//     local scopes, at or above the boundary) fresh on each check. The suffix is
//     small and bounded by the block, not the collection, so a check costs
//     O(block) rather than O(collection).
//
//   - Every scope pushed inside the region is epoch-neutral (see Env.epochNeutral
//     and pushEnv): because it is re-walked fresh every check, a binding write
//     confined to it cannot make the memoized prefix stale, so those writes skip
//     the epoch bump that would otherwise invalidate the memo each iteration. A
//     write that escapes the block — an outer-variable rebind that resolves up
//     the parent chain to a prefix scope, or a mutation of a container reachable
//     from the prefix (which goes through the value package) — still bumps, so
//     the prefix memo is invalidated exactly when the prefix truly changes.
//
//   - The memo is engaged only while the undeclared-builtin depth equals the
//     region's driver depth, i.e. in the block body the region drives directly.
//     A deeper builtin called from the block body re-raises the ordinary bypass,
//     because it may mutate reachable containers through raw writes the epoch
//     cannot observe — unless it has declared that it does not, in which case it
//     is not counted, and a check inside it keeps the region's memo. Without
//     that, a block body calling a member which runs a memory check of its own
//     (a string transform charging its scan, say) paid a whole-graph re-walk per
//     element even though nothing it did could invalidate the prefix.
//
// Correctness rests on one invariant: an epoch-neutral scope is re-walked fresh
// on every check for as long as it is on the stack. The boundary guarantees it —
// every epoch-neutral scope sits at or above the boundary, and the region base
// walk always re-walks the suffix from the boundary up. The differential oracle
// (estimatorVerify) recomputes the full-stack reference on every region check —
// including memo hits, unlike the ordinary memo — and panics on any divergence,
// so a stale prefix is caught the instant it is exercised.

// blockRegionScope is the handle a driver holds for the duration of its block
// iteration. The driver opens a region with beginBlockIterationRegion and closes
// it with a deferred end, which restores the enclosing region's state so nested
// drivers (a map inside a group_by block) compose.
type blockRegionScope struct {
	exec             *Execution
	prevActive       bool
	prevBoundary     int
	prevBuiltinDepth int
}

// blockRegionDriverDepth is the depth the region compares against to decide it
// is running in the block body it drives rather than inside a deeper builtin.
// It counts only the builtins that have not declared non-mutation, so a
// declared one called from the block body does not look like a new driver: the
// region stays engaged through it, which is the whole point of the declaration
// for a member that runs a memory check of its own.
func (exec *Execution) blockRegionDriverDepth() int { return exec.undeclaredBuiltinDepth }

// beginBlockIterationRegion opens a block-iteration region anchored at the
// current env-stack depth. A driver calls it as
// `defer exec.beginBlockIterationRegion().end()` around its iteration loop,
// after any pre-loop scratch reservation and before the first runner.call. It is
// a cheap no-op without an enforced memory quota (no estimator runs, so there is
// nothing to make incremental). Nested regions keep the outermost boundary — so
// the memoized prefix excludes every active region's scopes — while tracking the
// innermost driver's builtin depth, so the memo engages in whichever block body
// is currently executing.
func (exec *Execution) beginBlockIterationRegion() blockRegionScope {
	scope := blockRegionScope{
		exec:             exec,
		prevActive:       exec.blockRegionActive,
		prevBoundary:     exec.blockRegionBoundary,
		prevBuiltinDepth: exec.blockRegionBuiltinDepth,
	}
	if exec.memoryQuota <= 0 {
		return scope
	}
	if !exec.blockRegionActive {
		exec.blockRegionBoundary = len(exec.envStack)
	}
	exec.blockRegionActive = true
	exec.blockRegionBuiltinDepth = exec.blockRegionDriverDepth()
	return scope
}

// end closes the region, restoring the enclosing region's state (or no region at
// the outermost level). It runs on the normal and error return paths through the
// driver's defer; a panic tears the execution down wholesale, so the restored
// state is never observed after one.
func (scope blockRegionScope) end() {
	scope.exec.blockRegionActive = scope.prevActive
	scope.exec.blockRegionBoundary = scope.prevBoundary
	scope.exec.blockRegionBuiltinDepth = scope.prevBuiltinDepth
}

// blockRegionBaseWalkEngaged reports whether the current check should take the
// region base walk: a region is active, the check is running in the block body
// that region drives directly (not a deeper builtin), the boundary still indexes
// the live stack, and the test kill switch is off, so a cache-disabled
// differential run falls through to the unmemoized reference walk.
func (exec *Execution) blockRegionBaseWalkEngaged() bool {
	return exec.blockRegionActive &&
		exec.blockRegionDriverDepth() == exec.blockRegionBuiltinDepth &&
		exec.blockRegionBoundary >= 0 &&
		exec.blockRegionBoundary <= len(exec.envStack) &&
		!baseWalkCacheDisabled.Load()
}

// beginRegionBaseWalk opens a base-walk session that memoizes the region's stable
// prefix and re-walks its active suffix fresh. It mirrors the ordinary memoized
// session (beginBaseWalk): the memo's graphBytes holds the prefix's reachable
// graph keyed on (epoch, topo, boundary), the suffix is walked as journaled
// extras that s.close rolls back, and the caller's own extras stack on top and
// deduplicate against both. A boundary change (region open/close, or a nested
// region) fails the key and forces a re-walk, so the memo never serves a
// prefix-only total to a whole-stack check.
func (exec *Execution) beginRegionBaseWalk(est *memoryEstimator, scalars int) baseWalkSession {
	walked0 := est.walked
	c := exec.baseWalkCache
	if c == nil {
		if exec.engine != nil {
			c = exec.engine.spareBaseWalkCache.Swap(nil)
		}
		if c == nil {
			c = new(baseWalkCache)
		}
		c.valid = false
		exec.baseWalkCache = c
	}

	boundary := exec.blockRegionBoundary
	epoch := value.MutationEpoch()
	if !c.valid || !c.mutationsCurrent(est, epoch) || c.topo != exec.baseTopoVersion || c.regionBoundary != boundary {
		est.journal = nil
		est.reset()
		est.dormant = nil
		c.captureMutationEpochs(epoch)
		c.topo = exec.baseTopoVersion
		c.regionBoundary = boundary
		c.graphBytes = exec.estimateGraphBasePrefix(est, boundary)
		// Walked after the prefix and committed alongside it, so the driver's
		// later results deduplicate against everything already counted and the
		// checks between two of them pay nothing (see memory_output.go).
		c.outputBytes = exec.outputWalkBytes(est)
		c.journalBudget = sessionJournalBudget(est.identityCount())
		c.valid = true
	}

	exec.baseWalkOpen = true
	c.prevFrozen = est.seenFrozen
	c.journal.clear()
	c.journal.limit = c.journalBudget
	est.journal = &c.journal
	// The suffix walk and the caller's extras deduplicate against the committed
	// prefix through est's own seen-state, not the dormant set, which the region
	// walk does not use.
	est.dormant = nil

	suffix := 0
	for _, env := range exec.envStack[boundary:] {
		suffix += est.env(env)
	}
	base := scalars + c.graphBytes + c.outputBytes + suffix

	if estimatorVerify {
		// Recompute the whole-stack reference and require the prefix memo plus the
		// fresh suffix walk to equal it. Unlike the ordinary memo's oracle this runs
		// on every region check, hit or miss, so a prefix left stale by a wrongly
		// suppressed epoch bump is caught the instant a check observes it.
		//
		// Registered driver outputs are part of both sides. The reference walks
		// them after the whole stack while the memo commits them between the
		// prefix and the suffix; the estimator's total is a deduplicated union
		// over identities, so the two orders agree.
		refEst := newMemoryEstimator()
		ref := exec.estimateGraphBase(refEst) + exec.outputWalkBytes(refEst)
		if got := c.graphBytes + c.outputBytes + suffix; ref != got {
			// On divergence, recompute the prefix and suffix fresh so the panic can
			// attribute the gap: a cachedPrefix below freshPrefix means the memo went
			// stale (a prefix-reachable mutation skipped its epoch bump), while a
			// suffix that differs from freshSuffix points at the re-walk itself. This
			// runs only on the failure path, so it costs nothing in a passing run.
			freshEst := newMemoryEstimator()
			freshPrefix := exec.estimateGraphBasePrefix(freshEst, boundary)
			freshOutputs := exec.outputWalkBytes(freshEst)
			freshSuffix := 0
			for _, env := range exec.envStack[boundary:] {
				freshSuffix += freshEst.env(env)
			}
			panic(fmt.Sprintf(
				"vibescript: block-region estimator mismatch: prefix+outputs+suffix=%d reference=%d "+
					"(cachedPrefix=%d freshPrefix=%d cachedOutputs=%d freshOutputs=%d suffix=%d freshSuffix=%d freshTotal=%d) "+
					"(stackDepth=%d boundary=%d builtinDepth=%d regionBuiltinDepth=%d)",
				got, ref, c.graphBytes, freshPrefix, c.outputBytes, freshOutputs, suffix, freshSuffix,
				freshPrefix+freshOutputs+freshSuffix,
				len(exec.envStack), boundary, exec.builtinDepth,
				exec.blockRegionBuiltinDepth,
			))
		}
	}

	return baseWalkSession{
		exec: exec, est: est, base: base, walked0: walked0, cached: true,
	}
}

// estimateGraphBasePrefix walks the reachable graph of the region's stable
// prefix: the root, every env-stack frame below the boundary, and the module
// tail. It is the region counterpart of estimateGraphBase, which walks the whole
// stack; the frames at or above the boundary are the active suffix the region
// session re-walks fresh instead of memoizing.
func (exec *Execution) estimateGraphBasePrefix(est *memoryEstimator, boundary int) int {
	total := est.env(exec.root)
	for _, env := range exec.envStack[:boundary] {
		total += est.env(env)
	}
	total += exec.estimateGraphTail(est)
	return total
}
