package runtime

import (
	"context"
	"fmt"
	"strings"
	"sync"
	"testing"
)

func unusedDeclarationSource(kind string, count int) string {
	var source strings.Builder
	if kind == "methods" || kind == "used_methods" {
		source.WriteString("class Unused\n")
	}
	for i := range count {
		switch kind {
		case "functions", "methods", "used_methods":
			fmt.Fprintf(&source, "def helper_%d\n  1\nend\n", i)
		case "classes":
			fmt.Fprintf(&source, "class Unused%d\n  def helper\n    1\n  end\nend\n", i)
		case "enums":
			fmt.Fprintf(&source, "enum Unused%d\n  Active\n  Inactive\nend\n", i)
		}
	}
	if kind == "methods" || kind == "used_methods" {
		source.WriteString("def self.answer\n  1\nend\nend\n")
	}
	if kind == "used_methods" {
		source.WriteString("def run\n  Unused.answer\nend\n")
	} else {
		source.WriteString("def run\n  1\nend\n")
	}
	return source.String()
}

func BenchmarkCallUnusedDeclarations(b *testing.B) {
	for _, kind := range []string{"functions", "methods", "classes", "enums", "used_methods"} {
		for _, count := range []int{0, 100, 1000} {
			b.Run(fmt.Sprintf("%s/%d", kind, count), func(b *testing.B) {
				engine := MustNewEngine(Config{MemoryQuotaBytes: 64 << 20})
				script := compileScriptWithEngine(b, engine, unusedDeclarationSource(kind, count))
				b.ReportAllocs()
				b.ResetTimer()
				for range b.N {
					result, err := script.Call(context.Background(), "run", nil, CallOptions{})
					if err != nil || result.Int() != 1 {
						b.Fatalf("run = %v, %v; want 1, nil", result, err)
					}
				}
			})
		}
	}
}

func TestCallUnusedDeclarationsDoNotScaleAllocations(t *testing.T) {
	for _, kind := range []string{"functions", "methods", "classes", "enums", "used_methods"} {
		t.Run(kind, func(t *testing.T) {
			allocs := func(count int) float64 {
				script := compileScriptDefault(t, unusedDeclarationSource(kind, count))
				return testing.AllocsPerRun(10, func() {
					result, err := script.Call(context.Background(), "run", nil, CallOptions{})
					if err != nil || result.Int() != 1 {
						t.Fatalf("run = %v, %v; want 1, nil", result, err)
					}
				})
			}
			baseline := allocs(0)
			large := allocs(1000)
			if large > baseline+2 {
				t.Fatalf("1000 unused %s increased allocations from %.0f to %.0f", kind, baseline, large)
			}
		})
	}
}

func TestCallLazyDeclarationsPreserveClassStateAndNamespaces(t *testing.T) {
	script := compileScriptDefault(t, `module Outer
  module Inner
    def self.offset
      1
    end
  end
end
class Counter
  def self.set(n)
    @@value = n
  end
  def self.bump
    @@value += Outer::Inner.offset
  end
  def self.value
    @@value
  end
end
enum Status
  Ready
end
def helper
  Counter.bump
end
def run(n)
  Counter.set(n)
  helper
  if Status::Ready == Status::Ready
    Counter.value
  else
    0
  end
end`)
	var wg sync.WaitGroup
	for n := range 32 {
		wg.Go(func() {
			got, err := script.Call(context.Background(), "run", []Value{NewInt(int64(n))}, CallOptions{})
			if err != nil || got.Int() != int64(n+1) {
				t.Errorf("run(%d) = %v, %v; want %d, nil", n, got, err, n+1)
			}
		})
	}
	wg.Wait()
}

func TestCallLazyDeclarationsRespectGlobalOverrides(t *testing.T) {
	for _, composite := range []bool{false, true} {
		t.Run(fmt.Sprintf("composite_%t", composite), func(t *testing.T) {
			body := "helper + Box + Status"
			globals := map[string]Value{"helper": NewInt(1), "Box": NewInt(2), "Status": NewInt(3)}
			if composite {
				body = "helper[0] + Box[0] + Status[0]"
				for name, val := range globals {
					globals[name] = NewArray([]Value{val})
				}
			}
			script := compileScriptDefault(t, "def helper\n  99\nend\nclass Box\nend\nenum Status\n  Ready\nend\ndef run\n  "+body+"\nend\n")
			got := callScript(t, context.Background(), script, "run", nil, CallOptions{Globals: globals})
			if got.Int() != 6 {
				t.Fatalf("run with overrides = %v, want 6", got)
			}
		})
	}
}

func TestReturnedEnvironmentDetachesUnreadDeclarations(t *testing.T) {
	script := compileScriptDefault(t, `class Exported
  def marker
    1
  end
end
class Unread
  def answer
    return 7
  end
end
enum Status
  Ready
end
def unread
  return 11
end
def leak
  Exported
end
def run
  [unread, Unread.new.answer, Status::Ready.name]
end`)
	exported := valueClass(callScript(t, context.Background(), script, "leak", nil, CallOptions{}))
	env := exported.Methods["marker"].Env
	fnVal, ok := env.Get("unread")
	if !ok {
		t.Fatal("returned environment lost unread function")
	}
	valueFunction(fnVal).Body[0].(*ReturnStmt).Value.(*IntegerLiteral).Value = 99
	classVal, ok := env.Get("Unread")
	if !ok {
		t.Fatal("returned environment lost unread class")
	}
	valueClass(classVal).Methods["answer"].Body[0].(*ReturnStmt).Value.(*IntegerLiteral).Value = 99
	enumVal, ok := env.Get("Status")
	if !ok {
		t.Fatal("returned environment lost unread enum")
	}
	valueEnum(enumVal).Members["Ready"].Name = "Changed"
	got := callScript(t, context.Background(), script, "run", nil, CallOptions{})
	want := NewArray([]Value{NewInt(11), NewInt(7), NewString("Ready")})
	if !got.Equal(want) {
		t.Fatalf("run after host mutations = %v, want %v", got, want)
	}
}
