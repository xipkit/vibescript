package runtime

import (
	"context"
	"errors"
	"regexp"
	"strings"
	"testing"
)

func TestRegexNamespaceScanQuotas(t *testing.T) {
	t.Parallel()
	for _, expr := range []string{`Regex.match("z", s)`, `Regex.replace(s, "z", "x")`, `Regex.replace_all(s, "z", "x")`, `Regex.replace_all(*[s, "z", "x"])`} {
		for _, quota := range []int{32, 100000} {
			script := compileScriptWithConfig(t, Config{StepQuota: quota, MemoryQuotaBytes: Unlimited}, "def run(s)\n"+expr+"\nend")
			_, err := script.Call(context.Background(), "run", []Value{NewString(strings.Repeat("a", 64<<10))}, CallOptions{})
			if quota == 32 {
				var runtimeErr *RuntimeError
				if !errors.As(err, &runtimeErr) || runtimeErr.Type != runtimeErrorTypeLimit {
					t.Errorf("%s quota=%d error=%v, want LimitError", expr, quota, err)
				}
			} else if err != nil {
				t.Errorf("%s quota=%d: %v", expr, quota, err)
			}
		}
	}
}

func TestRegexNamespaceChargesRepeatedScanning(t *testing.T) {
	t.Parallel()
	for _, call := range []string{"Regex.replace_all(text, pattern, replacement)", "Regex.replace_all(*[text, pattern, replacement])"} {
		script := compileScriptWithConfig(t, Config{StepQuota: 3000, MemoryQuotaBytes: Unlimited}, "def run(text, pattern, replacement)\n"+call+"\nend")
		text := NewString(strings.Repeat("a", 4096))
		got, err := script.Call(context.Background(), "run", []Value{text, NewString("a"), NewString("")}, CallOptions{})
		if err != nil || got.String() != "" {
			t.Fatalf("dense ordinary replacement failed: %v", err)
		}
		requireCallRuntimeErrorType(t, script, "run", []Value{text, NewString("a.*z|a"), NewString("")}, CallOptions{}, runtimeErrorTypeLimit)
	}
}

func TestRegexNamespaceChargesRepeatedTemplates(t *testing.T) {
	t.Parallel()
	script := compileScriptWithConfig(t, Config{StepQuota: 2000, MemoryQuotaBytes: Unlimited}, "def run(text, replacement)\nRegex.replace_all(text, \"a\", replacement)\nend")
	text := NewString(strings.Repeat("a", 512))
	requireCallRuntimeErrorType(t, script, "run", []Value{text, NewString(strings.Repeat("$missing", 512))}, CallOptions{}, runtimeErrorTypeLimit)
	got, err := script.Call(context.Background(), "run", []Value{text, NewString("$missing")}, CallOptions{})
	if err != nil || got.String() != "" {
		t.Fatalf("ordinary missing-capture replacement failed: %v", err)
	}
}

func TestRegexNamespaceChargesCaptureNameSearches(t *testing.T) {
	t.Parallel()
	re := regexp.MustCompile(strings.Repeat("(?P<same>)", 128))
	loc := re.FindStringSubmatchIndex("")
	work := regexWork{exec: &Execution{ctx: context.Background(), quota: 64}}
	if _, err := appendRegexReplacement(&work, nil, re, strings.Repeat("$none", 128), "", loc); !errors.Is(err, errStepQuotaExceeded) {
		t.Fatalf("capture-name search error=%v, want step exhaustion", err)
	}
	work = regexWork{exec: &Execution{ctx: context.Background(), quota: 64}}
	got, err := appendRegexReplacement(&work, nil, re, "$none", "", loc)
	if err != nil || len(got) != 0 {
		t.Fatalf("ordinary missing-name lookup failed: result=%q, error=%v", got, err)
	}
}

func TestRegexNamespaceChargesActiveProgramStates(t *testing.T) {
	t.Parallel()
	script := compileScriptWithConfig(t, Config{StepQuota: 20000, MemoryQuotaBytes: Unlimited}, "def run(text)\nRegex.match(\"a{1000}bc?\", text)\nend")
	if _, err := compileCachedRegex("a{1000}bc?"); err != nil {
		t.Fatal(err)
	}
	requireCallRuntimeErrorType(t, script, "run", []Value{NewString(strings.Repeat("a", 32<<10))}, CallOptions{}, runtimeErrorTypeLimit)
	got, err := script.Call(context.Background(), "run", []Value{NewString(strings.Repeat("a", 128))}, CallOptions{})
	if err != nil || !got.IsNil() {
		t.Fatalf("short legitimate scan failed: result=%v, error=%v", got, err)
	}
}

func TestRegexNamespaceChargesCaptureStateCopiesOnEmptyInput(t *testing.T) {
	t.Parallel()
	script := compileScriptWithConfig(t, Config{StepQuota: 1000, MemoryQuotaBytes: Unlimited}, "def run(pattern)\nRegex.replace_all(\"\", pattern, \"\")\nend")
	pattern := strings.Repeat("(a?)", 256)
	if _, err := compileCachedRegex(pattern); err != nil {
		t.Fatal(err)
	}
	requireCallRuntimeErrorType(t, script, "run", []Value{NewString(pattern)}, CallOptions{}, runtimeErrorTypeLimit)
	got, err := script.Call(context.Background(), "run", []Value{NewString("(a?)")}, CallOptions{})
	if err != nil || got.String() != "" {
		t.Fatalf("ordinary empty-input replacement failed: %v", err)
	}
}

func TestRegexNamespaceReaderRejectsPartialMatch(t *testing.T) {
	t.Parallel()
	for _, pattern := range []string{"a*", "z"} {
		work := regexWork{exec: &Execution{ctx: context.Background(), quota: 2}}
		scan := regexNamespaceScan{re: regexp.MustCompile(pattern), work: &work}
		loc, err := scan.find(strings.Repeat("a", 1024), 0)
		if !errors.Is(err, errStepQuotaExceeded) || loc != nil {
			t.Errorf("pattern=%q loc=%v error=%v; want rejection without partial indices", pattern, loc, err)
		}
	}
}

func TestRegexNamespaceChargesCompilationOnMiss(t *testing.T) {
	t.Parallel()
	cache := newRegexCache(2, compiledRegexCacheInstructionBudget)
	pattern := strings.Repeat("a{999}", 10)
	work := regexWork{exec: &Execution{ctx: context.Background(), quota: 32}}
	if _, err := cache.compileWithWork(pattern, &work, maxCompiledRegexInstructions); !errors.Is(err, errStepQuotaExceeded) {
		t.Fatalf("compile error=%v, want step exhaustion before expansion", err)
	}
	if len(cache.entries) != 0 {
		t.Fatal("rejected compilation populated cache")
	}
	work = regexWork{exec: &Execution{ctx: context.Background(), quota: 100000}}
	if _, err := cache.compileWithWork(pattern, &work, maxCompiledRegexInstructions); err != nil {
		t.Fatal(err)
	}
	work = regexWork{exec: &Execution{ctx: context.Background(), quota: 1}}
	if _, err := cache.compileWithWork(pattern, &work, maxCompiledRegexInstructions); err != nil || work.exec.steps != 0 {
		t.Fatalf("cache hit charged nonexistent compilation: steps=%d, error=%v", work.exec.steps, err)
	}
}

func TestRegexNamespaceWrapperPreservesProgramLimit(t *testing.T) {
	t.Parallel()
	pattern := strings.Repeat("a{999}", 99) + "a{996}[bc]"
	re, err := compileCachedRegex(pattern)
	if err != nil {
		t.Fatal(err)
	}
	scan := regexNamespaceScan{re: re}
	if _, err := scan.find("x", 1); err != nil {
		t.Fatalf("internal wrapper rejected an admitted user pattern: %v", err)
	}
	if _, err := compileCachedRegex(scan.from.String()); err == nil {
		t.Fatal("cached internal wrapper bypassed the user program cap")
	}
}

func TestRegexNamespaceWrapperPreservesNestingLimit(t *testing.T) {
	t.Parallel()
	pattern := strings.Repeat("(", 998) + "a" + strings.Repeat(")", 998)
	re, err := compileCachedRegex(pattern)
	if err != nil {
		t.Fatal(err)
	}
	got, err := builtinRegexReplaceValues(nil, NewString("aa"), NewString(pattern), NewString("x"), true)
	if err != nil || got.String() != re.ReplaceAllString("aa", "x") {
		t.Fatalf("internal wrapper rejected an admitted nested pattern: %v", err)
	}
	work := regexWork{exec: &Execution{ctx: context.Background(), quota: 100000}}
	scan := regexNamespaceScan{re: re, work: &work}
	if loc, err := scan.find("aa", 1); !errors.Is(err, errStepQuotaExceeded) || loc != nil {
		t.Fatalf("nesting fallback returned %v,%v under an insufficient quota", loc, err)
	}
}

func TestRegexNamespaceReplacementMatchesGo(t *testing.T) {
	t.Parallel()
	patterns := []string{"", "a", "é", "�", "(a)", "a*", "a.*z|a", "aa|", ".", "^", "$", `\A`, `\z`, `\b`, `\B`, "(?m)^|$", "(a)(b)?", "(?P<x>a)|(?P<x>b)"}
	for _, pattern := range patterns {
		re := regexp.MustCompile(pattern)
		for _, text := range []string{"", "a", "aaaa", "ab", "ab\na", "éa界", "a\xffb"} {
			for _, replacement := range []string{"", "x", "${0}", "$1-$2", "$x"} {
				want := re.ReplaceAllString(text, replacement)
				got, err := builtinRegexReplaceValues(nil, NewString(text), NewString(pattern), NewString(replacement), true)
				if err != nil || got.String() != want {
					t.Errorf("pattern=%q text=%q replacement=%q: got %q,%v; want %q", pattern, text, replacement, got.String(), err, want)
				}
			}
		}
	}
}

func FuzzRegexNamespaceReplacement(f *testing.F) {
	for _, pattern := range []string{"a.*z|a", "aa|", "(?P<x>a)|(?P<x>b)", `\b`, "(?m)^|$"} {
		f.Add(pattern, "aab\né", "${0}:$x")
	}
	f.Fuzz(func(t *testing.T, pattern, text, replacement string) {
		if len(pattern) > 64 || len(text) > 32 || len(replacement) > 64 {
			t.Skip()
		}
		re, err := compileCachedRegex(pattern)
		if err != nil {
			t.Skip()
		}
		want := re.ReplaceAllString(text, replacement)
		got, err := builtinRegexReplaceValues(nil, NewString(text), NewString(pattern), NewString(replacement), true)
		if err != nil || got.String() != want {
			t.Fatalf("pattern=%q text=%q replacement=%q: got %q,%v; want %q", pattern, text, replacement, got.String(), err, want)
		}
	})
}
