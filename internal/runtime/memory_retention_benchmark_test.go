package runtime

import (
	"context"
	"strings"
	"testing"
)

func BenchmarkRegexNamespaceMatch(b *testing.B) {
	for _, tc := range []struct {
		name    string
		pattern string
		text    string
	}{
		{name: "small_match", pattern: "x", text: strings.Repeat("z", 4096) + "x"},
		{name: "full_match", pattern: "(?s).*", text: strings.Repeat("a", 128)},
		{name: "no_match", pattern: "x", text: strings.Repeat("z", 4096)},
	} {
		b.Run(tc.name, func(b *testing.B) {
			script := compileScriptWithEngine(b, benchmarkEngine(), "def run(pattern, text)\nRegex.match(pattern, text)\nend")
			args := []Value{NewString(tc.pattern), NewString(tc.text)}
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

func BenchmarkRegexCachePattern(b *testing.B) {
	for _, mode := range []string{"hit", "miss"} {
		b.Run(mode, func(b *testing.B) {
			cache := newRegexCache(1, compiledRegexCacheInstructionBudget)
			patterns := []string{"a[0-9]+", "b[0-9]+"}
			if mode == "hit" {
				patterns[1] = patterns[0]
			}
			if _, err := cache.compile(patterns[0]); err != nil {
				b.Fatal(err)
			}
			b.ReportAllocs()
			b.ResetTimer()
			for i := range b.N {
				if _, err := cache.compile(patterns[i%len(patterns)]); err != nil {
					b.Fatal(err)
				}
			}
		})
	}
}

func BenchmarkMemoryScopeLoops(b *testing.B) {
	for _, tc := range []struct {
		name   string
		source string
	}{
		{name: "hash_delete", source: `def run()
  row = {}
  for i in 1..80
    row[i.to_s] = i
  end
  for i in 1..80
    row.delete(i.to_s)
  end
  row
end`},
		{name: "rescue", source: `def run()
  for i in 1..80
    begin
      raise("stop")
    rescue
      nil
    end
  end
  80
end`},
	} {
		b.Run(tc.name, func(b *testing.B) {
			script := compileScriptWithEngine(b, benchmarkEngine(), tc.source)
			b.ReportAllocs()
			b.ResetTimer()
			for range b.N {
				if _, err := script.Call(context.Background(), "run", nil, CallOptions{}); err != nil {
					b.Fatal(err)
				}
			}
		})
	}
}
