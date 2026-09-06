package parser

import (
	"strings"
	"testing"

	"github.com/mgomes/vibescript/internal/ast"
)

// multiplicationContinuationLines builds a body whose every line after the
// first begins with "*". Each of them asks whether it opens a destructuring
// assignment or continues the line before it as a multiplication, which is the
// question the lookahead answers.
func multiplicationContinuationLines(lines int) string {
	var sb strings.Builder
	sb.WriteString("def run\n  a\n")
	// Keep each expression shallow while probing positions throughout the file.
	for i := range lines {
		if i > 0 && i%32 == 0 {
			sb.WriteString("  a\n")
		}
		sb.WriteString("  * a\n")
	}
	sb.WriteString("end\n")
	return sb.String()
}

// Asking whether a line-leading "*" opens a destructuring assignment must not
// re-read the source before it. The lookahead resolved the "*" position by
// counting lines from byte 0 and then rebuilt its lexer state by re-lexing from
// byte 0 as well, both because it opened a fresh source replay instead of
// sharing the one for the source being parsed. 8,000 such lines, 48 KB, took
// 10.5s and allocated 2.1 GB, all of it during compile where no step or memory
// quota reaches it (#38).
//
// This counts the input bytes the re-walks cover rather than timing them:
// elapsed time would fold in scheduling, GC, and the race and coverage
// instrumentation this repository runs across three operating systems. It does
// not call t.Parallel because sourceReplayCounting and sourceReplayBytes are
// process-wide.
func TestSplatLookaheadDoesNotRewalkTheSourceBeforeIt(t *testing.T) {
	measure := func(t *testing.T, src string) uint64 {
		t.Helper()

		sourceReplayBytes.Store(0)
		sourceReplayCounting.Store(true)
		defer sourceReplayCounting.Store(false)
		if _, errs := Parse(src); len(errs) != 0 {
			t.Fatalf("the source no longer parses cleanly, so it no longer exercises the lookahead: %v", errs[0])
		}
		return sourceReplayBytes.Load()
	}

	small := measure(t, multiplicationContinuationLines(500))
	large := measure(t, multiplicationContinuationLines(2000))

	// Measured 6,020 then 24,020 bytes, so four times the lines re-walk four
	// times the source. Before, the same pair re-walked 3,024,984 then
	// 48,099,984: sixteen times for four times the lines. The assertion allows
	// 8x so it states that the re-walk no longer compounds rather than pinning
	// counts that ordinary lexer changes would shift.
	if large > small*8 {
		t.Fatalf("four times the continuation lines re-walked %d source bytes against %d -- over 8x, so every"+
			" line-leading \"*\" reads the source before it again", large, small)
	}
}

// unclosedGroupContinuationLines builds a body whose every line after the first
// begins with "*" and opens a bracket group it never closes. The lookahead
// keeps reading past a newline whenever the target list is mid-continuation,
// and an open group holds it mid-continuation for the rest of the file.
func unclosedGroupContinuationLines(lines int) string {
	var sb strings.Builder
	sb.WriteString("a\n")
	for range lines {
		sb.WriteString("  * (b\n")
	}
	return sb.String()
}

// The lookahead behind one "*" must not read the whole file. It stopped only
// at a top-level "=", a token that cannot appear in a target list, or end of
// input, so under an unclosed bracket group every "*" read to end of input:
// 4,000 such lines, 28 KB, walked 56 MB and took over a second, quadrupling
// per doubling (#38).
//
// It does not call t.Parallel because splatScanCounting and splatScanBytes are
// process-wide.
func TestSplatLookaheadDoesNotReadToTheEndOfTheSource(t *testing.T) {
	measure := func(t *testing.T, src string) uint64 {
		t.Helper()

		splatScanBytes.Store(0)
		splatScanCounting.Store(true)
		defer splatScanCounting.Store(false)
		Parse(src)
		walked := splatScanBytes.Load()
		if walked == 0 {
			t.Fatal("the source no longer reaches the lookahead, so it no longer exercises it")
		}
		return walked
	}

	small := measure(t, unclosedGroupContinuationLines(500))
	large := measure(t, unclosedGroupContinuationLines(2000))

	// Measured 213,245 then 897,245 bytes, so four times the lines walk four
	// times as far: each line is read by the bounded number of lookaheads that
	// can reach it. Without the bound the same pair walked 875,750 then
	// 14,003,000, sixteen times for four times the lines. The assertion allows
	// 8x so it states that the walk no longer compounds rather than pinning
	// counts that ordinary parser changes would shift.
	if large > small*8 {
		t.Fatalf("four times the continuation lines walked %d lookahead bytes against %d -- over 8x, so a"+
			" line-leading \"*\" reads to the end of the source again", large, small)
	}
}

// splatTargetListSpanning builds a destructuring assignment whose target list
// runs the given number of lines past the leading "*", by splitting a
// parenthesized sub-target across that many lines. The "*" is line-leading and
// follows an expression, so it is ambiguous and the lookahead has to read to
// the "=" to settle it.
func splatTargetListSpanning(lines int) string {
	return "x = a\n  *rest, (m,\n" + strings.Repeat("  n,\n", lines-1) + "  o) = values\n"
}

// The bound has to be reachable, or it would be bounding something other than
// what it claims to: a target list whose "=" sits exactly maxSplatTargetLines
// lines past the "*" still reads as a destructuring assignment.
func TestSplatTargetListAtTheBoundStillParses(t *testing.T) {
	t.Parallel()

	program, errs := Parse(splatTargetListSpanning(maxSplatTargetLines))
	if len(errs) != 0 {
		t.Fatalf("a target list spanning %d lines no longer parses: %v", maxSplatTargetLines, errs[0])
	}
	if len(program.Statements) != 2 {
		t.Fatalf("parsed %d statements, want 2", len(program.Statements))
	}
	assign, ok := program.Statements[1].(*ast.AssignStmt)
	if !ok {
		t.Fatalf("second statement is %T, want an assignment", program.Statements[1])
	}
	if _, ok := assign.Target.(*ast.DestructureTarget); !ok {
		t.Fatalf("assignment target is %T, want a destructuring target", assign.Target)
	}
}

// One line further the lookahead stops before the "=", so the "*" reads as the
// multiplication it was ambiguous with. That is the whole behavior change the
// bound makes, and it is here so it cannot move without being noticed.
func TestSplatTargetListPastTheBoundReadsAsMultiplication(t *testing.T) {
	t.Parallel()

	program, errs := Parse(splatTargetListSpanning(maxSplatTargetLines + 1))
	if len(errs) == 0 {
		t.Fatalf("a target list spanning %d lines still parses as a destructuring assignment", maxSplatTargetLines+1)
	}
	if len(program.Statements) < 2 {
		t.Fatalf("parsed %d statements, want the multiplication to have joined the first", len(program.Statements))
	}
	if _, ok := program.Statements[1].(*ast.AssignStmt); ok {
		t.Fatal("second statement is still an assignment, so the lookahead read past the bound")
	}
}
