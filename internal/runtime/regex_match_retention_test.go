package runtime

import (
	"context"
	"errors"
	goruntime "runtime"
	"strings"
	"testing"
)

func TestRegexNamespaceMatchReleasesSubject(t *testing.T) {
	script := compileScriptWithConfig(t, Config{StepQuota: 2_000_000, MemoryQuotaBytes: 4 << 20}, `def run(text)
  Regex.match("x$", text)
end`)
	if _, err := script.Call(context.Background(), "run", []Value{NewString("warmx")}, CallOptions{}); err != nil {
		t.Fatal(err)
	}
	var before, after goruntime.MemStats
	goruntime.GC()
	goruntime.ReadMemStats(&before)
	kept := make([]Value, 24)
	for i := range kept {
		text := strings.Repeat("z", 512<<10) + "x"
		got, err := script.Call(context.Background(), "run", []Value{NewString(text)}, CallOptions{})
		if err != nil || got.Kind() != KindString || got.String() != "x" {
			t.Fatalf("Regex.match(x$, subject) = %v, %v, want x, nil", got, err)
		}
		kept[i] = got
	}
	goruntime.GC()
	goruntime.ReadMemStats(&after)
	held := int64(after.HeapAlloc) - int64(before.HeapAlloc)
	t.Logf("24 one-byte matches retain %d bytes", held)
	if limit := int64(2 << 20); held > limit {
		t.Errorf("24 one-byte matches retain %d bytes, want less than %d", held, limit)
	}
	goruntime.KeepAlive(kept)
	goruntime.KeepAlive(script)
}

func TestRegexNamespaceMatchReservesCopy(t *testing.T) {
	const bytes = 600 << 10
	script := compileScriptWithConfig(t, Config{StepQuota: 500_000_000, MemoryQuotaBytes: bytes + bytes/2}, `def run(text)
  Regex.match("a+", text)
end`)
	if _, err := script.Call(context.Background(), "run", []Value{NewString("ba")}, CallOptions{}); err != nil {
		t.Fatal(err)
	}
	args := []Value{NewString("b" + strings.Repeat("a", bytes))}
	var before, after goruntime.MemStats
	goruntime.GC()
	goruntime.ReadMemStats(&before)
	_, err := script.Call(context.Background(), "run", args, CallOptions{})
	goruntime.ReadMemStats(&after)
	var runtimeErr *RuntimeError
	if !errors.As(err, &runtimeErr) || runtimeErr.Type != runtimeErrorTypeLimit || !strings.Contains(runtimeErr.Message, "memory quota exceeded") {
		t.Fatalf("Regex.match() error = %v, want memory quota exhaustion", err)
	}
	if allocated := after.TotalAlloc - before.TotalAlloc; allocated >= bytes/2 {
		t.Errorf("rejected Regex.match allocated %d bytes, want less than %d before copying", allocated, bytes/2)
	}
}

func TestRegexNamespaceMatchPreservesBytes(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		name    string
		pattern string
		text    string
		want    string
	}{
		{name: "full", pattern: "(?s).*", text: "a\xffb", want: "a\xffb"},
		{name: "invalid UTF-8", pattern: "�", text: "a\xffb", want: "\xff"},
		{name: "unicode", pattern: "é", text: "café", want: "é"},
		{name: "empty", pattern: "$", text: "abc", want: ""},
	} {
		t.Run(tc.name, func(t *testing.T) {
			got, err := builtinRegexMatch(nil, NewNil(), []Value{NewString(tc.pattern), NewString(tc.text)}, nil, NewNil())
			if err != nil || got.Kind() != KindString || got.String() != tc.want {
				t.Errorf("Regex.match(%q, %q) = %q, %v, want %q, nil", tc.pattern, tc.text, got.String(), err, tc.want)
			}
		})
	}
	got, err := builtinRegexMatch(nil, NewNil(), []Value{NewString("z"), NewString("abc")}, nil, NewNil())
	if err != nil || !got.IsNil() {
		t.Errorf("Regex.match(z, abc) = %v, %v, want nil, nil", got, err)
	}
}
