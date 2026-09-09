package runtime

import (
	"fmt"
	"strings"
	"unicode"

	"github.com/mgomes/vibescript/internal/ast"
	"github.com/mgomes/vibescript/internal/parser"
)

func (e *Engine) Compile(source string) (*Script, error) {
	script, _, _, err := CompileWithProgram(e, source)
	return script, err
}

// CompileWithProgram compiles source and returns the parsed program from the
// same parser pass. It is intended for internal tooling paths that need both
// diagnostics and navigation data without reparsing clean source.
func CompileWithProgram(e *Engine, source string) (*Script, *ast.Program, []error, error) {
	program, parseErrors, err := parseSource(e, source)
	if err != nil {
		return nil, program, parseErrors, err
	}

	script, err := compileParsed(e, source, program)
	return script, program, nil, err
}

// CompileSnippet compiles source as an inline snippet. Top-level declarations
// remain top-level, while executable top-level statements are moved into a
// synthetic entrypoint function so callers can invoke the snippet through the
// same Script.Call contract as ordinary scripts.
func (e *Engine) CompileSnippet(source, entrypoint string) (*Script, error) {
	script, _, _, err := CompileSnippetWithProgram(e, source, entrypoint)
	return script, err
}

// CompileSnippetWithProgram compiles source as an inline snippet and returns
// the parsed program from the same parser pass. The returned program reflects
// the user's source; only the compiled script receives the synthetic entrypoint.
func CompileSnippetWithProgram(e *Engine, source, entrypoint string) (*Script, *ast.Program, []error, error) {
	if strings.TrimSpace(entrypoint) == "" {
		return nil, nil, nil, fmt.Errorf("snippet entrypoint cannot be empty")
	}

	program, parseErrors, err := parseSource(e, source)
	if err != nil {
		return nil, program, parseErrors, err
	}

	entrypointProgram, deferredClassBodies := snippetEntrypointProgram(program, entrypoint)
	script, err := compileParsed(e, source, entrypointProgram)
	if script != nil {
		script.entrypoint = entrypoint
		script.deferredClassBodies = deferredClassBodies
	}
	return script, program, nil, err
}

func parseSource(e *Engine, source string) (*ast.Program, []error, error) {
	if e.config.MaxSourceBytes > 0 && len(source) > e.config.MaxSourceBytes {
		return nil, nil, fmt.Errorf("source exceeds maximum size (%d > %d bytes)", len(source), e.config.MaxSourceBytes)
	}

	program, parseErrors := parser.Parse(source)
	if len(parseErrors) > 0 {
		return program, parseErrors, combineErrors(parseErrors)
	}

	return program, nil, nil
}

func snippetEntrypointProgram(program *ast.Program, entrypoint string) (*ast.Program, map[string]struct{}) {
	if program == nil {
		return &ast.Program{}, nil
	}

	out := &ast.Program{Statements: make([]ast.Statement, 0, len(program.Statements)+1)}
	body := make([]ast.Statement, 0)
	deferredClassBodies := make(map[string]struct{})
	hasExecutableTopLevel := snippetHasExecutableTopLevel(program)
	pos := Position{Line: 1, Column: 1}
	for _, stmt := range program.Statements {
		switch typed := stmt.(type) {
		case *FunctionStmt, *EnumStmt, *AliasStmt:
			out.Statements = append(out.Statements, typed)
		case *ClassStmt:
			out.Statements = append(out.Statements, typed)
			if len(typed.Body) > 0 && hasExecutableTopLevel {
				if len(body) == 0 {
					pos = typed.Pos()
				}
				body = append(body, typed)
				deferredClassBodies[typed.Name] = struct{}{}
			}
		default:
			if len(body) == 0 {
				pos = stmt.Pos()
			}
			body = append(body, stmt)
		}
	}
	out.Statements = append(out.Statements, &FunctionStmt{Name: entrypoint, Body: body, Position: pos})
	if len(deferredClassBodies) == 0 {
		deferredClassBodies = nil
	}
	return out, deferredClassBodies
}

func snippetHasExecutableTopLevel(program *ast.Program) bool {
	for _, stmt := range program.Statements {
		switch stmt.(type) {
		case *FunctionStmt, *ClassStmt, *EnumStmt, *AliasStmt:
			continue
		default:
			return true
		}
	}
	return false
}

func compileParsed(e *Engine, source string, program *ast.Program) (*Script, error) {
	if program == nil {
		return nil, fmt.Errorf("program is nil")
	}

	functionCount, classCount, enumCount := countTopLevelDeclarations(program.Statements)
	functions := make(map[string]*ScriptFunction, functionCount)
	classes := make(map[string]*ClassDef, classCount)
	classOrder := make([]string, 0, classCount)
	enums := make(map[string]*EnumDef, enumCount)

	if err := checkDirectiveNameCollisions(program.Statements); err != nil {
		return nil, err
	}

	for _, stmt := range program.Statements {
		switch s := stmt.(type) {
		case *FunctionStmt:
			if _, exists := functions[s.Name]; exists {
				return nil, fmt.Errorf("duplicate function %s", s.Name)
			}
			if _, exists := classes[s.Name]; exists {
				return nil, fmt.Errorf("duplicate top-level name %s", s.Name)
			}
			if _, exists := enums[s.Name]; exists {
				return nil, fmt.Errorf("duplicate top-level name %s", s.Name)
			}
			functions[s.Name] = compileFunctionDef(s)
		case *ClassStmt:
			var err error
			classOrder, err = registerClassStmt(s, "", functions, classes, enums, classOrder)
			if err != nil {
				return nil, err
			}
		case *EnumStmt:
			if _, exists := enums[s.Name]; exists {
				return nil, fmt.Errorf("duplicate enum %s", s.Name)
			}
			if _, exists := functions[s.Name]; exists {
				return nil, fmt.Errorf("duplicate top-level name %s", s.Name)
			}
			if _, exists := classes[s.Name]; exists {
				return nil, fmt.Errorf("duplicate top-level name %s", s.Name)
			}
			enumDef, err := compileEnumDef(s)
			if err != nil {
				return nil, err
			}
			enums[s.Name] = enumDef
		case *AliasStmt:
			if s.Method {
				return nil, fmt.Errorf("method alias %s is not valid at the top level", s.NewName)
			}
			if _, exists := functions[s.NewName]; exists {
				return nil, fmt.Errorf("duplicate function %s", s.NewName)
			}
			if _, exists := classes[s.NewName]; exists {
				return nil, fmt.Errorf("duplicate top-level name %s", s.NewName)
			}
			if _, exists := enums[s.NewName]; exists {
				return nil, fmt.Errorf("duplicate top-level name %s", s.NewName)
			}
			target, ok := functions[s.OldName]
			if !ok {
				return nil, fmt.Errorf("alias target function %s is not defined", s.OldName)
			}
			functions[s.NewName] = aliasScriptFunction(target, s.NewName)
		default:
			return nil, fmt.Errorf("unsupported top-level statement %T", stmt)
		}
	}

	script := &Script{engine: e, functions: functions, classes: classes, classOrder: classOrder, enums: enums, source: source}
	for _, name := range classOrder {
		if len(classes[name].Body) > 0 {
			script.classInitializers = append(script.classInitializers, name)
		}
	}
	script.bindFunctionOwnership()
	script.symbolLiterals = collectSymbolLiterals(script)
	return script, nil
}

// checkDirectiveNameCollisions rejects scripts that use a bare word both as a
// class-body directive and as the name of a script function. Before the
// contextual directives existed, bare `public :b` or `protected` in a class
// body were parenless calls to the user's function, so compiling both meanings
// would silently reinterpret existing code. The scan runs before class
// compilation so the collision diagnostic wins over later errors, and it
// consults declaration names directly so a function defined after the class
// still collides.
func checkDirectiveNameCollisions(statements []ast.Statement) error {
	names := make(map[string]struct{})
	for _, stmt := range statements {
		switch s := stmt.(type) {
		case *FunctionStmt:
			names[s.Name] = struct{}{}
		case *AliasStmt:
			if !s.Method {
				names[s.NewName] = struct{}{}
			}
		}
	}
	if len(names) == 0 {
		return nil
	}
	for _, stmt := range statements {
		if classStmt, ok := stmt.(*ClassStmt); ok {
			if err := checkClassDirectiveNameCollisions(classStmt, names); err != nil {
				return err
			}
		}
	}
	return nil
}

func checkClassDirectiveNameCollisions(stmt *ClassStmt, functions map[string]struct{}) error {
	kind := "class"
	if stmt.IsModule {
		kind = "module"
	}
	for _, member := range stmt.Members {
		switch {
		case member.Visibility != nil:
			if err := visibilityDirectiveCollision(member.Visibility.Level, kind, stmt.Name, functions); err != nil {
				return err
			}
		case member.Function != nil:
			if err := visibilityDirectiveCollision(member.Function.Visibility, kind, stmt.Name, functions); err != nil {
				return err
			}
		case member.Property != nil:
			if err := visibilityDirectiveCollision(member.Property.Visibility, kind, stmt.Name, functions); err != nil {
				return err
			}
		}
	}
	for _, nested := range stmt.Modules {
		if err := checkClassDirectiveNameCollisions(nested, functions); err != nil {
			return err
		}
	}
	for _, body := range stmt.Body {
		if nested, ok := body.(*ClassStmt); ok {
			if err := checkClassDirectiveNameCollisions(nested, functions); err != nil {
				return err
			}
		}
	}
	return nil
}

// visibilityDirectiveCollision reports the collision for the visibility
// levels spelled with a contextual bare word. `private` is a lexer keyword,
// so a function of that name can never exist and the level never collides.
func visibilityDirectiveCollision(level, kind, name string, functions map[string]struct{}) error {
	if level != ast.VisibilityPublic && level != ast.VisibilityProtected {
		return nil
	}
	if _, ok := functions[level]; !ok {
		return nil
	}
	return fmt.Errorf("%s in %s %s is a visibility directive, but this script also defines a function named %s; rename the function, or call it with parentheses (%s(:name)), which stays a call", level, kind, name, level, level)
}

func countTopLevelDeclarations(statements []ast.Statement) (functions, classes, enums int) {
	for _, stmt := range statements {
		switch stmt.(type) {
		case *FunctionStmt:
			functions++
		case *ClassStmt:
			classes++
		case *EnumStmt:
			enums++
		}
	}
	return functions, classes, enums
}

// registerClassStmt compiles a class or module declaration into the classes
// map. Nested module declarations register first, under their qualified name
// (Outer::Inner), so the enclosing body can reference them; the enclosing
// definition records their short names for per-call constant linking. The
// updated class order (nested definitions before their parent, so nested
// bodies initialize first) is returned.
func registerClassStmt(stmt *ClassStmt, qualifier string, functions map[string]*ScriptFunction, classes map[string]*ClassDef, enums map[string]*EnumDef, classOrder []string) ([]string, error) {
	name := stmt.Name
	if qualifier != "" {
		name = qualifier + "::" + stmt.Name
	}
	kind := "class"
	if stmt.IsModule {
		kind = "module"
	}
	if _, exists := classes[name]; exists {
		return nil, fmt.Errorf("duplicate %s %s", kind, name)
	}
	if _, exists := functions[name]; exists {
		return nil, fmt.Errorf("duplicate top-level name %s", name)
	}
	if _, exists := enums[name]; exists {
		return nil, fmt.Errorf("duplicate top-level name %s", name)
	}
	var err error
	for _, nested := range stmt.Modules {
		classOrder, err = registerClassStmt(nested, name, functions, classes, enums, classOrder)
		if err != nil {
			return nil, err
		}
	}
	classDef, err := compileClassDef(stmt)
	if err != nil {
		return nil, err
	}
	classDef.Name = name
	for _, nested := range stmt.Modules {
		classDef.NestedModules = append(classDef.NestedModules, nested.Name)
	}
	classes[name] = classDef
	return append(classOrder, name), nil
}

// compileClassDef compiles a class or module declaration.
func compileClassDef(stmt *ClassStmt) (*ClassDef, error) {
	classDef := &ClassDef{
		Name:         stmt.Name,
		IsModule:     stmt.IsModule,
		Methods:      make(map[string]*ScriptFunction),
		ClassMethods: make(map[string]*ScriptFunction),
		ClassVars:    make(map[string]Value),
		Body:         stmt.Body,
	}
	if len(stmt.Members) == 0 {
		compiled, err := compileClassDefLegacyOrder(stmt, classDef)
		if err != nil {
			return nil, err
		}
		resolvePropertyParamContracts(compiled)
		return compiled, nil
	}
	// Section directives (`private` on its own line) apply to every
	// definition that follows them until another section directive, matching
	// Ruby's sticky visibility sections. An inline modifier on a single
	// definition overrides the section for that definition only.
	sectionVisibility := ""
	for _, member := range stmt.Members {
		if member.Property != nil {
			compileClassProperty(classDef, *member.Property, sectionVisibility)
			continue
		}
		if member.Function != nil {
			compileClassMethod(classDef, member.Function, sectionVisibility)
			continue
		}
		if member.Alias != nil {
			if err := compileClassAlias(classDef, member.Alias, stmt.Name); err != nil {
				return nil, err
			}
			continue
		}
		if member.Visibility != nil {
			if len(member.Visibility.Names) == 0 {
				sectionVisibility = member.Visibility.Level
				continue
			}
			if err := applyNamedVisibility(classDef, member.Visibility, stmt.Name); err != nil {
				return nil, err
			}
		}
	}
	resolvePropertyParamContracts(classDef)
	return classDef, nil
}

// applyNamedVisibility applies a named directive (`private :hidden, :other`)
// to methods that are already defined, mirroring Ruby's retroactive
// symbol-argument visibility calls. Instance methods are consulted first,
// then class methods.
func applyNamedVisibility(classDef *ClassDef, decl *ast.VisibilityDecl, className string) error {
	for _, name := range decl.Names {
		if fn, ok := classDef.Methods[name]; ok {
			setFunctionVisibility(fn, decl.Level)
			continue
		}
		if fn, ok := classDef.ClassMethods[name]; ok {
			setFunctionVisibility(fn, decl.Level)
			continue
		}
		return fmt.Errorf("%s target method %s is not defined on class %s", decl.Level, name, className)
	}
	return nil
}

func setFunctionVisibility(fn *ScriptFunction, level string) {
	fn.Private = level == ast.VisibilityPrivate
	fn.Protected = level == ast.VisibilityProtected
}

func compileClassDefLegacyOrder(stmt *ClassStmt, classDef *ClassDef) (*ClassDef, error) {
	for _, prop := range stmt.Properties {
		compileClassProperty(classDef, prop, "")
	}
	for _, fn := range stmt.Methods {
		compileClassMethod(classDef, fn, "")
	}
	for _, fn := range stmt.ClassMethods {
		compileClassMethod(classDef, fn, "")
	}
	for _, alias := range stmt.Aliases {
		if err := compileClassAlias(classDef, alias, stmt.Name); err != nil {
			return nil, err
		}
	}
	return classDef, nil
}

func compileClassProperty(classDef *ClassDef, prop PropertyDecl, sectionVisibility string) {
	visibility := prop.Visibility
	if visibility == "" {
		visibility = sectionVisibility
	}
	for _, entry := range prop.Names {
		name := entry.Name
		if prop.Kind == "property" || prop.Kind == "getter" {
			getter := &ScriptFunction{
				Name:         name,
				ReturnTy:     entry.Type,
				Body:         []Statement{&ReturnStmt{Value: &IvarExpr{Name: name, Position: prop.Position}, Position: prop.Position}},
				Pos:          prop.Position,
				Accessor:     functionAccessorGetter,
				AccessorName: name,
			}
			getter.reuseCallEnv = functionCanReuseCallEnv(getter)
			setFunctionVisibility(getter, visibility)
			classDef.Methods[name] = getter
		}
		if prop.Kind == "property" || prop.Kind == "setter" {
			setter := &ScriptFunction{
				Name: name + "=",
				Params: []Param{{
					Name: "value",
					Type: entry.Type,
				}},
				Body: []Statement{
					&ReturnStmt{Value: &IvarExpr{Name: name, Position: prop.Position}, Position: prop.Position},
				},
				Pos:          prop.Position,
				Accessor:     functionAccessorSetter,
				AccessorName: name,
			}
			setter.reuseCallEnv = functionCanReuseCallEnv(setter)
			setFunctionVisibility(setter, visibility)
			classDef.Methods[name+"="] = setter
		}
	}
}

func compileClassMethod(classDef *ClassDef, fn *FunctionStmt, sectionVisibility string) {
	compiled := compileFunctionDef(fn)
	visibility := fn.Visibility
	if visibility == "" {
		if fn.Private {
			visibility = ast.VisibilityPrivate
		} else {
			visibility = sectionVisibility
		}
	}
	setFunctionVisibility(compiled, visibility)
	if fn.Name == "initialize" {
		compiled.Private = true
	}
	if fn.IsClassMethod {
		classDef.ClassMethods[fn.Name] = compiled
		return
	}
	classDef.Methods[fn.Name] = compiled
}

func compileClassAlias(classDef *ClassDef, alias *AliasStmt, className string) error {
	target, ok := classDef.Methods[alias.OldName]
	if !ok {
		return fmt.Errorf("alias target method %s is not defined on class %s", alias.OldName, className)
	}
	classDef.Methods[alias.NewName] = aliasScriptFunction(target, alias.NewName)
	return nil
}

func aliasScriptFunction(fn *ScriptFunction, name string) *ScriptFunction {
	alias := *fn
	alias.Name = name
	return &alias
}

func compileEnumDef(stmt *EnumStmt) (*EnumDef, error) {
	if strings.HasSuffix(stmt.Name, "?") {
		return nil, fmt.Errorf("enum name %s must not end with '?'", stmt.Name)
	}
	if typ, _ := ast.ResolveType(stmt.Name); typ != TypeUnknown {
		return nil, fmt.Errorf("enum name %s conflicts with built-in type", stmt.Name)
	}
	enumDef := &EnumDef{
		Name:         stmt.Name,
		Members:      make(map[string]*EnumValueDef, len(stmt.Members)),
		MembersByKey: make(map[string]*EnumValueDef, len(stmt.Members)),
		Order:        make([]string, 0, len(stmt.Members)),
	}
	for i, member := range stmt.Members {
		symbol := enumMemberSymbol(member.Name)
		if _, exists := enumDef.Members[member.Name]; exists {
			return nil, fmt.Errorf("duplicate enum member %s.%s", stmt.Name, member.Name)
		}
		if prior, exists := enumDef.MembersByKey[symbol]; exists {
			return nil, fmt.Errorf("enum %s member %s conflicts with %s after symbol normalization", stmt.Name, member.Name, prior.Name)
		}
		value := &EnumValueDef{
			Enum:   enumDef,
			Name:   member.Name,
			Symbol: symbol,
			Index:  i,
		}
		enumDef.Members[member.Name] = value
		enumDef.MembersByKey[symbol] = value
		enumDef.Order = append(enumDef.Order, member.Name)
	}
	return enumDef, nil
}

func enumMemberSymbol(name string) string {
	if name == "" {
		return ""
	}

	var b strings.Builder
	runes := []rune(name)
	lastUnderscore := false
	for i, r := range runes {
		if r == '_' {
			if b.Len() > 0 && !lastUnderscore {
				b.WriteRune('_')
				lastUnderscore = true
			}
			continue
		}
		if unicode.IsUpper(r) {
			if i > 0 {
				prev := runes[i-1]
				var next rune
				if i+1 < len(runes) {
					next = runes[i+1]
				}
				if prev != '_' && (unicode.IsLower(prev) || unicode.IsDigit(prev) || (next != 0 && unicode.IsLower(next))) {
					b.WriteRune('_')
				}
			}
			b.WriteRune(unicode.ToLower(r))
			lastUnderscore = false
			continue
		}
		b.WriteRune(unicode.ToLower(r))
		lastUnderscore = false
	}
	return b.String()
}

func compileFunctionDef(stmt *FunctionStmt) *ScriptFunction {
	fn := &ScriptFunction{
		Name:     stmt.Name,
		Params:   stmt.Params,
		ReturnTy: stmt.ReturnTy,
		Body:     stmt.Body,
		Pos:      stmt.Pos(),
		Exported: stmt.Exported,
		Private:  stmt.Private,
	}
	fn.reuseCallEnv = functionCanReuseCallEnv(fn)
	return fn
}

func combineErrors(errs []error) error {
	if len(errs) == 1 {
		return errs[0]
	}
	return &combinedError{errs: errs}
}

// combinedError aggregates multiple errors while keeping the individual
// errors reachable through Unwrap, so structured data (such as parse
// positions) survives aggregation instead of being flattened to text.
type combinedError struct {
	errs []error
}

func (e *combinedError) Error() string {
	msgs := make([]string, len(e.errs))
	for i, err := range e.errs {
		msgs[i] = err.Error()
	}
	return strings.Join(msgs, "\n\n")
}

func (e *combinedError) Unwrap() []error {
	return e.errs
}
