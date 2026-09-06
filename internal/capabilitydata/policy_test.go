package capabilitydata

import (
	"errors"
	"testing"

	"github.com/mgomes/vibescript/vibes/value"
)

func TestCloneSharesInsensitiveChildrenAcrossPolicies(t *testing.T) {
	t.Parallel()
	for _, preserveFirst := range []bool{false, true} {
		t.Run(map[bool]string{false: "strip_first", true: "preserve_first"}[preserveFirst], func(t *testing.T) {
			t.Parallel()
			shared := value.NewArray([]value.Value{value.NewInt(1)})
			tagged := value.NewTaggedObject(map[string]value.Value{"child": shared}, value.ObjectTagRescuedError, "original")
			source := value.NewArray([]value.Value{tagged, shared})
			cloner := NewCloner(nil, Options{})
			first, err := cloner.CloneWithOptions("first", source, Options{PreserveObjectTags: preserveFirst})
			if err != nil {
				t.Fatal(err)
			}
			second, err := cloner.CloneWithOptions("second", source, Options{PreserveObjectTags: !preserveFirst})
			if err != nil {
				t.Fatal(err)
			}
			preserved, stripped := first, second
			if !preserveFirst {
				preserved, stripped = second, first
			}
			if value.ArrayIdentity(preserved) == value.ArrayIdentity(stripped) {
				t.Error("Clone reused an ancestor that needs two containment views")
			}
			if preserved.Array()[0].ObjectTag() != value.ObjectTagRescuedError || stripped.Array()[0].ObjectTag() != value.ObjectTagNone {
				t.Error("Clone changed a containment view's provenance")
			}
			id := value.ArrayIdentity(first.Array()[1])
			for _, child := range []value.Value{second.Array()[1], first.Array()[0].HashEntryMap()["child"], second.Array()[0].HashEntryMap()["child"]} {
				if value.ArrayIdentity(child) != id {
					t.Error("Clone duplicated a tag-free child across containment views")
				}
			}
		})
	}
}

func TestStrictCloneRejectsPermissiveMemoEntry(t *testing.T) {
	t.Parallel()
	source := value.NewArray([]value.Value{value.NewValue(value.KindFunction, nil)})
	cloner := NewCloner(nil, Options{})
	if _, err := cloner.CloneWithOptions("validated", source, Options{AllowRuntimeValues: true}); err != nil {
		t.Fatal(err)
	}
	if _, err := cloner.Clone("strict", source); !errors.Is(err, errCallable) {
		t.Fatalf("Clone(strict) error = %v, want data-only rejection", err)
	}
}
