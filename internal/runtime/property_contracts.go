package runtime

import "errors"

// propertyContract returns the generated accessor whose annotation declares
// the contract for the named instance variable, along with the declared
// type. A generated setter's parameter annotation wins when one exists (an
// untyped generated setter keeps the ivar dynamic even beside a typed
// getter, matching the unchecked public write path); otherwise a generated
// getter's return annotation declares the contract. Handwritten methods
// never declare an ivar contract: a handwritten setter takes over the write
// path entirely, so direct writes stay dynamic even when a generated typed
// getter remains, and a handwritten getter leaves a generated typed setter's
// contract in force.
func propertyContract(classDef *ClassDef, name string) (*ScriptFunction, *TypeExpr) {
	if classDef == nil {
		return nil, nil
	}
	if setter, ok := classDef.Methods[name+"="]; ok {
		if setter.Accessor != functionAccessorSetter || setter.AccessorName != name {
			return nil, nil
		}
		if len(setter.Params) == 1 {
			return setter, setter.Params[0].Type
		}
		return nil, nil
	}
	if getter, ok := classDef.Methods[name]; ok && getter.Accessor == functionAccessorGetter && getter.AccessorName == name {
		return getter, getter.ReturnTy
	}
	return nil, nil
}

// resolvePropertyParamContracts records the property contract backing each
// unannotated ivar parameter of the class's instance methods, once the
// class has fully compiled. The resolved contract shapes argument and default
// evaluation exactly like an annotation — a callable-typed contract keeps a
// bare zero-arity callable un-invoked — while binding validation stays with
// the ivar write.
func resolvePropertyParamContracts(classDef *ClassDef) {
	for _, fn := range classDef.Methods {
		if fn.Accessor != functionAccessorNone {
			continue
		}
		var params []Param
		for i, param := range fn.Params {
			if !param.IsIvar || param.Type != nil {
				continue
			}
			_, ty := propertyContract(classDef, param.Name)
			if param.PropertyType == ty {
				continue
			}
			if params == nil {
				params = append([]Param(nil), fn.Params...)
			}
			params[i].PropertyType = ty
		}
		if params != nil {
			fn.Params = params
		}
	}
}

// evalIvarAssignment evaluates a direct instance-variable assignment whose
// declared property contract shapes the right-hand side's evaluation,
// mirroring evalMemberAssignment through the generated setter: a bare
// zero-arity callable assigned to a function-typed property is stored
// un-invoked instead of being auto-called, and typed-container literals
// evaluate under the contract's element types. Targets without a usable
// expectation fall through to the plain assignment path.
func (exec *Execution) evalIvarAssignment(stmt *AssignStmt, env *Env) (Value, bool, error) {
	target, ok := stmt.Target.(*IvarExpr)
	if !ok {
		return NewNil(), false, nil
	}
	expectation := exec.ivarAssignmentValueExpectation(target, stmt.Value, env)
	if expectation.empty() {
		return NewNil(), false, nil
	}
	val, err := exec.evalAssignmentValueWithExpectation(stmt, env, expectation)
	if err != nil {
		return NewNil(), true, err
	}
	if err := exec.checkMemoryValue(val); err != nil {
		return NewNil(), true, err
	}
	if err := exec.assign(stmt.Target, val, env); err != nil {
		if errors.Is(err, errStepQuotaExceeded) || errors.Is(err, errMemoryQuotaExceeded) {
			return NewNil(), true, err
		}
		return NewNil(), true, exec.wrapError(err, stmt.Pos())
	}
	return val, true, nil
}

// ivarAssignmentValueExpectation derives the right-hand-side expectation for
// a direct write to the named instance variable: the declared property
// contract of self's class, for the same side-effect-free value shapes the
// member setter path accepts. Everything else evaluates normally.
func (exec *Execution) ivarAssignmentValueExpectation(target *IvarExpr, value Expression, env *Env) expressionExpectation {
	if value != nil && !memberAssignmentValueCanUseExpectation(value) {
		return expressionExpectation{}
	}
	self, ok := env.Get("self")
	if !ok || self.Kind() != KindInstance {
		return expressionExpectation{}
	}
	_, ty := propertyContract(valueInstance(self).Class, target.Name)
	return typeExpressionExpectation(ty)
}

// normalizeIvarWrite applies the declared property contract, if any, to a
// direct instance-variable write. Values normalize exactly like the
// generated setter's argument boundary (a matching symbol coerces into a
// declared enum, for example) and incompatible values raise when the write
// executes. Instance variables without a typed generated accessor pass
// through untouched.
func (exec *Execution) normalizeIvarWrite(inst *Instance, name string, value Value, pos Position) (Value, error) {
	accessor, ty := propertyContract(inst.Class, name)
	if ty == nil {
		return value, nil
	}
	owner := accessor.owner
	if owner == nil {
		owner = inst.Class.owner
	}
	normalized, err := normalizeValueForType(value, ty, typeContext{
		owner:    owner,
		env:      inst.Class.bindMethod(accessor).Env,
		fallback: exec.root,
		exec:     exec,
	})
	if err != nil {
		if isHostControlSignal(err) {
			return NewNil(), err
		}
		if isNormalizationLimitError(err) {
			return NewNil(), exec.wrapError(err, pos)
		}
		return NewNil(), exec.errorAt(pos, "%s", formatIvarTypeMismatch(name, err))
	}
	return normalized, nil
}
