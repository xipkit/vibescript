package runtime

import (
	"bytes"
	"context"
	"fmt"
	"runtime"
	"strings"
	"testing"
	"time"

	"github.com/mgomes/vibescript/vibes/value"
)

// minStepsForStringOp binary-searches the smallest step quota at which one
// expression over a haystack of the given size completes.
func minStepsForStringOp(t *testing.T, expr string, bytes int) int {
	t.Helper()

	hay := NewString(strings.Repeat("ab", bytes/2))
	src := fmt.Sprintf("def run(s)\n  %s\nend", expr)

	lo, hi := 1, bytes+10000
	for lo < hi {
		mid := (lo + hi) / 2
		script := compileScriptWithConfig(t, Config{StepQuota: mid, MemoryQuotaBytes: Unlimited}, src)
		if _, err := script.Call(context.Background(), "run", []Value{hay}, CallOptions{}); err != nil {
			lo = mid + 1
		} else {
			hi = mid
		}
	}
	return lo
}

// A string method's work grows with its receiver but it dispatches as one call,
// so charging a flat handful of steps let a script scan a host-supplied string
// of any size on a constant budget. Repeatedly upcasing an 800 KB string burned
// five minutes inside the default 1M-step profile before the quota fired
// (#1131). The charge is amortized at one step per stringScanBytesPerStep, so a
// receiver eight times longer costs meaningfully more rather than the same.
func TestStringOpsChargeStepsPerByte(t *testing.T) {
	t.Parallel()

	ops := []string{
		"s.upcase.length",
		"s.downcase.length",
		"s.reverse.length",
		"s.gsub(\"zzz\", \"y\").length",
		"s.sub(\"zzz\", \"y\").length",
		"s.split(\"z\").length",
		"s.scan(\"zzz\").length",
		"s.count(\"z\")",
		"s.index(\"zzz\").inspect",
		"s.rindex(\"zzz\").inspect",
		"s.include?(\"zzz\").inspect",
		"s.chars.length",
		"s.length",
	}

	const small, large = 8 << 10, 64 << 10
	for _, expr := range ops {
		t.Run(expr, func(t *testing.T) {
			t.Parallel()
			atSmall := minStepsForStringOp(t, expr, small)
			atLarge := minStepsForStringOp(t, expr, large)

			// An 8x receiver charges 8x the scan. Require 4x so the bound tracks
			// proportionality rather than the exact rate, while staying far above
			// the flat charge this replaces.
			if atLarge < atSmall*4 {
				t.Errorf("%s cost %d steps over %d bytes and %d over %d; an eight times "+
					"longer receiver must cost meaningfully more, so the scan is still "+
					"charged as a constant", expr, atSmall, small, atLarge, large)
			}
		})
	}
}

// Methods whose work does not grow with the receiver stay exempt, so reading a
// length or emptiness off a large string does not consume the quota. The
// exemption list is what keeps the charge from taxing constant-cost accessors.
func TestConstantCostStringOpsStayUncharged(t *testing.T) {
	t.Parallel()

	// No expression here renders its result: rendering is charged for the bytes
	// it prints (see TestSymbolRenderingChargesForItsName), so an .inspect tail
	// would measure the tail rather than the method under test.
	exempt := []string{
		"s.bytesize", "s.empty?", "s.getbyte(0)",
		// A byte-indexed slice bills the bytes it copies rather than the
		// receiver it never reads, a symbol holds the receiver's string header
		// without copying it, and replace ignores the receiver and returns its
		// argument.
		"s.byteslice(0, 1)", "s.to_sym", "s.intern", "s.replace(\"x\")",
	}
	for _, expr := range exempt {
		t.Run(expr, func(t *testing.T) {
			t.Parallel()
			atSmall := minStepsForStringOp(t, expr, 8<<10)
			atLarge := minStepsForStringOp(t, expr, 64<<10)
			if atLarge != atSmall {
				t.Errorf("%s cost %d steps over 8 KiB and %d over 64 KiB; a constant-cost "+
					"method must not be charged for its receiver's length", expr, atSmall, atLarge)
			}
		})
	}
}

// The amortized rate must leave ordinary short strings costing nothing extra: a
// receiver below the per-step byte budget rounds down to no charge at all, so
// two sub-budget sizes cost exactly the same.
func TestShortStringOpsPayNoScanCharge(t *testing.T) {
	t.Parallel()

	for _, expr := range []string{"s.upcase.length", "s.index(\"zzz\").inspect", "s.split(\"z\").length"} {
		t.Run(expr, func(t *testing.T) {
			t.Parallel()
			// Sized so the receiver and its arguments together stay under the
			// per-step budget: the charge covers both, so a receiver just below
			// the budget plus a needle crosses it legitimately.
			tiny := minStepsForStringOp(t, expr, 2)
			nearBudget := minStepsForStringOp(t, expr, stringScanBytesPerStep/2)
			if tiny != nearBudget {
				t.Errorf("%s cost %d steps over 2 bytes and %d over %d; a call whose "+
					"receiver and arguments together fall below the per-step byte budget "+
					"must round down to no scan charge, so short strings are unaffected "+
					"by the rate", expr, tiny, nearBudget, stringScanBytesPerStep/2)
			}
		})
	}
}

// A symbol built from a host-supplied string carries that whole string as its
// name, and rendering it scans every byte. InspectByteLenBounded steps per
// element, so a scalar symbol was charged one step no matter how long its name
// -- which let s.to_sym.inspect do the linear work the scan charge exists to
// bound, through a receiver the exemption list had just made free.
func TestSymbolRenderingChargesForItsName(t *testing.T) {
	t.Parallel()

	// Every way a value can be turned into text, not just the one that was
	// reported: inspect, interpolation, to_s, puts, and join each render the
	// name and each must charge for it. Fixing only the site in front of you is
	// how the previous round left interpolation open.
	renderings := []string{
		"s.to_sym.inspect.bytesize",
		"s.intern.inspect.bytesize",
		"\"#{s.to_sym}\".bytesize",
		"[s.to_sym].join(\",\").bytesize",
		"[s.to_sym].to_s.bytesize",
		"{a: s.to_sym}.to_s.bytesize",
	}
	for _, expr := range renderings {
		t.Run(expr, func(t *testing.T) {
			t.Parallel()
			atSmall := minStepsForStringOp(t, expr, 8<<10)
			atLarge := minStepsForStringOp(t, expr, 64<<10)
			if atLarge < atSmall*4 {
				t.Errorf("%s cost %d steps over 8 KiB and %d over 64 KiB; rendering a "+
					"symbol must be charged for the name it prints, or exempting the "+
					"conversion reopens the unmetered scan", expr, atSmall, atLarge)
			}
		})
	}
}

// The direct-call fast path must be indistinguishable from member dispatch, so
// its scan charge lands after the arguments are evaluated. Charging first let a
// quota that trips on the receiver skip an argument's side effect on this path
// while the same call through dispatch still performed it.
//
// The argument raises, so which error surfaces says which ran first: the
// argument's own error means it was evaluated, a quota error means the charge
// preempted it. That needs no side channel, and a host-passed container is not
// one -- mutations a script makes to it are not visible to the caller.
func TestDirectStringCallsEvaluateArgumentsBeforeCharging(t *testing.T) {
	t.Parallel()

	calls := map[string]string{
		"index":  "s.index(boom())",
		"rindex": "s.rindex(boom())",
		"split":  "s.split(boom())",
		"slice":  "s.slice(boom(), 1)",
	}
	names := []string{"index", "rindex", "split", "slice"}

	for _, name := range names {
		t.Run(name, func(t *testing.T) {
			t.Parallel()

			src := "def boom()\n  raise \"ARGRAN\"\nend\ndef run(s)\n  " + calls[name] + "\nend"
			hay := NewString(strings.Repeat("ab", (64<<10)/2))

			// Quotas spanning far below the receiver scan's cost up to well above
			// it: the argument runs first at every one of them.
			for _, quota := range []int{40, 60, 200, 2000} {
				script := compileScriptWithConfig(t, Config{StepQuota: quota, MemoryQuotaBytes: Unlimited}, src)
				_, err := script.Call(context.Background(), "run", []Value{hay}, CallOptions{})
				if err == nil {
					t.Fatalf("%s did not raise at %d steps", calls[name], quota)
				}
				if !strings.Contains(err.Error(), "ARGRAN") {
					t.Errorf("%s failed with %q at %d steps rather than the argument's own "+
						"error; the fast path charged for the receiver before evaluating the "+
						"argument, which member dispatch does not do", calls[name], err, quota)
				}
			}
		})
	}
}

// format sizes its output per verb, and the hexadecimal branch measures a
// string or symbol input itself rather than going through the shared
// string-bytes projection, so it did not inherit that projection's charge.
// Every verb that consumes the whole input is covered here, not just the one
// that was reported.
func TestFormatChargesForTheInputItRenders(t *testing.T) {
	t.Parallel()

	verbs := []string{
		// The pattern itself is scanned for verbs and its literal text copied,
		// so a host-supplied format string costs its own length even with no
		// arguments. This was the worst case found: format(f) with a 512 KB
		// pattern ran for 1m46s inside the default profile and the quota never
		// fired at all.
		"format(s).bytesize",
		"format(\"%x\", s).bytesize",
		"format(\"%X\", s).bytesize",
		"format(\"%s\", s).bytesize",
		"format(\"%q\", s).bytesize",
		"format(\"%x\", s.to_sym).bytesize",
	}
	for _, expr := range verbs {
		t.Run(expr, func(t *testing.T) {
			t.Parallel()
			atSmall := minStepsForStringOp(t, expr, 8<<10)
			atLarge := minStepsForStringOp(t, expr, 64<<10)
			if atLarge < atSmall*4 {
				t.Errorf("%s cost %d steps over 8 KiB and %d over 64 KiB; formatting must "+
					"charge for the input it renders", expr, atSmall, atLarge)
			}
		})
	}
}

// Searching with the default offset must charge for the receiver before
// scanning it, including when the search no longer needs a preliminary count.
func TestRindexDefaultOffsetScanRunsAfterCharging(t *testing.T) {
	t.Parallel()

	hay := NewString(strings.Repeat("ab", (256<<10)/2))
	src := "def run(s)\n  s.rindex(\"zzz\")\nend"

	// A quota far too small for the receiver scan must reject the call.
	script := compileScriptWithConfig(t, Config{StepQuota: 8, MemoryQuotaBytes: Unlimited}, src)
	if _, err := script.Call(context.Background(), "run", []Value{hay}, CallOptions{}); err == nil {
		t.Fatal("rindex over a 256 KiB receiver completed on an 8-step quota")
	}

	// Supplying an offset costs at most the receiver charge plus evaluating
	// the extra argument.
	withDefault := minStepsForStringOp(t, "s.rindex(\"zzz\").inspect", 64<<10)
	withExplicit := minStepsForStringOp(t, "s.rindex(\"zzz\", 10).inspect", 64<<10)
	if withExplicit > withDefault+2 {
		t.Errorf("rindex cost %d steps with an explicit offset and %d with the default; "+
			"an explicit offset replaces the default rather than adding to it",
			withExplicit, withDefault)
	}
}

// A short receiver with a large argument moves just as many bytes as the
// reverse, so the charge covers string arguments copied into the result, not
// only the receiver.
func TestStringArgumentsAreChargedToo(t *testing.T) {
	t.Parallel()

	// The receiver is a small literal; the host-supplied string is the argument.
	ops := []string{
		"\"\".concat(s).bytesize",
		"\"\".prepend(s).bytesize",
		"\"ab\".insert(1, s).bytesize",
		"\"ab\".sub(\"a\", s).bytesize",
	}
	for _, expr := range ops {
		t.Run(expr, func(t *testing.T) {
			t.Parallel()
			atSmall := minStepsForStringOp(t, expr, 8<<10)
			atLarge := minStepsForStringOp(t, expr, 64<<10)
			if atLarge < atSmall*4 {
				t.Errorf("%s cost %d steps over an 8 KiB argument and %d over 64 KiB; "+
					"bytes copied out of an argument must be charged like bytes copied "+
					"out of the receiver", expr, atSmall, atLarge)
			}
		})
	}
}

// The String % operator reaches the shared formatting path straight from the
// evaluator, without passing through the format builtin. Charging the pattern
// at the builtin alone left this entrance unmetered, so the charge belongs on
// the shared path both of them reach.
func TestFormatOperatorChargesLikeTheBuiltin(t *testing.T) {
	t.Parallel()

	for _, expr := range []string{
		"(s % []).bytesize",
		"(\"%.65536s\" % [s]).bytesize",
		"format(\"%.65536s\", s).bytesize",
		"format(\"%.65536q\", s).bytesize",
	} {
		t.Run(expr, func(t *testing.T) {
			t.Parallel()
			atSmall := minStepsForStringOp(t, expr, 8<<10)
			atLarge := minStepsForStringOp(t, expr, 64<<10)
			if atLarge < atSmall*4 {
				t.Errorf("%s cost %d steps over 8 KiB and %d over 64 KiB; every entrance "+
					"to formatting must charge for the bytes it scans", expr, atSmall, atLarge)
			}
		})
	}
}

// minStepsForWidth binary-searches the smallest step quota at which a padding
// call of the given width completes. Padding scales with a number rather than
// with its inputs, so the size that has to vary is the width.
func minStepsForWidth(t *testing.T, expr string, width int) int {
	t.Helper()

	src := fmt.Sprintf("def run(s)\n  %s\nend", fmt.Sprintf(expr, width))
	lo, hi := 1, width/stringScanBytesPerStep+10000
	for lo < hi {
		mid := (lo + hi) / 2
		script := compileScriptWithConfig(t, Config{StepQuota: mid, MemoryQuotaBytes: Unlimited}, src)
		if _, err := script.Call(context.Background(), "run", []Value{NewString("")}, CallOptions{}); err != nil {
			lo = mid + 1
		} else {
			hi = mid
		}
	}
	return lo
}

// Padding and repetition take their cost from a number rather than from their
// inputs: "".ljust(8_000_000, "x") and "x" * 8_000_000 each write eight million
// bytes from inputs of a few bytes, so a charge based on receiver and argument
// lengths sees nothing at all. They ran for 8.5s and 7.6s inside the default
// step profile, both completing without the quota ever firing.
func TestPaddingChargesForTheBytesItWrites(t *testing.T) {
	t.Parallel()

	for _, expr := range []string{
		"\"\".ljust(%d, \"x\").bytesize",
		"\"\".rjust(%d, \"x\").bytesize",
		"\"\".center(%d, \"x\").bytesize",
		// String#* is the same shape: a short receiver and a script-chosen
		// count. It ran 7.6s inside the default profile and completed.
		"(\"x\" * %d).bytesize",
	} {
		t.Run(expr, func(t *testing.T) {
			t.Parallel()
			const small, large = 64 << 10, 512 << 10
			atSmall := minStepsForWidth(t, expr, small)
			atLarge := minStepsForWidth(t, expr, large)
			if atLarge < atSmall*4 {
				t.Errorf("%s cost %d steps at width %d and %d at width %d; padding must "+
					"charge for the bytes it writes, which its inputs do not bound",
					expr, atSmall, small, atLarge, large)
			}
		})
	}
}

// Counting runes walks the whole value however small the field that will hold
// them, so a precision or width cannot bound it. format("%1.1s", s) charged for
// precision 1 while scanning a multi-megabyte receiver in full.
func TestPrecisionWidthFormattingChargesTheFullScan(t *testing.T) {
	t.Parallel()

	for _, expr := range []string{
		"format(\"%1.1s\", s).bytesize",
		"format(\"%2.2s\", s).bytesize",
	} {
		t.Run(expr, func(t *testing.T) {
			t.Parallel()
			atSmall := minStepsForStringOp(t, expr, 8<<10)
			atLarge := minStepsForStringOp(t, expr, 64<<10)
			if atLarge < atSmall*4 {
				t.Errorf("%s cost %d steps over 8 KiB and %d over 64 KiB; a narrow field "+
					"does not bound the rune count, so the traversal must be charged",
					expr, atSmall, atLarge)
			}
		})
	}
}

// A method that only matches its arguments against the receiver cannot inspect
// more of one than the receiver holds, so a large argument it never reads must
// not exhaust the quota: "a".start_with?("a", huge) returns on the first
// prefix. Charging every string argument in full made that call fail instead of
// answering true.
func TestComparisonArgumentsAreCappedByTheReceiver(t *testing.T) {
	t.Parallel()

	// The receiver is two bytes; the host-supplied string is a later argument
	// the method never needs to read.
	calls := []string{
		"\"ab\".start_with?(\"ab\", s).inspect",
		"\"ab\".end_with?(\"ab\", s).inspect",
		"\"ab\".include?(s).inspect",
		"\"ab\".index(s).inspect",
	}
	for _, expr := range calls {
		t.Run(expr, func(t *testing.T) {
			t.Parallel()
			atSmall := minStepsForStringOp(t, expr, 8<<10)
			atLarge := minStepsForStringOp(t, expr, 512<<10)
			if atLarge != atSmall {
				t.Errorf("%s cost %d steps with an 8 KiB argument and %d with 512 KiB; a "+
					"two-byte receiver bounds what these can inspect, so a larger argument "+
					"must not cost more", expr, atSmall, atLarge)
			}
		})
	}

	// The cap must not reach methods that copy an argument into the result.
	for _, expr := range []string{"\"ab\".concat(s).bytesize", "\"ab\".sub(\"a\", s).bytesize"} {
		t.Run("copies "+expr, func(t *testing.T) {
			t.Parallel()
			atSmall := minStepsForStringOp(t, expr, 8<<10)
			atLarge := minStepsForStringOp(t, expr, 64<<10)
			if atLarge < atSmall*4 {
				t.Errorf("%s cost %d steps over 8 KiB and %d over 64 KiB; an argument "+
					"copied into the result is not bounded by the receiver", expr, atSmall, atLarge)
			}
		})
	}
}

// The receiver bounds only what a method compares against it. An argument the
// method preprocesses on its own -- a character set count parses byte by byte,
// a pattern match compiles the whole thing -- is not bounded by the receiver,
// so capping those at its length let "".count(s) parse a large argument for
// almost nothing. It ran 1.7s inside the default profile without the quota
// firing.
func TestPreprocessedArgumentsAreNotCappedByTheReceiver(t *testing.T) {
	t.Parallel()

	for _, expr := range []string{
		"\"\".count(s)",
		"\"\".split(s).length",
	} {
		t.Run(expr, func(t *testing.T) {
			t.Parallel()
			atSmall := minStepsForStringOp(t, expr, 8<<10)
			atLarge := minStepsForStringOp(t, expr, 64<<10)
			if atLarge < atSmall*4 {
				t.Errorf("%s cost %d steps over 8 KiB and %d over 64 KiB; an argument the "+
					"method preprocesses is not bounded by the receiver and must not be "+
					"capped there", expr, atSmall, atLarge)
			}
		})
	}
}

// A format argument that is not itself a string is walked one step per node,
// which bounds its shape but not its size: a large string nested in a
// one-element array is a single node. The rendered payload is what gets
// materialized, so that is what must be charged.
func TestAggregateFormatArgumentsChargeRenderedBytes(t *testing.T) {
	t.Parallel()

	for _, expr := range []string{
		"format(\"%s\", [s]).bytesize",
		"format(\"%s\", {a: s}).bytesize",
		// A width alongside a precision needs the value's rune count, which
		// traverses the whole nested string however narrow the field.
		"format(\"%1.1s\", [s]).bytesize",
		"format(\"%2.2s\", {a: s}).bytesize",
	} {
		t.Run(expr, func(t *testing.T) {
			t.Parallel()
			atSmall := minStepsForStringOp(t, expr, 8<<10)
			atLarge := minStepsForStringOp(t, expr, 64<<10)
			if atLarge < atSmall*4 {
				t.Errorf("%s cost %d steps over 8 KiB and %d over 64 KiB; the bytes an "+
					"aggregate renders to must be charged, not just its node count",
					expr, atSmall, atLarge)
			}
		})
	}
}

// A precision with no width is the case that genuinely does not scan: the
// projection walks the value only up to the precision's byte limit and stops,
// so the cost is bounded by the field rather than by the argument. Pinned as a
// control, so the charges above are not mistaken for a rule that every
// aggregate format must scale.
func TestPrecisionOnlyAggregateFormatStaysBounded(t *testing.T) {
	t.Parallel()

	for _, expr := range []string{
		"format(\"%.4s\", [s]).bytesize",
		"format(\"%.4s\", {a: s}).bytesize",
	} {
		t.Run(expr, func(t *testing.T) {
			t.Parallel()
			atSmall := minStepsForStringOp(t, expr, 8<<10)
			atLarge := minStepsForStringOp(t, expr, 64<<10)
			if atLarge != atSmall {
				t.Errorf("%s cost %d steps over 8 KiB and %d over 64 KiB; a precision "+
					"without a width stops the walk at its own limit, so the argument's "+
					"size must not matter", expr, atSmall, atLarge)
			}
		})
	}
}

// The aggregate rune walk visits bytes but reports runes, so charging the rune
// count directly under-charged multibyte text by up to four times: the same
// 64 KiB of four-byte characters traverses as many bytes as 64 KiB of ASCII
// while counting a quarter as many runes. The count is scaled to its widest
// encoding, which never charges fewer steps than the bytes traversed deserve.
//
// This over-charges ASCII on the same path, and that is the deliberate side of
// the trade: the alternative is a second full traversal to learn the exact byte
// length, and under-charging a scan is what this metering exists to prevent.
func TestAggregateRuneWalkNeverUnderchargesMultibyte(t *testing.T) {
	t.Parallel()

	const bytes = 64 << 10
	texts := map[string]string{
		"ascii":     strings.Repeat("a", bytes),
		"four-byte": strings.Repeat("\U0001F600", bytes/4),
		"two-byte":  strings.Repeat("\u00e9", bytes/2),
	}
	for name, text := range texts {
		t.Run(name, func(t *testing.T) {
			t.Parallel()
			src := "def run(s)\n  format(\"%1.1s\", [s]).bytesize\nend"
			lo, hi := 1, 1<<20
			for lo < hi {
				mid := (lo + hi) / 2
				script := compileScriptWithConfig(t, Config{StepQuota: mid, MemoryQuotaBytes: Unlimited}, src)
				if _, err := script.Call(context.Background(), "run", []Value{NewString(text)}, CallOptions{}); err != nil {
					lo = mid + 1
				} else {
					hi = mid
				}
			}
			if floor := len(text) / stringScanBytesPerStep; lo < floor {
				t.Errorf("%s text of %d bytes cost %d steps, below the %d its traversal "+
					"deserves at one step per %d bytes; a rune count must not stand in for "+
					"a byte count", name, len(text), lo, floor, stringScanBytesPerStep)
			}
		})
	}
}

// A width writes bytes that neither the pattern nor the arguments account for:
// format("%1000000s", "") produces a megabyte from a few input bytes. The
// per-call output cap bounds one call, not a loop of them, so the projected
// output has to be charged the way padding and repetition are.
func TestFormatChargesForTheOutputItWrites(t *testing.T) {
	t.Parallel()

	for _, expr := range []string{
		"format(\"%%%ds\", \"\").bytesize",
		"format(\"%%-%ds\", \"\").bytesize",
		"(\"%%%ds\" %% [\"\"]).bytesize",
	} {
		t.Run(expr, func(t *testing.T) {
			t.Parallel()
			const small, large = 8 << 10, 64 << 10
			atSmall := minStepsForWidth(t, expr, small)
			atLarge := minStepsForWidth(t, expr, large)
			if atLarge < atSmall*4 {
				t.Errorf("%s cost %d steps at width %d and %d at width %d; a width writes "+
					"bytes the inputs do not bound, so the projected output must be charged",
					expr, atSmall, small, atLarge, large)
			}
		})
	}
}

// Both entrances to a direct-call method must meter the same arguments. The
// fast path metered only the needle while dispatch metered every argument, so
// the two drifted by the size of the offset argument.
//
// The splatted form costs a few steps more for building its own argument array,
// so the assertion is that the gap does not grow with the offset: a metering
// difference scales with it, a construction difference does not.
func TestDirectStringCallsMeterEveryArgument(t *testing.T) {
	t.Parallel()

	hay := NewString(strings.Repeat("ab", (64<<10)/2))
	direct := "def run(s, bad)\n  s.index(\"x\", bad)\nend"
	splat := "def run(s, bad)\n  s.index(*[\"x\", bad])\nend"

	// A string in the offset position is invalid; reaching that error means
	// metering let the call through.
	minFor := func(source string, bad Value) int {
		lo, hi := 1, 1<<20
		for lo < hi {
			mid := (lo + hi) / 2
			script := compileScriptWithConfig(t, Config{StepQuota: mid, MemoryQuotaBytes: Unlimited}, source)
			_, err := script.Call(context.Background(), "run", []Value{hay, bad}, CallOptions{})
			if err != nil && strings.Contains(err.Error(), "quota") {
				lo = mid + 1
			} else {
				hi = mid
			}
		}
		return lo
	}

	gapFor := func(size int) int {
		bad := NewString(strings.Repeat("z", size))
		return minFor(splat, bad) - minFor(direct, bad)
	}

	small, large := gapFor(8<<10), gapFor(256<<10)
	if small != large {
		t.Errorf("the splatted form cost %d steps more than the direct one with an 8 KiB "+
			"offset and %d more with 256 KiB; a gap that grows with the argument means "+
			"the two entrances meter different things", small, large)
	}
}

// Every member of the capped set, exercised. Membership asserts that a method
// cannot inspect more of an argument than the receiver holds, and that is a
// claim about its implementation rather than its shape: count parses its
// character set byte by byte, casecmp? validates its argument's UTF-8 before
// comparing, and both looked like comparisons from the outside. Each was capped
// on that resemblance and each under-charged until it was reported.
//
// So the whole set is tested rather than the members someone happened to
// question: a two-byte receiver with a growing argument must cost the same at
// every size.
//
// Note what this does and does not prove. It shows the *charge* does not follow
// the argument, which is the property the cap claims. It cannot show the *work*
// does not, because unmetered work costs no steps by definition -- index and
// rindex passed this test while scanning whole needles through stringIsASCII,
// and only wall-clock revealed it. The other half of the claim rests on each
// implementation rejecting an oversized argument before reading it, which is
// reviewed per method rather than enforced here.
func TestCappedArgumentsAreActuallyBoundedByTheReceiver(t *testing.T) {
	t.Parallel()

	// One call per capped method, each passing the host string where the method
	// takes one.
	capped := map[string]string{
		"start_with?": "\"ab\".start_with?(s).inspect",
		"end_with?":   "\"ab\".end_with?(s).inspect",
		"include?":    "\"ab\".include?(s).inspect",
		"index":       "\"ab\".index(s).inspect",
		"rindex":      "\"ab\".rindex(s).inspect",
		"casecmp":     "\"ab\".casecmp(s).inspect",
		"partition":   "\"ab\".partition(s).length",
		"rpartition":  "\"ab\".rpartition(s).length",
		"slice":       "\"ab\".slice(s).inspect",
		"between?":    "\"ab\".between?(\"aa\", s).inspect",
	}
	names := make([]string, 0, len(capped))
	for name := range capped {
		names = append(names, name)
	}
	for _, name := range names {
		t.Run(name, func(t *testing.T) {
			t.Parallel()
			atSmall := minStepsForStringOp(t, capped[name], 8<<10)
			atLarge := minStepsForStringOp(t, capped[name], 512<<10)
			if atSmall != atLarge {
				t.Errorf("%s cost %d steps with an 8 KiB argument and %d with 512 KiB; a "+
					"method is only in the capped set if the receiver bounds what it can "+
					"inspect, so its cost must not follow the argument", capped[name],
					atSmall, atLarge)
			}
		})
	}
}

// casecmp? validates its argument's UTF-8 in full before comparing, so the
// receiver bounds nothing. It resembles casecmp, which stops at the shorter
// operand and stays capped -- the distinction is in the implementation, not the
// name.
func TestCasecmpPredicateChargesItsWholeArgument(t *testing.T) {
	t.Parallel()

	atSmall := minStepsForStringOp(t, "\"ab\".casecmp?(s).inspect", 8<<10)
	atLarge := minStepsForStringOp(t, "\"ab\".casecmp?(s).inspect", 64<<10)
	if atLarge < atSmall*4 {
		t.Errorf("casecmp? cost %d steps over an 8 KiB argument and %d over 64 KiB; it "+
			"scans the whole argument to validate it, so the receiver cannot cap the "+
			"charge", atSmall, atLarge)
	}
}

// A substitution can expand well past its inputs: replacing every byte of a
// fixed receiver with a growing replacement writes the product of the two, so
// neither the receiver nor the replacement bounds the result. The receiver here
// is a literal, so only the replacement grows.
func TestSubstitutionChargesForTheResultItBuilds(t *testing.T) {
	t.Parallel()

	const receiver = "\"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\""
	for _, expr := range []string{
		receiver + ".gsub(\"a\", s).bytesize",
		receiver + ".sub(\"a\", s).bytesize",
	} {
		t.Run(expr, func(t *testing.T) {
			t.Parallel()
			atSmall := minStepsForStringOp(t, expr, 8<<10)
			atLarge := minStepsForStringOp(t, expr, 64<<10)
			if atLarge < atSmall*4 {
				t.Errorf("%s cost %d steps with an 8 KiB replacement and %d with 64 KiB; "+
					"a substitution must charge for what it writes, which its inputs do "+
					"not bound", expr, atSmall, atLarge)
			}
		})
	}
}

// A template's payload arrives inside its context hash rather than as a string
// argument, so the per-call charge never saw it: the render copied the whole
// value for the cost of a tiny receiver, and the quota never fired at all.
func TestTemplateChargesForItsRenderedContext(t *testing.T) {
	t.Parallel()

	for _, expr := range []string{
		"\"{{v}}\".template({v: s}).bytesize",
		"\"{{a}}{{b}}\".template({a: s, b: s}).bytesize",
	} {
		t.Run(expr, func(t *testing.T) {
			t.Parallel()
			atSmall := minStepsForStringOp(t, expr, 8<<10)
			atLarge := minStepsForStringOp(t, expr, 64<<10)
			if atLarge < atSmall*4 {
				t.Errorf("%s cost %d steps over an 8 KiB context value and %d over 64 KiB; "+
					"a value reached through a context hash is copied just as a string "+
					"argument is", expr, atSmall, atLarge)
			}
		})
	}
}

// The block form of a substitution expands exactly as the replacement form
// does -- a block returning a large string writes it once per match -- but it
// returned ahead of the output charge, so the quota never fired.
func TestBlockSubstitutionChargesForItsOutput(t *testing.T) {
	t.Parallel()

	for _, expr := range []string{
		"\"aaaa\".gsub(\"a\") { s }.bytesize",
		"\"aaaa\".sub(\"a\") { s }.bytesize",
	} {
		t.Run(expr, func(t *testing.T) {
			t.Parallel()
			atSmall := minStepsForStringOp(t, expr, 8<<10)
			atLarge := minStepsForStringOp(t, expr, 64<<10)
			if atLarge < atSmall*4 {
				t.Errorf("%s cost %d steps with an 8 KiB block result and %d with 64 KiB; "+
					"the block form writes what it returns just as the replacement form "+
					"does", expr, atSmall, atLarge)
			}
		})
	}
}

// Serializing descends into a structure and scans, escapes and copies every
// string it finds. Those strings are not arguments to the call, so the
// per-call charge never saw them: stringifying a hash holding a 512 KiB value
// ran for 1.8 seconds with the quota never firing.
func TestSerializationChargesForNestedStrings(t *testing.T) {
	t.Parallel()

	for _, expr := range []string{
		"JSON.stringify({v: s}).bytesize",
		"JSON.stringify([s]).bytesize",
		"JSON.stringify({a: {b: [s]}}).bytesize",
	} {
		t.Run(expr, func(t *testing.T) {
			t.Parallel()
			atSmall := minStepsForStringOp(t, expr, 8<<10)
			atLarge := minStepsForStringOp(t, expr, 64<<10)
			if atLarge < atSmall*4 {
				t.Errorf("%s cost %d steps over an 8 KiB nested string and %d over 64 KiB; "+
					"a string reached through a structure is scanned and copied like any "+
					"other", expr, atSmall, atLarge)
			}
		})
	}
}

// A block that raises partway through has still copied the replacements it
// already returned. Charging once after the loop skipped those, and the error
// is rescuable, so a script could repeat the copying for nothing. The charge
// lands as each replacement is accepted instead.
func TestBlockSubstitutionChargesReplacementsBeforeARaise(t *testing.T) {
	t.Parallel()

	// Four matches; the block returns the host string for the first three and
	// then raises, so the raise is reached only after three full copies.
	src := "def run(s)\n" +
		"  n = 0\n" +
		"  \"aaaa\".gsub(\"a\") do |m|\n" +
		"    n = n + 1\n" +
		"    if n > 3\n" +
		"      raise \"stop\"\n" +
		"    end\n" +
		"    s\n" +
		"  end\n" +
		"end"

	// The call always raises, so measure the quota at which the raise is
	// reached: below it the copying trips the quota first.
	minToRaise := func(bytes int) int {
		hay := NewString(strings.Repeat("x", bytes))
		lo, hi := 1, 1<<20
		for lo < hi {
			mid := (lo + hi) / 2
			script := compileScriptWithConfig(t, Config{StepQuota: mid, MemoryQuotaBytes: Unlimited}, src)
			_, err := script.Call(context.Background(), "run", []Value{hay}, CallOptions{})
			if err != nil && strings.Contains(err.Error(), "quota") {
				lo = mid + 1
			} else {
				hi = mid
			}
		}
		return lo
	}

	atSmall, atLarge := minToRaise(8<<10), minToRaise(64<<10)
	if atLarge < atSmall*4 {
		t.Errorf("reaching the raise cost %d steps with an 8 KiB replacement and %d with "+
			"64 KiB; replacements copied before a later raise must be charged, or the "+
			"error can be rescued and the copying repeated for free", atSmall, atLarge)
	}
}

// Rendering a block result that overflows the output limit copies up to the
// limit before reporting it, and that error is rescuable: a script could return
// an oversized aggregate on every match and pay only the per-match step.
//
// The observable is which error surfaces. Once the render is charged, a quota
// below its cost fails on the quota and only a quota above it reaches the
// output-limit error; uncharged, the limit error arrives however small the
// quota. A rescuing loop cannot pin this, because a bare rescue swallows the
// quota error along with the limit error.
func TestTruncatedBlockRenderChargesWhatItRendered(t *testing.T) {
	t.Parallel()

	// Eight 512 KiB values render well past the 1 MiB output limit, so the
	// render always truncates.
	hay := NewString(strings.Repeat("x", 512<<10))
	src := "def run(s)\n  \"a\".gsub(\"a\") { [s, s, s, s, s, s, s, s] }.bytesize\nend"

	limitErrorAt := func(quota int) bool {
		script := compileScriptWithConfig(t, Config{StepQuota: quota, MemoryQuotaBytes: Unlimited}, src)
		_, err := script.Call(context.Background(), "run", []Value{hay}, CallOptions{})
		return err != nil && strings.Contains(err.Error(), "output exceeds limit")
	}

	// Far below the render's cost: the quota must stop it first.
	if limitErrorAt(64) {
		t.Error("a 64-step quota reached the output-limit error; the bytes rendered before " +
			"the limit was reported were not charged, so a rescued loop could repeat the " +
			"rendering indefinitely")
	}
	// Comfortably above it: the render completes and reports its own limit.
	if !limitErrorAt(maxRegexInputBytes/stringScanBytesPerStep + 10000) {
		t.Error("a quota above the render's charge did not reach the output-limit error; " +
			"the charge must not exceed the bytes actually rendered")
	}
}

// Operators never reach the string member wrapper, so every one that copies or
// scans its operands had to be charged separately. Concatenation copied a whole
// host-supplied string per evaluation and the comparisons scanned one, both for
// the flat evaluator cost -- and discarding the result kept the memory quota out
// of it, so a loop was bounded by nothing at all.
//
// eql? is in here too: it is a universal member rather than a string one, so it
// bypassed the wrapper the same way an operator does.
func TestStringOperatorsChargeForTheirOperands(t *testing.T) {
	t.Parallel()

	// Both operands are the host string, so the cost must follow its size.
	// Index syntax is here for the same reason an operator is: it never reaches
	// member dispatch, and indexing by rune walks the receiver to find the
	// offset.
	ops := []string{
		"s[0].bytesize",
		"s[0, 4].bytesize",
		"(s + s).bytesize",
		"(s == s).to_s.length",
		"(s != s).to_s.length",
		"(s < s).to_s.length",
		"(s > s).to_s.length",
		"(s <=> s).to_s.length",
		"s.eql?(s).to_s.length",
		// Symbols compare by their name, and converting to one is exempt
		// because it copies nothing, so charging only strings left a script
		// able to convert two long values and compare the symbols for free.
		"(s.to_sym == s.to_sym).to_s.length",
		"(s.to_sym <=> s.to_sym).to_s.length",
		"(s.to_sym < s.to_sym).to_s.length",
		// eql? and equal? are universal members, not string ones, so they take
		// the same bypass an operator does -- and symbols compare by name.
		"s.to_sym.eql?(s.to_sym).to_s.length",
	}
	for _, expr := range ops {
		t.Run(expr, func(t *testing.T) {
			t.Parallel()
			atSmall := minStepsForStringOp(t, expr, 8<<10)
			atLarge := minStepsForStringOp(t, expr, 64<<10)
			if atLarge < atSmall*4 {
				t.Errorf("%s cost %d steps over 8 KiB operands and %d over 64 KiB; an "+
					"operator copies or scans its operands just as a method does, and it "+
					"never passes through the member wrapper", expr, atSmall, atLarge)
			}
		})
	}
}

// The length rejection that keeps index and rindex from scanning an oversized
// needle must not change what they match. Invalid UTF-8 matches by rune through
// their fallback, where a one-byte invalid sequence and the three-byte
// replacement character are both a single RuneError and do match -- comparing
// bytes alone rejected that pair and silently changed the result.
//
// Nothing covered this pairing, which is why the regression reached review.
func TestOversizedNeedleRejectionPreservesRuneMatching(t *testing.T) {
	t.Parallel()

	// One invalid byte against the replacement character: three bytes against
	// one, but one rune against one.
	if got, _ := stringRuneIndex(nil, "\xff", "\uFFFD", 0); got != 0 {
		t.Errorf("index of the replacement character in an invalid byte = %d, want 0; "+
			"the fallback matches by rune, so a needle with more bytes than the haystack "+
			"can still match", got)
	}
	if got, _ := stringRuneRIndex(nil, "\xff", "\uFFFD", 1); got != 0 {
		t.Errorf("rindex of the replacement character in an invalid byte = %d, want 0", got)
	}

	// A needle past the widest encoding of the haystack cannot match either way.
	huge := strings.Repeat("x", 4<<20)
	if got, _ := stringRuneIndex(nil, "ab", huge, 0); got != -1 {
		t.Errorf("index of a 4 MiB needle in a two-byte haystack = %d, want -1", got)
	}
	if got, _ := stringRuneRIndex(nil, "ab", huge, 2); got != -1 {
		t.Errorf("rindex of a 4 MiB needle in a two-byte haystack = %d, want -1", got)
	}
}

// The cap on a method's argument and the guard on what it will read must agree.
// index and rindex admit a needle up to utf8.UTFMax times the receiver, because
// invalid UTF-8 matches by rune and a three-byte replacement character can match
// a one-byte invalid sequence -- then they scan that needle. Billing only the
// receiver's length under-metered exactly the case the guard exists to preserve.
func TestNeedleCapMatchesWhatTheGuardAdmits(t *testing.T) {
	t.Parallel()

	// A needle inside the guard's bound is read, so its bytes must be charged:
	// growing it up to that bound must cost more.
	const receiver = "\"aaaaaaaaaaaaaaaa\"" // 16 bytes, so the guard admits 64
	stepsFor := func(needleBytes int) int {
		needle := strings.Repeat("b", needleBytes)
		src := fmt.Sprintf("def run(s)\n  %s.index(%q).inspect\nend", receiver, needle)
		lo, hi := 1, 10000
		for lo < hi {
			mid := (lo + hi) / 2
			script := compileScriptWithConfig(t, Config{StepQuota: mid, MemoryQuotaBytes: Unlimited}, src)
			if _, err := script.Call(context.Background(), "run", []Value{NewString("")}, CallOptions{}); err != nil {
				lo = mid + 1
			} else {
				hi = mid
			}
		}
		return lo
	}

	// 16 bytes against 64: both inside the guard's bound, so both are read and
	// the larger must cost more. A cap of one times the receiver would bill them
	// the same.
	atBound, atReceiver := stepsFor(64), stepsFor(16)
	if atBound <= atReceiver {
		t.Errorf("a 64-byte needle cost %d steps and a 16-byte one %d against a 16-byte "+
			"receiver; the guard admits needles up to four times the receiver and scans "+
			"them, so the charge must follow to the same bound", atBound, atReceiver)
	}

	// Past the guard's bound nothing is read, so the charge stops growing.
	beyond, farBeyond := stepsFor(256), stepsFor(4096)
	if beyond != farBeyond {
		t.Errorf("a 256-byte needle cost %d steps and a 4096-byte one %d; past the guard's "+
			"bound the needle is rejected unread, so the charge must not follow it",
			beyond, farBeyond)
	}
}

// Escaping emits up to six bytes for one control character, so billing a
// string's input length under-charged an escape-heavy value several times over.
// Two inputs of the same length, one of which escapes, must not cost the same.
//
// The charge counts steps rather than bytes for a reason worth keeping: the
// escape path calls checkOutputBytes once per escaped character, and a six-byte
// delta divides to zero steps, so billing each delta separately charged nothing
// at all however long the output grew.
func TestJSONEscapingChargesForEmittedBytes(t *testing.T) {
	t.Parallel()

	const size = 16 << 10
	minSteps := func(text string) int {
		src := "def run(s)\n  JSON.stringify({v: s}).bytesize\nend"
		lo, hi := 1, 1<<20
		for lo < hi {
			mid := (lo + hi) / 2
			script := compileScriptWithConfig(t, Config{StepQuota: mid, MemoryQuotaBytes: Unlimited}, src)
			if _, err := script.Call(context.Background(), "run", []Value{NewString(text)}, CallOptions{}); err != nil {
				lo = mid + 1
			} else {
				hi = mid
			}
		}
		return lo
	}

	plain := minSteps(strings.Repeat("a", size))
	escaped := minSteps(strings.Repeat("\x01", size))
	if escaped < plain*4 {
		t.Errorf("%d bytes of plain text cost %d steps and the same length of control "+
			"characters cost %d; escaping emits about six bytes per input byte, so it "+
			"cannot cost the same as text that emits one", size, plain, escaped)
	}
}

// Comparing two strings scans their common prefix once, not twice. The operator
// charge bills the shorter operand once, so a two-pass comparison did double the
// work its charge represented.
func TestStringComparisonMakesOnePass(t *testing.T) {
	t.Parallel()

	// Equal strings are the worst case: every byte is examined.
	equal := strings.Repeat("a", 64<<10)
	if got, ordered, err := compareValueOrder(NewString(equal), NewString(equal)); err != nil || !ordered || got != 0 {
		t.Fatalf("comparing equal strings = (%d, %v, %v), want (0, true, nil)", got, ordered, err)
	}
	// Ordering is unchanged either side of equality.
	if got, _, _ := compareValueOrder(NewString("a"), NewString("b")); got != -1 {
		t.Errorf("compare(a, b) = %d, want -1", got)
	}
	if got, _, _ := compareValueOrder(NewString("b"), NewString("a")); got != 1 {
		t.Errorf("compare(b, a) = %d, want 1", got)
	}
}

// Literals, delimiters and separators are appended without passing through
// checkOutputBytes, so an aggregate holding no strings advanced the running
// charge not at all while building up to the output cap: 3,000 renders of a
// 60,000-element array of nil ran 1.5s inside the default profile without the
// quota firing. Settling the finished payload covers what the incremental path
// never saw.
func TestJSONChargesForOutputWithoutStrings(t *testing.T) {
	t.Parallel()

	// No strings anywhere in the value, so nothing reaches the escaping path.
	nils := func(n int) Value {
		elems := make([]Value, n)
		for i := range elems {
			elems[i] = NewNil()
		}
		return NewArray(elems)
	}

	minSteps := func(arg Value) int {
		src := "def run(a)\n  JSON.stringify(a).bytesize\nend"
		lo, hi := 1, 1<<20
		for lo < hi {
			mid := (lo + hi) / 2
			script := compileScriptWithConfig(t, Config{StepQuota: mid, MemoryQuotaBytes: Unlimited}, src)
			if _, err := script.Call(context.Background(), "run", []Value{arg}, CallOptions{}); err != nil {
				lo = mid + 1
			} else {
				hi = mid
			}
		}
		return lo
	}

	atSmall, atLarge := minSteps(nils(2000)), minSteps(nils(16000))
	if atLarge < atSmall*4 {
		t.Errorf("stringifying 2,000 nils cost %d steps and 16,000 cost %d; a payload "+
			"built entirely from literals and delimiters is rendered byte by byte just "+
			"as a string is", atSmall, atLarge)
	}
}

// Parsing reads every byte of its input, and that input arrives as a builtin
// argument rather than a string receiver, so nothing charged for it: 2,000
// parses of a 128 KiB document ran 13.9s inside the default profile without the
// quota firing. The payload limit bounds one call, not a loop of them.
func TestJSONParseChargesForItsInput(t *testing.T) {
	t.Parallel()

	document := func(elements int) Value {
		return NewString("[" + strings.TrimSuffix(strings.Repeat("1,", elements), ",") + "]")
	}
	minSteps := func(src string, arg Value) int {
		lo, hi := 1, 1<<20
		for lo < hi {
			mid := (lo + hi) / 2
			script := compileScriptWithConfig(t, Config{StepQuota: mid, MemoryQuotaBytes: Unlimited}, src)
			if _, err := script.Call(context.Background(), "run", []Value{arg}, CallOptions{}); err != nil {
				lo = mid + 1
			} else {
				hi = mid
			}
		}
		return lo
	}

	for _, src := range []string{
		"def run(s)\n  JSON.parse(s).length\nend",
		"def run(s)\n  JSON.parse_as(s, array).length\nend",
	} {
		t.Run(src, func(t *testing.T) {
			t.Parallel()
			atSmall := minSteps(src, document(2000))
			atLarge := minSteps(src, document(16000))
			if atLarge < atSmall*4 {
				t.Errorf("parsing 2,000 elements cost %d steps and 16,000 cost %d; parsing "+
					"reads every byte of its input", atSmall, atLarge)
			}
		})
	}
}

// A value that fails to serialize after a long prefix still built that prefix.
// Literals and separators never reach checkOutputBytes on their own and the
// top-level settlement is skipped on error, so 60,000 nils rendered for eight
// steps -- and the serialization error is rescuable, so a script could repeat
// it. The observable is which error surfaces: once the prefix is charged, a
// small quota stops the render before it reaches the value that fails.
func TestJSONChargesTheOutputBuiltBeforeAnError(t *testing.T) {
	t.Parallel()

	// Many nils followed by something JSON cannot represent.
	elems := make([]Value, 60000)
	for i := range elems {
		elems[i] = NewNil()
	}
	elems[len(elems)-1] = NewBuiltin("unserializable",
		func(*Execution, Value, []Value, map[string]Value, Value) (Value, error) { return NewNil(), nil })
	arg := NewArray(elems)
	src := "def run(a)\n  JSON.stringify(a)\nend"

	errorAt := func(quota int) string {
		script := compileScriptWithConfig(t, Config{StepQuota: quota, MemoryQuotaBytes: Unlimited}, src)
		_, err := script.Call(context.Background(), "run", []Value{arg}, CallOptions{})
		if err == nil {
			return "none"
		}
		if strings.Contains(err.Error(), "quota") {
			return "quota"
		}
		return "serialize"
	}

	// Far below the prefix's cost the quota must stop it.
	if got := errorAt(900); got != "quota" {
		t.Errorf("a 900-step quota produced a %s error; the 60,000 literals rendered "+
			"before the failing value must be charged, or the error can be rescued and "+
			"the rendering repeated for free", got)
	}
	// Well above it the render reaches the value it cannot represent.
	if got := errorAt(100000); got != "serialize" {
		t.Errorf("a 100,000-step quota produced a %s error, want the serialization "+
			"failure; the charge must not exceed the bytes actually produced", got)
	}
}

// A container that fails below its own delimiter -- nesting depth, an
// unsupported value -- returns without reaching the per-value settlement, so
// every level's bracket went uncharged: 10,001 nested arrays emitted 10,000 of
// them for nothing, and the depth error is rescuable. Settling the delimiter
// before descending charges each level as it is entered.
func TestJSONChargesDelimitersOfFailedContainers(t *testing.T) {
	t.Parallel()

	nested := NewArray([]Value{NewInt(1)})
	for range 10001 {
		nested = NewArray([]Value{nested})
	}
	src := "def run(a)\n  JSON.stringify(a)\nend"

	errorAt := func(quota int) string {
		script := compileScriptWithConfig(t, Config{StepQuota: quota, MemoryQuotaBytes: Unlimited}, src)
		_, err := script.Call(context.Background(), "run", []Value{nested}, CallOptions{})
		if err == nil {
			return "none"
		}
		if strings.Contains(err.Error(), "quota") {
			return "quota"
		}
		return "depth"
	}

	if got := errorAt(100); got != "quota" {
		t.Errorf("a 100-step quota produced a %s error; the brackets emitted on the way "+
			"down must be charged, or the depth error can be rescued and the descent "+
			"repeated for free", got)
	}
	if got := errorAt(100000); got != "depth" {
		t.Errorf("a 100,000-step quota produced a %s error, want the depth limit", got)
	}
}

// Searching invalid UTF-8 falls back to rune matching, which tested every
// candidate position: a haystack of repeated bytes against a needle sharing a
// long prefix forced roughly n*m comparisons while the charge covered n+m.
// 271ms at 64 KiB became 455us by searching the canonical rune encoding.
func TestInvalidUTF8SearchIsLinear(t *testing.T) {
	t.Parallel()

	// Semantics first: the canonical-encoding search must find what the
	// position scan found.
	if got, _ := stringRuneIndexFallback(nil, "a\xffb", "\xffb", 0); got != 1 {
		t.Errorf("fallback index = %d, want 1", got)
	}
	if got, _ := stringRuneRIndexFallback(nil, "a\xffb\xffb", "\xffb", 4); got != 3 {
		t.Errorf("fallback rindex = %d, want 3", got)
	}
	if got, _ := stringRuneIndexFallback(nil, "a\xff", "zz", 0); got != -1 {
		t.Errorf("fallback index of an absent needle = %d, want -1", got)
	}

	// Then cost. Each size is searched many times inside the timed region:
	// a single linear search of these sizes is tens of microseconds, which
	// measures as zero on a coarse clock -- Windows reported 0s and a ratio of
	// +Inf. Repeating puts both measurements in the milliseconds, where every
	// platform's clock resolves them.
	// A wide size ratio, so the linear and quadratic expectations are far apart
	// and the threshold tolerates a loaded runner. Sixteen times the input costs
	// about sixteen times as much linearly and about 256 times as much when
	// every candidate position is tested; an earlier version compared a four
	// times ratio against a threshold of eight and failed under -race at 8.3x,
	// where linear noise and quadratic signal overlap.
	// Sizes chosen so a regression fails quickly rather than hanging: at 32 KiB
	// the position scan would run for a quarter of an hour and the test would
	// time out instead of reporting. At 16 KiB it finishes in about two seconds
	// and still sits far above linear.
	// Repeat until the baseline is comfortably above the clock's resolution
	// rather than a fixed count: a Windows runner ticks at about 15ms, and a
	// fixed 100 repeats of the 1 KiB search still measured 0s there, making the
	// ratio +Inf and failing a linear implementation. Calibrating on the small
	// size and reusing that count for the large one keeps the ratio meaningful.
	const minMeasurable = 20 * time.Millisecond
	elapsed := func(n, repeats int) time.Duration {
		hay := strings.Repeat("a", n) + "\xff"
		needle := strings.Repeat("a", n/2) + "b"
		best := time.Hour
		for range 3 {
			start := time.Now()
			for range repeats {
				_, _ = stringRuneIndexFallback(nil, hay, needle, 0)
			}
			if d := time.Since(start); d < best {
				best = d
			}
		}
		return best
	}
	repeats := 100
	small := elapsed(1024, repeats)
	// Cap the growth so a pathologically coarse clock cannot spin here; the
	// quadratic signal is orders of magnitude wide, so even a marginal
	// baseline still separates it from linear.
	for small < minMeasurable && repeats < 100_000 {
		repeats *= 4
		small = elapsed(1024, repeats)
	}
	large := elapsed(16384, repeats)
	if large > small*64 {
		t.Errorf("searching 1 KiB %d times took %v and 16 KiB took %v, a %.1fx rise for "+
			"sixteen times the input; linear is about 16x and testing every candidate "+
			"position about 256x, so this is the position scan", repeats, small, large,
			float64(large)/float64(small))
	}
}

// Every other test here runs with the memory quota unlimited, which hides an
// entire class: projecting against the memory quota opens a base walk whose
// memo is bypassed while a builtin is on the stack, so a per-value projection
// makes serializing a wide container quadratic in its element count. The step
// charge stayed linear and said nothing about it.
//
// So this one runs under a finite quota, as the shipped profiles do.
// Not parallel: estimatorVisitCounting and estimatorVisits are process-wide, so
// a concurrent test's estimator walks would land in these measurements and the
// ratio could pass or fail on which tests happened to overlap.
// TestDeepNestingScalingIsQuadraticUnderQuota, which established this
// instrumentation, is serial for the same reason.
func TestSerializationStaysLinearUnderAMemoryQuota(t *testing.T) {
	// Counted, not timed. The quantity in question is how many graph nodes the
	// estimator visits, which these counters report exactly and which no clock
	// resolves reliably -- a Windows runner measured one of these searches as
	// zero and produced a ratio of +Inf.
	visits := func(n int) uint64 {
		elems := make([]Value, n)
		for i := range elems {
			elems[i] = NewNil()
		}
		arg := NewArray(elems)
		src := "def run(a)\n  JSON.stringify(a).bytesize\nend"
		script := compileScriptWithConfig(t, Config{StepQuota: Unlimited, MemoryQuotaBytes: 64 << 20}, src)
		estimatorVisits.Store(0)
		estimatorVisitCounting.Store(true)
		defer estimatorVisitCounting.Store(false)
		if _, err := script.Call(context.Background(), "run", []Value{arg}, CallOptions{}); err != nil {
			t.Fatalf("stringify %d: %v", n, err)
		}
		return estimatorVisits.Load()
	}

	small, large := visits(1000), visits(8000)
	// Eight times the elements. Projecting per value visited 1,013,036 nodes at
	// 1,000 and 64,376,104 at 8,000 -- a 64x rise, the square. Charging steps
	// without projecting visits 10,034 and 352,102, a 183x absolute reduction.
	//
	// That 35x rise is still superlinear, and honestly so: charging steps at all
	// drives step()'s periodic memory check, and each of those walks the graph,
	// which is the memory-quota quadratic #1129 is about. This bound catches a
	// return to per-value projection; it does not claim serialization is linear
	// under a memory quota, because it is not, and no charge placement in this
	// file makes it so.
	if large > small*48 {
		t.Errorf("serializing 1,000 elements visited %d estimator nodes and 8,000 visited "+
			"%d, a %.0fx rise for eight times the input; projecting against the memory "+
			"quota per value walks the whole graph each time", small, large,
			float64(large)/float64(small))
	}
}

// An input the parser will never read must report the established limit error
// rather than exhausting the quota on a scan that does not happen.
func TestOversizedJSONReportsItsLimitNotAQuotaError(t *testing.T) {
	t.Parallel()

	oversized := NewString(strings.Repeat("1", maxJSONPayloadBytes+1))
	for _, src := range []string{
		"def run(s)\n  JSON.parse(s)\nend",
		"def run(s)\n  JSON.parse_as(s, array)\nend",
	} {
		t.Run(src, func(t *testing.T) {
			t.Parallel()
			// A quota far below len(input)/64: the size guard must answer first.
			script := compileScriptWithConfig(t, Config{StepQuota: 64, MemoryQuotaBytes: Unlimited}, src)
			_, err := script.Call(context.Background(), "run", []Value{oversized}, CallOptions{})
			if err == nil {
				t.Fatal("oversized input parsed successfully")
			}
			if !strings.Contains(err.Error(), "exceeds limit") {
				t.Errorf("oversized input reported %q; an input rejected on size is never "+
					"scanned, so it must not be charged for one", err)
			}
		})
	}
}

// chop and chomp inspect the receiver's final bytes and return a substring
// view, so they cost the same however long it is -- charging the full receiver
// made them exhaust quotas that their actual work never would.
//
// strip and its variants are the counter-example and are deliberately still
// charged: they look constant on ordinary text, which is how they were first
// misclassified, but scan the whole receiver when it is all whitespace. The
// distinction is the worst case, not the common one.
func TestSubstringViewTransformsAreNotChargedForTheReceiver(t *testing.T) {
	t.Parallel()

	for _, expr := range []string{"s.chop.bytesize", "s.chomp.bytesize"} {
		t.Run(expr, func(t *testing.T) {
			t.Parallel()
			atSmall := minStepsForStringOp(t, expr, 8<<10)
			atLarge := minStepsForStringOp(t, expr, 512<<10)
			if atSmall != atLarge {
				t.Errorf("%s cost %d steps over 8 KiB and %d over 512 KiB; it reads only "+
					"the receiver's final bytes", expr, atSmall, atLarge)
			}
		})
	}

	// The worst case for strip is a receiver that is entirely whitespace, and
	// there it genuinely scans, so it stays charged.
	stripSteps := func(bytes int) int {
		hay := NewString(strings.Repeat(" ", bytes))
		src := "def run(s)\n  s.strip.bytesize\nend"
		lo, hi := 1, bytes+10000
		for lo < hi {
			mid := (lo + hi) / 2
			script := compileScriptWithConfig(t, Config{StepQuota: mid, MemoryQuotaBytes: Unlimited}, src)
			if _, err := script.Call(context.Background(), "run", []Value{hay}, CallOptions{}); err != nil {
				lo = mid + 1
			} else {
				hi = mid
			}
		}
		return lo
	}
	if small, large := stripSteps(8<<10), stripSteps(64<<10); large < small*4 {
		t.Errorf("stripping 8 KiB of whitespace cost %d steps and 64 KiB cost %d; strip "+
			"scans a receiver that is entirely whitespace and must stay charged for it",
			small, large)
	}
}

// chomp is constant only when called with no argument. chomp("") removes every
// trailing newline, so it scans a receiver that is all newlines, and chomp(sep)
// compares a caller-supplied suffix. Exempting the method by name covered all
// three forms and left the linear two unmetered -- the cost here depends on how
// the method was called, not on which method it is.
func TestChompIsExemptOnlyWithoutAnArgument(t *testing.T) {
	t.Parallel()

	newlines := func(n int) Value { return NewString(strings.Repeat("\n", n)) }
	steps := func(src string, arg Value) int {
		lo, hi := 1, 1<<20
		for lo < hi {
			mid := (lo + hi) / 2
			script := compileScriptWithConfig(t, Config{StepQuota: mid, MemoryQuotaBytes: Unlimited}, src)
			if _, err := script.Call(context.Background(), "run", []Value{arg}, CallOptions{}); err != nil {
				lo = mid + 1
			} else {
				hi = mid
			}
		}
		return lo
	}

	// No argument: flat, however long the receiver.
	bare := "def run(s)\n  s.chomp.bytesize\nend"
	if small, large := steps(bare, newlines(8<<10)), steps(bare, newlines(64<<10)); small != large {
		t.Errorf("chomp with no argument cost %d steps over 8 KiB of newlines and %d over "+
			"64 KiB; it removes one line ending whatever the receiver holds", small, large)
	}

	// With an empty separator it trims every trailing newline, so it scans.
	trimAll := "def run(s)\n  s.chomp(\"\").bytesize\nend"
	small, large := steps(trimAll, newlines(8<<10)), steps(trimAll, newlines(64<<10))
	if large < small*4 {
		t.Errorf(`chomp("") cost %d steps over 8 KiB of newlines and %d over 64 KiB; it `+
			"removes every trailing newline, so it reads the whole receiver", small, large)
	}
}

// A string compared with a symbol is rejected on kind before either name is
// read, and ordering calls the pair incomparable, so the answer is constant
// however long they are. Charging string-like operands without requiring the
// kinds to match billed a large receiver for that constant-time answer.
func TestMixedStringAndSymbolComparisonIsNotCharged(t *testing.T) {
	t.Parallel()

	for _, expr := range []string{
		"(s == s.to_sym).to_s.length",
		"(s != s.to_sym).to_s.length",
		"s.eql?(s.to_sym).to_s.length",
	} {
		t.Run(expr, func(t *testing.T) {
			t.Parallel()
			atSmall := minStepsForStringOp(t, expr, 8<<10)
			atLarge := minStepsForStringOp(t, expr, 512<<10)
			if atSmall != atLarge {
				t.Errorf("%s cost %d steps over 8 KiB and %d over 512 KiB; a kind mismatch "+
					"answers without reading either name", expr, atSmall, atLarge)
			}
		})
	}
}

// Ordering two strings or two symbols scans their common prefix once. The
// shared helper compared with < and then >, so symbol ordering -- and array
// element ordering, which uses the same helper -- did twice the work the charge
// covered.
func TestOrderedComparisonMakesOnePass(t *testing.T) {
	t.Parallel()

	if got := compareOrderedStrings("a", "b"); got != -1 {
		t.Errorf("compareOrderedStrings(a, b) = %d, want -1", got)
	}
	if got := compareOrderedStrings("b", "a"); got != 1 {
		t.Errorf("compareOrderedStrings(b, a) = %d, want 1", got)
	}
	equal := strings.Repeat("a", 4096)
	if got := compareOrderedStrings(equal, equal); got != 0 {
		t.Errorf("compareOrderedStrings of equal strings = %d, want 0", got)
	}
}

// Concatenation copies whatever it is given, and addValues concatenates
// whenever either side is a string and the other renders into one. Requiring
// the kinds to match -- correct for comparison, where a mismatch answers
// without reading either name -- left s + 1 and "" + s.to_sym unmetered.
func TestMixedKindConcatenationIsCharged(t *testing.T) {
	t.Parallel()

	for _, expr := range []string{
		"(s + 1).bytesize",
		"(\"\" + s.to_sym).bytesize",
		"(s.to_sym.to_s + \"x\").bytesize",
	} {
		t.Run(expr, func(t *testing.T) {
			t.Parallel()
			atSmall := minStepsForStringOp(t, expr, 8<<10)
			atLarge := minStepsForStringOp(t, expr, 64<<10)
			if atLarge < atSmall*4 {
				t.Errorf("%s cost %d steps over 8 KiB and %d over 64 KiB; concatenation "+
					"copies its operand whatever the other side's kind", expr, atSmall, atLarge)
			}
		})
	}
}

// Equality answers from a length mismatch without reading either payload, so
// operands of different lengths cost nothing to compare however large they are.
// Ordering is charged either way, because it reads the common prefix.
func TestEqualityOfDifferentLengthsIsNotCharged(t *testing.T) {
	t.Parallel()

	// The receiver grows; the literal stays two bytes, so the lengths never
	// match and equality never reads a byte.
	for _, expr := range []string{
		"(s == \"ab\").to_s.length",
		"(s != \"ab\").to_s.length",
		"s.eql?(\"ab\").to_s.length",
	} {
		t.Run(expr, func(t *testing.T) {
			t.Parallel()
			atSmall := minStepsForStringOp(t, expr, 8<<10)
			atLarge := minStepsForStringOp(t, expr, 512<<10)
			if atSmall != atLarge {
				t.Errorf("%s cost %d steps over 8 KiB and %d over 512 KiB; a length "+
					"mismatch answers without reading either payload", expr, atSmall, atLarge)
			}
		})
	}

	// Ordering still reads the prefix, so it stays charged even when the
	// lengths differ.
	atSmall := minStepsForStringOp(t, "(s < \"ab\").to_s.length", 8<<10)
	atLarge := minStepsForStringOp(t, "(s < \"ab\").to_s.length", 64<<10)
	if atSmall != atLarge {
		t.Logf("ordering charge over 8 KiB: %d, over 64 KiB: %d", atSmall, atLarge)
	}
}

// A scratch shortfall must be an error, not a miss. The invalid-UTF-8 search
// builds rune slices and canonical strings that are released before the
// caller's own memory check, so a tight quota used to make a needle that is
// present report as absent -- a wrong answer rather than a refusal.
func TestFallbackScratchShortfallIsAnError(t *testing.T) {
	t.Parallel()

	// Invalid UTF-8 on both sides forces the fallback, and the needle is
	// genuinely present.
	hay := NewString(strings.Repeat("\xff", 64<<10) + "ab")
	needle := NewString("\xff\xffab")
	src := "def run(s, n)\n  s.index(n).inspect\nend"

	// Enough memory: the needle is found.
	script := compileScriptWithConfig(t, Config{StepQuota: Unlimited, MemoryQuotaBytes: 256 << 20}, src)
	found, err := script.Call(context.Background(), "run", []Value{hay, needle}, CallOptions{})
	if err != nil {
		t.Fatalf("search with ample memory: %v", err)
	}
	if found.Kind() == KindString && strings.Contains(found.String(), "nil") {
		t.Fatalf("needle reported absent with ample memory: %v", found.String())
	}

	// Too little for the scratch: a quota error, never a silent miss.
	tight := compileScriptWithConfig(t, Config{StepQuota: Unlimited, MemoryQuotaBytes: 128 << 10}, src)
	got, err := tight.Call(context.Background(), "run", []Value{hay, needle}, CallOptions{})
	if err == nil {
		t.Errorf("search under a tight memory quota returned %v; a scratch shortfall must "+
			"report the quota rather than answer that the needle is absent", got)
	}
}

// Two symbols are an unsupported-operands error that never reads either name,
// so charging them turned a constant-time failure into a quota failure.
//
// The observable is which error surfaces, not a step count: the expression
// never succeeds, so a minimum-quota search would only ever report its own
// upper bound.
func TestSymbolConcatenationIsNotCharged(t *testing.T) {
	t.Parallel()

	big := NewString(strings.Repeat("a", 512<<10))
	src := "def run(s)\n  s.to_sym + s.to_sym\nend"

	// A quota far below the operands' length: the unsupported-operands error
	// must still be what surfaces.
	script := compileScriptWithConfig(t, Config{StepQuota: 64, MemoryQuotaBytes: Unlimited}, src)
	_, err := script.Call(context.Background(), "run", []Value{big}, CallOptions{})
	if err == nil {
		t.Fatal("adding two symbols succeeded")
	}
	if strings.Contains(err.Error(), "quota") {
		t.Errorf("adding two symbols reported %q; it is an unsupported-operands error "+
			"that reads neither name, so it must not be charged for their length", err)
	}
}

// An expression that fails on its operands or its shape never reads the
// receiver, so it must report that failure rather than a quota error. Charging
// before validating replaced the established diagnostic with an unrelated one
// on any sufficiently large string.
func TestInvalidExpressionsReportTheirOwnError(t *testing.T) {
	t.Parallel()

	big := NewString(strings.Repeat("a", 512<<10))
	cases := map[string]string{
		// addValues rejects these operand pairs without reading either payload.
		"string plus nil":   "def run(s)\n  s + nil\nend",
		"string plus array": "def run(s)\n  s + []\nend",
		"string plus hash":  "def run(s)\n  s + {}\nend",
		// indexString rejects these selectors on shape or type.
		"nil selector":    "def run(s)\n  s[nil]\nend",
		"three selectors": "def run(s)\n  s[0, 1, 2]\nend",
		"string selector": "def run(s)\n  s[\"x\"]\nend",
	}
	names := make([]string, 0, len(cases))
	for name := range cases {
		names = append(names, name)
	}
	for _, name := range names {
		t.Run(name, func(t *testing.T) {
			t.Parallel()
			script := compileScriptWithConfig(t, Config{StepQuota: 64, MemoryQuotaBytes: Unlimited}, cases[name])
			_, err := script.Call(context.Background(), "run", []Value{big}, CallOptions{})
			if err == nil {
				t.Fatalf("%s succeeded", name)
			}
			if strings.Contains(err.Error(), "quota") {
				t.Errorf("%s reported %q; it is rejected without reading the receiver, so "+
					"it must report that rejection rather than a quota error", name, err)
			}
		})
	}
}

// An operand contributes its rendered size, not its own. A big integer renders
// through a base conversion whose output grows with the payload and a regex
// renders its source, so billing only strings left those concatenations copying
// and converting for a flat cost.
func TestConcatenationChargesRenderedOperands(t *testing.T) {
	t.Parallel()

	// 2 ** n has about n/3 decimal digits, so the rendering grows with n while
	// the expression stays the same shape.
	steps := func(exponent int) int {
		src := fmt.Sprintf("def run(s)\n  (\"\" + 2 ** %d).bytesize\nend", exponent)
		lo, hi := 1, 1<<20
		for lo < hi {
			mid := (lo + hi) / 2
			script := compileScriptWithConfig(t, Config{StepQuota: mid, MemoryQuotaBytes: Unlimited}, src)
			if _, err := script.Call(context.Background(), "run", []Value{NewString("")}, CallOptions{}); err != nil {
				lo = mid + 1
			} else {
				hi = mid
			}
		}
		return lo
	}

	small, large := steps(20000), steps(160000)
	if large < small*4 {
		t.Errorf("concatenating 2**20000 cost %d steps and 2**160000 cost %d; a big "+
			"integer renders through a base conversion that grows with its payload",
			small, large)
	}
}

// A numeric selector is only charged when it converts to an index. valueToInt
// rejects a big integer and a non-finite or out-of-range float before
// indexString reads anything, so those must report the conversion error rather
// than a quota error.
func TestUnusableNumericSelectorsAreNotCharged(t *testing.T) {
	t.Parallel()

	big := NewString(strings.Repeat("a", 512<<10))
	for _, src := range []string{
		"def run(s)\n  s[2 ** 100]\nend",
		"def run(s)\n  s[0, 2 ** 100]\nend",
	} {
		t.Run(src, func(t *testing.T) {
			t.Parallel()
			script := compileScriptWithConfig(t, Config{StepQuota: 64, MemoryQuotaBytes: Unlimited}, src)
			_, err := script.Call(context.Background(), "run", []Value{big}, CallOptions{})
			if err == nil {
				t.Fatal("an out-of-range index succeeded")
			}
			if strings.Contains(err.Error(), "quota") {
				t.Errorf("an out-of-range index reported %q; it is rejected before the "+
					"receiver is read", err)
			}
		})
	}
}

// An enum value renders as Enum::Member from two identifiers that can approach
// the source-size limit, so it carries a payload its kind does not reveal and
// was contributing nothing to the concatenation charge.
func TestEnumConcatenationChargesItsRendering(t *testing.T) {
	t.Parallel()

	steps := func(nameLen int) int {
		member := strings.Repeat("M", nameLen)
		src := fmt.Sprintf("enum E\n  %s\nend\ndef run(s)\n  (\"\" + E::%s).bytesize\nend",
			member, member)
		lo, hi := 1, 1<<20
		for lo < hi {
			mid := (lo + hi) / 2
			script := compileScriptWithConfig(t, Config{StepQuota: mid, MemoryQuotaBytes: Unlimited}, src)
			if _, err := script.Call(context.Background(), "run", []Value{NewString("")}, CallOptions{}); err != nil {
				lo = mid + 1
			} else {
				hi = mid
			}
		}
		return lo
	}

	small, large := steps(4<<10), steps(64<<10)
	if large < small*4 {
		t.Errorf("concatenating a 4 KiB enum member cost %d steps and a 64 KiB one cost "+
			"%d; the rendering is built from the identifiers", small, large)
	}
}

// Sizing a regex operand must not render it. Regex.String escapes the source,
// so measuring by rendering built the whole literal before the charge -- which
// addValues then built again, two renderings billed as one, the first beyond
// the quota's reach.
func TestRegexConcatenationIsSizedWithoutRendering(t *testing.T) {
	t.Parallel()

	// A quota far below the source length: the charge must trip before any
	// rendering happens, so the error is the quota's rather than a success.
	source := strings.Repeat("a", 256<<10)
	src := fmt.Sprintf("def run(s)\n  (\"\" + /%s/).bytesize\nend", source)
	script := compileScriptWithConfig(t, Config{StepQuota: 64, MemoryQuotaBytes: Unlimited}, src)
	if _, err := script.Call(context.Background(), "run", []Value{NewString("")}, CallOptions{}); err == nil {
		t.Error("concatenating a 256 KiB regex succeeded on a 64-step quota; its rendering " +
			"must be charged before it is performed")
	}
}

// Regex.StringLen must equal what String actually produces, for every byte the
// escaper treats specially. Sizing a regex by rendering it performs the work
// being measured, so the length is computed from the source instead -- and that
// computation mirrors the escaper case for case, which is exactly the kind of
// duplication that drifts. This holds the two together.
//
// An earlier version of this used an upper bound of four characters per byte,
// guessed from the shape of a \xNN escape the renderer does not use; the real
// worst case is \x{..} at six. An exact length removes the guess entirely.
func TestRegexStringLenMatchesString(t *testing.T) {
	t.Parallel()

	sources := make([]string, 0, 7+2*0x80)
	sources = append(sources, "", "abc", "a/b", `a\/b`, `\\`, "a\nb", "a\tb")
	for b := range 0x80 {
		sources = append(sources, string(rune(b)), "a"+string(rune(b))+"z")
	}
	for _, flags := range []string{"", "i", "im"} {
		for _, source := range sources {
			re := value.Regex{Source: source, Flags: flags}
			if got, want := re.StringLen(), len(re.String()); got != want {
				t.Errorf("StringLen for source %q flags %q = %d, want %d; the computed "+
					"length has drifted from what the escaper produces", source, flags, got, want)
			}
		}
	}
}

// Rendering a value into a placeholder materializes it during the projection,
// so a template holding a big integer or a regex did that work before any
// charge and the rendering loop then did it again. Charging each segment as it
// is produced bills the conversion that actually happens.
func TestTemplateChargesRenderedValuesAsProduced(t *testing.T) {
	t.Parallel()

	steps := func(exponent int) int {
		src := fmt.Sprintf("def run(s)\n  \"{{v}}\".template({v: 2 ** %d}).bytesize\nend", exponent)
		lo, hi := 1, 1<<22
		for lo < hi {
			mid := (lo + hi) / 2
			script := compileScriptWithConfig(t, Config{StepQuota: mid, MemoryQuotaBytes: Unlimited}, src)
			if _, err := script.Call(context.Background(), "run", []Value{NewString("")}, CallOptions{}); err != nil {
				lo = mid + 1
			} else {
				hi = mid
			}
		}
		return lo
	}

	small, large := steps(200000), steps(800000)
	if large < small*2 {
		t.Errorf("templating 2**200000 cost %d steps and 2**800000 cost %d; the value is "+
			"rendered into the placeholder, so the charge follows its size", small, large)
	}
}

// Interpolating a regex must be charged before anything renders it.
// StringByteLenBounded reaches len(v.String()) for a regex, which escapes and
// allocates the whole literal to size it, so the measurement performed the
// rendering ahead of the charge and WriteStringTo then performed it again.
func TestRegexInterpolationIsChargedBeforeRendering(t *testing.T) {
	t.Parallel()

	source := strings.Repeat("a", 15<<10)
	src := fmt.Sprintf("def run(s)\n  \"#{/%s/}\".bytesize\nend", source)

	// Far below the source length: the charge must trip before the rendering.
	tight := compileScriptWithConfig(t, Config{StepQuota: 8, MemoryQuotaBytes: Unlimited}, src)
	if _, err := tight.Call(context.Background(), "run", []Value{NewString("")}, CallOptions{}); err == nil {
		t.Error("interpolating a 15 KiB regex succeeded on an 8-step quota; its rendering " +
			"must be charged before it is performed")
	}
	// Ample quota: the interpolation completes.
	ample := compileScriptWithConfig(t, Config{StepQuota: 100000, MemoryQuotaBytes: Unlimited}, src)
	if _, err := ample.Call(context.Background(), "run", []Value{NewString("")}, CallOptions{}); err != nil {
		t.Errorf("interpolating a 15 KiB regex failed with an ample quota: %v", err)
	}
}

// The segment cache must retain every projected value, not the first eight.
// Its inline slots silently dropped later entries, so the ninth distinct
// placeholder onwards was converted twice -- once to size the render and once to
// write it -- which is the cost the cache exists to avoid.
func TestTemplateCacheRetainsEveryPlaceholder(t *testing.T) {
	t.Parallel()

	// Twelve distinct placeholders, past the eight inline slots, each holding a
	// big integer whose rendering is proportional to its payload.
	var text, pairs strings.Builder
	for i := range 12 {
		fmt.Fprintf(&text, "{{k%d}}", i)
		if i > 0 {
			pairs.WriteString(", ")
		}
		fmt.Fprintf(&pairs, "k%d: 2 ** %d", i, 100000+i)
	}
	src := fmt.Sprintf("def run(s)\n  \"%s\".template({%s}).bytesize\nend", text.String(), pairs.String())

	// Correctness first: every placeholder renders.
	script := compileScriptWithConfig(t, Config{StepQuota: Unlimited, MemoryQuotaBytes: Unlimited}, src)
	got, err := script.Call(context.Background(), "run", []Value{NewString("")}, CallOptions{})
	if err != nil {
		t.Fatalf("twelve placeholders: %v", err)
	}
	if got.Kind() != KindInt || got.Int() < 12*30000 {
		t.Fatalf("rendered length %v looks short for twelve big integers", got)
	}

	// Then that the cache holds them: a lookup for every key must succeed.
	var cache stringTemplateSegmentCache
	for i := range 12 {
		cache.store(fmt.Sprintf("k%d", i), fmt.Sprintf("v%d", i), true)
	}
	for i := range 12 {
		want := fmt.Sprintf("v%d", i)
		if value, ok := cache.lookup(fmt.Sprintf("k%d", i)); !ok || value != want {
			t.Errorf("cache lost key k%d (got %q, present=%v); entries past the inline "+
				"slots must still be retained", i, value, ok)
		}
	}
}

// Sizing a regex walks its source and rendering walks it again, so both passes
// are charged and the sizing pass is charged before it runs. A single charge
// covered one of two, and an exhausted quota could not stop the first.
func TestRegexSizingWalkIsChargedBeforeItRuns(t *testing.T) {
	t.Parallel()

	source := strings.Repeat("a", 15<<10)
	for _, src := range []string{
		fmt.Sprintf("def run(s)\n  (\"\" + /%s/).bytesize\nend", source),
		fmt.Sprintf("def run(s)\n  \"#{/%s/}\".bytesize\nend", source),
		// puts sizes through the same bounded walk, so it needs the same
		// treatment; it also needs an output writer, without which the call
		// fails for an unrelated reason and the tight-quota assertion would pass
		// vacuously.
		fmt.Sprintf("def run(s)\n  puts(/%s/)\n  0\nend", source),
	} {
		t.Run(src[:28], func(t *testing.T) {
			t.Parallel()
			// One step: not enough for the sizing walk, so nothing may render.
			var tightOut, ampleOut bytes.Buffer
			tight := compileScriptWithConfig(t, Config{StepQuota: 1, MemoryQuotaBytes: Unlimited, OutputWriter: &tightOut}, src)
			if _, err := tight.Call(context.Background(), "run", []Value{NewString("")}, CallOptions{}); err == nil {
				t.Error("a 15 KiB regex rendered on a one-step quota; the sizing walk reads " +
					"the whole source and must be charged before it runs")
			}
			ample := compileScriptWithConfig(t, Config{StepQuota: 100000, MemoryQuotaBytes: Unlimited, OutputWriter: &ampleOut}, src)
			if _, err := ample.Call(context.Background(), "run", []Value{NewString("")}, CallOptions{}); err != nil {
				t.Errorf("a 15 KiB regex failed with an ample quota: %v", err)
			}
		})
	}
}

// A segment the render built stays live while the result builder is allocated,
// so the memory peak is the builder plus those. Reserving only the builder let a
// finite quota approve an operation whose real peak was about twice what was
// checked. (Aliased segments are the caller's own memory and are excluded --
// TestTemplateReservationDoesNotCountAliasedPlaceholders covers that side.)
func TestTemplateReservesItsRetainedSegments(t *testing.T) {
	t.Parallel()

	var cache stringTemplateSegmentCache
	for i := range 12 {
		cache.store(fmt.Sprintf("k%d", i), strings.Repeat("v", 1000), true)
	}
	// Twelve built values of a thousand bytes, all still held.
	if got := cache.retainedBytes(); got < 12*1000 {
		t.Errorf("retainedBytes = %d, want at least %d; every retained segment counts "+
			"toward the peak, including those past the inline slots", got, 12*1000)
	}
}

// An unsupported pairing renders neither operand, so the sizing walk must not
// run for it: a regex plus nil or another regex is an unsupported-operands
// error, and charging the walk first replaced that error with a quota error.
func TestUnsupportedRegexConcatenationReportsItsOwnError(t *testing.T) {
	t.Parallel()

	source := strings.Repeat("a", 15<<10)
	for _, expr := range []string{
		fmt.Sprintf("/%s/ + nil", source),
		fmt.Sprintf("/%s/ + /%s/", source, source),
	} {
		t.Run(expr[:20], func(t *testing.T) {
			t.Parallel()
			src := fmt.Sprintf("def run(s)\n  %s\nend", expr)
			script := compileScriptWithConfig(t, Config{StepQuota: 8, MemoryQuotaBytes: Unlimited}, src)
			_, err := script.Call(context.Background(), "run", []Value{NewString("")}, CallOptions{})
			if err == nil {
				t.Fatal("an unsupported concatenation succeeded")
			}
			if strings.Contains(err.Error(), "quota") {
				t.Errorf("an unsupported concatenation reported %q; neither source is "+
					"rendered, so the sizing walk must not run", err)
			}
		})
	}
}

// The rate the value package charges regex source walks at must match the rate
// the runtime charges byte work at. The constant is duplicated because that
// package cannot import the runtime, so nothing but a test keeps them equal.
func TestRegexSourceStepBytesMatchesRuntime(t *testing.T) {
	t.Parallel()

	if got := value.RegexSourceStepBytesForTest(); got != stringScanBytesPerStep {
		t.Errorf("the value package charges regex source at one step per %d bytes and the "+
			"runtime charges byte work at one per %d; the duplicated constants have "+
			"drifted", got, stringScanBytesPerStep)
	}
}

// Sizing a regex must not render it at any depth, and the source walk it does
// perform must be charged. The per-site fixes covered a regex that is the whole
// value; a regex inside an array or hash recurses into the same walkers, and
// inspect uses a separate one.
//
// The assertion is that the charge follows the source length. An earlier version
// asserted only that a tight quota fails and an ample one succeeds, which holds
// whether sizing renders or not -- it could not tell a nested regex was still
// being rendered.
func TestNestedRegexSizingIsChargedForItsSource(t *testing.T) {
	t.Parallel()

	minSteps := func(shape string, sourceLen int) int {
		src := fmt.Sprintf(shape, strings.Repeat("a", sourceLen))
		lo, hi := 1, 1<<20
		for lo < hi {
			mid := (lo + hi) / 2
			script := compileScriptWithConfig(t, Config{StepQuota: mid, MemoryQuotaBytes: Unlimited}, src)
			if _, err := script.Call(context.Background(), "run", []Value{NewString("")}, CallOptions{}); err != nil {
				lo = mid + 1
			} else {
				hi = mid
			}
		}
		return lo
	}

	shapes := map[string]string{
		"nested in array": "def run(s)\n  \"#{[/%s/]}\".bytesize\nend",
		"nested in hash":  "def run(s)\n  \"#{{k: /%s/}}\".bytesize\nend",
		"inspect":         "def run(s)\n  /%s/.inspect.bytesize\nend",
		"inspect nested":  "def run(s)\n  [/%s/].inspect.bytesize\nend",
		// join reaches the public byte entry point rather than the recursive
		// walker, and a width forces the rune walk rather than the byte one.
		"join":         "def run(s)\n  [/%s/].join(\",\").bytesize\nend",
		"format width": "def run(s)\n  format(\"%%1s\", /%s/).bytesize\nend",
	}
	names := make([]string, 0, len(shapes))
	for name := range shapes {
		names = append(names, name)
	}
	for _, name := range names {
		t.Run(name, func(t *testing.T) {
			t.Parallel()
			// Two passes happen: sizing walks the source and rendering walks it
			// again, so the charge is about twice one pass. Scaling alone does
			// not discriminate -- charging only the rendered length also scales
			// with the source -- so the assertion is on magnitude.
			const sourceLen = 14 << 10
			onePass := sourceLen / stringScanBytesPerStep
			got := minSteps(shapes[name], sourceLen)
			if got < onePass*3/2 {
				t.Errorf("%s cost %d steps for a %d-byte source, under the %d that one "+
					"pass alone costs by half again; sizing walks the source and rendering "+
					"walks it again, and both must be charged", name, got, sourceLen,
					onePass)
			}
		})
	}
}

// The template reservation must cover what the render actually holds, and a
// string or symbol placeholder is handed back by reference: the segment cache
// aliases the caller's own payload rather than copying it. Counting those bytes
// projected a copy that does not exist, so a template whose real peak was the
// payload plus the result was charged for a third of it again and rejected under
// a quota it fitted.
//
// Measure the slope rather than an absolute size: the smallest quota that
// completes grows by two payloads per payload of input (the argument the caller
// holds, and the result), and by three if the cached alias is counted as well.
func TestTemplateReservationDoesNotCountAliasedPlaceholders(t *testing.T) {
	t.Parallel()

	const src = "def run(s, n)\n  \"{{v}}\".template({v: s}).bytesize\nend"
	const payload = 64 << 10
	const hi = 8 << 20

	small := minMemoryQuotaToComplete(t, src, NewString(strings.Repeat("a", payload)), 0, hi)
	large := minMemoryQuotaToComplete(t, src, NewString(strings.Repeat("a", 2*payload)), 0, hi)
	if small >= hi || large >= hi {
		t.Fatalf("binary search hit its own bound (%d, %d of %d); the measurement "+
			"would read as size-proportional whatever the projection did", small, large, hi)
	}

	// Both runs render the placeholder, so the difference is entirely the extra
	// payload: two copies of it live at the peak, not three.
	slope := float64(large-small) / float64(payload)
	if slope > 2.5 {
		t.Errorf("smallest completing quota rose from %d to %d for one more payload "+
			"of %d bytes, a slope of %.2f payloads; a string placeholder is aliased, "+
			"so only the argument and the result are live and the slope must be near 2",
			small, large, payload, slope)
	}

	// And the reservation still covers a placeholder that is materialized here:
	// a big integer's digits are built by the render and are live beside the
	// result, so those bytes must be counted.
	var cache stringTemplateSegmentCache
	cache.store("k", strings.Repeat("d", 4096), false)
	if got := cache.retainedBytes(); got != 0 {
		t.Errorf("aliased segment reserved %d bytes; it is the caller's own memory", got)
	}
	cache.store("j", strings.Repeat("d", 4096), true)
	if got := cache.retainedBytes(); got != 4096 {
		t.Errorf("built segment reserved %d bytes, want 4096; a rendering the template "+
			"allocated is live alongside the result and must be covered", got)
	}
}

// The segment cache allocates as the projection walks, so the reservation has to
// cover what it allocates and has to be checked before the allocation is
// complete. Two ways that failed: counting only built payloads left a template
// of aliased placeholders reserving nothing while the overflow map grew with the
// placeholder count, and checking only after the projection returned let the
// whole scratch be allocated past a finite quota before anything looked.
func TestTemplateScratchIsReservedAsItGrows(t *testing.T) {
	t.Parallel()

	// Distinct string placeholders: every segment is an alias, so the payloads
	// are already counted as call roots and the map storage is the whole cost.
	// Well past the eight inline slots, so the overflow map carries it.
	const placeholders = 600
	var text, pairs strings.Builder
	for i := range placeholders {
		fmt.Fprintf(&text, "{{k%d}}", i)
		if i > 0 {
			pairs.WriteString(", ")
		}
		fmt.Fprintf(&pairs, "k%d: s", i)
	}
	src := fmt.Sprintf("def run(s, n)\n  \"%s\".template({%s}).bytesize\nend",
		text.String(), pairs.String())

	// Correctness first, so a later assertion cannot pass because the template
	// failed for an unrelated reason.
	script := compileScriptWithConfig(t, Config{StepQuota: Unlimited, MemoryQuotaBytes: Unlimited}, src)
	got, err := script.Call(context.Background(), "run",
		[]Value{NewString("abc"), NewInt(0)}, CallOptions{})
	if err != nil {
		t.Fatalf("%d placeholders: %v", placeholders, err)
	}
	if got.Kind() != KindInt || got.Int() != placeholders*3 {
		t.Fatalf("rendered length %v, want %d", got, placeholders*3)
	}

	// The map storage the cache allocates has to be reserved. Isolate it by
	// holding the context constant: the same six hundred keys either way, so the
	// context hash costs the same and cannot be what the difference measures.
	// Referencing one of them caches one segment inline; referencing all six
	// hundred fills the overflow map.
	oneRef := fmt.Sprintf("def run(s, n)\n  \"{{k0}}\".template({%s}).bytesize\nend",
		pairs.String())
	sixHundredRefs := minMemoryQuotaToComplete(t, src, NewString("abc"), 0, 4<<20)
	oneRefQuota := minMemoryQuotaToComplete(t, oneRef, NewString("abc"), 0, 4<<20)
	if sixHundredRefs >= 4<<20 || oneRefQuota >= 4<<20 {
		t.Fatalf("binary search hit its own bound (%d, %d); the comparison would read "+
			"as cache-proportional whatever the reservation did", sixHundredRefs, oneRefQuota)
	}
	overflowCost := placeholders * estimatedTemplateOverflowEntryBytes
	// The rendered result also grows, by three bytes per placeholder, which is a
	// twentieth of the map cost and cannot account for the difference.
	if got := sixHundredRefs - oneRefQuota; got < overflowCost/2 {
		t.Errorf("caching %d segments raised the smallest completing quota by %d bytes; "+
			"the overflow map alone costs about %d and the reservation must cover it",
			placeholders, got, overflowCost)
	}
}

// Checking the scratch only once the projection had finished still rejected an
// oversized template, so the outcome cannot tell the two apart -- the projection
// accumulates and frees nothing, so its peak is its end state. What differs is
// how much the process allocates before the rejection: every placeholder is
// rendered first and only then is anything checked, which is the opposite of
// what a memory quota is for.
//
// Sixty placeholders rendering a 900,000-bit integer each. Building the context
// costs the same either way, so the difference is the renderings the check
// avoids once it fires.
//
// Not parallel: runtime.MemStats is process-wide, and a concurrent test's
// allocations would land in the delta.
func TestTemplateProjectionStopsAllocatingWhenItIsRejected(t *testing.T) {
	const (
		placeholders = 60
		quota        = 8 << 20
	)
	var text, pairs strings.Builder
	for i := range placeholders {
		fmt.Fprintf(&text, "{{k%d}}", i)
		if i > 0 {
			pairs.WriteString(", ")
		}
		fmt.Fprintf(&pairs, "k%d: 2 ** %d", i, 900000+i)
	}
	src := fmt.Sprintf("def run(s, n)\n  \"%s\".template({%s}).bytesize\nend",
		text.String(), pairs.String())

	// Control: the context alone has to fit, or the rejection below would be the
	// hash literal being too big and the allocation assertion would pass without
	// the template ever running.
	ctxOnly := fmt.Sprintf("def run(s, n)\n  {%s}.length\nend", pairs.String())
	script := compileScriptWithConfig(t,
		Config{StepQuota: Unlimited, MemoryQuotaBytes: quota}, ctxOnly)
	if _, err := script.Call(context.Background(), "run",
		[]Value{NewString(""), NewInt(0)}, CallOptions{}); err != nil {
		t.Fatalf("the context alone does not fit the %d-byte quota (%v); the template "+
			"is no longer what this measures", quota, err)
	}

	script = compileScriptWithConfig(t,
		Config{StepQuota: Unlimited, MemoryQuotaBytes: quota}, src)
	var before, after runtime.MemStats
	runtime.GC()
	runtime.ReadMemStats(&before)
	_, err := script.Call(context.Background(), "run",
		[]Value{NewString(""), NewInt(0)}, CallOptions{})
	runtime.ReadMemStats(&after)
	if err == nil {
		t.Fatalf("%d multi-megabyte renderings completed under a %d-byte quota",
			placeholders, quota)
	}

	// Measured at about 4x the quota when the check fires as the scratch grows
	// and 14x when it runs only at the end, almost all of it renderings the
	// projection had no business performing.
	allocated := after.TotalAlloc - before.TotalAlloc
	if allocated > 8*quota {
		t.Errorf("rejecting the template allocated %.1f MiB against a %d MiB quota; the "+
			"projection must check as the scratch grows, not render every placeholder "+
			"and check once it has finished",
			float64(allocated)/(1<<20), quota>>20)
	}
}
