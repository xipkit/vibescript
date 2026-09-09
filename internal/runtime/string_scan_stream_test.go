package runtime

import (
	"context"
	"errors"
	"fmt"
	"regexp"
	"regexp/syntax"
	goruntime "runtime"
	"strings"
	"testing"

	"github.com/google/go-cmp/cmp"
)

func TestStringScanBlockEarlyReturnAllocations(t *testing.T) {
	script := compileScriptWithConfig(t, Config{StepQuota: Unlimited, MemoryQuotaBytes: 64 << 20}, `def run(text)
  text.scan("a") { |part| return 7 }
  0
end`)
	allocations := func(size int) float64 {
		args := []Value{NewString(strings.Repeat("a", size))}
		return testing.AllocsPerRun(5, func() {
			got, err := script.Call(context.Background(), "run", args, CallOptions{})
			if err != nil || got.Int() != 7 {
				t.Fatalf("early-return scan of %d bytes = %v, %v; want 7, nil", size, got, err)
			}
		})
	}
	small, large := allocations(1024), allocations(262144)
	if large > small+32 {
		t.Errorf("first-match allocations grew from %.0f to %.0f with input length; want at most 32 additional allocations", small, large)
	}
}

func TestStringScanBlockNoMatchWithManyCaptures(t *testing.T) {
	t.Parallel()
	engine := MustNewEngine(Config{StepQuota: Unlimited, MemoryQuotaBytes: 64 << 10})
	engine.RegisterBuiltin("scratch_bytes", func(exec *Execution, _ Value, _ []Value, _ map[string]Value, _ Value) (Value, error) {
		return NewInt(int64(exec.reservedScratchBytes)), nil
	})
	script := compileScriptWithEngine(t, engine, `def block_scan(text, pattern)
  result = text.scan(pattern) { |part| nil }
  [result, scratch_bytes()]
end

def array_scan(text, pattern)
  text.scan(pattern)
end`)
	for _, test := range []struct {
		name    string
		text    string
		pattern string
	}{
		{name: "empty", pattern: strings.Repeat("(a)", 1000)},
		{name: "too short", text: "a", pattern: strings.Repeat("(a)", 1000)},
		{name: "sparse", text: strings.Repeat("b", 1000), pattern: strings.Repeat("(a)", 1000)},
		{name: "erased captures", text: "b", pattern: strings.Repeat("(a){0}", 1000) + "z"},
	} {
		t.Run(test.name, func(t *testing.T) {
			args := []Value{NewString(test.text), NewString(test.pattern)}
			compareArrays(t, callFunc(t, script, "array_scan", args), nil)
			compareArrays(t, callFunc(t, script, "block_scan", args), []Value{NewString(test.text), NewInt(0)})
		})
	}
}

func TestStringScanBlockManyCapturesRejectsBeforeYield(t *testing.T) {
	t.Parallel()
	engine := MustNewEngine(Config{StepQuota: Unlimited, MemoryQuotaBytes: 64 << 10})
	yielded := false
	engine.RegisterBuiltin("note_yield", func(_ *Execution, _ Value, _ []Value, _ map[string]Value, _ Value) (Value, error) {
		yielded = true
		return NewNil(), nil
	})
	script := compileScriptWithEngine(t, engine, `def run(text, pattern)
  text.scan(pattern) { |part| note_yield() }
end`)
	for _, test := range []struct {
		name    string
		text    string
		pattern string
	}{
		{name: "nonempty", text: strings.Repeat("a", 1000), pattern: strings.Repeat("(a)", 1000)},
		{name: "empty", pattern: strings.Repeat("()", 1000)},
		{name: "erased captures", pattern: strings.Repeat("(a){0}", 1000)},
	} {
		t.Run(test.name, func(t *testing.T) {
			yielded = false
			args := []Value{NewString(test.text), NewString(test.pattern)}
			requireCallRuntimeErrorType(t, script, "run", args, CallOptions{}, runtimeErrorTypeLimit)
			if yielded {
				t.Error("scan yielded a match before rejecting its quota footprint")
			}
		})
	}
}

func TestStringScanBlockLargeCaptureAdmission(t *testing.T) {
	t.Parallel()
	script := compileScriptWithConfig(t, Config{StepQuota: Unlimited, MemoryQuotaBytes: 64 << 10}, `def run(text, pattern)
  text.scan(pattern) { |part| nil }
end`)
	for _, groups := range []int{500, 600} {
		for _, erased := range []bool{false, true} {
			t.Run(fmt.Sprintf("groups=%d/erased=%t", groups, erased), func(t *testing.T) {
				text, pattern := strings.Repeat("a", groups), strings.Repeat("(a)", groups)
				if erased {
					text, pattern = "", strings.Repeat("(a){0}", groups)
				}
				got := callFunc(t, script, "run", []Value{NewString(text), NewString(pattern)})
				if got.String() != text {
					t.Errorf("large-capture block scan = %q, want receiver %q", got.String(), text)
				}
			})
		}
	}
}

func TestStringScanBlockRestCaptureAdmission(t *testing.T) {
	t.Parallel()
	script := compileScriptWithConfig(t, Config{StepQuota: Unlimited, MemoryQuotaBytes: 64 << 10}, `def run(text, pattern)
  text.scan(pattern) { |(head, *tail)| nil }
end`)
	text := strings.Repeat("a", 500)
	got := callFunc(t, script, "run", []Value{NewString(text), NewString(strings.Repeat("(a)", 500))})
	if got.String() != text {
		t.Errorf("destructuring block scan = %q, want receiver %q", got.String(), text)
	}
}

func TestStringScanBlockReleasesIndicesBeforeYield(t *testing.T) {
	t.Parallel()
	engine := MustNewEngine(Config{StepQuota: Unlimited, MemoryQuotaBytes: 64 << 10})
	var reservations []int
	engine.RegisterBuiltin("note_scratch", func(exec *Execution, _ Value, _ []Value, _ map[string]Value, _ Value) (Value, error) {
		reservations = append(reservations, exec.reservedScratchBytes)
		return NewNil(), nil
	})
	script := compileScriptWithEngine(t, engine, `def run(text)
  text.scan("(a)") { |part| note_scratch() }
end`)
	callFunc(t, script, "run", []Value{NewString("aa")})
	if diff := cmp.Diff([]int{0, 0}, reservations); diff != "" {
		t.Errorf("index scratch during callbacks mismatch (-want +got):\n%s", diff)
	}
}

func TestStringScanLastCaptureMatchesCompiledProgram(t *testing.T) {
	t.Parallel()
	for _, pattern := range []string{"", "(a)", "(a){0}", "(a){0}(b)", "(a)(b){0}", "((a){0})", "((a)(b)){0}", "(a){0,0}|(b){0,2}", "(?P<x>a){1000}", "((a)?)*"} {
		parsed, err := syntax.Parse(pattern, syntax.Perl)
		if err != nil {
			t.Fatal(err)
		}
		program, err := syntax.Compile(parsed.Simplify())
		if err != nil {
			t.Fatal(err)
		}
		if got := 2 * (stringScanLastCapture(parsed) + 1); got != program.NumCap {
			t.Errorf("surviving capture slots in %q = %d, want compiled program's %d", pattern, got, program.NumCap)
		}
	}
}

func TestStringScanIndexScratchCoversReturnedCapacity(t *testing.T) {
	t.Parallel()
	for _, groups := range []int{1, 15, 63, 255, 511, 1000} {
		re := regexp.MustCompile(strings.Repeat("(a)", groups))
		loc := re.FindStringSubmatchIndex(strings.Repeat("a", groups))
		want := cap(loc) * estimatedIntBytes
		if got := stringScanIndexScratchBytes(re, groups); got != want {
			t.Errorf("index scratch for %d surviving captures = %d, want actual capacity %d", groups, got, want)
		}
	}
	for _, pattern := range []string{"(a){0}", "(a)(b){0}", strings.Repeat("(a){0}", 1000)} {
		re := regexp.MustCompile(pattern)
		loc := re.FindStringSubmatchIndex("a")
		if got, want := stringScanIndexScratchBytes(re, re.NumSubexp()), cap(loc)*estimatedIntBytes; got < want {
			t.Errorf("index scratch for erased captures %q = %d, want at least actual capacity %d", pattern, got, want)
		}
	}
}

func TestStringScanBlockNestingLimit(t *testing.T) {
	t.Parallel()
	for _, core := range []string{`\b..`, `\B..`, `(?m)^.`, `a*`} {
		pattern := strings.Repeat("(", 998) + core + strings.Repeat(")", 998)
		re, err := compileCachedRegex(pattern)
		if err != nil {
			t.Fatal(err)
		}
		script := compileScriptWithConfig(t, Config{StepQuota: Unlimited, MemoryQuotaBytes: 64 << 20}, `def run(text, pattern)
  out = []
  text.scan(pattern) { |m| out.push(m) }
  out
end`)
		for _, text := range []string{"abcd", "ab\ncd", "éabcd", "aaab"} {
			got := callFunc(t, script, "run", []Value{NewString(text), NewString(pattern)})
			compareArrays(t, got, scanWantFromRegexp(re, text))
		}
	}
}

func TestStringScanBlockNestingLimitEarlyReturn(t *testing.T) {
	t.Parallel()
	for _, test := range []struct {
		name   string
		core   string
		groups int
	}{
		{name: "suffix", core: ".", groups: 999},
		{name: "left context", core: `\b.`, groups: 998},
	} {
		t.Run(test.name, func(t *testing.T) {
			pattern := strings.Repeat("(", test.groups) + test.core + strings.Repeat(")", test.groups)
			script := compileScriptWithConfig(t, Config{StepQuota: Unlimited, MemoryQuotaBytes: 2 << 20}, `def run(text, pattern)
  text.scan(pattern) { |m| return m.size }
  0
end`)
			got := callFunc(t, script, "run", []Value{NewString(strings.Repeat("a ", 32<<10)), NewString(pattern)})
			if got.Int() != int64(test.groups) {
				t.Errorf("deep-pattern early-return scan = %v, want %d under 2 MiB", got, test.groups)
			}
		})
	}
}

// Adding left context to a pattern at Go's AST-height limit requires the exact
// bounded-table fallback after its first yield. That retained table must count
// against the quota even when the block discards each result.
func TestStringScanBlockNestingLimitQuota(t *testing.T) {
	t.Parallel()
	pattern := strings.Repeat("(", 998) + `\b.` + strings.Repeat(")", 998)
	script := compileScriptWithConfig(t, Config{StepQuota: Unlimited, MemoryQuotaBytes: 256 << 10}, `def run(text, pattern)
  text.scan(pattern) { |m| nil }
end`)
	requireCallRuntimeErrorType(t, script, "run", []Value{NewString(strings.Repeat("a ", 100)), NewString(pattern)}, CallOptions{}, runtimeErrorTypeLimit)
}

func TestStringScanBlockStepQuota(t *testing.T) {
	t.Parallel()
	script := compileScriptWithConfig(t, Config{StepQuota: 20, MemoryQuotaBytes: Unlimited}, `def run(text)
  text.scan("a") { |m| nil }
end`)
	requireCallRuntimeErrorType(t, script, "run", []Value{NewString(strings.Repeat("a", 1024))}, CallOptions{}, runtimeErrorTypeLimit)
}

func TestStringScanBlockChecksCancellationBetweenMatches(t *testing.T) {
	t.Parallel()
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	engine := MustNewEngine(Config{StepQuota: Unlimited, MemoryQuotaBytes: Unlimited})
	engine.RegisterBuiltin("cancel_scan", func(_ *Execution, _ Value, _ []Value, _ map[string]Value, _ Value) (Value, error) {
		cancel()
		return NewNil(), nil
	})
	script := compileScriptWithEngine(t, engine, `def run(text)
  text.scan("a") { |m| cancel_scan() }
end`)
	_, err := script.Call(ctx, "run", []Value{NewString(strings.Repeat("a", 4096))}, CallOptions{})
	if !errors.Is(err, context.Canceled) {
		t.Errorf("block scan canceled after a match = %v, want context.Canceled", err)
	}
}

func FuzzStringScanCursor(f *testing.F) {
	for _, pattern := range []string{"", "..", `\b`, `\B`, "a.*z|a", "a*", "(?m)^|$", "(a)(b)?"} {
		f.Add(pattern, "aab\né\xff")
	}
	f.Fuzz(func(t *testing.T, pattern, text string) {
		if len(pattern) > 64 || len(text) > 128 {
			t.Skip()
		}
		re, err := regexp.Compile(pattern)
		if err != nil {
			t.Skip()
		}
		for _, indicesOnly := range []bool{false, true} {
			cursor := stringScanCursor{re: re, previousEnd: -1, indicesOnly: indicesOnly}
			var got [][]int
			for {
				loc, err := cursor.next(text)
				if err != nil {
					t.Fatal(err)
				}
				if loc == nil {
					break
				}
				got = append(got, loc)
				if len(got) > len(text)+1 {
					t.Fatalf("scan(%q, %q) did not advance", pattern, text)
				}
			}
			want := re.FindAllStringSubmatchIndex(text, -1)
			if indicesOnly {
				want = re.FindAllStringIndex(text, -1)
			}
			if diff := cmp.Diff(want, got); diff != "" {
				t.Errorf("scan(%q, %q, indicesOnly=%t) mismatch (-want +got):\n%s", pattern, text, indicesOnly, diff)
			}
		}
	})
}

func TestStringScanIndexProbeNestingFallback(t *testing.T) {
	t.Parallel()
	pattern := strings.Repeat("(", 998) + `\b.` + strings.Repeat(")", 998)
	re := regexp.MustCompile(pattern)
	text := "a b"
	matches := re.FindAllStringSubmatchIndex(text, -1)
	cursor := stringScanCursor{re: re, previousEnd: -1}
	cursor.atNestingLimit = func(_ string, start int) ([]int, error) {
		for _, loc := range matches {
			if loc[0] >= start {
				return loc, nil
			}
		}
		return nil, nil
	}
	if _, err := cursor.next(text); err != nil {
		t.Fatal(err)
	}
	probe := cursor
	probe.indicesOnly = true
	got, err := probe.next(text)
	if err != nil {
		t.Fatal(err)
	}
	if diff := cmp.Diff(matches[1], got); diff != "" {
		t.Fatalf("fallback probe mismatch (-want +got):\n%s", diff)
	}
	if cursor.position != 1 || cursor.previousEnd != 1 || !probe.fallback {
		t.Fatalf("probe changed source cursor or missed fallback: cursor=%+v probe=%+v", cursor, probe)
	}
	got, err = cursor.next(text)
	if err != nil {
		t.Fatal(err)
	}
	if diff := cmp.Diff(matches[1], got); diff != "" {
		t.Errorf("scan after probe mismatch (-want +got):\n%s", diff)
	}
	if diff := cmp.Diff(re.FindAllStringSubmatchIndex(text, -1), matches); diff != "" {
		t.Errorf("probe mutated fallback table (-want +got):\n%s", diff)
	}
}

func BenchmarkStringScanEarlyReturn(b *testing.B) {
	for _, size := range []int{1024, 16384, 262144} {
		for _, method := range []string{"scan", "match"} {
			b.Run(fmt.Sprintf("n=%d/%s", size, method), func(b *testing.B) {
				script := compileScriptWithConfig(b, Config{StepQuota: Unlimited, MemoryQuotaBytes: 64 << 20}, "def run(text)\ntext."+method+"(\"a\") { |part| return 7 }\n0\nend")
				args := []Value{NewString(strings.Repeat("a", size))}
				b.ReportAllocs()
				b.ResetTimer()
				for range b.N {
					got, err := script.Call(context.Background(), "run", args, CallOptions{})
					if err != nil || got.Int() != 7 {
						b.Fatalf("early-return %s of %d bytes = %v, %v; want 7, nil", method, size, got, err)
					}
				}
			})
		}
	}
}

func TestStringScanBlockRetainedMatchesCharged(t *testing.T) {
	t.Parallel()
	script := compileScriptWithConfig(t, Config{StepQuota: Unlimited, MemoryQuotaBytes: 32 << 10}, `def run(text)
  out = []
  text.scan("a") { |m| out.push(m) }
  out
end`)
	requireCallRuntimeErrorType(t, script, "run", []Value{NewString(strings.Repeat("a", 2000))}, CallOptions{}, runtimeErrorTypeLimit)
}

func TestStringScanBlockReleasesScratch(t *testing.T) {
	t.Parallel()
	for _, body := range []string{`text.scan("a") { |m| nil }`, `text.scan("z") { |m| nil }`, `text.scan("a") { |m| return 7 }`} {
		engine := MustNewEngine(Config{StepQuota: Unlimited, MemoryQuotaBytes: 64 << 10})
		engine.RegisterBuiltin("scratch_bytes", func(exec *Execution, _ Value, _ []Value, _ map[string]Value, _ Value) (Value, error) {
			return NewInt(int64(exec.reservedScratchBytes)), nil
		})
		script := compileScriptWithEngine(t, engine, "def scan(text)\n"+body+"\nend\ndef run(text)\nscan(text)\nscratch_bytes()\nend")
		got := callFunc(t, script, "run", []Value{NewString("aaa")})
		if got.Int() != 0 {
			t.Errorf("reserved scratch after %s = %v, want 0", body, got)
		}
	}
}

func BenchmarkStringScanBlockDrain(b *testing.B) {
	for _, size := range []int{1024, 16384} {
		for _, pattern := range []string{"a", "(a)", ".."} {
			b.Run(fmt.Sprintf("n=%d/pattern=%s", size, pattern), func(b *testing.B) {
				script := compileScriptWithConfig(b, Config{StepQuota: Unlimited, MemoryQuotaBytes: 64 << 20}, "def run(text)\ntext.scan("+goStringToVibescript(pattern)+") { |part| nil }\nend")
				args := []Value{NewString(strings.Repeat("a", size))}
				b.ReportAllocs()
				b.ResetTimer()
				for range b.N {
					got, err := script.Call(context.Background(), "run", args, CallOptions{})
					if err != nil || got.String() != args[0].String() {
						b.Fatalf("full scan of %d bytes = %v, %v; want receiver, nil", size, got, err)
					}
				}
			})
		}
	}
}

func TestStringScanCursorKeepsSmallStringMatcher(t *testing.T) {
	re := regexp.MustCompile(strings.Repeat("(a?)", 120))
	// Empty regexp's machine pools so this measures the initial search's
	// scratch rather than reusing allocations from another test.
	goruntime.GC()
	goruntime.GC()
	var before, after goruntime.MemStats
	goruntime.ReadMemStats(&before)
	cursor := stringScanCursor{re: re, previousEnd: -1}
	loc, err := cursor.next("a")
	goruntime.ReadMemStats(&after)
	if err != nil || len(loc) != 242 || loc[0] != 0 || loc[1] != 1 {
		t.Fatalf("nullable-capture scan of a = %v, %v; want the match and 120 captures", loc, err)
	}
	// FindString uses about 44 KiB; FindReader disables Go's bounded
	// backtracker and needs over 500 KiB for this same small subject.
	if allocated := after.TotalAlloc - before.TotalAlloc; allocated > 256<<10 {
		t.Errorf("one-byte nullable-capture scan allocated %d bytes, want under 256 KiB", allocated)
	}
}

func TestStringScanCursorWrapperPreservesProgramLimit(t *testing.T) {
	t.Parallel()
	pattern := `\b` + strings.Repeat("a{999}", 99) + "a{997}[bc]"
	re, err := compileCachedRegex(pattern)
	if err != nil {
		t.Fatal(err)
	}
	cursor := stringScanCursor{re: re, position: 1, previousEnd: -1}
	if _, err := cursor.next("x"); err != nil {
		t.Fatalf("scan wrapper rejected an admitted pattern: %v", err)
	}
	if _, err := compileCachedRegex(cursor.from.String()); err == nil {
		t.Error("cached internal scan wrapper bypassed the user program cap")
	}
}

func BenchmarkStringScanSparseDrain(b *testing.B) {
	for _, pattern := range []string{"a", "a+", "^a"} {
		b.Run(pattern, func(b *testing.B) {
			script := compileScriptWithConfig(b, Config{StepQuota: Unlimited, MemoryQuotaBytes: 64 << 20}, "def run(text)\ntext.scan("+goStringToVibescript(pattern)+") { |part| nil }\nend")
			args := []Value{NewString("a" + strings.Repeat("z", (1<<20)-1))}
			b.ReportAllocs()
			b.ResetTimer()
			for range b.N {
				got, err := script.Call(context.Background(), "run", args, CallOptions{})
				if err != nil || got.String() != args[0].String() {
					b.Fatalf("sparse scan = %v, %v; want receiver, nil", got, err)
				}
			}
		})
	}
}
