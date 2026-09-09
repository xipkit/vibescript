package runtime

import (
	"math/rand/v2"
	"strings"
	"testing"
)

func referenceASCIICaseCompare(a, b string) int {
	fold := func(c byte) byte {
		if c >= 'A' && c <= 'Z' {
			return c + 'a' - 'A'
		}
		return c
	}
	for i := range min(len(a), len(b)) {
		ca, cb := fold(a[i]), fold(b[i])
		if ca < cb {
			return -1
		}
		if ca > cb {
			return 1
		}
	}
	switch {
	case len(a) < len(b):
		return -1
	case len(a) > len(b):
		return 1
	default:
		return 0
	}
}

func checkASCIICaseComparison(t *testing.T, a, b string) {
	t.Helper()
	want := referenceASCIICaseCompare(a, b)
	for _, implementation := range []struct {
		name    string
		compare func(string, string) int
		equal   func(string, string) bool
	}{
		{name: "bytes", compare: asciiCaseCompareBytes, equal: asciiCaseEqualBytes},
		{name: "words", compare: asciiCaseCompareWords, equal: asciiCaseEqualWords},
		{name: "selected", compare: asciiCaseCompare, equal: asciiCaseEqual},
	} {
		if got := implementation.compare(a, b); got != want {
			t.Errorf("%s compare(%q, %q) = %d, want %d", implementation.name, a, b, got, want)
		}
		if got := implementation.equal(a, b); got != (want == 0) {
			t.Errorf("%s equal(%q, %q) = %t, want %t", implementation.name, a, b, got, want == 0)
		}
	}
}

func TestASCIICaseComparisonAllBytes(t *testing.T) {
	t.Parallel()
	for a := range 256 {
		for b := range 256 {
			left := strings.Repeat(string([]byte{byte(a)}), 48)
			right := strings.Repeat(string([]byte{byte(b)}), 48)
			checkASCIICaseComparison(t, left, right)
			checkASCIICaseComparison(t, "AaBb"+left, "aAbB"+right)
			var alternating [32]byte
			for i := range alternating {
				if i%2 == 0 {
					alternating[i] = byte(a)
				} else {
					alternating[i] = byte(b)
				}
			}
			folded := []byte(string(alternating[:]))
			for i, c := range folded {
				if c >= 'A' && c <= 'Z' {
					folded[i] += 'a' - 'A'
				}
			}
			checkASCIICaseComparison(t, string(alternating[:]), string(folded))
		}
	}
}

func TestASCIICaseComparisonBoundaries(t *testing.T) {
	t.Parallel()
	for _, n := range []int{0, 1, 3, 4, 7, 8, 15, 16, 17, 19, 20, 21, 31, 32, 33, 35, 36, 37, 63, 64, 65, 128, 257} {
		a, b := strings.Repeat("A", n), strings.Repeat("a", n)
		checkASCIICaseComparison(t, a, b)
		checkASCIICaseComparison(t, a, b+"a")
		checkASCIICaseComparison(t, a+"a", b)
		for pos := range n {
			for _, mismatch := range []byte{0, 'Z', '[', '\\', ']', '^', '_', '`', '{', 127, 128, 255} {
				changed := []byte(b)
				changed[pos] = mismatch
				checkASCIICaseComparison(t, a, string(changed))
				checkASCIICaseComparison(t, string(changed), a)
			}
		}
	}
}

func TestASCIICaseComparisonRandom(t *testing.T) {
	t.Parallel()
	rng := rand.New(rand.NewPCG(42, 73))
	for range 2000 {
		n := rng.IntN(1025)
		a, b := make([]byte, n), make([]byte, n)
		for i := range n {
			a[i] = byte(rng.Uint32())
			b[i] = a[i]
			if b[i] >= 'A' && b[i] <= 'Z' {
				b[i] += 'a' - 'A'
			}
		}
		checkASCIICaseComparison(t, string(a), string(b))
		if n != 0 {
			b[rng.IntN(n)] = byte(rng.Uint32())
			checkASCIICaseComparison(t, string(a), string(b))
		}
	}
}

func TestASCIICaseComparisonAllocations(t *testing.T) {
	a := strings.Repeat("AbC[]_\xff", 512)
	b := strings.Repeat("aBc[]_\xff", 512)
	for _, implementation := range []struct {
		name    string
		compare func(string, string) int
		equal   func(string, string) bool
	}{
		{name: "bytes", compare: asciiCaseCompareBytes, equal: asciiCaseEqualBytes},
		{name: "words", compare: asciiCaseCompareWords, equal: asciiCaseEqualWords},
		{name: "selected", compare: asciiCaseCompare, equal: asciiCaseEqual},
	} {
		t.Run(implementation.name, func(t *testing.T) {
			allocs := testing.AllocsPerRun(100, func() {
				if implementation.compare(a, b) != 0 || !implementation.equal(a, b) {
					t.Error("case comparison differs for equal folded strings")
				}
			})
			if allocs != 0 {
				t.Errorf("%s comparison allocations = %v, want 0", implementation.name, allocs)
			}
		})
	}
}

func FuzzASCIICaseComparison(f *testing.F) {
	for _, seed := range [][2]string{
		{"", ""},
		{"A", "["},
		{"\xff", "\xfe"},
		{"é界", "É界"},
		{strings.Repeat("aB_C", 65), strings.Repeat("Ab_c", 65)},
		{strings.Repeat("A", 31) + "\xff", strings.Repeat("a", 31) + "\xfe"},
	} {
		f.Add(seed[0], seed[1])
	}
	f.Fuzz(func(t *testing.T, a, b string) {
		if len(a) > 4096 || len(b) > 4096 {
			t.Skip()
		}
		checkASCIICaseComparison(t, a, b)
	})
}
