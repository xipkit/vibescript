package runtime

import (
	"fmt"
	"strings"
	"testing"
)

func TestJSONEscapedBufferPreservesAllocationBoundaries(t *testing.T) {
	t.Parallel()
	for _, size := range []int{1, 7, 8, 15, 16, 31, 32, 63, 64, 127, 128, 4095, 4096, 8192, 8193, 32768, 32769} {
		for _, escapeFirst := range []bool{false, true} {
			t.Run(fmt.Sprintf("size=%d/escape-first=%t", size, escapeFirst), func(t *testing.T) {
				text := `"` + strings.Repeat("a", size-1) + `\n"`
				if escapeFirst {
					text = `"\n` + strings.Repeat("a", size-1) + `"`
				}
				budget := jsonSpanBudget{quota: 1 << 20, memory: 1 << 20}
				full := jsonSpanExecute(text, "", budget, false, true)
				if full.Error.Type != "" || len(full.Output) != size {
					t.Fatalf("reference escaped parse produced %d bytes with error %+v, want %d bytes", len(full.Output), full.Error, size)
				}
				lo, hi := 1, budget.memory
				for lo < hi {
					mid := lo + (hi-lo)/2
					budget.memory = mid
					if result := jsonSpanExecute(text, "", budget, false, true); result.Error.Type == "" {
						hi = mid
					} else {
						lo = mid + 1
					}
				}
				for _, memory := range []int{lo - 1, lo, lo + 1} {
					budget.memory = memory
					checkJSONSpans(t, text, "", budget, false)
				}
			})
		}
	}
}
