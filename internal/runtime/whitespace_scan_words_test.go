package runtime

import (
	"encoding/binary"
	"math/rand/v2"
	"strings"
	"testing"
)

func TestWhitespaceByteBits(t *testing.T) {
	for first := range 256 {
		for second := range 256 {
			word := uint64(first)*0x0100010001000100 | uint64(second)*0x0001000100010001
			var bytes [8]byte
			binary.LittleEndian.PutUint64(bytes[:], word)
			for _, strip := range []bool{false, true} {
				var want uint64
				for i, b := range bytes {
					if referenceWhitespace(b, strip) {
						want |= uint64(0x80) << (8 * i)
					}
				}
				if got := whitespaceByteBits(word, strip); got != want {
					t.Fatalf("word %016x, strip %t: mask %016x, want %016x", word, strip, got, want)
				}
			}
		}
	}
}

func TestWhitespaceWordsMatchReference(t *testing.T) {
	rng := rand.New(rand.NewPCG(41, 97))
	for size := range 1025 {
		bytes := make([]byte, size)
		for i := range bytes {
			bytes[i] = byte(rng.Uint32())
		}
		for _, text := range []string{string(bytes), strings.Repeat(" \t\n\r\v\f\x00", size), strings.Repeat("aé\xff", size)} {
			for _, strip := range []bool{false, true} {
				prefix, suffix := 0, 0
				for prefix < len(text) && referenceWhitespace(text[prefix], strip) {
					prefix++
				}
				for suffix < len(text) && referenceWhitespace(text[len(text)-1-suffix], strip) {
					suffix++
				}
				if got := whitespacePrefixWords(text, strip); got != prefix {
					t.Fatalf("size %d, strip %t: prefix %d, want %d", len(text), strip, got, prefix)
				}
				if got := whitespaceSuffixWords(text, strip); got != suffix {
					t.Fatalf("size %d, strip %t: suffix %d, want %d", len(text), strip, got, suffix)
				}
			}
			end := 0
			for end < len(text) && !referenceWhitespace(text[end], false) {
				end++
			}
			if got := nonWhitespacePrefixWords(text); got != end {
				t.Fatalf("size %d: nonspace prefix %d, want %d", len(text), got, end)
			}
		}
	}
}
