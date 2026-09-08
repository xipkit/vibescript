package value

import (
	"math"
	"sync/atomic"
	"time"
	"unsafe"
)

// NewNil returns a nil Value.
func NewNil() Value { return Value{kind: KindNil} }

// NewBool returns a boolean Value.
func NewBool(b bool) Value {
	if b {
		return Value{kind: KindBool, scalar: 1}
	}
	return Value{kind: KindBool}
}

// NewInt returns an integer Value.
func NewInt(i int64) Value { return Value{kind: KindInt, scalar: uint64(i)} }

// NewFloat returns a floating-point Value.
func NewFloat(f float64) Value { return Value{kind: KindFloat, scalar: math.Float64bits(f)} }

// NewString returns a string Value.
func NewString(s string) Value { return Value{kind: KindString, data: s} }

// arrayData backs a KindArray value. Boxing the element slice behind a pointer
// lets the runtime's mutators (push, pop, clear, ...) grow or shrink an array in
// place while every Value that still names this wrapper keeps describing it.
//
// Arrays are values, so naming one wrapper from two places is a representation
// detail, not language-visible sharing: a mutator may only write through a
// wrapper it can prove is named from at most one durable slot, and copies the
// wrapper first when it cannot (see refs). Two wrappers therefore never share an
// element backing, which is what NewArray's ownership rule already required.
//
// head is how many element slots sit between the start of the slice the wrapper
// was first built over and the start of elems. A shrink that takes elements off
// the front hands the wrapper a window further into the same allocation, and Go
// keeps an allocation live as a whole for any pointer into it, so those slots
// are still held while nothing addresses them any more -- and a slice header
// says nothing about what is in front of it.
//
// Only the runtime's shrink maintains it, and only to decide when the vacated
// prefix has grown large enough to be worth copying the survivors out of.
// Nothing reads it as an accounting figure, so it is a cost heuristic rather
// than a fact: too large means one copy that was not needed, and too small --
// which is what a wrapper built over a slice that was already a window of
// something larger starts out with -- means a prefix the host put there stays
// held, exactly as it would have without any of this. It is an int32 so that it
// and refs share one word and the wrapper stays the size it was before refs
// existed; a head beyond two billion slots is tens of gigabytes of elements,
// far past any quota, and saturating there only forgoes a copy heuristic.
type arrayData struct {
	elems []Value
	head  int32
	refs  uint32
}

// ArrayDataBytes is the heap footprint of the arrayData wrapper every KindArray
// value allocates, excluding the element backing it points at. Memory-quota
// accounting charges it once per distinct array so a workload retaining many
// small arrays cannot hold the per-array wrapper cost uncharged.
//
// It is derived from the struct rather than restated as a number so that a
// field added to arrayData is charged by the same commit that adds it. head was
// added without one, which took the wrapper from 24 bytes to 32 and left 8 of
// them unmetered per array -- an under-count introduced by a fix for an
// under-count. It is intended for the interpreter's internal use; hosts should
// not rely on it, and it carries no compatibility promise (see
// docs/embedding-api-stability.md).
const ArrayDataBytes = int(unsafe.Sizeof(arrayData{}))

// NewArray returns an array Value backed by a, which it takes ownership of.
//
// The slice must not be one another array Value is already built over. An
// in-place shrink clears the slots it vacates, so a second array over the same
// storage would find elements it still shows blanked out, and a growing push
// through one of them reallocates without the other seeing it. Build a second
// array over the same contents with a copy of the slice, not the slice.
//
// Every collection among the elements is published: an element slot now names
// it as well as wherever the caller got it from, so a later write through
// either must copy first. This is the one place array construction can see all
// of the elements at once, which is why the publish lives here rather than at
// the hundred builders that call it.
func NewArray(a []Value) Value {
	PublishRefElems(a)
	return Value{kind: KindArray, data: &arrayData{elems: a}}
}

// AdoptArray wraps a without publishing its elements. Callers use it when
// those elements were already published as they entered the slice — a map
// that counted each block result at insert time — so wrapping the finished
// slice must not count the same slots again.
//
// It is intended for the interpreter's internal use; hosts should not call
// it, and it carries no compatibility promise (see
// docs/embedding-api-stability.md).
func AdoptArray(a []Value) Value {
	return Value{kind: KindArray, data: &arrayData{elems: a}}
}

// SetArrayElems replaces the element slice of an existing array wrapper in
// place. It is the primitive behind the runtime's Ruby-style mutators: because
// the wrapper is shared by every Value that aliases the array, the new elements
// are visible through all of them. It is a no-op when v is not an array.
// It is intended for the interpreter's internal use; hosts should not call
// it, and it carries no compatibility promise (see
// docs/embedding-api-stability.md).
func (v Value) SetArrayElems(elems []Value) {
	if v.kind != KindArray {
		return
	}
	if ad, ok := v.data.(*arrayData); ok {
		v.BumpMutationEpoch()
		if !sameElemStart(ad.elems, elems) {
			// Elements that start somewhere else are a different allocation --
			// a mutator's freshly built slice, or an append that outgrew its
			// backing -- so whatever prefix the old one had is not in front of
			// this one. A window further into the same allocation must go
			// through SetArrayWindow, which says how far in it now starts.
			ad.head = 0
		}
		ad.elems = elems
	}
}

// SetArrayWindow narrows an array onto a window of the allocation its elements
// already sit in, recording how many slots of that allocation now sit in front
// of the window. head is counted from the start of the allocation, not from the
// elements the array showed before.
//
// It exists because a narrowed array goes on holding the whole allocation while
// the slice header it keeps describes less and less of it, and nothing about
// that header says how much is in front. It is intended for the interpreter's
// internal use; hosts should not call it, and it carries no compatibility
// promise (see docs/embedding-api-stability.md).
func (v Value) SetArrayWindow(elems []Value, head int) {
	if v.kind != KindArray {
		return
	}
	if ad, ok := v.data.(*arrayData); ok {
		v.BumpMutationEpoch()
		ad.elems = elems
		ad.head = clampWindowHead(head)
	}
}

// ArrayWindowHead returns how many element slots an array's elements start
// past the beginning of the allocation they sit in, or 0 when v is not an
// array. It is intended for the interpreter's internal use; hosts should not
// call it, and it carries no compatibility promise (see
// docs/embedding-api-stability.md).
func ArrayWindowHead(v Value) int {
	if v.kind != KindArray {
		return 0
	}
	if ad, ok := v.data.(*arrayData); ok {
		return int(ad.head)
	}
	return 0
}

// clampWindowHead saturates a window head into the int32 the wrapper stores it
// in. Saturating only forgoes the copy heuristic head feeds, never correctness;
// see arrayData for why the field is narrow.
func clampWindowHead(head int) int32 {
	if head > math.MaxInt32 {
		return math.MaxInt32
	}
	if head < 0 {
		return 0
	}
	return int32(head)
}

// sameElemStart reports whether two element slices begin at the same address,
// which is what tells an in-place append from one that moved the elements to a
// new allocation. Two empty slices are not the same start: an empty one has no
// address to speak of, and a wrapper handed one is holding nothing whatever it
// held before.
func sameElemStart(old, next []Value) bool {
	if len(old) == 0 || len(next) == 0 {
		return false
	}
	return &old[0] == &next[0]
}

// AppendArrayElemNoEpoch appends elem to an array in place without bumping
// the mutation epoch. The epoch exists to invalidate memoized reachable-graph
// walks; the interpreter's charged-append path commits the element's bytes
// into that memo itself before appending, so the bump would only throw away
// accounting that is already correct. Callers that do not maintain the memo
// must use SetArrayElems. It is intended for the interpreter's internal use;
// hosts should not call it, and it carries no compatibility promise (see
// docs/embedding-api-stability.md).
func (v Value) AppendArrayElemNoEpoch(elem Value) {
	if v.kind != KindArray {
		return
	}
	if ad, ok := v.data.(*arrayData); ok {
		grown := append(ad.elems, elem)
		if !sameElemStart(ad.elems, grown) {
			ad.head = 0
		}
		ad.elems = grown
	}
}

// ArrayIdentity returns an identity for an array wrapper, or 0 when v is not
// an array. It identifies the shared mutable wrapper itself rather than the
// current element backing, so identity survives in-place growth that reallocates
// the element slice. Two Values are the same array object exactly when their
// identities match.
//
// The identity is the wrapper's address as a bare uintptr, so it is only
// meaningful between captures taken while the address cannot move: Go may
// stack-allocate a wrapper that never escapes its function, and a goroutine
// stack growth then relocates it, changing the identity of a still-live array.
// The runtime only compares identities captured within a single traversal of a
// heap-reachable graph, where this cannot happen. It is intended for the
// interpreter's internal use and carries no compatibility promise (see
// docs/embedding-api-stability.md). Wrapper identity is not part of the
// supported host API: Value.Identical compares collection contents, not
// wrappers. Hosts that need provenance must track it outside Value.
func ArrayIdentity(v Value) uintptr {
	if v.kind != KindArray {
		return 0
	}
	if ad, ok := v.data.(*arrayData); ok {
		return uintptr(unsafe.Pointer(ad))
	}
	return 0
}

// hashData backs a KindHash value. It holds the entry map and the Ruby-style
// insertion order of its keys; KindObject keeps a bare map because an
// attribute bag records no order.
//
// order lists each entry key exactly once, as the KindString Value iteration
// hands out: HashSet appends a new key and keeps an overwritten key at its
// original position. A bare Go map handed to NewHash carries no insertion
// record, so NewHash seeds order from its sorted keys and the hash iterates
// sorted, as it always has.
type hashData struct {
	entries       map[string]Value
	entryCapacity int32
	refs          uint32
	order         []Value
	// orderUntrusted is set when Hash() hands out the live map, or when
	// NewHash retains a non-empty caller map. A host can then delete a
	// key, HashSet it again (which would append a duplicate), and insert
	// another key through the map, leaving a same-length order that names
	// a key twice. The flag stays set for the life of that exposure so
	// later HashSet traffic keeps reconciling rather than trusting the
	// record. It is atomic because Hash() is a documented concurrent read.
	orderUntrusted atomic.Bool
}

// HashDataBytes is the heap footprint of the hashData wrapper every KindHash
// value allocates, excluding the entry map and order backing it points at.
// Memory-quota accounting charges it once per distinct hash so an array of many
// small hashes cannot retain the per-hash wrapper cost uncharged.
// It is intended for the interpreter's internal use; hosts should not rely
// on it, and it carries no compatibility promise (see
// docs/embedding-api-stability.md).
const HashDataBytes = int(unsafe.Sizeof(hashData{}))

// NewHash returns a hash (map) Value over h. A non-empty map records no
// insertion order, so the hash iterates in sorted key order. The map is
// retained, not copied: if the caller keeps it, later mutation is treated
// like Hash() exposure and the recorded order stays untrusted.
func NewHash(h map[string]Value) Value {
	PublishRefEntries(h)
	hd := &hashData{entries: h, entryCapacity: int32(len(h))}
	if len(h) > 0 {
		hd.order = sortedMapKeysInto(h, nil)
		hd.orderUntrusted.Store(true)
	}
	return Value{kind: KindHash, data: hd}
}

// AdoptHash wraps h without publishing its values. Callers use it when the
// map is accounting scratch that disappears after a memory check, so the
// wrapper must not mark those values retained.
//
// It is intended for the interpreter's internal use; hosts should not call
// it, and it carries no compatibility promise (see
// docs/embedding-api-stability.md).
func AdoptHash(h map[string]Value) Value {
	hd := &hashData{entries: h, entryCapacity: int32(len(h))}
	if len(h) > 0 {
		hd.order = sortedMapKeysInto(h, nil)
	}
	return Value{kind: KindHash, data: hd}
}

// NewHashWithOrder returns a hash over entries that iterates in the given key
// order. The order slice is adopted, so callers must not retain or reuse it; an
// order that does not cover entries falls back to sorted iteration. It exists
// for the copiers that clone an entry map wholesale and must reproduce the
// source's iteration order without rehashing every key.
// It is intended for the interpreter's internal use; hosts should not call
// it, and it carries no compatibility promise (see
// docs/embedding-api-stability.md).
func NewHashWithOrder(entries map[string]Value, order []Value) Value {
	if !orderNamesUnique(order) {
		// A caller-supplied order that repeats a name can match the
		// entry count while omitting another live key. Drop it so
		// iteration takes the sorted fallback.
		order = nil
	}
	return NewHashWithTrustedOrder(entries, order)
}

// NewHashWithTrustedOrder is NewHashWithOrder for an order that is already
// unique, such as HashKeyOrder() output. The inbound clone path uses this
// so Script.Call does not re-validate a snapshot it just took.
// It is intended for the interpreter's internal use; hosts should not call
// it, and it carries no compatibility promise (see
// docs/embedding-api-stability.md).
func NewHashWithTrustedOrder(entries map[string]Value, order []Value) Value {
	PublishRefEntries(entries)
	return Value{kind: KindHash, data: &hashData{
		entries:       entries,
		entryCapacity: int32(len(entries)),
		order:         order,
	}}
}

func orderNamesUnique(order []Value) bool {
	for i := range order {
		name := order[i].data.(string)
		for j := i + 1; j < len(order); j++ {
			if order[j].data.(string) == name {
				return false
			}
		}
	}
	return true
}

// NewHashWithCapacity returns an empty hash whose entry map and insertion-order
// backing are pre-sized for capacity entries.
func NewHashWithCapacity(capacity int) Value {
	var order []Value
	if capacity > 0 {
		order = make([]Value, 0, capacity)
	}
	return Value{kind: KindHash, data: &hashData{
		entries:       make(map[string]Value, capacity),
		entryCapacity: int32(capacity),
		order:         order,
	}}
}

// HashIdentity returns an identity for a hash wrapper, or 0 when v is not
// a hash. Unlike the entry-map pointer, this identifies the whole hashData
// wrapper, so two KindHash values that share an entry map are distinct.
//
// Like ArrayIdentity, the identity is a bare uintptr wrapper address and is
// only meaningful between captures taken while the address cannot move (see
// ArrayIdentity for the stack-allocation caveat and host guidance). It is
// intended for the interpreter's internal use and carries no compatibility
// promise (see docs/embedding-api-stability.md).
func HashIdentity(v Value) uintptr {
	if v.kind != KindHash {
		return 0
	}
	if hd, ok := v.data.(*hashData); ok {
		return uintptr(unsafe.Pointer(hd))
	}
	return 0
}

// ObjectIdentity returns the identity of the objectData wrapper a KindObject
// value allocates, or 0 for anything else. Each wrapper is a distinct
// allocation even when several share one entry map, so the sandbox's memory
// accounting deduplicates on this rather than on the map.
//
// Like ArrayIdentity, it is a bare uintptr wrapper address (see ArrayIdentity
// for the stack-allocation caveat and host guidance). It is intended for the
// interpreter's internal use and carries no compatibility promise (see
// docs/embedding-api-stability.md).
func ObjectIdentity(v Value) uintptr {
	if v.kind != KindObject {
		return 0
	}
	if od, ok := v.data.(*objectData); ok {
		return uintptr(unsafe.Pointer(od))
	}
	return 0
}

// NewMoney returns a money Value.
func NewMoney(m Money) Value { return Value{kind: KindMoney, data: m} }

// NewDuration returns a duration Value.
func NewDuration(d Duration) Value {
	return Value{kind: KindDuration, scalar: uint64(d.Seconds())}
}

// NewTime returns a time Value.
func NewTime(t time.Time) Value { return Value{kind: KindTime, data: t} }

// NewSymbol returns a symbol Value.
func NewSymbol(name string) Value { return Value{kind: KindSymbol, data: name} }

// NewObject returns an object Value with the given attributes. A nil map is
// replaced with a freshly allocated empty map so that every object has its own
// backing storage and thus a distinct object identity. (Hashes get the same
// per-instance identity from their hashData wrapper, which NewHash allocates
// fresh on every call.)
func NewObject(attrs map[string]Value) Value {
	if attrs == nil {
		attrs = map[string]Value{}
	}
	PublishRefEntries(attrs)
	return Value{kind: KindObject, data: &objectData{entries: attrs}}
}

// objectData is a KindObject payload: the entry map plus, for the few bags the
// runtime builds to stand for something specific, the provenance and the
// string form fixed at construction.
//
// The string form is stored rather than read back out of the entries because
// the entries are mutable and reachable through the public API -- Value.Hash()
// hands out the live map, and a host builtin receives the value itself. A
// rendering derived from the entries could therefore be rewritten by anything
// holding the bag, which is exactly the spoof the provenance exists to
// prevent. Fixing it at construction makes the rendering immutable no matter
// who mutates the map afterwards.
type objectData struct {
	entries    map[string]Value
	refs       uint32
	tag        ObjectTag
	stringForm string
}

// ObjectTag records what an attribute bag is, for the few bags the runtime
// builds to stand for something specific.
//
// It is intended for the interpreter's internal use; hosts should not rely
// on it, and it carries no compatibility promise (see
// docs/embedding-api-stability.md). Behavior that would otherwise have
// to be inferred from a bag's field names reads the tag instead: field names
// are public, host-settable data, so any bag could carry the same ones and
// claim the same treatment.
type ObjectTag uint8

const (
	// ObjectTagNone marks an ordinary attribute bag, which is every bag
	// NewObject builds. It is the zero value, so untagged is the default.
	ObjectTagNone ObjectTag = iota
	// ObjectTagRescuedError marks the bag a rescue binds, whose to_s is the
	// error message.
	ObjectTagRescuedError
	// ObjectTagMatchData marks the bag a regexp match returns, whose to_s is
	// the matched text as in Ruby's MatchData.
	ObjectTagMatchData
)

// NewTaggedObject returns an attribute bag carrying provenance. The tag rides
// in the scalar word, which an object otherwise leaves unused, so it costs
// nothing and cannot appear as an entry: it is invisible to keys, values,
// inspect, and JSON, and script code has no way to set it.
//
// It is intended for the interpreter's internal use; hosts should not call
// it, and it carries no compatibility promise (see
// docs/embedding-api-stability.md).
func NewTaggedObject(attrs map[string]Value, tag ObjectTag, stringForm string) Value {
	if attrs == nil {
		attrs = map[string]Value{}
	}
	PublishRefEntries(attrs)
	return Value{kind: KindObject, data: &objectData{entries: attrs, tag: tag, stringForm: stringForm}}
}

// ObjectTag reports the provenance of an attribute bag, or ObjectTagNone for
// anything else.
//
// It is intended for the interpreter's internal use; hosts should not rely
// on it, and it carries no compatibility promise (see
// docs/embedding-api-stability.md). A bag that has been rebuilt (merged, duplicated, or produced
// by host code) reports ObjectTagNone, so the tag only ever vouches for a bag
// the runtime built itself.
func (v Value) ObjectTag() ObjectTag {
	if v.kind != KindObject {
		return ObjectTagNone
	}
	return v.data.(*objectData).tag
}

// ObjectStringForm returns the rendering a tagged bag published at
// construction, and reports false for an ordinary bag. It is fixed then and
// never read back out of the entries, so mutating them cannot change it.
//
// It is intended for the interpreter's internal use; hosts should not rely
// on it, and it carries no compatibility promise (see
// docs/embedding-api-stability.md).
func (v Value) ObjectStringForm() (string, bool) {
	if v.kind != KindObject {
		return "", false
	}
	obj := v.data.(*objectData)
	if obj.tag == ObjectTagNone {
		return "", false
	}
	return obj.stringForm, true
}

// NewRange returns a range Value.
func NewRange(r Range) Value { return Value{kind: KindRange, data: r} }

// ObjectDataBytes is the heap footprint of the objectData wrapper every
// KindObject value allocates around its entry map, for the sandbox's memory
// accounting.
//
// It is intended for the interpreter's internal use; hosts should not rely
// on it, and it carries no compatibility promise (see
// docs/embedding-api-stability.md).
const ObjectDataBytes = int(unsafe.Sizeof(objectData{}))
