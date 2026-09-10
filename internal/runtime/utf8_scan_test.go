package runtime

import (
	"encoding/binary"
	"slices"
	"strings"
	"testing"
	"unicode/utf8"
)

func TestUTF8ContinuationBits(t *testing.T) {
	t.Parallel()
	for first := range 256 {
		for second := range 256 {
			var input, want [8]byte
			for i := range input {
				input[i] = byte(first)
				if i%2 != 0 {
					input[i] = byte(second)
				}
				if input[i]&0xc0 == 0x80 {
					want[i] = 0x80
				}
			}
			word := binary.LittleEndian.Uint64(input[:])
			if got := utf8ContinuationBits(word); got != binary.LittleEndian.Uint64(want[:]) {
				t.Fatalf("UTF-8 continuation mask(%x) = %x, want %x", input, got, want)
			}
		}
	}
}

func checkValidUTF8Scan(t *testing.T, text string) {
	t.Helper()
	if got, want := validUTF8RuneCount(text), utf8.RuneCountInString(text); got != want {
		t.Fatalf("valid UTF-8 rune count(%q) = %d, want %d", text, got, want)
	}
	offset := 0
	for index := range text {
		if got, ok := validUTF8ByteIndex(text, offset); !ok || got != index {
			t.Fatalf("valid UTF-8 byte index(%q, %d) = %d/%t, want %d/true", text, offset, got, ok, index)
		}
		offset++
	}
	if got, ok := validUTF8ByteIndex(text, offset); !ok || got != len(text) {
		t.Fatalf("valid UTF-8 end offset(%q, %d) = %d/%t, want %d/true", text, offset, got, ok, len(text))
	}
	for _, outside := range []int{-1, offset + 1, int(^uint(0) >> 1)} {
		if got, ok := validUTF8ByteIndex(text, outside); ok {
			t.Errorf("valid UTF-8 out-of-range offset(%q, %d) = %d/true, want false", text, outside, got)
		}
	}
}

func TestValidUTF8ScanBoundaries(t *testing.T) {
	t.Parallel()
	for _, pattern := range []string{"", "a", "é", "終", "😀", "aé終😀\uFFFD\x00"} {
		for prefix := range 16 {
			for repeats := range 20 {
				text := strings.Repeat("x", prefix) + strings.Repeat(pattern, repeats)
				checkValidUTF8Scan(t, text)
			}
		}
	}
}

func FuzzValidUTF8Scan(f *testing.F) {
	for _, text := range []string{"", "abcdefgh", "aé終😀", "\xff\x80", strings.Repeat("héllo ", 100)} {
		f.Add(text)
	}
	f.Fuzz(func(t *testing.T, text string) {
		if len(text) > 1024 {
			t.Skip()
		}
		checkValidUTF8Scan(t, string([]rune(text)))
	})
}

func TestStringSearchPreservesRuneOffsets(t *testing.T) {
	t.Parallel()
	for _, text := range []string{"", "a", "aé終😀aé", strings.Repeat("abé終😀", 16), "a\xffé\x80😀b\uFFFD"} {
		for _, needle := range []string{"", "a", "é", "終😀", "😀b", "\uFFFD", "\xfe", "missing"} {
			offsets := []int{len(text), len(text) + 1, int(^uint(0) >> 1)}
			for i := range utf8.RuneCountInString(text) + 3 {
				offsets = append(offsets, i-1)
			}
			for _, offset := range offsets {
				for _, reverse := range []bool{false, true} {
					want := referenceRuneSearch(text, needle, offset, reverse)
					search := stringRuneIndex
					if reverse {
						search = stringRuneRIndex
					}
					if got, err := search(nil, text, needle, offset); err != nil || got != want {
						t.Fatalf("search(%q, %q, %d, reverse=%t) = %d/%v, want %d/nil", text, needle, offset, reverse, got, err, want)
					}
				}
			}
		}
	}
}

func referenceRuneSearch(text, needle string, offset int, reverse bool) int {
	haystack, sought := []rune(text), []rune(needle)
	if offset < 0 || !reverse && offset > len(haystack) {
		return -1
	}
	if reverse {
		offset = min(offset, len(haystack))
	}
	result := -1
	for i := range len(haystack) + 1 {
		if reverse && i > offset || !reverse && i < offset || i+len(sought) > len(haystack) {
			continue
		}
		if slices.Equal(haystack[i:i+len(sought)], sought) {
			result = i
			if !reverse {
				break
			}
		}
	}
	return result
}

func TestValidUTF8ScanAllocations(t *testing.T) {
	text := strings.Repeat("aé終😀", 410)
	if got := testing.AllocsPerRun(50, func() {
		if validUTF8RuneCount(text) != 1640 {
			t.Fatal("valid UTF-8 count changed during allocation check")
		}
		if _, ok := validUTF8ByteIndex(text, 1000); !ok {
			t.Fatal("valid UTF-8 byte index failed during allocation check")
		}
	}); got != 0 {
		t.Errorf("valid UTF-8 scans allocated %g times, want zero", got)
	}
}
