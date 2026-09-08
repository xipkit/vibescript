package value

import (
	"runtime"
	"sync"
	"testing"
)

func TestWrapperMutationJournalConcurrentWriters(t *testing.T) {
	var wg sync.WaitGroup
	for range 4 {
		hash := NewHashWithCapacity(1)
		wg.Go(func() {
			identity := HashIdentity(hash)
			for i := range 1000 {
				before := WrapperMutationEpoch()
				if err := hash.HashSet(NewString("state"), NewInt(int64(i))); err != nil {
					t.Error(err)
					return
				}
				after := WrapperMutationEpoch()
				if !WrapperMutationsAffect(before, after, func(kind MutationKind, id, _ uintptr) bool {
					return kind == MutationHash && id == identity
				}) {
					t.Errorf("journal missed hash mutation between %d and %d", before, after)
					return
				}
			}
		})
	}
	wg.Wait()
}

func TestWrapperMutationJournalIdentities(t *testing.T) {
	hash := NewHashWithCapacity(1)
	// Journal identities must refer to heap values across stack growth.
	t.Cleanup(func() { runtime.KeepAlive(hash) })
	before := WrapperMutationEpoch()
	if err := hash.HashSet(NewString("state"), NewInt(1)); err != nil {
		t.Fatal(err)
	}
	after := WrapperMutationEpoch()
	if WrapperMutationsAffect(before, after, func(MutationKind, uintptr, uintptr) bool { return false }) {
		t.Fatal("one unrelated mutation invalidated a graph")
	}
	if !WrapperMutationsAffect(before, after, func(kind MutationKind, id, _ uintptr) bool {
		return kind == MutationHash && id == HashIdentity(hash)
	}) {
		t.Fatal("journal did not identify the mutated hash wrapper")
	}
}

func TestEmptyArrayMutationUsesOpaqueEpoch(t *testing.T) {
	array := NewArray(nil)
	before := OpaqueMutationEpoch()
	array.SetArrayElems([]Value{NewInt(1)})
	if OpaqueMutationEpoch() == before {
		t.Fatal("empty array growth did not advance the conservative epoch")
	}
}
