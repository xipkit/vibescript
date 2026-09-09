package runtime

import (
	"context"
	"fmt"
	"math"
	"strings"
	"testing"

	"github.com/google/go-cmp/cmp"
)

func jsonStringifyMemoCall(t *testing.T, input Value, budget jsonSpanBudget, nested, reference bool) (jsonSpanResult, uint64) {
	t.Helper()
	engine := MustNewEngine(Config{StepQuota: budget.quota, MemoryQuotaBytes: budget.memory})
	builtin := valueBuiltin(engine.builtins["JSON"].HashEntryMap()["stringify"])
	if reference {
		builtin.nonMutating = false
	}
	var execution *Execution
	fn := builtin.Fn
	builtin.Fn = func(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
		execution = exec
		return fn(exec, receiver, args, kwargs, block)
	}
	source := "def run(input) JSON.stringify(input) end"
	if nested {
		engine.RegisterBuiltin("outer", func(exec *Execution, _ Value, _ []Value, _ map[string]Value, block Value) (Value, error) {
			return exec.CallBlock(block, nil)
		})
		source = "def run(input) outer do JSON.stringify(input) end end"
	}
	script := compileScriptWithEngine(t, engine, source)
	ctx := &jsonSpanContext{Context: context.Background(), done: make(chan struct{}), cancelAfter: budget.cancelAfter}
	before := estimatorVisits.Load()
	out, err := script.Call(ctx, "run", []Value{input}, CallOptions{})
	visits := estimatorVisits.Load() - before
	result := jsonSpanResult{Error: jsonSpanErrorResult(err), ContextPolls: ctx.polls}
	if out.Kind() == KindString {
		result.Output = out.String()
	}
	if execution != nil {
		result.Exhausted = jsonSpanErrorResult(execution.exhausted)
		result.Steps = execution.steps
		result.Scratch = execution.reservedScratchBytes
		result.Sections = execution.accumMeteredSections
	}
	return result, visits
}

func TestJSONStringifyMemoAvoidsRepeatedGraphWalks(t *testing.T) {
	// The counter is global, so this test must not run in parallel.
	wasCounting := estimatorVisitCounting.Swap(true)
	defer estimatorVisitCounting.Store(wasCounting)
	budget := jsonSpanBudget{quota: 1 << 20, memory: 16 << 20}
	for _, nested := range []bool{false, true} {
		visits := make([]uint64, 2)
		for i, escapes := range []int{128, 1024} {
			input := NewHash(map[string]Value{
				"context": loopMemoArray(64),
				"text":    NewString(strings.Repeat("\n", escapes)),
			})
			result, count := jsonStringifyMemoCall(t, input, budget, nested, false)
			if result.Error.Type != "" {
				t.Fatalf("nested=%t, escapes=%d: %+v", nested, escapes, result.Error)
			}
			visits[i] = count
		}
		if nested {
			if visits[1] < visits[0]*4 {
				t.Errorf("undeclared outer builtin reused graph walks: %v visits for 128/1024 escapes", visits)
			}
		} else if visits[1] > visits[0]*2 {
			t.Errorf("ordinary stringify repeatedly walked unchanged roots: %v visits for 128/1024 escapes", visits)
		}
	}
}

func TestJSONStringifyMemoPreservesAccounting(t *testing.T) {
	shared := NewArray([]Value{NewInt(7), NewNil(), NewBool(true)})
	inputs := []Value{
		NewString(strings.Repeat("a", 4096)),
		NewString(strings.Repeat("a\n\t\"\\", 256)),
		NewString(strings.Repeat("é終😀\u2028\u2029\xff\x00", 32)),
		NewHash(map[string]Value{"left": shared, "right": shared, "text": NewString(strings.Repeat("\n", 128))}),
	}
	for i, input := range inputs {
		for _, nested := range []bool{false, true} {
			t.Run(fmt.Sprintf("input=%d/nested=%t", i, nested), func(t *testing.T) {
				budget := jsonSpanBudget{quota: 1 << 20, memory: 16 << 20}
				check := func(budget jsonSpanBudget) jsonSpanResult {
					t.Helper()
					want, _ := jsonStringifyMemoCall(t, input, budget, nested, true)
					got, _ := jsonStringifyMemoCall(t, input, budget, nested, false)
					if diff := cmp.Diff(want, got); diff != "" {
						t.Fatalf("budget=%+v mismatch (-want +got):\n%s", budget, diff)
					}
					return want
				}
				full := check(budget)
				if full.Error.Type != "" {
					t.Fatalf("unrestricted stringify failed: %+v", full.Error)
				}
				for _, quota := range []int{1, 15, 16, 17, full.Steps - 1, full.Steps, full.Steps + 1} {
					limited := budget
					limited.quota = quota
					check(limited)
				}
				lo, hi := 1, budget.memory
				for lo < hi {
					mid := lo + (hi-lo)/2
					limited := budget
					limited.memory = mid
					result, _ := jsonStringifyMemoCall(t, input, limited, nested, true)
					if result.Error.Type == "" {
						hi = mid
					} else {
						lo = mid + 1
					}
				}
				for _, memory := range []int{1, lo - 1, lo, lo + 1} {
					limited := budget
					limited.memory = memory
					result := check(limited)
					if (result.Error.Type == "") != (memory >= lo) {
						t.Fatalf("memory=%d did not straddle minimum %d: %+v", memory, lo, result.Error)
					}
				}
				for _, cancelAfter := range []int{1, 2, 3, full.ContextPolls / 2, full.ContextPolls, full.ContextPolls + 1} {
					limited := budget
					limited.cancelAfter = cancelAfter
					check(limited)
				}
				check(jsonSpanBudget{quota: 1, memory: 1, cancelAfter: 1})
			})
		}
	}
}

func TestJSONStringifyMemoPreservesFailurePrecedence(t *testing.T) {
	cycle := make([]Value, 1)
	cyclic := NewArray(cycle)
	cycle[0] = cyclic
	for _, input := range []Value{
		NewArray([]Value{NewString(strings.Repeat("\n", 128)), NewFloat(math.Inf(1))}),
		cyclic,
	} {
		for _, nested := range []bool{false, true} {
			for _, budget := range []jsonSpanBudget{
				{quota: 1 << 20, memory: 16 << 20},
				{quota: 16, memory: 16 << 20},
				{quota: 1 << 20, memory: 1},
				{quota: 1 << 20, memory: 16 << 20, cancelAfter: 5},
			} {
				want, _ := jsonStringifyMemoCall(t, input, budget, nested, true)
				got, _ := jsonStringifyMemoCall(t, input, budget, nested, false)
				if want.Error.Type == "" {
					t.Fatal("invalid JSON value unexpectedly succeeded")
				}
				if diff := cmp.Diff(want, got); diff != "" {
					t.Errorf("nested=%t, budget=%+v mismatch (-want +got):\n%s", nested, budget, diff)
				}
			}
		}
	}
}
