package runtime

import (
	"context"
	"strings"
	"sync"
	"testing"
	"unicode/utf8"
	"unsafe"
)

func referenceUnicodeSwapCase(text string) string {
	var out strings.Builder
	out.Grow(len(text))
	for _, r := range text {
		switch {
		case isUppercaseLike(r):
			out.WriteString(unicodeDowncase(string(r)))
		case isLowercaseLike(r):
			out.WriteString(unicodeUpcase(string(r)))
		default:
			out.WriteRune(r)
		}
	}
	return out.String()
}

func TestStringSwapCaseEveryUnicodeScalar(t *testing.T) {
	t.Parallel()
	var input strings.Builder
	for r := range utf8.MaxRune + 1 {
		if utf8.ValidRune(rune(r)) {
			input.WriteRune(rune(r))
		}
	}
	text := input.String()
	want := referenceUnicodeSwapCase(text)
	got := stringSwapCase(text, caseModeDefault)
	if got != want {
		for i := range min(len(got), len(want)) {
			if got[i] != want[i] {
				t.Fatalf("swapcase(all Unicode scalars) differs at byte %d: got %q, want %q", i, got[i:min(i+32, len(got))], want[i:min(i+32, len(want))])
			}
		}
		t.Fatalf("swapcase(all Unicode scalars) length = %d, want %d", len(got), len(want))
	}
}

func TestStringSwapCaseIndependentRuneMappings(t *testing.T) {
	t.Parallel()
	for _, text := range []string{
		"", "aZ", "İiıIΣςσ", "ΟΣ ΟΣΑ ΣΟΣ", "ßẞﬁﬃǅǆᾀᾈ", "ⒶⓐⅠⅰͅ",
		"i\u0307j\u0301Σ\u0301A", "I\u0307I\u0301i\u0301", "😀終\x00",
		strings.Repeat("İiΣςßẞﬁǅ\u0345éΣ😀aZ", 410),
	} {
		want := referenceUnicodeSwapCase(text)
		if got := stringSwapCase(text, caseModeDefault); got != want {
			t.Errorf("swapcase(%q) = %q, want %q", text, got, want)
		}
	}
	for b := range 256 {
		text := "éΣ😀aZ" + string([]byte{byte(b)}) + "ßİxY"
		want := scalarASCIICase(text, "swapcase")
		if got := stringSwapCase(text, caseModeASCII); got != want {
			t.Errorf("swapcase(%q, :ascii) = %q, want %q", text, got, want)
		}
		if !utf8.ValidString(text) {
			if got := stringSwapCase(text, caseModeDefault); got != want {
				t.Errorf("swapcase(invalid %q) = %q, want %q", text, got, want)
			}
		}
	}
}

func TestStringSwapCaseAllocationsDoNotScaleWithCasedRunes(t *testing.T) {
	for _, text := range []string{"", "😀終123", strings.Repeat("123_😀終", 410)} {
		got := testing.AllocsPerRun(50, func() {
			asciiCaseAllocationSink = stringSwapCase(text, caseModeDefault)
		})
		want := testing.AllocsPerRun(50, func() {
			asciiCaseAllocationSink = referenceUnicodeSwapCase(text)
		})
		if got > want {
			t.Errorf("swapcase(uncased, %d bytes) allocated %g times, want at most %g", len(text), got, want)
		}
	}
	counts := make([]float64, 2)
	for i, repeats := range []int{8, 410} {
		text := strings.Repeat("éΣ😀aZ", repeats)
		counts[i] = testing.AllocsPerRun(50, func() {
			asciiCaseAllocationSink = stringSwapCase(text, caseModeDefault)
		})
	}
	if counts[1] > counts[0]+4 {
		t.Errorf("swapcase allocations grew from %g to %g for 8/410 repetitions; temporary buffers must be reused", counts[0], counts[1])
	}
}

func TestStringSwapCaseOwnsOutput(t *testing.T) {
	t.Parallel()
	backing := strings.Repeat("😀終123", 16*1024)
	text := backing[0:1024]
	got := stringSwapCase(text, caseModeDefault)
	if got != text {
		t.Fatalf("swapcase(uncased text) = %q, want %q", got, text)
	}
	if unsafe.StringData(got) == unsafe.StringData(text) {
		t.Error("swapcase retained the large source allocation")
	}
	stringSwapCase(strings.Repeat("İßΣ", 512), caseModeDefault)
	if got != text {
		t.Error("another swapcase call overwrote the previous result")
	}
}

func TestStringSwapCaseConcurrentCalls(t *testing.T) {
	t.Parallel()
	script := compileScript(t, "def run(text) text.swapcase end")
	var workers sync.WaitGroup
	for i := range 8 {
		workers.Go(func() {
			text := strings.Repeat("İiΣςßẞﬁǅ\u0345éΣ😀aZ", i+1)
			want := referenceUnicodeSwapCase(text)
			for range 20 {
				got, err := script.Call(context.Background(), "run", []Value{NewString(text)}, CallOptions{})
				if err != nil {
					t.Errorf("concurrent swapcase(%q): %v", text, err)
					return
				}
				if got.String() != want {
					t.Errorf("concurrent swapcase(%q) = %q, want %q", text, got.String(), want)
					return
				}
			}
		})
	}
	workers.Wait()
}
