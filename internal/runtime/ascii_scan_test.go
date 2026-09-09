package runtime

import (
	"fmt"
	"strings"
	"testing"
)

func scalarASCIIReference(text string) bool {
	for i := range len(text) {
		if text[i] >= 128 {
			return false
		}
	}
	return true
}

func TestStringIsASCII(t *testing.T) {
	for _, impl := range []struct {
		name string
		fn   func(string) bool
	}{
		{name: "selected", fn: stringIsASCII},
		{name: "words", fn: stringIsASCIIWords},
	} {
		t.Run(impl.name, func(t *testing.T) {
			for n := range 130 {
				buf := []byte(strings.Repeat("a", n))
				if !impl.fn(string(buf)) {
					t.Fatalf("%s(%q) = false, want true", impl.name, buf)
				}
				for i := range n {
					for c := range 256 {
						buf[i] = byte(c)
						text := string(buf)
						if got, want := impl.fn(text), scalarASCIIReference(text); got != want {
							t.Fatalf("%s(%q) = %t, want %t", impl.name, text, got, want)
						}
					}
					buf[i] = 'a'
				}
			}
		})
	}
}

func TestStringIsASCIIAllocations(t *testing.T) {
	for _, n := range []int{0, 7, 8, 15, 16, 31, 32, 33, 4096, 1 << 20} {
		t.Run(fmt.Sprint(n), func(t *testing.T) {
			text := strings.Repeat("a", n)
			if got := testing.AllocsPerRun(20, func() {
				if !stringIsASCII(text) {
					t.Errorf("stringIsASCII(%d ASCII bytes) = false, want true", n)
				}
			}); got != 0 {
				t.Errorf("stringIsASCII(%d bytes) allocated %v objects, want zero", n, got)
			}
		})
	}
}

func FuzzStringIsASCII(f *testing.F) {
	for _, s := range []string{"", "hello", "é", "\xff", strings.Repeat("a", 65), strings.Repeat("a", 31) + "\x80"} {
		f.Add(s)
	}
	f.Fuzz(func(t *testing.T, s string) {
		if len(s) > 1<<20 {
			t.Skip()
		}
		want := scalarASCIIReference(s)
		if got := stringIsASCII(s); got != want {
			t.Fatalf("stringIsASCII(%q) = %t, want %t", s, got, want)
		}
		if got := stringIsASCIIWords(s); got != want {
			t.Fatalf("stringIsASCIIWords(%q) = %t, want %t", s, got, want)
		}
	})
}
