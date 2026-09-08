package runtime

import (
	"fmt"
	"strings"
	"testing"
)

func scalarLocalSource(locals int) string {
	var source strings.Builder
	source.WriteString("def run(x: int)\n")
	for i := range locals {
		fmt.Fprintf(&source, "  value_%d = %d\n", i, i)
	}
	source.WriteString("  1\nend\n")
	return source.String()
}

func TestCheckScalarLocalAllocationStaysLinear(t *testing.T) {
	// Comparing every scalar with all prior locals allocated an empty mutable
	// container map for every pair. Doubling locals must stay below 3x bytes.
	small := measureCheckAllocation(t, scalarLocalSource(200))
	large := measureCheckAllocation(t, scalarLocalSource(400))
	if large > small*3 {
		t.Fatalf("CheckWarnings with 400 scalar locals allocated %d bytes, want at most 3x the %d bytes for 200 locals", large, small)
	}
}

func TestCheckScalarLocalWorkStaysLinear(t *testing.T) {
	small := measureCheckWork(t, scalarLocalSource(200))
	large := measureCheckWork(t, scalarLocalSource(400))
	if large > small*3 {
		t.Fatalf("CheckWarnings with 400 scalar locals inspected %d elements, want at most 3x the %d elements for 200 locals", large, small)
	}
}

func BenchmarkCheckScalarLocals(b *testing.B) {
	for _, locals := range []int{100, 200, 400, 800} {
		b.Run(fmt.Sprint(locals), func(b *testing.B) {
			script := compileScriptWithEngine(b, benchmarkEngine(), scalarLocalSource(locals))
			b.ReportAllocs()
			b.ResetTimer()
			for range b.N {
				if warnings := script.CheckWarnings(); len(warnings) != 0 {
					b.Fatalf("CheckWarnings with %d scalar locals = %v, want no warnings", locals, warnings)
				}
			}
		})
	}
}

func TestBindScalarLocalStaticValuesKeepsFacts(t *testing.T) {
	t.Parallel()

	for _, value := range []Expression{
		&IntegerLiteral{Value: 1},
		&FloatLiteral{Value: 1.5},
		&BoolLiteral{Value: true},
		&BoolLiteral{Value: false},
		&NilLiteral{},
		&StringLiteral{Value: "text"},
		&SymbolLiteral{Name: "ready"},
	} {
		t.Run(fmt.Sprintf("%T/%v", value, value), func(t *testing.T) {
			t.Parallel()
			checker := &scriptChecker{
				localTypes:       []checkTypeFrame{{"original": nil, "copied": nil}},
				localClassValues: []checkClassValueFrame{nil},
			}
			checker.bindLocalStaticValues("original", []Expression{value})
			checker.bindLocalStaticValues("copied", []Expression{value})
			for _, name := range []string{"original", "copied"} {
				values, exact := checker.localStaticValuesFor(name)
				if !exact || len(values) != 1 || values[0] != value {
					t.Errorf("localStaticValuesFor(%q) = %v, %t, want exact value %v", name, values, exact, value)
				}
			}
			checker.poisonLocalStaticValues("original")
			if values, exact := checker.localStaticValuesFor("copied"); !exact {
				t.Errorf("localStaticValuesFor(copied) after poisoning original = %v, false, want exact value %v", values, value)
			}
		})
	}
}

func TestBindLocalStaticValuesKeepsMixedContainerAlternatives(t *testing.T) {
	t.Parallel()

	for _, container := range []Expression{&ArrayLiteral{}, &HashLiteral{}} {
		t.Run(fmt.Sprintf("%T", container), func(t *testing.T) {
			t.Parallel()
			checker := &scriptChecker{
				localTypes:       []checkTypeFrame{{"original": nil, "copied": nil}},
				localClassValues: []checkClassValueFrame{nil},
			}
			checker.bindLocalStaticValues("original", []Expression{container})
			checker.bindLocalStaticValues("copied", []Expression{&NilLiteral{}, container})
			checker.poisonLocalStaticValues("original")
			if values, exact := checker.localStaticValuesFor("copied"); exact {
				t.Errorf("localStaticValuesFor(copied) after poisoning original = %v, true, want unknown", values)
			}
		})
	}
}

func TestBindLocalStaticValuesKeepsContainerDependencies(t *testing.T) {
	t.Parallel()

	for _, names := range [][]string{{"child", "parent"}, {"parent", "child"}} {
		t.Run(strings.Join(names, " then "), func(t *testing.T) {
			t.Parallel()
			child := &ArrayLiteral{Elements: []Expression{&IntegerLiteral{Value: 1}}}
			parent := &HashLiteral{Pairs: []HashPair{{
				Key:   &SymbolLiteral{Name: "child"},
				Value: child,
			}}}
			checker := &scriptChecker{
				localTypes:       []checkTypeFrame{{"child": nil, "parent": nil}},
				localClassValues: []checkClassValueFrame{nil},
			}
			values := map[string]Expression{"child": child, "parent": parent}
			for _, name := range names {
				checker.bindLocalStaticValues(name, []Expression{values[name]})
			}
			checker.poisonLocalStaticValues("child")
			if values, exact := checker.localStaticValuesFor("parent"); exact {
				t.Errorf("localStaticValuesFor(parent) after poisoning child = %v, true, want unknown", values)
			}
		})
	}
}

func TestCheckScalarLocalsPreserveContainerRelationships(t *testing.T) {
	t.Parallel()

	for _, tc := range []struct {
		name  string
		setup string
		write string
		read  string
	}{
		{
			name:  "same array root",
			setup: "original = [1]\n  other = original",
			write: `other[0] = "text"`,
			read:  "original[0]",
		},
		{
			name:  "same hash root",
			setup: "original = { value: 1 }\n  other = original",
			write: `other[:value] = "text"`,
			read:  "original[:value]",
		},
		{
			name:  "old contains new",
			setup: "outer = [{ value: 1 }]\n  inner = outer[0]",
			write: `inner[:value] = "text"`,
			read:  "outer[0][:value]",
		},
		{
			name:  "shared nested array",
			setup: "inner = [1]\n  left = [inner]\n  right = { value: inner }",
			write: `left[0][0] = "text"`,
			read:  "right[:value][0]",
		},
		{
			name:  "shared nested hash",
			setup: "inner = { value: 1 }\n  left = { child: inner }\n  right = { child: inner }",
			write: `left[:child][:value] = "text"`,
			read:  "right[:child][:value]",
		},
		{
			name:  "destructured array",
			setup: "outer = [[1]]\n  inner, padding = outer",
			write: `inner[0] = "text"`,
			read:  "outer[0][0]",
		},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			source := fmt.Sprintf(`
def takes_string(value: string)
  value
end

def run()
  scalar = :ready
  %s
  copied = scalar
  %%s
  takes_string(%s)
end
`, tc.setup, tc.read)
			requireCheckWarningContains(t, compileScript(t, fmt.Sprintf(source, "")),
				"call to takes_string argument value expected string, got int")
			requireNoCheckWarnings(t, compileScript(t, fmt.Sprintf(source, tc.write)))
		})
	}
}
