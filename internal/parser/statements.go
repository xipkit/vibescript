package parser

import (
	"slices"
	"strings"

	"github.com/mgomes/vibescript/internal/ast"
)

func (p *parser) parseStatement() ast.Statement {
	if !p.enterSyntax() {
		return nil
	}
	defer func() { p.syntaxDepth-- }()

	var stmt ast.Statement
	switch p.curToken.Type {
	case ast.TokenDef:
		stmt = p.parseFunctionStatement()
	case ast.TokenClass:
		stmt = p.parseClassStatement()
	case ast.TokenEnum:
		stmt = p.parseEnumStatement()
	case ast.TokenExport:
		stmt = p.parseExportStatement()
	case ast.TokenPrivate:
		stmt = p.parsePrivateStatement()
	case ast.TokenReturn:
		stmt = p.parseReturnStatement()
	case ast.TokenRaise:
		stmt = p.parseRaiseStatement()
	case ast.TokenIf:
		stmt = p.parseIfStatement()
	case ast.TokenUnless:
		stmt = p.parseUnlessStatement()
	case ast.TokenFor:
		stmt = p.parseForStatement()
	case ast.TokenWhile:
		stmt = p.parseWhileStatement()
	case ast.TokenUntil:
		stmt = p.parseUntilStatement()
	case ast.TokenBreak:
		stmt = p.parseBreakStatement()
	case ast.TokenNext:
		stmt = p.parseNextStatement()
	case ast.TokenRetry:
		stmt = p.parseRetryStatement()
	case ast.TokenBegin:
		stmt = p.parseBeginStatement()
	case ast.TokenIdent:
		if p.curToken.Literal == "assert" {
			stmt = p.parseAssertStatement()
		} else if p.curToken.Literal == "alias" && p.peekStartsAliasNameOnLine(p.curToken.Pos.Line) {
			if p.canParseTopLevelAliasStatement() {
				stmt = p.parseAliasStatement(false)
			} else {
				stmt = p.parseUnsupportedAliasStatement()
			}
		} else if p.curToken.Literal == "module" && p.peekStartsModuleDeclaration() {
			stmt = p.parseModuleDeclarationStatement()
		} else {
			stmt = p.parseExpressionOrAssignStatement()
		}
	default:
		stmt = p.parseExpressionOrAssignStatement()
	}
	if continued := p.continueStatementExpression(stmt); continued != nil {
		stmt = continued
	}
	stmt = p.parseStatementModifier(stmt)
	if !p.checkSyntaxNode(stmt) {
		return nil
	}
	return stmt
}

func (p *parser) continueStatementExpression(stmt ast.Statement) ast.Statement {
	expr, ok := stmt.(ast.Expression)
	if !ok {
		return nil
	}
	if isStatementModifier(p.peekToken.Type) && p.peekToken.Pos.Line == p.curToken.Pos.Line {
		return nil
	}
	if p.peekToken.Pos.Line != p.curToken.Pos.Line {
		return nil
	}
	if lowestPrec >= p.peekPrecedence() && !p.canParseParenlessCall(expr, lowestPrec, false) {
		return nil
	}
	continued := p.continueExpressionParse(expr, lowestPrec, 0, false)
	if continued == nil {
		return nil
	}
	return &ast.ExprStmt{Expr: continued, Position: expr.Pos()}
}

func (p *parser) skipStatementSeparators() {
	for p.curToken.Type == ast.TokenSemicolon {
		p.nextToken()
	}
}

func (p *parser) parseStatementModifier(stmt ast.Statement) ast.Statement {
	if stmt == nil || !isStatementModifier(p.peekToken.Type) || p.peekToken.Pos.Line != p.curToken.Pos.Line {
		return stmt
	}

	modifier := p.peekToken
	if !canUseStatementModifier(stmt) {
		p.nextToken()
		p.nextToken()
		_ = p.parseLineExpression(lowestPrec)
		p.addParseError(modifier.Pos, "modifier %s is only supported after expression or assignment statements, or leaf control-flow statements", strings.ToLower(modifier.Type.String()))
		return stmt
	}

	p.nextToken()
	p.nextToken()
	condition := p.parseLineExpression(lowestPrec)
	if condition == nil {
		return nil
	}

	body := []ast.Statement{stmt}
	switch modifier.Type {
	case ast.TokenIf:
		return &ast.IfStmt{Condition: condition, Consequent: body, ModifierBodyFirst: true, Position: modifier.Pos}
	case ast.TokenWhile:
		return &ast.WhileStmt{Condition: condition, Body: body, BodyFirst: true, Position: modifier.Pos}
	case ast.TokenUntil:
		return &ast.UntilStmt{Condition: condition, Body: body, BodyFirst: true, Position: modifier.Pos}
	case ast.TokenUnless:
		return &ast.IfStmt{Condition: condition, Alternate: body, ModifierBodyFirst: true, Position: modifier.Pos}
	default:
		return stmt
	}
}

func isStatementModifier(tt ast.TokenType) bool {
	return tt == ast.TokenIf || tt == ast.TokenWhile || tt == ast.TokenUntil || tt == ast.TokenUnless
}

func canUseStatementModifier(stmt ast.Statement) bool {
	switch stmt.(type) {
	case *ast.AssignStmt, *ast.ExprStmt, *ast.ReturnStmt, *ast.RaiseStmt, *ast.BreakStmt, *ast.NextStmt, *ast.RetryStmt:
		return true
	default:
		return false
	}
}

func (p *parser) parseReturnStatement() ast.Statement {
	pos := p.curToken.Pos
	if p.peekEndsStatement(pos) || p.peekStartsSameLineStatementModifier(pos) {
		return &ast.ReturnStmt{Position: pos}
	}
	p.nextToken()
	value := p.parseCommaSeparatedStatementExpression(ast.TokenIf, ast.TokenUnless, ast.TokenWhile, ast.TokenUntil)
	if value == nil {
		return nil
	}
	return &ast.ReturnStmt{Value: value, Position: pos}
}

func (p *parser) parseRaiseStatement() ast.Statement {
	pos := p.curToken.Pos
	if p.peekEndsStatement(pos) || p.peekStartsSameLineStatementModifier(pos) {
		return &ast.RaiseStmt{Position: pos}
	}
	p.nextToken()
	value := p.parseStatementValueExpression()
	if value == nil {
		return nil
	}
	if p.peekToken.Type == ast.TokenComma && p.peekToken.Pos.Line == p.curToken.Pos.Line {
		// The comma introducing a message must sit on the line the first
		// argument ENDS on (like return's list detection), so a multiline
		// first argument keeps its message.
		p.nextToken()
		p.nextToken()
		message := p.parseLineExpression(lowestPrec)
		if message == nil {
			return nil
		}
		return &ast.RaiseStmt{Value: value, Message: message, Position: pos}
	}
	return &ast.RaiseStmt{Value: value, Position: pos}
}

func (p *parser) parseCommaSeparatedStatementExpression(stop ...ast.TokenType) ast.Expression {
	first := p.parseLineExpressionUntil(lowestPrec, stop...)
	if first == nil || p.peekToken.Type != ast.TokenComma || p.peekToken.Pos.Line != p.curToken.Pos.Line {
		return first
	}

	elements := []ast.Expression{first}
	for p.peekToken.Type == ast.TokenComma && p.peekToken.Pos.Line == p.curToken.Pos.Line {
		p.nextToken()
		p.nextToken()
		next := p.parseLineExpressionUntil(lowestPrec, stop...)
		if next == nil {
			return nil
		}
		elements = append(elements, next)
	}
	return &ast.ArrayLiteral{Elements: elements, Position: first.Pos()}
}

func (p *parser) parseBlock(stop ...ast.TokenType) []ast.Statement {
	stmts := []ast.Statement{}
	p.statementNesting++
	defer func() {
		p.statementNesting--
	}()

	for {
		p.skipStatementSeparators()
		if isBlockStopToken(p.curToken.Type, stop) || p.curToken.Type == ast.TokenEOF {
			return stmts
		}
		stmt := p.parseStatement()
		if stmt != nil {
			stmts = append(stmts, stmt)
		}
		p.nextToken()
	}
}

func isBlockStopToken(token ast.TokenType, stop []ast.TokenType) bool {
	return slices.Contains(stop, token)
}

func (p *parser) parseIfStatement() ast.Statement {
	pos := p.curToken.Pos
	p.nextToken()
	condition := p.parseLineExpressionUntilForced(lowestPrec, ast.TokenThen)
	if condition == nil {
		return nil
	}

	p.nextToken()
	p.consumeConditionalBodySeparator()
	consequent := p.parseBlock(ast.TokenEnd, ast.TokenElse, ast.TokenElsif)

	var elseifClauses []*ast.IfStmt
	for p.curToken.Type == ast.TokenElsif {
		p.nextToken()
		cond := p.parseLineExpressionUntilForced(lowestPrec, ast.TokenThen)
		if cond == nil {
			return nil
		}
		p.nextToken()
		p.consumeConditionalBodySeparator()
		body := p.parseBlock(ast.TokenEnd, ast.TokenElse, ast.TokenElsif)
		clause := &ast.IfStmt{Condition: cond, Consequent: body, Position: cond.Pos()}
		elseifClauses = append(elseifClauses, clause)
	}

	var alternate []ast.Statement
	if p.curToken.Type == ast.TokenElse {
		p.nextToken()
		alternate = p.parseBlock(ast.TokenEnd)
	}

	if p.curToken.Type != ast.TokenEnd {
		p.errorExpected(p.curToken, "end")
	}

	return &ast.IfStmt{Condition: condition, Consequent: consequent, ElseIf: elseifClauses, Alternate: alternate, Position: pos}
}

func (p *parser) parseUnlessStatement() ast.Statement {
	pos := p.curToken.Pos
	p.nextToken()
	condition := p.parseLineExpressionUntilForced(lowestPrec, ast.TokenThen)
	if condition == nil {
		return nil
	}

	p.nextToken()
	p.consumeConditionalBodySeparator()
	body := p.parseBlock(ast.TokenEnd, ast.TokenElse)

	var alternate []ast.Statement
	if p.curToken.Type == ast.TokenElse {
		p.nextToken()
		alternate = p.parseBlock(ast.TokenEnd)
	}

	if p.curToken.Type != ast.TokenEnd {
		p.errorExpected(p.curToken, "end")
	}

	return &ast.IfStmt{Condition: condition, Consequent: alternate, Alternate: body, AlternateFirst: true, Position: pos}
}

func (p *parser) consumeConditionalBodySeparator() {
	if p.curToken.Type == ast.TokenThen {
		p.nextToken()
	}
	p.skipStatementSeparators()
}

func (p *parser) parseForStatement() ast.Statement {
	pos := p.curToken.Pos
	p.nextToken()
	target := p.parseForTarget()
	if target == nil {
		return nil
	}

	if !p.expectPeek(ast.TokenIn) {
		return nil
	}

	p.nextToken()
	iterable := p.parseLoopConditionExpression()
	if iterable == nil {
		return nil
	}

	p.advanceToLoopBody()
	// The loop target binds locals in the surrounding scope, so register it
	// before parsing the body for name-sensitive parsing decisions such as
	// percent-literal vs modulo disambiguation.
	p.declareLocalTarget(target)
	body := p.parseBlock(ast.TokenEnd)

	if p.curToken.Type != ast.TokenEnd {
		p.errorExpected(p.curToken, "end")
	}

	return &ast.ForStmt{Target: target, Iterable: iterable, Body: body, Position: pos}
}

func (p *parser) parseForTarget() ast.Expression {
	switch p.curToken.Type {
	case ast.TokenAsterisk:
		target := p.parseDestructureTargetList(nil, false, ast.TokenIn)
		if target == nil {
			return nil
		}
		if !isBlockParameterTarget(target) {
			p.addParseError(target.Pos(), "invalid for loop target")
			return nil
		}
		return target
	default:
		first := p.parseDestructureSingleTarget(false)
		if first == nil {
			return nil
		}
		if p.peekToken.Type == ast.TokenComma {
			target := p.parseDestructureTargetList(first, false, ast.TokenIn)
			if target == nil {
				return nil
			}
			if !isBlockParameterTarget(target) {
				p.addParseError(target.Pos(), "invalid for loop target")
				return nil
			}
			return target
		}
		if !isBlockParameterTarget(first) {
			p.addParseError(first.Pos(), "invalid for loop target")
			return nil
		}
		return first
	}
}

func (p *parser) parseWhileStatement() ast.Statement {
	pos := p.curToken.Pos
	p.nextToken()
	condition := p.parseLoopConditionExpression()
	if condition == nil {
		return nil
	}

	p.advanceToLoopBody()
	body := p.parseBlock(ast.TokenEnd)

	if p.curToken.Type != ast.TokenEnd {
		p.errorExpected(p.curToken, "end")
	}

	return &ast.WhileStmt{Condition: condition, Body: body, Position: pos}
}

func (p *parser) parseUntilStatement() ast.Statement {
	pos := p.curToken.Pos
	p.nextToken()
	condition := p.parseLoopConditionExpression()
	if condition == nil {
		return nil
	}

	p.advanceToLoopBody()
	body := p.parseBlock(ast.TokenEnd)

	if p.curToken.Type != ast.TokenEnd {
		p.errorExpected(p.curToken, "end")
	}

	return &ast.UntilStmt{Condition: condition, Body: body, Position: pos}
}

func (p *parser) parseLoopConditionExpression() ast.Expression {
	return p.parseLineExpressionUntil(lowestPrec, ast.TokenDo)
}

func (p *parser) advanceToLoopBody() {
	if p.peekToken.Type == ast.TokenDo && p.peekToken.Pos.Line == p.curToken.Pos.Line {
		p.nextToken()
	}
	p.nextToken()
}

func (p *parser) parseBreakStatement() ast.Statement {
	pos := p.curToken.Pos
	if p.peekEndsStatement(pos) || p.peekStartsSameLineStatementModifier(pos) {
		return &ast.BreakStmt{Position: pos}
	}
	p.nextToken()
	value := p.parseStatementValueExpression()
	if value == nil {
		return nil
	}
	return &ast.BreakStmt{Value: value, Position: pos}
}

func (p *parser) parseNextStatement() ast.Statement {
	pos := p.curToken.Pos
	if p.peekEndsStatement(pos) || p.peekStartsSameLineStatementModifier(pos) {
		return &ast.NextStmt{Position: pos}
	}
	p.nextToken()
	value := p.parseStatementValueExpression()
	if value == nil {
		return nil
	}
	return &ast.NextStmt{Value: value, Position: pos}
}

func (p *parser) parseStatementValueExpression() ast.Expression {
	return p.parseLineExpressionUntil(lowestPrec, ast.TokenIf, ast.TokenUnless, ast.TokenWhile, ast.TokenUntil)
}

func (p *parser) parseRetryStatement() ast.Statement {
	pos := p.curToken.Pos
	if p.peekEndsStatement(pos) || p.peekStartsSameLineStatementModifier(pos) {
		return &ast.RetryStmt{Position: pos}
	}
	p.addParseError(p.peekToken.Pos, "retry does not accept a value")
	p.recoverSameLineStatementRemainder(pos.Line)
	return nil
}

func (p *parser) peekStartsSameLineStatementModifier(pos ast.Position) bool {
	return isStatementModifier(p.peekToken.Type) && p.peekToken.Pos.Line == pos.Line
}

func (p *parser) parseBeginStatement() ast.Statement {
	pos := p.curToken.Pos
	p.nextToken()
	body := p.parseBlock(ast.TokenRescue, ast.TokenElse, ast.TokenEnsure, ast.TokenEnd)
	return p.parseRescueElseEnsureTail(pos, body, "begin")
}

func (p *parser) parseRescueElseEnsureTail(pos ast.Position, body []ast.Statement, owner string) ast.Statement {
	var rescues []ast.RescueClause
	anyRescueBody := false
	for p.curToken.Type == ast.TokenRescue {
		rescuePos := p.curToken.Pos
		rescueTy, rescueBinding, ok := p.parseRescueClause(rescuePos)
		if !ok {
			p.recoverToBlockEnd()
			return nil
		}
		p.nextToken()
		// The rescue binding is local only within the rescue body: at runtime
		// it lives in a child env and is undefined afterward. Other locals
		// assigned in the body belong to the surrounding scope, so parse the
		// body in the current scope and remove only the binding afterward
		// (unless it was already a local before the handler).
		bindingWasLocal := rescueBinding != "" && p.localDeclaredInTop(rescueBinding)
		if rescueBinding != "" {
			p.declareLocal(rescueBinding)
		}
		rescueBody := p.parseBlock(ast.TokenRescue, ast.TokenElse, ast.TokenEnsure, ast.TokenEnd)
		if rescueBinding != "" && !bindingWasLocal {
			p.undeclareLocal(rescueBinding)
		}
		if len(rescueBody) > 0 {
			anyRescueBody = true
		}
		rescues = append(rescues, ast.RescueClause{Ty: rescueTy, Binding: rescueBinding, Body: rescueBody, Position: rescuePos})
	}

	var elseBody []ast.Statement
	if p.curToken.Type == ast.TokenElse {
		if len(rescues) == 0 {
			p.addParseError(p.curToken.Pos, "%s else requires rescue", owner)
			return nil
		}
		p.nextToken()
		elseBody = p.parseBlock(ast.TokenEnsure, ast.TokenEnd)
	}

	var ensureBody []ast.Statement
	if p.curToken.Type == ast.TokenEnsure {
		p.nextToken()
		ensureBody = p.parseBlock(ast.TokenEnd)
	}

	if p.curToken.Type != ast.TokenEnd {
		p.errorExpected(p.curToken, "end")
		return nil
	}

	needsHandler := len(rescues) > 0 || owner == "function"
	if needsHandler && !anyRescueBody && len(ensureBody) == 0 {
		p.addParseError(pos, "%s requires rescue and/or ensure", owner)
		return nil
	}

	return &ast.TryStmt{Body: body, Rescues: rescues, Else: elseBody, Ensure: ensureBody, Position: pos}
}

func (p *parser) parseRescueClause(rescuePos ast.Position) (*ast.TypeExpr, string, bool) {
	if p.peekToken.Pos.Line != rescuePos.Line {
		return nil, "", true
	}

	var rescueTy *ast.TypeExpr
	switch p.peekToken.Type {
	case ast.TokenLParen:
		p.nextToken()
		p.nextToken()
		rescueTy = p.parseTypeExpr()
		if rescueTy == nil {
			return nil, "", false
		}
		if !p.validateRescueTypeExpr(rescueTy, rescuePos) {
			p.recoverRescueHeaderRemainder(rescuePos.Line)
			return nil, "", false
		}
		if !p.expectPeek(ast.TokenRParen) {
			return nil, "", false
		}
	case ast.TokenIdent:
		p.nextToken()
		rescueTy = p.parseTypeExpr()
		if rescueTy == nil {
			return nil, "", false
		}
		if !p.validateRescueTypeExpr(rescueTy, rescuePos) {
			p.recoverRescueHeaderRemainder(rescuePos.Line)
			return nil, "", false
		}
	case ast.TokenArrow:
	case ast.TokenThinArrow:
		p.addParseError(p.peekToken.Pos, "rescue binding must use =>")
		p.recoverRescueHeaderRemainder(rescuePos.Line)
		return nil, "", false
	default:
		return nil, "", true
	}

	binding := ""
	if p.peekToken.Type == ast.TokenThinArrow && p.peekToken.Pos.Line == rescuePos.Line {
		p.addParseError(p.peekToken.Pos, "rescue binding must use =>")
		p.recoverRescueHeaderRemainder(rescuePos.Line)
		return nil, "", false
	}
	if p.peekToken.Type == ast.TokenArrow && p.peekToken.Pos.Line == rescuePos.Line {
		var ok bool
		binding, ok = p.parseRescueBinding()
		if !ok {
			return nil, "", false
		}
	}
	return rescueTy, binding, true
}

func (p *parser) parseRescueBinding() (string, bool) {
	p.nextToken()
	if p.peekToken.Type != ast.TokenIdent {
		p.addParseError(p.peekToken.Pos, "rescue binding must be an identifier")
		p.recoverRescueHeaderRemainder(p.curToken.Pos.Line)
		return "", false
	}
	p.nextToken()
	return p.curToken.Literal, true
}

func (p *parser) recoverRescueHeaderRemainder(line int) {
	p.recoverSameLineStatementRemainder(line)
}

func (p *parser) recoverSameLineStatementRemainder(line int) {
	for p.peekToken.Type != ast.TokenEOF && p.peekToken.Pos.Line == line && p.peekToken.Type != ast.TokenSemicolon {
		p.nextToken()
	}
}

func (p *parser) recoverToBlockEnd() {
	depth := 0
	pendingDoDepth := -1
	pendingDoLine := -1
	for p.curToken.Type != ast.TokenEOF {
		p.nextToken()
		if pendingDoLine != -1 && p.curToken.Pos.Line != pendingDoLine {
			pendingDoDepth = -1
			pendingDoLine = -1
		}
		switch p.curToken.Type {
		case ast.TokenDef, ast.TokenClass, ast.TokenEnum, ast.TokenBegin, ast.TokenIf, ast.TokenUnless, ast.TokenCase:
			depth++
		case ast.TokenFor, ast.TokenWhile, ast.TokenUntil:
			depth++
			pendingDoDepth = depth
			pendingDoLine = p.curToken.Pos.Line
		case ast.TokenDo:
			if pendingDoDepth == depth && pendingDoLine == p.curToken.Pos.Line {
				pendingDoDepth = -1
				pendingDoLine = -1
				break
			}
			depth++
		case ast.TokenEnd:
			if depth == 0 {
				return
			}
			depth--
			if pendingDoDepth > depth {
				pendingDoDepth = -1
				pendingDoLine = -1
			}
		}
	}
}

func (p *parser) validateRescueTypeExpr(ty *ast.TypeExpr, pos ast.Position) bool {
	if ty == nil {
		p.addParseError(pos, "rescue type cannot be empty")
		return false
	}

	if ty.Kind == ast.TypeUnion {
		ok := true
		for _, option := range ty.Union {
			if !p.validateRescueTypeExpr(option, option.Position) {
				ok = false
			}
		}
		return ok
	}

	if len(ty.TypeArgs) > 0 || len(ty.Shape) > 0 {
		p.addParseError(pos, "rescue type must be an error class, got %s", typeText{ty})
		return false
	}
	if _, ok := ast.CanonicalRuntimeErrorType(ty.Name); !ok {
		p.addParseError(pos, "unknown rescue error type %s", srcText(ty.Name))
		return false
	}
	return true
}

func (p *parser) parseFunctionStatement() ast.Statement {
	pos := p.curToken.Pos
	p.nextToken()

	isClassMethod := false
	isOperatorMethod := false
	var name string
	if p.curToken.Type == ast.TokenSelf && p.peekToken.Type == ast.TokenDot {
		isClassMethod = true
		p.nextToken() // consume dot
		if !p.expectPeek(ast.TokenIdent) {
			return nil
		}
		name = p.curToken.Literal
		p.nextToken()
	} else if opName, ok := p.parseOperatorMethodName(); ok {
		if !p.insideClass {
			p.addParseError(pos, "operator method %s must be defined in a class", opName)
			return nil
		}
		isOperatorMethod = true
		name = opName
	} else {
		if p.curToken.Type != ast.TokenIdent {
			p.errorExpected(p.curToken, "function name")
			return nil
		}
		name = p.curToken.Literal
		p.nextToken()
	}

	if p.curToken.Type == ast.TokenAssign && (!isOperatorMethod || name == "[]") {
		name += "="
		p.nextToken()
	}

	// Push the function scope before parsing parameters so each parameter is
	// visible to later parameters' default values, matching the runtime which
	// binds earlier parameters before evaluating later defaults. The scope is
	// also kept active through any rescue/else/ensure tail so the tail resolves
	// function-body locals for name-sensitive parsing such as percent-literal
	// vs modulo disambiguation.
	p.pushLocalScope(nil, true)
	defer p.popLocalScope()

	params := []ast.Param{}
	var returnTy *ast.TypeExpr
	// signatureEndLine tracks the line the def signature ends on so the
	// optional `-> Type` return annotation only binds when it sits on that
	// line. A `->` on a later line is the first body statement — a stabby
	// lambda literal — not an annotation.
	signatureEndLine := pos.Line
	// Optional parens on the same line.
	if p.curToken.Type == ast.TokenLParen && p.curToken.Pos.Line == pos.Line {
		if p.peekToken.Type == ast.TokenRParen {
			p.nextToken() // consume ')'
			signatureEndLine = p.curToken.Pos.Line
			p.nextToken()
		} else {
			p.nextToken()
			params = p.parseParams()
			if !p.expectPeek(ast.TokenRParen) {
				return nil
			}
			signatureEndLine = p.curToken.Pos.Line
			p.nextToken()
		}
	} else if p.curToken.Pos.Line == pos.Line && isFunctionParamStart(p.curToken.Type) {
		params = p.parseParamsWithOptions(paramParseOptions{lineLimitedDefaults: true})
		signatureEndLine = p.curToken.Pos.Line
		p.nextToken()
	}
	if p.curToken.Type == ast.TokenThinArrow && p.curToken.Pos.Line == signatureEndLine {
		p.nextToken()
		returnTy = p.parseTypeExpr()
		if returnTy == nil {
			return nil
		}
		p.nextToken()
	}
	// Expose the signature's parameters while the body parses, so a member
	// receiver captured inside it can be resolved against its declared type.
	savedParams := p.currentParams
	p.currentParams = params
	body := p.parseBlock(ast.TokenRescue, ast.TokenElse, ast.TokenEnsure, ast.TokenEnd)
	p.currentParams = savedParams
	switch p.curToken.Type {
	case ast.TokenRescue, ast.TokenElse, ast.TokenEnsure:
		tryStmt := p.parseRescueElseEnsureTail(pos, body, "function")
		if tryStmt == nil {
			return nil
		}
		body = []ast.Statement{tryStmt}
	case ast.TokenEnd:
	default:
		p.errorExpected(p.curToken, "end")
		return nil
	}

	visibility := ""
	if p.insideClass && p.pendingVisibility != "" {
		visibility = p.pendingVisibility
		p.pendingVisibility = ""
	}

	return &ast.FunctionStmt{Name: name, Params: params, ReturnTy: returnTy, Body: body, IsClassMethod: isClassMethod, Private: visibility == ast.VisibilityPrivate, Visibility: visibility, Position: pos}
}

// parseOperatorMethodName recognizes a Ruby operator token in def-name
// position (def +, def <<, def [], def []=) and consumes it, returning the
// method name the runtime dispatches on. Compound-assignment tokens (+=) and
// operators the grammar cannot dispatch (|, unary forms) are not accepted. The
// caller appends "=" for the []= form via the shared setter suffix handling.
func (p *parser) parseOperatorMethodName() (string, bool) {
	switch p.curToken.Type {
	case ast.TokenPlus, ast.TokenMinus, ast.TokenAsterisk, ast.TokenSlash, ast.TokenPercent,
		ast.TokenPower, ast.TokenShovel, ast.TokenAmpersand, ast.TokenEQ, ast.TokenNotEQ,
		ast.TokenLT, ast.TokenLTE, ast.TokenGT, ast.TokenGTE, ast.TokenSpaceship:
		name := p.curToken.Literal
		p.nextToken()
		return name, true
	case ast.TokenLBracket:
		if p.peekToken.Type != ast.TokenRBracket {
			return "", false
		}
		p.nextToken()
		p.nextToken()
		return "[]", true
	default:
		return "", false
	}
}

// moduleFunctionExample and namespaceCallExample are the two spellings the
// migration diagnostics recommend. They are named so a diagnostic cannot
// recommend code the parser rejects without a test noticing: every snippet a
// message can emit is derived from one of these or from moduleFunctionForm.
const (
	moduleFunctionExample = "def self.name"
	namespaceCallExample  = "Naming.display_name(person)"
)

// moduleNamespaceReplacement names what replaces every removed form of
// behavior injection: a module is a namespace, so the code it holds is
// reached by calling it on the module rather than by copying it into a class.
const moduleNamespaceReplacement = "define module functions with " + moduleFunctionExample +
	" and call them on the module (" + namespaceCallExample + ")"

// moduleFunctionForm spells the module-function declaration that replaces an
// instance-style method of the given name, and reports whether that spelling
// exists at all. A `def self.` name is an identifier, so a method the grammar
// names with an operator (+, <<, [], []=) has no module-function form and must
// be recommended something else. The question goes to the lexer rather than to
// a second copy of the identifier rule, so an operator the grammar gains later
// is covered without editing this.
func moduleFunctionForm(name string) (string, bool) {
	base := strings.TrimSuffix(name, "=")
	if base == "" {
		return "", false
	}
	l := newLexer(base)
	if tok := l.NextToken(); tok.Type != ast.TokenIdent || tok.Literal != base {
		return "", false
	}
	if l.NextToken().Type != ast.TokenEOF {
		return "", false
	}
	return "def self." + name, true
}

// moduleNamespaceHint states the rule and the replacement together, for the
// diagnostics that need both.
const moduleNamespaceHint = "modules are namespaces: " + moduleNamespaceReplacement

// The removed behavior-injection directives. The words are contextual, so
// they are still recognized in class-member position to report the
// replacement rather than fail as an unknown call.
const (
	mixinIncludeWord = "include"
	mixinExtendWord  = "extend"
)

func (p *parser) parseClassStatement() ast.Statement {
	pos := p.curToken.Pos
	if p.peekToken.Type == ast.TokenShovel {
		p.addParseError(p.peekToken.Pos, "class << self definitions are not supported; use def self.name")
		return nil
	}
	if !p.expectPeek(ast.TokenIdent) {
		return nil
	}
	// `class B < A` is a syntax error that says nothing about why. Inheritance
	// is deliberately absent and shared behavior lives in a module namespace,
	// so the error names the decision rather than leaving the author to assume
	// a syntax slip and retry variants.
	if p.peekToken.Type == ast.TokenLT {
		p.addParseError(p.peekToken.Pos, "class inheritance is not supported; "+moduleNamespaceHint)
		return nil
	}
	return p.parseClassLikeBody(pos, false)
}

// parseModuleStatement parses a `module Name ... end` declaration with
// curToken on the contextual `module` keyword. Module names are constants, so
// they must start with an uppercase letter.
func (p *parser) parseModuleStatement() ast.Statement {
	pos := p.curToken.Pos
	if !p.expectPeek(ast.TokenIdent) {
		return nil
	}
	if !startsUppercaseIdentifier(p.curToken.Literal) {
		p.addParseError(p.curToken.Pos, "module name must start with an uppercase letter")
		return nil
	}
	return p.parseClassLikeBody(pos, true)
}

// peekStartsModuleDeclaration reports whether a `module` identifier at
// curToken is followed by a declaration name on the same line, i.e. the token
// sequence reads as a module declaration rather than an unrelated expression
// (`module = require(...)`, `module.helper`).
func (p *parser) peekStartsModuleDeclaration() bool {
	return p.peekToken.Type == ast.TokenIdent && p.peekToken.Pos.Line == p.curToken.Pos.Line
}

// parseModuleDeclarationStatement handles a module declaration recognized in
// general statement position. Declarations are only legal at the top level
// (module bodies parse their nested declarations directly), so class bodies,
// function bodies, and other nested contexts get a targeted diagnostic.
func (p *parser) parseModuleDeclarationStatement() ast.Statement {
	if p.insideClass || p.statementNesting > 0 {
		p.addParseError(p.curToken.Pos, "module declarations are only supported at the top level and nested in module bodies")
		return nil
	}
	return p.parseModuleStatement()
}

// parseClassLikeBody parses the shared class/module body with curToken on the
// declaration name. Module bodies additionally accept nested module
// declarations and reject class declarations.
func (p *parser) parseClassLikeBody(pos ast.Position, isModule bool) ast.Statement {
	if !p.enterSyntax() {
		return nil
	}
	defer func() { p.syntaxDepth-- }()

	name := p.curToken.Literal
	p.nextToken()

	stmt := &ast.ClassStmt{
		Name:     name,
		IsModule: isModule,
		Position: pos,
	}

	prevInside := p.insideClass
	prevVisibility := p.pendingVisibility
	p.insideClass = true
	p.pendingVisibility = ""
	p.statementNesting++
	// Body-level assignments are the class's constants: scope them to the
	// body so they stay visible inside method definitions (isLocalName
	// crosses the funcDef boundary into class-body scopes) without leaking
	// into the enclosing scope after `end`.
	p.pushClassBodyScope()
	defer func() {
		p.popLocalScope()
		p.statementNesting--
	}()

	for p.curToken.Type != ast.TokenEnd && p.curToken.Type != ast.TokenEOF {
		p.skipStatementSeparators()
		if p.curToken.Type == ast.TokenEnd || p.curToken.Type == ast.TokenEOF {
			break
		}
		switch p.curToken.Type {
		case ast.TokenClass:
			if isModule {
				p.addParseError(p.curToken.Pos, "class declarations are not supported in module bodies")
				return nil
			}
			s := p.parseStatement()
			if s != nil {
				stmt.Body = append(stmt.Body, s)
			}
		case ast.TokenDef:
			fnStmt := p.parseFunctionStatement()
			if fnStmt == nil {
				return nil
			}
			fn := fnStmt.(*ast.FunctionStmt)
			if isModule && !fn.IsClassMethod {
				if form, ok := moduleFunctionForm(fn.Name); ok {
					p.addParseError(fn.Position, "def %s in module %s must be %s; a module is a namespace, not a method source", srcText(fn.Name), srcText(name), srcText(form))
				} else {
					p.addParseError(fn.Position, "operator method %s is not supported in module %s; an operator dispatches on an instance and a module has none, so define %s on a class", srcText(fn.Name), srcText(name), srcText(fn.Name))
				}
				break
			}
			if fn.IsClassMethod {
				stmt.ClassMethods = append(stmt.ClassMethods, fn)
			} else {
				stmt.Methods = append(stmt.Methods, fn)
			}
			stmt.Members = append(stmt.Members, ast.ClassMemberDecl{Function: fn})
		case ast.TokenIdent:
			switch p.curToken.Literal {
			case "alias":
				if p.peekStartsAliasNameOnLine(p.curToken.Pos.Line) {
					alias := p.parseAliasStatement(true)
					if alias == nil {
						return nil
					}
					aliasStmt := alias.(*ast.AliasStmt)
					if isModule {
						p.addParseError(aliasStmt.Pos(), "alias in module %s is not supported; a module has no instance methods to rename, so %s", srcText(name), moduleNamespaceReplacement)
						break
					}
					stmt.Aliases = append(stmt.Aliases, aliasStmt)
					stmt.Members = append(stmt.Members, ast.ClassMemberDecl{Alias: aliasStmt})
				} else {
					s := p.parseStatement()
					if s != nil {
						stmt.Body = append(stmt.Body, s)
					}
				}
			case "alias_method":
				alias := p.parseAliasMethodStatement()
				if alias == nil {
					return nil
				}
				aliasStmt := alias.(*ast.AliasStmt)
				if isModule {
					p.addParseError(aliasStmt.Pos(), "alias_method in module %s is not supported; a module has no instance methods to rename, so %s", srcText(name), moduleNamespaceReplacement)
					break
				}
				stmt.Aliases = append(stmt.Aliases, aliasStmt)
				stmt.Members = append(stmt.Members, ast.ClassMemberDecl{Alias: aliasStmt})
			case ast.VisibilityPublic, ast.VisibilityProtected:
				if p.startsVisibilityDirective() {
					if p.parseVisibilityMember(stmt, p.curToken.Literal) {
						continue
					}
				} else {
					s := p.parseStatement()
					if s != nil {
						stmt.Body = append(stmt.Body, s)
					}
				}
			case "module":
				if isModule && p.peekStartsModuleDeclaration() {
					nested := p.parseModuleStatement()
					if nested == nil {
						return nil
					}
					stmt.Modules = append(stmt.Modules, nested.(*ast.ClassStmt))
				} else {
					// Class bodies (and non-declaration uses) route through the
					// general statement path, which reports the targeted
					// module-placement diagnostic for declaration-shaped input.
					s := p.parseStatement()
					if s != nil {
						stmt.Body = append(stmt.Body, s)
					}
				}
			case mixinIncludeWord, mixinExtendWord:
				if p.startsMixinDirective() {
					p.reportRemovedMixinDirective()
					break
				}
				s := p.parseStatement()
				if s != nil {
					stmt.Body = append(stmt.Body, s)
				}
			default:
				s := p.parseStatement()
				if s != nil {
					stmt.Body = append(stmt.Body, s)
				}
			}
		case ast.TokenPrivate:
			if p.parseVisibilityMember(stmt, ast.VisibilityPrivate) {
				continue
			}
		case ast.TokenProperty, ast.TokenGetter, ast.TokenSetter:
			kind := p.curToken.Literal
			decl := p.parsePropertyDecl(p.curToken.Type)
			if p.pendingVisibility != "" {
				decl.Visibility = p.pendingVisibility
				p.pendingVisibility = ""
			}
			if isModule {
				p.addParseError(decl.Position, "%s in module %s is not supported; a module has no instances, so %s", srcText(kind), srcText(name), moduleNamespaceReplacement)
				break
			}
			stmt.Properties = append(stmt.Properties, decl)
			stmt.Members = append(stmt.Members, ast.ClassMemberDecl{Property: &decl})
		default:
			s := p.parseStatement()
			if s != nil {
				stmt.Body = append(stmt.Body, s)
			}
		}
		p.nextToken()
	}

	p.insideClass = prevInside
	p.pendingVisibility = prevVisibility

	if p.curToken.Type != ast.TokenEnd {
		p.errorExpected(p.curToken, "end")
	}

	return stmt
}

// startsMixinDirective reports whether an `include`/`extend` identifier in
// class-member position is written as a mixin directive rather than as an
// ordinary statement. Both directives are removed, so the shape is still
// recognized only to report where behavior injection went; see
// reportRemovedMixinDirective. It is a directive when followed on the same
// line by a module name, an opening paren, or `self`, or when it stands alone
// — unless a local of that name is in scope, in which case the bare word reads
// the local as it always has. Anything else — an assignment such as
// `include = 1` — parses as a normal statement so the words stay usable as
// identifiers.
func (p *parser) startsMixinDirective() bool {
	if p.peekToken.Pos.Line == p.curToken.Pos.Line {
		switch p.peekToken.Type {
		case ast.TokenIdent, ast.TokenLParen, ast.TokenSelf:
			return true
		}
	}
	return !p.isLocalName(p.curToken.Literal) && p.peekEndsStatement(p.curToken.Pos)
}

// reportRemovedMixinDirective reports an include/extend directive with
// curToken on the directive word, and consumes the rest of the line so the
// module list that follows does not produce a second, unrelated error.
func (p *parser) reportRemovedMixinDirective() {
	pos := p.curToken.Pos
	p.addParseError(pos, "%s is not supported; %s", srcText(p.curToken.Literal), moduleNamespaceHint)
	p.recoverSameLineStatementRemainder(pos.Line)
}

// startsVisibilityDirective reports whether a `public`/`protected` identifier
// in class-member position begins a visibility directive rather than an
// ordinary statement. The identifier is a directive when it stands alone
// (section form), precedes a definition (`public def`), or precedes symbol
// method names (`protected :compare`). A local of the same name suppresses
// the section form — a bare word after `protected = 5` reads the local as it
// always has, matching Ruby, where only the argument forms reach the
// directive method. Anything else — an assignment such as `public = 1` or an
// unrelated expression — parses as a normal statement so the words stay
// usable as plain identifiers outside directive positions.
func (p *parser) startsVisibilityDirective() bool {
	if p.startsVisibilityInlineTarget() {
		return true
	}
	if p.peekToken.Type == ast.TokenSymbol && p.peekToken.Pos.Line == p.curToken.Pos.Line {
		return true
	}
	return !p.isLocalName(p.curToken.Literal) && p.peekEndsStatement(p.curToken.Pos)
}

// startsVisibilityInlineTarget reports whether the token after a visibility
// keyword begins a declaration the modifier applies to inline: a method
// definition (`private def hidden`) or an accessor declaration
// (`private property secret`). The declaration must start on the same line as
// the modifier — a visibility word alone on its own line is a section
// directive that covers every following definition, matching Ruby.
func (p *parser) startsVisibilityInlineTarget() bool {
	if p.peekToken.Pos.Line != p.curToken.Pos.Line {
		return false
	}
	switch p.peekToken.Type {
	case ast.TokenDef, ast.TokenProperty, ast.TokenGetter, ast.TokenSetter:
		return true
	default:
		return false
	}
}

// parseVisibilityMember handles a visibility keyword in class-member position.
// It returns true when the directive inlines onto the next declaration
// (`private def`, `public def self.x`, `private property secret`), leaving
// curToken on the declaration keyword so the class-body loop parses it next
// without advancing. Otherwise
// it appends a section directive (bare `private`) or a named directive
// (`private :hidden, :other`) to the class members and returns false.
func (p *parser) parseVisibilityMember(stmt *ast.ClassStmt, level string) bool {
	pos := p.curToken.Pos
	if p.startsVisibilityInlineTarget() {
		p.pendingVisibility = level
		p.nextToken()
		return true
	}

	decl := &ast.VisibilityDecl{Level: level, Position: pos}
	if p.peekToken.Type == ast.TokenSymbol && p.peekToken.Pos.Line == pos.Line {
		for {
			p.nextToken()
			decl.Names = append(decl.Names, p.curToken.Literal)
			if p.peekToken.Type != ast.TokenComma {
				break
			}
			p.nextToken()
			if p.peekToken.Type != ast.TokenSymbol {
				p.errorExpected(p.peekToken, "method name symbol")
				return false
			}
		}
	} else if !p.peekEndsStatement(pos) {
		p.addParseError(p.peekToken.Pos, "%s expects a method definition, symbol method names, or no argument", level)
		p.recoverSameLineStatementRemainder(pos.Line)
		return false
	}
	stmt.Members = append(stmt.Members, ast.ClassMemberDecl{Visibility: decl})
	return false
}

func (p *parser) parseAliasStatement(method bool) ast.Statement {
	pos := p.curToken.Pos
	if !p.expectPeekAliasName(pos.Line) {
		return nil
	}
	newName := p.curToken.Literal
	if !p.expectPeekAliasName(pos.Line) {
		return nil
	}
	oldName := p.curToken.Literal
	return &ast.AliasStmt{NewName: newName, OldName: oldName, Method: method, Position: pos}
}

func (p *parser) canParseTopLevelAliasStatement() bool {
	return !p.insideClass && p.statementNesting == 0
}

func (p *parser) parseUnsupportedAliasStatement() ast.Statement {
	pos := p.curToken.Pos
	p.addParseError(pos, "alias declarations are only supported at the top level or in class bodies")
	if p.peekStartsAliasNameOnLine(pos.Line) {
		p.nextToken()
	}
	if p.peekStartsAliasNameOnLine(pos.Line) {
		p.nextToken()
	}
	return nil
}

func (p *parser) parseAliasMethodStatement() ast.Statement {
	pos := p.curToken.Pos
	parenthesized := false
	if p.peekToken.Type == ast.TokenLParen {
		parenthesized = true
		p.nextToken()
	}
	if !p.expectPeek(ast.TokenSymbol) {
		return nil
	}
	newName := p.curToken.Literal
	if !p.expectPeek(ast.TokenComma) {
		return nil
	}
	if !p.expectPeek(ast.TokenSymbol) {
		return nil
	}
	oldName := p.curToken.Literal
	if parenthesized && !p.expectPeek(ast.TokenRParen) {
		return nil
	}
	return &ast.AliasStmt{NewName: newName, OldName: oldName, Method: true, Position: pos}
}

func (p *parser) expectPeekAliasName(line int) bool {
	if !p.peekStartsAliasNameOnLine(line) {
		p.errorExpected(p.peekToken, "alias name")
		return false
	}
	p.nextToken()
	return true
}

func (p *parser) peekStartsAliasName() bool {
	return p.peekToken.Type == ast.TokenIdent || p.peekToken.Type == ast.TokenSymbol
}

func (p *parser) peekStartsAliasNameOnLine(line int) bool {
	return p.peekToken.Pos.Line == line && p.peekStartsAliasName()
}

func (p *parser) parseEnumStatement() ast.Statement {
	pos := p.curToken.Pos
	if p.insideClass || p.statementNesting > 0 {
		p.addParseError(pos, "enum is only supported at the top level")
		return nil
	}
	if !p.expectPeek(ast.TokenIdent) {
		return nil
	}
	name := p.curToken.Literal
	p.nextToken()

	stmt := &ast.EnumStmt{
		Name:     name,
		Members:  make([]ast.EnumMemberStmt, 0),
		Position: pos,
	}
	memberNames := make(map[string]struct{})

	for p.curToken.Type != ast.TokenEnd && p.curToken.Type != ast.TokenEOF {
		p.skipStatementSeparators()
		if p.curToken.Type == ast.TokenEnd || p.curToken.Type == ast.TokenEOF {
			break
		}
		if p.curToken.Type != ast.TokenIdent && p.curToken.Type != ast.TokenEnum {
			p.errorExpected(p.curToken, "enum member name")
			return nil
		}
		member := ast.EnumMemberStmt{
			Name:     p.curToken.Literal,
			Position: p.curToken.Pos,
		}
		if _, exists := memberNames[member.Name]; exists {
			p.addParseError(member.Position, "duplicate enum member %s", srcText(member.Name))
			return nil
		}
		memberNames[member.Name] = struct{}{}
		stmt.Members = append(stmt.Members, member)
		p.nextToken()
	}

	if len(stmt.Members) == 0 {
		p.addParseError(pos, "enum %s must define at least one member", srcText(name))
		return nil
	}
	if p.curToken.Type != ast.TokenEnd {
		p.errorExpected(p.curToken, "end")
	}

	return stmt
}

func (p *parser) parseExportStatement() ast.Statement {
	pos := p.curToken.Pos
	if p.insideClass || p.statementNesting > 0 {
		p.addParseError(pos, "export is only supported for top-level functions")
		return nil
	}
	if !p.expectPeek(ast.TokenDef) {
		return nil
	}
	fnStmt := p.parseFunctionStatement()
	if fnStmt == nil {
		return nil
	}
	fn, ok := fnStmt.(*ast.FunctionStmt)
	if !ok {
		p.addParseError(pos, "export expects a function definition")
		return nil
	}
	if fn.IsClassMethod {
		p.addParseError(pos, "export cannot be used with class methods")
		return nil
	}
	fn.Exported = true
	return fn
}

func (p *parser) parsePrivateStatement() ast.Statement {
	pos := p.curToken.Pos
	if p.insideClass || p.statementNesting > 0 {
		p.addParseError(pos, "private is only supported for top-level functions and class methods")
		return nil
	}
	if !p.expectPeek(ast.TokenDef) {
		return nil
	}
	fnStmt := p.parseFunctionStatement()
	if fnStmt == nil {
		return nil
	}
	fn, ok := fnStmt.(*ast.FunctionStmt)
	if !ok {
		p.addParseError(pos, "private expects a function definition")
		return nil
	}
	if fn.IsClassMethod {
		p.addParseError(pos, "private cannot be used with class methods")
		return nil
	}
	fn.Private = true
	return fn
}

type paramParseOptions struct {
	lineLimitedDefaults bool
}

func (p *parser) parseParams() []ast.Param {
	return p.parseParamsWithOptions(paramParseOptions{})
}

func (p *parser) parseParamsWithOptions(options paramParseOptions) []ast.Param {
	params := []ast.Param{}
	seenRest := false
	seenKeyword := false
	seenKeywordRest := false
	for {
		param, paramPos, ok := p.parseParam(options)
		if !ok {
			return params
		}
		switch param.Kind {
		case ast.ParamNormal:
			if seenRest || seenKeyword || seenKeywordRest {
				format, args := ordinaryParamOrderMessage(param, params)
				p.addParseError(paramPos, format, args...)
				return params
			}
		case ast.ParamRest:
			if seenRest {
				p.addParseError(paramPos, "duplicate rest parameter")
				return params
			}
			if seenKeyword || seenKeywordRest {
				p.addParseError(paramPos, "rest parameter must precede keyword and keyword rest parameters")
				return params
			}
			seenRest = true
		case ast.ParamKeyword:
			if seenKeywordRest {
				p.addParseError(paramPos, "keyword parameters must precede keyword rest parameters")
				return params
			}
			seenKeyword = true
		case ast.ParamKeywordRest:
			if seenKeywordRest {
				p.addParseError(paramPos, "duplicate keyword rest parameter")
				return params
			}
			seenKeywordRest = true
		}

		params = append(params, param)
		// Declare the parameter as a local now so later parameters' default
		// values resolve it (see parseFunctionStatement for why the scope is
		// already active here).
		p.declareParamLocal(param)
		if p.peekToken.Type != ast.TokenComma {
			break
		}
		p.nextToken()
		p.nextToken()
	}
	return params
}

func (p *parser) parseParam(options paramParseOptions) (ast.Param, ast.Position, bool) {
	kind := ast.ParamNormal
	switch p.curToken.Type {
	case ast.TokenAsterisk:
		kind = ast.ParamRest
		if p.peekToken.Type == ast.TokenAsterisk {
			kind = ast.ParamKeywordRest
			p.nextToken()
		}
		p.nextToken()
	case ast.TokenPower:
		kind = ast.ParamKeywordRest
		p.nextToken()
	case ast.TokenAmpersand:
		// A block capture parameter turned the caller's block into a value the
		// callee could store, return, or forward. ADR-006 removed it; `yield`
		// runs the caller's block where it was supplied.
		p.addParseError(p.curToken.Pos, "block capture parameters are not supported; a block is not a value. Run the caller's block with `yield`, and ask `block_given?` when it is optional")
		return ast.Param{}, ast.Position{}, false
	}

	if p.curToken.Type != ast.TokenIdent && (kind != ast.ParamNormal || p.curToken.Type != ast.TokenIvar) {
		p.errorExpected(p.curToken, parameterNameExpectation(kind))
		return ast.Param{}, ast.Position{}, false
	}
	pos := p.curToken.Pos
	param := ast.Param{Name: p.curToken.Literal, Kind: kind}
	if p.curToken.Type == ast.TokenIvar {
		param.IsIvar = true
		param.Name = strings.TrimPrefix(param.Name, "@")
	}
	if p.peekToken.Type == ast.TokenColon {
		p.nextToken()
		if kind == ast.ParamNormal && !param.IsIvar && p.peekEndsRequiredKeywordParam(options) {
			param.Kind = ast.ParamKeyword
			return param, pos, true
		}
		// `name: default` declares an optional keyword-only parameter, while
		// `name: Type` declares a typed positional parameter. The token after
		// the colon disambiguates the two forms; see colonIntroducesKeywordDefault.
		if kind == ast.ParamNormal && !param.IsIvar && p.colonIntroducesKeywordDefault(options) {
			param.Kind = ast.ParamKeyword
			p.nextToken()
			if options.lineLimitedDefaults {
				param.DefaultVal = p.parseLineExpressionUntil(lowestPrec, ast.TokenComma)
			} else {
				param.DefaultVal = p.parseExpression(lowestPrec)
			}
			return param, pos, true
		}
		p.nextToken()
		param.Type = p.parseTypeExpr()
		if param.Type == nil {
			return ast.Param{}, ast.Position{}, false
		}
		if format, args := captureParamTypeError(param); format != "" {
			p.addParseError(param.Type.Position, format, args...)
			return ast.Param{}, ast.Position{}, false
		}
		if kind == ast.ParamNormal && !param.IsIvar && p.peekToken.Type == ast.TokenColon {
			p.nextToken()
			if !p.peekEndsRequiredKeywordParam(options) {
				p.addParseError(p.curToken.Pos, "typed required keyword parameter must end after trailing ':'")
				return ast.Param{}, ast.Position{}, false
			}
			param.Kind = ast.ParamKeyword
		}
	}
	if p.peekToken.Type == ast.TokenAssign {
		if kind != ast.ParamNormal {
			p.addParseError(p.peekToken.Pos, "capture parameters cannot have default values")
			return ast.Param{}, ast.Position{}, false
		}
		p.nextToken()
		p.nextToken()
		if options.lineLimitedDefaults {
			param.DefaultVal = p.parseLineExpressionUntil(lowestPrec, ast.TokenComma)
		} else {
			param.DefaultVal = p.parseExpression(lowestPrec)
		}
	}
	return param, pos, true
}

// captureParamTypeError reports the diagnostic for a capture parameter whose
// annotation cannot hold what it captures, as a format and its arguments so
// the message is built only under the error budget. An empty format means the
// annotation is acceptable.
func captureParamTypeError(param ast.Param) (string, []any) {
	switch param.Kind {
	case ast.ParamRest:
		if !restCaptureTypeAcceptsArray(param.Type) {
			return "rest parameter %s captures an array; annotate it as array<...> or any", []any{srcText(param.Name)}
		}
	case ast.ParamKeywordRest:
		if !keywordRestCaptureTypeAcceptsHash(param.Type) {
			return "keyword rest parameter %s captures a hash; annotate it as hash<...>, object, a shape type, or any", []any{srcText(param.Name)}
		}
	}
	return "", nil
}

func restCaptureTypeAcceptsArray(ty *ast.TypeExpr) bool {
	if ty == nil {
		return true
	}
	switch ty.Kind {
	case ast.TypeAny, ast.TypeArray:
		return true
	case ast.TypeUnion:
		if slices.ContainsFunc(ty.Union, restCaptureTypeAcceptsArray) {
			return true
		}
	}
	return false
}

func keywordRestCaptureTypeAcceptsHash(ty *ast.TypeExpr) bool {
	if ty == nil {
		return true
	}
	switch ty.Kind {
	case ast.TypeAny, ast.TypeHash, ast.TypeShape:
		return true
	case ast.TypeUnion:
		if slices.ContainsFunc(ty.Union, keywordRestCaptureTypeAcceptsHash) {
			return true
		}
	}
	return false
}

func isFunctionParamStart(tt ast.TokenType) bool {
	switch tt {
	case ast.TokenIdent, ast.TokenIvar, ast.TokenAsterisk, ast.TokenPower, ast.TokenAmpersand:
		return true
	default:
		return false
	}
}

func (p *parser) peekEndsRequiredKeywordParam(options paramParseOptions) bool {
	switch p.peekToken.Type {
	case ast.TokenComma, ast.TokenRParen:
		return true
	case ast.TokenThinArrow, ast.TokenSemicolon, ast.TokenEOF:
		return options.lineLimitedDefaults
	default:
		return options.lineLimitedDefaults && p.peekToken.Pos.Line != p.curToken.Pos.Line
	}
}

// colonIntroducesKeywordDefault reports whether the token sequence following a
// parameter's `:` introduces an optional keyword-only default value rather than
// a type annotation. It is consulted only after peekEndsRequiredKeywordParam has
// ruled out the bare `name:` required-keyword form, so the colon is always
// followed by at least one more token here.
//
// Vibescript overloads the post-colon position: `name: Type` is a typed
// positional parameter while `name: default` is an optional keyword-only
// parameter. The decision rests on whether the colon is followed by something
// that can begin a type annotation:
//
//   - A value-only token (a literal, prefix operator, grouping, etc.) cannot
//     start a type, so it begins a default value.
//   - `nil` reads as a default value (`a: nil`), matching Ruby and the stdlib's
//     documented optional keywords; a bare `nil` positional type is useless.
//     A `|` continuation keeps it a type, so a nil-leading union annotation
//     (`a: nil | int`) parses as the union rather than a `nil` default.
//   - `{` opens either a shape type (`name: { field: Type }`) or a hash literal
//     default (`name: { key: value }`). The two share a `{ name: X }` skeleton,
//     so a bounded speculative parse decides: the brace group reads as a shape
//     type only when it parses as one and is followed by a parameter boundary,
//     otherwise it is a hash default.
//   - A bare identifier is the genuinely ambiguous case. It reads as a type when
//     it stands alone at a parameter boundary or continues as a type (`<` for
//     generic arguments on a container type, `|` for a union). Otherwise the
//     identifier begins an expression (`a + 1`, `helper(x)`, `obj.value`) and is
//     treated as a default value.
func (p *parser) colonIntroducesKeywordDefault(options paramParseOptions) bool {
	switch p.peekToken.Type {
	case ast.TokenNil:
		// A bare `nil` is a default value, but a `|` continuation makes it the
		// head of a nil-leading union type annotation (`a: nil | int`).
		return p.peekPeek.Type != ast.TokenPipe
	case ast.TokenLBrace:
		return !p.bracedGroupIsShapeType(options)
	case ast.TokenIdent:
		return p.identAfterColonStartsExpression(options)
	default:
		return prefixParserKind(p.peekToken.Type) != prefixParserNone
	}
}

// bracedGroupIsShapeType reports whether the brace group beginning at
// peekToken is a shape type rather than a hash literal default. The two
// share a `{ field: X }` skeleton, so a bounded speculative parse decides:
// the group is a shape type only when the whole group parses as one,
// because a shape type's field values are themselves types all the way
// down. `{ x: int }` parses as a shape, while `{ retry: 3 }` does not (an
// integer is not a type) and `{ a: { b: 1 } }` does not (the nested
// integer is not a type), so both are hash defaults.
//
// A clean shape parse is necessary but not sufficient: a hash default
// whose values happen to be type names (`{ x: int }.merge(...)`) parses
// as a shape but continues with a postfix expression. The group is a
// shape type only when it also reaches a parameter boundary (`,` `)` `=`,
// or the line-limited terminators), so a postfix continuation marks the
// group as a hash default instead.
//
// The empty group `{}` is treated as a hash default rather than an empty
// shape type. An empty shape type as a parameter annotation is degenerate
// (it accepts only an empty hash), whereas `opts: {}` is the common
// Ruby-style empty-hash default. An empty *nested* shape is degenerate for
// the same reason: `{ headers: {} }` reads as a shape whose `headers` field
// accepts only an empty hash, but the Ruby-style intent is the hash default
// `def f(opts: { headers: {} })`. shapeHasEmptyNestedShape marks such groups
// as hash defaults too, mirroring the top-level `{}` case at any depth.
//
// A clean shape parse with a bare `nil` field type is likewise degenerate:
// `{ previous: nil }` reads as a shape whose `previous` field accepts only
// nil, but the Ruby-style intent is the hash default
// `def f(opts: { previous: nil })`. shapeHasDegenerateNilField marks such
// groups as hash defaults; a nullable union field (`{ previous: int | nil }`)
// is a legitimate shape and is left untouched.
//
// The parser state is fully restored afterward regardless of the outcome,
// leaving the real parse to proceed from the colon.
func (p *parser) bracedGroupIsShapeType(options paramParseOptions) bool {
	if p.peekPeek.Type == ast.TokenRBrace {
		return false
	}

	saved := p.snapshot()
	defer p.restore(saved)

	p.nextToken()
	shape := p.parseTypeExpr()
	if shape == nil {
		// A clean failure where the contents were not types (`{ retry: 3 }`)
		// is a hash default. A structural shape error (`{ id: string, id: int }`)
		// instead has field values that all parsed as types, so the braces are a
		// malformed shape annotation, not a hash default: route them to the type
		// path so the real parse re-emits the shape diagnostic rather than
		// silently turning a typed positional parameter into a keyword default.
		return p.shapeStructurallyInvalid
	}
	if len(p.errors) != saved.errorCount {
		return false
	}
	if shapeHasDegenerateNilField(shape) {
		return false
	}
	if shapeHasEmptyNestedShape(shape) {
		return false
	}
	if p.shapeFieldNamesLocalValue(shape) {
		return false
	}
	return p.typeAnnotationBoundaryFollows(options)
}

// shapeFieldNamesLocalValue reports whether ty is a shape type with a bare
// identifier field type that names a local value in scope, looking through
// nested shapes.
//
// A bare identifier field such as the `a` in `{ sum: a }` parses cleanly as a
// type atom, so the speculative shape parse alone cannot tell `def f(opts: {
// status: pending })` (a shape whose `status` field is the `pending` enum type)
// from `def g(a:, b: { sum: a })` (a hash default whose `sum` value references
// the earlier keyword parameter `a`). When the identifier names a value already
// in scope it is a value reference, so the brace group is a hash default rather
// than a shape type. This mirrors identAfterColonStartsExpression and
// identLessThanStartsExpression, which already treat a bare identifier naming a
// local value as the head of a default expression.
//
// A bare identifier whose spelling matches a built-in type (such as the `time`
// in `def f(time:, opts: { start: time })`) parses to that built-in's kind
// (TypeTime here) while keeping the original literal in Name, so the check
// inspects the name rather than the kind. This covers common parameter names
// like time, string, int, hash, and array.
//
// The check targets bare atoms only. A union field (`{ x: a | b }`) stays a
// type because `|` continues a type annotation; a nullable atom (`{ x: a? }`)
// is type syntax; and an identifier appearing as a generic type argument
// (`{ x: array<a> }`) is a genuine type, matching how those forms are
// disambiguated outside shapes.
func (p *parser) shapeFieldNamesLocalValue(ty *ast.TypeExpr) bool {
	if ty == nil || ty.Kind != ast.TypeShape || ty.Open {
		// An open shape's `...` marker rules out any hash-literal reading:
		// even a field naming a local value cannot be a hash default here.
		return false
	}
	for _, fieldType := range ty.Shape {
		if fieldType == nil {
			continue
		}
		if p.typeAtomNamesLocalValue(fieldType) {
			return true
		}
		if p.shapeFieldNamesLocalValue(fieldType) {
			return true
		}
	}
	return false
}

// typeAtomNamesLocalValue reports whether ty is a bare named type atom whose
// spelling names a local value in scope. A bare atom is a single identifier
// with no type modifiers: it carries no generic arguments, is not nullable, and
// is neither a shape nor a union (those forms are unambiguous type syntax). The
// spelling is checked rather than the resolved kind so that an identifier
// matching a built-in type name (resolved to that built-in's kind) is still
// recognized as a value reference when it names a local.
func (p *parser) typeAtomNamesLocalValue(ty *ast.TypeExpr) bool {
	if ty == nil || ty.Name == "" || ty.Nullable || len(ty.TypeArgs) > 0 {
		return false
	}
	switch ty.Kind {
	case ast.TypeShape, ast.TypeUnion:
		return false
	default:
		return p.isLocalName(ty.Name)
	}
}

// bracedFieldIsHashDefault reports whether a braced-group field value, parsed as
// a type expression during the speculative shape parse, actually reads as a hash
// default rather than a shape field type. A value qualifies when it is:
//
//   - a bare identifier naming a local value (typeAtomNamesLocalValue),
//   - a bare `nil` atom (the degenerate field type recognized by
//     shapeHasDegenerateNilField),
//   - an empty shape `{}` (the degenerate empty-hash default), or
//   - a nested shape that is itself a hash default, i.e. one with a degenerate
//     `nil` field (shapeHasDegenerateNilField), an empty nested shape
//     (shapeHasEmptyNestedShape), or a field naming a local value
//     (shapeFieldNamesLocalValue).
//
// The nested-shape cases mirror the disambiguation bracedGroupIsShapeType
// applies to the whole group, so a nested value like `{ previous: nil }` or
// `{ sum: a }` is recognized as a hash default at any depth, not only when it is
// the outermost group.
//
// parseTypeShape uses it on the repeated values of a duplicate key so that a
// group like `{ previous: nil, previous: nil }`, `{ x: a, x: a }`, or
// `{ x: { previous: nil }, x: { previous: nil } }` is left to fall back to a
// hash default instead of being marked a structural shape error.
func (p *parser) bracedFieldIsHashDefault(ty *ast.TypeExpr) bool {
	if ty == nil {
		return false
	}
	if ty.Kind == ast.TypeNil {
		return true
	}
	if typeIsEmptyShape(ty) {
		return true
	}
	if p.typeAtomNamesLocalValue(ty) {
		return true
	}
	return shapeHasDegenerateNilField(ty) ||
		shapeHasEmptyNestedShape(ty) ||
		p.shapeFieldNamesLocalValue(ty)
}

// shapeHasDegenerateNilField reports whether ty is a shape type with a field
// whose type is the bare `nil` atom, looking through nested shapes.
//
// A `nil`-only field type accepts solely the value nil, which is degenerate as
// a positional annotation, so a group like `{ previous: nil }` is the common
// Ruby-style hash default (`def f(opts: { previous: nil })`) rather than a
// shape type. The check targets the bare atom only: a nullable union such as
// `{ previous: int | nil }` is a legitimate shape field and is left untouched.
func shapeHasDegenerateNilField(ty *ast.TypeExpr) bool {
	if ty == nil || ty.Kind != ast.TypeShape || ty.Open {
		// The `...` rest marker has no hash-literal reading, so an open
		// shape's fields are never hash-default evidence: the marker itself
		// proves shape intent, nil-only fields included.
		return false
	}
	for _, fieldType := range ty.Shape {
		if fieldType == nil {
			continue
		}
		if fieldType.Kind == ast.TypeNil || shapeHasDegenerateNilField(fieldType) {
			return true
		}
	}
	return false
}

// typeIsEmptyShape reports whether ty is the empty shape `{}`, i.e. a shape
// type with no fields. An empty shape is degenerate as a parameter annotation
// (it accepts only an empty hash), so it is the Ruby-style empty-hash default
// rather than a meaningful shape type, matching the top-level `{}` handling in
// bracedGroupIsShapeType. The open shape `{ ... }` is excluded: it accepts any
// hash, is meaningful as an annotation, and has no hash-literal reading.
func typeIsEmptyShape(ty *ast.TypeExpr) bool {
	return ty != nil && ty.Kind == ast.TypeShape && len(ty.Shape) == 0 && !ty.Open
}

// shapeHasEmptyNestedShape reports whether ty is a shape type with a field
// whose type is an empty shape `{}`, looking through nested shapes.
//
// An empty shape field type accepts solely an empty hash, which is degenerate
// as a positional annotation, so a group like `{ headers: {} }` is the common
// Ruby-style hash default (`def f(opts: { headers: {} })`) rather than a shape
// type. This mirrors shapeHasDegenerateNilField for the empty-shape case,
// extending the top-level `{}` handling in bracedGroupIsShapeType to any depth.
func shapeHasEmptyNestedShape(ty *ast.TypeExpr) bool {
	if ty == nil || ty.Kind != ast.TypeShape || ty.Open {
		// An open shape's `...` marker rules out any hash-literal reading,
		// so its nested groups are never hash-default evidence.
		return false
	}
	for _, fieldType := range ty.Shape {
		if fieldType == nil {
			continue
		}
		if typeIsEmptyShape(fieldType) || shapeHasEmptyNestedShape(fieldType) {
			return true
		}
	}
	return false
}

// typeAnnotationBoundaryFollows reports whether peekToken terminates a
// parameter's type annotation. Valid terminators are a comma or closing
// paren (the next parameter or the list end), a trailing colon for typed
// required keywords, an `=` introducing a `name: Type = default` positional
// default, a `|` continuing a union, and, for line-limited parameter lists,
// the constructs that end such a list.
func (p *parser) typeAnnotationBoundaryFollows(options paramParseOptions) bool {
	switch p.peekToken.Type {
	case ast.TokenComma, ast.TokenRParen, ast.TokenColon, ast.TokenAssign, ast.TokenPipe:
		return true
	case ast.TokenThinArrow, ast.TokenSemicolon, ast.TokenEOF:
		return options.lineLimitedDefaults
	default:
		return options.lineLimitedDefaults && p.peekToken.Pos.Line != p.curToken.Pos.Line
	}
}

// identAfterColonStartsExpression reports whether an identifier that follows a
// parameter's `:` begins a default-value expression rather than a bare type
// name. The token after the identifier decides: a parameter boundary keeps the
// identifier a standalone type, `|` continues it as a union type, `<` is
// disambiguated by identLessThanStartsExpression on whether the identifier is a
// value, and anything else (other binary operators, calls, member access,
// indexing) makes it the head of an expression.
func (p *parser) identAfterColonStartsExpression(options paramParseOptions) bool {
	switch p.peekPeek.Type {
	case ast.TokenComma, ast.TokenRParen, ast.TokenAssign, ast.TokenPipe, ast.TokenColon:
		// A boundary (`,` `)`), an `=` introducing a `name: Type = default`
		// positional default, a `|` union continuation, or a trailing `:` for
		// typed required keywords (`name: Type:`) all keep the identifier a type
		// name.
		return false
	case ast.TokenLT:
		return p.identLessThanStartsExpression()
	case ast.TokenDot:
		return !p.dottedTypeAnnotationFollows(options)
	case ast.TokenThinArrow, ast.TokenSemicolon, ast.TokenEOF:
		return !options.lineLimitedDefaults
	default:
		if options.lineLimitedDefaults && p.peekPeek.Pos.Line != p.peekToken.Pos.Line {
			return false
		}
		return true
	}
}

func (p *parser) dottedTypeAnnotationFollows(options paramParseOptions) bool {
	saved := p.snapshot()
	defer p.restore(saved)

	p.nextToken()
	namespace := p.curToken.Literal
	if p.isLocalName(namespace) {
		return false
	}
	p.nextToken()
	if p.peekToken.Type != ast.TokenIdent {
		return false
	}
	p.nextToken()
	if !dottedMemberLooksLikeTypeName(p.curToken.Literal) {
		return false
	}
	if dottedDefaultConstant(namespace, p.curToken.Literal) {
		return false
	}
	if strings.HasSuffix(p.curToken.Literal, "?") {
		return p.typeAnnotationBoundaryFollows(options)
	}
	if p.peekToken.Type == ast.TokenQuestion {
		p.nextToken()
	}
	return p.typeAnnotationBoundaryFollows(options)
}

func startsUppercaseIdentifier(name string) bool {
	return len(name) > 0 && name[0] >= 'A' && name[0] <= 'Z'
}

// dottedMemberLooksLikeTypeName reports whether the member of a dotted name in
// annotation position is shaped like a type: PascalCase (Statuses.Status).
// A SCREAMING_CASE member (Data.RESULT, settings.MAX) is a constant by
// convention, so `name: obj.CONST` stays a keyword default expression rather
// than an unresolvable qualified type annotation.
func dottedMemberLooksLikeTypeName(name string) bool {
	name = strings.TrimSuffix(name, "?")
	if !startsUppercaseIdentifier(name) {
		return false
	}
	for i := 1; i < len(name); i++ {
		if name[i] >= 'a' && name[i] <= 'z' {
			return true
		}
	}
	return false
}

func dottedDefaultConstant(namespace, member string) bool {
	member = strings.TrimSuffix(member, "?")
	switch namespace {
	case "Math":
		return member == "PI" || member == "E"
	default:
		return false
	}
}

// identLessThanStartsExpression reports whether `ident <` (with ident at
// peekToken) begins a default-value expression rather than continuing a
// generic type annotation.
//
// A built-in generic container name (`array`, `hash`, `object`) always
// continues as a type: `<` opens its type arguments and nothing else can.
// This takes precedence over any value local that shadows the name, so
// `def f(array, values: array<int>)` keeps `array<int>` a generic type
// even though an earlier parameter is named `array`. Built-in generic
// type parsing is never shadowed by value locals.
//
// Otherwise the `<` is a comparison only when the identifier names a
// value, i.e. a local already in scope such as an earlier keyword
// parameter, so `def f(limit:, ok: limit < 10)` reads as a default
// expression. Failing both, the identifier is a (scalar or enum) type
// name and `<` produces the clear "does not accept type arguments"
// diagnostic rather than a misparsed comparison.
func (p *parser) identLessThanStartsExpression() bool {
	if isGenericContainerTypeName(p.peekToken.Literal) {
		return false
	}
	return p.isLocalName(p.peekToken.Literal)
}

// isGenericContainerTypeName reports whether name resolves to a built-in
// container type that accepts angle-bracket type arguments (`array<...>`,
// `hash<...>`, `object<...>`). These are the only types parseTypeAtom lets
// carry type arguments, so a following `<` always continues the type.
func isGenericContainerTypeName(name string) bool {
	kind, _ := resolveType(name)
	return kind == ast.TypeArray || kind == ast.TypeHash
}

func parameterNameExpectation(kind ast.ParamKind) string {
	switch kind {
	case ast.ParamRest:
		return "rest parameter name"
	case ast.ParamKeywordRest:
		return "keyword rest parameter name"
	default:
		return "parameter name"
	}
}

func (p *parser) parsePropertyDecl(kind ast.TokenType) ast.PropertyDecl {
	decl := ast.PropertyDecl{Kind: strings.ToLower(kind.String()), Position: p.curToken.Pos}
	p.nextToken()
	name, ok := p.parsePropertyName()
	if !ok {
		return decl
	}
	decl.Names = append(decl.Names, name)
	for p.peekToken.Type == ast.TokenComma {
		p.nextToken()
		p.nextToken()
		name, ok := p.parsePropertyName()
		if !ok {
			break
		}
		decl.Names = append(decl.Names, name)
	}
	return decl
}

func (p *parser) parsePropertyName() (ast.PropertyName, bool) {
	if p.curToken.Type != ast.TokenIdent {
		// An author arriving from attr_accessor :x reaches property :x next,
		// and "expected property name, got symbol" does not say that the
		// argument shape differs too.
		if p.curToken.Type == ast.TokenSymbol {
			p.addParseError(p.curToken.Pos, "property takes a bare name: property %s, not property :%s", srcText(p.curToken.Literal), srcText(p.curToken.Literal))
			return ast.PropertyName{}, false
		}
		p.errorExpected(p.curToken, "property name")
		return ast.PropertyName{}, false
	}
	name := ast.PropertyName{Name: p.curToken.Literal}
	if p.peekToken.Type == ast.TokenColon {
		p.nextToken()
		p.nextToken()
		name.Type = p.parseTypeExpr()
		if name.Type == nil {
			return ast.PropertyName{}, false
		}
	}
	return name, true
}

func (p *parser) parseExpressionOrAssignStatement() ast.Statement {
	if p.curToken.Type == ast.TokenAsterisk {
		target := p.parseDestructureTargetList(nil, false)
		if target == nil {
			return nil
		}
		return p.parseAssignmentValue(target)
	}

	expr := p.parseLineExpression(lowestPrec)
	if expr == nil {
		return nil
	}

	if p.canAttachPeekBlock() {
		p.nextToken()
		expr = p.callWithBlock(expr, p.parseBlockLiteral())
	}

	if p.peekToken.Type == ast.TokenComma {
		target := p.parseDestructureTargetList(expr, false)
		if target == nil {
			return nil
		}
		return p.parseAssignmentValue(target)
	}

	if isAssignmentOperator(p.peekToken.Type) {
		if pos, ok := safeNavigationPos(expr); ok {
			p.addParseError(pos, "safe navigation cannot be used as an assignment target")
			p.recoverAssignmentRemainder()
			return nil
		}
		if isAssignable(expr) {
			return p.parseAssignmentValue(expr)
		}
	}

	return &ast.ExprStmt{Expr: expr, Position: expr.Pos()}
}

func (p *parser) parseAssignmentValue(target ast.Expression) ast.Statement {
	operatorToken := p.peekToken.Type
	if !isAssignmentOperator(operatorToken) {
		p.addParseError(p.curToken.Pos, "parallel assignment targets require '='")
		return nil
	}
	if operatorToken != ast.TokenAssign {
		if _, ok := target.(*ast.DestructureTarget); ok {
			p.addParseErrorSpan(p.peekToken.Pos, tokenEnd(p.peekToken), "compound assignment is not supported for destructuring targets")
			p.recoverAssignmentRemainder()
			return nil
		}
	}

	pos := target.Pos()
	p.nextToken()
	p.nextToken()
	value := p.parseAssignmentExpression()
	if _, ok := target.(*ast.DestructureTarget); ok && p.peekToken.Type == ast.TokenComma {
		value = p.parseCommaSeparatedAssignmentValue(value, pos)
	}
	stmt := &ast.AssignStmt{Target: target, Value: value, Operator: compoundAssignmentOperator(operatorToken), Position: pos}
	p.declareLocalTarget(target)
	return stmt
}

func (p *parser) parseCommaSeparatedAssignmentValue(first ast.Expression, pos ast.Position) ast.Expression {
	if first == nil {
		return nil
	}
	values := []ast.Expression{first}
	for p.peekToken.Type == ast.TokenComma {
		p.nextToken()
		if p.peekEndsStatement(p.curToken.Pos) {
			break
		}
		p.nextToken()
		values = append(values, p.parseExpressionWithBlock())
	}
	return &ast.ArrayLiteral{Elements: values, Position: pos}
}

func (p *parser) parseAssignmentExpression() ast.Expression {
	expr := p.parseLineExpression(lowestPrec)
	if expr == nil {
		return nil
	}
	if p.canAttachPeekBlock() {
		p.nextToken()
		return p.callWithBlock(expr, p.parseBlockLiteral())
	}
	return expr
}

func (p *parser) recoverAssignmentRemainder() {
	startLine := p.peekToken.Pos.Line
	for p.peekToken.Type != ast.TokenEOF && p.peekToken.Type != ast.TokenSemicolon && p.peekToken.Pos.Line == startLine {
		p.nextToken()
	}
}

func isAssignmentOperator(tt ast.TokenType) bool {
	return tt == ast.TokenAssign || compoundAssignmentOperator(tt) != ast.TokenNone
}

func compoundAssignmentOperator(tt ast.TokenType) ast.TokenType {
	switch tt {
	case ast.TokenPlusAssign:
		return ast.TokenPlus
	case ast.TokenMinusAssign:
		return ast.TokenMinus
	case ast.TokenAsteriskAssign:
		return ast.TokenAsterisk
	case ast.TokenPowerAssign:
		return ast.TokenPower
	case ast.TokenSlashAssign:
		return ast.TokenSlash
	case ast.TokenPercentAssign:
		return ast.TokenPercent
	case ast.TokenAndAssign:
		return ast.TokenAndAssign
	case ast.TokenOrAssign:
		return ast.TokenOrAssign
	default:
		return ast.TokenNone
	}
}

func (p *parser) parseDestructureTargetList(first ast.Expression, allowElementTypes bool, extraAnonymousRestTerminators ...ast.TokenType) ast.Expression {
	var pos ast.Position
	elements := []ast.DestructureElement{}
	seenRest := false

	if first != nil {
		if pos, ok := safeNavigationPos(first); ok {
			p.addParseError(pos, "safe navigation cannot be used as an assignment target")
			return nil
		}
		if !isAssignable(first) {
			p.addParseError(first.Pos(), "invalid destructuring assignment target")
			return nil
		}
		pos = first.Pos()
		elements = append(elements, ast.DestructureElement{Target: first, Position: first.Pos()})
	} else {
		element, ok := p.parseDestructureElement(allowElementTypes, extraAnonymousRestTerminators...)
		if !ok {
			return nil
		}
		pos = element.Position
		seenRest = element.Rest
		elements = append(elements, element)
	}

	for p.peekToken.Type == ast.TokenComma {
		p.nextToken()
		p.nextToken()
		element, ok := p.parseDestructureElement(allowElementTypes, extraAnonymousRestTerminators...)
		if !ok {
			return nil
		}
		if element.Rest {
			if seenRest {
				p.addParseError(element.Position, "duplicate rest assignment target")
				return nil
			}
			seenRest = true
		}
		elements = append(elements, element)
	}

	return ast.NewDestructureTarget(elements, pos)
}

func (p *parser) parseDestructureElement(allowType bool, extraAnonymousRestTerminators ...ast.TokenType) (ast.DestructureElement, bool) {
	rest := false
	if p.curToken.Type == ast.TokenAsterisk {
		restPos := p.curToken.Pos
		// A bare "*" not followed by a target is an anonymous (discard) rest.
		// Leave curToken on "*" so the caller's lookahead sees the terminator.
		if isAnonymousRestTerminator(p.peekToken.Type, extraAnonymousRestTerminators...) {
			return ast.DestructureElement{Rest: true, Position: restPos}, true
		}
		rest = true
		p.nextToken()
	}

	target := p.parseDestructureSingleTarget(allowType)
	if target == nil {
		return ast.DestructureElement{}, false
	}
	element := ast.DestructureElement{Target: target, Rest: rest, Position: target.Pos()}
	if allowType && p.peekToken.Type == ast.TokenColon {
		p.nextToken()
		p.nextToken()
		element.Type = p.parseTypeExpr()
		if element.Type == nil {
			return ast.DestructureElement{}, false
		}
		if rest && !restCaptureTypeAcceptsArray(element.Type) {
			p.addParseError(element.Type.Position, "rest destructuring target %s captures an array; annotate it as array<...> or any", targetText{target})
			return ast.DestructureElement{}, false
		}
	}
	return element, true
}

// isAnonymousRestTerminator reports whether tt ends a bare "*" destructuring
// target, leaving an anonymous (discard) rest with no bound name. A "*" is
// anonymous when the next token closes the target list (an assignment operator),
// continues it (a comma), or closes a nested target group (")" or "]").
func isAnonymousRestTerminator(tt ast.TokenType, extra ...ast.TokenType) bool {
	if slices.Contains(extra, tt) {
		return true
	}
	switch tt {
	case ast.TokenComma, ast.TokenRParen, ast.TokenRBracket:
		return true
	default:
		return isAssignmentOperator(tt)
	}
}

func (p *parser) parseDestructureSingleTarget(allowElementTypes bool) ast.Expression {
	switch p.curToken.Type {
	case ast.TokenLParen:
		return p.parseNestedDestructureTarget(ast.TokenRParen, ")", allowElementTypes)
	case ast.TokenLBracket:
		return p.parseNestedDestructureTarget(ast.TokenRBracket, "]", allowElementTypes)
	default:
		target := p.parseLineExpression(lowestPrec)
		if target == nil {
			return nil
		}
		if pos, ok := safeNavigationPos(target); ok {
			p.addParseError(pos, "safe navigation cannot be used as an assignment target")
			return nil
		}
		if !isAssignable(target) {
			p.addParseError(target.Pos(), "invalid destructuring assignment target")
			return nil
		}
		return target
	}
}

func (p *parser) parseNestedDestructureTarget(stop ast.TokenType, _ string, allowElementTypes bool) ast.Expression {
	if !p.enterSyntax() {
		return nil
	}
	defer func() { p.syntaxDepth-- }()

	pos := p.curToken.Pos
	if p.peekToken.Type == stop {
		p.errorExpected(p.peekToken, "destructuring assignment target")
		return nil
	}

	p.nextToken()
	target := p.parseDestructureTargetList(nil, allowElementTypes)
	if target == nil {
		return nil
	}
	if !p.expectPeek(stop) {
		return nil
	}
	destructure, ok := target.(*ast.DestructureTarget)
	if !ok {
		return nil
	}
	return ast.NewDestructureTarget(destructure.Elements, pos)
}

func (p *parser) parseExpressionWithBlock() ast.Expression {
	expr := p.parseLineExpression(lowestPrec)
	if expr == nil {
		return nil
	}
	if p.canAttachPeekBlock() {
		p.nextToken()
		return p.callWithBlock(expr, p.parseBlockLiteral())
	}
	return expr
}

func (p *parser) parseAssertStatement() ast.Statement {
	pos := p.curToken.Pos
	// `assert(cond, message)` is an ordinary parenthesised call and parses as
	// one. Only the paren-less form needs the argument loop below, which reads
	// the comma-separated arguments the expression parser stops short of. The
	// check is adjacency, not merely "next token is `(`": a space makes the
	// parentheses a grouped first argument, so `assert (a > b), "msg"` stays a
	// paren-less call with two arguments, as in Ruby.
	if p.peekToken.Type == ast.TokenLParen && p.peekToken.Pos == p.curToken.End {
		return p.parseExpressionOrAssignStatement()
	}
	callee := &ast.Identifier{Name: p.curToken.Literal, Position: pos}
	args := []ast.Expression{}
	if p.peekEndsStatement(pos) {
		return &ast.ExprStmt{Expr: callee, Position: pos}
	}
	p.nextToken()
	first := p.parseLineExpression(lowestPrec)
	if first != nil {
		args = append(args, first)
		for p.peekToken.Type == ast.TokenComma {
			p.nextToken()
			p.nextToken()
			args = append(args, p.parseLineExpression(lowestPrec))
		}
	}
	call := &ast.CallExpr{Callee: callee, Args: args, Position: pos}
	return &ast.ExprStmt{Expr: call, Position: pos}
}

func (p *parser) peekEndsStatement(pos ast.Position) bool {
	switch p.peekToken.Type {
	case ast.TokenEOF, ast.TokenSemicolon, ast.TokenEnd, ast.TokenElse, ast.TokenElsif, ast.TokenEnsure, ast.TokenRescue, ast.TokenRBrace:
		return true
	default:
		return p.peekToken.Pos.Line != pos.Line
	}
}

func isAssignable(expr ast.Expression) bool {
	if _, ok := safeNavigationPos(expr); ok {
		return false
	}
	switch expr.(type) {
	case *ast.MemberExpr, *ast.Identifier, *ast.IndexExpr, *ast.IvarExpr, *ast.ClassVarExpr:
		return true
	default:
		return false
	}
}

// safeNavigationPos reports whether an assignment target contains a
// safe-navigation operator anywhere in its receiver chain, returning the
// position of the first such operator. Safe navigation (`object&.prop` or
// `object&.method(...)`) short-circuits to nil on a nil receiver, so allowing
// it on the left of an assignment would silently assign through nil. Vibescript
// rejects every such target—`user&.name = x`, `user&.profile.name = x`, and
// `user&.items[0] = x` alike—so the receiver chain is walked rather than only
// the outermost node.
func safeNavigationPos(expr ast.Expression) (ast.Position, bool) {
	for {
		switch e := expr.(type) {
		case *ast.MemberExpr:
			if e.Safe {
				return e.Position, true
			}
			expr = e.Object
		case *ast.CallExpr:
			if e.Safe {
				return e.Position, true
			}
			expr = e.Callee
		case *ast.IndexExpr:
			expr = e.Object
		default:
			return ast.Position{}, false
		}
	}
}
