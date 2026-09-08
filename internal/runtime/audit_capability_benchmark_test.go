//go:build perfaudit

package runtime

import (
	"context"
	"fmt"
	goruntime "runtime"
	"strings"
	"testing"
	"unsafe"
)

func TestAuditJSONExtractRetainsDocument(t *testing.T) {
	script := compileScriptWithConfig(t, Config{
		StepQuota:        2_000_000,
		MemoryQuotaBytes: 16 << 20,
	}, "def run(raw)\n  JSON.parse(raw)[\"id\"]\nend")
	for _, token := range []string{`"x"`, `"\u0078"`} {
		raw := `{"id":` + token + `,"unused":"` + strings.Repeat("z", 512<<10) + `"}`
		got, err := script.Call(context.Background(), "run", []Value{NewString(raw)}, CallOptions{})
		if err != nil {
			t.Fatal(err)
		}
		if got.String() != "x" {
			t.Fatalf("JSON.parse(raw)[id] = %q, want x", got.String())
		}
		start := uintptr(unsafe.Pointer(unsafe.StringData(raw)))
		result := uintptr(unsafe.Pointer(unsafe.StringData(got.String())))
		t.Logf("id token %s: input=%d bytes output=%d bytes aliases input=%t", token, len(raw), len(got.String()), result >= start && result < start+uintptr(len(raw)))
		goruntime.KeepAlive(raw)
	}
}

func TestAuditJSONKeyRetainsDocument(t *testing.T) {
	script := compileScriptWithConfig(t, Config{
		StepQuota:        2_000_000,
		MemoryQuotaBytes: 16 << 20,
	}, "def run(raw)\n  JSON.parse(raw).keys[0]\nend")
	for _, token := range []string{`"tiny"`, `"\u0074iny"`} {
		raw := `{` + token + `:0,"unused":"` + strings.Repeat("z", 512<<10) + `"}`
		got, err := script.Call(context.Background(), "run", []Value{NewString(raw)}, CallOptions{})
		if err != nil {
			t.Fatal(err)
		}
		if got.String() != "tiny" {
			t.Fatalf("JSON.parse(raw).keys[0] = %q, want tiny", got.String())
		}
		start := uintptr(unsafe.Pointer(unsafe.StringData(raw)))
		result := uintptr(unsafe.Pointer(unsafe.StringData(got.String())))
		t.Logf("key token %s: input=%d bytes output=%d bytes aliases input=%t", token, len(raw), len(got.String()), result >= start && result < start+uintptr(len(raw)))
		goruntime.KeepAlive(raw)
	}
}

func TestAuditJSONExtractRetainedHeap(t *testing.T) {
	const documents = 32
	const payloadBytes = 512 << 10
	script := compileScriptWithConfig(t, Config{
		StepQuota:        2_000_000,
		MemoryQuotaBytes: 16 << 20,
	}, "def run(raw)\n  JSON.parse(raw)[\"id\"]\nend")
	for _, token := range []string{`"x"`, `"\u0078"`} {
		t.Run(token, func(t *testing.T) {
			// Warm the per-engine cache before taking the heap baseline.
			if _, err := script.Call(context.Background(), "run", []Value{NewString(`{"id":"x"}`)}, CallOptions{}); err != nil {
				t.Fatal(err)
			}
			goruntime.GC()
			var before, after goruntime.MemStats
			goruntime.ReadMemStats(&before)
			results := make([]Value, documents)
			for i := range documents {
				raw := `{"id":` + token + `,"unused":"` + strings.Repeat("z", payloadBytes) + `"}`
				got, err := script.Call(context.Background(), "run", []Value{NewString(raw)}, CallOptions{})
				if err != nil {
					t.Fatal(err)
				}
				if got.String() != "x" {
					t.Fatalf("JSON.parse(raw)[id] = %q, want x", got.String())
				}
				results[i] = got
			}
			goruntime.GC()
			goruntime.ReadMemStats(&after)
			t.Logf("%d calls, %d-byte documents, %d result bytes: retained heap delta=%d bytes", documents, payloadBytes, documents, int64(after.HeapAlloc)-int64(before.HeapAlloc))
			goruntime.KeepAlive(results)
			goruntime.KeepAlive(script)
		})
	}
}

func BenchmarkAuditJSONExtractField(b *testing.B) {
	for _, size := range []int{1024, 64 << 10, 512 << 10} {
		b.Run(fmt.Sprintf("bytes_%d", size), func(b *testing.B) {
			script := compileScriptWithConfig(b, Config{
				StepQuota:        2_000_000,
				MemoryQuotaBytes: 16 << 20,
			}, "def run(raw)\n  JSON.parse(raw)[\"id\"]\nend")
			raw := `{"id":"x","unused":"` + strings.Repeat("z", size) + `"}`
			args := []Value{NewString(raw)}
			b.ReportAllocs()
			b.ResetTimer()
			for range b.N {
				if _, err := script.Call(context.Background(), "run", args, CallOptions{}); err != nil {
					b.Fatal(err)
				}
			}
		})
	}
}
