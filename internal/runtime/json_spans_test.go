package runtime

import (
	"context"
	"errors"
	"fmt"
	"strings"
	"testing"

	"github.com/google/go-cmp/cmp"
)

type jsonSpanBudget struct {
	quota       int
	memory      int
	steps       int
	cancelAfter int
}

type jsonSpanContext struct {
	context.Context
	done        chan struct{}
	polls       int
	cancelAfter int
}

func (c *jsonSpanContext) Done() <-chan struct{} {
	c.polls++
	if c.cancelAfter > 0 && c.polls == c.cancelAfter {
		close(c.done)
	}
	return c.done
}

func (c *jsonSpanContext) Err() error {
	select {
	case <-c.done:
		return context.Canceled
	default:
		return nil
	}
}

type jsonSpanError struct {
	Type   string
	Text   string
	Step   bool
	Memory bool
	Cancel bool
}

func jsonSpanErrorResult(err error) jsonSpanError {
	if err == nil {
		return jsonSpanError{}
	}
	return jsonSpanError{
		Type:   fmt.Sprintf("%T", err),
		Text:   err.Error(),
		Step:   errors.Is(err, errStepQuotaExceeded),
		Memory: errors.Is(err, errMemoryQuotaExceeded),
		Cancel: errors.Is(err, context.Canceled),
	}
}

type jsonSpanResult struct {
	Output       string
	Error        jsonSpanError
	Exhausted    jsonSpanError
	Position     int
	Base         int
	Used         int
	Steps        int
	ChargedSteps int
	Scratch      int
	Sections     int
	ContextPolls int
}

func jsonSpanExecute(text, prefix string, budget jsonSpanBudget, stringify, reference bool) jsonSpanResult {
	ctx := &jsonSpanContext{Context: context.Background(), done: make(chan struct{}), cancelAfter: budget.cancelAfter}
	exec := &Execution{ctx: ctx, quota: budget.quota, memoryQuota: budget.memory, steps: budget.steps}
	var result jsonSpanResult
	var err error
	if stringify {
		state := &jsonStringifyState{exec: exec}
		var out []byte
		if reference {
			out, err = jsonSpanReferenceAppendString([]byte(prefix), text, state)
		} else {
			out, err = appendJSONString([]byte(prefix), text, state)
		}
		result.Output = string(out)
		result.ChargedSteps = state.chargedSteps
	} else {
		p := jsonValueParser{raw: text, exec: exec}
		if reference {
			ref := jsonSpanReferenceParser{jsonValueParser: p}
			result.Output, err = ref.parseString()
			p = ref.jsonValueParser
		} else {
			result.Output, err = p.parseString()
		}
		result.Position, result.Base, result.Used = p.pos, p.base, p.used
	}
	result.Error = jsonSpanErrorResult(err)
	result.Exhausted = jsonSpanErrorResult(exec.exhausted)
	result.Steps = exec.steps
	result.Scratch = exec.reservedScratchBytes
	result.Sections = exec.accumMeteredSections
	result.ContextPolls = ctx.polls
	return result
}

func checkJSONSpans(t *testing.T, text, prefix string, budget jsonSpanBudget, stringify bool) {
	t.Helper()
	want := jsonSpanExecute(text, prefix, budget, stringify, true)
	got := jsonSpanExecute(text, prefix, budget, stringify, false)
	if diff := cmp.Diff(want, got); diff != "" {
		t.Errorf("JSON string scan(%q, prefix=%q, budget=%+v, stringify=%t) mismatch (-want +got):\n%s", text, prefix, budget, stringify, diff)
	}
	if got.Sections != 0 || got.Scratch != 0 {
		t.Errorf("JSON string scan(%q) retained sections/scratch = %d/%d, want 0/0", text, got.Sections, got.Scratch)
	}
}

func TestJSONSpansPreserveBytesAndErrors(t *testing.T) {
	budget := jsonSpanBudget{quota: 1 << 20, memory: 16 << 20}
	for _, offset := range []int{0, 15, 16, 31, 32, 63, 64} {
		for b := range 256 {
			text := strings.Repeat("a", offset) + string([]byte{byte(b)}) + strings.Repeat("z", 33)
			checkJSONSpans(t, `"`+text+`"`, "", budget, false)
			checkJSONSpans(t, text, "", budget, true)
		}
	}
	for _, n := range []int{0, 1, 15, 16, 17, 31, 32, 33, 63, 64, 65, 127, 128, 129, 255, 4095} {
		prefix := strings.Repeat("a", n)
		for _, tail := range []string{
			`"`, `\"z"`, `\\z"`, `\n\t\r\b\f\/"`, `\u0000\uD83D\uDE00"`,
			`\uD800"`, `\uDC00"`, `\uD800\u0041"`, `\uZZZZ"`, `\x"`,
			"é終😀\u2028\u2029\"", "\xff\xfe\"", "\xc2\"", "\x00\"", "", `\`,
			`\n` + strings.Repeat("z", 63) + `\t` + strings.Repeat("b", 33) + `é\r"`,
		} {
			checkJSONSpans(t, `"`+prefix+tail, "", budget, false)
		}
		for _, tail := range []string{"", "\"\\\n\r\b\f\t", "<>&", "é終😀\u2028\u2029", "\xff\xfe\xc2", "\x00\x1f", "\n" + strings.Repeat("z", 63) + "\té\r"} {
			checkJSONSpans(t, prefix+tail, strings.Repeat(" ", n%65), budget, true)
		}
	}
}

func TestJSONSpansPreserveAccounting(t *testing.T) {
	for _, stringify := range []bool{false, true} {
		for _, text := range []string{
			strings.Repeat("a", 4095),
			strings.Repeat("a", 63) + `\n` + strings.Repeat("z", 257),
			strings.Repeat(`\u0000`, 80),
			strings.Repeat("a", 129) + "<>&é終😀\u2028\u2029\xff",
		} {
			prefix := strings.Repeat(" ", 63)
			if !stringify {
				text = `"` + text + `"`
			}
			for _, startingSteps := range []int{0, 15} {
				budget := jsonSpanBudget{quota: 1 << 20, memory: 1 << 20, steps: startingSteps}
				full := jsonSpanExecute(text, prefix, budget, stringify, true)
				if full.Error.Type != "" {
					t.Fatalf("reference JSON string scan(%q, stringify=%t) = %+v, want success", text, stringify, full.Error)
				}
				for _, quota := range []int{1, 15, 16, 17, full.Steps - 1, full.Steps, full.Steps + 1} {
					if quota > 0 {
						budget.quota = quota
						checkJSONSpans(t, text, prefix, budget, stringify)
					}
				}
				budget.quota = 1 << 20
				lo, hi := 1, 1<<20
				for lo < hi {
					mid := lo + (hi-lo)/2
					budget.memory = mid
					if result := jsonSpanExecute(text, prefix, budget, stringify, true); result.Error.Type == "" {
						hi = mid
					} else {
						lo = mid + 1
					}
				}
				for _, memory := range []int{1, lo - 1, lo, lo + 1} {
					budget.memory = memory
					checkJSONSpans(t, text, prefix, budget, stringify)
				}
				budget.memory = 1 << 20
				for _, cancelAfter := range []int{1, 2, 3} {
					budget.cancelAfter = cancelAfter
					checkJSONSpans(t, text, prefix, budget, stringify)
				}
			}
		}
	}
}

func FuzzJSONSpans(f *testing.F) {
	for _, seed := range []string{"", "hello", "é終😀", "\xff\x00", strings.Repeat("a", 63) + `\n` + strings.Repeat("b", 65) + "é\xff"} {
		f.Add(seed)
	}
	f.Fuzz(func(t *testing.T, text string) {
		if len(text) > 4096 {
			t.Skip()
		}
		budget := jsonSpanBudget{quota: 1 << 20, memory: 16 << 20}
		checkJSONSpans(t, `"`+text+`"`, "", budget, false)
		checkJSONSpans(t, text, "prefix:", budget, true)
	})
}
