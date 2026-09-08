package runtime

import (
	"context"
	"fmt"
	goruntime "runtime"
	"testing"
)

// These tests sample the process heap while Script.Call is still running, so
// returned frames cannot disappear merely because the Execution was collected.
// They must not run in parallel with other heap measurements.
func TestReturnedFunctionsReleaseLocalPayloads(t *testing.T) {
	for _, region := range []bool{false, true} {
		for _, depth := range []int{1, 16} {
			t.Run(fmt.Sprintf("region=%t/depth=%d", region, depth), func(t *testing.T) {
				call := "scratch(bytes, depth)"
				if region {
					call = "1.times do\n      scratch(bytes, depth)\n    end"
				}
				source := fmt.Sprintf(`def warm()
  1
end

def scratch(bytes, depth)
  warm()
  if depth > 0
    scratch(bytes, depth - 1)
  else
    payload = "x" * bytes
    1.times do
      payload.size
    end
  end
  nil
end

def run(bytes, depth)
  marker = 19
  measure_heap()
  while depth > 0
    %s
    depth = depth - 1
  end
  measure_heap()
  marker
end
`, call)
				bytes := 16 << 20
				if depth > 1 {
					bytes = 2 << 20
				}
				retained := returnedFrameHeapBytes(t, source, []Value{NewInt(int64(bytes)), NewInt(int64(depth))})
				t.Logf("returned frames retain %d bytes", retained)
				if limit := int64(4 << 20); retained > limit {
					t.Errorf("run(bytes=%d, depth=%d) retains %d bytes, want at most %d", bytes, depth, retained, limit)
				}
			})
		}
	}
}

func TestReturnedFunctionReleasesPayloadAfterEnvStackGrowth(t *testing.T) {
	const source = `def descend(depth)
  if depth > 0
    descend(depth - 1)
  end
  nil
end

def scratch(bytes)
  payload = "x" * bytes
  descend(16)
  1.times do
    payload.size
  end
  nil
end

def run(bytes)
  marker = 19
  measure_heap()
  scratch(bytes)
  measure_heap()
  marker
end
`
	const bytes = 16 << 20
	retained := returnedFrameHeapBytes(t, source, []Value{NewInt(bytes)})
	t.Logf("returned frame after stack growth retains %d bytes", retained)
	if limit := int64(4 << 20); retained > limit {
		t.Errorf("run(bytes=%d) retains %d bytes after stack growth, want at most %d", bytes, retained, limit)
	}
}

func returnedFrameHeapBytes(t *testing.T, source string, args []Value) int64 {
	t.Helper()
	var samples []uint64
	engine := MustNewEngine(Config{StepQuota: Unlimited, MemoryQuotaBytes: 48 << 20})
	engine.RegisterBuiltin("measure_heap", func(_ *Execution, _ Value, _ []Value, _ map[string]Value, _ Value) (Value, error) {
		var stats goruntime.MemStats
		goruntime.GC()
		goruntime.ReadMemStats(&stats)
		samples = append(samples, stats.HeapAlloc)
		return NewNil(), nil
	})
	script, err := engine.Compile(source)
	if err != nil {
		t.Fatalf("Compile() failed: %v", err)
	}
	got, err := script.Call(context.Background(), "run", args, CallOptions{})
	if err != nil {
		t.Fatalf("Call(run, %v) failed: %v", args, err)
	}
	if got.Int() != 19 {
		t.Errorf("Call(run, %v) = %v, want 19", args, got)
	}
	if len(samples) != 2 {
		t.Fatalf("Call(run, %v) measured the heap %d times, want 2", args, len(samples))
	}
	return int64(samples[1]) - int64(samples[0])
}
