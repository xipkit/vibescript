package runtime

import (
	"cmp"
	"maps"
	"math"
	"reflect"
	"slices"
	"strconv"
	"strings"
)

// Implicit local type inference for the check path (ADR-004).
//
// The checker binds locals to the inferred *TypeExpr of the expressions
// assigned to them and reports an error wherever known types contradict a
// typed boundary. A nil *TypeExpr means "unknown": the checker proves nothing
// about the value and stays silent, deferring to the runtime contract. The
// governing rule is: error on known contradictions, permit unknowns.
//
// Two structures carry the facts:
//
//   - localTypes is a stack of frames parallel to scriptChecker.scopes. It is
//     snapshotted, restored, and merged together with the definedness scopes
//     at every branch point, so sibling branches join into unions and loop
//     bodies degrade to unknown.
//   - typePoison is a function-scoped, monotone set of locals whose values
//     escaped precise tracking (an in-place mutation through an index or
//     member write, a member call that may mutate a container, a container
//     passed by reference into a call). Poisoning survives branch and loop
//     state restores by design, so a fact killed inside a block body cannot
//     resurface after the walk backtracks.

const (
	boolTrueFactMarker  = "\x00bool-true"
	boolFalseFactMarker = "\x00bool-false"
)

var (
	checkTypeInt      = &TypeExpr{Kind: TypeInt}
	checkTypeFloat    = &TypeExpr{Kind: TypeFloat}
	checkTypeNumber   = &TypeExpr{Kind: TypeNumber}
	checkTypeString   = &TypeExpr{Kind: TypeString}
	checkTypeBool     = &TypeExpr{Kind: TypeBool}
	checkTypeNil      = &TypeExpr{Kind: TypeNil}
	checkTypeSymbol   = &TypeExpr{Kind: TypeSymbol}
	checkTypeArray    = &TypeExpr{Kind: TypeArray}
	checkTypeAnyArray = &TypeExpr{Kind: TypeArray, TypeArgs: []*TypeExpr{{Kind: TypeAny}}}
	checkTypeIntArray = &TypeExpr{Kind: TypeArray, TypeArgs: []*TypeExpr{checkTypeInt}}
	checkTypeHash     = &TypeExpr{Kind: TypeHash}
	checkTypeRange    = &TypeExpr{Kind: TypeRange}
	checkTypeDuration = &TypeExpr{Kind: TypeDuration}
	checkTypeTime     = &TypeExpr{Kind: TypeTime}
	checkTypeMoney    = &TypeExpr{Kind: TypeMoney}
	checkTypeFunction = &TypeExpr{Kind: TypeFunction}
	checkTypeTrue     = &TypeExpr{Kind: TypeBool, Name: boolTrueFactMarker}
	checkTypeFalse    = &TypeExpr{Kind: TypeBool, Name: boolFalseFactMarker}

	// checkTypeMethodName matches respond_to?'s method-name argument, which
	// the runtime accepts as a symbol or string.
	checkTypeMethodName = &TypeExpr{Kind: TypeUnion, Union: []*TypeExpr{checkTypeSymbol, checkTypeString}}
)

type checkTypeFrame map[string]*TypeExpr

type checkLocalValueFact struct {
	classNames              []string
	instanceOrigins         []Expression
	callables               []*ScriptFunction
	staticVals              []Expression
	staticChoice            checkStaticChoiceFact
	keywordSplatFails       bool
	invalidKeywordSplatKeys map[string]struct{}
}

type checkClassValueFrame map[string]checkLocalValueFact

// checkContainerAliasTransfer snapshots the mutable-object relationships a
// direct assignment must retain after either binding is rebound.
type checkContainerAliasTransfer struct {
	identities       map[string]struct{}
	aliases          map[string]struct{}
	staticSources    map[string]struct{}
	staticDependents map[string]struct{}
}

// beginIfClassBranchCapture arms condition-time selection when the main
// condition is a decidable direct local and clears any fact from a prior walk.
func (c *scriptChecker) beginIfClassBranchCapture(expr *IfExpr) bool {
	delete(c.evaluatedIfClassFacts, expr)
	if expr == nil {
		return false
	}
	_, decided := c.directLocalConditionTruthiness(expr.Condition)
	return decided
}

func (c *scriptChecker) finishIfClassBranchCapture(
	expr *IfExpr,
	branch Expression,
	selected bool,
	completed bool,
) {
	delete(c.evaluatedIfClassFacts, expr)
	if !selected || !completed {
		return
	}
	classNames, exact := c.stableEvaluatedClassNames(branch, false)
	if !exact {
		return
	}
	if c.evaluatedIfClassFacts == nil {
		c.evaluatedIfClassFacts = make(map[*IfExpr][]string)
	}
	c.evaluatedIfClassFacts[expr] = append([]string(nil), classNames...)
}

func (c *scriptChecker) directLocalConditionTruthiness(condition Expression) (bool, bool) {
	ident, ok := condition.(*Identifier)
	if !ok {
		return false, false
	}
	if !c.localTypeTracked(ident.Name) {
		return false, false
	}
	if fact, exact := c.localValueFactFor(ident.Name); exact {
		if len(fact.callables) > 0 {
			return false, false
		}
		if truthy, known := localValueFactTruthiness(fact, true); known {
			return truthy, true
		}
	}
	ty := c.localTypeFor(ident.Name)
	if typeExprMayIncludeCallable(ty) {
		return false, false
	}
	if typeExprDefinitelyTruthy(ty) {
		return true, true
	}
	if typeExprIsNilOnly(ty) {
		return false, true
	}
	return false, false
}

func (c *scriptChecker) stableIfClassConditionTruthiness(condition Expression) (bool, bool) {
	if truthy, known := staticExpressionTruthiness(condition); known {
		return truthy, true
	}
	return c.directLocalConditionTruthiness(condition)
}

// stableEvaluatedClassNames resolves identities that remain valid after an
// expression has completed. stateIndependent is set when later evaluation
// may have changed local facts; tracked reads then stay unsupported instead
// of being replayed from post-evaluation state.
func (c *scriptChecker) stableEvaluatedClassNames(
	expr Expression,
	stateIndependent bool,
) ([]string, bool) {
	switch typed := expr.(type) {
	case *Identifier:
		if stateIndependent {
			if c.localTypeTracked(typed.Name) {
				return nil, false
			}
			classDef, ok := c.staticClassArgument(typed)
			if !ok {
				return nil, false
			}
			return []string{classDef.Name}, true
		}
		return c.classValueExpressionNames(typed)
	case *ConditionalExpr:
		branch, ok := staticConditionalExpressionBranch(typed)
		if !ok {
			return nil, false
		}
		return c.stableEvaluatedClassNames(branch, stateIndependent)
	case *IfExpr:
		if classNames, evaluated := c.evaluatedIfClassFacts[typed]; evaluated {
			return append([]string(nil), classNames...), true
		}
		branch, ok := staticIfExpressionBranch(typed)
		if !ok {
			return nil, false
		}
		return c.stableEvaluatedClassNames(branch, stateIndependent)
	case *BinaryExpr:
		if typed.Operator != tokenAnd && typed.Operator != tokenOr {
			return nil, false
		}
		truthy, known := staticExpressionTruthiness(typed.Left)
		if !known {
			return nil, false
		}
		if truthy == (typed.Operator == tokenAnd) {
			return c.stableEvaluatedClassNames(typed.Right, stateIndependent)
		}
		return c.stableEvaluatedClassNames(typed.Left, stateIndependent)
	case *IndexExpr:
		return c.stableLiteralProjectionClassNames(typed, stateIndependent)
	case *CallExpr:
		member, ok := typed.Callee.(*MemberExpr)
		if !ok || member.Property != "itself" || len(typed.Args) != 0 ||
			len(typed.KwArgs) != 0 || typed.Block != nil {
			return nil, false
		}
		classNames, exact := c.stableEvaluatedClassNames(member.Object, stateIndependent)
		if !exact {
			return nil, false
		}
		for _, className := range classNames {
			classDef := c.script.classes[className]
			if classDef == nil || classDef.ClassMethods["itself"] != nil {
				return nil, false
			}
		}
		return classNames, true
	default:
		return nil, false
	}
}

func (c *scriptChecker) localTypeTracked(name string) bool {
	for i := len(c.localTypes) - 1; i >= 0; i-- {
		if _, tracked := c.localTypes[i][name]; tracked {
			return true
		}
	}
	return false
}

func (c *scriptChecker) stableLiteralProjectionClassNames(
	expr *IndexExpr,
	stateIndependent bool,
) ([]string, bool) {
	if expr == nil || len(expr.Indices) != 1 {
		return nil, false
	}
	switch object := expr.Object.(type) {
	case *ArrayLiteral:
		for _, element := range object.Elements {
			if _, splat := element.(*SplatArg); splat {
				return nil, false
			}
		}
		value, ok := staticLiteralValue(expr.Indices[0])
		if !ok {
			return nil, false
		}
		index, ok := staticArrayFetchIndex(value)
		if !ok {
			return nil, false
		}
		if index < 0 {
			index += int64(len(object.Elements))
		}
		if index < 0 || index >= int64(len(object.Elements)) {
			return nil, false
		}
		for _, later := range object.Elements[index+1:] {
			if _, static := staticLiteralValue(later); !static {
				stateIndependent = true
				break
			}
		}
		return c.stableEvaluatedClassNames(object.Elements[index], stateIndependent)
	case *HashLiteral:
		if object.ShapeType != nil && !c.hashShapeStaticallyShadowed(object) {
			return nil, false
		}
		want, ok := staticLiteralValue(expr.Indices[0])
		if !ok {
			return nil, false
		}
		selected := -1
		for i, pair := range object.Pairs {
			key, static := staticLiteralValue(pair.Key)
			if !static {
				return nil, false
			}
			if key.Equal(want) {
				selected = i
			}
		}
		if selected < 0 {
			return nil, false
		}
		for _, later := range object.Pairs[selected+1:] {
			if _, static := staticLiteralValue(later.Value); !static {
				stateIndependent = true
				break
			}
		}
		return c.stableEvaluatedClassNames(object.Pairs[selected].Value, stateIndependent)
	default:
		return nil, false
	}
}

func cloneCheckTypeFrame(frame checkTypeFrame) checkTypeFrame {
	if len(frame) == 0 {
		return nil
	}
	clone := make(checkTypeFrame, len(frame))
	maps.Copy(clone, frame)
	return clone
}

func cloneCheckClassValueFrame(frame checkClassValueFrame) checkClassValueFrame {
	if len(frame) == 0 {
		return nil
	}
	clone := make(checkClassValueFrame, len(frame))
	for name, fact := range frame {
		clone[name] = checkLocalValueFact{
			classNames:              append([]string(nil), fact.classNames...),
			instanceOrigins:         append([]Expression(nil), fact.instanceOrigins...),
			callables:               append([]*ScriptFunction(nil), fact.callables...),
			staticVals:              append([]Expression(nil), fact.staticVals...),
			staticChoice:            cloneCheckStaticChoiceFact(fact.staticChoice),
			keywordSplatFails:       fact.keywordSplatFails,
			invalidKeywordSplatKeys: cloneCheckStringSet(fact.invalidKeywordSplatKeys),
		}
	}
	return clone
}

func (c *scriptChecker) localClassValueFor(name string) (string, bool) {
	fact, ok := c.localValueFactFor(name)
	if !ok || len(fact.classNames) != 1 || len(fact.callables) > 0 || len(fact.staticVals) > 0 ||
		len(fact.instanceOrigins) > 0 ||
		fact.keywordSplatFails {
		return "", false
	}
	return fact.classNames[0], true
}

func (c *scriptChecker) localClassValuesFor(name string) ([]string, bool) {
	fact, ok := c.localValueFactFor(name)
	return fact.classNames, ok && len(fact.classNames) > 0 && len(fact.callables) == 0 &&
		len(fact.instanceOrigins) == 0 &&
		len(fact.staticVals) == 0 &&
		!fact.keywordSplatFails
}

func (c *scriptChecker) localValueFactFor(name string) (checkLocalValueFact, bool) {
	for i := len(c.localTypes) - 1; i >= 0; i-- {
		if _, tracked := c.localTypes[i][name]; !tracked {
			continue
		}
		fact, ok := c.localClassValues[i][name]
		return fact, ok
	}
	return checkLocalValueFact{}, false
}

func (c *scriptChecker) bindLocalClassValue(name, className string) {
	if className == "" {
		c.bindLocalClassValues(name, nil)
		return
	}
	c.bindLocalClassValues(name, []string{className})
}

func (c *scriptChecker) bindLocalClassValues(name string, classNames []string) {
	if name == "" || len(c.localTypes) == 0 {
		return
	}
	for i := len(c.localTypes) - 1; i >= 0; i-- {
		if _, tracked := c.localTypes[i][name]; !tracked {
			continue
		}
		if len(classNames) == 0 {
			delete(c.localClassValues[i], name)
			return
		}
		if c.localClassValues[i] == nil {
			c.localClassValues[i] = make(checkClassValueFrame)
		}
		c.localClassValues[i][name] = checkLocalValueFact{classNames: normalizeCheckClassNames(classNames)}
		return
	}
}

func (c *scriptChecker) localCallableValuesFor(name string) ([]*ScriptFunction, bool) {
	fact, ok := c.localValueFactFor(name)
	return fact.callables, ok && len(fact.callables) > 0 && len(fact.classNames) == 0 &&
		len(fact.instanceOrigins) == 0 &&
		len(fact.staticVals) == 0 &&
		!fact.keywordSplatFails
}

func (c *scriptChecker) bindLocalCallableValues(name string, fns []*ScriptFunction) {
	if name == "" || len(c.localTypes) == 0 {
		return
	}
	for i := len(c.localTypes) - 1; i >= 0; i-- {
		if _, tracked := c.localTypes[i][name]; !tracked {
			continue
		}
		if len(fns) == 0 {
			delete(c.localClassValues[i], name)
			return
		}
		if c.localClassValues[i] == nil {
			c.localClassValues[i] = make(checkClassValueFrame)
		}
		c.localClassValues[i][name] = checkLocalValueFact{callables: normalizeCheckCallables(fns)}
		return
	}
}

func (c *scriptChecker) localStaticValuesFor(name string) ([]Expression, bool) {
	if _, poisoned := c.typePoison[name]; poisoned {
		return nil, false
	}
	if _, poisoned := c.staticValuePoison[name]; poisoned {
		return nil, false
	}
	fact, ok := c.localValueFactFor(name)
	return append([]Expression(nil), fact.staticVals...), ok && len(fact.staticVals) > 0 &&
		len(fact.instanceOrigins) == 0 &&
		len(fact.classNames) == 0 && len(fact.callables) == 0 &&
		!fact.keywordSplatFails
}

func (c *scriptChecker) captureArrayReceiverLength(expr Expression) checkArrayReceiverCapture {
	capture := checkArrayReceiverCapture{}
	var alternative Expression
	switch typed := expr.(type) {
	case *ArrayLiteral:
		alternative = typed
		capture.literal = true
	case *Identifier:
		values, exact := c.localStaticValuesFor(typed.Name)
		if !exact || len(values) != 1 {
			return capture
		}
		alternative = values[0]
		capture.name = typed.Name
		capture.generation = c.localBindingGenerations[typed.Name]
	default:
		return capture
	}

	array, exact := alternative.(*ArrayLiteral)
	if !exact {
		return checkArrayReceiverCapture{}
	}
	for _, element := range array.Elements {
		if _, splat := element.(*SplatArg); splat {
			return checkArrayReceiverCapture{}
		}
	}
	capture.alternative = alternative
	capture.length = len(array.Elements)
	capture.exact = true
	return capture
}

func (c *scriptChecker) currentArrayReceiverLength(
	capture checkArrayReceiverCapture,
) checkArrayReceiverLength {
	if !capture.exact {
		return checkArrayReceiverLength{}
	}
	if capture.literal {
		return checkArrayReceiverLength{
			length: capture.length,
			exact:  true,
		}
	}
	if capture.name == "" ||
		c.localBindingGenerations[capture.name] != capture.generation {
		return checkArrayReceiverLength{}
	}
	current, exact := c.localStaticValuesFor(capture.name)
	if !exact || len(current) != 1 || current[0] != capture.alternative {
		return checkArrayReceiverLength{}
	}
	return checkArrayReceiverLength{
		length: capture.length,
		exact:  true,
	}
}

func cloneCheckCallSplatSource(source checkCallSplatSource) checkCallSplatSource {
	return checkCallSplatSource{
		identity:     append([]capturedContainerRoot(nil), source.identity...),
		alternatives: append([]Expression(nil), source.alternatives...),
		evaluation:   source.evaluation,
	}
}

func cloneCheckStaticChoiceFact(fact checkStaticChoiceFact) checkStaticChoiceFact {
	return checkStaticChoiceFact{
		source:  cloneCheckCallSplatSource(fact.source),
		indices: append([]int(nil), fact.indices...),
	}
}

func (c *scriptChecker) localStaticChoiceFor(name string) (checkStaticChoiceFact, bool) {
	if _, poisoned := c.typePoison[name]; poisoned {
		return checkStaticChoiceFact{}, false
	}
	if _, poisoned := c.staticValuePoison[name]; poisoned {
		return checkStaticChoiceFact{}, false
	}
	fact, ok := c.localValueFactFor(name)
	return cloneCheckStaticChoiceFact(fact.staticChoice),
		ok && len(fact.staticVals) > 0 &&
			len(fact.staticChoice.indices) == len(fact.staticVals) &&
			checkCallSplatSourceIdentified(fact.staticChoice.source)
}

func checkCallSplatSourceIdentified(source checkCallSplatSource) bool {
	return len(source.identity) > 0 || source.evaluation != nil
}

func (c *scriptChecker) checkCallSplatSourceForLocal(
	name string,
	alternatives []Expression,
) checkCallSplatSource {
	names := c.containerIdentityNames(name)
	ordered := make([]string, 0, len(names))
	for identityName := range names {
		ordered = append(ordered, identityName)
	}
	slices.Sort(ordered)
	identity := make([]capturedContainerRoot, 0, len(ordered))
	for _, identityName := range ordered {
		identity = append(identity, capturedContainerRoot{
			name:       identityName,
			generation: c.localBindingGenerations[identityName],
		})
	}
	return checkCallSplatSource{
		identity:     identity,
		alternatives: append([]Expression(nil), alternatives...),
	}
}

func (c *scriptChecker) bindLocalStaticValues(name string, values []Expression) {
	c.bindLocalExactValueFact(name, checkLocalValueFact{staticVals: values})
}

func (c *scriptChecker) bindLocalStaticValuesWithChoice(
	name string,
	values []Expression,
	choice checkStaticChoiceFact,
) {
	c.bindLocalExactValueFact(name, checkLocalValueFact{
		staticVals:   values,
		staticChoice: cloneCheckStaticChoiceFact(choice),
	})
}

func (c *scriptChecker) bindLocalExactValueFact(name string, valueFact checkLocalValueFact) {
	if name == "" || len(c.localTypes) == 0 {
		return
	}
	for i := len(c.localTypes) - 1; i >= 0; i-- {
		if _, tracked := c.localTypes[i][name]; !tracked {
			continue
		}
		originalStaticValues := valueFact.staticVals
		valueFact.instanceOrigins = normalizeCheckExpressionIdentities(valueFact.instanceOrigins)
		valueFact.staticVals = c.normalizeCheckStaticValues(valueFact.staticVals)
		if len(valueFact.instanceOrigins) == 0 &&
			len(valueFact.staticVals) == 0 {
			delete(c.localClassValues[i], name)
			return
		}
		choiceAligned := len(valueFact.staticChoice.indices) == len(originalStaticValues) &&
			len(valueFact.staticVals) == len(originalStaticValues) &&
			checkCallSplatSourceIdentified(valueFact.staticChoice.source)
		if choiceAligned {
			for i := range valueFact.staticVals {
				if valueFact.staticVals[i] != originalStaticValues[i] {
					choiceAligned = false
					break
				}
			}
		}
		if !choiceAligned {
			valueFact.staticChoice = checkStaticChoiceFact{}
		} else {
			valueFact.staticChoice = cloneCheckStaticChoiceFact(valueFact.staticChoice)
		}
		for _, frame := range c.localClassValues {
			for otherName, otherFact := range frame {
				if otherName == name {
					continue
				}
				sameRoot, newContainsOther, otherContainsNew, sharesNested := staticValueMutableRelationships(valueFact.staticVals, otherFact.staticVals)
				if !sameRoot && !newContainsOther && !otherContainsNew && !sharesNested {
					continue
				}
				definiteRoot := sameRoot && len(valueFact.staticVals) == 1 &&
					len(otherFact.staticVals) == 1
				if definiteRoot {
					c.linkContainerIdentityAlias(name, otherName)
				} else {
					c.linkContainerAlias(name, otherName)
				}
				switch {
				case sameRoot || sharesNested:
					c.linkStaticValueAlias(name, otherName)
				default:
					if newContainsOther {
						c.linkStaticValueDependency(otherName, name)
					}
					if otherContainsNew {
						c.linkStaticValueDependency(name, otherName)
					}
				}
			}
		}
		if c.localClassValues[i] == nil {
			c.localClassValues[i] = make(checkClassValueFrame)
		}
		c.localClassValues[i][name] = valueFact
		return
	}
}

func staticValueMutableRelationships(left, right []Expression) (
	sameRoot,
	leftContainsRight,
	rightContainsLeft,
	sharesNested bool,
) {
	for _, leftValue := range left {
		leftContainers := mutableStaticContainers(leftValue)
		if len(leftContainers) == 0 {
			continue
		}
		for _, rightValue := range right {
			rightContainers := mutableStaticContainers(rightValue)
			if len(rightContainers) == 0 {
				continue
			}
			if leftValue == rightValue {
				sameRoot = true
				continue
			}
			if _, contained := leftContainers[rightValue]; contained {
				leftContainsRight = true
			}
			if _, contained := rightContainers[leftValue]; contained {
				rightContainsLeft = true
			}
			for container := range leftContainers {
				if container == leftValue || container == rightValue {
					continue
				}
				if _, shared := rightContainers[container]; shared {
					sharesNested = true
					break
				}
			}
		}
	}
	return sameRoot, leftContainsRight, rightContainsLeft, sharesNested
}

func mutableStaticContainers(expr Expression) map[Expression]struct{} {
	containers := make(map[Expression]struct{})
	var collect func(Expression)
	collect = func(current Expression) {
		switch typed := current.(type) {
		case *ArrayLiteral:
			containers[current] = struct{}{}
			for _, element := range typed.Elements {
				collect(element)
			}
		case *HashLiteral:
			containers[current] = struct{}{}
			for _, pair := range typed.Pairs {
				collect(pair.Value)
			}
		}
	}
	collect(expr)
	return containers
}

// capturedDestructureProjectionContainer recognizes exact container snapshots
// whose non-literal leaves have durable checker identities.
func (c *scriptChecker) capturedDestructureProjectionContainer(expr Expression) bool {
	validLeaf := func(element Expression) bool {
		if c.checkStaticValueCandidate(element) {
			return true
		}
		_, captured := c.destructureProjectionFacts[element]
		return captured
	}
	switch typed := expr.(type) {
	case *ArrayLiteral:
		for _, element := range typed.Elements {
			if !validLeaf(element) {
				return false
			}
		}
		return true
	case *HashLiteral:
		if typed.ShapeType != nil && !c.hashShapeStaticallyShadowed(typed) {
			return false
		}
		for _, pair := range typed.Pairs {
			if _, static := staticLiteralValue(pair.Key); !static ||
				!validLeaf(pair.Value) {
				return false
			}
		}
		return true
	default:
		return false
	}
}

func (c *scriptChecker) checkStaticValueCandidate(expr Expression) bool {
	if _, static := staticLiteralValue(expr); static {
		return true
	}
	return c.capturedDestructureProjectionContainer(expr)
}

func (c *scriptChecker) localKeywordSplatFails(name string) bool {
	fact, ok := c.localValueFactFor(name)
	return ok && fact.keywordSplatFails
}

func (c *scriptChecker) bindLocalKeywordSplatFailure(name string, keys ...string) {
	if name == "" || len(c.localTypes) == 0 {
		return
	}
	for i := len(c.localTypes) - 1; i >= 0; i-- {
		if _, tracked := c.localTypes[i][name]; !tracked {
			continue
		}
		if c.localClassValues[i] == nil {
			c.localClassValues[i] = make(checkClassValueFrame)
		}
		fact := c.localClassValues[i][name]
		if !fact.keywordSplatFails || len(fact.invalidKeywordSplatKeys) > 0 {
			if len(keys) == 0 {
				fact.invalidKeywordSplatKeys = nil
			} else {
				if fact.invalidKeywordSplatKeys == nil {
					fact.invalidKeywordSplatKeys = make(map[string]struct{}, len(keys))
				}
				for _, key := range keys {
					fact.invalidKeywordSplatKeys[key] = struct{}{}
				}
			}
		}
		fact.classNames = nil
		fact.instanceOrigins = nil
		fact.callables = nil
		fact.staticVals = nil
		fact.staticChoice = checkStaticChoiceFact{}
		fact.keywordSplatFails = true
		c.localClassValues[i][name] = fact
		return
	}
}

func (c *scriptChecker) bindLocalKeywordSplatFailureAliases(name string, keys ...string) {
	seen := make(map[string]struct{})
	var bind func(string)
	bind = func(current string) {
		if current == "" {
			return
		}
		if _, visited := seen[current]; visited {
			return
		}
		seen[current] = struct{}{}
		c.bindLocalKeywordSplatFailure(current, keys...)
		for alias, edge := range c.typeAliases[current] {
			if c.bindingEdgeCurrent(current, alias, edge) {
				bind(alias)
			}
		}
	}
	bind(name)
}

func (c *scriptChecker) applyKeywordSplatDeleteFact(call *CallExpr) {
	if call == nil || len(call.Args) != 1 || len(call.KwArgs) != 0 {
		return
	}
	member, ok := call.Callee.(*MemberExpr)
	if !ok || member.Property != "delete" {
		return
	}
	ident, ok := member.Object.(*Identifier)
	if !ok {
		return
	}
	key, ok := staticLiteralHashKey(call.Args[0])
	if !ok {
		return
	}
	seen := make(map[string]struct{})
	var update func(string)
	update = func(name string) {
		if _, visited := seen[name]; visited {
			return
		}
		seen[name] = struct{}{}
		for i := len(c.localTypes) - 1; i >= 0; i-- {
			if _, tracked := c.localTypes[i][name]; !tracked {
				continue
			}
			fact, exists := c.localClassValues[i][name]
			if exists && fact.keywordSplatFails && len(fact.invalidKeywordSplatKeys) > 0 {
				delete(fact.invalidKeywordSplatKeys, key)
				if len(fact.invalidKeywordSplatKeys) == 0 {
					delete(c.localClassValues[i], name)
				} else {
					c.localClassValues[i][name] = fact
				}
			}
			break
		}
		for alias, edge := range c.typeAliases[name] {
			if c.bindingEdgeCurrent(name, alias, edge) {
				update(alias)
			}
		}
	}
	update(ident.Name)
}

// localTypeFor returns the innermost inferred type fact for name, or nil when
// the checker knows nothing about it.
func (c *scriptChecker) localTypeFor(name string) *TypeExpr {
	if _, poisoned := c.typePoison[name]; poisoned {
		return nil
	}
	for i := len(c.localTypes) - 1; i >= 0; i-- {
		if ty, ok := c.localTypes[i][name]; ok {
			return ty
		}
	}
	return nil
}

// bindLocalType records an assignment fact in the innermost frame that
// already tracks the name, mirroring how runtime assignment writes through to
// a captured outer binding, or in the current frame for a fresh local.
func (c *scriptChecker) bindLocalType(name string, ty *TypeExpr) {
	if name == "" || len(c.localTypes) == 0 {
		return
	}
	for i := len(c.localTypes) - 1; i >= 0; i-- {
		if _, ok := c.localTypes[i][name]; ok {
			c.localTypes[i][name] = ty
			if strings.HasPrefix(name, "@") {
				c.instanceIvarFactsDirty = true
			}
			return
		}
	}
	c.bindLocalTypeInCurrentFrame(name, ty)
}

// bindLocalTypeInCurrentFrame records a fact in the innermost frame
// unconditionally. Parameter bindings use it so a block parameter that
// shadows an outer local does not overwrite the outer fact.
func (c *scriptChecker) bindLocalTypeInCurrentFrame(name string, ty *TypeExpr) {
	if name == "" || len(c.localTypes) == 0 {
		return
	}
	frame := c.localTypes[len(c.localTypes)-1]
	if frame == nil {
		frame = make(checkTypeFrame)
		c.localTypes[len(c.localTypes)-1] = frame
	}
	frame[name] = ty
	if strings.HasPrefix(name, "@") {
		c.instanceIvarFactsDirty = true
	}
}

func (c *scriptChecker) advanceLocalBindingGeneration(name string) {
	if name == "" {
		return
	}
	if c.localBindingGenerations == nil {
		c.localBindingGenerations = make(map[string]uint64)
	}
	c.localBindingGenerations[name]++
	delete(c.containerSelections, name)
	if c.mutationRegionDepth == 0 {
		delete(c.degradedContainerBindings, name)
	}
}

// preserveContainerBindingBeforeDegrade records container provenance without
// restoring the discarded type fact. Zero-or-many regions use it to recognize
// later escapes of a binding whose value is deliberately unknown.
func (c *scriptChecker) preserveContainerBindingBeforeDegrade(name string) {
	if name == "" {
		return
	}
	if !c.typeExprHasContainerArm(c.localTypeFor(name)) &&
		!c.hasDegradedContainerBinding(name) &&
		!c.hasCurrentContainerAlias(name) {
		return
	}
	if c.degradedContainerBindings == nil {
		c.degradedContainerBindings = make(map[string]struct{})
	}
	c.degradedContainerBindings[name] = struct{}{}
}

func (c *scriptChecker) hasDegradedContainerBinding(name string) bool {
	_, degraded := c.degradedContainerBindings[name]
	return degraded
}

func (c *scriptChecker) hasPossibleContainerBinding(name string) bool {
	return c.hasDegradedContainerBinding(name) || c.hasCurrentContainerAlias(name)
}

// poisonLocalType marks a local as permanently unknown for the rest of the
// current function walk. The set is monotone: branch and loop restores do not
// clear it, so a fact invalidated inside a region the checker walks
// out-of-order can never resurface.
func (c *scriptChecker) poisonLocalType(name string) {
	if name == "" {
		return
	}
	// A name may have been poisoned before a later container relationship was
	// discovered. Walk the current graph even in that case so a subsequent
	// mutation through the already-unknown name still invalidates its newer
	// aliases. The local seen set, rather than the poison set, breaks cycles.
	seen := make(map[string]struct{})
	stack := []string{name}
	for len(stack) > 0 {
		last := len(stack) - 1
		current := stack[last]
		stack = stack[:last]
		if _, visited := seen[current]; visited {
			continue
		}
		seen[current] = struct{}{}
		c.poisonLocalTypeOnly(current)
		// Containers assign by reference, so a mutation through any alias
		// invalidates every name sharing the value.
		for alias, edge := range c.typeAliases[current] {
			if c.bindingEdgeCurrent(current, alias, edge) {
				stack = append(stack, alias)
			}
		}
	}
}

func (c *scriptChecker) poisonLocalTypeOnly(name string) bool {
	if name == "" {
		return false
	}
	c.poisonEvaluatedDestructureSourceType(name)
	c.preserveContainerBindingBeforeDegrade(name)
	if c.typePoison == nil {
		c.typePoison = make(map[string]struct{})
	}
	if _, done := c.typePoison[name]; done {
		return false
	}
	c.typePoison[name] = struct{}{}
	return true
}

func (c *scriptChecker) poisonEvaluatedDestructureSourceType(name string) {
	generation := c.localBindingGenerations[name]
	for expr, fact := range c.evaluatedDestructureFacts {
		if len(fact.staticVals) > 0 {
			continue
		}
		invalidated := fact.sourceName == name && fact.sourceGen == generation
		for _, root := range fact.retainedRoots {
			if root.name == name && root.generation == generation {
				invalidated = true
				break
			}
		}
		if !invalidated {
			continue
		}
		fact.assigned = nil
		c.evaluatedDestructureFacts[expr] = fact
	}
}

// poisonLocalStaticValues permanently invalidates exact whole-value facts
// without discarding a compatible declared type or another value-fact kind.
// It is monotone so loop and branch restores cannot resurrect a pre-mutation
// array value.
func (c *scriptChecker) poisonLocalStaticValues(name string) {
	if name == "" {
		return
	}
	localFact, _ := c.localValueFactFor(name)
	values := append([]Expression(nil), localFact.staticVals...)
	if c.staticValuePoison == nil {
		c.staticValuePoison = make(map[string]struct{})
	}
	if _, done := c.staticValuePoison[name]; done {
		return
	}
	c.staticValuePoison[name] = struct{}{}
	c.poisonEvaluatedDestructureFacts(values)
	for dependent, edge := range c.staticValueDependents[name] {
		if c.bindingEdgeCurrent(name, dependent, edge) {
			c.poisonLocalStaticValues(dependent)
		}
	}
}

func (c *scriptChecker) poisonEvaluatedDestructureFacts(values []Expression) {
	if len(values) == 0 {
		return
	}
	for expr, fact := range c.evaluatedDestructureFacts {
		if len(fact.staticVals) == 0 {
			continue
		}
		sameRoot, valuesContainFact, factContainsValues, sharesNested := staticValueMutableRelationships(values, fact.staticVals)
		if !sameRoot && !valuesContainFact && !factContainsValues && !sharesNested {
			continue
		}
		fact.staticVals = nil
		fact.staticChoice = checkStaticChoiceFact{}
		fact.assigned = nil
		if fact.factKind == destructureStaticFact {
			fact.factKind = 0
		}
		c.evaluatedDestructureFacts[expr] = fact
	}
}

func (c *scriptChecker) linkStaticValueDependency(source, dependent string) {
	if source == "" || dependent == "" || source == dependent {
		return
	}
	if c.staticValueDependents == nil {
		c.staticValueDependents = make(map[string]map[string]checkBindingEdge)
	}
	if c.staticValueDependents[source] == nil {
		c.staticValueDependents[source] = make(map[string]checkBindingEdge)
	}
	c.staticValueDependents[source][dependent] = c.newBindingEdge(source, dependent)
	if _, poisoned := c.staticValuePoison[source]; poisoned {
		c.poisonLocalStaticValues(dependent)
	}
}

func (c *scriptChecker) linkStaticValueAlias(a, b string) {
	c.linkStaticValueDependency(a, b)
	c.linkStaticValueDependency(b, a)
}

// linkValueAlias records that two current local bindings read the same
// runtime value. Binding generations expire the relation when either local is
// rebound, while assignment transfers preserve the remaining aliases.
func (c *scriptChecker) linkValueAlias(a, b string) {
	if a == "" || b == "" || a == b {
		return
	}
	if c.valueAliases == nil {
		c.valueAliases = make(map[string]map[string]checkBindingEdge)
	}
	if c.valueAliases[a] == nil {
		c.valueAliases[a] = make(map[string]checkBindingEdge)
	}
	if c.valueAliases[b] == nil {
		c.valueAliases[b] = make(map[string]checkBindingEdge)
	}
	c.valueAliases[a][b] = c.newBindingEdge(a, b)
	c.valueAliases[b][a] = c.newBindingEdge(b, a)
}

func (c *scriptChecker) valueAliasNames(name string) map[string]struct{} {
	aliases := map[string]struct{}{name: {}}
	stack := []string{name}
	for len(stack) > 0 {
		last := len(stack) - 1
		current := stack[last]
		stack = stack[:last]
		for alias, edge := range c.valueAliases[current] {
			if !c.bindingEdgeCurrent(current, alias, edge) {
				continue
			}
			if _, visited := aliases[alias]; visited {
				continue
			}
			aliases[alias] = struct{}{}
			stack = append(stack, alias)
		}
	}
	return aliases
}

func (c *scriptChecker) captureValueAliasTransfer(value Expression) map[string]struct{} {
	identifier, ok := value.(*Identifier)
	if !ok || c.localTypeFor(identifier.Name) == nil {
		return nil
	}
	return c.valueAliasNames(identifier.Name)
}

func (c *scriptChecker) applyValueAliasTransfer(target string, aliases map[string]struct{}) {
	for alias := range aliases {
		if alias != target {
			c.linkValueAlias(target, alias)
		}
	}
}

// linkContainerAlias records that two locals may share one mutable
// container, so poisoning either cascades to the other. Links retain their
// function-scoped history, while binding generations make an edge inactive
// after either endpoint is rebound.
func (c *scriptChecker) linkContainerAlias(a, b string) {
	if a == "" || b == "" || a == b {
		return
	}
	if c.typeAliases == nil {
		c.typeAliases = make(map[string]map[string]checkBindingEdge)
	}
	if c.typeAliases[a] == nil {
		c.typeAliases[a] = make(map[string]checkBindingEdge)
	}
	if c.typeAliases[b] == nil {
		c.typeAliases[b] = make(map[string]checkBindingEdge)
	}
	c.typeAliases[a][b] = c.newBindingEdge(a, b)
	c.typeAliases[b][a] = c.newBindingEdge(b, a)
}

func (c *scriptChecker) linkContainerIdentityAlias(a, b string) {
	if a == "" || b == "" || a == b {
		return
	}
	c.linkContainerAlias(a, b)
	if c.containerIdentityAliases == nil {
		c.containerIdentityAliases = make(map[string]map[string]checkBindingEdge)
	}
	if c.containerIdentityAliases[a] == nil {
		c.containerIdentityAliases[a] = make(map[string]checkBindingEdge)
	}
	if c.containerIdentityAliases[b] == nil {
		c.containerIdentityAliases[b] = make(map[string]checkBindingEdge)
	}
	c.containerIdentityAliases[a][b] = c.newBindingEdge(a, b)
	c.containerIdentityAliases[b][a] = c.newBindingEdge(b, a)
}

func (c *scriptChecker) newBindingEdge(from, to string) checkBindingEdge {
	return checkBindingEdge{
		fromGeneration: c.localBindingGenerations[from],
		toGeneration:   c.localBindingGenerations[to],
	}
}

func (c *scriptChecker) bindingEdgeCurrent(from, to string, edge checkBindingEdge) bool {
	return edge.fromGeneration == c.localBindingGenerations[from] &&
		edge.toGeneration == c.localBindingGenerations[to]
}

func (c *scriptChecker) hasCurrentContainerAlias(name string) bool {
	for alias, edge := range c.typeAliases[name] {
		if c.bindingEdgeCurrent(name, alias, edge) {
			return true
		}
	}
	return false
}

func (c *scriptChecker) containerIdentityNames(name string) map[string]struct{} {
	identities := map[string]struct{}{name: {}}
	stack := []string{name}
	for len(stack) > 0 {
		last := len(stack) - 1
		current := stack[last]
		stack = stack[:last]
		for alias, edge := range c.containerIdentityAliases[current] {
			if !c.bindingEdgeCurrent(current, alias, edge) {
				continue
			}
			if _, visited := identities[alias]; visited {
				continue
			}
			identities[alias] = struct{}{}
			stack = append(stack, alias)
		}
	}
	return identities
}

func (c *scriptChecker) captureContainerAliasTransfer(value Expression) checkContainerAliasTransfer {
	ident, direct := value.(*Identifier)
	if !direct {
		return checkContainerAliasTransfer{}
	}
	source, retained := c.retainedContainerRoot(ident)
	if !retained {
		return checkContainerAliasTransfer{}
	}
	identities := c.containerIdentityNames(source)
	aliases := c.containerAliasNames(source)
	staticSources, staticDependents := c.staticDependencyReachability(identities, aliases)
	return checkContainerAliasTransfer{
		identities:       identities,
		aliases:          aliases,
		staticSources:    staticSources,
		staticDependents: staticDependents,
	}
}

func (c *scriptChecker) containerAliasNames(name string) map[string]struct{} {
	aliases := map[string]struct{}{name: {}}
	stack := []string{name}
	for len(stack) > 0 {
		last := len(stack) - 1
		current := stack[last]
		stack = stack[:last]
		for alias, edge := range c.typeAliases[current] {
			if !c.bindingEdgeCurrent(current, alias, edge) {
				continue
			}
			if _, visited := aliases[alias]; visited {
				continue
			}
			aliases[alias] = struct{}{}
			stack = append(stack, alias)
		}
	}
	return aliases
}

func (c *scriptChecker) staticDependencyReachability(
	roots, aliases map[string]struct{},
) (map[string]struct{}, map[string]struct{}) {
	dependents := c.snapshotBindingRelations(c.staticValueDependents)
	sources := make(checkNameRelations)
	for source, targets := range dependents {
		for target := range targets {
			if sources[target] == nil {
				sources[target] = make(map[string]struct{})
			}
			sources[target][source] = struct{}{}
		}
	}
	return relationReachability(roots, aliases, sources),
		relationReachability(roots, aliases, dependents)
}

func relationReachability(
	roots, relevantNames map[string]struct{},
	relations checkNameRelations,
) map[string]struct{} {
	reachable := cloneCheckStringSet(roots)
	stack := make([]string, 0, len(roots))
	for name := range roots {
		stack = append(stack, name)
	}
	for len(stack) > 0 {
		last := len(stack) - 1
		current := stack[last]
		stack = stack[:last]
		for related := range relations[current] {
			if _, relevant := relevantNames[related]; !relevant {
				continue
			}
			if _, visited := reachable[related]; visited {
				continue
			}
			reachable[related] = struct{}{}
			stack = append(stack, related)
		}
	}
	return reachable
}

func (c *scriptChecker) applyContainerAliasTransfer(target string, transfer checkContainerAliasTransfer) {
	if len(transfer.identities) == 0 {
		return
	}
	if c.localTypeFor(target) == nil {
		if c.degradedContainerBindings == nil {
			c.degradedContainerBindings = make(map[string]struct{})
		}
		c.degradedContainerBindings[target] = struct{}{}
	}
	for name := range transfer.identities {
		if name == target {
			continue
		}
		c.linkContainerIdentityAlias(target, name)
		c.linkStaticValueAlias(target, name)
	}
	for name := range transfer.aliases {
		if name == target {
			continue
		}
		if _, identical := transfer.identities[name]; identical {
			continue
		}
		c.linkContainerAlias(target, name)
	}
	for source := range transfer.staticSources {
		if source != target {
			c.linkStaticValueDependency(source, target)
		}
	}
	for dependent := range transfer.staticDependents {
		if dependent != target {
			c.linkStaticValueDependency(target, dependent)
		}
	}
}

// applyPossibleContainerAliasTransfer retains only the may-relations from a
// binding that survives on one branch of a logical assignment.
func (c *scriptChecker) applyPossibleContainerAliasTransfer(target string, transfer checkContainerAliasTransfer) {
	if len(transfer.identities) == 0 {
		return
	}
	if c.localTypeFor(target) == nil {
		if c.degradedContainerBindings == nil {
			c.degradedContainerBindings = make(map[string]struct{})
		}
		c.degradedContainerBindings[target] = struct{}{}
	}
	for name := range transfer.identities {
		if name != target {
			c.linkContainerAlias(target, name)
		}
	}
	for name := range transfer.aliases {
		if name == target {
			continue
		}
		if _, identical := transfer.identities[name]; identical {
			continue
		}
		c.linkContainerAlias(target, name)
	}
	for source := range transfer.staticSources {
		if source != target {
			c.linkStaticValueDependency(source, target)
		}
	}
	for dependent := range transfer.staticDependents {
		if dependent != target {
			c.linkStaticValueDependency(target, dependent)
		}
	}
}

func (c *scriptChecker) snapshotContainerIdentityRelations() checkNameRelations {
	relations := make(checkNameRelations)
	visited := make(map[string]struct{})
	for name := range c.containerIdentityAliases {
		if _, seen := visited[name]; seen {
			continue
		}
		identities := c.containerIdentityNames(name)
		for identity := range identities {
			visited[identity] = struct{}{}
		}
		if len(identities) < 2 {
			continue
		}
		for from := range identities {
			if relations[from] == nil {
				relations[from] = make(map[string]struct{}, len(identities)-1)
			}
			for to := range identities {
				if from != to {
					relations[from][to] = struct{}{}
				}
			}
		}
	}
	return relations
}

func (c *scriptChecker) restoreContainerIdentityRelations(relations checkNameRelations) {
	c.containerIdentityAliases = nil
	for from, aliases := range relations {
		for to := range aliases {
			if from < to {
				c.linkContainerIdentityAlias(from, to)
			}
		}
	}
}

func intersectContainerIdentityRelations(states []checkScopeState) checkNameRelations {
	return intersectScopeRelations(states, func(state checkScopeState) checkNameRelations {
		return state.containerIdentity
	})
}

func intersectValueAliasRelations(states []checkScopeState) checkNameRelations {
	return intersectScopeRelations(states, func(state checkScopeState) checkNameRelations {
		return state.valueAlias
	})
}

func intersectScopeRelations(
	states []checkScopeState,
	selectRelations func(checkScopeState) checkNameRelations,
) checkNameRelations {
	if len(states) == 0 {
		return nil
	}
	intersection := cloneCheckNameRelations(selectRelations(states[0]))
	for _, state := range states[1:] {
		relations := selectRelations(state)
		for from, aliases := range intersection {
			for to := range aliases {
				if _, exists := relations[from][to]; !exists {
					delete(aliases, to)
				}
			}
			if len(aliases) == 0 {
				delete(intersection, from)
			}
		}
	}
	return intersection
}

func cloneCheckNameRelations(relations checkNameRelations) checkNameRelations {
	if len(relations) == 0 {
		return nil
	}
	clone := make(checkNameRelations, len(relations))
	for from, aliases := range relations {
		clone[from] = cloneCheckStringSet(aliases)
	}
	return clone
}

func (c *scriptChecker) snapshotBindingRelations(
	bindings map[string]map[string]checkBindingEdge,
) checkNameRelations {
	relations := make(checkNameRelations)
	for from, aliases := range bindings {
		for to, edge := range aliases {
			if !c.bindingEdgeCurrent(from, to, edge) {
				continue
			}
			if relations[from] == nil {
				relations[from] = make(map[string]struct{})
			}
			relations[from][to] = struct{}{}
		}
	}
	return relations
}

func (c *scriptChecker) restoreContainerAliasRelations(relations checkNameRelations) {
	clear(c.typeAliases)
	for from, aliases := range relations {
		for to := range aliases {
			if from < to {
				c.linkContainerAlias(from, to)
			}
		}
	}
}

func (c *scriptChecker) restoreStaticValueDependencyRelations(relations checkNameRelations) {
	clear(c.staticValueDependents)
	for source, dependents := range relations {
		for dependent := range dependents {
			c.linkStaticValueDependency(source, dependent)
		}
	}
}

func (c *scriptChecker) restoreValueAliasRelations(relations checkNameRelations) {
	clear(c.valueAliases)
	for from, aliases := range relations {
		for to := range aliases {
			if from < to {
				c.linkValueAlias(from, to)
			}
		}
	}
}

func unionContainerAliasRelations(states []checkScopeState) checkNameRelations {
	return unionScopeRelations(states, func(state checkScopeState) checkNameRelations {
		return state.containerAlias
	})
}

func unionStaticValueDependencyRelations(states []checkScopeState) checkNameRelations {
	return unionScopeRelations(states, func(state checkScopeState) checkNameRelations {
		return state.staticDependents
	})
}

func unionDegradedContainerBindings(states []checkScopeState) map[string]struct{} {
	var union map[string]struct{}
	for _, state := range states {
		union = unionCheckStringSet(union, state.degradedContainers)
	}
	return union
}

func (c *scriptChecker) mergeScopeBindingRelations(states []checkScopeState) {
	c.restoreContainerAliasRelations(unionContainerAliasRelations(states))
	c.restoreStaticValueDependencyRelations(unionStaticValueDependencyRelations(states))
	c.restoreValueAliasRelations(intersectValueAliasRelations(states))
	c.restoreContainerIdentityRelations(intersectContainerIdentityRelations(states))
	c.restoreContainerSelections(intersectContainerSelections(states))
	c.degradedContainerBindings = unionDegradedContainerBindings(states)
}

// restoreScopeBindingRelationsForNames keeps a block parameter's name-keyed
// relation changes from escaping into a shadowed outer binding.
func restoreScopeBindingRelationsForNames(
	state *checkScopeState,
	entry checkScopeState,
	names map[string]struct{},
) {
	if state == nil || len(names) == 0 {
		return
	}
	state.containerAlias = restoreCheckNameRelationsForNames(
		state.containerAlias,
		entry.containerAlias,
		names,
	)
	state.containerIdentity = restoreCheckNameRelationsForNames(
		state.containerIdentity,
		entry.containerIdentity,
		names,
	)
	state.staticDependents = restoreCheckNameRelationsForNames(
		state.staticDependents,
		entry.staticDependents,
		names,
	)
	state.valueAlias = restoreCheckNameRelationsForNames(
		state.valueAlias,
		entry.valueAlias,
		names,
	)
	state.degradedContainers = restoreCheckStringSetNames(
		state.degradedContainers,
		entry.degradedContainers,
		names,
	)
	if state.containerSelection == nil && len(entry.containerSelection) > 0 {
		state.containerSelection = make(map[string]checkContainerSelection)
	}
	for name := range names {
		delete(state.containerSelection, name)
		if selection, exists := entry.containerSelection[name]; exists {
			state.containerSelection[name] = selection
		}
	}
}

func removeScopeBindingRelationsForNames(state *checkScopeState, names map[string]struct{}) {
	if state == nil || len(names) == 0 {
		return
	}
	state.containerAlias = removeCheckNameRelationsForNames(state.containerAlias, names)
	state.containerIdentity = removeCheckNameRelationsForNames(state.containerIdentity, names)
	state.staticDependents = removeCheckNameRelationsForNames(state.staticDependents, names)
	state.valueAlias = removeCheckNameRelationsForNames(state.valueAlias, names)
	state.degradedContainers = restoreCheckStringSetNames(state.degradedContainers, nil, names)
	for name := range names {
		delete(state.containerSelection, name)
	}
}

func restoreCheckNameRelationsForNames(
	current,
	entry checkNameRelations,
	names map[string]struct{},
) checkNameRelations {
	restored := removeCheckNameRelationsForNames(current, names)
	for from, relations := range entry {
		_, fromShadowed := names[from]
		for to := range relations {
			_, toShadowed := names[to]
			if !fromShadowed && !toShadowed {
				continue
			}
			if restored == nil {
				restored = make(checkNameRelations)
			}
			if restored[from] == nil {
				restored[from] = make(map[string]struct{})
			}
			restored[from][to] = struct{}{}
		}
	}
	return restored
}

func removeCheckNameRelationsForNames(
	current checkNameRelations,
	names map[string]struct{},
) checkNameRelations {
	restored := cloneCheckNameRelations(current)
	for from, relations := range restored {
		if _, shadowed := names[from]; shadowed {
			delete(restored, from)
			continue
		}
		for to := range relations {
			if _, shadowed := names[to]; shadowed {
				delete(relations, to)
			}
		}
		if len(relations) == 0 {
			delete(restored, from)
		}
	}
	return restored
}

func restoreCheckStringSetNames(
	current,
	entry,
	names map[string]struct{},
) map[string]struct{} {
	for name := range names {
		if _, existed := entry[name]; !existed {
			delete(current, name)
			continue
		}
		if current == nil {
			current = make(map[string]struct{})
		}
		current[name] = struct{}{}
	}
	if len(current) == 0 {
		return nil
	}
	return current
}

func unionScopeRelations(
	states []checkScopeState,
	selectRelations func(checkScopeState) checkNameRelations,
) checkNameRelations {
	var union checkNameRelations
	for _, state := range states {
		for from, aliases := range selectRelations(state) {
			if union == nil {
				union = make(checkNameRelations)
			}
			if union[from] == nil {
				union[from] = make(map[string]struct{})
			}
			for to := range aliases {
				union[from][to] = struct{}{}
			}
		}
	}
	return union
}

func (c *scriptChecker) snapshotContainerSelections() map[string]checkContainerSelection {
	if len(c.containerSelections) == 0 {
		return nil
	}
	selections := make(map[string]checkContainerSelection, len(c.containerSelections))
	for name, selection := range c.containerSelections {
		if selection.generation == c.localBindingGenerations[name] {
			selections[name] = selection
		}
	}
	return selections
}

func (c *scriptChecker) restoreContainerSelections(selections map[string]checkContainerSelection) {
	if len(selections) == 0 {
		c.containerSelections = nil
		return
	}
	c.containerSelections = make(map[string]checkContainerSelection, len(selections))
	for name, selection := range selections {
		selection.generation = c.localBindingGenerations[name]
		c.containerSelections[name] = selection
	}
}

func intersectContainerSelections(states []checkScopeState) map[string]checkContainerSelection {
	if len(states) == 0 || len(states[0].containerSelection) == 0 {
		return nil
	}
	intersection := make(map[string]checkContainerSelection, len(states[0].containerSelection))
	maps.Copy(intersection, states[0].containerSelection)
	for _, state := range states[1:] {
		for name, selection := range intersection {
			other, exists := state.containerSelection[name]
			if !exists || other.key != selection.key {
				delete(intersection, name)
			}
		}
	}
	return intersection
}

func (c *scriptChecker) bindContainerSelectionIdentity(target string, value Expression) {
	key, ok := c.containerSelectionIdentityKey(value)
	if !ok {
		return
	}
	if c.containerSelections == nil {
		c.containerSelections = make(map[string]checkContainerSelection)
	}
	for other, selection := range c.containerSelections {
		if other == target || selection.generation != c.localBindingGenerations[other] || selection.key != key {
			continue
		}
		c.linkContainerIdentityAlias(target, other)
		c.linkStaticValueAlias(target, other)
	}
	c.containerSelections[target] = checkContainerSelection{
		key:        key,
		generation: c.localBindingGenerations[target],
	}
}

func (c *scriptChecker) containerSelectionIdentityKey(value Expression) (string, bool) {
	conditional, ok := value.(*ConditionalExpr)
	if !ok {
		return "", false
	}
	condition, ok := conditional.Condition.(*Identifier)
	if !ok || c.localTypeFor(condition.Name) == nil ||
		typeExprMayIncludeCallable(c.localTypeFor(condition.Name)) {
		return "", false
	}
	consequent, consequentOK := conditional.Consequent.(*Identifier)
	alternate, alternateOK := conditional.Alternate.(*Identifier)
	if !consequentOK || !alternateOK {
		return "", false
	}
	if _, ok := c.retainedContainerRoot(consequent); !ok {
		return "", false
	}
	if _, ok := c.retainedContainerRoot(alternate); !ok {
		return "", false
	}
	var key strings.Builder
	key.WriteString(condition.Name)
	key.WriteByte(':')
	key.WriteString(strconv.FormatUint(c.localBindingGenerations[condition.Name], 10))
	key.WriteByte('?')
	key.WriteString(consequent.Name)
	key.WriteByte(':')
	key.WriteString(strconv.FormatUint(c.localBindingGenerations[consequent.Name], 10))
	key.WriteByte(':')
	key.WriteString(alternate.Name)
	key.WriteByte(':')
	key.WriteString(strconv.FormatUint(c.localBindingGenerations[alternate.Name], 10))
	return key.String(), true
}

func (c *scriptChecker) withFreshLocalInference(check func()) {
	defer c.withFreshLocalInferenceScope()()
	check()
}

func (c *scriptChecker) withFreshLocalInferenceScope() func() {
	previousPoison := c.typePoison
	previousStaticValuePoison := c.staticValuePoison
	previousStaticValueDependents := c.staticValueDependents
	previousValueAliases := c.valueAliases
	previousAliases := c.typeAliases
	previousIdentityAliases := c.containerIdentityAliases
	previousSelections := c.containerSelections
	previousDegradedContainers := c.degradedContainerBindings
	previousBindingGenerations := c.localBindingGenerations
	previousPinned := c.pinnedExpressionFacts
	previousPinnedSources := c.pinnedExpressionSources
	previousPinnedOrigins := c.pinnedInstanceOrigins
	previousConstructors := c.constructorInstanceFacts
	previousWidenedIvars := c.widenedIvarFacts
	previousInstanceIvarsDirty := c.instanceIvarFactsDirty
	previousIfClassFacts := c.evaluatedIfClassFacts
	c.typePoison = nil
	c.staticValuePoison = nil
	c.staticValueDependents = nil
	c.valueAliases = nil
	c.typeAliases = nil
	c.containerIdentityAliases = nil
	c.containerSelections = nil
	c.degradedContainerBindings = nil
	c.localBindingGenerations = nil
	c.pinnedExpressionFacts = nil
	c.pinnedExpressionSources = nil
	c.pinnedInstanceOrigins = nil
	c.constructorInstanceFacts = nil
	c.widenedIvarFacts = nil
	// The ivar dirty marker describes localTypes, which this reset keeps
	// (checkNonExecutingDefaultExpression walks a default against the seeded
	// @ facts): forcing it clean would let an unknown call in the fresh
	// scope skip widening those facts. Callers that do replace the frames
	// leave at worst a spuriously dirty marker, which costs one widening
	// pass over facts that do not exist.
	c.evaluatedIfClassFacts = nil
	return func() {
		c.typePoison = previousPoison
		c.staticValuePoison = previousStaticValuePoison
		c.staticValueDependents = previousStaticValueDependents
		c.valueAliases = previousValueAliases
		c.typeAliases = previousAliases
		c.containerIdentityAliases = previousIdentityAliases
		c.containerSelections = previousSelections
		c.degradedContainerBindings = previousDegradedContainers
		c.localBindingGenerations = previousBindingGenerations
		c.pinnedExpressionFacts = previousPinned
		c.pinnedExpressionSources = previousPinnedSources
		c.pinnedInstanceOrigins = previousPinnedOrigins
		c.constructorInstanceFacts = previousConstructors
		c.widenedIvarFacts = previousWidenedIvars
		c.instanceIvarFactsDirty = previousInstanceIvarsDirty
		c.evaluatedIfClassFacts = previousIfClassFacts
	}
}

// withIsolatedLocalInference walls off the local type-fact environment so a
// module collection pass can run assignment inference without reading or
// corrupting the facts of whatever function walk is in flight.
func (c *scriptChecker) withIsolatedLocalInference() func() {
	previousTypes := c.localTypes
	previousClassValues := c.localClassValues
	previousLive := c.liveLocalNames
	previousDepth := c.mutationRegionDepth
	previousIsolated := c.isolatedCollectInference
	c.localTypes = nil
	c.localClassValues = nil
	c.liveLocalNames = nil
	c.mutationRegionDepth = 0
	c.isolatedCollectInference = true
	restoreScope := c.withFreshLocalInferenceScope()
	return func() {
		restoreScope()
		c.localTypes = previousTypes
		c.localClassValues = previousClassValues
		c.liveLocalNames = previousLive
		c.mutationRegionDepth = previousDepth
		c.isolatedCollectInference = previousIsolated
	}
}

// poisonSkippedMutationFacts drops the container facts a subexpression the
// collection pass does not walk may invalidate: a maybe-evaluated operand is
// skipped entirely, but any mutation inside it must still degrade facts.
func (c *scriptChecker) poisonSkippedMutationFacts(expr Expression) {
	var sites []Expression
	c.collectMutationCandidateRootsFromExpression(expr, &sites)
	for _, site := range sites {
		c.poisonEscapedCallValue(site, true)
	}
}

// forTargetElementType resolves a for-loop iterable to its element type when
// it is statically known (typed arrays and ranges). It runs before the loop
// degrades body-assigned locals, because the iterable evaluates once with
// pre-loop facts.
func (c *scriptChecker) forTargetElementType(stmt *ForStmt) *TypeExpr {
	iterable := c.inferExpressionType(stmt.Iterable)
	if iterable == nil || iterable.Nullable {
		return nil
	}
	switch iterable.Kind {
	case TypeArray:
		if len(iterable.TypeArgs) == 1 && !literalArrayElementsPartial(iterable) {
			return iterable.TypeArgs[0]
		}
	case TypeRange:
		return checkTypeInt
	}
	return nil
}

// bindForTargetType binds a for-loop target to the pre-computed element type.
func (c *scriptChecker) bindForTargetType(stmt *ForStmt, elemType *TypeExpr) {
	if elemType == nil {
		return
	}
	if target, ok := stmt.Target.(*Identifier); ok {
		c.bindLocalType(target.Name, elemType)
	}
}

// degradeBlockBodyBindings resets to unknown the outer locals a block body
// may assign, excluding the names the block binds itself: a body assignment
// to a shadowing block parameter writes the block-local, so the outer fact
// still holds.
func (c *scriptChecker) degradeBlockBodyBindings(block *BlockLiteral) {
	c.degradeBlockBodyBindingsWithIvarWidening(block, true)
}

func (c *scriptChecker) degradeBlockBodyBindingsWithIvarWidening(
	block *BlockLiteral,
	widenIvars bool,
) {
	names := make(map[string]struct{})
	collectLocalBindings(block.Body, names)
	for _, frame := range c.localTypes {
		for name := range frame {
			if statementsMayAssignName(block.Body, name) {
				names[name] = struct{}{}
			}
		}
	}
	collectMutatedContainerRoots(block.Body, names)
	c.degradeMutationCandidates(block.Body, names)
	blockBound := make(map[string]struct{})
	for _, param := range block.Params {
		if param.Name != "" {
			blockBound[param.Name] = struct{}{}
		}
		collectBindingTarget(param.Target, blockBound)
	}
	for _, name := range block.ImplicitParams {
		if name != "" {
			blockBound[name] = struct{}{}
		}
	}
	for name := range names {
		if _, bound := blockBound[name]; bound {
			continue
		}
		c.preserveContainerBindingBeforeDegrade(name)
		c.bindLocalType(name, nil)
		c.bindLocalClassValue(name, "")
	}
	if widenIvars {
		c.widenRepeatedRegionBlockIvarFacts(block)
	}
}

// binaryRightUnreachable reports whether inferred facts prove a short-circuit
// right operand never evaluates. Condition-outcome reachability covers both
// direct value facts and predicates whose requested outcome contradicts an
// already narrowed receiver.
func (c *scriptChecker) binaryRightUnreachable(expr *BinaryExpr) bool {
	var evaluatingOutcome bool
	switch expr.Operator {
	case tokenOr:
		evaluatingOutcome = false
	case tokenAnd:
		evaluatingOutcome = true
	default:
		return false
	}
	_, reachable := c.probeConditionOutcome(expr.Left, evaluatingOutcome)
	return !reachable
}

// binaryRightAlwaysEvaluatesInferred reports whether inferred facts prove the
// skipped left outcome unreachable, so the right operand's effects must not be
// rolled back into a branch join.
func (c *scriptChecker) binaryRightAlwaysEvaluatesInferred(expr *BinaryExpr) bool {
	var skippedOutcome bool
	switch expr.Operator {
	case tokenAnd:
		skippedOutcome = false
	case tokenOr:
		skippedOutcome = true
	default:
		return false
	}
	_, reachable := c.probeConditionOutcome(expr.Left, skippedOutcome)
	return !reachable
}

// collectMutatedContainerRoots gathers the root identifiers of index and
// member writes so loop and block degradation clears facts about containers
// the region mutates in place, not only locals it rebinds.
func collectMutatedContainerRoots(statements []Statement, out map[string]struct{}) {
	for _, stmt := range statements {
		switch typed := stmt.(type) {
		case *AssignStmt:
			collectMutatedContainerTargetRoots(typed.Target, out)
		case *IfStmt:
			collectMutatedContainerRoots(typed.Consequent, out)
			for _, elseIf := range typed.ElseIf {
				collectMutatedContainerRoots(elseIf.Consequent, out)
			}
			collectMutatedContainerRoots(typed.Alternate, out)
		case *ForStmt:
			collectMutatedContainerRoots(typed.Body, out)
		case *WhileStmt:
			collectMutatedContainerRoots(typed.Body, out)
		case *UntilStmt:
			collectMutatedContainerRoots(typed.Body, out)
		case *TryStmt:
			collectMutatedContainerRoots(typed.Body, out)
			for i := range typed.Rescues {
				collectMutatedContainerRoots(typed.Rescues[i].Body, out)
			}
			collectMutatedContainerRoots(typed.Else, out)
			collectMutatedContainerRoots(typed.Ensure, out)
		}
	}
}

func collectMutatedContainerTargetRoots(target Expression, out map[string]struct{}) {
	switch typed := target.(type) {
	case *IndexExpr, *MemberExpr:
		if name, ok := rootIdentifierName(target); ok {
			out[name] = struct{}{}
		}
	case *DestructureTarget:
		for _, element := range typed.Elements {
			collectMutatedContainerTargetRoots(element.Target, out)
		}
	}
}

type loopBackedgeFlow struct {
	reachesBackedge bool
	exitsLoop       bool
	fallsThrough    bool
}

// loopBodyMayReachBackedge reports whether a loop body can evaluate its
// condition again. Falling off the body and next reach the backedge; break,
// return, and raise do not. Unknown constructs stay conservative.
func loopBodyMayReachBackedge(statements []Statement) bool {
	flow := loopBackedgeFlowForStatements(statements)
	return flow.reachesBackedge || flow.fallsThrough
}

func loopBackedgeFlowForStatements(statements []Statement) loopBackedgeFlow {
	flow := loopBackedgeFlow{fallsThrough: true}
	for _, stmt := range statements {
		if !flow.fallsThrough {
			break
		}
		stmtFlow := loopBackedgeFlowForStatement(stmt)
		flow.reachesBackedge = flow.reachesBackedge || stmtFlow.reachesBackedge
		flow.exitsLoop = flow.exitsLoop || stmtFlow.exitsLoop
		flow.fallsThrough = stmtFlow.fallsThrough
	}
	return flow
}

func loopBackedgeFlowForStatement(stmt Statement) loopBackedgeFlow {
	switch typed := stmt.(type) {
	case *BreakStmt:
		return loopBackedgeFlow{exitsLoop: true}
	case *ReturnStmt, *RaiseStmt:
		return loopBackedgeFlow{}
	case *NextStmt:
		return loopBackedgeFlow{reachesBackedge: true}
	case *IfStmt:
		return loopBackedgeFlowForIf(typed)
	case *TryStmt:
		return loopBackedgeControlFlowForTry(typed)
	default:
		return loopBackedgeFlow{fallsThrough: true}
	}
}

func loopBackedgeFlowForIf(stmt *IfStmt) loopBackedgeFlow {
	var flow loopBackedgeFlow
	merge := func(branch loopBackedgeFlow) {
		flow.reachesBackedge = flow.reachesBackedge || branch.reachesBackedge
		flow.exitsLoop = flow.exitsLoop || branch.exitsLoop
		flow.fallsThrough = flow.fallsThrough || branch.fallsThrough
	}

	truthy, known := staticExpressionTruthiness(stmt.Condition)
	if !known || truthy {
		merge(loopBackedgeFlowForStatements(stmt.Consequent))
	}
	if known && truthy {
		return flow
	}
	for _, branch := range stmt.ElseIf {
		truthy, known = staticExpressionTruthiness(branch.Condition)
		if !known || truthy {
			merge(loopBackedgeFlowForStatements(branch.Consequent))
		}
		if known && truthy {
			return flow
		}
	}
	merge(loopBackedgeFlowForStatements(stmt.Alternate))
	return flow
}

func loopBackedgeControlFlowForTry(stmt *TryStmt) loopBackedgeFlow {
	flow := loopBackedgeFlow{fallsThrough: true}
	merge := func(statements []Statement) {
		branch := loopBackedgeFlowForStatements(statements)
		flow.reachesBackedge = flow.reachesBackedge || branch.reachesBackedge
		flow.exitsLoop = flow.exitsLoop || branch.exitsLoop
	}
	merge(stmt.Body)
	merge(stmt.Else)
	for i := range stmt.Rescues {
		merge(stmt.Rescues[i].Body)
	}
	merge(stmt.Ensure)
	return flow
}

// collectMutationCandidateRoots gathers the escape-site expressions a
// region contains (member-call receivers, call and yield arguments — the
// walk-time poison sources), so pre-region degradation can clear the
// affected container facts before reads earlier in the region are checked.
// Dispatches whose registered contracts preserve receiver facts are skipped
// with the same gate the walk-time poison uses; the caller applies the
// container-typed escape filter.
func (c *scriptChecker) collectMutationCandidateRoots(statements []Statement, out *[]Expression) {
	for _, stmt := range statements {
		switch typed := stmt.(type) {
		case nil:
		case *ReturnStmt:
			c.collectMutationCandidateRootsFromExpression(typed.Value, out)
		case *RaiseStmt:
			c.collectMutationCandidateRootsFromExpression(typed.Value, out)
			c.collectMutationCandidateRootsFromExpression(typed.Message, out)
		case *BreakStmt:
			c.collectMutationCandidateRootsFromExpression(typed.Value, out)
		case *NextStmt:
			c.collectMutationCandidateRootsFromExpression(typed.Value, out)
		case *AssignStmt:
			c.collectMutationCandidateRootsFromExpression(typed.Target, out)
			c.collectMutationCandidateRootsFromExpression(typed.Value, out)
		case *ExprStmt:
			c.collectMutationCandidateRootsFromExpression(typed.Expr, out)
		case *IfStmt:
			c.collectMutationCandidateRootsFromExpression(typed.Condition, out)
			c.collectMutationCandidateRoots(typed.Consequent, out)
			for _, elseIf := range typed.ElseIf {
				c.collectMutationCandidateRootsFromExpression(elseIf.Condition, out)
				c.collectMutationCandidateRoots(elseIf.Consequent, out)
			}
			c.collectMutationCandidateRoots(typed.Alternate, out)
		case *ForStmt:
			c.collectMutationCandidateRootsFromExpression(typed.Iterable, out)
			c.collectMutationCandidateRoots(typed.Body, out)
		case *WhileStmt:
			c.collectMutationCandidateRootsFromExpression(typed.Condition, out)
			c.collectMutationCandidateRoots(typed.Body, out)
		case *UntilStmt:
			c.collectMutationCandidateRootsFromExpression(typed.Condition, out)
			c.collectMutationCandidateRoots(typed.Body, out)
		case *TryStmt:
			c.collectMutationCandidateRoots(typed.Body, out)
			for i := range typed.Rescues {
				c.collectMutationCandidateRoots(typed.Rescues[i].Body, out)
			}
			c.collectMutationCandidateRoots(typed.Else, out)
			c.collectMutationCandidateRoots(typed.Ensure, out)
		}
	}
}

func (c *scriptChecker) collectMutationCandidateRootsFromExpression(expr Expression, out *[]Expression) {
	switch typed := expr.(type) {
	case nil, *Identifier, *IntegerLiteral, *FloatLiteral, *StringLiteral, *RegexLiteral,
		*BoolLiteral, *NilLiteral, *SymbolLiteral, *IvarExpr, *ClassVarExpr:
	case *ArrayLiteral:
		for _, element := range typed.Elements {
			c.collectMutationCandidateRootsFromExpression(element, out)
		}
	case *HashLiteral:
		for _, pair := range typed.Pairs {
			c.collectMutationCandidateRootsFromExpression(pair.Key, out)
			c.collectMutationCandidateRootsFromExpression(pair.Value, out)
		}
	case *CallExpr:
		if member, ok := typed.Callee.(*MemberExpr); ok && c.memberCallPreservesReceiverFacts(typed) {
			// A dispatch proven to preserve receiver facts contributes no
			// mutation site of its own, matching the walk-time gate;
			// expressions nested inside the receiver still can.
			c.collectMutationCandidateRootsFromExpression(member.Object, out)
		} else {
			c.collectMutationCandidateRootsFromExpression(typed.Callee, out)
		}
		for _, arg := range typed.Args {
			*out = append(*out, arg)
			c.collectMutationCandidateRootsFromExpression(arg, out)
		}
		for _, kwarg := range typed.KwArgs {
			*out = append(*out, kwarg.Value)
			c.collectMutationCandidateRootsFromExpression(kwarg.Value, out)
		}
		if typed.Block != nil {
			for _, param := range typed.Block.Params {
				c.collectMutationCandidateRootsFromExpression(param.DefaultVal, out)
			}
			c.collectMutationCandidateRoots(typed.Block.Body, out)
		}
	case *MemberExpr:
		if !c.memberDispatchPreservesReceiverFacts(typed) {
			*out = append(*out, typed.Object)
		}
		c.collectMutationCandidateRootsFromExpression(typed.Object, out)
	case *ScopeExpr:
		c.collectMutationCandidateRootsFromExpression(typed.Object, out)
	case *IndexExpr:
		c.collectMutationCandidateRootsFromExpression(typed.Object, out)
		for _, index := range typed.Indices {
			c.collectMutationCandidateRootsFromExpression(index, out)
		}
	case *DestructureTarget:
		for _, element := range typed.Elements {
			c.collectMutationCandidateRootsFromExpression(element.Target, out)
		}
	case *SplatArg:
		c.collectMutationCandidateRootsFromExpression(typed.Value, out)
	case *TypeLiteral:
		if typed.Fallback != nil {
			c.collectMutationCandidateRootsFromExpression(typed.Fallback, out)
		}
	case *UnaryExpr:
		c.collectMutationCandidateRootsFromExpression(typed.Right, out)
	case *BinaryExpr:
		c.collectMutationCandidateRootsFromExpression(typed.Left, out)
		c.collectMutationCandidateRootsFromExpression(typed.Right, out)
	case *ConditionalExpr:
		c.collectMutationCandidateRootsFromExpression(typed.Condition, out)
		c.collectMutationCandidateRootsFromExpression(typed.Consequent, out)
		c.collectMutationCandidateRootsFromExpression(typed.Alternate, out)
	case *RescueExpr:
		c.collectMutationCandidateRootsFromExpression(typed.Body, out)
		c.collectMutationCandidateRootsFromExpression(typed.Fallback, out)
	case *IfExpr:
		c.collectMutationCandidateRootsFromExpression(typed.Condition, out)
		c.collectMutationCandidateRootsFromExpression(typed.Consequent, out)
		for _, branch := range typed.ElseIf {
			c.collectMutationCandidateRootsFromExpression(branch.Condition, out)
			c.collectMutationCandidateRootsFromExpression(branch.Result, out)
		}
		c.collectMutationCandidateRootsFromExpression(typed.Alternate, out)
	case *RangeExpr:
		c.collectMutationCandidateRootsFromExpression(typed.Start, out)
		c.collectMutationCandidateRootsFromExpression(typed.End, out)
	case *CaseExpr:
		c.collectMutationCandidateRootsFromExpression(typed.Target, out)
		for _, clause := range typed.Clauses {
			for _, value := range clause.Values {
				c.collectMutationCandidateRootsFromExpression(value.Expr, out)
			}
			c.collectMutationCandidateRootsFromExpression(clause.Result, out)
		}
		c.collectMutationCandidateRootsFromExpression(typed.ElseExpr, out)
	case *BlockLiteral:
		for _, param := range typed.Params {
			c.collectMutationCandidateRootsFromExpression(param.DefaultVal, out)
		}
		c.collectMutationCandidateRoots(typed.Body, out)
	case *YieldExpr:
		for _, arg := range typed.Args {
			*out = append(*out, arg)
			c.collectMutationCandidateRootsFromExpression(arg, out)
		}
	case *InterpolatedString:
		for _, part := range typed.Parts {
			if exprPart, ok := part.(StringExpr); ok {
				c.collectMutationCandidateRootsFromExpression(exprPart.Expr, out)
			}
		}
	case *InterpolatedSymbol:
		for _, part := range typed.Parts {
			if exprPart, ok := part.(StringExpr); ok {
				c.collectMutationCandidateRootsFromExpression(exprPart.Expr, out)
			}
		}
	case *IfStmt, *ForStmt, *WhileStmt, *UntilStmt, *TryStmt:
		c.collectMutationCandidateRoots([]Statement{typed.(Statement)}, out)
	}
}

type regionIvarEffects struct {
	writes map[string]struct{}
	// reentrant records that a lambda body reached itself, so the region can
	// run again with state the first pass has already changed.
	reentrant bool
	unknown   bool
}

func mergeRegionIvarEffects(dst *regionIvarEffects, src regionIvarEffects) {
	if dst == nil {
		return
	}
	dst.unknown = dst.unknown || src.unknown
	dst.reentrant = dst.reentrant || src.reentrant
	for name := range src.writes {
		if dst.writes == nil {
			dst.writes = make(map[string]struct{})
		}
		dst.writes[name] = struct{}{}
	}
}

// collectRepeatedRegionIvarEffects gathers the initializer-ivar effects a
// loop or block must apply before its first checker walk. Reachability pruning
// uses syntax-static outcomes only: inferred entry facts may be changed by an
// earlier statement or a later iteration. It reports whether any path may
// reach the end of the statement list.
func (c *scriptChecker) collectRepeatedRegionIvarEffects(
	statements []Statement,
	effects *regionIvarEffects,
) bool {
	for _, stmt := range statements {
		switch typed := stmt.(type) {
		case nil:
		case *ReturnStmt:
			c.collectRepeatedRegionIvarEffectsFromExpression(typed.Value, effects, true)
		case *RaiseStmt:
			if !c.unshadowedStaticRaiseErrorClass(typed) {
				c.collectRepeatedRegionIvarEffectsFromExpression(typed.Value, effects, true)
				if !c.expressionMayCompleteForBinding(typed.Value) {
					return false
				}
			}
			c.collectRepeatedRegionIvarEffectsFromExpression(typed.Message, effects, true)
		case *BreakStmt:
			c.collectRepeatedRegionIvarEffectsFromExpression(typed.Value, effects, true)
		case *NextStmt:
			c.collectRepeatedRegionIvarEffectsFromExpression(typed.Value, effects, true)
		case *AssignStmt:
			if !c.collectRepeatedRegionAssignmentIvarEffects(typed, effects) {
				return false
			}
		case *ExprStmt:
			c.collectRepeatedRegionIvarEffectsFromExpression(typed.Expr, effects, true)
			if !c.expressionMayCompleteForBinding(typed.Expr) {
				return false
			}
		case *IfStmt:
			if !c.collectRepeatedRegionIvarEffectsFromIfStatement(typed, effects) {
				return false
			}
		case *ForStmt:
			c.collectRepeatedRegionIvarEffectsFromExpression(typed.Iterable, effects, true)
			if !c.expressionMayCompleteForBinding(typed.Iterable) {
				return false
			}
			if c.exactIterableProvablyEmpty(typed.Iterable) {
				continue
			}
			bodyCompletes := c.collectRepeatedRegionIvarEffects(typed.Body, effects)
			if !bodyCompletes && c.exactIterableProvablyNonEmpty(typed.Iterable) {
				flow := loopBackedgeFlowForStatements(typed.Body)
				if !flow.reachesBackedge && !flow.exitsLoop {
					return false
				}
			}
		case *WhileStmt:
			c.collectRepeatedRegionIvarEffectsFromExpression(typed.Condition, effects, true)
			if !c.expressionMayCompleteForBinding(typed.Condition) {
				return false
			}
			truthy, known := staticExpressionTruthiness(typed.Condition)
			if !known || truthy {
				c.collectRepeatedRegionIvarEffects(typed.Body, effects)
			}
			if known && truthy && !loopBackedgeFlowForStatements(typed.Body).exitsLoop {
				return false
			}
		case *UntilStmt:
			c.collectRepeatedRegionIvarEffectsFromExpression(typed.Condition, effects, true)
			if !c.expressionMayCompleteForBinding(typed.Condition) {
				return false
			}
			truthy, known := staticExpressionTruthiness(typed.Condition)
			if !known || !truthy {
				c.collectRepeatedRegionIvarEffects(typed.Body, effects)
			}
			if known && !truthy && !loopBackedgeFlowForStatements(typed.Body).exitsLoop {
				return false
			}
		case *TryStmt:
			if !c.collectRepeatedRegionIvarEffectsFromTryStatement(typed, effects) {
				return false
			}
		}
		if statementAlwaysExits(stmt) {
			return false
		}
	}
	return true
}

func (c *scriptChecker) collectRepeatedRegionIvarEffectsFromTryStatement(
	stmt *TryStmt,
	effects *regionIvarEffects,
) bool {
	if stmt == nil {
		return true
	}

	// Body completion reaches else, errors reach only the selected rescue, and
	// every non-retry outcome reaches ensure. Keep the branch inference states
	// separate until ensure; unknown failure prefixes degrade assigned locals
	// rather than letting the syntactically last branch dictate its effects.
	baseScopeState := c.snapshotScopeState()
	selectedRescue, rescueSelectionExact := c.staticallySelectedRescue(
		stmt.Body,
		stmt.Rescues,
	)
	bodyProvenNonRaising := repeatedRegionStatementsProvenNonRaising(stmt.Body)
	rescueBodiesReachable := !bodyProvenNonRaising
	bodyCompletes := c.collectRepeatedRegionIvarEffects(stmt.Body, effects)
	bodyScopeState := c.snapshotScopeState()

	normalCompletes := false
	ensureScopeStates := make([]checkScopeState, 0, len(stmt.Rescues)+2)
	if bodyCompletes {
		normalCompletes = c.collectRepeatedRegionIvarEffects(stmt.Else, effects)
		ensureScopeStates = append(ensureScopeStates, c.snapshotScopeState())
	} else if bodyProvenNonRaising {
		ensureScopeStates = append(ensureScopeStates, bodyScopeState)
	}

	rescueCompletes := false
	var reachableRescueBodies [][]Statement
	if rescueBodiesReachable {
		failureScopeState := bodyScopeState
		if !rescueSelectionExact {
			c.restoreScopeStateWithObservedBindings(baseScopeState, bodyScopeState)
			c.degradeLocalTypesForBindings(stmt.Body)
			failureScopeState = c.snapshotScopeState()
			ensureScopeStates = append(ensureScopeStates, failureScopeState)
		} else if selectedRescue < 0 ||
			len(stmt.Rescues[selectedRescue].Body) == 0 {
			ensureScopeStates = append(ensureScopeStates, bodyScopeState)
		}
		for i := range stmt.Rescues {
			if rescueSelectionExact && i != selectedRescue {
				continue
			}
			clause := &stmt.Rescues[i]
			if len(clause.Body) == 0 {
				continue
			}
			reachableRescueBodies = append(reachableRescueBodies, clause.Body)
			c.restoreScopeState(failureScopeState)
			popScope := c.pushRescueScope(clause)
			if clause.Binding != "" {
				c.bindLocalTypeInCurrentFrame(clause.Binding, nil)
				c.bindLocalClassValue(clause.Binding, "")
			}
			clauseCompletes := c.collectRepeatedRegionIvarEffects(clause.Body, effects)
			popScope()
			ensureScopeStates = append(ensureScopeStates, c.snapshotScopeState())
			if clauseCompletes {
				rescueCompletes = true
			}
			if rescueSelectionExact {
				break
			}
		}
	}

	if rescueSelectionExact && selectedRescue >= 0 &&
		repeatedRegionBlockAlwaysRetries(stmt.Rescues[selectedRescue].Body) {
		c.restoreScopeState(baseScopeState)
		c.degradeLocalTypesForBindings(stmt.Body)
		c.degradeLocalTypesForBindings(stmt.Rescues[selectedRescue].Body)
		return false
	}

	c.mergeScopeStates(baseScopeState, ensureScopeStates)
	if rescueBodiesReachable && !rescueSelectionExact {
		c.degradeLocalTypesForBindings(stmt.Body)
	}
	if bodyCompletes {
		c.degradeLocalTypesForBindings(stmt.Else)
	}
	for _, body := range reachableRescueBodies {
		c.degradeLocalTypesForBindings(body)
	}
	ensureCompletes := c.collectRepeatedRegionIvarEffects(stmt.Ensure, effects)
	return ensureCompletes && (normalCompletes || rescueCompletes)
}

func repeatedRegionStatementsProvenNonRaising(statements []Statement) bool {
	for _, stmt := range statements {
		switch typed := stmt.(type) {
		case nil:
		case *ExprStmt:
			if !expressionProvenNonRaising(typed.Expr) {
				return false
			}
		case *AssignStmt:
			if typed.Operator != tokenNone {
				return false
			}
			if _, local := typed.Target.(*Identifier); !local ||
				!expressionProvenNonRaising(typed.Value) {
				return false
			}
		case *ReturnStmt:
			return expressionProvenNonRaising(typed.Value)
		case *BreakStmt:
			return expressionProvenNonRaising(typed.Value)
		case *NextStmt:
			return expressionProvenNonRaising(typed.Value)
		default:
			return false
		}
	}
	return true
}

func repeatedRegionBlockAlwaysRetries(statements []Statement) bool {
	for _, stmt := range statements {
		if _, retries := stmt.(*RetryStmt); retries {
			return true
		}
		if statementAlwaysExits(stmt) {
			return false
		}
	}
	return false
}

func (c *scriptChecker) collectRepeatedRegionAssignmentIvarEffects(
	stmt *AssignStmt,
	effects *regionIvarEffects,
) bool {
	if stmt == nil {
		return true
	}
	previousEvaluatedFacts := c.evaluatedDestructureFacts
	if previousEvaluatedFacts == nil {
		c.evaluatedDestructureFacts = make(map[Expression]capturedDestructureValueFact)
		defer func() { c.evaluatedDestructureFacts = previousEvaluatedFacts }()
	}
	switch stmt.Operator {
	case tokenNone:
		expectation := c.assignmentValueExpectation(stmt.Target, stmt.Value)
		c.collectRepeatedRegionIvarEffectsFromExpressionWithExpectation(
			stmt.Value,
			effects,
			expectation,
		)
		if !c.expressionMayCompleteForBindingWithExpectation(stmt.Value, expectation) {
			return false
		}
		c.captureEvaluatedDestructureFact(stmt.Value)
		return c.collectRepeatedRegionPlainAssignmentTargetIvarEffects(
			stmt.Target,
			stmt.Value,
			effects,
		)
	case tokenOrAssign, tokenAndAssign:
		receiver, targetCompletes := c.collectRepeatedRegionAssignmentReadIvarEffects(
			stmt.Target,
			effects,
			false,
		)
		if !targetCompletes {
			return false
		}
		truthy, known := c.logicalAssignmentTargetTruthiness(stmt.Target, nil)
		rhsReachable := !known ||
			stmt.Operator == tokenOrAssign && !truthy ||
			stmt.Operator == tokenAndAssign && truthy
		if !rhsReachable {
			return true
		}
		baseScopeState := c.snapshotScopeState()
		expectation := c.assignmentValueExpectation(stmt.Target, stmt.Value)
		c.collectRepeatedRegionIvarEffectsFromExpressionWithExpectation(
			stmt.Value,
			effects,
			expectation,
		)
		rhsCompletes := c.expressionMayCompleteForBindingWithExpectation(
			stmt.Value,
			expectation,
		)
		if !rhsCompletes {
			if !known {
				c.restoreScopeState(baseScopeState)
			}
			return !known
		}
		c.captureEvaluatedDestructureFact(stmt.Value)
		storeCompletes := c.collectRepeatedRegionStoreIvarEffects(
			stmt.Target,
			stmt.Value,
			receiver,
			effects,
		)
		if target, ok := stmt.Target.(*Identifier); ok {
			if known && storeCompletes {
				c.bindLocalType(target.Name, c.inferExpressionType(stmt.Value))
				c.bindExpressionLocalValueFact(target.Name, stmt.Value)
			} else {
				c.bindLocalType(target.Name, nil)
				c.bindLocalClassValue(target.Name, "")
			}
		}
		if !known {
			if !storeCompletes {
				c.restoreScopeState(baseScopeState)
				return true
			}
			writtenScopeState := c.snapshotScopeState()
			c.restoreScopeState(baseScopeState)
			c.mergeScopeStates(
				baseScopeState,
				[]checkScopeState{baseScopeState, writtenScopeState},
			)
			return true
		}
		return storeCompletes || !known
	default:
		receiver, targetCompletes := c.collectRepeatedRegionAssignmentReadIvarEffects(
			stmt.Target,
			effects,
			true,
		)
		if !targetCompletes {
			return false
		}
		operatorType := c.inferExpressionType(stmt.Target)
		c.collectRepeatedRegionIvarEffectsFromExpression(stmt.Value, effects, true)
		if !c.expressionMayCompleteForBinding(stmt.Value) {
			return false
		}
		c.captureEvaluatedDestructureFactOnce(stmt.Value)
		operatorValue := &BinaryExpr{
			Left:     stmt.Target,
			Operator: stmt.Operator,
			Right:    stmt.Value,
			Position: stmt.Pos(),
		}
		operatorCompletes := true
		c.withEvaluatedDestructureArgumentFacts([]Expression{stmt.Value}, func() {
			dispatch := c.binaryScriptDispatch(operatorValue, operatorType)
			if dispatch.mayRunScript() {
				mergeRegionIvarEffects(effects, c.scriptDispatchIvarEffects(dispatch))
			}
			operatorCompletes = c.binaryExpressionMayCompleteWithReceiver(
				operatorValue,
				operatorType,
			)
			if operatorCompletes {
				c.captureEvaluatedDestructureFact(operatorValue)
			}
		})
		if !operatorCompletes {
			return false
		}
		storeCompletes := c.collectRepeatedRegionStoreIvarEffects(
			stmt.Target,
			operatorValue,
			receiver,
			effects,
		)
		if target, ok := stmt.Target.(*Identifier); ok {
			c.bindLocalType(target.Name, nil)
			c.bindLocalClassValue(target.Name, "")
		}
		return storeCompletes
	}
}

func (c *scriptChecker) collectRepeatedRegionAssignmentReadIvarEffects(
	target Expression,
	effects *regionIvarEffects,
	autoCall bool,
) (checkAssignmentReceiverCapture, bool) {
	previous := c.assignmentReceiverCapture
	capture := &checkAssignmentReceiverCapture{target: target}
	c.assignmentReceiverCapture = capture
	defer func() { c.assignmentReceiverCapture = previous }()
	c.collectRepeatedRegionIvarEffectsFromExpression(target, effects, autoCall)
	if indexed, ok := target.(*IndexExpr); ok && capture.captured {
		return *capture, c.indexExpressionMayCompleteWithReceiver(
			indexed,
			capture.receiverType,
		)
	}
	return *capture, c.expressionMayCompleteForBinding(target)
}

func (c *scriptChecker) logicalAssignmentTargetTruthiness(
	target Expression,
	receiverFact *TypeExpr,
) (bool, bool) {
	if member, ok := target.(*MemberExpr); ok && receiverFact != nil {
		if truthy, known := c.hashLikeMemberGetterTruthiness(member, receiverFact); known {
			return truthy, true
		}
	}
	ident, local := target.(*Identifier)
	if !local {
		return c.inferredConditionTruthiness(target)
	}
	fact, tracked := c.localValueFactFor(ident.Name)
	if truthy, known := localValueFactTruthiness(fact, tracked); known {
		return truthy, true
	}
	current := c.localTypeFor(ident.Name)
	switch {
	case typeExprDefinitelyTruthy(current):
		return true, true
	case typeExprIsNilOnly(current):
		return false, true
	default:
		return false, false
	}
}

func (c *scriptChecker) exactIterableProvablyEmpty(expr Expression) bool {
	values, exact := c.staticValueExpressionAlternatives(expr)
	if !exact || len(values) == 0 {
		return false
	}
	for _, value := range values {
		array, ok := value.(*ArrayLiteral)
		if !ok || len(array.Elements) != 0 {
			return false
		}
	}
	return true
}

func (c *scriptChecker) exactIterableProvablyNonEmpty(expr Expression) bool {
	values, exact := c.staticValueExpressionAlternatives(expr)
	if !exact || len(values) == 0 {
		return false
	}
	for _, value := range values {
		literal, static := staticLiteralValue(value)
		if !static || literal.Kind() != KindArray || len(literal.Array()) == 0 {
			return false
		}
	}
	return true
}

func (c *scriptChecker) collectRepeatedRegionIvarEffectsFromExpressionWithExpectation(
	expr Expression,
	effects *regionIvarEffects,
	expectation expressionExpectation,
) {
	if expectation.empty() {
		c.collectRepeatedRegionIvarEffectsFromExpression(expr, effects, true)
		return
	}
	if expectation.includesCallable() {
		if _, bindable := c.bareMemberArgumentCallableFact(expr); bindable {
			c.collectRepeatedRegionIvarEffectsFromExpression(expr, effects, false)
			return
		}
		if callableExpr, bindable := bareIdentifierCallableValue(expr); bindable {
			c.collectRepeatedRegionIvarEffectsFromExpression(callableExpr, effects, false)
			return
		}
	}
	switch typed := expr.(type) {
	case *ArrayLiteral:
		elementExpectation, ok := expectation.arrayElementExpectation()
		if !ok {
			break
		}
		for i, element := range typed.Elements {
			c.collectRepeatedRegionIvarEffectsFromExpressionWithExpectation(
				element,
				effects,
				elementExpectation(i, len(typed.Elements)),
			)
			if !c.expressionMayCompleteForBindingWithExpectation(
				element,
				elementExpectation(i, len(typed.Elements)),
			) {
				return
			}
		}
		return
	case *MemberExpr:
		c.collectRepeatedRegionIvarEffectsFromExpression(typed, effects, true)
		return
	}
	c.collectRepeatedRegionIvarEffectsFromExpression(
		expr,
		effects,
		!expectation.includesCallable(),
	)
}

func (c *scriptChecker) collectRepeatedRegionPlainAssignmentTargetIvarEffects(
	target Expression,
	value Expression,
	effects *regionIvarEffects,
) bool {
	switch typed := target.(type) {
	case nil, *ClassVarExpr:
		return true
	case *Identifier:
		c.bindLocalType(typed.Name, c.inferExpressionType(value))
		c.bindExpressionLocalValueFact(typed.Name, value)
		return true
	case *IvarExpr:
		return c.collectRepeatedRegionStoreIvarEffects(
			typed,
			value,
			checkAssignmentReceiverCapture{},
			effects,
		)
	case *MemberExpr:
		c.collectRepeatedRegionIvarEffectsFromExpression(typed.Object, effects, true)
		if !c.expressionMayCompleteForBinding(typed.Object) {
			return false
		}
		receiver, _ := c.assignmentReceiverSnapshot(typed)
		return c.collectRepeatedRegionStoreIvarEffects(typed, value, receiver, effects)
	case *IndexExpr:
		previousEvaluatedFacts := c.evaluatedDestructureFacts
		if previousEvaluatedFacts == nil {
			c.evaluatedDestructureFacts = make(map[Expression]capturedDestructureValueFact)
			defer func() { c.evaluatedDestructureFacts = previousEvaluatedFacts }()
		}
		c.collectRepeatedRegionIvarEffectsFromExpression(typed.Object, effects, true)
		if !c.expressionMayCompleteForBinding(typed.Object) {
			return false
		}
		receiver, _ := c.assignmentReceiverSnapshot(typed)
		for _, index := range typed.Indices {
			c.collectRepeatedRegionIvarEffectsFromExpression(index, effects, true)
			if !c.expressionMayCompleteForBinding(index) {
				return false
			}
			c.captureEvaluatedDestructureFact(index)
		}
		return c.collectRepeatedRegionStoreIvarEffects(typed, value, receiver, effects)
	case *DestructureTarget:
		values := destructureAssignmentExpressions(typed, value)
		for i, element := range typed.Elements {
			var elementValue Expression
			if i < len(values) {
				elementValue = values[i]
			}
			if !c.collectRepeatedRegionPlainAssignmentTargetIvarEffects(
				element.Target,
				elementValue,
				effects,
			) {
				return false
			}
		}
		return true
	default:
		c.collectRepeatedRegionIvarEffectsFromExpression(target, effects, true)
		return c.expressionMayCompleteForBinding(target)
	}
}

func (c *scriptChecker) collectRepeatedRegionStoreIvarEffects(
	target Expression,
	value Expression,
	receiver checkAssignmentReceiverCapture,
	effects *regionIvarEffects,
) bool {
	switch typed := target.(type) {
	case nil, *Identifier, *ClassVarExpr:
		return true
	case *IvarExpr:
		if !c.ivarAssignmentMayComplete(typed, value) {
			return false
		}
		collectRegionIvarWriteTargets(typed, effects)
		return true
	case *MemberExpr, *IndexExpr:
		completed := true
		c.withEvaluatedAssignmentSetterArgumentFacts(target, value, func() {
			selection := c.assignmentSetterScriptDispatch(target, value, receiver)
			if selection.mayRunScript() {
				mergeRegionIvarEffects(effects, c.scriptDispatchIvarEffects(selection))
			}
			completed = c.assignmentSetterMayCompleteWithReceiver(target, value, receiver)
		})
		return completed
	case *DestructureTarget:
		values := destructureAssignmentExpressions(typed, value)
		for i, element := range typed.Elements {
			var elementValue Expression
			if i < len(values) {
				elementValue = values[i]
			}
			if !c.collectRepeatedRegionStoreIvarEffects(
				element.Target,
				elementValue,
				checkAssignmentReceiverCapture{},
				effects,
			) {
				return false
			}
		}
		return true
	default:
		return true
	}
}

func (c *scriptChecker) collectRepeatedRegionIvarEffectsFromIfStatement(
	stmt *IfStmt,
	effects *regionIvarEffects,
) bool {
	if stmt == nil {
		return true
	}
	c.collectRepeatedRegionIvarEffectsFromExpression(stmt.Condition, effects, true)
	if !c.expressionMayCompleteForBinding(stmt.Condition) {
		return false
	}
	mayComplete := false
	truthy, known := staticExpressionTruthiness(stmt.Condition)
	if !known || truthy {
		mayComplete = c.collectRepeatedRegionIvarEffects(stmt.Consequent, effects)
	}
	if known && truthy {
		return mayComplete
	}
	for _, branch := range stmt.ElseIf {
		c.collectRepeatedRegionIvarEffectsFromExpression(branch.Condition, effects, true)
		if !c.expressionMayCompleteForBinding(branch.Condition) {
			return mayComplete
		}
		truthy, known = staticExpressionTruthiness(branch.Condition)
		if !known || truthy {
			mayComplete = c.collectRepeatedRegionIvarEffects(branch.Consequent, effects) ||
				mayComplete
		}
		if known && truthy {
			return mayComplete
		}
	}
	return c.collectRepeatedRegionIvarEffects(stmt.Alternate, effects) || mayComplete
}

func (c *scriptChecker) collectRepeatedRegionIvarEffectsFromIfExpression(
	expr *IfExpr,
	effects *regionIvarEffects,
	autoCall bool,
) {
	if expr == nil {
		return
	}
	c.collectRepeatedRegionIvarEffectsFromExpression(expr.Condition, effects, true)
	if !c.expressionMayCompleteForBinding(expr.Condition) {
		return
	}
	truthy, known := staticExpressionTruthiness(expr.Condition)
	if !known || truthy {
		c.collectRepeatedRegionIvarEffectsFromExpression(expr.Consequent, effects, autoCall)
	}
	if known && truthy {
		return
	}
	for _, branch := range expr.ElseIf {
		c.collectRepeatedRegionIvarEffectsFromExpression(branch.Condition, effects, true)
		if !c.expressionMayCompleteForBinding(branch.Condition) {
			return
		}
		truthy, known = staticExpressionTruthiness(branch.Condition)
		if !known || truthy {
			c.collectRepeatedRegionIvarEffectsFromExpression(branch.Result, effects, autoCall)
		}
		if known && truthy {
			return
		}
	}
	c.collectRepeatedRegionIvarEffectsFromExpression(expr.Alternate, effects, autoCall)
}

func (c *scriptChecker) collectRepeatedRegionIvarEffectsFromExpression(
	expr Expression,
	effects *regionIvarEffects,
	autoCall bool,
) {
	switch typed := expr.(type) {
	case nil, *IntegerLiteral, *FloatLiteral, *StringLiteral, *RegexLiteral,
		*BoolLiteral, *NilLiteral, *SymbolLiteral, *IvarExpr, *ClassVarExpr:
	case *Identifier:
		if autoCall && !c.pureCallArgument(typed) {
			if dispatch, exact := c.implicitSelfIdentifierDispatch(typed); exact {
				if dispatch.mayRunScript() {
					mergeRegionIvarEffects(effects, c.scriptDispatchIvarEffects(dispatch))
				}
			} else {
				effects.unknown = true
			}
		}
	case *ArrayLiteral:
		for _, element := range typed.Elements {
			c.collectRepeatedRegionIvarEffectsFromExpression(element, effects, true)
			if !c.expressionMayCompleteForBinding(element) {
				return
			}
		}
	case *HashLiteral:
		for _, pair := range typed.Pairs {
			c.collectRepeatedRegionIvarEffectsFromExpression(pair.Key, effects, true)
			if !c.expressionMayCompleteForBinding(pair.Key) {
				return
			}
			c.collectRepeatedRegionIvarEffectsFromExpression(pair.Value, effects, true)
			if !c.expressionMayCompleteForBinding(pair.Value) {
				return
			}
		}
	case *CallExpr:
		target, resolved := c.resolveCallable(typed)
		unknownDispatch := false
		implicitSelfCall := false
		if member, ok := typed.Callee.(*MemberExpr); ok {
			c.collectRepeatedRegionIvarEffectsFromExpression(member.Object, effects, true)
			if !c.expressionMayCompleteForBinding(member.Object) {
				return
			}
			if staticNilSafeNavigationCall(typed) {
				return
			}
			unknownDispatch = c.memberCallMayWriteUnknownIvar(typed)
		} else {
			c.collectRepeatedRegionIvarEffectsFromExpression(typed.Callee, effects, false)
			if !c.expressionMayCompleteForBindingWithAuto(typed.Callee, false) {
				return
			}
			unknownDispatch = true
		}
		dynamicCandidates := c.captureDynamicCallCandidates(typed)
		deferForwardedTargets := callResolvesForwardedTargetAfterArguments(
			typed,
			target,
			resolved,
		)
		var dynamicResolution checkDynamicCallResolution
		if !resolved && !deferForwardedTargets {
			dynamicResolution = c.exactDynamicCallTargets(
				typed,
				target,
				false,
				dynamicCandidates,
			)
		}
		if c.callCalleeLookupFails(
			typed,
			target,
			resolved,
			deferForwardedTargets,
			dynamicCandidates,
			dynamicResolution,
		) {
			return
		}
		for _, arg := range typed.Args {
			c.collectRepeatedRegionIvarEffectsFromExpression(arg, effects, true)
			if !c.expressionMayCompleteForBinding(arg) ||
				!c.positionalArgumentExpansionMaySucceed(arg) {
				return
			}
		}
		for _, kwarg := range typed.KwArgs {
			c.collectRepeatedRegionIvarEffectsFromExpression(kwarg.Value, effects, true)
			if !c.expressionMayCompleteForBinding(kwarg.Value) ||
				!c.keywordArgumentExpansionMaySucceed(kwarg) {
				return
			}
		}
		if dispatch, exact := c.implicitSelfCallDispatch(typed); exact {
			implicitSelfCall = true
			if dispatch.mayRunScript() {
				mergeRegionIvarEffects(effects, c.scriptDispatchIvarEffects(dispatch))
			}
		}
		if unknownDispatch && !implicitSelfCall {
			effects.unknown = true
		}
		blockMayRun := c.callMayInvokeSuppliedBlock(typed)
		if blockMayRun && typed.Block != nil {
			c.collectRepeatedRegionIvarEffectsFromBlock(typed.Block, effects)
		}
	case *MemberExpr:
		c.collectRepeatedRegionIvarEffectsFromExpression(typed.Object, effects, true)
		if !c.expressionMayCompleteForBinding(typed.Object) {
			return
		}
		c.captureAssignmentReceiver(typed)
		if autoCall {
			if staticNilSafeNavigationMember(typed) {
				return
			}
			if c.memberDispatchEffect(typed) == effectUnknown {
				effects.unknown = true
			}
		}
	case *ScopeExpr:
		c.collectRepeatedRegionIvarEffectsFromExpression(typed.Object, effects, true)
	case *IndexExpr:
		previousEvaluatedFacts := c.evaluatedDestructureFacts
		if previousEvaluatedFacts == nil {
			c.evaluatedDestructureFacts = make(map[Expression]capturedDestructureValueFact)
			defer func() { c.evaluatedDestructureFacts = previousEvaluatedFacts }()
		}
		c.collectRepeatedRegionIvarEffectsFromExpression(typed.Object, effects, true)
		if !c.expressionMayCompleteForBinding(typed.Object) {
			return
		}
		c.captureEvaluatedDestructureFactOnce(typed.Object)
		c.captureAssignmentReceiver(typed)
		dispatchType := c.instanceDispatchReceiverType(
			typed.Object,
			c.inferExpressionType(typed.Object),
		)
		for _, index := range typed.Indices {
			c.collectRepeatedRegionIvarEffectsFromExpression(index, effects, true)
			if !c.expressionMayCompleteForBinding(index) {
				return
			}
			c.captureEvaluatedDestructureFactOnce(index)
		}
		c.withEvaluatedDestructureArgumentFacts(typed.Indices, func() {
			dispatch := c.indexScriptDispatch(typed, dispatchType)
			if dispatch.mayRunScript() {
				mergeRegionIvarEffects(effects, c.scriptDispatchIvarEffects(dispatch))
			}
		})
		c.withEvaluatedDestructureArgumentFacts(
			append([]Expression{typed.Object}, typed.Indices...),
			func() { c.captureEvaluatedDestructureFact(typed) },
		)
	case *DestructureTarget:
		for _, element := range typed.Elements {
			c.collectRepeatedRegionIvarEffectsFromExpression(element.Target, effects, false)
			if !c.expressionMayCompleteForBindingWithAuto(element.Target, false) {
				return
			}
		}
	case *SplatArg:
		c.collectRepeatedRegionIvarEffectsFromExpression(typed.Value, effects, true)
	case *TypeLiteral:
		c.collectRepeatedRegionIvarEffectsFromExpression(typed.Fallback, effects, true)
	case *UnaryExpr:
		c.collectRepeatedRegionIvarEffectsFromExpression(typed.Right, effects, true)
	case *BinaryExpr:
		previousEvaluatedFacts := c.evaluatedDestructureFacts
		if previousEvaluatedFacts == nil {
			c.evaluatedDestructureFacts = make(map[Expression]capturedDestructureValueFact)
			defer func() { c.evaluatedDestructureFacts = previousEvaluatedFacts }()
		}
		c.collectRepeatedRegionIvarEffectsFromExpression(typed.Left, effects, true)
		if !c.expressionMayCompleteForBinding(typed.Left) {
			return
		}
		if binaryRightMayEvaluate(typed) {
			c.collectRepeatedRegionIvarEffectsFromExpression(typed.Right, effects, true)
			if !c.expressionMayCompleteForBinding(typed.Right) {
				return
			}
			c.captureEvaluatedDestructureFactOnce(typed.Right)
		}
		c.withEvaluatedDestructureArgumentFacts([]Expression{typed.Right}, func() {
			dispatch := c.binaryScriptDispatch(typed, c.inferExpressionType(typed.Left))
			if dispatch.mayRunScript() {
				mergeRegionIvarEffects(effects, c.scriptDispatchIvarEffects(dispatch))
			}
		})
	case *ConditionalExpr:
		c.collectRepeatedRegionIvarEffectsFromExpression(typed.Condition, effects, true)
		if !c.expressionMayCompleteForBinding(typed.Condition) {
			return
		}
		truthy, known := staticExpressionTruthiness(typed.Condition)
		if !known || truthy {
			c.collectRepeatedRegionIvarEffectsFromExpression(typed.Consequent, effects, autoCall)
		}
		if !known || !truthy {
			c.collectRepeatedRegionIvarEffectsFromExpression(typed.Alternate, effects, autoCall)
		}
	case *RescueExpr:
		c.collectRepeatedRegionIvarEffectsFromExpression(typed.Body, effects, autoCall)
		if expressionProvenNonRaising(typed.Body) {
			return
		}
		if errorKind, exact := c.staticallyRaisedExpressionErrorKind(typed.Body); exact &&
			!staticErrorKindMatchesRescue(errorKind, nil) {
			return
		}
		c.collectRepeatedRegionIvarEffectsFromExpression(typed.Fallback, effects, autoCall)
	case *IfExpr:
		c.collectRepeatedRegionIvarEffectsFromIfExpression(typed, effects, autoCall)
	case *RangeExpr:
		c.collectRepeatedRegionIvarEffectsFromExpression(typed.Start, effects, true)
		if !c.expressionMayCompleteForBinding(typed.Start) ||
			!c.rangeEndpointConversionMaySucceed(typed.Start) {
			return
		}
		c.collectRepeatedRegionIvarEffectsFromExpression(typed.End, effects, true)
	case *CaseExpr:
		c.collectRepeatedRegionIvarEffectsFromExpression(typed.Target, effects, true)
		if !c.expressionMayCompleteForBinding(typed.Target) {
			return
		}
		if result, known := staticCaseExpressionResult(typed); known {
			c.collectRepeatedRegionIvarEffectsFromExpression(result, effects, autoCall)
			return
		}
		for _, clause := range typed.Clauses {
			for _, value := range clause.Values {
				c.collectRepeatedRegionIvarEffectsFromExpression(value.Expr, effects, true)
				if !c.expressionMayCompleteForBinding(value.Expr) ||
					!c.caseWhenSplatExpansionMaySucceed(value.Expr, value.Splat) {
					return
				}
				c.collectRepeatedRegionIvarEffectsFromExpression(
					clause.Result,
					effects,
					autoCall,
				)
			}
		}
		c.collectRepeatedRegionIvarEffectsFromExpression(typed.ElseExpr, effects, autoCall)
	case *BlockLiteral:
		// Evaluating a lambda only constructs its closure. Call sites and
		// dispatches that may invoke it account for those effects separately.
	case *YieldExpr:
		if c.yieldBlockKnownAbsent() {
			return
		}
		for _, arg := range typed.Args {
			c.collectRepeatedRegionIvarEffectsFromExpression(arg, effects, true)
			if !c.expressionMayCompleteForBinding(arg) {
				return
			}
		}
		effects.unknown = true
	case *InterpolatedString:
		for _, part := range typed.Parts {
			if exprPart, ok := part.(StringExpr); ok {
				c.collectRepeatedRegionIvarEffectsFromExpression(exprPart.Expr, effects, true)
				if !c.expressionMayCompleteForBinding(exprPart.Expr) {
					return
				}
			}
		}
	case *InterpolatedSymbol:
		for _, part := range typed.Parts {
			if exprPart, ok := part.(StringExpr); ok {
				c.collectRepeatedRegionIvarEffectsFromExpression(exprPart.Expr, effects, true)
				if !c.expressionMayCompleteForBinding(exprPart.Expr) {
					return
				}
			}
		}
	case *IfStmt, *ForStmt, *WhileStmt, *UntilStmt, *TryStmt:
		c.collectRepeatedRegionIvarEffects([]Statement{typed.(Statement)}, effects)
	}
}

func (c *scriptChecker) blockLiteralTypeMayNormalize(
	valueType *TypeExpr,
	ty *TypeExpr,
) bool {
	if ty == nil {
		return true
	}
	if blockLiteralTypeAcceptsAny(ty) {
		return true
	}
	if valueType == nil || valueType.Kind == TypeAny || valueType.Kind == TypeUnknown {
		return true
	}
	if arms, exact := typeExprArms(valueType, 0); exact && len(arms) > 1 {
		for _, arm := range arms {
			if c.blockLiteralTypeMayNormalize(arm, ty) {
				return true
			}
		}
		return false
	}
	resolve := c.checkNamedTypeResolver()
	for _, option := range blockLiteralNormalizationTypeOptions(ty) {
		if empty, constrained := c.blockLiteralEmptyContainerNormalization(
			valueType,
			option,
		); constrained {
			if empty != nil {
				return true
			}
			continue
		}
		if option.Kind == TypeAny {
			return true
		}
		if valueType.Kind == TypeEnum || option.Kind == TypeEnum {
			if valueType.Kind == TypeSymbol && option.Kind == TypeEnum {
				match, resolved := resolve(option)
				if resolved && match.enum != nil {
					return true
				}
			}
			if valueType.Kind == TypeEnum && option.Kind == TypeEnum &&
				!typeExprsDisjoint(valueType, option, resolve) {
				return true
			}
			continue
		}
		if !typeExprsDisjoint(valueType, option, resolve) {
			return true
		}
	}
	return false
}

func (c *scriptChecker) blockLiteralTypeMustNormalize(
	valueType *TypeExpr,
	ty *TypeExpr,
) bool {
	if ty == nil {
		return true
	}
	if blockLiteralTypeAcceptsAny(ty) {
		return true
	}
	if valueType == nil || valueType.Kind == TypeAny || valueType.Kind == TypeUnknown {
		return false
	}
	if arms, exact := typeExprArms(valueType, 0); exact && len(arms) > 1 {
		for _, arm := range arms {
			if !c.blockLiteralTypeMustNormalize(arm, ty) {
				return false
			}
		}
		return true
	}
	options := blockLiteralNormalizationTypeOptions(ty)
	if valueType.Kind == TypeSymbol {
		for _, option := range options {
			if option.Kind == TypeAny || option.Kind == TypeSymbol {
				return true
			}
		}
		return false
	}
	if valueType.Kind == TypeEnum {
		nonSymbol := make([]*TypeExpr, 0, len(options))
		for _, option := range options {
			if option.Kind != TypeSymbol {
				nonSymbol = append(nonSymbol, option)
			}
		}
		return typeExprSatisfies(
			valueType,
			unionTypeExprs(nonSymbol...),
			c.checkNamedTypeResolver(),
		)
	}
	return typeExprSatisfies(valueType, ty, c.checkNamedTypeResolver())
}

func blockLiteralTypeAcceptsAny(ty *TypeExpr) bool {
	if ty == nil {
		return true
	}
	if ty.Kind == TypeAny {
		return true
	}
	if ty.Kind != TypeUnion {
		return false
	}
	return slices.ContainsFunc(ty.Union, blockLiteralTypeAcceptsAny)
}

func (c *scriptChecker) blockLiteralNormalizedType(
	valueType *TypeExpr,
	ty *TypeExpr,
) *TypeExpr {
	if ty == nil {
		return valueType
	}
	if arms, exact := typeExprArms(valueType, 0); exact && len(arms) > 1 {
		normalized := make([]*TypeExpr, 0, len(arms))
		for _, arm := range arms {
			if next := c.blockLiteralNormalizedType(arm, ty); next != nil {
				normalized = append(normalized, next)
			}
		}
		return unionTypeExprs(normalized...)
	}
	options := blockLiteralNormalizationTypeOptions(ty)
	normalized := make([]*TypeExpr, 0, len(options))
	resolve := c.checkNamedTypeResolver()
	for _, option := range options {
		if empty, constrained := c.blockLiteralEmptyContainerNormalization(
			valueType,
			option,
		); constrained {
			if empty != nil {
				normalized = append(normalized, empty)
			}
			continue
		}
		if valueType != nil && !c.blockLiteralTypeMayNormalize(valueType, option) {
			continue
		}
		if valueType == nil {
			if option.Kind == TypeAny {
				return nil
			}
			normalized = append(normalized, option)
			continue
		}
		if option.Kind == TypeEnum && valueType.Kind == TypeSymbol {
			if match, resolved := resolve(option); resolved && match.enum != nil {
				normalized = append(normalized, option)
				continue
			}
		}
		if typeExprSatisfies(valueType, option, resolve) {
			normalized = append(normalized, valueType)
		} else {
			normalized = append(normalized, option)
		}
	}
	return unionTypeExprs(normalized...)
}

// blockLiteralEmptyContainerNormalization recognizes typed hashes whose
// entries can never satisfy a hash or closed-shape option. Such an option can
// accept only the empty hash (and only when the shape has no required field);
// retaining that fact prevents a later nested target from inventing fields.
func (c *scriptChecker) blockLiteralEmptyContainerNormalization(
	valueType *TypeExpr,
	option *TypeExpr,
) (*TypeExpr, bool) {
	if valueType == nil || option == nil ||
		valueType.Kind != TypeHash || len(valueType.TypeArgs) != 2 {
		return nil, false
	}
	inputKey := valueType.TypeArgs[0]
	inputValue := valueType.TypeArgs[1]
	constrained := false
	switch option.Kind {
	case TypeHash:
		if len(option.TypeArgs) != 2 {
			return nil, false
		}
		constrained = !c.blockLiteralTypeMayNormalize(inputKey, option.TypeArgs[0]) ||
			!c.blockLiteralTypeMayNormalize(inputValue, option.TypeArgs[1])
	case TypeShape:
		if option.Open {
			return nil, false
		}
		shapeKey := unionTypeExprs(checkTypeString, checkTypeSymbol)
		keyCompatible := c.blockLiteralTypeMayNormalize(inputKey, shapeKey)
		valueCompatible := false
		for _, fieldType := range option.Shape {
			if c.blockLiteralTypeMayNormalize(
				inputValue,
				shapeFieldValueType(fieldType),
			) {
				valueCompatible = true
				break
			}
		}
		constrained = !keyCompatible || !valueCompatible
		if constrained {
			for _, fieldType := range option.Shape {
				if !shapeFieldOptional(fieldType) {
					return nil, true
				}
			}
		}
	}
	if !constrained {
		return nil, false
	}
	return &TypeExpr{
		Kind:  TypeShape,
		Name:  shapeKeysStringMarker,
		Shape: map[string]*TypeExpr{},
	}, true
}

func blockLiteralNormalizationTypeOptions(ty *TypeExpr) []*TypeExpr {
	if ty == nil {
		return nil
	}
	if ty.Kind == TypeUnion {
		var options []*TypeExpr
		for _, option := range unionNormalizationOrder(ty.Union) {
			options = append(options, blockLiteralNormalizationTypeOptions(option)...)
		}
		return options
	}
	if !ty.Nullable {
		return []*TypeExpr{ty}
	}
	nonNil := *ty
	nonNil.Nullable = false
	return []*TypeExpr{&nonNil, checkTypeNil}
}

func collectRegionIvarWriteTargets(target Expression, effects *regionIvarEffects) {
	switch typed := target.(type) {
	case *IvarExpr:
		if effects.writes == nil {
			effects.writes = make(map[string]struct{})
		}
		effects.writes[typed.Name] = struct{}{}
	case *DestructureTarget:
		for _, element := range typed.Elements {
			collectRegionIvarWriteTargets(element.Target, effects)
		}
	}
}

// maxRepeatedRegionBlockWalks caps how often one lambda body is walked for
// ivar effects per outermost walk of that body. The first walk uses the
// caller's facts; the second runs under the state the recursive call left
// behind and so reaches the writes the recursion enables; a third adds nothing
// the second did not reach.
//
// maxSummaryYieldBlockWalks allows the same two walks but still restores its
// count at each call site rather than spending it, and cannot be changed to
// match this one. That walk re-checks the body with the full checker, which
// prunes on inferred facts, so a yield only a later site enables is reachable
// only by re-entering at that site; spending the count there stops reaching it
// and invents a diagnostic on a body whose yield does run.
const maxRepeatedRegionBlockWalks = 2

// collectRepeatedRegionIvarEffectsFromBlock unions in the ivar effects a
// lambda body can produce each time the region repeats. A body reachable from
// itself (`h = -> { h.call }; h.call`) would otherwise re-enter its own
// statements once per nested call and never finish. Re-entry collects the
// writes the recursion can reach rather than declaring every ivar unknown, so
// recursion that writes no ivar leaves exact facts standing.
//
// A walk spends its count rather than borrowing it: a nested walk leaves the
// count raised for the rest of the outermost walk instead of restoring it on
// return. The budget then bounds the whole walk, so a body holding N recursive
// call sites costs 2N statement visits. Restoring the count per site made every
// site start its own re-entry walk over the same N statements, which is
// quadratic in a source the checker accepts.
func (c *scriptChecker) collectRepeatedRegionIvarEffectsFromBlock(
	block *BlockLiteral,
	effects *regionIvarEffects,
) {
	if block == nil {
		return
	}
	walks := c.repeatedRegionBlockWalks[block]
	if walks >= maxRepeatedRegionBlockWalks {
		effects.reentrant = true
		return
	}
	if c.repeatedRegionBlockWalks == nil {
		c.repeatedRegionBlockWalks = make(map[*BlockLiteral]int)
	}
	c.repeatedRegionBlockWalks[block] = walks + 1
	if walks == 0 {
		defer delete(c.repeatedRegionBlockWalks, block)
	}
	popScope := c.pushBlockCheckScope(block)
	defer popScope()
	if walks > 0 {
		effects.reentrant = true
	}
	for _, name := range block.ImplicitParams {
		c.bindLocalTypeInCurrentFrame(name, nil)
		c.bindLocalClassValue(name, "")
	}
	for _, param := range block.Params {
		c.collectRepeatedRegionIvarEffectsFromExpression(param.DefaultVal, effects, true)
		c.bindParamLocalType(param)
	}
	bodyBindings := make(map[string]struct{})
	collectLocalBindings(block.Body, bodyBindings)
	currentScope := c.scopes[len(c.scopes)-1]
	for name := range bodyBindings {
		if _, blockBound := currentScope[name]; !blockBound {
			continue
		}
		c.bindLocalTypeInCurrentFrame(name, nil)
		c.bindLocalClassValue(name, "")
	}
	c.collectRepeatedRegionIvarEffects(block.Body, effects)
}

// widenRepeatedRegionIvarFacts applies only the initializer-ivar uncertainty
// the region can create. Direct ivar targets widen individually; a dispatch,
// setter, or yield whose effects are unclassified widens every unset ivar.
func (c *scriptChecker) widenRepeatedRegionIvarFacts(
	statements []Statement,
	repeated ...Expression,
) {
	scopeState := c.snapshotScopeState()
	bindings := make(map[string]struct{})
	collectLocalBindings(statements, bindings)
	for name := range bindings {
		c.bindLocalType(name, nil)
		c.bindLocalClassValue(name, "")
	}
	var effects regionIvarEffects
	c.collectRepeatedRegionIvarEffects(statements, &effects)
	for _, expr := range repeated {
		c.collectRepeatedRegionIvarEffectsFromExpression(expr, &effects, true)
	}
	c.restoreScopeState(scopeState)
	c.widenRegionIvarFacts(effects)
}

func (c *scriptChecker) widenRepeatedLoopIvarFacts(
	condition Expression,
	statements []Statement,
) {
	if loopBodyMayReachBackedge(statements) {
		c.widenRepeatedRegionIvarFacts(statements, condition)
		return
	}
	c.widenRepeatedRegionIvarFacts(statements)
}

func (c *scriptChecker) widenRepeatedRegionBlockIvarFacts(block *BlockLiteral) {
	scopeState := c.snapshotScopeState()
	var effects regionIvarEffects
	c.collectRepeatedRegionIvarEffectsFromBlock(block, &effects)
	c.restoreScopeState(scopeState)
	c.widenRegionIvarFacts(effects)
}

func (c *scriptChecker) widenRegionIvarFacts(effects regionIvarEffects) {
	if effects.unknown {
		c.widenUnsetInstanceIvarFacts()
		return
	}
	for name := range effects.writes {
		c.widenUnsetInstanceIvarFact(name)
	}
}

// degradeMutationCandidates clears container-typed locals a region may
// mutate through member dispatch or call arguments; scalar receivers keep
// their facts (immutable kinds cannot be mutated in place).
func (c *scriptChecker) degradeMutationCandidates(
	statements []Statement,
	names map[string]struct{},
	repeated ...Expression,
) {
	var sites []Expression
	c.collectMutationCandidateRoots(statements, &sites)
	for _, expr := range repeated {
		c.collectMutationCandidateRootsFromExpression(expr, &sites)
	}
	for _, site := range sites {
		if name, ok := c.shovelEscapeStaticValueTarget(site); ok {
			c.poisonLocalStaticValues(name)
		}
		if name, ok := c.escapePoisonTarget(site); ok {
			names[name] = struct{}{}
		}
	}
}

// degradeLocalTypesForBindings resets to unknown every local the statements
// (plus any extra binding targets) may assign. It runs before regions whose
// execution count the checker cannot know — loop and block bodies — so a
// first-iteration fact cannot leak into the region or survive it.
func (c *scriptChecker) degradeLocalTypesForBindings(statements []Statement, extraTargets ...Expression) {
	c.degradeLocalTypesForRegion(statements, nil, extraTargets...)
}

func (c *scriptChecker) degradeLocalTypesForRepeatedLoop(
	condition Expression,
	statements []Statement,
) {
	var repeated []Expression
	if loopBodyMayReachBackedge(statements) {
		repeated = []Expression{condition}
	}
	c.degradeLocalTypesForRegion(statements, repeated)
}

func (c *scriptChecker) degradeLocalTypesForRegion(
	statements []Statement,
	repeated []Expression,
	extraTargets ...Expression,
) {
	names := make(map[string]struct{})
	collectLocalBindings(statements, names)
	for _, frame := range c.localTypes {
		for name := range frame {
			if statementsMayAssignName(statements, name) {
				names[name] = struct{}{}
				continue
			}
			for _, expr := range repeated {
				if expressionMayRunBlockLiteralAssigning(expr, name) {
					names[name] = struct{}{}
					break
				}
			}
		}
	}
	mutatedContainers := make(map[string]struct{})
	collectMutatedContainerRoots(statements, mutatedContainers)
	for name := range mutatedContainers {
		ty := c.localTypeFor(name)
		if ty == nil || c.typeExprHasContainerArm(ty) {
			names[name] = struct{}{}
		}
	}
	c.degradeMutationCandidates(statements, names, repeated...)
	for _, target := range extraTargets {
		if target != nil {
			collectBindingTarget(target, names)
		}
	}
	for name := range names {
		c.preserveContainerBindingBeforeDegrade(name)
		c.bindLocalType(name, nil)
		c.bindLocalClassValue(name, "")
	}
}

func (c *scriptChecker) snapshotLocalTypes() []checkTypeFrame {
	if len(c.localTypes) == 0 {
		return nil
	}
	state := make([]checkTypeFrame, len(c.localTypes))
	for i, frame := range c.localTypes {
		state[i] = cloneCheckTypeFrame(frame)
	}
	return state
}

func (c *scriptChecker) restoreLocalTypes(state []checkTypeFrame) {
	if len(state) == 0 {
		c.localTypes = nil
		return
	}
	c.localTypes = make([]checkTypeFrame, len(state))
	for i, frame := range state {
		c.localTypes[i] = cloneCheckTypeFrame(frame)
	}
}

func (c *scriptChecker) snapshotLocalClassValues() []checkClassValueFrame {
	if len(c.localClassValues) == 0 {
		return nil
	}
	state := make([]checkClassValueFrame, len(c.localClassValues))
	for i, frame := range c.localClassValues {
		state[i] = cloneCheckClassValueFrame(frame)
	}
	return state
}

func (c *scriptChecker) restoreLocalClassValues(state []checkClassValueFrame) {
	if len(state) == 0 {
		c.localClassValues = nil
		return
	}
	c.localClassValues = make([]checkClassValueFrame, len(state))
	for i, frame := range state {
		c.localClassValues[i] = cloneCheckClassValueFrame(frame)
	}
}

func (c *scriptChecker) mergeLocalClassValueStates(states []checkScopeState) {
	if len(states) == 0 {
		return
	}
	for i := range c.localClassValues {
		if i >= len(states[0].classValues) {
			continue
		}
		common := cloneCheckClassValueFrame(states[0].classValues[i])
		for _, state := range states[1:] {
			if i >= len(state.classValues) {
				clear(common)
				break
			}
			for name, fact := range common {
				other, ok := state.classValues[i][name]
				if !ok {
					delete(common, name)
					continue
				}
				fact, ok = c.mergeLocalValueFacts(fact, other)
				if !ok {
					delete(common, name)
					continue
				}
				common[name] = fact
			}
		}
		c.localClassValues[i] = common
	}
}

func (c *scriptChecker) mergeLocalValueFacts(
	left checkLocalValueFact,
	right checkLocalValueFact,
) (checkLocalValueFact, bool) {
	var merged checkLocalValueFact
	if len(left.classNames) > 0 && len(right.classNames) > 0 {
		merged.classNames = normalizeCheckClassNames(append(left.classNames, right.classNames...))
	}
	if len(left.instanceOrigins) > 0 && len(right.instanceOrigins) > 0 {
		merged.instanceOrigins = normalizeCheckExpressionIdentities(
			append(left.instanceOrigins, right.instanceOrigins...),
		)
	}
	if len(left.callables) > 0 && len(right.callables) > 0 {
		merged.callables = normalizeCheckCallables(append(left.callables, right.callables...))
	}
	if len(left.staticVals) > 0 && len(right.staticVals) > 0 {
		merged.staticVals = c.normalizeCheckStaticValues(append(left.staticVals, right.staticVals...))
	}
	if left.keywordSplatFails && right.keywordSplatFails {
		merged.keywordSplatFails = true
		switch {
		case len(left.invalidKeywordSplatKeys) == 0 ||
			len(right.invalidKeywordSplatKeys) == 0:
		case len(left.invalidKeywordSplatKeys) > 0 &&
			len(right.invalidKeywordSplatKeys) > 0:
			merged.invalidKeywordSplatKeys = make(map[string]struct{})
			for key := range left.invalidKeywordSplatKeys {
				if _, common := right.invalidKeywordSplatKeys[key]; common {
					merged.invalidKeywordSplatKeys[key] = struct{}{}
				}
			}
			if len(merged.invalidKeywordSplatKeys) == 0 {
				merged.keywordSplatFails = false
				merged.invalidKeywordSplatKeys = nil
			}
		}
	}
	exact := len(merged.classNames) > 0 ||
		len(merged.instanceOrigins) > 0 ||
		len(merged.callables) > 0 ||
		len(merged.staticVals) > 0 ||
		merged.keywordSplatFails
	return merged, exact
}

func normalizeCheckClassNames(names []string) []string {
	if len(names) == 0 {
		return nil
	}
	normalized := append([]string(nil), names...)
	slices.Sort(normalized)
	out := normalized[:0]
	for _, name := range normalized {
		if len(out) == 0 || out[len(out)-1] != name {
			out = append(out, name)
		}
	}
	return out
}

func normalizeCheckExpressionIdentities(expressions []Expression) []Expression {
	if len(expressions) == 0 {
		return nil
	}
	normalized := make([]Expression, 0, len(expressions))
	for _, candidate := range expressions {
		if candidate == nil {
			continue
		}
		duplicate := slices.Contains(normalized, candidate)
		if !duplicate {
			normalized = append(normalized, candidate)
		}
	}
	return normalized
}

func (c *scriptChecker) normalizeCheckStaticValues(values []Expression) []Expression {
	const maxStaticValueAlternatives = 32
	if len(values) == 0 {
		return nil
	}
	normalized := make([]Expression, 0, len(values))
	for _, candidate := range values {
		if !c.checkStaticValueCandidate(candidate) {
			continue
		}
		duplicate := false
		for _, existing := range normalized {
			if sameCheckStaticValueCandidate(candidate, existing) {
				duplicate = true
				break
			}
		}
		if !duplicate {
			normalized = append(normalized, candidate)
			if len(normalized) > maxStaticValueAlternatives {
				return nil
			}
		}
	}
	return normalized
}

func sameCheckStaticValueCandidate(left, right Expression) bool {
	if left == right {
		return true
	}
	leftValue, leftStatic := staticLiteralValue(left)
	rightValue, rightStatic := staticLiteralValue(right)
	if !leftStatic || !rightStatic ||
		staticLiteralHasMutableIdentity(left) ||
		staticLiteralHasMutableIdentity(right) {
		return false
	}
	return reflect.DeepEqual(leftValue, rightValue)
}

func staticLiteralHasMutableIdentity(expr Expression) bool {
	switch expr.(type) {
	case *ArrayLiteral, *HashLiteral:
		return true
	default:
		return false
	}
}

func normalizeCheckCallables(fns []*ScriptFunction) []*ScriptFunction {
	if len(fns) == 0 {
		return nil
	}
	normalized := append([]*ScriptFunction(nil), fns...)
	slices.SortFunc(normalized, func(a, b *ScriptFunction) int {
		return cmp.Compare(reflect.ValueOf(a).Pointer(), reflect.ValueOf(b).Pointer())
	})
	out := normalized[:0]
	for _, fn := range normalized {
		if len(out) == 0 || out[len(out)-1] != fn {
			out = append(out, fn)
		}
	}
	return out
}

// applyLoopEntryTypeRefinements overlays only facts changed by a condition
// outcome. Loop bodies use it after degrading their assignments so the first
// entry's proven scalar refinement remains visible without restoring
// unrelated first-iteration facts or container interiors that later
// iterations may mutate. The degraded state still survives the loop.
func (c *scriptChecker) applyLoopEntryTypeRefinements(base, refined []checkTypeFrame) {
	for i, refinedFrame := range refined {
		if i >= len(base) || i >= len(c.localTypes) {
			continue
		}
		for name, refinedType := range refinedFrame {
			baseType, ok := base[i][name]
			if !ok || refinedType == nil || refinedType == baseType ||
				c.typeExprHasContainerArm(refinedType) {
				continue
			}
			if c.localTypes[i] == nil {
				c.localTypes[i] = make(checkTypeFrame)
			}
			c.localTypes[i][name] = refinedType
		}
	}
}

// mergeLocalTypeStates joins the branch type states into the live frames:
// each local becomes the union of its type on every fall-through path. A path
// that never bound a name contributes nil (branch-assigned locals are
// predeclared as nil at runtime) when the name is new, or the base fact when
// the name predates the branch.
func (c *scriptChecker) mergeLocalTypeStates(base checkScopeState, states []checkScopeState) {
	if len(states) == 0 {
		return
	}
	for i := range c.localTypes {
		names := make(map[string]struct{})
		for _, state := range states {
			if i < len(state.types) {
				for name := range state.types[i] {
					names[name] = struct{}{}
				}
			}
		}
		for name := range names {
			var baseTy *TypeExpr
			inBase := false
			if i < len(base.types) {
				baseTy, inBase = base.types[i][name]
			}
			var merged *TypeExpr
			known := true
			for _, state := range states {
				arm := checkTypeNil
				if inBase {
					arm = baseTy
				}
				if i < len(state.types) {
					if ty, ok := state.types[i][name]; ok {
						arm = ty
					}
				}
				if arm == nil {
					known = false
					break
				}
				if merged == nil {
					merged = arm
					continue
				}
				merged = unionTypeExprs(merged, arm)
				if merged == nil {
					known = false
					break
				}
			}
			if !known {
				merged = nil
			}
			if c.localTypes[i] == nil {
				c.localTypes[i] = make(checkTypeFrame)
			}
			c.localTypes[i][name] = merged
		}
	}
}

// --- assignment inference ---

// inferAssignStatementTypes updates the local type environment for an
// assignment and reports a reassignment that contradicts the local's known
// type (ADR-004: sequential reassignment to a conflicting type is an error).
// assignmentReceiverFact carries the target local's declared bound from before
// the value expression walked. Plain assignment selects the receiver after
// the value, but ordinary escapes cannot rebind a caller local; the caller
// clears this fact only when an inline block can rebind it. Nil defers to the
// current state.
type logicalAssignmentTargetFact struct {
	current            *TypeExpr
	priorAliasTransfer checkContainerAliasTransfer
	rhsReachable       bool
	known              bool
}

func (c *scriptChecker) inferAssignStatementTypes(
	function string,
	stmt *AssignStmt,
	assignmentReceiverFact *TypeExpr,
	logicalTargetFact *logicalAssignmentTargetFact,
) {
	switch target := stmt.Target.(type) {
	case *Identifier:
		var aliasTransfer checkContainerAliasTransfer
		var valueAliasTransfer map[string]struct{}
		if stmt.Operator == tokenNone {
			aliasTransfer = c.captureContainerAliasTransfer(stmt.Value)
			valueAliasTransfer = c.captureValueAliasTransfer(stmt.Value)
		}
		var priorLogicalAliasTransfer checkContainerAliasTransfer
		mayRebind := true
		if stmt.Operator == tokenOrAssign || stmt.Operator == tokenAndAssign {
			if logicalTargetFact == nil {
				mayRebind = c.logicalAssignmentRHSReachable(
					target.Name,
					stmt.Operator,
					c.localTypeFor(target.Name),
				)
				if stmt.Operator == tokenOrAssign && mayRebind {
					priorLogicalAliasTransfer = c.captureContainerAliasTransfer(target)
				}
			} else {
				mayRebind = !logicalTargetFact.known || logicalTargetFact.rhsReachable
				priorLogicalAliasTransfer = logicalTargetFact.priorAliasTransfer
			}
		}
		if logicalTargetFact != nil &&
			logicalTargetFact.known &&
			logicalTargetFact.rhsReachable {
			valueAliasTransfer = c.captureValueAliasTransfer(stmt.Value)
		}
		if mayRebind {
			c.advanceLocalBindingGeneration(target.Name)
		}
		current := c.localTypeFor(target.Name)
		next := c.inferExpressionType(stmt.Value)
		if stmt.Operator == tokenOrAssign || stmt.Operator == tokenAndAssign {
			if logicalTargetFact == nil {
				rhsReachable := c.logicalAssignmentRHSReachable(target.Name, stmt.Operator, current)
				c.bindLocalType(target.Name, logicalAssignmentFact(stmt.Operator, current, next))
				c.bindLogicalAssignmentValueFact(target.Name, stmt.Operator, current, stmt.Value)
				c.applyPossibleContainerAliasTransfer(target.Name, priorLogicalAliasTransfer)
				if rhsReachable {
					c.linkContainerAssignmentAlias(target.Name, stmt.Value, next)
					if next == nil || c.typeExprHasContainerArm(next) {
						for _, root := range c.containerAliasRoots(stmt.Value) {
							c.linkContainerAlias(target.Name, root)
						}
					}
				}
				return
			}
			current = logicalTargetFact.current
			rhsReachable := logicalTargetFact.rhsReachable
			known := logicalTargetFact.known
			result := logicalAssignmentFact(stmt.Operator, current, next)
			if known {
				result = current
				if rhsReachable {
					result = next
				}
			}
			c.bindLocalType(target.Name, result)
			if known {
				if rhsReachable {
					c.bindExpressionLocalValueFact(target.Name, stmt.Value)
					c.applyValueAliasTransfer(target.Name, valueAliasTransfer)
				}
			} else {
				c.bindLocalClassValue(target.Name, "")
			}
			if stmt.Operator == tokenOrAssign && !known {
				c.applyPossibleContainerAliasTransfer(target.Name, priorLogicalAliasTransfer)
			}
			if rhsReachable {
				if known {
					c.linkContainerAssignmentAlias(target.Name, stmt.Value, next)
				}
				if next == nil || c.typeExprHasContainerArm(next) {
					for _, root := range c.containerAliasRoots(stmt.Value) {
						c.linkContainerAlias(target.Name, root)
					}
				}
			}
			return
		}
		if stmt.Operator != tokenNone {
			outcome := c.binaryOperationOutcome(stmt.Operator, current, next)
			if outcome.invalid {
				c.add(function, stmt.Pos(), "unsupported %s operands %s and %s",
					binaryOperatorNoun(stmt.Operator), formatTypeExpr(current), formatTypeExpr(next))
			}
			c.bindLocalType(target.Name, outcome.result)
			c.bindLocalClassValue(target.Name, "")
			return
		}
		if c.reassignmentConflicts(current, next) {
			c.add(function, stmt.Pos(), "reassignment of %s expected %s, got %s",
				target.Name, formatTypeExpr(current), formatTypeExpr(next))
		}
		c.bindLocalType(target.Name, next)
		c.bindExpressionLocalValueFact(target.Name, stmt.Value)
		c.applyValueAliasTransfer(target.Name, valueAliasTransfer)
		c.applyContainerAliasTransfer(target.Name, aliasTransfer)
		c.linkContainerAssignmentAlias(target.Name, stmt.Value, next)
		c.bindContainerSelectionIdentity(target.Name, stmt.Value)
		if next == nil || c.typeExprHasContainerArm(next) {
			for _, root := range c.containerAliasRoots(stmt.Value) {
				c.linkContainerAlias(target.Name, root)
			}
		}
	case *IvarExpr:
		c.inferIvarAssignStatementTypes(function, stmt, target, logicalTargetFact)
	case *DestructureTarget:
		valueFacts := c.captureDestructureValueFacts(target, stmt.Value)
		valueFacts = c.flattenCapturedDestructureValueFacts(valueFacts)
		c.bindCapturedDestructureValueFacts(valueFacts)
		c.inferDestructureIvarWrites(function, valueFacts)
	case *IndexExpr:
		// An index write mutates the container in place; a direct write
		// against a declared array<T>, hash<K, V>, or shape fact is checked
		// and may preserve or refine the fact. Exact literal-array facts
		// advance independently; every fact neither model preserves is
		// dropped.
		if name, ok := rootIdentifierName(stmt.Target); ok {
			writeStmt := stmt
			logicalUnknown := false
			if stmt.Operator == tokenOrAssign || stmt.Operator == tokenAndAssign {
				copy := *stmt
				copy.Operator = tokenNone
				writeStmt = &copy
				logicalUnknown = logicalTargetFact == nil || !logicalTargetFact.known
			}
			object, direct := target.Object.(*Identifier)
			badKeywordSplatKey, invalidKey := indexWriteInvalidKeywordSplatKey(target)
			badKeywordSplatKey = direct && object.Name == name &&
				typeExprHashLikeOnly(c.inferExpressionType(target.Object)) && badKeywordSplatKey
			if (stmt.Operator == tokenOrAssign || stmt.Operator == tokenAndAssign) &&
				logicalTargetFact != nil && logicalTargetFact.known && !logicalTargetFact.rhsReachable {
				// A skipped ||= at an invalid key proves that the key already
				// exists with a truthy value. A skipped &&= may instead be a
				// missing key, so it does not establish keyword-splat failure.
				if badKeywordSplatKey && stmt.Operator == tokenOrAssign {
					c.bindInvalidKeywordSplatKey(name, invalidKey)
				}
				return
			}
			declaredPreserved := false
			var written *TypeExpr
			writeMayLand := false
			abortsBeforeWrite := false
			applyDeclaredWrite := func() {
				receiverFact := assignmentReceiverFact
				if receiverFact == nil {
					receiverFact = c.inferExpressionType(target.Object)
				}
				keyBound, _ := declaredHashEntryTypes(receiverFact)
				if keyBound != nil ||
					(receiverFact != nil && receiverFact.Kind == TypeShape && !receiverFact.Nullable) {
					declaredPreserved, abortsBeforeWrite = c.applyIndexedWriteFacts(
						function,
						writeStmt,
						target,
						receiverFact,
					)
					if abortsBeforeWrite {
						return
					}
					written = c.inferExpressionType(writeStmt.Value)
					writeMayLand = true
					return
				}
				declaredPreserved, written, writeMayLand, abortsBeforeWrite = c.applyIndexedElementWriteFacts(
					function,
					writeStmt,
					target,
					assignmentReceiverFact,
				)
			}
			applyDeclaredWrite()
			if abortsBeforeWrite {
				return
			}
			staticUpdated := false
			if writeStmt.Operator == tokenNone && !logicalUnknown {
				staticUpdated = c.applyExactStaticArrayIndexWrite(
					name,
					target,
					writeStmt.Value,
					declaredPreserved,
				)
			}
			if declaredPreserved && !staticUpdated {
				c.invalidateElementWriteAliases(name, written)
			}
			if !declaredPreserved && !staticUpdated {
				if _, direct := target.Object.(*Identifier); direct {
					c.poisonElementWriteFacts(name)
				} else {
					// A projected receiver mutates an object retained inside the
					// root. Without an exact replacement, invalidate both containment
					// directions so aliases to that nested object cannot stay stale.
					c.poisonLocalStaticValues(name)
					c.poisonLocalType(name)
				}
			}
			if writeMayLand && !staticUpdated {
				c.clearLocalStaticValueAliases(name)
			}
			if writeMayLand {
				// Alias invalidation must describe the graph that existed before
				// this write. Link a retained container value only afterwards so a
				// newly inserted child is not mistaken for an old alias to weaken.
				c.linkContainerWriteAlias(name, writeStmt.Value, written)
			}
			if badKeywordSplatKey && (!logicalUnknown || stmt.Operator == tokenOrAssign) {
				c.bindInvalidKeywordSplatKey(name, invalidKey)
			}
		}
	case *MemberExpr:
		if name, ok := rootIdentifierName(stmt.Target); ok {
			if (stmt.Operator == tokenOrAssign || stmt.Operator == tokenAndAssign) &&
				logicalTargetFact != nil && logicalTargetFact.known && !logicalTargetFact.rhsReachable {
				return
			}
			receiverFact := assignmentReceiverFact
			if receiverFact == nil {
				receiverFact = c.inferExpressionType(target.Object)
			}
			preserved, written, mayWrite := c.applyMemberWriteFacts(
				function,
				stmt,
				target,
				name,
				receiverFact,
			)
			if !mayWrite {
				return
			}
			if preserved {
				c.invalidateElementWriteAliases(name, written)
			} else {
				c.poisonLocalType(name)
			}
			c.clearLocalStaticValueAliases(name)
			// Invalidate the pre-write graph before recording the newly retained
			// value, so weakening this receiver cannot poison a child that was
			// not reachable from it until the setter completed.
			c.linkContainerWriteAlias(name, stmt.Value, written)
		}
	}
}

// applyMemberWriteFacts checks hash/object field assignment syntax against a
// local-rooted receiver's declared hash or shape fact. Member assignment
// writes the property name as a hash key, and strings and symbols share that
// keyspace, so hash<string, V> and hash<symbol, V> both admit h.foo = ....
// A declared shape checks the property's logical field name independent of
// its backing representation.
func (c *scriptChecker) applyMemberWriteFacts(
	function string,
	stmt *AssignStmt,
	target *MemberExpr,
	name string,
	receiverFact *TypeExpr,
) (preserved bool, written *TypeExpr, mayWrite bool) {
	if stmt == nil || target == nil {
		return false, nil, false
	}
	contentFact := nonNilMutatorReceiverFact(receiverFact)
	if contentFact == nil {
		return false, nil, false
	}

	current, getterMayResolve := c.memberWriteCurrentType(target, contentFact)
	switch stmt.Operator {
	case tokenNone:
		written = c.inferExpressionType(stmt.Value)
	case tokenOrAssign, tokenAndAssign:
		if !getterMayResolve {
			return false, nil, false
		}
		if truthy, known := c.memberWriteUniversalGetterTruthiness(contentFact, target.Property); known {
			if stmt.Operator == tokenOrAssign && truthy ||
				stmt.Operator == tokenAndAssign && !truthy {
				return true, nil, false
			}
			written = c.inferExpressionType(stmt.Value)
			break
		}
		if typeExprDefinitelyTruthy(current) {
			if stmt.Operator == tokenOrAssign {
				return true, nil, false
			}
		} else if typeExprIsNilOnly(current) {
			if stmt.Operator == tokenAndAssign {
				return true, nil, false
			}
		}
		written = c.inferExpressionType(stmt.Value)
	default:
		if !getterMayResolve {
			return false, nil, false
		}
		right := c.inferExpressionType(stmt.Value)
		outcome := c.binaryOperationOutcome(stmt.Operator, current, right)
		if outcome.invalid {
			return false, nil, false
		}
		written = outcome.result
	}

	if keyBound, valueBound := declaredHashEntryTypes(contentFact); keyBound != nil {
		resolve := c.checkNamedTypeResolver()
		// Member assignment writes the property name as a hash key. Strings
		// and symbols share that keyspace, so hash<string, V> and
		// hash<symbol, V> both admit h.foo = ....
		keyType := checkTypeSymbol
		keyCompatible := hashKeyTypeSatisfies(keyType, keyBound, resolve)
		valueCompatible := written != nil && typeExprSatisfies(written, valueBound, resolve)
		if hashKeyTypesDisjoint(keyType, keyBound, resolve) {
			c.add(function, stmt.Pos(), "write to %s expected key %s, got %s",
				name, formatTypeExpr(keyBound), formatTypeExpr(keyType))
		}
		if written != nil && typedWriteRejected(written, valueBound, resolve) {
			c.add(function, stmt.Pos(), "write to %s expected value %s, got %s",
				name, formatTypeExpr(valueBound), formatTypeExpr(written))
		}
		return keyCompatible && valueCompatible &&
			mutatorReceiverFactIntact(c.localTypeFor(name), receiverFact), written, true
	}

	if contentFact.Kind == TypeShape && !contentFact.Nullable && contentFact.Name == "" {
		field, present := contentFact.Shape[target.Property]
		if !present {
			if !contentFact.Open {
				c.add(function, stmt.Pos(), "write to %s adds field %s to exact shape %s",
					name, target.Property, formatTypeExpr(contentFact))
			}
			return false, written, true
		}
		if written != nil &&
			typedWriteRejected(written, shapeFieldValueType(field), c.checkNamedTypeResolver()) {
			c.add(function, stmt.Pos(), "write to %s field %s expected %s, got %s",
				name, target.Property, formatTypeExpr(field), formatTypeExpr(written))
		}
	}
	return false, written, true
}

// memberWriteCurrentType reports the value a compound/logical member target
// can read on a path that reaches its setter. Hash-owned readers and universal
// helpers dispatch before ordinary hash/object data; only the latter uses a
// typed hash value bound or declared shape field. A data getter whose hash key
// is impossible, or a missing closed-shape field, raises before the right side
// runs and cannot reach the setter.
func (c *scriptChecker) memberWriteCurrentType(target *MemberExpr, receiver *TypeExpr) (*TypeExpr, bool) {
	if target == nil {
		return nil, false
	}
	property := target.Property
	if current, resolved := c.hashOwnedMemberWriteCurrentType(receiver, property); resolved {
		return current, true
	}
	if isUniversalMember(property) {
		if c.memberWriteUsesUniversalDispatch(receiver, property) {
			switch property {
			case "itself", "dup", "clone", "freeze":
				return receiver, true
			case "frozen?", "nil?":
				return checkTypeBool, true
			}
		}
		if callable, resolved := c.resolveMemberCallable(target); resolved {
			if callable.fn == nil {
				if callable.spec.autoInvoke {
					return callable.spec.resultType, true
				}
				return checkTypeFunction, true
			}
			return c.inferExpressionType(target), true
		}
	}
	if valueBound, getterMayResolve := c.declaredHashDataMemberResult(receiver); getterMayResolve {
		return valueBound, true
	}
	if receiver == nil || receiver.Kind != TypeShape || receiver.Nullable {
		return nil, false
	}
	field, present := receiver.Shape[property]
	if !present {
		return nil, false
	}
	return shapeFieldValueType(field), true
}

func (c *scriptChecker) declaredHashDataMemberResult(receiver *TypeExpr) (*TypeExpr, bool) {
	keyBound, valueBound := declaredHashEntryTypes(receiver)
	if valueBound == nil ||
		typeExprsDisjoint(checkTypeMethodName, keyBound, c.checkNamedTypeResolver()) {
		return nil, false
	}
	return valueBound, true
}

// hashOwnedMemberWriteCurrentType derives the known result of hash-owned
// readers used by compound assignment. KindHash always dispatches the builtin;
// a declared shape or typed hash may also be backed by KindObject, where a
// same-named string field wins. Join that field's bound when it can exist.
func (c *scriptChecker) hashOwnedMemberWriteCurrentType(receiver *TypeExpr, property string) (*TypeExpr, bool) {
	var builtin *TypeExpr
	switch property {
	case "size", "length":
		builtin = checkTypeInt
	case "empty?":
		builtin = checkTypeBool
	case "keys", "values", "values_at", "fetch_values", "to_a", "flatten":
		builtin = checkTypeArray
	case "merge", "clear", "slice", "except", "compact":
		builtin = checkTypeHash
	case "inspect":
		builtin = checkTypeString
	case "replace", "store", "delete", "remap_keys":
		// These members are not auto-invoked, so a bare getter returns the
		// callable itself and logical assignment can reach the setter.
		builtin = checkTypeFunction
	default:
		return nil, false
	}

	arms, ok := typeExprArms(receiver, 0)
	if !ok || len(arms) == 0 {
		return nil, false
	}
	currents := make([]*TypeExpr, 0, len(arms))
	for _, arm := range arms {
		if arm.Kind != TypeHash && arm.Kind != TypeShape {
			return nil, false
		}
		current := c.hashOwnedMemberWriteArmType(arm, property, builtin)
		if current == nil {
			return nil, true
		}
		currents = append(currents, current)
	}
	return unionTypeExprs(currents...), true
}

func (c *scriptChecker) hashOwnedMemberWriteArmType(
	receiver *TypeExpr,
	property string,
	builtin *TypeExpr,
) *TypeExpr {
	if keyBound, valueBound := declaredHashEntryTypes(receiver); valueBound != nil {
		if hashKeyTypesDisjoint(checkTypeString, keyBound, c.checkNamedTypeResolver()) {
			return builtin
		}
		if typeExprMayIncludeCallable(valueBound) {
			return nil
		}
		return unionTypeExprs(builtin, valueBound)
	}
	if receiver == nil || receiver.Kind != TypeShape || receiver.Nullable {
		return nil
	}
	if receiver.Name != "" {
		return builtin
	}
	field, present := receiver.Shape[property]
	if !present {
		if receiver.Open {
			return nil
		}
		return builtin
	}
	fieldType := shapeFieldValueType(field)
	if typeExprMayIncludeCallable(fieldType) {
		return nil
	}
	return unionTypeExprs(builtin, fieldType)
}

// memberWriteUsesUniversalDispatch reports that every non-nil hash-like
// receiver arm reaches the universal helper rather than a callable object
// export with the same name.
func (c *scriptChecker) memberWriteUsesUniversalDispatch(receiver *TypeExpr, property string) bool {
	if !isUniversalMember(property) || !typeExprHashLikeOnly(receiver) {
		return false
	}
	return typeExprArmsAll(receiver, func(arm *TypeExpr) bool {
		if isUniversalDataSafe(property) {
			if arm.Kind == TypeShape && arm.Name != "" {
				return true
			}
			if keyBound, valueBound := declaredHashEntryTypes(arm); valueBound != nil &&
				hashKeyTypesDisjoint(checkTypeString, keyBound, c.checkNamedTypeResolver()) {
				return true
			}
		}
		return typeArmUsesUniversalMemberDispatch(arm, property)
	})
}

// memberWriteUniversalGetterTruthiness records the nullary universal helpers
// whose result has fixed truthiness on the non-nil path that can reach a member
// setter.
func (c *scriptChecker) memberWriteUniversalGetterTruthiness(
	receiver *TypeExpr,
	property string,
) (bool, bool) {
	if !c.memberWriteUsesUniversalDispatch(receiver, property) {
		return false, false
	}
	switch property {
	case "nil?":
		return false, true
	case "itself", "dup", "clone", "freeze", "frozen?":
		return true, true
	default:
		return false, false
	}
}

func (c *scriptChecker) hashLikeMemberGetterTruthiness(
	member *MemberExpr,
	receiver *TypeExpr,
) (bool, bool) {
	if member == nil || !typeExprNeverNil(receiver) || !typeExprHashLikeOnly(receiver) {
		return false, false
	}
	if isUniversalMember(member.Property) {
		return c.memberWriteUniversalGetterTruthiness(receiver, member.Property)
	}
	current, getterMayResolve := c.memberWriteCurrentType(member, receiver)
	if !getterMayResolve || current == nil || typeExprMayIncludeCallable(current) {
		return false, false
	}
	if typeExprDefinitelyTruthy(current) {
		return true, true
	}
	if typeExprIsNilOnly(current) {
		return false, true
	}
	return false, false
}

func (c *scriptChecker) bindInvalidKeywordSplatKey(name, invalidKey string) {
	if invalidKey == "" {
		c.bindLocalKeywordSplatFailureAliases(name)
		return
	}
	c.bindLocalKeywordSplatFailureAliases(name, invalidKey)
}

func (c *scriptChecker) capturedDestructureAliasStillCurrent(fact capturedDestructureValueFact) bool {
	if !fact.evaluated {
		return true
	}
	if len(fact.staticVals) == 0 {
		return false
	}
	current, exact := c.staticValueExpressionAlternatives(fact.value)
	if !exact {
		return false
	}
	sameRoot, _, _, _ := staticValueMutableRelationships(fact.staticVals, current)
	return sameRoot
}

func (c *scriptChecker) containerAliasRoots(expr Expression) []string {
	seen := make(map[string]struct{})
	var collect func(Expression)
	collect = func(expr Expression) {
		switch typed := expr.(type) {
		case *Identifier, *IndexExpr, *MemberExpr:
			if root, ok := c.retainedContainerRoot(expr); ok {
				seen[root] = struct{}{}
			}
		case *ConditionalExpr:
			if branch, known := staticConditionalExpressionBranch(typed); known {
				collect(branch)
				return
			}
			collect(typed.Consequent)
			collect(typed.Alternate)
		case *IfExpr:
			if branch, known := staticIfExpressionBranch(typed); known {
				collect(branch)
				return
			}
			collect(typed.Consequent)
			for _, branch := range typed.ElseIf {
				collect(branch.Result)
			}
			collect(typed.Alternate)
		case *RescueExpr:
			collect(typed.Body)
			collect(typed.Fallback)
		case *CaseExpr:
			if result, known := staticCaseExpressionResult(typed); known {
				collect(result)
				return
			}
			for _, clause := range typed.Clauses {
				collect(clause.Result)
			}
			collect(typed.ElseExpr)
		}
	}
	collect(expr)
	roots := make([]string, 0, len(seen))
	for name := range seen {
		roots = append(roots, name)
	}
	slices.Sort(roots)
	return roots
}

func (c *scriptChecker) retainedContainerRoot(expr Expression) (string, bool) {
	root, ok := rootIdentifierName(expr)
	if !ok {
		return "", false
	}
	rootType := c.localTypeFor(root)
	if rootType == nil {
		if !c.hasPossibleContainerBinding(root) {
			return "", false
		}
	} else if !c.typeExprHasContainerArm(rootType) || typeExprMayIncludeCallable(rootType) {
		return "", false
	}
	if _, autoInvoked := c.evaluatedIdentityExpression(expr, true); autoInvoked {
		return "", false
	}
	return root, true
}

func (c *scriptChecker) applyExactStaticArrayIndexWrite(
	name string,
	target *IndexExpr,
	value Expression,
	preserveCompatibleDeclaredTypes bool,
) bool {
	if c.mutationRegionDepth != 0 || target == nil {
		return false
	}
	_, valueStatic := staticLiteralValue(value)
	if !valueStatic {
		return false
	}
	indices, root, exactPath := staticArrayIndexWritePath(target)
	if !exactPath || root != name {
		return false
	}
	rootValues, exact := c.localStaticValuesFor(name)
	if !exact || len(rootValues) == 0 {
		return false
	}
	replacements := make(map[Expression]Expression, len(rootValues))
	for _, candidate := range rootValues {
		updated, ok := replaceStaticArrayIndexPath(candidate, indices, value, replacements)
		if !ok {
			return false
		}
		replacements[candidate] = updated
	}
	identities := c.containerIdentityNames(name)
	for frameIndex, frame := range c.localClassValues {
		for localName, fact := range frame {
			if len(fact.staticVals) == 0 {
				continue
			}
			_, definitelySame := identities[localName]
			if !definitelySame && len(rootValues) == 1 {
				definitelySame = true
				for _, candidate := range fact.staticVals {
					if _, replaced := replaceStaticLiteralAliases(candidate, replacements); !replaced {
						definitelySame = false
						break
					}
				}
			}
			values := make([]Expression, 0, len(fact.staticVals)*2)
			matched := false
			for _, candidate := range fact.staticVals {
				if replacement, replaced := replaceStaticLiteralAliases(candidate, replacements); replaced {
					matched = true
					if !definitelySame {
						// This local may refer to a receiver alternative, but is not
						// known to track the receiver on every path. Preserve both the
						// unmodified and modified outcomes instead of treating a
						// conditional alias as definite identity.
						values = append(values, candidate)
					}
					values = append(values, replacement)
					continue
				}
				values = append(values, candidate)
			}
			if !matched {
				continue
			}
			fact.staticVals = c.normalizeCheckStaticValues(values)
			fact.staticChoice = checkStaticChoiceFact{}
			frame[localName] = fact
			if frameIndex >= len(c.localTypes) || c.localTypes[frameIndex] == nil {
				continue
			}
			current, tracked := c.localTypes[frameIndex][localName]
			if !tracked || current == nil {
				continue
			}
			preserve := localName == name && preserveCompatibleDeclaredTypes
			updatedTypes := make([]*TypeExpr, 0, len(fact.staticVals))
			for _, updated := range fact.staticVals {
				if inferred := c.inferExpressionType(updated); inferred != nil {
					updatedTypes = append(updatedTypes, inferred)
				}
			}
			updated := unionTypeExprs(updatedTypes...)
			if literalArrayElementsWitnessed(current) ||
				current.Name == blockRestElementsMarker {
				c.localTypes[frameIndex][localName] = updated
				continue
			}
			if !preserve && updated != nil {
				preserve = typeExprSatisfies(updated, current, c.checkNamedTypeResolver())
			}
			if !preserve {
				c.poisonLocalTypeOnly(localName)
			}
		}
	}
	c.replaceEvaluatedDestructureStaticAliases(replacements)
	return true
}

func (c *scriptChecker) replaceEvaluatedDestructureStaticAliases(replacements map[Expression]Expression) {
	for expr, fact := range c.evaluatedDestructureFacts {
		if len(fact.staticVals) == 0 {
			continue
		}
		values := append([]Expression(nil), fact.staticVals...)
		matched := false
		for i, candidate := range values {
			if replacement, replaced := replaceStaticLiteralAliases(candidate, replacements); replaced {
				values[i] = replacement
				matched = true
			}
		}
		if !matched {
			continue
		}
		fact.staticVals = c.normalizeCheckStaticValues(values)
		fact.staticChoice = checkStaticChoiceFact{}
		types := make([]*TypeExpr, 0, len(fact.staticVals))
		for _, value := range fact.staticVals {
			if inferred := c.inferExpressionType(value); inferred != nil {
				types = append(types, inferred)
			}
		}
		fact.assigned = unionTypeExprs(types...)
		c.evaluatedDestructureFacts[expr] = fact
	}
}

func (c *scriptChecker) clearLocalStaticValueAliases(name string) {
	c.poisonLocalStaticValues(name)
}

func staticArrayIndexWritePath(target *IndexExpr) ([]int, string, bool) {
	var reversed []int
	for current := target; current != nil; {
		if len(current.Indices) != 1 {
			return nil, "", false
		}
		indexValue, static := staticLiteralValue(current.Indices[0])
		if !static || indexValue.Kind() != KindInt || indexValue.Int() < 0 {
			return nil, "", false
		}
		reversed = append(reversed, int(indexValue.Int()))
		switch object := current.Object.(type) {
		case *Identifier:
			indices := make([]int, len(reversed))
			for i, index := range reversed {
				indices[len(reversed)-1-i] = index
			}
			return indices, object.Name, true
		case *IndexExpr:
			current = object
		default:
			return nil, "", false
		}
	}
	return nil, "", false
}

func replaceStaticArrayIndexPath(
	expr Expression,
	indices []int,
	value Expression,
	replacements map[Expression]Expression,
) (Expression, bool) {
	if len(indices) == 0 {
		return value, true
	}
	array, ok := expr.(*ArrayLiteral)
	if !ok || indices[0] >= len(array.Elements) {
		return nil, false
	}
	replacement, ok := replaceStaticArrayIndexPath(
		array.Elements[indices[0]],
		indices[1:],
		value,
		replacements,
	)
	if !ok {
		return nil, false
	}
	clone := *array
	clone.Elements = append([]Expression(nil), array.Elements...)
	clone.Elements[indices[0]] = replacement
	replacements[array] = &clone
	return &clone, true
}

func replaceStaticLiteralAliases(
	expr Expression,
	replacements map[Expression]Expression,
) (Expression, bool) {
	if replacement, ok := replacements[expr]; ok {
		return replacement, true
	}
	switch typed := expr.(type) {
	case *ArrayLiteral:
		elements := append([]Expression(nil), typed.Elements...)
		matched := false
		for i, element := range elements {
			if replacement, ok := replaceStaticLiteralAliases(element, replacements); ok {
				elements[i] = replacement
				matched = true
			}
		}
		if matched {
			clone := *typed
			clone.Elements = elements
			return &clone, true
		}
	case *HashLiteral:
		pairs := append([]HashPair(nil), typed.Pairs...)
		matched := false
		for i, pair := range pairs {
			if replacement, ok := replaceStaticLiteralAliases(pair.Value, replacements); ok {
				pairs[i].Value = replacement
				matched = true
			}
		}
		if matched {
			clone := *typed
			clone.Pairs = pairs
			return &clone, true
		}
	}
	return expr, false
}

func indexWriteInvalidKeywordSplatKey(target *IndexExpr) (bool, string) {
	if target == nil || len(target.Indices) != 1 {
		return false, ""
	}
	key, static := staticLiteralValue(target.Indices[0])
	if !static {
		return false, ""
	}
	if _, err := hashKeyString(key); err != nil {
		return true, ""
	}
	// The key is a legal string key, so it cannot invalidate the splat.
	return false, ""
}

func (c *scriptChecker) bindExpressionLocalValueFact(name string, expr Expression) {
	instanceOrigins, instanceExact := c.instanceValueOrigins(expr)
	identityExpr, autoInvoked := c.evaluatedIdentityExpression(expr, true)
	classNames, classExact := c.classValueExpressionNames(identityExpr)
	if autoInvoked {
		classNames, classExact = c.dispatchClassValueExpressionNames(identityExpr)
	}
	staticValues, staticExact := c.staticValueExpressionAlternatives(expr)
	var staticChoice checkStaticChoiceFact
	if staticExact {
		if choice, correlated := c.staticValueChoiceForExpression(expr); correlated {
			staticChoice = choice
		}
	}
	if instanceExact {
		c.bindLocalExactValueFact(name, checkLocalValueFact{
			instanceOrigins: instanceOrigins,
		})
	} else if classExact {
		c.bindLocalClassValues(name, classNames)
	} else if fns, ok := c.callableExpressionFunctions(identityExpr); ok {
		c.bindLocalCallableValues(name, fns)
	} else if staticExact {
		c.bindLocalExactValueFact(name, checkLocalValueFact{
			staticVals:   staticValues,
			staticChoice: staticChoice,
		})
	} else if c.keywordSplatExpressionAlwaysFails(expr) &&
		c.expressionMayHaveExpansionType(expr, KindHash, checkTypeHash) {
		c.bindLocalKeywordSplatFailure(name)
	} else {
		c.bindLocalClassValue(name, "")
	}
}

type checkInstanceOriginsCapture struct {
	origins []Expression
	exact   bool
}

func (c *scriptChecker) pinExpressionInstanceOrigins(expr Expression) {
	if expr == nil {
		return
	}
	origins, exact := c.instanceValueOrigins(expr)
	if c.pinnedInstanceOrigins == nil {
		c.pinnedInstanceOrigins = make(map[Expression]checkInstanceOriginsCapture)
	}
	c.pinnedInstanceOrigins[expr] = checkInstanceOriginsCapture{
		origins: append([]Expression(nil), origins...),
		exact:   exact,
	}
}

func (c *scriptChecker) evaluatedInstanceValueOrigins(expr Expression) ([]Expression, bool) {
	if captured, ok := c.pinnedInstanceOrigins[expr]; ok {
		return append([]Expression(nil), captured.origins...), captured.exact
	}
	return c.instanceValueOrigins(expr)
}

// instanceValueOrigins follows exact local aliases and value-preserving
// branches back to the constructor expressions that produced an instance.
func (c *scriptChecker) instanceValueOrigins(expr Expression) ([]Expression, bool) {
	if fact, captured := c.destructureProjectionFacts[expr]; captured &&
		fact.instanceExact && len(fact.instanceOrigins) > 0 {
		return append([]Expression(nil), fact.instanceOrigins...), true
	}
	if fact, captured := c.evaluatedDestructureFacts[expr]; captured &&
		fact.instanceExact && len(fact.instanceOrigins) > 0 {
		return append([]Expression(nil), fact.instanceOrigins...), true
	}
	if fact, captured := c.constructorInstanceFacts[expr]; captured {
		if !fact.exact || len(fact.classNames) == 0 {
			return nil, false
		}
		return []Expression{expr}, true
	}
	merge := func(expressions ...Expression) ([]Expression, bool) {
		var origins []Expression
		for _, expression := range expressions {
			candidates, exact := c.instanceValueOrigins(expression)
			if !exact {
				return nil, false
			}
			origins = append(origins, candidates...)
		}
		origins = normalizeCheckExpressionIdentities(origins)
		return origins, len(origins) > 0
	}

	switch typed := expr.(type) {
	case *Identifier:
		if typed.Name == "self" {
			originFact, captured := c.reachableParamFacts[reachableInstanceOriginFact]
			if captured && len(originFact.staticVals) > 0 {
				return append([]Expression(nil), originFact.staticVals...), true
			}
		}
		fact, exact := c.localValueFactFor(typed.Name)
		if exact && len(fact.instanceOrigins) > 0 {
			return append([]Expression(nil), fact.instanceOrigins...), true
		}
		if c.identifierShadowed(typed.Name) || c.hostGlobalShadows(typed.Name) {
			return nil, false
		}
		fn := c.script.functions[typed.Name]
		if fn == nil || len(fn.Params) > 0 {
			return nil, false
		}
		return c.scriptCallableInstanceOrigins(
			nil,
			staticCallable{name: typed.Name, fn: fn},
		)
	case *CallExpr:
		target, resolved := c.resolveCallable(typed)
		if !resolved || target.fn == nil {
			return nil, false
		}
		return c.scriptCallableInstanceOrigins(typed, target)
	case *ConditionalExpr:
		if branch, known := staticConditionalExpressionBranch(typed); known {
			return c.instanceValueOrigins(branch)
		}
		return merge(typed.Consequent, typed.Alternate)
	case *IfExpr:
		if branch, known := c.inferredIfExpressionBranch(typed); known {
			return c.instanceValueOrigins(branch)
		}
		branches := make([]Expression, 0, len(typed.ElseIf)+2)
		branches = append(branches, typed.Consequent)
		for _, branch := range typed.ElseIf {
			branches = append(branches, branch.Result)
		}
		branches = append(branches, typed.Alternate)
		return merge(branches...)
	case *RescueExpr:
		if expressionProvenNonRaising(typed.Body) {
			return c.instanceValueOrigins(typed.Body)
		}
		return merge(typed.Body, typed.Fallback)
	case *IndexExpr:
		projected, exact := c.staticLiteralProjections(typed)
		if !exact {
			return nil, false
		}
		return merge(projected...)
	}
	return nil, false
}

// logicalAssignmentFact models x ||= v and x &&= v: the runtime keeps the
// current value when the short circuit takes it and otherwise binds the
// right-hand side directly, so a decided current picks one side and an
// undecided one joins both.
func logicalAssignmentFact(operator TokenType, current, next *TypeExpr) *TypeExpr {
	switch operator {
	case tokenOrAssign:
		if typeExprDefinitelyTruthy(current) {
			return current
		}
		if typeExprDefinitelyFalsey(current) {
			return next
		}
	case tokenAndAssign:
		if typeExprDefinitelyFalsey(current) {
			return current
		}
		if typeExprDefinitelyTruthy(current) {
			return next
		}
	}
	if current == nil || next == nil {
		return nil
	}
	return unionTypeExprs(current, next)
}

// bindLogicalAssignmentValueFact mirrors logicalAssignmentFact for exact
// class, callable, and literal facts whose truthiness may be known even when
// their TypeExpr is absent or less precise.
func (c *scriptChecker) bindLogicalAssignmentValueFact(
	name string,
	operator TokenType,
	currentType *TypeExpr,
	next Expression,
) {
	currentFact, currentTracked := c.localValueFactFor(name)
	bindNext := func() {
		c.bindExpressionLocalValueFact(name, next)
	}
	currentTruthy, currentKnown := localValueFactTruthiness(currentFact, currentTracked)
	if !currentKnown {
		switch {
		case typeExprDefinitelyTruthy(currentType):
			currentTruthy, currentKnown = true, true
		case typeExprIsNilOnly(currentType):
			currentTruthy, currentKnown = false, true
		}
	}

	switch operator {
	case tokenOrAssign:
		if currentKnown && currentTruthy {
			return
		}
		if currentKnown {
			bindNext()
			return
		}
	case tokenAndAssign:
		if currentKnown && currentTruthy {
			bindNext()
			return
		}
		if currentKnown {
			return
		}
	}
	c.bindLocalClassValue(name, "")
}

func (c *scriptChecker) logicalAssignmentRHSReachable(
	name string,
	operator TokenType,
	currentType *TypeExpr,
) bool {
	currentFact, tracked := c.localValueFactFor(name)
	truthy, known := localValueFactTruthiness(currentFact, tracked)
	if !known {
		switch {
		case typeExprDefinitelyTruthy(currentType):
			truthy, known = true, true
		case typeExprIsNilOnly(currentType):
			truthy, known = false, true
		}
	}
	if !known {
		return true
	}
	return operator == tokenOrAssign && !truthy || operator == tokenAndAssign && truthy
}

func localValueFactTruthiness(fact checkLocalValueFact, tracked bool) (bool, bool) {
	if !tracked || fact.keywordSplatFails {
		return false, false
	}
	if len(fact.classNames) > 0 || len(fact.callables) > 0 ||
		len(fact.instanceOrigins) > 0 {
		return true, true
	}
	if len(fact.staticVals) == 0 {
		return false, false
	}
	var truthy bool
	for i, value := range fact.staticVals {
		candidate, known := staticExpressionTruthiness(value)
		if !known || i > 0 && candidate != truthy {
			return false, false
		}
		truthy = candidate
	}
	return truthy, true
}

type capturedDestructureValueFact struct {
	target          Expression
	value           Expression
	assigned        *TypeExpr
	declared        *TypeExpr
	known           bool
	evaluated       bool
	sourceName      string
	sourceGen       uint64
	identityRoots   []capturedContainerRoot
	retainedRoots   []capturedContainerRoot
	classNames      []string
	instanceOrigins []Expression
	instanceExact   bool
	callables       []*ScriptFunction
	staticVals      []Expression
	staticChoice    checkStaticChoiceFact
	factKind        byte
}

type capturedContainerRoot struct {
	name       string
	generation uint64
}

const (
	destructureClassFact byte = iota + 1
	destructureCallableFact
	destructureStaticFact
)

func capturedDestructureStaticChoiceExact(fact capturedDestructureValueFact) bool {
	return len(fact.staticVals) > 0 &&
		len(fact.staticChoice.indices) == len(fact.staticVals) &&
		checkCallSplatSourceIdentified(fact.staticChoice.source)
}

// newDestructureProjection creates a checker-only expression whose evaluated
// identity stays available after the statement-scoped capture table is gone.
func (c *scriptChecker) newDestructureProjection(
	fact capturedDestructureValueFact,
	pos Position,
) Expression {
	projection := &Identifier{
		Name:     "\x00destructure-projection",
		Position: pos,
	}
	if c.destructureProjectionFacts == nil {
		c.destructureProjectionFacts = make(map[Expression]capturedDestructureValueFact)
	}
	c.destructureProjectionFacts[projection] = capturedDestructureValueFact{
		assigned:        fact.assigned,
		known:           true,
		evaluated:       true,
		identityRoots:   append([]capturedContainerRoot(nil), fact.identityRoots...),
		retainedRoots:   append([]capturedContainerRoot(nil), fact.retainedRoots...),
		classNames:      append([]string(nil), fact.classNames...),
		instanceOrigins: append([]Expression(nil), fact.instanceOrigins...),
		instanceExact:   fact.instanceExact,
		callables:       append([]*ScriptFunction(nil), fact.callables...),
		staticVals:      append([]Expression(nil), fact.staticVals...),
		staticChoice:    cloneCheckStaticChoiceFact(fact.staticChoice),
		factKind:        fact.factKind,
	}
	return projection
}

func (c *scriptChecker) captureEvaluatedDestructureFact(expr Expression) {
	c.captureEvaluatedDestructureFactWithAuto(expr, c.inferExpressionType(expr), true)
}

func (c *scriptChecker) captureEvaluatedDestructureFactWithExpectation(
	expr Expression,
	expectation expressionExpectation,
) {
	c.captureEvaluatedDestructureFactWithAuto(
		expr,
		c.inferExpressionTypeWithExpectation(expr, expectation),
		!typeExprIncludesCallable(expectation.ty),
	)
}

func (c *scriptChecker) captureEvaluatedDestructureFactWithAuto(
	expr Expression,
	assigned *TypeExpr,
	autoCall bool,
) {
	if expr == nil || c.evaluatedDestructureFacts == nil {
		return
	}
	fact := capturedDestructureValueFact{
		value:     expr,
		assigned:  assigned,
		known:     true,
		evaluated: true,
	}
	if origins, exact := c.instanceValueOrigins(expr); exact {
		fact.instanceOrigins = append([]Expression(nil), origins...)
		fact.instanceExact = true
	}
	if ident, ok := expr.(*Identifier); ok {
		fact.sourceName = ident.Name
		fact.sourceGen = c.localBindingGenerations[ident.Name]
		if _, retained := c.retainedContainerRoot(expr); retained {
			names := c.containerIdentityNames(ident.Name)
			ordered := make([]string, 0, len(names))
			for name := range names {
				ordered = append(ordered, name)
			}
			slices.Sort(ordered)
			for _, name := range ordered {
				fact.identityRoots = append(fact.identityRoots, capturedContainerRoot{
					name:       name,
					generation: c.localBindingGenerations[name],
				})
			}
		}
	}
	if _, direct := expr.(*Identifier); !direct && c.typeExprHasContainerArm(assigned) {
		for _, name := range c.containerAliasRoots(expr) {
			fact.retainedRoots = append(
				fact.retainedRoots,
				capturedContainerRoot{
					name:       name,
					generation: c.localBindingGenerations[name],
				},
			)
		}
	}
	c.pinExpressionFact(expr, fact.assigned)
	if callables, exact := c.callableExpressionFunctionsAfterEvaluation(expr, autoCall); !autoCall && exact {
		fact.factKind = destructureCallableFact
		fact.callables = append([]*ScriptFunction(nil), callables...)
	} else if classNames, exact := c.classValueExpressionNames(expr); exact {
		fact.factKind = destructureClassFact
		fact.classNames = append([]string(nil), classNames...)
	} else if callables, exact := c.callableExpressionFunctionsAfterEvaluation(expr, autoCall); exact {
		fact.factKind = destructureCallableFact
		fact.callables = append([]*ScriptFunction(nil), callables...)
	} else if staticVals, exact := c.evaluatedStaticValueExpressionAlternatives(expr); exact {
		fact.factKind = destructureStaticFact
		fact.staticVals = append([]Expression(nil), staticVals...)
		if choice, correlated := c.staticValueChoiceForExpression(expr); correlated {
			fact.staticChoice = cloneCheckStaticChoiceFact(choice)
		}
	} else if array, ok := expr.(*ArrayLiteral); ok {
		if captured, exact := c.capturedDestructureArrayFact(array); exact {
			fact.assigned = captured.assigned
			fact.factKind = captured.factKind
			fact.staticVals = append([]Expression(nil), captured.staticVals...)
			fact.retainedRoots = append([]capturedContainerRoot(nil), captured.retainedRoots...)
		}
	}
	c.evaluatedDestructureFacts[expr] = fact
}

func (c *scriptChecker) captureEvaluatedDestructureFactOnce(expr Expression) {
	if _, captured := c.evaluatedDestructureFacts[expr]; captured {
		return
	}
	c.captureEvaluatedDestructureFact(expr)
}

// captureDestructureValueFacts captures one destructure level. Nested targets
// remain boundary facts so their values can be projected after earlier sibling
// targets have run.
func (c *scriptChecker) captureDestructureValueFacts(target *DestructureTarget, value Expression) []capturedDestructureValueFact {
	if target == nil {
		return nil
	}
	if value == nil {
		return captureUnknownDestructureValueFacts(target)
	}
	if _, literal := value.(*ArrayLiteral); !literal {
		if facts, exact := c.captureStaticChoiceDestructureValueFacts(target, value); exact {
			return facts
		}
	}
	if retained, exact := c.exactEvaluatedDestructureValue(value); exact {
		value = retained
	}
	if _, array := value.(*ArrayLiteral); !array {
		captured, evaluated := c.evaluatedDestructureFacts[value]
		capturedScalar := evaluated &&
			(captured.factKind == destructureClassFact ||
				captured.factKind == destructureCallableFact ||
				typeExprDefinitelyDestructuresAsScalar(captured.assigned))
		if _, static := staticLiteralValue(value); !static && !capturedScalar {
			if evaluated {
				if facts, projected := c.captureTypedDestructureValueFacts(
					target,
					captured,
				); projected {
					return facts
				}
			}
			return captureUnknownDestructureValueFacts(target)
		}
	}
	values := destructureAssignmentExpressions(target, value)
	var facts []capturedDestructureValueFact
	for i, element := range target.Elements {
		var elementValue Expression
		if i < len(values) {
			elementValue = values[i]
		}
		switch elementTarget := element.Target.(type) {
		case *DestructureTarget:
			fact := c.capturedDestructureValueFact(elementValue)
			fact.target = elementTarget
			fact.declared = element.Type
			facts = append(facts, fact)
		default:
			if elementTarget == nil {
				continue
			}
			fact := c.capturedDestructureValueFact(elementValue)
			fact.target = elementTarget
			fact.declared = element.Type
			facts = append(facts, fact)
		}
	}
	return facts
}

func (c *scriptChecker) captureStaticChoiceDestructureValueFacts(
	target *DestructureTarget,
	value Expression,
) ([]capturedDestructureValueFact, bool) {
	if target == nil || value == nil || len(target.Elements) == 0 {
		return nil, false
	}
	for _, element := range target.Elements {
		if element.Rest {
			return nil, false
		}
		switch element.Target.(type) {
		case nil, *Identifier, *IvarExpr:
		default:
			return nil, false
		}
	}

	alternatives, exact := c.staticValueExpressionAlternatives(value)
	if !exact || len(alternatives) == 0 {
		return nil, false
	}
	arrays := make([]*ArrayLiteral, len(alternatives))
	for i, alternative := range alternatives {
		array, ok := alternative.(*ArrayLiteral)
		if !ok {
			return nil, false
		}
		for _, element := range array.Elements {
			if _, splat := element.(*SplatArg); splat {
				return nil, false
			}
		}
		arrays[i] = array
	}

	source := checkCallSplatSource{
		alternatives: append([]Expression(nil), alternatives...),
		evaluation:   value,
	}
	if ident, direct := value.(*Identifier); direct {
		source = c.checkCallSplatSourceForLocal(ident.Name, alternatives)
	}
	if !checkCallSplatSourceIdentified(source) {
		return nil, false
	}

	facts := make([]capturedDestructureValueFact, 0, len(target.Elements))
	for slot, element := range target.Elements {
		if element.Target == nil {
			continue
		}
		projected := make([]Expression, 0, len(arrays))
		indices := make([]int, 0, len(arrays))
		types := make([]*TypeExpr, 0, len(arrays))
		for choice, array := range arrays {
			var candidate Expression = &NilLiteral{Position: value.Pos()}
			if slot < len(array.Elements) {
				candidate = array.Elements[slot]
			}
			values, valueExact := c.staticValueExpressionAlternatives(candidate)
			if !valueExact || len(values) != 1 {
				return nil, false
			}
			if _, static := staticLiteralValue(values[0]); !static ||
				staticLiteralHasMutableIdentity(values[0]) {
				return nil, false
			}
			projected = append(projected, values[0])
			indices = append(indices, choice)
			if inferred := c.inferExpressionType(values[0]); inferred != nil {
				types = append(types, inferred)
			}
		}
		normalized := c.normalizeCheckStaticValues(projected)
		if len(normalized) == 0 {
			return nil, false
		}
		staticChoice := checkStaticChoiceFact{}
		if len(normalized) == len(projected) {
			aligned := true
			for i := range normalized {
				if normalized[i] != projected[i] {
					aligned = false
					break
				}
			}
			if aligned {
				staticChoice = checkStaticChoiceFact{
					source:  cloneCheckCallSplatSource(source),
					indices: indices,
				}
			}
		}
		facts = append(facts, capturedDestructureValueFact{
			target:    element.Target,
			value:     value,
			assigned:  unionTypeExprs(types...),
			declared:  element.Type,
			known:     true,
			evaluated: true,
			staticVals: append(
				[]Expression(nil),
				normalized...,
			),
			staticChoice: staticChoice,
			factKind:     destructureStaticFact,
		})
	}
	return facts, len(facts) > 0
}

func (c *scriptChecker) capturedDestructureValueFact(value Expression) capturedDestructureValueFact {
	fact, captured := c.evaluatedDestructureFacts[value]
	if !captured {
		if projection, projected := c.destructureProjectionFacts[value]; projected {
			fact = projection
			captured = true
		} else if array, synthetic := value.(*ArrayLiteral); synthetic {
			fact, captured = c.capturedDestructureArrayFact(array)
		}
	}
	fact.value = value
	if ident, ok := value.(*Identifier); ok &&
		ident.Name != "\x00destructure-projection" &&
		fact.sourceName == "" {
		fact.sourceName = ident.Name
		fact.sourceGen = c.localBindingGenerations[ident.Name]
	}
	fact.known = captured || value != nil
	if !fact.known || captured {
		return fact
	}
	fact.assigned = c.inferExpressionType(value)
	if origins, exact := c.instanceValueOrigins(value); exact {
		fact.instanceOrigins = append([]Expression(nil), origins...)
		fact.instanceExact = true
	}
	if classNames, exact := c.classValueExpressionNames(value); exact {
		fact.factKind = destructureClassFact
		fact.classNames = append([]string(nil), classNames...)
	} else if callables, exact := c.callableExpressionFunctionsAfterEvaluation(value, true); exact {
		fact.factKind = destructureCallableFact
		fact.callables = append([]*ScriptFunction(nil), callables...)
	} else if staticVals, exact := c.staticValueExpressionAlternatives(value); exact {
		fact.factKind = destructureStaticFact
		fact.staticVals = append([]Expression(nil), staticVals...)
		if choice, correlated := c.staticValueChoiceForExpression(value); correlated {
			fact.staticChoice = cloneCheckStaticChoiceFact(choice)
		}
	}
	return fact
}

func (c *scriptChecker) refreshCapturedDestructureContainerFact(
	fact capturedDestructureValueFact,
) capturedDestructureValueFact {
	// A typed projection has no concrete expression to recapture. If an
	// earlier LHS call poisoned its retained container, discard the projected
	// contents. A rebind advances the root generation but does not change the
	// already-evaluated container reference, so that snapshot stays valid.
	for _, root := range fact.retainedRoots {
		if c.localBindingGenerations[root.name] != root.generation {
			continue
		}
		if _, poisoned := c.typePoison[root.name]; poisoned {
			fact.assigned = nil
			fact.classNames = nil
			fact.instanceOrigins = nil
			fact.instanceExact = false
			fact.callables = nil
			fact.staticVals = nil
			fact.staticChoice = checkStaticChoiceFact{}
			fact.factKind = 0
			return fact
		}
	}
	if capturedDestructureStaticChoiceExact(fact) {
		return fact
	}
	if fact.value == nil || !c.typeExprHasContainerArm(fact.assigned) {
		return fact
	}
	// Earlier LHS leaves may mutate a container captured by a later leaf.
	// Refresh its contents while preserving scalar leaves as the values read
	// when the RHS was evaluated.
	refreshed := c.capturedDestructureValueFact(fact.value)
	refreshed.target = fact.target
	refreshed.declared = fact.declared
	return refreshed
}

func (c *scriptChecker) capturedDestructureArrayFact(array *ArrayLiteral) (capturedDestructureValueFact, bool) {
	if array == nil {
		return capturedDestructureValueFact{}, false
	}
	fact := capturedDestructureValueFact{
		value:     array,
		known:     true,
		evaluated: true,
	}
	elementTypes := make([]*TypeExpr, 0, len(array.Elements))
	sawUnknownType := false
	alternatives := []*ArrayLiteral{{Position: array.Position}}
	staticExact := true
	for _, element := range array.Elements {
		elementFact, captured := c.evaluatedDestructureFacts[element]
		if !captured {
			elementFact, captured = c.destructureProjectionFacts[element]
		}
		if !captured {
			return capturedDestructureValueFact{}, false
		}
		fact.retainedRoots = mergeCapturedContainerRoots(
			fact.retainedRoots,
			elementFact.identityRoots,
			elementFact.retainedRoots,
		)
		if elementFact.assigned == nil {
			sawUnknownType = true
		} else {
			elementTypes = append(elementTypes, elementFact.assigned)
		}
		elementValues := elementFact.staticVals
		if len(elementValues) == 0 {
			elementValues = []Expression{
				c.newDestructureProjection(elementFact, element.Pos()),
			}
		}
		if len(elementValues) == 0 || len(alternatives)*len(elementValues) > 32 {
			staticExact = false
			continue
		}
		if !staticExact {
			continue
		}
		next := make([]*ArrayLiteral, 0, len(alternatives)*len(elementValues))
		for _, prefix := range alternatives {
			for _, value := range elementValues {
				candidate := &ArrayLiteral{
					Elements: append(append([]Expression(nil), prefix.Elements...), value),
					Position: array.Position,
				}
				next = append(next, candidate)
			}
		}
		alternatives = next
	}
	if len(elementTypes) == 0 {
		fact.assigned = checkTypeArray
	} else {
		marker := literalElementsMarker
		if sawUnknownType {
			marker = literalPartialElementsMarker
		}
		fact.assigned = &TypeExpr{
			Kind:     TypeArray,
			Name:     marker,
			TypeArgs: []*TypeExpr{unionTypeExprs(elementTypes...)},
		}
	}
	if staticExact {
		fact.factKind = destructureStaticFact
		fact.staticVals = make([]Expression, len(alternatives))
		for i, alternative := range alternatives {
			fact.staticVals[i] = alternative
		}
	}
	return fact, true
}

func mergeCapturedContainerRoots(
	roots []capturedContainerRoot,
	groups ...[]capturedContainerRoot,
) []capturedContainerRoot {
	for _, group := range groups {
		for _, candidate := range group {
			duplicate := slices.Contains(roots, candidate)
			if !duplicate {
				roots = append(roots, candidate)
			}
		}
	}
	return roots
}

func typeExprDefinitelyDestructuresAsScalar(ty *TypeExpr) bool {
	// This probe runs for every non-literal destructure, and typeExprArms
	// allocates per arm, so an unbudgeted flatten of one wide declared union
	// bills every destructure that mentions it. Too complex to size is the
	// conservative answer here: the caller falls through to the projection,
	// which abandons the same type against the same budget.
	if !typeExprFactSizeWithin(ty, maxTypedDestructureProjections) {
		return false
	}
	arms, exact := typeExprArms(ty, 0)
	if !exact || len(arms) == 0 {
		return false
	}
	for _, arm := range arms {
		if arm.Kind == TypeArray {
			return false
		}
	}
	return true
}

func (c *scriptChecker) captureTypedDestructureValueFacts(
	target *DestructureTarget,
	valueFact capturedDestructureValueFact,
) ([]capturedDestructureValueFact, bool) {
	retainedRoots := mergeCapturedContainerRoots(
		nil,
		valueFact.identityRoots,
		valueFact.retainedRoots,
	)
	return c.captureTypedDestructureValueFactsWithRoots(
		target,
		valueFact.assigned,
		retainedRoots,
	)
}

func (c *scriptChecker) captureTypedDestructureValueFactsWithRoots(
	target *DestructureTarget,
	valueType *TypeExpr,
	retainedRoots []capturedContainerRoot,
) ([]capturedDestructureValueFact, bool) {
	types, projected := typedDestructureElementTypes(target, valueType)
	if !projected {
		return nil, false
	}
	var facts []capturedDestructureValueFact
	for i, element := range target.Elements {
		assigned := types[i]
		retainsRoots := c.projectedDestructureElementRetainsRoots(element, assigned)
		elementRoots := retainedRoots
		if !retainsRoots {
			elementRoots = nil
		}
		switch elementTarget := element.Target.(type) {
		case *DestructureTarget:
			fact := capturedDestructureValueFact{
				target:    elementTarget,
				assigned:  assigned,
				declared:  element.Type,
				known:     assigned != nil,
				evaluated: assigned != nil,
			}
			if c.typeExprHasContainerArm(assigned) && len(elementRoots) > 0 {
				fact.retainedRoots = append(
					[]capturedContainerRoot(nil),
					elementRoots...,
				)
			}
			facts = append(facts, fact)
		default:
			if elementTarget == nil {
				continue
			}
			// A projected container without a durable source root may still
			// be mutated through an untracked alias. Keep immediate target
			// checks, but do not persist that type on a local.
			if _, local := elementTarget.(*Identifier); local &&
				retainsRoots && len(elementRoots) == 0 {
				assigned = nil
			}
			fact := capturedDestructureValueFact{
				target:    elementTarget,
				assigned:  assigned,
				declared:  element.Type,
				known:     assigned != nil,
				evaluated: assigned != nil,
			}
			if c.typeExprHasContainerArm(assigned) && len(elementRoots) > 0 {
				fact.retainedRoots = append(
					[]capturedContainerRoot(nil),
					elementRoots...,
				)
			}
			facts = append(facts, fact)
		}
	}
	return facts, true
}

// maxTypedDestructureProjections budgets one destructure projection, counted in
// canonicalized type nodes. The projection derives a candidate type per target
// and joins it, and joining canonicalizes the whole candidate, so the work is
// the target count times the value type's size. Declared annotations are capped
// neither to maxInferredUnionArms nor in nesting, so a wide or deeply wrapped
// declared union destructured into many targets would otherwise make that
// product quadratic in the source size, inside a static check that runs outside
// the runtime's step and memory quotas. Past the budget the projection gives up
// and the caller falls back to unknown element facts.
const maxTypedDestructureProjections = 1024

func typedDestructureElementTypes(
	target *DestructureTarget,
	valueType *TypeExpr,
) ([]*TypeExpr, bool) {
	if target == nil {
		return nil, false
	}
	// Size the value type before flattening it: typeExprArms allocates per arm,
	// so charging the budget only afterwards would let a wide union bill the
	// flatten to every destructure that mentions it. Divide rather than
	// multiply so a hostile target count cannot overflow the product.
	if len(target.Elements) > 0 && !typeExprFactSizeWithin(
		valueType,
		maxTypedDestructureProjections/len(target.Elements),
	) {
		return nil, false
	}
	arms, exact := typeExprArms(valueType, 0)
	if !exact || len(arms) == 0 {
		return nil, false
	}
	options := make([][]*TypeExpr, len(target.Elements))
	unknown := make([]bool, len(target.Elements))
	for _, arm := range arms {
		projected := destructureTypeArmElementTypes(target, arm)
		for i, ty := range projected {
			if ty == nil {
				unknown[i] = true
				continue
			}
			options[i] = append(options[i], ty)
		}
	}
	result := make([]*TypeExpr, len(target.Elements))
	for i, candidates := range options {
		if unknown[i] || len(candidates) == 0 {
			continue
		}
		result[i] = unionTypeExprs(candidates...)
	}
	return result, true
}

func destructureTypeArmElementTypes(
	target *DestructureTarget,
	arm *TypeExpr,
) []*TypeExpr {
	if arm.Kind != TypeArray {
		return destructureSingleValueElementTypes(target, arm)
	}
	result := make([]*TypeExpr, len(target.Elements))
	elementType := splattedElementBound(arm)
	restType := checkTypeArray
	if elementType != nil {
		restType = &TypeExpr{
			Kind:     TypeArray,
			TypeArgs: []*TypeExpr{elementType},
		}
	}
	for i, element := range target.Elements {
		if element.Rest {
			result[i] = restType
		} else if elementType != nil {
			result[i] = unionTypeExprs(elementType, checkTypeNil)
		}
	}
	return result
}

func destructureSingleValueElementTypes(
	target *DestructureTarget,
	valueType *TypeExpr,
) []*TypeExpr {
	source := []*TypeExpr{valueType}
	valueAt := func(index int) *TypeExpr {
		if index < 0 || index >= len(source) {
			return checkTypeNil
		}
		return source[index]
	}

	result := make([]*TypeExpr, len(target.Elements))
	restIndex := -1
	for i, element := range target.Elements {
		if element.Rest {
			restIndex = i
			break
		}
	}
	if restIndex < 0 {
		for i := range target.Elements {
			result[i] = valueAt(i)
		}
		return result
	}

	trailing := len(target.Elements) - restIndex - 1
	restStart := min(restIndex, len(source))
	restEnd := max(restStart, len(source)-trailing)
	for i := range target.Elements {
		switch {
		case i < restIndex:
			result[i] = valueAt(i)
		case i == restIndex:
			if restStart == restEnd {
				result[i] = checkTypeArray
			} else {
				result[i] = &TypeExpr{
					Kind:     TypeArray,
					TypeArgs: []*TypeExpr{valueType},
				}
			}
		default:
			result[i] = valueAt(restEnd + i - restIndex - 1)
		}
	}
	return result
}

// projectedDestructureElementRetainsRoots reports whether the projected
// value may share a mutable container with its source. A rest target always
// receives a fresh outer array, so scalar elements retain no source roots.
func (c *scriptChecker) projectedDestructureElementRetainsRoots(
	element DestructureElement,
	assigned *TypeExpr,
) bool {
	if !c.typeExprHasContainerArm(assigned) {
		return false
	}
	if !element.Rest {
		return true
	}
	arms, exact := typeExprArms(assigned, 0)
	if !exact {
		return true
	}
	for _, arm := range arms {
		if arm.Kind != TypeArray {
			return true
		}
		elementType := splattedElementBound(arm)
		elementArms, elementExact := typeExprArms(elementType, 0)
		if !elementExact {
			return true
		}
		for _, elementArm := range elementArms {
			switch elementArm.Kind {
			case TypeArray, TypeHash, TypeShape:
				return true
			}
		}
	}
	return false
}

func captureUnknownDestructureValueFacts(target *DestructureTarget) []capturedDestructureValueFact {
	if target == nil {
		return nil
	}
	var facts []capturedDestructureValueFact
	for _, element := range target.Elements {
		switch elementTarget := element.Target.(type) {
		case *DestructureTarget:
			facts = append(facts, capturedDestructureValueFact{
				target:   elementTarget,
				declared: element.Type,
			})
		default:
			if elementTarget == nil {
				continue
			}
			fact := capturedDestructureValueFact{
				target:   elementTarget,
				declared: element.Type,
			}
			if element.Rest {
				// A named rest always materializes an array even when its
				// element types are unknown.
				fact.assigned = checkTypeAnyArray
				fact.known = true
				fact.evaluated = true
			}
			facts = append(facts, fact)
		}
	}
	return facts
}

// expandCapturedNestedDestructureFact projects a nested level only when replay
// reaches it, after refreshing container facts changed by earlier siblings.
func (c *scriptChecker) expandCapturedNestedDestructureFact(
	fact capturedDestructureValueFact,
) []capturedDestructureValueFact {
	target, ok := fact.target.(*DestructureTarget)
	if !ok || target == nil {
		return nil
	}
	fact = c.refreshCapturedDestructureContainerFact(fact)
	if fact.value != nil {
		return c.captureDestructureValueFacts(target, fact.value)
	}
	if fact.known {
		if facts, projected := c.captureTypedDestructureValueFacts(target, fact); projected {
			return facts
		}
	}
	return captureUnknownDestructureValueFacts(target)
}

// flattenCapturedDestructureValueFacts preserves the leaf-only contract used
// by inference passes that do not replay target effects.
func (c *scriptChecker) flattenCapturedDestructureValueFacts(
	facts []capturedDestructureValueFact,
) []capturedDestructureValueFact {
	var flattened []capturedDestructureValueFact
	for _, fact := range facts {
		if _, nested := fact.target.(*DestructureTarget); nested {
			flattened = append(
				flattened,
				c.flattenCapturedDestructureValueFacts(
					c.expandCapturedNestedDestructureFact(fact),
				)...,
			)
			continue
		}
		flattened = append(flattened, fact)
	}
	return flattened
}

func (c *scriptChecker) bindCapturedDestructureValueFacts(facts []capturedDestructureValueFact) {
	for _, fact := range facts {
		c.bindCapturedDestructureValueFact(fact)
	}
}

func (c *scriptChecker) bindCapturedDestructureValueFact(fact capturedDestructureValueFact) {
	target, ok := fact.target.(*Identifier)
	if !ok || target == nil {
		return
	}

	sourceCurrent := fact.sourceName != "" &&
		c.localBindingGenerations[fact.sourceName] == fact.sourceGen
	c.advanceLocalBindingGeneration(target.Name)
	if sourceCurrent {
		fact.assigned = c.localTypeFor(fact.sourceName)
		if fact.sourceName != target.Name &&
			(fact.assigned == nil || c.typeExprHasContainerArm(fact.assigned)) {
			c.linkContainerIdentityAlias(target.Name, fact.sourceName)
			c.linkStaticValueAlias(target.Name, fact.sourceName)
		}
	}
	for _, root := range fact.identityRoots {
		if root.name == target.Name ||
			c.localBindingGenerations[root.name] != root.generation {
			continue
		}
		c.linkContainerIdentityAlias(target.Name, root.name)
		c.linkStaticValueAlias(target.Name, root.name)
	}
	for _, root := range fact.retainedRoots {
		if root.name == target.Name ||
			c.localBindingGenerations[root.name] != root.generation {
			continue
		}
		c.linkContainerAlias(target.Name, root.name)
		c.linkStaticValueDependency(root.name, target.Name)
	}

	if fact.known && c.capturedDestructureAliasStillCurrent(fact) {
		c.linkContainerAssignmentAlias(target.Name, fact.value, fact.assigned)
		if fact.assigned == nil || c.typeExprHasContainerArm(fact.assigned) {
			for _, root := range c.containerAliasRoots(fact.value) {
				c.linkContainerAlias(target.Name, root)
			}
		}
	}

	c.bindLocalType(target.Name, fact.declared)
	c.bindLocalClassValue(target.Name, "")
	if fact.known && fact.declared == nil {
		c.bindLocalType(target.Name, fact.assigned)
	}
	if !fact.known {
		return
	}
	switch fact.factKind {
	case destructureClassFact:
		c.bindLocalClassValues(target.Name, fact.classNames)
	case destructureCallableFact:
		c.bindLocalCallableValues(target.Name, fact.callables)
	case destructureStaticFact:
		c.bindLocalStaticValuesWithChoice(target.Name, fact.staticVals, fact.staticChoice)
	}
}

func (c *scriptChecker) evaluatedStaticValueExpressionAlternatives(
	expr Expression,
) ([]Expression, bool) {
	const maxAlternatives = 32
	merge := func(alternatives []Expression, expr Expression) ([]Expression, bool) {
		values, exact := c.evaluatedStaticValueExpressionAlternatives(expr)
		if !exact || len(alternatives)+len(values) > maxAlternatives {
			return nil, false
		}
		return c.normalizeCheckStaticValues(append(alternatives, values...)), true
	}

	switch typed := expr.(type) {
	case *ConditionalExpr:
		baseScopeState := c.snapshotScopeState()
		defer c.restoreScopeState(baseScopeState)

		var alternatives []Expression
		c.restoreScopeState(baseScopeState)
		if c.applyConditionOutcomeEffects(typed.Condition, true, nil) {
			var exact bool
			alternatives, exact = merge(alternatives, typed.Consequent)
			if !exact {
				return nil, false
			}
		}
		c.restoreScopeState(baseScopeState)
		if c.applyConditionOutcomeEffects(typed.Condition, false, nil) {
			var exact bool
			alternatives, exact = merge(alternatives, typed.Alternate)
			if !exact {
				return nil, false
			}
		}
		return alternatives, len(alternatives) > 0
	case *IfExpr:
		baseScopeState := c.snapshotScopeState()
		defer c.restoreScopeState(baseScopeState)

		var alternatives []Expression
		collectCondition := func(condition, result Expression) (bool, bool) {
			conditionScopeState := c.snapshotScopeState()
			if c.applyConditionOutcomeEffects(condition, true, nil) {
				var exact bool
				alternatives, exact = merge(alternatives, result)
				if !exact {
					return false, false
				}
			}
			c.restoreScopeState(conditionScopeState)
			return c.applyConditionOutcomeEffects(condition, false, nil), true
		}

		falseReachable, exact := collectCondition(typed.Condition, typed.Consequent)
		if !exact {
			return nil, false
		}
		for _, branch := range typed.ElseIf {
			if !falseReachable {
				break
			}
			falseReachable, exact = collectCondition(branch.Condition, branch.Result)
			if !exact {
				return nil, false
			}
		}
		if falseReachable {
			alternatives, exact = merge(alternatives, typed.Alternate)
			if !exact {
				return nil, false
			}
		}
		return alternatives, len(alternatives) > 0
	case *RescueExpr:
		if expressionProvenNonRaising(typed.Body) {
			return c.evaluatedStaticValueExpressionAlternatives(typed.Body)
		}
	}
	return c.staticValueExpressionAlternatives(expr)
}

func (c *scriptChecker) staticValueExpressionAlternatives(expr Expression) ([]Expression, bool) {
	const maxAlternatives = 32
	merge := func(left []Expression, leftOK bool, right []Expression, rightOK bool) ([]Expression, bool) {
		if !leftOK || !rightOK || len(left)+len(right) > maxAlternatives {
			return nil, false
		}
		merged := c.normalizeCheckStaticValues(append(append([]Expression(nil), left...), right...))
		return merged, len(merged) > 0
	}

	if fact, captured := c.evaluatedDestructureFacts[expr]; captured &&
		fact.factKind == destructureStaticFact && len(fact.staticVals) > 0 {
		return append([]Expression(nil), fact.staticVals...), true
	}

	switch typed := expr.(type) {
	case *ArrayLiteral:
		if c.checkStaticValueCandidate(typed) {
			return []Expression{typed}, true
		}
		for _, element := range typed.Elements {
			if _, splat := element.(*SplatArg); splat {
				return nil, false
			}
			values, exact := c.staticValueExpressionAlternatives(element)
			if !exact || len(values) != 1 {
				return nil, false
			}
		}
		return []Expression{typed}, true
	case *Identifier:
		return c.localStaticValuesFor(typed.Name)
	case *ConditionalExpr:
		if branch, known := staticConditionalExpressionBranch(typed); known {
			return c.staticValueExpressionAlternatives(branch)
		}
		left, leftOK := c.staticValueExpressionAlternatives(typed.Consequent)
		right, rightOK := c.staticValueExpressionAlternatives(typed.Alternate)
		return merge(left, leftOK, right, rightOK)
	case *IfExpr:
		if branch, known := c.inferredIfExpressionBranch(typed); known {
			return c.staticValueExpressionAlternatives(branch)
		}
		branches := make([]Expression, 0, len(typed.ElseIf)+2)
		branches = append(branches, typed.Consequent)
		for _, branch := range typed.ElseIf {
			branches = append(branches, branch.Result)
		}
		branches = append(branches, typed.Alternate)
		var alternatives []Expression
		for _, branch := range branches {
			values, ok := c.staticValueExpressionAlternatives(branch)
			if !ok || len(alternatives)+len(values) > maxAlternatives {
				return nil, false
			}
			alternatives = c.normalizeCheckStaticValues(append(alternatives, values...))
		}
		return alternatives, len(alternatives) > 0
	case *RescueExpr:
		left, leftOK := c.staticValueExpressionAlternatives(typed.Body)
		right, rightOK := c.staticValueExpressionAlternatives(typed.Fallback)
		return merge(left, leftOK, right, rightOK)
	case *BinaryExpr:
		if typed.Operator != tokenAnd && typed.Operator != tokenOr {
			break
		}
		truthy, known := staticExpressionTruthiness(typed.Left)
		if !known {
			break
		}
		if truthy == (typed.Operator == tokenAnd) {
			return c.staticValueExpressionAlternatives(typed.Right)
		}
		return c.staticValueExpressionAlternatives(typed.Left)
	case *IndexExpr:
		projected, ok := c.staticLiteralProjections(typed)
		if !ok {
			break
		}
		var alternatives []Expression
		for _, candidate := range projected {
			values, exact := c.staticValueExpressionAlternatives(candidate)
			if !exact {
				return nil, false
			}
			alternatives = c.normalizeCheckStaticValues(append(alternatives, values...))
		}
		return alternatives, len(alternatives) > 0
	case *CaseExpr:
		if result, ok := c.inferredCaseExpressionResult(typed); ok {
			return c.staticValueExpressionAlternatives(result)
		}
	}
	if c.checkStaticValueCandidate(expr) {
		return []Expression{expr}, true
	}
	return nil, false
}

// keywordSplatExpressionAlwaysFails recognizes values that are guaranteed to
// abort `**` expansion. The dedicated local witness covers a hash that was
// successfully mutated with a non-string/non-symbol key; its ordinary type is
// intentionally poisoned because the mutation invalidates structural facts.
func (c *scriptChecker) keywordSplatExpressionAlwaysFails(expr Expression) bool {
	switch typed := expr.(type) {
	case nil:
		return true
	case *Identifier:
		if c.localKeywordSplatFails(typed.Name) {
			return true
		}
	case *ConditionalExpr:
		if branch, ok := staticConditionalExpressionBranch(typed); ok {
			return c.keywordSplatExpressionAlwaysFails(branch)
		}
		return c.keywordSplatExpressionAlwaysFails(typed.Consequent) &&
			c.keywordSplatExpressionAlwaysFails(typed.Alternate)
	case *IfExpr:
		if !c.keywordSplatExpressionAlwaysFails(typed.Consequent) ||
			!c.keywordSplatExpressionAlwaysFails(typed.Alternate) {
			return false
		}
		for _, branch := range typed.ElseIf {
			if !c.keywordSplatExpressionAlwaysFails(branch.Result) {
				return false
			}
		}
		return true
	case *RescueExpr:
		return c.keywordSplatExpressionAlwaysFails(typed.Body) &&
			c.keywordSplatExpressionAlwaysFails(typed.Fallback)
	case *HashLiteral:
		if typed.ShapeType != nil && !c.hashShapeStaticallyShadowed(typed) {
			return true
		}
		for _, pair := range typed.Pairs {
			key, static := staticLiteralValue(pair.Key)
			if static && key.Kind() != KindString && key.Kind() != KindSymbol {
				return true
			}
		}
	}
	return !c.expressionMayHaveExpansionType(expr, KindHash, checkTypeHash)
}

// classValueExpressionNames returns the exhaustive script-class identities an
// expression can produce. It recognizes value-preserving branches and literal
// projections so dynamic dispatch can schedule only methods the receiver can
// actually reach; an incomplete result is rejected instead of widening to all
// same-named methods and hiding their independent pristine checks.
func (c *scriptChecker) classValueExpressionNames(expr Expression) ([]string, bool) {
	c.speculativeInference++
	defer func() { c.speculativeInference-- }()
	return c.classValueExpressionNamesSeen(expr, nil, false)
}

func (c *scriptChecker) dispatchClassValueExpressionNames(expr Expression) ([]string, bool) {
	c.speculativeInference++
	defer func() { c.speculativeInference-- }()
	return c.classValueExpressionNamesSeen(expr, nil, true)
}

func (c *scriptChecker) classValueExpressionNamesSeen(
	expr Expression,
	seen map[*ScriptFunction]struct{},
	allowFunctionReturns bool,
) ([]string, bool) {
	if fact, captured := c.destructureProjectionFacts[expr]; captured &&
		fact.factKind == destructureClassFact && len(fact.classNames) > 0 {
		return append([]string(nil), fact.classNames...), true
	}
	switch typed := expr.(type) {
	case *Identifier:
		if classNames, ok := c.localClassValuesFor(typed.Name); ok {
			return classNames, true
		}
		if typed.Name == "self" && c.selfClass != nil && c.selfClassContext {
			return []string{c.selfClass.Name}, true
		}
		classDef, ok := c.staticClassArgument(typed)
		if !ok {
			return nil, false
		}
		return []string{classDef.Name}, true
	case *ScopeExpr:
		classDef, ok := c.staticClassArgument(typed)
		if !ok {
			return nil, false
		}
		return []string{classDef.Name}, true
	case *ConditionalExpr:
		if branch, ok := staticConditionalExpressionBranch(typed); ok {
			return c.classValueExpressionNamesSeen(branch, seen, allowFunctionReturns)
		}
		left, leftOK := c.classValueExpressionNamesSeen(typed.Consequent, seen, allowFunctionReturns)
		right, rightOK := c.classValueExpressionNamesSeen(typed.Alternate, seen, allowFunctionReturns)
		return mergeCheckStringCandidates(left, leftOK, right, rightOK)
	case *IfExpr:
		if classNames, evaluated := c.evaluatedIfClassFacts[typed]; evaluated {
			return append([]string(nil), classNames...), true
		}
		if branch, known := c.inferredIfExpressionBranch(typed); known {
			return c.classValueExpressionNamesSeen(branch, seen, allowFunctionReturns)
		}
		branches := make([]Expression, 0, len(typed.ElseIf)+2)
		branches = append(branches, typed.Consequent)
		for _, branch := range typed.ElseIf {
			branches = append(branches, branch.Result)
		}
		branches = append(branches, typed.Alternate)
		return c.mergeClassValueExpressionCandidates(branches, seen, allowFunctionReturns)
	case *RescueExpr:
		body, bodyOK := c.classValueExpressionNamesSeen(typed.Body, seen, allowFunctionReturns)
		fallback, fallbackOK := c.classValueExpressionNamesSeen(typed.Fallback, seen, allowFunctionReturns)
		return mergeCheckStringCandidates(body, bodyOK, fallback, fallbackOK)
	case *BinaryExpr:
		if typed.Operator != tokenAnd && typed.Operator != tokenOr {
			return nil, false
		}
		if truthy, known := staticExpressionTruthiness(typed.Left); known {
			if truthy == (typed.Operator == tokenAnd) {
				return c.classValueExpressionNamesSeen(typed.Right, seen, allowFunctionReturns)
			}
			return c.classValueExpressionNamesSeen(typed.Left, seen, allowFunctionReturns)
		}
		if left, ok := c.classValueExpressionNamesSeen(typed.Left, seen, allowFunctionReturns); ok {
			if typed.Operator == tokenOr {
				return left, true
			}
			return c.classValueExpressionNamesSeen(typed.Right, seen, allowFunctionReturns)
		}
		return nil, false
	case *IndexExpr:
		projected, ok := c.staticLiteralProjections(typed)
		if !ok {
			return nil, false
		}
		var merged []string
		for _, candidate := range projected {
			names, exact := c.classValueExpressionNamesSeen(candidate, seen, allowFunctionReturns)
			if !exact {
				return nil, false
			}
			if merged == nil {
				merged = names
				continue
			}
			merged, _ = mergeCheckStringCandidates(merged, true, names, true)
		}
		return merged, len(merged) > 0
	case *CallExpr:
		if member, ok := typed.Callee.(*MemberExpr); ok && member.Property == "itself" &&
			len(typed.Args) == 0 && len(typed.KwArgs) == 0 && typed.Block == nil {
			candidates, exact := c.classValueExpressionNamesSeen(member.Object, seen, allowFunctionReturns)
			if !exact {
				return nil, false
			}
			for _, className := range candidates {
				classDef := c.script.classes[className]
				if classDef == nil || classDef.ClassMethods["itself"] != nil {
					return nil, false
				}
			}
			return candidates, true
		}
		if !allowFunctionReturns {
			return nil, false
		}
		target, ok := c.resolveCallable(typed)
		if !ok || target.fn == nil || target.constructor || len(target.fn.Body) != 1 {
			return nil, false
		}
		if seen == nil {
			seen = make(map[*ScriptFunction]struct{})
		}
		if _, recursive := seen[target.fn]; recursive {
			return nil, false
		}
		seen[target.fn] = struct{}{}
		defer delete(seen, target.fn)
		switch stmt := target.fn.Body[0].(type) {
		case *ExprStmt:
			return c.classValueExpressionNamesSeen(stmt.Expr, seen, true)
		case *ReturnStmt:
			return c.classValueExpressionNamesSeen(stmt.Value, seen, true)
		}
	}
	return nil, false
}

func (c *scriptChecker) mergeClassValueExpressionCandidates(
	branches []Expression,
	seen map[*ScriptFunction]struct{},
	allowFunctionReturns bool,
) ([]string, bool) {
	var merged []string
	for _, branch := range branches {
		candidates, ok := c.classValueExpressionNamesSeen(branch, seen, allowFunctionReturns)
		if !ok {
			return nil, false
		}
		if merged == nil {
			merged = candidates
			continue
		}
		merged, _ = mergeCheckStringCandidates(merged, true, candidates, true)
	}
	return merged, len(merged) > 0
}

func mergeCheckStringCandidates(left []string, leftOK bool, right []string, rightOK bool) ([]string, bool) {
	if !leftOK || !rightOK || len(left) == 0 || len(right) == 0 {
		return nil, false
	}
	merged := append([]string(nil), left...)
	seen := make(map[string]struct{}, len(left)+len(right))
	for _, candidate := range left {
		seen[candidate] = struct{}{}
	}
	for _, candidate := range right {
		if _, duplicate := seen[candidate]; duplicate {
			continue
		}
		seen[candidate] = struct{}{}
		merged = append(merged, candidate)
	}
	return normalizeCheckClassNames(merged), true
}

func (c *scriptChecker) staticValueChoiceForExpression(
	expr Expression,
) (checkStaticChoiceFact, bool) {
	switch typed := expr.(type) {
	case *Identifier:
		return c.localStaticChoiceFor(typed.Name)
	case *IndexExpr:
		if len(typed.Indices) != 1 {
			return checkStaticChoiceFact{}, false
		}
		object, direct := typed.Object.(*Identifier)
		if !direct {
			return checkStaticChoiceFact{}, false
		}
		alternatives, exact := c.localStaticValuesFor(object.Name)
		if !exact || len(alternatives) == 0 {
			return checkStaticChoiceFact{}, false
		}
		projected := make([]Expression, 0, len(alternatives))
		indices := make([]int, 0, len(alternatives))
		for choice, alternative := range alternatives {
			value, ok := c.staticLiteralProjectionFrom(alternative, typed.Indices[0])
			if !ok {
				return checkStaticChoiceFact{}, false
			}
			values, valueExact := c.staticValueExpressionAlternatives(value)
			if !valueExact || len(values) != 1 {
				return checkStaticChoiceFact{}, false
			}
			projected = append(projected, values[0])
			indices = append(indices, choice)
		}
		normalized := c.normalizeCheckStaticValues(projected)
		if len(normalized) != len(projected) {
			return checkStaticChoiceFact{}, false
		}
		for i := range normalized {
			if normalized[i] != projected[i] {
				return checkStaticChoiceFact{}, false
			}
		}
		return checkStaticChoiceFact{
			source:  c.checkCallSplatSourceForLocal(object.Name, alternatives),
			indices: indices,
		}, true
	}
	return checkStaticChoiceFact{}, false
}

func (c *scriptChecker) staticLiteralProjections(expr *IndexExpr) ([]Expression, bool) {
	if expr == nil || len(expr.Indices) != 1 {
		return nil, false
	}
	objects := []Expression{expr.Object}
	switch expr.Object.(type) {
	case *ArrayLiteral, *HashLiteral:
	default:
		var exact bool
		objects, exact = c.callStaticValueAlternatives(expr.Object)
		if !exact {
			return nil, false
		}
	}
	indices, exact := c.callStaticValueAlternatives(expr.Indices[0])
	if !exact {
		return nil, false
	}
	const maxProjectionAlternatives = 32
	if len(indices) == 0 || len(objects) > maxProjectionAlternatives/len(indices) {
		return nil, false
	}
	projected := make([]Expression, 0, len(objects)*len(indices))
	for _, object := range objects {
		for _, index := range indices {
			value, ok := c.staticLiteralProjectionFrom(object, index)
			if !ok {
				return nil, false
			}
			projected = append(projected, value)
		}
	}
	return projected, len(projected) > 0
}

func (c *scriptChecker) staticLiteralProjectionFrom(object, indexExpr Expression) (Expression, bool) {
	switch object := object.(type) {
	case *ArrayLiteral:
		for _, element := range object.Elements {
			if _, splat := element.(*SplatArg); splat {
				return nil, false
			}
		}
		value, ok := staticLiteralValue(indexExpr)
		if !ok {
			return nil, false
		}
		index, ok := staticArrayFetchIndex(value)
		if !ok {
			return nil, false
		}
		if index < 0 {
			index += int64(len(object.Elements))
		}
		if index < 0 || index >= int64(len(object.Elements)) {
			return nil, false
		}
		return object.Elements[index], true
	case *HashLiteral:
		if object.ShapeType != nil && !c.hashShapeStaticallyShadowed(object) {
			return nil, false
		}
		want, ok := staticLiteralHashIdentity(indexExpr)
		if !ok {
			return nil, false
		}
		var projected Expression
		for _, pair := range object.Pairs {
			key, ok := staticLiteralHashIdentity(pair.Key)
			if !ok {
				return nil, false
			}
			if key == want {
				projected = pair.Value
			}
		}
		return projected, projected != nil
	}
	return nil, false
}

// bindParamLocalType seeds a parameter's declared type into the current
// frame: annotated parameters enter the body with their declared type,
// unannotated ones with an explicit unknown fact.
func (c *scriptChecker) bindParamLocalType(param Param) {
	if param.Name != "" {
		c.bindLocalTypeInCurrentFrame(param.Name, param.Type)
	}
	if target, ok := param.Target.(*DestructureTarget); ok {
		for _, element := range target.Elements {
			c.bindDestructureParamElementType(element)
		}
	}
}

func (c *scriptChecker) bindDestructureParamElementType(element DestructureElement) {
	switch target := element.Target.(type) {
	case *Identifier:
		c.bindLocalTypeInCurrentFrame(target.Name, element.Type)
	case *DestructureTarget:
		for _, nested := range target.Elements {
			c.bindDestructureParamElementType(nested)
		}
	}
}

// typeFactForValue derives a static fact from a concrete host-supplied
// argument value, so per-call checks treat CLI and host arguments like local
// literals. Kinds the inference does not model stay unknown.
func typeFactForValue(val Value) *TypeExpr {
	switch val.Kind() {
	case KindNil:
		return checkTypeNil
	case KindBool:
		return checkTypeBool
	case KindInt:
		return checkTypeInt
	case KindFloat:
		return checkTypeFloat
	case KindString:
		return checkTypeString
	case KindSymbol:
		return checkTypeSymbol
	case KindRange:
		return checkTypeRange
	case KindDuration:
		return checkTypeDuration
	case KindTime:
		return checkTypeTime
	case KindMoney:
		return checkTypeMoney
	case KindArray:
		elements := val.Array()
		if len(elements) == 0 {
			return checkTypeArray
		}
		arms := make([]*TypeExpr, 0, len(elements))
		for _, element := range elements {
			arm := typeFactForValue(element)
			if arm == nil {
				return checkTypeArray
			}
			arms = append(arms, arm)
		}
		union := unionTypeExprs(arms...)
		if union == nil {
			return checkTypeArray
		}
		return &TypeExpr{Kind: TypeArray, Name: literalElementsMarker, TypeArgs: []*TypeExpr{union}}
	case KindHash, KindObject:
		return checkTypeHash
	case KindShape:
		if shape := valueShape(val); shape != nil {
			return shapeValueType(shape)
		}
	}
	return nil
}

// bindParamValueFact refines an unannotated parameter with the concrete
// argument value a per-call check received; annotated parameters keep their
// declared seed (the value was validated against it already).
func (c *scriptChecker) bindParamValueFact(param Param, val Value, present bool) {
	if !present || param.Name == "" || param.Type != nil {
		return
	}
	fact := typeFactForValue(val)
	if param.Kind == ParamKeywordRest && val.Kind() == KindHash {
		fact = keywordRestFact(val.HashEntryMap())
	}
	if fact != nil {
		c.bindLocalTypeInCurrentFrame(param.Name, fact)
	}
}

// keywordRestFact models the hash the runtime binds for a keyword-rest
// parameter: raw keyword names make a string-keyed store, and the concrete
// argument facts fill an exact shape. Empty or unmodeled entries degrade to
// a plain hash fact.
func keywordRestFact(entries map[string]Value) *TypeExpr {
	if len(entries) == 0 {
		return checkTypeHash
	}
	shape := make(map[string]*TypeExpr, len(entries))
	for name, val := range entries {
		fact := typeFactForValue(val)
		if fact == nil {
			return checkTypeHash
		}
		shape[name] = fact
	}
	return &TypeExpr{Kind: TypeShape, Name: shapeKeysStringMarker, Shape: shape}
}

// bindParamDefaultFact refines an unannotated defaulted parameter with the
// default expression's inferred type when a per-call check omits the
// argument: the default is exactly the value the runtime binds.
func (c *scriptChecker) bindParamDefaultFact(param Param) {
	if param.Name == "" || param.Type != nil || param.DefaultVal == nil {
		return
	}
	if fact := c.inferExpressionType(param.DefaultVal); fact != nil {
		c.bindLocalTypeInCurrentFrame(param.Name, fact)
	}
}

// refineAnnotatedParamFact narrows an annotated parameter's declared seed
// with the concrete fact a per-call binding established: the runtime bound
// exactly one annotation arm, so arms the fact contradicts cannot be this
// call's value. Coercing arms survive (a named type is never disjoint from
// the fact that coerces into it), and a fact that eliminates nothing or
// everything keeps the declared seed.
func (c *scriptChecker) refineAnnotatedParamFact(param Param, fact *TypeExpr) {
	if param.Name == "" || param.Type == nil || fact == nil {
		return
	}
	arms, ok := typeExprArms(param.Type, 0)
	if !ok || len(arms) < 2 {
		return
	}
	resolve := c.checkNamedTypeResolver()
	kept := make([]*TypeExpr, 0, len(arms))
	for _, arm := range arms {
		if !typeExprsDisjoint(fact, arm, resolve) {
			kept = append(kept, arm)
		}
	}
	if len(kept) == 0 || len(kept) == len(arms) {
		return
	}
	if refined := unionTypeExprs(kept...); refined != nil {
		c.bindLocalTypeInCurrentFrame(param.Name, refined)
	}
}

// reassignmentConflicts reports whether rebinding a local of type current to
// a value of type next is a known contradiction. Unknowns never conflict, nil
// acts as the neutral initializer in both directions, numeric retyping widens
// instead of erroring (arithmetic freely mixes int and float), and container
// re-initialization (hash/shape literals of different shapes) stays legal.
func reassignmentConflicts(current, next *TypeExpr, resolve namedTypeResolver) bool {
	if current == nil || next == nil {
		return false
	}
	if typeExprIsNilOnly(current) || typeExprIsNilOnly(next) {
		return false
	}
	if typeExprNumericOnly(current) && typeExprNumericOnly(next) {
		return false
	}
	if typeExprHashLikeOnly(current) && typeExprHashLikeOnly(next) {
		return false
	}
	if typeExprArrayOnly(current) && typeExprArrayOnly(next) {
		return false
	}
	return typeExprsDisjoint(current, next, resolve)
}

// narrowLocalArms rebinds a local to the subset of its known arms that keep
// and reports whether that subset is reachable. Unknown, poisoned, and
// any-typed locals stay unknown, and a filter that changes nothing binds
// nothing. An empty subset proves the requested outcome cannot occur.
func (c *scriptChecker) narrowLocalArms(name string, keep func(*TypeExpr) bool) bool {
	current := c.localTypeFor(name)
	if current == nil {
		return true
	}
	arms, ok := typeExprArms(current, 0)
	if !ok || len(arms) == 0 {
		return true
	}
	kept := make([]*TypeExpr, 0, len(arms))
	for _, arm := range arms {
		if keep(arm) {
			kept = append(kept, arm)
		}
	}
	if len(kept) == 0 {
		return false
	}
	if len(kept) == len(arms) {
		return true
	}
	if refined := unionTypeExprs(kept...); refined != nil {
		c.bindLocalType(name, refined)
	}
	return true
}

// narrowLocalTruthiness refines a local after a truthiness test: the truthy
// path drops nil arms, and the falsy path keeps only arms that can be falsy
// (nil, and bool for false).
func (c *scriptChecker) narrowLocalTruthiness(name string, truthy bool) bool {
	if truthy {
		return c.narrowLocalArms(name, func(arm *TypeExpr) bool {
			return arm.Kind != TypeNil && (arm.Kind != TypeBool || arm.Name != boolFalseFactMarker)
		})
	}
	return c.narrowLocalArms(name, func(arm *TypeExpr) bool {
		if _, isShapeValue := shapeValuePayload(arm); isShapeValue {
			// Runtime shape values are always truthy.
			return false
		}
		return arm.Kind == TypeNil || (arm.Kind == TypeBool && arm.Name != boolTrueFactMarker)
	})
}

// narrowLocalNilness refines a local after an explicit nil test (`x == nil`,
// `x.nil?`): the nil path keeps only nil arms, and the non-nil path drops
// them (bool arms survive: false is falsy but not nil). The non-nil direction
// is always sound — a nil receiver or operand answers the test itself — but
// the nil-only direction trusts the test's positive answer, which a class
// instance can forge by overriding nil? or ==, so named arms disable it.
func (c *scriptChecker) narrowLocalNilness(name string, isNil bool) bool {
	if !isNil {
		return c.narrowLocalArms(name, func(arm *TypeExpr) bool { return arm.Kind != TypeNil })
	}
	arms, ok := typeExprArms(c.localTypeFor(name), 0)
	if !ok {
		return true
	}
	for _, arm := range arms {
		if arm.Kind == TypeEnum {
			return true
		}
	}
	return c.narrowLocalArms(name, func(arm *TypeExpr) bool { return arm.Kind == TypeNil })
}

// narrowNilPredicateMember narrows a bare `x.nil?` receiver. Safe navigation
// is excluded: `x&.nil?` yields nil (falsy) for a nil receiver, so its falsy
// path does not prove the receiver non-nil.
func (c *scriptChecker) narrowNilPredicateMember(member *MemberExpr, truthy bool) bool {
	if member == nil || member.Safe || member.Property != "nil?" {
		return true
	}
	ident, ok := member.Object.(*Identifier)
	if !ok {
		return true
	}
	return c.narrowLocalNilness(ident.Name, truthy)
}

// narrowClassPredicateMember narrows `x.is_a?(User)`, `x.kind_of?(Mod)`, and
// `x.instance_of?(User)` on a plain local against a statically resolved class
// or module argument. Both branches refine exactly: without inheritance an
// instance arm either always or never satisfies the predicate, so the true
// path keeps matching arms and the false path drops them. Narrowing applies
// only when every arm provably reaches the runtime universal predicate —
// named arms whose class overrides it, module-typed arms (their concrete
// class is unknown), and dynamic receivers stay unchanged.
func (c *scriptChecker) narrowClassPredicateMember(member *MemberExpr, arg Expression, truthy bool) bool {
	if member == nil || member.Safe {
		return true
	}
	exact := false
	switch member.Property {
	case isAMemberName, kindOfMemberName:
	case instanceOfMemberName:
		exact = true
	default:
		return true
	}
	ident, ok := member.Object.(*Identifier)
	if !ok {
		return true
	}
	want, ok := c.staticClassArgument(arg)
	if !ok {
		return true
	}
	arms, ok := typeExprArms(c.localTypeFor(ident.Name), 0)
	if !ok || len(arms) == 0 {
		return true
	}
	resolve := c.checkNamedTypeResolver()
	for _, arm := range arms {
		if !classPredicateArmUsesUniversalDispatch(arm, member.Property, resolve) {
			return true
		}
		if _, known := classPredicateArmMatch(arm, want, exact, resolve); !known {
			return true
		}
	}
	matches := func(arm *TypeExpr) bool {
		m, _ := classPredicateArmMatch(arm, want, exact, resolve)
		return m
	}
	if truthy {
		return c.narrowLocalArms(ident.Name, matches)
	}
	return c.narrowLocalArms(ident.Name, func(arm *TypeExpr) bool { return !matches(arm) })
}

// staticClassArgument resolves a class-valued expression to its script class
// or module. Shadowed names, dynamic values, and constants changed through an
// opaque or external write stay unknown; a stable class-valued assignment in
// self's namespace resolves to the class it actually stores.
func (c *scriptChecker) staticClassArgument(arg Expression) (*ClassDef, bool) {
	if scoped, ok := arg.(*ScopeExpr); ok {
		return c.staticScopedClassArgument(scoped)
	}
	ident, ok := arg.(*Identifier)
	if !ok {
		return nil, false
	}
	if className, ok := c.localClassValueFor(ident.Name); ok {
		classDef, exists := c.script.classes[className]
		return classDef, exists && classDef != nil
	}
	if c.identifierShadowed(ident.Name) || c.hostGlobalShadows(ident.Name) {
		return nil, false
	}
	if c.liveLocalNameHas(ident.Name) {
		// A partial-path assignment post-predeclares the name as a call
		// local (possibly nil), so the runtime reads the local — not the
		// class — even on paths that never assigned it.
		return nil, false
	}
	if c.selfClass != nil {
		if c.opaqueClassConstants || c.classConstantContext.opaque ||
			c.namespaceMemberMutated(c.selfClass.Name, ident.Name) {
			return nil, false
		}
		if c.selfClassMayBindConstant(c.selfClass, ident.Name) {
			return c.stableAssignedClassConstant(c.selfClass, ident.Name)
		}
	}
	return c.staticTopLevelClass(ident.Name)
}

func (c *scriptChecker) staticTopLevelClass(name string) (*ClassDef, bool) {
	classDef, ok := c.script.classes[name]
	if !ok || classDef == nil {
		return nil, false
	}
	if val, bound := checkRootBinding(c.runtimeTypeRoot, name); bound {
		if val.Kind() != KindClass {
			return nil, false
		}
		runtimeClass := valueClass(val)
		if runtimeClass == nil || runtimeClass.Name != classDef.Name || runtimeClass.owner != classDef.owner {
			return nil, false
		}
	}
	return classDef, true
}

func (c *scriptChecker) staticScopedClassArgument(scoped *ScopeExpr) (*ClassDef, bool) {
	if scoped == nil {
		return nil, false
	}
	namespace, ok := c.staticClassArgument(scoped.Object)
	if !ok || namespace == nil ||
		c.opaqueClassConstants || c.classConstantContext.opaque ||
		c.namespaceMemberMutated(namespace.Name, scoped.Property) {
		return nil, false
	}
	if classDef, exact := c.stableAssignedClassConstant(namespace, scoped.Property); exact {
		return classDef, true
	}
	if c.nestedClassConstantMayChange(namespace, scoped.Property) {
		return nil, false
	}
	qualified := namespace.Name + "::" + scoped.Property
	classDef, ok := c.script.classes[qualified]
	if !ok || classDef == nil {
		return nil, false
	}
	if slices.Contains(namespace.NestedModules, scoped.Property) {
		return classDef, true
	}
	return nil, false
}

// stableAssignedClassConstant resolves the narrow class-body alias form whose
// value is guaranteed to exist before any later class-body statement or method
// can read it. Broader control flow remains dynamic until the checker tracks a
// class body's current execution position.
func (c *scriptChecker) stableAssignedClassConstant(
	owner *ClassDef,
	name string,
) (*ClassDef, bool) {
	if owner == nil || name == "" || len(owner.Body) == 0 ||
		c.opaqueClassConstants || c.classConstantContext.opaque ||
		c.namespaceMemberMutated(owner.Name, name) {
		return nil, false
	}
	if _, deferred := c.script.deferredClassBodies[owner.Name]; deferred {
		return nil, false
	}
	if _, bound := owner.ClassVars[name]; bound {
		return nil, false
	}
	if slices.Contains(owner.NestedModules, name) {
		return nil, false
	}
	for _, fn := range owner.ClassMethods {
		if fn != nil && astAssignsClassName(fn.Body, owner, name, false, true) {
			return nil, false
		}
	}
	if classDefInstanceMethodsAssignName(owner, owner, name) {
		return nil, false
	}
	first, ok := owner.Body[0].(*AssignStmt)
	if !ok || first.Operator != tokenNone {
		return nil, false
	}
	target, ok := first.Target.(*Identifier)
	if !ok || target.Name != name {
		return nil, false
	}
	if astAssignsClassName(first.Value, owner, name, true, true) ||
		astAssignsClassName(owner.Body[1:], owner, name, true, true) {
		return nil, false
	}
	rhs, ok := first.Value.(*Identifier)
	if !ok || rhs.Name == name || c.hostGlobalShadows(rhs.Name) ||
		c.selfClassMayBindConstant(owner, rhs.Name) {
		return nil, false
	}
	return c.staticTopLevelClass(rhs.Name)
}

func (c *scriptChecker) nestedClassConstantMayChange(classDef *ClassDef, name string) bool {
	if classDef == nil || classDefAssignsName(classDef, name) {
		return true
	}
	if _, ok := classDef.ClassVars[name]; ok {
		return true
	}
	return false
}

// selfClassMayBindConstant reports whether self's class can supply name
// ahead of the top-level binding: a class variable, a nested module, or a
// class-body assignment that creates the variable when the body runs.
func (c *scriptChecker) selfClassMayBindConstant(cl *ClassDef, name string) bool {
	if cl == nil {
		return false
	}
	if _, ok := cl.ClassVars[name]; ok {
		return true
	}
	if slices.Contains(cl.NestedModules, name) {
		return true
	}
	if classDefAssignsName(cl, name) {
		return true
	}
	return false
}

// classDefAssignsName reports whether the class body or one of its methods
// contains an assignment whose target can bind name on the class. Bare class
// body assignments, class-variable writes, and writes through the class value
// qualify; call-local and unrelated-receiver assignments do not. The walk is
// reflective so destructuring targets and future statement forms stay covered.
func classDefAssignsName(cl *ClassDef, name string) bool {
	return classDefNamespaceAssignsName(cl, name) ||
		classDefInstanceMethodsAssignName(cl, cl, name)
}

func classDefNamespaceAssignsName(cl *ClassDef, name string) bool {
	if astAssignsClassName(cl.Body, cl, name, true, true) {
		return true
	}
	for _, fn := range cl.ClassMethods {
		if fn != nil && astAssignsClassName(fn.Body, cl, name, false, true) {
			return true
		}
	}
	return false
}

func classDefInstanceMethodsAssignName(methodOwner, receiverClass *ClassDef, name string) bool {
	for _, fn := range methodOwner.Methods {
		if fn != nil && astAssignsClassName(fn.Body, receiverClass, name, false, false) {
			return true
		}
	}
	return false
}

// astAssignsClassName walks assignment targets that can mutate cl's class
// constant. Bare identifiers do so only in the class body; call scopes bind
// them as locals. Explicit self writes require class-valued self, and writes
// through another class never affect cl.
func astAssignsClassName(root any, cl *ClassDef, name string, classBody, classSelf bool) bool {
	found := false
	walkASTValue(reflect.ValueOf(root), 0, func(node any) {
		assign, ok := node.(*AssignStmt)
		if !ok || found {
			return
		}
		walkASTValue(reflect.ValueOf(assign.Target), 0, func(target any) {
			switch typed := target.(type) {
			case *Identifier:
				if classBody && typed.Name == name {
					found = true
				}
			case *ClassVarExpr:
				if typed.Name == name {
					found = true
				}
			case *MemberExpr:
				if typed.Property != name {
					return
				}
				switch object := typed.Object.(type) {
				case *Identifier:
					if classMemberAssignmentIntercepted(cl, name) {
						return
					}
					if object.Name == cl.Name || (object.Name == "self" && classSelf) {
						found = true
					}
				case *MemberExpr:
					ident, ok := object.Object.(*Identifier)
					if ok && ident.Name == "self" && object.Property == "class" {
						if classMemberAssignmentIntercepted(cl, name) {
							return
						}
						found = true
					}
				}
			}
		})
	})
	return found
}

func classMemberAssignmentIntercepted(cl *ClassDef, name string) bool {
	if cl == nil {
		return false
	}
	_, hasSetter := cl.ClassMethods[name+"="]
	_, hasGetter := cl.ClassMethods[name]
	return hasSetter || hasGetter
}

const maxASTWalkDepth = 200

// walkASTValue reflectively visits every node reachable from v, invoking
// visit for each addressable struct pointer. The AST is acyclic; the depth
// cap guards pathological inputs.
func walkASTValue(v reflect.Value, depth int, visit func(any)) {
	if depth > maxASTWalkDepth || !v.IsValid() {
		return
	}
	switch v.Kind() {
	case reflect.Pointer, reflect.Interface:
		if v.IsNil() {
			return
		}
		if v.Kind() == reflect.Pointer && v.Elem().Kind() == reflect.Struct && v.CanInterface() {
			visit(v.Interface())
		}
		walkASTValue(v.Elem(), depth+1, visit)
	case reflect.Struct:
		for i := range v.NumField() {
			field := v.Field(i)
			if !field.CanInterface() {
				continue
			}
			walkASTValue(field, depth+1, visit)
		}
	case reflect.Slice, reflect.Array:
		for i := range v.Len() {
			walkASTValue(v.Index(i), depth+1, visit)
		}
	case reflect.Map:
		for _, key := range v.MapKeys() {
			walkASTValue(v.MapIndex(key), depth+1, visit)
		}
	}
}

// classPredicateArmUsesUniversalDispatch reports whether a known fact arm must
// reach the universal class predicate. A named arm qualifies when it resolves
// to an enum (no user methods) or to a non-module class without its own
// override; module-typed arms stay conservative because their concrete class
// is unknown. Plain data arms defer to the shared dispatch proof.
func classPredicateArmUsesUniversalDispatch(arm *TypeExpr, property string, resolve namedTypeResolver) bool {
	if arm == nil {
		return false
	}
	if arm.Kind == TypeEnum {
		match, ok := resolve(arm)
		if !ok {
			return false
		}
		if match.enum != nil {
			return true
		}
		if match.class == nil || match.class.IsModule {
			return false
		}
		_, overridden := match.class.Methods[property]
		return !overridden
	}
	return typeArmUsesUniversalMemberDispatch(arm, property)
}

// classPredicateArmMatch reports how a value of the arm answers the
// predicate. The runtime compares class definitions by identity, so the
// positive direction requires the arm to resolve to the argument's own
// definition. A same-named but distinct definition leaves identity unproven —
// known=false — and the caller must not narrow.
func classPredicateArmMatch(arm *TypeExpr, want *ClassDef, exact bool, resolve namedTypeResolver) (matches, known bool) {
	if arm == nil || arm.Kind != TypeEnum {
		return false, true
	}
	match, ok := resolve(arm)
	if !ok {
		return false, false
	}
	if match.enum != nil {
		// Enum values are never class instances.
		return false, true
	}
	if match.class == nil {
		return false, false
	}
	if classDefsIdentical(match.class, want) {
		return true, true
	}
	if match.class.Name == want.Name {
		return false, false
	}
	return false, true
}

// classDefsIdentical mirrors enumDefsEqual for classes: definition clones
// keep their owner script, so identity survives cloning while same-named
// definitions from different scripts stay distinct.
func classDefsIdentical(left, right *ClassDef) bool {
	if left == right {
		return true
	}
	if left == nil || right == nil || left.Name != right.Name {
		return false
	}
	return left.owner != nil && left.owner == right.owner
}

// narrowIsTypePredicateMember narrows `x.is_type?(:atom)` on a plain local
// with a literal builtin atom. Both branches refine: the true path keeps arms
// that may satisfy the atom, the false path drops arms that always satisfy
// it. Named atoms, non-literal atoms, and receivers without proven universal
// dispatch stay unchanged.
func (c *scriptChecker) narrowIsTypePredicateMember(member *MemberExpr, arg Expression, truthy bool) bool {
	if member == nil || member.Safe || member.Property != isTypeMemberName {
		return true
	}
	ident, ok := member.Object.(*Identifier)
	if !ok {
		return true
	}
	if c.memberDispatchEffect(member) != effectPure {
		return true
	}
	atomValue, ok := staticLiteralValue(arg)
	if !ok {
		return true
	}
	text, ok := typeAtomArg(atomValue)
	if !ok {
		return true
	}
	atom, err := parseTypeAtom(text)
	if err != nil || atom.Kind == TypeEnum {
		return true
	}
	if truthy {
		return c.narrowLocalArms(ident.Name, func(arm *TypeExpr) bool {
			may, _ := typeArmAtomMatch(arm, atom)
			return may
		})
	}
	return c.narrowLocalArms(ident.Name, func(arm *TypeExpr) bool {
		_, must := typeArmAtomMatch(arm, atom)
		return !must
	})
}

// typeArmAtomMatch reports whether a known fact arm may and must satisfy a
// builtin is_type? atom. Arms reaching this point passed the universal
// dispatch proof, so only plain data kinds appear. A number arm may hold an
// int or a float, so it may — but does not have to — satisfy either atom.
func typeArmAtomMatch(arm, atom *TypeExpr) (may, must bool) {
	if atom.Nullable {
		if arm.Kind == TypeNil {
			return true, true
		}
		bare := *atom
		bare.Nullable = false
		return typeArmAtomMatch(arm, &bare)
	}
	switch atom.Kind {
	case TypeNumber:
		switch arm.Kind {
		case TypeInt, TypeFloat, TypeNumber:
			return true, true
		}
		return false, false
	case TypeInt, TypeFloat:
		if arm.Kind == atom.Kind {
			return true, true
		}
		if arm.Kind == TypeNumber {
			return true, false
		}
		return false, false
	case TypeHash:
		// Hash atoms cover hash and object receivers, and shape facts
		// describe hash-shaped data.
		switch arm.Kind {
		case TypeHash, TypeShape:
			return true, true
		}
		return false, false
	default:
		exact := arm.Kind == atom.Kind
		return exact, exact
	}
}

// memberDispatchEffect resolves the registered receiver effect of a member
// dispatch from the receiver's known arms: effectPure only when every arm
// that can dispatch proves a pure registered contract, effectMutatesReceiver
// when every arm resolves and at least one is a registered mutator, and
// effectUnknown otherwise. Named receivers (user overrides may shadow any
// member), unregistered members, and unknown arms all stay unknown, so
// dynamic dispatch keeps its conservative treatment. Safe navigation skips
// nil arms: a nil receiver skips the dispatch entirely.
func (c *scriptChecker) memberDispatchEffect(member *MemberExpr) memberEffect {
	if member == nil {
		return effectUnknown
	}
	arms, ok := typeExprArms(c.inferExpressionType(member.Object), 0)
	if !ok || len(arms) == 0 {
		return effectUnknown
	}
	combined := effectPure
	dispatchArms := 0
	for _, arm := range arms {
		if member.Safe && arm.Kind == TypeNil {
			continue
		}
		combined = combineMemberEffects(combined, c.typeArmMemberEffect(arm, member.Property))
		if combined == effectUnknown {
			return effectUnknown
		}
		dispatchArms++
	}
	if dispatchArms == 0 {
		return effectUnknown
	}
	return combined
}

// typeArmMemberEffect resolves the registered effect a member dispatch has
// on one known receiver arm, mirroring runtime dispatch order: a typed
// contract wins over the universal fallback, a kind's own unregistered
// member shadows the universal helper with an unknown effect, and hash-like
// arms qualify only when no stored callable can shadow the helper.
func (c *scriptChecker) typeArmMemberEffect(arm *TypeExpr, property string) memberEffect {
	if arm == nil {
		return effectUnknown
	}
	switch arm.Kind {
	case TypeHash, TypeShape:
		if !typeArmUsesUniversalMemberDispatch(arm, property) {
			return effectUnknown
		}
		if effect, ok := universalMemberEffects[property]; ok {
			return effect
		}
		return effectUnknown
	case TypeNumber:
		return combineMemberEffects(kindMemberEffect("int", property), kindMemberEffect("float", property))
	case TypeEnum:
		// A nominal arm dispatches a class predicate through the pure
		// universal contract when its class provably lacks an override, so
		// mixed nominal and container unions keep their receiver facts for
		// the narrowing that follows.
		switch property {
		case isAMemberName, kindOfMemberName, instanceOfMemberName:
			if classPredicateArmUsesUniversalDispatch(arm, property, c.checkNamedTypeResolver()) {
				if effect, ok := universalMemberEffects[property]; ok {
					return effect
				}
			}
		}
		return effectUnknown
	case TypeAny, TypeUnknown, TypeUnion:
		return effectUnknown
	}
	kind, ok := receiverKindForTypeArm(arm)
	if !ok {
		return effectUnknown
	}
	return kindMemberEffect(kind, property)
}

// kindMemberEffect resolves the registered effect of a member on one fixed
// receiver kind. A registered typed contract answers directly; a member the
// kind dispatches itself without a contract stays unknown even when a
// universal helper shares its name; otherwise the universal contract's
// effect applies.
func kindMemberEffect(kind, property string) memberEffect {
	if effect, ok := staticMemberEffects[kind+"."+property]; ok {
		return effect
	}
	if memberKindOwns(kind, property) {
		return effectUnknown
	}
	if effect, ok := universalMemberEffects[property]; ok {
		return effect
	}
	return effectUnknown
}

// combineMemberEffects joins the effects of two possible dispatches: any
// unknown side stays unknown, a mutating side dominates a pure one.
func combineMemberEffects(a, b memberEffect) memberEffect {
	if a == effectUnknown || b == effectUnknown {
		return effectUnknown
	}
	if a == effectMutatesReceiver || b == effectMutatesReceiver {
		return effectMutatesReceiver
	}
	return effectPure
}

// memberDispatchPreservesReceiverFacts reports whether a member dispatch is
// proven pure by its registered contracts and cannot hand the caller a
// mutable alias into the receiver's interior. Purity alone is not enough to
// keep the receiver's facts: a pure read like at on array<array<int>>
// returns a nested container the caller can mutate through a chained call
// (`a.at(0).push("x")`), and that receiver spelling is not an identifier
// projection escapePoisonTarget could trace back to the root, so the deep
// fact would silently go stale.
func (c *scriptChecker) memberDispatchPreservesReceiverFacts(member *MemberExpr) bool {
	if result, ok := c.hashDataMemberResultFact(member); ok {
		return !typeExprMayEscapeReceiverInterior(result)
	}
	if c.memberDispatchEffect(member) != effectPure {
		return false
	}
	arms, ok := typeExprArms(c.inferExpressionType(member.Object), 0)
	if !ok || len(arms) == 0 {
		return false
	}
	for _, arm := range arms {
		if member.Safe && arm.Kind == TypeNil {
			continue
		}
		if typeArmMemberResultMayAliasInterior(arm, member.Property) {
			return false
		}
	}
	return true
}

// typeArmMemberResultMayAliasInterior reports whether the member's result on
// one receiver arm may still reach into the receiver. An arm whose interior
// provably holds neither mutable containers nor callables has nothing to
// hand out; otherwise only a declared result that can never be a mutable
// container or a callable (predicates and conversions return fresh scalars)
// proves the call does not leak an interior reference.
func typeArmMemberResultMayAliasInterior(arm *TypeExpr, property string) bool {
	if arm == nil {
		return true
	}
	if !typeArmInteriorMayEscape(arm) {
		return false
	}
	result, known := typeArmMemberResultType(arm, property)
	return !known || result == nil || typeExprMayEscapeReceiverInterior(result)
}

// typeArmMemberResultType resolves the declared result type of the
// registered contract governing a member dispatch on one receiver arm,
// mirroring the dispatch order of kindMemberEffect: typed contracts win, a
// kind's own unregistered member stays unknown, then the universal
// contract answers.
func typeArmMemberResultType(arm *TypeExpr, property string) (*TypeExpr, bool) {
	switch arm.Kind {
	case TypeHash, TypeShape:
		if spec, ok := universalMemberSpecs[property]; ok {
			return spec.resultType, true
		}
		return nil, false
	}
	kind, ok := receiverKindForTypeArm(arm)
	if !ok {
		return nil, false
	}
	if spec, ok := staticMemberSpecs[kind+"."+property]; ok {
		return spec.resultType, true
	}
	if valueType, ok := staticMemberValueTypes[kind+"."+property]; ok {
		return valueType, true
	}
	if memberKindOwns(kind, property) {
		return nil, false
	}
	if spec, ok := universalMemberSpecs[property]; ok {
		return spec.resultType, true
	}
	return nil, false
}

// typeArmInteriorMayEscape reports whether values stored inside one
// receiver arm could reach back into it after being read out: a nested
// mutable container aliases its storage directly, and a stored callable
// can close over the receiver and mutate it when invoked later.
func typeArmInteriorMayEscape(arm *TypeExpr) bool {
	switch arm.Kind {
	case TypeArray:
		if len(arm.TypeArgs) != 1 {
			return true
		}
		return typeExprMayEscapeReceiverInterior(arm.TypeArgs[0])
	case TypeHash:
		if len(arm.TypeArgs) != 2 {
			return true
		}
		return typeExprMayEscapeReceiverInterior(arm.TypeArgs[1])
	case TypeShape:
		for _, field := range arm.Shape {
			if typeExprMayEscapeReceiverInterior(field) {
				return true
			}
		}
		return false
	}
	// Scalar receivers store no mutable interior.
	return false
}

// typeExprMayEscapeReceiverInterior reports whether a value of this type,
// handed out of a receiver's interior, could still reach the receiver: a
// mutable container aliases its storage, and a callable can run user code
// that mutates the receiver through a captured alias. Unknown types stay
// conservative.
func typeExprMayEscapeReceiverInterior(ty *TypeExpr) bool {
	if ty == nil {
		return true
	}
	if typeExprMayIncludeCallable(ty) {
		return true
	}
	arms, ok := typeExprArms(ty, 0)
	if !ok || len(arms) == 0 {
		return true
	}
	for _, arm := range arms {
		if arm == nil {
			return true
		}
		switch arm.Kind {
		case TypeArray, TypeHash, TypeShape, TypeAny, TypeUnknown:
			return true
		}
	}
	return false
}

// typeArmUsesUniversalMemberDispatch reports whether a known fact arm must
// reach the universal implementation of property. Named values may override
// it. Hash-like facts are safe only when their exact value or field contract
// rules out a callable export with the same name. Primitive kinds with their
// own typed contract also dispatch before the universal fallback.
func typeArmUsesUniversalMemberDispatch(arm *TypeExpr, property string) bool {
	if arm == nil {
		return false
	}
	switch arm.Kind {
	case TypeEnum:
		return false
	case TypeHash:
		return len(arm.TypeArgs) == 2 && !typeExprMayIncludeCallable(arm.TypeArgs[1])
	case TypeShape:
		field, present := arm.Shape[property]
		if !present {
			// An open shape may hold an undeclared callable export with the
			// same name, so only a closed shape rules the override out.
			return !arm.Open
		}
		return !typeExprMayIncludeCallable(field)
	case TypeNumber:
		_, intOverride := staticMemberSpecs["int."+property]
		_, floatOverride := staticMemberSpecs["float."+property]
		return !intOverride && !floatOverride
	case TypeAny, TypeUnknown, TypeUnion:
		return false
	}
	kind, ok := receiverKindForTypeArm(arm)
	if !ok {
		return false
	}
	_, overridden := staticMemberSpecs[kind+"."+property]
	return !overridden
}

func typeExprMayIncludeCallable(ty *TypeExpr) bool {
	if ty == nil {
		return true
	}
	switch ty.Kind {
	case TypeAny, TypeUnknown, TypeFunction:
		return true
	case TypeUnion:
		if slices.ContainsFunc(ty.Union, typeExprMayIncludeCallable) {
			return true
		}
	}
	return false
}

// memberCallPreservesReceiverFacts recognizes a call whose member dispatch
// preserves the receiver's facts (a registered pure contract that cannot
// alias the receiver's interior) and whose arguments provably run no user
// code. Arguments evaluate before the member dispatches, so an argument
// that can call into script code may mutate the receiver through an alias,
// and a block runs user code during the dispatch itself — the receiver's
// fact must not survive either.
func (c *scriptChecker) memberCallPreservesReceiverFacts(call *CallExpr) bool {
	if call == nil || len(call.KwArgs) > 0 || call.Block != nil {
		return false
	}
	for _, arg := range call.Args {
		if !c.pureCallArgument(arg) {
			return false
		}
	}
	member, ok := call.Callee.(*MemberExpr)
	return ok && c.memberDispatchPreservesReceiverFacts(member)
}

// memberCallMayWriteUnknownIvar reports whether dispatch can run unclassified
// code whose writes to the current self are not visible in its arguments. A
// supplied block's effects are handled separately when the dispatch may run it.
// callTargetsCoreHashNew recognizes the core Hash.new constructor, which
// builds an empty hash and runs no script code, so the caller's ivar facts
// survive the call. The resolved target and builtin identity both matter:
// a script binding or host replacement may use the same spelling while
// doing anything at all.
func (c *scriptChecker) callTargetsCoreHashNew(
	call *CallExpr,
	target staticCallable,
	resolved bool,
) bool {
	if call == nil || !resolved || target.fn != nil || target.constructor {
		return false
	}
	callee, ok := call.Callee.(*MemberExpr)
	if !ok || callee.Property != "new" {
		return false
	}
	object, ok := callee.Object.(*Identifier)
	if !ok || object.Name != "Hash" || target.name != "Hash.new" {
		return false
	}
	return c.coreNamespaceBuiltinBinding("Hash", "new", builtinHashNew)
}

// coreNamespaceBuiltinBinding reports whether namespace.member still resolves
// to the engine's own builtin rather than a host replacement or a mutated
// snapshot; the function identity is what carries the purity claim.
func (c *scriptChecker) coreNamespaceBuiltinBinding(
	namespace string,
	member string,
	fn BuiltinFunc,
) bool {
	if c.hostGlobalShadows(namespace) &&
		(c.optionGlobalsOverride || c.optionGlobalSeeded(namespace)) {
		object := c.optionGlobals[namespace]
		if object.Kind() != KindObject {
			return false
		}
		value, ok := object.Hash()[member]
		return ok && builtinValueUsesFunction(value, fn)
	}
	return !c.hostBuiltinOverrides(namespace) &&
		!c.namespaceMemberMutated(namespace, member)
}

func builtinValueUsesFunction(value Value, fn BuiltinFunc) bool {
	builtin := valueBuiltin(value)
	return builtin != nil && builtin.Fn != nil &&
		reflect.ValueOf(builtin.Fn).Pointer() == reflect.ValueOf(fn).Pointer()
}

func (c *scriptChecker) memberCallMayWriteUnknownIvar(call *CallExpr) bool {
	member, ok := call.Callee.(*MemberExpr)
	if !ok {
		return true
	}
	return c.memberDispatchEffect(member) == effectUnknown
}

// pureCallArgument reports whether a call argument provably runs no user
// code when it evaluates: literals and plain non-callable reads qualify. An
// identifier stays pure only when it cannot auto-invoke a callable —
// neither as a resolved zero-arity function or builtin, nor as a local
// whose value may itself be callable (a stored zero-arity function
// auto-invokes when the argument evaluates).
func (c *scriptChecker) pureCallArgument(expr Expression) bool {
	switch typed := expr.(type) {
	case *IntegerLiteral, *FloatLiteral, *StringLiteral, *BoolLiteral,
		*NilLiteral, *SymbolLiteral:
		return true
	case *Identifier:
		if typed.Name == "self" && c.selfClass != nil {
			return true
		}
		if _, autoCallable := c.resolveCallable(&CallExpr{Callee: typed}); autoCallable {
			return false
		}
		if _, ok := c.staticClassArgument(typed); ok {
			// A bare class or module reference evaluates to the definition
			// value without running any code.
			return true
		}
		if dispatch, exact := c.implicitSelfIdentifierDispatch(typed); exact {
			return !dispatch.mayRunScript()
		}
		return !typeExprMayIncludeCallable(c.inferExpressionType(typed))
	case *UnaryExpr:
		return c.pureCallArgument(typed.Right)
	}
	return false
}

// typeExprDefinitelyTruthy reports whether every possible value of the type
// is truthy: everything except nil and false is truthy, so any arm that can
// be nil or bool (or is unknown) stays undecided.
func typeExprDefinitelyTruthy(ty *TypeExpr) bool {
	return typeExprArmsAll(ty, func(arm *TypeExpr) bool {
		if _, isShapeValue := shapeValuePayload(arm); isShapeValue {
			// Runtime shape values are always truthy.
			return true
		}
		return arm.Kind != TypeNil && (arm.Kind != TypeBool || arm.Name == boolTrueFactMarker) &&
			arm.Kind != TypeAny && arm.Kind != TypeUnknown
	})
}

func typeExprDefinitelyFalsey(ty *TypeExpr) bool {
	return typeExprArmsAll(ty, func(arm *TypeExpr) bool {
		return arm.Kind == TypeNil || (arm.Kind == TypeBool && arm.Name == boolFalseFactMarker)
	})
}

// typeExprTruthinessFact keeps the arms that a logical expression can return
// directly from its left operand. Unknown types keep the result gradual.
func typeExprTruthinessFact(ty *TypeExpr, truthy bool) *TypeExpr {
	arms, ok := typeExprArms(ty, 0)
	if !ok {
		return nil
	}
	kept := make([]*TypeExpr, 0, len(arms))
	for _, arm := range arms {
		if _, shapeValue := shapeValuePayload(arm); shapeValue {
			if truthy {
				kept = append(kept, arm)
			}
			continue
		}
		switch arm.Kind {
		case TypeNil:
			if !truthy {
				kept = append(kept, arm)
			}
		case TypeBool:
			if arm.Name == boolTrueFactMarker && !truthy ||
				arm.Name == boolFalseFactMarker && truthy {
				continue
			}
			kept = append(kept, arm)
		default:
			if truthy {
				kept = append(kept, arm)
			}
		}
	}
	return unionTypeExprs(kept...)
}

// typeExprNeverNil reports whether no arm can hold nil. Unlike definite
// truthiness, bool arms qualify: false is a legal value but not nil, and
// safe navigation skips only on nil.
func typeExprNeverNil(ty *TypeExpr) bool {
	return typeExprArmsAll(ty, func(arm *TypeExpr) bool {
		if _, isShapeValue := shapeValuePayload(arm); isShapeValue {
			return true
		}
		return arm.Kind != TypeNil && arm.Kind != TypeAny && arm.Kind != TypeUnknown
	})
}

func typeExprIsNilOnly(ty *TypeExpr) bool {
	return typeExprArmsAll(ty, func(arm *TypeExpr) bool { return arm.Kind == TypeNil })
}

func typeExprNumericOnly(ty *TypeExpr) bool {
	return typeExprArmsAll(ty, func(arm *TypeExpr) bool {
		return arm.Kind == TypeInt || arm.Kind == TypeFloat || arm.Kind == TypeNumber
	})
}

func typeExprArrayOnly(ty *TypeExpr) bool {
	return typeExprArmsAll(ty, func(arm *TypeExpr) bool { return arm.Kind == TypeArray })
}

func typeExprHashLikeOnly(ty *TypeExpr) bool {
	return typeExprArmsAll(ty, func(arm *TypeExpr) bool {
		return arm.Kind == TypeHash || arm.Kind == TypeShape
	})
}

func typeExprHasOpenShapeArm(ty *TypeExpr) bool {
	arms, ok := typeExprArms(ty, 0)
	if !ok {
		return false
	}
	for _, arm := range arms {
		if arm.Kind == TypeShape && arm.Open {
			return true
		}
	}
	return false
}

func typeExprArmsAll(ty *TypeExpr, pred func(*TypeExpr) bool) bool {
	arms, ok := typeExprArms(ty, 0)
	if !ok || len(arms) == 0 {
		return false
	}
	for _, arm := range arms {
		if !pred(arm) {
			return false
		}
	}
	return true
}

// safeNavigationReceiver returns the receiver of a safe-navigation call
// with argument-skip semantics.
func safeNavigationReceiver(call *CallExpr) (Expression, bool) {
	if call == nil || !call.Safe {
		return nil, false
	}
	member, ok := call.Callee.(*MemberExpr)
	if !ok || !member.Safe {
		return nil, false
	}
	return member.Object, true
}

// safeNavigationCallSkipsInferred reports whether inferred facts prove a
// safe-navigation receiver is nil, so the dispatch and its arguments never
// evaluate.
func (c *scriptChecker) safeNavigationCallSkipsInferred(call *CallExpr) bool {
	obj, ok := safeNavigationReceiver(call)
	if !ok {
		return false
	}
	return typeExprIsNilOnly(c.inferExpressionType(obj))
}

// safeNavigationArgumentsAlwaysEvaluateInferred reports whether inferred
// facts prove a safe-navigation receiver is never nil, so the skipped-
// arguments path is impossible and must not join the merge.
func (c *scriptChecker) safeNavigationArgumentsAlwaysEvaluateInferred(call *CallExpr) bool {
	obj, ok := safeNavigationReceiver(call)
	if !ok {
		return false
	}
	return c.safeNavigationReceiverKnownNonNil(obj)
}

// --- expression type inference ---

// inferExpressionType computes the static type of an expression, or nil when
// it is not statically known. It is pure: it never emits warnings and never
// mutates checker state.
func (c *scriptChecker) inferExpressionType(expr Expression) *TypeExpr {
	if fact, captured := c.destructureProjectionFacts[expr]; captured {
		return fact.assigned
	}
	// A pinned node keeps the fact captured at its own walk: a call whose
	// callee mutates a builtin namespace dispatched under the pre-mutation
	// bindings, so its result must not recompute under the context its own
	// write markers created.
	if fact, ok := c.pinnedExpressionFacts[expr]; ok {
		return fact
	}
	// Inference computes facts speculatively (branch results, argument
	// captures) and must not mutate runtime module state along the way.
	c.speculativeInference++
	defer func() { c.speculativeInference-- }()
	switch typed := expr.(type) {
	case *IntegerLiteral:
		return checkTypeInt
	case *FloatLiteral:
		return checkTypeFloat
	case *StringLiteral, *InterpolatedString:
		return checkTypeString
	case *BoolLiteral:
		return checkTypeBool
	case *NilLiteral:
		return checkTypeNil
	case *SymbolLiteral, *InterpolatedSymbol:
		return checkTypeSymbol
	case *RangeExpr:
		return checkTypeRange
	case *ArrayLiteral:
		return c.inferArrayLiteralType(typed)
	case *HashLiteral:
		return c.inferHashLiteralType(typed)
	case *TypeLiteral:
		if c.typeLiteralStaticallyShadowed(typed) {
			return c.inferExpressionType(typed.Fallback)
		}
		return shapeValueType(typed.Type)
	case *BlockLiteral:
		return checkTypeFunction
	case *Identifier:
		if ty := c.localTypeFor(typed.Name); ty != nil {
			return ty
		}
		return c.autoInvokedIdentifierResultFact(typed.Name)
	case *IvarExpr:
		return c.localTypeFor(ivarFactKey(typed.Name))
	case *MemberExpr:
		return c.memberResultFact(typed)
	case *UnaryExpr:
		return c.inferUnaryExprType(typed)
	case *BinaryExpr:
		left := c.inferExpressionType(typed.Left)
		right := c.inferExpressionType(typed.Right)
		switch typed.Operator {
		case tokenAnd, tokenOr:
			// `a && b` is `a ? b : a`: a left operand whose truthiness is
			// statically known picks one side instead of the union.
			isAnd := typed.Operator == tokenAnd
			if val, ok := staticLiteralValue(typed.Left); ok {
				if val.Truthy() == isAnd {
					return right
				}
				return left
			}
			if typeExprDefinitelyTruthy(left) {
				if isAnd {
					return right
				}
				return left
			}
			if typeExprDefinitelyFalsey(left) {
				if isAnd {
					return left
				}
				return right
			}
			if isAnd {
				if falsey := typeExprTruthinessFact(left, false); falsey != nil {
					return unionTypeExprs(falsey, right)
				}
			} else if truthy := typeExprTruthinessFact(left, true); truthy != nil {
				return unionTypeExprs(truthy, right)
			}
		}
		return c.binaryOperationOutcome(typed.Operator, left, right).result
	case *ConditionalExpr:
		return c.inferConditionalExpressionType(typed)
	case *IfExpr:
		return c.inferIfExpressionType(typed)
	case *CaseExpr:
		if result, known := c.inferredCaseExpressionResult(typed); known {
			return c.inferExpressionType(result)
		}
		branches := make([]Expression, 0, len(typed.Clauses)+1)
		for _, clause := range typed.Clauses {
			branches = append(branches, clause.Result)
		}
		branches = append(branches, typed.ElseExpr)
		return c.inferExpectedBranchUnion(expressionExpectation{}, branches...)
	case *RescueExpr:
		return c.inferRescueExpressionTypeWithExpectation(typed, expressionExpectation{})
	case *CallExpr:
		return c.inferCallExprType(typed)
	case *IndexExpr:
		return c.inferIndexExprType(typed)
	}
	return nil
}

// inferExpressionTypeWithExpectation mirrors the runtime's typed argument
// evaluation. A callable expectation turns a bare bound method into a
// function value, and branch/container expectations flow to the expression
// that actually produces the value.
func (c *scriptChecker) inferExpressionTypeWithExpectation(expr Expression, expectation expressionExpectation) *TypeExpr {
	if expectation.empty() {
		return c.inferExpressionType(expr)
	}
	if expectation.includesCallable() {
		if identity, ok := bareIdentifierCallableValue(expr); ok {
			if _, resolved := c.bareIdentifierCallableArgument(expr); resolved {
				return checkTypeFunction
			}
			return c.inferExpressionType(identity)
		}
		if callableFact, ok := c.bareMemberArgumentCallableFact(expr); ok {
			return callableFact
		}
	}
	switch typed := expr.(type) {
	case *ConditionalExpr:
		return c.inferConditionalExpressionTypeWithExpectation(typed, expectation)
	case *IfExpr:
		return c.inferIfExpressionTypeWithExpectation(typed, expectation)
	case *CaseExpr:
		if result, known := c.inferredCaseExpressionResult(typed); known {
			return c.inferExpectedBranchUnion(expectation, result)
		}
		branches := make([]Expression, 0, len(typed.Clauses)+1)
		for _, clause := range typed.Clauses {
			branches = append(branches, clause.Result)
		}
		branches = append(branches, typed.ElseExpr)
		return c.inferExpectedBranchUnion(expectation, branches...)
	case *RescueExpr:
		return c.inferRescueExpressionTypeWithExpectation(
			typed,
			autoCallExpectation(!expectation.includesCallable()),
		)
	case *ArrayLiteral:
		return c.inferExpectedArrayLiteralType(typed, expectation)
	case *HashLiteral:
		return c.inferExpectedHashLiteralType(typed, expectation)
	default:
		return c.inferExpressionType(expr)
	}
}

func (c *scriptChecker) inferExpressionTypeWithAuto(expr Expression, autoCall bool) *TypeExpr {
	if autoCall {
		return c.inferExpressionType(expr)
	}
	if callableFact, ok := c.bareMemberArgumentCallableFact(expr); ok {
		return callableFact
	}
	if identity, ok := bareIdentifierCallableValue(expr); ok {
		if _, resolved := c.bareIdentifierCallableArgument(expr); resolved {
			return checkTypeFunction
		}
		return c.inferExpressionType(identity)
	}
	switch typed := expr.(type) {
	case *RescueExpr:
		return c.inferRescueExpressionTypeWithExpectation(
			typed,
			typeExpressionExpectation(checkTypeFunction),
		)
	case *TypeLiteral:
		if c.typeLiteralStaticallyShadowed(typed) {
			return c.inferExpressionTypeWithAuto(typed.Fallback, false)
		}
	}
	return c.inferExpressionType(expr)
}

// bareIdentifierCallableArgument matches the identifier forms the runtime
// preserves under a callable expectation (`accept(rand)`).
func (c *scriptChecker) bareIdentifierCallableArgument(expr Expression) (Expression, bool) {
	identity, ok := bareIdentifierCallableValue(expr)
	if !ok {
		return nil, false
	}
	call := &CallExpr{Callee: identity}
	if _, ok := c.resolveCallable(call); !ok {
		ident := identity.(*Identifier)
		if c.identifierShadowed(ident.Name) || c.hostGlobalShadows(ident.Name) ||
			c.typeRootHasBinding(ident.Name) || c.hostBuiltinOverrides(ident.Name) ||
			c.implicitSelfFunction(ident.Name) == nil {
			return nil, false
		}
	}
	return expr, true
}

func bareIdentifierCallableValue(expr Expression) (Expression, bool) {
	switch typed := expr.(type) {
	case *Identifier:
		return typed, true
	case *CallExpr:
		if typed.Parenthesized || len(typed.Args) > 0 || len(typed.KwArgs) > 0 ||
			typed.Block != nil {
			return nil, false
		}
		ident, ok := typed.Callee.(*Identifier)
		return ident, ok
	default:
		return nil, false
	}
}

func (c *scriptChecker) inferExpectedBranchUnion(expectation expressionExpectation, branches ...Expression) *TypeExpr {
	var merged *TypeExpr
	for i, branch := range branches {
		arm := checkTypeNil
		if branch != nil {
			arm = c.inferExpressionTypeWithExpectation(branch, expectation)
		}
		if arm == nil {
			return nil
		}
		if i == 0 {
			merged = arm
			continue
		}
		merged = unionTypeExprs(merged, arm)
		if merged == nil {
			return nil
		}
	}
	return merged
}

func (c *scriptChecker) inferExpectedArrayLiteralType(lit *ArrayLiteral, expectation expressionExpectation) *TypeExpr {
	elementExpectation, ok := expectation.arrayElementExpectation()
	if !ok || len(lit.Elements) == 0 {
		return c.inferArrayLiteralType(lit)
	}
	elements := make([]*TypeExpr, 0, len(lit.Elements))
	knownElements := make([]Expression, 0, len(lit.Elements))
	sawUnknown := false
	for i, element := range lit.Elements {
		if _, splat := element.(*SplatArg); splat {
			sawUnknown = true
			continue
		}
		elementType := c.inferExpressionTypeWithExpectation(element, elementExpectation(i, len(lit.Elements)))
		if elementType == nil {
			sawUnknown = true
			continue
		}
		elements = append(elements, elementType)
		knownElements = append(knownElements, element)
	}
	if len(elements) == 0 {
		return checkTypeArray
	}
	union := unionTypeExprs(elements...)
	if union == nil {
		return checkTypeArray
	}
	marker := c.arrayLiteralElementsMarker(knownElements, sawUnknown)
	return &TypeExpr{Kind: TypeArray, Name: marker, TypeArgs: []*TypeExpr{union}}
}

func (c *scriptChecker) inferExpectedHashLiteralType(lit *HashLiteral, expectation expressionExpectation) *TypeExpr {
	if !hashLiteralTypeHasValueSlots(expectation.ty) ||
		(lit.ShapeType != nil && !c.hashShapeStaticallyShadowed(lit)) {
		return c.inferHashLiteralType(lit)
	}
	shape := make(map[string]*TypeExpr, len(lit.Pairs))
	sources := make(map[string]checkValueSource, len(lit.Pairs))
	allSymbolKeys, allStringKeys := true, true
	for _, pair := range lit.Pairs {
		switch pair.Key.(type) {
		case *SymbolLiteral:
			allStringKeys = false
		case *StringLiteral:
			allSymbolKeys = false
		default:
			return checkTypeHash
		}
		key, ok := staticLiteralHashKey(pair.Key)
		if !ok {
			return checkTypeHash
		}
		valueExpectation := expressionExpectation{}
		if valueKey, ok := staticLiteralValue(pair.Key); ok {
			valueExpectation = typeExpressionExpectation(hashLiteralValueType(expectation.ty, valueKey))
		}
		fieldType := c.inferExpressionTypeWithExpectation(pair.Value, valueExpectation)
		if fieldType == nil {
			return checkTypeHash
		}
		if _, duplicate := shape[key]; duplicate {
			return checkTypeHash
		}
		shape[key] = fieldType
		if source, ok := c.evaluatedValueSourceForExpression(pair.Value); ok {
			sources[key] = source
		}
	}
	fact := &TypeExpr{Kind: TypeShape, Shape: shape}
	switch {
	case allSymbolKeys:
		fact.Name = shapeKeysSymbolMarker
	case allStringKeys:
		fact.Name = shapeKeysStringMarker
	}
	c.recordShapeFieldSources(fact, sources)
	return fact
}

func (c *scriptChecker) inferConditionalExpressionType(expr *ConditionalExpr) *TypeExpr {
	return c.inferConditionalExpressionTypeWithExpectation(expr, expressionExpectation{})
}

// inferConditionalExpressionTypeWithExpectation infers a ternary under the
// narrowing each condition outcome proves, flowing any expectation into the
// branch results. Unreachable outcomes contribute no arm.
func (c *scriptChecker) inferConditionalExpressionTypeWithExpectation(expr *ConditionalExpr, expectation expressionExpectation) *TypeExpr {
	baseScopeState := c.snapshotScopeState()
	defer c.restoreScopeState(baseScopeState)

	branches := make([]*TypeExpr, 0, 2)
	if c.applyConditionOutcomeEffects(expr.Condition, true, nil) {
		branches = append(branches, c.inferExpressionTypeWithExpectation(expr.Consequent, expectation))
	}
	c.restoreScopeState(baseScopeState)
	if c.applyConditionOutcomeEffects(expr.Condition, false, nil) {
		branches = append(branches, c.inferExpressionTypeWithExpectation(expr.Alternate, expectation))
	}
	return unionTypeExprs(branches...)
}

func (c *scriptChecker) inferIfExpressionType(expr *IfExpr) *TypeExpr {
	return c.inferIfExpressionTypeWithExpectation(expr, expressionExpectation{})
}

func (c *scriptChecker) inferIfExpressionTypeWithExpectation(expr *IfExpr, expectation expressionExpectation) *TypeExpr {
	baseScopeState := c.snapshotScopeState()
	defer c.restoreScopeState(baseScopeState)

	branches := make([]*TypeExpr, 0, len(expr.ElseIf)+2)
	appendBranch := func(result Expression) {
		if result == nil {
			branches = append(branches, checkTypeNil)
			return
		}
		branches = append(branches, c.inferExpressionTypeWithExpectation(result, expectation))
	}
	collectCondition := func(condition, result Expression) bool {
		conditionScopeState := c.snapshotScopeState()
		if c.applyConditionOutcomeEffects(condition, true, nil) {
			appendBranch(result)
		}
		c.restoreScopeState(conditionScopeState)
		return c.applyConditionOutcomeEffects(condition, false, nil)
	}

	falseReachable := collectCondition(expr.Condition, expr.Consequent)
	for _, branch := range expr.ElseIf {
		if !falseReachable {
			break
		}
		falseReachable = collectCondition(branch.Condition, branch.Result)
	}
	if falseReachable {
		appendBranch(expr.Alternate)
	}
	return unionTypeExprs(branches...)
}

// inferredConditionTruthiness resolves a condition's truthiness from static
// literals first, then from inferred facts, mirroring the short-circuit
// operators: a definitely-truthy type proves the true path runs, a nil-only
// type proves it never does.
func (c *scriptChecker) inferredConditionTruthiness(condition Expression) (bool, bool) {
	if truthy, known := staticExpressionTruthiness(condition); known {
		return truthy, known
	}
	if member, ok := condition.(*MemberExpr); ok {
		if target, resolved := c.resolveMemberCallable(member); resolved &&
			target.fn != nil && !target.constructor {
			if result, static := scriptFunctionLiteralReturnExpression(target.fn); static {
				if truthy, known := staticExpressionTruthiness(result); known {
					return truthy, true
				}
			}
		}
	}
	if ident, ok := condition.(*Identifier); ok {
		if fact, exact := c.localValueFactFor(ident.Name); exact {
			if truthy, known := localValueFactTruthiness(fact, true); known {
				return truthy, true
			}
		}
	}
	ty := c.inferExpressionType(condition)
	if typeExprDefinitelyTruthy(ty) {
		return true, true
	}
	if typeExprDefinitelyFalsey(ty) {
		return false, true
	}
	return false, false
}

func (c *scriptChecker) inferredIfExpressionBranch(expr *IfExpr) (Expression, bool) {
	if expr == nil {
		return nil, false
	}
	truthy, known := c.inferredConditionTruthiness(expr.Condition)
	if !known {
		return nil, false
	}
	if truthy {
		return expr.Consequent, true
	}
	for _, branch := range expr.ElseIf {
		truthy, known = c.inferredConditionTruthiness(branch.Condition)
		if !known {
			return nil, false
		}
		if truthy {
			return branch.Result, true
		}
	}
	return expr.Alternate, true
}

func (c *scriptChecker) inferRescueExpressionTypeWithExpectation(
	expr *RescueExpr,
	expectation expressionExpectation,
) *TypeExpr {
	if expr == nil {
		return nil
	}
	autoCall := !expectation.includesCallable()
	if expressionProvenNonRaising(expr.Body) {
		return c.inferExpressionTypeWithAuto(expr.Body, autoCall)
	}
	if errorKind, exact := c.staticallyRaisedExpressionErrorKind(expr.Body); exact &&
		!staticErrorKindMatchesRescue(errorKind, nil) {
		return c.inferExpressionTypeWithAuto(expr.Body, autoCall)
	}
	branches := make([]*TypeExpr, 0, 2)
	if c.expressionMayCompleteForBindingWithAuto(expr.Body, autoCall) {
		branches = append(branches, c.inferExpressionTypeWithAuto(expr.Body, autoCall))
	}
	if c.expressionMayCompleteForBindingWithAuto(expr.Fallback, autoCall) {
		branches = append(branches, c.inferExpressionTypeWithAuto(expr.Fallback, autoCall))
	}
	if len(branches) == 0 {
		return nil
	}
	return unionTypeExprs(branches...)
}

// inferHashLiteralType infers an exact shape for a hash literal whose keys
// and field types are all statically known, giving downstream indexing
// field-level facts. Anything less certain degrades to a plain hash.
func (c *scriptChecker) inferHashLiteralType(lit *HashLiteral) *TypeExpr {
	if lit.ShapeType != nil {
		if !c.hashShapeStaticallyShadowed(lit) {
			return shapeValueType(lit.ShapeType)
		}
		// A proven shadow forces the hash reading at runtime, so the
		// literal infers its hash facts below like any other braced group.
	}
	shape := make(map[string]*TypeExpr, len(lit.Pairs))
	sources := make(map[string]checkValueSource, len(lit.Pairs))
	sawSymbolKeys, sawStringKeys, sawOtherKeys := false, false, false
	for _, pair := range lit.Pairs {
		switch pair.Key.(type) {
		case *SymbolLiteral:
			sawSymbolKeys = true
		case *StringLiteral:
			sawStringKeys = true
		default:
			sawOtherKeys = true
		}
		key, ok := staticLiteralHashKey(pair.Key)
		if !ok {
			return checkTypeHash
		}
		fieldType := c.inferExpressionType(pair.Value)
		if fieldType == nil {
			return checkTypeHash
		}
		if _, duplicate := shape[key]; duplicate {
			return checkTypeHash
		}
		shape[key] = fieldType
		if source, ok := c.evaluatedValueSourceForExpression(pair.Value); ok {
			sources[key] = source
		}
	}
	fact := &TypeExpr{Kind: TypeShape, Shape: shape}
	// Runtime hashes distinguish symbol keys from string keys, so an exact
	// index fact needs the store's key representation. A literal with mixed
	// or non-string, non-symbol keys carries its witnessed key kinds, so a
	// marker-less shape always means an annotation-declared contract. An
	// empty literal keeps the symbol default; its first static write adopts
	// the write's representation.
	switch {
	case len(lit.Pairs) == 0 || (sawSymbolKeys && !sawStringKeys && !sawOtherKeys):
		fact.Name = shapeKeysSymbolMarker
	case sawStringKeys && !sawSymbolKeys && !sawOtherKeys:
		fact.Name = shapeKeysStringMarker
	default:
		fact.Name = mixedKeysMarker(sawSymbolKeys, sawStringKeys, sawOtherKeys)
	}
	c.recordShapeFieldSources(fact, sources)
	return fact
}

// checkValueSource identifies repeated reads and aligned conditional choices.
// Conditional keys keep opposite branch orderings from appearing correlated.
type checkValueSource struct {
	name              string
	generation        uint64
	consequentTypeKey string
	alternateTypeKey  string
}

type checkValueSourceCapture struct {
	source checkValueSource
	exact  bool
}

func (c *scriptChecker) pinExpressionValueSource(expr Expression) {
	if expr == nil {
		return
	}
	source, exact := c.valueSourceForExpression(expr)
	if c.pinnedExpressionSources == nil {
		c.pinnedExpressionSources = make(map[Expression]checkValueSourceCapture)
	}
	c.pinnedExpressionSources[expr] = checkValueSourceCapture{
		source: source,
		exact:  exact,
	}
}

func (c *scriptChecker) evaluatedValueSourceForExpression(expr Expression) (checkValueSource, bool) {
	if captured, ok := c.pinnedExpressionSources[expr]; ok {
		return captured.source, captured.exact
	}
	return c.valueSourceForExpression(expr)
}

func (c *scriptChecker) valueSourceForExpression(expr Expression) (checkValueSource, bool) {
	switch typed := expr.(type) {
	case *Identifier:
		if c.localTypeFor(typed.Name) == nil {
			return checkValueSource{}, false
		}
		source := checkValueSource{
			name:       typed.Name,
			generation: c.localBindingGenerations[typed.Name],
		}
		for alias := range c.valueAliasNames(typed.Name) {
			candidate := checkValueSource{
				name:       alias,
				generation: c.localBindingGenerations[alias],
			}
			if candidate.name < source.name ||
				candidate.name == source.name && candidate.generation < source.generation {
				source = candidate
			}
		}
		return source, true
	case *ConditionalExpr:
		if !c.pureCallArgument(typed.Condition) ||
			!c.pureCallArgument(typed.Consequent) ||
			!c.pureCallArgument(typed.Alternate) {
			return checkValueSource{}, false
		}
		source, ok := c.valueSourceForExpression(typed.Condition)
		if !ok || source.consequentTypeKey != "" || source.alternateTypeKey != "" {
			return checkValueSource{}, false
		}
		consequent := c.inferExpressionType(typed.Consequent)
		alternate := c.inferExpressionType(typed.Alternate)
		if consequent == nil || alternate == nil {
			return checkValueSource{}, false
		}
		source.consequentTypeKey = formatTypeExpr(consequent)
		source.alternateTypeKey = formatTypeExpr(alternate)
		return source, true
	}
	return checkValueSource{}, false
}

func (c *scriptChecker) recordShapeFieldSources(
	shape *TypeExpr,
	sources map[string]checkValueSource,
) {
	if shape == nil || len(sources) == 0 {
		return
	}
	counts := make(map[checkValueSource]int, len(sources))
	for _, source := range sources {
		counts[source]++
	}
	correlated := make(map[string]checkValueSource)
	for field, source := range sources {
		if counts[source] > 1 {
			correlated[field] = source
		}
	}
	if len(correlated) == 0 {
		return
	}
	if c.shapeFieldSources == nil {
		c.shapeFieldSources = make(map[*TypeExpr]map[string]checkValueSource)
	}
	c.shapeFieldSources[shape] = correlated
}

func (c *scriptChecker) boundaryShapeFieldSource(
	shape *TypeExpr,
	field string,
) (checkValueSource, bool) {
	source, ok := c.shapeFieldSources[shape][field]
	return source, ok
}

func (c *scriptChecker) inferUnaryExprType(expr *UnaryExpr) *TypeExpr {
	switch expr.Operator {
	case tokenBang:
		return checkTypeBool
	case tokenMinus:
		operand := c.inferExpressionType(expr.Right)
		if kind, ok := staticOperandKind(operand); ok {
			switch kind {
			case TypeInt, TypeFloat, TypeNumber:
				return operand
			}
		}
		return nil
	case tokenPlus:
		operand := c.inferExpressionType(expr.Right)
		if kind, ok := staticOperandKind(operand); ok {
			switch kind {
			case TypeInt, TypeFloat, TypeNumber, TypeString:
				return operand
			}
		}
		return nil
	}
	return nil
}

// inferCallExprType exposes an invariant result fact for a resolved call: a
// script function's annotation, a constructor's nominal class, or a builtin
// contract. Constructor identity survives splat expansion because expansion
// changes only argument binding, never the class produced by a successful
// call. Safe navigation returns nil when it must skip and otherwise adds nil
// unless the receiver is known non-nil.
func (c *scriptChecker) inferCallExprType(call *CallExpr) *TypeExpr {
	member, memberCall := call.Callee.(*MemberExpr)
	if memberCall && member.Safe && typeExprIsNilOnly(c.safeNavigationReceiverFact(member.Object)) {
		return checkTypeNil
	}
	target, ok := c.resolveCallable(call)
	if !ok {
		return nil
	}
	return c.inferResolvedCallExprType(call, target)
}

func (c *scriptChecker) implicitSelfCallSummaryTarget(callee Expression) (staticCallable, bool) {
	switch typed := callee.(type) {
	case *Identifier:
		if c.identifierShadowed(typed.Name) || c.hostGlobalShadows(typed.Name) ||
			c.typeRootHasBinding(typed.Name) || c.hostBuiltinOverrides(typed.Name) {
			return staticCallable{}, false
		}
		return c.implicitSelfSummaryCallable(typed.Name)
	case *MemberExpr:
		ident, ok := typed.Object.(*Identifier)
		if !ok || ident.Name != "self" {
			return staticCallable{}, false
		}
		return c.implicitSelfSummaryCallable(typed.Property)
	default:
		return staticCallable{}, false
	}
}

func (c *scriptChecker) inferResolvedCallExprType(call *CallExpr, target staticCallable) *TypeExpr {
	member, memberCall := call.Callee.(*MemberExpr)
	if memberCall && member.Safe && typeExprIsNilOnly(c.safeNavigationReceiverFact(member.Object)) {
		return checkTypeNil
	}
	var result *TypeExpr
	if target.constructorClass != "" {
		result = &TypeExpr{Kind: TypeEnum, Name: target.constructorClass}
	} else if callExpandsArguments(call) {
		return nil
	} else if target.fn != nil {
		if target.constructor {
			return nil
		}
		result = target.fn.ReturnTy
		if result == nil {
			result = c.scriptCallableReturnSummary(call, target)
		}
	} else if target.name == "JSON.parse_as" && len(call.Args) == 2 {
		if shape, ok := shapeValuePayload(c.inferExpressionType(call.Args[1])); ok {
			// JSON object keys are strings, so the validated result and its
			// nested shapes are string-keyed stores.
			result = stringKeyedShapeFact(shape)
		}
	} else {
		result = target.spec.resultType
	}
	if memberCall {
		return c.safeNavigationMemberResultFact(member, result)
	}
	return result
}

func (c *scriptChecker) inferDynamicCallExprType(
	resolution checkDynamicCallResolution,
) (*TypeExpr, bool) {
	if resolution.nonScriptMayComplete {
		return nil, false
	}
	results := make([]*TypeExpr, 0, len(resolution.targets))
	for _, candidate := range resolution.targets {
		if !candidate.mayEnter || !c.scriptFunctionCallMayComplete(
			candidate.call,
			candidate.target,
		) {
			continue
		}
		result := c.inferResolvedCallExprType(candidate.call, candidate.target)
		if result == nil {
			return nil, false
		}
		results = append(results, result)
	}
	if len(results) == 0 {
		return nil, false
	}
	result := unionTypeExprs(results...)
	return result, result != nil
}

// memberResultFact reports the result of a bare member read that auto-invokes
// in a value context: constructors carry their nominal class, script methods
// expose an explicit return annotation, builtins expose their invariant
// contract results, and temporal conversions surface as direct scalar values.
// A nil-only safe-navigation receiver yields nil without dispatch; safe
// navigation otherwise adds nil unless the receiver is known non-nil.
func (c *scriptChecker) memberResultFact(member *MemberExpr) *TypeExpr {
	if member.Safe && typeExprIsNilOnly(c.safeNavigationReceiverFact(member.Object)) {
		return checkTypeNil
	}
	if result, ok := c.hashDataMemberResultFact(member); ok {
		return c.safeNavigationMemberResultFact(member, result)
	}
	if result := c.staticMemberValueResultFact(member); result != nil {
		return c.safeNavigationMemberResultFact(member, result)
	}
	target, ok := c.resolveMemberCallable(member)
	if !ok {
		target, ok = c.implicitSelfCallSummaryTarget(member)
	}
	if !ok {
		return nil
	}
	var result *TypeExpr
	if target.constructorClass != "" {
		result = &TypeExpr{Kind: TypeEnum, Name: target.constructorClass}
	} else if target.fn != nil && !target.constructor {
		result = target.fn.ReturnTy
		if result == nil {
			result = c.scriptCallableReturnSummary(nil, target)
		}
	} else if target.spec.autoInvoke {
		result = target.spec.resultType
	}
	return c.safeNavigationMemberResultFact(member, result)
}

// hashDataMemberResultFact resolves a non-callable hash/object data field when
// both possible backing kinds use data lookup for the property. Hash builtins
// and universal helpers stay on the normal dispatch path because a KindHash
// may choose the builtin where a KindObject chooses stored data.
func (c *scriptChecker) hashDataMemberResultFact(member *MemberExpr) (*TypeExpr, bool) {
	if member == nil || memberKindOwns("hash", member.Property) ||
		isUniversalMember(member.Property) {
		return nil, false
	}
	receiver := nonNilMutatorReceiverFact(c.inferExpressionType(member.Object))
	var result *TypeExpr
	if valueBound, getterMayResolve := c.declaredHashDataMemberResult(receiver); getterMayResolve {
		result = valueBound
	} else if receiver != nil && receiver.Kind == TypeShape && !receiver.Nullable {
		field, present := receiver.Shape[member.Property]
		if !present {
			return nil, false
		}
		result = shapeFieldValueType(field)
	} else {
		return nil, false
	}
	if result == nil || typeExprMayIncludeCallable(result) {
		return nil, false
	}
	return result, true
}

// hashLikeDataMemberLookupProvablyFails reports a non-builtin data member that
// is absent from every exact shape arm or whose string/symbol key is excluded
// by every declared hash arm. Both hash and object receivers raise on that
// miss, so compound and logical assignment cannot evaluate their right side or
// reach their setter. A safe-navigation nil arm is handled by the caller as the
// one completing path; nil on a plain member read also fails the lookup.
func (c *scriptChecker) hashLikeDataMemberLookupProvablyFails(member *MemberExpr) bool {
	if member == nil || memberKindOwns("hash", member.Property) ||
		isUniversalMember(member.Property) {
		return false
	}
	receiver := c.inferExpressionType(member.Object)
	sawClosedHashLike := false
	allFail := typeExprArmsAll(receiver, func(arm *TypeExpr) bool {
		if arm.Kind == TypeNil {
			return member.Safe || !memberKindOwns("nil", member.Property)
		}
		if arm.Kind == TypeHash {
			if _, valueBound := declaredHashEntryTypes(arm); valueBound == nil {
				return false
			}
			sawClosedHashLike = true
			_, getterMayResolve := c.declaredHashDataMemberResult(arm)
			return !getterMayResolve
		}
		if arm.Kind != TypeShape || arm.Open {
			return false
		}
		sawClosedHashLike = true
		_, present := arm.Shape[member.Property]
		return !present
	})
	return sawClosedHashLike && allFail
}

func scriptFunctionLiteralReturnExpression(fn *ScriptFunction) (Expression, bool) {
	if fn == nil || len(fn.Body) != 1 {
		return nil, false
	}
	var result Expression
	switch stmt := fn.Body[0].(type) {
	case *ExprStmt:
		result = stmt.Expr
	case *ReturnStmt:
		result = stmt.Value
	default:
		return nil, false
	}
	if result == nil {
		return &NilLiteral{Position: fn.Pos}, true
	}
	if _, static := staticLiteralValue(result); !static {
		return nil, false
	}
	return result, true
}

func (c *scriptChecker) safeNavigationMemberResultFact(member *MemberExpr, result *TypeExpr) *TypeExpr {
	if result == nil || member == nil || !member.Safe || c.safeNavigationReceiverKnownNonNil(member.Object) {
		return result
	}
	if result.Kind == TypeUnion {
		return unionTypeExprs(result, checkTypeNil)
	}
	return nullableTypeExpr(result)
}

func (c *scriptChecker) safeNavigationReceiverKnownNonNil(expr Expression) bool {
	ident, ok := expr.(*Identifier)
	if !ok {
		return typeExprNeverNil(c.inferExpressionType(expr))
	}
	if ident.Name == "self" && c.selfClass != nil {
		return true
	}
	if typeExprNeverNil(c.localTypeFor(ident.Name)) {
		return true
	}
	if c.identifierShadowed(ident.Name) || c.hostGlobalShadows(ident.Name) {
		return false
	}
	_, ok = c.script.classes[ident.Name]
	return ok
}

func (c *scriptChecker) safeNavigationReceiverFact(expr Expression) *TypeExpr {
	if ident, ok := expr.(*Identifier); ok {
		return c.localTypeFor(ident.Name)
	}
	return c.inferExpressionType(expr)
}

// nullableTypeExpr returns the type with a nil arm added, sharing the
// original when it already admits nil.
func nullableTypeExpr(ty *TypeExpr) *TypeExpr {
	if ty == nil || ty.Nullable || ty.Kind == TypeNil {
		return ty
	}
	clone := *ty
	clone.Nullable = true
	return &clone
}

// staticMemberValueResultFact resolves a direct scalar value only when every
// known receiver arm dispatches through the same built-in kind. Named or
// dynamic receivers stay unknown so user-defined members keep precedence.
func (c *scriptChecker) staticMemberValueResultFact(member *MemberExpr) *TypeExpr {
	kinds, ok := c.staticMemberReceiverKinds(member)
	if !ok {
		return nil
	}
	kind := kinds[0]
	for _, candidate := range kinds[1:] {
		if candidate != kind {
			return nil
		}
	}
	return staticMemberValueTypes[kind+"."+member.Property]
}

// shapeValueMarkerName tags the synthetic type that carries a first-class
// shape value through the local type environment.
const shapeValueMarkerName = "shape"

// shapeValueType wraps a shape used as a first-class value so the fact can
// flow through locals into JSON.parse_as (schema = { ... } then
// JSON.parse_as(raw, schema)). Kind TypeUnknown keeps the marker inert
// everywhere else: unknowns never participate in contradictions or operand
// checks, so only the parse_as resolution above looks inside. The parser
// never produces this spelling — an annotation naming an unknown type
// resolves to TypeEnum, not TypeUnknown.
func shapeValueType(shape *TypeExpr) *TypeExpr {
	return &TypeExpr{Kind: TypeUnknown, Name: shapeValueMarkerName, TypeArgs: []*TypeExpr{shape}}
}

func shapeValuePayload(ty *TypeExpr) (*TypeExpr, bool) {
	if ty == nil || ty.Kind != TypeUnknown || ty.Name != shapeValueMarkerName || len(ty.TypeArgs) != 1 {
		return nil, false
	}
	return ty.TypeArgs[0], true
}

// Shape key-representation markers, stored in the Name field of inferred
// shape facts (unused by TypeShape formatting and comparison). The leading
// NUL keeps them disjoint from any parser-produced spelling.
const (
	shapeKeysStringMarker = "\x00string-keyed"
	shapeKeysSymbolMarker = "\x00symbol-keyed"
	// shapeKeysMixedPrefix tags a literal-derived shape whose keys mix (or
	// fall outside) the string and symbol representations, so marker-less
	// shapes always denote annotation-declared contracts. The suffix
	// records which key kinds the literal witnessed — "s" symbol, "t"
	// string, "o" other hashable literals — so disjointness can use both
	// positive witnesses (a symbol key against a bound excluding symbols)
	// and negative ones (a non-string, non-symbol key against a bound
	// admitting only those).
	shapeKeysMixedPrefix = "\x00mixed-keyed:"
)

// mixedKeysMarker builds the witnessed-kind marker for a mixed-key literal.
func mixedKeysMarker(sawSymbol, sawString, sawOther bool) string {
	marker := shapeKeysMixedPrefix
	if sawSymbol {
		marker += "s"
	}
	if sawString {
		marker += "t"
	}
	if sawOther {
		marker += "o"
	}
	return marker
}

// literalElementsMarker tags an inferred array type whose element union is
// existential: every arm is witnessed by an actual element of a literal, so
// an arm that contradicts a declared element type proves the whole array
// does. It lives in the Name field, which TypeArray formatting ignores.
const literalElementsMarker = "\x00literal-elements"

// literalAlternativeElementsMarker tags an exact literal whose elements all
// read one unchanged value. Its element union represents alternatives for the
// whole array instead of independently coexisting elements.
const literalAlternativeElementsMarker = "\x00literal-alternative-elements"

// literalPartialAlternativeElementsMarker is the gradual counterpart of an
// alternative literal: its known elements share one choice, while at least
// one other element is unknown.
const literalPartialAlternativeElementsMarker = "\x00literal-partial-alternative-elements"

// literalPartialElementsMarker tags an inferred array whose witnessed arms
// are real elements but do not cover the whole literal (some elements were
// unknown): sound for disjointness, unusable as an element bound.
const literalPartialElementsMarker = "\x00literal-partial-elements"

func literalArrayElementsPartial(ty *TypeExpr) bool {
	return ty != nil &&
		(ty.Name == literalPartialElementsMarker ||
			ty.Name == literalPartialAlternativeElementsMarker)
}

func literalArrayElementsWitnessed(ty *TypeExpr) bool {
	if ty == nil {
		return false
	}
	switch ty.Name {
	case literalElementsMarker,
		literalPartialElementsMarker,
		literalAlternativeElementsMarker,
		literalPartialAlternativeElementsMarker:
		return true
	}
	return false
}

// inferArrayLiteralType infers a witnessed element union for an array
// literal. Empty literals and literals with unknown elements stay a bare
// array.
func (c *scriptChecker) inferArrayLiteralType(lit *ArrayLiteral) *TypeExpr {
	if len(lit.Elements) == 0 {
		return checkTypeArray
	}
	elements := make([]*TypeExpr, 0, len(lit.Elements))
	knownElements := make([]Expression, 0, len(lit.Elements))
	sawUnknown := false
	for _, element := range lit.Elements {
		if _, splat := element.(*SplatArg); splat {
			sawUnknown = true
			continue
		}
		elementType := c.inferExpressionType(element)
		if elementType == nil {
			sawUnknown = true
			continue
		}
		elements = append(elements, elementType)
		knownElements = append(knownElements, element)
	}
	if len(elements) == 0 {
		return checkTypeArray
	}
	union := unionTypeExprs(elements...)
	if union == nil {
		return checkTypeArray
	}
	marker := c.arrayLiteralElementsMarker(knownElements, sawUnknown)
	return &TypeExpr{Kind: TypeArray, Name: marker, TypeArgs: []*TypeExpr{union}}
}

func (c *scriptChecker) arrayLiteralElementsMarker(elements []Expression, partial bool) string {
	alternative := c.arrayLiteralElementsShareValueSource(elements)
	switch {
	case partial && alternative:
		return literalPartialAlternativeElementsMarker
	case partial:
		return literalPartialElementsMarker
	case alternative:
		return literalAlternativeElementsMarker
	default:
		return literalElementsMarker
	}
}

func (c *scriptChecker) arrayLiteralElementsShareValueSource(elements []Expression) bool {
	if len(elements) == 1 {
		return true
	}
	if len(elements) == 0 {
		return false
	}
	source, ok := c.evaluatedValueSourceForExpression(elements[0])
	if !ok {
		return false
	}
	for _, element := range elements[1:] {
		other, ok := c.evaluatedValueSourceForExpression(element)
		if !ok || other != source {
			return false
		}
	}
	return true
}

// applyShovelMutationFacts accounts for the in-place append the shovel
// operator performs on its receiver: a witnessed-element array gains the
// appended element's type as a new witness, and any other container fact is
// poisoned since the checker no longer describes the mutated value.
func (c *scriptChecker) applyShovelMutationFacts(expr *BinaryExpr) {
	if expr.Operator != tokenShovel {
		return
	}
	if _, chained := expr.Left.(*BinaryExpr); chained {
		// A chained shovel ((values << a) << b) appends through the returned
		// receiver. A compatible append against the chain root's declared
		// bound keeps the fact true exactly like the direct form; anything
		// else drops the root fact — the outer appends are not retyped.
		root, isIdent := unwrapShovelChain(expr.Left).(*Identifier)
		if !isIdent {
			if name, ok := rootIdentifierName(unwrapShovelChain(expr.Left)); ok {
				c.poisonElementWriteFacts(name)
			}
			return
		}
		c.applyShovelMutationToLocal(root.Name, expr.Right, false)
		return
	}
	ident, ok := expr.Left.(*Identifier)
	if !ok {
		if name, ok := rootIdentifierName(expr.Left); ok {
			c.poisonElementWriteFacts(name)
		}
		return
	}
	c.applyShovelMutationToLocal(ident.Name, expr.Right, true)
}

func (c *scriptChecker) applyShovelMutationToLocal(name string, value Expression, allowRefine bool) {
	current := c.localTypeFor(name)
	appended := c.inferExpressionType(value)
	hadAliases := c.hasCurrentContainerAlias(name)
	if current == nil {
		if hadAliases {
			c.poisonElementWriteFacts(name)
		}
		return
	}

	preserved := false
	if elem := declaredArrayElementType(current); elem != nil {
		preserved = appended != nil && typeExprSatisfies(appended, elem, c.checkNamedTypeResolver())
		if preserved {
			c.invalidateElementWriteAliases(name, appended)
		} else if allowRefine && appended != nil && c.mutationRegionDepth == 0 && !hadAliases {
			if refined := appendedArrayFact(current, appended); refined != nil {
				c.bindLocalType(name, refined)
				preserved = true
			}
		}
	} else if allowRefine && appended != nil && c.mutationRegionDepth == 0 &&
		!hadAliases && current.Kind == TypeArray && !current.Nullable {
		if refined := appendedArrayFact(current, appended); refined != nil {
			c.bindLocalType(name, refined)
			preserved = true
		}
	}
	if !preserved && c.typeExprHasContainerArm(current) {
		c.poisonElementWriteFacts(name)
	}

	// Updating an exact value can itself discover that the appended value is
	// retained, so it must happen after invalidating or poisoning the pre-write
	// graph. Explicit retained-root links are likewise installed last.
	c.updateShoveledStaticArrayFacts(name, value)
	c.linkContainerWriteAlias(name, value, appended)
}

func (c *scriptChecker) updateShoveledStaticArrayFacts(name string, appended Expression) {
	if c.mutationRegionDepth != 0 {
		c.clearLocalStaticValueAliases(name)
		return
	}
	if !c.hasCurrentContainerAlias(name) {
		c.updateShoveledStaticArrayValues(name, appended)
		return
	}
	// Alias links also represent retained nested containers, not just two
	// names for the same array. Updating every linked value as though it were
	// the shovel receiver would be wrong, so drop their exact value facts
	// while retaining compatible declared type bounds.
	c.clearLocalStaticValueAliases(name)
}

// updateShoveledStaticArrayValues advances an exact local array value through
// one unaliased shovel. Forwarded-call names are read from these whole-value
// facts, so leaving the pre-mutation array in place would resolve a later
// send/public_send against a value the runtime no longer holds.
func (c *scriptChecker) updateShoveledStaticArrayValues(name string, appended Expression) {
	values, exact := c.localStaticValuesFor(name)
	if !exact {
		return
	}
	if _, static := staticLiteralValue(appended); !static {
		c.bindLocalStaticValues(name, nil)
		return
	}
	updated := make([]Expression, 0, len(values))
	replacements := make(map[Expression]Expression, len(values))
	for _, value := range values {
		array, ok := value.(*ArrayLiteral)
		if !ok {
			c.bindLocalStaticValues(name, nil)
			return
		}
		clone := *array
		clone.Elements = append(append([]Expression(nil), array.Elements...), appended)
		replacement := &clone
		updated = append(updated, replacement)
		replacements[value] = replacement
	}
	c.bindLocalStaticValues(name, updated)
	c.replaceEvaluatedDestructureStaticAliases(replacements)
}

func unwrapShovelChain(expr Expression) Expression {
	for {
		binary, ok := expr.(*BinaryExpr)
		if !ok || binary.Operator != tokenShovel {
			return expr
		}
		expr = binary.Left
	}
}

// appendedArrayFact derives the array fact after appending a value of a
// known type: witnessed receivers join the new arm, while annotation-typed
// and bare arrays start a partial witness set with just the appended arm —
// their prior elements were never witnessed (the array may have been
// empty), so only the append may prove a contradiction.
func appendedArrayFact(current, appended *TypeExpr) *TypeExpr {
	switch current.Name {
	case literalElementsMarker, literalPartialElementsMarker:
		if len(current.TypeArgs) != 1 {
			return nil
		}
		union := unionTypeExprs(current.TypeArgs[0], appended)
		if union == nil {
			return nil
		}
		return &TypeExpr{Kind: TypeArray, Name: current.Name, TypeArgs: []*TypeExpr{union}}
	case literalAlternativeElementsMarker, literalPartialAlternativeElementsMarker:
		if len(current.TypeArgs) != 1 {
			return nil
		}
		union := unionTypeExprs(current.TypeArgs[0], appended)
		if union == nil {
			return nil
		}
		marker := literalElementsMarker
		if current.Name == literalPartialAlternativeElementsMarker {
			marker = literalPartialElementsMarker
		}
		return &TypeExpr{Kind: TypeArray, Name: marker, TypeArgs: []*TypeExpr{union}}
	case blockRestElementsMarker:
		if len(current.TypeArgs) == 0 {
			return &TypeExpr{
				Kind:     TypeArray,
				Name:     literalElementsMarker,
				TypeArgs: []*TypeExpr{appended},
			}
		}
		if len(current.TypeArgs) != 1 {
			return nil
		}
		union := unionTypeExprs(current.TypeArgs[0], appended)
		if union == nil {
			return nil
		}
		return &TypeExpr{
			Kind:     TypeArray,
			Name:     literalElementsMarker,
			TypeArgs: []*TypeExpr{union},
		}
	default:
		return &TypeExpr{Kind: TypeArray, Name: literalPartialElementsMarker, TypeArgs: []*TypeExpr{appended}}
	}
}

// declaredArrayElementType returns the element bound of a definite
// annotation-derived array<T> fact: a boundary-validated array whose every
// element is known to satisfy T, so an element write can be checked against
// it. Witnessed literal facts carry no such bound (their unions describe
// elements that exist, not a constraint on writes), and nullable or union
// facts are not definitely arrays.
func declaredArrayElementType(ty *TypeExpr) *TypeExpr {
	if ty == nil || ty.Kind != TypeArray || ty.Nullable {
		return nil
	}
	if literalArrayElementsWitnessed(ty) ||
		ty.Name == blockRestElementsMarker {
		return nil
	}
	if len(ty.TypeArgs) != 1 {
		return nil
	}
	return ty.TypeArgs[0]
}

// declaredHashEntryTypes returns the key and value bounds of a definite
// annotation-derived hash<K, V> fact: a boundary-validated hash whose every
// entry is known to satisfy them, so an entry write can be checked against
// both. Hash facts carry no witness markers, and bare, nullable, or union
// facts are not definitely bounded hashes.
func declaredHashEntryTypes(ty *TypeExpr) (key, value *TypeExpr) {
	if ty == nil || ty.Kind != TypeHash || ty.Nullable || len(ty.TypeArgs) != 2 {
		return nil, nil
	}
	return ty.TypeArgs[0], ty.TypeArgs[1]
}

// reportIncompatibleElementWrite records the diagnostic shared by shovel,
// indexed, and mutator-call element writes whose value can never satisfy the
// receiver's declared element type.
func (c *scriptChecker) reportIncompatibleElementWrite(function string, pos Position, name string, elem, written *TypeExpr) {
	c.add(function, pos, "write to %s expected element %s, got %s",
		name, formatTypeExpr(elem), formatTypeExpr(written))
}

// checkShovelElementWrite reports a shovel append whose value is provably
// disjoint from the receiver's declared element type. Chained shovels append
// through the returned receiver, so the chain root's fact is the one the
// append contradicts. The receiver fact arrives as captured before the right
// operand evaluated — a right side that escapes the same local cannot erase
// the bound the append contradicts — and the check runs before
// applyShovelMutationFacts updates the receiver's fact for the append.
func (c *scriptChecker) checkShovelElementWrite(function string, expr *BinaryExpr, receiverFact *TypeExpr) {
	if expr.Operator != tokenShovel {
		return
	}
	ident, ok := unwrapShovelChain(expr.Left).(*Identifier)
	if !ok {
		return
	}
	elem := declaredArrayElementType(receiverFact)
	if elem == nil {
		return
	}
	written := c.inferExpressionType(expr.Right)
	if written == nil {
		return
	}
	if typedWriteRejected(written, elem, c.checkNamedTypeResolver()) {
		c.reportIncompatibleElementWrite(function, expr.Pos(), ident.Name, elem, written)
	}
}

// applyIndexedWriteFacts checks a direct c[k] = value write against the
// receiver's declared array, hash, or shape fact and reports whether the
// fact still holds (or was refined); every other write weakens through the
// caller's poison. A compatible write keeps the fact true for the receiver
// and every alias, inside regions too — nothing rebinds. receiverFact
// arrives as captured before the value expression walked (the runtime
// evaluates the receiver first); a value that escaped the same local keeps
// the bound for diagnosis, while preservation and refinement require the
// local's fact to have survived the value walk unchanged. abortsBeforeWrite
// reports a key the hash runtime rejects before retaining the value.
func (c *scriptChecker) applyIndexedWriteFacts(
	function string,
	stmt *AssignStmt,
	target *IndexExpr,
	receiverFact *TypeExpr,
) (preserved, abortsBeforeWrite bool) {
	if stmt.Operator != tokenNone {
		return false, false
	}
	ident, ok := target.Object.(*Identifier)
	if !ok {
		return false, false
	}
	if len(target.Indices) != 1 {
		return false, true
	}
	current := c.localTypeFor(ident.Name)
	if receiverFact == nil {
		receiverFact = current
	}
	intact := mutatorReceiverFactIntact(current, receiverFact)
	if elem := declaredArrayElementType(receiverFact); elem != nil {
		return c.applyIndexedArrayWriteFacts(
			function,
			stmt,
			target,
			ident.Name,
			elem,
			intact,
		), false
	}
	if keyBound, valueBound := declaredHashEntryTypes(receiverFact); keyBound != nil {
		if keyType := c.inferExpressionType(target.Indices[0]); keyType != nil &&
			typeExprProvablyUnstorableKey(keyType) {
			return false, true
		}
		return c.applyHashEntryWriteFacts(
			function,
			stmt,
			ident.Name,
			keyBound,
			valueBound,
			target.Indices[0],
			intact,
		), false
	}
	if receiverFact != nil && receiverFact.Kind == TypeShape && !receiverFact.Nullable {
		if keyType := c.inferExpressionType(target.Indices[0]); keyType != nil &&
			typeExprProvablyUnstorableKey(keyType) {
			return false, true
		}
		return c.applyShapeFieldWriteFacts(
			function,
			stmt,
			ident.Name,
			receiverFact,
			target.Indices[0],
			intact,
		), false
	}
	return false, false
}

// applyIndexedArrayWriteFacts checks a direct arr[i] = value element write
// against the receiver's declared element type: a compatible in-bounds write
// replaces one element with another admitted one, so the fact survives.
func (c *scriptChecker) applyIndexedArrayWriteFacts(function string, stmt *AssignStmt, target *IndexExpr, name string, elem *TypeExpr, intact bool) bool {
	// A provably non-numeric index raises before any element is written, so
	// the write never lands and neither diagnosis nor preservation applies.
	if kind, known := staticOperandKind(c.inferExpressionType(target.Indices[0])); known &&
		kind != TypeInt && kind != TypeFloat && kind != TypeNumber {
		return false
	}
	written := c.inferExpressionType(stmt.Value)
	if written == nil {
		return false
	}
	resolve := c.checkNamedTypeResolver()
	// The receiver retains the written element regardless of compatibility,
	// so a container-rooted value's local links in: a later mutation or
	// escape through either side weakens both.
	c.linkContainerWriteAlias(name, stmt.Value, written)
	if typedWriteRejected(written, elem, resolve) {
		c.reportIncompatibleElementWrite(function, stmt.Pos(), name, elem, written)
		return false
	}
	return typeExprSatisfies(written, elem, resolve) && intact
}

// applyHashEntryWriteFacts checks h[k] = value against a declared
// hash<K, V> fact: a key or value provably disjoint from its bound is
// reported, and a provably compatible entry keeps the fact true (hash types
// carry no exactness, so replacing and adding an entry both preserve the
// bound).
func (c *scriptChecker) applyHashEntryWriteFacts(function string, stmt *AssignStmt, name string, keyBound, valueBound *TypeExpr, index Expression, intact bool) bool {
	resolve := c.checkNamedTypeResolver()
	// A provably unsupported key kind raises before the entry is written,
	// so neither the key nor the value diagnostics apply.
	if keyType := c.inferExpressionType(index); keyType != nil && typeExprProvablyUnstorableKey(keyType) {
		return false
	}
	preserved := intact
	if keyType := c.inferExpressionType(index); keyType == nil {
		preserved = false
	} else if hashKeyTypesDisjoint(keyType, keyBound, resolve) {
		c.add(function, stmt.Pos(), "write to %s expected key %s, got %s",
			name, formatTypeExpr(keyBound), formatTypeExpr(keyType))
		preserved = false
	} else if !hashKeyTypeSatisfies(keyType, keyBound, resolve) {
		preserved = false
	}
	if valueType := c.inferExpressionType(stmt.Value); valueType == nil {
		preserved = false
	} else if typedWriteRejected(valueType, valueBound, resolve) {
		c.add(function, stmt.Pos(), "write to %s expected value %s, got %s",
			name, formatTypeExpr(valueBound), formatTypeExpr(valueType))
		preserved = false
	} else if !typeExprSatisfies(valueType, valueBound, resolve) {
		preserved = false
	}
	// The receiver retains the written key and value regardless of
	// compatibility — only the unstorable-key abort above keeps them out —
	// so container roots link in: a later mutation or escape through
	// either side weakens both.
	c.linkContainerWriteAlias(name, index, c.inferExpressionType(index))
	c.linkContainerWriteAlias(name, stmt.Value, c.inferExpressionType(stmt.Value))
	return preserved
}

// applyShapeFieldWriteFacts checks user[:field] = value against a shape
// fact. A declared (marker-less) shape is a boundary contract with an unknown
// key representation: field-type contradictions are reported, as are static
// extra fields on closed shapes. No write preserves the fact, since a same-name
// write through the other representation could add a second key; open-shape
// extras therefore weaken silently. Witnessed literal and JSON.parse_as shapes
// carry their store's key representation and are evidence rather than
// contracts: a matching-representation write with a known key and value
// refines the exact fact in place, and everything else weakens it silently.
func (c *scriptChecker) applyShapeFieldWriteFacts(function string, stmt *AssignStmt, name string, shape *TypeExpr, index Expression, intact bool) bool {
	return c.applyShapeFieldWrite(function, name, shape, index, stmt.Value, stmt.Pos(), intact)
}

// maxRefinedShapeNodes caps the type nodes one chain of field writes may copy
// while it keeps refining a witnessed literal shape. Refining copies the whole
// fact so the one every other holder observed stays intact, and the copy is
// deep, so both the number of writes and the size of what each copies count:
// 2,000 `h[:kN] = 1` writes against a growing literal allocated 520MB, and 800
// writes of one key already in a literal whose other field is an 800-field
// shape allocated 97MB, both quadrupling per doubling inside CheckWarnings
// where no script step or memory quota applies. Counting fields instead of
// nodes only caught the first: the second never widens the shape it copies.
//
// A budget spent by the copies rather than a cap on the shape's width bounds
// the product of the two, and it travels with the fact, so a fresh literal in
// another function starts with the whole budget. The first write to a fact
// always refines, however wide, since the source spells that shape once.
//
// While the budget pays, every write refines the field to exactly what it
// wrote, narrowing a claim wide enough to admit it rather than leaving the
// wider one standing: `nil | int` proves nothing about a branch reading the
// field, `int` proves its else arm dead. Only a write of the type the fact
// already states is free, since refining would rebuild the same fact.
//
// What running out gives up is the claim that the fact names every key, never a
// key it already names. The asymmetry is the point. The checker rules a branch
// out from the type of a field the fact names -- every value but nil and false
// is truthy, so a fact naming an int field proves the else arm of a branch on it
// dead -- and a fact that stops naming that field stops ruling the arm out, so
// the arm is walked and whatever is wrong in it reported. The claim to name
// every key decides nothing by itself, so giving that up costs no diagnostic.
// Review after review found another way to lose a field, so the rule is now one
// rule: keep every field, or keep no fact at all.
//
// Past the budget the fact is open and only one case still costs a copy, the
// claim a write contradicts. Restating it widens it to admit both what the
// field held and what it holds now, which is what makes the writes after it
// free: a key alternating between two types is restated once and admitted from
// then on, and never narrowed again, since narrowing it would let the pair
// alternate forever at one copy per write. That convergence is the whole
// mechanism -- an earlier version restarted the budget instead, which put the
// chain back to refining exactly, re-narrowed the field, and never converged at
// all.
var maxRefinedShapeNodes = 4000

// maxWidenedShapeNodes caps the copies a chain spends widening claims once the
// exact budget is gone. Widening converges -- a claim that admits both types a
// key alternates between never pays again -- so what spends this is a source
// contradicting key after key, one copy each. It is the whole cost a chain can
// reach, so it bounds the work per fact rather than per write.
var maxWidenedShapeNodes = maxRefinedShapeNodes * 8

// typeExprNodes reports how many type nodes a fact holds, memoized by node
// identity. A fact is immutable once built, so a count taken once stays true,
// and a refinement records its own count rather than walking the copy it just
// made.
func (c *scriptChecker) typeExprNodes(ty *TypeExpr) int {
	if ty == nil {
		return 0
	}
	if nodes, counted := c.typeExprNodeCounts[ty]; counted {
		return nodes
	}
	nodes := 1
	for _, arg := range ty.TypeArgs {
		nodes += c.typeExprNodes(arg)
	}
	for _, field := range ty.Shape {
		nodes += c.typeExprNodes(field)
	}
	for _, option := range ty.Union {
		nodes += c.typeExprNodes(option)
	}
	c.noteTypeExprNodes(ty, nodes)
	return nodes
}

func (c *scriptChecker) noteTypeExprNodes(ty *TypeExpr, nodes int) {
	if c.typeExprNodeCounts == nil {
		c.typeExprNodeCounts = make(map[*TypeExpr]int)
	}
	c.typeExprNodeCounts[ty] = nodes
}

// shapeRefinementState is what one chain of field writes has spent and what it
// has already given up, carried on the fact rather than the name so a fresh
// literal elsewhere starts with the whole budget.
type shapeRefinementState struct {
	// nodes is the type nodes this chain's copies have walked.
	nodes int

	// widened names the fields the budget has widened to admit more than one
	// write put there. Those claims never narrow again, which is what makes the
	// writes after them free.
	widened map[string]struct{}
}

func (c *scriptChecker) noteShapeRefinement(ty *TypeExpr, state shapeRefinementState) {
	if c.shapeRefinements == nil {
		c.shapeRefinements = make(map[*TypeExpr]shapeRefinementState)
	}
	c.shapeRefinements[ty] = state
}

// sameTypeFact reports whether a write puts back the fact the field already
// holds, which is what lets the write keep the receiver's fact rather than copy
// it.
//
// This one has to be exact in both directions, which is what separates it from
// the questions typeFactKey answers. Saying two facts differ costs a copy the
// budget pays for. Saying they are the same leaves the receiver bound to the
// fact it had, so getting that wrong means every later read of the field is
// answered from a value the script stopped holding.
//
// typeFactKey cannot carry that weight. It stops at maxTypeArmDepth and writes
// `?` for everything below, which is what makes it cheap to build and what
// makes it an approximation: two shapes alike for eight levels and different
// underneath produce one key. It groups facts correctly and it proves nothing
// about a pair, so a match is a hint here rather than an answer.
//
// Walking is the cheaper answer anyway. Nothing is allocated, the first
// difference ends it rather than both sides being rendered in full first, and
// the same node is the same fact at every level rather than only at the root --
// a write putting back a wide fact the field already holds settles without
// descending into it. What exactness adds is the part below maxTypeArmDepth
// that the truncation was skipping, which is reached only by a fact nested
// deeper than that.
//
// The walk is charged rather than bounded. It is proportional to the written
// fact, which the source had to spell out to produce, and no source found makes
// it grow faster than the source does -- but a source that did would show up in
// this counter rather than nowhere.
func sameTypeFact(existing, written *TypeExpr) bool {
	var walk typeFactWalk
	same := typeFactsIdenticalCounted(existing, written, &walk)
	noteCheckWork(walk.visited)
	return same
}

// typeFactsIdentical reports whether two facts are the same fact, node for
// node. It is the definitive answer typeFactKey only approximates, and the one
// predicate both places that need that answer go through: a truncated key
// cannot settle whether two facts are the same, and two places deciding it by
// different means is how that keeps being rediscovered.
//
// The two reach it differently, and the arity is the reason. A field write
// compares one pair, so it walks and skips the key entirely. Union arm dedup
// compares each new arm against every arm kept so far, so it keeps the key as a
// bucket -- cheap, and wrong only by grouping facts that are not the same --
// and calls this to confirm before dropping one.
func typeFactsIdentical(left, right *TypeExpr) bool {
	var walk typeFactWalk
	same := typeFactsIdenticalCounted(left, right, &walk)
	// Charged like sameTypeFact's: this walk is bounded by the pairs the two
	// facts reach rather than by a budget, so a source that makes it grow
	// faster than its own text shows up in this counter rather than nowhere.
	noteCheckWork(walk.visited)
	return same
}

// typeFactPair names one left/right node pair an exact comparison reached.
type typeFactPair struct{ left, right *TypeExpr }

// typeFactWalk is what one exact comparison accumulates: the node pairs it had
// to compare, and the pairs it has already proved equal.
type typeFactWalk struct {
	visited int
	equal   map[typeFactPair]struct{}
}

// maxUnmemoizedFactPairVisits is how many node pairs a comparison compares
// before it starts recording the ones it proved equal.
//
// A fact is a DAG, not a tree: `x = { a: x, b: x }` binds the same node under
// both keys, so N of those lines build a fact with N nodes and 2^N paths
// through them. Comparing two such facts built independently -- which is what a
// mutator operand that rebinds its receiver hands this, and what two union arms
// sharing a bucket hand it -- never hits the pointer-identity shortcut, so
// walking it as a tree reaches the same child pair once per path. 22 lines took
// 443ms and every line after that doubled it, inside CheckWarnings where no
// script step or memory quota applies.
//
// Remembering the pairs already proved equal walks the DAG as a DAG, and only
// pairs that came back equal need remembering: the first difference ends the
// whole comparison, so an unequal pair is never reached twice.
//
// The threshold is what keeps that free for the comparisons that are not DAGs.
// Ordinary facts settle in a handful of pairs, and allocating a map for each of
// them would cost more than the walk it protects -- these run on every field
// write of a growing shape. Below the threshold nothing is allocated and
// nothing is recorded; a walk that stays small never learns the map exists. A
// walk that crosses it pays at most this many repeated pairs first, which is a
// constant, and is linear in the pairs it reaches from then on.
const maxUnmemoizedFactPairVisits = 64

// typeFactsIdenticalCounted is typeFactsIdentical, counting the nodes it had to
// compare to decide, for the caller that charges the walk.
//
// The fields it compares are the ones typeFactKey renders, so it answers the
// same question the key was standing in for, minus the truncation. Shape is
// compared by lookup rather than by rendering sorted names, which also drops
// the key's ambiguity: a field name holding the key's own delimiters cannot
// make two different shapes agree here.
func typeFactsIdenticalCounted(left, right *TypeExpr, walk *typeFactWalk) bool {
	if left == right {
		return true
	}
	if left == nil || right == nil {
		return false
	}
	pair := typeFactPair{left: left, right: right}
	if _, proved := walk.equal[pair]; proved {
		return true
	}
	walk.visited++
	if left.Kind != right.Kind || left.Name != right.Name ||
		left.Nullable != right.Nullable || left.Optional != right.Optional ||
		left.Open != right.Open ||
		len(left.TypeArgs) != len(right.TypeArgs) ||
		len(left.Shape) != len(right.Shape) ||
		len(left.Union) != len(right.Union) {
		return false
	}
	for i, arg := range left.TypeArgs {
		if !typeFactsIdenticalCounted(arg, right.TypeArgs[i], walk) {
			return false
		}
	}
	for name, field := range left.Shape {
		other, named := right.Shape[name]
		if !named || !typeFactsIdenticalCounted(field, other, walk) {
			return false
		}
	}
	for i, option := range left.Union {
		if !typeFactsIdenticalCounted(option, right.Union[i], walk) {
			return false
		}
	}
	// Recorded on the way back up, so a pair's own subtree is already in the
	// memo when a second edge reaches it.
	if walk.visited > maxUnmemoizedFactPairVisits {
		if walk.equal == nil {
			walk.equal = make(map[typeFactPair]struct{})
		}
		walk.equal[pair] = struct{}{}
	}
	return true
}

// shapeFieldProvenanceRecorded reports whether anything keyed by this fact's
// node identity records how the fact was built, rather than what it states,
// about this field.
//
// A write of the type a fact already states rebuilds that fact exactly, so it
// can keep the node instead of copying it. Keeping the node keeps every map
// keyed by it, and those maps do not all survive a write. Three are keyed this
// way. `typeExprNodeCounts` holds the node's own size and `shapeRefinements`
// holds what the chain of writes has spent: the first is a property of the
// type, which the write did not change, and the second is a property of the
// budget, which the write did spend, so keeping both is right.
// `shapeFieldSources` is provenance -- it records that two fields hold the
// value of the same local, so a union boundary may pick one variant for both --
// and a write from a different local breaks that while nothing about the type
// changes. Copying is what used to drop it, since correlation is keyed by node
// identity and a copy is a new node.
//
// The rule for anything keyed by a fact node later: an association derived from
// the type may be carried across a write that restates the fact, and an
// association derived from how the fact was built may not. Whatever is added of
// the second kind belongs here, so the no-copy path falls through to the copy
// that drops it.
func (c *scriptChecker) shapeFieldProvenanceRecorded(shape *TypeExpr, field string) bool {
	_, recorded := c.shapeFieldSources[shape][field]
	return recorded
}

func widenedShapeFieldsWith(widened map[string]struct{}, key string) map[string]struct{} {
	next := make(map[string]struct{}, len(widened)+1)
	for field := range widened {
		next[field] = struct{}{}
	}
	next[key] = struct{}{}
	return next
}

// applyShapeFieldWrite is the shared core for shape field writes: index
// assignment and the store mutator write one field the same way.
func (c *scriptChecker) applyShapeFieldWrite(function, name string, shape *TypeExpr, index, value Expression, pos Position, intact bool) bool {
	key, keyOK := staticLiteralHashKey(index)
	switch shape.Name {
	case shapeKeysStringMarker, shapeKeysSymbolMarker:
		if !keyOK {
			return false
		}
		repr := indexKeyReprMarker(index)
		marker := shape.Name
		// An empty literal's store has no key representation yet — the
		// first static write establishes it — so the write's representation
		// is adopted rather than matched against the empty-literal default.
		if len(shape.Shape) == 0 &&
			(repr == shapeKeysStringMarker || repr == shapeKeysSymbolMarker) {
			marker = repr
		}
		// String and symbol keys address one entry, so a write through the
		// other spelling still replaces this field.
		receiverAliased := len(c.typeAliases[name]) != 0
		written := c.inferExpressionType(value)
		// The store retains the written value even when aliasing, a mutation
		// region, or a changed receiver fact prevents shape refinement.
		c.linkContainerWriteAlias(name, value, written)
		// Inside a loop or block body a refinement rolls back with the
		// region's state restore, it cannot reach other names sharing the
		// store, and a value walk that changed the local's fact invalidated
		// the captured shape, so all three cases weaken instead.
		if c.mutationRegionDepth != 0 || receiverAliased || !intact {
			return false
		}
		if written == nil {
			return false
		}
		// A write of the type the fact already states rebuilds the same fact, so
		// it needs no copy at all. This is what a run of writes repeating one
		// key costs. Keeping the fact keeps everything keyed by its node, which
		// is only sound while none of that describes where the field's value
		// came from -- see shapeFieldProvenanceRecorded.
		existing, claimed := shape.Shape[key]
		reusable := claimed && !c.shapeFieldProvenanceRecorded(shape, key)
		if reusable && sameTypeFact(existing, written) {
			return true
		}
		resolve := c.checkNamedTypeResolver()
		state := c.shapeRefinements[shape]
		widening := state.nodes > maxRefinedShapeNodes
		if widening {
			// The budget for restating this fact exactly is gone, so the fact
			// has given up claiming to name every key. A write of a key it does
			// not name changes nothing it says.
			if !claimed {
				return shape.Open
			}
			if _, wide := state.widened[key]; wide && reusable &&
				typeExprSatisfies(written, existing, resolve) {
				// This claim was already widened to admit both what the field
				// held and what a write put there, so writes of either are free
				// from here on. Narrowing it again would undo exactly that and
				// let a key alternating between two types pay a copy per write,
				// which is the quadratic the budget exists to refuse. This keeps
				// the fact, so it carries the same condition as the shortcut
				// above; only a fact a copy produced can be widened, and a copy
				// leaves its provenance behind, so today nothing reaches here
				// with any.
				return true
			}
			if state.nodes > maxWidenedShapeNodes {
				// Nothing true is left to say for the price of no copy. Give
				// the fact up rather than rebuild a smaller one: an open shape
				// assembled from whichever fields were convenient asserts
				// nothing false, but it loses the fields it leaves out just as
				// surely, so it buys no diagnostic back and leaves two shapes of
				// degradation to keep right instead of one.
				return false
			}
		}
		copied := c.typeExprNodes(shape)
		noteCheckWork(copied)
		refined := cloneTypeExpr(shape)
		refined.Name = marker
		if refined.Shape == nil {
			refined.Shape = make(map[string]*TypeExpr, 1)
		}
		next := shapeRefinementState{nodes: state.nodes + copied, widened: state.widened}
		if next.nodes > maxRefinedShapeNodes {
			// From the copy that spends the budget onwards the fact stops
			// claiming to name every key, which is what makes the writes after
			// it free. It keeps every field it named.
			refined.Open = true
		}
		if widening && !typeExprSatisfies(written, existing, resolve) {
			refined.Shape[key] = unionTypeExprs(existing, written)
			next.widened = widenedShapeFieldsWith(state.widened, key)
		} else {
			refined.Shape[key] = written
		}
		// The copy's own size is what the next write would pay, and adding a
		// field can only grow it, so charging the field without crediting the
		// one it replaced keeps the estimate on the safe side of the budget.
		c.noteTypeExprNodes(refined, copied+c.typeExprNodes(written)+1)
		c.noteShapeRefinement(refined, next)
		c.bindLocalType(name, refined)
		return true
	case "":
		if !keyOK {
			return false
		}
		field, present := shape.Shape[key]
		if !present {
			if !shape.Open {
				c.add(function, pos, "write to %s adds field %s to exact shape %s",
					name, key, formatTypeExpr(shape))
			}
			return false
		}
		written := c.inferExpressionType(value)
		if written == nil {
			return false
		}
		if typedWriteRejected(written, field, c.checkNamedTypeResolver()) {
			c.add(function, pos, "write to %s field %s expected %s, got %s",
				name, key, formatTypeExpr(field), formatTypeExpr(written))
			return false
		}
		// A compatible write replaces the same unified-key field. Keep the
		// declared fact so later reads still see the required field type,
		// and link a retained container so a later write through that
		// value invalidates this field too.
		receiverAliased := len(c.typeAliases[name]) != 0
		c.linkContainerWriteAlias(name, value, written)
		if c.mutationRegionDepth != 0 || receiverAliased || !intact {
			return false
		}
		return true
	}
	return false
}

// indexKeyReprMarker reports the key-representation marker a static index
// literal writes through, mirroring the read-side dispatch in
// inferIndexExprType. Non-string, non-symbol keys carry no representation.
func indexKeyReprMarker(index Expression) string {
	switch index.(type) {
	case *SymbolLiteral:
		return shapeKeysSymbolMarker
	case *StringLiteral:
		return shapeKeysStringMarker
	}
	return ""
}

// applyIndexedElementWriteFacts checks a direct arr[i] = value element write
// against the receiver's declared element type and reports whether the fact
// still holds: a compatible write replaces one element with another admitted
// one (for the receiver and every alias, inside regions too — nothing
// rebinds), while every other write weakens through the caller's poison.
// receiverFact arrives as captured before the value expression walked; a
// value that escaped the same local keeps the bound for diagnosis, while
// preservation requires the local's fact to have survived unchanged.
func (c *scriptChecker) applyIndexedElementWriteFacts(
	function string,
	stmt *AssignStmt,
	target *IndexExpr,
	receiverFact *TypeExpr,
) (preserved bool, written *TypeExpr, mayWrite, abortsBeforeWrite bool) {
	if stmt.Operator != tokenNone {
		return false, nil, false, false
	}
	ident, ok := target.Object.(*Identifier)
	if !ok {
		return false, c.inferExpressionType(stmt.Value), true, false
	}
	current := c.localTypeFor(ident.Name)
	if receiverFact == nil {
		receiverFact = current
	}
	elem := declaredArrayElementType(receiverFact)
	if elem == nil {
		return false, c.inferExpressionType(stmt.Value), true, false
	}
	if len(target.Indices) != 1 {
		return false, c.inferExpressionType(stmt.Value), true, false
	}
	// A provably non-numeric index raises before any element is written, so
	// the write never lands and neither diagnosis nor preservation applies.
	if kind, known := staticOperandKind(c.inferExpressionType(target.Indices[0])); known &&
		kind != TypeInt && kind != TypeFloat && kind != TypeNumber {
		return false, nil, false, true
	}
	written = c.inferExpressionType(stmt.Value)
	if written == nil {
		return false, nil, true, false
	}
	resolve := c.checkNamedTypeResolver()
	disjoint := typedWriteRejected(written, elem, resolve)
	preserved = !disjoint && typeExprSatisfies(written, elem, resolve) &&
		mutatorReceiverFactIntact(current, receiverFact)
	if disjoint {
		c.reportIncompatibleElementWrite(function, stmt.Pos(), ident.Name, elem, written)
		return false, written, true, false
	}
	return preserved, written, true, false
}

type arrayMutatorWriteModel struct {
	elements     []Expression
	preservable  bool
	mayWrite     bool
	alwaysRaises bool
}

// arrayMutatorElementWrites returns the writes an in-place builtin array
// mutator may perform. insert and fill can mutate through implicit nil padding
// without writing an explicit element expression, so mayWrite is independent
// from elements. A definite keyword argument makes every mutator raise before
// writing.
func arrayMutatorElementWrites(
	call *CallExpr,
	property string,
	argumentFacts map[Expression]*TypeExpr,
	argumentStaticValues map[Expression][]Expression,
	argumentStaticChoices map[Expression]checkStaticChoiceFact,
	blockResult checkBlockResult,
	receiverLength checkArrayReceiverLength,
) (arrayMutatorWriteModel, bool) {
	if call == nil {
		return arrayMutatorWriteModel{}, false
	}
	switch property {
	case "push", "append", "prepend", "unshift", "insert", "fill":
	default:
		return arrayMutatorWriteModel{}, false
	}
	if len(call.KwArgs) != 0 {
		for _, kwarg := range call.KwArgs {
			if !kwarg.Splat {
				return arrayMutatorWriteModel{
					preservable:  true,
					alwaysRaises: true,
				}, true
			}
		}
		return arrayMutatorWriteModel{}, false
	}
	switch property {
	case "push", "append", "prepend", "unshift":
		return arrayMutatorWriteModel{
			elements:    call.Args,
			preservable: true,
			mayWrite:    len(call.Args) > 0,
		}, true
	case "insert":
		if len(call.Args) == 0 {
			return arrayMutatorWriteModel{
				preservable:  true,
				alwaysRaises: true,
			}, true
		}
		// An index-only insert writes nothing: the runtime returns the
		// receiver unchanged after validating the index, so the fact
		// survives. With values the beyond-end nil padding still applies.
		return arrayMutatorWriteModel{
			elements:    call.Args[1:],
			preservable: len(call.Args) == 1,
			mayWrite:    len(call.Args) > 1,
		}, true
	case "fill":
		return arrayFillElementWrites(
			call,
			argumentFacts,
			argumentStaticValues,
			argumentStaticChoices,
			blockResult,
			receiverLength,
		)
	}
	return arrayMutatorWriteModel{}, false
}

func arrayFillElementWrites(
	call *CallExpr,
	argumentFacts map[Expression]*TypeExpr,
	argumentStaticValues map[Expression][]Expression,
	argumentStaticChoices map[Expression]checkStaticChoiceFact,
	blockResult checkBlockResult,
	receiverLength checkArrayReceiverLength,
) (arrayMutatorWriteModel, bool) {
	if call == nil {
		return arrayMutatorWriteModel{}, false
	}
	for _, arg := range call.Args {
		if _, splat := arg.(*SplatArg); splat {
			return arrayMutatorWriteModel{}, false
		}
	}
	blockWrites := func(
		element Expression,
		selectors []Expression,
	) (arrayMutatorWriteModel, bool) {
		// Fill validates arity before invoking its block.
		if len(selectors) > 2 {
			return arrayMutatorWriteModel{
				preservable:  true,
				alwaysRaises: true,
			}, true
		}
		skipped, blockMayRun, selectorsExact := staticArrayFillBlockSelectorOutcomes(
			selectors,
			argumentFacts,
			argumentStaticValues,
			argumentStaticChoices,
			receiverLength,
		)
		if selectorsExact && !blockMayRun {
			return skipped, true
		}
		if !blockResult.exact {
			return arrayMutatorWriteModel{}, false
		}
		if !blockResult.mayComplete {
			if selectorsExact {
				return skipped, true
			}
			return arrayMutatorWriteModel{}, false
		}
		return arrayFillBlockElementWrites(
			element,
			selectors,
			argumentFacts,
			argumentStaticValues,
			argumentStaticChoices,
			receiverLength,
		)
	}
	if call.Block != nil {
		return blockWrites(
			call.Block,
			call.Args,
		)
	}
	return arrayFillValueElementWrites(
		call.Args,
		argumentFacts,
		argumentStaticValues,
		argumentStaticChoices,
		receiverLength,
	)
}

func arrayFillValueElementWrites(
	args []Expression,
	argumentFacts map[Expression]*TypeExpr,
	argumentStaticValues map[Expression][]Expression,
	argumentStaticChoices map[Expression]checkStaticChoiceFact,
	receiverLength checkArrayReceiverLength,
) (arrayMutatorWriteModel, bool) {
	if len(args) < 1 || len(args) > 3 {
		return arrayMutatorWriteModel{
			preservable:  true,
			alwaysRaises: true,
		}, true
	}
	return arrayFillFormElementWrites(
		args[0],
		args[1:],
		argumentFacts,
		argumentStaticValues,
		argumentStaticChoices,
		receiverLength,
	)
}

func arrayFillBlockElementWrites(
	element Expression,
	selectors []Expression,
	argumentFacts map[Expression]*TypeExpr,
	argumentStaticValues map[Expression][]Expression,
	argumentStaticChoices map[Expression]checkStaticChoiceFact,
	receiverLength checkArrayReceiverLength,
) (arrayMutatorWriteModel, bool) {
	if len(selectors) > 2 {
		return arrayMutatorWriteModel{
			preservable:  true,
			alwaysRaises: true,
		}, true
	}
	return arrayFillFormElementWrites(
		element,
		selectors,
		argumentFacts,
		argumentStaticValues,
		argumentStaticChoices,
		receiverLength,
	)
}

func arrayFillFormElementWrites(
	element Expression,
	selectors []Expression,
	argumentFacts map[Expression]*TypeExpr,
	argumentStaticValues map[Expression][]Expression,
	argumentStaticChoices map[Expression]checkStaticChoiceFact,
	receiverLength checkArrayReceiverLength,
) (arrayMutatorWriteModel, bool) {
	if spans, exact := staticArrayFillResolvedSpans(
		receiverLength,
		selectors,
		argumentStaticValues,
		argumentStaticChoices,
	); exact {
		return staticArrayFillResolvedWriteModel(element, spans), true
	}
	if len(selectors) == 0 {
		return arrayMutatorWriteModel{
			elements:    []Expression{element},
			preservable: true,
			mayWrite:    true,
		}, true
	}

	var countValues []Value
	var countStatic bool
	if len(selectors) == 2 {
		countExpr := selectors[1]
		if literalBignum(countExpr) {
			return arrayMutatorWriteModel{
				preservable:  true,
				alwaysRaises: true,
			}, true
		}
		countValues, countStatic = staticMutatorArgumentValues(countExpr, argumentStaticValues)
	}

	startExpr := selectors[0]
	if literalBignum(startExpr) {
		return arrayMutatorWriteModel{
			preservable:  true,
			alwaysRaises: true,
		}, true
	}
	startValues, startStatic := staticMutatorArgumentValues(startExpr, argumentStaticValues)
	if startStatic {
		if len(selectors) == 1 || countStatic {
			if tuples, exact := staticArrayFillSelectorTuples(
				selectors,
				argumentStaticValues,
				argumentStaticChoices,
			); exact {
				return staticArrayFillSelectorWriteModel(element, tuples), true
			}
			return staticArrayFillWriteModel(element, startValues, countValues, len(selectors) == 2), true
		}
		countExpr := selectors[1]
		if !arrayFillSelectorHasNumericOrNilFact(countExpr, argumentFacts) {
			return arrayMutatorWriteModel{}, false
		}
		return staticArrayFillUnknownCountWriteModel(element, startValues), true
	}
	if countStatic {
		model := staticArrayFillUnknownStartWriteModel(element, countValues)
		if !model.mayWrite {
			return model, true
		}
		if !arrayFillSelectorHasNumericOrNilFact(startExpr, argumentFacts) {
			return arrayMutatorWriteModel{}, false
		}
		return model, true
	}
	if !arrayFillSelectorHasNumericOrNilFact(startExpr, argumentFacts) {
		return arrayMutatorWriteModel{}, false
	}
	if len(selectors) == 1 {
		// A bare start at or before the end replaces only existing slots;
		// a start past the end is a no-op rather than a padding operation.
		return arrayMutatorWriteModel{
			elements:    []Expression{element},
			preservable: true,
			mayWrite:    true,
		}, true
	}

	countExpr := selectors[1]
	if !arrayFillSelectorHasNumericOrNilFact(countExpr, argumentFacts) {
		return arrayMutatorWriteModel{}, false
	}
	return arrayMutatorWriteModel{
		elements: []Expression{element},
		mayWrite: true,
	}, true
}

func mergeArrayMutatorWriteModels(models ...arrayMutatorWriteModel) arrayMutatorWriteModel {
	merged := arrayMutatorWriteModel{
		preservable:  true,
		alwaysRaises: true,
	}
	for _, model := range models {
		merged.elements = append(merged.elements, model.elements...)
		merged.preservable = merged.preservable && model.preservable
		merged.mayWrite = merged.mayWrite || model.mayWrite
		merged.alwaysRaises = merged.alwaysRaises && model.alwaysRaises
	}
	return merged
}

func arrayMutatorBuiltinProperty(name string) (string, bool) {
	switch name {
	case "array.push":
		return "push", true
	case "array.append":
		return "append", true
	case "array.prepend":
		return "prepend", true
	case "array.unshift":
		return "unshift", true
	case "array.insert":
		return "insert", true
	case "array.fill":
		return "fill", true
	default:
		return "", false
	}
}

func (c *scriptChecker) arrayMutatorCallMayComplete(call *CallExpr, property string) bool {
	variants, exact := c.staticallyExpandedArrayMutatorCalls(
		call,
		c.callArgumentStaticValues,
		c.callArgumentSplatSources,
	)
	if !exact {
		return !c.arrayMutatorExpansionShapesAlwaysInvalid(call, property)
	}
	for _, variant := range variants {
		if c.arrayMutatorVariantMayComplete(variant, property) {
			return true
		}
	}
	return false
}

type arrayMutatorExpansionShapeGroup struct {
	source     checkCallSplatSource
	positional int
	keyword    int
}

func (c *scriptChecker) arrayMutatorExpansionShapesAlwaysInvalid(
	call *CallExpr,
	property string,
) bool {
	if call == nil || !callExpandsArguments(call) {
		return false
	}
	sourceGroups := arrayMutatorExpansionSourceGroups(call, c.callArgumentSplatSources)
	grouped := make(map[int]*arrayMutatorExpansionShapeGroup)
	directArity := 0
	var independentPositional [][]Expression
	var independentKeyword [][]Expression
	for _, arg := range call.Args {
		splat, expanded := arg.(*SplatArg)
		if !expanded {
			directArity++
			continue
		}
		if group, correlated := sourceGroups[splat.Value]; correlated {
			shape := grouped[group]
			if shape == nil {
				shape = &arrayMutatorExpansionShapeGroup{
					source: c.callArgumentSplatSources[splat.Value],
				}
				grouped[group] = shape
			}
			shape.positional++
			continue
		}
		values, exact := c.callArgumentStaticValues[splat.Value]
		if !exact || len(values) == 0 {
			return false
		}
		independentPositional = append(independentPositional, values)
	}
	for _, kwarg := range call.KwArgs {
		if !kwarg.Splat {
			return true
		}
		if group, correlated := sourceGroups[kwarg.Value]; correlated {
			shape := grouped[group]
			if shape == nil {
				shape = &arrayMutatorExpansionShapeGroup{
					source: c.callArgumentSplatSources[kwarg.Value],
				}
				grouped[group] = shape
			}
			shape.keyword++
			continue
		}
		values, exact := c.callArgumentStaticValues[kwarg.Value]
		if !exact || len(values) == 0 {
			return false
		}
		independentKeyword = append(independentKeyword, values)
	}

	clamp := arrayMutatorShapeArityClamp(property)
	arities := []int{min(directArity, clamp)}
	for _, shape := range grouped {
		contributions := c.arrayMutatorShapeGroupContributions(shape, clamp)
		if len(contributions) == 0 {
			return true
		}
		arities = combineArrayMutatorShapeArities(arities, contributions, clamp)
	}
	for _, alternatives := range independentPositional {
		contributions := make([]int, 0, len(alternatives))
		for _, alternative := range alternatives {
			if array, ok := alternative.(*ArrayLiteral); ok {
				contributions = append(contributions, min(len(array.Elements), clamp))
			}
		}
		if len(contributions) == 0 {
			return true
		}
		arities = combineArrayMutatorShapeArities(arities, contributions, clamp)
	}
	for _, alternatives := range independentKeyword {
		valid := false
		for _, alternative := range alternatives {
			hash, ok := alternative.(*HashLiteral)
			if ok && c.arrayMutatorKeywordExpansionIsEmpty(hash) {
				valid = true
				break
			}
		}
		if !valid {
			return true
		}
	}
	for _, arity := range arities {
		if c.arrayMutatorShapeArityMayComplete(call, property, arity) {
			return false
		}
	}
	return true
}

func arrayMutatorShapeArityClamp(property string) int {
	switch property {
	case "insert":
		return 1
	case "fill":
		return 4
	default:
		return 1
	}
}

func (c *scriptChecker) arrayMutatorShapeGroupContributions(
	group *arrayMutatorExpansionShapeGroup,
	clamp int,
) []int {
	if group == nil {
		return nil
	}
	contributions := make([]int, 0, len(group.source.alternatives))
	for _, alternative := range group.source.alternatives {
		contribution := 0
		if group.positional > 0 {
			array, ok := alternative.(*ArrayLiteral)
			if !ok {
				continue
			}
			for range group.positional {
				contribution = min(contribution+len(array.Elements), clamp)
			}
		}
		if group.keyword > 0 {
			hash, ok := alternative.(*HashLiteral)
			if !ok || !c.arrayMutatorKeywordExpansionIsEmpty(hash) {
				continue
			}
		}
		contributions = append(contributions, contribution)
	}
	return contributions
}

func (c *scriptChecker) arrayMutatorKeywordExpansionIsEmpty(hash *HashLiteral) bool {
	return hash != nil &&
		(hash.ShapeType == nil || c.hashShapeStaticallyShadowed(hash)) &&
		len(hash.Pairs) == 0
}

func combineArrayMutatorShapeArities(current, added []int, clamp int) []int {
	seen := make([]bool, clamp+1)
	for _, left := range current {
		for _, right := range added {
			seen[min(left+right, clamp)] = true
		}
	}
	combined := make([]int, 0, len(seen))
	for arity, possible := range seen {
		if possible {
			combined = append(combined, arity)
		}
	}
	return combined
}

func (c *scriptChecker) arrayMutatorShapeArityMayComplete(
	call *CallExpr,
	property string,
	arity int,
) bool {
	switch property {
	case "push", "append", "prepend", "unshift":
		return true
	case "insert":
		return arity >= 1
	case "fill":
		switch {
		case call.Block != nil:
			return arity <= 2
		default:
			return arity >= 1 && arity <= 3
		}
	default:
		return true
	}
}

func (c *scriptChecker) arrayMutatorVariantMayComplete(
	variant arrayMutatorCallVariant,
	property string,
) bool {
	if variant.expansionRaises || variant.call == nil || len(variant.call.KwArgs) > 0 {
		return false
	}
	switch property {
	case "push", "append", "prepend", "unshift":
		return true
	case "insert":
		if len(variant.call.Args) == 0 {
			return false
		}
		return c.arrayInsertIndexMayComplete(variant.call.Args[0])
	case "fill":
		return c.arrayFillCallMayComplete(variant.call)
	default:
		return true
	}
}

func (c *scriptChecker) arrayInsertIndexMayComplete(index Expression) bool {
	values, exact := staticMutatorArgumentValues(index, c.callArgumentStaticValues)
	if exact {
		for _, value := range values {
			if value.Kind() == KindInt || value.Kind() == KindFloat {
				if _, err := valueToInt(value); err == nil {
					return true
				}
			}
		}
		return false
	}
	fact, captured := c.callArgumentFacts[index]
	if !captured {
		fact = c.inferExpressionType(index)
	}
	kind, known := staticOperandKind(fact)
	return !known || kind == TypeInt || kind == TypeFloat || kind == TypeNumber
}

func (c *scriptChecker) arrayFillCallMayComplete(call *CallExpr) bool {
	switch {
	case call == nil:
		return true
	case call.Block != nil:
		return c.arrayFillFormMayComplete(call.Args)
	default:
		return c.arrayFillValueFormMayComplete(call.Args)
	}
}

func (c *scriptChecker) arrayFillCallMayCompleteWithoutInvokingBlock(call *CallExpr) bool {
	variants, exact := c.staticallyExpandedArrayMutatorCalls(
		call,
		c.callArgumentStaticValues,
		c.callArgumentSplatSources,
	)
	if !exact {
		return true
	}
	for _, variant := range variants {
		if variant.expansionRaises || variant.call == nil || len(variant.call.KwArgs) > 0 {
			continue
		}
		switch {
		case variant.call.Block != nil:
			skipped, _, selectorsExact := staticArrayFillBlockSelectorOutcomes(
				variant.call.Args,
				c.callArgumentFacts,
				c.callArgumentStaticValues,
				c.callArgumentStaticChoices,
				c.callArrayReceiverLength,
			)
			if !selectorsExact || !skipped.alwaysRaises {
				return true
			}
		default:
			if c.arrayFillValueFormMayComplete(variant.call.Args) {
				return true
			}
		}
	}
	return false
}

func (c *scriptChecker) arrayFillValueFormMayComplete(args []Expression) bool {
	return len(args) >= 1 && len(args) <= 3 &&
		c.arrayFillFormMayComplete(args[1:])
}

func (c *scriptChecker) arrayFillFormMayComplete(selectors []Expression) bool {
	if len(selectors) > 2 {
		return false
	}
	if spans, exact := staticArrayFillResolvedSpans(
		c.callArrayReceiverLength,
		selectors,
		c.callArgumentStaticValues,
		c.callArgumentStaticChoices,
	); exact {
		return len(spans) > 0
	}
	if len(selectors) == 0 {
		return true
	}
	if tuples, exact := staticArrayFillSelectorTuples(
		selectors,
		c.callArgumentStaticValues,
		c.callArgumentStaticChoices,
	); exact {
		for _, selectorTuple := range tuples {
			if len(selectorTuple) == 1 {
				if staticArrayFillStartMayComplete(selectorTuple[0], false) {
					return true
				}
				continue
			}
			if staticArrayFillStartCountMayComplete(selectorTuple[0], selectorTuple[1]) {
				return true
			}
		}
		return false
	}
	startValues, startExact := staticMutatorArgumentValues(
		selectors[0],
		c.callArgumentStaticValues,
	)
	var countValues []Value
	countExact := false
	if len(selectors) == 2 {
		countValues, countExact = staticMutatorArgumentValues(
			selectors[1],
			c.callArgumentStaticValues,
		)
	}
	if startExact {
		startMayComplete := false
		for _, start := range startValues {
			if staticArrayFillStartMayComplete(start, len(selectors) == 2) {
				startMayComplete = true
				break
			}
		}
		if !startMayComplete {
			return false
		}
	} else if !arrayFillSelectorFactMayComplete(
		c.callArgumentFacts[selectors[0]],
		len(selectors) == 1,
	) {
		return false
	}
	if len(selectors) == 1 {
		return true
	}
	if countExact {
		return slices.ContainsFunc(countValues, staticArrayFillCountMayComplete)
	}
	return arrayFillSelectorFactMayComplete(c.callArgumentFacts[selectors[1]], false)
}

func staticArrayFillStartMayComplete(value Value, hasCount bool) bool {
	if value.Kind() == KindRange {
		if hasCount {
			return false
		}
		_, known := staticArrayFillRangeWrites(value.Range())
		return known
	}
	_, _, valid := staticArrayFillInteger(value)
	return valid
}

func staticArrayFillCountMayComplete(value Value) bool {
	_, _, valid := staticArrayFillInteger(value)
	return valid
}

func staticArrayFillStartCountMayComplete(startValue, countValue Value) bool {
	start, _, startValid := staticArrayFillInteger(startValue)
	count, nilLength, countValid := staticArrayFillInteger(countValue)
	if !startValid || !countValid {
		return false
	}
	if nilLength || count <= 0 || start < 0 {
		return true
	}
	return start <= math.MaxInt-count
}

func arrayFillSelectorFactMayComplete(fact *TypeExpr, allowRange bool) bool {
	arms, known := typeExprArms(fact, 0)
	if !known || len(arms) == 0 {
		return true
	}
	for _, arm := range arms {
		switch arm.Kind {
		case TypeInt, TypeFloat, TypeNumber, TypeNil:
			return true
		case TypeRange:
			if allowRange {
				return true
			}
		}
	}
	return false
}

func (c *scriptChecker) arrayFillBlockMayEvaluate(call *CallExpr) bool {
	if call == nil || call.Block == nil {
		return false
	}
	variants, exact := c.staticallyExpandedArrayMutatorCalls(
		call,
		c.callArgumentStaticValues,
		c.callArgumentSplatSources,
	)
	if !exact {
		return true
	}
	for _, variant := range variants {
		if variant.expansionRaises || variant.call == nil || len(variant.call.KwArgs) > 0 {
			continue
		}
		if variant.call.Block == nil {
			continue
		}
		_, blockMayRun, selectorsExact := staticArrayFillBlockSelectorOutcomes(
			variant.call.Args,
			c.callArgumentFacts,
			c.callArgumentStaticValues,
			c.callArgumentStaticChoices,
			c.callArrayReceiverLength,
		)
		if !selectorsExact || blockMayRun {
			return true
		}
	}
	return false
}

func staticArrayFillWriteModel(
	element Expression,
	startValues, countValues []Value,
	hasCount bool,
) arrayMutatorWriteModel {
	model := arrayMutatorWriteModel{preservable: true}
	for _, startValue := range startValues {
		if !hasCount {
			mergeArrayFillWriteEffect(
				&model,
				element,
				staticArrayFillWriteEffect(startValue, NewNil(), false),
			)
			continue
		}
		for _, countValue := range countValues {
			mergeArrayFillWriteEffect(
				&model,
				element,
				staticArrayFillWriteEffect(startValue, countValue, true),
			)
		}
	}
	return model
}

func staticArrayFillSelectorWriteModel(
	element Expression,
	selectorTuples [][]Value,
) arrayMutatorWriteModel {
	model := arrayMutatorWriteModel{preservable: true}
	for _, selectors := range selectorTuples {
		switch len(selectors) {
		case 1:
			mergeArrayFillWriteEffect(
				&model,
				element,
				staticArrayFillWriteEffect(selectors[0], NewNil(), false),
			)
		case 2:
			mergeArrayFillWriteEffect(
				&model,
				element,
				staticArrayFillWriteEffect(selectors[0], selectors[1], true),
			)
		}
	}
	return model
}

func staticArrayFillBlockSelectorOutcomes(
	selectors []Expression,
	argumentFacts map[Expression]*TypeExpr,
	argumentStaticValues map[Expression][]Expression,
	argumentStaticChoices map[Expression]checkStaticChoiceFact,
	receiverLength checkArrayReceiverLength,
) (arrayMutatorWriteModel, bool, bool) {
	model := arrayMutatorWriteModel{
		preservable:  true,
		alwaysRaises: true,
	}
	if len(selectors) > 2 {
		return model, false, true
	}
	if spans, exact := staticArrayFillResolvedSpans(
		receiverLength,
		selectors,
		argumentStaticValues,
		argumentStaticChoices,
	); exact {
		return staticArrayFillResolvedBlockSelectorOutcomes(spans)
	}
	selectorTuples, exact := staticArrayFillSelectorTuples(
		selectors,
		argumentStaticValues,
		argumentStaticChoices,
	)
	if !exact {
		return staticArrayFillPartialBlockSelectorOutcomes(
			selectors,
			argumentFacts,
			argumentStaticValues,
		)
	}
	blockMayRun := false
	for _, selectorTuple := range selectorTuples {
		tupleModel, tupleMayRun := staticArrayFillBlockSelectorOutcome(selectorTuple)
		model = mergeArrayMutatorWriteModels(model, tupleModel)
		blockMayRun = blockMayRun || tupleMayRun
	}
	return model, blockMayRun, true
}

func staticArrayFillPartialBlockSelectorOutcomes(
	selectors []Expression,
	argumentFacts map[Expression]*TypeExpr,
	argumentStaticValues map[Expression][]Expression,
) (arrayMutatorWriteModel, bool, bool) {
	model := arrayMutatorWriteModel{
		preservable:  true,
		alwaysRaises: true,
	}
	recordSkip := func(mayPad bool) {
		model.alwaysRaises = false
		model.mayWrite = model.mayWrite || mayPad
		model.preservable = model.preservable && !mayPad
	}
	switch len(selectors) {
	case 1:
		start := selectors[0]
		fact, captured := argumentFacts[start]
		if captured && !arrayFillSelectorFactMayComplete(fact, true) {
			return model, false, true
		}
		if arrayFillSelectorHasNumericOrNilFact(start, argumentFacts) {
			// A bare numeric start can land before the end or at/past it.
			// The latter path is a no-op and never pads.
			recordSkip(false)
			return model, true, true
		}
		return arrayMutatorWriteModel{}, false, false
	case 2:
	default:
		return arrayMutatorWriteModel{}, false, false
	}

	startValues, startExact := staticMutatorArgumentValues(
		selectors[0],
		argumentStaticValues,
	)
	countValues, countExact := staticMutatorArgumentValues(
		selectors[1],
		argumentStaticValues,
	)
	if countExact {
		startMayComplete := true
		if fact, captured := argumentFacts[selectors[0]]; captured {
			startMayComplete = arrayFillSelectorFactMayComplete(fact, false)
		}
		blockMayRun := false
		for _, countValue := range countValues {
			count, nilLength, valid := staticArrayFillInteger(countValue)
			if !valid || !startMayComplete {
				continue
			}
			switch {
			case nilLength:
				// A nil count fills to the end. Depending on the start and
				// receiver length, the block may run or the call may no-op.
				recordSkip(false)
				blockMayRun = true
			case count < 0:
				recordSkip(false)
			case count == 0:
				// With no exact start, a valid positive start can grow the
				// receiver even though the block is never invoked.
				recordSkip(arrayFillStartFactMayBePositive(argumentFacts[selectors[0]]))
			default:
				blockMayRun = true
			}
		}
		return model, blockMayRun, true
	}
	if !startExact ||
		!arrayFillSelectorHasNumericOrNilFact(selectors[1], argumentFacts) {
		return arrayMutatorWriteModel{}, false, false
	}

	countNilOnly := typeExprIsNilOnly(argumentFacts[selectors[1]])
	blockMayRun := false
	for _, startValue := range startValues {
		start, _, valid := staticArrayFillInteger(startValue)
		if !valid {
			continue
		}
		blockMayRun = true
		if countNilOnly {
			recordSkip(false)
			continue
		}
		// A dynamic numeric count may be negative or zero. Those paths skip
		// the block; only a positive start paired with zero can add nil padding.
		recordSkip(start > 0)
	}
	return model, blockMayRun, true
}

func arrayFillStartFactMayBePositive(fact *TypeExpr) bool {
	arms, known := typeExprArms(fact, 0)
	if !known || len(arms) == 0 {
		return true
	}
	for _, arm := range arms {
		switch arm.Kind {
		case TypeInt, TypeFloat, TypeNumber:
			return true
		}
	}
	return false
}

func staticArrayFillSelectorTuples(
	selectors []Expression,
	argumentStaticValues map[Expression][]Expression,
	argumentStaticChoices map[Expression]checkStaticChoiceFact,
) ([][]Value, bool) {
	if len(selectors) > 2 {
		return nil, false
	}
	if len(selectors) == 0 {
		return [][]Value{nil}, true
	}
	selectorValues := make([][]Value, len(selectors))
	for i, selector := range selectors {
		values, exact := staticMutatorArgumentValues(selector, argumentStaticValues)
		if !exact {
			return nil, false
		}
		selectorValues[i] = values
	}
	if len(selectorValues) == 1 {
		tuples := make([][]Value, 0, len(selectorValues[0]))
		for _, value := range selectorValues[0] {
			tuples = append(tuples, []Value{value})
		}
		return tuples, true
	}

	leftChoice, leftCorrelated := argumentStaticChoices[selectors[0]]
	rightChoice, rightCorrelated := argumentStaticChoices[selectors[1]]
	correlated := leftCorrelated &&
		rightCorrelated &&
		len(leftChoice.indices) == len(selectorValues[0]) &&
		len(rightChoice.indices) == len(selectorValues[1]) &&
		sameCheckCallSplatSource(leftChoice.source, rightChoice.source)
	tuples := make([][]Value, 0, len(selectorValues[0])*len(selectorValues[1]))
	for leftIndex, start := range selectorValues[0] {
		for rightIndex, count := range selectorValues[1] {
			if correlated && leftChoice.indices[leftIndex] != rightChoice.indices[rightIndex] {
				continue
			}
			tuples = append(tuples, []Value{start, count})
		}
	}
	if correlated && len(tuples) == 0 {
		return nil, false
	}
	return tuples, true
}

type staticArrayFillResolvedSpan struct {
	arrayFillSpan
	receiverLength int
}

func staticArrayFillResolvedSpans(
	receiverLength checkArrayReceiverLength,
	selectors []Expression,
	argumentStaticValues map[Expression][]Expression,
	argumentStaticChoices map[Expression]checkStaticChoiceFact,
) ([]staticArrayFillResolvedSpan, bool) {
	if !receiverLength.exact {
		return nil, false
	}
	if len(selectors) > 2 {
		return nil, true
	}
	selectorTuples, exact := staticArrayFillSelectorTuples(
		selectors,
		argumentStaticValues,
		argumentStaticChoices,
	)
	if !exact {
		return nil, false
	}

	spans := make([]staticArrayFillResolvedSpan, 0, len(selectorTuples))
	for _, selectorTuple := range selectorTuples {
		span, err := arrayFillResolveSpan(selectorTuple, receiverLength.length)
		if err != nil {
			continue
		}
		spans = append(spans, staticArrayFillResolvedSpan{
			arrayFillSpan:  span,
			receiverLength: receiverLength.length,
		})
	}
	return spans, true
}

func staticArrayFillResolvedBlockSelectorOutcomes(
	spans []staticArrayFillResolvedSpan,
) (arrayMutatorWriteModel, bool, bool) {
	model := arrayMutatorWriteModel{
		preservable:  true,
		alwaysRaises: true,
	}
	blockMayRun := false
	for _, span := range spans {
		if span.end > span.begin {
			blockMayRun = true
			continue
		}
		mayPad := span.finalLength > span.receiverLength
		model.alwaysRaises = false
		model.mayWrite = model.mayWrite || mayPad
		model.preservable = model.preservable && !mayPad
	}
	return model, blockMayRun, true
}

func staticArrayFillResolvedWriteModel(
	element Expression,
	spans []staticArrayFillResolvedSpan,
) arrayMutatorWriteModel {
	model := arrayMutatorWriteModel{
		preservable:  true,
		alwaysRaises: len(spans) == 0,
	}
	writesValue := false
	for _, span := range spans {
		writesValue = writesValue || span.end > span.begin
		model.mayWrite = model.mayWrite ||
			span.end > span.begin ||
			span.finalLength > span.receiverLength
		if span.finalLength > span.receiverLength &&
			span.begin > span.receiverLength {
			model.preservable = false
		}
	}
	if writesValue {
		model.elements = []Expression{element}
	}
	return model
}

func staticArrayFillBlockSelectorOutcome(
	selectors []Value,
) (arrayMutatorWriteModel, bool) {
	model := arrayMutatorWriteModel{
		preservable:  true,
		alwaysRaises: true,
	}
	recordSkip := func(mayPad bool) {
		model.alwaysRaises = false
		model.mayWrite = model.mayWrite || mayPad
		model.preservable = model.preservable && !mayPad
	}
	switch len(selectors) {
	case 0:
		// An empty receiver completes without invoking the block; every
		// nonempty receiver invokes it.
		recordSkip(false)
		return model, true
	case 1, 2:
	default:
		return model, false
	}

	startValue := selectors[0]
	if startValue.Kind() == KindRange {
		if len(selectors) == 2 {
			return model, false
		}
		effect, valid := staticArrayFillRangeWrites(startValue.Range())
		if !valid {
			return model, false
		}
		blockMayRun := effect.writesValue
		if !staticArrayFillRangeAlwaysInvokesBlock(startValue.Range()) {
			start := startValue.Range().Start
			if startValue.Range().Beginless {
				start = 0
			}
			recordSkip(start > 0)
		}
		return model, blockMayRun
	}

	start, _, startValid := staticArrayFillInteger(startValue)
	if !startValid {
		return model, false
	}
	if len(selectors) == 1 {
		// A bare start may reach or pass the end of some receiver length.
		// Those completing spans are no-ops; shorter starts invoke the block.
		recordSkip(false)
		return model, true
	}

	countValue := selectors[1]
	count, nilLength, countValid := staticArrayFillInteger(countValue)
	if !countValid {
		return model, false
	}
	if !staticArrayFillStartCountMayComplete(startValue, countValue) {
		return model, false
	}
	switch {
	case nilLength:
		recordSkip(false)
		return model, true
	case count < 0:
		recordSkip(false)
		return model, false
	case count == 0:
		recordSkip(start > 0)
		return model, false
	default:
		// A positive explicit count invokes the block even when it grows an
		// empty receiver.
		return model, true
	}
}

func staticArrayFillRangeAlwaysInvokesBlock(rng Range) bool {
	if rng.Beginless {
		rng.Start = 0
	}
	if rng.Endless {
		// A negative start either fails validation on a receiver that is too
		// short or resolves strictly before its end, so every completing call
		// invokes the block.
		return rng.Start < 0
	}
	end := rng.End
	if !rng.Exclusive {
		if end == math.MaxInt64 {
			return false
		}
		end++
	}
	switch {
	case rng.Start >= 0 && rng.End >= 0:
		return end > rng.Start
	case rng.Start < 0 && rng.End < 0:
		return end > rng.Start
	default:
		return false
	}
}

func staticArrayFillUnknownCountWriteModel(
	element Expression,
	startValues []Value,
) arrayMutatorWriteModel {
	model := arrayMutatorWriteModel{preservable: true}
	for _, startValue := range startValues {
		if startValue.Kind() == KindRange {
			continue
		}
		start, _, valid := staticArrayFillInteger(startValue)
		if !valid {
			continue
		}
		model.elements = []Expression{element}
		model.mayWrite = true
		if start > 0 {
			model.preservable = false
		}
	}
	return model
}

func staticArrayFillUnknownStartWriteModel(
	element Expression,
	countValues []Value,
) arrayMutatorWriteModel {
	model := arrayMutatorWriteModel{preservable: true}
	for _, countValue := range countValues {
		count, nilLength, valid := staticArrayFillInteger(countValue)
		if !valid || count < 0 && !nilLength {
			continue
		}
		model.mayWrite = true
		if nilLength {
			model.elements = []Expression{element}
			continue
		}
		model.preservable = false
		if count > 0 {
			model.elements = []Expression{element}
		}
	}
	return model
}

func staticArrayFillWriteEffect(
	startValue, countValue Value,
	hasCount bool,
) arrayFillRangeWriteEffect {
	if startValue.Kind() == KindRange {
		if hasCount {
			return arrayFillRangeWriteEffect{preservable: true}
		}
		effect, known := staticArrayFillRangeWrites(startValue.Range())
		if !known {
			return arrayFillRangeWriteEffect{preservable: true}
		}
		return effect
	}
	start, _, valid := staticArrayFillInteger(startValue)
	if !valid {
		return arrayFillRangeWriteEffect{preservable: true}
	}
	if !hasCount {
		return arrayFillRangeWriteEffect{
			writesValue: true,
			mayWrite:    true,
			preservable: true,
		}
	}
	count, nilLength, valid := staticArrayFillInteger(countValue)
	if !valid || count < 0 && !nilLength {
		return arrayFillRangeWriteEffect{preservable: true}
	}
	if nilLength {
		return arrayFillRangeWriteEffect{
			writesValue: true,
			mayWrite:    true,
			preservable: true,
		}
	}
	if count == 0 {
		return arrayFillRangeWriteEffect{
			mayWrite:    start > 0,
			preservable: start <= 0,
		}
	}
	return arrayFillRangeWriteEffect{
		writesValue: true,
		mayWrite:    true,
		preservable: start <= 0,
	}
}

func mergeArrayFillWriteEffect(
	model *arrayMutatorWriteModel,
	element Expression,
	effect arrayFillRangeWriteEffect,
) {
	if effect.writesValue {
		model.elements = []Expression{element}
	}
	model.mayWrite = model.mayWrite || effect.mayWrite
	model.preservable = model.preservable && effect.preservable
}

func staticMutatorArgumentValues(
	expr Expression,
	argumentStaticValues map[Expression][]Expression,
) ([]Value, bool) {
	values, captured := argumentStaticValues[expr]
	if !captured {
		if value, static := staticLiteralValue(expr); static {
			return []Value{value}, true
		}
		return nil, false
	}
	if len(values) == 0 {
		return nil, false
	}
	result := make([]Value, 0, len(values))
	for _, candidate := range values {
		value, static := staticLiteralValue(candidate)
		if !static {
			return nil, false
		}
		result = append(result, value)
	}
	return result, true
}

func staticArrayFillInteger(value Value) (n int, nilValue, valid bool) {
	if value.Kind() == KindNil {
		return 0, true, true
	}
	if value.Kind() != KindInt && value.Kind() != KindFloat {
		return 0, false, false
	}
	n, err := valueToInt(value)
	return n, false, err == nil
}

func literalBignum(expr Expression) bool {
	value, exact := integerLiteralValue(expr)
	return exact && value.IsBigInt()
}

func integerLiteralValue(expr Expression) (Value, bool) {
	switch typed := expr.(type) {
	case *IntegerLiteral:
		if typed.Big != nil {
			return newBigIntValue(typed.Big), true
		}
		return NewInt(typed.Value), true
	case *UnaryExpr:
		value, exact := integerLiteralValue(typed.Right)
		if !exact {
			return NewNil(), false
		}
		switch typed.Operator {
		case tokenPlus:
			return value, true
		case tokenMinus:
			if value.IsBigInt() || value.Int() == math.MinInt64 {
				return negIntValueBig(value), true
			}
			return NewInt(-value.Int()), true
		}
	}
	return NewNil(), false
}

func arrayFillSelectorHasNumericOrNilFact(
	expr Expression,
	argumentFacts map[Expression]*TypeExpr,
) bool {
	fact, captured := argumentFacts[expr]
	if !captured {
		return false
	}
	return typeExprArmsAll(fact, func(arm *TypeExpr) bool {
		switch arm.Kind {
		case TypeInt, TypeFloat, TypeNumber, TypeNil:
			return true
		default:
			return false
		}
	})
}

type arrayFillRangeWriteEffect struct {
	writesValue bool
	mayWrite    bool
	preservable bool
}

func staticArrayFillRangeWrites(rng Range) (arrayFillRangeWriteEffect, bool) {
	start := rng.Start
	if rng.Beginless {
		start = 0
	}
	if start > int64(math.MaxInt) {
		return arrayFillRangeWriteEffect{}, false
	}
	preservable := start <= 0
	if rng.Endless {
		return arrayFillRangeWriteEffect{
			writesValue: true,
			mayWrite:    true,
			preservable: preservable,
		}, true
	}
	if rng.End > int64(math.MaxInt) {
		return arrayFillRangeWriteEffect{}, false
	}
	end := rng.End
	if !rng.Exclusive {
		if end == int64(math.MaxInt) {
			return arrayFillRangeWriteEffect{}, false
		}
		end++
	}
	var writesValue bool
	switch {
	case start >= 0 && rng.End >= 0:
		writesValue = end > start
	case start >= 0:
		// A negative end is relative to the receiver length. Some receiver
		// length always makes the resolved end exceed a finite start.
		writesValue = true
	case rng.End < 0:
		// Both bounds shift by the receiver length, so their ordering is
		// independent of that length once the negative start is valid.
		writesValue = end > start
	default:
		// A negative start first becomes valid with resolved begin zero.
		writesValue = end > 0
	}
	return arrayFillRangeWriteEffect{
		writesValue: writesValue,
		mayWrite:    writesValue || start > 0,
		preservable: preservable,
	}, true
}

func arrayInsertIndexCannotPad(
	index Expression,
	argumentStaticValues map[Expression][]Expression,
) bool {
	values, static := staticMutatorArgumentValues(index, argumentStaticValues)
	if !static {
		return false
	}
	for _, value := range values {
		if value.Kind() != KindInt && value.Kind() != KindFloat {
			return false
		}
		n, err := valueToInt(value)
		if err != nil || n > 0 {
			return false
		}
	}
	return true
}

func arrayMutatorRetainsArgumentsWithoutCalling(call *CallExpr, property string, receiver *TypeExpr) bool {
	receiver = nonNilMutatorReceiverFact(receiver)
	if receiver == nil || !typeExprArmsAll(receiver, func(arm *TypeExpr) bool {
		return arm.Kind == TypeArray
	}) {
		return false
	}
	model, ok := arrayMutatorElementWrites(
		call,
		property,
		nil,
		nil,
		nil,
		checkBlockResult{},
		checkArrayReceiverLength{},
	)
	if !ok {
		return false
	}
	for _, element := range model.elements {
		if _, splat := element.(*SplatArg); splat {
			return false
		}
	}
	return true
}

// applyContainerMutatorCallFacts checks the writes an in-place builtin
// container mutator call performs against the receiver's declared fact.
// preserved reports whether every write is provably compatible, in which
// case the receiver's fact still holds and the caller skips its escape
// poison. modeled reports whether the call's argument effects are fully
// accounted for — the builtin only reads and retains its arguments, with
// retention tracked through container write aliases — so the caller also
// skips the generic argument escape poison that would otherwise cascade
// through those aliases and undo the preservation. Both the receiver fact
// and the argument facts are read as captured at their own evaluation
// points: the receiver evaluates before any argument, so an argument that
// escapes the same local cannot erase the bound the writes contradict.
// mayWrite reports that at least one position can be mutated, so true no-op
// calls keep exact value facts.
func (c *scriptChecker) applyContainerMutatorCallFacts(
	function string,
	call, checkedCall *CallExpr,
	member *MemberExpr,
	argumentFacts map[Expression]*TypeExpr,
	argumentStaticValues map[Expression][]Expression,
	argumentStaticChoices map[Expression]checkStaticChoiceFact,
	argumentRetainedAliases map[Expression]checkRetainedContainerCapture,
	argumentSplatOrigins map[Expression][]*SplatArg,
	argumentSplatSources map[Expression]checkCallSplatSource,
	blockResult checkBlockResult,
	receiverFact *TypeExpr,
	receiverLength checkArrayReceiverLength,
) (preserved, modeled, mayWrite bool) {
	ident, ok := member.Object.(*Identifier)
	if !ok {
		return false, false, false
	}
	// Dispatch only proceeds on the non-nil path (safe navigation skips
	// nil and a plain member call on nil raises), so the non-nil arm
	// bounds the writes; preservation still compares the original fact.
	contentFact := nonNilMutatorReceiverFact(receiverFact)
	if contentFact != nil && typeExprArmsAll(contentFact, func(arm *TypeExpr) bool {
		return arm.Kind == TypeArray
	}) {
		return c.applyArrayMutatorCallFacts(
			function,
			call,
			checkedCall,
			member,
			argumentFacts,
			argumentStaticValues,
			argumentStaticChoices,
			argumentRetainedAliases,
			argumentSplatOrigins,
			argumentSplatSources,
			blockResult,
			receiverFact,
			receiverLength,
		)
	}
	if keyBound, valueBound := declaredHashEntryTypes(contentFact); keyBound != nil {
		// A hash<K, V> boundary may be backed by an object, whose member
		// dispatch returns a same-named entry before the builtin. With a
		// non-callable value bound such a field can only raise (no write
		// lands), but a callable one could dispatch anywhere, so modeling
		// requires ruling callables out.
		if typeExprMayIncludeCallable(valueBound) {
			return false, false, false
		}
		preserved, modeled := c.applyHashMutatorCallFacts(
			function,
			call,
			checkedCall,
			member,
			argumentFacts,
			ident.Name,
			receiverFact,
			contentFact,
			keyBound,
			valueBound,
		)
		return preserved, modeled, c.hashMutatorCallMayWrite(
			checkedCall,
			member.Property,
			argumentFacts,
		)
	}
	if contentFact != nil && contentFact.Kind == TypeShape && !contentFact.Nullable {
		preserved, modeled := c.applyShapeMutatorCallFacts(
			function,
			call,
			checkedCall,
			member,
			argumentFacts,
			ident.Name,
			receiverFact,
			contentFact,
		)
		return preserved, modeled, c.shapeMutatorCallMayWrite(
			checkedCall,
			member.Property,
			argumentFacts,
		)
	}
	return false, false, false
}

func (c *scriptChecker) containerMutatorCallProvablyAborts(
	call *CallExpr,
	receiverFact *TypeExpr,
	argumentFacts map[Expression]*TypeExpr,
) bool {
	if call == nil {
		return false
	}
	member, ok := call.Callee.(*MemberExpr)
	if !ok {
		return false
	}
	if _, direct := member.Object.(*Identifier); !direct {
		return false
	}
	switch member.Property {
	case "store", "replace":
		return c.hashMutatorCallProvablyAborts(
			call,
			"hash."+member.Property,
			receiverFact,
			argumentFacts,
		)
	default:
		return false
	}
}

// hashMutatorCallProvablyAborts reports a builtin hash mutation that cannot
// reach a write after its arguments evaluate. Exact shapes can be backed by
// hashes, where the builtin wins, or objects, where a same-named field wins.
// A non-callable field therefore falls through to the builtin checks:
// both backing kinds abort only when the builtin call also must abort.
func (c *scriptChecker) hashMutatorCallProvablyAborts(
	call *CallExpr,
	name string,
	receiverFact *TypeExpr,
	argumentFacts map[Expression]*TypeExpr,
) bool {
	if call == nil {
		return false
	}
	property := ""
	switch name {
	case "hash.store":
		property = "store"
	case "hash.replace":
		property = "replace"
	default:
		return false
	}
	contentFact := nonNilMutatorReceiverFact(receiverFact)
	if _, valueBound := declaredHashEntryTypes(contentFact); valueBound != nil {
		if typeExprMayIncludeCallable(valueBound) {
			return false
		}
	} else if contentFact != nil && contentFact.Kind == TypeShape && !contentFact.Nullable {
		if contentFact.Name == "" {
			if field, present := contentFact.Shape[property]; present {
				if typeExprMayIncludeCallable(shapeFieldValueType(field)) {
					return false
				}
			} else if contentFact.Open {
				return false
			}
		}
	} else {
		return false
	}
	switch property {
	case "store":
		if !c.hashMutatorCallMayHaveNoKeywords(call, argumentFacts) ||
			!storeCallArityMayMatch(call) {
			return true
		}
		// An unresolved splat may supply the missing positions (or expand
		// empty beside two fixed arguments), so no individual AST argument
		// is guaranteed to be the runtime key until expansion completes.
		if callHasSplatArg(call) {
			return false
		}
		keyType := c.mutatorCallArgumentFact(call.Args[0], argumentFacts)
		return keyType != nil && typeExprProvablyUnstorableKey(keyType)
	case "replace":
		// An unresolved splat may still expand to exactly one hash or no
		// keywords. Exact splats have already been expanded in checkedCall,
		// so only the remaining dynamic shapes stay gradual here.
		if callHasSplatArg(call) {
			return false
		}
		for _, kwarg := range call.KwArgs {
			if kwarg.Splat {
				return false
			}
		}
		if len(call.Args) != 1 || len(call.KwArgs) != 0 {
			return true
		}
		written := c.mutatorCallArgumentFact(call.Args[0], argumentFacts)
		return written != nil && typeExprProvablyNotHash(written)
	default:
		return false
	}
}

// storeCallArityMayMatch reports whether runtime splat expansion can produce
// Hash#store's two positional arguments. Non-splat arguments set a minimum;
// any unresolved splat may contribute the remaining positions, including none.
func storeCallArityMayMatch(call *CallExpr) bool {
	fixed := 0
	hasSplat := false
	for _, arg := range call.Args {
		if _, splat := arg.(*SplatArg); splat {
			hasSplat = true
			continue
		}
		fixed++
	}
	return fixed <= 2 && (hasSplat || fixed == 2)
}

// hashMutatorCallMayHaveNoKeywords reports whether keyword expansion can leave
// the runtime keyword map empty. Named keywords and required fields make the
// rejected map provably nonempty; exact empty, optional-only, generic, and
// unknown hash facts retain the successful empty-map path.
func (c *scriptChecker) hashMutatorCallMayHaveNoKeywords(
	call *CallExpr,
	argumentFacts map[Expression]*TypeExpr,
) bool {
	for _, kwarg := range call.KwArgs {
		if !kwarg.Splat {
			return false
		}
		fact := c.mutatorCallArgumentFact(kwarg.Value, argumentFacts)
		if !c.typeFactMayBeEmptyKeywordHash(fact) {
			return false
		}
	}
	return true
}

type arrayMutatorCallVariant struct {
	call            *CallExpr
	splatOrigins    map[Expression][]*SplatArg
	expansionRaises bool
	choices         map[int]int
}

// staticallyExpandedArrayMutatorCalls preserves each exact positional or
// keyword splat alternative as a complete normalized call. Repeated reads of
// the same local reuse one alternative choice, retaining the runtime value's
// correlation, while independent splat sources form a bounded Cartesian
// product.
func (c *scriptChecker) staticallyExpandedArrayMutatorCalls(
	call *CallExpr,
	argumentStaticValues map[Expression][]Expression,
	argumentSplatSources map[Expression]checkCallSplatSource,
) ([]arrayMutatorCallVariant, bool) {
	const maxVariants = 32

	if call == nil {
		return nil, false
	}
	if !callExpandsArguments(call) {
		return []arrayMutatorCallVariant{{call: call}}, true
	}
	base := *call
	base.Args = nil
	base.KwArgs = nil
	sourceGroups := arrayMutatorExpansionSourceGroups(call, argumentSplatSources)
	variants := []arrayMutatorCallVariant{{call: &base}}
	for _, arg := range call.Args {
		splat, ok := arg.(*SplatArg)
		if !ok {
			for i := range variants {
				variantCall := *variants[i].call
				variantCall.Args = append(
					append([]Expression(nil), variantCall.Args...),
					arg,
				)
				variants[i].call = &variantCall
			}
			continue
		}
		values, captured := argumentStaticValues[splat.Value]
		if !captured || len(values) == 0 {
			return nil, false
		}
		choiceGroup, correlated := sourceGroups[splat.Value]
		next := make([]arrayMutatorCallVariant, 0, min(maxVariants, len(variants)*len(values)))
		for _, variant := range variants {
			indices := make([]int, len(values))
			for i := range values {
				indices[i] = i
			}
			if correlated {
				if selected, exists := variant.choices[choiceGroup]; exists {
					if selected >= len(values) {
						return nil, false
					}
					indices = []int{selected}
				}
			}
			for _, index := range indices {
				if len(next) >= maxVariants {
					return nil, false
				}
				variantCall := *variant.call
				origins := cloneArrayMutatorSplatOrigins(variant.splatOrigins)
				expansionRaises := variant.expansionRaises
				if array, ok := values[index].(*ArrayLiteral); ok {
					variantCall.Args = append(
						append([]Expression(nil), variant.call.Args...),
						array.Elements...,
					)
					if origins == nil {
						origins = make(map[Expression][]*SplatArg)
					}
					for _, element := range array.Elements {
						origins[element] = append(origins[element], splat)
					}
				} else {
					variantCall.Args = append([]Expression(nil), variant.call.Args...)
					expansionRaises = true
				}
				choices := cloneArrayMutatorExpansionChoices(variant.choices)
				if correlated {
					if choices == nil {
						choices = make(map[int]int)
					}
					choices[choiceGroup] = index
				}
				next = append(next, arrayMutatorCallVariant{
					call:            &variantCall,
					splatOrigins:    origins,
					expansionRaises: expansionRaises,
					choices:         choices,
				})
			}
		}
		variants = next
	}
	for _, kwarg := range call.KwArgs {
		if !kwarg.Splat {
			for i := range variants {
				variantCall := *variants[i].call
				variantCall.KwArgs = append(
					append([]KeywordArg(nil), variantCall.KwArgs...),
					kwarg,
				)
				variants[i].call = &variantCall
			}
			continue
		}
		values, captured := argumentStaticValues[kwarg.Value]
		if !captured || len(values) == 0 {
			return nil, false
		}
		choiceGroup, correlated := sourceGroups[kwarg.Value]
		next := make([]arrayMutatorCallVariant, 0, min(maxVariants, len(variants)*len(values)))
		for _, variant := range variants {
			indices := make([]int, len(values))
			for i := range values {
				indices[i] = i
			}
			if correlated {
				if selected, exists := variant.choices[choiceGroup]; exists {
					if selected >= len(values) {
						return nil, false
					}
					indices = []int{selected}
				}
			}
			for _, index := range indices {
				if len(next) >= maxVariants {
					return nil, false
				}
				variantCall := *variant.call
				variantCall.KwArgs = append([]KeywordArg(nil), variant.call.KwArgs...)
				expansionRaises := variant.expansionRaises
				hash, isHash := values[index].(*HashLiteral)
				if !isHash {
					expansionRaises = true
				} else if hash.ShapeType != nil && !c.hashShapeStaticallyShadowed(hash) {
					expansionRaises = true
				} else if len(hash.Pairs) > 0 {
					normalized := kwarg
					normalized.Splat = false
					variantCall.KwArgs = append(variantCall.KwArgs, normalized)
				}
				choices := cloneArrayMutatorExpansionChoices(variant.choices)
				if correlated {
					if choices == nil {
						choices = make(map[int]int)
					}
					choices[choiceGroup] = index
				}
				next = append(next, arrayMutatorCallVariant{
					call:            &variantCall,
					splatOrigins:    cloneArrayMutatorSplatOrigins(variant.splatOrigins),
					expansionRaises: expansionRaises,
					choices:         choices,
				})
			}
		}
		variants = next
	}
	return variants, true
}

func arrayMutatorExpansionSourceGroups(
	call *CallExpr,
	argumentSplatSources map[Expression]checkCallSplatSource,
) map[Expression]int {
	groups := make(map[Expression]int)
	var sources []checkCallSplatSource
	record := func(expr Expression) {
		source, captured := argumentSplatSources[expr]
		if !captured {
			return
		}
		for group, existing := range sources {
			if sameCheckCallSplatSource(source, existing) {
				groups[expr] = group
				return
			}
		}
		groups[expr] = len(sources)
		sources = append(sources, source)
	}
	for _, arg := range call.Args {
		if splat, expanded := arg.(*SplatArg); expanded {
			record(splat.Value)
		}
	}
	for _, kwarg := range call.KwArgs {
		if kwarg.Splat {
			record(kwarg.Value)
		}
	}
	return groups
}

func sameCheckCallSplatSource(left, right checkCallSplatSource) bool {
	if left.evaluation != nil || right.evaluation != nil {
		if left.evaluation == nil || left.evaluation != right.evaluation {
			return false
		}
	} else {
		if len(left.identity) == 0 || len(left.identity) != len(right.identity) {
			return false
		}
		for i := range left.identity {
			if left.identity[i] != right.identity[i] {
				return false
			}
		}
	}
	if len(left.alternatives) != len(right.alternatives) {
		return false
	}
	for i := range left.alternatives {
		if left.alternatives[i] != right.alternatives[i] {
			return false
		}
	}
	return true
}

// typeFactMayBeEmptyKeywordHash reports whether a fact admits a hash whose
// keyword expansion is valid and contributes no entries.
func (c *scriptChecker) typeFactMayBeEmptyKeywordHash(fact *TypeExpr) bool {
	if fact == nil {
		return true
	}
	arms, ok := typeExprArms(fact, 0)
	if !ok {
		return true
	}
	resolve := c.checkNamedTypeResolver()
	for _, arm := range arms {
		if _, shapeValue := shapeValuePayload(arm); shapeValue {
			continue
		}
		if arm.Kind != TypeShape {
			if !typeExprsDisjoint(arm, checkTypeHash, resolve) {
				return true
			}
			continue
		}
		empty := true
		for _, fieldType := range arm.Shape {
			if !shapeFieldOptional(fieldType) {
				empty = false
				break
			}
		}
		if empty {
			return true
		}
	}
	return false
}

func (c *scriptChecker) shapeMutatorCallMayWrite(
	call *CallExpr,
	property string,
	argumentFacts map[Expression]*TypeExpr,
) bool {
	if call == nil || len(call.KwArgs) != 0 {
		return false
	}
	switch property {
	case "store":
		if len(call.Args) != 2 {
			return false
		}
		keyType := c.mutatorCallArgumentFact(call.Args[0], argumentFacts)
		return keyType == nil || !typeExprProvablyUnstorableKey(keyType)
	case "replace":
		if len(call.Args) != 1 || callHasSplatArg(call) {
			return false
		}
		written := c.mutatorCallArgumentFact(call.Args[0], argumentFacts)
		return written == nil || !typeExprProvablyNotHash(written)
	default:
		return false
	}
}

// applyShapeMutatorCallFacts checks the fields an in-place builtin hash
// mutator writes against a shape receiver fact: store is index assignment,
// merge!/update fold entries into the existing store, and replace validates
// the complete adopted hash. Shape exactness also pins the object-backed
// shadowing risk: dispatch can only be shadowed by a field named like the
// mutator. A non-callable field aborts on an object-backed receiver when
// present, so any continuing path must be the hash builtin and can be modeled.
func (c *scriptChecker) applyShapeMutatorCallFacts(
	function string,
	call, checkedCall *CallExpr,
	member *MemberExpr,
	argumentFacts map[Expression]*TypeExpr,
	name string,
	receiverFact, shape *TypeExpr,
) (preserved, modeled bool) {
	writesCall := call
	if checkedCall != nil {
		writesCall = checkedCall
	}
	if len(writesCall.KwArgs) != 0 {
		return false, false
	}
	// A declared shape may be backed by KindObject, whose stored fields resolve
	// before hash builtins. Any same-named field can therefore prevent the
	// mutator from dispatching; its value need not be callable. Witnessed shape
	// markers pin a KindHash, where the builtin wins over stored data.
	if shape.Name == "" {
		if field, present := shape.Shape[member.Property]; present {
			if typeExprMayIncludeCallable(shapeFieldValueType(field)) {
				return false, false
			}
		} else if shape.Open {
			return false, false
		}
	}
	switch member.Property {
	case "store":
		if len(writesCall.Args) != 2 {
			return false, false
		}
		// store canonicalizes its key before writing, so a provably
		// unsupported key kind raises without storing anything.
		if keyType := c.mutatorCallArgumentFact(writesCall.Args[0], argumentFacts); keyType != nil &&
			typeExprProvablyUnstorableKey(keyType) {
			return false, true
		}
		return c.applyShapeFieldWrite(function, name, shape, writesCall.Args[0], writesCall.Args[1], writesCall.Args[0].Pos(),
			c.mutatorCallPreservable(call, name, receiverFact)), true
	case "replace":
		if len(writesCall.Args) != 1 || callHasSplatArg(writesCall) {
			return false, false
		}
		arg := writesCall.Args[0]
		written := c.mutatorCallArgumentFact(arg, argumentFacts)
		if written != nil && typeExprProvablyNotHash(written) {
			return false, true
		}
		// A witnessed shape pins a concrete key representation. Replacing it
		// from another hash may change that representation even when the
		// logical fields match, so only annotation-declared shapes preserve.
		if shape.Name != "" {
			return false, false
		}
		preserved = c.mutatorCallPreservable(call, name, receiverFact)
		if lit, isLiteral := arg.(*HashLiteral); isLiteral && lit.ShapeType == nil {
			compatible := c.checkShapeReplacementLiteral(function, name, shape, lit)
			return preserved && compatible, true
		}
		// replace copies entries rather than retaining the source hash root,
		// but the two stores can share nested container keys and values.
		// Linking the source conservatively preserves that interior aliasing.
		c.linkContainerWriteAlias(name, arg, written)
		if written == nil {
			return false, true
		}
		resolve := c.checkNamedTypeResolver()
		if typeExprHashLikeOnly(written) && typedWriteRejected(written, shape, resolve) {
			c.add(function, arg.Pos(), "write to %s expected %s, got %s",
				name, formatTypeExpr(shape), formatTypeExpr(written))
			return false, true
		}
		if typeExprHasOpenShapeArm(written) ||
			!typeExprSatisfies(written, shape, resolve) {
			return false, true
		}
		return preserved, true
	}
	return false, false
}

// checkShapeReplacementLiteral validates every effective entry in a literal
// replacement and also checks that every required declared field survives the
// whole-store replacement. Logical field lookup uses display names, while
// physical keys remain distinct so a string/symbol display collision violates
// exactness just as it does during runtime shape validation.
func (c *scriptChecker) checkShapeReplacementLiteral(
	function, name string,
	shape *TypeExpr,
	lit *HashLiteral,
) bool {
	compatible := true
	supplied := make(map[string]string, len(lit.Pairs))
	resolve := c.checkNamedTypeResolver()
	for _, pair := range effectiveHashLiteralPairs(lit) {
		key, keyOK := staticLiteralHashKey(pair.Key)
		physicalKey, physicalKeyOK := staticLiteralHashIdentity(pair.Key)
		keyType := c.inferExpressionType(pair.Key)
		valueType := c.inferExpressionType(pair.Value)
		c.linkContainerWriteAlias(name, pair.Key, keyType)
		c.linkContainerWriteAlias(name, pair.Value, valueType)
		if !keyOK || !physicalKeyOK {
			compatible = false
			continue
		}
		if previous, present := supplied[key]; present && previous != physicalKey {
			c.add(function, pair.Key.Pos(), "write to %s adds field %s to exact shape %s",
				name, key, formatTypeExpr(shape))
			compatible = false
			continue
		}
		supplied[key] = physicalKey
		field, present := shape.Shape[key]
		if !present {
			c.add(function, pair.Key.Pos(), "write to %s adds field %s to exact shape %s",
				name, key, formatTypeExpr(shape))
			compatible = false
			continue
		}
		field = shapeFieldValueType(field)
		if valueType == nil {
			compatible = false
			continue
		}
		if typedWriteRejected(valueType, field, resolve) {
			c.add(function, pair.Value.Pos(), "write to %s field %s expected %s, got %s",
				name, key, formatTypeExpr(field), formatTypeExpr(valueType))
			compatible = false
			continue
		}
		if !typeExprSatisfies(valueType, field, resolve) {
			compatible = false
		}
	}
	fields := make([]string, 0, len(shape.Shape))
	for field, fieldType := range shape.Shape {
		if shapeFieldOptional(fieldType) {
			continue
		}
		fields = append(fields, field)
	}
	slices.Sort(fields)
	for _, field := range fields {
		if _, present := supplied[field]; present {
			continue
		}
		c.add(function, lit.Pos(), "write to %s removes required field %s from exact shape %s",
			name, field, formatTypeExpr(shape))
		compatible = false
	}
	return compatible
}

func typeExprProvablyUnstorableKey(ty *TypeExpr) bool {
	return typeExprArmsAll(ty, func(arm *TypeExpr) bool {
		if _, isShapeValue := shapeValuePayload(arm); isShapeValue {
			return true
		}
		switch arm.Kind {
		case TypeHash, TypeShape, TypeFunction, TypeTime, TypeDuration, TypeMoney:
			return true
		}
		return false
	})
}

// typeExprProvablyNotHash reports a fact no arm of which can be a hash at
// runtime: shapes are hashes, first-class shape values are not, and named
// arms stay conservatively hash-possible (an enum's values are opaque).
func typeExprProvablyNotHash(ty *TypeExpr) bool {
	return typeExprArmsAll(ty, func(arm *TypeExpr) bool {
		if _, isShapeValue := shapeValuePayload(arm); isShapeValue {
			return true
		}
		switch arm.Kind {
		case TypeHash, TypeShape, TypeEnum:
			return false
		}
		return true
	})
}

// hashBoundsContainerFree reports whether every key and value the bounds
// admit is a scalar or nominal kind, so entries copied out of a source hash
// share no mutable containers with it. Unknown and any-typed bounds stay
// conservatively container-possible.
func hashBoundsContainerFree(keyBound, valueBound *TypeExpr) bool {
	containerFree := func(ty *TypeExpr) bool {
		return typeExprArmsAll(ty, func(arm *TypeExpr) bool {
			if _, isShapeValue := shapeValuePayload(arm); isShapeValue {
				return false
			}
			switch arm.Kind {
			case TypeArray, TypeHash, TypeShape:
				return false
			}
			return true
		})
	}
	return containerFree(keyBound) && containerFree(valueBound)
}

// mutatorCallArgumentFact reads an argument's fact as captured at its own
// evaluation point, falling back to the current state.
func (c *scriptChecker) mutatorCallArgumentFact(arg Expression, argumentFacts map[Expression]*TypeExpr) *TypeExpr {
	if written, captured := argumentFacts[arg]; captured {
		return written
	}
	return c.inferExpressionType(arg)
}

// mutatorCallPreservable reports whether a mutator call's receiver fact may
// survive at all: the mutators return their receiver (or, for store, a value
// the receiver retains), so a consumed result (a chained call, an argument,
// an assignment value) hands the container to code the checker cannot
// follow, and an argument walk that poisoned or rebound the local already
// invalidated the captured fact. Only a statement-level call whose local
// still carries the captured fact can keep the declared bound.
//
// "Still carries" has to be decided, not grouped, so it walks. The write the
// call applies lands on the captured fact and is bound back to the local, so
// answering yes about a local an argument rebound puts a fact the script
// stopped holding back in place.
func (c *scriptChecker) mutatorCallPreservable(call *CallExpr, name string, receiverFact *TypeExpr) bool {
	if Expression(call) != c.expressionStatementRoot {
		return false
	}
	current := c.localTypeFor(name)
	return current != nil && typeFactsIdentical(current, receiverFact)
}

func cloneArrayMutatorExpansionChoices(choices map[int]int) map[int]int {
	if len(choices) == 0 {
		return nil
	}
	clone := make(map[int]int, len(choices))
	maps.Copy(clone, choices)
	return clone
}

func cloneArrayMutatorSplatOrigins(
	origins map[Expression][]*SplatArg,
) map[Expression][]*SplatArg {
	if len(origins) == 0 {
		return nil
	}
	clone := make(map[Expression][]*SplatArg, len(origins))
	for expression, expressionOrigins := range origins {
		clone[expression] = append([]*SplatArg(nil), expressionOrigins...)
	}
	return clone
}

// applyArrayMutatorCallFacts checks the elements an in-place builtin array
// mutator writes against the receiver's declared element type. preserved
// reports whether every write is provably compatible, in which case the
// receiver's fact still holds and the caller skips its escape poison.
// modeled reports whether the call's argument effects are fully accounted
// for — the builtin only reads and retains its arguments, with retention
// tracked through container write aliases — so the caller also skips the
// generic argument escape poison that would otherwise cascade through those
// aliases and undo the preservation. mayWrite reports that at least one
// element position can be mutated, so true no-op calls keep exact value
// facts. The receiver fact and argument type/static facts are read as captured
// at their own evaluation points: the receiver evaluates before any argument,
// so an argument that escapes the same local cannot erase the bound the writes
// contradict. Preservation additionally requires the local's fact to have
// survived the argument walk unchanged.
func (c *scriptChecker) applyArrayMutatorCallFacts(
	function string,
	call, checkedCall *CallExpr,
	member *MemberExpr,
	argumentFacts map[Expression]*TypeExpr,
	argumentStaticValues map[Expression][]Expression,
	argumentStaticChoices map[Expression]checkStaticChoiceFact,
	argumentRetainedAliases map[Expression]checkRetainedContainerCapture,
	argumentSplatOrigins map[Expression][]*SplatArg,
	argumentSplatSources map[Expression]checkCallSplatSource,
	blockResult checkBlockResult,
	receiverFact *TypeExpr,
	receiverLength checkArrayReceiverLength,
) (preserved, modeled, mayWrite bool) {
	ident, ok := member.Object.(*Identifier)
	if !ok {
		return false, false, false
	}
	receiver := nonNilMutatorReceiverFact(receiverFact)
	if receiver == nil || !typeExprArmsAll(receiver, func(arm *TypeExpr) bool {
		return arm.Kind == TypeArray
	}) {
		return false, false, false
	}
	writesCall := call
	if checkedCall != nil {
		writesCall = checkedCall
	}
	variants := []arrayMutatorCallVariant{{
		call:         writesCall,
		splatOrigins: argumentSplatOrigins,
	}}
	if callExpandsArguments(writesCall) {
		if expanded, exact := c.staticallyExpandedArrayMutatorCalls(
			writesCall,
			argumentStaticValues,
			argumentSplatSources,
		); exact {
			variants = expanded
		}
	}
	elem := declaredArrayElementType(receiver)
	resolve := c.checkNamedTypeResolver()
	model := arrayMutatorWriteModel{
		preservable:  true,
		alwaysRaises: true,
	}
	modelSplatOrigins := make(map[Expression][]*SplatArg)
	var elementVariantIDs []int
	for variantID, variant := range variants {
		var variantModel arrayMutatorWriteModel
		if variant.expansionRaises {
			variantModel = arrayMutatorWriteModel{
				preservable:  true,
				alwaysRaises: true,
			}
		} else {
			var ok bool
			variantModel, ok = arrayMutatorElementWrites(
				variant.call,
				member.Property,
				argumentFacts,
				argumentStaticValues,
				argumentStaticChoices,
				blockResult,
				receiverLength,
			)
			if !ok {
				return false, false, false
			}
		}
		// insert validates its index before any element lands. A
		// provably non-numeric index therefore contributes only a
		// raising, non-mutating outcome, while an unresolved splatted
		// index keeps the call gradual.
		if !variant.expansionRaises && member.Property == "insert" && len(variant.call.Args) > 0 {
			if _, isSplat := variant.call.Args[0].(*SplatArg); isSplat {
				return false, true, false
			}
			index, captured := argumentFacts[variant.call.Args[0]]
			if !captured {
				index = c.inferExpressionType(variant.call.Args[0])
			}
			if kind, known := staticOperandKind(index); known &&
				kind != TypeInt && kind != TypeFloat && kind != TypeNumber {
				variantModel = arrayMutatorWriteModel{
					preservable:  true,
					alwaysRaises: true,
				}
			} else if len(variantModel.elements) > 0 &&
				(arrayInsertIndexCannotPad(variant.call.Args[0], argumentStaticValues) ||
					elem != nil && typeExprSatisfies(checkTypeNil, elem, resolve)) {
				// Zero inserts at the front and every negative index
				// either resolves inside the array or raises before
				// writing. Neither successful case can introduce nil
				// padding. An element bound that already admits nil
				// also survives any padding.
				variantModel.preservable = true
			}
		}
		if member.Property == "fill" && !variantModel.preservable && elem != nil &&
			typeExprSatisfies(checkTypeNil, elem, resolve) {
			variantModel.preservable = true
		}
		originOffsets := make(map[Expression]int)
		for _, element := range variantModel.elements {
			origins := variant.splatOrigins[element]
			offset := originOffsets[element]
			if offset < len(origins) {
				modelSplatOrigins[element] = append(
					modelSplatOrigins[element],
					origins[offset],
				)
				originOffsets[element]++
			}
			elementVariantIDs = append(elementVariantIDs, variantID)
		}
		model.elements = append(model.elements, variantModel.elements...)
		model.preservable = model.preservable && variantModel.preservable
		model.mayWrite = model.mayWrite || variantModel.mayWrite
		model.alwaysRaises = model.alwaysRaises && variantModel.alwaysRaises
	}
	argumentSplatOrigins = modelSplatOrigins
	mayWrite = model.mayWrite
	// The mutators return their receiver, so a consumed result (a chained
	// call, an argument, an assignment value) hands the array to code the
	// checker cannot follow: only a statement-level call, whose value is
	// discarded, can keep the declared bound. A call that always raises has
	// no value to escape, so a rescue can retain the receiver even when the
	// call appears in a consumed expression. In either case the argument
	// walk must have left the local's fact unchanged (an argument may poison
	// or rebind the same local).
	preserved = model.preservable &&
		(model.alwaysRaises || Expression(call) == c.expressionStatementRoot) &&
		mutatorReceiverFactIntact(c.localTypeFor(ident.Name), receiverFact)
	if !mayWrite {
		return preserved, true, false
	}
	splatOriginOffsets := make(map[Expression]int)
	nextSplatOrigin := func(arg Expression) *SplatArg {
		origins := argumentSplatOrigins[arg]
		offset := splatOriginOffsets[arg]
		if offset >= len(origins) {
			return nil
		}
		splatOriginOffsets[arg]++
		return origins[offset]
	}
	linkRetainedElement := func(arg Expression, written *TypeExpr, splat *SplatArg) {
		if splat != nil {
			if captured, ok := argumentRetainedAliases[splat]; ok {
				c.linkCapturedContainerWriteAliases(ident.Name, captured)
				return
			}
		}
		if captured, ok := argumentRetainedAliases[arg]; ok {
			c.linkCapturedContainerWriteAliases(ident.Name, captured)
			return
		}
		c.linkContainerWriteAlias(ident.Name, arg, written)
	}
	if elem == nil {
		for _, arg := range model.elements {
			if _, splat := arg.(*SplatArg); splat {
				return false, false, true
			}
			written, captured := argumentFacts[arg]
			if !captured {
				written = c.inferExpressionType(arg)
			}
			linkRetainedElement(arg, written, nextSplatOrigin(arg))
		}
		return false, true, true
	}
	type writeDiagnosticGroup struct {
		byVariant [][]*TypeExpr
	}
	diagnosticGroups := make(map[Position]*writeDiagnosticGroup)
	var diagnosticOrder []Position
	for elementIndex, arg := range model.elements {
		if splat, isSplat := arg.(*SplatArg); isSplat {
			compatible, aborts := c.applySplattedElementWriteFacts(
				function,
				splat,
				ident.Name,
				elem,
				resolve,
			)
			if aborts {
				// Expansion raises before dispatch and before later
				// arguments evaluate, so no write lands and the remaining
				// elements must not be modeled.
				return false, true, false
			}
			if !compatible {
				preserved = false
			}
			continue
		}
		splatOrigin := nextSplatOrigin(arg)
		written, captured := argumentFacts[arg]
		if member.Property == "fill" && blockResult.fact != nil &&
			arg == writesCall.Block {
			written = blockResult.fact
		} else if !captured {
			written = c.inferExpressionType(arg)
		}
		if written == nil {
			linkRetainedElement(arg, written, splatOrigin)
			preserved = false
			continue
		}
		disjoint := typedWriteRejected(written, elem, resolve)
		compatible := !disjoint && typeExprSatisfies(written, elem, resolve)
		fillValueIntact := mutatorReceiverFactIntact(c.inferExpressionType(arg), written)
		if captured, ok := argumentRetainedAliases[arg]; ok {
			fillValueIntact = c.capturedContainerWriteFactIntact(captured, written)
		}
		if member.Property == "fill" && c.typeExprHasContainerArm(written) && !fillValueIntact {
			// The explicit fill value evaluates before its selectors. A later
			// selector can mutate that retained container before dispatch, so
			// only its still-intact fact can preserve the receiver bound.
			preserved = false
		}
		if compatible {
			c.invalidateElementWriteAliases(ident.Name, written)
		}
		// The receiver retains every written element regardless of
		// compatibility, so a container-rooted element's local links in: a
		// later mutation through it weakens both.
		linkRetainedElement(arg, written, splatOrigin)
		if disjoint {
			pos := arg.Pos()
			if splatOrigin != nil {
				pos = splatOrigin.Pos()
			}
			group := diagnosticGroups[pos]
			if group == nil {
				group = &writeDiagnosticGroup{
					byVariant: make([][]*TypeExpr, len(variants)),
				}
				diagnosticGroups[pos] = group
				diagnosticOrder = append(diagnosticOrder, pos)
			}
			variantID := elementVariantIDs[elementIndex]
			group.byVariant[variantID] = append(group.byVariant[variantID], written)
			preserved = false
			continue
		}
		if !compatible {
			preserved = false
		}
	}
	for _, pos := range diagnosticOrder {
		group := diagnosticGroups[pos]
		maxOccurrences := 0
		for _, actuals := range group.byVariant {
			maxOccurrences = max(maxOccurrences, len(actuals))
		}
		for occurrence := range maxOccurrences {
			var actuals []*TypeExpr
			for _, variantActuals := range group.byVariant {
				if occurrence < len(variantActuals) {
					actuals = append(actuals, variantActuals[occurrence])
				}
			}
			c.reportIncompatibleElementWrite(
				function,
				pos,
				ident.Name,
				elem,
				unionTypeExprs(actuals...),
			)
		}
	}
	return preserved, true, mayWrite
}

// applySplattedElementWriteFacts checks a splatted mutator element argument:
// the runtime expands the splatted array's elements into the written
// positions, so a witnessed element arm provably disjoint from the bound is
// reported (witness arms are real elements), and the write is compatible
// only when the splatted array's own element bound — a declared array<V> or
// a full witness union — satisfies the receiver's. A typed but possibly
// empty splat stays silent: no element is proven to land. aborts reports a
// splat value that is provably not an array: expansion raises before
// dispatch and before later arguments evaluate, so no write occurs at all.
func (c *scriptChecker) applySplattedElementWriteFacts(function string, splat *SplatArg, name string, elem *TypeExpr, resolve namedTypeResolver) (compatible, aborts bool) {
	written := c.inferExpressionType(splat.Value)
	if written != nil && typeExprArmsAll(written, func(arm *TypeExpr) bool {
		if _, isShapeValue := shapeValuePayload(arm); isShapeValue {
			return true
		}
		return arm.Kind != TypeArray
	}) {
		return false, true
	}
	if written == nil || written.Kind != TypeArray || written.Nullable {
		// The receiver retains whatever elements the splat expands to, so
		// an unknown splatted local links in conservatively.
		c.linkContainerWriteAlias(name, splat.Value, nil)
		return false, false
	}
	// The receiver retains the splatted array's elements regardless of
	// compatibility, so its root local links in when those elements may be
	// containers: a later mutation through it weakens both.
	bound := splattedElementBound(written)
	compatible = bound != nil && typeExprSatisfies(bound, elem, resolve)
	if compatible {
		c.invalidateElementWriteAliases(name, bound)
	}
	c.linkContainerWriteAlias(name, splat.Value, bound)
	if literalArrayElementsWitnessed(written) ||
		written.Name == blockRestElementsMarker {
		if len(written.TypeArgs) == 1 {
			if arms, ok := typeExprArms(written.TypeArgs[0], 0); ok {
				for _, arm := range arms {
					if typedWriteRejected(arm, elem, resolve) {
						c.reportIncompatibleElementWrite(function, splat.Pos(), name, elem, arm)
						return false, false
					}
				}
			}
		}
	}
	return compatible, false
}

// splattedElementBound returns the element bound of a splatted array fact:
// declared array<V> facts and full witness unions bound every element, while
// partial witnesses and bare arrays do not.
func splattedElementBound(ty *TypeExpr) *TypeExpr {
	if ty == nil || ty.Kind != TypeArray || ty.Nullable || len(ty.TypeArgs) != 1 {
		return nil
	}
	if literalArrayElementsPartial(ty) {
		return nil
	}
	return ty.TypeArgs[0]
}

func (c *scriptChecker) hashMutatorCallMayWrite(
	call *CallExpr,
	property string,
	argumentFacts map[Expression]*TypeExpr,
) bool {
	if call == nil || len(call.KwArgs) != 0 {
		return false
	}
	switch property {
	case "store":
		if len(call.Args) != 2 {
			return false
		}
		keyType := c.mutatorCallArgumentFact(call.Args[0], argumentFacts)
		return keyType == nil || !typeExprProvablyUnstorableKey(keyType)
	case "replace":
		if len(call.Args) != 1 || callHasSplatArg(call) {
			return false
		}
		written := c.mutatorCallArgumentFact(call.Args[0], argumentFacts)
		return written == nil || !typeExprProvablyNotHash(written)
	default:
		return false
	}
}

// applyHashMutatorCallFacts checks the entries an in-place builtin hash
// mutator writes against the receiver's declared hash<K, V> fact. store writes
// one checked entry, merge!/update fold whole hash arguments into the existing
// store, and replace validates the one hash whose complete contents are
// adopted. A merge conflict block's results are unknown, so its presence
// checks nothing and preserves nothing.
func (c *scriptChecker) applyHashMutatorCallFacts(
	function string,
	call, checkedCall *CallExpr,
	member *MemberExpr,
	argumentFacts map[Expression]*TypeExpr,
	name string,
	receiverFact, hashFact, keyBound, valueBound *TypeExpr,
) (preserved, modeled bool) {
	writesCall := call
	if checkedCall != nil {
		writesCall = checkedCall
	}
	if len(writesCall.KwArgs) != 0 {
		return false, false
	}
	resolve := c.checkNamedTypeResolver()
	switch member.Property {
	case "store":
		if len(writesCall.Args) != 2 {
			return false, false
		}
		// store canonicalizes its key before writing, so a provably
		// unsupported key kind raises without storing anything.
		if keyType := c.mutatorCallArgumentFact(writesCall.Args[0], argumentFacts); keyType != nil &&
			typeExprProvablyUnstorableKey(keyType) {
			return false, true
		}
		preserved = c.mutatorCallPreservable(call, name, receiverFact)
		checkEntry := func(arg Expression, bound *TypeExpr, noun string) {
			written := c.mutatorCallArgumentFact(arg, argumentFacts)
			// The receiver retains the stored entry regardless of
			// compatibility, so a written container's root local links in.
			c.linkContainerWriteAlias(name, arg, written)
			if written == nil {
				preserved = false
				return
			}
			rejected := typedWriteRejected(written, bound, resolve)
			satisfies := typeExprSatisfies(written, bound, resolve)
			if noun == "key" {
				rejected = hashKeyTypesDisjoint(written, bound, resolve)
				satisfies = hashKeyTypeSatisfies(written, bound, resolve)
			}
			if rejected {
				c.add(function, arg.Pos(), "write to %s expected %s %s, got %s",
					name, noun, formatTypeExpr(bound), formatTypeExpr(written))
				preserved = false
				return
			}
			if !satisfies {
				preserved = false
				return
			}
			// The receiver retains the stored entry, so a written
			// container's root local links in: a later mutation through it
			// weakens both.
			c.linkContainerWriteAlias(name, arg, written)
		}
		checkEntry(writesCall.Args[0], keyBound, "key")
		checkEntry(writesCall.Args[1], valueBound, "value")
		return preserved, true
	case "replace":
		if len(writesCall.Args) != 1 || callHasSplatArg(writesCall) {
			return false, false
		}
		arg := writesCall.Args[0]
		written := c.mutatorCallArgumentFact(arg, argumentFacts)
		if written != nil && typeExprProvablyNotHash(written) {
			return false, true
		}
		preserved = c.mutatorCallPreservable(call, name, receiverFact)
		if lit, isLiteral := arg.(*HashLiteral); isLiteral && lit.ShapeType == nil {
			compatible := c.checkHashLiteralMergeEntries(
				function,
				name,
				lit,
				keyBound,
				valueBound,
				resolve,
				false,
			)
			return preserved && compatible, true
		}
		// replace copies the source's entries without retaining its root.
		// Nested containers remain shared, so keep the same conservative
		// interior link used for whole-hash merge arguments.
		if !hashBoundsContainerFree(keyBound, valueBound) {
			c.linkContainerWriteAlias(name, arg, written)
		}
		if written == nil {
			return false, true
		}
		if typeExprHashLikeOnly(written) && typedWriteRejected(written, hashFact, resolve) {
			c.add(function, arg.Pos(), "write to %s expected %s, got %s",
				name, formatTypeExpr(hashFact), formatTypeExpr(written))
			return false, true
		}
		if typeExprHasOpenShapeArm(written) ||
			!typeExprSatisfies(written, hashFact, resolve) {
			return false, true
		}
		return preserved, true
	}
	return false, false
}

// mutatorReceiverFactIntact reports whether a mutator receiver's local fact
// survived a later operand or argument walk unchanged, so preserving it is
// still about the same fact the writes were checked against.
//
// A walk that left the fact alone leaves the same node in place, so identity
// settles the case that matters without comparing a wide fact node for node to
// discover it. Serializing both sides to compare them was what made that
// expensive -- doing it on every field write of a growing shape allocated 185MB
// across 2,000 writes (#14) -- and typeFactsIdentical keeps the identity
// shortcut at every level rather than only at the root, allocates nothing, and
// stops at the first difference, so it is cheaper than the key it replaces as
// well as exact.
func mutatorReceiverFactIntact(current, captured *TypeExpr) bool {
	if current == captured {
		return current != nil
	}
	return current != nil && typeFactsIdentical(current, captured)
}

func (c *scriptChecker) invalidateElementWriteAliases(name string, written *TypeExpr) {
	if written == nil {
		return
	}
	identities := map[string]struct{}{name: {}}
	stack := []string{name}
	for len(stack) > 0 {
		last := len(stack) - 1
		current := stack[last]
		stack = stack[:last]
		for alias, edge := range c.containerIdentityAliases[current] {
			if !c.bindingEdgeCurrent(current, alias, edge) {
				continue
			}
			if _, visited := identities[alias]; visited {
				continue
			}
			identities[alias] = struct{}{}
			stack = append(stack, alias)
		}
	}
	// Retained-container dependencies are directional: mutating a child can
	// invalidate a parent that contains it, while appending to the parent does
	// not change a previously retained child. Direct aliases use reciprocal
	// dependencies and therefore still flow both ways.
	seen := map[string]struct{}{name: {}}
	stack = []string{name}
	resolve := c.checkNamedTypeResolver()
	for len(stack) > 0 {
		last := len(stack) - 1
		current := stack[last]
		stack = stack[:last]
		for alias, edge := range c.staticValueDependents[current] {
			if !c.bindingEdgeCurrent(current, alias, edge) {
				continue
			}
			if _, visited := seen[alias]; visited {
				continue
			}
			seen[alias] = struct{}{}
			stack = append(stack, alias)
			if _, identical := identities[alias]; identical {
				elem := declaredArrayElementType(c.localTypeFor(alias))
				if elem != nil && typeExprSatisfies(written, elem, resolve) {
					continue
				}
			}
			c.poisonLocalTypeOnly(alias)
			c.poisonLocalStaticValues(alias)
		}
	}
}

// poisonElementWriteFacts invalidates the mutated receiver and values that
// depend on it without poisoning containers merely retained inside it. The
// dependency graph is directional: child mutations flow to their parents,
// while parent appends and replacements leave existing children unchanged.
func (c *scriptChecker) poisonElementWriteFacts(name string) {
	if name == "" {
		return
	}
	c.poisonLocalTypeOnly(name)
	c.poisonLocalStaticValues(name)
	seen := map[string]struct{}{name: {}}
	stack := []string{name}
	for len(stack) > 0 {
		last := len(stack) - 1
		current := stack[last]
		stack = stack[:last]
		for dependent, edge := range c.staticValueDependents[current] {
			if !c.bindingEdgeCurrent(current, dependent, edge) {
				continue
			}
			if _, visited := seen[dependent]; visited {
				continue
			}
			seen[dependent] = struct{}{}
			stack = append(stack, dependent)
			c.poisonLocalTypeOnly(dependent)
			c.poisonLocalStaticValues(dependent)
		}
	}
}

// nonNilMutatorReceiverFact strips the nil possibility from a mutator
// receiver's fact: dispatch only proceeds on the non-nil path (safe
// navigation skips nil and a plain member call on nil raises), so the
// non-nil arm bounds the writes that actually land. Preservation still
// compares the original fact — a nullable receiver stays nullable.
func nonNilMutatorReceiverFact(ty *TypeExpr) *TypeExpr {
	if ty == nil {
		return nil
	}
	if ty.Nullable {
		clone := *ty
		clone.Nullable = false
		return &clone
	}
	if ty.Kind != TypeUnion {
		return ty
	}
	arms, ok := typeExprArms(ty, 0)
	if !ok {
		return ty
	}
	kept := make([]*TypeExpr, 0, len(arms))
	for _, arm := range arms {
		if arm.Kind != TypeNil {
			kept = append(kept, arm)
		}
	}
	if len(kept) == len(arms) {
		return ty
	}
	return unionTypeExprs(kept...)
}

// expressionMayRunBlockLiteralAssigning reports whether evaluating expr can
// run an inline block or lambda body that assigns name through an enclosing
// local. A literal that is merely constructed does not execute during the
// evaluation; lambdas defined earlier already degraded the affected locals
// when their literals walked, so only inline literals need detecting here.
func expressionMayRunBlockLiteralAssigning(expr Expression, name string) bool {
	return blockLiteralMayRun(expr, false, name)
}

func blockLiteralMayRun(e Expression, invocable bool, assignedName string) bool {
	switch typed := e.(type) {
	case nil:
		return false
	case *BlockLiteral:
		if !invocable || assignedName == "" {
			return invocable
		}
		return blockMayAssignOuterName(typed, assignedName)
	case *ArrayLiteral:
		for _, element := range typed.Elements {
			if blockLiteralMayRun(element, invocable, assignedName) {
				return true
			}
		}
	case *HashLiteral:
		for _, pair := range typed.Pairs {
			if blockLiteralMayRun(pair.Key, invocable, assignedName) ||
				blockLiteralMayRun(pair.Value, invocable, assignedName) {
				return true
			}
		}
	case *CallExpr:
		if typed.Block != nil && blockLiteralMayRun(typed.Block, true, assignedName) {
			return true
		}
		// The callee resolution and the call itself may invoke any literal
		// reachable from the callee or the arguments.
		if blockLiteralMayRun(typed.Callee, true, assignedName) {
			return true
		}
		for _, arg := range typed.Args {
			if blockLiteralMayRun(arg, true, assignedName) {
				return true
			}
		}
		for _, kwarg := range typed.KwArgs {
			if blockLiteralMayRun(kwarg.Value, true, assignedName) {
				return true
			}
		}
	case *MemberExpr:
		return blockLiteralMayRun(typed.Object, true, assignedName)
	case *ScopeExpr:
		return blockLiteralMayRun(typed.Object, invocable, assignedName)
	case *IndexExpr:
		if blockLiteralMayRun(typed.Object, invocable, assignedName) {
			return true
		}
		for _, index := range typed.Indices {
			if blockLiteralMayRun(index, invocable, assignedName) {
				return true
			}
		}
	case *SplatArg:
		return blockLiteralMayRun(typed.Value, invocable, assignedName)
	case *UnaryExpr:
		return blockLiteralMayRun(typed.Right, invocable, assignedName)
	case *BinaryExpr:
		return blockLiteralMayRun(typed.Left, invocable, assignedName) ||
			blockLiteralMayRun(typed.Right, invocable, assignedName)
	case *ConditionalExpr:
		return blockLiteralMayRun(typed.Condition, invocable, assignedName) ||
			blockLiteralMayRun(typed.Consequent, invocable, assignedName) ||
			blockLiteralMayRun(typed.Alternate, invocable, assignedName)
	case *RescueExpr:
		return blockLiteralMayRun(typed.Body, invocable, assignedName) ||
			blockLiteralMayRun(typed.Fallback, invocable, assignedName)
	case *IfExpr:
		if blockLiteralMayRun(typed.Condition, invocable, assignedName) ||
			blockLiteralMayRun(typed.Consequent, invocable, assignedName) ||
			blockLiteralMayRun(typed.Alternate, invocable, assignedName) {
			return true
		}
		for _, branch := range typed.ElseIf {
			if blockLiteralMayRun(branch.Condition, invocable, assignedName) ||
				blockLiteralMayRun(branch.Result, invocable, assignedName) {
				return true
			}
		}
	case *RangeExpr:
		return blockLiteralMayRun(typed.Start, invocable, assignedName) ||
			blockLiteralMayRun(typed.End, invocable, assignedName)
	case *CaseExpr:
		if blockLiteralMayRun(typed.Target, invocable, assignedName) {
			return true
		}
		for _, clause := range typed.Clauses {
			for _, value := range clause.Values {
				if blockLiteralMayRun(value.Expr, invocable, assignedName) {
					return true
				}
			}
			if blockLiteralMayRun(clause.Result, invocable, assignedName) {
				return true
			}
		}
		return blockLiteralMayRun(typed.ElseExpr, invocable, assignedName)
	case *YieldExpr:
		// The caller-supplied block may invoke any literal yielded to it.
		for _, arg := range typed.Args {
			if blockLiteralMayRun(arg, true, assignedName) {
				return true
			}
		}
	case *InterpolatedString:
		for _, part := range typed.Parts {
			if exprPart, ok := part.(StringExpr); ok &&
				blockLiteralMayRun(exprPart.Expr, invocable, assignedName) {
				return true
			}
		}
	case *InterpolatedSymbol:
		for _, part := range typed.Parts {
			if exprPart, ok := part.(StringExpr); ok &&
				blockLiteralMayRun(exprPart.Expr, invocable, assignedName) {
				return true
			}
		}
	}
	return false
}

func blockMayAssignOuterName(block *BlockLiteral, name string) bool {
	if block == nil || name == "" || blockBindsName(block, name) {
		return false
	}
	return statementsMayAssignName(block.Body, name)
}

func blockBindsName(block *BlockLiteral, name string) bool {
	bindings := make(map[string]struct{})
	for _, param := range block.Params {
		if param.Name != "" {
			bindings[param.Name] = struct{}{}
		}
		collectBindingTarget(param.Target, bindings)
	}
	for _, implicit := range block.ImplicitParams {
		bindings[implicit] = struct{}{}
	}
	_, bound := bindings[name]
	return bound
}

func statementsMayAssignName(statements []Statement, name string) bool {
	for _, stmt := range statements {
		switch typed := stmt.(type) {
		case *ReturnStmt:
			if expressionMayRunBlockLiteralAssigning(typed.Value, name) {
				return true
			}
		case *RaiseStmt:
			if expressionMayRunBlockLiteralAssigning(typed.Value, name) ||
				expressionMayRunBlockLiteralAssigning(typed.Message, name) {
				return true
			}
		case *BreakStmt:
			if expressionMayRunBlockLiteralAssigning(typed.Value, name) {
				return true
			}
		case *NextStmt:
			if expressionMayRunBlockLiteralAssigning(typed.Value, name) {
				return true
			}
		case *AssignStmt:
			bindings := make(map[string]struct{})
			collectBindingTarget(typed.Target, bindings)
			if _, assigns := bindings[name]; assigns {
				return true
			}
			if expressionMayRunBlockLiteralAssigning(typed.Target, name) ||
				expressionMayRunBlockLiteralAssigning(typed.Value, name) {
				return true
			}
		case *ExprStmt:
			if expressionMayRunBlockLiteralAssigning(typed.Expr, name) {
				return true
			}
		case *IfStmt:
			if expressionMayRunBlockLiteralAssigning(typed.Condition, name) ||
				statementsMayAssignName(typed.Consequent, name) ||
				statementsMayAssignName(typed.Alternate, name) {
				return true
			}
			for _, branch := range typed.ElseIf {
				if expressionMayRunBlockLiteralAssigning(branch.Condition, name) ||
					statementsMayAssignName(branch.Consequent, name) {
					return true
				}
			}
		case *ForStmt:
			bindings := make(map[string]struct{})
			collectBindingTarget(typed.Target, bindings)
			if _, assigns := bindings[name]; assigns {
				return true
			}
			if expressionMayRunBlockLiteralAssigning(typed.Iterable, name) ||
				statementsMayAssignName(typed.Body, name) {
				return true
			}
		case *WhileStmt:
			if expressionMayRunBlockLiteralAssigning(typed.Condition, name) ||
				statementsMayAssignName(typed.Body, name) {
				return true
			}
		case *UntilStmt:
			if expressionMayRunBlockLiteralAssigning(typed.Condition, name) ||
				statementsMayAssignName(typed.Body, name) {
				return true
			}
		case *TryStmt:
			if statementsMayAssignName(typed.Body, name) ||
				statementsMayAssignName(typed.Else, name) ||
				statementsMayAssignName(typed.Ensure, name) {
				return true
			}
			for _, clause := range typed.Rescues {
				if clause.Binding != name && statementsMayAssignName(clause.Body, name) {
					return true
				}
			}
		}
	}
	return false
}

// linkContainerAssignmentAlias records retained roots for a container bound
// to a local. Unknown projections and shovel results keep their previous
// conservative root links, while other unknown expressions remain unlinked.
// Calls keep their existing declared-return inference because their retained
// roots are not statically visible here.
func (c *scriptChecker) linkContainerAssignmentAlias(target string, value Expression, assigned *TypeExpr) {
	if assigned == nil {
		switch typed := value.(type) {
		case *IndexExpr, *MemberExpr:
		case *BinaryExpr:
			if typed.Operator != tokenShovel {
				return
			}
		default:
			return
		}
	} else if !c.typeExprHasContainerArm(assigned) {
		return
	}
	c.linkRetainedContainerAliases(target, value, assigned, false, true)
}

// linkContainerWriteAlias records retained roots for a container write. A
// container-valued call can return an untracked alias, so it conservatively
// weakens the enclosing receiver.
func (c *scriptChecker) linkContainerWriteAlias(receiver string, value Expression, written *TypeExpr) {
	c.linkRetainedContainerAliases(receiver, value, written, true, false)
}

type checkRetainedContainerCapture struct {
	roots           []capturedContainerRoot
	identityRoots   []capturedContainerRoot
	poisonUntracked bool
}

// captureRetainedContainerAliases snapshots the mutable roots exposed by an
// argument when it finishes evaluating. A later argument may rebind the same
// local before dispatch, but the runtime retains the value already produced.
func (c *scriptChecker) captureRetainedContainerAliases(
	value Expression,
	written *TypeExpr,
) checkRetainedContainerCapture {
	var capture checkRetainedContainerCapture
	seen := make(map[capturedContainerRoot]struct{})
	seenIdentity := make(map[capturedContainerRoot]struct{})
	var collect func(Expression, *TypeExpr)
	collect = func(value Expression, written *TypeExpr) {
		if written != nil && !c.typeExprHasContainerArm(written) {
			return
		}
		switch typed := value.(type) {
		case *Identifier, *IndexExpr, *MemberExpr:
			root, ok := c.retainedContainerRoot(value)
			if !ok {
				return
			}
			for name := range c.containerAliasNames(root) {
				candidate := capturedContainerRoot{
					name:       name,
					generation: c.localBindingGenerations[name],
				}
				if _, duplicate := seen[candidate]; duplicate {
					continue
				}
				seen[candidate] = struct{}{}
				capture.roots = append(capture.roots, candidate)
			}
			if _, direct := value.(*Identifier); direct {
				for name := range c.containerIdentityNames(root) {
					candidate := capturedContainerRoot{
						name:       name,
						generation: c.localBindingGenerations[name],
					}
					if _, duplicate := seenIdentity[candidate]; duplicate {
						continue
					}
					seenIdentity[candidate] = struct{}{}
					capture.identityRoots = append(capture.identityRoots, candidate)
				}
			}
		case *ArrayLiteral:
			for _, element := range typed.Elements {
				collect(element, c.inferExpressionType(element))
			}
		case *HashLiteral:
			if typed.ShapeType != nil && !c.hashShapeStaticallyShadowed(typed) {
				return
			}
			for _, pair := range typed.Pairs {
				collect(pair.Key, c.inferExpressionType(pair.Key))
				collect(pair.Value, c.inferExpressionType(pair.Value))
			}
		case *ConditionalExpr:
			if branch, known := staticConditionalExpressionBranch(typed); known {
				collect(branch, c.inferExpressionType(branch))
				return
			}
			collect(typed.Consequent, c.inferExpressionType(typed.Consequent))
			collect(typed.Alternate, c.inferExpressionType(typed.Alternate))
		case *IfExpr:
			if branch, known := staticIfExpressionBranch(typed); known {
				collect(branch, c.inferExpressionType(branch))
				return
			}
			collect(typed.Consequent, c.inferExpressionType(typed.Consequent))
			for _, branch := range typed.ElseIf {
				collect(branch.Result, c.inferExpressionType(branch.Result))
			}
			collect(typed.Alternate, c.inferExpressionType(typed.Alternate))
		case *RescueExpr:
			collect(typed.Body, c.inferExpressionType(typed.Body))
			collect(typed.Fallback, c.inferExpressionType(typed.Fallback))
		case *CaseExpr:
			if result, known := staticCaseExpressionResult(typed); known {
				collect(result, c.inferExpressionType(result))
				return
			}
			for _, clause := range typed.Clauses {
				collect(clause.Result, c.inferExpressionType(clause.Result))
			}
			collect(typed.ElseExpr, c.inferExpressionType(typed.ElseExpr))
		case *BinaryExpr:
			switch typed.Operator {
			case tokenAnd, tokenOr:
				collect(typed.Left, c.inferExpressionType(typed.Left))
				collect(typed.Right, c.inferExpressionType(typed.Right))
			case tokenShovel, tokenPlus:
				collect(typed.Left, c.inferExpressionType(typed.Left))
				collect(typed.Right, c.inferExpressionType(typed.Right))
			case tokenMinus, tokenAmpersand:
				collect(typed.Left, c.inferExpressionType(typed.Left))
			}
		case *CallExpr:
			capture.poisonUntracked = true
		}
	}
	collect(value, written)
	slices.SortFunc(capture.roots, func(a, b capturedContainerRoot) int {
		return cmp.Or(cmp.Compare(a.name, b.name), cmp.Compare(a.generation, b.generation))
	})
	slices.SortFunc(capture.identityRoots, func(a, b capturedContainerRoot) int {
		return cmp.Or(cmp.Compare(a.name, b.name), cmp.Compare(a.generation, b.generation))
	})
	return capture
}

func (c *scriptChecker) linkCapturedContainerWriteAliases(
	receiver string,
	capture checkRetainedContainerCapture,
) {
	if capture.poisonUntracked {
		c.poisonLocalType(receiver)
	}
	for _, root := range capture.roots {
		if c.localBindingGenerations[root.name] != root.generation {
			continue
		}
		c.linkContainerAlias(receiver, root.name)
		c.linkStaticValueDependency(root.name, receiver)
	}
}

func (c *scriptChecker) capturedContainerWriteFactIntact(
	capture checkRetainedContainerCapture,
	written *TypeExpr,
) bool {
	if capture.poisonUntracked {
		return false
	}
	if len(capture.identityRoots) == 0 {
		return true
	}
	current := false
	for _, root := range capture.identityRoots {
		if c.localBindingGenerations[root.name] != root.generation {
			continue
		}
		current = true
		if mutatorReceiverFactIntact(c.localTypeFor(root.name), written) {
			return true
		}
	}
	if current {
		return false
	}
	for _, root := range capture.identityRoots {
		if _, poisoned := c.typePoison[root.name]; poisoned {
			return false
		}
	}
	return true
}

// linkRetainedContainerAliases links a container to the retained roots
// exposed by its value expression. Container literals expose their elements
// and entries recursively, while value-producing branches expose whichever
// result is selected.
func (c *scriptChecker) linkRetainedContainerAliases(receiver string, value Expression, written *TypeExpr, poisonUntracked, directAlias bool) {
	if written != nil && !c.typeExprHasContainerArm(written) {
		return
	}
	switch typed := value.(type) {
	case *Identifier:
		if root, ok := c.retainedContainerRoot(value); ok {
			if directAlias {
				c.linkContainerIdentityAlias(receiver, root)
				c.linkStaticValueAlias(receiver, root)
			} else {
				c.linkContainerAlias(receiver, root)
				c.linkStaticValueDependency(root, receiver)
			}
		}
	case *IndexExpr, *MemberExpr:
		if root, ok := c.retainedContainerRoot(value); ok {
			c.linkContainerAlias(receiver, root)
			if directAlias {
				c.linkStaticValueDependency(receiver, root)
			} else {
				c.linkStaticValueDependency(root, receiver)
			}
		}
	case *ArrayLiteral:
		for _, element := range typed.Elements {
			c.linkRetainedContainerAliases(receiver, element, c.inferExpressionType(element), poisonUntracked, false)
		}
	case *HashLiteral:
		if typed.ShapeType != nil && !c.hashShapeStaticallyShadowed(typed) {
			return
		}
		for _, pair := range typed.Pairs {
			c.linkRetainedContainerAliases(receiver, pair.Key, c.inferExpressionType(pair.Key), poisonUntracked, false)
			c.linkRetainedContainerAliases(receiver, pair.Value, c.inferExpressionType(pair.Value), poisonUntracked, false)
		}
	case *ConditionalExpr:
		if branch, known := staticConditionalExpressionBranch(typed); known {
			c.linkRetainedContainerAliases(receiver, branch, c.inferExpressionType(branch), poisonUntracked, directAlias)
			return
		}
		c.linkPossibleDirectContainerAlias(receiver, typed.Consequent, poisonUntracked, directAlias)
		c.linkPossibleDirectContainerAlias(receiver, typed.Alternate, poisonUntracked, directAlias)
	case *IfExpr:
		if branch, known := staticIfExpressionBranch(typed); known {
			c.linkRetainedContainerAliases(receiver, branch, c.inferExpressionType(branch), poisonUntracked, directAlias)
			return
		}
		c.linkPossibleDirectContainerAlias(receiver, typed.Consequent, poisonUntracked, directAlias)
		for _, branch := range typed.ElseIf {
			c.linkPossibleDirectContainerAlias(receiver, branch.Result, poisonUntracked, directAlias)
		}
		c.linkPossibleDirectContainerAlias(receiver, typed.Alternate, poisonUntracked, directAlias)
	case *RescueExpr:
		c.linkPossibleDirectContainerAlias(receiver, typed.Body, poisonUntracked, directAlias)
		c.linkPossibleDirectContainerAlias(receiver, typed.Fallback, poisonUntracked, directAlias)
	case *CaseExpr:
		if result, known := staticCaseExpressionResult(typed); known {
			c.linkRetainedContainerAliases(receiver, result, c.inferExpressionType(result), poisonUntracked, directAlias)
			return
		}
		for _, clause := range typed.Clauses {
			c.linkPossibleDirectContainerAlias(receiver, clause.Result, poisonUntracked, directAlias)
		}
		c.linkPossibleDirectContainerAlias(receiver, typed.ElseExpr, poisonUntracked, directAlias)
	case *BinaryExpr:
		switch typed.Operator {
		case tokenAnd, tokenOr:
			c.linkPossibleDirectContainerAlias(receiver, typed.Left, poisonUntracked, directAlias)
			c.linkPossibleDirectContainerAlias(receiver, typed.Right, poisonUntracked, directAlias)
		case tokenShovel:
			c.linkRetainedContainerAliases(receiver, typed.Left, c.inferExpressionType(typed.Left), poisonUntracked, directAlias)
			c.linkRetainedContainerAliases(receiver, typed.Right, c.inferExpressionType(typed.Right), poisonUntracked, false)
		case tokenPlus:
			c.linkRetainedContainerAliases(receiver, typed.Left, c.inferExpressionType(typed.Left), poisonUntracked, false)
			c.linkRetainedContainerAliases(receiver, typed.Right, c.inferExpressionType(typed.Right), poisonUntracked, false)
		case tokenMinus, tokenAmpersand:
			c.linkRetainedContainerAliases(receiver, typed.Left, c.inferExpressionType(typed.Left), poisonUntracked, false)
		}
	case *CallExpr:
		if poisonUntracked {
			c.poisonLocalType(receiver)
		}
	}
}

func (c *scriptChecker) linkPossibleDirectContainerAlias(
	receiver string,
	value Expression,
	poisonUntracked, directAlias bool,
) {
	c.linkRetainedContainerAliases(receiver, value, c.inferExpressionType(value), poisonUntracked, false)
	if !directAlias {
		return
	}
	if _, direct := value.(*Identifier); !direct {
		return
	}
	if root, ok := c.retainedContainerRoot(value); ok {
		// Exactly one branch supplies the result, so this is a may-alias rather
		// than a must-identity edge. Reciprocal invalidation is still required:
		// a mutation through either name may affect the other at runtime.
		c.linkStaticValueAlias(receiver, root)
	}
}

// effectiveHashLiteralPairs returns the entries that survive construction
// when the runtime identity of a literal key is statically known. Runtime
// HashSet keeps an equal key's first insertion-order slot but replaces the
// stored entry with the last pair; unknown keys stay distinct so the checker
// does not discard a write it cannot prove is overwritten.
func effectiveHashLiteralPairs(lit *HashLiteral) []HashPair {
	if len(lit.Pairs) < 2 {
		return lit.Pairs
	}
	lastPairIndex := make(map[string]int, len(lit.Pairs))
	hasDuplicate := false
	for i, pair := range lit.Pairs {
		key, ok := staticLiteralHashIdentity(pair.Key)
		if !ok {
			continue
		}
		if _, present := lastPairIndex[key]; present {
			hasDuplicate = true
		}
		lastPairIndex[key] = i
	}
	if !hasDuplicate {
		return lit.Pairs
	}
	effectivePairs := make([]HashPair, 0, len(lit.Pairs)-1)
	for _, pair := range lit.Pairs {
		key, ok := staticLiteralHashIdentity(pair.Key)
		if !ok {
			effectivePairs = append(effectivePairs, pair)
			continue
		}
		lastIndex := lastPairIndex[key]
		if lastIndex < 0 {
			continue
		}
		effectivePairs = append(effectivePairs, lit.Pairs[lastIndex])
		lastPairIndex[key] = -1
	}
	return effectivePairs
}

// staticLiteralHashIdentity is the entry a static literal key addresses.
// Strings and symbols with the same spelling address one entry, so they share
// one identity here too.
func staticLiteralHashIdentity(expr Expression) (string, bool) {
	value, ok := staticLiteralValue(expr)
	if !ok {
		return "", false
	}
	key, err := hashKeyString(value)
	return key, err == nil
}

// checkHashLiteralMergeEntries checks a literal merge!/update argument entry
// by entry against the receiver's declared bounds and reports whether every
// entry provably satisfies both. With a conflict block, a value only lands
// unmediated when its key provably cannot already exist (an impossible key
// never conflicts), so value diagnostics gate on that.
func (c *scriptChecker) checkHashLiteralMergeEntries(function, name string, lit *HashLiteral, keyBound, valueBound *TypeExpr, resolve namedTypeResolver, blockConflicts bool) bool {
	compatible := true
	check := func(expr Expression, bound *TypeExpr, noun string, warn bool) (disjoint bool) {
		if expr == nil {
			compatible = false
			return false
		}
		written := c.inferExpressionType(expr)
		// The receiver retains the entry regardless of compatibility, so a
		// written container's root local links in: a later mutation or
		// escape through either side weakens both.
		c.linkContainerWriteAlias(name, expr, written)
		if written == nil {
			compatible = false
			return false
		}
		rejected := typedWriteRejected(written, bound, resolve)
		satisfies := typeExprSatisfies(written, bound, resolve)
		if noun == "key" {
			rejected = hashKeyTypesDisjoint(written, bound, resolve)
			satisfies = hashKeyTypeSatisfies(written, bound, resolve)
		}
		if rejected {
			if warn {
				c.add(function, expr.Pos(), "write to %s expected %s %s, got %s",
					name, noun, formatTypeExpr(bound), formatTypeExpr(written))
			}
			compatible = false
			return true
		}
		if !satisfies {
			compatible = false
		}
		return false
	}
	for _, pair := range effectiveHashLiteralPairs(lit) {
		keyDisjoint := check(pair.Key, keyBound, "key", true)
		check(pair.Value, valueBound, "value", !blockConflicts || keyDisjoint)
	}
	return compatible
}

// typedWriteRejected preserves the checker's existing overlap rule for
// ordinary writes while keeping correlation-only array alternatives from
// hiding an incompatible arm in a value that a typed store will retain.
func typedWriteRejected(written, required *TypeExpr, resolve namedTypeResolver) bool {
	if hasAlternativeArrayFact(written, 0) {
		return boundaryTypeRejected(written, required, resolve)
	}
	return typeExprsDisjoint(written, required, resolve)
}

func hasAlternativeArrayFact(ty *TypeExpr, depth int) bool {
	if ty == nil || depth > maxNormalizeDepth {
		return false
	}
	if ty.Kind == TypeArray &&
		(ty.Name == literalAlternativeElementsMarker ||
			ty.Name == literalPartialAlternativeElementsMarker) {
		return true
	}
	for _, arm := range ty.Union {
		if hasAlternativeArrayFact(arm, depth+1) {
			return true
		}
	}
	for _, arg := range ty.TypeArgs {
		if hasAlternativeArrayFact(arg, depth+1) {
			return true
		}
	}
	for _, field := range ty.Shape {
		if hasAlternativeArrayFact(field, depth+1) {
			return true
		}
	}
	return false
}

// literalArrayDisjoint reports whether a witnessed-element array can never
// satisfy another array type: some witnessed element arm is disjoint from
// the other side's declared element type.
func literalArrayDisjoint(lit, other *TypeExpr, resolve namedTypeResolver) bool {
	if !literalArrayElementsWitnessed(lit) &&
		lit.Name != blockRestElementsMarker {
		return false
	}
	if len(lit.TypeArgs) != 1 || len(other.TypeArgs) != 1 {
		return false
	}
	arms, ok := typeExprArms(lit.TypeArgs[0], 0)
	if !ok {
		return false
	}
	alternative := lit.Name == literalAlternativeElementsMarker ||
		lit.Name == literalPartialAlternativeElementsMarker
	for _, arm := range arms {
		if typeExprsDisjoint(arm, other.TypeArgs[0], resolve) {
			if !alternative {
				return true
			}
			continue
		}
		if alternative {
			return false
		}
	}
	return alternative
}

// shapeVsTypedHashDisjoint reports whether an exact shape can never satisfy
// a generic hash type: shapes witness every required field, so a required
// field type disjoint from the hash's value type contradicts it. Key types
// are left to runtime (key representation is not always known statically).
func shapeVsTypedHashDisjoint(shape, hash *TypeExpr, resolve namedTypeResolver) bool {
	if len(hash.TypeArgs) != 2 {
		return false
	}
	// A known key representation contradicts a disjoint hash key type. Strings
	// and symbols share one keyspace, so a string-keyed store satisfies
	// hash<symbol, ...> and vice versa; an "other" witness still contradicts
	// a bound that admits only those two.
	if len(shape.Shape) > 0 {
		var keyType *TypeExpr
		switch {
		case shape.Name == shapeKeysStringMarker:
			keyType = checkTypeString
		case shape.Name == shapeKeysSymbolMarker:
			keyType = checkTypeSymbol
		case strings.HasPrefix(shape.Name, shapeKeysMixedPrefix):
			flags := strings.TrimPrefix(shape.Name, shapeKeysMixedPrefix)
			if strings.Contains(flags, "s") && hashKeyTypesDisjoint(checkTypeSymbol, hash.TypeArgs[0], resolve) {
				return true
			}
			if strings.Contains(flags, "t") && hashKeyTypesDisjoint(checkTypeString, hash.TypeArgs[0], resolve) {
				return true
			}
			if strings.Contains(flags, "o") && typeExprArmsAll(hash.TypeArgs[0], func(arm *TypeExpr) bool {
				return arm.Kind == TypeString || arm.Kind == TypeSymbol
			}) {
				return true
			}
		}
		if keyType != nil && hashKeyTypesDisjoint(keyType, hash.TypeArgs[0], resolve) {
			return true
		}
	}
	valueType := hash.TypeArgs[1]
	for _, field := range shape.Shape {
		// An optional field may be absent, so only a required field's type
		// is witnessed in every value of the shape.
		if shapeFieldOptional(field) {
			continue
		}
		if typeExprsDisjoint(field, valueType, resolve) {
			return true
		}
	}
	return false
}

// shapeFieldValueType strips the field-level optional marker for use as a
// value fact: optionality describes the field's presence in the store, not
// the value read from a present field.
func shapeFieldValueType(fieldType *TypeExpr) *TypeExpr {
	if fieldType == nil || !fieldType.Optional {
		return fieldType
	}
	clone := *fieldType
	clone.Optional = false
	return &clone
}

// stringKeyedShapeFact clones a shape and marks it (and every nested shape)
// as a string-keyed store, so literal string indexing yields exact field
// facts and symbol indexing is known to miss.
func stringKeyedShapeFact(shape *TypeExpr) *TypeExpr {
	clone := cloneTypeExpr(shape)
	markShapeNodesStringKeyed(clone)
	return clone
}

func markShapeNodesStringKeyed(ty *TypeExpr) {
	if ty == nil {
		return
	}
	if ty.Kind == TypeShape {
		ty.Name = shapeKeysStringMarker
		for _, field := range ty.Shape {
			markShapeNodesStringKeyed(field)
		}
	}
	for _, option := range ty.Union {
		markShapeNodesStringKeyed(option)
	}
	for _, arg := range ty.TypeArgs {
		markShapeNodesStringKeyed(arg)
	}
}

// walkShapeTypeNames visits the identifier spelling of every named leaf in a
// shape type: built-in atoms (string, array<...>), including nested shape
// fields, union arms, and generic arguments. nil atoms and structural nodes
// carry no identifier.
func walkShapeTypeNames(ty *TypeExpr, visit func(string)) {
	if ty == nil {
		return
	}
	switch ty.Kind {
	case TypeShape:
		for _, field := range ty.Shape {
			walkShapeTypeNames(field, visit)
		}
		return
	case TypeUnion:
		for _, option := range ty.Union {
			walkShapeTypeNames(option, visit)
		}
		return
	case TypeNil:
		return
	}
	if name := strings.TrimSuffix(ty.Name, "?"); name != "" {
		visit(name)
	}
	for _, arg := range ty.TypeArgs {
		walkShapeTypeNames(arg, visit)
	}
}

// hashShapeStaticallyShadowed mirrors the runtime's shape-versus-hash choice
// for a dual-reading braced group: when any of its type names resolves to a
// known binding (a local — including one the function predeclares by
// assigning it anywhere, matching runtime predeclaration — a host global,
// script definition, or host builtin), the group keeps hash semantics. A
// group without a hash reading is always a shape.
func (c *scriptChecker) hashShapeStaticallyShadowed(lit *HashLiteral) bool {
	if len(lit.Pairs) == 0 {
		return false
	}
	return c.shapeTypeNamesStaticallyShadowed(lit.ShapeType)
}

// typeLiteralStaticallyShadowed mirrors the runtime's type-versus-value
// choice for an argument type literal: a literal without a value reading is
// always a type, and one with a value reading — always a bare identifier —
// keeps it only when that identifier's verbatim spelling resolves (`string?`
// is shadowed by a binding named `string?`, not by one named `string`).
func (c *scriptChecker) typeLiteralStaticallyShadowed(lit *TypeLiteral) bool {
	ident, ok := lit.Fallback.(*Identifier)
	if !ok {
		return false
	}
	return c.staticNameShadowed(ident.Name)
}

func (c *scriptChecker) shapeTypeNamesStaticallyShadowed(ty *TypeExpr) bool {
	shadowed := false
	walkShapeTypeNames(ty, func(name string) {
		if !shadowed && c.staticNameShadowed(name) {
			shadowed = true
		}
	})
	return shadowed
}

func (c *scriptChecker) staticNameShadowed(name string) bool {
	return c.identifierShadowed(name) || c.liveLocalNameHas(name) ||
		c.hostGlobalShadows(name) || c.typeRootResolvesName(name) ||
		c.hostBuiltinOverrides(name) || c.implicitSelfShadows(name)
}

// implicitSelfShadows reports whether a bare identifier would resolve
// through implicit self in the current context, mirroring the runtime probe:
// instance methods dispatch through the class's instance methods, class
// methods and class bodies through its class methods, so only the matching
// member kind shadows. An unknown receiver context stays conservative.
func (c *scriptChecker) implicitSelfShadows(name string) bool {
	if !c.selfScope {
		return false
	}
	if c.selfClass == nil {
		return true
	}
	if c.selfClassContext {
		_, ok := c.selfClass.ClassMethods[name]
		return ok
	}
	_, ok := c.selfClass.Methods[name]
	return ok
}

// typeRootResolvesName mirrors the runtime's env.Get chain walk: engine
// builtins (a lowercase money, for example) resolve in every environment,
// and an exported module function invoked by its caller executes under the
// caller's root, so caller-context parent roots shadow too.
func (c *scriptChecker) typeRootResolvesName(name string) bool {
	for _, root := range []*Env{c.runtimeTypeRoot, c.typeRoot} {
		if root == nil {
			continue
		}
		if _, ok := root.Get(name); ok {
			return true
		}
	}
	return false
}

// inferIndexExprType propagates field-level facts out of shape-typed values:
// indexing with a known key yields the field's type, and a key outside an
// exact shape is known to read nil. Typed arrays and hashes yield their
// element type joined with nil (missing index).
func (c *scriptChecker) inferIndexExprType(expr *IndexExpr) *TypeExpr {
	if len(expr.Indices) != 1 {
		return nil
	}
	if projected, exact := c.staticLiteralProjections(expr); exact {
		var result *TypeExpr
		for _, candidate := range projected {
			candidateType := c.inferExpressionType(candidate)
			if candidateType == nil {
				result = nil
				break
			}
			if result == nil {
				result = candidateType
			} else {
				result = unionTypeExprs(result, candidateType)
			}
		}
		if result != nil {
			return result
		}
	}
	objectType := c.inferExpressionType(expr.Object)
	if objectType == nil || objectType.Nullable {
		return nil
	}
	index := expr.Indices[0]
	switch objectType.Kind {
	case TypeShape:
		key, ok := staticLiteralHashKey(index)
		if !ok {
			if objectType.Name == shapeKeysStringMarker || objectType.Name == shapeKeysSymbolMarker {
				if _, isLit := staticLiteralValue(index); isLit {
					return checkTypeNil
				}
			}
			return nil
		}
		fieldType, present := objectType.Shape[key]
		// String and symbol keys address one entry, including on a declared
		// markerless shape such as `{ name: string }`. A required field is
		// therefore an exact hit; an optional field may be absent.
		if present {
			if shapeFieldOptional(fieldType) {
				return unionTypeExprs(shapeFieldValueType(fieldType), checkTypeNil)
			}
			return fieldType
		}
		// An open shape may hold an undeclared field of any type, so the
		// read stays unknown; a closed shape is known to miss.
		if objectType.Open {
			return nil
		}
		return checkTypeNil
	case TypeArray:
		if elements, exact := exactBlockRestElementTypes(objectType); exact {
			indexValue, static := staticLiteralValue(index)
			if !static || indexValue.Kind() != KindInt || indexValue.IsBigInt() {
				return nil
			}
			position := int(indexValue.Int())
			if position < 0 {
				position += len(elements)
			}
			if position < 0 || position >= len(elements) {
				return checkTypeNil
			}
			return elements[position]
		}
		if len(objectType.TypeArgs) != 1 || literalArrayElementsPartial(objectType) {
			return nil
		}
		indexKind, ok := staticOperandKind(c.inferExpressionType(index))
		if !ok || indexKind != TypeInt {
			return nil
		}
		return unionTypeExprs(objectType.TypeArgs[0], checkTypeNil)
	case TypeHash:
		if len(objectType.TypeArgs) != 2 {
			return nil
		}
		return unionTypeExprs(objectType.TypeArgs[1], checkTypeNil)
	}
	return nil
}

// --- contradiction checks at typed boundaries ---

func (c *scriptChecker) checkInferredExpressionAgainstTypeWithExpectation(
	function string,
	expr Expression,
	ty *TypeExpr,
	subject string,
	expectation expressionExpectation,
) {
	if ty == nil {
		return
	}
	inferred := c.inferExpressionTypeWithExpectation(expr, expectation)
	if inferred == nil {
		return
	}
	if err := validateTypeExprResolved(ty, c.runtimeTypeContext()); err != nil {
		return
	}
	if c.boundaryTypeRejected(inferred, ty) {
		c.add(function, expr.Pos(), "%s expected %s, got %s", subject, formatTypeExpr(ty), formatTypeExpr(inferred))
	}
}

// checkInferredArgument is the call-argument variant of
// checkInferredExpressionAgainstType, matching the existing literal-argument
// diagnostic format.
func (c *scriptChecker) checkInferredArgument(function string, expr Expression, ty *TypeExpr, callName, paramName string) {
	if ty == nil {
		return
	}
	// Prefer the fact captured at the argument's own evaluation point: a
	// later argument's mutations must not erase (or supply) facts for an
	// earlier one.
	inferred, captured := c.callArgumentFacts[expr]
	if !captured {
		inferred = c.inferExpressionTypeWithExpectation(expr, typeExpressionExpectation(ty))
	}
	if inferred == nil {
		return
	}
	if err := validateTypeExprResolved(ty, c.runtimeTypeContext()); err != nil {
		return
	}
	if c.boundaryTypeRejected(inferred, ty) {
		c.add(function, expr.Pos(), "call to %s argument %s expected %s, got %s%s",
			callName, paramName, formatTypeExpr(ty), formatTypeExpr(inferred),
			c.capturedShapeKeyKindHint(expr))
	}
}

// unknownShapeKeyKindHint used to explain a nil arm that came from not knowing
// how a shape's keys were stored. Strings and symbols now share one keyspace,
// so a required field is an exact hit and the hint is empty.
func (c *scriptChecker) unknownShapeKeyKindHint(Expression) string {
	return ""
}

// capturedShapeKeyKindHint prefers the hint captured when the argument was
// evaluated. A later argument can mutate the shape an earlier one read, which
// poisons the receiver fact, so re-deriving it here would silently drop a
// captured explanation. Under the unified keyspace a required field is an
// exact hit, so the derived hint is empty.
func (c *scriptChecker) capturedShapeKeyKindHint(expr Expression) string {
	if hint, captured := c.callArgumentHints[expr]; captured {
		return hint
	}
	return c.unknownShapeKeyKindHint(expr)
}

// bareMemberArgumentCallableFact mirrors the runtime's callable-parameter
// expectation: a bound script method (including a constructor backed by
// initialize) is passed through instead of auto-invoked. Safe navigation adds
// nil when the receiver may skip dispatch. Generated getters still evaluate
// to their property value.
func (c *scriptChecker) bareMemberArgumentCallableFact(expr Expression) (*TypeExpr, bool) {
	member, ok := expr.(*MemberExpr)
	if !ok {
		return nil, false
	}
	target, ok := c.explicitSelfMemberCallable(member)
	if !ok {
		target, ok = c.resolveMemberCallable(member)
	}
	if !ok || target.fn == nil || target.fn.Accessor == functionAccessorGetter {
		return nil, false
	}
	if member.Safe && !c.safeNavigationReceiverKnownNonNil(member.Object) {
		if typeExprIsNilOnly(c.safeNavigationReceiverFact(member.Object)) {
			return checkTypeNil, true
		}
		return unionTypeExprs(checkTypeFunction, checkTypeNil), true
	}
	return checkTypeFunction, true
}

// checkBinaryOperandTypes rejects operator uses whose operand types are known
// to be invalid at runtime, mirroring evalBinaryOperator's kind matrix.
func (c *scriptChecker) checkBinaryOperandTypes(function string, expr *BinaryExpr) {
	left := c.inferExpressionType(expr.Left)
	right := c.inferExpressionType(expr.Right)
	outcome := c.binaryOperationOutcome(expr.Operator, left, right)
	if outcome.invalid {
		c.add(function, expr.Pos(), "unsupported %s operands %s and %s",
			binaryOperatorNoun(expr.Operator), formatTypeExpr(left), formatTypeExpr(right))
	}
}

// checkUnaryOperandTypes rejects unary operator uses on operand kinds the
// runtime provably refuses.
func (c *scriptChecker) checkUnaryOperandTypes(function string, expr *UnaryExpr) {
	kind, ok := staticOperandKind(c.inferExpressionType(expr.Right))
	if !ok {
		return
	}
	switch expr.Operator {
	case tokenMinus:
		if kind != TypeInt && kind != TypeFloat && kind != TypeNumber {
			c.add(function, expr.Pos(), "unsupported unary - operand %s", formatTypeExpr(c.inferExpressionType(expr.Right)))
		}
	case tokenPlus:
		if kind != TypeInt && kind != TypeFloat && kind != TypeNumber && kind != TypeString {
			c.add(function, expr.Pos(), "unsupported unary + operand %s", formatTypeExpr(c.inferExpressionType(expr.Right)))
		}
	}
}

func (c *scriptChecker) unaryExpressionMayComplete(expr *UnaryExpr) bool {
	if expr == nil {
		return true
	}
	kind, known := staticOperandKind(c.inferExpressionType(expr.Right))
	if !known {
		return true
	}
	switch expr.Operator {
	case tokenMinus:
		return kind == TypeInt || kind == TypeFloat || kind == TypeNumber
	case tokenPlus:
		return kind == TypeInt || kind == TypeFloat || kind == TypeNumber || kind == TypeString
	default:
		return true
	}
}

// poisonEscapedIdentifier drops tracking for a mutable container local whose
// value escapes into code the checker cannot follow: a member call that may
// mutate it in place, or a by-reference argument position. An indexed or
// member projection (user["profile"]) escapes the root's interior, so the
// root local is poisoned too whenever the projected value may itself be a
// mutable container.
func (c *scriptChecker) poisonEscapedIdentifier(expr Expression) {
	if name, ok := c.escapePoisonTarget(expr); ok {
		c.poisonLocalStaticValues(name)
		c.poisonLocalType(name)
	}
}

// poisonEscapedCallValue also invalidates a shovel receiver's value and type
// facts on a path where dispatch can mutate it. A definitely non-completing
// call that never reads its parameters keeps the argument-evaluation facts
// that its rescue path observes.
func (c *scriptChecker) poisonEscapedCallValue(expr Expression, callMayComplete bool) {
	if callMayComplete {
		if name, ok := c.shovelEscapeStaticValueTarget(expr); ok {
			c.poisonLocalStaticValues(name)
			c.poisonLocalType(name)
			return
		}
	}
	c.poisonEscapedIdentifier(expr)
}

// applyExactScriptArrayArgumentMutations advances a caller's exact array
// value through a straight-line helper that only appends static literals and
// returns an unrelated static value. Every other callee shape keeps the
// ordinary escape poison.
func (c *scriptChecker) applyExactScriptArrayArgumentMutations(
	call *CallExpr,
	target staticCallable,
	resolved bool,
) map[Expression]struct{} {
	if !resolved || call == nil || target.fn == nil || target.fn.owner != c.script ||
		callExpandsArguments(call) || len(call.KwArgs) > 0 ||
		call.Block != nil ||
		len(call.Args) != len(target.fn.Params) || c.mutationRegionDepth != 0 {
		return nil
	}
	arguments := make(map[string]Expression, len(target.fn.Params))
	argumentNames := make(map[string]struct{}, len(target.fn.Params))
	for i, param := range target.fn.Params {
		if param.Kind != ParamNormal || param.Name == "" ||
			param.DefaultVal != nil || param.Type != nil {
			// Typed parameters may normalize their argument into a distinct
			// value before the body runs. Replaying mutations against the
			// caller would then mutate the wrong object.
			return nil
		}
		ident, direct := call.Args[i].(*Identifier)
		if !direct {
			continue
		}
		if _, duplicate := argumentNames[ident.Name]; duplicate ||
			c.hasCurrentContainerAlias(ident.Name) {
			return nil
		}
		values, exact := c.localStaticValuesFor(ident.Name)
		if !exact || len(values) == 0 {
			continue
		}
		for _, value := range values {
			if _, array := value.(*ArrayLiteral); !array {
				return nil
			}
		}
		argumentNames[ident.Name] = struct{}{}
		arguments[param.Name] = call.Args[i]
	}

	type exactAppend struct {
		argument Expression
		value    Expression
	}
	var appends []exactAppend
	terminalSafe := false
	for i, statement := range target.fn.Body {
		last := i == len(target.fn.Body)-1
		switch typed := statement.(type) {
		case *ExprStmt:
			binary, shovel := typed.Expr.(*BinaryExpr)
			if shovel && binary.Operator == tokenShovel {
				param, direct := binary.Left.(*Identifier)
				argument := arguments[param.Name]
				if !direct || argument == nil {
					return nil
				}
				if _, static := staticLiteralValue(binary.Right); !static {
					return nil
				}
				appends = append(appends, exactAppend{
					argument: argument,
					value:    binary.Right,
				})
				continue
			}
			if !last {
				return nil
			}
			if _, static := staticLiteralValue(typed.Expr); !static {
				return nil
			}
			terminalSafe = true
		case *ReturnStmt:
			if !last {
				return nil
			}
			if _, static := staticLiteralValue(typed.Value); !static {
				return nil
			}
			terminalSafe = true
		default:
			return nil
		}
	}
	if len(appends) == 0 || !terminalSafe {
		return nil
	}
	mutated := make(map[Expression]struct{}, len(appends))
	for _, appendEffect := range appends {
		ident := appendEffect.argument.(*Identifier)
		c.applyShovelMutationToLocal(ident.Name, appendEffect.value, true)
		mutated[appendEffect.argument] = struct{}{}
	}
	return mutated
}

// nonCompletingScriptCallLeavesParametersUnused recognizes the narrow case
// where a local script callee cannot return and never references any supplied
// parameter. Its rescue path therefore observes only argument-evaluation
// effects; every ambiguous, callback-bearing, or defaulted call stays
// poison-by-default.
func (c *scriptChecker) nonCompletingScriptCallLeavesParametersUnused(
	call *CallExpr,
	target staticCallable,
	resolved bool,
) bool {
	if !resolved || target.fn == nil || target.fn.owner != c.script || call == nil ||
		callExpandsArguments(call) || call.Block != nil {
		return false
	}
	plan := c.scriptCallBindingPlan(call, target)
	if !plan.bodyMayEnter || len(plan.defaultParams) != 0 {
		return false
	}
	params := make(map[string]struct{}, len(target.fn.Params))
	for _, param := range target.fn.Params {
		if param.Name != "" {
			params[param.Name] = struct{}{}
		}
	}
	return len(params) > 0 && !statementsReferenceAnyName(target.fn.Body, params)
}

func statementsReferenceAnyName(statements []Statement, names map[string]struct{}) bool {
	for _, statement := range statements {
		switch typed := statement.(type) {
		case *ReturnStmt:
			if expressionReferencesAnyName(typed.Value, names) {
				return true
			}
		case *RaiseStmt:
			if expressionReferencesAnyName(typed.Value, names) ||
				expressionReferencesAnyName(typed.Message, names) {
				return true
			}
		case *BreakStmt:
			if expressionReferencesAnyName(typed.Value, names) {
				return true
			}
		case *NextStmt:
			if expressionReferencesAnyName(typed.Value, names) {
				return true
			}
		case *AssignStmt:
			if expressionReferencesAnyName(typed.Target, names) ||
				expressionReferencesAnyName(typed.Value, names) {
				return true
			}
		case *ExprStmt:
			if expressionReferencesAnyName(typed.Expr, names) {
				return true
			}
		case *IfStmt:
			if expressionReferencesAnyName(typed.Condition, names) ||
				statementsReferenceAnyName(typed.Consequent, names) ||
				statementsReferenceAnyName(typed.Alternate, names) {
				return true
			}
			for _, branch := range typed.ElseIf {
				if expressionReferencesAnyName(branch.Condition, names) ||
					statementsReferenceAnyName(branch.Consequent, names) {
					return true
				}
			}
		case *ForStmt:
			if expressionReferencesAnyName(typed.Target, names) ||
				expressionReferencesAnyName(typed.Iterable, names) ||
				statementsReferenceAnyName(typed.Body, names) {
				return true
			}
		case *WhileStmt:
			if expressionReferencesAnyName(typed.Condition, names) ||
				statementsReferenceAnyName(typed.Body, names) {
				return true
			}
		case *UntilStmt:
			if expressionReferencesAnyName(typed.Condition, names) ||
				statementsReferenceAnyName(typed.Body, names) {
				return true
			}
		case *TryStmt:
			if statementsReferenceAnyName(typed.Body, names) ||
				statementsReferenceAnyName(typed.Else, names) ||
				statementsReferenceAnyName(typed.Ensure, names) {
				return true
			}
			for _, clause := range typed.Rescues {
				if statementsReferenceAnyName(clause.Body, names) {
					return true
				}
			}
		case *FunctionStmt:
			for _, param := range typed.Params {
				if expressionReferencesAnyName(param.DefaultVal, names) {
					return true
				}
			}
		case *ClassStmt:
			if statementsReferenceAnyName(typed.Body, names) {
				return true
			}
		}
	}
	return false
}

func expressionReferencesAnyName(expr Expression, names map[string]struct{}) bool {
	switch typed := expr.(type) {
	case nil:
		return false
	case *Identifier:
		_, referenced := names[typed.Name]
		return referenced
	case *ArrayLiteral:
		for _, element := range typed.Elements {
			if expressionReferencesAnyName(element, names) {
				return true
			}
		}
	case *HashLiteral:
		for _, pair := range typed.Pairs {
			if expressionReferencesAnyName(pair.Key, names) ||
				expressionReferencesAnyName(pair.Value, names) {
				return true
			}
		}
	case *CallExpr:
		if expressionReferencesAnyName(typed.Callee, names) {
			return true
		}
		for _, arg := range typed.Args {
			if expressionReferencesAnyName(arg, names) {
				return true
			}
		}
		for _, kwarg := range typed.KwArgs {
			if expressionReferencesAnyName(kwarg.Value, names) {
				return true
			}
		}
		if typed.Block != nil {
			for _, param := range typed.Block.Params {
				if expressionReferencesAnyName(param.DefaultVal, names) {
					return true
				}
			}
			return statementsReferenceAnyName(typed.Block.Body, names)
		}
	case *BlockLiteral:
		for _, param := range typed.Params {
			if expressionReferencesAnyName(param.DefaultVal, names) {
				return true
			}
		}
		return statementsReferenceAnyName(typed.Body, names)
	case *MemberExpr:
		return expressionReferencesAnyName(typed.Object, names)
	case *ScopeExpr:
		return expressionReferencesAnyName(typed.Object, names)
	case *IndexExpr:
		if expressionReferencesAnyName(typed.Object, names) {
			return true
		}
		for _, index := range typed.Indices {
			if expressionReferencesAnyName(index, names) {
				return true
			}
		}
	case *DestructureTarget:
		for _, element := range typed.Elements {
			if expressionReferencesAnyName(element.Target, names) {
				return true
			}
		}
	case *SplatArg:
		return expressionReferencesAnyName(typed.Value, names)
	case *TypeLiteral:
		return expressionReferencesAnyName(typed.Fallback, names)
	case *UnaryExpr:
		return expressionReferencesAnyName(typed.Right, names)
	case *BinaryExpr:
		return expressionReferencesAnyName(typed.Left, names) ||
			expressionReferencesAnyName(typed.Right, names)
	case *ConditionalExpr:
		return expressionReferencesAnyName(typed.Condition, names) ||
			expressionReferencesAnyName(typed.Consequent, names) ||
			expressionReferencesAnyName(typed.Alternate, names)
	case *RescueExpr:
		return expressionReferencesAnyName(typed.Body, names) ||
			expressionReferencesAnyName(typed.Fallback, names)
	case *IfExpr:
		if expressionReferencesAnyName(typed.Condition, names) ||
			expressionReferencesAnyName(typed.Consequent, names) ||
			expressionReferencesAnyName(typed.Alternate, names) {
			return true
		}
		for _, branch := range typed.ElseIf {
			if expressionReferencesAnyName(branch.Condition, names) ||
				expressionReferencesAnyName(branch.Result, names) {
				return true
			}
		}
	case *RangeExpr:
		return expressionReferencesAnyName(typed.Start, names) ||
			expressionReferencesAnyName(typed.End, names)
	case *CaseExpr:
		if expressionReferencesAnyName(typed.Target, names) ||
			expressionReferencesAnyName(typed.ElseExpr, names) {
			return true
		}
		for _, clause := range typed.Clauses {
			for _, value := range clause.Values {
				if expressionReferencesAnyName(value.Expr, names) {
					return true
				}
			}
			if expressionReferencesAnyName(clause.Result, names) {
				return true
			}
		}
	case *YieldExpr:
		for _, arg := range typed.Args {
			if expressionReferencesAnyName(arg, names) {
				return true
			}
		}
	case *InterpolatedString:
		for _, part := range typed.Parts {
			if expression, ok := part.(StringExpr); ok &&
				expressionReferencesAnyName(expression.Expr, names) {
				return true
			}
		}
	case *InterpolatedSymbol:
		for _, part := range typed.Parts {
			if expression, ok := part.(StringExpr); ok &&
				expressionReferencesAnyName(expression.Expr, names) {
				return true
			}
		}
	case *IfStmt, *ForStmt, *WhileStmt, *UntilStmt, *TryStmt:
		return statementsReferenceAnyName([]Statement{typed.(Statement)}, names)
	}
	return false
}

// shovelEscapeStaticValueTarget reports the witnessed container returned by
// a shovel expression. Unknown code may replace existing elements, so the
// whole-value fact cannot survive even when its witnessed element type can.
func (c *scriptChecker) shovelEscapeStaticValueTarget(expr Expression) (string, bool) {
	binary, ok := expr.(*BinaryExpr)
	if !ok || binary.Operator != tokenShovel {
		return "", false
	}
	ident, ok := unwrapShovelChain(binary.Left).(*Identifier)
	if !ok {
		return "", false
	}
	rootType := c.localTypeFor(ident.Name)
	if !c.typeExprHasContainerArm(rootType) {
		if rootType == nil && c.hasPossibleContainerBinding(ident.Name) {
			return ident.Name, true
		}
		values, exact := c.localStaticValuesFor(ident.Name)
		if !exact {
			return "", false
		}
		arrayExact := false
		for _, value := range values {
			if _, array := value.(*ArrayLiteral); array {
				arrayExact = true
				break
			}
		}
		if !arrayExact {
			return "", false
		}
	}
	return ident.Name, true
}

// escapePoisonTarget reports the root local whose container facts stop
// holding when expr escapes into unfollowed code: a bare container-typed
// identifier, or a projection whose value may itself be a mutable container.
func (c *scriptChecker) escapePoisonTarget(expr Expression) (string, bool) {
	if binary, ok := expr.(*BinaryExpr); ok && binary.Operator == tokenShovel {
		// A shovel expression evaluates to its mutated receiver, so the
		// receiver escapes wherever the expression's value does. A declared
		// element bound cannot survive code that may append arbitrary elements;
		// witnessed element types are invalidated only when writes disprove
		// them, while the exact whole-value fact is handled separately.
		if ident, ok := unwrapShovelChain(binary.Left).(*Identifier); ok {
			if declaredArrayElementType(c.localTypeFor(ident.Name)) != nil {
				return ident.Name, true
			}
		}
		return "", false
	}
	// A statically shadowed type literal evaluates its value reading, so the
	// escaping value is the fallback identifier's (a container local named
	// array, for example); an unshadowed literal escapes only a fresh type
	// value and poisons nothing.
	if lit, ok := expr.(*TypeLiteral); ok {
		if !c.typeLiteralStaticallyShadowed(lit) {
			return "", false
		}
		expr = lit.Fallback
	}
	name, ok := rootIdentifierName(expr)
	if !ok {
		return "", false
	}
	if _, isIdent := expr.(*Identifier); isIdent && c.localKeywordSplatFails(name) {
		return name, true
	}
	rootType := c.localTypeFor(name)
	if rootType == nil {
		if c.hasPossibleContainerBinding(name) {
			return name, true
		}
		return "", false
	}
	if !c.typeExprHasContainerArm(rootType) {
		return "", false
	}
	if _, isIdent := expr.(*Identifier); !isIdent {
		projected := c.inferExpressionType(expr)
		if projected != nil && !c.typeExprHasContainerArm(projected) {
			return "", false
		}
	}
	return name, true
}

func (c *scriptChecker) typeExprHasContainerArm(ty *TypeExpr) bool {
	result := c.typeArmSummary(ty, 0)
	return result.valid && result.kinds&containerArmKinds != 0
}

func rootIdentifierName(expr Expression) (string, bool) {
	for {
		switch typed := expr.(type) {
		case *Identifier:
			return typed.Name, true
		case *IndexExpr:
			expr = typed.Object
		case *MemberExpr:
			expr = typed.Object
		default:
			return "", false
		}
	}
}

// --- binary operator matrix ---

type binaryOutcome struct {
	result  *TypeExpr
	invalid bool
}

// binaryOperationOutcome mirrors evalBinaryOperator and the values.go kind
// matrices on inferred operand types. invalid is reported only when every
// combination of the operands' possible kinds is rejected at runtime; a
// number operand expands to both int and float before deciding.
func (c *scriptChecker) binaryOperationOutcome(op TokenType, left, right *TypeExpr) binaryOutcome {
	switch op {
	case tokenAnd, tokenOr:
		if left == nil || right == nil {
			return binaryOutcome{}
		}
		return binaryOutcome{result: unionTypeExprs(left, right)}
	case tokenEQ, tokenNotEQ, tokenCaseEQ:
		return binaryOutcome{result: checkTypeBool}
	case tokenSpaceship:
		return binaryOutcome{result: unionTypeExprs(checkTypeInt, checkTypeNil)}
	case tokenPlus, tokenMinus, tokenAsterisk, tokenSlash, tokenPercent,
		tokenPower, tokenShovel, tokenAmpersand,
		tokenLT, tokenLTE, tokenGT, tokenGTE:
	default:
		return binaryOutcome{}
	}

	leftKind, leftOK := staticOperandKind(left)
	rightKind, rightOK := staticOperandKind(right)
	if op == tokenShovel && leftOK && leftKind == TypeArray {
		// A compatible append returns the receiver still satisfying its
		// declared bound, so an assignment of the result carries the bound
		// (and the alias link) instead of degrading to a partial witness.
		if elem := declaredArrayElementType(left); elem != nil {
			if right != nil && typeExprSatisfies(right, elem, c.checkNamedTypeResolver()) {
				return binaryOutcome{result: left}
			}
		}
		// The shovel operator returns its receiver with the element
		// appended, so the appended type joins (or seeds) the witnessed
		// arms; anything less certain degrades to a bare array.
		if right != nil {
			if refined := appendedArrayFact(left, right); refined != nil {
				return binaryOutcome{result: refined}
			}
		}
		return binaryOutcome{result: checkTypeArray}
	}
	if !leftOK || !rightOK {
		// Partial knowledge decides a couple of left-driven results but never
		// an invalidity -- with one exception. When every combination of the
		// operands' alternatives is rejected, the outcome is not partially
		// known: the expression fails whichever alternatives the values take.
		if op == tokenPlus && everyConcatOperandPairInvalid(left, right) {
			return binaryOutcome{invalid: true}
		}
		if leftOK && op == tokenPercent && leftKind == TypeString {
			return binaryOutcome{result: checkTypeString}
		}
		return binaryOutcome{}
	}

	var results []*TypeExpr
	anyValid := false
	for _, lk := range expandNumericKinds(leftKind) {
		for _, rk := range expandNumericKinds(rightKind) {
			result, valid := binaryScalarOutcome(op, lk, rk)
			if !valid {
				continue
			}
			anyValid = true
			if result != nil {
				results = append(results, result)
			}
		}
	}
	if !anyValid {
		return binaryOutcome{invalid: true}
	}
	if len(results) == 0 {
		return binaryOutcome{}
	}
	return binaryOutcome{result: unionTypeExprs(results...)}
}

func expandNumericKinds(kind TypeKind) []TypeKind {
	if kind == TypeNumber {
		return []TypeKind{TypeInt, TypeFloat}
	}
	return []TypeKind{kind}
}

// binaryScalarOutcome is the per-kind matrix for a single operand-kind pair
// (no TypeNumber inputs; callers expand it first). It mirrors addValues,
// subtractValues, multiplyValues, powerValues, divideValues, moduloValues,
// shovelValues, intersectValues, and compareValueOrder, including case order
// (string concatenation is the late fallback for +).
// everyConcatOperandPairInvalid reports whether `+` rejects every combination
// of the operands' possible kinds.
//
// The checker otherwise declines to decide an invalidity from partial
// knowledge, which is right when some alternative could succeed. It is not
// right when none can: `a + b` with `a: string?` and `b: array<int>` fails as
// string + array and as nil + array alike, so the expression cannot run
// whichever alternatives the values take.
//
// The decision reuses binaryScalarOutcome rather than restating any operator's
// rules, so this stays correct as those rules change.
func everyConcatOperandPairInvalid(left, right *TypeExpr) bool {
	leftKinds, leftKnown := operandKindAlternatives(left)
	rightKinds, rightKnown := operandKindAlternatives(right)
	if !leftKnown || !rightKnown || len(leftKinds) == 0 || len(rightKinds) == 0 {
		return false
	}
	// Only decide over kinds binaryScalarOutcome actually models. A class,
	// instance, function, or enum type reaches its default and would be read as
	// "rejected" when the rules simply say nothing about it, so those keep the
	// ordinary "partial knowledge decides nothing" treatment.
	for _, kind := range append(append([]TypeKind{}, leftKinds...), rightKinds...) {
		if !concatDecidableKind(kind) {
			return false
		}
	}
	// binaryScalarOutcome takes no TypeNumber inputs, so expand each
	// alternative the way the fully-known path does before consulting it.
	for _, leftKind := range leftKinds {
		for _, lk := range expandNumericKinds(leftKind) {
			for _, rightKind := range rightKinds {
				for _, rk := range expandNumericKinds(rightKind) {
					if _, valid := binaryScalarOutcome(tokenPlus, lk, rk); valid {
						return false
					}
				}
			}
		}
	}
	return true
}

// concatDecidableKind reports whether binaryScalarOutcome models `+` for the
// kind, so a rejection genuinely means "cannot succeed" rather than "not
// described here".
func concatDecidableKind(kind TypeKind) bool {
	switch kind {
	case TypeString, TypeInt, TypeFloat, TypeNumber, TypeBool, TypeSymbol,
		TypeMoney, TypeDuration, TypeTime, TypeRange,
		TypeNil, TypeArray, TypeHash, TypeShape, TypeFunction:
		return true
	default:
		return false
	}
}

// operandKindAlternatives expands a type into the concrete kinds a value of it
// can take: a union contributes each member, and a nullable additionally
// contributes nil. It reports false when any part is unconstrained, since an
// unknown alternative could succeed.
func operandKindAlternatives(ty *TypeExpr) ([]TypeKind, bool) {
	if ty == nil {
		return nil, false
	}
	var kinds []TypeKind
	if ty.Nullable {
		kinds = append(kinds, TypeNil)
	}
	switch ty.Kind {
	case TypeAny, TypeUnknown:
		return nil, false
	case TypeUnion:
		if len(ty.Union) == 0 {
			return nil, false
		}
		for _, option := range ty.Union {
			optionKinds, ok := operandKindAlternatives(option)
			if !ok {
				return nil, false
			}
			kinds = append(kinds, optionKinds...)
		}
		return kinds, true
	case TypeShape:
		return append(kinds, TypeHash), true
	}
	return append(kinds, ty.Kind), true
}

// concatenableTypeKind mirrors the runtime's concatenableWithString for a
// statically known type kind, so a script whose operands are both known does
// not pass the checker and then fail at the runtime guard. TypeAny and
// TypeUnknown fall to the default and are treated as not statically
// concatenable, which only means the outcome is unresolved here -- the caller
// leaves such a pair to the runtime rather than reporting it.
func concatenableTypeKind(kind TypeKind) bool {
	switch kind {
	case TypeString, TypeInt, TypeFloat, TypeNumber, TypeBool, TypeSymbol,
		TypeMoney, TypeDuration, TypeTime, TypeRange, TypeEnum:
		return true
	default:
		// Mirrors the runtime allowlist: nil, containers, callables, and
		// anything unknown are not renderable into a concatenation.
		return false
	}
}

func binaryScalarOutcome(op TokenType, lk, rk TypeKind) (*TypeExpr, bool) {
	isNum := func(kind TypeKind) bool { return kind == TypeInt || kind == TypeFloat }
	numResult := func() *TypeExpr {
		if lk == TypeInt && rk == TypeInt {
			return checkTypeInt
		}
		return checkTypeFloat
	}
	switch op {
	case tokenPlus:
		switch {
		case isNum(lk) && isNum(rk):
			return numResult(), true
		case lk == TypeTime && (rk == TypeDuration || isNum(rk)):
			return checkTypeTime, true
		case rk == TypeTime && (lk == TypeDuration || isNum(lk)):
			return checkTypeTime, true
		case lk == TypeDuration && (rk == TypeDuration || isNum(rk)):
			return checkTypeDuration, true
		case rk == TypeDuration && isNum(lk):
			return checkTypeDuration, true
		case lk == TypeArray && rk == TypeArray:
			return checkTypeArray, true
		case lk == TypeString || rk == TypeString:
			// Concatenation renders the other operand, but only kinds with a
			// meaningful string form are accepted at runtime: nil renders as
			// empty and the containers render as a placeholder, so those pairs
			// raise rather than producing text (see concatenableWithString).
			if !concatenableTypeKind(lk) || !concatenableTypeKind(rk) {
				return nil, false
			}
			return checkTypeString, true
		case lk == TypeMoney && rk == TypeMoney:
			return checkTypeMoney, true
		}
	case tokenMinus:
		switch {
		case isNum(lk) && isNum(rk):
			return numResult(), true
		case lk == TypeTime && (rk == TypeDuration || isNum(rk)):
			return checkTypeTime, true
		case lk == TypeTime && rk == TypeTime:
			return checkTypeFloat, true
		case lk == TypeDuration && (rk == TypeDuration || isNum(rk)):
			return checkTypeDuration, true
		case lk == TypeArray && rk == TypeArray:
			return checkTypeArray, true
		case lk == TypeMoney && rk == TypeMoney:
			return checkTypeMoney, true
		}
	case tokenAsterisk:
		switch {
		case isNum(lk) && isNum(rk):
			return numResult(), true
		// String repetition: the count may be a float, which the runtime
		// truncates toward zero as Ruby does. Only a string on the left
		// repeats; multiplication is not commutative here.
		case lk == TypeString && isNum(rk):
			return checkTypeString, true
		case lk == TypeDuration && isNum(rk):
			return checkTypeDuration, true
		case rk == TypeDuration && isNum(lk):
			return checkTypeDuration, true
		case lk == TypeMoney && rk == TypeInt:
			return checkTypeMoney, true
		case lk == TypeInt && rk == TypeMoney:
			return checkTypeMoney, true
		}
	case tokenPower:
		switch {
		case lk == TypeInt && rk == TypeInt:
			// Negative exponents fall through to float exponentiation.
			return checkTypeNumber, true
		case isNum(lk) && isNum(rk):
			return checkTypeFloat, true
		}
	case tokenSlash:
		switch {
		case lk == TypeInt && rk == TypeInt:
			return checkTypeInt, true
		case isNum(lk) && isNum(rk):
			return checkTypeFloat, true
		case lk == TypeDuration && rk == TypeDuration:
			return checkTypeFloat, true
		case lk == TypeDuration && isNum(rk):
			return checkTypeDuration, true
		case lk == TypeMoney && rk == TypeInt:
			return checkTypeMoney, true
		}
	case tokenPercent:
		switch {
		case lk == TypeString:
			return checkTypeString, true
		case lk == TypeInt && rk == TypeInt:
			return checkTypeInt, true
		case lk == TypeDuration && rk == TypeDuration:
			return checkTypeDuration, true
		}
	case tokenShovel:
		if lk == TypeArray {
			return checkTypeArray, true
		}
	case tokenAmpersand:
		if lk == TypeArray && rk == TypeArray {
			return checkTypeArray, true
		}
	case tokenLT, tokenLTE, tokenGT, tokenGTE:
		switch {
		case isNum(lk) && isNum(rk),
			lk == TypeString && rk == TypeString,
			// Symbol includes Comparable in Ruby, so the relational operators
			// order symbols at runtime; the matrix has to agree or check
			// rejects a script that runs.
			lk == TypeSymbol && rk == TypeSymbol,
			lk == TypeMoney && rk == TypeMoney,
			lk == TypeDuration && rk == TypeDuration,
			lk == TypeTime && rk == TypeTime:
			return checkTypeBool, true
		}
	}
	return nil, false
}

func binaryOperatorNoun(op TokenType) string {
	switch op {
	case tokenPlus:
		return "addition"
	case tokenMinus:
		return "subtraction"
	case tokenAsterisk:
		return "multiplication"
	case tokenSlash:
		return "division"
	case tokenPercent:
		return "modulo"
	case tokenPower:
		return "exponentiation"
	case tokenShovel:
		return "shovel"
	case tokenAmpersand:
		return "intersection"
	case tokenLT, tokenLTE, tokenGT, tokenGTE:
		return "comparison"
	}
	return "operator"
}

// staticOperandKind resolves an inferred type to a single operand kind for
// the operator matrix. Unions collapse only when purely numeric; nullable,
// enum, any, and unknown types stay undecided.
func staticOperandKind(ty *TypeExpr) (TypeKind, bool) {
	if ty == nil || ty.Nullable {
		return TypeUnknown, false
	}
	switch ty.Kind {
	case TypeAny, TypeUnknown, TypeEnum:
		return TypeUnknown, false
	case TypeUnion:
		for _, option := range ty.Union {
			kind, ok := staticOperandKind(option)
			if !ok {
				return TypeUnknown, false
			}
			switch kind {
			case TypeInt, TypeFloat, TypeNumber:
			default:
				return TypeUnknown, false
			}
		}
		return TypeNumber, true
	case TypeShape:
		return TypeHash, true
	}
	return ty.Kind, true
}

// --- type joins and disjointness ---

const maxInferredUnionArms = 6

// unionTypeExprs joins types into a deduplicated union; any unknown input or
// an oversized result collapses to unknown.
func unionTypeExprs(types ...*TypeExpr) *TypeExpr {
	arms := make([]*TypeExpr, 0, len(types))
	seen := make(map[string][]*TypeExpr, len(types))
	appendArm := func(arm *TypeExpr) {
		// The dedup key canonicalizes the whole fact, including the internal
		// Name markers at every nesting level (shape key kinds, witnessed
		// array elements), so arms that render identically but carry
		// different markers stay distinct instead of collapsing to whichever
		// branch was joined first.
		//
		// It groups arms rather than deciding them. The key stops at
		// maxTypeArmDepth and renders everything below as `?`, so two arms
		// alike that far down and different underneath share a key while being
		// different types. Dropping one on the key alone lost the difference:
		// widening a field past the exact-refinement budget joins what the
		// field held to what a write put there, and a write differing from the
		// field only below the cutoff was joined away, leaving the field
		// claiming the shape it held before the write.
		key := typeFactKey(arm)
		for _, kept := range seen[key] {
			if typeFactsIdentical(kept, arm) {
				return
			}
		}
		seen[key] = append(seen[key], arm)
		arms = append(arms, arm)
	}
	for _, ty := range types {
		if ty == nil {
			return nil
		}
		if ty.Kind == TypeUnion && !ty.Nullable {
			for _, option := range ty.Union {
				if option == nil {
					return nil
				}
				appendArm(option)
			}
			continue
		}
		appendArm(ty)
	}
	arms = collapseBooleanFactArms(arms)
	if len(arms) == 0 || len(arms) > maxInferredUnionArms {
		return nil
	}
	if len(arms) == 1 {
		return arms[0]
	}
	return &TypeExpr{Kind: TypeUnion, Union: arms}
}

func collapseBooleanFactArms(arms []*TypeExpr) []*TypeExpr {
	var hasTrue, hasFalse, hasGeneral bool
	for _, arm := range arms {
		if arm == nil || arm.Kind != TypeBool {
			continue
		}
		switch arm.Name {
		case boolTrueFactMarker:
			hasTrue = true
		case boolFalseFactMarker:
			hasFalse = true
		default:
			hasGeneral = true
		}
	}
	if !hasGeneral && (!hasTrue || !hasFalse) {
		return arms
	}

	collapsed := make([]*TypeExpr, 0, len(arms))
	insertedBool := false
	for _, arm := range arms {
		if arm == nil || arm.Kind != TypeBool ||
			(arm.Name != boolTrueFactMarker && arm.Name != boolFalseFactMarker) {
			collapsed = append(collapsed, arm)
			continue
		}
		if hasGeneral {
			continue
		}
		if !insertedBool {
			collapsed = append(collapsed, checkTypeBool)
			insertedBool = true
		}
	}
	return collapsed
}

// typeExprFactSizeWithin reports whether ty canonicalizes to at most limit
// nodes. It walks what appendTypeFactKey walks, under the same depth cap, and
// abandons the walk the moment the budget is spent, so measuring a type a
// caller is about to refuse costs the budget rather than the type's own size.
func typeExprFactSizeWithin(ty *TypeExpr, limit int) bool {
	remaining := limit
	return typeExprFactSizeRemaining(ty, 0, &remaining)
}

func typeExprFactSizeRemaining(ty *TypeExpr, depth int, remaining *int) bool {
	if *remaining <= 0 {
		return false
	}
	*remaining--
	if ty == nil || depth > maxTypeArmDepth {
		return true
	}
	for _, arg := range ty.TypeArgs {
		if !typeExprFactSizeRemaining(arg, depth+1, remaining) {
			return false
		}
	}
	for _, field := range ty.Shape {
		if !typeExprFactSizeRemaining(field, depth+1, remaining) {
			return false
		}
	}
	for _, option := range ty.Union {
		if !typeExprFactSizeRemaining(option, depth+1, remaining) {
			return false
		}
	}
	return true
}

// typeFactKey canonicalizes a type fact for deduplication. Unlike
// formatTypeExpr it includes the Name field (which carries the internal
// key-kind and witnessed-element markers) at every nesting level.
//
// It groups facts. It does not decide them. The walk stops at maxTypeArmDepth
// and renders everything below as `?`, so two facts alike that far down and
// different underneath produce one key. A match means "these belong in the same
// bucket", never "these are the same fact". Use typeFactsIdentical for the
// second question.
//
// The trap is not that it approximates, it is that it approximates silently.
// Three walkers here stop at maxTypeArmDepth and the other two say so:
// typeExprArms returns ok=false when it hits the limit and every caller refuses
// to conclude, and typeExprFactSizeRemaining answers with a bool the same way.
// This one hands back a string that looks like a complete answer and carries no
// sign it stopped early, so a caller has to already know. Four defects came from
// callers that did not: a field write took a matching key for proof that the
// write restated the field, the union join took one for proof that an arm was a
// duplicate, and the two mutator predicates took one for proof that an argument
// walk had left the receiver's local alone and put a fact the script had
// stopped holding back in place. All four now walk.
//
// The rule that generalizes is that an approximate answer has to carry its own
// exactness, and there were two ways to give this one that: report whether it
// truncated, or stop asking it the question it cannot answer. The second is
// what happened, at every caller that decides, and the reason is that all four
// compare one pair. typeFactsIdentical is not a fallback for those -- it is
// cheaper than the key outright, since it allocates nothing, stops at the first
// difference instead of rendering both sides in full, and treats the same node
// as the same fact at every level rather than only at the root. A truncation
// flag would have bought them nothing but a refusal to conclude on deep facts,
// which is a wrong answer where the walk has a right one.
//
// Every caller left groups: four cache keys and the union join's bucket, all of
// which want facts that render alike in one bucket and are correct with it. A
// new caller that needs to decide should call typeFactsIdentical rather than
// reach for this, and one that needs to decide across many pairs should use
// this as a bucket and confirm with that, which is what the join does.
func typeFactKey(ty *TypeExpr) string {
	var b strings.Builder
	appendTypeFactKey(&b, ty, 0)
	return b.String()
}

func appendTypeFactKey(b *strings.Builder, ty *TypeExpr, depth int) {
	if ty == nil || depth > maxTypeArmDepth {
		b.WriteString("?")
		return
	}
	b.WriteString(strconv.Itoa(int(ty.Kind)))
	b.WriteString(":")
	b.WriteString(ty.Name)
	if ty.Nullable {
		b.WriteString("?")
	}
	if ty.Optional {
		b.WriteString("~")
	}
	if ty.Open {
		b.WriteString("+")
	}
	if len(ty.TypeArgs) > 0 {
		b.WriteString("<")
		for i, arg := range ty.TypeArgs {
			if i > 0 {
				b.WriteString(",")
			}
			appendTypeFactKey(b, arg, depth+1)
		}
		b.WriteString(">")
	}
	if len(ty.Shape) > 0 {
		fields := make([]string, 0, len(ty.Shape))
		for field := range ty.Shape {
			fields = append(fields, field)
		}
		slices.Sort(fields)
		b.WriteString("{")
		for _, field := range fields {
			b.WriteString(field)
			b.WriteString(":")
			appendTypeFactKey(b, ty.Shape[field], depth+1)
			b.WriteString(",")
		}
		b.WriteString("}")
	}
	if len(ty.Union) > 0 {
		b.WriteString("(")
		for i, option := range ty.Union {
			if i > 0 {
				b.WriteString("|")
			}
			appendTypeFactKey(b, option, depth+1)
		}
		b.WriteString(")")
	}
}

// namedTypeResolver resolves a named (TypeEnum kind) annotation to its enum
// or class definition. A nil resolver, or a miss, keeps named arms
// conservatively compatible with everything, preserving gradual behavior for
// unresolved and host-supplied names.
type namedTypeResolver func(*TypeExpr) (namedTypeMatch, bool)

// checkNamedTypeResolver resolves named annotations through the same
// environments the runtime binds, so two spellings of one definition compare
// equal by resolved identity.
func (c *scriptChecker) checkNamedTypeResolver() namedTypeResolver {
	return func(ty *TypeExpr) (namedTypeMatch, bool) {
		match, ok, err := lookupNamedTypeForType(ty, c.runtimeTypeContext())
		if err != nil || !ok {
			return namedTypeMatch{}, false
		}
		return match, true
	}
}

// typeExprsDisjoint reports whether no runtime value can satisfy both types.
// It is deliberately conservative: unknown, any, and unresolved named types
// never count as disjoint, empty containers make all array and hash types
// overlap, and only exact shapes compare structurally. Resolved named types
// compare by definition identity through resolve.
func typeExprsDisjoint(a, b *TypeExpr, resolve namedTypeResolver) bool {
	aArms, ok := typeExprArms(a, 0)
	if !ok || len(aArms) == 0 {
		return false
	}
	bArms, ok := typeExprArms(b, 0)
	if !ok || len(bArms) == 0 {
		return false
	}
	for _, x := range aArms {
		for _, y := range bArms {
			if !typeArmPairDisjoint(x, y, resolve) {
				return false
			}
		}
	}
	return true
}

type boundaryTypeRelation uint8

const (
	boundaryRelationGradual boundaryTypeRelation = iota
	boundaryRelationAccepted
	boundaryRelationRejected
)

type boundaryTypeContext struct {
	resolve          namedTypeResolver
	shapeFieldSource func(*TypeExpr, string) (checkValueSource, bool)
}

// boundaryTypeRejected reports whether a finite inferred alternative cannot
// satisfy a typed boundary. Opaque alternatives defer individually, so any
// and unknown values stay gradual without hiding an incompatible known arm.
func boundaryTypeRejected(inferred, required *TypeExpr, resolve namedTypeResolver) bool {
	return boundaryTypeExprRelation(
		inferred,
		required,
		boundaryTypeContext{resolve: resolve},
		0,
	) == boundaryRelationRejected
}

func (c *scriptChecker) boundaryTypeRejected(inferred, required *TypeExpr) bool {
	return boundaryTypeExprRelation(
		inferred,
		required,
		boundaryTypeContext{
			resolve:          c.checkNamedTypeResolver(),
			shapeFieldSource: c.boundaryShapeFieldSource,
		},
		0,
	) == boundaryRelationRejected
}

func boundaryTypeExprRelation(
	inferred,
	required *TypeExpr,
	ctx boundaryTypeContext,
	depth int,
) boundaryTypeRelation {
	if inferred == nil || required == nil || depth > maxNormalizeDepth {
		return boundaryRelationGradual
	}
	inferredArms, ok := boundaryTypeExprArms(inferred, depth)
	if !ok || len(inferredArms) == 0 {
		return boundaryRelationGradual
	}
	requiredArms, ok := boundaryTypeExprArms(required, depth)
	if !ok || len(requiredArms) == 0 {
		return boundaryRelationGradual
	}

	result := boundaryRelationAccepted
	for _, inferredArm := range inferredArms {
		armResult := boundaryRelationRejected
		for _, requiredArm := range requiredArms {
			relation := boundaryTypeArmRelation(inferredArm, requiredArm, ctx, depth)
			if relation == boundaryRelationAccepted {
				armResult = boundaryRelationAccepted
				break
			}
			if relation == boundaryRelationGradual {
				armResult = boundaryRelationGradual
			}
		}
		if armResult == boundaryRelationRejected {
			armResult = boundaryTypeUnionCoverage(inferredArm, requiredArms, ctx, depth)
		}
		if armResult == boundaryRelationRejected {
			return boundaryRelationRejected
		}
		if armResult == boundaryRelationGradual {
			result = boundaryRelationGradual
		}
	}
	return result
}

func boundaryTypeUnionCoverage(
	inferred *TypeExpr,
	required []*TypeExpr,
	ctx boundaryTypeContext,
	depth int,
) boundaryTypeRelation {
	switch inferred.Kind {
	case TypeArray:
		return boundaryArrayUnionCoverage(inferred, required, ctx, depth)
	case TypeShape:
		return boundaryShapeUnionCoverage(inferred, required, ctx, depth)
	default:
		return boundaryRelationRejected
	}
}

func boundaryArrayUnionCoverage(
	inferred *TypeExpr,
	required []*TypeExpr,
	ctx boundaryTypeContext,
	depth int,
) boundaryTypeRelation {
	if (inferred.Name != literalAlternativeElementsMarker &&
		inferred.Name != literalPartialAlternativeElementsMarker) ||
		len(inferred.TypeArgs) != 1 {
		return boundaryRelationRejected
	}
	elementTypes := make([]*TypeExpr, 0, len(required))
	for _, arm := range required {
		if arm.Kind != TypeArray {
			continue
		}
		if len(arm.TypeArgs) == 0 {
			return boundaryRelationAccepted
		}
		if len(arm.TypeArgs) == 1 {
			elementTypes = append(elementTypes, arm.TypeArgs[0])
		}
	}
	requiredElements := unionTypeExprs(elementTypes...)
	if requiredElements == nil {
		return boundaryRelationRejected
	}
	relation := boundaryTypeExprRelation(inferred.TypeArgs[0], requiredElements, ctx, depth+1)
	if inferred.Name == literalPartialAlternativeElementsMarker &&
		relation != boundaryRelationRejected {
		return boundaryRelationGradual
	}
	return relation
}

type boundaryShapeFieldGroup struct {
	fields   []string
	options  []*TypeExpr
	optional bool
}

const maxBoundaryShapeVariants = 1024

func boundaryShapeUnionCoverage(
	inferred *TypeExpr,
	required []*TypeExpr,
	ctx boundaryTypeContext,
	depth int,
) boundaryTypeRelation {
	fields := make([]string, 0, len(inferred.Shape))
	for field := range inferred.Shape {
		fields = append(fields, field)
	}
	slices.Sort(fields)

	groups := make([]boundaryShapeFieldGroup, 0, len(fields))
	groupBySource := make(map[checkValueSource]int, len(fields))
	hasAlternatives := false
	for _, field := range fields {
		template := inferred.Shape[field]
		valueType := shapeFieldValueType(template)
		options, ok := boundaryTypeExprArms(valueType, depth+1)
		if !ok || len(options) == 0 {
			return boundaryRelationGradual
		}
		if len(options) > 1 {
			hasAlternatives = true
		}
		optional := shapeFieldOptional(template)
		if optional {
			hasAlternatives = true
		} else if ctx.shapeFieldSource != nil {
			if source, sourced := ctx.shapeFieldSource(inferred, field); sourced {
				if index, ok := groupBySource[source]; ok {
					groups[index].fields = append(groups[index].fields, field)
					continue
				}
				groupBySource[source] = len(groups)
			}
		}
		groups = append(groups, boundaryShapeFieldGroup{
			fields:   []string{field},
			options:  options,
			optional: optional,
		})
	}
	if !hasAlternatives {
		return boundaryRelationRejected
	}

	variant := &TypeExpr{
		Kind:  TypeShape,
		Name:  inferred.Name,
		Shape: make(map[string]*TypeExpr, len(inferred.Shape)),
		Open:  inferred.Open,
	}
	result := boundaryRelationAccepted
	variants := 0
	var walk func(int) boundaryTypeRelation
	walk = func(index int) boundaryTypeRelation {
		if variants >= maxBoundaryShapeVariants {
			return boundaryRelationGradual
		}
		if index == len(groups) {
			variants++
			return boundaryShapeVariantRelation(variant, required, ctx, depth)
		}
		group := groups[index]
		groupResult := boundaryRelationAccepted
		if group.optional {
			relation := walk(index + 1)
			if relation == boundaryRelationRejected {
				return boundaryRelationRejected
			}
			if relation == boundaryRelationGradual {
				groupResult = boundaryRelationGradual
			}
		}
		for _, option := range group.options {
			for _, field := range group.fields {
				fieldType := *option
				fieldType.Optional = false
				variant.Shape[field] = &fieldType
			}
			relation := walk(index + 1)
			for _, field := range group.fields {
				delete(variant.Shape, field)
			}
			if relation == boundaryRelationRejected {
				return boundaryRelationRejected
			}
			if relation == boundaryRelationGradual {
				groupResult = boundaryRelationGradual
			}
		}
		return groupResult
	}
	if relation := walk(0); relation != boundaryRelationAccepted {
		result = relation
	}
	return result
}

func boundaryShapeVariantRelation(
	inferred *TypeExpr,
	required []*TypeExpr,
	ctx boundaryTypeContext,
	depth int,
) boundaryTypeRelation {
	result := boundaryRelationRejected
	for _, arm := range required {
		relation := boundaryTypeArmRelation(inferred, arm, ctx, depth)
		if relation == boundaryRelationAccepted {
			return boundaryRelationAccepted
		}
		if relation == boundaryRelationGradual {
			result = boundaryRelationGradual
		}
	}
	return result
}

func boundaryTypeExprArms(ty *TypeExpr, depth int) ([]*TypeExpr, bool) {
	if ty == nil || depth > maxNormalizeDepth {
		return nil, false
	}
	arms := make([]*TypeExpr, 0, 2)
	switch ty.Kind {
	case TypeUnion:
		for _, option := range ty.Union {
			sub, ok := boundaryTypeExprArms(option, depth+1)
			if !ok {
				return nil, false
			}
			arms = append(arms, sub...)
		}
	case TypeNumber:
		arms = append(arms, checkTypeInt, checkTypeFloat)
	default:
		if ty.Nullable {
			clone := *ty
			clone.Nullable = false
			arms = append(arms, &clone)
		} else {
			arms = append(arms, ty)
		}
	}
	if ty.Nullable {
		arms = append(arms, checkTypeNil)
	}
	return arms, true
}

func boundaryTypeArmRelation(
	inferred,
	required *TypeExpr,
	ctx boundaryTypeContext,
	depth int,
) boundaryTypeRelation {
	if inferred == nil || required == nil {
		return boundaryRelationGradual
	}
	if required.Kind == TypeAny && !required.Nullable {
		return boundaryRelationAccepted
	}
	if _, shapeValue := shapeValuePayload(inferred); !shapeValue &&
		(inferred.Kind == TypeAny || inferred.Kind == TypeUnknown) {
		return boundaryRelationGradual
	}
	if required.Kind == TypeUnknown {
		return boundaryRelationGradual
	}
	if typeArmAdmits(required, inferred, ctx.resolve) {
		return boundaryRelationAccepted
	}

	switch {
	case inferred.Kind == TypeArray && required.Kind == TypeArray:
		return boundaryArrayRelation(inferred, required, ctx, depth+1)
	case inferred.Kind == TypeHash && required.Kind == TypeHash:
		return boundaryHashRelation(inferred, required, ctx, depth+1)
	case inferred.Kind == TypeShape && required.Kind == TypeShape:
		return boundaryShapeRelation(inferred, required, ctx, depth+1)
	case inferred.Kind == TypeShape && required.Kind == TypeHash:
		return boundaryShapeHashRelation(inferred, required, ctx, depth+1)
	}
	if typeArmPairDisjoint(inferred, required, ctx.resolve) {
		return boundaryRelationRejected
	}
	return boundaryRelationGradual
}

func boundaryArrayRelation(
	inferred,
	required *TypeExpr,
	ctx boundaryTypeContext,
	depth int,
) boundaryTypeRelation {
	if len(required.TypeArgs) == 0 {
		return boundaryRelationAccepted
	}
	if len(required.TypeArgs) != 1 || len(inferred.TypeArgs) != 1 {
		return boundaryRelationGradual
	}
	relation := boundaryTypeExprRelation(inferred.TypeArgs[0], required.TypeArgs[0], ctx, depth)
	if (inferred.Name == literalPartialElementsMarker ||
		inferred.Name == literalPartialAlternativeElementsMarker) &&
		relation != boundaryRelationRejected {
		return boundaryRelationGradual
	}
	return relation
}

func boundaryHashKeyRelation(
	inferred,
	required *TypeExpr,
	ctx boundaryTypeContext,
	depth int,
) boundaryTypeRelation {
	decidedInf, matchesInf := typeAllowsStringHashKey(inferred)
	decidedReq, matchesReq := typeAllowsStringHashKey(required)
	if decidedInf && decidedReq {
		if matchesInf && matchesReq {
			return boundaryRelationAccepted
		}
		if matchesInf != matchesReq {
			return boundaryRelationRejected
		}
	}
	return boundaryTypeExprRelation(inferred, required, ctx, depth)
}

func boundaryHashRelation(
	inferred,
	required *TypeExpr,
	ctx boundaryTypeContext,
	depth int,
) boundaryTypeRelation {
	if len(required.TypeArgs) == 0 {
		return boundaryRelationAccepted
	}
	if len(required.TypeArgs) != 2 || len(inferred.TypeArgs) != 2 {
		return boundaryRelationGradual
	}
	keyRelation := boundaryHashKeyRelation(inferred.TypeArgs[0], required.TypeArgs[0], ctx, depth)
	if keyRelation == boundaryRelationRejected {
		return boundaryRelationRejected
	}
	valueRelation := boundaryTypeExprRelation(inferred.TypeArgs[1], required.TypeArgs[1], ctx, depth)
	if valueRelation == boundaryRelationRejected {
		return boundaryRelationRejected
	}
	if keyRelation == boundaryRelationGradual || valueRelation == boundaryRelationGradual {
		return boundaryRelationGradual
	}
	return boundaryRelationAccepted
}

func boundaryShapeRelation(
	inferred,
	required *TypeExpr,
	ctx boundaryTypeContext,
	depth int,
) boundaryTypeRelation {
	result := boundaryRelationAccepted
	for field, requiredField := range required.Shape {
		inferredField, ok := inferred.Shape[field]
		if !ok {
			if shapeFieldOptional(requiredField) {
				continue
			}
			if inferred.Open {
				result = boundaryRelationGradual
				continue
			}
			return boundaryRelationRejected
		}
		if shapeFieldOptional(inferredField) && !shapeFieldOptional(requiredField) {
			return boundaryRelationRejected
		}
		relation := boundaryTypeExprRelation(
			shapeFieldValueType(inferredField),
			shapeFieldValueType(requiredField),
			ctx,
			depth,
		)
		if relation == boundaryRelationRejected {
			return boundaryRelationRejected
		}
		if relation == boundaryRelationGradual {
			result = boundaryRelationGradual
		}
	}
	if !required.Open {
		for field := range inferred.Shape {
			if _, ok := required.Shape[field]; !ok {
				return boundaryRelationRejected
			}
		}
		if inferred.Open {
			result = boundaryRelationGradual
		}
	}
	return result
}

func boundaryShapeHashRelation(
	inferred,
	required *TypeExpr,
	ctx boundaryTypeContext,
	depth int,
) boundaryTypeRelation {
	if len(required.TypeArgs) == 0 || !inferred.Open && len(inferred.Shape) == 0 {
		return boundaryRelationAccepted
	}
	if len(required.TypeArgs) != 2 {
		return boundaryRelationGradual
	}
	if after, ok := strings.CutPrefix(inferred.Name, shapeKeysMixedPrefix); ok {
		flags := after
		if strings.Contains(flags, "o") &&
			typeExprArmsAll(required.TypeArgs[0], func(arm *TypeExpr) bool {
				return arm.Kind == TypeString || arm.Kind == TypeSymbol
			}) {
			return boundaryRelationRejected
		}
	}
	keyType := boundaryShapeKeyType(inferred)
	result := boundaryRelationAccepted
	if keyType == nil {
		result = boundaryRelationGradual
	} else if relation := boundaryHashKeyRelation(keyType, required.TypeArgs[0], ctx, depth); relation == boundaryRelationRejected {
		return boundaryRelationRejected
	} else if relation == boundaryRelationGradual {
		result = boundaryRelationGradual
	}
	for _, fieldType := range inferred.Shape {
		relation := boundaryTypeExprRelation(
			shapeFieldValueType(fieldType),
			required.TypeArgs[1],
			ctx,
			depth,
		)
		if relation == boundaryRelationRejected {
			return boundaryRelationRejected
		}
		if relation == boundaryRelationGradual {
			result = boundaryRelationGradual
		}
	}
	if inferred.Open {
		result = boundaryRelationGradual
	}
	return result
}

func boundaryShapeKeyType(shape *TypeExpr) *TypeExpr {
	switch {
	case shape.Name == shapeKeysStringMarker:
		return checkTypeString
	case shape.Name == shapeKeysSymbolMarker:
		return checkTypeSymbol
	case strings.HasPrefix(shape.Name, shapeKeysMixedPrefix):
		flags := strings.TrimPrefix(shape.Name, shapeKeysMixedPrefix)
		var kinds []*TypeExpr
		if strings.Contains(flags, "s") {
			kinds = append(kinds, checkTypeSymbol)
		}
		if strings.Contains(flags, "t") {
			kinds = append(kinds, checkTypeString)
		}
		if strings.Contains(flags, "o") {
			kinds = append(kinds, &TypeExpr{Kind: TypeUnknown})
		}
		return unionTypeExprs(kinds...)
	default:
		return nil
	}
}

// typeExprSatisfies reports whether every runtime value of the written type
// provably satisfies the declared annotation — the dual of typeExprsDisjoint,
// and just as deliberately conservative: unknown or any-typed writes and
// relations the checker does not model all report false, so callers weaken a
// fact instead of preserving it.
func typeExprSatisfies(written, declared *TypeExpr, resolve namedTypeResolver) bool {
	if written == nil || declared == nil {
		return false
	}
	if declared.Kind == TypeAny && !declared.Nullable {
		return true
	}
	writtenArms, ok := typeExprArms(written, 0)
	if !ok || len(writtenArms) == 0 {
		return false
	}
	declaredArms, ok := typeExprArms(declared, 0)
	if !ok || len(declaredArms) == 0 {
		return false
	}
	for _, w := range writtenArms {
		admitted := false
		for _, d := range declaredArms {
			if typeArmAdmits(d, w, resolve) {
				admitted = true
				break
			}
		}
		if !admitted {
			return false
		}
	}
	return true
}

// typeArmAdmits reports whether every value of the written arm satisfies the
// declared arm, mirroring runtime annotation normalization: exact kind
// matches, number admitting int and float, containers whose facts bound
// every contained value, and resolved named types by definition identity.
// First-class shape values satisfy no annotation the arms model here.
func typeArmAdmits(declared, written *TypeExpr, resolve namedTypeResolver) bool {
	if _, isShapeValue := shapeValuePayload(written); isShapeValue {
		return false
	}
	if _, isShapeValue := shapeValuePayload(declared); isShapeValue {
		return false
	}
	switch declared.Kind {
	case TypeNumber:
		return written.Kind == TypeInt || written.Kind == TypeFloat || written.Kind == TypeNumber
	case TypeInt, TypeFloat, TypeString, TypeBool, TypeNil, TypeSymbol,
		TypeDuration, TypeTime, TypeMoney, TypeRange, TypeFunction:
		return written.Kind == declared.Kind
	case TypeArray:
		if written.Kind != TypeArray {
			return false
		}
		if len(declared.TypeArgs) != 1 {
			// A bare array annotation admits every array.
			return len(declared.TypeArgs) == 0
		}
		if written.Name == blockRestElementsMarker && len(written.TypeArgs) == 0 {
			return true
		}
		if literalArrayElementsPartial(written) || len(written.TypeArgs) != 1 {
			// Partial witnesses and bare arrays do not bound their elements.
			return false
		}
		return typeExprSatisfies(written.TypeArgs[0], declared.TypeArgs[0], resolve)
	case TypeHash:
		switch written.Kind {
		case TypeHash:
			if len(declared.TypeArgs) == 0 {
				return true
			}
			if len(declared.TypeArgs) != 2 || len(written.TypeArgs) != 2 {
				return false
			}
			return hashKeyTypeSatisfies(written.TypeArgs[0], declared.TypeArgs[0], resolve) &&
				typeExprSatisfies(written.TypeArgs[1], declared.TypeArgs[1], resolve)
		case TypeShape:
			// A shape is a hash at runtime; with a known key representation
			// its keys and witnessed fields can be checked against the hash
			// bounds. Open shapes may contain unwitnessed values, so they can
			// prove only an any-valued typed hash; any shape satisfies a bare
			// hash annotation.
			if len(declared.TypeArgs) == 0 {
				return true
			}
			if len(declared.TypeArgs) != 2 {
				return false
			}
			// An exact shape witnesses every entry, so an empty shape has
			// none and satisfies any hash bounds.
			if !written.Open && len(written.Shape) == 0 {
				return true
			}
			var keyType *TypeExpr
			switch {
			case written.Name == shapeKeysStringMarker:
				keyType = checkTypeString
			case written.Name == shapeKeysSymbolMarker:
				keyType = checkTypeSymbol
			case strings.HasPrefix(written.Name, shapeKeysMixedPrefix):
				// A mixed store whose witnessed kinds are only symbols and
				// strings bounds its keys by that union; an "other" witness
				// leaves the key kinds unprovable.
				flags := strings.TrimPrefix(written.Name, shapeKeysMixedPrefix)
				if strings.Contains(flags, "o") {
					return false
				}
				kinds := make([]*TypeExpr, 0, 2)
				if strings.Contains(flags, "s") {
					kinds = append(kinds, checkTypeSymbol)
				}
				if strings.Contains(flags, "t") {
					kinds = append(kinds, checkTypeString)
				}
				keyType = unionTypeExprs(kinds...)
				if keyType == nil {
					return false
				}
			default:
				return false
			}
			if !hashKeyTypeSatisfies(keyType, declared.TypeArgs[0], resolve) {
				return false
			}
			if written.Open && !typeExprSatisfies(
				&TypeExpr{Kind: TypeAny},
				declared.TypeArgs[1],
				resolve,
			) {
				return false
			}
			for _, field := range written.Shape {
				if !typeExprSatisfies(field, declared.TypeArgs[1], resolve) {
					return false
				}
			}
			return true
		}
		return false
	case TypeShape:
		// Runtime shape normalization rejects extra and missing required
		// fields but permits optional fields to be absent. An exact shape
		// fact satisfies the annotation when every required field is
		// guaranteed present and every possible field satisfies its bound.
		if written.Kind != TypeShape {
			return false
		}
		if written.Open && !declared.Open {
			return false
		}
		for field, declaredField := range declared.Shape {
			writtenField, ok := written.Shape[field]
			if !ok {
				if shapeFieldOptional(declaredField) && !written.Open {
					continue
				}
				return false
			}
			if shapeFieldOptional(writtenField) && !shapeFieldOptional(declaredField) {
				return false
			}
			if !typeExprSatisfies(shapeFieldValueType(writtenField), shapeFieldValueType(declaredField), resolve) {
				return false
			}
		}
		if !declared.Open {
			for field := range written.Shape {
				if _, ok := declared.Shape[field]; !ok {
					return false
				}
			}
		}
		return true
	case TypeEnum:
		if written.Kind != TypeEnum || resolve == nil {
			return false
		}
		dm, ok := resolve(declared)
		if !ok {
			return false
		}
		wm, ok := resolve(written)
		if !ok {
			return false
		}
		if dm.enum != nil || wm.enum != nil {
			return dm.enum != nil && dm.enum == wm.enum
		}
		if dm.class == nil || wm.class == nil {
			return false
		}
		// Only the same definition admits: classes have no inheritance and a
		// module has no instances, mirroring runtime named normalization.
		return dm.class == wm.class
	}
	return false
}

const maxTypeArmDepth = 8

// typeExprArms flattens a type into its non-union arms; a nullable type
// contributes an extra nil arm. ok is false when the type contains an arm the
// checker cannot reason about (any or unknown).
func typeExprArms(ty *TypeExpr, depth int) ([]*TypeExpr, bool) {
	if ty == nil || depth > maxTypeArmDepth {
		return nil, false
	}
	var arms []*TypeExpr
	switch ty.Kind {
	case TypeAny, TypeUnknown:
		if _, isShapeValue := shapeValuePayload(ty); !isShapeValue {
			return nil, false
		}
		// A first-class shape value is a concrete runtime kind and takes
		// part in disjointness like any other arm.
		arms = append(arms, ty)
	case TypeUnion:
		for _, option := range ty.Union {
			sub, ok := typeExprArms(option, depth+1)
			if !ok {
				return nil, false
			}
			arms = append(arms, sub...)
		}
	default:
		if ty.Nullable {
			clone := *ty
			clone.Nullable = false
			arms = append(arms, &clone)
		} else {
			arms = append(arms, ty)
		}
	}
	if ty.Nullable {
		arms = append(arms, checkTypeNil)
	}
	return arms, true
}

func typeArmPairDisjoint(x, y *TypeExpr, resolve namedTypeResolver) bool {
	// A first-class shape value satisfies no annotation but any (the
	// runtime has no kind that matches it), and overlaps another shape
	// value.
	if _, isShapeValue := shapeValuePayload(x); isShapeValue {
		return shapeValueArmDisjoint(y)
	}
	if _, isShapeValue := shapeValuePayload(y); isShapeValue {
		return shapeValueArmDisjoint(x)
	}
	kx, ky := x.Kind, y.Kind
	if kx == TypeAny || ky == TypeAny || kx == TypeUnknown || ky == TypeUnknown {
		return false
	}
	if kx == TypeEnum || ky == TypeEnum {
		return namedArmPairDisjoint(x, y, resolve)
	}
	numeric := func(kind TypeKind) bool {
		return kind == TypeInt || kind == TypeFloat || kind == TypeNumber
	}
	if numeric(kx) && numeric(ky) {
		if kx == TypeNumber || ky == TypeNumber {
			return false
		}
		return kx != ky
	}
	hashLike := func(kind TypeKind) bool { return kind == TypeHash || kind == TypeShape }
	if hashLike(kx) && hashLike(ky) {
		switch {
		case kx == TypeShape && ky == TypeShape:
			return shapeTypesDisjoint(x, y, resolve)
		case kx == TypeShape && ky == TypeHash:
			return shapeVsTypedHashDisjoint(x, y, resolve)
		case kx == TypeHash && ky == TypeShape:
			return shapeVsTypedHashDisjoint(y, x, resolve)
		}
		return false
	}
	if kx == TypeArray && ky == TypeArray {
		return literalArrayDisjoint(x, y, resolve) || literalArrayDisjoint(y, x, resolve)
	}
	return kx != ky
}

// namedArmPairDisjoint decides disjointness for arm pairs involving a named
// (enum, class, or module) annotation, mirroring runtime named normalization:
// an enum admits its own values and coercible symbols, and a class admits its
// instances. Unresolved names stay conservatively compatible.
func namedArmPairDisjoint(x, y *TypeExpr, resolve namedTypeResolver) bool {
	if resolve == nil {
		return false
	}
	if x.Kind == TypeEnum && y.Kind == TypeEnum {
		mx, ok := resolve(x)
		if !ok {
			return false
		}
		my, ok := resolve(y)
		if !ok {
			return false
		}
		return namedMatchesDisjoint(mx, my)
	}
	named, other := x, y
	if named.Kind != TypeEnum {
		named, other = y, x
	}
	match, ok := resolve(named)
	if !ok {
		return false
	}
	if match.enum != nil {
		// Runtime enum normalization admits the enum's own values and
		// coercible symbols; every other kind is rejected
		// (normalizeEnumValueForDef).
		return other.Kind != TypeSymbol
	}
	// A class or module annotation admits only instances, which no plain
	// runtime kind produces (normalizeClassInstanceForDef).
	return true
}

// namedMatchesDisjoint compares two resolved named definitions. Classes have
// no inheritance and a module has no instances at all, so distinct
// definitions never share a value.
func namedMatchesDisjoint(x, y namedTypeMatch) bool {
	if x.enum != nil || y.enum != nil {
		if x.enum != nil && y.enum != nil {
			return x.enum != y.enum
		}
		// An enum value is never a class instance and vice versa.
		return true
	}
	cx, cy := x.class, y.class
	if cx == nil || cy == nil || cx == cy {
		return false
	}
	return true
}

func shapeValueArmDisjoint(other *TypeExpr) bool {
	if _, isShapeValue := shapeValuePayload(other); isShapeValue {
		return false
	}
	switch other.Kind {
	case TypeAny, TypeUnknown:
		return false
	}
	// Named annotations (TypeEnum) included: runtime named normalization
	// admits only enum values and class instances, never a shape value.
	return true
}

// shapeTypesDisjoint compares two shapes. A required field missing from the
// other shape's key set contradicts it only when that other shape is closed:
// the field must be present, and a closed shape rejects unknown fields, while
// an open shape admits (and leaves unvalidated) the extra. A field required
// on at least one side is witnessed in every common value, so a disjoint
// field type pair contradicts too; a field optional on both sides can be
// absent, satisfying either type.
func shapeTypesDisjoint(x, y *TypeExpr, resolve namedTypeResolver) bool {
	for field, xField := range x.Shape {
		yField, ok := y.Shape[field]
		if !ok {
			if !shapeFieldOptional(xField) && !y.Open {
				return true
			}
			continue
		}
		if shapeFieldOptional(xField) && shapeFieldOptional(yField) {
			continue
		}
		if typeExprsDisjoint(xField, yField, resolve) {
			return true
		}
	}
	for field, yField := range y.Shape {
		if _, ok := x.Shape[field]; !ok && !shapeFieldOptional(yField) && !x.Open {
			return true
		}
	}
	return false
}
