package runtime

import (
	"errors"
	"fmt"
	"maps"
	"reflect"
	"slices"
	"strings"
	"unsafe"
)

const (
	maxNormalizeDepth          = 64
	normalizationCheckInterval = 64
)

type typeContext struct {
	owner    *Script
	env      *Env
	fallback *Env
	exec     *Execution
	depth    int
}

func normalizeValueForType(val Value, ty *TypeExpr, ctx typeContext) (Value, error) {
	if err := ctx.checkSandbox(); err != nil {
		return NewNil(), err
	}
	if ty == nil {
		return val, nil
	}
	if ctx.depth == 0 {
		if err := validateTypeExprResolved(ty, ctx); err != nil {
			return NewNil(), err
		}
	}
	if ctx.depth >= maxNormalizeDepth {
		return NewNil(), guardLimitErrorf("type normalization exceeded maximum depth")
	}
	ctx.depth++
	if nullableNilCanBypassResolution(ty, val) {
		return val, nil
	}
	switch ty.Kind {
	case TypeAny:
		return val, nil
	case TypeInt:
		if val.Kind() == KindInt {
			return val, nil
		}
	case TypeFloat:
		if val.Kind() == KindFloat {
			return val, nil
		}
	case TypeNumber:
		if val.Kind() == KindInt || val.Kind() == KindFloat {
			return val, nil
		}
	case TypeString:
		if val.Kind() == KindString {
			return val, nil
		}
	case TypeBool:
		if val.Kind() == KindBool {
			return val, nil
		}
	case TypeNil:
		if val.Kind() == KindNil {
			return val, nil
		}
	case TypeDuration:
		if val.Kind() == KindDuration {
			return val, nil
		}
	case TypeTime:
		if val.Kind() == KindTime {
			return val, nil
		}
	case TypeMoney:
		if val.Kind() == KindMoney {
			return val, nil
		}
	case TypeFunction:
		if isCallableValue(val) {
			return val, nil
		}
	case TypeArray:
		return normalizeArrayForType(val, ty, ctx)
	case TypeHash:
		return normalizeHashForType(val, ty, ctx)
	case TypeRange:
		if val.Kind() == KindRange {
			return val, nil
		}
	case TypeSymbol:
		if val.Kind() == KindSymbol {
			return val, nil
		}
	case TypeShape:
		return normalizeShapeForType(val, ty, ctx)
	case TypeUnion:
		for _, option := range unionNormalizationOrder(ty.Union) {
			normalized, err := normalizeValueForType(val, option, ctx)
			if err == nil {
				return normalized, nil
			}
			var mismatch *typeMismatchError
			if !errorAsTypeMismatch(err, &mismatch) {
				return NewNil(), err
			}
		}
	case TypeEnum:
		if ty.Nullable && val.Kind() == KindNil {
			if err := ensureNamedTypeExists(ty, ctx); err != nil {
				return NewNil(), err
			}
			return val, nil
		}
		return normalizeNamedForType(val, ty, ctx)
	case TypeUnknown:
		return NewNil(), unknownTypeError(ty.Name)
	}

	return NewNil(), &typeMismatchError{
		Expected: formatTypeExpr(ty),
		Actual:   formatValueTypeExpr(val),
	}
}

func unionNormalizationOrder(options []*TypeExpr) []*TypeExpr {
	if len(options) < 2 {
		return options
	}
	ordered := make([]*TypeExpr, 0, len(options))
	for _, option := range options {
		if option.Kind != TypeAny {
			ordered = append(ordered, option)
		}
	}
	if len(ordered) == len(options) {
		return options
	}
	for _, option := range options {
		if option.Kind == TypeAny {
			ordered = append(ordered, option)
		}
	}
	return ordered
}

func (ctx typeContext) checkSandbox(extra ...Value) error {
	if ctx.exec == nil {
		return nil
	}
	if err := ctx.exec.checkContext(); err != nil {
		return err
	}
	if len(extra) > 0 {
		return ctx.exec.checkMemoryWith(extra...)
	}
	return nil
}

func (ctx typeContext) checkSandboxEvery(index int, extra ...Value) error {
	if index%normalizationCheckInterval != 0 {
		return nil
	}
	return ctx.checkSandbox(extra...)
}

func (ctx typeContext) reserveArraySlots(source Value, count int) error {
	if ctx.exec == nil {
		return nil
	}
	return newArrayBuildAccumulator(ctx.exec, source, nil, nil, NewNil()).reserveSlots(count)
}

func (ctx typeContext) reserveHashEntries(source Value, count int) error {
	if ctx.exec == nil {
		return nil
	}
	return ctx.exec.checkProjectedHashBytes(count, source, nil, nil, NewNil())
}

func (ctx typeContext) normalizedMap(source Value, entries map[string]Value) (map[string]Value, error) {
	if err := ctx.reserveHashEntries(source, len(entries)); err != nil {
		return nil, err
	}
	out := make(map[string]Value, len(entries))
	maps.Copy(out, entries)
	return out, nil
}

func nullableNilCanBypassResolution(ty *TypeExpr, val Value) bool {
	return ty.Nullable && val.Kind() == KindNil && ty.Kind != TypeUnknown && ty.Kind != TypeEnum
}

func isNormalizationLimitError(err error) bool {
	return classifyRuntimeErrorType(err) == runtimeErrorTypeLimit
}

func normalizeArrayForType(val Value, ty *TypeExpr, ctx typeContext) (Value, error) {
	if val.Kind() != KindArray {
		return NewNil(), &typeMismatchError{Expected: formatTypeExpr(ty), Actual: formatValueTypeExpr(val)}
	}
	if len(ty.TypeArgs) == 0 {
		return val, nil
	}
	if len(ty.TypeArgs) != 1 {
		return NewNil(), fmt.Errorf("array type expects exactly 1 type argument")
	}

	items := val.Array()
	var out []Value
	for i, item := range items {
		if err := ctx.checkSandboxEvery(i); err != nil {
			return NewNil(), err
		}
		normalized, err := normalizeValueForType(item, ty.TypeArgs[0], ctx)
		if err != nil {
			var mismatch *typeMismatchError
			if errorAsTypeMismatch(err, &mismatch) {
				return NewNil(), &typeMismatchError{Expected: formatTypeExpr(ty), Actual: formatValueTypeExpr(val)}
			}
			return NewNil(), err
		}
		if !sameNormalizedValue(normalized, item) {
			if out == nil {
				if err := ctx.reserveArraySlots(val, len(items)); err != nil {
					return NewNil(), err
				}
				out = make([]Value, len(items))
				copy(out, items[:i])
			}
		}
		if out != nil {
			out[i] = normalized
		}
	}
	if out == nil {
		return val, nil
	}
	result := NewArray(out)
	if err := ctx.checkSandbox(result); err != nil {
		return NewNil(), err
	}
	return result, nil
}

func normalizeHashForType(val Value, ty *TypeExpr, ctx typeContext) (Value, error) {
	if val.Kind() != KindHash && val.Kind() != KindObject {
		return NewNil(), &typeMismatchError{Expected: formatTypeExpr(ty), Actual: formatValueTypeExpr(val)}
	}
	if len(ty.TypeArgs) == 0 {
		return val, nil
	}
	if len(ty.TypeArgs) != 2 {
		return NewNil(), fmt.Errorf("hash type expects exactly 2 type arguments")
	}

	keyType := ty.TypeArgs[0]
	valueType := ty.TypeArgs[1]
	if val.Kind() == KindHash {
		return normalizeHashEntriesForType(val, ty, keyType, valueType, ctx)
	}
	entries := val.HashEntryMap()
	var out map[string]Value

	if decided, keyMatches := typeAllowsStringHashKey(keyType); decided {
		if !keyMatches {
			return NewNil(), &typeMismatchError{Expected: formatTypeExpr(ty), Actual: formatValueTypeExpr(val)}
		}
	} else {
		i := 0
		for key := range entries {
			if err := ctx.checkSandboxEvery(i); err != nil {
				return NewNil(), err
			}
			i++
			if _, err := normalizeValueForType(NewString(key), keyType, ctx); err != nil {
				var mismatch *typeMismatchError
				if errorAsTypeMismatch(err, &mismatch) {
					return NewNil(), &typeMismatchError{Expected: formatTypeExpr(ty), Actual: formatValueTypeExpr(val)}
				}
				return NewNil(), err
			}
		}
	}

	i := 0
	for key, item := range entries {
		if err := ctx.checkSandboxEvery(i); err != nil {
			return NewNil(), err
		}
		i++
		normalized, err := normalizeValueForType(item, valueType, ctx)
		if err != nil {
			var mismatch *typeMismatchError
			if errorAsTypeMismatch(err, &mismatch) {
				return NewNil(), &typeMismatchError{Expected: formatTypeExpr(ty), Actual: formatValueTypeExpr(val)}
			}
			return NewNil(), err
		}
		if !sameNormalizedValue(normalized, item) {
			if out == nil {
				var err error
				out, err = ctx.normalizedMap(val, entries)
				if err != nil {
					return NewNil(), err
				}
			}
		}
		if out != nil {
			out[key] = normalized
		}
	}

	if out == nil {
		return val, nil
	}
	result := NewObject(out)
	if val.Kind() != KindObject {
		result = NewHash(out)
	}
	if err := ctx.checkSandbox(result); err != nil {
		return NewNil(), err
	}
	return result, nil
}

func normalizeHashEntriesForType(val Value, ty, keyType, valueType *TypeExpr, ctx typeContext) (Value, error) {
	var entryBuf [smallHashKeyBufferSize]HashEntry
	entries := val.HashEntriesInto(entryBuf[:])

	var out Value
	outInitialized := false
	initOut := func(processed int) error {
		if outInitialized {
			return nil
		}
		if err := ctx.reserveHashEntries(val, len(entries)); err != nil {
			return err
		}
		out = NewHashWithCapacity(len(entries))
		for _, entry := range entries[:processed] {
			if err := hashSet(out, entry.Key, entry.Value); err != nil {
				return err
			}
		}
		outInitialized = true
		return nil
	}

	// Every key is a string, so the key type is decided once for the whole hash
	// rather than per entry. A `symbol` key type describes the same keyspace, so
	// typeAllowsStringHashKey accepts both spellings.
	if decided, keyMatches := typeAllowsStringHashKey(keyType); decided {
		if !keyMatches {
			return NewNil(), &typeMismatchError{Expected: formatTypeExpr(ty), Actual: formatValueTypeExpr(val)}
		}
	} else {
		for i, entry := range entries {
			if err := ctx.checkSandboxEvery(i); err != nil {
				return NewNil(), err
			}
			if _, err := normalizeValueForType(entry.Key, keyType, ctx); err != nil {
				var mismatch *typeMismatchError
				if errorAsTypeMismatch(err, &mismatch) {
					return NewNil(), &typeMismatchError{Expected: formatTypeExpr(ty), Actual: formatValueTypeExpr(val)}
				}
				return NewNil(), err
			}
		}
	}

	for i, entry := range entries {
		if err := ctx.checkSandboxEvery(i); err != nil {
			return NewNil(), err
		}
		normalizedValue, err := normalizeValueForType(entry.Value, valueType, ctx)
		if err != nil {
			var mismatch *typeMismatchError
			if errorAsTypeMismatch(err, &mismatch) {
				return NewNil(), &typeMismatchError{Expected: formatTypeExpr(ty), Actual: formatValueTypeExpr(val)}
			}
			return NewNil(), err
		}
		if !sameNormalizedValue(normalizedValue, entry.Value) {
			if err := initOut(i); err != nil {
				return NewNil(), err
			}
		}
		if outInitialized {
			if err := hashSet(out, entry.Key, normalizedValue); err != nil {
				return NewNil(), err
			}
		}
	}

	if !outInitialized {
		return val, nil
	}
	if err := ctx.checkSandbox(out); err != nil {
		return NewNil(), err
	}
	return out, nil
}

func normalizeShapeForType(val Value, ty *TypeExpr, ctx typeContext) (Value, error) {
	if val.Kind() != KindHash && val.Kind() != KindObject {
		return NewNil(), &typeMismatchError{Expected: formatTypeExpr(ty), Actual: formatValueTypeExpr(val)}
	}
	if val.Kind() == KindObject {
		return normalizeStringKeyShapeForType(val, ty, ctx)
	}

	var entryBuf [smallHashKeyBufferSize]HashEntry
	entries := val.HashEntriesInto(entryBuf[:])
	if !ty.Open && len(entries) > len(ty.Shape) {
		return NewNil(), &typeMismatchError{Expected: formatTypeExpr(ty), Actual: formatValueTypeExpr(val)}
	}

	var normalizedEntries []HashEntry
	seenFields := make(map[string]struct{}, len(entries))
	for i, entry := range entries {
		if err := ctx.checkSandboxEvery(i); err != nil {
			return NewNil(), err
		}
		field := entry.Key.String()
		fieldType, declared := ty.Shape[field]
		if !declared {
			if !ty.Open {
				return NewNil(), &typeMismatchError{Expected: formatTypeExpr(ty), Actual: formatValueTypeExpr(val)}
			}
			// An undeclared field of an open shape passes through unvalidated.
			if normalizedEntries != nil {
				normalizedEntries[i] = entry
			}
			continue
		}
		seenFields[field] = struct{}{}
		normalized, err := normalizeValueForType(entry.Value, fieldType, ctx)
		if err != nil {
			var mismatch *typeMismatchError
			if errorAsTypeMismatch(err, &mismatch) {
				return NewNil(), &typeMismatchError{Expected: formatTypeExpr(ty), Actual: formatValueTypeExpr(val)}
			}
			return NewNil(), err
		}
		if !sameNormalizedValue(normalized, entry.Value) {
			if normalizedEntries == nil {
				normalizedEntries = make([]HashEntry, len(entries))
				copy(normalizedEntries, entries[:i])
			}
		}
		if normalizedEntries != nil {
			normalizedEntries[i] = HashEntry{Key: entry.Key, Value: normalized}
		}
	}
	if len(seenFields) != len(ty.Shape) && shapeMissingRequiredField(ty, func(field string) bool {
		_, ok := seenFields[field]
		return ok
	}) {
		return NewNil(), &typeMismatchError{Expected: formatTypeExpr(ty), Actual: formatValueTypeExpr(val)}
	}
	if normalizedEntries == nil {
		return val, nil
	}
	result := NewHashWithCapacity(len(normalizedEntries))
	for _, entry := range normalizedEntries {
		if err := result.HashSet(entry.Key, entry.Value); err != nil {
			return NewNil(), err
		}
	}
	if err := ctx.checkSandbox(result); err != nil {
		return NewNil(), err
	}
	return result, nil
}

func normalizeStringKeyShapeForType(val Value, ty *TypeExpr, ctx typeContext) (Value, error) {
	entries := val.HashEntryMap()
	// An open shape's entry count says nothing about coverage (extras may
	// stand in for missing declared fields), so it always scans.
	if (ty.Open || len(entries) != len(ty.Shape)) && shapeMissingRequiredField(ty, func(field string) bool {
		_, ok := entries[field]
		return ok
	}) {
		return NewNil(), &typeMismatchError{Expected: formatTypeExpr(ty), Actual: formatValueTypeExpr(val)}
	}

	changed := false
	i := 0
	for key, item := range entries {
		if err := ctx.checkSandboxEvery(i); err != nil {
			return NewNil(), err
		}
		i++
		fieldType, declared := ty.Shape[key]
		if !declared {
			if !ty.Open {
				return NewNil(), &typeMismatchError{Expected: formatTypeExpr(ty), Actual: formatValueTypeExpr(val)}
			}
			// An undeclared field of an open shape passes through unvalidated.
			continue
		}
		normalized, err := normalizeValueForType(item, fieldType, ctx)
		if err != nil {
			var mismatch *typeMismatchError
			if errorAsTypeMismatch(err, &mismatch) {
				return NewNil(), &typeMismatchError{Expected: formatTypeExpr(ty), Actual: formatValueTypeExpr(val)}
			}
			return NewNil(), err
		}
		if !sameNormalizedValue(normalized, item) {
			changed = true
			break
		}
	}
	if !changed {
		return val, nil
	}

	out := make(map[string]Value, len(entries))
	i = 0
	for key, item := range entries {
		if err := ctx.checkSandboxEvery(i); err != nil {
			return NewNil(), err
		}
		i++
		fieldType, declared := ty.Shape[key]
		if !declared {
			// Reachable only for open shapes: a closed shape already failed
			// on the undeclared key in the detection pass above.
			out[key] = item
			continue
		}
		normalized, err := normalizeValueForType(item, fieldType, ctx)
		if err != nil {
			var mismatch *typeMismatchError
			if errorAsTypeMismatch(err, &mismatch) {
				return NewNil(), &typeMismatchError{Expected: formatTypeExpr(ty), Actual: formatValueTypeExpr(val)}
			}
			return NewNil(), err
		}
		out[key] = normalized
	}
	result := NewHash(out)
	if val.Kind() == KindObject {
		result = NewObject(out)
	}
	if err := ctx.checkSandbox(result); err != nil {
		return NewNil(), err
	}
	return result, nil
}

func normalizeNamedForType(val Value, ty *TypeExpr, ctx typeContext) (Value, error) {
	match, ok, err := lookupNamedTypeForType(ty, ctx)
	if err != nil {
		return NewNil(), err
	}
	if ok {
		if match.enum != nil {
			return normalizeEnumValueForDef(val, ty, match.enum)
		}
		return normalizeClassInstanceForDef(val, ty, match.class)
	}
	return NewNil(), unknownTypeError(ty.Name)
}

func normalizeEnumValueForDef(val Value, ty *TypeExpr, enumDef *EnumDef) (Value, error) {
	switch val.Kind() {
	case KindEnumValue:
		if member := valueEnumValue(val); member != nil && member.Enum == enumDef {
			return val, nil
		}
	case KindSymbol:
		if member, ok := enumDef.MembersByKey[val.String()]; ok {
			return NewEnumValue(member), nil
		}
	}

	return NewNil(), &typeMismatchError{
		Expected: formatTypeExpr(ty),
		Actual:   formatValueTypeExpr(val),
	}
}

// normalizeClassInstanceForDef accepts an instance of the named class. A
// module names a namespace rather than a type, so a module contract accepts
// nothing.
func normalizeClassInstanceForDef(val Value, ty *TypeExpr, classDef *ClassDef) (Value, error) {
	if val.Kind() == KindInstance {
		inst := valueInstance(val)
		if inst != nil && inst.Class == classDef {
			return val, nil
		}
	}
	return NewNil(), &typeMismatchError{
		Expected: formatTypeExpr(ty),
		Actual:   formatValueTypeExpr(val),
	}
}

// unknownTypeError spells the unknown-type diagnostic for a named annotation.
// The function type died with first-class callables (ADR-006), so its
// spelling gets a teaching error naming the replacement instead of reading as
// a typo.
func unknownTypeError(name string) error {
	if strings.EqualFold(strings.TrimSuffix(name, "?"), "function") {
		return errors.New("the function type was removed with first-class callables; executable code is not a value. Accept a block attached to the call instead")
	}
	return fmt.Errorf("unknown type %s", name)
}

func ensureNamedTypeExists(ty *TypeExpr, ctx typeContext) error {
	if _, ok, err := lookupNamedTypeForType(ty, ctx); err != nil || ok {
		return err
	}
	return unknownTypeError(ty.Name)
}

type namedTypeMatch struct {
	enum  *EnumDef
	class *ClassDef
}

func lookupNamedTypeForType(ty *TypeExpr, ctx typeContext) (namedTypeMatch, bool, error) {
	match, ok, err := lookupNamedTypeExact(ty, ctx)
	if err != nil {
		return namedTypeMatch{}, false, err
	}
	if ok {
		return match, true, nil
	}
	return lookupNamedTypeFold(ty, ctx)
}

func lookupNamedTypeExact(ty *TypeExpr, ctx typeContext) (namedTypeMatch, bool, error) {
	if ty == nil {
		return namedTypeMatch{}, false, fmt.Errorf("unknown type")
	}
	if ty.Kind != TypeEnum {
		return namedTypeMatch{}, false, unknownTypeError(ty.Name)
	}
	if qualifier, enumName, qualified := strings.Cut(ty.Name, "."); qualified {
		// A qualified name (Module.Enum) resolves through the module namespace
		// only: a miss is a hard unknown-type error rather than a fold-lookup
		// fallback, since dotted names are never stored as plain bindings.
		enumDef, ok, err := lookupQualifiedEnumInEnv(ctx.env, qualifier, enumName)
		if err != nil {
			return namedTypeMatch{}, false, err
		}
		if !ok && ctx.fallback != ctx.env {
			enumDef, ok, err = lookupQualifiedEnumInEnv(ctx.fallback, qualifier, enumName)
			if err != nil {
				return namedTypeMatch{}, false, err
			}
		}
		if ok {
			return namedTypeMatch{enum: enumDef}, true, nil
		}
		return namedTypeMatch{}, false, unknownTypeError(ty.Name)
	}
	match, ok := lookupNamedTypeInEnvExact(ctx.env, ty.Name)
	if ok {
		return match, true, nil
	}
	if ctx.fallback != ctx.env {
		match, ok = lookupNamedTypeInEnvExact(ctx.fallback, ty.Name)
		if ok {
			return match, true, nil
		}
	}
	match = lookupNamedTypeDefExact(ctx.owner, ty.Name)
	return match, match.enum != nil || match.class != nil, nil
}

func lookupNamedTypeFold(ty *TypeExpr, ctx typeContext) (namedTypeMatch, bool, error) {
	if ty == nil {
		return namedTypeMatch{}, false, fmt.Errorf("unknown type")
	}
	if ty.Kind != TypeEnum {
		return namedTypeMatch{}, false, unknownTypeError(ty.Name)
	}
	match, ok, err := lookupNamedTypeInEnvFold(ctx.env, ty.Name)
	if err != nil || ok {
		return match, ok, err
	}
	if ctx.fallback != ctx.env {
		match, ok, err = lookupNamedTypeInEnvFold(ctx.fallback, ty.Name)
		if err != nil || ok {
			return match, ok, err
		}
	}
	return lookupNamedTypeDefFold(ctx.owner, ty.Name)
}

func validateTypeExprResolved(ty *TypeExpr, ctx typeContext) error {
	if ty == nil {
		return nil
	}

	switch ty.Kind {
	case TypeUnknown:
		return unknownTypeError(ty.Name)
	case TypeEnum:
		return ensureNamedTypeExists(ty, ctx)
	}

	for _, arg := range ty.TypeArgs {
		if err := validateTypeExprResolved(arg, ctx); err != nil {
			return err
		}
	}
	for _, option := range ty.Union {
		if err := validateTypeExprResolved(option, ctx); err != nil {
			return err
		}
	}
	for _, field := range ty.Shape {
		if err := validateTypeExprResolved(field, ctx); err != nil {
			return err
		}
	}
	return nil
}

func lookupQualifiedEnumInEnv(env *Env, qualifier, enumName string) (*EnumDef, bool, error) {
	for scope := env; scope != nil; scope = scope.parent {
		val, ok := scope.getOwn(qualifier)
		if !ok {
			continue
		}
		enumDef, ok, err := enumFromNamespaceValue(val, enumName)
		if err != nil || ok {
			return enumDef, ok, err
		}
	}
	return nil, false, nil
}

func enumFromNamespaceValue(val Value, enumName string) (*EnumDef, bool, error) {
	if val.Kind() != KindObject {
		return nil, false, nil
	}
	entries := val.HashEntryMap()
	if enumVal, ok := entries[enumName]; ok && enumVal.Kind() == KindEnum {
		return valueEnum(enumVal), true, nil
	}

	var match *EnumDef
	matches := make([]string, 0, 2)
	for name, enumVal := range entries {
		if enumVal.Kind() != KindEnum || !strings.EqualFold(name, enumName) {
			continue
		}
		matches = append(matches, name)
		if match == nil {
			match = valueEnum(enumVal)
			continue
		}
		if match != valueEnum(enumVal) {
			return nil, false, ambiguousEnumTypeError(enumName, matches)
		}
	}
	if match != nil {
		return match, true, nil
	}
	return nil, false, nil
}

func lookupEnumDefExact(owner *Script, name string) (*EnumDef, bool) {
	if owner == nil || len(owner.enums) == 0 {
		return nil, false
	}
	enumDef, ok := owner.enums[name]
	return enumDef, ok
}

func lookupEnumInEnv(env *Env, name string) (*EnumDef, bool, error) {
	for scope := env; scope != nil; scope = scope.parent {
		if enumDef, ok, err := lookupEnumInScope(scope, name); err != nil {
			return nil, false, err
		} else if ok {
			return enumDef, true, nil
		}
	}
	return nil, false, nil
}

func lookupNamedTypeInEnvFold(env *Env, name string) (namedTypeMatch, bool, error) {
	for scope := env; scope != nil; scope = scope.parent {
		if match, ok, err := lookupNamedTypeInScopeFold(scope, name); err != nil || ok {
			return match, ok, err
		}
	}
	return namedTypeMatch{}, false, nil
}

func lookupNamedTypeInEnvExact(env *Env, name string) (namedTypeMatch, bool) {
	for scope := env; scope != nil; scope = scope.parent {
		if match, ok := lookupNamedTypeInScopeExact(scope, name); ok {
			return match, true
		}
	}
	return namedTypeMatch{}, false
}

func lookupNamedTypeInScopeExact(scope *Env, name string) (namedTypeMatch, bool) {
	val, ok := scope.getOwn(name)
	if !ok {
		return namedTypeMatch{}, false
	}
	switch val.Kind() {
	case KindEnum:
		return namedTypeMatch{enum: valueEnum(val)}, true
	case KindClass:
		return namedTypeMatch{class: valueClass(val)}, true
	default:
		return namedTypeMatch{}, false
	}
}

// lookupEnumInScope considers a scope's dynamic and static bindings as
// one namespace: an exact name wins outright (a name lives in only one
// of the two maps), while case-insensitive matches accumulate across
// both maps so a collision between, say, a script-defined static enum
// and a host-supplied dynamic one still reports ambiguity.
func lookupEnumInScope(scope *Env, name string) (*EnumDef, bool, error) {
	if val, ok := scope.getOwn(name); ok && val.Kind() == KindEnum {
		return valueEnum(val), true, nil
	}

	var match *EnumDef
	matches := make([]string, 0, 2)
	var scanErr error
	scan := func(key string, val Value) {
		if scanErr != nil || key == name || !strings.EqualFold(key, name) || val.Kind() != KindEnum {
			return
		}
		matches = append(matches, key)
		if match == nil {
			match = valueEnum(val)
			return
		}
		if match != valueEnum(val) {
			scanErr = ambiguousEnumTypeError(name, matches)
		}
	}
	scope.rangeDynamicBindings(scan)
	for key, val := range scope.statics {
		scan(key, val)
	}
	scope.rangeUnboundTypesFold(name, scan)
	if scanErr != nil {
		return nil, false, scanErr
	}
	if match != nil {
		return match, true, nil
	}
	return nil, false, nil
}

func lookupNamedTypeInScopeFold(scope *Env, name string) (namedTypeMatch, bool, error) {
	var match namedTypeMatch
	enumMatches := make([]string, 0, 2)
	classMatches := make([]string, 0, 2)
	var scanErr error
	scan := func(key string, val Value) {
		if scanErr != nil || key == name || !strings.EqualFold(key, name) {
			return
		}
		switch val.Kind() {
		case KindEnum:
			enumMatches = append(enumMatches, key)
			enumDef := valueEnum(val)
			if match.enum == nil {
				match.enum = enumDef
				return
			}
			if match.enum != enumDef {
				scanErr = ambiguousEnumTypeError(name, enumMatches)
			}
		case KindClass:
			classMatches = append(classMatches, key)
			classDef := valueClass(val)
			if match.class == nil {
				match.class = classDef
				return
			}
			if match.class != classDef {
				scanErr = ambiguousClassTypeError(name, classMatches)
			}
		}
	}
	scope.rangeDynamicBindings(scan)
	for key, val := range scope.statics {
		scan(key, val)
	}
	scope.rangeUnboundTypesFold(name, scan)
	return resolveNamedTypeFoldMatch(name, match, enumMatches, classMatches, scanErr)
}

func lookupClassTypeExact(ty *TypeExpr, ctx typeContext) (*ClassDef, bool, error) {
	if ty == nil {
		return nil, false, fmt.Errorf("unknown type")
	}
	if ty.Kind != TypeEnum {
		return nil, false, unknownTypeError(ty.Name)
	}
	classDef, ok := lookupClassInEnvExact(ctx.env, ty.Name)
	if ok {
		return classDef, true, nil
	}
	if ctx.fallback != ctx.env {
		classDef, ok = lookupClassInEnvExact(ctx.fallback, ty.Name)
		if ok {
			return classDef, true, nil
		}
	}
	classDef, ok = lookupClassDefExact(ctx.owner, ty.Name)
	if ok {
		return classDef, true, nil
	}
	return nil, false, nil
}

func lookupClassDefExact(owner *Script, name string) (*ClassDef, bool) {
	if owner == nil || len(owner.classes) == 0 {
		return nil, false
	}
	classDef, ok := owner.classes[name]
	return classDef, ok
}

func lookupNamedTypeDefExact(owner *Script, name string) namedTypeMatch {
	if classDef, ok := lookupClassDefExact(owner, name); ok {
		return namedTypeMatch{class: classDef}
	}
	if enumDef, ok := lookupEnumDefExact(owner, name); ok {
		return namedTypeMatch{enum: enumDef}
	}
	return namedTypeMatch{}
}

func lookupClassInEnvExact(env *Env, name string) (*ClassDef, bool) {
	for scope := env; scope != nil; scope = scope.parent {
		if val, ok := scope.getOwn(name); ok && val.Kind() == KindClass {
			return valueClass(val), true
		}
	}
	return nil, false
}

func lookupNamedTypeDefFold(owner *Script, name string) (namedTypeMatch, bool, error) {
	if owner == nil || len(owner.enums)+len(owner.classes) == 0 {
		return namedTypeMatch{}, false, nil
	}

	var match namedTypeMatch
	enumMatches := make([]string, 0, 2)
	classMatches := make([]string, 0, 2)
	for enumName, enumDef := range owner.enums {
		if enumName == name || !strings.EqualFold(enumName, name) {
			continue
		}
		enumMatches = append(enumMatches, enumName)
		if match.enum == nil {
			match.enum = enumDef
			continue
		}
		if match.enum != enumDef {
			return namedTypeMatch{}, false, ambiguousEnumTypeError(name, enumMatches)
		}
	}
	for className, classDef := range owner.classes {
		if className == name || !strings.EqualFold(className, name) {
			continue
		}
		classMatches = append(classMatches, className)
		if match.class == nil {
			match.class = classDef
			continue
		}
		if match.class != classDef {
			return namedTypeMatch{}, false, ambiguousClassTypeError(name, classMatches)
		}
	}
	return resolveNamedTypeFoldMatch(name, match, enumMatches, classMatches, nil)
}

func resolveNamedTypeFoldMatch(name string, match namedTypeMatch, enumMatches, classMatches []string, err error) (namedTypeMatch, bool, error) {
	if err != nil {
		return namedTypeMatch{}, false, err
	}
	if match.enum != nil && match.class != nil {
		return namedTypeMatch{}, false, ambiguousNamedTypeError(name, enumMatches, classMatches)
	}
	if match.enum != nil || match.class != nil {
		return match, true, nil
	}
	return namedTypeMatch{}, false, nil
}

func ambiguousClassTypeError(name string, matches []string) error {
	slices.Sort(matches)
	return fmt.Errorf("ambiguous class type %s matches %s", name, strings.Join(matches, ", "))
}

func ambiguousEnumTypeError(name string, matches []string) error {
	slices.Sort(matches)
	return fmt.Errorf("ambiguous enum type %s matches %s", name, strings.Join(matches, ", "))
}

func ambiguousNamedTypeError(name string, enumMatches, classMatches []string) error {
	slices.Sort(enumMatches)
	slices.Sort(classMatches)
	matches := make([]string, 0, len(enumMatches)+len(classMatches))
	for _, enumName := range enumMatches {
		matches = append(matches, "enum "+enumName)
	}
	for _, className := range classMatches {
		matches = append(matches, "class "+className)
	}
	return fmt.Errorf("ambiguous type %s matches %s", name, strings.Join(matches, ", "))
}

func errorAsTypeMismatch(err error, target **typeMismatchError) bool {
	if err == nil {
		return false
	}
	return errors.As(err, target)
}

func sameNormalizedValue(left, right Value) bool {
	if left.Kind() != right.Kind() {
		return false
	}

	switch left.Kind() {
	case KindNil:
		return true
	case KindBool:
		return left.Bool() == right.Bool()
	case KindInt:
		return left.Int() == right.Int()
	case KindFloat:
		return left.Float() == right.Float()
	case KindString, KindSymbol:
		return left.String() == right.String()
	case KindMoney:
		return left.Money() == right.Money()
	case KindDuration:
		return left.Duration() == right.Duration()
	case KindTime:
		return left.Time().Equal(right.Time())
	case KindRange:
		return left.Range() == right.Range()
	case KindRegex:
		leftRegex := left.Regex()
		rightRegex := right.Regex()
		return leftRegex.Source == rightRegex.Source && leftRegex.Flags == rightRegex.Flags
	case KindArray:
		leftArr := left.Array()
		rightArr := right.Array()
		return len(leftArr) == len(rightArr) &&
			cap(leftArr) == cap(rightArr) &&
			sliceDataPointer(leftArr) == sliceDataPointer(rightArr)
	case KindHash:
		return hashIdentity(left) == hashIdentity(right)
	case KindObject:
		return reflect.ValueOf(left.HashEntryMap()).Pointer() == reflect.ValueOf(right.HashEntryMap()).Pointer()
	case KindFunction:
		return valueFunction(left) == valueFunction(right)
	case KindBuiltin:
		return valueBuiltin(left) == valueBuiltin(right)
	case KindBlock:
		return valueBlock(left) == valueBlock(right)
	case KindClass:
		return valueClass(left) == valueClass(right)
	case KindInstance:
		return valueInstance(left) == valueInstance(right)
	case KindEnum:
		return valueEnum(left) == valueEnum(right)
	case KindEnumValue:
		return valueEnumValue(left) == valueEnumValue(right)
	default:
		return left.Equal(right)
	}
}

func sliceDataPointer(items []Value) uintptr {
	if len(items) == 0 && cap(items) == 0 {
		return 0
	}
	return uintptr(unsafe.Pointer(unsafe.SliceData(items)))
}
