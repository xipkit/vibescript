package runtime

import (
	"context"
	"errors"
	"strings"
	"testing"
	"unicode/utf8"
)

func callStringMemberForTest(t *testing.T, exec *Execution, receiver Value, name string, args []Value) (Value, error) {
	t.Helper()
	member, err := stringMember(receiver, name)
	if err != nil {
		t.Fatalf("stringMember(%s): %v", name, err)
	}
	builtin := valueBuiltin(member)
	if builtin == nil {
		t.Fatalf("string member %s is not a builtin", name)
	}
	return builtin.Fn(exec, receiver, args, nil, NewNil())
}

func TestStringTemplateScanner(t *testing.T) {
	t.Parallel()
	context := NewHash(map[string]Value{
		"name": NewString("Alex"),
	})
	malformedPrefix := strings.Repeat("{{1", 8) + "}} "
	tests := []struct {
		name string
		text string
		want string
	}{
		{
			name: "invalid_placeholder_preserved",
			text: "{{1}} {{ name }}",
			want: "{{1}} Alex",
		},
		{
			name: "missing_placeholder_preserved",
			text: "Hello {{missing}}",
			want: "Hello {{missing}}",
		},
		{
			name: "repeated_missing_placeholder_preserves_each_literal",
			text: "{{ missing }} {{missing}}",
			want: "{{ missing }} {{missing}}",
		},
		{
			name: "nested_valid_placeholder_after_invalid_open",
			text: "{{ bad {{name}}",
			want: "{{ bad Alex",
		},
		{
			name: "overlapping_valid_placeholder_after_invalid_open",
			text: "{{{name}}",
			want: "{Alex",
		},
		{
			name: "malformed_openers_before_valid_placeholder",
			text: malformedPrefix + "{{name}}",
			want: malformedPrefix + "Alex",
		},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			got, err := stringTemplate(tc.text, context, false)
			if err != nil {
				t.Fatalf("stringTemplate(%q) error = %v", tc.text, err)
			}
			if got != tc.want {
				t.Fatalf("stringTemplate(%q) = %q, want %q", tc.text, got, tc.want)
			}
		})
	}
}

func TestStringTemplateHonorsMemoryQuotaWhileRendering(t *testing.T) {
	t.Parallel()
	script := compileScriptWithConfig(t, Config{
		StepQuota:        100_000,
		MemoryQuotaBytes: 64 * 1024,
	}, `
def run(text, payload)
  text.template({ value: payload })
end
`)

	requireCallErrorContains(t, script, "run", []Value{
		NewString(strings.Repeat("{{value}}", 128)),
		NewString(strings.Repeat("x", 1024)),
	}, CallOptions{}, "memory quota exceeded")
}

func TestStringTemplateHonorsCanceledContextWhileRendering(t *testing.T) {
	t.Parallel()
	script := compileScriptWithConfig(t, Config{StepQuota: 1_000_000}, `
def run(text, payload)
  text.template({ value: payload })
end
`)

	ctx, cancel := context.WithCancel(context.Background())
	cancel()
	_, err := script.Call(ctx, "run", []Value{
		NewString(strings.Repeat("{{value}}", 128)),
		NewString("x"),
	}, CallOptions{})
	if !errors.Is(err, context.Canceled) {
		t.Fatalf("string.template canceled context error = %v, want context.Canceled", err)
	}
}

func TestStringTemplateEmptySubstitutionsHonorStepQuota(t *testing.T) {
	t.Parallel()
	script := compileScriptWithConfig(t, Config{StepQuota: 20, MemoryQuotaBytes: 64 << 20}, `
def run(text)
  text.template({ value: "" })
end
`)

	_, err := script.Call(context.Background(), "run", []Value{
		NewString(strings.Repeat("{{value}}", 100)),
	}, CallOptions{})
	requireRuntimeErrorType(t, err, runtimeErrorTypeLimit)
}

func TestFixedStringTransformsHonorMemoryQuotaBeforeMaterializing(t *testing.T) {
	t.Parallel()

	receiver := NewString(strings.Repeat("x", 40*1024))
	arg := NewString(strings.Repeat("y", 40*1024))
	exec := &Execution{ctx: context.Background(), quota: 1 << 30, memoryQuota: 96 * 1024}
	_, err := callStringMemberForTest(t, exec, receiver, "concat", []Value{arg})
	requireErrorIs(t, err, errMemoryQuotaExceeded)

	receiver = NewString(strings.Repeat("x", 16*1024))
	exec = &Execution{ctx: context.Background(), quota: 1 << 30, memoryQuota: 48 * 1024}
	_, err = callStringMemberForTest(t, exec, receiver, "reverse", nil)
	requireErrorIs(t, err, errMemoryQuotaExceeded)
}

func TestASCIICaseTransformsHonorScratchQuota(t *testing.T) {
	t.Parallel()

	for _, tc := range []struct {
		name string
		text string
		mode caseMode
		args []Value
	}{
		{name: "ascii changed", text: strings.Repeat("aZ!9", 4096), mode: caseModeASCII, args: []Value{NewSymbol("ascii")}},
		{name: "ascii unchanged", text: strings.Repeat("1234", 4096), mode: caseModeASCII, args: []Value{NewSymbol("ascii")}},
		{name: "invalid vector changed", text: strings.Repeat("aZ!9", 1024) + "\xff"},
		{name: "invalid vector unchanged", text: strings.Repeat("1234", 1024) + "\xff"},
		{name: "invalid changed", text: strings.Repeat("aZ!9", 4096) + "\xff"},
		{name: "invalid unchanged", text: strings.Repeat("1234", 4096) + "\xff"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			receiver := NewString(tc.text)
			outputBytes, scratchBytes := projectedCaseTransformBytesAndScratch(tc.text, tc.mode)
			var b strings.Builder
			probe := &Execution{ctx: context.Background(), quota: 1 << 30, memoryQuota: 1 << 60}
			quota := probe.estimateMemoryUsageForCallRoots(NewNil(), receiver, tc.args, nil, NewNil())
			quota = saturatingAdd(quota, estimatedValueBytes+estimatedStringHeaderBytes)
			quota = saturatingAdd(quota, projectedBuilderCap(&b, outputBytes))
			quota = saturatingAdd(quota, scratchBytes)

			for _, method := range []string{"upcase", "downcase", "capitalize", "swapcase"} {
				for _, suffix := range []string{"", "!"} {
					t.Run(method+suffix, func(t *testing.T) {
						t.Parallel()
						want := scalarASCIICase(tc.text, method)
						for _, limit := range []int{quota - 1, quota, quota + 1} {
							exec := &Execution{ctx: context.Background(), quota: 1 << 30, memoryQuota: limit}
							got, err := callStringMemberForTest(t, exec, receiver, method+suffix, tc.args)
							if limit < quota {
								requireErrorIs(t, err, errMemoryQuotaExceeded)
								continue
							}
							if err != nil {
								t.Fatalf("%s%s at memory quota %d: %v", method, suffix, limit, err)
							}
							if suffix == "!" && want == tc.text {
								if got.Kind() != KindNil {
									t.Errorf("unchanged %s! at memory quota %d = %v, want nil", method, limit, got)
								}
							} else if got.Kind() != KindString || got.String() != want {
								t.Errorf("%s%s at memory quota %d = %v, want %q", method, suffix, limit, got, want)
							}
							if receiver.String() != tc.text {
								t.Errorf("%s%s changed its receiver", method, suffix)
							}
						}
					})
				}
			}
		})
	}
}

func TestProjectedStringReverseBytesIncludesInvalidExpansionAndScratch(t *testing.T) {
	t.Parallel()

	outputBytes, scratchBytes := projectedStringReverseBytes("\xffa")
	wantOutput := utf8.RuneLen(utf8.RuneError) + len("a")
	if outputBytes != wantOutput {
		t.Fatalf("projected reverse output bytes = %d, want %d", outputBytes, wantOutput)
	}
	wantScratch := estimatedSliceBaseBytes + 2*estimatedRuneBytes
	if scratchBytes != wantScratch {
		t.Fatalf("projected reverse scratch bytes = %d, want %d", scratchBytes, wantScratch)
	}

	outputBytes, scratchBytes = projectedCaseTransformBytesAndScratch("\xffa", caseModeDefault)
	if outputBytes != 2 {
		t.Fatalf("projected invalid-UTF8 case output bytes = %d, want 2", outputBytes)
	}
	if scratchBytes != byteBufferScratchBytes(2) {
		t.Fatalf("projected invalid-UTF8 case scratch bytes = %d, want %d", scratchBytes, byteBufferScratchBytes(2))
	}
}
