package runtime

// This file is a completeness gate for the hand-maintained AST walkers: every
// switch-based traversal over ast.Statement / ast.Expression listed in the
// inventory below must have an explicit case arm for every AST node type (or
// an explicit, justified exemption here). Adding a new node type to
// internal/ast without teaching each walker about it fails this test with the
// walker and type named, which is exactly the bug class behind the historical
// NextStmt / CallExpr.BlockArg / ClassMemberDecl misses.
//
// The gate works on source text via go/parser, so it can reach walkers in
// other packages (internal/ast, internal/tools/analyze) and in test files
// without import cycles. The inventory is deliberately explicit: a brand-new
// walker being added without a gate entry is accepted residual risk; missing
// TYPES in known walkers is the observed failure mode.

import (
	"fmt"
	goast "go/ast"
	goparser "go/parser"
	"go/token"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"testing"
)

// walkerNodeKind selects which AST type universe a walker must cover.
type walkerNodeKind int

const (
	walkerStatements walkerNodeKind = iota
	walkerExpressions
)

// gatedWalker names one hand-maintained walker and the node universe it must
// exhaust. Exemptions map an AST type name to the reason the walker may
// legitimately skip it; anything else must have a case arm.
type gatedWalker struct {
	file string
	// recv is the walker's method receiver type name, empty for plain
	// functions. It disambiguates methods that share a name across types.
	recv string
	fn   string
	kind walkerNodeKind
	// exempt lists AST types this walker deliberately does not handle. Keep
	// this rare: most walkers should carry an explicit (possibly no-op) arm
	// so the decision is visible at the switch.
	exempt map[string]string
}

func gatedWalkers() []gatedWalker {
	const astDir = "../ast"
	const analyzeDir = "../tools/analyze"
	return []gatedWalker{
		{file: "../parser/nesting.go", recv: "parser", fn: "nodeDepth", kind: walkerStatements},
		{file: "../parser/nesting.go", recv: "parser", fn: "nodeDepth", kind: walkerExpressions},
		{file: filepath.Join(astDir, "clone.go"), fn: "cloneStatement", kind: walkerStatements},
		{file: filepath.Join(astDir, "clone.go"), fn: "cloneExpression", kind: walkerExpressions},

		{file: "check_names.go", fn: "collectOwnScopeNamesFromStatement", kind: walkerStatements},
		{file: "check_names.go", fn: "collectOwnScopeNamesFromExpression", kind: walkerExpressions},
		{file: "check_names.go", fn: "visitCallExprsInStatement", kind: walkerStatements},
		{file: "check_names.go", fn: "visitCallExprsInExpression", kind: walkerExpressions},

		{file: "check.go", fn: "statementMayEscapeIteration", kind: walkerStatements},
		{file: "check.go", fn: "expressionMayEscapeIteration", kind: walkerExpressions},

		{file: "symbol_literals.go", recv: "symbolLiteralCollector", fn: "collectStatement", kind: walkerStatements},
		{file: "symbol_literals.go", recv: "symbolLiteralCollector", fn: "collectExpression", kind: walkerExpressions},

		{file: "eval.go", recv: "Execution", fn: "evalStatement", kind: walkerStatements, exempt: map[string]string{
			"FunctionStmt": "functions are hoisted at compile time; a nested def reaches the default arm and reports 'unsupported statement' by design",
			"AliasStmt":    "aliases are applied at compile time and the parser rejects them outside the top level and class bodies",
			"EnumStmt":     "enums are hoisted at compile time and the parser rejects them outside the top level",
		}},
		{file: "eval.go", recv: "Execution", fn: "evalExpressionWithAuto", kind: walkerExpressions, exempt: map[string]string{
			"DestructureTarget": "assignment-target-only node evaluated by the assignment path; evaluating one as a value is 'unsupported expression' by design",
		}},
		{file: "eval.go", fn: "statementCapturesCurrentEnv", kind: walkerStatements},
		{file: "eval.go", fn: "expressionCapturesCurrentEnv", kind: walkerExpressions},

		{file: filepath.Join(analyzeDir, "analyze.go"), fn: "statementTerminates", kind: walkerStatements},
		{file: filepath.Join(analyzeDir, "analyze.go"), fn: "lintExpression", kind: walkerExpressions},

		{file: "fuzz_test.go", fn: "validateFuzzStatement", kind: walkerStatements},
		{file: "fuzz_test.go", fn: "validateFuzzExpression", kind: walkerExpressions},
	}
}

// TestWalkerTypeSwitchCoverage fails when any gated walker's type switches
// miss an AST node type that is neither handled nor exempted, or when an
// exemption goes stale.
func TestWalkerTypeSwitchCoverage(t *testing.T) {
	t.Parallel()

	stmtTypes, exprTypes := loadASTNodeTypeUniverse(t, "../ast")
	if len(stmtTypes) == 0 || len(exprTypes) == 0 {
		t.Fatalf("AST universe is empty (statements=%d expressions=%d); the ../ast scan is broken", len(stmtTypes), len(exprTypes))
	}
	for _, sentinel := range []string{"ExprStmt", "ClassStmt"} {
		if _, ok := stmtTypes[sentinel]; !ok {
			t.Fatalf("AST statement universe %v is missing sentinel %s; the ../ast scan is broken", sortedNames(stmtTypes), sentinel)
		}
	}
	for _, sentinel := range []string{"CallExpr", "Identifier"} {
		if _, ok := exprTypes[sentinel]; !ok {
			t.Fatalf("AST expression universe %v is missing sentinel %s; the ../ast scan is broken", sortedNames(exprTypes), sentinel)
		}
	}

	for _, walker := range gatedWalkers() {
		label := walker.fn
		if walker.recv != "" {
			label = walker.recv + "." + walker.fn
		}
		label = fmt.Sprintf("%s:%s", filepath.ToSlash(walker.file), label)

		universe := stmtTypes
		if walker.kind == walkerExpressions {
			universe = exprTypes
		}

		handled := typeSwitchCaseTypes(t, walker)
		if len(handled) == 0 {
			t.Errorf("walker %s has no type switch cases; the extraction or the walker moved", label)
			continue
		}

		for _, name := range sortedNames(universe) {
			_, isHandled := handled[name]
			reason, isExempt := walker.exempt[name]
			switch {
			case isHandled && isExempt:
				t.Errorf("walker %s handles ast.%s but also exempts it (%q); remove the stale exemption", label, name, reason)
			case !isHandled && !isExempt:
				t.Errorf("walker %s does not handle ast.%s; add a case arm for it (a deliberate no-op arm is fine) or record an exemption with a justification in gatedWalkers", label, name)
			}
		}
		for name := range walker.exempt {
			if _, ok := universe[name]; !ok {
				t.Errorf("walker %s exempts unknown type %q; remove or fix the exemption", label, name)
			}
		}
	}
}

// loadASTNodeTypeUniverse parses the internal/ast package source and returns
// the names of every concrete type implementing the Statement and Expression
// marker methods. Types implementing both (statement-expressions) appear in
// both sets.
func loadASTNodeTypeUniverse(t *testing.T, dir string) (stmtTypes, exprTypes map[string]struct{}) {
	t.Helper()

	stmtTypes = make(map[string]struct{})
	exprTypes = make(map[string]struct{})
	for _, file := range goSourceFiles(t, dir) {
		fset := token.NewFileSet()
		parsed, err := goparser.ParseFile(fset, file, nil, 0)
		if err != nil {
			t.Fatalf("parse %s: %v", file, err)
		}
		for _, decl := range parsed.Decls {
			fn, ok := decl.(*goast.FuncDecl)
			if !ok || fn.Recv == nil {
				continue
			}
			recv := receiverTypeName(fn.Recv)
			if recv == "" {
				continue
			}
			switch fn.Name.Name {
			case "stmtNode":
				stmtTypes[recv] = struct{}{}
			case "exprNode":
				exprTypes[recv] = struct{}{}
			}
		}
	}
	return stmtTypes, exprTypes
}

func goSourceFiles(t *testing.T, dir string) []string {
	t.Helper()

	entries, err := os.ReadDir(dir)
	if err != nil {
		t.Fatalf("read dir %s: %v", dir, err)
	}
	var files []string
	for _, entry := range entries {
		name := entry.Name()
		if entry.IsDir() || !strings.HasSuffix(name, ".go") || strings.HasSuffix(name, "_test.go") {
			continue
		}
		files = append(files, filepath.Join(dir, name))
	}
	return files
}

// typeSwitchCaseTypes parses the walker's file and returns the union of type
// names appearing in case clauses of every type switch inside the walker's
// function body, with pointer stars and package qualifiers stripped.
func typeSwitchCaseTypes(t *testing.T, walker gatedWalker) map[string]struct{} {
	t.Helper()

	fset := token.NewFileSet()
	parsed, err := goparser.ParseFile(fset, walker.file, nil, 0)
	if err != nil {
		t.Fatalf("parse %s: %v", walker.file, err)
	}
	for _, decl := range parsed.Decls {
		fn, ok := decl.(*goast.FuncDecl)
		if !ok || fn.Name.Name != walker.fn {
			continue
		}
		recv := ""
		if fn.Recv != nil {
			recv = receiverTypeName(fn.Recv)
		}
		if recv != walker.recv {
			continue
		}
		handled := make(map[string]struct{})
		goast.Inspect(fn, func(node goast.Node) bool {
			sw, ok := node.(*goast.TypeSwitchStmt)
			if !ok {
				return true
			}
			for _, clause := range sw.Body.List {
				caseClause, ok := clause.(*goast.CaseClause)
				if !ok {
					continue
				}
				for _, expr := range caseClause.List {
					if name := caseTypeName(expr); name != "" {
						handled[name] = struct{}{}
					}
				}
			}
			return true
		})
		return handled
	}
	t.Fatalf("walker function %s (receiver %q) not found in %s; update the gate inventory", walker.fn, walker.recv, walker.file)
	return nil
}

func caseTypeName(expr goast.Expr) string {
	switch typed := expr.(type) {
	case *goast.StarExpr:
		return caseTypeName(typed.X)
	case *goast.SelectorExpr:
		return typed.Sel.Name
	case *goast.Ident:
		if typed.Name == "nil" {
			return ""
		}
		return typed.Name
	default:
		return ""
	}
}

func receiverTypeName(fields *goast.FieldList) string {
	if fields == nil || len(fields.List) == 0 {
		return ""
	}
	expr := fields.List[0].Type
	if star, ok := expr.(*goast.StarExpr); ok {
		expr = star.X
	}
	if ident, ok := expr.(*goast.Ident); ok {
		return ident.Name
	}
	return ""
}

func sortedNames(set map[string]struct{}) []string {
	names := make([]string, 0, len(set))
	for name := range set {
		names = append(names, name)
	}
	sort.Strings(names)
	return names
}
