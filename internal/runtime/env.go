package runtime

import (
	"maps"
	"reflect"

	"github.com/mgomes/vibescript/vibes/value"
)

const inlineEnvBindingCapacity = 3

type envBinding struct {
	name  string
	value Value
}

// Env represents a lexical scope that maps variable names to values.
//
// Bindings live in two stores: inline holds small normal script scopes without
// a map allocation, values holds larger normal script scopes, and
// statics holds bindings whose deep size never changes after definition
// (builtins, per-call function clones). Statics are stored separately so
// memory-quota estimation can account for them in O(1) through the
// staticBytes counter instead of re-walking every binding on each check
// -- the root env's builtin set dominated estimation cost otherwise.
type Env struct {
	parent             *Env
	mutationVersion    uint64
	inline             [inlineEnvBindingCapacity]envBinding
	inlineLen          uint8
	values             map[string]Value
	statics            map[string]Value
	staticBytes        int32
	arrayAppendBuffers map[string][]Value
	assignBoundary     bool
	rebindOuter        bool
	classBody          bool

	// frozen marks engine-shared scopes (the builtin proto). Their
	// bindings are readable through the chain but never written:
	// assignments to names found here rebind in the nearest call-local
	// scope instead, exactly as if the binding lived in the call root.
	frozen bool

	// callRoot marks the per-call ambient root: the scope that holds a
	// Script.Call's globals, capabilities, and per-call function clones,
	// chained beneath the engine-shared builtin proto. The inbound rebinder
	// uses this marker to re-root an escaped closure's captured environment
	// onto the live call. The marker is preserved when a closure is cloned
	// across the host boundary so it survives re-entry.
	callRoot bool

	// callBlock holds the block supplied to the call that owns this scope, and
	// hasCallBlock marks the scope as a call frame that carries one (the block
	// is nil when the call received none). The block lives in this dedicated
	// slot rather than a named binding so yield and block_given? read the
	// enclosing call's actual block through the lexical chain without a script
	// identifier (for example a parameter named __block__) being able to shadow
	// it. lookupCallBlock walks to the nearest scope with hasCallBlock set,
	// stopping at the owning call frame just as a named lookup would, while
	// nested non-call scopes (if/while/rescue bodies) leave the flag clear and
	// transparently chain to it.
	callBlock    Value
	hasCallBlock bool

	// poisoned marks a recycled call frame in env-recycle verification builds
	// (envRecycleVerify). Production leaves it false. When verification is on, a
	// recycled env is poisoned and retained instead of reused, so any access to a
	// frame the recycler wrongly judged dead panics loudly instead of silently
	// reading stale bindings — turning a missed capture site into a test failure.
	poisoned bool

	// epochNeutral marks a scope that sits inside an active block-iteration
	// region (see memory_blockregion.go): the block's own parameter and local
	// scopes, which the region's per-check base walk re-walks fresh every time
	// rather than folding into the memoized prefix. Because such a scope is
	// always re-measured, a binding write confined to it cannot make the
	// memoized prefix stale, so those writes skip the mutation-epoch bump that
	// would otherwise invalidate the memo every iteration. Writes that escape
	// the scope — an outer-variable rebind resolved up the parent chain, or a
	// mutation of a container reachable from the prefix — land on a non-neutral
	// scope (or go through the value package) and still bump, so the prefix is
	// invalidated exactly when it truly changes.
	//
	// This flag governs mutation neutrality only. The structural push/pop
	// bookkeeping a region scope skips (baseTopoVersion and nonBaseParentDepth)
	// is keyed on exec.blockRegionActive, not on this flag, because push and pop
	// of any one scope always observe the same region state (regions nest fully
	// within env scopes). Keeping the two concerns separate lets a capture strip
	// neutrality mid-region (see revokeBlockRegionNeutrality) without unbalancing
	// the counters the pop must mirror.
	epochNeutral bool

	// neutralityRevoked records that a closure captured this scope while it was
	// epoch-neutral, so revokeBlockRegionNeutrality stripped neutrality to keep
	// its later writes charged. A capture is a property of the frame for its
	// whole lifetime, so the flag is sticky: it survives the frame's push/pop
	// cycles (a call frame is pushed once for its pre-body memory check and again
	// for its body, and the revocation happens during pre-push binding before
	// either), and pushEnv must not restore neutrality it sees revoked. It is
	// reset only per frame lifetime, at acquisition (markRegionNeutral) — never at
	// pop — so a body push cannot re-neutralize an escaped frame.
	neutralityRevoked bool
}

func (e *Env) bumpMutationVersion() {
	e.mutationVersion++
	value.BumpLocalMutationEpoch()
}

// bumpEpochUnlessNeutral advances this environment's mutation version for a binding
// write to this scope, unless the scope is epoch-neutral — inside an active
// block-iteration region, where every check re-walks the scope fresh so its own
// binding writes cannot stale the memoized prefix (see the epochNeutral field
// and memory_blockregion.go). Only writes whose target is this scope may use it;
// writes that mutate shared value state (through the value package) or resolve
// to an outer scope bump unconditionally.
func (e *Env) bumpEpochUnlessNeutral() {
	if !e.epochNeutral {
		e.bumpMutationVersion()
	}
}

// markRegionNeutral initializes a freshly acquired or created call frame's
// region-neutrality state before it is pushed. Inside an active block-iteration
// region the frame's pre-push argument and default binding writes are
// epoch-neutral (the region walk re-measures the frame every check); outside one
// they are not. It clears any stale revocation inherited from a prior tenant of
// a recycled frame, so pushEnv's neutrality restore is governed only by a
// capture that happens after this point (see neutralityRevoked and pushEnv).
func (e *Env) markRegionNeutral(active bool) {
	e.epochNeutral = active
	e.neutralityRevoked = false
}

// envRecycleVerify enables env-recycle verification: recycled call frames are
// poisoned and never reused, and accessing one panics. It is enabled only by
// tests (see the env-recycle verification test), so production pays a single
// predictable branch on the false path.
var envRecycleVerify = false

// assertNotPoisoned panics if e is a poisoned (recycled) env while verification
// is enabled, so a stale reference to a recycled frame is caught at the point of
// use instead of silently reading or writing a dead scope.
//
// For the tripwire to be sound it must guard every path that touches a scope's
// own storage. Rather than annotate all ~40 Env methods, the guard sits on the
// few primitives every access funnels through: inlineIndex for all name-keyed
// inline/values reads and writes (Get, Assign, has-checks, define, delete all
// route through it), rangeDynamicBindings and rangeStaticBindings for whole-scope
// iteration, setArrayAppendBuffer and settleArrayAppendResult for the concat
// accumulator map, and setCallBlock/lookupCallBlock for the call-block slot.
// Every other accessor reaches one of these before it can observe a binding, so
// a poisoned scope trips the guard no matter how it is reached.
func (e *Env) assertNotPoisoned() {
	if envRecycleVerify && e.poisoned {
		panic("runtime: access to recycled environment")
	}
}

func newEnv(parent *Env) *Env {
	return newEnvWithCapacity(parent, 0)
}

func newEnvWithCapacity(parent *Env, capacity int) *Env {
	if capacity < 0 {
		capacity = 0
	}
	env := &Env{parent: parent}
	if capacity > inlineEnvBindingCapacity {
		env.values = make(map[string]Value, capacity)
	}
	return env
}

// newAssignmentBoundaryEnv can read parent bindings, but writes stop in this
// scope instead of escaping into an outer mutable scope.
func newAssignmentBoundaryEnv(parent *Env) *Env {
	env := newEnv(parent)
	env.assignBoundary = true
	return env
}

func newBlockAssignmentEnv(parent *Env) *Env {
	env := newAssignmentBoundaryEnv(parent)
	env.rebindOuter = true
	return env
}

// resetForReuse clears every binding and flag and rebinds the scope to parent so
// the Env struct can back a fresh block invocation or function call without a new
// allocation. It is the reuse path for both block-iteration scopes (see
// blockCallRunner) and recyclable function call frames (see acquireCallEnv); a
// scope is only reused when static analysis proves its body cannot capture it.
func (e *Env) resetForReuse(parent *Env) {
	// A reused scope is one static analysis proved its body cannot capture, so
	// while it is being prepared here it is off the env stack and unreachable
	// from any root; clearing it changes nothing a memory check can observe.
	// The bump is therefore only meaningful once the scope is pushed, and it is
	// skipped for an epoch-neutral block-iteration scope the caller has already
	// marked (see blockCallRunner and memory_blockregion.go), whose every check
	// re-walks it fresh regardless.
	e.bumpEpochUnlessNeutral()
	e.parent = parent
	for i := range int(e.inlineLen) {
		e.inline[i] = envBinding{}
	}
	e.inlineLen = 0
	clear(e.values)
	e.statics = nil
	e.staticBytes = 0
	e.arrayAppendBuffers = nil
	e.assignBoundary = false
	e.rebindOuter = false
	e.classBody = false
	e.frozen = false
	e.callRoot = false
	e.callBlock = Value{}
	e.hasCallBlock = false
	e.poisoned = false
}

// Get looks up a variable by name, traversing parent scopes if needed.
func (e *Env) Get(name string) (Value, bool) {
	val, _, ok := e.getWithScope(name)
	return val, ok
}

// getWithScope resolves name and also returns the scope that holds the binding,
// so a caller can act on that exact scope without walking the chain a second
// time. The scope is where any hidden array-append accumulator for name lives
// (buffers are only ever registered on a binding's own scope), which is what
// getEscaping uses to settle in a single walk.
func (e *Env) getWithScope(name string) (Value, *Env, bool) {
	var lastMutable *Env
	for scope := e; scope != nil; scope = scope.parent {
		if !scope.frozen {
			lastMutable = scope
		}
		if val, ok := scope.getBoundValue(name, lastMutable); ok {
			return val, scope, true
		}
	}
	return Value{}, nil, false
}

// getEscaping resolves name as an escaping variable reference, settling any
// hidden array-append accumulator on the binding scope in the same walk. It
// replaces a Get followed by clearArrayAppendBuffer, which walked the chain
// twice — once to read, once to relocate the binding scope to settle it. The
// settle is byte-for-byte the same: getWithScope returns the nearest binding
// scope, exactly the scope clearArrayAppendBuffer's lookupBindingScope would
// find, and dropArrayAppendBuffer on a scope with no buffer is a nil-map no-op.
func (e *Env) getEscaping(name string) (Value, bool) {
	val, scope, ok := e.getWithScope(name)
	if ok {
		scope.dropArrayAppendBuffer(name)
	}
	return val, ok
}

func (e *Env) getCallLocal(name string) (Value, bool) {
	for scope := e; scope != nil; scope = scope.parent {
		if scope.callRoot || scope.frozen {
			break
		}
		if val, ok := scope.getBoundValue(name, nil); ok {
			return val, true
		}
		if scope.classBody {
			break
		}
	}
	return Value{}, false
}

func (e *Env) hasCallLocalBinding(name string) bool {
	for scope := e; scope != nil; scope = scope.parent {
		if scope.callRoot || scope.frozen {
			return false
		}
		if scope.hasOwnBinding(name) {
			return true
		}
		if scope.classBody {
			return false
		}
	}
	return false
}

func (e *Env) getBoundValue(name string, lastMutable *Env) (Value, bool) {
	if idx, ok := e.inlineIndex(name); ok {
		val := e.inline[idx].value
		if lazy, ok := lazyValue(val); ok {
			e.bumpMutationVersion()
			previous := val
			val = lazy.materialize()
			publishBindingReplacement(previous, val)
			e.inline[idx].value = val
			e.dropArrayAppendBuffer(name)
		}
		return val, true
	}
	if val, ok := e.values[name]; ok {
		if lazy, ok := lazyValue(val); ok {
			e.bumpMutationVersion()
			previous := val
			val = lazy.materialize()
			publishBindingReplacement(previous, val)
			e.values[name] = val
			e.dropArrayAppendBuffer(name)
		}
		return val, true
	}
	if val, ok := e.statics[name]; ok {
		val = e.materializeStatic(name, val)
		if e.frozen && lastMutable != nil && builtinNeedsCallClone(val) {
			cloned := cloneBuiltinValueForCall(val)
			lastMutable.DefineStatic(name, cloned)
			return cloned, true
		}
		return val, true
	}
	return Value{}, false
}

func (e *Env) getSkipping(name string, skip map[*Env]struct{}) (Value, bool) {
	var lastMutable *Env
	for scope := e; scope != nil; scope = scope.parent {
		if _, skipped := skip[scope]; skipped {
			continue
		}
		if !scope.frozen {
			lastMutable = scope
		}
		if idx, ok := scope.inlineIndex(name); ok {
			val := scope.inline[idx].value
			if lazy, ok := lazyValue(val); ok {
				scope.bumpMutationVersion()
				previous := val
				val = lazy.materialize()
				publishBindingReplacement(previous, val)
				scope.inline[idx].value = val
				scope.dropArrayAppendBuffer(name)
			}
			return val, true
		}
		if val, ok := scope.values[name]; ok {
			if lazy, ok := lazyValue(val); ok {
				scope.bumpMutationVersion()
				previous := val
				val = lazy.materialize()
				publishBindingReplacement(previous, val)
				scope.values[name] = val
				scope.dropArrayAppendBuffer(name)
			}
			return val, true
		}
		if val, ok := scope.statics[name]; ok {
			val = scope.materializeStatic(name, val)
			if scope.frozen && lastMutable != nil && builtinNeedsCallClone(val) {
				cloned := cloneBuiltinValueForCall(val)
				lastMutable.DefineStatic(name, cloned)
				return cloned, true
			}
			return val, true
		}
	}
	return Value{}, false
}

// setCallBlock marks this scope as a call frame and records the block supplied
// to the call (nil when none was given). It must be set on the call
// environment so lookupCallBlock can find it from any nested scope.
func (e *Env) setCallBlock(block Value) {
	e.assertNotPoisoned()
	e.bumpEpochUnlessNeutral()
	e.callBlock = block
	e.hasCallBlock = true
}

// lookupCallBlock returns the block supplied to the nearest enclosing call
// frame and whether such a frame exists. It walks parent scopes until it finds
// one marked as a call frame, so a yield or block_given? deep inside an
// if/while/rescue body resolves to its method's block, while a nested call's
// own frame shadows the caller's. The block is never resolved by name, so a
// script binding cannot intercept it.
func (e *Env) lookupCallBlock() (Value, bool) {
	for scope := e; scope != nil; scope = scope.parent {
		scope.assertNotPoisoned()
		if scope.hasCallBlock {
			return scope.callBlock, true
		}
	}
	return Value{}, false
}

// Define binds a new variable in the current scope.
func (e *Env) Define(name string, val Value) {
	e.setDynamic(name, val)
	e.dropStatic(name)
	e.dropArrayAppendBuffer(name)
}

func (e *Env) PredeclareLocal(name string) {
	if name == "" || e.hasOwnBinding(name) {
		return
	}
	if e.parent != nil && e.parent.hasEnclosingLocalBinding(name) {
		return
	}
	e.Define(name, NewNil())
}

func (e *Env) PredeclareAssignmentLocal(name string) {
	if name == "" || e.hasOwnBinding(name) {
		return
	}
	if e.parent != nil {
		if e.parent.hasEnclosingLocalBinding(name) || e.parent.hasAmbientAssignmentBinding(name) {
			return
		}
	}
	e.Define(name, NewNil())
}

// growStatics pre-sizes the statics map for n upcoming DefineStatic
// calls so bulk binding (builtins, per-call function clones) does not
// rehash repeatedly.
func (e *Env) growStatics(n int) {
	if e.statics == nil {
		e.bumpEpochUnlessNeutral()
		e.statics = make(map[string]Value, n)
	}
}

// DefineStatic binds a variable whose deep size is fixed at definition
// time, keeping it out of the per-check estimation walk.
func (e *Env) DefineStatic(name string, val Value) {
	publishBindingReplacement(e.statics[name], val)
	e.bumpEpochUnlessNeutral()
	e.deleteDynamic(name)
	if e.statics == nil {
		e.statics = make(map[string]Value)
	}
	if _, exists := e.statics[name]; !exists {
		e.staticBytes += int32(staticEntryBytes(name))
	}
	e.statics[name] = val
}

// Assign updates an existing variable in the nearest enclosing scope.
// Names not bound anywhere are defined in the outermost mutable scope,
// and names found in a frozen scope rebind in the nearest mutable scope
// below it, so engine-shared bindings are never written.
func (e *Env) Assign(name string, val Value) bool {
	e.assignValue(name, val)
	return true
}

func (e *Env) assignArrayAppendBuffer(name string, val Value, buffer []Value) bool {
	scope := e.assignValueWithAppendBufferHandling(name, val, false)
	scope.setArrayAppendBuffer(name, buffer)
	return true
}

func (e *Env) assignValue(name string, val Value) *Env {
	return e.assignValueWithAppendBufferHandling(name, val, true)
}

func (e *Env) assignValueWithAppendBufferHandling(name string, val Value, dropAppendBuffer bool) *Env {
	last := e
	for scope := e; scope != nil; scope = scope.parent {
		if scope.frozen {
			inValues := scope.hasDynamic(name)
			_, inStatics := scope.statics[name]
			if inValues || inStatics {
				last.setDynamic(name, val)
				last.dropStatic(name)
				if dropAppendBuffer {
					last.dropArrayAppendBuffer(name)
				}
				return last
			}
			continue
		}
		if scope.setExistingDynamic(name, val) {
			if dropAppendBuffer {
				scope.dropArrayAppendBuffer(name)
			}
			return scope
		}
		if _, ok := scope.statics[name]; ok {
			// The binding is no longer immutable-by-binding; demote it
			// so estimation starts walking its (now mutable) value.
			scope.dropStatic(name)
			scope.setDynamic(name, val)
			if dropAppendBuffer {
				scope.dropArrayAppendBuffer(name)
			}
			return scope
		}
		if scope.assignBoundary {
			if scope.rebindOuter && scope.parent != nil {
				if target, ok := scope.parent.assignExistingValue(name, val); ok {
					return target
				}
			}
			scope.setDynamic(name, val)
			scope.dropStatic(name)
			if dropAppendBuffer {
				scope.dropArrayAppendBuffer(name)
			}
			return scope
		}
		last = scope
	}
	last.setDynamic(name, val)
	last.dropStatic(name)
	if dropAppendBuffer {
		last.dropArrayAppendBuffer(name)
	}
	return last
}

func (e *Env) assignExistingValue(name string, val Value) (*Env, bool) {
	last := e
	for scope := e; scope != nil; scope = scope.parent {
		if scope.frozen {
			inValues := scope.hasDynamic(name)
			_, inStatics := scope.statics[name]
			if inValues || inStatics {
				last.setDynamic(name, val)
				last.dropStatic(name)
				last.dropArrayAppendBuffer(name)
				return last, true
			}
			continue
		}
		if scope.setExistingDynamic(name, val) {
			scope.dropArrayAppendBuffer(name)
			return scope, true
		}
		if _, ok := scope.statics[name]; ok {
			scope.dropStatic(name)
			scope.setDynamic(name, val)
			scope.dropArrayAppendBuffer(name)
			return scope, true
		}
		if scope.assignBoundary && !scope.rebindOuter {
			return nil, false
		}
		last = scope
	}
	return nil, false
}

func (e *Env) arrayAppendBuffer(name string) ([]Value, bool) {
	scope, ok := e.lookupBindingScope(name)
	if !ok || scope.arrayAppendBuffers == nil {
		return nil, false
	}
	buffer, ok := scope.arrayAppendBuffers[name]
	return buffer, ok
}

// clearArrayAppendBuffer settles the hidden concat accumulator for name when
// its wrapper escapes through a variable read. Settling only unregisters the
// buffer, so the binding keeps the exact wrapper the reader received. Once
// unregistered, the next `x = x + [...]` takes the copy path into a fresh
// buffer, so nothing ever appends into the escaped wrapper's backing again --
// which is what keeps the reuse invisible now that the escaped value and the
// binding are two values.
func (e *Env) clearArrayAppendBuffer(name string) {
	if scope, ok := e.lookupBindingScope(name); ok {
		scope.dropArrayAppendBuffer(name)
	}
}

func (e *Env) lookupBindingScope(name string) (*Env, bool) {
	for scope := e; scope != nil; scope = scope.parent {
		if scope.hasDynamic(name) {
			return scope, true
		}
		if _, ok := scope.statics[name]; ok {
			return scope, true
		}
	}
	return nil, false
}

func (e *Env) setArrayAppendBuffer(name string, buffer []Value) {
	e.assertNotPoisoned()
	e.bumpEpochUnlessNeutral()
	if e.arrayAppendBuffers == nil {
		e.arrayAppendBuffers = make(map[string][]Value)
	}
	e.arrayAppendBuffers[name] = buffer
}

// settleArrayAppendResult settles a hidden concat accumulator whose wrapper
// escapes as a function or block result. It unregisters the matching buffer
// and returns the same wrapper, so the escaping result and the variable stay
// one Ruby object while no later fast-path concat can append into the
// escaped backing (the next `x = x + [...]` copies into a fresh buffer).
func (e *Env) settleArrayAppendResult(val Value) Value {
	if val.Kind() != KindArray {
		return val
	}
	items := val.Array()
	if len(items) == 0 {
		return val
	}
	ptr := reflect.ValueOf(items).Pointer()
	if ptr == 0 {
		return val
	}
	for scope := e; scope != nil; scope = scope.parent {
		scope.assertNotPoisoned()
		for name, buffer := range scope.arrayAppendBuffers {
			if len(buffer) != len(items) || reflect.ValueOf(buffer).Pointer() != ptr {
				continue
			}
			scope.dropArrayAppendBuffer(name)
			return val
		}
	}
	return val
}

// visibleNames returns every name bound in this scope or any enclosing
// scope, reporting shadowed names once. It is intended for error-path
// suggestions and is never called on successful lookups.
func (e *Env) visibleNames() []string {
	seen := make(map[string]struct{})
	names := make([]string, 0, e.dynamicLen())
	add := func(name string) {
		if _, ok := seen[name]; ok {
			return
		}
		seen[name] = struct{}{}
		names = append(names, name)
	}
	for scope := e; scope != nil; scope = scope.parent {
		scope.rangeDynamicBindings(func(name string, _ Value) {
			add(name)
		})
		for name := range scope.statics {
			add(name)
		}
	}
	return names
}

// CloneShallow returns a copy of the environment with the same parent and a shallow copy of its bindings.
func (e *Env) CloneShallow() *Env {
	clone := newEnvWithCapacity(e.parent, e.dynamicLen())
	e.rangeDynamicBindings(func(name string, val Value) {
		clone.setDynamic(name, val)
	})
	if len(e.statics) > 0 {
		clone.statics = make(map[string]Value, len(e.statics))
		maps.Copy(clone.statics, e.statics)
		clone.staticBytes = e.staticBytes
	}
	clone.callBlock = e.callBlock
	clone.hasCallBlock = e.hasCallBlock
	clone.assignBoundary = e.assignBoundary
	clone.rebindOuter = e.rebindOuter
	clone.classBody = e.classBody
	return clone
}

type lazyEnvValue interface {
	materialize() Value
}

func (e *Env) defineLazy(name string, lazy lazyEnvValue) {
	e.setDynamic(name, newLazyValue(lazy))
	e.dropStatic(name)
	e.dropArrayAppendBuffer(name)
}

func (e *Env) dynamicLen() int {
	return int(e.inlineLen) + len(e.values)
}

func (e *Env) inlineIndex(name string) (int, bool) {
	e.assertNotPoisoned()
	for i := range int(e.inlineLen) {
		if e.inline[i].name == name {
			return i, true
		}
	}
	return 0, false
}

func (e *Env) hasDynamic(name string) bool {
	if _, ok := e.inlineIndex(name); ok {
		return true
	}
	_, ok := e.values[name]
	return ok
}

func (e *Env) hasOwnBinding(name string) bool {
	if e.hasDynamic(name) {
		return true
	}
	_, ok := e.statics[name]
	return ok
}

func (e *Env) hasEnclosingLocalBinding(name string) bool {
	for scope := e; scope != nil; scope = scope.parent {
		if scope.callRoot || scope.frozen {
			return false
		}
		if scope.hasOwnBinding(name) {
			return true
		}
		if scope.classBody {
			return false
		}
	}
	return false
}

func (e *Env) hasAmbientAssignmentBinding(name string) bool {
	for scope := e; scope != nil; scope = scope.parent {
		if !scope.callRoot && !scope.frozen {
			continue
		}
		if scope.hasDynamic(name) {
			return true
		}
		if val, ok := scope.statics[name]; ok {
			if _, lazy := lazyValue(val); lazy {
				return false
			}
			return val.Kind() != KindFunction
		}
	}
	return false
}

func (e *Env) getOwn(name string) (Value, bool) {
	if idx, ok := e.inlineIndex(name); ok {
		return e.inline[idx].value, true
	}
	if val, ok := e.values[name]; ok {
		return val, true
	}
	if val, ok := e.statics[name]; ok {
		return e.materializeStatic(name, val), true
	}
	return Value{}, false
}

func (e *Env) setExistingDynamic(name string, val Value) bool {
	if idx, ok := e.inlineIndex(name); ok {
		publishBindingReplacement(e.inline[idx].value, val)
		e.bumpEpochUnlessScalarRebind(e.inline[idx].value, val)
		e.inline[idx].value = val
		return true
	}
	if old, ok := e.values[name]; ok {
		publishBindingReplacement(old, val)
		e.bumpEpochUnlessScalarRebind(old, val)
		e.values[name] = val
		return true
	}
	return false
}

// bumpEpochUnlessScalarRebind advances the mutation epoch for a rebind of an
// existing binding, unless the binding goes from one compact scalar to another.
//
// The epoch exists solely to invalidate the estimator's base-walk memos (see
// beginBaseWalk and beginRegionBaseWalk, the only two readers), so a bump may be
// skipped exactly when the scope's contribution to an estimate cannot have
// changed. committableScalar names that class: its members cost exactly
// estimatedValueBytes with no marginal payload, and they are never deduplicated
// against another identity. Replacing one with another therefore leaves every
// memoized total bit-identical -- no identity enters or leaves the reachable
// union, and the byte count is equal by construction -- so the estimate stays
// exact rather than merely conservative.
//
// This matters because an accumulator in a block body (total = total + x)
// resolves past the block-iteration region boundary to a prefix scope, which is
// not epoch-neutral. Bumping there invalidated the memoized prefix on every
// iteration and re-walked the receiver collection, restoring the O(n^2) the
// region memo removes. Any rebind that is not scalar-to-scalar still bumps, so
// a growing or aliasing value invalidates the memo exactly as before.
func (e *Env) bumpEpochUnlessScalarRebind(old, val Value) {
	if e.epochNeutral {
		return
	}
	if committableScalar(old) && committableScalar(val) {
		return
	}
	e.bumpMutationVersion()
}

func (e *Env) setDynamic(name string, val Value) {
	if e.setExistingDynamic(name, val) {
		return
	}
	publishCollection(val)
	e.bumpEpochUnlessNeutral()
	if e.values != nil {
		e.values[name] = val
		return
	}
	if int(e.inlineLen) < len(e.inline) {
		e.inline[e.inlineLen] = envBinding{name: name, value: val}
		e.inlineLen++
		return
	}
	e.promoteInlineBindings(int(e.inlineLen) + 1)
	e.values[name] = val
}

func (e *Env) deleteDynamic(name string) {
	e.bumpEpochUnlessNeutral()
	if idx, ok := e.inlineIndex(name); ok {
		last := int(e.inlineLen) - 1
		copy(e.inline[idx:last], e.inline[idx+1:int(e.inlineLen)])
		e.inline[last] = envBinding{}
		e.inlineLen--
		return
	}
	delete(e.values, name)
}

func (e *Env) promoteInlineBindings(capacity int) {
	if capacity < int(e.inlineLen) {
		capacity = int(e.inlineLen)
	}
	if e.values == nil {
		e.values = make(map[string]Value, capacity)
	}
	for i := range int(e.inlineLen) {
		binding := e.inline[i]
		e.values[binding.name] = binding.value
		e.inline[i] = envBinding{}
	}
	e.inlineLen = 0
}

func (e *Env) rangeDynamicBindings(visit func(string, Value)) {
	e.assertNotPoisoned()
	for i := range int(e.inlineLen) {
		binding := e.inline[i]
		visit(binding.name, binding.value)
	}
	for name, val := range e.values {
		visit(name, val)
	}
}

// rangeDynamicBindingsWhile visits bindings until visit returns false. A
// bounded walk needs to stop iterating, not merely ignore what it is handed:
// an environment with many bindings would otherwise cost one callback each
// however little budget remains.
func (e *Env) rangeDynamicBindingsWhile(visit func(string, Value) bool) {
	e.assertNotPoisoned()
	for i := range int(e.inlineLen) {
		binding := e.inline[i]
		if !visit(binding.name, binding.value) {
			return
		}
	}
	for name, val := range e.values {
		if !visit(name, val) {
			return
		}
	}
}

// rangeStaticBindingsWhile visits statics until visit returns false. Stopping
// matters more here than for dynamic bindings, because materializing a static
// is real work the caller would otherwise pay for on every remaining name.
// rangeRawStaticBindingsWhile visits statics in their stored form, without
// materializing them, until visit returns false.
//
// Materializing is real work -- a call carrying several script functions
// stores them as lazy statics, and materializing one clones it -- so a scan
// that only inspects values must not trigger it. Callers that need the
// materialized value use rangeStaticBindings instead.
func (e *Env) rangeRawStaticBindingsWhile(visit func(string, Value) bool) {
	e.assertNotPoisoned()
	for name, val := range e.statics {
		if !visit(name, val) {
			return
		}
	}
}

func (e *Env) rangeStaticBindings(visit func(string, Value)) {
	e.assertNotPoisoned()
	for name, val := range e.statics {
		visit(name, e.materializeStatic(name, val))
	}
}

func (e *Env) materializeStatic(name string, val Value) Value {
	lazy, ok := lazyValue(val)
	if !ok {
		return val
	}
	e.bumpMutationVersion()
	materialized := lazy.materialize()
	publishBindingReplacement(val, materialized)
	e.statics[name] = materialized
	return materialized
}

func (e *Env) dropStatic(name string) {
	if e.statics == nil {
		return
	}
	if _, ok := e.statics[name]; !ok {
		return
	}
	e.bumpEpochUnlessNeutral()
	delete(e.statics, name)
	e.staticBytes -= int32(staticEntryBytes(name))
	if len(e.statics) == 0 {
		e.statics = nil
	}
}

func (e *Env) dropArrayAppendBuffer(name string) {
	if e.arrayAppendBuffers == nil {
		return
	}
	e.bumpEpochUnlessNeutral()
	delete(e.arrayAppendBuffers, name)
	if len(e.arrayAppendBuffers) == 0 {
		e.arrayAppendBuffers = nil
	}
}

// staticEntryBytes is the estimation cost of one static binding: its map
// entry, name header and bytes, and the value header. Deep value size is
// intentionally excluded — static bindings are compile-time artifacts
// whose payloads do not count against script memory quotas.
func staticEntryBytes(name string) int {
	return estimatedMapEntryBytes + estimatedStringHeaderBytes + len(name) + estimatedValueBytes
}

func newLazyValue(lazy lazyEnvValue) Value {
	return value.NewValue(KindBuiltin, lazy)
}

func lazyValue(val Value) (lazyEnvValue, bool) {
	if val.Kind() != KindBuiltin {
		return nil, false
	}
	lazy, ok := val.Data().(lazyEnvValue)
	return lazy, ok
}
