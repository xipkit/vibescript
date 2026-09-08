//go:build perfaudit

package runtime

import (
	"context"
	"fmt"
	"strings"
	"testing"
)

func BenchmarkAuditUnusedDeclarations(b *testing.B) {
	for _, kind := range []string{"functions", "methods", "classes", "enums"} {
		for _, count := range []int{0, 100, 1000} {
			b.Run(fmt.Sprintf("%s/%d", kind, count), func(b *testing.B) {
				var source strings.Builder
				if kind == "methods" {
					source.WriteString("class Unused\n")
				}
				for i := range count {
					switch kind {
					case "functions", "methods":
						fmt.Fprintf(&source, "def helper_%d\n  1\nend\n", i)
					case "classes":
						fmt.Fprintf(&source, "class Unused%d\n  def helper\n    1\n  end\nend\n", i)
					case "enums":
						fmt.Fprintf(&source, "enum Unused%d\n  Active\n  Inactive\nend\n", i)
					}
				}
				if kind == "methods" {
					source.WriteString("end\n")
				}
				source.WriteString("def run\n  1\nend\n")
				engine := MustNewEngine(Config{MemoryQuotaBytes: 64 << 20})
				script := compileScriptWithEngine(b, engine, source.String())
				b.ReportAllocs()
				b.ResetTimer()
				for range b.N {
					result, err := script.Call(context.Background(), "run", nil, CallOptions{})
					if err != nil || result.Int() != 1 {
						b.Fatalf("result=%v, error=%v", result, err)
					}
				}
			})
		}
	}
}

func BenchmarkAuditCheckerScaling(b *testing.B) {
	for _, kind := range []string{"locals", "leaf_calls", "functions", "self_calls"} {
		for _, count := range []int{100, 200, 400} {
			b.Run(fmt.Sprintf("%s/%d", kind, count), func(b *testing.B) {
				var source strings.Builder
				if kind == "functions" {
					for i := range count {
						fmt.Fprintf(&source, "def helper_%d\n  1\nend\n", i)
					}
				} else if kind == "self_calls" {
					source.WriteString(selfCallSiteSource(count))
				} else {
					source.WriteString("def leaf(x: int)\n  x\nend\ndef run(x: int)\n")
					for i := range count {
						if kind == "locals" {
							fmt.Fprintf(&source, "  value_%d = %d\n", i, i)
						} else {
							source.WriteString("  leaf(x)\n")
						}
					}
					source.WriteString("  1\nend\n")
				}
				script := compileScriptWithEngine(b, benchmarkEngine(), source.String())
				b.ReportAllocs()
				b.ResetTimer()
				for range b.N {
					if warnings := script.CheckWarnings(); len(warnings) != 0 {
						b.Fatalf("warnings=%v", warnings)
					}
				}
			})
		}
	}
}

func BenchmarkAuditCompileAliasedBody(b *testing.B) {
	for _, aliases := range []bool{false, true} {
		for _, count := range []int{100, 200, 400, 800} {
			b.Run(fmt.Sprintf("aliases_%t/%d", aliases, count), func(b *testing.B) {
				var source strings.Builder
				source.WriteString("def original\n")
				for range count {
					source.WriteString("  :ready\n")
				}
				source.WriteString("end\n")
				if aliases {
					for i := range count {
						fmt.Fprintf(&source, "alias alternate_%d original\n", i)
					}
				}
				text := source.String()
				engine := benchmarkEngine()
				b.ReportAllocs()
				b.ResetTimer()
				for range b.N {
					if _, err := engine.Compile(text); err != nil {
						b.Fatal(err)
					}
				}
			})
		}
	}
}
