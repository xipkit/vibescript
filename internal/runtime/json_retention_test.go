package runtime

import (
	"context"
	"fmt"
	"runtime"
	"strings"
	"testing"
)

func TestJSONParsedStringsReleaseSourceDocuments(t *testing.T) {
	// This measures process-wide heap, so the subtests must run serially.
	const documents = 32
	const payloadBytes = 512 << 10
	for _, tc := range []struct {
		name string
		expr string
		want string
	}{
		{name: "value", expr: `JSON.parse(raw)["id"]`, want: "x"},
		{name: "key", expr: `JSON.parse(raw).keys[0]`, want: "id"},
		{name: "typed value", expr: `JSON.parse_as(raw, { id: string, unused: string })["id"]`, want: "x"},
		{name: "typed key", expr: `JSON.parse_as(raw, { id: string, unused: string }).keys[0]`, want: "id"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			script := compileScriptWithConfig(t, Config{
				StepQuota:        2_000_000,
				MemoryQuotaBytes: 16 << 20,
			}, "def run(raw)\n  "+tc.expr+"\nend")
			if _, err := script.Call(context.Background(), "run", []Value{NewString(`{"id":"x","unused":""}`)}, CallOptions{}); err != nil {
				t.Fatalf("warming %s: %v", tc.expr, err)
			}

			runtime.GC()
			var before, after runtime.MemStats
			runtime.ReadMemStats(&before)
			kept := make([]Value, documents)
			for i := range kept {
				raw := `{"id":"x","unused":"` + strings.Repeat("z", payloadBytes) + `"}`
				got, err := script.Call(context.Background(), "run", []Value{NewString(raw)}, CallOptions{})
				if err != nil {
					t.Fatalf("%s for document %d: %v", tc.expr, i, err)
				}
				if got.Kind() != KindString || got.String() != tc.want {
					t.Fatalf("%s = %s, want %q", tc.expr, got.Inspect(), tc.want)
				}
				kept[i] = got
			}
			runtime.GC()
			runtime.ReadMemStats(&after)
			held := int64(after.HeapAlloc) - int64(before.HeapAlloc)
			t.Logf("%s: %d result bytes retain %d heap bytes", tc.expr, documents*len(tc.want), held)
			// Aliasing all source documents retains at least 16 MiB.
			if held > 4<<20 {
				t.Errorf("%s retains %d heap bytes, want <=4 MiB", tc.expr, held)
			}
			runtime.KeepAlive(kept)
			runtime.KeepAlive(script)
		})
	}
}

func BenchmarkJSONParseExtractString(b *testing.B) {
	for _, size := range []int{1024, 64 << 10, 512 << 10} {
		b.Run(fmt.Sprintf("bytes_%d", size), func(b *testing.B) {
			script := compileScriptWithConfig(b, Config{
				StepQuota:        2_000_000,
				MemoryQuotaBytes: 16 << 20,
			}, "def run(raw)\n  JSON.parse(raw)[\"id\"]\nend")
			raw := `{"id":"x","unused":"` + strings.Repeat("z", size) + `"}`
			args := []Value{NewString(raw)}
			b.ReportAllocs()
			b.SetBytes(int64(len(raw)))
			for b.Loop() {
				if _, err := script.Call(context.Background(), "run", args, CallOptions{}); err != nil {
					b.Fatal(err)
				}
			}
		})
	}
}
