package runtime

import (
	"context"

	"github.com/mgomes/vibescript/internal/ast"
)

// This file implements static name resolution for check mode: bare value and
// function references that cannot resolve to any binding the checker can see
// are reported as "undefined variable NAME", matching the runtime error the
// reference would raise. The resolution model deliberately over-approximates
// the set of defined names (locals from any branch, module exports from any
// statically resolvable require in the script, host globals, builtins), so a
// warning is only produced when the reference is guaranteed to be unresolved
// at runtime. Names that only become knowable dynamically (a require with a
// non-literal module name) suppress the check for the whole script.
//
// The pass runs only in check mode (the CheckWarnings* entry points, i.e.
// vibes run -check). It is deliberately absent from vibes analyze: analyze is
// used over documentation fragments and host-embedded scripts whose free
// names (capability objects such as db or ctx, host builtins, prose-level
// examples) are legal at runtime, so name resolution there cannot avoid
// false positives. Hosts that inject CallOptions.Globals or capabilities must
// check with the same options they later pass to Call; those names then
// resolve through the check roots and are never reported.

// CheckOrderIndependentWarnings returns the whole-script check warnings that
// hold regardless of which function runs first or what state earlier calls
// established: undefined value/function names and typed block parameters
// contradicted by literal receivers. Hosts that check functions outside any
// entrypoint execution order use it where state-sensitive warnings (for
// example a type annotation that resolves only after a require in the
// entrypoint runs) would misfire.
//
// The pass checks against empty CallOptions, so hosts that inject Globals or
// Capabilities should prefer CheckWarningsWithOptions with their real
// options: free names that only those options bind are reported here.
func (s *Script) CheckOrderIndependentWarnings() []CheckWarning {
	return s.checkWarningsMode(context.Background(), CallOptions{}, checkTarget{}, true)
}

// checkNameFacts caches script-wide facts the identifier resolution pass
// needs: the names any statically resolvable require in the script could
// bind, and whether a dynamic require defeats static resolution entirely.
type checkNameFacts struct {
	requireExports map[string]struct{}
	// suppress disables undefined-name warnings for the whole script when a
	// require call's module cannot be statically resolved: such a call can
	// bind arbitrary export names at runtime.
	suppress bool
}

func (c *scriptChecker) checkIdentifierResolved(function string, ident *Identifier) {
	if ident == nil || c.selfScope {
		return
	}
	name := ident.Name
	if name == "" || name == "self" || name == blockGivenName {
		return
	}
	if c.scopeHas(name) || c.localNameUnionHas(name) {
		return
	}
	if c.envHasVisibleBinding(name) {
		return
	}
	if c.hostGlobalShadows(name) || c.hostBuiltinOverrides(name) {
		return
	}
	facts := c.nameFacts()
	if facts.suppress {
		return
	}
	if _, ok := facts.requireExports[name]; ok {
		return
	}
	c.addOrderIndependent(function, ident.Pos(), "undefined variable %s%s", name, c.topLevelBindingHint(name))
}

// envHasVisibleBinding reports whether name is bound anywhere in the check
// roots, including the frozen builtin proto env and host globals defined into
// the root.
func (c *scriptChecker) envHasVisibleBinding(name string) bool {
	for _, root := range []*Env{c.runtimeTypeRoot, c.typeRoot} {
		for scope := root; scope != nil; scope = scope.parent {
			if scope.hasOwnBinding(name) {
				return true
			}
		}
	}
	return false
}

// staticRaiseErrorClass reports whether a raise statement's class expression
// is a bare canonical error type name (raise RuntimeError, "boom"). The
// runtime resolves that spelling without an env binding, so the identifier
// must not be treated as an ordinary value reference.
func staticRaiseErrorClass(stmt *RaiseStmt) bool {
	if stmt == nil || stmt.Message == nil {
		return false
	}
	ident, ok := stmt.Value.(*Identifier)
	if !ok || !isConstantIdentifier(ident.Name) {
		return false
	}
	_, ok = ast.CanonicalRuntimeErrorType(ident.Name)
	return ok
}

// pushFunctionNameScope enters the local-name scope of a checked function:
// it records every name the function body can bind (parameters plus the
// union of assignment targets in any branch, mirroring the runtime's local
// predeclaration) and switches the self-scope flag for methods, where bare
// identifiers fall through to implicit self member lookup that the checker
// cannot resolve statically.
func (c *scriptChecker) pushFunctionNameScope(fn *ScriptFunction) func() {
	names := make(map[string]struct{}, len(fn.Params))
	for _, param := range fn.Params {
		if param.Name != "" {
			names[param.Name] = struct{}{}
		}
		collectBindingTarget(param.Target, names)
	}
	collectOwnScopeNames(fn.Body, names)
	popUnion := c.pushLocalNameUnion(names)
	previousSelf := c.selfScope
	previousClass := c.selfClass
	previousClassContext := c.selfClassContext
	c.selfScope = c.functionHasSelfScope(fn)
	c.selfClass = nil
	c.selfClassContext = false
	if c.selfScope {
		c.selfClass = c.selfScopeFnClasses[fn]
		_, c.selfClassContext = c.selfScopeClassFns[fn]
	}
	return func() {
		c.selfScope = previousSelf
		c.selfClass = previousClass
		c.selfClassContext = previousClassContext
		popUnion()
	}
}

// pushBlockNameScope enters the local-name scope of a block literal: block
// parameters (including implicit ones) plus the union of names the block
// body can bind. Blocks close over the enclosing function scope, so the
// enclosing frames stay visible.
func (c *scriptChecker) pushBlockNameScope(block *BlockLiteral) func() {
	names := make(map[string]struct{}, len(block.Params)+len(block.ImplicitParams))
	for _, param := range block.Params {
		if param.Name != "" {
			names[param.Name] = struct{}{}
		}
		collectBindingTarget(param.Target, names)
	}
	for _, name := range block.ImplicitParams {
		if name != "" {
			names[name] = struct{}{}
		}
	}
	collectOwnScopeNames(block.Body, names)
	return c.pushLocalNameUnion(names)
}

func (c *scriptChecker) pushLocalNameUnion(names map[string]struct{}) func() {
	c.localNameUnions = append(c.localNameUnions, names)
	return func() {
		c.localNameUnions = c.localNameUnions[:len(c.localNameUnions)-1]
	}
}

// Live local names mirror the runtime's predeclaration timeline: an
// assignment's own targets go live before its value evaluates, compound
// statements post-declare their bindings when they complete, and loop and
// block bodies go live at region entry because later iterations see every
// body binding. Shape shadowing consults this instead of the whole-body
// name union, which exists for undefined-name checks and deliberately
// includes names bound only later.

// predeclareStatementLiveNames records the names the runtime predeclares
// when stmt starts: only an assignment's own targets go live before the
// statement body runs.
func (c *scriptChecker) predeclareStatementLiveNames(stmt Statement) {
	assign, ok := stmt.(*AssignStmt)
	if !ok {
		return
	}
	names := make(map[string]struct{})
	collectBindingTarget(assign.Target, names)
	c.recordLiveLocalNames(names)
}

// postdeclareStatementLiveNames records the bindings the runtime
// predeclares after a compound statement completes, whichever paths ran.
func (c *scriptChecker) postdeclareStatementLiveNames(stmt Statement) {
	switch stmt.(type) {
	case *IfStmt, *ForStmt, *WhileStmt, *UntilStmt, *TryStmt:
	default:
		return
	}
	names := make(map[string]struct{})
	collectOwnScopeNamesFromStatement(stmt, names)
	c.recordLiveLocalNames(names)
}

// recordLiveStatementNames marks every binding of a multi-run region's body
// live at region entry: from the second iteration on, all of them exist.
func (c *scriptChecker) recordLiveStatementNames(statements []Statement) {
	names := make(map[string]struct{})
	collectOwnScopeNames(statements, names)
	c.recordLiveLocalNames(names)
}

func (c *scriptChecker) recordLiveLocalNames(names map[string]struct{}) {
	if len(names) == 0 || len(c.liveLocalNames) == 0 {
		return
	}
	frame := c.liveLocalNames[len(c.liveLocalNames)-1]
	if frame == nil {
		frame = make(map[string]struct{}, len(names))
		c.liveLocalNames[len(c.liveLocalNames)-1] = frame
	}
	for name := range names {
		frame[name] = struct{}{}
	}
}

func (c *scriptChecker) liveLocalNameHas(name string) bool {
	for i := len(c.liveLocalNames) - 1; i >= 0; i-- {
		if _, ok := c.liveLocalNames[i][name]; ok {
			return true
		}
	}
	return false
}

func (c *scriptChecker) localNameUnionHas(name string) bool {
	for i := len(c.localNameUnions) - 1; i >= 0; i-- {
		if _, ok := c.localNameUnions[i][name]; ok {
			return true
		}
	}
	return false
}

// functionHasSelfScope reports whether fn runs with self bound, i.e. it is an
// instance or class method of one of the script's classes.
func (c *scriptChecker) functionHasSelfScope(fn *ScriptFunction) bool {
	c.prepareSelfScopeFunctions()
	_, ok := c.selfScopeFns[fn]
	return ok
}

// prepareSelfScopeFunctions lazily builds the method-ownership maps. Every
// reader must call it first: a call can be checked (a constructor call in a
// class body, for example) before any function scope has been entered.
func (c *scriptChecker) prepareSelfScopeFunctions() {
	if c.selfScopeFns == nil {
		c.selfScopeFns = make(map[*ScriptFunction]struct{})
		c.selfScopeFnClasses = make(map[*ScriptFunction]*ClassDef)
		c.selfScopeClassFns = make(map[*ScriptFunction]struct{})
		for _, classDef := range c.script.classes {
			for _, method := range classDef.Methods {
				c.selfScopeFns[method] = struct{}{}
				c.selfScopeFnClasses[method] = classDef
			}
			for _, method := range classDef.ClassMethods {
				c.selfScopeFns[method] = struct{}{}
				c.selfScopeFnClasses[method] = classDef
				c.selfScopeClassFns[method] = struct{}{}
			}
		}
	}
}

// collectOwnScopeNames gathers every local name the statements can bind in
// the CURRENT env scope, matching the runtime's predeclaration rules: any
// branch's assignment targets, for-loop targets, and rescue-clause body
// locals are predeclared as nil once the enclosing statement runs, while
// block literals bind into their own child scope and rescue bindings stay
// scoped to their clause (both are therefore excluded here).
func collectOwnScopeNames(statements []Statement, out map[string]struct{}) {
	for _, stmt := range statements {
		collectOwnScopeNamesFromStatement(stmt, out)
	}
}

func collectOwnScopeNamesFromStatement(stmt Statement, out map[string]struct{}) {
	switch typed := stmt.(type) {
	case nil:
		return
	case *FunctionStmt, *ClassStmt, *AliasStmt, *EnumStmt, *RetryStmt:
		// None of these bind locals in the enclosing scope: nested def and
		// class statements parse but never bind (the runtime rejects them
		// before any binding could happen), alias and enum declarations are
		// parser-rejected outside the top level, and retry has no operands.
		return
	case *AssignStmt:
		collectBindingTarget(typed.Target, out)
		collectOwnScopeNamesFromExpression(typed.Target, out)
		collectOwnScopeNamesFromExpression(typed.Value, out)
	case *ReturnStmt:
		collectOwnScopeNamesFromExpression(typed.Value, out)
	case *RaiseStmt:
		collectOwnScopeNamesFromExpression(typed.Value, out)
		collectOwnScopeNamesFromExpression(typed.Message, out)
	case *BreakStmt:
		collectOwnScopeNamesFromExpression(typed.Value, out)
	case *NextStmt:
		collectOwnScopeNamesFromExpression(typed.Value, out)
	case *ExprStmt:
		collectOwnScopeNamesFromExpression(typed.Expr, out)
	case *IfStmt:
		collectOwnScopeNamesFromExpression(typed.Condition, out)
		collectOwnScopeNames(typed.Consequent, out)
		for _, elseIf := range typed.ElseIf {
			collectOwnScopeNamesFromExpression(elseIf.Condition, out)
			collectOwnScopeNames(elseIf.Consequent, out)
		}
		collectOwnScopeNames(typed.Alternate, out)
	case *ForStmt:
		collectBindingTarget(typed.Target, out)
		collectOwnScopeNamesFromExpression(typed.Iterable, out)
		collectOwnScopeNames(typed.Body, out)
	case *WhileStmt:
		collectOwnScopeNamesFromExpression(typed.Condition, out)
		collectOwnScopeNames(typed.Body, out)
	case *UntilStmt:
		collectOwnScopeNamesFromExpression(typed.Condition, out)
		collectOwnScopeNames(typed.Body, out)
	case *TryStmt:
		collectOwnScopeNames(typed.Body, out)
		for i := range typed.Rescues {
			collectOwnScopeNames(typed.Rescues[i].Body, out)
		}
		collectOwnScopeNames(typed.Else, out)
		collectOwnScopeNames(typed.Ensure, out)
	}
}

// collectOwnScopeNamesFromExpression descends expressions looking for
// embedded statement-expressions (begin/end, if, while, until, for used as
// expressions), whose bodies bind locals in the enclosing scope. Block
// literals are skipped: their bodies bind into a child scope.
func collectOwnScopeNamesFromExpression(expr Expression, out map[string]struct{}) {
	switch typed := expr.(type) {
	case nil:
		return
	case *Identifier, *IntegerLiteral, *FloatLiteral, *StringLiteral, *RegexLiteral,
		*BoolLiteral, *NilLiteral, *SymbolLiteral, *IvarExpr, *ClassVarExpr:
		// Leaves: no embedded statement-expressions to descend into.
		return
	case *BlockLiteral:
		// Block bodies bind into their own child scope, never the enclosing
		// one, so nothing here contributes to the current scope's names.
		return
	case *DestructureTarget:
		// Element targets can embed statement-expressions (an index selector
		// holding a begin/end body, for example) whose assignments bind in
		// the enclosing scope.
		for _, element := range typed.Elements {
			collectOwnScopeNamesFromExpression(element.Target, out)
		}
	case *TryStmt, *IfStmt, *WhileStmt, *UntilStmt, *ForStmt:
		collectOwnScopeNamesFromStatement(typed.(Statement), out)
	case *ArrayLiteral:
		for _, elem := range typed.Elements {
			collectOwnScopeNamesFromExpression(elem, out)
		}
	case *HashLiteral:
		for _, pair := range typed.Pairs {
			collectOwnScopeNamesFromExpression(pair.Key, out)
			collectOwnScopeNamesFromExpression(pair.Value, out)
		}
	case *CallExpr:
		collectOwnScopeNamesFromExpression(typed.Callee, out)
		for _, arg := range typed.Args {
			collectOwnScopeNamesFromExpression(arg, out)
		}
		for _, kwarg := range typed.KwArgs {
			collectOwnScopeNamesFromExpression(kwarg.Value, out)
		}
	case *SplatArg:
		collectOwnScopeNamesFromExpression(typed.Value, out)
	case *TypeLiteral:
		if typed.Fallback != nil {
			collectOwnScopeNamesFromExpression(typed.Fallback, out)
		}
	case *MemberExpr:
		collectOwnScopeNamesFromExpression(typed.Object, out)
	case *ScopeExpr:
		collectOwnScopeNamesFromExpression(typed.Object, out)
	case *IndexExpr:
		collectOwnScopeNamesFromExpression(typed.Object, out)
		for _, index := range typed.Indices {
			collectOwnScopeNamesFromExpression(index, out)
		}
	case *UnaryExpr:
		collectOwnScopeNamesFromExpression(typed.Right, out)
	case *BinaryExpr:
		collectOwnScopeNamesFromExpression(typed.Left, out)
		collectOwnScopeNamesFromExpression(typed.Right, out)
	case *ConditionalExpr:
		collectOwnScopeNamesFromExpression(typed.Condition, out)
		collectOwnScopeNamesFromExpression(typed.Consequent, out)
		collectOwnScopeNamesFromExpression(typed.Alternate, out)
	case *RescueExpr:
		collectOwnScopeNamesFromExpression(typed.Body, out)
		collectOwnScopeNamesFromExpression(typed.Fallback, out)
	case *IfExpr:
		collectOwnScopeNamesFromExpression(typed.Condition, out)
		collectOwnScopeNamesFromExpression(typed.Consequent, out)
		for _, branch := range typed.ElseIf {
			collectOwnScopeNamesFromExpression(branch.Condition, out)
			collectOwnScopeNamesFromExpression(branch.Result, out)
		}
		collectOwnScopeNamesFromExpression(typed.Alternate, out)
	case *RangeExpr:
		collectOwnScopeNamesFromExpression(typed.Start, out)
		collectOwnScopeNamesFromExpression(typed.End, out)
	case *CaseExpr:
		collectOwnScopeNamesFromExpression(typed.Target, out)
		for _, clause := range typed.Clauses {
			for _, value := range clause.Values {
				collectOwnScopeNamesFromExpression(value.Expr, out)
			}
			collectOwnScopeNamesFromExpression(clause.Result, out)
		}
		collectOwnScopeNamesFromExpression(typed.ElseExpr, out)
	case *YieldExpr:
		for _, arg := range typed.Args {
			collectOwnScopeNamesFromExpression(arg, out)
		}
	case *InterpolatedString:
		collectOwnScopeNamesFromStringParts(typed.Parts, out)
	case *InterpolatedSymbol:
		collectOwnScopeNamesFromStringParts(typed.Parts, out)
	}
}

func collectOwnScopeNamesFromStringParts(parts []StringPart, out map[string]struct{}) {
	for _, part := range parts {
		if exprPart, ok := part.(StringExpr); ok {
			collectOwnScopeNamesFromExpression(exprPart.Expr, out)
		}
	}
}

// nameFacts lazily computes the script-wide require facts for the checker's
// script. Requires bind their module's exports (and alias, when given) into
// the call root at runtime regardless of where the require statement runs,
// so every statically resolvable require anywhere in the script contributes
// names that any function may legally reference.
func (c *scriptChecker) nameFacts() *checkNameFacts {
	if c.nameFactsCache != nil {
		return c.nameFactsCache
	}
	facts := &checkNameFacts{requireExports: make(map[string]struct{})}
	c.nameFactsCache = facts
	visit := func(call *CallExpr) {
		callee, ok := call.Callee.(*Identifier)
		if !ok || callee.Name != "require" {
			return
		}
		moduleName, alias, ok := c.staticRequireCallShape(call)
		if !ok {
			facts.suppress = true
			return
		}
		entry, err := c.loadModule(moduleName)
		if err != nil {
			facts.suppress = true
			return
		}
		if alias != "" {
			facts.requireExports[alias] = struct{}{}
		}
		for name := range c.moduleExportValue(entry).Hash() {
			facts.requireExports[name] = struct{}{}
		}
	}
	for _, fn := range c.script.functions {
		visitFunctionCallExprs(fn, visit)
	}
	for _, classDef := range c.script.classes {
		visitCallExprsInStatements(classDef.Body, visit)
		for _, method := range classDef.Methods {
			visitFunctionCallExprs(method, visit)
		}
		for _, method := range classDef.ClassMethods {
			visitFunctionCallExprs(method, visit)
		}
	}
	return facts
}

// staticRequireCallShape resolves the module name and alias of a require
// call from literal arguments only. Unlike staticRequireCall it performs no
// shadowing checks: the facts pass over-approximates what a require might
// bind, which can only suppress warnings.
func (c *scriptChecker) staticRequireCallShape(call *CallExpr) (string, string, bool) {
	if len(call.Args) != 1 || call.Block != nil {
		return "", "", false
	}
	moduleName, ok := staticRequireModuleName(call.Args[0])
	if !ok {
		return "", "", false
	}
	kwargs := make(map[string]Value, len(call.KwArgs))
	for _, kwarg := range call.KwArgs {
		val, ok := staticLiteralValue(kwarg.Value)
		if !ok {
			return "", "", false
		}
		kwargs[kwarg.Name] = val
	}
	alias, err := parseRequireAlias(kwargs)
	if err != nil {
		return "", "", false
	}
	return moduleName, alias, true
}

func visitFunctionCallExprs(fn *ScriptFunction, visit func(*CallExpr)) {
	if fn == nil {
		return
	}
	for _, param := range fn.Params {
		visitCallExprsInExpression(param.DefaultVal, visit)
	}
	visitCallExprsInStatements(fn.Body, visit)
}

func visitCallExprsInStatements(statements []Statement, visit func(*CallExpr)) {
	for _, stmt := range statements {
		visitCallExprsInStatement(stmt, visit)
	}
}

func visitCallExprsInStatement(stmt Statement, visit func(*CallExpr)) {
	switch typed := stmt.(type) {
	case nil:
		return
	case *FunctionStmt:
		// Nested defs parse (the runtime rejects them at execution), so walk
		// them for completeness: over-approximating the visited calls can
		// only suppress undefined-name warnings, never add them.
		for _, param := range typed.Params {
			visitCallExprsInExpression(param.DefaultVal, visit)
		}
		visitCallExprsInStatements(typed.Body, visit)
	case *AliasStmt, *EnumStmt, *RetryStmt:
		// No expression operands, so no calls to visit.
		return
	case *ReturnStmt:
		visitCallExprsInExpression(typed.Value, visit)
	case *RaiseStmt:
		visitCallExprsInExpression(typed.Value, visit)
		visitCallExprsInExpression(typed.Message, visit)
	case *BreakStmt:
		visitCallExprsInExpression(typed.Value, visit)
	case *NextStmt:
		visitCallExprsInExpression(typed.Value, visit)
	case *AssignStmt:
		visitCallExprsInExpression(typed.Target, visit)
		visitCallExprsInExpression(typed.Value, visit)
	case *ExprStmt:
		visitCallExprsInExpression(typed.Expr, visit)
	case *IfStmt:
		visitCallExprsInExpression(typed.Condition, visit)
		visitCallExprsInStatements(typed.Consequent, visit)
		for _, elseIf := range typed.ElseIf {
			visitCallExprsInExpression(elseIf.Condition, visit)
			visitCallExprsInStatements(elseIf.Consequent, visit)
		}
		visitCallExprsInStatements(typed.Alternate, visit)
	case *ForStmt:
		visitCallExprsInExpression(typed.Iterable, visit)
		visitCallExprsInStatements(typed.Body, visit)
	case *WhileStmt:
		visitCallExprsInExpression(typed.Condition, visit)
		visitCallExprsInStatements(typed.Body, visit)
	case *UntilStmt:
		visitCallExprsInExpression(typed.Condition, visit)
		visitCallExprsInStatements(typed.Body, visit)
	case *TryStmt:
		visitCallExprsInStatements(typed.Body, visit)
		for i := range typed.Rescues {
			visitCallExprsInStatements(typed.Rescues[i].Body, visit)
		}
		visitCallExprsInStatements(typed.Else, visit)
		visitCallExprsInStatements(typed.Ensure, visit)
	case *ClassStmt:
		visitCallExprsInStatements(typed.Body, visit)
	}
}

func visitCallExprsInExpression(expr Expression, visit func(*CallExpr)) {
	switch typed := expr.(type) {
	case nil:
		return
	case *Identifier, *IntegerLiteral, *FloatLiteral, *StringLiteral, *RegexLiteral,
		*BoolLiteral, *NilLiteral, *SymbolLiteral, *IvarExpr, *ClassVarExpr:
		// Leaves: no nested expressions, so no calls to visit.
		return
	case *TryStmt, *IfStmt, *WhileStmt, *UntilStmt, *ForStmt:
		visitCallExprsInStatement(typed.(Statement), visit)
	case *ArrayLiteral:
		for _, elem := range typed.Elements {
			visitCallExprsInExpression(elem, visit)
		}
	case *HashLiteral:
		for _, pair := range typed.Pairs {
			visitCallExprsInExpression(pair.Key, visit)
			visitCallExprsInExpression(pair.Value, visit)
		}
	case *CallExpr:
		visit(typed)
		visitCallExprsInExpression(typed.Callee, visit)
		for _, arg := range typed.Args {
			visitCallExprsInExpression(arg, visit)
		}
		for _, kwarg := range typed.KwArgs {
			visitCallExprsInExpression(kwarg.Value, visit)
		}
		if typed.Block != nil {
			for _, param := range typed.Block.Params {
				visitCallExprsInExpression(param.DefaultVal, visit)
			}
			visitCallExprsInStatements(typed.Block.Body, visit)
		}
	case *SplatArg:
		visitCallExprsInExpression(typed.Value, visit)
	case *TypeLiteral:
		if typed.Fallback != nil {
			visitCallExprsInExpression(typed.Fallback, visit)
		}
	case *MemberExpr:
		visitCallExprsInExpression(typed.Object, visit)
	case *ScopeExpr:
		visitCallExprsInExpression(typed.Object, visit)
	case *IndexExpr:
		visitCallExprsInExpression(typed.Object, visit)
		for _, index := range typed.Indices {
			visitCallExprsInExpression(index, visit)
		}
	case *DestructureTarget:
		for _, element := range typed.Elements {
			visitCallExprsInExpression(element.Target, visit)
		}
	case *UnaryExpr:
		visitCallExprsInExpression(typed.Right, visit)
	case *BinaryExpr:
		visitCallExprsInExpression(typed.Left, visit)
		visitCallExprsInExpression(typed.Right, visit)
	case *ConditionalExpr:
		visitCallExprsInExpression(typed.Condition, visit)
		visitCallExprsInExpression(typed.Consequent, visit)
		visitCallExprsInExpression(typed.Alternate, visit)
	case *RescueExpr:
		visitCallExprsInExpression(typed.Body, visit)
		visitCallExprsInExpression(typed.Fallback, visit)
	case *IfExpr:
		visitCallExprsInExpression(typed.Condition, visit)
		visitCallExprsInExpression(typed.Consequent, visit)
		for _, branch := range typed.ElseIf {
			visitCallExprsInExpression(branch.Condition, visit)
			visitCallExprsInExpression(branch.Result, visit)
		}
		visitCallExprsInExpression(typed.Alternate, visit)
	case *RangeExpr:
		visitCallExprsInExpression(typed.Start, visit)
		visitCallExprsInExpression(typed.End, visit)
	case *CaseExpr:
		visitCallExprsInExpression(typed.Target, visit)
		for _, clause := range typed.Clauses {
			for _, value := range clause.Values {
				visitCallExprsInExpression(value.Expr, visit)
			}
			visitCallExprsInExpression(clause.Result, visit)
		}
		visitCallExprsInExpression(typed.ElseExpr, visit)
	case *BlockLiteral:
		for _, param := range typed.Params {
			visitCallExprsInExpression(param.DefaultVal, visit)
		}
		visitCallExprsInStatements(typed.Body, visit)
	case *YieldExpr:
		for _, arg := range typed.Args {
			visitCallExprsInExpression(arg, visit)
		}
	case *InterpolatedString:
		visitCallExprsInStringParts(typed.Parts, visit)
	case *InterpolatedSymbol:
		visitCallExprsInStringParts(typed.Parts, visit)
	}
}

func visitCallExprsInStringParts(parts []StringPart, visit func(*CallExpr)) {
	for _, part := range parts {
		if exprPart, ok := part.(StringExpr); ok {
			visitCallExprsInExpression(exprPart.Expr, visit)
		}
	}
}
