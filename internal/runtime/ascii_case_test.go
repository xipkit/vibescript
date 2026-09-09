package runtime

import (
	"encoding/binary"
	"math/rand/v2"
	"strings"
	"testing"
	"unsafe"
)

func scalarASCIICase(text, method string) string {
	out := []byte(text)
	for i, c := range out {
		switch {
		case c >= 'a' && c <= 'z' && (method == "upcase" || method == "swapcase" || method == "capitalize" && i == 0):
			out[i] = c - 32
		case c >= 'A' && c <= 'Z' && (method == "downcase" || method == "swapcase" || method == "capitalize" && i > 0):
			out[i] = c + 32
		}
	}
	return string(out)
}

func checkASCIICaseTransforms(t *testing.T, text string) {
	t.Helper()
	for _, tc := range []struct {
		name      string
		transform func(string) string
	}{
		{name: "upcase", transform: asciiUpcase},
		{name: "downcase", transform: asciiDowncase},
		{name: "swapcase", transform: asciiSwapCase},
		{name: "capitalize", transform: asciiCapitalize},
	} {
		if text == "" && tc.name == "capitalize" {
			continue
		}
		want := scalarASCIICase(text, tc.name)
		if got := tc.transform(text); got != want {
			t.Errorf("%s(%q) = %q, want %q", tc.name, text, got, want)
		}
	}
}

func TestASCIICaseEveryByteAndBoundary(t *testing.T) {
	t.Parallel()
	for _, size := range []int{0, 1, 7, 8, 9, 15, 16, 17, 31, 32, 33, 63, 64, 65} {
		text := []byte(strings.Repeat("aZ[{@`9_", (size+7)/8)[:size])
		checkASCIICaseTransforms(t, string(text))
		for value := range 256 {
			for position := range text {
				old := text[position]
				text[position] = byte(value)
				checkASCIICaseTransforms(t, string(text))
				text[position] = old
			}
		}
	}
}

func TestASCIICaseWordNeighbors(t *testing.T) {
	t.Parallel()
	for first := range 256 {
		for second := range 256 {
			var text, want [8]byte
			for i := range text {
				if i%2 == 0 {
					text[i] = byte(first)
				} else {
					text[i] = byte(second)
				}
				if c := text[i]; c >= 'A' && c <= 'Z' || c >= 'a' && c <= 'z' {
					want[i] = 32
				}
			}
			word := binary.LittleEndian.Uint64(text[:])
			if got := asciiLetterCaseBits(word); got != binary.LittleEndian.Uint64(want[:]) {
				t.Errorf("asciiLetterCaseBits(%#x) = %#x, want %#x", word, got, binary.LittleEndian.Uint64(want[:]))
			}
		}
	}
}

func TestASCIICaseRandomAndUnicode(t *testing.T) {
	t.Parallel()
	for _, text := range []string{
		"", "a", "Z", "@AZ[az{", "Straße İΣ😀 AbCd", "éÉ\x00ABC xyz",
		strings.Repeat("abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ", 128),
		strings.Repeat("[]{}@`0123456789", 256),
		strings.Repeat("éΣ😀Az", 512),
		strings.Repeat("\x80\xffA\xc0z\xfe", 512),
	} {
		checkASCIICaseTransforms(t, text)
	}
	random := rand.New(rand.NewPCG(121, 727))
	for range 500 {
		text := make([]byte, random.IntN(4097))
		for i := range text {
			text[i] = byte(random.Uint32())
		}
		checkASCIICaseTransforms(t, string(text))
	}
}

func TestASCIICaseLargeBoundary(t *testing.T) {
	t.Parallel()
	var alphabet [256]byte
	for i := range alphabet {
		alphabet[i] = byte(i)
	}
	text := strings.Repeat(string(alphabet[:]), 65)
	for _, size := range []int{16383, 16384, 16385} {
		checkASCIICaseTransforms(t, text[:size])
	}
}

func TestASCIICaseDetachesUnchangedInput(t *testing.T) {
	t.Parallel()
	backing := strings.Repeat("1234_[]{}", 16*1024)
	text := backing[4096 : 4096+1024]
	for _, transform := range []func(string) string{asciiUpcase, asciiDowncase, asciiSwapCase, asciiCapitalize} {
		got := transform(text)
		if got != text {
			t.Errorf("ASCII case transform(%q) = %q, want unchanged input", text, got)
		}
		if unsafe.StringData(got) == unsafe.StringData(text) {
			t.Error("ASCII case transform retained the large source allocation")
		}
	}
}

var asciiCaseAllocationSink string

func TestASCIICaseAllocations(t *testing.T) {
	for _, size := range []int{0, 1, 8, 16, 31, 32, 33, 64, 4096} {
		text := strings.Repeat("aZ!9", (size+3)/4)[:size]
		for _, tc := range []struct {
			name      string
			transform func(string) string
		}{
			{name: "upcase", transform: asciiUpcase},
			{name: "downcase", transform: asciiDowncase},
			{name: "swapcase", transform: asciiSwapCase},
			{name: "capitalize", transform: asciiCapitalize},
		} {
			if size == 0 && tc.name == "capitalize" {
				continue
			}
			want := testing.AllocsPerRun(100, func() {
				asciiCaseAllocationSink = scalarASCIICase(text, tc.name)
			})
			got := testing.AllocsPerRun(100, func() {
				asciiCaseAllocationSink = tc.transform(text)
			})
			if got != want {
				t.Errorf("%s(%q) allocated %g times, want %g", tc.name, text, got, want)
			}
		}
	}
}
