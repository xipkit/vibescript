package runtime

import (
	"fmt"
	"strings"
	"testing"
)

func TestContainerTypeArmsPreserveFlattening(t *testing.T) {
	cyclic := &TypeExpr{Kind: TypeUnion}
	cyclic.Union = []*TypeExpr{checkTypeArray, cyclic}
	for _, tc := range []struct {
		name     string
		typeExpr *TypeExpr
	}{
		{name: "absent"},
		{name: "any", typeExpr: &TypeExpr{Kind: TypeAny}},
		{name: "unknown", typeExpr: &TypeExpr{Kind: TypeUnknown}},
		{name: "scalar", typeExpr: checkTypeInt},
		{name: "named", typeExpr: &TypeExpr{Kind: TypeEnum, Name: "Item"}},
		{name: "array", typeExpr: &TypeExpr{Kind: TypeArray, TypeArgs: []*TypeExpr{nil}}},
		{name: "hash", typeExpr: checkTypeHash},
		{name: "shape", typeExpr: &TypeExpr{Kind: TypeShape, Shape: map[string]*TypeExpr{"field": nil}}},
		{name: "nullable array", typeExpr: &TypeExpr{Kind: TypeArray, Nullable: true}},
		{name: "shape value", typeExpr: shapeValueType(checkTypeArray)},
		{name: "nil shape payload", typeExpr: shapeValueType(nil)},
		{name: "empty union", typeExpr: &TypeExpr{Kind: TypeUnion}},
		{name: "cycle", typeExpr: cyclic},
	} {
		t.Run(tc.name, func(t *testing.T) {
			var checker scriptChecker
			for _, options := range [][]*TypeExpr{
				{tc.typeExpr},
				{checkTypeArray, tc.typeExpr},
				{tc.typeExpr, checkTypeArray},
			} {
				ty := &TypeExpr{Kind: TypeUnion, Union: options}
				for depth := range maxTypeArmDepth + 2 {
					arms, valid := typeExprArms(ty, 0)
					want := false
					if valid {
						for _, arm := range arms {
							if arm.Kind == TypeArray || arm.Kind == TypeHash || arm.Kind == TypeShape {
								want = true
							}
						}
					}
					for range 2 {
						if got := checker.typeExprHasContainerArm(ty); got != want {
							t.Fatalf("container query at depth %d = %t, want %t from flattened arms", depth, got, want)
						}
					}
					ty = &TypeExpr{Kind: TypeUnion, Union: []*TypeExpr{ty}}
				}
			}
		})
	}
}

func TestContainerTypeArmsPreserveSharedDepth(t *testing.T) {
	shared := &TypeExpr{Kind: TypeUnion, Union: []*TypeExpr{checkTypeArray}}
	deep := shared
	for range maxTypeArmDepth {
		deep = &TypeExpr{Kind: TypeUnion, Union: []*TypeExpr{deep}}
	}
	for _, shallowFirst := range []bool{true, false} {
		t.Run(fmt.Sprintf("shallow-first-%t", shallowFirst), func(t *testing.T) {
			var checker scriptChecker
			first, second := shared, deep
			if !shallowFirst {
				first, second = second, first
			}
			for _, ty := range []*TypeExpr{first, second, first, second} {
				want := ty == shared
				if got := checker.typeExprHasContainerArm(ty); got != want {
					t.Errorf("shared union container query = %t, want %t", got, want)
				}
			}
		})
	}
}

func TestContainerTypeArmsReuseSharedUnionWork(t *testing.T) {
	union := &TypeExpr{Kind: TypeUnion, Union: make([]*TypeExpr, 512)}
	for i := range union.Union {
		union.Union[i] = checkTypeInt
	}
	var checker scriptChecker
	checkWorkUnits.Store(0)
	checkWorkCounting.Store(true)
	t.Cleanup(func() { checkWorkCounting.Store(false) })
	for range 512 {
		if checker.typeExprHasContainerArm(union) {
			t.Fatal("scalar union unexpectedly contains a container")
		}
	}
	if work := checkWorkUnits.Load(); work != uint64(len(union.Union)) {
		t.Errorf("repeated scalar-union queries inspected %d arms, want %d", work, len(union.Union))
	}
	if allocs := testing.AllocsPerRun(100, func() { checker.typeExprHasContainerArm(union) }); allocs != 0 {
		t.Errorf("repeated container query allocated %g times, want zero", allocs)
	}
}

func TestCheckRepeatedUnionAssignmentContainerWorkStaysLinear(t *testing.T) {
	small := measureCheckWork(t, repeatedUnionAssignmentSource(128))
	large := measureCheckWork(t, repeatedUnionAssignmentSource(512))
	if small == 0 || large > small*8 {
		t.Errorf("checking four times the union arms and assignments inspected %d arms versus %d, want nonzero work with at most 8x growth", large, small)
	}
}

func repeatedUnionAssignmentSource(size int) string {
	var source strings.Builder
	source.WriteString("def run(input: ")
	appendIntUnion(&source, size)
	source.WriteString(")\n")
	for range size {
		source.WriteString("  current = input\n")
	}
	source.WriteString("  current\nend\n")
	return source.String()
}

func BenchmarkCheckRepeatedUnionAssignments(b *testing.B) {
	for _, size := range []int{128, 512} {
		b.Run(fmt.Sprintf("size-%d", size), func(b *testing.B) {
			script := compileScriptDefault(b, repeatedUnionAssignmentSource(size))
			b.ReportAllocs()
			b.ResetTimer()
			for range b.N {
				if warnings := script.CheckWarnings(); len(warnings) != 0 {
					b.Fatalf("checking repeated valid assignments produced warnings: %v", warnings)
				}
			}
		})
	}
}

func BenchmarkContainerTypeArms(b *testing.B) {
	for _, size := range []int{128, 512} {
		b.Run(fmt.Sprintf("size-%d", size), func(b *testing.B) {
			union := &TypeExpr{Kind: TypeUnion, Union: make([]*TypeExpr, size)}
			for i := range union.Union {
				union.Union[i] = checkTypeInt
			}
			var checker scriptChecker
			checker.typeExprHasContainerArm(union)
			b.ReportAllocs()
			b.ResetTimer()
			for range b.N {
				if checker.typeExprHasContainerArm(union) {
					b.Fatal("scalar union unexpectedly contains a container")
				}
			}
		})
	}
}
