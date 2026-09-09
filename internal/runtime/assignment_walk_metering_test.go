package runtime

import (
	"context"
	"errors"
	"strings"
	"testing"

	"github.com/mgomes/vibescript/vibes/value"
)

func TestAssignmentWalksConsumeSteps(t *testing.T) {
	for _, tc := range []struct {
		name   string
		source string
	}{
		{"local", deepNestingSource},
		{"logical", strings.ReplaceAll(deepNestingSource, "cur = [cur]", "cur &&= [cur]")},
		{"destructure", strings.ReplaceAll(deepNestingSource, "cur = [cur]", "cur, other = [[cur], 0]")},
		{"concat", strings.ReplaceAll(deepNestingSource, "cur = [cur]", "cur = cur + [1]")},
		{"region local", `def run(depth)
  [0].each { |x|
    cur = [1]
    i = 0
    while i < depth
      cur = [cur]
      i = i + 1
    end
  }
end`},
		{"region outer", `def run(depth)
  cur = [1]
  [0].each { |x|
    i = 0
    while i < depth
      cur = [cur]
      i = i + 1
    end
  }
end`},
		{"nested tap", `def run(depth)
  [0].each { |x|
    nil.tap { |unused|
      cur = [1]
      i = 0
      while i < depth
        cur = [cur]
        i = i + 1
      end
    }
  }
  42
end`},
		{"generated setter", `class Box
  property cur
  def initialize
    @cur = [1]
  end
end
def run(depth)
  box = Box.new
  i = 0
  while i < depth
    box.cur = [box.cur]
    i = i + 1
  end
  42
end`},
		{"ivar parameter", `class Box
  property cur
  def initialize
    @cur = [1]
  end
  def replace(@cur)
  end
end
def run(depth)
  box = Box.new
  i = 0
  while i < depth
    box.replace([box.cur])
    i = i + 1
  end
  42
end`},
	} {
		t.Run(tc.name, func(t *testing.T) {
			script := compileScriptWithConfig(t, Config{StepQuota: 40_000, MemoryQuotaBytes: 8 << 20}, tc.source)
			if _, err := script.Call(context.Background(), "run", []Value{NewInt(16)}, CallOptions{}); err != nil {
				t.Fatalf("ordinary depth 16 failed: %v", err)
			}
			requireCallErrorContains(t, script, "run", []Value{NewInt(2000)}, CallOptions{}, "step quota exceeded")
			unlimitedMemory := compileScriptWithConfig(t, Config{StepQuota: 40_000, MemoryQuotaBytes: Unlimited}, tc.source)
			if _, err := unlimitedMemory.Call(context.Background(), "run", []Value{NewInt(2000)}, CallOptions{}); err != nil {
				t.Fatalf("unlimited-memory assignment acquired an estimator charge: %v", err)
			}
		})
	}
}

func TestAssignmentWalkChargesOnce(t *testing.T) {
	exec, env := newEstimatorCacheExec()
	rows := estimatorCacheRows(1000)
	env.Define("cur", rows)
	exec.estimateMemoryUsage()
	exec.assignBinding(env, "cur", NewArray([]Value{rows}))
	if err := exec.checkMemory(); err != nil {
		t.Fatal(err)
	}
	charged := exec.steps
	if charged == 0 {
		t.Fatal("a changed binding's graph walk charged no steps")
	}
	if err := exec.checkMemory(); err != nil {
		t.Fatal(err)
	}
	if exec.steps != charged || exec.assignmentWalkNodes != 0 || exec.baseWalkOpen {
		t.Fatalf("a repeated check rebilled work or left a session open: steps=%d, first=%d, pending=%d, open=%t", exec.steps, charged, exec.assignmentWalkNodes, exec.baseWalkOpen)
	}
}

func TestAssignmentWalkQuotaExhaustion(t *testing.T) {
	exec, env := newEstimatorCacheExec()
	rows := estimatorCacheRows(1000)
	env.Define("cur", rows)
	exec.estimateMemoryUsage()
	exec.quota = 1
	exec.assignBinding(env, "cur", NewArray([]Value{rows}))
	if err := exec.checkMemory(); !errors.Is(err, errStepQuotaExceeded) {
		t.Fatalf("assignment check returned %v, want step quota exceeded", err)
	}
	if exec.assignmentWalkNodes != 0 || exec.baseWalkOpen {
		t.Fatalf("failed charge left pending=%d, open=%t", exec.assignmentWalkNodes, exec.baseWalkOpen)
	}
	if err := exec.step(); !errors.Is(err, errStepQuotaExceeded) {
		t.Fatalf("next step returned %v, want latched quota exhaustion", err)
	}
}

func TestAssignmentWalkIgnoresUnrelatedInvalidation(t *testing.T) {
	for _, invalidate := range []struct {
		name string
		run  func()
	}{
		{"opaque", value.BumpMutationEpoch},
		{"journal overflow", func() {
			foreign := NewArray([]Value{NewNil()})
			for range 300 {
				foreign.BumpMutationEpoch()
			}
		}},
	} {
		t.Run(invalidate.name, func(t *testing.T) {
			exec, env := newEstimatorCacheExec()
			env.Define("rows", estimatorCacheRows(1000))
			env.Define("i", NewInt(0))
			exec.estimateMemoryUsage()
			invalidate.run()
			if err := exec.assign(&Identifier{Name: "i"}, NewInt(1), env); err != nil {
				t.Fatal(err)
			}
			if err := exec.checkMemory(); err != nil {
				t.Fatal(err)
			}
			if exec.steps != 0 {
				t.Fatalf("scalar assignment charged %d steps for unrelated invalidation, want 0", exec.steps)
			}
		})
	}
}

func TestRegionAssignmentWalkIgnoresUnrelatedPrefix(t *testing.T) {
	for _, mode := range []struct {
		name  string
		depth int
	}{
		{"driver", 0},
		{"nested builtin", 1},
	} {
		for _, write := range []string{"binding", "index"} {
			t.Run(mode.name+"/"+write, func(t *testing.T) {
				measure := func(invalidate bool) int {
					exec, env := newEstimatorCacheExec()
					env.Define("rows", estimatorCacheRows(1000))
					region := exec.beginBlockIterationRegion()
					defer region.end()
					exec.undeclaredBuiltinDepth += mode.depth
					local := newBlockAssignmentEnv(env)
					exec.pushEnv(local)
					defer exec.popEnv()
					arr := NewArray([]Value{NewNil()})
					local.Define("cur", arr)
					exec.estimateMemoryUsage()
					if invalidate {
						value.BumpMutationEpoch()
					}
					switch write {
					case "binding":
						exec.assignBinding(local, "cur", NewArray([]Value{arr}))
					case "index":
						if err := exec.assignToEvaluatedIndex(&IndexExpr{}, arr, []Value{NewInt(0)}, NewArray([]Value{NewNil()})); err != nil {
							t.Fatal(err)
						}
					}
					if err := exec.checkMemory(); err != nil {
						t.Fatal(err)
					}
					return exec.steps
				}
				quiet, noisy := measure(false), measure(true)
				if noisy != quiet {
					t.Fatalf("region %s assignment charged %d steps with unrelated traffic, want %d", write, noisy, quiet)
				}
			})
		}
	}
}

func TestRegionAssignmentWalkChecksRawPrefixWrites(t *testing.T) {
	exec, env := newEstimatorCacheExec()
	arr := NewArray([]Value{NewNil()})
	env.Define("arr", arr)
	region := exec.beginBlockIterationRegion()
	defer region.end()
	exec.undeclaredBuiltinDepth++
	local := newBlockAssignmentEnv(env)
	exec.pushEnv(local)
	defer exec.popEnv()
	local.Define("cur", NewNil())
	previous := exec.estimateMemoryUsage()
	for i := range 3 {
		arr.Array()[0] = NewString(strings.Repeat("x", (i+1)*4096))
		exec.assignBinding(local, "cur", NewArray([]Value{NewNil()}))
		fresh := freshUncachedEstimate(exec)
		if fresh <= previous {
			t.Fatalf("raw write did not grow the prefix: previous=%d, fresh=%d", previous, fresh)
		}
		if got := exec.estimateMemoryUsage(); got != fresh {
			t.Fatalf("nested builtin reused stale prefix bytes: got %d, want %d", got, fresh)
		}
		if exec.baseWalkCache.valid {
			t.Fatal("nested builtin committed a reusable byte estimate")
		}
		previous = fresh
	}
}
