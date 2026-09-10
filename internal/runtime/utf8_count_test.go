package runtime

import (
	"testing"
	"unicode/utf8"
)

func TestStringRuneCountAllScalars(t *testing.T) {
	for r := rune(0); r <= utf8.MaxRune; r++ {
		if !utf8.ValidRune(r) {
			continue
		}
		text := string(r)
		for end := range len(text) + 1 {
			for _, sample := range []string{text[:end], "a\xff" + text[:end] + "\x80z"} {
				if got, want := stringRuneCount(sample), utf8.RuneCountInString(sample); got != want {
					t.Fatalf("%x: count %d, want %d", sample, got, want)
				}
			}
		}
	}
}

func TestStringRuneCountInvalidWidths(t *testing.T) {
	for first := range 256 {
		for second := range 256 {
			for _, tail := range []string{"", "\x80", "\xbf", "\x80\x80", "\xbf\xbf", "a\x80", "\x80z"} {
				text := string([]byte{byte(first), byte(second)}) + tail
				if got, want := stringRuneCount(text), utf8.RuneCountInString(text); got != want {
					t.Fatalf("%x: count %d, want %d", text, got, want)
				}
			}
		}
	}
}

func FuzzStringRuneCount(f *testing.F) {
	for _, text := range []string{"", "ascii", "é界aZ😀", "\xff\x80", "\xed\xa0\x80", "\xf4\x90\x80\x80", "\xe0\x80\x80", "\xf0\x80\x80\x80"} {
		f.Add(text)
	}
	f.Fuzz(func(t *testing.T, text string) {
		if got, want := stringRuneCount(text), utf8.RuneCountInString(text); got != want {
			t.Fatalf("%x: count %d, want %d", text, got, want)
		}
	})
}
