package parser

import (
	"errors"
	"math/big"
	"slices"
	"strconv"
	"strings"
	"sync/atomic"
	"unicode/utf8"

	"github.com/mgomes/vibescript/internal/ast"
)

func (p *parser) parseExpression(precedence int) ast.Expression {
	if p.lineLimitedExprs > 0 {
		return p.parseExpressionWithLineLimit(precedence, p.curToken.Pos.Line, true)
	}
	return p.parseExpressionWithLineLimit(precedence, 0, false)
}

func (p *parser) parseLineExpression(precedence int) ast.Expression {
	return p.parseLineExpressionUntil(precedence)
}

func (p *parser) parseLineExpressionUntil(precedence int, stop ...ast.TokenType) ast.Expression {
	p.lineLimitedExprs++
	stopLen := len(p.lineLimitedStops)
	p.lineLimitedStops = append(p.lineLimitedStops, stop...)
	defer func() {
		p.lineLimitedExprs--
		p.lineLimitedStops = p.lineLimitedStops[:stopLen]
	}()
	return p.parseExpression(precedence)
}

func (p *parser) parseLineExpressionUntilForced(precedence int, stop ...ast.TokenType) ast.Expression {
	p.lineLimitedExprs++
	stopLen := len(p.lineLimitedStops)
	forcedStopLen := len(p.lineLimitedForcedStops)
	p.lineLimitedStops = append(p.lineLimitedStops, stop...)
	for _, token := range stop {
		p.lineLimitedForcedStops = append(p.lineLimitedForcedStops, lineLimitedForcedStop{
			token:       token,
			suppression: p.lineLimitedStopSuppression,
		})
	}
	defer func() {
		p.lineLimitedExprs--
		p.lineLimitedStops = p.lineLimitedStops[:stopLen]
		p.lineLimitedForcedStops = p.lineLimitedForcedStops[:forcedStopLen]
	}()
	return p.parseExpression(precedence)
}

func (p *parser) parseParenlessArgumentExpression() ast.Expression {
	// In command calls, same-line do/end binds to the outer call, not to the
	// final argument expression. Rescue stops here too so rescue modifiers bind
	// to the command call instead of being swallowed by the last argument.
	p.parenlessArgDoStops++
	defer func() {
		p.parenlessArgDoStops--
	}()
	return p.parseLineExpressionUntil(lowestPrec, ast.TokenDo, ast.TokenRescue)
}

func (p *parser) parseExpressionWithLineLimit(precedence, limitLine int, lineLimited bool) ast.Expression {
	if !p.enterSyntax() {
		return nil
	}
	defer func() { p.syntaxDepth-- }()

	prefix := prefixParserKind(p.curToken.Type)
	if prefix == prefixParserNone {
		if p.curToken.Type == ast.TokenRange || p.curToken.Type == ast.TokenRangeExcl {
			return p.parseBeginlessRangeExpression()
		}
		p.errorUnexpected(p.curToken)
		return nil
	}

	left := p.parsePrefix(prefix)
	if left == nil {
		return nil
	}

	return p.continueExpressionParse(left, precedence, limitLine, lineLimited)
}

// continueExpressionParse applies infix and postfix parselets to an already
// parsed left-hand expression, following precedence and line-limit rules. It
// is the shared continuation used both after parsing a prefix and after the
// parser materializes an operand directly (such as a percent-array call
// argument) that must still accept trailing postfixes like `[i]` or `.member`.
func (p *parser) continueExpressionParse(left ast.Expression, precedence, limitLine int, lineLimited bool) ast.Expression {
	if !p.checkSyntaxNode(left) {
		return nil
	}
	for p.peekToken.Type != ast.TokenEOF {
		if lineLimited && p.peekStopsLineExpression() {
			return left
		}

		if lineLimited && p.peekToken.Type == ast.TokenSemicolon {
			return left
		}

		if lineLimited && p.peekToken.Pos.Line > limitLine && !p.lineLimitedContinuationToken(p.peekToken) {
			return left
		}

		if p.canParseParenlessCall(left, precedence, lineLimited) {
			left = p.parseParenlessCallExpression(left)
			if left == nil {
				return nil
			}
			if !p.checkSyntaxNode(left) {
				return nil
			}
			if lineLimited {
				limitLine = p.curToken.Pos.Line
			}
			continue
		}

		if precedence >= p.peekPrecedence() {
			return left
		}
		infix := infixParserKind(p.peekToken.Type)
		if infix == infixParserNone {
			return left
		}
		p.nextToken()
		left = p.parseInfix(infix, left)
		if left == nil {
			return nil
		}
		if !p.checkSyntaxNode(left) {
			return nil
		}
		if lineLimited {
			limitLine = p.curToken.Pos.Line
		}
	}

	return left
}

func (p *parser) peekStopsLineExpression() bool {
	if p.peekToken.Pos.Line != p.curToken.Pos.Line {
		return false
	}
	for _, stop := range p.lineLimitedStops {
		if p.peekToken.Type == stop {
			if p.lineLimitedStopSuppression > 0 && !p.lineStopForced(stop) {
				return false
			}
			if stop == ast.TokenDo && p.peekPeek.Type == ast.TokenPipe && p.parenlessArgDoStops == 0 {
				return false
			}
			return true
		}
	}
	return false
}

func (p *parser) lineStopForced(stop ast.TokenType) bool {
	for _, forced := range p.lineLimitedForcedStops {
		if forced.token == stop && forced.suppression == p.lineLimitedStopSuppression {
			return true
		}
	}
	return false
}

func (p *parser) canParseParenlessCall(left ast.Expression, precedence int, lineLimited bool) bool {
	if !lineLimited || precedence > precPrefix {
		return false
	}
	if !isParenlessCallCallee(left) {
		return false
	}
	// self parses as an identifier but can never be a command callee in
	// Ruby: "self [0]" always indexes through a user-defined [], and
	// "self *2" / "self /2" are the binary operators. Classify it like a
	// known local so every sigil arm below keeps the operator reading.
	if ident, ok := left.(*ast.Identifier); ok && ident.Name == "self" {
		return false
	}
	if p.peekToken.Pos.Line != p.curToken.Pos.Line {
		return false
	}
	if p.peekStartsPercentArrayArgument(left) {
		return true
	}
	if p.peekStartsParenlessKeywordLabel() {
		return true
	}
	if p.peekToken.Type == ast.TokenAmpersand {
		// "&" is both the binary intersection operator and the block-pass /
		// symbol-to-proc sigil. Ruby disambiguates by spacing: "foo &bar"
		// passes a block while "foo & bar", "foo&bar", and a trailing "&"
		// line continuation are all the binary operator. A known local can
		// never be a parenless callee, so "locals &other" stays intersection
		// in every spacing, matching Ruby's local-variable rule.
		if ident, ok := left.(*ast.Identifier); ok && p.isLocalName(ident.Name) {
			return false
		}
		return p.peekAmpersandStartsBlockPass()
	}
	if p.peekToken.Type == ast.TokenAsterisk || p.peekToken.Type == ast.TokenPower {
		// "*" (and "**") is both a binary operator and the splat sigil; the
		// same spacing shape Ruby uses for the ampersand applies — "foo *args"
		// splats while "foo * args", "foo*args", and a trailing "*" are the
		// operator — and a known local callee keeps the operator reading.
		if ident, ok := left.(*ast.Identifier); ok && p.isLocalName(ident.Name) {
			return false
		}
		return p.peekSigilStartsPrefixArgument()
	}
	if p.peekToken.Type == ast.TokenSlash {
		// "/" is both the division operator and the regex-literal delimiter;
		// the same spacing shape as the sigils above applies — "match /id/"
		// is a command-argument regex while "f / 2", "f/2", and a trailing
		// "/" are division — and a known local callee keeps the division
		// reading in every spacing ("total /2" divides), mirroring the
		// local-variable table Ruby's parser feeds back to its lexer. A "/="
		// after the callee stays a compound assignment (TokenSlashAssign
		// never reaches here), also matching Ruby, which gives op-assign
		// priority over a regex whose pattern would begin with "=".
		if ident, ok := left.(*ast.Identifier); ok && p.isLocalName(ident.Name) {
			return false
		}
		return p.peekSlashStartsRegexArgument()
	}
	if p.peekToken.Type == ast.TokenLBracket {
		// "[" is both the index postfix and the opener of an array-literal
		// command argument. Ruby disambiguates by callee spacing alone:
		// "puts [1]" (and "puts [ 1 ]") passes an array argument while
		// "puts[1]" indexes the callee. Unlike the sigil arms there is no
		// flush-operand requirement — the elements may sit apart from the
		// bracket or continue on later lines. A known local callee keeps the
		// indexing reading in every spacing ("a [0]" reads and "a [0] = 1"
		// assigns through the index), matching Ruby's local-variable rule
		// and the sigil arms above.
		if ident, ok := left.(*ast.Identifier); ok && p.isLocalName(ident.Name) {
			return false
		}
		calleeFlush := p.peekToken.Pos.Line == p.curToken.End.Line &&
			p.peekToken.Pos.Column == p.curToken.End.Column
		return !calleeFlush
	}
	return isParenlessArgumentStart(p.peekToken.Type)
}

// peekAmpersandStartsBlockPass reports whether the lookahead "&" has the
// spacing Ruby reads as a block-pass argument ("foo &bar") rather than the
// binary intersection operator. Ruby disambiguates purely by spacing: the
// block-pass shape requires the "&" to be separated from the callee yet flush
// against its operand. Concretely both of these must hold:
//
//   - The "&" is detached from the callee. "foo &bar" is a block-pass, while
//     "foo&bar" (flush on both sides) is the binary operator, so a "&" that
//     abuts the callee on the same line is never a block-pass.
//   - The operand is flush against the "&" on the same line. "foo & bar" is the
//     binary operator, and a trailing "&" that ends the line is the intersection
//     line-continuation operator (see lineLimitedContinuationToken); neither is
//     a block-pass.
func (p *parser) peekAmpersandStartsBlockPass() bool {
	if p.peekToken.Type != ast.TokenAmpersand {
		return false
	}
	return p.peekSigilStartsPrefixArgument()
}

// peekSigilStartsPrefixArgument reports whether the lookahead sigil (&, *,
// or **) has the argument-prefix spacing: detached from the callee yet flush
// against its operand on the same line.
func (p *parser) peekSigilStartsPrefixArgument() bool {
	calleeFlush := p.peekToken.Pos.Line == p.curToken.End.Line &&
		p.peekToken.Pos.Column == p.curToken.End.Column
	if calleeFlush {
		return false
	}
	operandFlush := p.peekPeek.Pos.Line == p.peekToken.Pos.Line &&
		p.peekPeek.Pos.Column == p.peekToken.End.Column
	return operandFlush
}

// peekSlashStartsRegexArgument reports whether the lookahead "/" has the
// spacing Ruby reads as a command-argument regex literal ("match /id/")
// rather than division: detached from the callee yet flush against pattern
// text on the same line. Unlike the token-based sigil check above, the flush
// side must consult the raw source: the lexer tokenized past the slash under
// the division reading, so the following token's position is unreliable (a
// "#" directly after the slash, for example, swallowed the rest of the line
// as a comment while filling the lookahead).
func (p *parser) peekSlashStartsRegexArgument() bool {
	calleeFlush := p.peekToken.Pos.Line == p.curToken.End.Line &&
		p.peekToken.Pos.Column == p.curToken.End.Column
	if calleeFlush {
		return false
	}
	offset, ok := p.l.offsetForPosition(p.peekToken.Pos)
	if !ok || offset+1 >= len(p.l.input) {
		return false
	}
	switch p.l.input[offset+1] {
	case ' ', '\t', '\r', '\n':
		return false
	}
	return true
}

// peekStartsParenlessKeywordLabel reports whether the lookahead begins a
// keyword-argument label (`name:`) for a parenless call. Reserved keywords
// such as `rescue` are valid only as labels here, so they are not accepted
// by isParenlessArgumentStart; recognizing the `label:` shape lets forms
// like `record rescue: 1` start a parenless call. The trailing colon is the
// disambiguator, mirroring Ruby, where `record rescue 1` is the rescue
// modifier while `record rescue: 1` is a keyword argument.
func (p *parser) peekStartsParenlessKeywordLabel() bool {
	return isLabelNameToken(p.peekToken) && p.peekPeek.Type == ast.TokenColon
}

func isParenlessCallCallee(expr ast.Expression) bool {
	switch expr.(type) {
	case *ast.Identifier, *ast.MemberExpr:
		return true
	default:
		return false
	}
}

func (p *parser) peekStartsPercentArrayArgument(callee ast.Expression) bool {
	if p.peekToken.Type != ast.TokenPercent {
		return false
	}
	// Only explicitly declared locals suppress the percent-array argument
	// reading. The wider isLocalName view would break two pinned callee
	// shapes: `it %w[a b]` must keep calling a user-defined `it` function
	// (the implicit-`it` candidate does not make `it` a modulo operand
	// here), and a class constant callee keeps the argument reading it has
	// always had rather than flipping to modulo.
	//
	// This is settled before the probe runs, not after. A modulo on a local
	// such as `a %w+ b` is not a percent-literal candidate at all, so probing
	// it would scan to the end of the source and charge the speculative-scan
	// allowance for a shape that could never have been a literal -- enough of
	// them and a later genuine `%w[...]` would be declined for want of
	// allowance and silently read as modulo instead.
	if ident, ok := callee.(*ast.Identifier); ok && p.isDeclaredLocal(ident.Name) {
		return false
	}
	return p.percentArrayLiteralArgumentAt(p.peekToken.Pos)
}

func isParenlessArgumentStart(tt ast.TokenType) bool {
	switch tt {
	case ast.TokenLParen, ast.TokenLBracket, ast.TokenLBrace, ast.TokenMinus, ast.TokenPlus:
		return false
	case ast.TokenIf, ast.TokenUnless, ast.TokenWhile, ast.TokenUntil:
		// A statement modifier keyword following a complete expression guards the
		// statement; it never opens a parenless argument. (`if` is otherwise a
		// prefix-expression starter, so without this `foo if cond` would parse as
		// `foo(if cond ... end)`. Passing an if/unless expression as an argument
		// requires explicit parentheses.)
		return false
	case ast.TokenAmpersand:
		return true
	}
	return prefixParserKind(tt) != prefixParserNone
}

func (p *parser) percentArrayLiteralArgumentAt(pos ast.Position) bool {
	// The allowance gates the whole probe, not just the scan it ends in, so a
	// source of dead candidates stops touching the input entirely once the
	// allowance is gone rather than still resolving a position per candidate.
	if p.l.percentScan.spent() {
		p.l.percentScan.noteDeclined(pos)
		return false
	}
	offset, ok := p.l.offsetForPosition(pos)
	if !ok || !offsetHasLeadingWhitespace(p.l.input, offset) {
		return false
	}
	_, _, _, ok, tooDeep := scanPercentArrayLiteralAt(p.l.input, offset, p.l.percentScan, p.l.interpDepth)
	if tooDeep {
		// The candidate is a percent literal; what cannot be read is its
		// interpolation. Saying so keeps `puts %W[#{...}]` naming the nesting
		// the way the same literal after an `=` does, rather than declining the
		// probe and leaving the reader to work back from whatever the modulo
		// reading of the line fails on (#46).
		p.addParseError(pos, interpolationTooDeepMessage)
		return false
	}
	return ok
}

// parseRegexLiteral decodes a TokenRegex literal produced by the lexer. The
// token literal is the raw source text `/pattern/flags`; the pattern is taken
// verbatim (Go RE2 syntax, like every string-pattern regex helper) and the
// trailing flags are validated here so unknown or repeated flags report a
// precise parse error instead of failing at first evaluation.
func (p *parser) parseRegexLiteral() ast.Expression {
	raw := p.curToken.Literal
	close := strings.LastIndexByte(raw, '/')
	if len(raw) < 2 || raw[0] != '/' || close <= 0 {
		p.addParseErrorSpan(p.curToken.Pos, tokenEnd(p.curToken), "malformed regex literal")
		return nil
	}
	pattern := raw[1:close]
	rawFlags := raw[close+1:]
	seen := map[byte]bool{}
	for i := range len(rawFlags) {
		flag := rawFlags[i]
		switch flag {
		case 'i', 'm':
			if seen[flag] {
				p.addParseErrorSpan(p.curToken.Pos, tokenEnd(p.curToken), "repeated regex flag %q", string(flag))
				return nil
			}
			seen[flag] = true
		default:
			p.addParseErrorSpan(p.curToken.Pos, tokenEnd(p.curToken), "unsupported regex flag %q; supported flags are i and m", string(flag))
			return nil
		}
	}
	// Flags normalize to a canonical order so /a/im and /a/mi are the same
	// regex (Ruby compares options as a bitmask, not source order).
	flags := ""
	if seen['i'] {
		flags += "i"
	}
	if seen['m'] {
		flags += "m"
	}
	return &ast.RegexLiteral{Pattern: pattern, Flags: flags, Position: p.curToken.Pos}
}

type prefixParseKind uint8

const (
	prefixParserNone prefixParseKind = iota
	prefixParserIdentifier
	prefixParserIntegerLiteral
	prefixParserFloatLiteral
	prefixParserStringLiteral
	prefixParserInterpolatedStringLiteral
	prefixParserPercentWordsLiteral
	prefixParserPercentSymbolsLiteral
	prefixParserPercentInterpWordsLiteral
	prefixParserPercentInterpSymbolsLiteral
	prefixParserBooleanLiteral
	prefixParserNilLiteral
	prefixParserSymbolLiteral
	prefixParserIvarLiteral
	prefixParserClassVarLiteral
	prefixParserSelfLiteral
	prefixParserGroupedExpression
	prefixParserArrayLiteral
	prefixParserHashLiteral
	prefixParserPrefixExpression
	prefixParserYieldExpression
	prefixParserIfExpression
	prefixParserUnlessExpression
	prefixParserCaseExpression
	prefixParserBeginExpression
	prefixParserForExpression
	prefixParserWhileExpression
	prefixParserUntilExpression
	prefixParserRegexLiteral
	prefixParserRemovedLambdaLiteral
)

func prefixParserKind(tt ast.TokenType) prefixParseKind {
	switch tt {
	case ast.TokenIdent, ast.TokenThen:
		return prefixParserIdentifier
	case ast.TokenInt:
		return prefixParserIntegerLiteral
	case ast.TokenFloat:
		return prefixParserFloatLiteral
	case ast.TokenString:
		return prefixParserStringLiteral
	case ast.TokenInterpolatedString:
		return prefixParserInterpolatedStringLiteral
	case ast.TokenWords:
		return prefixParserPercentWordsLiteral
	case ast.TokenSymbols:
		return prefixParserPercentSymbolsLiteral
	case ast.TokenInterpWords:
		return prefixParserPercentInterpWordsLiteral
	case ast.TokenInterpSymbols:
		return prefixParserPercentInterpSymbolsLiteral
	case ast.TokenTrue, ast.TokenFalse:
		return prefixParserBooleanLiteral
	case ast.TokenNil:
		return prefixParserNilLiteral
	case ast.TokenSymbol:
		return prefixParserSymbolLiteral
	case ast.TokenIvar:
		return prefixParserIvarLiteral
	case ast.TokenClassVar:
		return prefixParserClassVarLiteral
	case ast.TokenSelf:
		return prefixParserSelfLiteral
	case ast.TokenLParen:
		return prefixParserGroupedExpression
	case ast.TokenLBracket:
		return prefixParserArrayLiteral
	case ast.TokenLBrace:
		return prefixParserHashLiteral
	case ast.TokenBang, ast.TokenMinus, ast.TokenPlus:
		return prefixParserPrefixExpression
	case ast.TokenYield:
		return prefixParserYieldExpression
	case ast.TokenIf:
		return prefixParserIfExpression
	case ast.TokenUnless:
		return prefixParserUnlessExpression
	case ast.TokenCase:
		return prefixParserCaseExpression
	case ast.TokenBegin:
		return prefixParserBeginExpression
	case ast.TokenFor:
		return prefixParserForExpression
	case ast.TokenWhile:
		return prefixParserWhileExpression
	case ast.TokenUntil:
		return prefixParserUntilExpression
	case ast.TokenRegex:
		return prefixParserRegexLiteral
	case ast.TokenThinArrow:
		return prefixParserRemovedLambdaLiteral
	default:
		return prefixParserNone
	}
}

func (p *parser) parsePrefix(kind prefixParseKind) ast.Expression {
	switch kind {
	case prefixParserIdentifier:
		return p.parseIdentifier()
	case prefixParserIntegerLiteral:
		return p.parseIntegerLiteral()
	case prefixParserFloatLiteral:
		return p.parseFloatLiteral()
	case prefixParserStringLiteral:
		return p.parseStringLiteral()
	case prefixParserInterpolatedStringLiteral:
		return p.parseInterpolatedStringLiteral()
	case prefixParserPercentWordsLiteral:
		return p.parsePercentWordsLiteral()
	case prefixParserPercentSymbolsLiteral:
		return p.parsePercentSymbolsLiteral()
	case prefixParserPercentInterpWordsLiteral:
		return p.parsePercentInterpWordsLiteral()
	case prefixParserPercentInterpSymbolsLiteral:
		return p.parsePercentInterpSymbolsLiteral()
	case prefixParserBooleanLiteral:
		return p.parseBooleanLiteral()
	case prefixParserNilLiteral:
		return p.parseNilLiteral()
	case prefixParserSymbolLiteral:
		return p.parseSymbolLiteral()
	case prefixParserIvarLiteral:
		return p.parseIvarLiteral()
	case prefixParserClassVarLiteral:
		return p.parseClassVarLiteral()
	case prefixParserSelfLiteral:
		return p.parseSelfLiteral()
	case prefixParserGroupedExpression:
		return p.parseGroupedExpression()
	case prefixParserArrayLiteral:
		return p.parseArrayLiteral()
	case prefixParserHashLiteral:
		return p.parseHashLiteral()
	case prefixParserPrefixExpression:
		return p.parsePrefixExpression()
	case prefixParserYieldExpression:
		return p.parseYieldExpression()
	case prefixParserIfExpression:
		return p.parseIfExpression()
	case prefixParserUnlessExpression:
		return p.parseUnlessExpression()
	case prefixParserCaseExpression:
		return p.parseCaseExpression()
	case prefixParserBeginExpression:
		return p.parseBeginExpression()
	case prefixParserForExpression:
		return p.parseForExpression()
	case prefixParserWhileExpression:
		return p.parseWhileExpression()
	case prefixParserUntilExpression:
		return p.parseUntilExpression()
	case prefixParserRegexLiteral:
		return p.parseRegexLiteral()
	case prefixParserRemovedLambdaLiteral:
		return p.parseRemovedLambdaLiteral()
	default:
		return nil
	}
}

type infixParseKind uint8

const (
	infixParserNone infixParseKind = iota
	infixParserInfixExpression
	infixParserConditionalExpression
	infixParserRescueExpression
	infixParserRangeExpression
	infixParserCallExpression
	infixParserMemberExpression
	infixParserScopeExpression
	infixParserIndexExpression
	infixParserTrailingBlockExpression
)

func infixParserKind(tt ast.TokenType) infixParseKind {
	switch tt {
	case ast.TokenPlus, ast.TokenMinus, ast.TokenSlash, ast.TokenAsterisk, ast.TokenPower, ast.TokenPercent,
		ast.TokenEQ, ast.TokenCaseEQ, ast.TokenNotEQ, ast.TokenMatch, ast.TokenNotMatch, ast.TokenLT, ast.TokenLTE, ast.TokenGT, ast.TokenGTE,
		ast.TokenSpaceship, ast.TokenAnd, ast.TokenOr, ast.TokenShovel, ast.TokenAmpersand:
		return infixParserInfixExpression
	case ast.TokenQuestion:
		return infixParserConditionalExpression
	case ast.TokenRescue:
		return infixParserRescueExpression
	case ast.TokenRange, ast.TokenRangeExcl:
		return infixParserRangeExpression
	case ast.TokenLParen:
		return infixParserCallExpression
	case ast.TokenDot, ast.TokenSafeNav:
		return infixParserMemberExpression
	case ast.TokenScope:
		return infixParserScopeExpression
	case ast.TokenLBracket:
		return infixParserIndexExpression
	case ast.TokenDo, ast.TokenLBrace:
		return infixParserTrailingBlockExpression
	default:
		return infixParserNone
	}
}

func (p *parser) parseInfix(kind infixParseKind, left ast.Expression) ast.Expression {
	switch kind {
	case infixParserInfixExpression:
		return p.parseInfixExpression(left)
	case infixParserConditionalExpression:
		return p.parseConditionalExpression(left)
	case infixParserRescueExpression:
		return p.parseRescueExpression(left)
	case infixParserRangeExpression:
		return p.parseRangeExpression(left)
	case infixParserCallExpression:
		return p.parseCallExpression(left)
	case infixParserMemberExpression:
		return p.parseMemberExpression(left)
	case infixParserScopeExpression:
		return p.parseScopeExpression(left)
	case infixParserIndexExpression:
		return p.parseIndexExpression(left)
	case infixParserTrailingBlockExpression:
		return p.parseTrailingBlockExpression(left)
	default:
		return nil
	}
}

// maxSplatTargetLines bounds how far past its own line the lookahead behind a
// line-leading "*" may read before giving up. A target list continues across a
// newline whenever it is mid-continuation, and an unclosed bracket group leaves
// it mid-continuation for the rest of the file, so every "*" under one ran the
// lookahead to end of input: 4,000 lines of "* (b", 28 KB, walked 56 MB and
// took over a second, quadrupling per doubling, all of it during compile where
// no step or memory quota reaches it (#38). Bounding the reach makes the whole
// file's lookaheads walk each line at most this many times.
//
// A real target list is written on one line ("*rest, last = values") or splits
// across two ("*rest, (a,\n b) = values"), so 64 is far out of reach of
// anything written on purpose, matching the other parser bounds.
const maxSplatTargetLines = 64

// splatScanCounting and splatScanBytes let a test count the input bytes the
// line-leading "*" lookaheads walk, which is the work the complexity claim is
// about. Wall-clock would fold in scheduling, GC, and the race and coverage
// instrumentation this repository runs across three operating systems. Never
// set outside tests; when off this costs one relaxed load per lookahead.
var (
	splatScanCounting atomic.Bool
	splatScanBytes    atomic.Uint64
)

func noteSplatScan(walked int) {
	if splatScanCounting.Load() {
		splatScanBytes.Add(uint64(walked))
	}
}

func (p *parser) lineLimitedContinuationToken(tok ast.Token) bool {
	switch tok.Type {
	case ast.TokenDot, ast.TokenSafeNav, ast.TokenScope, ast.TokenSlash, ast.TokenPower, ast.TokenPercent, ast.TokenRange, ast.TokenRangeExcl, ast.TokenEQ, ast.TokenCaseEQ, ast.TokenNotEQ, ast.TokenMatch, ast.TokenNotMatch, ast.TokenLT, ast.TokenLTE, ast.TokenGT, ast.TokenGTE, ast.TokenSpaceship, ast.TokenAnd, ast.TokenOr, ast.TokenQuestion, ast.TokenShovel, ast.TokenAmpersand:
		return true
	case ast.TokenAsterisk:
		// A line that begins with "*" continues the previous expression as a
		// multiplication, unless it opens a destructuring-assignment target
		// list (such as "*, last = vals" or "*rest, last = vals"). In that
		// case the statement boundary wins so the "*" parses as an anonymous
		// or named rest target rather than a multiplication operator.
		return !p.lineStartsSplatAssignment(tok)
	case ast.TokenPlus, ast.TokenMinus:
		return p.signContinuesLine(tok)
	default:
		return false
	}
}

// lineStartsSplatAssignment reports whether the leading "*" token begins a
// destructuring-assignment target list rather than continuing the previous
// line as a multiplication. It scans ahead with a throwaway lexer (leaving
// the parser's own lookahead untouched) and accepts only the tokens that may
// form a destructuring left-hand side at the top level, requiring a top-level
// "=" to terminate the list.
//
// A target list may span several physical lines at exactly the points the
// real parser continues a statement across a newline:
//   - inside an open bracket/paren group, where a nested sub-target spans the
//     newline ("*rest, (a,\n b) = values");
//   - after a trailing top-level "," , where the list continues with another
//     element ("*rest,\n last = values"); and
//   - via Vibescript's newline-before-"=" continuation (the same rule that
//     lets "x\n  = 1" parse as an assignment), where the terminating "=" sits
//     on a later line.
//
// To keep the newline-before-"=" continuation from reviving the multiplication
// ambiguity, a bare "*operand" that crosses a newline only completes a splat
// assignment when the leading "*" is shaped like a splat target: bare
// (immediately followed by a terminator such as "," or "=") or flush against
// its operand ("*rest"). A spaced "*" ("* b") is a multiplication operator, so
// "x = a" / "* b" / "= c" stays a multiplication followed by a dangling "="
// rather than becoming "*b = c". Once a top-level "," has appeared the list is
// unambiguously a destructuring list (a comma cannot follow a multiplicand at
// the top level), so the splat-shaped guard no longer applies.
//
// Anything else - a multiplicand operand, a comparison, an arithmetic
// operator, or a token that starts a new line without a continuation point -
// means the "*" is an ordinary multiplication continuation, so it returns
// false.
//
// The lookahead reads at most maxSplatTargetLines lines past the "*", and
// reads a target list that runs longer as the multiplication it stops looking
// for.
func (p *parser) lineStartsSplatAssignment(star ast.Token) bool {
	offset, ok := p.l.offsetForPosition(star.Pos)
	if !ok {
		return false
	}

	scan := p.l.scanFrom(offset)
	defer func() { noteSplatScan(scan.currentOffset() - offset) }()

	tok := scan.NextToken()
	if tok.Type != ast.TokenAsterisk {
		return false
	}
	starEnd := tok.End

	first := true
	splatShaped := false
	sawTopLevelComma := false
	depth := 0
	prev := ast.Token{}
	prevLine := star.Pos.Line
	for {
		tok = scan.NextToken()
		if tok.Type == ast.TokenEOF {
			return false
		}
		if tok.Pos.Line-star.Pos.Line > maxSplatTargetLines {
			return false
		}
		if first {
			// The "*" is splat-shaped when it is bare (a terminator follows) or
			// flush against its operand, distinguishing the splat target "*rest"
			// from the multiplication operator "* b".
			splatShaped = isAnonymousRestTerminator(tok.Type) ||
				(tok.Pos.Line == starEnd.Line && tok.Pos.Column == starEnd.Column)
			first = false
		}
		// A token that starts a later physical line only stays part of the
		// target list when the list was mid-continuation: inside a bracket
		// group, after a trailing top-level comma, when a target splits a member
		// access across the newline ("record\n  .field = values"), or completing
		// the newline-before-"=" rule. Otherwise the leading "*" is a
		// multiplication continuation (such as "x = a" / "* b") and the lookahead
		// stops.
		if tok.Pos.Line > prevLine {
			switch {
			case depth > 0 || prev.Type == ast.TokenComma:
				// Bracket group or trailing-comma continuation: the list keeps
				// going, so fall through to the normal token handling below.
			case (splatShaped || sawTopLevelComma) && splitsMemberAccess(tok, prev):
				// The real target parser uses a line-limited expression, which
				// continues a member or scope access onto a line that begins
				// with "." or "::" ("record\n  .field"). Keep scanning so the
				// element completes instead of severing the list at the newline.
				// This only applies once the list is committed to a splat
				// assignment - a bare "*" target or a top-level comma. A spaced
				// "*" with no comma ("* obj\n  .field") stays a multiplication,
				// the same disambiguation the newline-before-"=" rule uses.
			case tok.Type == ast.TokenAssign && (sawTopLevelComma || splatShaped):
				return true
			default:
				return false
			}
		}
		if depth == 0 {
			if tok.Type == ast.TokenAssign {
				return true
			}
			if !splatAssignmentTopLevelToken(tok, prev) {
				return false
			}
			if tok.Type == ast.TokenComma {
				sawTopLevelComma = true
			}
		}
		switch tok.Type {
		case ast.TokenLParen, ast.TokenLBracket:
			depth++
		case ast.TokenRParen, ast.TokenRBracket:
			if depth > 0 {
				depth--
			}
		case ast.TokenSemicolon:
			return false
		}
		prev = tok
		prevLine = tok.Pos.Line
	}
}

// splitsMemberAccess reports whether tok begins a later physical line that
// continues the current destructuring target's member or scope access, as in
// "record\n  .field = values". The real target parser builds each element with
// a line-limited expression, and lineLimitedContinuationToken lets such an
// expression continue onto a line that starts with "." or "::". The lookahead
// honors the same rule so a split member target completes instead of severing
// the list at the newline (which would let the previous statement consume the
// leading "*" as multiplication). prev is the preceding depth-zero token: the
// continuation only applies when it can end a member-access receiver, so a
// leading "." or "::" with no operand before it is not mistaken for one.
func splitsMemberAccess(tok, prev ast.Token) bool {
	if tok.Type != ast.TokenDot && tok.Type != ast.TokenScope {
		return false
	}
	switch prev.Type {
	case ast.TokenIdent, ast.TokenIvar, ast.TokenClassVar, ast.TokenSelf,
		ast.TokenRParen, ast.TokenRBracket, ast.TokenEnum:
		return true
	default:
		// A member name (the token after a preceding ".") can itself be the
		// receiver of a further "." or "::", as in "a.b\n  .c = values".
		return prev.Type != ast.TokenDot && prev.Type != ast.TokenScope &&
			isMemberNameToken(prev)
	}
}

// splatAssignmentTopLevelToken reports whether tok may appear at the top
// level of a destructuring-assignment left-hand side, between the leading
// "*" and the terminating "=". prev is the preceding depth-zero token, used
// to recognize member names: after a "." the token is a method name and after
// "::" it is a scope name, so each may be any name the real member or scope
// parser accepts (including reserved-word labels such as "end" in
// "record.end = values"). Bracketed sub-targets are validated by depth
// tracking in lineStartsSplatAssignment, so this only governs depth-zero
// tokens. "self" is included because "self.member" and "self[index]" are
// valid assignment targets, so a target list may legitimately begin one of
// its elements with "self".
func splatAssignmentTopLevelToken(tok, prev ast.Token) bool {
	switch prev.Type {
	case ast.TokenDot:
		// parseMemberExpression accepts any member-name token after ".".
		return isMemberNameToken(tok)
	case ast.TokenScope:
		// parseScopeExpression accepts only identifiers and enum names after "::".
		return tok.Type == ast.TokenIdent || tok.Type == ast.TokenEnum
	}
	switch tok.Type {
	case ast.TokenIdent, ast.TokenIvar, ast.TokenClassVar, ast.TokenSelf, ast.TokenComma, ast.TokenAsterisk, ast.TokenDot, ast.TokenScope, ast.TokenLParen, ast.TokenRParen, ast.TokenLBracket, ast.TokenRBracket:
		return true
	default:
		return false
	}
}

// signContinuesLine reports whether a leading `+` or `-` continues the
// previous line as a binary operator rather than beginning a new unary
// expression. A sign immediately adjacent to its operand at the start of a
// fresh line (such as `-b` or `+b`) starts a new statement, while a sign with
// an intervening space or a line break before its operand continues the prior
// line. The flush case matches Ruby, which also treats `a\n-b` as a new `-b`
// statement. The spaced case (`a\n- b`) is Vibescript's indented-continuation
// rule and intentionally differs from Ruby, which would parse it as the two
// statements `a` and `- b` rather than as subtraction.
func (p *parser) signContinuesLine(tok ast.Token) bool {
	if p.peekPeek.Type == ast.TokenEOF {
		return false
	}
	if p.peekPeek.Pos.Line > tok.Pos.Line {
		return true
	}
	return p.peekPeek.Pos.Line == tok.Pos.Line && p.peekPeek.Pos.Column > tok.End.Column
}

func (p *parser) parseIdentifier() ast.Expression {
	return &ast.Identifier{Name: p.curToken.Literal, Position: p.curToken.Pos}
}

// maxIntegerLiteralDigits bounds the length of an out-of-int64-range integer
// literal. The big-integer conversion is superlinear in the digit count and
// runs at parse time, before any execution quota applies, so an unbounded
// literal would let a hostile source burn conversion CPU in every parse
// context (-check, analyze, the LSP). 100,000 digits converts in single-digit
// milliseconds while covering any plausible literal; larger values remain
// constructible at runtime through quota-charged arithmetic.
const maxIntegerLiteralDigits = 100_000

func (p *parser) parseIntegerLiteral() ast.Expression {
	value, err := parseIntegerToken(p.curToken.Literal)
	if err != nil {
		// A literal that overflows int64 is still a valid integer: it promotes
		// to an arbitrary-precision value. Anything else stays a parse error.
		if errors.Is(err, strconv.ErrRange) {
			if len(p.curToken.Literal) > maxIntegerLiteralDigits {
				p.addParseError(p.curToken.Pos, "integer literal exceeds %d digits", maxIntegerLiteralDigits)
				return nil
			}
			if big, ok := parseBigIntegerToken(p.curToken.Literal); ok {
				return &ast.IntegerLiteral{Big: big, Position: p.curToken.Pos}
			}
		}
		p.addParseError(p.curToken.Pos, "invalid integer literal")
		return nil
	}
	return &ast.IntegerLiteral{Value: value, Position: p.curToken.Pos}
}

// parseIntegerToken converts a lexer-produced integer literal into its value.
// The lexer strips underscore separators and validates digit sets, so the
// only remaining work is choosing the radix from any Ruby base prefix. Plain
// decimal literals are parsed in base 10 so a leading zero stays decimal
// rather than being read as octal.
func parseIntegerToken(literal string) (int64, error) {
	if len(literal) >= 2 && literal[0] == '0' {
		switch literal[1] {
		case 'd', 'D':
			return strconv.ParseInt(literal[2:], 10, 64)
		case 'x', 'X', 'b', 'B', 'o', 'O':
			return strconv.ParseInt(literal, 0, 64)
		}
	}
	return strconv.ParseInt(literal, 10, 64)
}

// parseBigIntegerToken converts an out-of-int64-range integer literal into a
// big.Int, honoring the same radix prefixes as parseIntegerToken. The result
// never fits int64 (parseIntegerToken already accepted everything that does),
// so the literal always evaluates to a big-integer value. The literal's length
// is bounded by the engine's source-size guard, which bounds this conversion's
// cost at parse time.
func parseBigIntegerToken(literal string) (*big.Int, bool) {
	if len(literal) >= 2 && literal[0] == '0' {
		switch literal[1] {
		case 'd', 'D':
			return new(big.Int).SetString(literal[2:], 10)
		case 'x', 'X', 'b', 'B', 'o', 'O':
			// Base 0 decodes the 0x/0b/0o prefixes like strconv.ParseInt.
			return new(big.Int).SetString(literal, 0)
		}
	}
	return new(big.Int).SetString(literal, 10)
}

func (p *parser) parseFloatLiteral() ast.Expression {
	value, err := strconv.ParseFloat(p.curToken.Literal, 64)
	if err != nil {
		// An out-of-range exponent overflows to +/-Infinity, which Ruby
		// accepts as a literal value rather than a syntax error. ParseFloat
		// still returns the correct signed infinity alongside ErrRange, so
		// only a genuine syntax error (ErrSyntax) is rejected here.
		if !errors.Is(err, strconv.ErrRange) {
			p.addParseError(p.curToken.Pos, "invalid float literal")
			return nil
		}
	}
	return &ast.FloatLiteral{Value: value, Position: p.curToken.Pos}
}

func (p *parser) parseStringLiteral() ast.Expression {
	return &ast.StringLiteral{Value: p.curToken.Literal, Position: p.curToken.Pos}
}

func (p *parser) parseInterpolatedStringLiteral() ast.Expression {
	parts, ok := p.parseInterpolatedStringParts(p.curToken.Literal, p.curToken.Pos)
	if !ok {
		return nil
	}
	return &ast.InterpolatedString{Parts: parts, Position: p.curToken.Pos}
}

func (p *parser) parseInterpolatedStringParts(raw string, pos ast.Position) ([]ast.StringPart, bool) {
	parts := []ast.StringPart{}
	textStart := 0
	for i := 0; i < len(raw); {
		if raw[i] == '#' && i+1 < len(raw) && raw[i+1] == '{' && !interpolationMarkerEscaped(raw, i) {
			if textStart < i {
				parts = append(parts, ast.StringText{Text: decodeDoubleQuotedText(raw[textStart:i])})
			}
			exprStart := i + 2
			exprEnd, ok, tooDeep := findStringInterpolationEnd(raw, exprStart, p.l.percentScan, p.l.interpDepth+1)
			if !ok {
				if tooDeep {
					p.addParseError(pos, interpolationTooDeepMessage)
					return nil, false
				}
				p.addParseError(pos, "unterminated string interpolation")
				return nil, false
			}
			exprRaw := strings.TrimSpace(raw[exprStart:exprEnd])
			if exprRaw == "" {
				p.addParseError(pos, "empty string interpolation")
				return nil, false
			}
			expr, ok := p.parseStringInterpolationExpression(exprRaw, pos)
			if !ok {
				return nil, false
			}
			parts = append(parts, ast.StringExpr{Expr: expr})
			i = exprEnd + 1
			textStart = i
			continue
		}
		i++
	}
	if textStart < len(raw) {
		parts = append(parts, ast.StringText{Text: decodeDoubleQuotedText(raw[textStart:])})
	}
	return parts, true
}

func interpolationMarkerEscaped(raw string, hash int) bool {
	backslashes := 0
	for i := hash - 1; i >= 0 && raw[i] == '\\'; i-- {
		backslashes++
	}
	return backslashes%2 == 1
}

// findStringInterpolationEnd locates the byte index of the "}" that closes the
// interpolation whose body begins at start (just past the opening "#{"). It
// drives the lexer over the body so that every construct the language
// understands—double- and single-quoted strings, nested "#{...}"
// interpolations, and percent-array literals such as %w/%i/%W/%I—is consumed as
// a single unit. This means a "}" that appears inside one of those constructs
// (for example %W[#{%w[}]}]) does not prematurely close the interpolation, and
// a bare "%" remains the modulo operator wherever the lexer would treat it as
// one.
//
// found is false when the body is never closed before the end of raw, and
// tooDeep says that what stopped it was maxInterpolationDepth rather than the
// input running out. The two are reported apart so the outermost string, the
// one the reader wrote, can name the nesting instead of calling itself
// unterminated.
//
// interpDepth is how many interpolations the body itself sits inside, and
// budget is the enclosing parse's speculative percent-array-literal allowance;
// the throwaway lexer this drives takes both so scans it starts in turn keep
// counting from where this one is and are metered against the same pool.
func findStringInterpolationEnd(raw string, start int, budget *percentScanBudget, interpDepth int) (end int, found, tooDeep bool) {
	if start < 0 || start > len(raw) {
		return 0, false, false
	}
	lex := newLexerWithBudget(raw[start:], budget)
	lex.interpDepth = interpDepth

	braceDepth := 0
	bracketDepth := 0
	parenDepth := 0
	for {
		tok := lex.NextToken()
		switch tok.Type {
		case ast.TokenEOF, ast.TokenIllegal:
			return 0, false, lex.nestingRefused
		case ast.TokenPercent:
			percentOffset := start + lex.currentOffset() - 1
			kind, _, endOffset, ok, tooDeep := scanPercentArrayLiteralAt(raw, percentOffset, budget, interpDepth)
			if tooDeep {
				// A literal inside this body nests past the bound, so the body
				// cannot be read whichever way the `%` goes. Reporting that
				// rather than lexing on keeps the refusal traveling out to
				// whoever asked for the interpolation's end.
				return 0, false, true
			}
			if ok && interpolationPercentArrayArgumentScanCanAdvance(raw, endOffset) {
				lex.seek(endOffset-start, ast.Token{Type: percentArrayLiteralTokenType(kind)})
			}
		case ast.TokenLParen:
			parenDepth++
		case ast.TokenRParen:
			if parenDepth > 0 {
				parenDepth--
			}
		case ast.TokenLBracket:
			bracketDepth++
		case ast.TokenRBracket:
			if bracketDepth > 0 {
				bracketDepth--
			}
		case ast.TokenLBrace:
			braceDepth++
		case ast.TokenRBrace:
			if braceDepth > 0 {
				braceDepth--
				continue
			}
			if bracketDepth == 0 && parenDepth == 0 {
				// The lexer has consumed the closing "}"; currentOffset now
				// points at the rune after it, so the "}" itself sits one byte
				// back ("}" is always a single byte).
				return start + lex.currentOffset() - 1, true, false
			}
		}
	}
}

func interpolationPercentArrayArgumentScanCanAdvance(raw string, endOffset int) bool {
	if endOffset >= len(raw) {
		return true
	}
	r, _ := utf8.DecodeRuneInString(raw[endOffset:])
	return r != '"' && r != '\''
}

func percentArrayLiteralTokenType(kind rune) ast.TokenType {
	switch kind {
	case 'w':
		return ast.TokenWords
	case 'i':
		return ast.TokenSymbols
	case 'W':
		return ast.TokenInterpWords
	case 'I':
		return ast.TokenInterpSymbols
	default:
		return ast.TokenIllegal
	}
}

func (p *parser) parseStringInterpolationExpression(raw string, pos ast.Position) (ast.Expression, bool) {
	exprParser := newParser(raw)
	exprParser.syntaxDepth = p.syntaxDepth
	if p.nodeDepths == nil {
		p.nodeDepths = make(map[ast.Node]syntaxNodeDepth)
	}
	exprParser.nodeDepths = p.nodeDepths
	// Inherit the enclosing local scopes so name-sensitive parsing (such as
	// percent-literal vs modulo disambiguation) resolves locals the same way
	// inside #{...} as it would inline. The copy keeps the sub-parser's scope
	// stack independent while sharing the (read-only) name sets.
	exprParser.localScopes = append([]localScope(nil), p.localScopes...)
	// The interpolation body is part of the enclosing source, so its
	// speculative scanning draws on the enclosing parse's allowance rather
	// than being handed a fresh one per interpolation, and its own
	// interpolations count from the depth this one sits at rather than
	// starting over (see maxInterpolationDepth).
	exprParser.l.percentScan = p.l.percentScan
	exprParser.l.interpDepth = p.l.interpDepth + 1
	expr := exprParser.parseLineExpression(lowestPrec)
	if exprParser.nestingError != nil {
		p.rejectNesting(pos)
		return nil, false
	}
	if len(exprParser.errors) > 0 {
		// The sub-parser's message is a finished diagnostic, not source text:
		// whatever source it quotes was already bounded where it was
		// interpolated, and nesting is capped at maxInterpolationDepth, so it
		// carries here in full. Bounding it again would cut the half that says
		// what is actually wrong.
		p.addParseError(pos, "invalid string interpolation: %s", parseErrorMessage(exprParser.errors[0]))
		return nil, false
	}
	if expr == nil {
		p.addParseError(pos, "invalid string interpolation")
		return nil, false
	}
	if exprParser.peekToken.Type != ast.TokenEOF {
		p.addParseError(pos, "string interpolation must contain a single expression")
		return nil, false
	}
	return expr, true
}

func parseErrorMessage(err error) string {
	if parseErr, ok := errors.AsType[*parseError](err); ok {
		return parseErr.Message()
	}
	return err.Error()
}

func decodeDoubleQuotedText(raw string) string {
	var sb strings.Builder
	for i := 0; i < len(raw); {
		r, size := utf8.DecodeRuneInString(raw[i:])
		if r != '\\' {
			sb.WriteRune(r)
			i += size
			continue
		}
		i += size
		if i >= len(raw) {
			sb.WriteRune('\\')
			break
		}
		next, nextSize := utf8.DecodeRuneInString(raw[i:])
		switch next {
		case '"', '\\':
			sb.WriteRune(next)
		case 'a':
			sb.WriteByte('\a')
		case 'b':
			sb.WriteByte('\b')
		case 'e':
			sb.WriteByte(0x1b)
		case 'f':
			sb.WriteByte('\f')
		case 'n':
			sb.WriteByte('\n')
		case 'r':
			sb.WriteByte('\r')
		case 't':
			sb.WriteByte('\t')
		case 'v':
			sb.WriteByte('\v')
		case 'x':
			decoded, ok := decodeVariableHexEscape(raw, i+nextSize, 1, 2)
			if ok {
				sb.WriteByte(decoded.byte)
				i = decoded.next
				continue
			}
			sb.WriteRune(next)
		case 'u':
			decoded, ok := decodeFixedHexEscape(raw, i+nextSize, 4)
			if ok {
				sb.WriteRune(decoded.rune)
				i = decoded.next
				continue
			}
			sb.WriteRune(next)
		default:
			sb.WriteRune(next)
		}
		i += nextSize
	}
	return sb.String()
}

type rawDecodedEscape struct {
	rune rune
	byte byte
	next int
}

func decodeFixedHexEscape(raw string, start, digits int) (rawDecodedEscape, bool) {
	if start+digits > len(raw) {
		return rawDecodedEscape{}, false
	}
	value := rune(0)
	for i := range digits {
		r, size := utf8.DecodeRuneInString(raw[start+i:])
		if size != 1 || !isBaseDigit(r, 16) {
			return rawDecodedEscape{}, false
		}
		value = value*16 + hexRuneValue(r)
	}
	if value > utf8.MaxRune || (value >= 0xd800 && value <= 0xdfff) {
		return rawDecodedEscape{}, false
	}
	return rawDecodedEscape{rune: value, next: start + digits}, true
}

func decodeVariableHexEscape(raw string, start, minDigits, maxDigits int) (rawDecodedEscape, bool) {
	value := rune(0)
	nextOffset := start
	digits := 0
	for digits < maxDigits && nextOffset < len(raw) {
		r, size := utf8.DecodeRuneInString(raw[nextOffset:])
		if size != 1 || !isBaseDigit(r, 16) {
			break
		}
		value = value*16 + hexRuneValue(r)
		nextOffset += size
		digits++
	}
	if digits < minDigits {
		return rawDecodedEscape{}, false
	}
	if value > utf8.MaxRune || (value >= 0xd800 && value <= 0xdfff) {
		return rawDecodedEscape{}, false
	}
	return rawDecodedEscape{rune: value, byte: byte(value), next: nextOffset}, true
}

func (p *parser) parsePercentWordsLiteral() ast.Expression {
	entries := decodePercentLiteralEntries(p.curToken.Literal)
	elements := make([]ast.Expression, len(entries))
	for i, entry := range entries {
		elements[i] = &ast.StringLiteral{Value: entry, Position: p.curToken.Pos}
	}
	return &ast.ArrayLiteral{Elements: elements, Position: p.curToken.Pos}
}

// parseRegexCommandArgument re-reads a command-argument regex literal whose
// opening slash the lexer scanned as division (canParseParenlessCall has
// already validated the callee and spacing). The lexer decides regex-vs-
// division from the token preceding the slash, so the parser repositions it
// at the slash behind a synthetic comma — a token that cannot end an
// expression — and the ordinary prefix-position regex rule takes over. That
// reuses the one regex scanner and its diagnostics: an unterminated literal
// surfaces as the lexer's "unterminated regex literal" error, and the token
// then flows through the normal argument path so flags, trailing postfixes,
// and following arguments behave exactly like the parenthesized form.
func (p *parser) parseRegexCommandArgument() ast.Expression {
	offset, ok := p.l.offsetForPosition(p.curToken.Pos)
	if !ok {
		p.errorUnexpected(p.curToken)
		return nil
	}
	pos := p.curToken.Pos
	p.reprimeAt(offset, ast.Token{Type: ast.TokenComma, Literal: ",", Pos: pos, End: pos})
	p.nextToken()
	return p.parseParenlessArgumentExpression()
}

func (p *parser) parsePercentArrayLiteralArgument() ast.Expression {
	pos := p.curToken.Pos
	offset, ok := p.l.offsetForPosition(p.curToken.Pos)
	if !ok {
		return nil
	}
	kind, entries, endOffset, ok, tooDeep := scanPercentArrayLiteralAt(p.l.input, offset, p.l.percentScan, p.l.interpDepth)
	if !ok {
		if tooDeep {
			p.addParseError(pos, interpolationTooDeepMessage)
		}
		return nil
	}
	end := p.l.positionForOffset(endOffset)
	elements := make([]ast.Expression, len(entries))
	litType := ast.TokenWords
	for i, entry := range entries {
		switch kind {
		case 'w':
			elements[i] = &ast.StringLiteral{Value: entry, Position: pos}
		case 'i':
			elements[i] = &ast.SymbolLiteral{Name: entry, Position: pos}
			litType = ast.TokenSymbols
		case 'W':
			element, ok := p.interpolatedWordElement(entry, pos)
			if !ok {
				return nil
			}
			elements[i] = element
			litType = ast.TokenInterpWords
		case 'I':
			element, ok := p.interpolatedSymbolElement(entry, pos)
			if !ok {
				return nil
			}
			elements[i] = element
			litType = ast.TokenInterpSymbols
		}
	}
	// The lexer already speculatively tokenized the literal's interior
	// (treating the leading % as modulo), so its lookahead — and the
	// bytes it has consumed — cannot be trusted past this point: a word
	// such as "#" would otherwise start a comment that swallows the
	// closing delimiter and following lines. Reposition the lexer to the
	// byte after the literal and rebuild the lookahead from there instead
	// of re-lexing the interior.
	p.reprimeAt(endOffset, ast.Token{Type: litType, Pos: pos, End: end})

	array := &ast.ArrayLiteral{Elements: elements, Position: pos}
	// Continue parsing so trailing postfixes (such as `[i]` or `.member`) and
	// operators bind to the literal, matching how other parenless arguments are
	// parsed through the normal expression continuation rather than returning
	// the bare array and leaving the postfix to apply to the whole call.
	lineLimited := p.lineLimitedExprs > 0
	limitLine := 0
	if lineLimited {
		limitLine = pos.Line
	}
	return p.continueExpressionParse(array, lowestPrec, limitLine, lineLimited)
}

func (p *parser) parsePercentSymbolsLiteral() ast.Expression {
	entries := decodePercentLiteralEntries(p.curToken.Literal)
	elements := make([]ast.Expression, len(entries))
	for i, entry := range entries {
		elements[i] = &ast.SymbolLiteral{Name: entry, Position: p.curToken.Pos}
	}
	return &ast.ArrayLiteral{Elements: elements, Position: p.curToken.Pos}
}

func (p *parser) parsePercentInterpWordsLiteral() ast.Expression {
	entries := decodePercentLiteralEntries(p.curToken.Literal)
	elements := make([]ast.Expression, 0, len(entries))
	for _, entry := range entries {
		element, ok := p.interpolatedWordElement(entry, p.curToken.Pos)
		if !ok {
			return nil
		}
		elements = append(elements, element)
	}
	return &ast.ArrayLiteral{Elements: elements, Position: p.curToken.Pos}
}

func (p *parser) parsePercentInterpSymbolsLiteral() ast.Expression {
	entries := decodePercentLiteralEntries(p.curToken.Literal)
	elements := make([]ast.Expression, 0, len(entries))
	for _, entry := range entries {
		element, ok := p.interpolatedSymbolElement(entry, p.curToken.Pos)
		if !ok {
			return nil
		}
		elements = append(elements, element)
	}
	return &ast.ArrayLiteral{Elements: elements, Position: p.curToken.Pos}
}

// interpolatedWordElement builds a single %W entry. Entries without an
// embedded expression collapse to a plain string literal so they match the
// AST produced by %w; entries with interpolation become an InterpolatedString.
func (p *parser) interpolatedWordElement(entry string, pos ast.Position) (ast.Expression, bool) {
	parts, ok := p.parseInterpolatedStringParts(entry, pos)
	if !ok {
		return nil, false
	}
	if text, plain := staticStringPart(parts); plain {
		return &ast.StringLiteral{Value: text, Position: pos}, true
	}
	return &ast.InterpolatedString{Parts: parts, Position: pos}, true
}

// interpolatedSymbolElement builds a single %I entry. Entries without an
// embedded expression collapse to a plain symbol literal so they match the
// AST produced by %i; entries with interpolation become an InterpolatedSymbol.
func (p *parser) interpolatedSymbolElement(entry string, pos ast.Position) (ast.Expression, bool) {
	parts, ok := p.parseInterpolatedStringParts(entry, pos)
	if !ok {
		return nil, false
	}
	if text, plain := staticStringPart(parts); plain {
		return &ast.SymbolLiteral{Name: text, Position: pos}, true
	}
	return &ast.InterpolatedSymbol{Parts: parts, Position: pos}, true
}

// staticStringPart returns the literal text and true when parts hold no
// embedded expression, so a %W/%I entry can collapse to a plain literal.
func staticStringPart(parts []ast.StringPart) (string, bool) {
	switch len(parts) {
	case 0:
		return "", true
	case 1:
		if text, ok := parts[0].(ast.StringText); ok {
			return text.Text, true
		}
	}
	return "", false
}

// percentScanBudgetFactor sets a parse's speculative percent-array-literal
// allowance as a multiple of the source length. Ordinary sources spend almost
// none of it: a `%` not followed by w/i/W/I and a percent-literal delimiter is
// rejected within three runes, and a candidate that does turn out to be a
// literal is not charged at all. Four times the source is therefore far above
// anything real code reaches, while a source made entirely of dead candidates
// stops at five times its own length (the allowance plus the one scan that
// overruns it) instead of its square.
const percentScanBudgetFactor = 4

// percentScanCounting and percentScanBytes let a test count the input bytes the
// speculative percent-array-literal scans walk, which is the work the
// complexity claim is about. Wall-clock would fold in scheduling and the race
// and coverage instrumentation this repository runs across three operating
// systems, and allocated bytes only approximate the walk. Never set outside
// tests; when off this costs one relaxed load per completed scan.
var (
	percentScanCounting atomic.Bool
	percentScanBytes    atomic.Uint64
)

// percentScanBudget bounds the input that one parse's speculative
// percent-array-literal scans may walk without finding a literal.
//
// scanPercentArrayLiteralAt second-guesses a `%` the lexer already tokenized as
// modulo, and it can only report failure by walking to the end of the input,
// because a delimiter that has not balanced yet may still balance later. Its
// callers advance a single byte past the `%` afterwards, so a source built from
// repeated ` %w[` candidates makes every candidate pay for another near-full
// re-scan. Nested interpolations compound it: each such scan re-enters
// findStringInterpolationEnd for every `#{` it crosses, and each of those
// speculates again. Charging the fruitless scans against one shared allowance
// keeps the total linear in the source size.
//
// The allowance is held by pointer and shared with every lexer and sub-parser
// that takes part in the same parse, including the throwaway ones these scans
// create. Spend is deliberately not rolled back by parser.restore: a
// speculative parse that is discarded still did the scanning.
//
// Running out is reported rather than absorbed. Past that point the parser
// stops second-guessing `%`, so a later `foo %w[a b]` reads as modulo instead
// of a literal argument -- a different program, not a slower parse. See
// parser.addPercentScanExhaustedError.
type percentScanBudget struct {
	remaining int

	// declinedAt is where the exhausted allowance first turned a probe away,
	// so the diagnostic can point at the source rather than at end of file.
	declinedAt ast.Position
}

func newPercentScanBudget(sourceLen int) *percentScanBudget {
	return &percentScanBudget{remaining: sourceLen * percentScanBudgetFactor}
}

// spent reports whether the allowance is gone, in which case no further
// speculative scan runs and a `%` stays modulo.
//
// A nil budget is unmetered. Nothing in a parse produces one -- every lexer is
// constructed with an allowance -- so this only covers a direct call to the
// scan functions from a test.
func (b *percentScanBudget) spent() bool {
	return b != nil && b.remaining <= 0
}

// noteDeclined records where an exhausted allowance first turned a probe away.
// Only the first is kept: it is the earliest point from which a percent literal
// could have been misread, and the diagnostic wants to name that, not the last
// place the parser happened to look.
func (b *percentScanBudget) noteDeclined(pos ast.Position) {
	if b == nil || b.declinedAt != (ast.Position{}) {
		return
	}
	b.declinedAt = pos
}

// record accounts for a scan that walked the given number of input bytes. Only
// a scan that found no literal draws the allowance down: one that did find a
// literal is productive work whose caller consumes the bytes it covered, so it
// is bounded by the literal's own length, and charging for it would penalize
// sources that use percent literals heavily.
func (b *percentScanBudget) record(walked int, found bool) {
	if percentScanCounting.Load() {
		percentScanBytes.Add(uint64(walked))
	}
	if b == nil || found {
		return
	}
	b.remaining -= walked
}

// scanPercentArrayLiteralAt second-guesses a `%` the lexer tokenized as modulo,
// reporting the literal it opens when it opens one.
//
// tooDeep says the candidate is a percent literal whose interpolation nests
// past maxInterpolationDepth, which is not the same answer as "not a literal":
// declining it silently would leave the `%` reading as modulo, and the `#{`
// that follows then opens a comment that swallows the rest of the line, so the
// source would be reported as whatever the remains of the line fail on instead
// of as the nesting the identical literal after an `=` reports (#46).
func scanPercentArrayLiteralAt(input string, start int, budget *percentScanBudget, interpDepth int) (kind rune, entries []string, end int, found, tooDeep bool) {
	if start < 0 || start >= len(input) || input[start] != '%' {
		return 0, nil, 0, false, false
	}
	if budget.spent() {
		return 0, nil, 0, false, false
	}
	idx := start + 1
	if idx >= len(input) {
		return 0, nil, 0, false, false
	}
	kind, width := utf8.DecodeRuneInString(input[idx:])
	if kind != 'w' && kind != 'i' && kind != 'W' && kind != 'I' {
		return 0, nil, 0, false, false
	}
	interpolating := kind == 'W' || kind == 'I'
	idx += width
	if idx >= len(input) {
		return 0, nil, 0, false, false
	}
	open, width := utf8.DecodeRuneInString(input[idx:])
	close, paired := percentLiteralClose(open)
	if close == 0 {
		return 0, nil, 0, false, false
	}
	idx += width

	depth := 1
	var raw strings.Builder
	for idx < len(input) {
		r, width := utf8.DecodeRuneInString(input[idx:])
		idx += width
		if r == '\\' {
			raw.WriteRune(r)
			if idx < len(input) {
				next, nextWidth := utf8.DecodeRuneInString(input[idx:])
				idx += nextWidth
				raw.WriteRune(next)
			}
			continue
		}
		// Skip over #{...} interpolation spans for the interpolating forms so a
		// delimiter inside an interpolation expression (including one nested in a
		// quoted string, e.g. %W[#{"]"}]) does not close the literal early. The
		// span is matched with the same string-aware logic used elsewhere. When
		// '#' is itself the closing delimiter it must close the literal instead of
		// being treated as interpolation, mirroring Ruby where %W#a #{b}# closes at
		// the first '#'.
		if interpolating && close != '#' && r == '#' && idx < len(input) && input[idx] == '{' {
			raw.WriteRune(r)
			raw.WriteByte('{')
			idx++
			end, ok, tooDeep := findStringInterpolationEnd(input, idx, budget, interpDepth+1)
			if !ok {
				// An unterminated interpolation is only reported once the lexer
				// driving it has reached the end of the input, so the walk this
				// scan is charged for runs to there too.
				budget.record(len(input)-start, false)
				return 0, nil, 0, false, tooDeep
			}
			raw.WriteString(input[idx : end+1])
			idx = end + 1
			continue
		}
		if paired && r == open {
			depth++
		}
		if r == close {
			depth--
			if depth == 0 {
				budget.record(idx-start, true)
				if interpolating {
					return kind, splitInterpolatedPercentLiteralWords(raw.String(), budget, interpDepth), idx, true, false
				}
				return kind, splitPercentLiteralWords(raw.String(), open, close), idx, true, false
			}
		}
		raw.WriteRune(r)
	}
	budget.record(len(input)-start, false)
	return 0, nil, 0, false, false
}

func offsetHasLeadingWhitespace(input string, offset int) bool {
	if offset <= 0 || offset > len(input) {
		return false
	}
	prev, _ := utf8.DecodeLastRuneInString(input[:offset])
	return prev == ' ' || prev == '\t' || prev == '\r' || prev == '\n'
}

func (p *parser) parseBooleanLiteral() ast.Expression {
	return &ast.BoolLiteral{Value: p.curToken.Type == ast.TokenTrue, Position: p.curToken.Pos}
}

func (p *parser) parseNilLiteral() ast.Expression {
	return &ast.NilLiteral{Position: p.curToken.Pos}
}

func (p *parser) parseSymbolLiteral() ast.Expression {
	return &ast.SymbolLiteral{Name: p.curToken.Literal, Position: p.curToken.Pos}
}

func (p *parser) parseIvarLiteral() ast.Expression {
	if p.curToken.Literal == "" {
		p.errorExpected(p.curToken, "instance variable name")
		return nil
	}
	return &ast.IvarExpr{Name: p.curToken.Literal, Position: p.curToken.Pos}
}

func (p *parser) parseClassVarLiteral() ast.Expression {
	if p.curToken.Literal == "" {
		p.errorExpected(p.curToken, "class variable name")
		return nil
	}
	return &ast.ClassVarExpr{Name: p.curToken.Literal, Position: p.curToken.Pos}
}

func (p *parser) parseSelfLiteral() ast.Expression {
	return &ast.Identifier{Name: "self", Position: p.curToken.Pos}
}

func (p *parser) parseGroupedExpression() ast.Expression {
	p.groupDepth++
	defer func() { p.groupDepth-- }()
	p.nextToken()
	p.lineLimitedStopSuppression++
	expr := p.parseExpression(lowestPrec)
	p.lineLimitedStopSuppression--
	if !p.expectPeek(ast.TokenRParen) {
		return nil
	}
	return expr
}

func (p *parser) parsePrefixExpression() ast.Expression {
	pos := p.curToken.Pos
	operator := p.curToken.Type
	p.nextToken()
	if lit, folded := p.parseNegatedNumericLiteral(operator, pos); folded {
		// The numeric token was consumed either way. Returning lit (which is
		// nil on an invalid literal, with its parse error already recorded)
		// avoids re-parsing the same token through the ordinary prefix path,
		// which would report the identical diagnostic a second time and eat
		// into the parser's error budget.
		return lit
	}
	right := p.parseExpression(precPrefix)
	if right == nil {
		return nil
	}
	return &ast.UnaryExpr{Operator: operator, Right: right, Position: pos}
}

// parseNegatedNumericLiteral folds a leading minus into a numeric literal so a
// following member call binds to the negative value, matching Ruby: -5.abs is
// 5, not -(5.abs). Without this the operand parse runs at precPrefix, which is
// below precCall, so the member access is swallowed into the operand and the
// sign ends up applied to the method's result -- silently returning a negative
// number from .abs, and failing outright on -5.to_s.
//
// `**` is deliberately excluded. Ruby binds exponentiation tighter than the
// literal's sign, so -2 ** 2 stays -(2 ** 2) = -4; falling through to the
// ordinary unary path preserves that.
//
// The second return reports whether folding applied, which is distinct from
// whether it produced an expression: an invalid numeric literal consumes its
// token and records a parse error, so the caller must not retry it.
func (p *parser) parseNegatedNumericLiteral(operator ast.TokenType, signPos ast.Position) (ast.Expression, bool) {
	if operator != ast.TokenMinus || p.peekToken.Type == ast.TokenPower {
		return nil, false
	}
	// Ruby folds only an adjacent sign: -5.abs is (-5).abs, but - 5.abs is
	// -(5.abs). Whitespace or a newline between the two keeps the ordinary
	// unary form.
	if p.curToken.Pos.Line != signPos.Line || p.curToken.Pos.Column != signPos.Column+1 {
		return nil, false
	}
	switch p.curToken.Type {
	case ast.TokenInt:
		lit, ok := p.parseIntegerLiteral().(*ast.IntegerLiteral)
		if !ok {
			return nil, true
		}
		if lit.Big != nil {
			negated := new(big.Int).Neg(lit.Big)
			// A magnitude that only overflows int64 while positive fits once
			// negated, so keep the compact form a direct literal would have.
			if negated.IsInt64() {
				return &ast.IntegerLiteral{Value: negated.Int64(), Position: lit.Position}, true
			}
			return &ast.IntegerLiteral{Big: negated, Position: lit.Position}, true
		}
		return &ast.IntegerLiteral{Value: -lit.Value, Position: lit.Position}, true
	case ast.TokenFloat:
		lit, ok := p.parseFloatLiteral().(*ast.FloatLiteral)
		if !ok {
			return nil, true
		}
		return &ast.FloatLiteral{Value: -lit.Value, Position: lit.Position}, true
	default:
		return nil, false
	}
}

func (p *parser) parseInfixExpression(left ast.Expression) ast.Expression {
	pos := p.curToken.Pos
	operator := p.curToken.Type
	precedence := p.curPrecedence()
	p.nextToken()
	rightPrecedence := precedence
	if operator == ast.TokenPower {
		rightPrecedence--
	}
	right := p.parseExpression(rightPrecedence)
	if right == nil {
		return nil
	}
	return &ast.BinaryExpr{Left: left, Operator: operator, Right: right, Position: pos}
}

func (p *parser) parseConditionalExpression(condition ast.Expression) ast.Expression {
	pos := p.curToken.Pos
	p.nextToken()
	consequent := p.parseExpression(lowestPrec)
	if consequent == nil {
		return nil
	}
	if !p.expectPeek(ast.TokenColon) {
		return nil
	}
	p.nextToken()
	alternate := p.parseExpression(precConditional - 1)
	if alternate == nil {
		return nil
	}
	return &ast.ConditionalExpr{
		Condition:  condition,
		Consequent: consequent,
		Alternate:  alternate,
		Position:   pos,
	}
}

func (p *parser) parseRescueExpression(body ast.Expression) ast.Expression {
	pos := p.curToken.Pos
	if p.peekToken.Pos.Line != pos.Line || prefixParserKind(p.peekToken.Type) == prefixParserNone {
		p.addParseError(pos, "rescue modifier requires fallback expression")
		return nil
	}
	p.nextToken()
	fallback := p.parseExpression(precRescue - 1)
	if fallback == nil {
		return nil
	}
	return &ast.RescueExpr{Body: body, Fallback: fallback, Position: pos}
}

func (p *parser) parseRangeExpression(left ast.Expression) ast.Expression {
	pos := p.curToken.Pos
	exclusive := p.curToken.Type == ast.TokenRangeExcl
	precedence := p.curPrecedence()
	if prefixParserKind(p.peekToken.Type) == prefixParserNone ||
		p.peekIsActiveExpressionStop() ||
		((p.groupDepth == 0 || p.atWhenValueGroupDepth()) && p.peekToken.Pos.Line != pos.Line) {
		// Nothing after the dots can start an expression (a closing bracket,
		// comma, end, EOF ...): this is Ruby's endless range. Inside when
		// values the line break itself ends the range (when 3..), while a
		// next-line expression elsewhere still continues a bounded range,
		// preserving multiline endpoints.
		return &ast.RangeExpr{Start: left, Exclusive: exclusive, Position: pos}
	}
	p.nextToken()
	right := p.parseExpression(precedence)
	if right == nil {
		return nil
	}
	return &ast.RangeExpr{Start: left, End: right, Exclusive: exclusive, Position: pos}
}

// parseBeginlessRangeExpression parses Ruby's beginless range (..n / ...n)
// with the range token in prefix position.
// peekIsActiveExpressionStop reports whether the peek token is a stop token of
// the enclosing line-limited expression, such as then in a when clause or
// condition. Range dots followed by such a token cannot continue into a
// bounded endpoint, so the range is endless.
func (p *parser) peekIsActiveExpressionStop() bool {
	return slices.Contains(p.lineLimitedStops, p.peekToken.Type)
}

// atWhenValueGroupDepth reports whether range dots sit directly in a when
// value (at the value's own group nesting), where a line break ends the range.
func (p *parser) atWhenValueGroupDepth() bool {
	n := len(p.whenValueGroupDepths)
	return n > 0 && p.whenValueGroupDepths[n-1] == p.groupDepth
}

func (p *parser) parseBeginlessRangeExpression() ast.Expression {
	pos := p.curToken.Pos
	exclusive := p.curToken.Type == ast.TokenRangeExcl
	if prefixParserKind(p.peekToken.Type) == prefixParserNone {
		p.addParseErrorSpan(pos, tokenEnd(p.curToken), "range is missing end expression")
		return nil
	}
	p.nextToken()
	right := p.parseExpression(precRange)
	if right == nil {
		return nil
	}
	return &ast.RangeExpr{End: right, Exclusive: exclusive, Position: pos}
}

func (p *parser) parseMemberExpression(object ast.Expression) ast.Expression {
	if object == nil {
		return nil
	}
	safe := p.curToken.Type == ast.TokenSafeNav
	p.nextToken()
	if !isMemberNameToken(p.curToken) {
		p.errorExpected(p.curToken, "member name")
		return nil
	}
	// The parent relationship is only visible here, so the ambiguous shape is
	// recorded on the call rather than rediscovered later by walking upward.
	if call, ok := object.(*ast.CallExpr); ok && call.SpacedParen {
		call.SpacedParenTakesMember = true
	}
	if p.memberReceiverProbe != "" && p.curToken.Literal == p.memberReceiverProbe && p.memberReceiver == nil {
		p.memberReceiver = object
		p.memberReceiverParams = p.currentParams
	}
	return &ast.MemberExpr{Object: object, Property: p.curToken.Literal, Safe: safe, Position: object.Pos()}
}

// isSafeMemberCallee reports whether a call's callee is a member access that
// used the safe-navigation operator (`receiver&.method`). Such calls propagate
// the safe flag so the runtime short-circuits to nil when the receiver is nil.
func isSafeMemberCallee(callee ast.Expression) bool {
	member, ok := callee.(*ast.MemberExpr)
	return ok && member.Safe
}

func (p *parser) parseScopeExpression(object ast.Expression) ast.Expression {
	if object == nil {
		return nil
	}
	p.nextToken()
	if p.curToken.Type != ast.TokenIdent && p.curToken.Type != ast.TokenEnum {
		p.errorExpected(p.curToken, "identifier")
		return nil
	}
	return &ast.ScopeExpr{Object: object, Property: p.curToken.Literal, Position: object.Pos()}
}

func (p *parser) parseIndexExpression(object ast.Expression) ast.Expression {
	p.groupDepth++
	defer func() { p.groupDepth-- }()
	pos := p.curToken.Pos
	if p.peekToken.Type == ast.TokenRBracket {
		p.addParseError(p.peekToken.Pos, "index expression requires at least one selector")
		return nil
	}
	p.nextToken()
	p.lineLimitedStopSuppression++
	defer func() {
		p.lineLimitedStopSuppression--
	}()
	indices := []ast.Expression{}
	index := p.parseExpression(lowestPrec)
	if index == nil {
		return nil
	}
	indices = append(indices, index)
	for p.peekToken.Type == ast.TokenComma {
		p.nextToken()
		p.nextToken()
		next := p.parseExpression(lowestPrec)
		if next == nil {
			return nil
		}
		indices = append(indices, next)
	}
	if !p.expectPeek(ast.TokenRBracket) {
		return nil
	}
	return &ast.IndexExpr{Object: object, Indices: indices, Position: pos}
}

func isMemberNameToken(tok ast.Token) bool {
	if isLabelNameToken(tok) {
		return true
	}
	// The spaceship operator doubles as the `<=>` comparison method name.
	return tok.Type == ast.TokenSpaceship
}

func (p *parser) parseArrayLiteral() ast.Expression {
	p.groupDepth++
	defer func() { p.groupDepth-- }()
	pos := p.curToken.Pos
	elements := []ast.Expression{}

	if p.peekToken.Type == ast.TokenRBracket {
		p.nextToken()
		return &ast.ArrayLiteral{Elements: elements, Position: pos}
	}

	p.nextToken()
	p.lineLimitedStopSuppression++
	defer func() {
		p.lineLimitedStopSuppression--
	}()
	elements = append(elements, p.parseExpression(lowestPrec))

	for p.peekToken.Type == ast.TokenComma {
		p.nextToken()
		if p.peekToken.Type == ast.TokenRBracket {
			break
		}
		p.nextToken()
		elements = append(elements, p.parseExpression(lowestPrec))
	}

	if !p.expectPeek(ast.TokenRBracket) {
		return nil
	}

	return &ast.ArrayLiteral{Elements: elements, Position: pos}
}

func (p *parser) parseHashLiteral() ast.Expression {
	pos := p.curToken.Pos
	shapeType, structuralError := p.speculativeShapeLiteralType()
	if shapeType == nil && !structuralError {
		return p.parseHashLiteralGroup()
	}
	hashSnapshot := p.snapshot()
	hash := p.parseHashLiteralGroup()
	if hashLit, ok := hash.(*ast.HashLiteral); ok && len(p.errors) == hashSnapshot.errorCount {
		if shapeType != nil {
			// The group reads both ways; evaluation picks the shape unless a
			// runtime binding shadows one of the type names.
			hashLit.ShapeType = shapeType
		}
		// A structurally invalid shape that still reads cleanly as a hash
		// (duplicate label keys) keeps the hash reading: duplicate data keys
		// are legal, and host globals may shadow the type names at runtime.
		// An unshadowed schema typo still fails the check path through its
		// undefined identifiers.
		return hashLit
	}
	// The group reads only under the type grammar (e.g. { note: string |
	// nil }): drop the failed hash parse and re-consume it as a shape, which
	// either yields the shape or surfaces its structural diagnostic.
	p.restore(hashSnapshot)
	p.parseTypeShape()
	if shapeType == nil {
		return nil
	}
	return &ast.HashLiteral{ShapeType: shapeType, Position: pos}
}

func (p *parser) parseHashLiteralGroup() ast.Expression {
	p.groupDepth++
	defer func() { p.groupDepth-- }()
	pos := p.curToken.Pos
	pairs := []ast.HashPair{}

	if p.peekToken.Type == ast.TokenRBrace {
		p.nextToken()
		return &ast.HashLiteral{Pairs: pairs, Position: pos}
	}

	p.nextToken()
	p.lineLimitedStopSuppression++
	defer func() {
		p.lineLimitedStopSuppression--
	}()
	if pair := p.parseHashPair(); pair.Key != nil {
		pairs = append(pairs, pair)
	}

	for p.peekToken.Type == ast.TokenComma {
		p.nextToken()
		if p.peekToken.Type == ast.TokenRBrace {
			break
		}
		p.nextToken()
		if pair := p.parseHashPair(); pair.Key != nil {
			pairs = append(pairs, pair)
		}
	}

	if !p.expectPeek(ast.TokenRBrace) {
		return nil
	}

	return &ast.HashLiteral{Pairs: pairs, Position: pos}
}

// speculativeShapeLiteralType parses a braced group in expression position
// under the type grammar (ADR-004: shapes become legal in expression
// position, e.g. JSON.parse_as(raw, { name: string })) and always restores
// the parser. The group qualifies as a shape only when it parses cleanly,
// every named leaf resolves to a built-in type, no field names a local
// value, and none of the hash-default degeneracies apply. Requiring
// built-in leaves keeps hashes with identifier values (`{ status: pending }`)
// on the value path, so their undefined-name diagnostics are unchanged, and
// it makes every accepted shape resolvable without an environment. Nullable
// outer shapes are rejected so `{ ... }?` never swallows the `?` of a
// ternary. Host-provided globals that reuse a type name cannot be seen at
// parse time, so the caller keeps the hash reading alongside and evaluation
// decides between the two.
func (p *parser) speculativeShapeLiteralType() (*ast.TypeExpr, bool) {
	if p.peekToken.Type == ast.TokenRBrace {
		return nil, false
	}

	saved := p.snapshot()
	defer p.restore(saved)
	pos := p.curToken.Pos
	shape := p.parseTypeShape()
	if shape == nil {
		// A structural shape error (duplicate field) means the field values
		// all parsed as types, so the braces are a malformed shape rather
		// than a hash; mirror bracedGroupIsShapeType and keep the diagnostic.
		return nil, p.shapeStructurallyInvalid
	}
	if shape.Kind != ast.TypeShape || shape.Nullable ||
		len(p.errors) != saved.errorCount ||
		p.peekToken.Type == ast.TokenQuestion ||
		shapeHasDegenerateNilField(shape) ||
		shapeHasEmptyNestedShape(shape) ||
		p.shapeFieldNamesLocalValue(shape) ||
		!shapeLeafTypesAllBuiltin(shape) {
		return nil, false
	}
	shape.Position = pos
	return shape, false
}

// shapeLeafTypesAllBuiltin reports whether every named leaf of the type
// resolves to a built-in type. A leaf left as an enum or unknown type names
// an identifier the expression grammar should treat as a value reference.
func shapeLeafTypesAllBuiltin(ty *ast.TypeExpr) bool {
	if ty == nil {
		return false
	}
	switch ty.Kind {
	case ast.TypeEnum, ast.TypeUnknown:
		return false
	}
	for _, option := range ty.Union {
		if !shapeLeafTypesAllBuiltin(option) {
			return false
		}
	}
	for _, arg := range ty.TypeArgs {
		if !shapeLeafTypesAllBuiltin(arg) {
			return false
		}
	}
	for _, field := range ty.Shape {
		if !shapeLeafTypesAllBuiltin(field) {
			return false
		}
	}
	return true
}

func (p *parser) parseHashPair() ast.HashPair {
	if p.peekToken.Type != ast.TokenColon {
		p.addParseError(p.curToken.Pos, invalidHashPairMessage)
		p.recoverHashPair()
		return ast.HashPair{}
	}

	var key ast.Expression
	// labelKey records a label-style key (name:) so its value may be omitted as
	// shorthand for the matching local variable.
	var labelKey *ast.SymbolLiteral
	switch {
	case isLabelNameToken(p.curToken):
		labelKey = &ast.SymbolLiteral{Name: p.curToken.Literal, Position: p.curToken.Pos}
		key = labelKey
	case p.curToken.Type == ast.TokenString:
		key = &ast.StringLiteral{Value: p.curToken.Literal, Position: p.curToken.Pos}
	default:
		p.addParseError(p.curToken.Pos, invalidHashPairMessage)
		p.recoverHashPair()
		return ast.HashPair{}
	}
	p.nextToken()
	if p.peekToken.Type == ast.TokenComma || p.peekToken.Type == ast.TokenRBrace || p.peekToken.Type == ast.TokenEOF {
		// Label keys support value omission: {name:} reads the local variable
		// `name`, matching call-site keyword shorthand (greet name:). Missing
		// locals fall through to the normal undefined-variable diagnostic at
		// evaluation time.
		if labelKey != nil {
			value := &ast.Identifier{Name: labelKey.Name, Position: labelKey.Position}
			return ast.HashPair{Key: key, Value: value}
		}
		p.addParseError(p.peekToken.Pos, "missing value for hash key %s", srcText(hashKeyName(key)))
		return ast.HashPair{}
	}

	p.nextToken()
	value := p.parseExpression(lowestPrec)
	if value == nil {
		p.recoverHashPair()
		return ast.HashPair{}
	}
	switch p.peekToken.Type {
	case ast.TokenComma, ast.TokenRBrace, ast.TokenEOF:
		return ast.HashPair{Key: key, Value: value}
	default:
		p.addParseError(p.peekToken.Pos, invalidHashPairMessage)
		p.recoverHashPair()
		return ast.HashPair{}
	}
}

// recoverHashPair advances the parser past a malformed hash entry so the
// surrounding hash literal can resume cleanly. It positions peekToken at the
// next top-level "," or "}" (or EOF), skipping over any balanced parentheses,
// brackets, or braces so that removed syntax such as a hash rocket yields a
// single actionable error instead of cascading diagnostics.
func (p *parser) recoverHashPair() {
	nesting := 0
	// curToken can already be an opener when recovery begins (e.g. the rejected
	// entry starts with "{", "[", or "(" as in `{ {a: 1} => v }`). In that case
	// the cursor is already inside that delimiter, so seed nesting accordingly to
	// keep its matching closer from being mistaken for the outer hash boundary.
	switch p.curToken.Type {
	case ast.TokenLParen, ast.TokenLBracket, ast.TokenLBrace:
		nesting++
	}
	for p.peekToken.Type != ast.TokenEOF {
		if nesting == 0 && (p.peekToken.Type == ast.TokenComma || p.peekToken.Type == ast.TokenRBrace) {
			return
		}
		p.nextToken()
		switch p.curToken.Type {
		case ast.TokenLParen, ast.TokenLBracket, ast.TokenLBrace:
			nesting++
		case ast.TokenRParen, ast.TokenRBracket, ast.TokenRBrace:
			if nesting > 0 {
				nesting--
			}
		}
	}
}

const invalidHashPairMessage = `invalid hash pair: expected key like name: or "name":`

func hashKeyName(key ast.Expression) string {
	switch k := key.(type) {
	case *ast.SymbolLiteral:
		return k.Name
	case *ast.StringLiteral:
		return k.Value
	default:
		return "unknown"
	}
}

func (p *parser) parseBlockLiteral() *ast.BlockLiteral {
	pos := p.curToken.Pos
	params := []ast.Param{}
	hasExplicitParams := false
	stopToken := ast.TokenEnd
	stopName := "end"
	if p.curToken.Type == ast.TokenLBrace {
		stopToken = ast.TokenRBrace
		stopName = "}"
	}

	p.nextToken()
	switch p.curToken.Type {
	case ast.TokenPipe:
		hasExplicitParams = true
		var ok bool
		params, ok = p.parseBlockParameters()
		if !ok {
			return nil
		}
		p.nextToken()
	case ast.TokenOr:
		hasExplicitParams = true
		p.nextToken()
	}

	inferImplicitIt := !p.isDeclaredLocal("it")
	p.pushLocalScope(params, false)
	if !hasExplicitParams {
		p.declareImplicitBlockParamCandidates()
	}
	body := p.parseBlock(stopToken)
	p.popLocalScope()
	if p.curToken.Type != stopToken {
		p.errorExpected(p.curToken, stopName)
	}

	implicitParams := []string(nil)
	if !hasExplicitParams {
		implicitParams = inferImplicitBlockParams(body, inferImplicitIt)
	}

	return &ast.BlockLiteral{Params: params, ImplicitParams: implicitParams, Body: body, Position: pos}
}

func (p *parser) parseBlockParameters() ([]ast.Param, bool) {
	params := []ast.Param{}
	p.nextToken()
	if p.curToken.Type == ast.TokenPipe {
		return params, true
	}

	param, ok := p.parseBlockParameter()
	if !ok {
		return nil, false
	}
	params = append(params, param)

	for p.peekToken.Type == ast.TokenComma {
		p.nextToken()
		p.nextToken()
		if p.curToken.Type == ast.TokenPipe {
			p.addParseError(p.curToken.Pos, "trailing comma in block parameter list")
			return nil, false
		}
		param, ok := p.parseBlockParameter()
		if !ok {
			return nil, false
		}
		params = append(params, param)
	}

	if !p.expectPeek(ast.TokenPipe) {
		return nil, false
	}

	return params, true
}

// parseRemovedLambdaLiteral reports the removal of the stabby lambda literal.
// `->` remains the return-type annotation on a `def` signature line; in
// expression position it used to open a callable value, which ADR-006 removed
// because executable code that escapes its call is what makes lifetime,
// capability, and memory accounting unpredictable. The diagnostic names both
// replacements because the right one depends on why the author reached for a
// lambda: a reusable transformation is a named function, and a one-off
// transformation passed to an enumerator is a block.
func (p *parser) parseRemovedLambdaLiteral() ast.Expression {
	p.addParseError(p.curToken.Pos, "lambda literals are not supported; executable code is not a value. Define a named function and call it, or attach a block to the call that runs it, as in `people.map { |person| person.name }`")
	return nil
}

func (p *parser) parseBlockParameter() (ast.Param, bool) {
	switch p.curToken.Type {
	case ast.TokenIdent:
		param := ast.Param{Name: p.curToken.Literal}
		if p.peekToken.Type == ast.TokenColon {
			p.nextToken()
			p.nextToken()
			param.Type = p.parseBlockParamType()
			if param.Type == nil {
				return ast.Param{}, false
			}
		}
		return param, true
	case ast.TokenLParen:
		return p.parseDestructuredBlockParameter(ast.TokenRParen, ")")
	case ast.TokenLBracket:
		return p.parseDestructuredBlockParameter(ast.TokenRBracket, "]")
	default:
		p.errorExpected(p.curToken, "block parameter")
		return ast.Param{}, false
	}
}

func (p *parser) parseDestructuredBlockParameter(stop ast.TokenType, stopName string) (ast.Param, bool) {
	target := p.parseNestedDestructureTarget(stop, stopName, true)
	if target == nil {
		return ast.Param{}, false
	}
	if !isBlockParameterTarget(target) {
		p.addParseError(target.Pos(), "invalid block parameter destructuring target")
		return ast.Param{}, false
	}
	return ast.Param{Target: target}, true
}

func isBlockParameterTarget(target ast.Expression) bool {
	switch t := target.(type) {
	case *ast.Identifier:
		return true
	case *ast.DestructureTarget:
		for _, element := range t.Elements {
			// A rest element with no target is an anonymous (discard) rest
			// ("*"), which is a valid block parameter that binds nothing.
			if element.Rest && element.Target == nil {
				continue
			}
			if !isBlockParameterTarget(element.Target) {
				return false
			}
		}
		return true
	default:
		return false
	}
}

func (p *parser) parseBlockParamType() *ast.TypeExpr {
	first := p.parseTypeAtom()
	if first == nil {
		return nil
	}

	union := []*ast.TypeExpr{first}
	for p.peekToken.Type == ast.TokenPipe && p.blockParamUnionContinues() {
		p.nextToken()
		p.nextToken()
		next := p.parseTypeAtom()
		if next == nil {
			return nil
		}
		union = append(union, next)
	}

	if len(union) == 1 {
		return first
	}

	names := make([]string, len(union))
	for i, option := range union {
		names[i] = ast.FormatTypeExpr(option)
	}
	return &ast.TypeExpr{
		Name:     strings.Join(names, " | "),
		Kind:     ast.TypeUnion,
		Union:    union,
		Position: first.Position,
	}
}

func (p *parser) blockParamUnionContinues() bool {
	if p.peekToken.Type != ast.TokenPipe {
		return false
	}

	saved := p.snapshot()
	defer p.restore(saved)

	p.nextToken()
	p.nextToken()
	atom := p.parseTypeAtom()
	return atom != nil && (p.peekToken.Type == ast.TokenComma || p.peekToken.Type == ast.TokenPipe)
}

func (p *parser) parseCallExpression(function ast.Expression) ast.Expression {
	spacedParen := p.curTokenIsSpacedFromPrevious()
	p.groupDepth++
	defer func() { p.groupDepth-- }()
	if function == nil {
		return nil
	}
	expr := &ast.CallExpr{Callee: function, Position: function.Pos(), Safe: isSafeMemberCallee(function), Parenthesized: true, SpacedParen: spacedParen}
	args := []ast.Expression{}
	kwargs := []ast.KeywordArg{}

	if p.peekToken.Type == ast.TokenRParen {
		p.nextToken()
		expr.Args = args
		expr.KwArgs = kwargs
		return expr
	}

	p.nextToken()
	p.lineLimitedStopSuppression++
	p.parseCallArgument(&args, &kwargs)

	for p.peekToken.Type == ast.TokenComma {
		p.nextToken()
		if p.peekToken.Type == ast.TokenRParen {
			break
		}
		p.nextToken()
		if len(kwargs) > 0 && !argumentMayFollowKeywords(p.curToken, p.peekToken) {
			p.addParseError(p.curToken.Pos, "positional arguments cannot follow keyword arguments")
		}
		p.parseCallArgument(&args, &kwargs)
	}
	p.lineLimitedStopSuppression--

	if !p.expectPeek(ast.TokenRParen) {
		return nil
	}

	expr.Args = args
	expr.KwArgs = kwargs
	// Mark keyword arguments as eligible to collapse into a positional options
	// hash. The runtime decides whether the collapse actually applies: plain
	// function calls (including a function value's `call` alias) collapse like
	// the parenless form, while parenthesized method and constructor calls stay
	// strict. The parser cannot distinguish a function value's `call` alias from
	// a method named `call`, so it defers that decision to the runtime.
	if len(kwargs) > 0 {
		expr.KeywordOptionsHash = true
	}
	if p.canAttachPeekBlock() {
		p.nextToken()
		expr.Block = p.parseBlockLiteral()
	}
	return expr
}

// maxParenlessCallDepth bounds how deeply parenless calls may nest. A parenless
// call parses its argument as a full expression, and that argument may be
// another parenless call, so `a a a a ...` on one line is a(a(a(...))) and
// drives seven parser frames per identifier. Without a bound, 500,000 of them
// -- half of the default MaxSourceBytes, so an ordinary upload -- overflowed the
// 1 GB goroutine stack, an uncatchable fatal that takes the host down rather
// than the script, and 200,000 already took 10s to parse (#47). Real code stacks
// two or three deep (`puts format x`), so the cap is far out of reach of
// anything written on purpose, and it matches maxTypeDepth, the same guard over
// the other unbounded parser recursion.
const maxParenlessCallDepth = 64

func (p *parser) parseParenlessCallExpression(function ast.Expression) ast.Expression {
	if function == nil {
		return nil
	}
	p.parenlessCallDepth++
	defer func() { p.parenlessCallDepth-- }()
	if p.parenlessCallDepth > maxParenlessCallDepth {
		p.addParseError(p.peekToken.Pos, "parenless call nesting too deep")
		return nil
	}
	expr := &ast.CallExpr{Callee: function, Position: function.Pos(), Safe: isSafeMemberCallee(function)}
	args := []ast.Expression{}
	kwargs := []ast.KeywordArg{}
	keywordOptionsHash := false

	p.nextToken()
	p.parseParenlessCallArgument(&args, &kwargs, &keywordOptionsHash)

	// TokenLBracket is excluded from isParenlessArgumentStart because a
	// bracket after the callee is spacing-sensitive (indexing vs argument),
	// but after a comma there is no indexing reading: "f x, [1]" can only
	// continue the argument list with an array literal.
	for p.peekToken.Type == ast.TokenComma &&
		p.peekToken.Pos.Line == p.curToken.Pos.Line &&
		p.peekPeek.Pos.Line == p.curToken.Pos.Line &&
		(isParenlessArgumentStart(p.peekPeek.Type) || isLabelNameToken(p.peekPeek) ||
			p.peekPeek.Type == ast.TokenAsterisk || p.peekPeek.Type == ast.TokenPower ||
			p.peekPeek.Type == ast.TokenLBracket) {
		p.nextToken()
		p.nextToken()
		if keywordOptionsHash && !argumentMayFollowKeywords(p.curToken, p.peekToken) {
			p.addParseError(p.curToken.Pos, "positional arguments cannot follow bare keyword arguments in parenless calls")
		}
		p.parseParenlessCallArgument(&args, &kwargs, &keywordOptionsHash)
	}

	expr.Args = args
	expr.KwArgs = kwargs
	expr.KeywordOptionsHash = keywordOptionsHash
	return expr
}

func (p *parser) parseTrailingBlockExpression(callee ast.Expression) ast.Expression {
	return p.callWithBlock(callee, p.parseBlockLiteral())
}

func (p *parser) callWithBlock(callee ast.Expression, block *ast.BlockLiteral) ast.Expression {
	if callee == nil || p.nestingError != nil {
		return nil
	}
	var call *ast.CallExpr
	if existing, ok := callee.(*ast.CallExpr); ok {
		call = existing
	} else {
		call = &ast.CallExpr{Callee: callee, Position: callee.Pos(), Safe: isSafeMemberCallee(callee)}
	}
	p.nodeDepth(call, 0)
	cached := p.nodeDepths[call]
	cached.depth = cached.callBase
	if block != nil {
		cached.depth = max(cached.depth, 1+p.nodeDepth(block, 0))
	}
	call.Block = block
	p.nodeDepths[call] = cached
	if !p.checkSyntaxNode(call) {
		return nil
	}
	return call
}

func (p *parser) canAttachPeekBlock() bool {
	if p.lineLimitedExprs > 0 && p.peekStopsLineExpression() {
		return false
	}
	if p.peekToken.Type == ast.TokenDo {
		return true
	}
	return p.peekToken.Type == ast.TokenLBrace && p.peekToken.Pos.Line == p.curToken.Pos.Line
}

func (p *parser) parseCallArgument(args *[]ast.Expression, kwargs *[]ast.KeywordArg) {
	switch p.curToken.Type {
	case ast.TokenAsterisk:
		pos := p.curToken.Pos
		p.nextToken()
		value := p.parseExpression(lowestPrec)
		if value != nil {
			*args = append(*args, &ast.SplatArg{Value: value, Position: pos})
		}
		return
	case ast.TokenPower:
		p.nextToken()
		value := p.parseExpression(lowestPrec)
		if value != nil {
			*kwargs = append(*kwargs, ast.KeywordArg{Value: value, Splat: true})
		}
		return
	}

	if p.curToken.Type == ast.TokenAmpersand {
		p.parseRemovedBlockPassArgument(false)
		return
	}

	if isLabelNameToken(p.curToken) && p.peekToken.Type == ast.TokenColon {
		name := p.curToken.Literal
		pos := p.curToken.Pos
		p.nextToken()
		if p.peekToken.Type == ast.TokenComma || p.peekToken.Type == ast.TokenRParen {
			*kwargs = append(*kwargs, ast.KeywordArg{Name: name, Value: &ast.Identifier{Name: name, Position: pos}})
			return
		}
		p.nextToken()
		value := p.parseExpression(lowestPrec)
		if value == nil {
			return
		}
		*kwargs = append(*kwargs, ast.KeywordArg{Name: name, Value: value})
		return
	}

	if typeLit := p.speculativeArgumentTypeLiteral(); typeLit != nil {
		*args = append(*args, typeLit)
		return
	}

	expr := p.parseExpression(lowestPrec)
	if expr != nil {
		*args = append(*args, expr)
	}
}

// speculativeArgumentTypeLiteral parses a parenthesized call argument under
// the type grammar, extending ADR-004's expression-position shape literals to
// non-shape roots (`JSON.parse_as(raw, array<int>)`). The argument reads as a
// type only when the whole group parses as one ending exactly at an argument
// boundary and every named leaf resolves to a built-in type, so ordinary
// identifier arguments (`f(count)`) and comparisons over locals
// (`f(array < brr)`) keep their value reading. A bare `nil` root stays the
// nil literal, and braced groups keep the established shape-literal path.
//
// When the same tokens also read as a value expression (`int` as an
// identifier, `int | nil` as a bitwise or), that reading is kept as the
// literal's fallback: evaluation prefers the type value and falls back to the
// expression when a type name is shadowed by a runtime binding, mirroring the
// dual-reading braced groups.
func (p *parser) speculativeArgumentTypeLiteral() ast.Expression {
	if p.curToken.Type != ast.TokenIdent && p.curToken.Type != ast.TokenNil {
		return nil
	}
	saved := p.snapshot()
	pos := p.curToken.Pos
	ty := p.parseTypeExpr()
	ok := ty != nil && len(p.errors) == saved.errorCount &&
		(p.peekToken.Type == ast.TokenComma || p.peekToken.Type == ast.TokenRParen) &&
		ty.Kind != ast.TypeNil && ty.Kind != ast.TypeShape &&
		shapeLeafTypesAllBuiltin(ty)
	typeEnd := p.curToken.Pos
	p.restore(saved)
	if !ok {
		return nil
	}

	fallback := p.parseExpression(lowestPrec)
	if fallback == nil || len(p.errors) != saved.errorCount ||
		(p.peekToken.Type != ast.TokenComma && p.peekToken.Type != ast.TokenRParen) ||
		p.curToken.Pos != typeEnd {
		// No value reading spans the same tokens (array<int> stops parsing at
		// the type arguments): re-consume the group under the type grammar
		// and keep the type-only literal.
		p.restore(saved)
		ty = p.parseTypeExpr()
		if ty == nil {
			return nil
		}
		return &ast.TypeLiteral{Type: ty, Position: pos}
	}
	return &ast.TypeLiteral{Type: ty, Fallback: fallback, Position: pos}
}

func (p *parser) parseParenlessCallArgument(args *[]ast.Expression, kwargs *[]ast.KeywordArg, keywordOptionsHash *bool) {
	if p.curToken.Type == ast.TokenPercent {
		expr := p.parsePercentArrayLiteralArgument()
		if expr != nil {
			*args = append(*args, expr)
		}
		return
	}

	if p.curToken.Type == ast.TokenSlash {
		expr := p.parseRegexCommandArgument()
		if expr != nil {
			*args = append(*args, expr)
		}
		return
	}

	switch p.curToken.Type {
	case ast.TokenAsterisk:
		pos := p.curToken.Pos
		p.nextToken()
		value := p.parseParenlessArgumentExpression()
		if value != nil {
			*args = append(*args, &ast.SplatArg{Value: value, Position: pos})
		}
		return
	case ast.TokenPower:
		p.nextToken()
		value := p.parseParenlessArgumentExpression()
		if value != nil {
			*kwargs = append(*kwargs, ast.KeywordArg{Value: value, Splat: true})
			*keywordOptionsHash = true
		}
		return
	}

	if p.curToken.Type == ast.TokenAmpersand {
		p.parseRemovedBlockPassArgument(true)
		return
	}

	if isLabelNameToken(p.curToken) && p.peekToken.Type == ast.TokenColon {
		name := p.curToken.Literal
		pos := p.curToken.Pos
		p.nextToken()
		*keywordOptionsHash = true
		if p.parenlessKeywordArgumentCanUseShorthand() {
			*kwargs = append(*kwargs, ast.KeywordArg{Name: name, Value: &ast.Identifier{Name: name, Position: pos}})
			return
		}
		p.nextToken()
		value := p.parseParenlessArgumentExpression()
		if value == nil {
			return
		}
		*kwargs = append(*kwargs, ast.KeywordArg{Name: name, Value: value})
		return
	}

	expr := p.parseParenlessArgumentExpression()
	if expr != nil {
		*args = append(*args, expr)
	}
}

// argumentMayFollowKeywords reports whether the argument starting at cur may
// legally follow keyword arguments in a call: another keyword label, a
// keyword splat (`**opts`), or the trailing block argument (`&blk`). Only
// plain positional arguments (including `*splat`) must precede keywords.
func argumentMayFollowKeywords(cur, peek ast.Token) bool {
	if cur.Type == ast.TokenPower || cur.Type == ast.TokenAmpersand {
		return true
	}
	return isLabelNameToken(cur) && peek.Type == ast.TokenColon
}

func (p *parser) parenlessKeywordArgumentCanUseShorthand() bool {
	if p.peekToken.Type == ast.TokenEOF || p.peekToken.Type == ast.TokenComma {
		return true
	}
	return p.peekToken.Pos.Line != p.curToken.Pos.Line
}

// parseRemovedBlockPassArgument reports the removal of the ampersand block
// argument. `f(&blk)` forwarded a captured block, `f(&fn)` a function value,
// and `f(&:name)` a symbol-to-proc, all of which turn a call's block into a
// value that outlives the call it was written on. ADR-006 removed them. The
// operand is still consumed so the rest of the argument list keeps parsing and
// the author sees one diagnostic rather than a cascade.
func (p *parser) parseRemovedBlockPassArgument(parenless bool) {
	ampPos := p.curToken.Pos
	p.nextToken()
	if parenless {
		p.parseParenlessArgumentExpression()
	} else {
		p.parseExpression(lowestPrec)
	}
	p.addParseError(ampPos, "block arguments are not supported; a block is not a value. Write the block at the call that runs it, as in `words.map { |word| word.upcase }`")
}

// isLabelNameToken reports whether a token may appear immediately before a
// colon as a label, such as a hash key (`{rescue: 1}`) or a keyword argument
// (`call(begin: 1)`). Every reserved keyword that can precede a colon is
// allowed, mirroring Ruby, which treats keyword-shaped labels uniformly.
func isLabelNameToken(tok ast.Token) bool {
	switch tok.Type {
	case ast.TokenIdent,
		ast.TokenDef, ast.TokenClass, ast.TokenEnum, ast.TokenExport, ast.TokenSelf, ast.TokenPrivate, ast.TokenProperty, ast.TokenGetter, ast.TokenSetter,
		ast.TokenBegin, ast.TokenRescue, ast.TokenEnsure, ast.TokenRaise,
		ast.TokenEnd, ast.TokenReturn, ast.TokenYield, ast.TokenDo, ast.TokenThen, ast.TokenFor, ast.TokenWhile, ast.TokenUntil,
		ast.TokenBreak, ast.TokenNext, ast.TokenRetry, ast.TokenIn, ast.TokenIf, ast.TokenUnless, ast.TokenCase, ast.TokenWhen, ast.TokenElsif, ast.TokenElse,
		ast.TokenTrue, ast.TokenFalse, ast.TokenNil:
		return true
	default:
		return false
	}
}

func (p *parser) parseIfExpression() ast.Expression {
	pos := p.curToken.Pos
	p.nextToken()
	condition := p.parseLineExpressionUntilForced(lowestPrec, ast.TokenThen)
	if condition == nil {
		return nil
	}

	p.nextToken()
	p.consumeIfExpressionResultSeparator()
	consequent := p.parseExpressionWithBlock()
	if consequent == nil {
		return nil
	}
	p.nextToken()
	p.skipStatementSeparators()

	var elseifBranches []ast.IfExprBranch
	for p.curToken.Type == ast.TokenElsif {
		p.nextToken()
		cond := p.parseLineExpressionUntilForced(lowestPrec, ast.TokenThen)
		if cond == nil {
			return nil
		}
		p.nextToken()
		p.consumeIfExpressionResultSeparator()
		result := p.parseExpressionWithBlock()
		if result == nil {
			return nil
		}
		elseifBranches = append(elseifBranches, ast.IfExprBranch{Condition: cond, Result: result})
		p.nextToken()
		p.skipStatementSeparators()
	}

	var alternate ast.Expression
	if p.curToken.Type == ast.TokenElse {
		p.nextToken()
		p.skipStatementSeparators()
		alternate = p.parseExpressionWithBlock()
		if alternate == nil {
			return nil
		}
		p.nextToken()
		p.skipStatementSeparators()
	}

	if p.curToken.Type != ast.TokenEnd {
		p.errorExpected(p.curToken, "end")
		return nil
	}

	return &ast.IfExpr{
		Condition:  condition,
		Consequent: consequent,
		ElseIf:     elseifBranches,
		Alternate:  alternate,
		Position:   pos,
	}
}

func (p *parser) parseUnlessExpression() ast.Expression {
	pos := p.curToken.Pos
	p.nextToken()
	condition := p.parseLineExpressionUntilForced(lowestPrec, ast.TokenThen)
	if condition == nil {
		return nil
	}

	p.nextToken()
	p.consumeIfExpressionResultSeparator()
	consequent := p.parseExpressionWithBlock()
	if consequent == nil {
		return nil
	}
	p.nextToken()
	p.skipStatementSeparators()

	var alternate ast.Expression
	if p.curToken.Type == ast.TokenElse {
		p.nextToken()
		p.skipStatementSeparators()
		alternate = p.parseExpressionWithBlock()
		if alternate == nil {
			return nil
		}
		p.nextToken()
		p.skipStatementSeparators()
	}

	if p.curToken.Type != ast.TokenEnd {
		p.errorExpected(p.curToken, "end")
		return nil
	}

	return &ast.IfExpr{
		Condition:  condition,
		Consequent: alternate,
		Alternate:  consequent,
		Position:   pos,
	}
}

func (p *parser) consumeIfExpressionResultSeparator() {
	if p.curToken.Type == ast.TokenThen {
		p.nextToken()
	}
	p.skipStatementSeparators()
}

func (p *parser) parseCaseExpression() ast.Expression {
	pos := p.curToken.Pos
	p.nextToken()
	p.skipStatementSeparators()
	var target ast.Expression
	if p.curToken.Type != ast.TokenWhen {
		target = p.parseLineExpression(lowestPrec)
		if target == nil {
			return nil
		}
		p.nextToken()
		p.skipStatementSeparators()
	}

	clauses := []ast.CaseWhenClause{}
	for p.curToken.Type == ast.TokenWhen {
		p.nextToken()
		values := []ast.CaseWhenValue{}
		first := p.parseCaseWhenValue()
		if first == nil {
			return nil
		}
		values = append(values, *first)
		for p.peekToken.Type == ast.TokenComma {
			p.nextToken()
			p.nextToken()
			value := p.parseCaseWhenValue()
			if value == nil {
				return nil
			}
			values = append(values, *value)
		}

		p.nextToken()
		p.consumeCaseResultSeparator()
		result := p.parseExpressionWithBlock()
		if result == nil {
			return nil
		}
		clauses = append(clauses, ast.CaseWhenClause{Values: values, Result: result})
		p.nextToken()
		p.skipStatementSeparators()
	}

	if len(clauses) == 0 {
		p.errorExpected(p.curToken, "when")
		return nil
	}

	var elseExpr ast.Expression
	if p.curToken.Type == ast.TokenElse {
		p.nextToken()
		p.skipStatementSeparators()
		elseExpr = p.parseExpressionWithBlock()
		if elseExpr == nil {
			return nil
		}
		p.nextToken()
		p.skipStatementSeparators()
	}

	if p.curToken.Type != ast.TokenEnd {
		p.errorExpected(p.curToken, "end")
		return nil
	}

	return &ast.CaseExpr{Target: target, Clauses: clauses, ElseExpr: elseExpr, Position: pos}
}

func (p *parser) parseCaseWhenValue() *ast.CaseWhenValue {
	splat := false
	if p.curToken.Type == ast.TokenAsterisk {
		splat = true
		p.nextToken()
	}
	// Inside when values, range dots ending the line form an endless range
	// (when 3.. matches 3 and up). The entry group depth is recorded so the
	// rule only applies to dots at the when value's own nesting level: a
	// bounded endpoint inside parens or call arguments may still continue
	// onto the next line.
	p.whenValueGroupDepths = append(p.whenValueGroupDepths, p.groupDepth)
	expr := p.parseLineExpressionUntilForced(lowestPrec, ast.TokenThen)
	p.whenValueGroupDepths = p.whenValueGroupDepths[:len(p.whenValueGroupDepths)-1]
	if expr == nil {
		return nil
	}
	return &ast.CaseWhenValue{Expr: expr, Splat: splat}
}

func (p *parser) consumeCaseResultSeparator() {
	if p.curToken.Type == ast.TokenThen {
		p.nextToken()
	}
	p.skipStatementSeparators()
}

func (p *parser) parseBeginExpression() ast.Expression {
	pos := p.curToken.Pos
	p.nextToken()
	body := p.parseBlock(ast.TokenRescue, ast.TokenElse, ast.TokenEnsure, ast.TokenEnd)
	stmt := p.parseRescueElseEnsureTail(pos, body, "begin")
	if stmt == nil {
		return nil
	}
	expr, ok := stmt.(ast.Expression)
	if !ok {
		return nil
	}
	return expr
}

func (p *parser) parseForExpression() ast.Expression {
	stmt := p.parseForStatement()
	if stmt == nil {
		return nil
	}
	return stmt.(ast.Expression)
}

func (p *parser) parseWhileExpression() ast.Expression {
	stmt := p.parseWhileStatement()
	if stmt == nil {
		return nil
	}
	return stmt.(ast.Expression)
}

func (p *parser) parseUntilExpression() ast.Expression {
	stmt := p.parseUntilStatement()
	if stmt == nil {
		return nil
	}
	return stmt.(ast.Expression)
}

func (p *parser) parseYieldExpression() ast.Expression {
	pos := p.curToken.Pos
	var args []ast.Expression
	if p.peekToken.Type == ast.TokenLParen {
		p.nextToken()
		p.nextToken()
		if p.curToken.Type != ast.TokenRParen {
			args = append(args, p.parseExpression(lowestPrec))
			for p.peekToken.Type == ast.TokenComma {
				p.nextToken()
				if p.peekToken.Type == ast.TokenRParen {
					break
				}
				p.nextToken()
				args = append(args, p.parseExpression(lowestPrec))
			}
			if !p.expectPeek(ast.TokenRParen) {
				return nil
			}
		}
	} else if p.peekToken.Pos.Line == pos.Line && prefixParserKind(p.peekToken.Type) != prefixParserNone {
		p.nextToken()
		args = append(args, p.parseLineExpression(lowestPrec))
		for p.peekToken.Type == ast.TokenComma &&
			p.peekToken.Pos.Line == pos.Line &&
			p.peekPeek.Pos.Line == pos.Line &&
			prefixParserKind(p.peekPeek.Type) != prefixParserNone {
			p.nextToken()
			p.nextToken()
			args = append(args, p.parseLineExpression(lowestPrec))
		}
	}
	return &ast.YieldExpr{Args: args, Position: pos}
}
