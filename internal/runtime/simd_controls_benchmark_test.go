package runtime

import (
	"context"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func simdBenchmarkEngine() *Engine {
	return MustNewEngine(Config{
		StepQuota:        2_000_000,
		MemoryQuotaBytes: 2 << 20,
	})
}

func simdBenchmarkCompileWithEngine(t testing.TB, engine *Engine, source string) *Script {
	t.Helper()
	script, err := engine.Compile(source)
	if err != nil {
		t.Fatalf("compile failed: %v", err)
	}
	return script
}

func simdBenchmarkCompileWithConfig(t testing.TB, cfg Config, source string) *Script {
	t.Helper()
	engine := MustNewEngine(cfg)
	script, err := engine.Compile(source)
	if err != nil {
		t.Fatalf("compile failed: %v", err)
	}
	return script
}

func simdBenchmarkCall(t testing.TB, ctx context.Context, script *Script, fn string, args []Value, opts CallOptions) Value {
	t.Helper()
	result, err := script.Call(ctx, fn, args, opts)
	if err != nil {
		t.Fatalf("call failed: %v", err)
	}
	return result
}

func simdBenchmarkSourceFromFile(b *testing.B, rel string) string {
	b.Helper()
	path := filepath.Join("..", "..", filepath.FromSlash(rel))
	source, err := os.ReadFile(path)
	if err != nil {
		b.Fatalf("read %s: %v", path, err)
	}
	return string(source)
}

func simdBenchmarkStringHelperLoop(b *testing.B, source string, args []Value) {
	b.Helper()
	script := simdBenchmarkCompileWithEngine(b, simdBenchmarkEngine(), source)

	b.ReportAllocs()
	b.ResetTimer()
	for range b.N {
		if _, err := script.Call(context.Background(), "run", args, CallOptions{}); err != nil {
			b.Fatalf("call failed: %v", err)
		}
	}
}

func simdBenchmarkASCIIStringText() string {
	return strings.Repeat("a", 4095) + "z"
}

func simdBenchmarkUnicodeStringText() string {
	return strings.Repeat("héllo ", 680) + "終"
}

func BenchmarkSIMDStringLengthLoopASCII(b *testing.B) {
	simdBenchmarkStringHelperLoop(b, `def run(text, n)
  total = 0
  for i in 1..n
    total = total + text.length
  end
  total
end`, []Value{NewString(simdBenchmarkASCIIStringText()), NewInt(200)})
}

func BenchmarkSIMDStringLengthLoopUnicode(b *testing.B) {
	simdBenchmarkStringHelperLoop(b, `def run(text, n)
  total = 0
  for i in 1..n
    total = total + text.length
  end
  total
end`, []Value{NewString(simdBenchmarkUnicodeStringText()), NewInt(200)})
}

func BenchmarkSIMDStringIndexLoopASCII(b *testing.B) {
	simdBenchmarkStringHelperLoop(b, `def run(text, needle, n)
  total = 0
  for i in 1..n
    total = total + text.index(needle)
  end
  total
end`, []Value{NewString(simdBenchmarkASCIIStringText()), NewString("z"), NewInt(200)})
}

func BenchmarkSIMDStringIndexLoopUnicode(b *testing.B) {
	simdBenchmarkStringHelperLoop(b, `def run(text, needle, n)
  total = 0
  for i in 1..n
    total = total + text.index(needle)
  end
  total
end`, []Value{NewString(simdBenchmarkUnicodeStringText()), NewString("終"), NewInt(200)})
}

func BenchmarkSIMDStringRIndexLoopASCII(b *testing.B) {
	simdBenchmarkStringHelperLoop(b, `def run(text, needle, n)
  total = 0
  for i in 1..n
    total = total + text.rindex(needle)
  end
  total
end`, []Value{NewString(simdBenchmarkASCIIStringText()), NewString("a"), NewInt(200)})
}

func BenchmarkSIMDStringRIndexLoopUnicode(b *testing.B) {
	simdBenchmarkStringHelperLoop(b, `def run(text, needle, n)
  total = 0
  for i in 1..n
    total = total + text.rindex(needle)
  end
  total
end`, []Value{NewString(simdBenchmarkUnicodeStringText()), NewString("é"), NewInt(200)})
}

func BenchmarkSIMDStringSliceLoopASCII(b *testing.B) {
	simdBenchmarkStringHelperLoop(b, `def run(text, start, n)
  total = 0
  for i in 1..n
    total = total + text.slice(start, 4).bytesize
  end
  total
end`, []Value{NewString(simdBenchmarkASCIIStringText()), NewInt(4092), NewInt(200)})
}

func BenchmarkSIMDStringSliceLoopUnicode(b *testing.B) {
	simdBenchmarkStringHelperLoop(b, `def run(text, start, n)
  total = 0
  for i in 1..n
    total = total + text.slice(start, 4).bytesize
  end
  total
end`, []Value{NewString(simdBenchmarkUnicodeStringText()), NewInt(4077), NewInt(200)})
}
