package runtime

import (
	"fmt"
	"strings"
	"testing"
)

func repeatedScalarUnionAssignmentsSource(size int, annotation string) string {
	var source strings.Builder
	source.WriteString("def run(input: ")
	for i := range size {
		if i > 0 {
			source.WriteString(" | ")
		}
		source.WriteString(annotation)
	}
	source.WriteString(")\n")
	for range size {
		source.WriteString("  current = input\n")
	}
	source.WriteString("  current\nend\n")
	return source.String()
}

func TestCheckScalarUnionAssignmentAllocationsStayLinear(t *testing.T) {
	for _, annotation := range []string{"int", "string", "int | nil", "int | float", "int | string"} {
		t.Run(annotation, func(t *testing.T) {
			small := compileScriptDefault(t, repeatedScalarUnionAssignmentsSource(128, annotation))
			large := compileScriptDefault(t, repeatedScalarUnionAssignmentsSource(512, annotation))
			for _, script := range []*Script{small, large} {
				if warnings := script.CheckWarnings(); len(warnings) != 0 {
					t.Fatalf("valid repeated %s assignments produced warnings: %v", annotation, warnings)
				}
			}
			smallAllocs, smallBytes := checkWarningCost(t, small)
			largeAllocs, largeBytes := checkWarningCost(t, large)
			if largeAllocs > smallAllocs*8 || largeBytes > smallBytes*8 {
				t.Errorf("four times the union arms and assignments allocated %d times / %d bytes versus %d / %d, want at most 8x growth", largeAllocs, largeBytes, smallAllocs, smallBytes)
			}
		})
	}
}

func BenchmarkCheckScalarUnionAssignments(b *testing.B) {
	for _, annotation := range []string{"int", "string", "int | nil", "int | float", "int | string"} {
		for _, size := range []int{128, 512} {
			b.Run(fmt.Sprintf("%s/size-%d", annotation, size), func(b *testing.B) {
				script := compileScriptDefault(b, repeatedScalarUnionAssignmentsSource(size, annotation))
				b.ReportAllocs()
				b.ResetTimer()
				for range b.N {
					if warnings := script.CheckWarnings(); len(warnings) != 0 {
						b.Fatalf("valid repeated %s assignments produced warnings: %v", annotation, warnings)
					}
				}
			})
		}
	}
}
