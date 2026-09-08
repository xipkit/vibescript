//go:build perfaudit

package runtime

import (
	"context"
	"fmt"
	goruntime "runtime"
	"testing"
)

func BenchmarkAuditMemoryUnrelatedMutation(b *testing.B) {
	for _, count := range []int{1000, 10000} {
		for _, mutate := range []bool{false, true} {
			b.Run(fmt.Sprintf("rows=%d/unrelated_mutation=%t", count, mutate), func(b *testing.B) {
				exec, env := newEstimatorCacheExec()
				env.Define("rows", estimatorCacheRows(count))
				unrelated := newEnv(nil)
				first := NewString("first")
				second := NewString("second")
				unrelated.Define("state", first)
				want := exec.estimateMemoryUsage()
				walked := exec.memoryEst.walked
				b.ReportAllocs()
				b.ResetTimer()
				for i := range b.N {
					if mutate {
						if i%2 == 0 {
							unrelated.Define("state", second)
						} else {
							unrelated.Define("state", first)
						}
					}
					if got := exec.estimateMemoryUsage(); got != want {
						b.Fatalf("estimateMemoryUsage() = %d, want %d", got, want)
					}
				}
				b.ReportMetric(float64(exec.memoryEst.walked-walked)/float64(b.N), "nodes/op")
			})
		}
	}
}

func TestAuditMemoryPoppedEnvRetention(t *testing.T) {
	for _, tc := range []struct {
		name  string
		bytes int64
		depth int64
	}{
		{"one_call", 32 << 20, 1},
		{"descending_depths", 4 << 20, 16},
	} {
		t.Run(tc.name, func(t *testing.T) {
			auditMemoryPoppedEnvRetention(t, tc.bytes, tc.depth)
		})
	}
}

func auditMemoryPoppedEnvRetention(t *testing.T, bytes, depth int64) {
	t.Helper()
	engine := MustNewEngine(Config{StepQuota: Unlimited, MemoryQuotaBytes: 48 << 20})
	probes := 0
	engine.RegisterBuiltin("audit_heap", func(exec *Execution, _ Value, _ []Value, _ map[string]Value, _ Value) (Value, error) {
		probes++
		var before, after goruntime.MemStats
		goruntime.GC()
		goruntime.ReadMemStats(&before)
		stale := 0
		for _, env := range exec.envStack[len(exec.envStack):cap(exec.envStack)] {
			if env != nil {
				stale++
			}
		}
		clear(exec.envStack[len(exec.envStack):cap(exec.envStack)])
		goruntime.GC()
		goruntime.ReadMemStats(&after)
		t.Logf("probe=%d active_envs=%d stale_envs=%d heap_before=%d heap_after=%d freed=%d", probes, len(exec.envStack), stale, before.HeapAlloc, after.HeapAlloc, int64(before.HeapAlloc)-int64(after.HeapAlloc))
		return NewNil(), nil
	})
	script := compileScriptWithEngine(t, engine, `def scratch(n, depth)
  if depth > 0
    scratch(n, depth - 1)
  else
    payload = "x" * n
    1.times do
      payload.size
    end
  end
  nil
end

def run(n, depth)
  audit_heap()
  while depth > 0
    scratch(n, depth)
    depth = depth - 1
  end
  audit_heap()
  nil
end`)
	if _, err := script.Call(context.Background(), "run", []Value{NewInt(bytes), NewInt(depth)}, CallOptions{}); err != nil {
		t.Fatalf("run(%d, %d): %v", bytes, depth, err)
	}
	if probes != 2 {
		t.Fatalf("audit_heap calls = %d, want 2", probes)
	}
}
