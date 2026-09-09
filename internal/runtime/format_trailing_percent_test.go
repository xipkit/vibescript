package runtime

import (
	"context"
	"errors"
	"fmt"
	"strings"
	"testing"
)

func TestFormatTrailingPercentOutputCap(t *testing.T) {
	t.Parallel()

	for _, expression := range []string{"format(pattern)", "sprintf(pattern)", "pattern % []"} {
		t.Run(expression, func(t *testing.T) {
			script := compileScriptWithConfig(t, Config{StepQuota: Unlimited, MemoryQuotaBytes: 64 << 20},
				"def run(pattern)\n  "+expression+"\nend")
			for _, size := range []int{0, 7, 4096, maxFormatOutputBytes - 11, maxFormatOutputBytes - 10, maxFormatOutputBytes - 9, maxFormatOutputBytes - 2} {
				prefix := strings.Repeat("x", size)
				got, err := script.Call(context.Background(), "run", []Value{NewString(prefix + "%")}, CallOptions{})
				want := prefix + "%!(NOVERB)"
				if len(want) > maxFormatOutputBytes {
					if err == nil || !strings.Contains(err.Error(), "format output exceeds limit 1048576 bytes") {
						t.Errorf("%s with %d prefix bytes: output length %d, error %v; want output limit error", expression, size, len(got.String()), err)
					}
					continue
				}
				if err != nil || got.Kind() != KindString || got.String() != want {
					t.Errorf("%s with %d prefix bytes: output length %d, error %v; want %d bytes ending in %%!(NOVERB)", expression, size, len(got.String()), err, len(want))
				}
			}
		})
	}
}

func TestFormatTrailingPercentPreflight(t *testing.T) {
	t.Parallel()

	for _, size := range []int{0, 7, 63, 4096, maxFormatOutputBytes - 10} {
		t.Run(fmt.Sprint(size), func(t *testing.T) {
			pattern := strings.Repeat("x", size) + "%"
			want := pattern[:size] + "%!(NOVERB)"
			args := []Value{NewString(pattern)}
			var normalized strings.Builder
			normalized.Grow(len(pattern))
			probe := &Execution{memoryQuota: 64 << 20}
			quota := probe.estimateMemoryUsageForCallRoots(NewNil(), NewNil(), args, nil, NewNil()) +
				estimatedValueBytes + estimatedStringHeaderBytes + len(want) + normalized.Cap()
			for _, limit := range []int{quota - 1, quota, quota + 1} {
				exec := &Execution{ctx: context.Background(), quota: 1 << 30, memoryQuota: limit}
				got, err := formatStringValuesChecked(exec, pattern, nil, NewNil(), args, nil, NewNil())
				if limit < quota {
					if !errors.Is(err, errMemoryQuotaExceeded) {
						t.Errorf("format with %d prefix bytes at memory quota %d: error %v; want memory quota exceeded before rendering", size, limit, err)
					}
				} else if err != nil || got.String() != want {
					t.Errorf("format with %d prefix bytes at memory quota %d: output length %d, error %v; want %d bytes", size, limit, len(got.String()), err, len(want))
				}
			}
		})
	}
}

func TestFormatTrailingPercentPublicMemoryBoundary(t *testing.T) {
	t.Parallel()

	for _, expression := range []string{"format(pattern)", "sprintf(pattern)", "pattern % []"} {
		t.Run(expression, func(t *testing.T) {
			prefix := strings.Repeat("x", maxFormatOutputBytes-10)
			args := []Value{NewString(prefix + "%")}
			want := prefix + "%!(NOVERB)"
			call := func(quota int) (Value, error) {
				script := compileScriptWithConfig(t, Config{StepQuota: Unlimited, MemoryQuotaBytes: quota},
					"def run(pattern)\n  "+expression+"\nend")
				return script.Call(context.Background(), "run", args, CallOptions{})
			}
			lo, hi := 1, 8<<20
			if got, err := call(hi); err != nil || got.String() != want {
				t.Fatalf("%s at memory quota %d: output length %d, error %v; want %d bytes", expression, hi, len(got.String()), err, len(want))
			}
			for lo < hi {
				mid := lo + (hi-lo)/2
				_, err := call(mid)
				if err == nil {
					hi = mid
				} else if isRuntimeErrorType(err, runtimeErrorTypeLimit) && strings.HasPrefix(err.Error(), "memory quota exceeded (") {
					lo = mid + 1
				} else {
					t.Fatalf("%s at memory quota %d: error %v; want success or memory quota exceeded", expression, mid, err)
				}
			}
			for _, quota := range []int{lo - 1, lo, lo + 1} {
				got, err := call(quota)
				if quota < lo {
					if !isRuntimeErrorType(err, runtimeErrorTypeLimit) || !strings.HasPrefix(err.Error(), "memory quota exceeded (") {
						t.Errorf("%s at memory quota %d: error %v; want memory quota exceeded", expression, quota, err)
					}
				} else if err != nil || got.String() != want {
					t.Errorf("%s at memory quota %d: output length %d, error %v; want %d bytes", expression, quota, len(got.String()), err, len(want))
				}
			}
		})
	}
}
