package parser

import (
	"fmt"
	"strings"

	"github.com/mgomes/vibescript/internal/ast"
	"github.com/mgomes/vibescript/vibes/source"
)

const maxParseErrors = 100

type parser struct {
	l *lexer

	curToken  ast.Token
	peekToken ast.Token
	peekPeek  ast.Token
	// prevEnd is the exclusive end position of the token before curToken, so
	// the parser can tell whether whitespace separates them.
	prevEnd source.Position

	// memberReceiverProbe names a member access whose receiver the caller
	// wants captured; memberReceiver and memberReceiverFunction hold it once
	// seen. See MemberReceiverFor.
	memberReceiverProbe  string
	memberReceiver       ast.Expression
	memberReceiverParams []ast.Param
	// currentParams are the parameters of the function whose body is being
	// parsed, empty at the top level.
	currentParams []ast.Param

	errors        []error
	omittedErrors int
	codeFrames    *source.CodeFrameFormatter

	insideClass                bool
	pendingVisibility          string
	lineLimitedExprs           int
	lineLimitedStops           []ast.TokenType
	lineLimitedForcedStops     []lineLimitedForcedStop
	lineLimitedStopSuppression int
	statementNesting           int
	typeDepth                  int
	parenlessCallDepth         int
	localScopes                []localScope
	parenlessArgDoStops        int
	whenValueGroupDepths       []int
	groupDepth                 int
	syntaxDepth                int
	nestingError               *parseError
	nodeDepths                 map[ast.Node]syntaxNodeDepth

	// shapeStructurallyInvalid records that the most recent parseTypeShape
	// rejected a brace group whose field values all parsed as types but whose
	// shape structure was malformed (a duplicate field or a missing field
	// separator). It lets the parameter-list speculation in
	// bracedGroupIsShapeType keep such a clearly-shape-like diagnostic instead
	// of silently reinterpreting the braces as a hash-literal default.
	shapeStructurallyInvalid bool
}

type lineLimitedForcedStop struct {
	token       ast.TokenType
	suppression int
}

// localScope records the local names declared within a single lexical
// scope. funcDef marks scopes introduced by a function definition, which
// act as a lookup boundary: name resolution does not see locals declared
// in scopes enclosing a function. Block scopes leave funcDef false so
// they continue to close over their surrounding locals.
//
// classBody marks the scope a class or module body pushes. It is the one
// scope kind whose names stay visible across a funcDef boundary: the
// runtime resolves body-level assignments (class constants) from method
// bodies, so isLocalName must too. implicitIt marks a block scope that
// may bind the implicit `it` parameter; isLocalName treats `it` as a
// local there so `it /2` divides like the pre-declared `_1`..`_9`
// candidates, while isDeclaredLocal ignores the mark so implicit-param
// inference still distinguishes a real enclosing `it` variable.
type localScope struct {
	names      map[string]struct{}
	funcDef    bool
	classBody  bool
	implicitIt bool
}

func newParser(input string) *parser {
	l := newLexer(input)
	p := &parser{l: l, localScopes: []localScope{{}}}

	p.nextToken()
	p.nextToken()
	p.nextToken()

	return p
}

func (p *parser) pushLocalScope(params []ast.Param, funcDef bool) {
	scope := localScope{funcDef: funcDef}
	p.localScopes = append(p.localScopes, scope)
	for _, param := range params {
		p.declareParamLocal(param)
	}
}

// pushClassBodyScope opens the scope a class or module body declares its
// body-level names (class constants) into, so they neither leak into the
// enclosing scope after `end` nor hide from method bodies (see isLocalName).
func (p *parser) pushClassBodyScope() {
	p.localScopes = append(p.localScopes, localScope{classBody: true})
}

func (p *parser) popLocalScope() {
	if len(p.localScopes) <= 1 {
		return
	}
	p.localScopes = p.localScopes[:len(p.localScopes)-1]
}

func (p *parser) declareParamLocal(param ast.Param) {
	if param.Target != nil {
		p.declareLocalTarget(param.Target)
		return
	}
	if param.Name != "" && !param.IsIvar {
		p.declareLocal(param.Name)
	}
}

func (p *parser) declareLocalTarget(target ast.Expression) {
	switch t := target.(type) {
	case *ast.Identifier:
		p.declareLocal(t.Name)
	case *ast.DestructureTarget:
		for _, element := range t.Elements {
			p.declareLocalTarget(element.Target)
		}
	}
}

func (p *parser) declareLocal(name string) {
	if len(p.localScopes) == 0 {
		p.localScopes = append(p.localScopes, localScope{})
	}
	scope := &p.localScopes[len(p.localScopes)-1]
	if scope.names == nil {
		scope.names = make(map[string]struct{})
	}
	scope.names[name] = struct{}{}
}

// localDeclaredInTop reports whether name is already declared in the
// innermost scope (not any enclosing scope).
func (p *parser) localDeclaredInTop(name string) bool {
	if len(p.localScopes) == 0 {
		return false
	}
	_, ok := p.localScopes[len(p.localScopes)-1].names[name]
	return ok
}

// undeclareLocal removes name from the innermost scope. It is used for
// names whose visibility is limited to a sub-region of their scope, such
// as a rescue exception binding.
func (p *parser) undeclareLocal(name string) {
	if len(p.localScopes) == 0 {
		return
	}
	delete(p.localScopes[len(p.localScopes)-1].names, name)
}

// isLocalName reports whether name resolves as a variable-like local at
// the current position. It drives name-sensitive disambiguation (division
// vs command-argument regex, splat and block-pass sigils, percent
// literals), so it mirrors what the runtime can resolve: locals up to the
// enclosing function definition, the implicit `it` block parameter, and —
// uniquely across the funcDef boundary — the body-level names of the
// method's own class or module, which the runtime reaches as class
// constants. Outer classes' bodies stay invisible, as at runtime.
func (p *parser) isLocalName(name string) bool {
	for i := len(p.localScopes) - 1; i >= 0; i-- {
		scope := &p.localScopes[i]
		if _, ok := scope.names[name]; ok {
			return true
		}
		if name == "it" && scope.implicitIt {
			return true
		}
		// A function definition is a lookup boundary: locals declared in
		// scopes enclosing the function (including snippet top-level
		// locals) are not visible inside the function body. The nearest
		// enclosing class/module body is the one exception: its
		// assignments are the class's constants, which method bodies
		// resolve at runtime.
		if scope.funcDef {
			for j := i - 1; j >= 0; j-- {
				if !p.localScopes[j].classBody {
					continue
				}
				_, ok := p.localScopes[j].names[name]
				return ok
			}
			break
		}
	}
	return false
}

// isDeclaredLocal reports whether name was explicitly declared (as an
// assignment target, parameter, or binding) in a scope reachable without
// crossing a function-definition boundary. Unlike isLocalName it ignores
// implicit-`it` candidacy and class-body names beyond the boundary; the
// implicit-parameter inference gate uses it so only a real enclosing `it`
// variable suppresses inferring the implicit block parameter.
func (p *parser) isDeclaredLocal(name string) bool {
	for i := len(p.localScopes) - 1; i >= 0; i-- {
		if _, ok := p.localScopes[i].names[name]; ok {
			return true
		}
		if p.localScopes[i].funcDef {
			break
		}
	}
	return false
}

func (p *parser) nextToken() {
	if p.nestingError != nil {
		p.rejectNesting(p.nestingError.pos)
		return
	}
	p.prevEnd = p.curToken.End
	p.curToken = p.peekToken
	p.peekToken = p.peekPeek
	p.peekPeek = p.l.NextToken()
}

// curTokenIsSpacedFromPrevious reports whether whitespace separates the
// current token from the one before it. A space is not decorative before an
// opening parenthesis: `f (x).length` and `f(x).length` are the same AST here
// but different programs in Ruby, so the parser has to record which was
// written for anything downstream to say so.
func (p *parser) curTokenIsSpacedFromPrevious() bool {
	if p.prevEnd == (source.Position{}) {
		return false
	}
	return p.curToken.Pos != p.prevEnd
}

// reprimeAt repositions the lexer to resume scanning at the given byte
// offset and rebuilds the lookahead from there. It is used after the
// parser consumes a construct directly from source (such as a
// percent-array call argument) whose interior the lexer may have
// tokenized incorrectly while filling its lookahead.
//
// last is the synthetic token standing in for the consumed construct. It
// becomes curToken so the expression-parsing contract holds (curToken is
// left on the construct's final token, not the token after it) and the
// lexer's lastToken, so token-adjacency gating stays correct. The two
// following tokens are scanned fresh from offset.
func (p *parser) reprimeAt(offset int, last ast.Token) {
	p.l.seek(offset, last)
	p.curToken = last
	p.peekToken = p.l.NextToken()
	p.peekPeek = p.l.NextToken()
}

// parserSnapshot captures the parser state needed to roll back a speculative
// parse. The lexer is captured by value; its bracket and pending-ternary stacks
// are immutable (see frameStack), so the captured pointers keep naming what the
// stacks held at capture no matter what the speculation pushes or pops.
// errorCount records how many diagnostics existed before the speculation so any
// added during it can be discarded on rollback.
type parserSnapshot struct {
	lexer      lexer
	curToken   ast.Token
	peekToken  ast.Token
	peekPeek   ast.Token
	typeDepth  int
	groupDepth int
	errorCount int
	omitCount  int
}

// snapshot records the current parser state for a later restore. It is
// the basis for bounded speculative parsing: try a parse, and if it does
// not pan out, restore and parse the alternative.
func (p *parser) snapshot() parserSnapshot {
	return parserSnapshot{
		lexer:      *p.l,
		curToken:   p.curToken,
		peekToken:  p.peekToken,
		peekPeek:   p.peekPeek,
		typeDepth:  p.typeDepth,
		groupDepth: p.groupDepth,
		errorCount: len(p.errors),
		omitCount:  p.omittedErrors,
	}
}

// restore rewinds the parser to a previously captured snapshot, discarding any
// tokens consumed and diagnostics recorded since. The snapshot stays usable
// afterwards: a push onto a restored stack leaves the frames under it alone, so
// the same snapshot can be restored again.
func (p *parser) restore(s parserSnapshot) {
	if p.nestingError != nil {
		return
	}
	*p.l = s.lexer
	p.curToken = s.curToken
	p.peekToken = s.peekToken
	p.peekPeek = s.peekPeek
	p.typeDepth = s.typeDepth
	p.groupDepth = s.groupDepth
	p.errors = p.errors[:s.errorCount]
	p.omittedErrors = s.omitCount
}

// Parse lexes and parses the given source text and returns the
// resulting AST together with any parse errors encountered. It is the
// stable entry point used by callers within the module.
func Parse(source string) (*ast.Program, []error) {
	return newParser(source).parseProgram()
}

func (p *parser) parseProgram() (*ast.Program, []error) {
	program := &ast.Program{}

	for p.curToken.Type != ast.TokenEOF {
		p.skipStatementSeparators()
		if p.curToken.Type == ast.TokenEOF {
			break
		}
		stmt := p.parseStatement()
		if stmt != nil {
			program.Statements = append(program.Statements, stmt)
		}
		p.nextToken()
	}

	p.addPercentScanExhaustedError()
	p.addOmittedParseError()
	if p.nestingError != nil {
		return &ast.Program{}, []error{p.nestingError}
	}
	return program, p.errors
}

// addPercentScanExhaustedError reports a source that used up its speculative
// percent-array-literal allowance (see percentScanBudget). Past that point the
// parser stops second-guessing whether a `%` the lexer read as modulo opens a
// %w/%i/%W/%I literal, so a later `foo %w[a b]` binds as `foo % w[a, b]`.
//
// That is a change of meaning rather than a slower parse, so the source is
// rejected instead of being handed back with a silently different program in
// it. Reaching the allowance at all takes four times the source length spent on
// candidates that led nowhere, which no ordinary source comes close to.
func (p *parser) addPercentScanExhaustedError() {
	if !p.l.percentScan.spent() {
		return
	}
	pos := p.l.percentScan.declinedAt
	if pos == (ast.Position{}) {
		pos = p.curToken.Pos
	}
	p.addParseError(pos, "%s", percentScanExhaustedMessage)
}

// percentScanExhaustedMessage is spelled as a constant because it contains
// literal percent signs, which a format string would read as verbs.
const percentScanExhaustedMessage = "too many ambiguous \"%\" operators to tell percent-array literals from modulo;" +
	" parenthesize the intended literals, as in foo(%w[a b])"

const (
	lowestPrec = iota
	precRescue
	precConditional
	precOr
	precAnd
	precEquality
	precComparison
	precRange
	precBitAnd
	precShift
	precSum
	precProduct
	precPrefix
	precPower
	precCall
)

var precedences = map[ast.TokenType]int{
	ast.TokenRescue:    precRescue,
	ast.TokenQuestion:  precConditional,
	ast.TokenOr:        precOr,
	ast.TokenAnd:       precAnd,
	ast.TokenEQ:        precEquality,
	ast.TokenCaseEQ:    precEquality,
	ast.TokenNotEQ:     precEquality,
	ast.TokenMatch:     precEquality,
	ast.TokenNotMatch:  precEquality,
	ast.TokenLT:        precComparison,
	ast.TokenLTE:       precComparison,
	ast.TokenGT:        precComparison,
	ast.TokenGTE:       precComparison,
	ast.TokenSpaceship: precComparison,
	ast.TokenRange:     precRange,
	ast.TokenRangeExcl: precRange,
	ast.TokenAmpersand: precBitAnd,
	ast.TokenShovel:    precShift,
	ast.TokenPlus:      precSum,
	ast.TokenMinus:     precSum,
	ast.TokenSlash:     precProduct,
	ast.TokenAsterisk:  precProduct,
	ast.TokenPercent:   precProduct,
	ast.TokenPower:     precPower,
	ast.TokenLParen:    precCall,
	ast.TokenDot:       precCall,
	ast.TokenSafeNav:   precCall,
	ast.TokenScope:     precCall,
	ast.TokenLBracket:  precCall,
	ast.TokenDo:        precCall,
	ast.TokenLBrace:    precCall,
}

func (p *parser) curPrecedence() int {
	if prec, ok := precedences[p.curToken.Type]; ok {
		return prec
	}
	return lowestPrec
}

func (p *parser) peekPrecedence() int {
	if prec, ok := precedences[p.peekToken.Type]; ok {
		return prec
	}
	return lowestPrec
}

func (p *parser) expectPeek(tt ast.TokenType) bool {
	if p.peekToken.Type == tt {
		p.nextToken()
		return true
	}
	p.errorExpected(p.peekToken, tokenLabel(tt))
	return false
}

var _ error = (*parseError)(nil)

type parseError struct {
	pos    ast.Position
	end    ast.Position
	msg    string
	frames *source.CodeFrameFormatter
}

func (e *parseError) Error() string {
	msg := fmt.Sprintf("parse error at %d:%d: %s", e.pos.Line, e.pos.Column, e.msg)
	if e.frames == nil {
		return msg
	}
	if frame := e.frames.Format(e.pos); frame != "" {
		return msg + "\n" + frame
	}
	return msg
}

// Pos returns the 1-indexed source position where the error starts.
func (e *parseError) Pos() ast.Position { return e.pos }

// End returns the exclusive 1-indexed end of the offending token, or a
// zero Position when the span is unknown.
func (e *parseError) End() ast.Position { return e.end }

// Message returns the error text without the position prefix or the
// rendered code frame.
func (e *parseError) Message() string { return e.msg }

func (p *parser) errorExpected(tok ast.Token, expected string) {
	// Diagnostic illegal tokens carry a human-readable lexer message in the
	// literal (such as a malformed numeric literal); surface it verbatim so
	// the cause is clear. The lexer writes those messages itself, so unlike
	// source text they are already bounded and are not truncated. Plain
	// illegal characters carry only the raw source rune, so they fall back to
	// the generic "expected X, got invalid token".
	if tok.Type == ast.TokenIllegal && tok.Diagnostic {
		p.addParseErrorSpan(tok.Pos, tokenEnd(tok), "%s", tok.Literal)
		return
	}
	p.addParseErrorSpan(tok.Pos, tokenEnd(tok), "expected %s, got %s", expected, tokenLabel(tok.Type))
}

func (p *parser) errorUnexpected(tok ast.Token) {
	// Diagnostic illegal tokens carry a human-readable lexer message in the
	// literal (such as a malformed numeric literal); surface it verbatim so
	// the cause is clear. The lexer writes those messages itself, so unlike
	// source text they are already bounded and are not truncated. Plain
	// illegal characters carry only the raw source rune, so they fall back to
	// the generic "unexpected token invalid token".
	if tok.Type == ast.TokenIllegal && tok.Diagnostic {
		p.addParseErrorSpan(tok.Pos, tokenEnd(tok), "%s", tok.Literal)
		return
	}
	p.addParseErrorSpan(tok.Pos, tokenEnd(tok), "unexpected token %s", tokenLabel(tok.Type))
}

// addParseError records a diagnostic at pos. It takes the format and its
// arguments rather than a finished message so that the error budget is
// consulted before any of the message is built: parsing runs before any
// sandbox exists, so an error the cap will discard must cost no work at all,
// however much source text its message would have quoted. Source text an
// argument carries into a message belongs in srcText, which bounds how much of
// it a diagnostic can reproduce.
func (p *parser) addParseError(pos ast.Position, format string, args ...any) {
	p.addParseErrorSpan(pos, ast.Position{}, format, args...)
}

func (p *parser) addParseErrorSpan(pos, end ast.Position, format string, args ...any) {
	if p.nestingError != nil {
		return
	}
	if len(p.errors) >= maxParseErrors {
		p.omittedErrors++
		return
	}
	msg := format
	if len(args) > 0 {
		msg = fmt.Sprintf(format, args...)
	}
	p.errors = append(p.errors, &parseError{pos: pos, end: end, msg: msg, frames: p.codeFrameFormatter()})
}

func (p *parser) addOmittedParseError() {
	if p.omittedErrors == 0 {
		return
	}
	msg := fmt.Sprintf("%d additional parse errors omitted", p.omittedErrors)
	if p.omittedErrors == 1 {
		msg = "1 additional parse error omitted"
	}
	p.errors = append(p.errors, &parseError{pos: p.curToken.Pos, msg: msg})
	p.omittedErrors = 0
}

func (p *parser) codeFrameFormatter() *source.CodeFrameFormatter {
	if p.codeFrames == nil {
		p.codeFrames = source.NewCodeFrameFormatter(p.l.input)
	}
	return p.codeFrames
}

// tokenEnd returns the lexer-stamped exclusive end of the token. EOF
// carries no span, yielding the zero Position.
func tokenEnd(tok ast.Token) ast.Position {
	return tok.End
}

func tokenLabel(tt ast.TokenType) string {
	switch tt {
	case ast.TokenIllegal:
		return "invalid token"
	case ast.TokenEOF:
		return "end of input"
	case ast.TokenIdent:
		return "identifier"
	case ast.TokenInt:
		return "integer"
	case ast.TokenFloat:
		return "float"
	case ast.TokenString, ast.TokenInterpolatedString:
		return "string"
	case ast.TokenSymbol:
		return "symbol"
	case ast.TokenWords:
		return "percent word array"
	case ast.TokenSymbols:
		return "percent symbol array"
	case ast.TokenInterpWords:
		return "percent interpolated word array"
	case ast.TokenInterpSymbols:
		return "percent interpolated symbol array"
	case ast.TokenSemicolon:
		return "\";\""
	case ast.TokenIvar:
		return "instance variable"
	case ast.TokenClassVar:
		return "class variable"
	case ast.TokenDef:
		return "'def'"
	case ast.TokenClass:
		return "'class'"
	case ast.TokenEnum:
		return "'enum'"
	case ast.TokenExport:
		return "'export'"
	case ast.TokenSelf:
		return "'self'"
	case ast.TokenPrivate:
		return "'private'"
	case ast.TokenProperty:
		return "'property'"
	case ast.TokenGetter:
		return "'getter'"
	case ast.TokenSetter:
		return "'setter'"
	case ast.TokenEnd:
		return "'end'"
	case ast.TokenRaise:
		return "'raise'"
	case ast.TokenReturn:
		return "'return'"
	case ast.TokenYield:
		return "'yield'"
	case ast.TokenDo:
		return "'do'"
	case ast.TokenThen:
		return "'then'"
	case ast.TokenFor:
		return "'for'"
	case ast.TokenIn:
		return "'in'"
	case ast.TokenIf:
		return "'if'"
	case ast.TokenUnless:
		return "'unless'"
	case ast.TokenElsif:
		return "'elsif'"
	case ast.TokenElse:
		return "'else'"
	case ast.TokenTrue:
		return "'true'"
	case ast.TokenFalse:
		return "'false'"
	case ast.TokenNil:
		return "'nil'"
	default:
		text := tt.String()
		if len(text) == 1 || strings.HasPrefix(text, "<") || strings.HasPrefix(text, ">") {
			return fmt.Sprintf("%q", text)
		}
		return fmt.Sprintf("%q", strings.ToLower(text))
	}
}
