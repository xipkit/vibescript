package runtime

import (
	"fmt"
	"testing"

	"github.com/mgomes/vibescript/vibes/value"
)

func TestHostHashScratchPreservesNestedValues(t *testing.T) {
	t.Parallel()
	for _, width := range []int{4, 8, 9, 32} {
		t.Run(fmt.Sprintf("width=%d", width), func(t *testing.T) {
			input := NewInt(9)
			for range 4 {
				hash := NewHashWithCapacity(width)
				setClonedHashEntry(hash, NewString("child"), input)
				for i := range width - 1 {
					setClonedHashEntry(hash, NewString(fmt.Sprintf("key%d", i)), NewInt(int64(i)))
				}
				input = hash
			}
			input.ReserveHashCapacity(width * 2)
			input.ReserveHashOrder(width * 2)
			cloned := cloneValueForHost(input)
			if !cloned.Equal(input) || hashIdentity(cloned) == hashIdentity(input) {
				t.Fatal("host clone must preserve nested values in an independent hash")
			}
			if value.HashEntryCapacity(cloned) != value.HashEntryCapacity(input) || value.HashOrderCapacity(cloned) != value.HashOrderCapacity(input) {
				t.Fatal("host clone changed reserved hash capacities")
			}
			child, found, err := cloned.HashGet(NewString("child"))
			if err != nil || !found {
				t.Fatalf("cloned child found=%t, error=%v, want true, nil", found, err)
			}
			if err := child.HashSet(NewString("changed"), NewBool(true)); err != nil {
				t.Fatal(err)
			}
			originalChild, _, err := input.HashGet(NewString("child"))
			if err != nil {
				t.Fatal(err)
			}
			if _, found, err := originalChild.HashGet(NewString("changed")); err != nil || found {
				t.Errorf("host mutation reached original child: found=%t, error=%v", found, err)
			}
		})
	}
}

func TestHostHashScratchPreservesCyclesAndAliases(t *testing.T) {
	t.Parallel()
	input := NewHashWithCapacity(3)
	shared := NewArray([]Value{NewInt(1)})
	setClonedHashEntry(input, NewSymbol("self"), input)
	setClonedHashEntry(input, NewString("first"), shared)
	setClonedHashEntry(input, NewString("second"), shared)
	cloned := cloneValueForHost(input)
	entries := cloned.HashEntryMap()
	if hashIdentity(entries["self"]) != hashIdentity(cloned) {
		t.Error("host clone broke its self reference")
	}
	if arrayIdentity(entries["first"]) != arrayIdentity(entries["second"]) || arrayIdentity(entries["first"]) == arrayIdentity(shared) {
		t.Error("host clone must preserve the array alias with independent storage")
	}
}
