package runtime

import (
	"context"
	"fmt"
	"strings"
	"testing"
)

func benchmarkWhitespaceCall(b *testing.B, method, text string) {
	b.Helper()
	script := compileScriptWithConfig(b, Config{StepQuota: 5_000_000, MemoryQuotaBytes: 64 << 20}, "def run(s)\ns."+method+"\nend")
	args := []Value{NewString(text)}
	b.ReportAllocs()
	b.SetBytes(int64(len(text)))
	b.ResetTimer()
	for range b.N {
		if _, err := script.Call(context.Background(), "run", args, CallOptions{}); err != nil {
			b.Fatal(err)
		}
	}
}

func BenchmarkWhitespaceStrip(b *testing.B) {
	for _, n := range []int{16, 4096, 65536} {
		for _, kind := range []string{"long-padding", "short-padding", "no-padding", "unicode"} {
			text := strings.Repeat(" ", n/2-2) + "aBcD" + strings.Repeat("\x00", n/2-2)
			switch kind {
			case "short-padding":
				text = " \t" + strings.Repeat("a", n-4) + "\x00 "
			case "no-padding":
				text = strings.Repeat("a", n)
			case "unicode":
				text = "\u2003" + strings.Repeat("a", n-6) + "\u2003"
			}
			b.Run(fmt.Sprintf("%d/%s", n, kind), func(b *testing.B) {
				benchmarkWhitespaceCall(b, "strip", text)
			})
		}
	}
}

func BenchmarkWhitespaceSplit(b *testing.B) {
	for _, n := range []int{16, 4096, 65536} {
		for _, kind := range []string{"long-fields", "short-fields", "all-space", "unicode", "long-then-short", "short-then-long"} {
			pattern := strings.Repeat("a", 255) + " "
			switch kind {
			case "short-fields", "long-then-short", "short-then-long":
				pattern = "one two three four "
			case "all-space":
				pattern = " \t\n\r\v\f"
			case "unicode":
				pattern = "é\u2003界 "
			}
			text := strings.Repeat(pattern, n/len(pattern)) + strings.Repeat("a", n%len(pattern))
			switch kind {
			case "all-space":
				text = strings.Repeat(pattern, (n+len(pattern)-1)/len(pattern))[:n]
			case "long-then-short":
				text = strings.Repeat("a", n/2-1) + " " + text[n/2:]
			case "short-then-long":
				text = text[:n/2-1] + " " + strings.Repeat("a", n/2)
			}
			b.Run(fmt.Sprintf("%d/%s", n, kind), func(b *testing.B) {
				benchmarkWhitespaceCall(b, "split", text)
			})
		}
	}
}
