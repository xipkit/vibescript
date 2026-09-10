package runtime

import (
	"context"
	"strings"
	"testing"
)

func TestStringRIndexDefaultByteBound(t *testing.T) {
	script := compileScript(t, "def run(text, needle) text.rindex(needle) end")
	for _, text := range []string{"", "a", "héllo 終", "😀😀😀", "a\xff終\xfea", strings.Repeat("héllo ", 100) + "終"} {
		for _, needle := range []string{"", "a", "é", "終", "😀", "\xff", "\uFFFD", "absent"} {
			want := referenceRuneSearch(text, needle, len(text), true)
			result, err := script.Call(context.Background(), "run", []Value{NewString(text), NewString(needle)}, CallOptions{})
			if err != nil {
				t.Fatalf("rindex(%q, %q): %v", text, needle, err)
			}
			memberResult, err := callStringMemberForTest(t, nil, NewString(text), "rindex", []Value{NewString(needle)})
			if err != nil || !memberResult.Eql(result) {
				t.Fatalf("rindex member(%q, %q) = %v/%v, want %v/nil", text, needle, memberResult, err, result)
			}
			if want < 0 {
				if result.Kind() != KindNil {
					t.Errorf("rindex(%q, %q) = %v, want nil", text, needle, result)
				}
			} else if result.Kind() != KindInt || result.Int() != int64(want) {
				t.Errorf("rindex(%q, %q) = %v, want %d", text, needle, result, want)
			}
		}
	}
}

func FuzzStringRIndexByteBound(f *testing.F) {
	for _, text := range []string{"", "hello", "héllo 終", "😀😀", "a\xff終\xfea"} {
		f.Add(text, "", 0)
		f.Add(text, "a", 1)
		f.Add(text, "\uFFFD", -1)
	}
	f.Fuzz(func(t *testing.T, text, needle string, offset int) {
		if len(text) > 256 || len(needle) > 256 {
			t.Skip()
		}
		for _, at := range []int{offset, len(text), int(^uint(0) >> 1)} {
			got, err := stringRuneRIndex(nil, text, needle, at)
			want := referenceRuneSearch(text, needle, at, true)
			if err != nil || got != want {
				t.Fatalf("rindex(%q, %q, %d) = %d/%v, want %d/nil", text, needle, at, got, err, want)
			}
		}
	})
}
