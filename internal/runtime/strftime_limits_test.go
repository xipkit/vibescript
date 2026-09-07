package runtime

import (
	"context"
	"errors"
	"strings"
	"testing"
	"time"
)

func TestStrftimeAggregateLimits(t *testing.T) {
	tm := time.Date(2024, 1, 2, 3, 4, 5, 0, time.UTC)
	for _, format := range []string{strings.Repeat("%c", 4096), strings.Repeat("%A", 16384), strings.Repeat("x", 65536), strings.Repeat("%Q", 32768)} {
		exec := &Execution{ctx: context.Background(), quota: 1 << 30, memoryQuota: 32768}
		if _, err := strftime(exec, tm, format); !errors.Is(err, errMemoryQuotaExceeded) {
			t.Errorf("format prefix %q: error = %v, want memory quota exceeded", format[:2], err)
		}
	}
	for _, format := range []string{strings.Repeat("%c", 4096), strings.Repeat("x", 4096), "%" + strings.Repeat("-", 4096) + "d"} {
		exec := &Execution{ctx: context.Background(), quota: 8}
		if _, err := strftime(exec, tm, format); !errors.Is(err, errStepQuotaExceeded) {
			t.Errorf("format prefix %q: error = %v, want step quota exceeded", format[:2], err)
		}
	}
	for _, format := range []string{strings.Repeat("%c", maxFormatOutputBytes/24+1), "%1048577N", strings.Repeat("x", maxFormatOutputBytes+1)} {
		if _, err := strftime(nil, tm, format); !errors.Is(err, errOutputLimitExceeded) {
			t.Errorf("format prefix %q: error = %v, want output limit exceeded", format[:2], err)
		}
	}
}

func TestStrftimeExpansionRefusesBeforeAllocation(t *testing.T) {
	format := strings.Repeat("%c", 16384)
	exec := &Execution{ctx: context.Background(), quota: 1 << 30, memoryQuota: 1 << 16}
	var err error
	allocated := allocBytes(func() {
		_, err = strftime(exec, time.Unix(0, 0).UTC(), format)
	})
	if !errors.Is(err, errMemoryQuotaExceeded) {
		t.Fatalf("error = %v, want memory quota exceeded", err)
	}
	if allocated > 512<<10 {
		t.Fatalf("rejected expansion allocated %d bytes, want at most 512 KiB", allocated)
	}
}

func TestStrftimeCanceledAndOrdinary(t *testing.T) {
	ctx, cancel := context.WithCancel(context.Background())
	cancel()
	exec := &Execution{ctx: ctx, quota: 1 << 30}
	if _, err := strftime(exec, time.Unix(0, 0).UTC(), "%c"); !errors.Is(err, context.Canceled) {
		t.Fatalf("error = %v, want cancellation", err)
	}
	format := strings.Repeat("%c|", 20)
	tm := time.Date(2024, 1, 2, 3, 4, 5, 0, time.UTC)
	exec = &Execution{ctx: context.Background(), quota: 10000, memoryQuota: 32768}
	got, err := strftime(exec, tm, format)
	if err != nil || got != strings.Repeat("Tue Jan  2 03:04:05 2024|", 20) {
		t.Fatalf("ordinary compound result = %q, error = %v", got, err)
	}
}

func TestStrftimeLimitDispatch(t *testing.T) {
	for _, expr := range []string{
		`t.strftime(format)`,
		`t.strftime(*[format])`,
	} {
		script := compileScriptWithConfig(t, Config{StepQuota: 1 << 30, MemoryQuotaBytes: 32768}, `
def run(format)
  t = Time.utc(2024, 1, 2, 3, 4, 5)
  `+expr+`
end
`)
		requireRunMemoryQuotaError(t, script, []Value{NewString(strings.Repeat("%c", 4096))}, CallOptions{})
	}
}

func TestStrftimeCallRoots(t *testing.T) {
	tm := time.Unix(0, 0).UTC()
	format := "%" + strings.Repeat("-", 8192) + "d"
	exec := &Execution{ctx: context.Background(), quota: 1 << 30, memoryQuota: 4096}
	if _, err := callTimeStrftime(exec, tm, []Value{NewString(format)}, nil, NewNil()); !errors.Is(err, errMemoryQuotaExceeded) {
		t.Fatalf("temporary argument error = %v, want memory quota exceeded", err)
	}
	env := newEnv(nil)
	env.Define("retained", NewString(strings.Repeat("x", 8192)))
	exec = &Execution{ctx: context.Background(), quota: 1 << 30, memoryQuota: 4096}
	if _, err := callTimeStrftime(exec, tm, []Value{NewString("%d")}, nil, NewBlock(nil, nil, env)); !errors.Is(err, errMemoryQuotaExceeded) {
		t.Fatalf("block capture error = %v, want memory quota exceeded", err)
	}
}

func TestStrftimeZoneCaseAndBoundaries(t *testing.T) {
	for _, zone := range []string{"UTC", "éȺı", "A\xff", ""} {
		tm := time.Unix(0, 0).In(time.FixedZone(zone, 0))
		for _, format := range []string{"%Z", "%^Z", "%#Z", "%20Z", "%-20Z"} {
			tok, _ := scanStrftimeDirective(format, 0)
			want := zone
			if !tok.noPad && want != "" && tok.width > len(want) {
				want = strings.Repeat(" ", tok.width-len(want)) + want
			}
			want = applyCase(want, directiveCase(tok, false))
			got, err := strftime(nil, tm, format)
			if err != nil || got != want {
				t.Errorf("zone %q, format %q: got %q, error %v, want %q", zone, format, got, err, want)
			}
		}
	}
	tm := time.Unix(0, 0).In(time.FixedZone(strings.Repeat("A", maxFormatOutputBytes), 0))
	if got, err := strftime(nil, tm, "%^Z"); err != nil || len(got) != maxFormatOutputBytes {
		t.Fatalf("exact-cap zone length = %d, error = %v", len(got), err)
	}
	for _, format := range []string{"%1048576N", "%1048576F", "%1048576Y", "%1048576z", strings.Repeat("x", maxFormatOutputBytes)} {
		got, err := strftime(nil, time.Unix(0, 0).UTC(), format)
		if err != nil || len(got) != maxFormatOutputBytes {
			t.Errorf("format prefix %q: exact-cap length = %d, error = %v", format[:2], len(got), err)
		}
	}
}

func BenchmarkStrftimeCompounds(b *testing.B) {
	format := strings.Repeat("%c", 1024)
	tm := time.Unix(0, 0).UTC()
	b.ReportAllocs()
	for b.Loop() {
		if _, err := strftime(nil, tm, format); err != nil {
			b.Fatal(err)
		}
	}
}

func TestStrftimeOverflowWidthDoesNotCopyInput(t *testing.T) {
	format := "%" + strings.Repeat("1", 65536) + "d"
	exec := &Execution{ctx: context.Background(), quota: 1 << 30, memoryQuota: len(format) + 4096}
	var got string
	var err error
	allocated := allocBytes(func() { got, err = strftime(exec, time.Date(2024, 1, 2, 0, 0, 0, 0, time.UTC), format) })
	if err != nil || got != "02" {
		t.Fatalf("overflow width: got %q, error %v", got, err)
	}
	if allocated > 8192 {
		t.Fatalf("overflow width allocated %d bytes, want at most 8 KiB", allocated)
	}
}

func TestStrftimeCompoundPeakCapacity(t *testing.T) {
	format := strings.Repeat("x", 24) + "%F"
	tm := time.Date(2024, 1, 2, 0, 0, 0, 0, time.UTC)
	exec := &Execution{ctx: context.Background(), quota: 1 << 30, memoryQuota: 1 << 20}
	base := exec.hashCallRootBytes(NewTime(tm), []Value{NewString(format)}, nil, NewNil()) + estimatedValueBytes + estimatedStringHeaderBytes
	exec.memoryQuota = base + 98
	if _, err := strftime(exec, tm, format); !errors.Is(err, errMemoryQuotaExceeded) {
		t.Fatalf("compound peak error = %v, want memory quota exceeded", err)
	}
	exec = &Execution{ctx: context.Background(), quota: 1 << 30, memoryQuota: base + 112}
	if got, err := strftime(exec, tm, format); err != nil || got != strings.Repeat("x", 24)+"2024-01-02" {
		t.Fatalf("compound within peak budget = %q, error = %v", got, err)
	}
}

func TestStrftimePaddedUnicodeRefusesBeforeAllocation(t *testing.T) {
	tm := time.Unix(0, 0).In(time.FixedZone("ȿ", 0))
	var err error
	allocated := allocBytes(func() {
		_, err = strftime(nil, tm, "%^1048576Z")
	})
	if !errors.Is(err, errOutputLimitExceeded) {
		t.Fatalf("error = %v, want output limit exceeded", err)
	}
	if allocated > 8192 {
		t.Fatalf("rejected padded zone allocated %d bytes, want at most 8 KiB", allocated)
	}
}

func TestStrftimeUnchangedGoLayoutWithinOutputLimit(t *testing.T) {
	tm := time.Date(2006, 1, 2, 15, 4, 5, 0, time.UTC)
	format := strings.Repeat("2006", 8193)
	got, err := callTimeStrftime(nil, tm, []Value{NewString(format)}, nil, NewNil())
	if err != nil || got.String() != format {
		t.Fatalf("unchanged layout length = %d, error = %v", len(got.String()), err)
	}
}

func TestStrftimeUnicodeProjectionPollsCancellation(t *testing.T) {
	for _, shift := range []caseFlag{caseUpper, caseToggle} {
		ctx := &jsonCancelContext{Context: context.Background(), done: make(chan struct{})}
		r := strftimeRenderer{budget: &strftimeBudget{exec: &Execution{ctx: ctx, quota: 1 << 30}}}
		if _, err := r.caseSize("A"+strings.Repeat("ȿ", 16384), shift); !errors.Is(err, context.Canceled) {
			t.Fatalf("case projection error = %v, want cancellation", err)
		}
	}
}
