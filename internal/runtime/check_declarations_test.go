package runtime

import (
	"fmt"
	"path/filepath"
	"strings"
	"testing"
)

func independentCheckDeclarationsSource(kind string, count int) string {
	var source strings.Builder
	if kind == "methods" {
		source.WriteString("class Helpers\n")
	}
	for i := range count {
		switch kind {
		case "functions", "methods":
			fmt.Fprintf(&source, "def helper_%d\n  1\nend\n", i)
		case "classes":
			fmt.Fprintf(&source, "class Helper%d\n  def value\n    1\n  end\nend\n", i)
		case "enums":
			fmt.Fprintf(&source, "enum State%d\n  Ready\nend\ndef helper_%d\n  1\nend\n", i, i)
		}
	}
	if kind == "methods" {
		source.WriteString("end\n")
	}
	return source.String()
}

func TestCheckIndependentDeclarationAllocationStaysLinear(t *testing.T) {
	for _, kind := range []string{"functions", "methods", "classes", "enums"} {
		t.Run(kind, func(t *testing.T) {
			small := measureCheckAllocation(t, independentCheckDeclarationsSource(kind, 100))
			large := measureCheckAllocation(t, independentCheckDeclarationsSource(kind, 200))
			if large > small*3 {
				t.Errorf("checking 200 independent %s allocated %d bytes versus %d for 100, want at most 3x", kind, large, small)
			}
		})
	}
}

func TestCheckIndependentDeclarationWorkStaysLinear(t *testing.T) {
	for _, kind := range []string{"methods", "classes"} {
		t.Run(kind, func(t *testing.T) {
			small := measureCheckWork(t, independentCheckDeclarationsSource(kind, 100))
			large := measureCheckWork(t, independentCheckDeclarationsSource(kind, 200))
			if small == 0 {
				t.Fatalf("checking 100 independent %s inspected no declarations, want the regression fixture to reach declaration work", kind)
			}
			if large > small*3 {
				t.Errorf("checking 200 independent %s inspected %d declarations versus %d for 100, want at most 3x", kind, large, small)
			}
		})
	}
}

func BenchmarkCheckIndependentDeclarations(b *testing.B) {
	for _, kind := range []string{"functions", "methods", "classes", "enums"} {
		for _, count := range []int{100, 200, 400} {
			b.Run(fmt.Sprintf("%s/%d", kind, count), func(b *testing.B) {
				script := compileScriptWithEngine(b, benchmarkEngine(), independentCheckDeclarationsSource(kind, count))
				b.ReportAllocs()
				b.ResetTimer()
				for range b.N {
					if warnings := script.CheckWarnings(); len(warnings) != 0 {
						b.Fatalf("CheckWarnings() = %v, want no warnings", warnings)
					}
				}
			})
		}
	}
}

func TestCheckDeclarationContextKeys(t *testing.T) {
	t.Parallel()
	const source = `
def helper
  1
end
class Box
end
enum State
  Ready
end
`
	script := compileScript(t, source)
	root := checkTypeRoot(script, nil)
	before := moduleCheckContextKey(root)
	snapshot := cloneCheckRoot(root)
	for _, name := range []string{"helper", "Box", "State"} {
		if _, ok := root.Get(name); !ok {
			t.Fatalf("Get(%q) = missing, want declared value", name)
		}
		if _, ok := checkRootOwnBinding(snapshot, name); !ok {
			t.Fatalf("snapshot binding %q is missing, want declared value", name)
		}
	}
	if after := moduleCheckContextKey(root); after != before {
		t.Errorf("declaration lookups changed checker context from %q to %q", before, after)
	}
	if got := moduleCheckContextKey(snapshot); got != before {
		t.Errorf("snapshot has checker context %q, want %q", got, before)
	}
	other := compileScriptWithEngine(t, script.engine, source)
	if got := moduleCheckContextKey(checkTypeRoot(other, nil)); got == before {
		t.Error("distinct declaration tables have the same checker context")
	}
	root.Define("helper", NewInt(2))
	if got := moduleCheckContextKey(root); got == before {
		t.Error("overriding a declaration did not change the checker context")
	}
}

func TestCheckTypeRootDeclarationPrecedence(t *testing.T) {
	t.Parallel()
	script := compileScript(t, `
def helper
  1
end
class Box
end
enum State
  Ready
end
`)
	globals := map[string]Value{
		"helper": NewString("override"),
		"Box":    NewString("override"),
		"State":  NewString("override"),
	}
	for _, override := range []bool{false, true} {
		parent := newEnv(nil)
		for name := range globals {
			parent.Define(name, NewInt(2))
		}
		root := checkTypeRootWithParentAndGlobals(script, globals, parent, override)
		for name, declarationKind := range map[string]ValueKind{
			"helper": KindFunction,
			"Box":    KindClass,
			"State":  KindEnum,
		} {
			want := declarationKind
			if override {
				want = KindString
			}
			val, found := checkRootBinding(root, name)
			if !found || val.Kind() != want {
				t.Errorf("override=%v binding %s = %v (found %v), want %s", override, name, val.Kind(), found, want)
			}
		}
	}
}

func TestCheckTypeRootsKeepClassStateIsolated(t *testing.T) {
	t.Parallel()
	script := compileScript(t, `
module Outer
  module Inner
    def self.answer
      1
    end
  end
end
`)
	first := checkTypeRoot(script, nil)
	second := checkTypeRoot(script, nil)
	firstValue, firstFound := first.Get("Outer")
	secondValue, secondFound := second.Get("Outer")
	if !firstFound || !secondFound {
		t.Fatal("checkTypeRoot did not bind Outer")
	}
	firstClass := valueClass(firstValue)
	secondClass := valueClass(secondValue)
	firstClass.ClassVars["written"] = NewInt(42)
	if _, found := secondClass.ClassVars["written"]; found {
		t.Error("class state escaped into another check root")
	}
	firstInner := valueClass(firstClass.ClassVars["Inner"])
	secondInner := valueClass(secondClass.ClassVars["Inner"])
	if firstInner == nil || secondInner == nil || firstInner == secondInner {
		t.Fatal("nested module declarations are not isolated between check roots")
	}
	inner, found := first.Get("Outer::Inner")
	if !found || valueClass(inner) != firstInner {
		t.Error("qualified nested module binding differs from its parent's constant")
	}
	firstInner.ClassVars["written"] = NewInt(42)
	if _, found := secondInner.ClassVars["written"]; found {
		t.Error("nested module state escaped into another check root")
	}
}

func TestCheckIndependentFunctionsKeepNamespaceContracts(t *testing.T) {
	t.Parallel()
	script := compileScript(t, `
def take(value: int)
  value
end

def a_mutate
  JSON.stringify = 1
end

def z_check
  take(JSON.stringify(1))
end
`)
	for range 2 {
		warnings := script.CheckWarnings()
		if len(warnings) != 1 || warnings[0].Function != "z_check" ||
			warnings[0].Message != "call to take argument value expected int, got string" {
			t.Errorf("CheckWarnings() = %+v, want only z_check's int/string mismatch", warnings)
		}
	}
}

func TestCheckDeclaredFunctionSurvivesBranchSnapshot(t *testing.T) {
	t.Parallel()
	script := compileScript(t, `
def run(flag: bool)
  if flag
    later(1)
  end
  later("wrong")
end

def later(value: int)
  value
end
`)
	warnings := script.CheckWarningsForFunction("run")
	if len(warnings) != 1 || warnings[0].Function != "run" ||
		warnings[0].Message != "call to later argument value expected int, got string" {
		t.Errorf("CheckWarningsForFunction(run) = %+v, want only the post-branch int/string mismatch", warnings)
	}
}

func TestCheckRequiredModuleUsesCallerDeclarations(t *testing.T) {
	t.Parallel()
	root := tempModuleTree(t,
		moduleFile{path: "good.vibe", content: `
enum Status
  Draft
end
def from_good
  require("consumer").make()
end
`},
		moduleFile{path: "bad.vibe", content: `
enum Status
  Done
end
def from_bad
  require("consumer").make()
end
`},
		moduleFile{path: "consumer.vibe", content: `
def make() -> Status
  :draft
end
`},
	)
	script := compileScriptWithEngine(t, mustNewEngineWithModuleRoot(t, root), `
def a_good
  require("good").from_good()
end
def z_bad
  require("bad").from_bad()
end
`)
	if warnings := script.CheckWarningsForFunction("a_good"); len(warnings) != 0 {
		t.Fatalf("CheckWarningsForFunction(a_good) = %+v, want no warnings", warnings)
	}
	warnings := script.CheckWarnings()
	consumerPath, err := filepath.EvalSymlinks(filepath.Join(root, "consumer.vibe"))
	if err != nil {
		t.Fatal(err)
	}
	want := CheckWarning{
		Function: "consumer.make",
		Pos:      Position{Line: 3, Column: 3},
		Message:  "return value expected Status, got symbol",
		Source:   consumerPath,
	}
	if len(warnings) != 1 || warnings[0] != want {
		t.Errorf("CheckWarnings() = %+v, want only %+v from the incompatible caller", warnings, want)
	}
}
