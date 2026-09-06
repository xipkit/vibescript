package capabilitydata

import (
	"context"
	"errors"
	"math"
	"testing"

	"github.com/mgomes/vibescript/vibes/value"
)

func TestCloneKeepsDistinctEmptyArrays(t *testing.T) {
	t.Parallel()
	first, second := value.NewArray(nil), value.NewArray(nil)
	cloned, err := NewCloner(nil, Options{}).Clone("payload", value.NewArray([]value.Value{first, second, first}))
	if err != nil {
		t.Fatal(err)
	}
	items := cloned.Array()
	if value.ArrayIdentity(items[0]) == value.ArrayIdentity(items[1]) {
		t.Error("Clone merged distinct empty arrays")
	}
	if value.ArrayIdentity(items[0]) != value.ArrayIdentity(items[2]) {
		t.Error("Clone duplicated a shared empty array")
	}
}

func TestCloneBudgetLimits(t *testing.T) {
	t.Parallel()
	for _, resource := range []string{"node", "edge", "byte", "work"} {
		t.Run(resource, func(t *testing.T) {
			t.Parallel()
			budget := NewBudget(context.Background(), nil, nil)
			switch resource {
			case "node":
				budget.nodes = maxNodes
			case "edge":
				budget.edges = maxEdges
			case "byte":
				budget.bytes = maxBytes
			case "work":
				budget.work = maxWork
			}
			cloner := NewCloner(budget, Options{})
			_, err := cloner.Clone("payload", value.NewArray([]value.Value{value.NewInt(1)}))
			var limit *limitError
			if !errors.As(err, &limit) {
				t.Fatalf("Clone with exhausted %s budget error = %v, want limit error", resource, err)
			}
			if cloner.memo != nil {
				t.Error("Clone allocated its identity memo after exhausting the budget")
			}
		})
	}
}

func TestCloneRejectsBeforeAllocating(t *testing.T) {
	t.Parallel()
	refused := errors.New("memory quota exceeded")
	budget := NewBudget(context.Background(), nil, func(int) error { return refused })
	cloner := NewCloner(budget, Options{})
	_, err := cloner.Clone("payload", value.NewArray(make([]value.Value, 1024)))
	if !errors.Is(err, refused) {
		t.Fatalf("Clone error = %v, want reservation failure", err)
	}
	if cloner.memo != nil || budget.bytes != 0 {
		t.Error("Clone retained allocation state after its first reservation failed")
	}
	if err := budget.reserveSlots(math.MaxInt, valueBytes, 0); err == nil {
		t.Error("reserveSlots accepted an overflowing allocation size")
	}
}

func TestCloneChecksContainerEdgesBeforeAllocating(t *testing.T) {
	t.Parallel()
	reserved := 0
	budget := NewBudget(context.Background(), nil, func(bytes int) error {
		reserved += bytes
		return nil
	})
	budget.edges = maxEdges - 2
	_, err := NewCloner(budget, Options{}).Clone("payload", value.NewArray([]value.Value{value.NewInt(1), value.NewInt(2)}))
	var limit *limitError
	if !errors.As(err, &limit) {
		t.Fatalf("Clone with one child edge remaining error = %v, want limit error", err)
	}
	if reserved != 0 {
		t.Errorf("Clone reserved %d bytes for a container exceeding its edge budget, want 0", reserved)
	}
}

func TestCloneBudgetSurvivesIndependentSnapshots(t *testing.T) {
	t.Parallel()
	budget := NewBudget(context.Background(), nil, nil)
	budget.nodes = maxNodes - 2
	source := value.NewArray([]value.Value{value.NewInt(1)})
	first, err := NewCloner(budget, Options{}).Clone("first row", source)
	if err != nil {
		t.Fatal(err)
	}
	first.Array()[0] = value.NewInt(2)
	second, err := NewCloner(budget, Options{}).Clone("second row", source)
	if err != nil {
		t.Fatal(err)
	}
	if !second.Array()[0].Equal(value.NewInt(1)) {
		t.Error("fresh snapshot reused the earlier mutated clone")
	}
	if _, err := NewCloner(budget, Options{}).Clone("third row", source); err == nil {
		t.Error("fresh snapshot reset the cumulative node budget")
	}
}

func TestCloneCancellation(t *testing.T) {
	t.Parallel()
	ctx, cancel := context.WithCancel(context.Background())
	t.Cleanup(cancel)
	steps := 0
	budget := NewBudget(ctx, func(n int) error {
		steps += n
		if steps >= 32 {
			cancel()
		}
		return nil
	}, nil)
	graph := value.NewInt(1)
	for range 64 {
		graph = value.NewArray([]value.Value{graph, graph})
	}
	cloner := NewCloner(budget, Options{})
	if _, err := cloner.Clone("payload", graph); !errors.Is(err, context.Canceled) {
		t.Fatalf("Clone error = %v, want context cancellation", err)
	}
	if len(cloner.memo) >= 64 {
		t.Error("Clone finished traversing the graph after cancellation")
	}
}

func TestCloneOptions(t *testing.T) {
	t.Parallel()
	callable := value.NewValue(value.KindFunction, nil)
	cloned, err := NewCloner(nil, Options{AllowRuntimeValues: true}).Clone("validated option", callable)
	if err != nil || cloned != callable {
		t.Fatalf("Clone(validated callable) = %v, %v, want unchanged value", cloned.Kind(), err)
	}
	if _, err := NewCloner(nil, Options{}).Clone("option", callable); !errors.Is(err, errCallable) {
		t.Fatalf("Clone(callable) error = %v, want data-only error", err)
	}
	entries := map[string]value.Value{"message": value.NewString("changed")}
	tagged := value.NewTaggedObject(entries, value.ObjectTagRescuedError, "original")
	plain := value.NewObject(entries)
	cloned, err = NewCloner(nil, Options{PreserveObjectTags: true}).Clone("result", value.NewArray([]value.Value{tagged, plain}))
	if err != nil {
		t.Fatal(err)
	}
	items := cloned.Array()
	if text, ok := items[0].ObjectStringForm(); !ok || text != "original" {
		t.Errorf("Clone(tagged object) string form = %q, %t, want original", text, ok)
	}
	if items[1].ObjectTag() != value.ObjectTagNone {
		t.Error("Clone gave an ordinary object another wrapper's provenance")
	}
	stripped, err := NewCloner(nil, Options{}).Clone("payload", tagged)
	if err != nil || stripped.ObjectTag() != value.ObjectTagNone {
		t.Errorf("Clone(default object) tag = %v, error = %v, want no tag", stripped.ObjectTag(), err)
	}
}
