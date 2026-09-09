package runtime

import (
	"context"
	"fmt"
	"math/rand"
	"sync"

	"github.com/mgomes/vibescript/vibes/source"
)

type functionAccessorKind uint8

const (
	functionAccessorNone functionAccessorKind = iota
	functionAccessorGetter
	functionAccessorSetter
)

// ScriptFunction represents a user-defined function within a Vibescript module.
type ScriptFunction struct {
	Name         string
	Params       []Param
	ReturnTy     *TypeExpr
	Body         []Statement
	Pos          Position
	Env          *Env
	Exported     bool
	Private      bool
	Protected    bool
	Accessor     functionAccessorKind
	AccessorName string
	owner        *Script

	// reuseCallEnv records whether this function's call frame may be recycled
	// after the call returns (see functionCanReuseCallEnv). It is a pure function
	// of Params and Body, computed once at construction (compileFunctionDef and
	// the accessor builders) and never mutated afterward, so it is read lock-free
	// during calls without racing a ScriptFunction shared across goroutines.
	// Struct-copy clones (cloneFunctionForEnv, cloneFunctionForHostWithState)
	// inherit the flag; a body clone preserves capture structure, so the flag
	// stays valid. The zero value is false, which disables reuse — the safe
	// default for any construction site that forgets to set it.
	reuseCallEnv bool
}

// Script represents a parsed Vibescript module ready for execution.
type Script struct {
	engine              *Engine
	entrypoint          string
	functions           map[string]*ScriptFunction
	classes             map[string]*ClassDef
	classOrder          []string
	deferredClassBodies map[string]struct{}
	enums               map[string]*EnumDef
	symbolLiterals      map[*SymbolLiteral]Value
	source              string
	// codeFrames lazily splits source into lines for error code frames.
	// FormatCodeFrame rebuilt that split on every error, and member lookups
	// construct errors on paths that discard them -- respond_to? probes the
	// typed table before falling back to the universal helper -- so a loop
	// over `"x".respond_to?(:missing)` re-split the whole source per
	// iteration: 200 calls allocated 127 MiB on a 240 KB source, none of it
	// visible to the script's memory quota (#5). One split per script makes
	// every later frame cost the frame rather than the file.
	codeFrames     *source.CodeFrameFormatter
	codeFramesOnce sync.Once
	moduleKey      string
	modulePath     string
	moduleRoot     string
}

// CallOptions configures globals, capabilities, and other settings for a script invocation.
type CallOptions struct {
	Globals      map[string]Value
	Capabilities []CapabilityAdapter
	AllowRequire bool
	Keywords     map[string]Value
}

// Execution holds the runtime state for a single script evaluation.
type Execution struct {
	engine                *Engine
	script                *Script
	ctx                   context.Context
	quota                 int
	memoryQuota           int
	recursionCap          int
	steps                 int
	assignmentBaseDirty   bool
	assignmentSuffixDirty bool
	assignmentWalkNodes   int
	// predeclareScanDebt carries the nodes a predeclaration scan walked that
	// did not fill a whole step, so that many small scans still add up to one.
	// Truncating per scan lost them: a loop over a hundred empty rescue clauses
	// is a hundred separate walks that each round down to nothing.
	predeclareScanDebt int
	// exhausted latches the first genuine budget-exhaustion error (step
	// quota, memory quota, or output limit) raised on this execution. Once
	// set, step() fails immediately with it and no rescue clause matches any
	// error, so a script cannot absorb the signal that its budget is gone —
	// not even through a capability adapter that swallows the raw error. A
	// nil value means the budget is live. Recursion-cap errors, stdlib input
	// guards, and script-raised LimitErrors never latch: those describe one
	// rejected operation, not an exhausted sandbox.
	exhausted error
	// exhaustedWrapped snapshots the first RuntimeError wrapError built from
	// the latched exhaustion, deep-copied before any adapter can hold its
	// pointer; the dispatch rebuild uses only this copy for diagnostics.
	exhaustedWrapped *RuntimeError
	// addressed records the receiver the current in-place write reached through
	// the path that owns it, together with that path (see collection_values.go).
	// It vouches for exactly one call and is restored when that call returns, so
	// a later expression reaching the same wrapper another way cannot inherit
	// the permission, and the write can isolate again through the path when
	// script code has published the receiver since the permission was granted.
	addressed        addressedScope
	callStack        []callFrame
	root             *Env
	modules          map[string]Value
	moduleSearchPins map[string]string
	moduleLoading    map[string]bool
	moduleLoadStack  []string
	moduleStack      []moduleContext
	// initializingModules holds the environments of modules whose initialization
	// is in flight, so the estimator can reach what they are building before
	// require publishes it (see pushInitializingModule).
	initializingModules []*Env
	// outputWalkRoots holds the Go-local outputs of the builtins currently
	// accumulating results while calling back into script code, so every base
	// walk reaches what they have retained (see memory_output.go).
	outputWalkRoots []outputWalkRoot
	// outputWalkNodes counts the graph nodes the output roots have been walked
	// over since a driver last billed them (see chargeRetainedOutputWalk).
	outputWalkNodes           int
	outputCallInvalidated     bool
	estimatorWalkCharging     bool
	bindingOwner              *Script
	capabilityContracts       map[*Builtin]CapabilityMethodContract
	capabilityContractScopes  map[*Builtin]*capabilityContractScope
	capabilityContractsByName map[string]CapabilityMethodContract
	receiverStack             []Value
	envStack                  []*Env
	validatedCapabilityArgs   []string
	capabilityReturnProof     capabilityReturnProof
	// capabilityBoundRoots are the values capability adapters bound into
	// this call, and hostStateIdentities the wrapper identities reachable
	// from them (refreshed after every mutating host dispatch). Together
	// they drive the receiver-liveness rule and the immediate post-dispatch
	// marking that keeps host-installed wrappers from crossing out of the
	// Call live (#1210).
	capabilityBoundRoots []Value
	hostStateIdentities  map[uintptr]struct{}
	// hostStateRevisits counts uncharged re-walk visits of recorded host
	// state; every 64th is charged one scan step so total walk CPU stays
	// coupled to the step quota.
	hostStateRevisits    uint64
	isolationForwards    map[uintptr]isolationForward
	addressedBrackets    int
	memoryEst            memoryEstimator
	reservedScratchBytes int

	// stringScanCharge caches chargeEqualityScanBytes as a bound function for
	// the equality byte charge (see stringScanChargeFunc), so metered
	// comparisons do not allocate a method value each. Lazily initialized.
	stringScanCharge func(int) error
	// equalityScanResidue carries the sub-step remainder of equality byte
	// charges across comparisons (see chargeEqualityScanBytes).
	equalityScanResidue int
	// charSetProbeResidue carries the sub-step remainder of character-set
	// entry comparisons across segments (see chargeStringCharSetProbes).
	charSetProbeResidue int
	// destructureScanResidue carries the sub-step remainder of destructuring
	// work charges across one assignment's targets (see
	// chargeDestructureScan).
	destructureScanResidue int
	// equalityScratchCheck caches the equality scratch validator closure
	// (see equalityScratchValidatorFunc). Lazily initialized.
	equalityScratchCheck func(int, Value, Value) error
	// equalityScratchPriced is the walk-scratch total the validator last
	// priced against the memory quota, so scratch is repriced once per
	// granule rather than per allocation (see equalityScratchValidatorFunc).
	equalityScratchPriced int
	// equalityScratchReserved is the walk scratch currently reflected in
	// reservedScratchBytes, so the validator can keep the execution's
	// reservation in step with the walk's held total and retire it when the
	// comparison ends (see equalityScratchValidatorFunc).
	equalityScratchReserved int
	// equalityScratchRelease caches the walk-end callback paired with the
	// validator (see equalityScratchReleaseFunc). Lazily initialized.
	equalityScratchRelease func()
	// equalityCtx pools the metered comparison context behind `==` (see
	// equalValues). Lazily initialized; nil while a comparison is running or
	// after one failed.
	equalityCtx *EqualityContext

	// capabilityYields records the values a contracted capability builtin
	// hands to script blocks while it runs, so dispatch can bind contracts to
	// exactly what the capability published (see capabilityYieldFrame). nil
	// outside such a call.
	capabilityYields *capabilityYieldFrame

	// baseWalkCache memoizes the reachable-graph portion of the memory
	// estimator's base walk (see beginBaseWalk). It is allocated lazily on the
	// first memoizable check, so executions that never reach one — no memory
	// quota, or only memo-bypassed checks — pay neither the struct nor its
	// journal backing. baseWalkOpen marks a base-walk session in flight (both
	// memoized and bypass sessions), guarding against nested sessions on the
	// shared estimator without touching the cache. baseTopoVersion invalidates
	// the memo whenever the walk's root set changes shape (env stack push/pop);
	// the process-wide mutation epoch invalidates it
	// whenever any reachable state mutates. builtinDepth counts the Go builtins
	// currently on the call stack: while it is non-zero the memo is neither
	// used nor refreshed, because builtin Go code can mutate containers through
	// raw slice/map writes between its own memory checks without bumping the
	// epoch.
	baseWalkCache   *baseWalkCache
	baseWalkOpen    bool
	baseTopoVersion uint64
	builtinDepth    int

	// undeclaredBuiltinDepth counts the subset of those builtins that have not
	// declared non-mutation, and it is that subset -- not builtinDepth -- that
	// the memo consults. The reason the memo stands aside for a builtin is
	// stated just above: a Go body may write through a raw slice or map between
	// its own checks without advancing the epoch. A builtin that declares it
	// does not is precisely the case that reason excludes, so it leaves this
	// counter alone and checks inside it keep using the memo.
	//
	// It is kept separate from builtinDepth rather than folded into it because
	// builtinDepth means something else to the array-backing claims, the
	// capability yield frames and the block region's driver identity, none of
	// which are about unobserved writes. Nothing here changes what those see.
	undeclaredBuiltinDepth int

	// builtinFrameReceiver, builtinFrameArgs and builtinFrameKwargs are the
	// values the innermost builtin frame was dispatched with. They live on that
	// frame's Go stack, where no walk reaches them, so a block the frame drives
	// cannot otherwise see what its caller is still holding. CallBlock folds
	// them into the live baseline for the block's duration; see
	// callerRetainedRoots.
	builtinFrameReceiver Value
	builtinFrameArgs     []Value
	builtinFrameKwargs   map[string]Value
	// builtinFrameHostCrossing records that the innermost builtin frame is a
	// host-driven builtin that did not declare itself non-retaining, so a
	// CallBlock under it is a host boundary: yielded arguments and the
	// block's return value must cross as independent values (#1210).
	builtinFrameHostCrossing bool
	// builtinFrameRootsReserved records that a CallBlock under this frame has
	// already folded those values into the reserved scratch. A block that enters
	// a script function which yields reaches CallBlock again under the same
	// frame, and reserving a second time charged the same ephemeral argument
	// twice: a 2MB ignored default needed 6,005,407 bytes where the real peak is
	// 4,004,336. It is saved and restored with the frame values, so a nested
	// builtin frame still reserves its own.
	builtinFrameRootsReserved bool

	// Dormant-frame accounting (see memory_dormant.go) makes the estimator's
	// env-stack walk incremental for block-free function recursion. dormant holds
	// the committed dormant prefix, dormantSet the same frames for O(1) dedup
	// during a walk, dormantBytes their cached byte sum, and dormantSlots the
	// number of leading env-stack slots they cover. nonBaseParentDepth counts the
	// active scopes whose parent is not a base env (blocks, closures, nested
	// lexical scopes); while it is non-zero the optimization disengages because
	// such a scope could rebind a dormant frame. All are maintained only under a
	// positive memory quota, since an unlimited quota skips the estimator walk.
	dormant            []dormantFrame
	dormantSet         map[*Env]struct{}
	dormantBytes       int
	dormantSlots       int
	nonBaseParentDepth int

	// accumMeteredSections counts the accumulator-metered sections currently
	// active (see beginAccumulatorMeteredSection). While non-zero, step()'s
	// periodic slow path skips the full reachable-graph memory walk: an
	// active section is a blockless native build loop that charges every
	// allocation against the quota before performing it, so the walk's answer
	// cannot change while one runs. Script re-entry and nested builtin
	// dispatch suspend the counter (see callBlock, callFunctionWithBoundEnv,
	// and the builtin dispatch in invokeCallable), so a section can never
	// weaken checks across code it does not meter.
	accumMeteredSections int

	// Block-iteration region accounting (see memory_blockregion.go). A pure
	// block driver (map/select/group_by/reduce/each and kin) marks the
	// env-stack prefix beneath its block scopes as stable for the duration of
	// its iteration. While a region is active the per-check base walk memoizes
	// that prefix once and re-walks only the block's own scopes (the active
	// suffix) each check, instead of re-charging the whole — often large —
	// input collection every periodic check. That turns quota-metered block
	// iteration from O(n^2) back to O(n). blockRegionActive is the switch;
	// blockRegionBoundary is the env-stack length at the outermost active
	// region's start (scopes at or above it are the region, walked fresh);
	// blockRegionBuiltinDepth is builtinDepth at the innermost active region's
	// driver, so the memo is engaged only in the block body that region drives
	// and not inside a deeper builtin whose raw writes the epoch cannot
	// observe. All are maintained only under a positive memory quota.
	blockRegionActive       bool
	blockRegionBoundary     int
	blockRegionBuiltinDepth int

	// heldArrayBackings is the stack of array element backings the live
	// builtin frames are holding across the script code they invoke. See
	// array_shrink.go: an in-place shrink consults it to decide whether it may
	// zero the slots it vacates, or must leave them alone instead.
	//
	// It has no inline backing, unlike the always-used per-call stacks below.
	// A claim is only ever pushed for an array receiver or argument, so giving
	// every execution room for eight of them put 896 bytes on the shortest
	// possible call whether or not the script ever touched an array.
	heldArrayBackings []heldArrayBacking

	// Inline backing storage for the always-used per-call stacks, so a
	// fresh Execution costs one allocation instead of one per stack.
	// Appends beyond these capacities spill to the heap as usual.
	callStackArr               [8]callFrame
	receiverStackArr           [8]Value
	envStackArr                [8]*Env
	validatedCapabilityArgsArr [4]string
	loopDepth                  int
	blockDepth                 int
	rescueDepth                int
	rescuedErrors              []error
	returnTokens               []uint64
	homeTokens                 []uint64
	localCallBypassStack       []localCallBypass
	randSource                 *rand.Rand
	randSeed                   int64
	randSeeded                 bool
	strictEffects              bool
	allowRequire               bool
	callOptions                CallOptions

	// argBufferPool is a free list of positional-argument backing slices,
	// reused across script-function calls. A call to a script function borrows
	// a buffer to evaluate its arguments into and returns it once the call has
	// fully unwound; bindFunctionArgs copies every element into the callee's
	// environment and never retains the slice, so the backing is free to reuse.
	// Only KindFunction calls pool: builtins and capabilities can retain the
	// args slice they are handed.
	argBufferPool [][]Value

	// callEnvFreeList is a free list of call-frame environments, reused across
	// script-function calls whose bodies provably cannot capture the frame
	// (ScriptFunction.reuseCallEnv). acquireCallEnv borrows a frame and
	// recycleCallEnv returns it once the call has fully unwound and any escaping
	// array-append result has settled. Like argBufferPool it lives on the
	// Execution, which is single-goroutine, so the pool never needs
	// synchronization. The memory estimator does not walk it, so pooled dead
	// frames never inflate the quota.
	callEnvFreeList []*Env
}

type localCallBypass struct {
	bindings map[string]*Env
}

type capabilityContractScope struct {
	contracts     map[string]CapabilityMethodContract
	roots         []Value
	knownBuiltins map[*Builtin]struct{}
}

type moduleContext struct {
	key    string
	path   string
	root   string
	script *Script
}

type callFrame struct {
	Function       string
	Pos            Position
	callSiteScript *Script
	functionScript *Script
}

func (exec *Execution) pushReceiver(v Value) {
	exec.receiverStack = append(exec.receiverStack, v)
}

func (exec *Execution) popReceiver() {
	if len(exec.receiverStack) == 0 {
		return
	}
	exec.receiverStack = exec.receiverStack[:len(exec.receiverStack)-1]
}

func (exec *Execution) currentReceiver() Value {
	if len(exec.receiverStack) == 0 {
		return NewNil()
	}
	return exec.receiverStack[len(exec.receiverStack)-1]
}

func (exec *Execution) isCurrentReceiver(v Value) bool {
	cur := exec.currentReceiver()
	switch {
	case v.Kind() == KindInstance && cur.Kind() == KindInstance:
		return valueInstance(v) == valueInstance(cur)
	case v.Kind() == KindClass && cur.Kind() == KindClass:
		return valueClass(v) == valueClass(cur)
	default:
		return false
	}
}

// protectedInstanceAccessAllowed reports whether the currently executing
// method may invoke a protected instance method defined on cl: the caller's
// self must itself be an instance of cl, mirroring Ruby's rule that protected
// methods are callable between instances of the same class (Vibescript has no
// inheritance, so the class itself is the whole family).
func (exec *Execution) protectedInstanceAccessAllowed(cl *ClassDef) bool {
	cur := exec.currentReceiver()
	return cur.Kind() == KindInstance && valueInstance(cur).Class == cl
}

// protectedClassAccessAllowed reports whether the currently executing method
// may invoke a protected class method defined on cl: the caller's self must
// be the class itself, i.e. the call happens inside another class method of
// the same class.
func (exec *Execution) protectedClassAccessAllowed(cl *ClassDef) bool {
	cur := exec.currentReceiver()
	return cur.Kind() == KindClass && valueClass(cur) == cl
}

// assignTargetProtectedAccessAllowed applies the protected rules to a setter
// assignment target, which may be an instance (protected instance setter) or
// a class (protected class setter).
func (exec *Execution) assignTargetProtectedAccessAllowed(obj Value) bool {
	switch obj.Kind() {
	case KindInstance:
		return exec.protectedInstanceAccessAllowed(valueInstance(obj).Class)
	case KindClass:
		return exec.protectedClassAccessAllowed(valueClass(obj))
	default:
		return false
	}
}

func (exec *Execution) pushFrame(function string, pos Position, callSiteScript, functionScript *Script) error {
	if exec.recursionCap > 0 && len(exec.callStack) >= exec.recursionCap {
		return exec.newRuntimeErrorWithType(runtimeErrorTypeLimit, fmt.Sprintf("recursion depth exceeded (limit %d)", exec.recursionCap), pos)
	}
	exec.callStack = append(exec.callStack, callFrame{
		Function:       function,
		Pos:            pos,
		callSiteScript: callSiteScript,
		functionScript: functionScript,
	})
	return nil
}

func (exec *Execution) popFrame() {
	if len(exec.callStack) == 0 {
		return
	}
	exec.callStack = exec.callStack[:len(exec.callStack)-1]
}

func (exec *Execution) pushValidatedCapabilityArgs(method string) func() {
	exec.validatedCapabilityArgs = append(exec.validatedCapabilityArgs, method)
	return func() {
		exec.validatedCapabilityArgs = exec.validatedCapabilityArgs[:len(exec.validatedCapabilityArgs)-1]
	}
}

func (exec *Execution) capabilityArgsValidated(method string) bool {
	for i := len(exec.validatedCapabilityArgs) - 1; i >= 0; i-- {
		if exec.validatedCapabilityArgs[i] == method {
			return true
		}
	}
	return false
}

// capabilityReturnProof is the runtime-internal, unforgeable record that a
// capability builtin's return value has already been validated and isolated
// from host-owned state. Only code in this package can record one (via
// markValidatedCapabilityReturn); the builtin dispatcher clears the slot
// before invoking a builtin and consumes it afterwards, so a proof can only
// describe the exact builtin invocation the dispatcher is completing. Host
// adapters, which see CapabilityMethodContract through the public facade,
// have no way to assert this proof and therefore cannot skip their declared
// ValidateReturn.
type capabilityReturnProof struct {
	recorded bool
	method   string
	result   Value
}

// covers reports whether the proof vouches for exactly this method returning
// exactly this value. Representation identity for collections keeps the proof
// bound to the wrapper the first-party builtin actually validated: returning
// any other value — even an equal one rebuilt from unvalidated state — falls
// back to the contract's ValidateReturn.
func (p capabilityReturnProof) covers(method string, result Value) bool {
	return p.recorded && p.method == method && capabilityProofMatchesValue(p.result, result)
}

// capabilityProofMatchesValue reports representation identity for collections
// and language identity for immutable or runtime-only values.
func capabilityProofMatchesValue(proven, returned Value) bool {
	if proven.Kind() != returned.Kind() {
		return false
	}
	if !isCollection(proven) {
		return proven.Identical(returned)
	}
	id := collectionIdentity(proven)
	return id != 0 && id == collectionIdentity(returned)
}

// markValidatedCapabilityReturn records the internal proof that the currently
// executing first-party capability builtin has validated and isolated result
// (for example via cloneCapabilityMethodResult). The dispatcher consumes the
// proof after the builtin returns and skips the contract's ValidateReturn for
// that value only, so first-party adapters do not validate the same result
// twice. It is intended for the interpreter's internal use; nothing outside
// this package can call it.
func (exec *Execution) markValidatedCapabilityReturn(method string, result Value) {
	exec.capabilityReturnProof = capabilityReturnProof{
		recorded: true,
		method:   method,
		result:   result,
	}
}

func (exec *Execution) pushEnv(env *Env) {
	// The non-base-parent bookkeeping runs ahead of the region branch below so no
	// push can skip it. A block-iteration region re-walks every scope it pushes
	// fresh on each check, but not the frames those scopes can *reach*: a helper
	// that runs a pure iterator such as array.each over a block closed on its
	// caller rebinds a scalar in that caller, which may already be committed
	// dormant. With the region skipping this, nonBaseParentDepth stayed zero for
	// the whole iteration and the committed total was never retracted, so the
	// fixture below ran under a 404,951-byte quota while retaining two live
	// 400KB strings; it now needs the 804,361 bytes it actually holds (#19).
	if exec.memoryQuota > 0 && env != nil && !exec.isBaseEnv(env.parent) {
		exec.nonBaseParentDepth++
		// Retract at the push, not at the walk that reads the counter.
		// envStackGraphBytes is the only reader that retracts, and beginBaseWalk
		// routes around it whenever a Go builtin that has not declared
		// non-mutation is on the call stack — which is exactly the state a
		// builtin driving a script
		// block is in, so the retraction never ran for the scope that most needs
		// it. A block reached through `cb.call` rebound a dormant caller's compact
		// Int to a 400KB string: the assignment's own check took the bypass
		// reference walk and measured it correctly, then the next memoized check
		// in the caller charged 4030 bytes where the reference walk charged
		// 404046, because dormantBytes still held the frame's scalar-only 245 and
		// dormantSet skipped it (#20).
		exec.retractAllDormant()
	}
	if exec.blockRegionActive && env != nil {
		// Every scope pushed inside an active block-iteration region is part of
		// the region's active suffix: the base walk re-walks it fresh each check
		// (see memory_blockregion.go), so its binding writes are epoch-neutral
		// and its push does not change the memoized prefix. Skipping the topo
		// bump keeps the prefix memo valid across every iteration; popEnv skips
		// it symmetrically under the same blockRegionActive condition, so the
		// two stay balanced regardless of a mid-region capture stripping
		// neutrality.
		//
		// Restore neutrality unless a closure already captured this frame during
		// pre-push argument or default binding: revokeBlockRegionNeutrality
		// cleared it so the escaped frame's later writes stay charged, and
		// blindly re-neutralizing here would undercount them (a security-boundary
		// escape). The scope was marked neutral at acquisition, so this only
		// re-asserts it for the common uncaptured case.
		if !env.neutralityRevoked {
			env.epochNeutral = true
		}
		exec.envStack = append(exec.envStack, env)
		return
	}
	if !exec.pushingDuplicateTop(env) {
		exec.baseTopoVersion++
	}
	exec.envStack = append(exec.envStack, env)
}

// pushingDuplicateTop reports whether env is already the top of the env stack,
// so pushing it adds a second slot for a scope the stack already holds.
//
// A statement list evaluates in the scope it is handed rather than a fresh one,
// so a loop body re-pushes the enclosing scope on every iteration. That is a
// duplicate slot, not a new scope: the estimator charges each env once by
// identity (see memoryEstimator.env), so the reachable set and its byte total
// are the same before and after. Bumping the topology version for it invalidated
// the base-walk memo twice per iteration -- once on the push, once on the pop --
// and each miss re-walks the whole reachable graph, which is what made a loop
// under a memory quota quadratic in its own body's iteration count (#1130).
//
// popEnv applies the mirrored test, so the two stay balanced: a push that
// skipped the bump is popped by a pop that skips it too.
func (exec *Execution) pushingDuplicateTop(env *Env) bool {
	return env != nil && len(exec.envStack) > 0 && exec.envStack[len(exec.envStack)-1] == env
}

// poppingDuplicateTop reports whether the slot about to be popped holds the same
// scope as the slot beneath it, leaving the reachable set unchanged. It is the
// mirror of pushingDuplicateTop; see there for why the topology bump is skipped.
func (exec *Execution) poppingDuplicateTop() bool {
	n := len(exec.envStack)
	return n >= 2 && exec.envStack[n-1] != nil && exec.envStack[n-1] == exec.envStack[n-2]
}

func (exec *Execution) currentEnv() *Env {
	if len(exec.envStack) == 0 {
		return nil
	}
	return exec.envStack[len(exec.envStack)-1]
}

func (exec *Execution) popEnv() {
	if len(exec.envStack) == 0 {
		return
	}
	last := len(exec.envStack) - 1
	env := exec.envStack[last]
	// Mirrors pushEnv: the decrement is taken ahead of the region branch on the
	// same test as the increment, so the two pair up whatever the region state
	// was at either end.
	if exec.memoryQuota > 0 && env != nil && !exec.isBaseEnv(env.parent) {
		if estimatorVerify && exec.nonBaseParentDepth <= 0 {
			// This decrement must pair with an increment from the same scope's
			// push. An underflow means a scope was pushed and popped through
			// asymmetric branches — the failure mode when the region skip was keyed
			// on a flag a mid-region capture could revoke — which can later drive
			// the counter back to zero while a non-base scope is live and wrongly
			// re-engage the dormant-frame memo. Trip loudly under the oracle.
			panic("runtime: nonBaseParentDepth underflow on env pop")
		}
		exec.nonBaseParentDepth--
	}
	regionScope := exec.blockRegionActive && env != nil
	if regionScope {
		// A block-iteration region scope: its push skipped the topo bump (see
		// pushEnv), so its pop must too. The decision is keyed on
		// blockRegionActive, not the scope's epochNeutral flag, so it holds even
		// when a mid-region capture revoked neutrality — push and pop of one scope
		// always see the same region state, so the skips stay balanced. Clear the
		// transient neutrality so a scope that escapes the region (captured by a
		// closure and re-pushed later, perhaps outside any region) does not carry
		// it past the region that justified it.
		//
		// Do NOT clear neutralityRevoked here: a capture is a property of the frame
		// for its whole lifetime, not of a single push. A call frame is pushed
		// twice — once for the pre-body memory check, once for the body — and the
		// revocation, which happens during pre-push binding, must survive the
		// intervening pop or the body push would re-neutralize the escaped frame
		// and undercount its later writes. The flag is reset per lifetime at frame
		// acquisition (markRegionNeutral). Leaving it set at worst over-charges a
		// reused scope, never undercounts.
		env.epochNeutral = false
	} else if !exec.poppingDuplicateTop() {
		exec.baseTopoVersion++
	}
	exec.envStack[last] = nil
	// Growing the stack copies its embedded backing array, which remains
	// reachable through exec even after envStack moves to a larger allocation.
	if last < len(exec.envStackArr) {
		exec.envStackArr[last] = nil
	}
	exec.envStack = exec.envStack[:last]
	if !regionScope && len(exec.dormant) > 0 {
		exec.retractDormantBeyond(last)
	}
}

// pushInitializingModule roots a module's environment for the estimator while
// the module initializes. Until require finishes, that environment is only a Go
// local: its parent is exec.root, but the base walk goes root-outward, and the
// module's exports are not published to exec.modules until initialization
// returns. So every class and constant the module builds is invisible to the
// memory checks running inside it, and a required file could grow its class
// constants without limit while each check measured a graph that did not
// contain them (#23).
func (exec *Execution) pushInitializingModule(env *Env) {
	exec.baseTopoVersion++
	exec.initializingModules = append(exec.initializingModules, env)
}

func (exec *Execution) popInitializingModule() {
	last := len(exec.initializingModules) - 1
	if last < 0 {
		return
	}
	exec.baseTopoVersion++
	// Clear before shortening: a truncated slice keeps the pointer alive in its
	// backing array, so the env would stay reachable for the collector while the
	// walk, which reads only the visible length, stopped charging it.
	exec.initializingModules[last] = nil
	exec.initializingModules = exec.initializingModules[:last]
}

func (exec *Execution) pushModuleContext(ctx moduleContext) {
	exec.moduleStack = append(exec.moduleStack, ctx)
}

func (exec *Execution) popModuleContext() {
	if len(exec.moduleStack) == 0 {
		return
	}
	exec.moduleStack = exec.moduleStack[:len(exec.moduleStack)-1]
}

func (exec *Execution) currentModuleContext() *moduleContext {
	if len(exec.moduleStack) == 0 {
		return nil
	}
	ctx := exec.moduleStack[len(exec.moduleStack)-1]
	return &ctx
}

func (exec *Execution) currentSourceScript() *Script {
	if ctx := exec.currentModuleContext(); ctx != nil && ctx.script != nil {
		return ctx.script
	}
	return exec.script
}

func (exec *Execution) pushRescuedError(err error) {
	exec.rescuedErrors = append(exec.rescuedErrors, err)
}

func (exec *Execution) popRescuedError() {
	if len(exec.rescuedErrors) == 0 {
		return
	}
	exec.rescuedErrors = exec.rescuedErrors[:len(exec.rescuedErrors)-1]
}

func (exec *Execution) currentRescuedError() error {
	if len(exec.rescuedErrors) == 0 {
		return nil
	}
	return exec.rescuedErrors[len(exec.rescuedErrors)-1]
}

// Context returns the execution's bound context. Capability adapters
// that have been carved into sibling packages (vibes/capability/...)
// rely on it to forward cancellation and request-scoped values to host
// callbacks without reaching into unexported runtime fields.
func (exec *Execution) Context() context.Context {
	return exec.ctx
}

// Step accounts for one interpreter step against quota and memory
// limits and returns the deadline error when the script's context has
// been canceled. Capability adapters call it inside per-row loops so
// long-running host callbacks honor the same budget as in-script work.
func (exec *Execution) Step() error {
	return exec.step()
}

// codeFrameFormatter returns the script's line index, building it once. The
// formatter is read-only after construction, so concurrent calls share it.
func (s *Script) codeFrameFormatter() *source.CodeFrameFormatter {
	s.codeFramesOnce.Do(func() {
		s.codeFrames = source.NewCodeFrameFormatter(s.source)
	})
	return s.codeFrames
}
