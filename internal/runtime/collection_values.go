package runtime

import (
	"maps"
	"os"
	"slices"

	"github.com/mgomes/vibescript/vibes/value"
)

// Arrays and hashes are values (ADR-006 item 2). Binding, passing, or returning
// one produces another logical value, and updating one binding or path can
// never change a sibling. The runtime implements that with copy-on-write:
// naming a collection is free, and the copy is paid only where a write meets a
// wrapper something else can still see.
//
// Two facts carry the whole scheme.
//
// The first is the ref state on each wrapper (see vibes/value/collection_refs.go),
// a saturating count of the durable slots naming it. publishCollection raises it
// wherever a collection is placed somewhere that outlives the expression that
// produced it. Because it saturates and never decrements, it can over-count --
// costing a copy that was not strictly needed -- but never under-count, which
// would lose an alias.
//
// The second is addressability. A write may only mutate in place when it can
// name the one slot it is updating: a local, an instance or class variable, or a
// path of index and hash-member steps rooted at one of those. resolveMutablePath
// walks that path, copying and rebinding each level that is shared, so the leaf
// it returns is a wrapper only this path can reach. A receiver that is not
// addressable -- a call result, a literal, an instance accessor -- is a
// temporary: it is copied before any write, so the update is returned but
// changes nothing else, which is exactly what the ADR requires.
//
// Nothing about this is script-visible. Sharing a wrapper is a representation
// detail the memory estimator may still measure; the language sees only values.

// alwaysCopyCollections makes an addressable write copy and rebind its whole
// path unconditionally, instead of asking whether each level is exclusively
// held. Copy-per-write is the reference semantics the ref state optimizes, so
// running a suite with it on and off must produce identical results: a write
// that mutated in place because refs under-counted is exactly the one the oracle
// copies instead, and the two runs then disagree.
//
// It is the only oracle for this design's one bug class -- a store that creates
// a durable handle without publishing it -- because such a store is invisible
// until some later write happens to reach the wrapper it left uncounted. It is
// read once, in the runtime package's test TestMain, and is never on in
// production.
var alwaysCopyCollections = false

// maybeEnableCollectionCopyVerify turns on the always-copy oracle when
// VIBES_COW_ALWAYS_COPY is set. It is called from TestMain before any test
// runs, so the flag is write-once.
func maybeEnableCollectionCopyVerify() {
	if os.Getenv("VIBES_COW_ALWAYS_COPY") != "" {
		alwaysCopyCollections = true
	}
}

// isCollection reports whether val carries a wrapper that value semantics
// governs. Objects are included: a host attribute bag is script-mutable through
// the same index and member writes a hash is, so it owes the same isolation.
func isCollection(val Value) bool {
	switch val.Kind() {
	case KindArray, KindHash, KindObject:
		return true
	default:
		return false
	}
}

// publishCollection records that another durable slot now names val. Every site
// that stores a value somewhere outliving the current expression calls it: the
// environment's terminal writes, instance and class variable stores, hash entry
// stores, and the array builders that adopt elements they did not construct.
//
// It is deliberately cheap and total -- a kind check and at most one store --
// so that calling it at a site that turns out not to need it costs nothing but
// a copy that a later write might not have made, while omitting it at a site
// that does need it loses an alias.
func publishCollection(val Value) {
	if isCollection(val) {
		val.PublishRef()
	}
}

// prepareHostDrivenCollections applies value-semantics obligations a host
// or capability builtin owes its collection inputs: publish anything a
// retaining call may keep, then isolate anything a mutating call may write
// so a sibling binding cannot observe the Go body's HashSet/Array stores.
func (exec *Execution) prepareHostDrivenCollections(builtin *Builtin, receiver Value, args []Value, kwargs map[string]Value) (Value, []Value, map[string]Value, error) {
	if builtin == nil || !builtin.hostDriven {
		return receiver, args, kwargs, nil
	}
	var err error
	if !builtin.declaredNonMutating() {
		// A receiver that is host state -- the capability object an adapter
		// bound, or anything a factory installed into it -- crosses live:
		// its in-place mutation is the sanctioned publication channel, and
		// everything it holds is marked shared the moment a mutating host
		// dispatch ends (markHostWritableState), so nothing there transfers
		// out of the Call live (#1210). Any other receiver is script data:
		// it detaches when shared, exactly as an argument would, so a host
		// write cannot reach a sibling binding.
		if !exec.receiverIsHostState(receiver) && isCollection(receiver) &&
			!receiver.Unpublished() && !receiver.SoleRef() {
			receiver, err = exec.detachHostCrossingValue(receiver)
			if err != nil {
				return NewNil(), nil, nil, err
			}
		}
		if len(args) > 0 {
			args = slices.Clone(args)
			for i, arg := range args {
				args[i], err = exec.isolateHostCollection(arg)
				if err != nil {
					return NewNil(), nil, nil, err
				}
			}
		}
		if len(kwargs) > 0 {
			kwargs = maps.Clone(kwargs)
			for key, arg := range kwargs {
				kwargs[key], err = exec.isolateHostCollection(arg)
				if err != nil {
					return NewNil(), nil, nil, err
				}
			}
		}
	}
	if !builtin.declaredNonRetaining() {
		// Mark everything the host will actually see, after isolation: the
		// host may retain any reachable wrapper, not just a root, and the
		// interpreter cannot see how many handles exist on the other side.
		// A marked wrapper makes a later script write copy and keeps the
		// Call boundary from transferring it out live. Marking first would
		// force a copy the host then mutates invisibly.
		seen := make(map[uintptr]struct{})
		// The receiver graph too: a retaining builtin may stash a receiver
		// sub-wrapper even when it declared non-mutation, and an unmarked
		// sole wrapper would transfer out of the Call live -- a script can
		// grow a host-state graph by assigning into it, so no walk can be
		// skipped. The walks stay cheap because only a wrapper's first
		// recording is charged; re-walks of stable host state are map hits.
		receiverRecord := exec.hostStateIdentities
		if !exec.receiverIsHostState(receiver) {
			receiverRecord = nil
		}
		if err := exec.markSharedGraphRecording(receiver, seen, receiverRecord); err != nil {
			return NewNil(), nil, nil, err
		}
		for _, arg := range args {
			if err := exec.markSharedGraph(arg, seen); err != nil {
				return NewNil(), nil, nil, err
			}
		}
		for _, arg := range kwargs {
			if err := exec.markSharedGraph(arg, seen); err != nil {
				return NewNil(), nil, nil, err
			}
		}
	}
	return receiver, args, kwargs, nil
}

// recordHostStateRoot registers a value the host can see and write for the
// duration of this call -- a capability adapter's bound object. Its graph's
// identities feed the receiver-liveness rule: dispatching on host state
// keeps the factory channel live, while dispatching a host builtin on plain
// script data does not hand that data to the host raw.
func (exec *Execution) recordHostStateRoot(val Value) {
	if !isCollection(val) {
		return
	}
	exec.capabilityBoundRoots = append(exec.capabilityBoundRoots, val)
	if exec.hostStateIdentities == nil {
		exec.hostStateIdentities = make(map[uintptr]struct{})
	}
	collectHostStateIdentities(val, exec.hostStateIdentities)
}

func collectHostStateIdentities(val Value, into map[uintptr]struct{}) {
	if !isCollection(val) {
		return
	}
	id := collectionIdentity(val)
	if id != 0 {
		if _, ok := into[id]; ok {
			return
		}
		into[id] = struct{}{}
	}
	switch val.Kind() {
	case KindArray:
		for _, elem := range val.Array() {
			collectHostStateIdentities(elem, into)
		}
	case KindHash, KindObject:
		val.RangeHashEntries(func(_ string, item Value) {
			collectHostStateIdentities(item, into)
		})
	}
}

// graphContainsBuiltin reports whether any builtin is reachable from val.
// A composite global carrying one is a host-visible object, not plain data.
func graphContainsBuiltin(val Value) bool {
	return graphContainsBuiltinSeen(val, nil)
}

func graphContainsBuiltinSeen(val Value, seen map[uintptr]struct{}) bool {
	if val.Kind() == KindBuiltin {
		return true
	}
	if !isCollection(val) {
		return false
	}
	if id := collectionIdentity(val); id != 0 {
		if _, ok := seen[id]; ok {
			return false
		}
		if seen == nil {
			seen = make(map[uintptr]struct{})
		}
		seen[id] = struct{}{}
	}
	switch val.Kind() {
	case KindArray:
		for _, elem := range val.Array() {
			if graphContainsBuiltinSeen(elem, seen) {
				return true
			}
		}
	case KindHash, KindObject:
		found := false
		val.RangeHashEntries(func(_ string, item Value) {
			if found {
				return
			}
			found = graphContainsBuiltinSeen(item, seen)
		})
		return found
	}
	return false
}

// receiverIsHostState reports whether the dispatched receiver is part of the
// state a host adapter bound into this call. Membership is by wrapper
// identity, refreshed after every mutating host dispatch, so the rule is
// stable under script aliasing: `c = cross` names the same host object and
// keeps the factory channel live.
func (exec *Execution) receiverIsHostState(receiver Value) bool {
	if len(exec.hostStateIdentities) == 0 || !isCollection(receiver) {
		return false
	}
	_, ok := exec.hostStateIdentities[collectionIdentity(receiver)]
	return ok
}

// markHostWritableState runs immediately after a mutating-capable host
// dispatch, success or error: everything the host could have written -- the
// dispatched receiver's graph and the host-visible roots -- is marked
// shared, and the host-state identity set is refreshed so wrappers a
// factory installed join the liveness rule. Immediate marking is what
// closes the mid-call windows: a later script write copies, a later
// dispatch isolates, and a wrapper removed from host state before the call
// returns was already marked when it was installed.
func (exec *Execution) markHostWritableState(receiver Value) error {
	// The identity set grows only from host state: the roots' graphs, plus
	// the receiver's own when it already is host state (a factory installed
	// into it). A plain script-data receiver is marked -- the host saw it --
	// but must not join the liveness rule, or its next dispatch would cross
	// live despite a sibling binding. One shared seen map keeps every
	// wrapper's walk to a single charged visit; the recorder attributes
	// exactly what each walk reaches first.
	seen := make(map[uintptr]struct{})
	// Roots first: a wrapper reachable from both a plain-data receiver and
	// host state must be recorded as host state, and the shared seen map
	// would otherwise let the receiver walk shadow the roots walk.
	for _, root := range exec.capabilityBoundRoots {
		if err := exec.markSharedGraphRecording(root, seen, exec.hostStateIdentities); err != nil {
			return err
		}
	}
	receiverRecord := exec.hostStateIdentities
	if !exec.receiverIsHostState(receiver) {
		receiverRecord = nil
	}
	return exec.markSharedGraphRecording(receiver, seen, receiverRecord)
}

// markSharedGraph forces every collection reachable from val to the shared
// state. The host boundary uses it where Go code gets live sight of script
// wrappers: the interpreter cannot see how many handles the other side
// keeps, so each must copy on the next script write and clone rather than
// transfer at the Call boundary.
func (exec *Execution) markSharedGraph(val Value, seen map[uintptr]struct{}) error {
	return exec.markSharedGraphRecording(val, seen, nil)
}

// markSharedGraphRecording is markSharedGraph with an optional recorder: each
// wrapper visited for the first time also joins record, which is how the
// host-state identity set grows from exactly the graphs that are host state.
func (exec *Execution) markSharedGraphRecording(val Value, seen, record map[uintptr]struct{}) error {
	if !isCollection(val) {
		return nil
	}
	charge := true
	if id := collectionIdentity(val); id != 0 {
		if _, ok := seen[id]; ok {
			// A duplicate edge to an already-visited wrapper is still work;
			// charging it at the discounted rate keeps fan-in shapes -- one
			// array holding thousands of references to one wrapper -- from
			// buying uncharged traversals.
			exec.hostStateRevisits++
			if exec.hostStateRevisits%64 == 0 {
				return exec.chargeScanSteps(1)
			}
			return nil
		}
		seen[id] = struct{}{}
		if record != nil {
			if _, recorded := record[id]; recorded {
				// Already host state from an earlier walk: re-marking is
				// cheap, so a loop of dispatches over stable host state pays
				// a heavily discounted rate rather than the full walk --
				// enough that CPU stays coupled to the step quota, since a
				// hostile script can otherwise grow the graph once and drive
				// unlimited free re-walks. Descend anyway: a script write
				// can hang a new wrapper under a recorded one.
				charge = false
				exec.hostStateRevisits++
				if exec.hostStateRevisits%64 == 0 {
					if err := exec.chargeScanSteps(1); err != nil {
						return err
					}
				}
			} else {
				record[id] = struct{}{}
			}
		}
	}
	if charge {
		if err := exec.chargeScanSteps(1); err != nil {
			return err
		}
	}
	val.MarkSharedRef()
	switch val.Kind() {
	case KindArray:
		for _, elem := range val.Array() {
			if !isCollection(elem) {
				// Scalar leaves are loop work too; the same discounted rate
				// keeps a huge scalar array from being a free re-walk.
				exec.hostStateRevisits++
				if exec.hostStateRevisits%64 == 0 {
					if err := exec.chargeScanSteps(1); err != nil {
						return err
					}
				}
				continue
			}
			if err := exec.markSharedGraphRecording(elem, seen, record); err != nil {
				return err
			}
		}
	case KindHash, KindObject:
		var walkErr error
		val.RangeHashEntries(func(_ string, item Value) {
			if walkErr != nil {
				return
			}
			if !isCollection(item) {
				exec.hostStateRevisits++
				if exec.hostStateRevisits%64 == 0 {
					walkErr = exec.chargeScanSteps(1)
				}
				return
			}
			walkErr = exec.markSharedGraphRecording(item, seen, record)
		})
		return walkErr
	}
	return nil
}

// detachHostBuiltinResult makes a host builtin's collection return independent
// of the host, so a backing map or slice the Go body still holds cannot write
// into script state after the call. A first-party capability return the
// dispatcher's proof covers was already validated and cloned by its adapter
// and crosses as it is; a builtin that declared itself non-retaining is
// trusted the same way and never reaches here.
func (exec *Execution) detachHostBuiltinResult(builtin *Builtin, result Value) (Value, error) {
	if !isCollection(result) {
		return result, nil
	}
	if exec.capabilityReturnProof.covers(builtin.Name, result) {
		return result, nil
	}
	return exec.detachHostCrossingValue(result)
}

// detachHostCrossingValue makes a host-supplied value independent of the Go
// state that built it before it enters script state: a yielded block
// argument, like a host builtin's return, may wrap a map or slice the host
// still holds. The copy is charged; it is script state now.
func (exec *Execution) detachHostCrossingValue(val Value) (Value, error) {
	if !isCollection(val) {
		return val, nil
	}
	if err := exec.preflightDeepClone(val); err != nil {
		return NewNil(), err
	}
	detached := deepCloneValueForContainment(val)
	if err := exec.checkMemoryValue(detached); err != nil {
		return NewNil(), err
	}
	return detached, nil
}

// isolateHostCollection returns a wrapper the host may write through. Host
// contracts treat mutation as a write to any reachable container, so a
// shallow copy of the root is not enough: `child = {x: 1}; a = {child: child}`
// leaves `a` unpublished while `child` is still named by a local, and a host
// HashSet through `args[0].Hash()["child"]` would otherwise change what the
// script observes.
//
// An unpublished graph is reachable from nothing but this call and is left
// in place: a host write to it is invisible to the script by construction.
// A published wrapper in the graph deep-copies the whole value, so a host
// write can never land in script-observable state (#1210).
func (exec *Execution) isolateHostCollection(val Value) (Value, error) {
	if !isCollection(val) {
		return val, nil
	}
	needed, err := exec.hostCollectionNeedsIsolation(val, make(map[uintptr]struct{}))
	if err != nil || !needed {
		return val, err
	}
	if err := exec.preflightDeepClone(val); err != nil {
		return NewNil(), err
	}
	cloned := deepCloneValueForContainment(val)
	if err := exec.checkMemoryValue(cloned); err != nil {
		return NewNil(), err
	}
	return cloned, nil
}

// hostCollectionNeedsIsolation reports whether val or any collection it
// reaches is named from a durable slot at all. A cycle is a single visit,
// not a reason to copy.
func (exec *Execution) hostCollectionNeedsIsolation(val Value, seen map[uintptr]struct{}) (bool, error) {
	if !isCollection(val) {
		return false, nil
	}
	if id := collectionIdentity(val); id != 0 {
		if _, ok := seen[id]; ok {
			return false, nil
		}
		seen[id] = struct{}{}
	}
	if err := exec.chargeScanSteps(1); err != nil {
		return false, err
	}
	if !val.Unpublished() {
		return true, nil
	}
	switch val.Kind() {
	case KindArray:
		for _, elem := range val.Array() {
			needed, err := exec.hostCollectionNeedsIsolation(elem, seen)
			if err != nil || needed {
				return needed, err
			}
		}
	case KindHash, KindObject:
		var needed bool
		var walkErr error
		val.RangeHashEntries(func(_ string, item Value) {
			if needed || walkErr != nil {
				return
			}
			needed, walkErr = exec.hostCollectionNeedsIsolation(item, seen)
		})
		return needed, walkErr
	}
	return false, nil
}

// publishBindingReplacement counts the handle a store into an already-occupied
// slot creates, unless the slot already names the same wrapper. Writing a
// mutator's result back over the receiver it updated -- `items = items.push(x)`,
// `@rows = @rows.delete_if { ... }` -- duplicates nothing, and counting it would
// leave the binding looking shared and make the next write copy.
func publishBindingReplacement(previous, next Value) {
	if isCollection(next) {
		value.PublishReplacement(previous, next)
	}
}

// publishCollectionElems publishes every collection among elems. The mutators
// that graft values into an existing array call it: the array's element slots
// now name them as well as wherever the arguments came from. Array construction
// publishes through NewArray instead, which sees every element at once.
func publishCollectionElems(elems []Value) {
	value.PublishRefElems(elems)
}

// writeArrayElems installs elems as the elements of the array a mutator is
// updating, checking writability at the write rather than trusting a check made
// earlier in the call. It returns the wrapper actually written, which is the
// value the mutator returns.
//
// Every in-place array mutator ends here, which is what makes the check
// structural: a mutator that runs script code between its first check and its
// write -- a block, an argument expression -- cannot forget to look again,
// because looking again is what writing is.
func (exec *Execution) writeArrayElems(receiver Value, elems []Value) (Value, error) {
	if len(elems) == 0 {
		return exec.writeClearedCollection(receiver)
	}
	target, err := exec.writableCollection(receiver)
	if err != nil {
		return NewNil(), err
	}
	setArrayElems(target, elems)
	return target, nil
}

// writeHashClear empties the hash a mutator is updating, checking writability at
// the write for the same reason writeArrayElems does.
func (exec *Execution) writeHashClear(receiver Value) (Value, error) {
	return exec.writeClearedCollection(receiver)
}

// writeClearedCollection empties an addressed collection without copying the
// contents that clear is about to discard. A sole wrapper is emptied in
// place so identity is preserved; a shared one is rebound to a fresh empty
// collection after isolating only the ancestors that must be rewritten.
func (exec *Execution) writeClearedCollection(receiver Value) (Value, error) {
	if !isCollection(receiver) {
		return receiver, nil
	}
	// A sole nested leaf can still be visible through a shared ancestor
	// (`a = [[1]]; b = a; a[0].clear`), so in-place clearing needs an
	// ancestor-free root; replaceMutableLeaf isolates shared ancestors.
	if receiver.Unpublished() || exec.addressedExclusiveRoot(receiver) {
		clearCollectionInPlace(receiver)
		return receiver, nil
	}
	empty := emptyCollectionLike(receiver)
	if exec.addressed.valid && exec.addressed.leaf == collectionIdentity(receiver) {
		return exec.replaceMutableLeaf(empty)
	}
	return empty, nil
}

func clearCollectionInPlace(val Value) {
	switch val.Kind() {
	case KindArray:
		setArrayElems(val, []Value{})
	case KindHash, KindObject:
		hashClearEntries(val)
	}
}

func emptyCollectionLike(val Value) Value {
	switch val.Kind() {
	case KindArray:
		return NewArray([]Value{})
	case KindHash:
		return NewHashWithCapacity(0)
	case KindObject:
		return NewObject(map[string]Value{})
	default:
		return val
	}
}

// collectionIdentity returns the wrapper identity of a collection, or 0 for
// anything else. It identifies the wrapper rather than its storage, so it
// survives an in-place growth that reallocates the backing.
func collectionIdentity(val Value) uintptr {
	switch val.Kind() {
	case KindArray:
		return arrayIdentity(val)
	case KindHash:
		return hashIdentity(val)
	case KindObject:
		return value.ObjectIdentity(val)
	default:
		return 0
	}
}

// copyCollection returns an independent wrapper over the same contents, charged
// against the memory quota before it allocates and billed to the step quota for
// the elements it walks. Nested collections are not copied: they are published
// instead, so each stays one value until something writes through it, at which
// point that write makes its own copy along its own path.
//
// The copy is the whole cost of value semantics, and it is why binding is free.
// A caller that already knows the wrapper is exclusively held must not call it.
func (exec *Execution) copyCollection(val Value) (Value, error) {
	switch val.Kind() {
	case KindArray:
		elems := val.Array()
		if err := exec.checkSlotReservationWithCallRoots(len(elems), val, nil, nil, NewNil()); err != nil {
			return NewNil(), err
		}
		if err := exec.chargeScanSteps(len(elems)); err != nil {
			return NewNil(), err
		}
		copied := slices.Clone(elems)
		if copied == nil {
			copied = []Value{}
		}
		return NewArray(copied), nil
	case KindHash:
		entries := val.Hash()
		if err := exec.checkProjectedHashBytes(len(entries), val, nil, nil, NewNil()); err != nil {
			return NewNil(), err
		}
		if err := exec.chargeScanSteps(len(entries)); err != nil {
			return NewNil(), err
		}
		copied := maps.Clone(entries)
		if copied == nil {
			copied = map[string]Value{}
		}
		return value.NewHashWithOrder(copied, val.HashKeyOrder()), nil
	case KindObject:
		entries := val.Hash()
		if err := exec.checkProjectedHashBytes(len(entries), val, nil, nil, NewNil()); err != nil {
			return NewNil(), err
		}
		if err := exec.chargeScanSteps(len(entries)); err != nil {
			return NewNil(), err
		}
		copied := maps.Clone(entries)
		if copied == nil {
			copied = map[string]Value{}
		}
		// A tagged bag renders a string form fixed at construction and refuses
		// writes, so a copy of one is only ever made on a path that already
		// rejected the write. Rebuilding it as an ordinary bag would launder
		// the provenance, so the tag rides along.
		if tag := val.ObjectTag(); tag != ObjectTagNone {
			form, _ := val.ObjectStringForm()
			return value.NewTaggedObject(copied, tag, form), nil
		}
		return NewObject(copied), nil
	default:
		return val, nil
	}
}

// writableCollection returns a wrapper the caller may write through. It is the
// guard every in-place mutator passes, so that no dispatch route -- a member
// call, send, an operator, a host builtin -- can reach a write with a receiver
// something else can still see.
//
// A wrapper no durable slot names yet is always writable. Beyond that the caller
// must have reached it through the path that owns it, which resolveMutablePath
// records on the execution for the duration of one call; without that record a
// receiver that lives in a slot is a temporary, and the write goes to a copy
// nothing else can see.
//
// Under the always-copy oracle the in-place fast path is off
// (addressedExclusiveRoot goes through exclusivelyHeld), so an addressed
// receiver re-isolates through its recorded path on every write. Isolation
// installs the copy at the slot the source names, so the write is never
// stranded in a wrapper installed nowhere; only a temporary, which owns no
// slot, falls through to the bare copy.
func (exec *Execution) writableCollection(val Value) (Value, error) {
	if !isCollection(val) {
		return val, nil
	}
	if val.Unpublished() {
		return val, nil
	}
	if exec.addressedExclusiveRoot(val) {
		return val, nil
	}
	// The receiver was resolved as an addressable path, and script code has
	// published it since -- an argument expression, this mutator's own block, a
	// right side. Isolating again through the recorded path both restores
	// exclusive ownership and reinstalls the result where the source names it,
	// which a bare copy would not: the write would land somewhere nothing can
	// reach and the update would simply be lost.
	if exec.addressed.valid && exec.addressed.leaf == collectionIdentity(val) {
		leaf, chain, err := exec.isolateMutablePath(exec.addressed.target, exec.addressed.env)
		if err != nil {
			return NewNil(), err
		}
		if isCollection(leaf) {
			exec.addressed.leaf = collectionIdentity(leaf)
			exec.addressed.path = chain
			return leaf, nil
		}
	}
	return exec.copyCollection(val)
}

// addressCollection records that val was reached through the path that owns it,
// so the write about to run may proceed in place. The returned restore function
// must run before control leaves the call, which is what keeps the record from
// vouching for a later expression that reached the same wrapper another way.
func (exec *Execution) addressCollection(val Value, chain []uintptr, target mutablePath, env *Env) {
	exec.addressed = addressedScope{
		leaf:   collectionIdentity(val),
		path:   chain,
		target: target,
		env:    env,
		valid:  true,
	}
}

// savedAddressedScope snapshots the permission currently in force. Callers take
// it before resolving a receiver and defer restoring it, which withdraws the
// permission on every exit without a closure to allocate. The save/restore
// pair also brackets the window in which a captured path can be pending,
// which is what bounds how long isolation forwards are retained.
func (exec *Execution) savedAddressedScope() addressedScope {
	exec.addressedBrackets++
	return exec.addressed
}

// withdrawAddressed clears the permission in force without closing a
// bracket: the caller's own deferred restore still runs. It is what a
// temporary's evaluation uses so an enclosing mutator's record cannot vouch
// for a wrapper reached another way.
func (exec *Execution) withdrawAddressed() {
	exec.addressed = addressedScope{}
}

// addressedScope is the permission state a write displaced, so that restoring it
// is a plain struct copy passed to a deferred call. A closure here allocated on
// every mutating write, which the shovel-in-a-loop benchmarks saw.
//
// It carries the resolved path, not just the leaf, because the permission has to
// survive script code running between the moment it was granted and the moment
// the write lands -- an argument expression, a mutator's block, a compound
// assignment's right side. Any of those can bind the receiver somewhere new, and
// the write then has to isolate again. Holding the path is what lets the write
// do that itself instead of trusting that nothing happened.
type addressedScope struct {
	leaf   uintptr
	path   []uintptr
	target mutablePath
	env    *Env
	valid  bool
}

// restore withdraws the permission addressCollection granted and closes the
// bracket its paired savedAddressedScope opened. When the outermost bracket
// closes no captured path can be pending anywhere, so the isolation forwards
// recorded inside the window are dropped -- that is what keeps the map (and
// the displaced wrappers it pins) from outliving one write's statement.
func (exec *Execution) restore(saved addressedScope) {
	exec.addressed = saved
	if exec.addressedBrackets > 0 {
		exec.addressedBrackets--
	}
	if exec.addressedBrackets == 0 && exec.isolationForwards != nil {
		exec.isolationForwards = nil
	}
}

// detachStoredCollection returns the value a write should place in a slot of the
// collection it is updating, copying it when it is the receiver itself or one of
// the containers the write reached the receiver through.
//
// Storing a container into itself is the one way collections could still form a
// cycle, and under value semantics it must not: `list.push(list)` appends the
// list as it was, not a window onto the list as it is about to become. Copying
// the value is what makes those two the same thing, and it leaves collections
// acyclic by construction -- a graph the estimator, equality, and rendering no
// longer have to defend against for arrays and hashes.
//
// A container reached some other way needs no copy, because reaching it that way
// published it, and the write that isolated the receiver already copied
// everything the two had in common.
func (exec *Execution) detachStoredCollection(val Value) (Value, error) {
	if !isCollection(val) || exec.addressed.leaf == 0 {
		return val, nil
	}
	id := collectionIdentity(val)
	if id == 0 {
		return val, nil
	}
	if id != exec.addressed.leaf && !slices.Contains(exec.addressed.path, id) {
		return val, nil
	}
	// The copy must be independent of the container being written, and a
	// shallow one is not: copying `a` for `a[0].push(a)` produces a second
	// wrapper that still holds the very element the push is about to write
	// into, which is the cycle this exists to prevent. A deep copy is also the
	// right answer rather than a workaround -- what the write stores is the
	// value the container had, all the way down, not a window onto the value it
	// is about to have.
	if err := exec.preflightDeepClone(val); err != nil {
		return NewNil(), err
	}
	detached := deepCloneValue(val)
	if err := exec.checkMemoryValue(detached); err != nil {
		return NewNil(), err
	}
	return detached, nil
}

// preflightDeepClone charges the walk and rejects a clone that would not fit
// before deepCloneValue allocates it. checkMemoryValue after the clone cannot
// unspend that peak.
func (exec *Execution) preflightDeepClone(val Value) error {
	est := newMemoryEstimator()
	projected := est.value(val)
	if err := exec.chargeScanSteps(est.walked); err != nil {
		return err
	}
	if exec.memoryQuota <= 0 {
		return nil
	}
	if exec.memoryExceeded(saturatingAdd(exec.estimateMemoryUsage(), projected)) {
		return exec.memoryQuotaExceededError()
	}
	return nil
}

// shovelArray implements array << value with the same isolation, detachment,
// and publication the operator uses. Reduce's "<<" shorthand and &:<< must
// go through this rather than shovelValues, or they mutate a shared element
// and can install a cycle.
func (exec *Execution) shovelArray(left, right Value) (Value, error) {
	if left.Kind() != KindArray {
		return shovelValues(left, right)
	}
	detached, err := exec.detachStoredCollection(right)
	if err != nil {
		return NewNil(), err
	}
	writable, err := exec.writableCollection(left)
	if err != nil {
		return NewNil(), err
	}
	publishCollection(detached)
	handled, err := exec.appendArrayCharged(writable, detached)
	if err != nil {
		return NewNil(), err
	}
	if handled {
		return writable, nil
	}
	if err := arrayReserveInPlaceGrowth(exec, writable, []Value{detached}, nil, NewNil(), 1); err != nil {
		return NewNil(), err
	}
	return shovelValues(writable, detached)
}

// detachStoredCollections applies detachStoredCollection across a mutator's
// arguments, returning the original slice untouched when none of them names a
// container on the write's own path.
func (exec *Execution) detachStoredCollections(vals []Value) ([]Value, error) {
	if exec.addressed.leaf == 0 {
		return vals, nil
	}
	var detached []Value
	for i, val := range vals {
		replacement, err := exec.detachStoredCollection(val)
		if err != nil {
			return nil, err
		}
		if collectionIdentity(replacement) == collectionIdentity(val) {
			continue
		}
		if detached == nil {
			detached = slices.Clone(vals)
		}
		detached[i] = replacement
	}
	if detached == nil {
		return vals, nil
	}
	return detached, nil
}

// mutableRootKind names where a mutable path begins.
type mutableRootKind uint8

const (
	// mutableRootNone marks an expression that names no slot the runtime can
	// rebind, so a write through it updates a temporary.
	mutableRootNone mutableRootKind = iota
	mutableRootLocal
	mutableRootIvar
	mutableRootClassVar
	// mutableRootCall marks a path that begins at an explicit call
	// (`a.last()[0]`, `geta().k`). The call's result is a temporary the
	// runtime cannot rebind, so the path is never addressable; the kind
	// exists so a write through it is classified as a temporary and
	// detached, rather than falling to the ordinary-evaluation fallback,
	// which writes with no isolation (#1221).
	mutableRootCall
)

// mutableRoot is the rebindable slot a mutable path starts at. A call root
// names no slot: expr carries the expression whose one evaluation stands in
// for the root read, and get and rebind treat it as unresolved. A local root
// keeps its source expression too, so a bare name that turns out to denote a
// call (see identifierRootAutoInvokes) can be evaluated the same way.
type mutableRoot struct {
	kind mutableRootKind
	name string
	env  *Env
	vars map[string]Value
	expr Expression
}

func (root mutableRoot) get() (Value, bool) {
	switch root.kind {
	case mutableRootLocal:
		return root.env.Get(root.name)
	case mutableRootIvar, mutableRootClassVar:
		val, ok := root.vars[root.name]
		return val, ok
	default:
		return NewNil(), false
	}
}

// rebind installs a copy at the slot the path starts at. It never publishes:
// the copy replaces the slot's previous occupant, so the slot count does not
// grow, and publishing here would make the next write through the same slot
// copy again -- turning a mutating loop quadratic.
func (root mutableRoot) rebind(val Value) {
	switch root.kind {
	case mutableRootLocal:
		root.env.Assign(root.name, val)
	case mutableRootIvar, mutableRootClassVar:
		bumpMutationEpoch()
		root.vars[root.name] = val
	}
	val.AdoptSoleRef()
}

// mutablePathStep is one index or hash-member hop of a mutable path.
//
// An index hop keeps its source expression because the index is evaluated once,
// on the way down, and reused for the write-back: re-evaluating it could run
// script code twice and address a different slot the second time.
type mutablePathStep struct {
	expr    *IndexExpr
	member  *MemberExpr
	index   Value
	indexed bool
	pos     Position
}

// mutablePath is an addressable receiver: a rebindable root plus the hops that
// reach the value being written.
//
// captured is the wrapper at each level when the path was first resolved, root
// first and leaf last. A later write isolates those wrappers, not whatever the
// live slots name now: `a[0] += (a = [9]; 1)` must leave the newly assigned a
// intact, and `@a[0][0] += (@a[0] = [9]; 1)` must leave the newly assigned
// child intact, applying the pending write to the receiver that was already
// evaluated.
type mutablePath struct {
	root     mutableRoot
	steps    []mutablePathStep
	captured []Value
}

// captureEvaluated records the wrappers the path names now, using keys the
// reading pass already resolved so nothing is evaluated twice.
func (exec *Execution) captureEvaluatedPath(path *mutablePath, env *Env) {
	current, ok := path.root.get()
	if !ok {
		path.captured = nil
		return
	}
	captured := make([]Value, 0, 1+len(path.steps))
	captured = append(captured, current)
	for i := range path.steps {
		child, found, err := exec.readCollectionStep(current, &path.steps[i], env)
		if err != nil || !found {
			break
		}
		captured = append(captured, child)
		current = child
	}
	path.captured = captured
}

// targetResolution describes what a receiver or target resolver produced: an
// addressable path, an already-evaluated temporary (use the value; do not
// evaluate the expression again, and the write must reach nothing), or
// nothing at all (ordinary evaluation owns the expression).
type targetResolution int

const (
	targetUnresolved targetResolution = iota
	targetTemporary
	targetAddressed
)

// resolveMutableReceiver evaluates expr as the receiver of an in-place write,
// reporting false when expr names no addressable path -- the caller then
// evaluates it however it ordinarily would, and the write goes to a temporary.
//
// When expr does name one, each level of that path is made exclusively held on
// the way down -- copied and rebound where it is shared -- so the leaf is a
// wrapper only this path reaches, and the write lands where the script can see
// it. The caller must have taken savedAddressedScope and deferred restoring it.
//
// Mutators that may not write should call addressMutableReceiver instead, so a
// no-op or a rejected call does not copy.
func (exec *Execution) resolveMutableReceiver(expr Expression, env *Env) (Value, targetResolution, error) {
	path, ok := exec.mutablePathFor(expr, env)
	if !ok {
		return NewNil(), targetUnresolved, nil
	}
	leaf, chain, addressable, resolved, err := exec.walkMutablePath(path, env)
	if err != nil {
		return NewNil(), targetTemporary, err
	}
	if !resolved {
		// The walk read nothing the caller can use -- the root name is unbound
		// or auto-invokes, so what the slot holds is not what the expression
		// denotes. A name that denotes a call is read here and classified as a
		// temporary, like an explicit call root; anything else is left to
		// ordinary evaluation.
		leaf, claimed, err := exec.readAutoInvokedRootPath(path, env)
		if err != nil {
			return NewNil(), targetTemporary, err
		}
		if claimed {
			exec.withdrawAddressed()
			return leaf, targetTemporary, nil
		}
		return NewNil(), targetUnresolved, nil
	}
	// A path that left collection storage -- a member of a class instance, an
	// index into a string -- read the value the ordinary evaluation would have,
	// so the caller keeps it, but it names no slot the write may update. The
	// permission is withdrawn outright: the record in force belongs to an
	// enclosing mutator's receiver, and it must not vouch for a temporary
	// that happens to be the same wrapper (`a.push(a.itself.push(2))`).
	if !addressable {
		exec.withdrawAddressed()
		return leaf, targetTemporary, nil
	}
	exec.captureEvaluatedPath(&path, env)
	exec.addressCollection(leaf, chain, path, env)
	return leaf, targetAddressed, nil
}

// addressMutableReceiver is resolveMutableReceiver without the isolating pass.
// It records the path that owns expr so a later write can isolate through it,
// but it does not copy: arguments have not been evaluated yet, and the
// mutator may not write at all (`a.push()`, `a.pop(0)`, a rejected call).
func (exec *Execution) addressMutableReceiver(expr Expression, env *Env) (Value, bool, error) {
	path, ok := exec.mutablePathFor(expr, env)
	if !ok || path.root.kind == mutableRootCall {
		// A call-rooted receiver stays with ordinary evaluation: mutators are
		// already guarded by writableCollection, which copies a published
		// receiver no addressed record vouches for. The claim exists for the
		// assignment funnel, which has no such guard.
		return NewNil(), false, nil
	}
	leaf, chain, addressable, resolved, err := exec.readAddressablePath(path, env, true)
	if err != nil {
		return NewNil(), true, err
	}
	if !resolved {
		return NewNil(), false, nil
	}
	if addressable {
		exec.captureEvaluatedPath(&path, env)
		exec.addressCollection(leaf, chain, path, env)
	} else {
		// The leaf is a temporary; the permission in force belongs to an
		// enclosing mutator's receiver and must not vouch for it.
		exec.withdrawAddressed()
	}
	return leaf, true, nil
}

// rootAutoInvokes reports whether reading this root the ordinary way would call
// it rather than yield it. A bare name bound to a function or builtin is invoked
// where a receiver is expected, so the value a path walk would find there is not
// the one the expression denotes; such a root is left to ordinary evaluation.
func rootAutoInvokes(val Value) bool {
	switch val.Kind() {
	case KindFunction, KindBuiltin:
		return true
	default:
		return false
	}
}

// resolveMutableTarget is addressMutableReceiver for a write whose value is only
// computed after the target is prepared -- a compound assignment evaluates
// `a[0] += f()` by reading a[0], running f, and writing back. It hands the path
// back so the write can isolate immediately before it lands, and it leaves the
// permission withdrawn, because evaluating the right side runs script code that
// must not inherit it.
//
// Isolation waits until the write. The right side can bind the receiver
// somewhere new (`a[0] += (b = a; 1)`), so a wrapper that was exclusively held
// when the target was prepared need not still be when the write happens; it can
// also rebind the root (`a[0] += (a = [9]; 1)`), in which case the write stays
// on the already-evaluated receiver. The isolating pass rereads an
// already-resolved path whose keys are cached, so nothing is evaluated or
// invoked twice.
func (exec *Execution) resolveMutableTarget(expr Expression, env *Env) (Value, mutablePath, targetResolution, error) {
	saved := exec.savedAddressedScope()
	defer exec.restore(saved)

	path, ok := exec.mutablePathFor(expr, env)
	if !ok {
		return NewNil(), mutablePath{}, targetUnresolved, nil
	}
	leaf, _, addressable, resolved, err := exec.readAddressablePath(path, env, false)
	if err != nil {
		return NewNil(), mutablePath{}, targetTemporary, err
	}
	if !resolved {
		// A root name that denotes a call is read here and classified as a
		// temporary, like an explicit call root (see resolveMutableReceiver);
		// anything else is left to ordinary evaluation.
		leaf, claimed, err := exec.readAutoInvokedRootPath(path, env)
		if err != nil {
			return NewNil(), mutablePath{}, targetTemporary, err
		}
		if claimed {
			return leaf, mutablePath{}, targetTemporary, nil
		}
		return NewNil(), mutablePath{}, targetUnresolved, nil
	}
	if !addressable {
		// The receiver is already evaluated; handing it back keeps its side
		// effects (`a.pop[0] = x` pops once) instead of evaluating again.
		return leaf, mutablePath{}, targetTemporary, nil
	}
	exec.captureEvaluatedPath(&path, env)
	return leaf, path, targetAddressed, nil
}

// detachTemporaryWriteReceiver returns the wrapper a write to a temporary may
// land in. A temporary some durable slot still names -- an accessor handout
// like `a.last`, a getter's ivar -- is copied first, so the write reaches
// nothing the script can see again while still validating like a real write;
// a fresh temporary is written directly, which is equally invisible and free.
func (exec *Execution) detachTemporaryWriteReceiver(obj Value) (Value, error) {
	if !isCollection(obj) || obj.Unpublished() {
		return obj, nil
	}
	return exec.copyCollection(obj)
}

// writeThroughMutablePath isolates the path again and runs write against the
// leaf that isolation produced, with the addressability record in force for
// exactly that write.
func (exec *Execution) writeThroughMutablePath(path mutablePath, env *Env, write func(Value) error) error {
	leaf, chain, err := exec.isolateMutablePath(path, env)
	if err != nil {
		return err
	}
	saved := exec.savedAddressedScope()
	exec.addressCollection(leaf, chain, path, env)
	defer exec.restore(saved)
	return write(leaf)
}

// mutablePathFor decomposes expr into a rebindable root and the hops from it,
// reporting false for anything that names no such slot.
//
// Index and hash-member hops qualify because the runtime can write the hop back:
// an array slot and a hash entry are storage it owns. A member hop onto a class
// instance does not, because reading it may run an accessor and writing it may
// run a setter, so the runtime cannot claim the read and the write address one
// slot. Inside the class the same state is addressable as an instance variable.
func (exec *Execution) mutablePathFor(expr Expression, env *Env) (mutablePath, bool) {
	var steps []mutablePathStep
	for {
		switch t := expr.(type) {
		case *Identifier:
			if isClassConstantAssignmentName(t.Name, env) {
				return mutablePath{}, false
			}
			// Whether the name is bound is left to the root read below: asking
			// here would look the binding up twice on every write.
			return mutablePath{
				root:  mutableRoot{kind: mutableRootLocal, name: t.Name, env: env, expr: t},
				steps: reverseSteps(steps),
			}, true
		case *IvarExpr:
			self, ok := env.Get("self")
			if !ok || self.Kind() != KindInstance {
				return mutablePath{}, false
			}
			return mutablePath{
				root:  mutableRoot{kind: mutableRootIvar, name: t.Name, vars: valueInstance(self).Ivars},
				steps: reverseSteps(steps),
			}, true
		case *ClassVarExpr:
			self, ok := env.Get("self")
			if !ok {
				return mutablePath{}, false
			}
			var vars map[string]Value
			switch self.Kind() {
			case KindInstance:
				vars = valueInstance(self).Class.ClassVars
			case KindClass:
				vars = valueClass(self).ClassVars
			default:
				return mutablePath{}, false
			}
			return mutablePath{
				root:  mutableRoot{kind: mutableRootClassVar, name: t.Name, vars: vars},
				steps: reverseSteps(steps),
			}, true
		case *CallExpr:
			// An explicit call roots a path that is never addressable: its
			// result is a temporary, possibly a live handout (`a.last()`, a
			// getter's ivar), and a write through it must detach. Claiming
			// the expression here routes the write through the temporary
			// classification instead of the ordinary-evaluation fallback,
			// which writes with no isolation.
			return mutablePath{
				root:  mutableRoot{kind: mutableRootCall, expr: t},
				steps: reverseSteps(steps),
			}, true
		case *IndexExpr:
			if len(t.Indices) != 1 {
				return mutablePath{}, false
			}
			steps = append(steps, mutablePathStep{expr: t, pos: t.Position})
			expr = t.Object
		case *MemberExpr:
			steps = append(steps, mutablePathStep{member: t, pos: t.Pos()})
			expr = t.Object
		default:
			return mutablePath{}, false
		}
	}
}

func reverseSteps(steps []mutablePathStep) []mutablePathStep {
	slices.Reverse(steps)
	return steps
}

// walkMutablePath resolves the value the caller is about to write through, in
// two passes.
//
// The first reads the path, evaluating each index exactly once and recording
// whether every hop stayed inside collection storage. A hop that leaves it -- a
// member of a class instance, an index into a string -- is read the way an
// ordinary expression reads it, and from that point the path addresses no slot
// the runtime can rebind, so what it produces is a temporary.
//
// The second pass runs only for a fully addressable path whose leaf is a
// collection, and makes each level exclusively held on the way down. Its reads
// are pure -- an array index and a hash lookup with the key the first pass
// already resolved -- so nothing is evaluated or invoked twice.
//
// Copying a level publishes the collections it holds, so a level below a copied
// one is shared by construction and is copied in turn. That is not incidental:
// after the copy both wrappers name the same child, and only one of them may go
// on writing through it.
func (exec *Execution) walkMutablePath(path mutablePath, env *Env) (Value, []uintptr, bool, bool, error) {
	if path.root.kind == mutableRootCall {
		leaf, err := exec.readCallRootedPath(path, env)
		return leaf, nil, false, true, err
	}
	if len(path.steps) == 0 {
		// A bare local, instance variable, or class variable is the common
		// case -- `a.push(x)`, `@rows << row` -- and needs neither the reading
		// pass nor an ancestor list, so it reads the slot once and isolates it
		// on the spot.
		current, ok := path.root.get()
		if !ok || rootAutoInvokes(current) {
			return NewNil(), nil, false, false, nil
		}
		if !isCollection(current) || exec.exclusivelyHeld(current) {
			return current, nil, true, true, nil
		}
		copied, err := exec.copyCollection(current)
		if err != nil {
			return NewNil(), nil, false, true, err
		}
		exec.recordIsolationForward(current, copied)
		path.root.rebind(copied)
		return copied, nil, true, true, nil
	}
	leaf, _, addressable, resolved, err := exec.readAddressablePath(path, env, false)
	if err != nil || !resolved {
		return NewNil(), nil, false, resolved, err
	}
	if !addressable || !isCollection(leaf) {
		return leaf, nil, false, true, nil
	}
	isolated, chain, err := exec.isolateMutablePath(path, env)
	return isolated, chain, err == nil, true, err
}

// readAddressablePath is the reading pass of walkMutablePath: it resolves the
// leaf without copying. Mutators and compound writes isolate later, once they
// know a write will land. The ancestor chain is recorded only for the caller
// that consumes it (wantChain), so the common assignment walk allocates
// nothing; the member-tail path rebuilds its short prefix from the cached
// keys instead.
//
// A hop that is not collection storage ends the addressable path: a member
// step with no stored entry (`a.dup`, `h.keys`) belongs to ordinary member
// evaluation, and readPathTailAsTemporary finishes the path that way. A
// missing indexed slot reads through evalIndexValue with its cached index,
// exactly as evaluating the expression would.
func (exec *Execution) readAddressablePath(path mutablePath, env *Env, wantChain bool) (Value, []uintptr, bool, bool, error) {
	if path.root.kind == mutableRootCall {
		leaf, err := exec.readCallRootedPath(path, env)
		return leaf, nil, false, true, err
	}
	if len(path.steps) == 0 {
		current, ok := path.root.get()
		if !ok || rootAutoInvokes(current) {
			return NewNil(), nil, false, false, nil
		}
		return current, nil, true, true, nil
	}
	current, ok := path.root.get()
	if !ok || rootAutoInvokes(current) {
		return NewNil(), nil, false, false, nil
	}
	var chain []uintptr
	if wantChain {
		chain = make([]uintptr, 0, len(path.steps))
	}
	for i := range path.steps {
		step := &path.steps[i]
		if !isCollection(current) {
			prefix := mutablePath{root: path.root, steps: path.steps[:i]}
			tail, err := exec.readPathTailAsTemporary(current, prefix, exec.prefixAncestors(path, i, env), path.steps[i:], env)
			return tail, nil, false, true, err
		}
		if wantChain {
			chain = append(chain, collectionIdentity(current))
		}
		child, found, err := exec.readCollectionStep(current, step, env)
		if err != nil {
			return NewNil(), nil, false, true, err
		}
		if !found {
			if temp, ok, err := exec.arrayRangeTemporary(current, step, path.steps[i+1:], env); ok {
				return temp, nil, false, true, err
			}
			if step.member != nil {
				prefix := mutablePath{root: path.root, steps: path.steps[:i]}
				tail, err := exec.readPathTailAsTemporary(current, prefix, exec.prefixAncestors(path, i, env), path.steps[i:], env)
				return tail, nil, false, true, err
			}
			// An indexed miss reads exactly as evaluating the expression
			// would -- through evalIndexValue with the already-evaluated
			// index, which resolves hash defaults and object specials like
			// MatchData captures -- and the remaining hops continue
			// ordinarily on the result, so their side effects still run.
			leaf, err := exec.evalIndexValue(step.expr, current, []Value{step.index})
			if err != nil {
				return NewNil(), nil, false, true, err
			}
			tail, err := exec.readPathTailWithdrawn(leaf, path.steps[i+1:], env)
			return tail, nil, false, true, err
		}
		current = child
	}
	return current, chain, true, true, nil
}

// prefixAncestors rebuilds the wrapper identities above step upto by
// re-walking the already-evaluated hops. The keys are cached, so nothing
// runs twice; it is called only on the rare leave-storage path, which is
// what lets the common walk skip recording a chain it would discard.
func (exec *Execution) prefixAncestors(path mutablePath, upto int, env *Env) []uintptr {
	if upto == 0 {
		return nil
	}
	current, ok := path.root.get()
	if !ok {
		return nil
	}
	ancestors := make([]uintptr, 0, upto)
	for i := 0; i < upto; i++ {
		if !isCollection(current) {
			break
		}
		ancestors = append(ancestors, collectionIdentity(current))
		child, found, err := exec.readCollectionStep(current, &path.steps[i], env)
		if err != nil || !found {
			break
		}
		current = child
	}
	return ancestors
}

// isolateMutablePath is the isolating pass: it descends an addressable path
// copying and rebinding every level a second slot can still reach.
func (exec *Execution) isolateMutablePath(path mutablePath, env *Env) (Value, []uintptr, error) {
	current, ok := path.root.get()
	if len(path.captured) > 0 && (!ok || !exec.capturedReceiverUnchanged(path.captured[0], current)) {
		// The root slot no longer names the receiver that was evaluated.
		// Isolate that receiver as a temporary; the live slot belongs to
		// whatever the right side (or an argument) just bound there.
		// Scalars have no collection identity, so a string replaced by an
		// array would otherwise look unchanged and the write would land on
		// the replacement.
		return exec.isolateCapturedLeaf(path)
	}
	if !ok {
		return NewNil(), nil, nil
	}
	if isCollection(current) && !exec.exclusivelyHeld(current) {
		copied, err := exec.copyCollection(current)
		if err != nil {
			return NewNil(), nil, err
		}
		exec.recordIsolationForward(current, copied)
		path.root.rebind(copied)
		current = copied
	}
	// The chain lists the containers above the leaf, which the leaf's own
	// identity on the execution completes. Only a path with steps has any, so
	// the common bare-root write allocates nothing.
	chain := make([]uintptr, 0, len(path.steps))
	for i := range path.steps {
		chain = append(chain, collectionIdentity(current))
		step := &path.steps[i]
		child, found, err := exec.readCollectionStep(current, step, env)
		if err != nil || !found {
			return NewNil(), nil, err
		}
		if len(path.captured) > 0 && i+1 >= len(path.captured) {
			// This hop was missing when the path was evaluated. A later
			// selector may have installed a child; the write still belongs
			// to the miss, not that newly created receiver.
			return NewNil(), nil, nil
		}
		if i+1 < len(path.captured) && !exec.capturedReceiverUnchanged(path.captured[i+1], child) {
			// An intermediate slot was rebound. The pending write belongs
			// to the receiver already evaluated, not the replacement.
			return exec.isolateCapturedLeaf(path)
		}
		if isCollection(child) && !exec.exclusivelyHeld(child) {
			copied, err := exec.copyCollection(child)
			if err != nil {
				return NewNil(), nil, err
			}
			if err := exec.storeMutableStep(current, step, copied); err != nil {
				return NewNil(), nil, err
			}
			copied.AdoptSoleRef()
			exec.recordIsolationForward(child, copied)
			child = copied
		}
		current = child
	}
	return current, chain, nil
}

// replaceMutableLeaf installs replacement at the addressed slot without
// copying the leaf's contents. Ancestors that are shared are still isolated
// so the rebind is visible only through this path.
func (exec *Execution) replaceMutableLeaf(replacement Value) (Value, error) {
	path := exec.addressed.target
	env := exec.addressed.env
	current, ok := path.root.get()
	if len(path.captured) > 0 && (!ok || !exec.capturedReceiverUnchanged(path.captured[0], current)) {
		replacement.AdoptSoleRef()
		return replacement, nil
	}
	if !ok {
		replacement.AdoptSoleRef()
		return replacement, nil
	}
	if len(path.steps) == 0 {
		if exec.exclusivelyHeld(current) {
			clearCollectionInPlace(current)
			return current, nil
		}
		replacement.AdoptSoleRef()
		exec.recordIsolationForward(current, replacement)
		path.root.rebind(replacement)
		return replacement, nil
	}
	if isCollection(current) && !exec.exclusivelyHeld(current) {
		copied, err := exec.copyCollection(current)
		if err != nil {
			return NewNil(), err
		}
		exec.recordIsolationForward(current, copied)
		path.root.rebind(copied)
		current = copied
	}
	for i := range path.steps[:len(path.steps)-1] {
		step := &path.steps[i]
		child, found, err := exec.readCollectionStep(current, step, env)
		if err != nil {
			return NewNil(), err
		}
		if !found {
			// The path vanished under the pending write; nothing to install,
			// but the mutator still returns the emptied value.
			replacement.AdoptSoleRef()
			return replacement, nil
		}
		if len(path.captured) > 0 && i+1 >= len(path.captured) {
			replacement.AdoptSoleRef()
			return replacement, nil
		}
		if i+1 < len(path.captured) && !exec.capturedReceiverUnchanged(path.captured[i+1], child) {
			replacement.AdoptSoleRef()
			return replacement, nil
		}
		if isCollection(child) && !exec.exclusivelyHeld(child) {
			copied, err := exec.copyCollection(child)
			if err != nil {
				return NewNil(), err
			}
			if err := exec.storeMutableStep(current, step, copied); err != nil {
				return NewNil(), err
			}
			copied.AdoptSoleRef()
			exec.recordIsolationForward(child, copied)
			child = copied
		}
		current = child
	}
	last := &path.steps[len(path.steps)-1]
	child, found, err := exec.readCollectionStep(current, last, env)
	if err != nil {
		return NewNil(), err
	}
	if !found || (len(path.captured) > 0 && len(path.steps) >= len(path.captured)) {
		// The slot is gone, or this hop was missing when the path was
		// evaluated; nothing to install, but the mutator still returns the
		// emptied value.
		replacement.AdoptSoleRef()
		return replacement, nil
	}
	if len(path.captured) > len(path.steps) && !exec.capturedReceiverUnchanged(path.captured[len(path.steps)], child) {
		// The leaf slot was rebound. The pending empty write belongs to
		// the previously evaluated receiver, not the replacement.
		replacement.AdoptSoleRef()
		return replacement, nil
	}
	if err := exec.storeMutableStep(current, last, replacement); err != nil {
		return NewNil(), err
	}
	replacement.AdoptSoleRef()
	exec.recordIsolationForward(child, replacement)
	return replacement, nil
}

// isolateCapturedLeaf isolates the receiver the path evaluated as a temporary:
// the live slots now name something else, so the pending write must not land
// in them.
func (exec *Execution) isolateCapturedLeaf(path mutablePath) (Value, []uintptr, error) {
	leaf := path.captured[len(path.captured)-1]
	if isCollection(leaf) && !exec.exclusivelyHeld(leaf) {
		copied, err := exec.copyCollection(leaf)
		if err != nil {
			return NewNil(), nil, err
		}
		copied.AdoptSoleRef()
		leaf = copied
	}
	chain := make([]uintptr, 0, len(path.captured)-1)
	for _, val := range path.captured[:len(path.captured)-1] {
		chain = append(chain, collectionIdentity(val))
	}
	return leaf, chain, nil
}

// isolationForward records that a copy-on-write isolation displaced one
// wrapper with another in the same slot. The displaced Value is retained so
// the identity used as the map key cannot be reused by a later allocation
// while a captured path may still consult it.
type isolationForward struct {
	displaced Value
	to        uintptr
}

// recordIsolationForward notes that an isolation installed replacement where
// displaced used to be. A captured path that still holds the displaced
// wrapper then recognizes the replacement as the same logical receiver,
// updated -- `a.push(a.pop)` must land the push even though pop's isolation
// rebound the root. A script rebind (`a = [9]`) records nothing and stays a
// mismatch, which is what keeps the captured-receiver doctrine intact.
func (exec *Execution) recordIsolationForward(displaced, replacement Value) {
	fromID := collectionIdentity(displaced)
	toID := collectionIdentity(replacement)
	if fromID == 0 || toID == 0 {
		return
	}
	if exec.isolationForwards == nil {
		exec.isolationForwards = make(map[uintptr]isolationForward)
	}
	exec.isolationForwards[fromID] = isolationForward{displaced: displaced, to: toID}
}

// forwardsToLive reports whether capturedID reaches liveID through recorded
// isolation displacements. The chain is bounded: a pathological write
// pattern cannot loop it.
func (exec *Execution) forwardsToLive(capturedID, liveID uintptr) bool {
	for range 64 {
		forward, ok := exec.isolationForwards[capturedID]
		if !ok {
			return false
		}
		if forward.to == liveID {
			return true
		}
		capturedID = forward.to
	}
	return false
}

// capturedReceiverUnchanged reports whether live is still the wrapper (or
// scalar) that was evaluated, or an isolation's copy of it installed in the
// same slot. Collection identity is zero for strings and nil, so a later
// collection in that slot is always a replacement.
func (exec *Execution) capturedReceiverUnchanged(captured, live Value) bool {
	capturedID := collectionIdentity(captured)
	liveID := collectionIdentity(live)
	if capturedID != 0 {
		if liveID == capturedID {
			return true
		}
		return exec.forwardsToLive(capturedID, liveID)
	}
	if liveID != 0 {
		return false
	}
	return captured.Identical(live)
}

// exclusivelyHeld reports whether a collection reached through the path that
// owns it may be written in place. The always-copy oracle answers no for every
// collection, which turns the whole scheme back into a copy per write.
func (exec *Execution) exclusivelyHeld(val Value) bool {
	return !alwaysCopyCollections && val.SoleRef()
}

// addressedExclusiveRoot reports whether val is the addressed leaf of an
// ancestor-free path and exclusively held, so an in-place update is visible
// only through that path. A sole nested leaf does not qualify: it can still
// be visible through a shared ancestor (`a = [[1]]; b = a; a[0].push(2)`).
// It goes through exclusivelyHeld so the always-copy oracle can turn every
// in-place fast path off at one switch.
func (exec *Execution) addressedExclusiveRoot(val Value) bool {
	return exec.exclusivelyHeld(val) &&
		exec.addressed.valid &&
		exec.addressed.leaf == collectionIdentity(val) &&
		len(exec.addressed.path) == 0
}

// readPathTailAsTemporary finishes a path whose next hop is not collection
// storage, evaluating the remaining hops exactly as ordinary evaluation
// would, once. The first hop may itself be a mutator (`a.pop.push(9)` must
// pop a exactly as `a.pop` alone does), so when the departed prefix is
// addressable collection storage that hop runs with the prefix addressed.
// Every later hop dispatches on the previous hop's result -- a temporary no
// path owns -- so the permission is withdrawn for them; the caller withdraws
// it again for its own dispatch on the returned value.
func (exec *Execution) readPathTailAsTemporary(current Value, prefix mutablePath, ancestors []uintptr, steps []mutablePathStep, env *Env) (Value, error) {
	saved := exec.savedAddressedScope()
	defer exec.restore(saved)
	exec.withdrawAddressed()
	if len(steps) > 0 && steps[0].member != nil && isCollection(current) {
		exec.addressCollection(current, ancestors, prefix, env)
		hopped, err := exec.readTailMemberHop(current, &steps[0])
		exec.withdrawAddressed()
		if err != nil {
			return NewNil(), err
		}
		current, steps = hopped, steps[1:]
	}
	return exec.readPathTailOrdinarily(current, steps, env)
}

// readCallRootedPath reads a path whose root is a call: an explicit call
// expression, or a bare name ordinary evaluation auto-invokes (see
// identifierRootAutoInvokes). The root runs exactly as ordinary evaluation
// would, once, and every remaining hop dispatches on its result -- a
// temporary no durable slot names -- so the permission is withdrawn
// throughout: neither the root's own script code nor a mutator hop may write
// through a record an enclosing mutator still holds. Tail hops still
// consume a step each, matching the MemberExpr/IndexExpr nodes ordinary
// evaluation would have charged.
func (exec *Execution) readCallRootedPath(path mutablePath, env *Env) (Value, error) {
	saved := exec.savedAddressedScope()
	defer exec.restore(saved)
	exec.withdrawAddressed()
	current, err := exec.evalExpression(path.root.expr, env)
	if err != nil {
		return NewNil(), err
	}
	return exec.readPathTailOrdinarily(current, path.steps, env)
}

// readAutoInvokedRootPath claims a local-rooted path the walk reported
// unresolved when its bare name denotes a call rather than a slot -- an
// implicit self method (`geta[0] = 9`) or a binding holding a callable. What
// such a name yields is a call result, exactly like an explicit call root, so
// the path reads through readCallRootedPath and the caller classifies the
// leaf as a temporary. A name that denotes no call -- undefined, a class
// constant, ivar-named data on self -- is left unclaimed, so its route and
// error identity stay exactly as they were.
func (exec *Execution) readAutoInvokedRootPath(path mutablePath, env *Env) (Value, bool, error) {
	if path.root.kind != mutableRootLocal || !exec.identifierRootAutoInvokes(path.root) {
		return NewNil(), false, nil
	}
	leaf, err := exec.readCallRootedPath(path, env)
	return leaf, true, err
}

// identifierRootAutoInvokes reports whether ordinary evaluation of this bare
// name reaches autoInvokeIfNeeded with a callable -- the condition under
// which the name denotes a call, not a slot. It mirrors the evaluator's
// Identifier resolution order exactly, with pure lookups only: block_given?
// is a special form, a call-local or env binding answers for itself, a class
// constant is a plain value read, and an unbound name resolves as an implicit
// member of self, where a callable member auto-invokes and anything else --
// a data member, a resolution error -- is a value read or an error the
// unclaimed route reproduces unchanged.
func (exec *Execution) identifierRootAutoInvokes(root mutableRoot) bool {
	name, env := root.name, root.env
	if name == blockGivenName {
		return false
	}
	if isConstantIdentifier(name) {
		if val, ok := env.getCallLocal(name); ok {
			return rootAutoInvokes(val)
		}
		if self, ok := env.Get("self"); ok && (self.Kind() == KindInstance || self.Kind() == KindClass) {
			if _, ok := classConstant(self, name); ok {
				return false
			}
		}
	}
	if val, ok := env.Get(name); ok {
		return rootAutoInvokes(val)
	}
	self, ok := env.Get("self")
	if !ok || (self.Kind() != KindInstance && self.Kind() != KindClass) {
		return false
	}
	member, err := exec.getMember(self, name, root.expr.Pos())
	if err != nil {
		return false
	}
	return rootAutoInvokes(member)
}

// readTailMemberHop reads one member hop the way ordinary evaluation would:
// resolve the public member and auto-invoke it against the receiver.
func (exec *Execution) readTailMemberHop(current Value, step *mutablePathStep) (Value, error) {
	if step.member.Safe && current.Kind() == KindNil {
		return NewNil(), nil
	}
	member, err := exec.getPublicMember(current, step.member.Property, step.member.Pos())
	if err != nil {
		return NewNil(), err
	}
	return exec.autoInvokeIfNeeded(step.member, member, current)
}

// readPathTailOrdinarily reads remaining hops on a temporary, each exactly as
// evaluating the expression would. It runs once, in the reading pass, so a
// getter it invokes runs once. Each hop charges exec.step and the
// receiver's memory the way the corresponding MemberExpr or IndexExpr
// node would under ordinary evaluation, so a long tail after a call
// root cannot skip the step or memory quota.
// The caller has already withdrawn the addressability permission, so a
// mutator hop writes the temporary, never a value an enclosing mutator's
// record still vouches for.
func (exec *Execution) readPathTailOrdinarily(current Value, steps []mutablePathStep, env *Env) (Value, error) {
	for i := range steps {
		if err := exec.step(); err != nil {
			return NewNil(), err
		}
		// Ordinary MemberExpr/IndexExpr evaluation charges the receiver
		// before the hop (finishMemberExpr, evalIndexExpr). Do the same
		// here so a large intermediate after a call root cannot vanish
		// from the live-base walk before the next hop selects a small leaf.
		if err := exec.checkMemoryValue(current); err != nil {
			return NewNil(), err
		}
		step := &steps[i]
		if step.member != nil {
			var err error
			current, err = exec.readTailMemberHop(current, step)
			if err != nil {
				return NewNil(), err
			}
			continue
		}
		indices, err := exec.evalIndexSelectors(step.expr, current, env)
		if err != nil {
			return NewNil(), err
		}
		result, err := exec.evalIndexValue(step.expr, current, indices)
		if err != nil {
			return NewNil(), err
		}
		if exec.memoryQuota > 0 {
			chargeable := make([]Value, 0, len(indices)+2)
			chargeable = append(chargeable, current)
			chargeable = append(chargeable, indices...)
			chargeable = append(chargeable, result)
			if err := exec.checkMemoryWith(chargeable...); err != nil {
				return NewNil(), err
			}
		}
		current = result
	}
	return current, nil
}

// arrayRangeTemporary evaluates a range index as an ordinary slice, then
// finishes any remaining hops the same way. The slice is a temporary, so the
// path is no longer addressable and the hops run with the permission
// withdrawn.
func (exec *Execution) arrayRangeTemporary(container Value, step *mutablePathStep, tail []mutablePathStep, env *Env) (Value, bool, error) {
	if container.Kind() != KindArray || !step.indexed || step.index.Kind() != KindRange {
		return NewNil(), false, nil
	}
	sliced, err := exec.evalIndexValue(step.expr, container, []Value{step.index})
	if err != nil {
		return NewNil(), true, err
	}
	result, err := exec.readPathTailWithdrawn(sliced, tail, env)
	return result, true, err
}

// readPathTailWithdrawn finishes remaining hops on an evaluated temporary
// with the permission withdrawn for all of them, so no hop can write through
// a record an enclosing mutator still holds.
func (exec *Execution) readPathTailWithdrawn(current Value, steps []mutablePathStep, env *Env) (Value, error) {
	saved := exec.savedAddressedScope()
	defer exec.restore(saved)
	exec.withdrawAddressed()
	return exec.readPathTailOrdinarily(current, steps, env)
}

// readCollectionStep reads one hop that addresses collection storage, caching
// the hop's key the first time so the isolating pass and the write-back address
// the same slot the read did.
func (exec *Execution) readCollectionStep(container Value, step *mutablePathStep, env *Env) (Value, bool, error) {
	if step.member != nil {
		switch container.Kind() {
		case KindHash, KindObject:
			key := NewString(step.member.Property)
			if container.Kind() == KindHash {
				key = hashMemberAssignmentKey(container, step.member.Property)
			}
			step.index = key
			val, ok, err := hashGet(container, key)
			if err != nil {
				return NewNil(), false, exec.errorAt(step.pos, "%s", err.Error())
			}
			return val, ok, nil
		default:
			return NewNil(), false, nil
		}
	}
	if !step.indexed {
		indices, err := exec.evalIndexSelectors(step.expr, container, env)
		if err != nil {
			return NewNil(), false, err
		}
		if len(indices) != 1 {
			return NewNil(), false, nil
		}
		step.index = indices[0]
		step.indexed = true
	}
	switch container.Kind() {
	case KindArray:
		// A range produces a fresh subarray, not a slot the write can rebind.
		if step.index.Kind() == KindRange {
			return NewNil(), false, nil
		}
		i, err := exec.indexSelectorToInt(step.expr, step.index, 0)
		if err != nil {
			return NewNil(), false, err
		}
		elems := container.Array()
		if i < 0 {
			i += len(elems)
		}
		if i < 0 || i >= len(elems) {
			return NewNil(), false, nil
		}
		return elems[i], true, nil
	case KindHash, KindObject:
		val, ok, err := hashGet(container, step.index)
		if err != nil {
			return NewNil(), false, exec.errorAt(step.pos, "%s", err.Error())
		}
		return val, ok, nil
	default:
		return NewNil(), false, nil
	}
}

// storeMutableStep writes a copied level back into its exclusively held parent.
// It does not publish: the copy replaces what the slot already held.
func (exec *Execution) storeMutableStep(container Value, step *mutablePathStep, val Value) error {
	switch container.Kind() {
	case KindArray:
		i, err := exec.indexSelectorToInt(step.expr, step.index, 0)
		if err != nil {
			return err
		}
		elems := container.Array()
		if i < 0 {
			i += len(elems)
		}
		if i < 0 || i >= len(elems) {
			return nil
		}
		container.BumpMutationEpoch()
		elems[i] = val
		return nil
	case KindHash, KindObject:
		if err := objectTagMutationError(container, "assignment"); err != nil {
			return exec.errorAt(step.pos, "%s", err.Error())
		}
		if err := container.HashSetOwned(step.index, val); err != nil {
			return exec.errorAt(step.pos, "%s", err.Error())
		}
		return nil
	default:
		return nil
	}
}

// isCollectionMutator reports whether property names a member that updates its
// receiver in place. Dispatch consults it before evaluating the receiver, so
// that a mutator gets an addressable path where the source provides one, and the
// mutators themselves consult writableCollection so that no other dispatch route
// -- send, an operator, a host builtin -- can reach a write past the guard.
//
// The bang-named members that duplicated a non-bang transformation are gone
// (ADR-006 item 2), so what is left is the set with no non-mutating spelling:
// growth, removal, and whole-receiver replacement.
//
// It is a switch rather than a set lookup because every member expression and
// every member call asks it, and hashing the property name there was
// measurable. The names are also strings a String receiver answers
// non-mutatingly (delete, replace, insert, clear, prepend), which costs those
// calls the reading pass of a path walk and nothing else: the walk isolates
// nothing once it sees the receiver is not a collection.
func isCollectionMutator(property string) bool {
	switch property {
	case "push", "append", "prepend", "unshift", "pop", "shift", "insert", "fill",
		"delete", "delete_if", "keep_if", "clear", "store", "replace":
		return true
	default:
		return false
	}
}

// recordsMutableReceiver reports whether a member call should record the
// receiver's addressable path. Collection mutators need it so the write can
// rebind the source. send and public_send need it when they dispatch to one
// of those mutators; the send builtin drops the record if the target is not
// a mutator.
func recordsMutableReceiver(property string) bool {
	return isCollectionMutator(property) || property == "send" || property == "public_send"
}

// skipUnderCollectionCopyVerify reports whether the always-copy oracle is on,
// for the tests that measure what a write costs rather than what it means.
//
// The oracle copies and rebinds an addressable path on every write instead of
// asking whether each level is exclusively held. That is the point -- it is how
// a missed publish shows up as a semantic disagreement -- but it also changes
// every byte charged, every step taken, and every backing retained, so a test
// asserting a quota threshold, a linear step count, or a released backing has no
// stable answer under it. Those tests skip; the semantic ones, which are what
// the oracle exists to check, all run.
func skipUnderCollectionCopyVerify() bool { return alwaysCopyCollections }
