package capabilitycontract

import (
	"fmt"
	"testing"

	"github.com/mgomes/vibescript/vibes/value"
)

func TestCloneDataOnlyValuePreservesSharedChildren(t *testing.T) {
	t.Parallel()
	for _, kind := range []value.ValueKind{value.KindArray, value.KindHash, value.KindObject} {
		t.Run(kind.String(), func(t *testing.T) {
			t.Parallel()
			child := value.NewArray([]value.Value{value.NewInt(7)})
			var source value.Value
			switch kind {
			case value.KindArray:
				source = value.NewArray([]value.Value{child, child})
			case value.KindHash:
				source = value.NewHash(map[string]value.Value{"a": child, "b": child})
			case value.KindObject:
				source = value.NewObject(map[string]value.Value{"a": child, "b": child})
			}
			cloned, err := CloneDataOnlyValue("payload", source)
			if err != nil {
				t.Fatalf("CloneDataOnlyValue(%s) error = %v", kind, err)
			}
			var first, second value.Value
			if kind == value.KindArray {
				first, second = cloned.Array()[0], cloned.Array()[1]
			} else {
				first, second = cloned.HashEntryMap()["a"], cloned.HashEntryMap()["b"]
			}
			if value.ArrayIdentity(first) != value.ArrayIdentity(second) {
				t.Error("CloneDataOnlyValue duplicated a shared child")
			}
			first.Array()[0] = value.NewInt(9)
			if !second.Array()[0].Equal(value.NewInt(9)) {
				t.Error("mutating one cloned alias did not update the other")
			}
			if !child.Array()[0].Equal(value.NewInt(7)) {
				t.Error("mutating the cloned child changed the source")
			}
		})
	}
}

func TestCloneKwargsDataOnlyPreservesSharedRoots(t *testing.T) {
	t.Parallel()
	child := value.NewArray([]value.Value{value.NewInt(7)})
	cloned, err := CloneKwargsDataOnly("db.find", map[string]value.Value{"a": child, "b": child})
	if err != nil {
		t.Fatalf("CloneKwargsDataOnly(shared roots) error = %v", err)
	}
	if value.ArrayIdentity(cloned["a"]) != value.ArrayIdentity(cloned["b"]) {
		t.Error("CloneKwargsDataOnly duplicated a root shared by two keywords")
	}
	if value.ArrayIdentity(cloned["a"]) == value.ArrayIdentity(child) {
		t.Error("CloneKwargsDataOnly retained a source alias")
	}
}

func BenchmarkCloneDataOnlySharedGraph(b *testing.B) {
	for _, depth := range []int{8, 12, 16} {
		b.Run(fmt.Sprintf("depth_%d", depth), func(b *testing.B) {
			graph := value.NewInt(7)
			for range depth {
				graph = value.NewArray([]value.Value{graph, graph})
			}
			b.ReportAllocs()
			b.ResetTimer()
			for range b.N {
				if _, err := CloneDataOnlyValue("payload", graph); err != nil {
					b.Fatal(err)
				}
			}
		})
	}
}
