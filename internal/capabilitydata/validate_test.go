package capabilitydata

import (
	"context"
	"errors"
	"strings"
	"testing"

	"github.com/mgomes/vibescript/vibes/value"
)

func TestValidatorErrorPrecedence(t *testing.T) {
	t.Parallel()
	cycle := value.NewArray(make([]value.Value, 1))
	cycle.Array()[0] = cycle
	callable := value.NewValue(value.KindFunction, nil)
	deep := value.NewInt(1)
	for range MaxDepth + 1 {
		deep = value.NewArray([]value.Value{deep})
	}
	for _, tc := range []struct {
		name  string
		items []value.Value
		want  string
	}{
		{"cycle_before_callable", []value.Value{cycle, callable}, "must be data-only"},
		{"callable_before_cycle", []value.Value{callable, cycle}, "must be data-only"},
		{"depth_after_callable", []value.Value{callable, deep}, "exceeds maximum depth"},
		{"depth_after_cycle", []value.Value{cycle, deep}, "exceeds maximum depth"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			err := NewValidator(nil).Validate("payload", value.NewArray(tc.items))
			if err == nil || !strings.Contains(err.Error(), tc.want) {
				t.Fatalf("Validate error = %v, want %s", err, tc.want)
			}
		})
	}
}

func TestValidatorChecksLongestAliasedPath(t *testing.T) {
	t.Parallel()
	leaf := value.NewArray([]value.Value{value.NewInt(1)})
	deep := leaf
	for range MaxDepth - 1 {
		deep = value.NewArray([]value.Value{deep})
	}
	validator := NewValidator(nil)
	if err := validator.Validate("first", leaf); err != nil {
		t.Fatal(err)
	}
	if err := validator.Validate("at limit", deep); err != nil {
		t.Fatal(err)
	}
	err := validator.Validate("over limit", value.NewArray([]value.Value{deep}))
	var limit *limitError
	if !errors.As(err, &limit) {
		t.Fatalf("Validate longer alias error = %v, want depth limit", err)
	}
}

func TestValidatorSharesMemoWithoutCloning(t *testing.T) {
	t.Parallel()
	graph := value.NewArray([]value.Value{value.NewInt(1)})
	for range 20 {
		graph = value.NewArray([]value.Value{graph, graph})
	}
	budget := NewBudget(context.Background(), nil, nil)
	validator := NewValidator(budget)
	if err := validator.Validate("first", graph); err != nil {
		t.Fatal(err)
	}
	nodes, bytes := budget.nodes, budget.bytes
	if err := validator.Kwargs("method", map[string]value.Value{"a": graph, "b": graph}); err != nil {
		t.Fatal(err)
	}
	if budget.nodes != nodes || budget.bytes != bytes {
		t.Fatal("validation allocated or walked a previously validated graph again")
	}
	if nodes != 21 || budget.edges > 64 {
		t.Fatalf("validation visited %d nodes and %d edges for a 21-node graph", nodes, budget.edges)
	}
}

func TestValidatorCancellationAndReservationFailure(t *testing.T) {
	t.Parallel()
	graph := value.NewInt(1)
	for range 32 {
		graph = value.NewArray([]value.Value{graph})
	}
	refused := errors.New("memory quota exceeded")
	budget := NewBudget(context.Background(), nil, func(int) error { return refused })
	if err := NewValidator(budget).Validate("payload", graph); !errors.Is(err, refused) {
		t.Fatalf("Validate error = %v, want memo reservation refusal", err)
	}
	ctx, cancel := context.WithCancel(context.Background())
	cancel()
	if err := NewValidator(NewBudget(ctx, nil, nil)).Validate("payload", graph); !errors.Is(err, context.Canceled) {
		t.Fatalf("Validate error = %v, want cancellation", err)
	}
}
