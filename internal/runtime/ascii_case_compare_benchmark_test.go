package runtime

import (
	"context"
	"fmt"
	"strings"
	"testing"
)

func BenchmarkStringCaseComparison(b *testing.B) {
	for _, size := range []int{16, 4096, 65536} {
		for _, kind := range []string{"equal", "first-mismatch", "last-mismatch", "unicode", "invalid-first", "invalid-last"} {
			a := strings.Repeat("aBcDef012_[", size/11) + strings.Repeat("a", size%11)
			other := strings.ToUpper(a)
			switch kind {
			case "first-mismatch":
				other = "!" + other[1:]
			case "last-mismatch":
				other = other[:size-1] + "!"
			case "unicode":
				a = strings.Repeat("é界aZ", size/7) + strings.Repeat("a", size%7)
				other = a
			case "invalid-first":
				a, other = "\xff"+a[1:], "\xff"+other[1:]
			case "invalid-last":
				a, other = a[:size-1]+"\xff", other[:size-1]+"\xff"
			}
			for _, method := range []string{"casecmp", "casecmp?"} {
				name := fmt.Sprintf("%s/%d/%s", strings.ReplaceAll(method, "?", "-predicate"), size, kind)
				b.Run(name, func(b *testing.B) {
					script := compileScriptWithConfig(b, Config{StepQuota: 5_000_000, MemoryQuotaBytes: 64 << 20}, "def run(a, b)\n  a."+method+"(b)\nend")
					args := []Value{NewString(a), NewString(other)}
					b.ReportAllocs()
					b.ResetTimer()
					for range b.N {
						if _, err := script.Call(context.Background(), "run", args, CallOptions{}); err != nil {
							b.Fatal(err)
						}
					}
				})
			}
		}
	}
}
