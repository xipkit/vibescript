package runtime

import (
	"context"
	"errors"
	"fmt"
	"strings"
	"testing"
	"time"
)

func TestFormatLiteralSpans(t *testing.T) {
	t.Parallel()
	for _, size := range []int{0, 1, 7, 8, 9, 15, 16, 17, 4095, 4096, 4097, 65536} {
		prefix := strings.Repeat("x", size)
		for _, tc := range []struct {
			pattern string
			values  []Value
			want    string
		}{
			{pattern: prefix, want: prefix},
			{pattern: prefix + "é\xff%%", want: prefix + "é\xff%"},
			{pattern: prefix + "%", want: prefix + "%!(NOVERB)"},
			{pattern: prefix + "%2$s:%1$04d", values: []Value{NewInt(7), NewString("é")}, want: prefix + "é:0007"},
			{pattern: prefix + "%[2]s:%[1]d", values: []Value{NewInt(7), NewString("é")}, want: prefix + "é:7"},
			{pattern: prefix + "%5.1s", values: []Value{NewString("éΣ")}, want: prefix + "    é"},
			{pattern: prefix + "%Q", values: []Value{NewInt(7)}, want: prefix + "%!Q(int64=7)"},
		} {
			got, err := formatStringValues(tc.pattern, tc.values)
			if err != nil || got.Kind() != KindString || got.String() != tc.want {
				t.Errorf("formatStringValues with %d literal bytes = %q, %v; want %q", size, got.String(), err, tc.want)
			}
		}
	}
}

func TestStrftimeLiteralSpans(t *testing.T) {
	t.Parallel()
	tm := time.Date(2024, 1, 2, 3, 4, 5, 0, time.UTC)
	for _, size := range []int{0, 1, 7, 8, 9, 15, 16, 17, 4095, 4096, 4097, 65536} {
		prefix := strings.Repeat("x", size)
		for _, tc := range []struct {
			pattern string
			want    string
		}{
			{pattern: prefix, want: prefix},
			{pattern: prefix + "é\xff%%", want: prefix + "é\xff%"},
			{pattern: prefix + "%Y", want: prefix + "2024"},
			{pattern: prefix + "%F", want: prefix + "2024-01-02"},
			{pattern: prefix + "%Q", want: prefix + "%Q"},
		} {
			got, err := strftime(nil, tm, tc.pattern)
			if err != nil || got != tc.want {
				t.Errorf("strftime with %d literal bytes = %q, %v; want %q", size, got, err, tc.want)
			}
		}
	}
}

func TestFormatLiteralSpansPreserveErrors(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		pattern string
		values  []Value
		want    string
	}{
		{pattern: "%*s", values: []Value{NewString("x")}, want: "format dynamic width is not supported"},
		{pattern: "%.*s", values: []Value{NewString("x")}, want: "format dynamic precision is not supported"},
		{pattern: "%[2]s", values: []Value{NewString("x")}, want: "format references missing operand 2"},
		{pattern: "", values: []Value{NewString("x")}, want: "format has 1 unused operand(s)"},
		{pattern: "%d", values: []Value{NewString("x")}, want: "format %d expects integer operand"},
	} {
		for _, size := range []int{4095, 4096, 4097, maxFormatOutputBytes, maxFormatOutputBytes + 1} {
			pattern := strings.Repeat("x", size) + tc.pattern
			_, err := formatStringValues(pattern, tc.values)
			want := tc.want
			if size > maxFormatOutputBytes {
				want = fmt.Sprintf("format output exceeds limit %d bytes", maxFormatOutputBytes)
			}
			if err == nil || err.Error() != want {
				t.Errorf("formatStringValues with %d literal bytes and %q error = %v, want %q", size, tc.pattern, err, want)
			}
		}
	}
}

func TestFormatLiteralSpansPreserveNormalizedCapacity(t *testing.T) {
	t.Parallel()
	for _, tail := range []int{1, 4095, 4096, 4097, 64 << 10} {
		pattern := strings.Repeat("%%", maxFormatOutputBytes/2-1) + strings.Repeat("x", tail)
		prepared, err := prepareFormatString(nil, pattern, nil)
		if err != nil {
			t.Fatalf("prepareFormatString with %d trailing literals: %v", tail, err)
		}
		var original strings.Builder
		original.Grow(min(len(pattern), maxFormatOutputBytes))
		for i := range len(pattern) {
			original.WriteByte(pattern[i])
		}
		wantBytes := maxFormatOutputBytes/2 - 1 + tail
		if prepared.pattern != pattern || prepared.projectedBytes != wantBytes || prepared.scratchBytes != original.Cap() {
			t.Errorf("prepareFormatString with %d trailing literals: normalized length %d, projected %d, scratch %d; want length %d, projected %d, scratch %d",
				tail, len(prepared.pattern), prepared.projectedBytes, prepared.scratchBytes, len(pattern), wantBytes, original.Cap())
		}
	}
}

func TestFormattingLiteralQuotaBoundaries(t *testing.T) {
	t.Parallel()
	tm := time.Date(2024, 1, 2, 3, 4, 5, 0, time.UTC)
	for _, size := range []int{4095, 4096, 4097, 8193} {
		pattern := strings.Repeat("x", size)
		args := []Value{NewString(pattern)}
		for _, name := range []string{"format", "strftime"} {
			t.Run(fmt.Sprintf("%s/%d", name, size), func(t *testing.T) {
				t.Parallel()
				probe := &Execution{memoryQuota: 64 << 20}
				var b strings.Builder
				var quota, steps int
				var call func(*Execution) (string, error)
				if name == "format" {
					b.Grow(size)
					quota = probe.estimateMemoryUsageForCallRoots(NewNil(), NewNil(), args, nil, NewNil()) + estimatedValueBytes + estimatedStringHeaderBytes + size + b.Cap()
					steps = 2 * (size / stringScanBytesPerStep)
					call = func(exec *Execution) (string, error) {
						got, err := formatStringValuesChecked(exec, pattern, nil, NewNil(), args, nil, NewNil())
						return got.String(), err
					}
				} else {
					peak := 0
					for remaining := size; remaining > 0; {
						n := min(remaining, 4096)
						capacity := projectedBuilderCap(&b, n)
						scratch := capacity
						if capacity > b.Cap() {
							scratch += b.Cap()
						}
						peak = max(peak, scratch)
						b.Grow(n)
						b.WriteString(pattern[:n])
						remaining -= n
					}
					quota = probe.hashCallRootBytes(NewTime(tm), args, nil, NewNil()) + estimatedValueBytes + estimatedStringHeaderBytes + peak
					steps = 2 * size / stringScanBytesPerStep
					call = func(exec *Execution) (string, error) { return strftime(exec, tm, pattern) }
				}
				for _, limit := range []int{quota - 1, quota, quota + 1} {
					exec := &Execution{ctx: context.Background(), quota: 1 << 30, memoryQuota: limit}
					got, err := call(exec)
					if limit < quota {
						if !errors.Is(err, errMemoryQuotaExceeded) {
							t.Errorf("%s at memory quota %d: error %v, want memory quota exceeded", name, limit, err)
						}
					} else if err != nil || got != pattern {
						t.Errorf("%s at memory quota %d: output length %d, error %v; want %d bytes", name, limit, len(got), err, size)
					}
				}
				for _, limit := range []int{steps - 1, steps, steps + 1} {
					exec := &Execution{ctx: context.Background(), quota: limit}
					got, err := call(exec)
					if limit < steps {
						if !errors.Is(err, errStepQuotaExceeded) {
							t.Errorf("%s at step quota %d: error %v, want step quota exceeded", name, limit, err)
						}
					} else if err != nil || got != pattern || exec.steps != steps {
						t.Errorf("%s at step quota %d: output length %d, error %v, steps %d; want %d bytes and %d steps", name, limit, len(got), err, exec.steps, size, steps)
					}
				}
			})
		}
	}
}

func TestStrftimeLiteralWindowCancellation(t *testing.T) {
	t.Parallel()
	for _, prefix := range []int{4095, 4096, 4097} {
		ctx := &jsonCancelContext{Context: context.Background(), done: make(chan struct{})}
		exec := &Execution{ctx: ctx, quota: 1 << 30}
		var b strings.Builder
		r := strftimeRenderer{builder: &b, budget: &strftimeBudget{exec: exec}}
		err := r.renderInto(strings.Repeat("x", prefix)+"%%"+strings.Repeat("x", 4096), false)
		if !errors.Is(err, context.Canceled) || b.Len() != min(prefix, 4096) || ctx.polls != 3 {
			t.Errorf("renderInto with %d literal bytes: error %v, written %d, polls %d; want cancellation after %d bytes and 3 polls", prefix, err, b.Len(), ctx.polls, min(prefix, 4096))
		}
	}
}
