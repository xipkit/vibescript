package runtime

import (
	"math"
	"math/rand/v2"
	"regexp"
	"strings"
	"testing"
)

func TestRegexpQuotedSpans(t *testing.T) {
	t.Parallel()
	check := func(t *testing.T, text string) {
		t.Helper()
		want := regexp.QuoteMeta(text)
		for _, limit := range []int{-1, 0, len(text) - 1, len(text), len(want) - 1, len(want), len(want) + 1, math.MaxInt} {
			got, ok := regexpQuotedSize(text, limit)
			if wantOK := len(want) <= limit; ok != wantOK || ok && got != len(want) || !ok && got != 0 {
				t.Fatalf("size(%q, %d) = (%d, %t), want (%d, %t)", text, limit, got, ok, len(want), wantOK)
			}
		}
		for _, prefix := range []string{"", "existing output:"} {
			var out strings.Builder
			out.Grow(len(prefix) + len(want))
			out.WriteString(prefix)
			writeRegexpQuoted(&out, text)
			if got := out.String(); got != prefix+want {
				t.Fatalf("quote(%q) after %q = %q, want %q", text, prefix, got, prefix+want)
			}
		}
	}
	for _, n := range []int{0, 1, 15, 16, 17, 31, 32, 33, 63, 64, 65, 127, 128, 129, 4096} {
		for _, pattern := range []string{"a", `\.+*?()|[]{}^$`, "é日本語", "\x00\xff\xc0\x80", "abc\\"} {
			text := strings.Repeat(pattern, n/len(pattern)+1)[:n]
			check(t, text)
			check(t, strings.Repeat("a", 64)+"."+text)
		}
	}
	for b := range 256 {
		check(t, strings.Repeat(string(byte(b)), 257))
		for pos := range 33 {
			text := []byte(strings.Repeat("a", 65))
			text[pos] = byte(b)
			check(t, string(text))
		}
	}
	rng := rand.New(rand.NewPCG(39, 71))
	for range 1000 {
		text := make([]byte, rng.IntN(2049))
		for i := range text {
			text[i] = byte(rng.Uint32())
		}
		check(t, string(text))
	}
}

func TestRegexpQuotedSpanAllocations(t *testing.T) {
	for _, text := range []string{strings.Repeat("plain", 820), strings.Repeat("a.\\é\xff", 700), strings.Repeat(`\.+*?()|[]{}^$`, 300)} {
		want := regexp.QuoteMeta(text)
		if got := testing.AllocsPerRun(20, func() {
			if size, ok := regexpQuotedSize(text, math.MaxInt); !ok || size != len(want) {
				t.Fatal("size differs in allocation check")
			}
		}); got != 0 {
			t.Fatalf("size allocations for %d bytes = %v, want 0", len(text), got)
		}
		if got := testing.AllocsPerRun(20, func() {
			var out strings.Builder
			out.Grow(len(want))
			writeRegexpQuoted(&out, text)
			if out.String() != want {
				t.Fatal("quote differs in allocation check")
			}
		}); got != 1 {
			t.Fatalf("quote allocations for %d bytes = %v, want one output buffer", len(text), got)
		}
	}
}
