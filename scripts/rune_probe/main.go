package main

import (
	"context"
	"flag"
	"fmt"
	"os"
	"regexp"
	"runtime/pprof"
	"strings"
	"testing"
	"unicode/utf8"

	"github.com/mgomes/vibescript/vibes"
	"github.com/mgomes/vibescript/vibes/value"
)

var result int

var inputs = []struct {
	name string
	text string
}{
	{"Empty", ""},
	{"Tiny", "hello"},
	{"ShortUnicode", "héllo 終🌍"},
	{"ASCII", strings.Repeat("a", 4095) + "z"},
	{"Unicode", strings.Repeat("héllo ", 680) + "終"},
	{"CJK", strings.Repeat("日本語漢字", 300)},
	{"Emoji", strings.Repeat("🌍🙂", 600)},
	{"ASCIIPrefix", strings.Repeat("a", 4095) + "終"},
	{"ASCIISuffix", "終" + strings.Repeat("a", 4095)},
	{"Invalid", strings.Repeat("\xffa\xc0\xaf\xed\xa0\x80", 600)},
	{"InvalidSuffix", strings.Repeat("héllo ", 680) + "\xf0\x80"},
}

func benchmarkCount(b *testing.B, count func(string) int) {
	for _, input := range inputs {
		b.Run(input.name, func(b *testing.B) {
			want := utf8.RuneCountInString(input.text)
			if got := count(input.text); got != want {
				b.Fatalf("count(%q) = %d, want %d", input.text, got, want)
			}
			b.ReportAllocs()
			b.SetBytes(int64(len(input.text)))
			b.ResetTimer()
			for range b.N {
				result = count(input.text)
			}
		})
	}
}

func benchmarkCalls(b *testing.B) {
	engine := vibes.MustNewEngine(vibes.Config{StepQuota: 2_000_000, MemoryQuotaBytes: 2 << 20})
	for _, operation := range []string{"length", "index(needle)", "rindex(needle)", "slice(20, 4).bytesize"} {
		for _, input := range inputs[1:] {
			b.Run(operation+"/"+input.name, func(b *testing.B) {
				source := "def run(text, needle) total = 0; for i in 1..200; total = total + text." + operation + "; end; total end"
				script, err := engine.Compile(source)
				if err != nil {
					b.Fatal(err)
				}
				needle := input.text[:1]
				if input.name == "Unicode" {
					needle = "終"
				}
				args := []value.Value{value.NewString(input.text), value.NewString(needle)}
				if strings.Contains(operation, "slice") && len(input.text) < 100 {
					b.Skip("short slice fixture")
				}
				b.ReportAllocs()
				b.ResetTimer()
				for range b.N {
					if _, err := script.Call(context.Background(), "run", args, vibes.CallOptions{}); err != nil {
						b.Fatal(err)
					}
				}
			})
		}
	}
}

func profileBenchmark(b *testing.B, run func(*testing.B)) {
	if path := os.Getenv("RUNE_CPU_PROFILE"); path != "" {
		output, err := os.Create(path)
		if err != nil {
			panic(err)
		}
		defer output.Close()
		if err := pprof.StartCPUProfile(output); err != nil {
			panic(err)
		}
		defer pprof.StopCPUProfile()
	}
	run(b)
}

func main() {
	testing.Init()
	flag.Parse()
	fmt.Println("probe: rune-width-candidates")
	benchmarks := []testing.InternalBenchmark{
		{Name: "BenchmarkRange", F: func(b *testing.B) { benchmarkCount(b, countRange) }},
		{Name: "BenchmarkValidated", F: func(b *testing.B) { benchmarkCount(b, countValidated) }},
		{Name: "BenchmarkTwoByte", F: func(b *testing.B) { benchmarkCount(b, countTwoByte) }},
		{Name: "BenchmarkWidthWords", F: func(b *testing.B) { benchmarkCount(b, countWidthWords) }},
		{Name: "BenchmarkASCIIRuns", F: func(b *testing.B) { benchmarkCount(b, countASCIIRuns) }},
		{Name: "BenchmarkIndexed", F: func(b *testing.B) { benchmarkCount(b, countIndexed) }},
		{Name: "BenchmarkRunWords", F: func(b *testing.B) { benchmarkCount(b, countRunWords) }},
		{Name: "BenchmarkWidthCalls", F: func(b *testing.B) { benchmarkCount(b, countWidthCalls) }},
		{Name: "BenchmarkWidths", F: func(b *testing.B) { benchmarkCount(b, countWidths) }},
		{Name: "BenchmarkCalls", F: benchmarkCalls},
	}
	for i := range benchmarks {
		run := benchmarks[i].F
		benchmarks[i].F = func(b *testing.B) { profileBenchmark(b, run) }
	}
	testing.Main(regexp.MatchString, nil, benchmarks, nil)
}
