package value

import (
	"reflect"
	"sync"
	"sync/atomic"
	"unsafe"
)

var (
	mutationEpoch        atomic.Uint64
	opaqueMutationEpoch  atomic.Uint64
	wrapperMutationEpoch atomic.Uint64
	wrapperMutations     [256]mutationRecord
)

type mutationRecord struct {
	mu       sync.Mutex
	sequence atomic.Uint64
	kind     atomic.Uint64
	identity atomic.Uintptr
	backing  atomic.Uintptr
}

// MutationKind identifies the estimator identity an observed mutation changes.
// It is internal interpreter bookkeeping and carries no compatibility promise.
type MutationKind uint64

const (
	// MutationSlice changes an existing array backing.
	MutationSlice MutationKind = iota + 1
	// MutationHash changes a hash wrapper or its backing.
	MutationHash
	// MutationObject changes an object wrapper or its backing.
	MutationObject
)

// MutationEpoch returns the process-wide mutation sequence. It is intended for
// interpreter bookkeeping and carries no compatibility promise.
func MutationEpoch() uint64 { return mutationEpoch.Load() }

// OpaqueMutationEpoch returns the sequence of writes whose affected identities
// are unknown, including raw host writes. Every estimator must invalidate after
// one of these writes. It carries no compatibility promise.
func OpaqueMutationEpoch() uint64 { return opaqueMutationEpoch.Load() }

// WrapperMutationEpoch returns the latest wrapper journal sequence. It is
// intended for interpreter bookkeeping and carries no compatibility promise.
func WrapperMutationEpoch() uint64 { return wrapperMutationEpoch.Load() }

// BumpMutationEpoch conservatively invalidates every estimator. Use it for raw
// writes whose affected identities cannot be tracked. It is intended for
// interpreter bookkeeping and carries no compatibility promise.
func BumpMutationEpoch() {
	opaqueMutationEpoch.Add(1)
	mutationEpoch.Add(1)
}

// BumpLocalMutationEpoch records a write tracked separately by its lexical
// environment. It preserves the process-wide signal used by contract checks
// without invalidating unrelated estimator graphs. It is intended for
// interpreter bookkeeping and carries no compatibility promise.
func BumpLocalMutationEpoch() { mutationEpoch.Add(1) }

func recordWrapperMutation(kind MutationKind, identity, backing uintptr) {
	sequence := wrapperMutationEpoch.Add(1)
	record := &wrapperMutations[sequence%uint64(len(wrapperMutations))]
	record.mu.Lock()
	// A reader only accepts a slot whose sequence matches both before and
	// after reading the payload. In-flight and overwritten records therefore
	// cost a conservative re-walk, never an unobserved mutation.
	record.sequence.Store(0)
	record.kind.Store(uint64(kind))
	record.identity.Store(identity)
	record.backing.Store(backing)
	record.sequence.Store(sequence)
	record.mu.Unlock()
	mutationEpoch.Add(1)
}

// WrapperMutationsAffect reports whether writes in (after, through] affect a
// cached graph. Missing or overwritten records conservatively answer true. The
// fixed journal stores addresses rather than pointers, so it retains no values
// and allocates no memory per write. It is intended for interpreter bookkeeping
// and carries no compatibility promise.
func WrapperMutationsAffect(after, through uint64, affects func(MutationKind, uintptr, uintptr) bool) bool {
	if through-after > uint64(len(wrapperMutations)) {
		return true
	}
	for offset := range through - after {
		sequence := after + offset + 1
		record := &wrapperMutations[sequence%uint64(len(wrapperMutations))]
		if record.sequence.Load() != sequence {
			return true
		}
		kind := MutationKind(record.kind.Load())
		identity := record.identity.Load()
		backing := record.backing.Load()
		if record.sequence.Load() != sequence || affects(kind, identity, backing) {
			return true
		}
	}
	return false
}

// BumpMutationEpoch records an in-place write to v before its backing changes.
// The estimator already remembers hash/object wrappers and array backings, so
// it can invalidate only graphs containing that identity. Empty arrays retain
// the conservative fallback because their backing identity is not reliable in
// every supported build configuration.
// It is intended for interpreter bookkeeping and carries no compatibility
// promise.
func (v Value) BumpMutationEpoch() {
	switch v.kind {
	case KindArray:
		ad := v.data.(*arrayData)
		if len(ad.elems) == 0 {
			BumpMutationEpoch()
			return
		}
		recordWrapperMutation(MutationSlice, uintptr(unsafe.Pointer(unsafe.SliceData(ad.elems))), 0)
	case KindHash:
		recordWrapperMutation(MutationHash, HashIdentity(v), reflect.ValueOf(v.hashEntryMap()).Pointer())
	case KindObject:
		recordWrapperMutation(MutationObject, ObjectIdentity(v), reflect.ValueOf(v.hashEntryMap()).Pointer())
	default:
		BumpMutationEpoch()
	}
}
