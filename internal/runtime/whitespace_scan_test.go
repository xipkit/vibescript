package runtime

import (
	"math/rand"
	"slices"
	"strings"
	"testing"
)

func referenceWhitespace(b byte, strip bool) bool {
	return b == ' ' || b >= 9 && b <= 13 || strip && b == 0
}

func referenceWhitespaceSplit(text string, limit int) []string {
	if text == "" {
		return nil
	}
	if limit == 1 {
		return []string{text}
	}
	var fields []string
	start := -1
	for i := range len(text) {
		if referenceWhitespace(text[i], false) {
			if start >= 0 {
				fields = append(fields, text[start:i])
				start = -1
			}
			continue
		}
		if start < 0 {
			if limit > 0 && len(fields) == limit-1 {
				return append(fields, text[i:])
			}
			start = i
		}
	}
	if start >= 0 {
		return append(fields, text[start:])
	}
	if limit != 0 {
		fields = append(fields, "")
	}
	return fields
}

func TestWhitespaceSplitLongRuns(t *testing.T) {
	for _, prefix := range []string{"", strings.Repeat(" \t", 128), strings.Repeat("a", 256), strings.Repeat("a b ", 64)} {
		for _, suffix := range []string{"", strings.Repeat("\r\n", 128), strings.Repeat("é\u2003\xff\x00", 64), strings.Repeat("a b ", 64)} {
			text := prefix + suffix
			for _, limit := range []int{-2, -1, 0, 1, 2, 3, 1024} {
				want := referenceWhitespaceSplit(text, limit)
				projection := splitOnASCIIWhitespaceLimitProjection(text, limit)
				if projection.count != len(want) {
					t.Fatalf("projection count(%q, %d) = %d, want %d", text, limit, projection.count, len(want))
				}
				payload := 0
				for _, field := range want {
					payload += estimatedStringHeaderBytes
					if len(field) != len(text) {
						payload += len(field)
					}
				}
				if projection.payload != payload {
					t.Fatalf("projection payload(%q, %d) = %d, want %d", text, limit, projection.payload, payload)
				}
				if got := splitOnASCIIWhitespaceLimit(text, limit, projection.count); !slices.Equal(got, want) {
					t.Fatalf("split(%q, %d) = %q, want %q", text, limit, got, want)
				}
			}
		}
	}
}

func checkWhitespaceSpans(t *testing.T, text string) {
	t.Helper()
	for _, strip := range []bool{false, true} {
		prefix := 0
		for prefix < len(text) && referenceWhitespace(text[prefix], strip) {
			prefix++
		}
		suffix := 0
		for suffix < len(text) && referenceWhitespace(text[len(text)-suffix-1], strip) {
			suffix++
		}
		if got := whitespacePrefixSIMD(text, strip); got != prefix {
			t.Fatalf("SIMD prefix(%q, strip=%v) = %d, want %d", text, strip, got, prefix)
		}
		if got := whitespacePrefixScalar(text, strip); got != prefix {
			t.Fatalf("Go prefix(%q, strip=%v) = %d, want %d", text, strip, got, prefix)
		}
		if got := whitespaceSuffixSIMD(text, strip); got != suffix {
			t.Fatalf("SIMD suffix(%q, strip=%v) = %d, want %d", text, strip, got, suffix)
		}
		if got := whitespaceSuffixScalar(text, strip); got != suffix {
			t.Fatalf("Go suffix(%q, strip=%v) = %d, want %d", text, strip, got, suffix)
		}
	}
	if got, want := rubyLstrip(text), text[whitespacePrefixScalar(text, true):]; got != want {
		t.Fatalf("rubyLstrip(%q) = %q, want %q", text, got, want)
	}
	if got, want := rubyRstrip(text), text[:len(text)-whitespaceSuffixScalar(text, true)]; got != want {
		t.Fatalf("rubyRstrip(%q) = %q, want %q", text, got, want)
	}
	prefix := 0
	for prefix < len(text) && !referenceWhitespace(text[prefix], false) {
		prefix++
	}
	if got := nonWhitespacePrefixSIMD(text); got != prefix {
		t.Fatalf("SIMD nonspace prefix(%q) = %d, want %d", text, got, prefix)
	}
	if got := nonWhitespacePrefixScalar(text); got != prefix {
		t.Fatalf("Go nonspace prefix(%q) = %d, want %d", text, got, prefix)
	}
}

func TestWhitespaceScanAllBytes(t *testing.T) {
	for _, n := range []int{0, 1, 7, 8, 15, 16, 17, 23, 24, 31, 32, 33, 47, 48, 63, 64, 65, 127, 128, 129, 255, 256} {
		padding := strings.Repeat(" \t\r\n\v\f", n/6+1)[:n]
		for b := range 256 {
			value := string([]byte{byte(b)})
			checkWhitespaceSpans(t, padding+value+padding)
			checkWhitespaceSpans(t, strings.Repeat("x", n)+value+padding)
			checkWhitespaceSpans(t, strings.Repeat(value, n))
		}
	}
}

func TestWhitespaceScanEveryStop(t *testing.T) {
	for n := range 161 {
		for stop := range n + 1 {
			checkWhitespaceSpans(t, strings.Repeat(" ", stop)+strings.Repeat("x", n-stop))
			checkWhitespaceSpans(t, strings.Repeat("\x00", stop)+strings.Repeat("x", n-stop))
			checkWhitespaceSpans(t, strings.Repeat("x", stop)+strings.Repeat(" ", n-stop))
		}
	}
}

func TestWhitespaceScanUnicodeAndInvalidUTF8(t *testing.T) {
	for _, text := range []string{"", "\u00a0", "\u1680", "\u2003", "\ufeff", "\xff\xfe", "a\x00b", "\u00a0 a", "a \u2003"} {
		for _, n := range []int{0, 8, 16, 24, 32, 64, 4096} {
			padding := strings.Repeat(" ", n)
			checkWhitespaceSpans(t, padding+text+padding)
		}
	}
}

func TestWhitespaceScanRandom(t *testing.T) {
	rng := rand.New(rand.NewSource(0x5ace))
	spaces := " \t\n\v\f\r\x00"
	for range 10000 {
		data := make([]byte, rng.Intn(513))
		for i := range data {
			if rng.Intn(2) == 0 {
				data[i] = spaces[rng.Intn(len(spaces))]
			} else {
				data[i] = byte(rng.Intn(256))
			}
		}
		checkWhitespaceSpans(t, string(data))
	}
}

func TestWhitespaceScanAllocations(t *testing.T) {
	spaces := strings.Repeat(" \t\r\n\v\f\x00", 1024)
	text := strings.Repeat("word\xc3\xa9", 1024)
	got := 0
	allocs := testing.AllocsPerRun(100, func() {
		got = whitespacePrefixSIMD(spaces, true) + whitespaceSuffixSIMD(spaces, true) + nonWhitespacePrefixSIMD(text)
	})
	if want := 2*len(spaces) + len(text); got != want {
		t.Fatalf("combined spans = %d, want %d", got, want)
	}
	if allocs != 0 {
		t.Fatalf("SIMD whitespace spans allocated %v times, want 0", allocs)
	}
}
