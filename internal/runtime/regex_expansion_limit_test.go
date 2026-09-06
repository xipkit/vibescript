package runtime

import (
	"bytes"
	"errors"
	"regexp"
	"runtime"
	"strings"
	"testing"
)

func TestRegexReplacementBoundsTemporaryAllocation(t *testing.T) {
	text := NewString(strings.Repeat("x", 64<<10))
	pattern := NewString("(.*)")
	replacement := NewString(strings.Repeat("${1}", 256))
	if _, err := compileCachedRegex(pattern.String()); err != nil {
		t.Fatal(err)
	}
	for _, all := range []bool{false, true} {
		runtime.GC()
		var before, after runtime.MemStats
		runtime.ReadMemStats(&before)
		_, err := builtinRegexReplaceValues(nil, text, pattern, replacement, all)
		runtime.ReadMemStats(&after)
		if err == nil {
			t.Fatalf("replaceAll=%v returned no output-limit error", all)
		}
		if allocated := after.TotalAlloc - before.TotalAlloc; allocated > 8<<20 {
			t.Errorf("replaceAll=%v allocated %d bytes before rejection, want <=8 MiB", all, allocated)
		} else {
			t.Logf("replaceAll=%v allocated %d bytes before rejection", all, allocated)
		}
	}
}

func TestRegexReplacementMatchesGo(t *testing.T) {
	t.Parallel()
	patterns := []string{
		`(?P<word>a+)(b)?`,
		`(?P<x>a)|(?P<x>b)`,
		`(?P<01>a)(?P<1000000000>b)?`,
		`(a)(b)?()`,
	}
	templates := []string{
		"", "$", "$$", "$$$1", "$0", "${0}", "$1$2$3", "$99",
		"$1x", "${1}x", "$word", "${word}", "$x", "${x}", "$missing",
		"$01", "${01}", "$000000000000", "$1000000000", "${1000000000}",
		"${}", "${word", "${word$1}", "$-", "$é", "$١", "$1_", "${_}",
		"\\$1", "before ${0} after", "$\xff", "${a\xff}$1", "$999999999",
	}
	for _, pattern := range patterns {
		re := regexp.MustCompile(pattern)
		for _, text := range []string{"a", "b", "aab"} {
			loc := re.FindStringSubmatchIndex(text)
			if loc == nil {
				continue
			}
			for _, template := range templates {
				want := re.ExpandString([]byte("prefix:"), template, text, loc)
				got, err := appendRegexReplacement(nil, []byte("prefix:"), re, template, text, loc)
				if err != nil || !bytes.Equal(got, want) {
					t.Errorf("pattern=%q text=%q template=%q: got %q, %v; want %q", pattern, text, template, got, err, want)
				}
			}
		}
	}
}

func TestRegexReplacementChecksBeforeAppend(t *testing.T) {
	t.Parallel()
	re := regexp.MustCompile(`(?P<word>abc)`)
	loc := re.FindStringSubmatchIndex("abc")
	for _, template := range []string{"xyz", "$0", "${1}", "$word"} {
		backing := bytes.Repeat([]byte{'?'}, maxRegexInputBytes+8)
		remaining := backing[maxRegexInputBytes-1:]
		before := bytes.Clone(remaining)
		_, err := appendRegexReplacement(nil, backing[:maxRegexInputBytes-1], re, template, "abc", loc)
		if !errors.Is(err, errRegexOutputLimit) {
			t.Errorf("template=%q error=%v, want output limit", template, err)
		}
		if !bytes.Equal(remaining, before) {
			t.Errorf("template=%q wrote rejected output into spare capacity", template)
		}
	}
}

func TestRegexReplacementOutputBoundary(t *testing.T) {
	t.Parallel()
	for _, all := range []bool{false, true} {
		for _, excess := range []int{0, 1} {
			text := "before" + strings.Repeat("x", maxRegexInputBytes-13+excess) + "after!"
			got, err := builtinRegexReplaceValues(nil, NewString(text), NewString("(x+)"), NewString("${1}y"), all)
			if excess == 0 {
				want := text[:len(text)-6] + "yafter!"
				if err != nil || got.String() != want {
					t.Errorf("replaceAll=%v exact-cap result length=%d error=%v, want %d", all, len(got.String()), err, len(want))
				}
			} else if err == nil {
				t.Errorf("replaceAll=%v accepted output above cap", all)
			}
		}
	}
}

func FuzzRegexReplacementMatchesGo(f *testing.F) {
	for _, template := range []string{"$0", "${word}", "$$$1", "${word$2}", "$01", "$é", "\\$2"} {
		f.Add("aab", template)
	}
	re := regexp.MustCompile(`(?P<word>a+)|(b?)`)
	f.Fuzz(func(t *testing.T, text, template string) {
		if len(text) > 32 || len(template) > 128 {
			t.Skip()
		}
		for _, loc := range re.FindAllStringSubmatchIndex(text, -1) {
			want := re.ExpandString(nil, template, text, loc)
			got, err := appendRegexReplacement(nil, nil, re, template, text, loc)
			if err != nil || !bytes.Equal(got, want) {
				t.Fatalf("text=%q template=%q: got %q, %v; want %q", text, template, got, err, want)
			}
		}
	})
}

func TestRegexReplacementLimitDispatch(t *testing.T) {
	t.Parallel()
	script := compileScript(t, `
def single(text, replacement)
  Regex.replace(text, "(x+)", replacement)
end
def direct(text, replacement)
  Regex.replace_all(text, "(x+)", replacement)
end
def indirect(text, replacement)
  args = [text, "(x+)", replacement]
  Regex.replace_all(*args)
end`)
	text := NewString(strings.Repeat("x", 64<<10))
	replacement := NewString(strings.Repeat("${1}", 17))
	for _, name := range []string{"single", "direct", "indirect"} {
		requireCallRuntimeErrorType(t, script, name, []Value{text, replacement}, CallOptions{}, runtimeErrorTypeLimit)
	}
}
