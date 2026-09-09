package runtime

import (
	"fmt"

	"github.com/mgomes/vibescript/vibes/value"
)

// Output-root accounting closes the hole a builtin opens when it accumulates
// results into a Go local while calling back into script code.
//
// The estimator measures what the interpreter can reach: the environments, the
// modules, the array backings an execution retains. A driver's own result slice
// is none of those. So while
// Hash#fetch_values, Hash#values_at, hash.map, array.map,
// Hash#map_with_index and Hash#transform_values run, every check performed
// inside the callback -- the block's own allocations, its calls, step()'s
// periodic walk -- measured a graph that omitted everything the loop had
// already retained. A script could return an individually permitted value per
// callback and accumulate past the quota one accepted result at a time.
//
// Registering the output as a base-walk root closes it by construction rather
// than by arithmetic. Every check re-derives what the output holds at the
// moment it runs, through the same deduplicating estimator that walks the
// receiver and the arguments, so a result that aliases something already
// counted is counted once, and a result the callback has just grown, stored, or
// detached is priced as it is now rather than as it was when it was produced.
// That last property is why pricing failed: a callback can grow a retained
// value, detach it from the receiver, and allocate against the difference all
// within one call, so no measurement taken before a callback is still true
// while the callback allocates.
//
// The roots are walked with the graph, committed into the same seen-state, and
// memoized beside it. A driver fills its output with a raw Go write that bumps
// no mutation epoch, so the memo alone would go stale by exactly the results
// the driver adds; each of those is committed through addRetainedOutput, the
// way the charged append commits a shovel's marginal (see memory_append.go).
// Everything else that can change what the output holds is a script-visible
// mutation, which bumps the epoch and discards the memo, so the output is
// re-derived from scratch before the next check reads it. Registering and
// unregistering a root changes the walk's root set, so both bump the topology
// version, exactly as pushing an initializing module's environment does.
//
// Memoizing them is not an optimization to be traded away: a driver runs many
// checks between two changes to its output, so re-walking the roots as session
// extras on every check is quadratic in the output's length. array.map over
// 4000 elements takes 216ms that way against 3.4ms here.

// outputWalkRoot charges the payload a driver's Go-local output currently
// retains, through est so it deduplicates against everything the walk has
// already counted. Only payloads are re-derived: the output's slot backing is a
// fixed allocation whose size is known before the first callback, and the
// drivers reserve it through reserveLoopScratch so the runner's bind baseline
// includes it.
type outputWalkRoot func(est *memoryEstimator) int

// retainedValues walks a result slice the driver fills or appends to. It takes
// a pointer because append can move the backing, and the walk must see the
// slice the driver holds now rather than the header it held at registration.
func retainedValues(out *[]Value) outputWalkRoot {
	return func(est *memoryEstimator) int {
		total := 0
		for _, val := range *out {
			total = saturatingAdd(total, est.valuePayload(val))
		}
		return total
	}
}

// retainedValuesWithReceiver walks a driver's receiver before its result slice,
// for the drivers whose results can alias the receiver they came from.
//
// Registering the receiver looks redundant -- a reachable one is already in the
// graph -- but it is what keeps two mechanisms from pricing the same bytes. An
// ephemeral receiver is not in the graph, so the bind charge reserves its
// marginal as scratch for the block body to see; a result aliasing it then went
// into the output, which is walked against a graph the receiver is absent from,
// and the same payload was charged twice. Measured on a 400,000-byte value held
// by an inline receiver and returned for a present key, the lookup needed
// 804,908 bytes of quota where the same receiver bound to a local needed
// 404,909.
//
// Walking it here dissolves the overlap rather than subtracting it: the charge's
// own baseline reads these roots first, so the receiver's marginal against it is
// nothing and no scratch is reserved for it, while the output's aliases
// deduplicate against the same walk. A receiver already in the graph costs a
// single deduplicated visit, because the estimator short-circuits a container it
// has seen.
func retainedValuesWithReceiver(receiver Value, out *[]Value) outputWalkRoot {
	return func(est *memoryEstimator) int {
		total := est.value(receiver)
		for _, val := range *out {
			total = saturatingAdd(total, est.valuePayload(val))
		}
		return total
	}
}

// pushOutputWalkRoot registers a driver's Go-local output for the duration of
// its loop; the driver pairs it with a deferred endOutputWalkRoot. It is a
// no-op without an enforced memory quota, where no estimator walk runs at all.
func (exec *Execution) pushOutputWalkRoot(walk outputWalkRoot) {
	if exec.memoryQuota <= 0 {
		return
	}
	exec.baseTopoVersion++
	exec.outputWalkRoots = append(exec.outputWalkRoots, walk)
}

// endOutputWalkRoot unregisters the driver's output and settles the walk its
// callbacks forced, returning the error the driver is leaving with. A driver
// defers it, so the settlement happens on the way out whatever the outcome:
// unregistering and billing are one operation because leaving either half undone
// is a defect, and there is no other way to unregister.
//
// Settling here is what makes the two outcomes cost the same. Whatever the loop
// recorded for billing, an error return used to skip, so a script could rescue and
// repeat the shape and pay less than one that completed; and the unsettled count
// stayed on the execution to be billed to whichever lookup ran next, which is the
// same defect seen from the other side. Settling on exit fixes both, because the
// charge zeroes the counter.
//
// What the counter holds is the bind charge's construction walk, and nothing else.
// The retained-output walk it once also held is no longer recorded at all: that
// walk is forced by a memo miss whose cause cannot be attributed to this
// execution, so charging it billed one script for another's mutations (see
// outputWalkBytes).
//
// The root is unregistered before the charge so the memory check inside the step
// charge does not re-walk the output this driver is abandoning.
//
// The driver's own error wins when both are present, and nothing is lost by that.
// Every error the settlement can raise is either latched on the execution -- both
// quota errors go through latchExhaustion, which step re-raises on the next charge
// and which makes rescue match nothing -- or sticky, as a canceled context is. So
// the settlement's verdict is delivered by the next check rather than discarded,
// while overriding would rewrite a script-visible failure into an infrastructure
// one.
func (exec *Execution) endOutputWalkRoot(err error) error {
	if exec.memoryQuota <= 0 {
		return err
	}
	if last := len(exec.outputWalkRoots) - 1; last >= 0 {
		exec.baseTopoVersion++
		// Clear before shortening: a truncated slice keeps the closure -- and
		// through it the whole output -- alive in its backing array, so the
		// values would stay reachable for the collector while the walk, which
		// reads only the visible length, stopped charging them.
		exec.outputWalkRoots[last] = nil
		exec.outputWalkRoots = exec.outputWalkRoots[:last]
	}
	if chargeErr := exec.chargeRetainedOutputWalk(); chargeErr != nil && err == nil {
		return chargeErr
	}
	return err
}

// outputWalkBytes charges every registered output root through est. Nested
// drivers (a map inside a map's block) each register their own, and they
// deduplicate against one another the way any two roots do.
func (exec *Execution) outputWalkBytes(est *memoryEstimator) int {
	if len(exec.outputWalkRoots) == 0 {
		return 0
	}
	total := 0
	for _, walk := range exec.outputWalkRoots {
		total = saturatingAdd(total, walk(est))
	}
	// This traversal is deliberately NOT recorded for billing. It happens whenever
	// the base-walk memo cannot answer, and the memo is keyed on a process-wide
	// mutation epoch that any execution in the process advances -- so an unrelated
	// script's mutation forces this walk exactly as this script's own does, and
	// there is no state on the Execution that tells the two apart. Billing it let
	// a concurrent mutator drive an innocent lookup's step usage from 10,053 nodes
	// to 166,753. What is still billed is the bind charge's construction walk,
	// which happens because this execution built a charge (see newBlockBindCharge).
	return total
}

// chargeRetainedOutputWalk bills the step quota for the estimator work recorded
// since it was last called.
//
// Drivers do not call this. It is settled at three structural points instead,
// each of which is where a charge is created or abandoned rather than somewhere
// a driver has to remember:
//
//   - newBlockCallRunner, immediately after building the charge that records the
//     walk. This is the one that covers a driver which never invokes its block --
//     array.each builds a runner before it discovers its receiver is empty -- so
//     no callback ever arrives to settle it.
//   - callBlock, before running any callback, for a charge built without a runner
//     (a host-driven block call).
//   - endOutputWalkRoot, on the way out, so nothing is left on the execution for
//     a later driver to be charged for.
//
// As a per-driver convention this could not be right, in two separate ways. The
// counter is filled once, when the charge is built, which is before the driver's
// first callback, so settling after each callback still let that first one run
// on a quota the walk had already spent. And a charge recorded by a nested
// driver that never invokes its block has no callback to settle it at all: a
// lookup went on to process 50,000 present keys, which invoke nothing and can
// cost no steps, against a quota that charge had already exhausted.
//
// Despite the name, what is recorded is NOT the retained-output walk. That walk is
// unbilled (see outputWalkBytes for why). What reaches this counter is the bind
// charge's construction walk, which happens because this execution built a charge
// and is therefore attributable to it. A lookup whose callback binds no named rest
// builds no charge, records nothing here, and pays nothing.
func (exec *Execution) chargeRetainedOutputWalk() error {
	nodes := exec.outputWalkNodes
	if nodes == 0 {
		return nil
	}
	exec.outputWalkNodes = 0
	return exec.chargeEstimatorWalk(nodes)
}

// addRetainedOutput records that val has just joined a registered output, and
// commits its marginal bytes into the memoized base walk so the next check sees
// it without re-walking the whole output. The driver calls it immediately after
// the write that publishes val into its Go local.
//
// A driver adds exactly one value at a time and never replaces one, so the
// deduplicated union the memo holds grows by exactly this value's marginal over
// the committed seen-state -- the same delta model the charged append uses. Any
// other way the output's contents can change is a mutation performed by script
// code, which bumps the mutation epoch and discards the memo. When the memo
// cannot take the commit it is invalidated instead: leaving it valid would
// serve a total that omits this result, which is the under-count the whole
// mechanism exists to prevent.
func (exec *Execution) addRetainedOutput(val Value) {
	if exec.memoryQuota <= 0 || len(exec.outputWalkRoots) == 0 {
		return
	}
	c := exec.baseWalkCache
	if c == nil || !c.valid {
		return
	}
	if exec.baseWalkOpen || !c.mutationsCurrent(&exec.memoryEst, value.MutationEpoch()) || c.topo != exec.baseTopoVersion ||
		c.regionBoundary != exec.currentWalkBoundary() {
		c.valid = false
		c.unmemoizedPrefix = false
		return
	}
	est := &exec.memoryEst
	est.journal = nil
	est.dormant = exec.walkDormantSet(c.regionBoundary)
	c.outputBytes = saturatingAdd(c.outputBytes, est.valuePayload(val))
	c.journalBudget = sessionJournalBudget(est.identityCount())
	exec.verifyRetainedOutputCommit(c)
}

// retainedOutputMarginalBytes reports what the registered driver outputs add
// beyond the reachable graph -- their marginal, deduplicated against everything
// the graph walk already reaches. That basis is the whole contract: it is the
// basis c.outputBytes is memoized on and the basis estimateMemoryUsageBase folds
// into a snapshot, so a caller comparing a snapshot with a later reading is
// comparing like with like.
//
// Getting that wrong was subtle and worth recording. The fallback used to return
// the roots' standalone footprint, which counts payloads the graph also holds. A
// nested driver reaches it every time, because registering the inner output bumps
// the topology and invalidates the memo, so a bind charge built there recorded a
// start value inflated by every outer result that aliases its receiver. Later
// readings came from the memo on the marginal basis and were smaller, so the
// growth between them read as zero and the driver's accumulation stopped being
// charged at all.
//
// The fallback therefore walks the graph first and prices the roots against it,
// which is the same computation the memo holds. It is reached only when the memo
// cannot answer, and the one caller that would hit it on every construction takes
// its start value from its own base walk instead. The walk is not billed: what
// forces it is a memo miss this execution cannot be shown to have caused.
func (exec *Execution) retainedOutputMarginalBytes() int {
	if len(exec.outputWalkRoots) == 0 {
		return 0
	}
	if c := exec.baseWalkCache; c != nil && c.valid && !exec.baseWalkOpen &&
		c.mutationsCurrent(&exec.memoryEst, value.MutationEpoch()) && c.topo == exec.baseTopoVersion &&
		c.regionBoundary == exec.currentWalkBoundary() {
		return c.outputBytes
	}
	est := newMemoryEstimator()
	exec.estimateGraphBase(est)
	return exec.outputWalkBytes(est)
}

// currentWalkBoundary reports the walk shape the next memory check would take:
// a block-iteration region's prefix boundary, or noBlockRegion for the ordinary
// whole-stack walk. A commit made against a different shape than the memo holds
// would be added to a total the next check discards, so the caller invalidates
// instead.
func (exec *Execution) currentWalkBoundary() int {
	if exec.blockRegionBaseWalkEngaged() {
		return exec.blockRegionBoundary
	}
	return noBlockRegion
}

// walkDormantSet returns the dormant-frame set the walk that built the memo
// used, so a commit prices an environment reachable from val exactly as that
// walk would have. A region walk does not use the optimization at all.
func (exec *Execution) walkDormantSet(boundary int) map[*Env]struct{} {
	if boundary != noBlockRegion {
		return nil
	}
	return exec.currentDormantSet()
}

// verifyRetainedOutputCommit is the differential oracle for the commit above:
// under VIBES_ESTIMATOR_VERIFY the memoized graph and output totals together
// must be byte-identical to a from-scratch reference walk of the same shape.
func (exec *Execution) verifyRetainedOutputCommit(c *baseWalkCache) {
	if !estimatorVerify {
		return
	}
	ref := newMemoryEstimator()
	var graph int
	if c.regionBoundary == noBlockRegion {
		graph = exec.estimateGraphBase(ref)
	} else {
		graph = exec.estimateGraphBasePrefix(ref, c.regionBoundary)
	}
	reference := graph + exec.outputWalkBytes(ref)
	if memoized := c.graphBytes + c.outputBytes; memoized != reference {
		panic(fmt.Sprintf(
			"vibescript: retained-output commit diverged: memo holds %d bytes (graph=%d output=%d), reference walk %d",
			memoized, c.graphBytes, c.outputBytes, reference,
		))
	}
}
