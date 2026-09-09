package runtime

import (
	"context"
	"errors"
	"fmt"
	"regexp"
	goruntime "runtime"
	"strings"
	"testing"
	"unsafe"
)

func matchDataCall(t *testing.T, method, text, pattern string) (BuiltinFunc, Value, []Value) {
	t.Helper()
	if method == "string" {
		receiver := NewString(text)
		member, err := stringMember(receiver, "match")
		if err != nil {
			t.Fatal(err)
		}
		return valueBuiltin(member).Fn, receiver, []Value{NewString(pattern)}
	}
	receiver := mustRegexValue(t, pattern)
	member, err := regexMember("match")
	if err != nil {
		t.Fatal(err)
	}
	return valueBuiltin(member).Fn, receiver, []Value{NewString(text)}
}

func TestMatchDataDetachesWindows(t *testing.T) {
	for _, method := range []string{"string", "regex"} {
		t.Run(method, func(t *testing.T) {
			text := strings.Repeat("p", 4096) + "é" + strings.Repeat("q", 4096)
			call, receiver, args := matchDataCall(t, method, text, `(?<letter>é)(?<empty>)(?<absent>z)?`)
			got, err := call(nil, receiver, args, nil, NewNil())
			if err != nil {
				t.Fatal(err)
			}
			entries := got.HashEntryMap()
			captures := entries["captures"].Array()
			if captures[0].String() != "é" || captures[1].String() != "" || !captures[2].IsNil() {
				t.Fatalf("captures = %s", entries["captures"].Inspect())
			}
			windows := []Value{entries[matchDataWholeKey], captures[0], entries["pre_match"], entries["post_match"]}
			sourceStart := uintptr(unsafe.Pointer(unsafe.StringData(text)))
			for _, window := range windows {
				start := uintptr(unsafe.Pointer(unsafe.StringData(window.String())))
				if start >= sourceStart && start < sourceStart+uintptr(len(text)) {
					t.Errorf("a %d-byte match window still retains its %d-byte subject", len(window.String()), len(text))
				}
			}
			named, ok := matchDataNamedCapture(got, "letter")
			if !ok || !sameStringBacking(named.String(), captures[0].String()) {
				t.Fatal("named and positional views must share the detached capture")
			}
			form, ok := got.ObjectStringForm()
			if !ok || !sameStringBacking(form, entries[matchDataWholeKey].String()) {
				t.Fatal("the tagged rendering must share the detached whole match")
			}
			goruntime.KeepAlive(text)
		})
	}
}

func TestMatchDataKeepsFullWindowBacking(t *testing.T) {
	for _, method := range []string{"string", "regex"} {
		t.Run(method, func(t *testing.T) {
			text := strings.Repeat("a", 64<<10)
			call, receiver, args := matchDataCall(t, method, text, `(?<whole>a+)`)
			exec := &Execution{ctx: context.Background(), quota: 1 << 30, memoryQuota: len(text) + 16<<10}
			got, err := call(exec, receiver, args, nil, NewNil())
			if err != nil {
				t.Fatal(err)
			}
			for _, window := range []Value{got.HashEntryMap()[matchDataWholeKey], got.HashEntryMap()["captures"].Array()[0]} {
				if !sameStringBacking(window.String(), text) {
					t.Fatal("a full-subject match must not copy its backing")
				}
			}
		})
	}
}

func TestMatchDataReservesCopiesBeforeAllocation(t *testing.T) {
	text := "b" + strings.Repeat("a", detachSubjectBytes)
	for _, method := range []string{"string", "regex"} {
		t.Run(method, func(t *testing.T) {
			call, receiver, args := matchDataCall(t, method, text, `(a+)`)
			// Warm the pattern cache before measuring the rejected result.
			if _, err := call(nil, receiver, args, nil, NewNil()); err != nil {
				t.Fatal(err)
			}
			exec := &Execution{ctx: context.Background(), quota: 1 << 30, memoryQuota: len(text) + len(text)/2}
			var before, after goruntime.MemStats
			goruntime.GC()
			goruntime.ReadMemStats(&before)
			_, err := call(exec, receiver, args, nil, NewNil())
			goruntime.ReadMemStats(&after)
			if !errors.Is(err, errMemoryQuotaExceeded) {
				t.Fatalf("got %v, want memory quota exceeded", err)
			}
			if allocated := after.TotalAlloc - before.TotalAlloc; allocated > uint64(len(text)/2) {
				t.Fatalf("rejected match allocated %d bytes before its quota check", allocated)
			}
			if exec.reservedScratchBytes != 0 {
				t.Fatalf("match left %d reserved bytes", exec.reservedScratchBytes)
			}
		})
	}
}

func TestMatchDataNestedCopiesConsumeSteps(t *testing.T) {
	text := "b" + strings.Repeat("a", 64<<10)
	pattern := strings.Repeat("(", 8) + "a+" + strings.Repeat(")", 8)
	for _, method := range []string{"string", "regex"} {
		t.Run(method, func(t *testing.T) {
			call, receiver, args := matchDataCall(t, method, text, pattern)
			exec := &Execution{ctx: context.Background(), quota: 2 * len(text) / stringScanBytesPerStep}
			_, err := call(exec, receiver, args, nil, NewNil())
			if !errors.Is(err, errStepQuotaExceeded) {
				t.Fatalf("got %v, want step quota exceeded for overlapping copies", err)
			}
		})
	}
}

func TestMatchDataNestedCopyQuota(t *testing.T) {
	text := "b" + strings.Repeat("a", 64<<10)
	pattern := strings.Repeat("(", 8) + "a+" + strings.Repeat(")", 8)
	for _, method := range []string{"string", "regex"} {
		t.Run(method, func(t *testing.T) {
			call, receiver, args := matchDataCall(t, method, text, pattern)
			exec := &Execution{ctx: context.Background(), quota: 1 << 30, memoryQuota: 4 * len(text)}
			_, err := call(exec, receiver, args, nil, NewNil())
			if !errors.Is(err, errMemoryQuotaExceeded) {
				t.Fatalf("got %v, want memory quota exceeded for all %d copies", err, regexp.MustCompile(pattern).NumSubexp()+1)
			}
		})
	}
}

func TestMatchDataProjectionCoversResult(t *testing.T) {
	for _, count := range []int{1, 32, 256} {
		for _, named := range []bool{false, true} {
			t.Run(fmt.Sprintf("groups=%d/named=%t", count, named), func(t *testing.T) {
				var pattern strings.Builder
				for i := range count {
					if named {
						fmt.Fprintf(&pattern, "(?<g%d>a)", i)
					} else {
						pattern.WriteString("(a)")
					}
				}
				text := "b" + strings.Repeat("a", count)
				re := regexp.MustCompile(pattern.String())
				indices := re.FindStringSubmatchIndex(text)
				receiver := NewString(text)
				got, err := newMatchData(nil, text, indices, re.SubexpNames(), receiver, nil, nil, NewNil())
				if err != nil {
					t.Fatal(err)
				}
				est := newMemoryEstimator()
				est.value(receiver)
				retained := est.value(got)
				projected, _ := matchDataAllocationBytes(text, indices, re.SubexpNames())
				if retained > projected {
					t.Fatalf("retained result costs %d bytes, exceeding the %d-byte construction projection", retained, projected)
				}
			})
		}
	}
}

func TestMatchDataBlockReleasesConstructionReservation(t *testing.T) {
	script := compileScriptWithConfig(t, Config{StepQuota: 1 << 30, MemoryQuotaBytes: 256 << 10}, `
def run(s)
  s.match(/(?<letter>a+)/) { |m| [m[:letter], m.begin(1), m.end(1)] }
end
`)
	text := "b" + strings.Repeat("a", 64<<10)
	got, err := script.Call(context.Background(), "run", []Value{NewString(text)}, CallOptions{})
	if err != nil {
		t.Fatal(err)
	}
	items := got.Array()
	if len(items) != 3 || items[0].String() != text[1:] || items[1].Int() != 1 || items[2].Int() != int64(len(text)) {
		t.Fatalf("match block returned %s, want the capture and rune offsets", got.Inspect())
	}
}
