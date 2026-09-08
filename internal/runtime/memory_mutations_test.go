package runtime

import (
	"context"
	"errors"
	"fmt"
	"strings"
	"sync"
	"testing"

	"github.com/mgomes/vibescript/vibes/value"
)

func TestMemoryMemoUnrelatedEnvironmentWrites(t *testing.T) {
	for _, rows := range []int{1000, 10000} {
		t.Run(fmt.Sprintf("rows=%d", rows), func(t *testing.T) {
			exec, env := newEstimatorCacheExec()
			env.Define("rows", estimatorCacheRows(rows))
			unrelated := newEnv(nil)
			want := exec.estimateMemoryUsage()
			walked := exec.memoryEst.walked
			for i := range 20 {
				// More writes than the wrapper journal can hold still have no
				// bearing on this graph: lexical versions are not journaled.
				for j := range 1000 {
					unrelated.Define("state", NewString(fmt.Sprintf("%d/%d", i, j)))
				}
				if got := exec.estimateMemoryUsage(); got != want {
					t.Fatalf("estimate after unrelated writes = %d, want %d", got, want)
				}
			}
			if got := exec.memoryEst.walked - walked; got != 0 {
				t.Fatalf("unrelated environment writes walked %d nodes, want 0", got)
			}
		})
	}
}

func TestMemoryMemoUnrelatedWrapperWrites(t *testing.T) {
	for _, rows := range []int{1000, 10000} {
		t.Run(fmt.Sprintf("rows=%d", rows), func(t *testing.T) {
			exec, env := newEstimatorCacheExec()
			env.Define("rows", estimatorCacheRows(rows))
			array := NewArray([]Value{NewInt(1)})
			hash := NewHash(map[string]Value{"state": NewInt(1)})
			object := NewObject(map[string]Value{"state": NewInt(1)})
			want := exec.estimateMemoryUsage()
			walked := exec.memoryEst.walked
			for range 100 {
				array.SetArrayElems([]Value{NewString("changed")})
				if err := hash.HashSet(NewString("state"), NewString("changed")); err != nil {
					t.Fatal(err)
				}
				if err := object.HashSet(NewString("state"), NewString("changed")); err != nil {
					t.Fatal(err)
				}
				if got := exec.estimateMemoryUsage(); got != want {
					t.Fatalf("estimate after unrelated wrapper writes = %d, want %d", got, want)
				}
			}
			if got := exec.memoryEst.walked - walked; got != 0 {
				t.Fatalf("unrelated wrapper writes walked %d nodes, want 0", got)
			}
		})
	}
}

func TestMemoryMemoSharedHostMap(t *testing.T) {
	for _, wrap := range []struct {
		name string
		fn   func(map[string]Value) Value
	}{
		{"hash", NewHash},
		{"object", NewObject},
	} {
		t.Run(wrap.name, func(t *testing.T) {
			shared := map[string]Value{"payload": NewString("small")}
			first, firstEnv := newEstimatorCacheExec()
			second, secondEnv := newEstimatorCacheExec()
			firstEnv.Define("host", wrap.fn(shared))
			other := wrap.fn(shared)
			secondEnv.Define("host", other)
			before := first.estimateMemoryUsage()
			second.estimateMemoryUsage()
			if err := other.HashSet(NewString("payload"), NewString(strings.Repeat("x", 8192))); err != nil {
				t.Fatal(err)
			}
			want := freshUncachedEstimate(first)
			if want <= before {
				t.Fatal("shared host write did not grow the first execution's graph")
			}
			if got := first.estimateMemoryUsage(); got != want {
				t.Fatalf("estimate after shared-map write = %d, want %d", got, want)
			}
			first.memoryQuota = before + 4096
			if err := first.checkMemory(); !errors.Is(err, errMemoryQuotaExceeded) {
				t.Fatalf("quota after shared-map growth: got %v, want memory quota exceeded", err)
			}
		})
	}
}

func TestMemoryMemoSharedHostArray(t *testing.T) {
	shared := NewArray([]Value{NewString("small")})
	first, firstEnv := newEstimatorCacheExec()
	second, secondEnv := newEstimatorCacheExec()
	firstEnv.Define("host", shared)
	secondEnv.Define("host", shared)
	before := first.estimateMemoryUsage()
	second.estimateMemoryUsage()
	shared.SetArrayElems([]Value{NewString(strings.Repeat("x", 8192))})
	want := freshUncachedEstimate(first)
	if got := first.estimateMemoryUsage(); got != want || got <= before {
		t.Fatalf("estimate after shared-array write = %d, want %d and > %d", got, want, before)
	}
}

func TestMemoryMemoJournalOverflow(t *testing.T) {
	exec, env := newEstimatorCacheExec()
	env.Define("rows", estimatorCacheRows(1000))
	unrelated := NewHash(map[string]Value{"state": NewInt(1)})
	want := exec.estimateMemoryUsage()
	walked := exec.memoryEst.walked
	for i := range 1000 {
		if err := unrelated.HashSet(NewString("state"), NewInt(int64(i))); err != nil {
			t.Fatal(err)
		}
	}
	if got := exec.estimateMemoryUsage(); got != want {
		t.Fatalf("estimate after journal overflow = %d, want %d", got, want)
	}
	if exec.memoryEst.walked == walked {
		t.Fatal("journal overflow reused a memo without complete mutation history")
	}
	walked = exec.memoryEst.walked
	exec.estimateMemoryUsage()
	if exec.memoryEst.walked != walked {
		t.Fatal("a fresh memo was not reusable after the overflow re-walk")
	}
}

func TestMemoryMemoConcurrentIndependentCalls(t *testing.T) {
	engine := MustNewEngine(Config{StepQuota: Unlimited, MemoryQuotaBytes: 8 << 20})
	script := compileScriptWithEngine(t, engine, `def run(n)
  state = "first"
  values = [1]
  while n > 0
    state = "second"
    values[0] = n
    n = n - 1
  end
  state
end`)
	exec, env := newEstimatorCacheExec()
	env.Define("rows", estimatorCacheRows(1000))
	// Initialize engine caches before measuring unrelated call activity.
	if _, err := script.Call(context.Background(), "run", []Value{NewInt(1)}, CallOptions{}); err != nil {
		t.Fatal(err)
	}
	want := exec.estimateMemoryUsage()
	walked := exec.memoryEst.walked
	var wg sync.WaitGroup
	for range 2 {
		wg.Go(func() {
			for range 20 {
				if _, err := script.Call(context.Background(), "run", []Value{NewInt(1)}, CallOptions{}); err != nil {
					t.Error(err)
					return
				}
			}
		})
	}
	wg.Wait()
	if got := exec.estimateMemoryUsage(); got != want {
		t.Fatalf("estimate after independent calls = %d, want %d", got, want)
	}
	if got := exec.memoryEst.walked - walked; got != 0 {
		t.Fatalf("independent calls walked %d nodes in idle execution, want 0", got)
	}
}

func TestMemoryMemoOpaqueHostMutation(t *testing.T) {
	exec, env := newEstimatorCacheExec()
	host := NewHash(map[string]Value{"payload": NewString("small")})
	env.Define("host", host)
	before := exec.estimateMemoryUsage()
	// Opaque dispatch invalidates before Go code runs; while it runs, the
	// existing undeclaredBuiltinDepth bypass covers every direct raw write.
	value.BumpMutationEpoch()
	host.Hash()["payload"] = NewString(strings.Repeat("x", 8192))
	want := freshUncachedEstimate(exec)
	if got := exec.estimateMemoryUsage(); got != want || got <= before {
		t.Fatalf("estimate after opaque host write = %d, want %d and > %d", got, want, before)
	}
}

func TestMemoryMemoCapturedEnvironmentVersions(t *testing.T) {
	exec, env := newEstimatorCacheExec()
	captured := newEnv(nil)
	for range inlineSeenEnvs + 2 {
		captured = newEnv(captured)
	}
	captured.Define("payload", NewString("small"))
	env.Define("closure", newBlock(nil, nil, nil, captured))
	before := exec.estimateMemoryUsage()
	captured.Define("payload", NewString(strings.Repeat("x", 8192)))
	want := freshUncachedEstimate(exec)
	if got := exec.estimateMemoryUsage(); got != want || got <= before {
		t.Fatalf("estimate after captured environment write = %d, want %d and > %d", got, want, before)
	}
}
