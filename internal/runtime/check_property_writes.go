package runtime

// Static checking of direct writes to typed accessor-backed instance
// variables. The runtime normalizes and validates every direct ivar write
// against the declared property contract, so the checker mirrors that
// boundary: instance-method analysis seeds a fact for each typed
// accessor-backed ivar, a write whose known value is provably incompatible
// with the contract warns, and unknown values pass to the runtime guard.

import (
	"errors"
	"maps"
)

// ivarFactKey names the local-inference fact slot for an instance variable.
// The @ prefix keeps ivar facts from colliding with same-named locals.
func ivarFactKey(name string) string { return "@" + name }

// instanceIvarContract returns the declared contract for an instance
// variable of the class under check. It is nil outside an instance-method
// scope and for instance variables without a typed generated accessor:
// undeclared and untyped ivars never gain an inferred contract.
func (c *scriptChecker) instanceIvarContract(name string) *TypeExpr {
	if c.selfClass == nil || c.selfClassContext {
		return nil
	}
	_, ty := propertyContract(c.selfClass, name)
	return ty
}

// ivarContractFact converts a declared contract into a read fact the
// analysis can hold across the whole method. Container contracts stay
// unknown: interior mutations of an ivar-rooted container are invisible to
// the local poisoning machinery, so only the write check uses them.
func (c *scriptChecker) ivarContractFact(ty *TypeExpr) *TypeExpr {
	if ty == nil || c.typeExprHasContainerArm(ty) {
		return nil
	}
	return ty
}

// seedInstanceIvarFacts binds entry facts for every typed accessor-backed
// instance variable of the class under check. An initializer starts with an
// empty ivar map, so every typed ivar reads as exactly nil before its first
// write. Other methods widen the declared contract with nil: an instance
// variable that was never written reads as nil regardless of its declared
// type, while every executed write satisfies the contract — statically or
// through the runtime guard.
func (c *scriptChecker) seedInstanceIvarFacts(fn *ScriptFunction) {
	if c.selfClass == nil || c.selfClassContext {
		return
	}
	for _, method := range c.selfClass.Methods {
		if method.Accessor == functionAccessorNone || method.AccessorName == "" {
			continue
		}
		ty := c.instanceIvarContract(method.AccessorName)
		if ty == nil {
			continue
		}
		if fn.Accessor == functionAccessorGetter && fn.AccessorName == method.AccessorName {
			fact, exact := c.reachableGetterIvarFact(method.AccessorName)
			if !exact {
				fact = c.ivarContractFact(ty)
				if fact != nil {
					fact = unionTypeExprs(fact, checkTypeNil)
				}
			} else if fact == nil {
				fact = c.ivarContractFact(ty)
			}
			if fact != nil {
				c.bindLocalTypeInCurrentFrame(ivarFactKey(method.AccessorName), fact)
			}
			continue
		}
		if fn.Name == "initialize" {
			c.bindLocalTypeInCurrentFrame(ivarFactKey(method.AccessorName), checkTypeNil)
			continue
		}
		if fact, exact := c.reachableInstanceIvarFact(method.AccessorName); exact {
			if fact == nil {
				fact = c.ivarContractFact(ty)
			}
			if fact != nil {
				c.bindLocalTypeInCurrentFrame(ivarFactKey(method.AccessorName), fact)
			}
			continue
		}
		fact := c.ivarContractFact(ty)
		if fact == nil {
			continue
		}
		c.bindLocalTypeInCurrentFrame(ivarFactKey(method.AccessorName), unionTypeExprs(fact, checkTypeNil))
	}
}

// captureReachableConstructorIvarFacts records the ivar state on every
// completing path of this exact constructor call for queued getter checks.
func (c *scriptChecker) captureReachableConstructorIvarFacts(
	fn *ScriptFunction,
	bodyFallsThrough bool,
	returnExitSites []checkStateSnapshot,
) {
	if fn == nil || fn.Name != "initialize" || c.selfClass == nil {
		return
	}
	originFact, captured := c.reachableParamFacts[reachableConstructorOriginFact]
	if !captured || len(originFact.staticVals) == 0 {
		return
	}
	typePaths := make([][]checkTypeFrame, 0, len(returnExitSites)+1)
	if bodyFallsThrough {
		typePaths = append(typePaths, c.localTypes)
	}
	for _, site := range returnExitSites {
		typePaths = append(typePaths, site.scopeState.types)
	}
	if len(typePaths) == 0 {
		return
	}
	for _, origin := range originFact.staticVals {
		for _, types := range typePaths {
			c.mergeConstructorIvarFacts(origin, c.constructorIvarFactsForTypes(types))
		}
	}
}

// captureReachableInstanceMethodIvarFacts replaces an exact receiver's
// constructor state with the ivar facts left on every normally completing
// method path. This preserves an unset arm across conditional writes while a
// definite guarded write refines the later generated getter to its contract.
func (c *scriptChecker) captureReachableInstanceMethodIvarFacts(
	fn *ScriptFunction,
	bodyFallsThrough bool,
	returnExitSites []checkStateSnapshot,
) {
	if fn == nil || fn.Name == "initialize" || c.selfClass == nil {
		return
	}
	originFact, captured := c.reachableParamFacts[reachableInstanceOriginFact]
	if !captured || len(originFact.staticVals) == 0 {
		return
	}
	typePaths := make([][]checkTypeFrame, 0, len(returnExitSites)+1)
	if bodyFallsThrough {
		typePaths = append(typePaths, c.localTypes)
	}
	for _, site := range returnExitSites {
		typePaths = append(typePaths, site.scopeState.types)
	}
	if len(typePaths) == 0 {
		return
	}
	var postFacts map[string]*TypeExpr
	for _, types := range typePaths {
		facts := c.constructorIvarFactsForTypes(types)
		if postFacts == nil {
			postFacts = cloneIvarFacts(facts)
			continue
		}
		mergeIvarFacts(postFacts, facts)
	}
	if c.constructorIvarFacts == nil {
		c.constructorIvarFacts = make(map[Expression]map[string]*TypeExpr)
	}
	if len(originFact.staticVals) == 1 {
		c.constructorIvarFacts[originFact.staticVals[0]] = postFacts
		return
	}
	for _, origin := range originFact.staticVals {
		current, exists := c.constructorIvarFacts[origin]
		if !exists {
			c.constructorIvarFacts[origin] = cloneIvarFacts(postFacts)
			continue
		}
		mergeIvarFacts(current, postFacts)
	}
}

// captureRescuedInstanceMethodIvarFacts carries ivar writes from escaping
// method failures into a caller path that can resume through rescue. A
// definitely failing call replaces the receiver state; a call with normal and
// rescued arms joins both post-states.
func (c *scriptChecker) captureRescuedInstanceMethodIvarFacts(
	fn *ScriptFunction,
	exceptionExitSites []checkStateSnapshot,
	normalCompletes bool,
) {
	if fn == nil || fn.Name == "initialize" || c.selfClass == nil ||
		len(exceptionExitSites) == 0 {
		return
	}
	originFact, captured := c.reachableParamFacts[reachableRescuedInstanceFact]
	if !captured || len(originFact.staticVals) == 0 {
		return
	}
	var failureFacts map[string]*TypeExpr
	for _, site := range exceptionExitSites {
		facts := c.constructorIvarFactsForTypes(site.scopeState.types)
		if failureFacts == nil {
			failureFacts = cloneIvarFacts(facts)
			continue
		}
		mergeIvarFacts(failureFacts, facts)
	}
	if c.constructorIvarFacts == nil {
		c.constructorIvarFacts = make(map[Expression]map[string]*TypeExpr)
	}
	if len(originFact.staticVals) == 1 && !normalCompletes {
		c.constructorIvarFacts[originFact.staticVals[0]] = failureFacts
		return
	}
	for _, origin := range originFact.staticVals {
		current, exists := c.constructorIvarFacts[origin]
		if !exists {
			c.constructorIvarFacts[origin] = cloneIvarFacts(failureFacts)
			continue
		}
		mergeIvarFacts(current, failureFacts)
	}
}

func (c *scriptChecker) constructorIvarFactsForTypes(
	types []checkTypeFrame,
) map[string]*TypeExpr {
	facts := make(map[string]*TypeExpr)
	for _, method := range c.selfClass.Methods {
		if method.Accessor == functionAccessorNone || method.AccessorName == "" {
			continue
		}
		if c.instanceIvarContract(method.AccessorName) == nil {
			continue
		}
		key := ivarFactKey(method.AccessorName)
		var fact *TypeExpr
		tracked := false
		for i := len(types) - 1; i >= 0; i-- {
			fact, tracked = types[i][key]
			if tracked {
				break
			}
		}
		if tracked {
			if _, widened := c.widenedIvarFacts[method.AccessorName]; widened {
				facts[method.AccessorName] = nil
				continue
			}
			facts[method.AccessorName] = fact
		}
	}
	return facts
}

func (c *scriptChecker) mergeConstructorIvarFacts(
	origin Expression,
	facts map[string]*TypeExpr,
) {
	if origin == nil {
		return
	}
	if c.constructorIvarFacts == nil {
		c.constructorIvarFacts = make(map[Expression]map[string]*TypeExpr)
	}
	current, exists := c.constructorIvarFacts[origin]
	if !exists {
		c.constructorIvarFacts[origin] = cloneIvarFacts(facts)
		return
	}
	mergeIvarFacts(current, facts)
}

func cloneIvarFacts(facts map[string]*TypeExpr) map[string]*TypeExpr {
	clone := make(map[string]*TypeExpr, len(facts))
	maps.Copy(clone, facts)
	return clone
}

func mergeIvarFacts(current, facts map[string]*TypeExpr) {
	for name, fact := range facts {
		previous, tracked := current[name]
		if !tracked || previous == nil || fact == nil {
			current[name] = nil
			continue
		}
		current[name] = unionTypeExprs(previous, fact)
	}
	for name := range current {
		if _, tracked := facts[name]; !tracked {
			current[name] = nil
		}
	}
}

func (c *scriptChecker) reachableInstanceIvarFact(name string) (*TypeExpr, bool) {
	return c.reachableOriginIvarFact(reachableInstanceOriginFact, name)
}

func (c *scriptChecker) reachableGetterIvarFact(name string) (*TypeExpr, bool) {
	return c.reachableOriginIvarFact(reachableGetterOriginFact, name)
}

func (c *scriptChecker) reachableOriginIvarFact(originName, name string) (*TypeExpr, bool) {
	originFact, captured := c.reachableParamFacts[originName]
	if !captured || len(originFact.staticVals) == 0 {
		return nil, false
	}
	var merged *TypeExpr
	for i, origin := range originFact.staticVals {
		facts, captured := c.constructorIvarFacts[origin]
		if !captured {
			return nil, false
		}
		fact, tracked := facts[name]
		if !tracked || fact == nil {
			return nil, true
		}
		if i == 0 {
			merged = fact
			continue
		}
		merged = unionTypeExprs(merged, fact)
	}
	return merged, true
}

// widenUnsetInstanceIvarFacts drops facts narrower than the property contract
// after code that may have written self. Scalar facts join with the contract;
// container contracts become unknown because their stable post-write shape is
// not tracked.
func (c *scriptChecker) widenUnsetInstanceIvarFacts() {
	if c.selfClass == nil || c.selfClassContext || !c.instanceIvarFactsDirty {
		return
	}
	for _, method := range c.selfClass.Methods {
		if method.Accessor == functionAccessorNone || method.AccessorName == "" {
			continue
		}
		c.widenUnsetInstanceIvarFact(method.AccessorName)
	}
	// No ivar fact can need another full-class widening until an ivar fact is
	// rebound. bindLocalType and bindLocalTypeInCurrentFrame mark that event,
	// keeping repeated calls between writes constant-time.
	c.instanceIvarFactsDirty = false
}

// widenUnsetInstanceIvarFact drops narrow certainty for one ivar a repeated
// region may write. Unknown calls use widenUnsetInstanceIvarFacts; direct ivar
// assignments can preserve unrelated facts through this narrower path.
func (c *scriptChecker) widenUnsetInstanceIvarFact(name string) {
	if c.selfClass == nil || c.selfClassContext {
		return
	}
	key := ivarFactKey(name)
	current := c.localTypeFor(key)
	if current == nil {
		return
	}
	ty := c.instanceIvarContract(name)
	if ty == nil {
		return
	}
	if c.widenedIvarFacts == nil {
		c.widenedIvarFacts = make(map[string]struct{})
	}
	c.widenedIvarFacts[name] = struct{}{}
	fact := c.ivarContractFact(ty)
	if fact == nil {
		c.bindLocalType(key, nil)
		return
	}
	c.bindLocalType(key, unionTypeExprs(current, fact))
}

// checkIvarParamBinding checks one ivar parameter as the direct write it
// performs when it binds, at its own position in the parameter walk so
// earlier defaults still read later ivars as unset. An annotation whose
// normalized output cannot satisfy the property contract warns at the
// definition (every call would fail during binding), the default value checks
// after the same ordered normalization the runtime performs (while preserving
// callable expectations), and the ivar's fact refines to the bare contract
// once the parameter has bound.
func (c *scriptChecker) checkIvarParamBinding(function string, fn *ScriptFunction, param Param) {
	ty := c.ivarParamContract(fn, param)
	if ty == nil || validateTypeExprResolved(ty, c.runtimeTypeContext()) != nil {
		return
	}
	if param.Type != nil &&
		validateTypeExprResolved(param.Type, c.runtimeTypeContext()) == nil &&
		!c.blockLiteralTypeMayNormalize(param.Type, ty) {
		c.add(function, param.Type.Position, "write to @%s expected %s, got %s",
			param.Name, formatTypeExpr(ty), formatTypeExpr(param.Type))
	}
	if param.DefaultVal != nil {
		subject := "default value for @" + param.Name
		fact := c.defaultExpressionBindingFact(param)
		if err := c.ivarParamBindingFactMismatch(fact, param, ty); err != nil {
			c.addValueTypeWarning(function, param.DefaultVal.Pos(), subject, err)
		}
	}
	c.bindLocalTypeInCurrentFrame(ivarFactKey(param.Name), c.ivarContractFact(ty))
}

// ivarParamContract returns the property contract backing an ivar parameter
// of fn, resolved through the class that owns the method. Parameters of
// plain functions, class methods (whose ivar parameters only bind a local —
// the runtime writes the ivar only when self is an instance), and ivars
// without a typed generated accessor have none.
func (c *scriptChecker) ivarParamContract(fn *ScriptFunction, param Param) *TypeExpr {
	if !param.IsIvar {
		return nil
	}
	c.prepareSelfScopeFunctions()
	if _, classMethod := c.selfScopeClassFns[fn]; classMethod {
		return nil
	}
	classDef := c.selfScopeFnClasses[fn]
	if classDef == nil {
		return nil
	}
	_, ty := propertyContract(classDef, param.Name)
	return ty
}

// checkIvarParamArgument checks a known call argument against both ordered
// boundary contracts a parameter carries: its own annotation first, then the
// property contract against the normalized parameter value. The store check
// is skipped when the annotation already rejected the argument.
func (c *scriptChecker) checkIvarParamArgument(function string, arg Expression, fn *ScriptFunction, param Param, callName string) {
	before := len(c.warnings)
	c.checkArgumentExpression(function, arg, param.Type, callName, param.Name)
	if len(c.warnings) > before {
		return
	}
	ty := c.ivarParamContract(fn, param)
	if ty == nil {
		return
	}
	fact := c.ivarParamArgumentBindingFact(arg, param)
	if err := c.ivarParamBindingFactMismatch(fact, param, ty); err != nil {
		c.addArgumentValueWarning(function, arg.Pos(), callName, param.Name, err)
	}
}

// ivarParamArgumentBindingFact captures the argument at its evaluation
// point. The parameter expectation may keep callable values un-invoked, so
// inference must use the same expectation when no captured fact is present.
func (c *scriptChecker) ivarParamArgumentBindingFact(arg Expression, param Param) defaultBindingFact {
	inferred, captured := c.callArgumentFacts[arg]
	if !captured {
		inferred = c.inferExpressionTypeWithExpectation(
			arg,
			positionalArgumentExpectation(param),
		)
	}
	fact := defaultBindingFact{inferred: inferred}
	if values, exact := c.callStaticLiteralValueAlternatives(arg); exact {
		fact.values = values
	}
	return fact
}

// normalizeIvarParamBindingFact applies the parameter annotation to a binding
// fact exactly once. Static values use the runtime normalizer so coercions and
// union ordering stay exact; inferred facts retain every plausible normalized
// output and remain unknown when the checker cannot prove one.
func (c *scriptChecker) normalizeIvarParamBindingFact(
	fact defaultBindingFact,
	param Param,
) (defaultBindingFact, bool) {
	if param.Type == nil {
		return fact, true
	}
	if validateTypeExprResolved(param.Type, c.runtimeTypeContext()) != nil {
		return defaultBindingFact{}, false
	}
	if fact.inferred != nil {
		if !c.blockLiteralTypeMayNormalize(fact.inferred, param.Type) {
			return defaultBindingFact{}, false
		}
		fact.inferred = c.blockLiteralNormalizedType(fact.inferred, param.Type)
	}
	if len(fact.values) > 0 {
		normalizedValues := make([]Value, 0, len(fact.values))
		for _, value := range fact.values {
			normalized, err := normalizeValueForType(
				value,
				param.Type,
				c.runtimeTypeContext(),
			)
			if err == nil {
				normalizedValues = append(normalizedValues, normalized)
			}
		}
		if len(normalizedValues) == 0 {
			return defaultBindingFact{}, false
		}
		fact.values = normalizedValues
		return fact, true
	}
	return fact, true
}

// ivarParamBindingFactMismatch checks the property guard against the value
// left by parameter normalization. A parameter mismatch is reported by the
// parameter boundary itself, so it does not also become a property warning.
func (c *scriptChecker) ivarParamBindingFactMismatch(
	fact defaultBindingFact,
	param Param,
	ty *TypeExpr,
) error {
	if ty == nil || validateTypeExprResolved(ty, c.runtimeTypeContext()) != nil {
		return nil
	}
	normalized, ok := c.normalizeIvarParamBindingFact(fact, param)
	if !ok {
		return nil
	}
	if normalized.inferred != nil &&
		!c.blockLiteralTypeMayNormalize(normalized.inferred, ty) {
		return &typeMismatchError{
			Expected: formatTypeExpr(ty),
			Actual:   formatTypeExpr(normalized.inferred),
		}
	}
	if len(normalized.values) > 0 {
		return c.staticValuesTypeMismatch(normalized.values, ty)
	}
	return nil
}

// ivarParamBindingFactMayStore reports whether some value represented by the
// raw binding fact can pass the parameter annotation and then the property
// guard. It preserves unknown facts as possible while keeping exact literals
// on the runtime normalization path.
func (c *scriptChecker) ivarParamBindingFactMayStore(
	fact defaultBindingFact,
	param Param,
	ty *TypeExpr,
) bool {
	normalized, ok := c.normalizeIvarParamBindingFact(fact, param)
	if !ok {
		return false
	}
	return c.bindingFactMayNormalizeType(normalized, ty)
}

// ivarParamBindingFactMustStore reports whether every value represented by
// the raw binding fact passes both normalization stages.
func (c *scriptChecker) ivarParamBindingFactMustStore(
	fact defaultBindingFact,
	param Param,
	ty *TypeExpr,
) bool {
	if !c.bindingFactMustNormalizeType(fact, param.Type) {
		return false
	}
	normalized, ok := c.normalizeIvarParamBindingFact(fact, param)
	if !ok {
		return false
	}
	return c.bindingFactMustNormalizeType(normalized, ty)
}

func (c *scriptChecker) bindingFactMayNormalizeType(
	fact defaultBindingFact,
	ty *TypeExpr,
) bool {
	if ty == nil {
		return true
	}
	if validateTypeExprResolved(ty, c.runtimeTypeContext()) != nil {
		return false
	}
	if fact.inferred != nil &&
		!c.blockLiteralTypeMayNormalize(fact.inferred, ty) {
		return false
	}
	if len(fact.values) > 0 {
		return c.staticValuesTypeMismatch(fact.values, ty) == nil
	}
	return fact.inferred == nil ||
		c.blockLiteralTypeMayNormalize(fact.inferred, ty)
}

func (c *scriptChecker) bindingFactMustNormalizeType(
	fact defaultBindingFact,
	ty *TypeExpr,
) bool {
	if ty == nil {
		return true
	}
	if validateTypeExprResolved(ty, c.runtimeTypeContext()) != nil {
		return false
	}
	if len(fact.values) > 0 {
		return c.staticValuesMustNormalizeType(fact.values, ty)
	}
	return fact.inferred != nil &&
		c.blockLiteralTypeMustNormalize(fact.inferred, ty)
}

// addIvarWriteWarning reports a direct-write contradiction in the standard
// write-to shape, unwrapping the normalization mismatch when present.
func (c *scriptChecker) addIvarWriteWarning(function string, pos Position, name string, err error) {
	if mismatch, ok := errors.AsType[*typeMismatchError](err); ok {
		c.add(function, pos, "write to @%s expected %s, got %s", name, mismatch.Expected, mismatch.Actual)
		return
	}
	c.add(function, pos, "write to @%s type check failed: %s", name, err)
}

// inferIvarAssignStatementTypes checks a direct write to an instance
// variable against its declared property contract and updates the ivar's
// fact. Plain writes warn when the value's known type is provably
// incompatible with the contract; compound and logical writes stay quiet
// and rely on the runtime guard.
func (c *scriptChecker) inferIvarAssignStatementTypes(
	function string,
	stmt *AssignStmt,
	target *IvarExpr,
	logicalTargetFact *logicalAssignmentTargetFact,
) {
	current := c.localTypeFor(ivarFactKey(target.Name))
	if logicalTargetFact != nil {
		current = logicalTargetFact.current
	}
	switch stmt.Operator {
	case tokenAndAssign:
		// &&= assigns only when the current value is truthy. A definitely
		// truthy fact proves the write always runs, so it checks and refines
		// like a plain write. Otherwise the write may be skipped: an unset
		// property keeps its nil arm, the fact must not refine to the bare
		// contract, and the maybe-written RHS stays unchecked — the fact
		// cannot prove a falsey current (a written nil refines to the
		// nullable contract, not to nil), so warning here would flag writes
		// that provably never run.
		if !typeExprDefinitelyTruthy(current) {
			return
		}
		c.checkIvarWrite(function, stmt.Pos(), target.Name, stmt.Value)
	case tokenOrAssign:
		// ||= short-circuits on a truthy current value: a definitely truthy
		// fact proves the RHS never evaluates or stores. Otherwise the write
		// can run — on an unset property in particular — through the same
		// runtime guard as a plain write, so a known RHS checks against the
		// contract, and the fact refines either way: a skipped write means
		// the current value was truthy and already within the contract.
		if typeExprDefinitelyTruthy(current) {
			return
		}
		c.checkIvarWrite(function, stmt.Pos(), target.Name, stmt.Value)
	case tokenNone:
		c.checkIvarWrite(function, stmt.Pos(), target.Name, stmt.Value)
	default:
		// Arithmetic compounds derive their stored value from both operands
		// and stay diagnostically quiet. Only a completing operator and
		// property guard can commit the post-write contract fact.
		value := &BinaryExpr{
			Left:     target,
			Operator: stmt.Operator,
			Right:    stmt.Value,
			Position: stmt.Pos(),
		}
		if !c.binaryExpressionMayComplete(value) ||
			!c.assignmentSetterMayComplete(target, value) {
			return
		}
		c.checkIvarWrite(function, stmt.Pos(), target.Name, nil)
	}
}

// checkIvarWrite applies the property-contract check for one direct write
// to the named instance variable and refines its fact when the write can
// complete. A nil value means the written value is not statically known, as
// with compound operators or destructured elements without a literal source;
// unknown writes stay
// quiet and rely on the runtime guard. Any executed write leaves the ivar
// satisfying its contract and a typed ivar can never return to the unset
// state, so the post-write fact is the contract itself, without the entry
// nil arm.
func (c *scriptChecker) checkIvarWrite(function string, pos Position, name string, value Expression) {
	ty := c.instanceIvarContract(name)
	if ty == nil {
		return
	}
	if value != nil && validateTypeExprResolved(ty, c.runtimeTypeContext()) == nil {
		if literal, static := staticLiteralValue(value); static {
			if err := c.checkRuntimeStaticValueType(literal, ty); err != nil {
				c.addIvarWriteWarning(function, pos, name, err)
			}
		} else {
			// The runtime evaluates the right-hand side under the property
			// expectation for the same value shapes as a member setter, so a
			// bare callable assigned to a function-typed property is stored
			// un-invoked. Destructuring replays an already evaluated leaf, so
			// prefer its captured fact instead of reinterpreting the source
			// expression under a new callable expectation.
			expectation := expressionExpectation{}
			if memberAssignmentValueCanUseExpectation(value) {
				expectation = typeExpressionExpectation(ty)
			}
			next := c.inferredAssignmentValueType(value, expectation)
			inferredMismatch := next != nil &&
				typeExprsDisjoint(next, ty, c.checkNamedTypeResolver())
			if inferredMismatch {
				c.add(function, pos, "write to @%s expected %s, got %s",
					name, formatTypeExpr(ty), formatTypeExpr(next))
			} else if values, exact := c.callStaticLiteralValueAlternatives(value); exact {
				// Retained exact values supplement kind-level inference with
				// value-sensitive normalization, such as enum member lookup.
				if err := c.staticValuesTypeMismatch(values, ty); err != nil {
					c.addIvarWriteWarning(function, pos, name, err)
				}
			}
		}
	}
	// Validation happens before the runtime mutates the ivar. Preserve the
	// incoming fact on a statically rejected write so rescue and ensure paths
	// observe the value that was actually left in the object.
	if value != nil && !c.callArgumentMayBindType(value, ty) {
		return
	}
	c.bindWrittenIvarFact(name, ty, value)
}

// bindWrittenIvarFact keeps a compatible nil or boolean literal's exact
// truthiness while retaining the declared contract for every other write.
func (c *scriptChecker) bindWrittenIvarFact(name string, ty *TypeExpr, value Expression) {
	c.bindLocalType(ivarFactKey(name), c.writtenIvarFact(ty, value))
}

func (c *scriptChecker) writtenIvarFact(ty *TypeExpr, value Expression) *TypeExpr {
	fact := c.ivarContractFact(ty)
	if value != nil {
		if values, exact := c.callStaticLiteralValueAlternatives(value); exact {
			exactFacts := make([]*TypeExpr, 0, len(values))
			for _, literal := range values {
				normalized, err := normalizeValueForType(literal, ty, c.runtimeTypeContext())
				if err != nil {
					exactFacts = nil
					break
				}
				switch normalized.Kind() {
				case KindNil:
					exactFacts = append(exactFacts, checkTypeNil)
				case KindBool:
					if normalized.Bool() {
						exactFacts = append(exactFacts, checkTypeTrue)
					} else {
						exactFacts = append(exactFacts, checkTypeFalse)
					}
				default:
					exactFacts = nil
				}
				if exactFacts == nil {
					break
				}
			}
			if len(exactFacts) > 0 {
				fact = unionTypeExprs(exactFacts...)
			}
		}
	}
	return fact
}

// narrowLogicalIvarFact selects the truthiness arm that a logical ivar
// assignment is currently walking. Unlike general local narrowing, it splits
// an unknown bool contract into its exact true or false fact so failure and
// skipped continuations cannot retain the opposite boolean value.
func (c *scriptChecker) narrowLogicalIvarFact(name string, truthy bool) bool {
	key := ivarFactKey(name)
	current := c.localTypeFor(key)
	if current == nil {
		return true
	}
	arms, ok := typeExprArms(current, 0)
	if !ok || len(arms) == 0 {
		return true
	}
	kept := make([]*TypeExpr, 0, len(arms))
	for _, arm := range arms {
		switch arm.Kind {
		case TypeNil:
			if !truthy {
				kept = append(kept, arm)
			}
		case TypeBool:
			switch arm.Name {
			case boolTrueFactMarker:
				if truthy {
					kept = append(kept, arm)
				}
			case boolFalseFactMarker:
				if !truthy {
					kept = append(kept, arm)
				}
			default:
				if truthy {
					kept = append(kept, checkTypeTrue)
				} else {
					kept = append(kept, checkTypeFalse)
				}
			}
		default:
			if truthy {
				kept = append(kept, arm)
			}
		}
	}
	if len(kept) == 0 {
		return false
	}
	c.bindLocalType(key, unionTypeExprs(kept...))
	return true
}

// commitLogicalIvarWritingArm records the successful store on the arm that
// evaluated a logical assignment's RHS. A maybe-skipped &&= keeps its legacy
// no-warning policy, but its successful arm still receives the written fact;
// the caller later merges that arm with the separately narrowed skip arm.
func (c *scriptChecker) commitLogicalIvarWritingArm(
	function string,
	stmt *AssignStmt,
	target *IvarExpr,
	current *TypeExpr,
) {
	if stmt.Operator != tokenAndAssign || typeExprDefinitelyTruthy(current) {
		c.checkIvarWrite(function, stmt.Pos(), target.Name, stmt.Value)
		return
	}
	ty := c.instanceIvarContract(target.Name)
	if ty == nil || !c.callArgumentMayBindType(stmt.Value, ty) {
		return
	}
	c.bindWrittenIvarFact(target.Name, ty, stmt.Value)
}

func (c *scriptChecker) ivarWriteProvablyCompletes(name string, value Expression) bool {
	ty := c.instanceIvarContract(name)
	if ty == nil {
		return true
	}
	if value == nil || validateTypeExprResolved(ty, c.runtimeTypeContext()) != nil {
		return false
	}
	if values, exact := c.callStaticLiteralValueAlternatives(value); exact {
		return c.staticValuesMustNormalizeType(values, ty)
	}
	expectation := expressionExpectation{}
	if memberAssignmentValueCanUseExpectation(value) {
		expectation = typeExpressionExpectation(ty)
	}
	inferred := c.inferredAssignmentValueType(value, expectation)
	return inferred != nil && typeExprSatisfies(inferred, ty, c.checkNamedTypeResolver())
}

func (c *scriptChecker) inferredAssignmentValueType(
	value Expression,
	expectation expressionExpectation,
) *TypeExpr {
	if inferred, captured := c.callArgumentFacts[value]; captured {
		return inferred
	}
	return c.inferExpressionTypeWithExpectation(value, expectation)
}

// inferDestructureIvarWrites routes the already projected leaf facts through
// the property-contract check. A checker-only expression carries a typed
// nonliteral projection; an absent fact stays gradual.
func (c *scriptChecker) inferDestructureIvarWrites(
	function string,
	facts []capturedDestructureValueFact,
) {
	for _, fact := range facts {
		ivar, ok := fact.target.(*IvarExpr)
		if !ok {
			continue
		}
		value := fact.value
		if value == nil && fact.known {
			value = &Identifier{
				Name:     "\x00destructure-value",
				Position: ivar.Pos(),
			}
		}
		if value == nil {
			c.checkIvarWrite(function, ivar.Pos(), ivar.Name, nil)
			continue
		}
		c.withCapturedDestructureArgumentFact(value, fact, func() {
			c.checkIvarWrite(function, ivar.Pos(), ivar.Name, value)
		})
	}
}

func (c *scriptChecker) exactEvaluatedDestructureValue(value Expression) (Expression, bool) {
	if value == nil {
		return nil, false
	}
	if _, literal := staticLiteralValue(value); literal {
		return value, true
	}
	fact, evaluated := c.evaluatedDestructureFacts[value]
	if !evaluated || fact.factKind != destructureStaticFact || len(fact.staticVals) != 1 {
		return nil, false
	}
	return fact.staticVals[0], true
}
