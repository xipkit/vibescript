package runtime

import (
	"math/rand/v2"
	"strings"
	"testing"
)

func TestJSONASCIISpans(t *testing.T) {
	for _, tc := range []struct {
		name string
		scan func(string) int
		stop func(byte) bool
	}{
		{name: "parse", scan: jsonParseASCIISpan, stop: func(c byte) bool {
			return c < 0x20 || c >= 0x80 || c == '"' || c == '\\'
		}},
		{name: "stringify", scan: jsonStringifyASCIISpan, stop: func(c byte) bool {
			return c < 0x20 || c >= 0x80 || c == '"' || c == '\\' || c == '<' || c == '>' || c == '&'
		}},
	} {
		t.Run(tc.name, func(t *testing.T) {
			check := func(text string) {
				t.Helper()
				want := len(text)
				for i := range len(text) {
					if tc.stop(text[i]) {
						want = i
						break
					}
				}
				if got := tc.scan(text); got != want {
					t.Errorf("%s span(%q) = %d, want %d", tc.name, text, got, want)
				}
			}
			for n := range 258 {
				check(strings.Repeat("a", n))
			}
			for _, n := range []int{1, 15, 16, 17, 31, 32, 33, 47, 48, 49, 63, 64, 65, 127, 128, 129} {
				for pos := range n {
					text := []byte(strings.Repeat("a", n))
					for value := range 256 {
						text[pos] = byte(value)
						check(string(text))
					}
				}
			}
			rng := rand.New(rand.NewPCG(42, 17))
			for range 1000 {
				text := make([]byte, rng.IntN(1025))
				for i := range text {
					text[i] = byte(rng.Uint32())
				}
				check(string(text))
			}
			text := strings.Repeat("a", 16*1024)
			result := 0
			allocs := testing.AllocsPerRun(100, func() { result = tc.scan(text) })
			if result != len(text) || allocs != 0 {
				t.Errorf("%s span(16 KiB ASCII) = %d with %v allocations, want %d with 0 allocations", tc.name, result, allocs, len(text))
			}
		})
	}
}
