package runtime

import (
	"context"
	"errors"
	"testing"

	"github.com/mgomes/vibescript/vibes/value"
)

func TestReplacementRenderDepthChargesPartialOutput(t *testing.T) {
	t.Parallel()
	result := NewInt(1)
	for range 16385 {
		result = NewArray([]Value{result})
	}
	exec := &Execution{ctx: context.Background(), quota: 64}
	if _, err := boundedReplacementString(exec, result); !errors.Is(err, errStepQuotaExceeded) {
		t.Fatalf("deep replacement with 64 steps: %v, want step quota error", err)
	}
	exec = &Execution{ctx: context.Background(), quota: 1024}
	_, err := boundedReplacementString(exec, result)
	if !errors.Is(err, value.ErrStringRenderDepthExceeded) {
		t.Fatalf("deep replacement with 1024 steps: %v, want depth error", err)
	}
	var limitErr *guardLimitError
	if !errors.As(err, &limitErr) {
		t.Fatalf("deep replacement: %v, want LimitError classification", err)
	}
	if want := 16384 / stringScanBytesPerStep; exec.steps != want {
		t.Fatalf("deep replacement charged %d steps, want %d for its partial output", exec.steps, want)
	}
}

func TestRescuedReplacementRenderDepthExhaustsQuota(t *testing.T) {
	const source = `def run
  a = 1
  16385.times { a = [a] }
  count = 0
  100.times do
    begin
      "x".sub("x") { a }
    rescue
      count += 1
    rescue LimitError
      count += 1
    end
  end
  count
end`
	script := compileScriptWithConfig(t, Config{StepQuota: 70000, MemoryQuotaBytes: Unlimited}, source)
	requireCallErrorContains(t, script, "run", nil, CallOptions{}, "quota")
}
