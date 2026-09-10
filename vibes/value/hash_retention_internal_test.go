package value

import (
	"fmt"
	"slices"
	"testing"
)

func TestHashDeleteClearsRetiredOrderSlots(t *testing.T) {
	t.Parallel()
	for _, names := range [][]string{{"c", "b", "a"}, {"a", "b", "c"}, {"b", "c", "a"}} {
		t.Run(fmt.Sprint(names), func(t *testing.T) {
			hash := NewHashWithCapacity(8)
			for i, name := range []string{"a", "b", "c"} {
				if err := hash.HashSet(NewString(name), NewInt(int64(i))); err != nil {
					t.Fatal(err)
				}
			}
			hd := hash.data.(*hashData)
			capacity := cap(hd.order)
			want := []string{"a", "b", "c"}
			for _, name := range names {
				_, found, err := hash.HashDeleteKey(NewSymbol(name))
				if err != nil || !found {
					t.Fatalf("HashDeleteKey(%q) found=%t, error=%v, want true, nil", name, found, err)
				}
				want = slices.Delete(want, slices.Index(want, name), slices.Index(want, name)+1)
				var got []string
				for _, entry := range hash.HashEntries() {
					got = append(got, entry.Key.String())
				}
				if !slices.Equal(got, want) {
					t.Errorf("order after deleting %q = %v, want %v", name, got, want)
				}
				if cap(hd.order) != capacity {
					t.Errorf("order capacity = %d, want reserved %d", cap(hd.order), capacity)
				}
				for i, slot := range hd.order[len(hd.order):cap(hd.order)] {
					if slot.data != nil {
						t.Errorf("retired order slot %d retains %v, want nil", len(hd.order)+i, slot)
					}
				}
			}
		})
	}
}

func TestHashReconciliationClearsRetiredOrderSlots(t *testing.T) {
	t.Parallel()
	hash := NewHashWithCapacity(8)
	for _, name := range []string{"first", "removed", "last"} {
		if err := hash.HashSet(NewString(name), NewInt(1)); err != nil {
			t.Fatal(err)
		}
	}
	entries := hash.Hash()
	delete(entries, "removed")
	delete(entries, "last")
	if err := hash.HashSet(NewString("added"), NewInt(2)); err != nil {
		t.Fatal(err)
	}
	hd := hash.data.(*hashData)
	if len(hd.order) != 2 || hd.order[0].String() != "first" || hd.order[1].String() != "added" {
		t.Fatalf("reconciled order = %v, want [first added]", hd.order)
	}
	for i, slot := range hd.order[len(hd.order):cap(hd.order)] {
		if slot.data != nil {
			t.Errorf("retired order slot %d retains %v, want nil", len(hd.order)+i, slot)
		}
	}
}
