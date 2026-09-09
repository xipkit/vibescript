package runtime

import (
	"maps"
	"strings"
	"weak"
)

// Compiled declaration tables are immutable. Only declarations a call reads
// acquire environment bindings, and only classes with executable bodies need
// state before the entrypoint runs. Snapshots detach metadata before exposing a
// previously unread declaration through an environment returned to the host.
type callDeclarations struct {
	script *Script
	// Bindings and live values own declaration state. Weak memo entries keep
	// aliases canonical without retaining overwritten classes or enums.
	classes  map[string]weak.Pointer[ClassDef]
	enums    map[string]weak.Pointer[EnumDef]
	rebinder weak.Pointer[callFunctionRebinder]
	snapshot bool
}

func newCallRoot(script *Script, capacity int) *Env {
	state := &struct {
		env          Env
		declarations callDeclarations
	}{declarations: callDeclarations{script: script}}
	root := &state.env
	root.declarations = &state.declarations
	if capacity > inlineEnvBindingCapacity {
		root.values = make(map[string]Value, capacity)
	}
	return root
}

func (d *callDeclarations) cloneShallow() *callDeclarations {
	if d == nil {
		return nil
	}
	clone := *d
	clone.classes = maps.Clone(d.classes)
	clone.enums = maps.Clone(d.enums)
	return &clone
}

func (e *Env) hasDeclaration(name string) bool {
	if e.declarations == nil {
		return false
	}
	script := e.declarations.script
	if _, ok := script.functions[name]; ok {
		return true
	}
	if _, ok := script.classes[name]; ok {
		return true
	}
	_, ok := script.enums[name]
	return ok
}

func (e *Env) materializeDeclaration(name string) (Value, bool) {
	e.assertNotPoisoned()
	d := e.declarations
	if d == nil {
		return Value{}, false
	}
	if fn, ok := d.script.functions[name]; ok {
		if d.snapshot {
			fn = cloneFunctionForSnapshot(fn, nil)
			fn.Env = e
		} else {
			fn = cloneFunctionForEnv(fn, e)
		}
		val := NewFunction(fn)
		e.DefineStatic(name, val)
		return val, true
	}
	if classDef, ok := d.class(e, name); ok {
		val := NewClass(classDef)
		return val, true
	}
	if enumDef, ok := d.enum(e, name); ok {
		val := NewEnum(enumDef)
		return val, true
	}
	return Value{}, false
}

func (d *callDeclarations) class(env *Env, name string) (*ClassDef, bool) {
	if classDef := d.classes[name].Value(); classDef != nil {
		return classDef, true
	}
	compiled, ok := d.script.classes[name]
	if !ok {
		return nil, false
	}
	classDef := &ClassDef{
		Name:          compiled.Name,
		IsModule:      compiled.IsModule,
		Methods:       compiled.Methods,
		ClassMethods:  compiled.ClassMethods,
		ClassVars:     make(map[string]Value),
		NestedModules: compiled.NestedModules,
		Body:          compiled.Body,
		env:           env,
		owner:         compiled.owner,
	}
	if d.snapshot {
		classDef = cloneClassForSnapshot(compiled, nil)
		classDef.env = env
		classDef.owner = compiled.owner
		for _, fn := range classDef.Methods {
			fn.Env = env
		}
		for _, fn := range classDef.ClassMethods {
			fn.Env = env
		}
	}
	if d.classes == nil {
		d.classes = make(map[string]weak.Pointer[ClassDef])
	}
	d.classes[name] = weak.Make(classDef)
	if rebinder := d.rebinder.Value(); rebinder != nil {
		if rebinder.callClasses == nil {
			rebinder.callClasses = make(map[string]*ClassDef)
		}
		rebinder.callClasses[name] = classDef
	}
	if !env.hasDynamic(name) {
		if _, bound := env.statics[name]; !bound {
			env.Define(name, NewClass(classDef))
		}
	}
	// A qualified root binding can be overwritten independently of the
	// containing namespace, whose nested constant must retain the same state.
	if namespace, _, nested := strings.Cut(name, "::"); nested {
		d.class(env, namespace)
	}
	for _, short := range classDef.NestedModules {
		if nested, ok := d.class(env, name+"::"+short); ok {
			classDef.ClassVars[short] = NewClass(nested)
		}
	}
	return classDef, true
}

func (d *callDeclarations) enum(env *Env, name string) (*EnumDef, bool) {
	if enumDef := d.enums[name].Value(); enumDef != nil {
		return enumDef, true
	}
	compiled, ok := d.script.enums[name]
	if !ok {
		return nil, false
	}
	enumDef := cloneEnumDef(compiled, compiled.owner)
	if d.enums == nil {
		d.enums = make(map[string]weak.Pointer[EnumDef])
	}
	d.enums[name] = weak.Make(enumDef)
	if rebinder := d.rebinder.Value(); rebinder != nil {
		if rebinder.callEnums == nil {
			rebinder.callEnums = make(map[string]*EnumDef)
		}
		rebinder.callEnums[name] = enumDef
	}
	if !env.hasDynamic(name) {
		if _, bound := env.statics[name]; !bound {
			env.DefineStatic(name, NewEnum(enumDef))
		}
	}
	return enumDef, true
}

// Deferred globals can rebind a declaration after its root binding changes.
// Their rebinder owns that state while an unread lazy binding keeps it alive;
// the declaration cache itself must not extend either lifetime.
func (d *callDeclarations) retainForDeferredGlobals(rebinder *callFunctionRebinder) {
	d.rebinder = weak.Make(rebinder)
	for name, ref := range d.classes {
		if classDef := ref.Value(); classDef != nil {
			if rebinder.callClasses == nil {
				rebinder.callClasses = make(map[string]*ClassDef)
			}
			rebinder.callClasses[name] = classDef
		}
	}
	for name, ref := range d.enums {
		if enumDef := ref.Value(); enumDef != nil {
			if rebinder.callEnums == nil {
				rebinder.callEnums = make(map[string]*EnumDef)
			}
			rebinder.callEnums[name] = enumDef
		}
	}
}

func materializeClassInitializers(script *Script, root *Env) map[string]*ClassDef {
	if len(script.classInitializers) == 0 {
		return nil
	}
	classes := make(map[string]*ClassDef, len(script.classInitializers))
	for _, name := range script.classInitializers {
		if classDef, ok := root.declarations.class(root, name); ok {
			classes[name] = classDef
		}
	}
	return classes
}

// Unbound methods share their immutable compiled definitions until invoked.
// Binding through the receiver keeps required modules on their own lexical root.
func bindMethodForCall(fn *ScriptFunction, receiver Value) *ScriptFunction {
	var classDef *ClassDef
	switch receiver.Kind() {
	case KindClass:
		classDef = valueClass(receiver)
	case KindInstance:
		classDef = valueInstance(receiver).Class
	}
	if classDef == nil {
		return fn
	}
	return classDef.bindMethod(fn)
}

func (c *ClassDef) bindMethod(fn *ScriptFunction) *ScriptFunction {
	if fn.Env != nil || c.env == nil {
		return fn
	}
	if bound, ok := c.boundMethods[fn]; ok {
		return bound
	}
	bound := cloneFunctionForEnv(fn, c.env)
	if c.boundMethods == nil {
		c.boundMethods = make(map[*ScriptFunction]*ScriptFunction)
	}
	c.boundMethods[fn] = bound
	return bound
}

func (e *Env) rangeUnboundTypesFold(name string, visit func(string, Value)) {
	if e.declarations == nil {
		return
	}
	visitName := func(key string) {
		if key == name || !strings.EqualFold(key, name) || e.hasDynamic(key) {
			return
		}
		if _, exists := e.statics[key]; exists {
			return
		}
		if val, ok := e.materializeDeclaration(key); ok {
			visit(key, val)
		}
	}
	for key := range e.declarations.script.classes {
		visitName(key)
	}
	for key := range e.declarations.script.enums {
		visitName(key)
	}
}
