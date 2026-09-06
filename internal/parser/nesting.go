package parser

import "github.com/mgomes/vibescript/internal/ast"

// Parsing precedes runtime quotas. Bound both recursive descent and AST
// height: iterative infix parsing can build a deep tree on a shallow stack.
const maxSyntaxDepth = 1024

type syntaxNodeDepth struct {
	depth    int
	callBase int // CallExpr depth without its replaceable trailing block.
}

func (p *parser) enterSyntax() bool {
	if p.nestingError != nil {
		return false
	}
	if p.syntaxDepth >= maxSyntaxDepth {
		p.rejectNesting(p.curToken.Pos)
		return false
	}
	p.syntaxDepth++
	return true
}

func (p *parser) rejectNesting(pos ast.Position) {
	if p.nestingError == nil {
		p.nestingError = &parseError{pos: pos, msg: "syntax nesting too deep", frames: p.codeFrameFormatter()}
	}
	// Stop without lexing the rejected suffix or attempting grammar recovery.
	eof := ast.Token{Type: ast.TokenEOF, Pos: pos, End: pos}
	p.curToken, p.peekToken, p.peekPeek = eof, eof, eof
}

func (p *parser) checkSyntaxNode(node ast.Node) bool {
	if p.nestingError != nil {
		return false
	}
	if p.nodeDepth(node, 0) > maxSyntaxDepth {
		p.rejectNesting(node.Pos())
		return false
	}
	return true
}

// Child depths are memoized when nodes finish parsing so a chain takes linear
// work. Types have their own bound in parseTypeExpr. The only structural
// mutation after an expression finishes is handled by callWithBlock.
func (p *parser) nodeDepth(node ast.Node, nesting int) int {
	if node == nil {
		return 0
	}
	if nesting > maxSyntaxDepth {
		return maxSyntaxDepth + 1
	}
	if cached, ok := p.nodeDepths[node]; ok {
		return cached.depth
	}
	depth := 1
	child := func(n ast.Node) {
		depth = max(depth, 1+p.nodeDepth(n, nesting+1))
	}
	statements := func(stmts []ast.Statement) {
		for _, stmt := range stmts {
			child(stmt)
		}
	}
	expressions := func(exprs []ast.Expression) {
		for _, expr := range exprs {
			child(expr)
		}
	}
	params := func(params []ast.Param) {
		for _, param := range params {
			child(param.Target)
			child(param.DefaultVal)
		}
	}
	parts := func(parts []ast.StringPart) {
		for _, part := range parts {
			if expr, ok := part.(ast.StringExpr); ok {
				child(expr.Expr)
			}
		}
	}
	callBase := 0
	switch n := node.(type) {
	case *ast.Identifier, *ast.IntegerLiteral, *ast.FloatLiteral, *ast.StringLiteral,
		*ast.RegexLiteral, *ast.BoolLiteral, *ast.NilLiteral, *ast.SymbolLiteral,
		*ast.IvarExpr, *ast.ClassVarExpr, *ast.AliasStmt, *ast.RetryStmt, *ast.EnumStmt:
		return 1
	case *ast.ArrayLiteral:
		expressions(n.Elements)
	case *ast.HashLiteral:
		for _, pair := range n.Pairs {
			child(pair.Key)
			child(pair.Value)
		}
	case *ast.CallExpr:
		child(n.Callee)
		expressions(n.Args)
		for _, arg := range n.KwArgs {
			child(arg.Value)
		}
		callBase = depth
		if n.Block != nil {
			child(n.Block)
		}
	case *ast.SplatArg:
		child(n.Value)
	case *ast.TypeLiteral:
		child(n.Fallback)
	case *ast.MemberExpr:
		child(n.Object)
	case *ast.ScopeExpr:
		child(n.Object)
	case *ast.IndexExpr:
		child(n.Object)
		expressions(n.Indices)
	case *ast.DestructureTarget:
		for _, element := range n.Elements {
			child(element.Target)
		}
	case *ast.UnaryExpr:
		child(n.Right)
	case *ast.BinaryExpr:
		child(n.Left)
		child(n.Right)
	case *ast.ConditionalExpr:
		child(n.Condition)
		child(n.Consequent)
		child(n.Alternate)
	case *ast.RescueExpr:
		child(n.Body)
		child(n.Fallback)
	case *ast.IfExpr:
		child(n.Condition)
		child(n.Consequent)
		for _, branch := range n.ElseIf {
			child(branch.Condition)
			child(branch.Result)
		}
		child(n.Alternate)
	case *ast.RangeExpr:
		child(n.Start)
		child(n.End)
	case *ast.CaseExpr:
		child(n.Target)
		for _, clause := range n.Clauses {
			for _, value := range clause.Values {
				child(value.Expr)
			}
			child(clause.Result)
		}
		child(n.ElseExpr)
	case *ast.BlockLiteral:
		params(n.Params)
		statements(n.Body)
	case *ast.YieldExpr:
		expressions(n.Args)
	case *ast.InterpolatedString:
		parts(n.Parts)
	case *ast.InterpolatedSymbol:
		parts(n.Parts)
	case *ast.FunctionStmt:
		params(n.Params)
		statements(n.Body)
	case *ast.ReturnStmt:
		child(n.Value)
	case *ast.RaiseStmt:
		child(n.Value)
		child(n.Message)
	case *ast.AssignStmt:
		child(n.Target)
		child(n.Value)
	case *ast.ExprStmt:
		child(n.Expr)
	case *ast.IfStmt:
		child(n.Condition)
		statements(n.Consequent)
		for _, branch := range n.ElseIf {
			child(branch)
		}
		statements(n.Alternate)
	case *ast.ForStmt:
		child(n.Target)
		child(n.Iterable)
		statements(n.Body)
	case *ast.WhileStmt:
		child(n.Condition)
		statements(n.Body)
	case *ast.UntilStmt:
		child(n.Condition)
		statements(n.Body)
	case *ast.BreakStmt:
		child(n.Value)
	case *ast.NextStmt:
		child(n.Value)
	case *ast.TryStmt:
		statements(n.Body)
		for _, rescue := range n.Rescues {
			statements(rescue.Body)
		}
		statements(n.Else)
		statements(n.Ensure)
	case *ast.ClassStmt:
		for _, method := range n.Methods {
			child(method)
		}
		for _, method := range n.ClassMethods {
			child(method)
		}
		for _, module := range n.Modules {
			child(module)
		}
		statements(n.Body)
	default:
		return maxSyntaxDepth + 1
	}
	if p.nodeDepths == nil {
		p.nodeDepths = make(map[ast.Node]syntaxNodeDepth)
	}
	p.nodeDepths[node] = syntaxNodeDepth{depth: depth, callBase: callBase}
	return depth
}
