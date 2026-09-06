package parser

import (
	"runtime"
	"strings"
	"testing"

	"github.com/mgomes/vibescript/internal/ast"
)

// Each level of a nested interpolation is read twice over: the lexer drives a
// throwaway lexer across the whole body to find the "}" that closes it, and the
// parser then reparses that body from scratch. The levels inside a given one
// were therefore walked again, and copied again, for every level outside it, so
// 1,600 levels of a 8 KB source took 68s and allocated 25 GB, and the reported
// 20,000 levels of a 100 KB source took 23s and over a GB of live memory, all
// of it during compile where no step or memory quota reaches it (#46).

// nestedInterpolation wraps expr in depth interpolated strings.
func nestedInterpolation(depth int, expr string) string {
	for range depth {
		expr = `"#{` + expr + `}"`
	}
	return expr + "\n"
}

// interpolationNesting counts the interpolated strings stacked inside the one
// the source above declares.
func interpolationNesting(t *testing.T, program *ast.Program) int {
	t.Helper()

	if len(program.Statements) != 1 {
		t.Fatalf("expected one statement, got %d", len(program.Statements))
	}
	stmt, ok := program.Statements[0].(*ast.ExprStmt)
	if !ok {
		t.Fatalf("expected an expression statement, got %T", program.Statements[0])
	}

	depth := 0
	expr := stmt.Expr
	for {
		str, ok := expr.(*ast.InterpolatedString)
		if !ok || len(str.Parts) != 1 {
			return depth
		}
		part, ok := str.Parts[0].(ast.StringExpr)
		if !ok {
			return depth
		}
		depth++
		expr = part.Expr
	}
}

// The cap has to be reachable, or it would be bounding something other than
// what it claims to: nesting exactly maxInterpolationDepth interpolations still
// parses, and parses whole rather than stopping short.
func TestInterpolationNestingAtTheCapParses(t *testing.T) {
	t.Parallel()

	program, errs := Parse(nestedInterpolation(maxInterpolationDepth, "x"))
	if len(errs) != 0 {
		t.Fatalf("nesting %d interpolations no longer parses: %v", maxInterpolationDepth, errs[0])
	}
	if depth := interpolationNesting(t, program); depth != maxInterpolationDepth {
		t.Fatalf("parsed %d nested interpolations, want %d", depth, maxInterpolationDepth)
	}
}

// One past the cap is a parse error naming the nesting, reported against the
// outermost string rather than as an unterminated one, and reported the same
// way wherever the literal is written.
//
// The parenless-argument forms are the ones that took work: there the lexer
// read the `%` as modulo and the parser second-guesses it with a speculative
// scan, and a scan that answered "not a percent literal" to a refusal left the
// `%` as modulo. The `#{` then opened a comment that swallowed the rest of the
// line, so the source came back as whatever the remains of the line failed on.
func TestInterpolationNestingPastTheCapIsRejected(t *testing.T) {
	t.Parallel()

	overCap := nestedInterpolation(maxInterpolationDepth, "a")
	for _, tc := range []struct {
		name string
		src  string
	}{
		{"nested strings", nestedInterpolation(maxInterpolationDepth+1, "x")},
		{"through a percent array literal", "x = %W[#{" + overCap + "}]"},
		{"through a parenless percent array argument", "puts %W[#{" + overCap + "}]"},
		{"through a parenless percent symbol argument", "puts %I[#{" + overCap + "}]"},
		{"parenless with the interpolation mid-word", "puts %W[a#{" + overCap + "}b]"},
		{"parenless across lines", "puts %W[a\n#{" + overCap + "}\nb]"},
		// 100,004 bytes at 20,000 levels, the size the report used. Rejecting
		// it takes under a millisecond.
		{"the reported source", nestedInterpolation(20_000, "x")},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()

			_, errs := Parse(tc.src)
			if len(errs) == 0 {
				t.Fatal("the source parsed without a diagnostic, so nothing bounds the nesting")
			}
			if !strings.Contains(errs[0].Error(), interpolationTooDeepMessage) {
				t.Fatalf("first diagnostic does not name the nesting: %v", errs[0])
			}
		})
	}
}

// Within the cap, wrapping a body in interpolations must not multiply what
// parsing it costs. An interpolated string's raw text was rebuilt rune by rune
// as the lexer read it, so every level that enclosed a body copied that whole
// body again.
//
// This compares what one body allocates at one level and at the deepest allowed
// one rather than timing either: allocated bytes are an exact running total,
// while elapsed time would fold in scheduling, GC, and the race and coverage
// instrumentation this repository runs across three operating systems. It does
// not call t.Parallel because the total counts every goroutine's allocations.
func TestNestedInterpolationDoesNotCopyItsBodyPerLevel(t *testing.T) {
	var body strings.Builder
	body.WriteString("[")
	for body.Len() < 64<<10 {
		body.WriteString("a + b + c + d, ")
	}
	body.WriteString("e]")

	measure := func(t *testing.T, depth int) uint64 {
		t.Helper()

		src := nestedInterpolation(depth, body.String())
		var before, after runtime.MemStats
		runtime.GC()
		runtime.ReadMemStats(&before)
		_, errs := Parse(src)
		runtime.ReadMemStats(&after)
		if len(errs) != 0 {
			t.Fatalf("nesting %d interpolations around the body no longer parses: %v", depth, errs[0])
		}
		return after.TotalAlloc - before.TotalAlloc
	}

	shallow := measure(t, 1)
	deep := measure(t, maxInterpolationDepth)

	// Measured 1,576,200 bytes at one level and 1,618,024 at eight, so the
	// levels cost about what their own source costs. Before, the same pair
	// allocated 1,861,624 and 19,885,800: eight levels for eleven times the
	// memory. The assertion allows 2x so it states that the levels no longer
	// multiply the body rather than pinning byte counts that ordinary parser
	// changes would shift.
	if deep > shallow*2 {
		t.Fatalf("nesting the body %d deep took the parse from %d to %d allocated bytes -- over 2x, so every"+
			" level copies the whole body again", maxInterpolationDepth, shallow, deep)
	}
}
