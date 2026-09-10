package runtime

import (
	"context"
	"strings"
	"testing"
)

func BenchmarkStringRIndexOffsets(b *testing.B) {
	for _, input := range []struct {
		name string
		text string
	}{
		{name: "Short", text: "aé終😀a"},
		{name: "ASCII", text: strings.Repeat("hello ", 680) + "a"},
		{name: "Unicode", text: strings.Repeat("héllo ", 680) + "a"},
		{name: "CJK", text: strings.Repeat("日本語漢字", 300) + "a"},
		{name: "Invalid", text: strings.Repeat("a\xff\xed\xa0\x80", 800) + "a"},
	} {
		for _, offset := range []struct {
			name string
			arg  string
		}{
			{name: "Default"},
			{name: "Zero", arg: ", 0"},
			{name: "Negative", arg: ", -1"},
			{name: "PastEnd", arg: ", 1000000"},
		} {
			b.Run(input.name+"/"+offset.name, func(b *testing.B) {
				source := `def run(text, needle)
  total = 0
  for i in 1..200
    found = text.rindex(needle` + offset.arg + `)
    if found != nil
      total = total + found
    end
  end
  total
end`
				script := simdBenchmarkCompileWithEngine(b, simdBenchmarkEngine(), source)
				args := []Value{NewString(input.text), NewString("a")}
				want := int64(200 * (len([]rune(input.text)) - 1))
				if offset.name == "Zero" {
					want = 0
				}
				result := simdBenchmarkCall(b, context.Background(), script, "run", args, CallOptions{})
				if result.Kind() != KindInt || result.Int() != want {
					b.Fatalf("rindex loop = %v, want %d", result, want)
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
}
