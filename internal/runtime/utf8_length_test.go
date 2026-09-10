package runtime

import (
	"strings"
	"testing"
	"unicode/utf8"
)

func TestStringRuneLenValidatedCount(t *testing.T) {
	for first := range 256 {
		for second := range 256 {
			for _, size := range []int{0, 63, 64, 65, 127} {
				tail := string([]byte{byte(first), byte(second)}) + "é日本🙂"
				text := strings.Repeat("a", max(0, size-len(tail))) + tail
				if got, want := stringRuneLen(text), utf8.RuneCountInString(text); got != want {
					t.Fatalf("size %d, bytes %x %x: length %d, want %d", len(text), first, second, got, want)
				}
			}
		}
	}
}
