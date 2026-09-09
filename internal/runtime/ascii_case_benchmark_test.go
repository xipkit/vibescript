package runtime

import (
	"context"
	"fmt"
	"strings"
	"testing"
)

func BenchmarkStringASCIICase(b *testing.B) {
	for _, method := range []string{"upcase", "downcase", "swapcase", "capitalize"} {
		for _, size := range []int{16, 4096, 65536} {
			text := strings.Repeat("aBcD9_! ", size/8)
			b.Run(fmt.Sprintf("%s/%d/ASCII", method, size), func(b *testing.B) {
				benchmarkStringCaseCall(b, method+"(:ascii)", text)
			})
		}
		for _, tc := range []struct {
			name string
			text string
			args string
		}{
			{name: "UnicodeASCII", text: strings.Repeat("éΣ😀aZ", 410), args: "(:ascii)"},
			{name: "UnicodeDefault", text: strings.Repeat("éΣ😀aZ", 410)},
			{name: "InvalidDefault", text: strings.Repeat("aBcD9_! ", 512) + "\xff"},
			{name: "UnchangedASCII", text: strings.Repeat("1234_[] ", 8192), args: "(:ascii)"},
		} {
			b.Run(method+"/"+tc.name, func(b *testing.B) {
				benchmarkStringCaseCall(b, method+tc.args, tc.text)
			})
		}
	}
}

func benchmarkStringCaseCall(b *testing.B, call, text string) {
	b.Helper()
	script := simdBenchmarkCompileWithEngine(b, MustNewEngine(Config{
		StepQuota:        5_000_000,
		MemoryQuotaBytes: 64 << 20,
	}), "def run(text) text."+call+" end")
	args := []Value{NewString(text)}
	b.ReportAllocs()
	b.SetBytes(int64(len(text)))
	b.ResetTimer()
	for range b.N {
		if _, err := script.Call(context.Background(), "run", args, CallOptions{}); err != nil {
			b.Fatalf("%s call failed: %v", call, err)
		}
	}
}

var asciiCaseBenchmarkSink string

func BenchmarkASCIICaseKernel(b *testing.B) {
	for _, tc := range []struct {
		name      string
		transform func(string) string
	}{
		{name: "upcase", transform: asciiUpcase},
		{name: "downcase", transform: asciiDowncase},
		{name: "swapcase", transform: asciiSwapCase},
		{name: "capitalize", transform: asciiCapitalize},
	} {
		for _, size := range []int{8, 16, 32, 63, 64, 65, 128, 256, 1024, 4096, 16384, 65536} {
			text := strings.Repeat("aBcD9_! ", (size+7)/8)[:size]
			b.Run(fmt.Sprintf("%s/%d", tc.name, size), func(b *testing.B) {
				b.ReportAllocs()
				b.SetBytes(int64(size))
				b.ResetTimer()
				for range b.N {
					asciiCaseBenchmarkSink = tc.transform(text)
				}
			})
		}
	}
}
