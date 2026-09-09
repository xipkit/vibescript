package runtime

import (
	"fmt"
	"strings"
	"testing"
)

var asciiScanBenchmarkResult bool

func BenchmarkStringASCIIShortCalls(b *testing.B) {
	for _, operation := range []struct {
		name string
		expr string
	}{
		{name: "Length", expr: "text.length"},
		{name: "Index", expr: `text.index("a")`},
		{name: "RIndex", expr: `text.rindex("a")`},
		{name: "Slice", expr: "text.slice(0, 4).bytesize"},
	} {
		for _, input := range []struct {
			name string
			text string
		}{
			{name: "ASCII", text: "abcdabcd"},
			{name: "Unicode", text: "éa😀z"},
			{name: "Invalid", text: "a\xffbcabcd"},
		} {
			b.Run(operation.name+"/"+input.name, func(b *testing.B) {
				source := "def run(text, n)\n  total = 0\n  for i in 1..n\n    total = total + " + operation.expr + "\n  end\n  total\nend"
				simdBenchmarkStringHelperLoop(b, source, []Value{NewString(input.text), NewInt(200)})
			})
		}
	}
}

func BenchmarkStringASCIIClassification(b *testing.B) {
	for _, size := range []int{0, 8, 16, 32, 4096, 65536} {
		text := strings.Repeat("a", size)
		b.Run(fmt.Sprintf("ASCII/%d", size), func(b *testing.B) {
			b.ReportAllocs()
			for range b.N {
				asciiScanBenchmarkResult = stringIsASCII(text)
			}
		})
		if size == 0 {
			continue
		}
		for _, position := range []int{0, size / 2, size - 1} {
			mixed := text[:position] + "\xff" + text[position+1:]
			b.Run(fmt.Sprintf("HighByte/%d/%d", size, position), func(b *testing.B) {
				b.ReportAllocs()
				for range b.N {
					asciiScanBenchmarkResult = stringIsASCII(mixed)
				}
			})
		}
	}
}
