package runtime

// ClassDef represents a user-defined class or module with its methods and
// class-level state. Module declarations (`module Name ... end`) compile to a
// ClassDef with IsModule set: a module is a namespace, so ClassMethods holds
// its `def self.` functions (`Billing.code`) and ClassVars its constants
// (`Billing::LIMIT`), while Methods stays empty. Modules cannot be
// instantiated, and their members are reachable only through the module's own
// name.
type ClassDef struct {
	Name         string
	IsModule     bool
	Methods      map[string]*ScriptFunction
	ClassMethods map[string]*ScriptFunction
	ClassVars    map[string]Value
	// NestedModules lists the short names of module declarations nested in
	// this definition's body. The compiled definitions are registered under
	// the qualified name (Name + "::" + short) and linked into ClassVars per
	// call so Outer::Inner resolves like any other scoped constant.
	NestedModules []string
	Body          []Statement
	env           *Env
	boundMethods  map[*ScriptFunction]*ScriptFunction
	bodyRan       bool
	owner         *Script
}

// Instance represents a runtime instance of a ClassDef with its own instance variables.
type Instance struct {
	Class *ClassDef
	Ivars map[string]Value
}

// EnumDef represents a user-defined enumeration with named members.
type EnumDef struct {
	Name         string
	Members      map[string]*EnumValueDef
	MembersByKey map[string]*EnumValueDef
	Order        []string
	owner        *Script
}

// EnumValueDef represents a single member within an EnumDef.
type EnumValueDef struct {
	Enum   *EnumDef
	Name   string
	Symbol string
	Index  int
}

func enumDefsEqual(left, right *EnumDef) bool {
	if left == right {
		return true
	}
	if left == nil || right == nil {
		return false
	}
	if left.Name != right.Name {
		return false
	}
	if left.owner == nil || right.owner == nil {
		return false
	}
	return left.owner == right.owner
}

func enumValueDefsEqual(left, right *EnumValueDef) bool {
	if left == right {
		return true
	}
	if left == nil || right == nil {
		return false
	}
	return left.Name == right.Name &&
		left.Symbol == right.Symbol &&
		left.Index == right.Index &&
		enumDefsEqual(left.Enum, right.Enum)
}
