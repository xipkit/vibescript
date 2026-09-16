package main

import (
	"context"
	"encoding/json"
	"math"
	"os"
	"testing"

	"github.com/google/go-cmp/cmp"
	"github.com/mgomes/vibescript/vibes"
	"github.com/mgomes/vibescript/vibes/value"
)

func TestTypedResultFixtures(t *testing.T) {
	raw, err := os.ReadFile("../../tests/encoding-cases.json")
	if err != nil {
		t.Fatal(err)
	}
	var cases []struct {
		Name     string
		Body     string
		Expected any
	}
	if err := json.Unmarshal(raw, &cases); err != nil {
		t.Fatal(err)
	}
	for _, tc := range cases {
		t.Run(tc.Name, func(t *testing.T) {
			engine, err := vibes.NewEngine(vibes.Config{})
			if err != nil {
				t.Fatal(err)
			}
			script, err := engine.Compile("def run;" + tc.Body + ";end")
			if err != nil {
				t.Fatalf("Compile(%q) = %v, want success", tc.Body, err)
			}
			result, err := script.Call(context.Background(), "run", nil, vibes.CallOptions{})
			if err != nil {
				t.Fatalf("Call(%q) = %v, want success", tc.Body, err)
			}
			encoded, err := encodeResult(context.Background(), nil, result, "typed")
			if err != nil {
				t.Fatalf("encodeResult(%q, typed) = %v, want success", tc.Body, err)
			}
			var got any
			if err := json.Unmarshal([]byte(encoded), &got); err != nil {
				t.Fatalf("Decode(%q) = %v, want valid JSON", encoded, err)
			}
			if diff := cmp.Diff(tc.Expected, got); diff != "" {
				t.Errorf("encodeResult(%q, typed) mismatch (-want +got):\n%s", tc.Body, diff)
			}
		})
	}
}

func TestTypedResultPreservesNaNPayloads(t *testing.T) {
	input := value.NewFloat(math.Float64frombits(0x7ff8000000000001))
	got, err := typedResult(input, 0)
	if err != nil {
		t.Fatalf("typedResult(NaN payload 1) = %v, want success", err)
	}
	want := []any{"float", "7ff8000000000001"}
	if diff := cmp.Diff(want, got); diff != "" {
		t.Errorf("typedResult(NaN payload 1) mismatch (-want +got):\n%s", diff)
	}
}

func TestTypedResultRejectsExecutablesAndCycles(t *testing.T) {
	method := vibes.NewBuiltin("hidden", func(_ *vibes.Execution, _ value.Value, _ []value.Value, _ map[string]value.Value, _ value.Value) (value.Value, error) {
		panic("encoder invoked a method")
	})
	if got, err := typedResult(method, 0); err == nil {
		t.Errorf("typedResult(builtin) = %v, want rejection", got)
	}
	cycle := value.NewHash(map[string]value.Value{})
	if err := cycle.HashSet(value.NewString("self"), cycle); err != nil {
		t.Fatal(err)
	}
	if got, err := typedResult(cycle, 0); err == nil {
		t.Errorf("typedResult(cycle) = %v, want bounded rejection", got)
	}
	if got, err := encodeResult(context.Background(), nil, value.NewNil(), "unknown"); err == nil {
		t.Errorf("encodeResult(nil, unknown) = %q, want rejection", got)
	}
}
