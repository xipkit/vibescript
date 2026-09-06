package runtime

import (
	"context"
	"errors"
	"regexp"
	"runtime"
	"strings"
	"testing"
)

func TestRegexpUnionBoundsTemporaryAllocation(t *testing.T) {
	arg := NewString(strings.Repeat(".", 16<<10))
	args := make([]Value, 128)
	for i := range args {
		args[i] = arg
	}
	runtime.GC()
	var before, after runtime.MemStats
	runtime.ReadMemStats(&before)
	_, err := builtinRegexpUnion(nil, NewNil(), args, nil, NewNil())
	runtime.ReadMemStats(&after)
	if err == nil {
		t.Fatal("oversized union returned no error")
	}
	allocated := after.TotalAlloc - before.TotalAlloc
	t.Logf("rejected union allocated %d bytes", allocated)
	if allocated > 1<<20 {
		t.Errorf("rejected union allocated %d bytes, want <=1 MiB", allocated)
	}
}

func TestRegexpPreprocessingWorkQuotas(t *testing.T) {
	t.Parallel()
	tests := []struct {
		name string
		expr string
		args []Value
	}{
		{"new", "Regexp.new(s)", []Value{NewString(strings.Repeat("a", 4096))}},
		{"union", "Regexp.union(*s)", []Value{NewArray([]Value{NewString(strings.Repeat("a.", 2048)), NewString("b")})}},
		{"escape", "Regexp.escape(s)", []Value{NewString(strings.Repeat("a.", 32<<10))}},
		{"quote", "Regexp.quote(s)", []Value{NewString(strings.Repeat("a.", 32<<10))}},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			for _, quota := range []int{32, 100000} {
				script := compileScriptWithConfig(t, Config{StepQuota: quota, MemoryQuotaBytes: Unlimited}, "def run(s)\n"+tc.expr+"\nend")
				_, err := script.Call(context.Background(), "run", tc.args, CallOptions{})
				if quota == 32 {
					var runtimeErr *RuntimeError
					if !errors.As(err, &runtimeErr) || runtimeErr.Type != runtimeErrorTypeLimit {
						t.Errorf("quota=%d error=%v, want LimitError", quota, err)
					}
				} else if err != nil {
					t.Errorf("quota=%d legitimate call failed: %v", quota, err)
				}
			}
		})
	}
}

func TestRegexpEscapeRejectsBeforeAllocation(t *testing.T) {
	script := compileScriptWithConfig(t, Config{StepQuota: Unlimited, MemoryQuotaBytes: 8 << 20}, "def run(s)\nRegexp.escape(s)\nend")
	plain := NewString(strings.Repeat("a", 3<<20))
	if _, err := script.Call(context.Background(), "run", []Value{plain}, CallOptions{}); err != nil {
		t.Fatalf("no-copy control failed: %v", err)
	}
	args := []Value{NewString(strings.Repeat(".", 3<<20))}
	runtime.GC()
	var before, after runtime.MemStats
	runtime.ReadMemStats(&before)
	_, err := script.Call(context.Background(), "run", args, CallOptions{})
	runtime.ReadMemStats(&after)
	var runtimeErr *RuntimeError
	if !errors.As(err, &runtimeErr) || runtimeErr.Type != runtimeErrorTypeLimit {
		t.Fatalf("error=%v, want memory LimitError", err)
	}
	allocated := after.TotalAlloc - before.TotalAlloc
	t.Logf("rejected escape allocated %d bytes", allocated)
	if allocated > 2<<20 {
		t.Errorf("rejected escape allocated %d bytes, want <=2 MiB", allocated)
	}
}

func TestRegexpUnionPatternBoundaries(t *testing.T) {
	t.Parallel()
	emptyArgs := make([]Value, maxRegexPatternSize+1)
	for i := range emptyArgs {
		emptyArgs[i] = NewString("")
	}
	tests := []struct {
		name string
		args []Value
		want string
		err  string
	}{
		{name: "empty", want: `[^\s\S]`},
		{name: "plain", args: []Value{NewString("a-b c")}, want: "a-b c"},
		{name: "literals", args: []Value{NewString("a.b"), NewString("(c)")}, want: `a\.b|\(c\)`},
		{name: "empty_alternative", args: []Value{NewString(""), NewString("a")}, want: "|a"},
		{name: "raw_at_cap", args: []Value{NewString(strings.Repeat("a", maxRegexPatternSize))}, want: strings.Repeat("a", maxRegexPatternSize)},
		{name: "raw_over_cap", args: []Value{NewString(strings.Repeat("a", maxRegexPatternSize+1))}, err: "Regexp.union pattern exceeds limit"},
		{name: "quoted_at_cap", args: []Value{NewString(strings.Repeat(".", maxRegexPatternSize/2))}, want: strings.Repeat(`\.`, maxRegexPatternSize/2)},
		{name: "quoted_over_cap", args: []Value{NewString(strings.Repeat(".", maxRegexPatternSize/2+1))}, err: "Regexp.union pattern exceeds limit"},
		{name: "joined_at_cap", args: []Value{NewString(strings.Repeat("a", maxRegexPatternSize-2)), NewString("b")}, want: strings.Repeat("a", maxRegexPatternSize-2) + "|b"},
		{name: "separator_over_cap", args: []Value{NewString(strings.Repeat("a", maxRegexPatternSize)), NewString("")}, err: "Regexp.union pattern exceeds limit"},
		{name: "empty_args_at_cap", args: emptyArgs, want: strings.Repeat("|", maxRegexPatternSize)},
		{name: "empty_args_over_cap", args: append(append([]Value(nil), emptyArgs...), NewString("")), err: "Regexp.union pattern exceeds limit"},
		{name: "type_before_limit", args: []Value{NewString(strings.Repeat("a", maxRegexPatternSize+1)), NewInt(1)}, err: "Regexp.union expects string patterns"},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			got, err := regexpUnionPattern(nil, NewNil(), tc.args)
			if tc.err != "" {
				if err == nil || !strings.Contains(err.Error(), tc.err) {
					t.Fatalf("error=%v, want %q", err, tc.err)
				}
				return
			}
			if err != nil || got != tc.want {
				t.Errorf("got length=%d, error=%v; want length=%d", len(got), err, len(tc.want))
			}
		})
	}
}

func TestRegexpQuotingMatchesGo(t *testing.T) {
	t.Parallel()
	allBytes := make([]byte, 256)
	for i := range allBytes {
		allBytes[i] = byte(i)
	}
	for _, text := range []string{"", "plain text-with-hyphens", `\.+*?()|[]{}^$`, "é世界🙂\n.", string(allBytes)} {
		want := regexp.QuoteMeta(text)
		for _, limit := range []int{len(want) - 1, len(want), len(want) + 1} {
			size, ok := regexpQuotedSize(text, limit)
			if ok != (len(want) <= limit) || ok && size != len(want) {
				t.Errorf("size(%q, %d)=%d,%v; want length=%d", text, limit, size, ok, len(want))
			}
		}
		got, err := regexpEscape(nil, NewNil(), []Value{NewString(text)})
		if err != nil || got != want {
			t.Errorf("escape(%q)=%q,%v; want %q", text, got, err, want)
		}
	}
}

func TestRegexpUnionLimitDispatch(t *testing.T) {
	t.Parallel()
	script := compileScript(t, "def run(s)\nRegexp.union(*s)\nend")
	text := NewString(strings.Repeat(".", 4096))
	requireCallRuntimeErrorType(t, script, "run", []Value{NewArray([]Value{text, text})}, CallOptions{}, runtimeErrorTypeLimit)
}

func TestRegexpEscapeMemoryQuota(t *testing.T) {
	t.Parallel()
	for _, method := range []string{"escape", "quote"} {
		t.Run(method, func(t *testing.T) {
			t.Parallel()
			script := compileScriptWithConfig(t, Config{StepQuota: Unlimited, MemoryQuotaBytes: 256 << 10}, "def run(s)\nRegexp."+method+"(s)\nend")
			for _, text := range []string{strings.Repeat("a", 128<<10), strings.Repeat(".", 32<<10)} {
				got, err := script.Call(context.Background(), "run", []Value{NewString(text)}, CallOptions{})
				if err != nil || got.String() != regexp.QuoteMeta(text) {
					t.Errorf("legitimate escape length=%d: result length=%d, error=%v", len(text), len(got.String()), err)
				}
			}
			requireCallRuntimeErrorType(t, script, "run", []Value{NewString(strings.Repeat(".", 128<<10))}, CallOptions{}, runtimeErrorTypeLimit)
		})
	}
}
