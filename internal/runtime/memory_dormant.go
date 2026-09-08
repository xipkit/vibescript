package runtime

import "github.com/mgomes/vibescript/vibes/value"

// estimatorVerify enables the dormant-frame differential oracle: estimateGraphBaseFast
// additionally computes the reference full-stack estimate and panics on any
// mismatch. Production leaves it false; the test suite sets it write-once from
// VIBES_ESTIMATOR_VERIFY=1 before any test runs (see the runtime TestMain), so
// running the whole corpus and the execution fuzzers under the flag proves the
// optimization byte-identical to the reference across every exercised program.
var estimatorVerify = false

// Dormant-frame accounting makes the memory-quota estimator's env-stack walk
// incremental for deep, block-free function recursion (naive fib being the
// canonical case), where the reference walk re-charges every frame on the stack
// at every check — O(depth) per check, O(depth^2) over a recursion.
//
// The walk sums a deduplicated union over every reachable identity, so its total
// is order-independent. That lets a frame be charged once, when it becomes a
// dormant ancestor, and skipped on every later check until it resumes — provided
// the frame's contribution cannot change while it is dormant. A frame satisfies
// that when:
//
//   - every binding it holds is an immutable, non-deduplicated scalar (a compact
//     Int, Float, Bool, or Nil): such values have a fixed byte cost that no
//     descendant can grow and that never aliases another identity, so summing
//     them independently is exact; and
//   - no scope that could rebind one of its bindings is on the stack. A dormant
//     frame's bindings are reachable for mutation only through a block or a
//     closure that captured the frame, and both push a scope whose parent is a
//     non-base env. exec.nonBaseParentDepth counts those scopes; while it is
//     zero, every stack frame is a plain call frame parented to a base env and
//     no dormant frame can be reached for rebinding.
//
// When nonBaseParentDepth is non-zero the optimization disengages entirely and
// the estimator falls back to the reference full-stack walk, so the fast path is
// only ever taken where its immutability invariant provably holds.
//
// Both mechanisms behind that second condition — raising the counter and
// dropping the committed prefix — run at the push, in pushEnv (see
// retractAllDormant), so the invariant holds for every walk path rather than for
// the one path that happens to read the counter (#19, #20).

// dormantFrame records one committed dormant env: the stack slots it occupies
// (a frame is pushed once per call plus once per evalStatements body scope, all
// sharing the same env identity, so a distinct env spans a run of consecutive
// slots) and the exact bytes est.env would charge it with its parent already
// walked.
type dormantFrame struct {
	env       *Env
	startSlot int
	slotCount int
	bytes     int
}

// isBaseEnv reports whether env is charged by the estimator's base walk outside
// the env stack — the execution root and its ancestor chain (the per-call root
// and the engine-shared frozen proto). A call frame whose parent is a base env
// therefore chains only into already-charged scopes, never into another stack
// frame, which is what lets dormant frames be summed independently of the walk.
func (exec *Execution) isBaseEnv(env *Env) bool {
	if env == nil {
		return true
	}
	return env == exec.root || env.callRoot || env.frozen
}

// committableScalar reports whether v is an immutable, non-deduplicated scalar
// whose estimator cost is exactly estimatedValueBytes (zero marginal payload).
// These are the only values a dormant frame may hold: their cost cannot grow and
// never aliases another identity, so a committed frame's byte total stays exact
// no matter what its descendants do.
func committableScalar(v Value) bool {
	if _, ok := lazyValue(v); ok {
		return false
	}
	switch v.Kind() {
	case KindNil, KindBool, KindFloat:
		return true
	case KindInt:
		_, isBig := value.BigIntPayload(v)
		return !isBig
	default:
		return false
	}
}

// committableFrameBytes returns the exact bytes est.env would charge env with its
// parent already walked, and whether env may be committed as dormant. The byte
// total mirrors est.env's accounting for the committable shape — a plain call
// frame with only inline scalar bindings and no map, statics, append buffers, or
// captured block — so skipping the frame in the walk and adding this total is
// byte-identical to walking it. Any other shape returns ok=false and is walked.
func committableFrameBytes(env *Env) (int, bool) {
	if env == nil || env.frozen || env.classBody {
		return 0, false
	}
	if env.statics != nil || env.values != nil || len(env.arrayAppendBuffers) > 0 {
		return 0, false
	}
	if env.hasCallBlock && !committableScalar(env.callBlock) {
		return 0, false
	}
	bytes := estimatedEnvBytes
	for i := range int(env.inlineLen) {
		binding := env.inline[i]
		if !committableScalar(binding.value) {
			return 0, false
		}
		bytes += len(binding.name)
	}
	return bytes, true
}

// reconcileDormant brings the committed dormant prefix into agreement with the
// current env stack before a walk. It retracts frames the stack has popped or
// resumed and commits newly dormant frames, both in strict last-in-first-out
// order against the stack's bottom-anchored prefix, so its cost is proportional
// to the frames pushed or popped since the previous check — amortized O(1) over a
// steady recursion. It must run only while nonBaseParentDepth is zero.
func (exec *Execution) reconcileDormant() {
	stack := exec.envStack
	n := len(stack)

	// The active region is the executing frame together with its consecutive
	// duplicate pushes (the call frame plus each evalStatements body scope share
	// one env identity). Those copies must stay walked: the executing frame can
	// still bind locals. Everything strictly below is a candidate for dormancy.
	active := n
	if n > 0 {
		top := stack[n-1]
		for active > 0 && stack[active-1] == top {
			active--
		}
	}

	// Retract committed frames that no longer occupy their recorded slot (the
	// stack popped or a frame was recycled) or that now fall inside the active
	// region (the frame resumed execution). LIFO guarantees a stale frame is
	// always at the top of the committed prefix.
	for len(exec.dormant) > 0 {
		d := exec.dormant[len(exec.dormant)-1]
		if d.startSlot+d.slotCount > active || d.startSlot >= n || stack[d.startSlot] != d.env {
			exec.dormantBytes -= d.bytes
			exec.dormantSlots -= d.slotCount
			delete(exec.dormantSet, d.env)
			exec.dormant[len(exec.dormant)-1] = dormantFrame{}
			exec.dormant = exec.dormant[:len(exec.dormant)-1]
			continue
		}
		break
	}

	// Commit forward over committable frames between the prefix and the active
	// region, coalescing each distinct env's consecutive duplicate slots. Stop at
	// the first non-committable frame so the committed prefix stays contiguous and
	// bottom-anchored.
	for i := exec.dormantSlots; i < active; {
		env := stack[i]
		j := i + 1
		for j < active && stack[j] == env {
			j++
		}
		bytes, ok := committableFrameBytes(env)
		if !ok {
			break
		}
		if exec.dormantSet == nil {
			exec.dormantSet = make(map[*Env]struct{})
		}
		exec.dormant = append(exec.dormant, dormantFrame{
			env:       env,
			startSlot: i,
			slotCount: j - i,
			bytes:     bytes,
		})
		exec.dormantSet[env] = struct{}{}
		exec.dormantBytes += bytes
		exec.dormantSlots = j
		i = j
	}
}

// currentDormantSet returns the committed dormant set to deduplicate a walk
// against, or nil when the optimization is disengaged (a non-base-parent scope is
// active). It reflects the current stack only between topology changes, which is
// exactly when a memoized session reuses it.
func (exec *Execution) currentDormantSet() map[*Env]struct{} {
	if exec.nonBaseParentDepth != 0 {
		return nil
	}
	return exec.dormantSet
}

// retractDormantBeyond retracts every committed frame with a slot at or beyond
// length. popEnv calls it as it shrinks the stack so a frame leaves the committed
// set the instant any of its slots is popped, never after. This keeps the
// committed set a strict prefix of the live stack and closes the env-recycle
// window: a popped frame's pointer can be recycled into a new frame, so leaving it
// committed would let the reused pointer be mistaken for the retired one and be
// charged the retired frame's stale byte total.
func (exec *Execution) retractDormantBeyond(length int) {
	for len(exec.dormant) > 0 {
		d := exec.dormant[len(exec.dormant)-1]
		if d.startSlot+d.slotCount <= length {
			return
		}
		exec.dormantBytes -= d.bytes
		exec.dormantSlots -= d.slotCount
		delete(exec.dormantSet, d.env)
		exec.dormant[len(exec.dormant)-1] = dormantFrame{}
		exec.dormant = exec.dormant[:len(exec.dormant)-1]
	}
}

// retractAllDormant drops the entire committed prefix, returning the estimator to
// a plain full-stack walk. A scope whose parent is not a base env could rebind a
// dormant frame, so once one is live no frame's contribution is provably stable
// and none may stay committed.
//
// pushEnv calls it as such a scope enters the stack rather than leaving it to the
// walk that reads nonBaseParentDepth, because most walks never reach that read:
// beginBaseWalk routes past envStackGraphBytes whenever a builtin is live, and a
// builtin driving a script block is the commonest way to put a rebinding scope
// on the stack at all (#20).
// envStackGraphBytes still calls it so a walk that does get there first cannot
// serve a prefix the push has not yet retracted.
func (exec *Execution) retractAllDormant() {
	if len(exec.dormant) == 0 {
		return
	}
	clear(exec.dormant)
	exec.dormant = exec.dormant[:0]
	exec.dormantBytes = 0
	exec.dormantSlots = 0
	clear(exec.dormantSet)
}

// envStackGraphBytes charges the env stack's portion of the graph walk. With no
// non-base-parent scope active it reconciles and skips the committed dormant
// prefix, walking only the active suffix and adding the prefix's cached byte sum;
// est.dormant is pointed at the committed set so the root walk, module walk, and
// any later extras walk on the shared estimator deduplicate against dormant
// frames (charging them zero) rather than double-counting them. Otherwise it
// retracts everything and walks the full stack, exactly as the pre-optimization
// estimator did.
func (exec *Execution) envStackGraphBytes(est *memoryEstimator) int {
	if exec.nonBaseParentDepth != 0 {
		exec.retractAllDormant()
		est.dormant = nil
		total := 0
		for _, env := range exec.envStack {
			total += est.env(env)
		}
		return total
	}

	exec.reconcileDormant()
	est.dormant = exec.dormantSet
	total := exec.dormantBytes
	for _, env := range exec.envStack[exec.dormantSlots:] {
		total += est.env(env)
	}
	return total
}
