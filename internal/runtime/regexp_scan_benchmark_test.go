package runtime

import (
	"context"
	"fmt"
	"strings"
	"testing"
)

func BenchmarkRegexpEscapeSpans(b *testing.B) {
	for _, n := range []int{16, 4096, 65536} {
		for _, kind := range []string{"plain", "sparse", "dense", "unicode", "invalid", "mixed"} {
			pattern := "abcdefgh012345"
			switch kind {
			case "sparse":
				pattern = strings.Repeat("a", 63) + "."
			case "dense":
				pattern = "[a]+(b)?$"
			case "unicode":
				pattern = "é界🙂aZ"
			case "invalid":
				pattern = "\xffa\xc0\x80"
			case "mixed":
				pattern = strings.Repeat("a", 128) + "[a]+(b)?$"
			}
			text := strings.Repeat(pattern, n/len(pattern)) + strings.Repeat("a", n%len(pattern))
			if kind == "sparse" && n < 64 {
				text = strings.Repeat("a", n-1) + "."
			}
			if kind == "mixed" && n < len(pattern) {
				text = strings.Repeat("a", n/2) + strings.Repeat("[a]+(b)?$", n/9+1)[:n-n/2]
			}
			b.Run(fmt.Sprintf("%d/%s", n, kind), func(b *testing.B) {
				script := simdBenchmarkCompileWithConfig(b, Config{StepQuota: 5_000_000, MemoryQuotaBytes: 64 << 20}, "def run(s)\nRegexp.escape(s)\nend")
				args := []Value{NewString(text)}
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
}

func BenchmarkRegexpEscapeCaller(b *testing.B) {
	const n = 65536
	for _, kind := range []string{"plain", "sparse", "dense", "unicode"} {
		pattern := "abcdefgh012345"
		switch kind {
		case "sparse":
			pattern = strings.Repeat("a", 63) + "."
		case "dense":
			pattern = "[a]+(b)?$"
		case "unicode":
			pattern = "é界🙂aZ"
		}
		text := strings.Repeat(pattern, n/len(pattern)) + strings.Repeat("a", n%len(pattern))
		b.Run(kind, func(b *testing.B) {
			benchmarkRegexpEscapeCall(b, "Regexp.escape(s)", []Value{NewString(text), NewNil()})
		})
	}
}

// Keep the separate helper and second argument to exercise a different caller layout.
func benchmarkRegexpEscapeCall(b *testing.B, expression string, args []Value) {
	b.Helper()
	script := simdBenchmarkCompileWithConfig(b, Config{StepQuota: 5_000_000, MemoryQuotaBytes: 64 << 20}, "def run(s, other)\n"+expression+"\nend")
	b.ReportAllocs()
	b.ResetTimer()
	for range b.N {
		if _, err := script.Call(context.Background(), "run", args, CallOptions{}); err != nil {
			b.Fatal(err)
		}
	}
}
