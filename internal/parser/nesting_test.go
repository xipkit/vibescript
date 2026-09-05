package parser

import (
	"strings"
	"testing"

	"github.com/mgomes/vibescript/internal/ast"
)

func TestSyntaxNesting(t *testing.T) {
	t.Parallel()

	tests := []struct {
		name   string
		prefix string
		inner  string
		suffix string
	}{
		{"unary", "!", "true", ""},
		{"groups", "(", "1", ")"},
		{"arrays", "[", "1", "]"},
		{"hashes", "{a: ", "1", "}"},
		{"calls", "f(", "1", ")"},
		{"indices", "a[", "0", "]"},
		{"powers", "1 ** ", "1", ""},
		{"ternaries", "true ? 1 : ", "1", ""},
		{"if", "if true\n", "1\n", "end\n"},
		{"while", "while false\n", "1\n", "end\n"},
		{"begin", "begin\n", "1\n", "ensure\n1\nend\n"},
		{"blocks", "f { ", "it", " }"},
		{"classes", "class A\n", "", "end\n"},
		{"modules", "module A\n", "", "end\n"},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			for _, depth := range []int{16, 1024} {
				source := strings.Repeat(tc.prefix, depth) + tc.inner + strings.Repeat(tc.suffix, depth)
				_, errs := Parse(source)
				if depth == 16 {
					if len(errs) != 0 {
						t.Fatalf("Parse at depth %d: %v", depth, errs[0])
					}
					continue
				}
				if len(errs) == 0 || !strings.Contains(parseErrorMessage(errs[0]), "nesting too deep") {
					t.Errorf("Parse at depth %d errors = %v, want nesting limit", depth, errs)
				}
			}
		})
	}
}

func TestSyntaxNestingBoundary(t *testing.T) {
	t.Parallel()
	for _, depth := range []int{maxSyntaxDepth - 1, maxSyntaxDepth} {
		_, errs := Parse(strings.Repeat("(", depth-1) + "1" + strings.Repeat(")", depth-1))
		if (len(errs) == 0) != (depth < maxSyntaxDepth) {
			t.Errorf("Parse expression depth %d errors = %v", depth, errs)
		}
	}
	for _, depth := range []int{maxSyntaxDepth - 1, maxSyntaxDepth} {
		_, errs := Parse("x" + strings.Repeat(".x", depth-1))
		if (len(errs) == 0) != (depth < maxSyntaxDepth) {
			t.Errorf("Parse AST depth %d errors = %v", depth, errs)
		}
	}
}

func TestSyntaxNestingAcrossInterpolations(t *testing.T) {
	t.Parallel()
	depth := maxSyntaxDepth * 3 / 4
	inner := strings.Repeat("(", depth) + "1" + strings.Repeat(")", depth)
	_, errs := Parse(inner)
	if len(errs) != 0 {
		t.Fatalf("control expression: %v", errs[0])
	}
	source := strings.Repeat("(", depth) + `"#{` + inner + `}"` + strings.Repeat(")", depth)
	_, errs = Parse(source)
	if len(errs) != 1 || parseErrorMessage(errs[0]) != "syntax nesting too deep" {
		t.Errorf("Parse combined nesting errors = %v, want nesting limit", errs)
	}
}

func TestSyntaxNestingStopsToolingAndSpeculation(t *testing.T) {
	t.Parallel()
	chain := "x" + strings.Repeat(".x", 1024)
	for _, source := range []string{
		chain + ".probe",
		"x.probe\n" + chain,
		"f(int | " + chain + ")",
		"def f(x: {a: " + chain + "})\nend",
	} {
		program, errs := Parse(source)
		if len(errs) != 1 || parseErrorMessage(errs[0]) != "syntax nesting too deep" {
			t.Fatalf("Parse errors = %v, want nesting limit", errs)
		}
		if len(program.Statements) != 0 {
			t.Error("Parse returned a partial program after nesting rejection")
		}
		if _, _, ok := MemberReceiverFor(source, "probe"); ok {
			t.Error("MemberReceiverFor returned a receiver after nesting rejection")
		}
	}
}

func TestSyntaxNestingWideInputs(t *testing.T) {
	t.Parallel()
	for _, source := range []string{
		"[" + strings.Repeat("1,", 10_000) + "1]",
		strings.Repeat("x = 1\n", 10_000),
		"f(" + strings.Repeat("1,", 10_000) + "1)" + strings.Repeat(" {}", 1000),
	} {
		if _, errs := Parse(source); len(errs) != 0 {
			t.Errorf("Parse shallow source: %v", errs[0])
		}
	}
}

func TestSyntaxNestingRejectsLargeInputs(t *testing.T) {
	t.Parallel()
	for _, source := range []string{
		strings.Repeat("!", 500_000) + "true",
		"x" + strings.Repeat(".x", 250_000),
		strings.Repeat("if true\n", 50_000) + "1\n" + strings.Repeat("end\n", 50_000),
	} {
		if len(source) >= 1<<20 {
			t.Fatal("fixture exceeds the default source-size limit")
		}
		_, errs := Parse(source)
		if len(errs) != 1 || parseErrorMessage(errs[0]) != "syntax nesting too deep" {
			t.Errorf("Parse large source errors = %v, want nesting limit", errs)
		}
	}
}

func TestSyntaxNestingUpdatesTrailingBlock(t *testing.T) {
	t.Parallel()
	deepBlock := " { x" + strings.Repeat(".x", maxSyntaxDepth-5) + " }"
	_, errs := Parse("f(1)" + deepBlock + ".x.x.x")
	if len(errs) != 1 || parseErrorMessage(errs[0]) != "syntax nesting too deep" {
		t.Errorf("Parse call with deep block errors = %v, want nesting limit", errs)
	}
	_, errs = Parse("f(1)" + deepBlock + " {}.x.x.x")
	if len(errs) != 0 {
		t.Errorf("Parse call after replacing deep block: %v", errs[0])
	}
}

func TestSyntaxNestingSurvivesSnapshotRestore(t *testing.T) {
	t.Parallel()
	p := newParser("x")
	saved := p.snapshot()
	p.rejectNesting(ast.Position{Line: 1, Column: 1})
	p.restore(saved)
	_, errs := p.parseProgram()
	if len(errs) != 1 || parseErrorMessage(errs[0]) != "syntax nesting too deep" {
		t.Errorf("Parse after restore errors = %v, want nesting limit", errs)
	}
}

func TestExpressionChainNesting(t *testing.T) {
	t.Parallel()
	for _, suffix := range []string{" + 1", ".x", "::X", "[0]", "()", " rescue 1"} {
		t.Run(suffix, func(t *testing.T) {
			t.Parallel()
			for _, depth := range []int{16, 1024} {
				source := "f { x" + strings.Repeat(suffix, depth) + " }"
				_, errs := Parse(source)
				if depth == 16 {
					if len(errs) != 0 {
						t.Fatalf("Parse chain at depth %d: %v", depth, errs[0])
					}
					continue
				}
				if len(errs) == 0 || !strings.Contains(parseErrorMessage(errs[0]), "nesting too deep") {
					t.Errorf("Parse chain at depth %d errors = %v, want nesting limit", depth, errs)
				}
			}
		})
	}
}

func TestDestructureNesting(t *testing.T) {
	t.Parallel()
	for _, depth := range []int{16, 1024} {
		source := "a, " + strings.Repeat("(", depth) + "b, c" + strings.Repeat(")", depth) + " = values"
		_, errs := Parse(source)
		if depth == 16 {
			if len(errs) != 0 {
				t.Fatalf("Parse destructure at depth %d: %v", depth, errs[0])
			}
			continue
		}
		if len(errs) == 0 || !strings.Contains(parseErrorMessage(errs[0]), "nesting too deep") {
			t.Errorf("Parse destructure at depth %d errors = %v, want nesting limit", depth, errs)
		}
	}
}
