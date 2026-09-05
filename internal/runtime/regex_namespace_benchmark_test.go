package runtime

import (
	"context"
	"strings"
	"testing"
)

func BenchmarkRegexNamespaceScanning(b *testing.B) {
	for _, tc := range []struct {
		name    string
		pattern string
	}{
		{"dense", "a"},
		{"sparse", "z"},
		{"long_literal", strings.Repeat("a", 256)},
	} {
		b.Run(tc.name, func(b *testing.B) {
			script := compileScriptWithEngine(b, benchmarkEngine(), "def run(text, pattern)\nRegex.replace_all(text, pattern, \"\")\nend")
			args := []Value{NewString(strings.Repeat("a", 4096)), NewString(tc.pattern)}
			if _, err := script.Call(context.Background(), "run", args, CallOptions{}); err != nil {
				b.Fatal(err)
			}
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
