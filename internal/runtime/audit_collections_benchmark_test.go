//go:build perfaudit

package runtime

import (
	"context"
	"fmt"
	"strconv"
	"strings"
	"testing"
)

func BenchmarkAuditCompositeUniq(b *testing.B) {
	for _, quota := range []int{Unlimited, 64 << 20} {
		for _, n := range []int{100, 200, 400, 800} {
			for _, expression := range []string{"values.uniq", "values.uniq do |row| row end", "values.uniq do |row| row[:id] end"} {
				b.Run(fmt.Sprintf("quota=%d/n=%d/%s", quota, n, expression), func(b *testing.B) {
					script := compileScriptWithConfig(b, Config{StepQuota: Unlimited, MemoryQuotaBytes: quota}, "def run(values)\n"+expression+"\nend")
					args := []Value{NewArray(benchmarkCompositeRows(n))}
					b.ReportAllocs()
					b.ResetTimer()
					for range b.N {
						result, err := script.Call(context.Background(), "run", args, CallOptions{})
						if err != nil {
							b.Fatalf("run(%d rows): %v", n, err)
						}
						if len(result.Array()) != n {
							b.Fatalf("run(%d rows) returned %d rows", n, len(result.Array()))
						}
					}
				})
			}
		}
	}
}

func BenchmarkAuditHashValueHit(b *testing.B) {
	for _, quota := range []int{Unlimited, 64 << 20} {
		for _, n := range []int{1000, 4000, 16000} {
			for _, ordered := range []bool{false, true} {
				b.Run(fmt.Sprintf("quota=%d/n=%d/ordered=%t", quota, n, ordered), func(b *testing.B) {
					entries := make(map[string]Value, n)
					keys := make([]Value, 0, n)
					for i := range n {
						key := strconv.Itoa(i)
						entries[key] = NewInt(7)
						keys = append(keys, NewString(key))
					}
					receiver := NewHash(entries)
					if ordered {
						receiver = NewHashWithCapacity(n)
						for _, key := range keys {
							if err := receiver.HashSet(key, NewInt(7)); err != nil {
								b.Fatalf("HashSet(%v): %v", key, err)
							}
						}
					}
					script := compileScriptWithConfig(b, Config{StepQuota: Unlimited, MemoryQuotaBytes: quota}, "def run(values)\nvalues.value?(7)\nend")
					args := []Value{receiver}
					b.ReportAllocs()
					b.ResetTimer()
					for range b.N {
						result, err := script.Call(context.Background(), "run", args, CallOptions{})
						if err != nil {
							b.Fatalf("run(%d entries): %v", n, err)
						}
						if !result.Bool() {
							b.Fatalf("run(%d entries) = false, want true", n)
						}
					}
				})
			}
		}
	}
}

func BenchmarkAuditStringScanEarlyReturn(b *testing.B) {
	for _, n := range []int{1024, 16384, 262144} {
		for _, method := range []string{"scan", "match"} {
			b.Run(fmt.Sprintf("n=%d/%s", n, method), func(b *testing.B) {
				script := compileScriptWithConfig(b, Config{StepQuota: Unlimited, MemoryQuotaBytes: 64 << 20}, "def run(text)\ntext."+method+"(\"a\") do |part|\nreturn 7\nend\n0\nend")
				args := []Value{NewString(strings.Repeat("a", n))}
				b.ReportAllocs()
				b.ResetTimer()
				for range b.N {
					result, err := script.Call(context.Background(), "run", args, CallOptions{})
					if err != nil {
						b.Fatalf("run(%d bytes): %v", n, err)
					}
					if result.Int() != 7 {
						b.Fatalf("run(%d bytes) = %v, want 7", n, result)
					}
				}
			})
		}
	}
}

func BenchmarkAuditCompositeUniqSection(b *testing.B) {
	for _, n := range []int{100, 200, 400, 800} {
		for _, section := range []bool{false, true} {
			b.Run(fmt.Sprintf("n=%d/section=%t", n, section), func(b *testing.B) {
				engine := MustNewEngine(Config{StepQuota: Unlimited, MemoryQuotaBytes: 64 << 20})
				engine.RegisterBuiltin("audit_uniq", func(exec *Execution, _ Value, args []Value, _ map[string]Value, _ Value) (Value, error) {
					if section {
						defer exec.beginAccumulatorMeteredSection()()
					}
					return arrayUniq(exec, args[0], nil, nil, NewNil(), "array.uniq")
				})
				script := compileScriptWithEngine(b, engine, "def run(values)\naudit_uniq(values)\nend")
				args := []Value{NewArray(benchmarkCompositeRows(n))}
				b.ReportAllocs()
				b.ResetTimer()
				for range b.N {
					result, err := script.Call(context.Background(), "run", args, CallOptions{})
					if err != nil {
						b.Fatalf("run(%d rows): %v", n, err)
					}
					if len(result.Array()) != n {
						b.Fatalf("run(%d rows) returned %d rows", n, len(result.Array()))
					}
				}
			})
		}
	}
}
