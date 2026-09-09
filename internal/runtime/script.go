package runtime

import (
	"context"
	"fmt"
	"slices"

	"github.com/mgomes/vibescript/internal/ast"
)

func (s *Script) Call(ctx context.Context, name string, args []Value, opts CallOptions) (Value, error) {
	// Composite globals bind lazily, and the lazy binding stores the per-call
	// rebinder, which would force it onto the heap for every call if the
	// store were reachable from this function. Route those calls through the
	// variant that already pays that cost, keeping the common no-globals /
	// scalar-globals path allocation-free at this layer.
	if globalsBindLazily(opts.Globals) {
		return s.callWithLazyGlobals(ctx, name, args, opts)
	}
	if ctx == nil {
		ctx = context.Background()
	}
	if err := ctx.Err(); err != nil {
		return NewNil(), err
	}

	_, ok := s.functions[name]
	if !ok {
		candidates := functionSuggestionCandidates(s.functions)
		return NewNil(), fmt.Errorf("function %s not found%s", name, didYouMean(name, candidates))
	}

	rootCapacity := len(opts.Globals) + len(opts.Capabilities)*2
	root := newCallRoot(s, rootCapacity)
	s.engine.attachBuiltins(root, 1)

	fn, ok := materializeCallFunction(root, name)
	if !ok {
		return NewNil(), fmt.Errorf("function %s not found", name)
	}

	// Bodies still initialize in declaration order, and their per-call state
	// exists before adapters bind so setup quota refusals precede host code.
	classes := materializeClassInitializers(s, root)
	rebinder := newCallFunctionRebinder(s, root, classes, nil)
	rebinder.inboundDataFast = scanInboundCallValues(args, opts.Keywords)

	exec := newExecutionForCall(s, ctx, root, opts)
	rebinder.exec = exec
	defer exec.releaseBaseWalkCache()

	// Refuse over-quota setup before any adapter runs host code.
	if err := exec.checkMemory(); err != nil {
		return NewNil(), exec.wrapError(err, fn.Pos)
	}

	if err := bindCapabilitiesForCall(exec, root, rebinder, opts.Capabilities); err != nil {
		return NewNil(), err
	}

	if err := bindGlobalsForCall(exec, root, rebinder, opts.Globals); err != nil {
		return NewNil(), err
	}

	if err := exec.checkContext(); err != nil {
		return NewNil(), err
	}
	if err := exec.checkMemory(); err != nil {
		return NewNil(), exec.wrapError(err, fn.Pos)
	}

	if err := initializeClassBodiesForCall(exec, root, classes, s.classInitializers, deferredClassBodiesForFunction(fn, s.deferredClassBodies)); err != nil {
		return NewNil(), err
	}
	if err := exec.checkContext(); err != nil {
		return NewNil(), err
	}

	// The invocation token opens before argument binding so a block built by a
	// default-argument expression homes to this entry invocation.
	token := exec.pushReturnToken()
	callEnv, err := prepareCallEnvForFunction(exec, root, rebinder, fn, args, opts.Keywords)
	if err != nil {
		exec.popReturnToken()
		if sig := matchNonLocalReturn(err, token); sig != nil {
			// A default-argument block returned during binding: that is the
			// entry function's return value, validated like any other.
			val, finishErr := finishFunctionForCall(exec, fn, sig.value)
			if finishErr != nil {
				return NewNil(), finishErr
			}
			if valueNeedsHostClone(val) {
				return cloneValueForHost(val), nil
			}
			return val, nil
		}
		return NewNil(), exec.wrapError(err, fn.Pos)
	}

	val, err := executeFunctionForCall(exec, fn, callEnv, token)
	exec.popReturnToken()
	if err != nil {
		return NewNil(), err
	}
	if err := exec.checkContext(); err != nil {
		return NewNil(), err
	}
	if valueNeedsHostClone(val) {
		return cloneValueForHost(val), nil
	}
	return val, nil
}

// callWithLazyGlobals keeps lazily bound composite host globals off the public
// Call hot path, so the per-call rebinder stays stack-allocated for calls that
// bind no deferred globals.
func (s *Script) callWithLazyGlobals(ctx context.Context, name string, args []Value, opts CallOptions) (Value, error) {
	if ctx == nil {
		ctx = context.Background()
	}
	if err := ctx.Err(); err != nil {
		return NewNil(), err
	}

	_, ok := s.functions[name]
	if !ok {
		candidates := functionSuggestionCandidates(s.functions)
		return NewNil(), fmt.Errorf("function %s not found%s", name, didYouMean(name, candidates))
	}

	rootCapacity := len(opts.Globals) + len(opts.Capabilities)*2
	root := newCallRoot(s, rootCapacity)
	s.engine.attachBuiltins(root, 1)

	fn, ok := materializeCallFunction(root, name)
	if !ok {
		return NewNil(), fmt.Errorf("function %s not found", name)
	}

	// Bodies still initialize in declaration order, and their per-call state
	// exists before adapters bind so setup quota refusals precede host code.
	classes := materializeClassInitializers(s, root)
	rebinder := newCallFunctionRebinder(s, root, classes, nil)
	root.declarations.retainForDeferredGlobals(rebinder)
	rebinder.inboundDataFast = scanInboundCallValues(args, opts.Keywords)

	exec := newExecutionForCall(s, ctx, root, opts)
	rebinder.exec = exec
	defer exec.releaseBaseWalkCache()

	// Refuse before any host code runs; see Call for why.
	if err := exec.checkMemory(); err != nil {
		return NewNil(), exec.wrapError(err, fn.Pos)
	}

	if err := bindCapabilitiesForCall(exec, root, rebinder, opts.Capabilities); err != nil {
		return NewNil(), err
	}

	if err := bindGlobalsForCallLazy(exec, root, rebinder, opts.Globals); err != nil {
		return NewNil(), err
	}

	if err := exec.checkContext(); err != nil {
		return NewNil(), err
	}
	if err := exec.checkMemory(); err != nil {
		return NewNil(), exec.wrapError(err, fn.Pos)
	}

	if err := initializeClassBodiesForCall(exec, root, classes, s.classInitializers, deferredClassBodiesForFunction(fn, s.deferredClassBodies)); err != nil {
		return NewNil(), err
	}
	if err := exec.checkContext(); err != nil {
		return NewNil(), err
	}

	// The invocation token opens before argument binding so a block built by a
	// default-argument expression homes to this entry invocation.
	token := exec.pushReturnToken()
	callEnv, err := prepareCallEnvForFunction(exec, root, rebinder, fn, args, opts.Keywords)
	if err != nil {
		exec.popReturnToken()
		if sig := matchNonLocalReturn(err, token); sig != nil {
			// A default-argument block returned during binding: that is the
			// entry function's return value, validated like any other.
			val, finishErr := finishFunctionForCall(exec, fn, sig.value)
			if finishErr != nil {
				return NewNil(), finishErr
			}
			if valueNeedsHostClone(val) {
				return cloneValueForHost(val), nil
			}
			return val, nil
		}
		return NewNil(), exec.wrapError(err, fn.Pos)
	}

	val, err := executeFunctionForCall(exec, fn, callEnv, token)
	exec.popReturnToken()
	if err != nil {
		return NewNil(), err
	}
	if err := exec.checkContext(); err != nil {
		return NewNil(), err
	}
	if valueNeedsHostClone(val) {
		return cloneValueForHost(val), nil
	}
	return val, nil
}

func deferredClassBodiesForFunction(fn *ScriptFunction, deferred map[string]struct{}) map[string]struct{} {
	if len(deferred) == 0 || fn == nil {
		return nil
	}
	for _, stmt := range fn.Body {
		classStmt, ok := stmt.(*ClassStmt)
		if !ok {
			continue
		}
		if _, ok := deferred[classStmt.Name]; ok {
			return deferred
		}
	}
	return nil
}

// Function looks up a compiled function by name.
func (s *Script) Function(name string) (*ScriptFunction, bool) {
	fn, ok := s.functions[name]
	if !ok {
		return nil, false
	}
	return cloneFunctionForSnapshot(fn, nil), true
}

// Functions returns compiled functions in deterministic name order.
func (s *Script) Functions() []*ScriptFunction {
	names := make([]string, 0, len(s.functions))
	for name := range s.functions {
		names = append(names, name)
	}
	slices.Sort(names)
	out := make([]*ScriptFunction, 0, len(names))
	for _, name := range names {
		out = append(out, cloneFunctionForSnapshot(s.functions[name], nil))
	}
	return out
}

// Classes returns compiled classes in deterministic name order.
func (s *Script) Classes() []*ClassDef {
	names := make([]string, 0, len(s.classes))
	for name := range s.classes {
		names = append(names, name)
	}
	slices.Sort(names)
	out := make([]*ClassDef, 0, len(names))
	// One memo for the whole snapshot, not one per class. A module's methods are
	// copied into every including class by shallow copy, and re-resolving the
	// contract against the including class lands on the module's own node again,
	// so all of them share one contract. A per-class memo would copy a wide
	// module property once per include and put the O(classes * type size) blowup
	// back, just spelled with `include` instead of with methods (#16).
	propertyTypes := ast.NewTypeExprMemo()
	for _, name := range names {
		out = append(out, cloneClassForSnapshot(s.classes[name], propertyTypes))
	}
	return out
}

// Enums returns compiled enums in deterministic name order.
func (s *Script) Enums() []*EnumDef {
	names := make([]string, 0, len(s.enums))
	for name := range s.enums {
		names = append(names, name)
	}
	slices.Sort(names)
	out := make([]*EnumDef, 0, len(names))
	for _, name := range names {
		out = append(out, cloneEnumForSnapshot(s.enums[name]))
	}
	return out
}

func (s *Script) bindFunctionOwnership() {
	for _, fn := range s.functions {
		fn.owner = s
	}
	for _, classDef := range s.classes {
		classDef.owner = s
		for _, fn := range classDef.Methods {
			fn.owner = s
		}
		for _, fn := range classDef.ClassMethods {
			fn.owner = s
		}
	}
	for _, enumDef := range s.enums {
		enumDef.owner = s
	}
}

func materializeCallFunction(root *Env, name string) (*ScriptFunction, bool) {
	val, ok := root.Get(name)
	if !ok {
		return nil, false
	}
	fn := valueFunction(val)
	return fn, fn != nil
}

func cloneClassesForCall(classes map[string]*ClassDef, env *Env) map[string]*ClassDef {
	if len(classes) == 0 {
		return nil
	}
	cloned := make(map[string]*ClassDef, len(classes))
	for name, classDef := range classes {
		classClone := &ClassDef{
			Name:          classDef.Name,
			IsModule:      classDef.IsModule,
			Methods:       make(map[string]*ScriptFunction, len(classDef.Methods)),
			ClassMethods:  make(map[string]*ScriptFunction, len(classDef.ClassMethods)),
			ClassVars:     make(map[string]Value),
			NestedModules: classDef.NestedModules,
			Body:          classDef.Body,
			owner:         classDef.owner,
		}
		for methodName, method := range classDef.Methods {
			classClone.Methods[methodName] = cloneFunctionForEnv(method, env)
		}
		for methodName, method := range classDef.ClassMethods {
			classClone.ClassMethods[methodName] = cloneFunctionForEnv(method, env)
		}
		cloned[name] = classClone
	}
	// Link nested module declarations into their parent's constants so
	// Outer::Inner resolves through the scoped-constant path. Linking runs
	// after the clone loop so parent and nested definitions reference this
	// call's clones regardless of map iteration order.
	for _, classClone := range cloned {
		for _, short := range classClone.NestedModules {
			if nested, ok := cloned[classClone.Name+"::"+short]; ok {
				classClone.ClassVars[short] = NewClass(nested)
			}
		}
	}
	return cloned
}

func cloneEnumsForCall(enums map[string]*EnumDef) map[string]*EnumDef {
	if len(enums) == 0 {
		return nil
	}
	cloned := make(map[string]*EnumDef, len(enums))
	for name, enumDef := range enums {
		cloned[name] = cloneEnumDef(enumDef, enumDef.owner)
	}
	return cloned
}

// cloneFunctionForSnapshot detaches a function for a caller that asked the
// script to describe itself. propertyTypes carries the property contracts the
// surrounding snapshot has already copied so a class's methods share one copy
// per property rather than one per parameter; it may be nil for a lone
// function, which has nothing to share with.
func cloneFunctionForSnapshot(fn *ScriptFunction, propertyTypes ast.TypeExprMemo) *ScriptFunction {
	if fn == nil {
		return nil
	}
	clone := *fn
	clone.Params = ast.CloneParamsWithTypeMemo(fn.Params, propertyTypes)
	clone.ReturnTy = ast.CloneTypeExprWithMemo(fn.ReturnTy, propertyTypes)
	clone.Body = cloneStatements(fn.Body)
	clone.Env = nil
	return &clone
}

// cloneClassForSnapshot detaches a class for a caller that asked the script to
// describe itself. propertyTypes spans the whole snapshot so contracts shared
// between classes — which is what a mixed-in property is — stay one copy.
func cloneClassForSnapshot(classDef *ClassDef, propertyTypes ast.TypeExprMemo) *ClassDef {
	if classDef == nil {
		return nil
	}
	classClone := &ClassDef{
		Name:          classDef.Name,
		IsModule:      classDef.IsModule,
		Methods:       make(map[string]*ScriptFunction, len(classDef.Methods)),
		ClassMethods:  make(map[string]*ScriptFunction, len(classDef.ClassMethods)),
		ClassVars:     cloneBuiltinMap(classDef.ClassVars),
		NestedModules: cloneStringSlice(classDef.NestedModules),
		Body:          cloneStatements(classDef.Body),
	}
	for methodName, method := range classDef.Methods {
		classClone.Methods[methodName] = cloneFunctionForSnapshot(method, propertyTypes)
	}
	for methodName, method := range classDef.ClassMethods {
		classClone.ClassMethods[methodName] = cloneFunctionForSnapshot(method, propertyTypes)
	}
	return classClone
}

func cloneEnumForSnapshot(enumDef *EnumDef) *EnumDef {
	return cloneEnumDef(enumDef, nil)
}

func cloneEnumDef(enumDef *EnumDef, owner *Script) *EnumDef {
	if enumDef == nil {
		return nil
	}
	clone := &EnumDef{
		Name:         enumDef.Name,
		Members:      make(map[string]*EnumValueDef, len(enumDef.Members)),
		MembersByKey: make(map[string]*EnumValueDef, len(enumDef.MembersByKey)),
		Order:        append([]string(nil), enumDef.Order...),
		owner:        owner,
	}
	for memberName, member := range enumDef.Members {
		if member == nil {
			continue
		}
		memberClone := &EnumValueDef{
			Enum:   clone,
			Name:   member.Name,
			Symbol: member.Symbol,
			Index:  member.Index,
		}
		clone.Members[memberName] = memberClone
		clone.MembersByKey[member.Symbol] = memberClone
	}
	return clone
}

func functionSuggestionCandidates(functions map[string]*ScriptFunction) []string {
	candidates := make([]string, 0, min(len(functions), suggestMaxCandidates))
	for name := range functions {
		if len(candidates) == suggestMaxCandidates {
			break
		}
		candidates = append(candidates, name)
	}
	return candidates
}
