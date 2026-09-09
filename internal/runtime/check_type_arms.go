package runtime

type typeArmKinds uint32

const (
	nilArmKind        typeArmKinds = 1 << TypeNil
	numericArmKinds   typeArmKinds = 1<<TypeInt | 1<<TypeFloat | 1<<TypeNumber
	hashArmKinds      typeArmKinds = 1<<TypeHash | 1<<TypeShape
	containerArmKinds              = 1<<TypeArray | hashArmKinds
	primitiveArmKinds typeArmKinds = (1<<(TypeUnknown+1) - 1) &^
		(1<<TypeAny | 1<<TypeArray | 1<<TypeHash | 1<<TypeShape | 1<<TypeUnion | 1<<TypeEnum)
	opaqueArmKind typeArmKinds = 1 << 31
)

type typeArmSummaryKey struct {
	typeExpr *TypeExpr
	depth    int
}

type typeArmSummary struct {
	kinds typeArmKinds
	valid bool
}

func (s typeArmSummary) only(kinds typeArmKinds) bool {
	return s.valid && s.kinds != 0 && s.kinds&^kinds == 0
}

// typeArmSummary preserves typeExprArms validity and outer kinds without
// materializing arms. Facts are immutable once built; depth remains part of the
// key because a shared union may exceed the depth limit through only one parent.
func (c *scriptChecker) typeArmSummary(ty *TypeExpr, depth int) typeArmSummary {
	if ty == nil || depth > maxTypeArmDepth {
		return typeArmSummary{}
	}
	key := typeArmSummaryKey{typeExpr: ty, depth: depth}
	result := typeArmSummary{valid: true}
	switch ty.Kind {
	case TypeUnion:
		if summary, ok := c.typeArmSummaries[key]; ok {
			return summary
		}
		noteCheckWork(len(ty.Union))
		for _, option := range ty.Union {
			arm := c.typeArmSummary(option, depth+1)
			if !arm.valid {
				result = typeArmSummary{}
				break
			}
			result.kinds |= arm.kinds
		}
	case TypeAny, TypeUnknown:
		if _, valid := shapeValuePayload(ty); !valid {
			return typeArmSummary{}
		}
		result.kinds = 1 << TypeUnknown
	default:
		if ty.Kind >= TypeInt && ty.Kind <= TypeUnknown {
			result.kinds = 1 << ty.Kind
		} else {
			result.kinds = opaqueArmKind
		}
	}
	if result.valid && ty.Nullable {
		result.kinds |= nilArmKind
	}
	if ty.Kind == TypeUnion {
		if c.typeArmSummaries == nil {
			c.typeArmSummaries = make(map[typeArmSummaryKey]typeArmSummary)
		}
		c.typeArmSummaries[key] = result
	}
	return result
}

func (c *scriptChecker) reassignmentConflicts(current, next *TypeExpr) bool {
	if current == nil || next == nil {
		return false
	}
	x := c.typeArmSummary(current, 0)
	if !x.valid || x.kinds == 0 || x.only(nilArmKind) {
		return false
	}
	y := c.typeArmSummary(next, 0)
	if !y.valid || y.kinds == 0 || y.only(nilArmKind) {
		return false
	}
	if x.only(numericArmKinds) && y.only(numericArmKinds) ||
		x.only(hashArmKinds) && y.only(hashArmKinds) ||
		x.only(1<<TypeArray) && y.only(1<<TypeArray) {
		return false
	}
	if x.only(primitiveArmKinds) && y.only(primitiveArmKinds) {
		if x.kinds&y.kinds != 0 ||
			x.kinds&(1<<TypeNumber) != 0 && y.kinds&numericArmKinds != 0 ||
			y.kinds&(1<<TypeNumber) != 0 && x.kinds&numericArmKinds != 0 {
			return false
		}
		return true
	}
	return reassignmentConflicts(current, next, c.checkNamedTypeResolver())
}

// typeMayExpandAs specializes only the unparameterized expansion contracts.
// Named types still resolve against the current environment on every query.
func (c *scriptChecker) typeMayExpandAs(inferred, expected *TypeExpr) bool {
	if expected != checkTypeArray && expected != checkTypeHash {
		return !typeExprsDisjoint(inferred, expected, c.checkNamedTypeResolver())
	}
	summary := c.typeArmSummary(inferred, 0)
	if !summary.valid || summary.kinds == 0 {
		return true
	}
	if summary.kinds&(1<<TypeEnum|opaqueArmKind) != 0 {
		return !typeExprsDisjoint(inferred, expected, c.checkNamedTypeResolver())
	}
	if expected == checkTypeArray {
		return summary.kinds&(1<<TypeArray) != 0
	}
	return summary.kinds&hashArmKinds != 0
}
