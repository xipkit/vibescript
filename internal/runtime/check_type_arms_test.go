package runtime

import "testing"

func typeArmClassificationCases() []*TypeExpr {
	cases := []*TypeExpr{
		nil,
		{Kind: TypeUnion},
		{Kind: TypeUnion, Nullable: true},
		shapeValueType(nil),
		{Kind: TypeShape, Shape: map[string]*TypeExpr{"field": checkTypeInt}},
		{Kind: TypeShape, Shape: map[string]*TypeExpr{"field": checkTypeString}},
		{Kind: TypeArray, Name: literalElementsMarker, TypeArgs: []*TypeExpr{checkTypeInt}},
		{Kind: TypeHash, TypeArgs: []*TypeExpr{checkTypeString, checkTypeInt}},
		{Kind: TypeKind(-1)},
		{Kind: TypeKind(40)},
	}
	for kind := range TypeUnknown + 1 {
		if kind != TypeUnion {
			cases = append(cases, &TypeExpr{Kind: kind})
		}
	}
	initial := len(cases)
	for _, ty := range cases[:initial] {
		cases = append(cases,
			&TypeExpr{Kind: TypeUnion, Union: []*TypeExpr{ty, checkTypeNil}},
			&TypeExpr{Kind: TypeUnion, Union: []*TypeExpr{checkTypeArray, ty}},
		)
		if ty != nil {
			nullable := *ty
			nullable.Nullable = true
			cases = append(cases, &nullable)
		}
	}
	cyclic := &TypeExpr{Kind: TypeUnion}
	cyclic.Union = []*TypeExpr{checkTypeArray, cyclic}
	return append(cases, cyclic)
}

func TestTypeArmSummaryPreservesClassification(t *testing.T) {
	var checker scriptChecker
	for _, ty := range typeArmClassificationCases() {
		for depth := range maxTypeArmDepth + 2 {
			summary := checker.typeArmSummary(ty, 0)
			for _, query := range []struct {
				name  string
				kinds typeArmKinds
				want  bool
			}{
				{name: "nil only", kinds: nilArmKind, want: typeExprIsNilOnly(ty)},
				{name: "numeric only", kinds: numericArmKinds, want: typeExprNumericOnly(ty)},
				{name: "array only", kinds: 1 << TypeArray, want: typeExprArrayOnly(ty)},
				{name: "hash-like only", kinds: hashArmKinds, want: typeExprHashLikeOnly(ty)},
			} {
				if got := summary.only(query.kinds); got != query.want {
					t.Fatalf("%s for %+v at depth %d = %t, want %t", query.name, ty, depth, got, query.want)
				}
			}
			ty = &TypeExpr{Kind: TypeUnion, Union: []*TypeExpr{ty}}
		}
	}
}

func TestTypeArmSummaryPreservesReassignmentAndExpansion(t *testing.T) {
	var checker scriptChecker
	cases := typeArmClassificationCases()
	for _, current := range cases {
		for _, next := range cases {
			want := reassignmentConflicts(current, next, checker.checkNamedTypeResolver())
			if got := checker.reassignmentConflicts(current, next); got != want {
				t.Fatalf("reassignment from %+v to %+v conflicts = %t, want %t", current, next, got, want)
			}
		}
		for _, expected := range []*TypeExpr{checkTypeArray, checkTypeHash, checkTypeInt} {
			want := !typeExprsDisjoint(current, expected, checker.checkNamedTypeResolver())
			if got := checker.typeMayExpandAs(current, expected); got != want {
				t.Fatalf("expansion of %+v as %+v = %t, want %t", current, expected, got, want)
			}
		}
	}
}

func TestTypeArmSummaryKeepsNamedResolutionCurrent(t *testing.T) {
	script := compileScript(t, "enum Choice\n  One\nend\ndef run\n  nil\nend")
	checker := scriptChecker{script: script}
	named := &TypeExpr{Kind: TypeUnion, Union: []*TypeExpr{{Kind: TypeEnum, Name: "Alias"}}}
	for _, resolved := range []bool{false, true, false, true} {
		checker.runtimeTypeRoot = checkTypeRoot(script, nil)
		if resolved {
			choice, ok := checker.runtimeTypeRoot.Get("Choice")
			if !ok {
				t.Fatal("compiled enum Choice is missing from the type root")
			}
			checker.runtimeTypeRoot.Define("Alias", choice)
		}
		if got := checker.typeMayExpandAs(named, checkTypeHash); got != !resolved {
			t.Errorf("named hash expansion with resolved=%t = %t, want %t", resolved, got, !resolved)
		}
		if got := checker.reassignmentConflicts(named, checkTypeInt); got != resolved {
			t.Errorf("named-to-int reassignment with resolved=%t conflicts = %t, want %t", resolved, got, resolved)
		}
	}
}
