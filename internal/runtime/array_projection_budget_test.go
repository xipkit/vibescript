package runtime

import (
	"context"
	"errors"
	"fmt"
	"math"
	"runtime"
	"testing"
)

func TestArrayProjectionNativeBudgets(t *testing.T) {
	const size = 4096
	flat := largeIntArray(size)
	for _, method := range []string{"first", "last", "take", "drop", "transpose"} {
		t.Run(method, func(t *testing.T) {
			receiver := flat
			args := []Value{NewInt(size)}
			resultBytes := arraySlotBackingBytes(size)
			if method == "drop" {
				args[0] = NewInt(0)
			}
			if method == "transpose" {
				receiver = NewArray([]Value{flat, flat})
				args = nil
				resultBytes += arrayTupleRowBackingBytes(size, 2)
			}
			probe := &Execution{memoryQuota: 1 << 30}
			base := probe.estimateMemoryUsageForCallRoots(NewNil(), receiver, args, nil, NewNil())
			for _, delta := range []int{-1, 0} {
				exec := &Execution{quota: 1 << 30, memoryQuota: base + resultBytes + delta}
				got, err := callArrayMember(t, exec, receiver, method, args, NewNil())
				if delta < 0 {
					requireErrorIs(t, err, errMemoryQuotaExceeded)
					continue
				}
				if err != nil || len(got.Array()) != size {
					t.Fatalf("exact memory threshold: size %d, error %v", len(got.Array()), err)
				}
				if method == "transpose" {
					compareArrays(t, got.Array()[size-1], []Value{NewInt(size - 1), NewInt(size - 1)})
				} else {
					compareArrays(t, got, flat.Array())
				}
			}
			exec := &Execution{quota: 64}
			_, err := callArrayMember(t, exec, receiver, method, args, NewNil())
			requireErrorIs(t, err, errStepQuotaExceeded)
			ctx, cancel := context.WithCancel(context.Background())
			cancel()
			exec = &Execution{ctx: ctx, quota: 1 << 30, steps: 5}
			_, err = callArrayMember(t, exec, receiver, method, args, NewNil())
			requireErrorIs(t, err, context.Canceled)
		})
	}
}

func TestArrayTransposeMetersEmptyRows(t *testing.T) {
	rows := make([]Value, 4096)
	for i := range rows {
		rows[i] = NewArray(nil)
	}
	_, err := callArrayMember(t, &Execution{quota: 64}, NewArray(rows), "transpose", nil, NewNil())
	requireErrorIs(t, err, errStepQuotaExceeded)
}

func TestArrayProjectionChargesOnlyCopiedWindow(t *testing.T) {
	receiver := largeIntArray(4096)
	for _, method := range []string{"first", "last", "take", "drop"} {
		for _, size := range []int{0, 1} {
			count := size
			if method == "drop" {
				count = 4096 - size
			}
			exec := &Execution{quota: 1}
			got, err := callArrayMember(t, exec, receiver, method, []Value{NewInt(int64(count))}, NewNil())
			if err != nil || len(got.Array()) != size || exec.steps != size {
				t.Errorf("%s(%d): size %d, steps %d, error %v", method, count, len(got.Array()), exec.steps, err)
			}
		}
	}
}

func TestArrayProjectionIgnoredKeywordRoots(t *testing.T) {
	receiver := NewArray([]Value{NewInt(1)})
	kwargs := map[string]Value{"ignored": largeIntArray(4096)}
	for _, method := range []string{"take", "drop"} {
		member, err := arrayMember(receiver, method)
		if err != nil {
			t.Fatal(err)
		}
		args := []Value{NewInt(1)}
		probe := &Execution{memoryQuota: 1 << 30}
		base := probe.estimateMemoryUsageForCallRoots(NewNil(), receiver, args, kwargs, NewNil())
		_, err = valueBuiltin(member).Fn(&Execution{memoryQuota: base, quota: 1 << 30}, receiver, args, kwargs, NewNil())
		requireErrorIs(t, err, errMemoryQuotaExceeded)
		if _, err := valueBuiltin(member).Fn(&Execution{memoryQuota: base + 4096, quota: 1 << 30}, receiver, args, kwargs, NewNil()); err != nil {
			t.Fatalf("%s with ignored keyword: %v", method, err)
		}
	}
}

func TestArrayProjectionDispatchWork(t *testing.T) {
	receiver := largeIntArray(4096)
	for _, expression := range []string{"a.first(4096)", "a.last(4096)", "a.take(4096)", "a.drop(0)", "[a, a].transpose", "a.first(*[4096])", "a.last(*[4096])", "a.take(*[4096])", "a.drop(*[0])", "[a, a].transpose(*[])"} {
		t.Run(expression, func(t *testing.T) {
			source := "def control(a); a.size; end\ndef run(a); " + expression + ".size; end"
			script := compileScriptWithConfig(t, Config{StepQuota: 512, MemoryQuotaBytes: Unlimited}, source)
			if _, err := script.Call(context.Background(), "control", []Value{receiver}, CallOptions{}); err != nil {
				t.Fatalf("input-only control: %v", err)
			}
			requireCallErrorContains(t, script, "run", []Value{receiver}, CallOptions{}, "step quota exceeded")
		})
	}
}

func TestArrayProjectionKeepsCallRootsAndSnapshots(t *testing.T) {
	for _, method := range []string{"first", "last", "take", "drop", "transpose"} {
		t.Run(method, func(t *testing.T) {
			nested := NewArray([]Value{NewString("original")})
			receiver := NewArray([]Value{nested})
			args := []Value{NewInt(1)}
			if method == "drop" {
				args[0] = NewInt(0)
			}
			if method == "transpose" {
				receiver = NewArray([]Value{receiver})
				args = nil
			}
			env := newEnv(nil)
			env.Define("payload", largeIntArray(4096))
			block := NewBlock(nil, nil, env)
			probe := &Execution{memoryQuota: 1 << 30}
			base := probe.estimateMemoryUsageForCallRoots(NewNil(), receiver, args, nil, block)
			_, err := callArrayMember(t, &Execution{memoryQuota: base, quota: 1 << 30}, receiver, method, args, block)
			requireErrorIs(t, err, errMemoryQuotaExceeded)
			count := 1
			if method == "drop" {
				count = 0
			}
			source := fmt.Sprintf("def run\n leaf = [1]\n input = [leaf]\n output = input.%s(%d) { raise \"ignored\" }\n output[0][0] = 2\n [input, output]\nend", method, count)
			if method == "transpose" {
				source = "def run\n input = [[[1]]]\n output = input.transpose { raise \"ignored\" }\n output[0][0][0] = 2\n [input, output]\nend"
			}
			got := callFunc(t, compileScript(t, source), "run", nil)
			if got.Array()[0].Array()[0].Kind() != KindArray {
				t.Fatalf("source shape changed: %v", got)
			}
			if method == "transpose" {
				compareArrays(t, got.Array()[0].Array()[0].Array()[0], []Value{NewInt(1)})
			} else {
				compareArrays(t, got.Array()[0].Array()[0], []Value{NewInt(1)})
			}
		})
	}
}

func TestArrayTransposeRejectsBeforeAllocatingColumns(t *testing.T) {
	row := largeIntArray(16384)
	rows := make([]Value, 16)
	for i := range rows {
		rows[i] = row
	}
	receiver := NewArray(rows)
	probe := &Execution{memoryQuota: 1 << 30}
	base := probe.estimateMemoryUsageForCallRoots(NewNil(), receiver, nil, nil, NewNil())
	exec := &Execution{memoryQuota: base + 4096, quota: 1 << 30}
	var before, after runtime.MemStats
	runtime.ReadMemStats(&before)
	_, err := callArrayMember(t, exec, receiver, "transpose", nil, NewNil())
	runtime.ReadMemStats(&after)
	if !errors.Is(err, errMemoryQuotaExceeded) {
		t.Errorf("transpose error = %v, want memory quota", err)
	}
	if allocated := after.TotalAlloc - before.TotalAlloc; allocated > 256<<10 {
		t.Errorf("rejected transpose allocated %d bytes, want at most 256 KiB", allocated)
	} else {
		t.Logf("rejected transpose allocated %d bytes", allocated)
	}
}

func TestArrayProjectionRejectsSaturatedMemoryBudget(t *testing.T) {
	receiver := NewArray([]Value{NewInt(1)})
	columns := math.MaxInt / estimatedValueBytes
	exec := &Execution{memoryQuota: math.MaxInt}
	err := checkArrayProjection(exec, receiver, nil, nil, NewNil(), columns, arrayTupleRowBackingBytes(columns, 2))
	requireErrorIs(t, err, errMemoryQuotaExceeded)
	receiver = largeIntArray(1024)
	exec = &Execution{memoryQuota: math.MaxInt, reservedScratchBytes: math.MaxInt - 4096}
	err = checkArrayProjection(exec, receiver, nil, nil, NewNil(), 1, 0)
	requireErrorIs(t, err, errMemoryQuotaExceeded)
}
