package runtime

import (
	"context"
	"fmt"
	"testing"

	"github.com/mgomes/vibescript/vibes/value"
)

func TestRetainedCallbackWalkConsumesSteps(t *testing.T) {
	for _, tc := range []struct {
		name string
		body string
	}{
		{"assignment", "state[0] = state[0] + 1"},
		{"clear", "state.clear"},
		{"send clear", "state.send(:clear)"},
		{"hash read", "h.length"},
		{"nested tap", "nil.tap { |unused| state.clear }"},
		{"rescued argument error", "begin\n      state.clear(1)\n    rescue\n      0\n    end"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			source := "def run(keys)\n  state = [0]\n  h = {}\n  h.fetch_values(*keys) { |k|\n    " + tc.body + "\n    [k, k]\n  }.length\nend"
			keys := make([]Value, 2000)
			for i := range keys {
				keys[i] = NewSymbol(fmt.Sprintf("missing%d", i))
			}
			script := compileScriptWithConfig(t, Config{StepQuota: 40_000, MemoryQuotaBytes: 8 << 20}, source)
			if got, err := script.Call(context.Background(), "run", []Value{NewArray(keys[:16])}, CallOptions{}); err != nil || got.Int() != 16 {
				t.Fatalf("ordinary lookup returned %v, %v; want 16, nil", got, err)
			}
			unlimited := compileScriptWithConfig(t, Config{StepQuota: 40_000, MemoryQuotaBytes: Unlimited}, source)
			if got, err := unlimited.Call(context.Background(), "run", []Value{NewArray(keys)}, CallOptions{}); err != nil || got.Int() != int64(len(keys)) {
				t.Fatalf("lookup without memory accounting returned %v, %v; want %d, nil", got, err, len(keys))
			}
			requireCallErrorContains(t, script, "run", []Value{NewArray(keys)}, CallOptions{}, "step quota exceeded")
		})
	}
}

func TestRetainedCallbackWalkIgnoresForeignInvalidation(t *testing.T) {
	for _, tc := range []struct {
		name       string
		invalidate func()
	}{
		{"opaque", value.BumpMutationEpoch},
		{"journal overflow", func() {
			foreign := NewArray([]Value{NewNil()})
			for range 300 {
				foreign.BumpMutationEpoch()
			}
		}},
	} {
		t.Run(tc.name, func(t *testing.T) {
			exec, _ := newEstimatorCacheExec()
			out := []Value{estimatorCacheRows(1000)}
			exec.pushOutputWalkRoot(retainedValues(&out))
			defer func() { _ = exec.endOutputWalkRoot(nil) }()
			exec.undeclaredBuiltinDepth = 1
			region := exec.beginBlockIterationRegion()
			defer region.end()
			exec.estimateMemoryUsage()
			tc.invalidate()
			if err := exec.checkMemory(); err != nil {
				t.Fatal(err)
			}
			if exec.steps != 0 {
				t.Fatalf("unrelated invalidation charged %d steps", exec.steps)
			}
		})
	}
}

func TestRetainedCallbackWalkChargeDoesNotReenterItself(t *testing.T) {
	exec, _ := newEstimatorCacheExec()
	exec.quota = 10_000
	out := []Value{estimatorCacheRows(5000)}
	exec.pushOutputWalkRoot(retainedValues(&out))
	defer func() { _ = exec.endOutputWalkRoot(nil) }()
	exec.undeclaredBuiltinDepth = 1
	region := exec.beginBlockIterationRegion()
	defer region.end()
	exec.undeclaredBuiltinDepth++
	for range 2 {
		before := exec.steps
		if err := exec.checkMemory(); err != nil {
			t.Fatalf("one nested check exhausted the budget by rebilling itself: %v", err)
		}
		if exec.steps <= before || exec.steps-before >= exec.quota/2 {
			t.Fatalf("one check charged %d steps, want a positive bounded charge", exec.steps-before)
		}
		if exec.assignmentWalkNodes != 0 || exec.baseWalkOpen {
			t.Fatal("check left pending work or an open session")
		}
	}
}
