package parser

import (
	"fmt"
	"strings"
	"sync/atomic"
	"unicode"
	"unicode/utf8"

	"github.com/mgomes/vibescript/internal/ast"
)

// Lexer is the public name for the package-private lexer state.
type Lexer = lexer

// NewLexer returns a new lexer over the given source text.
func NewLexer(input string) *Lexer { return newLexer(input) }

type lexer struct {
	input string

	offset int
	width  int

	line   int
	column int

	// prevLine/prevColumn hold the position of the rune consumed
	// before ch, i.e. the final rune of the token that scanning just
	// moved past. NextToken derives exclusive token ends from them.
	prevLine   int
	prevColumn int

	ch            rune
	prevPrevToken ast.Token
	prevToken     ast.Token
	lastToken     ast.Token

	// bracketDepth counts the open `(`, `[`, and `{` brackets the lexer has
	// scanned past. Each opener increments it and each matching closer
	// decrements it, so it names the bracket nesting level of the rune the
	// lexer is currently at. It tags pending ternaries so a `:` only matches a
	// `?` opened at the same nesting level, never a label `:` inside a hash,
	// array, or paren group opened after the `?`.
	bracketDepth int
	bracketStack *frameStack[bracketFrame]

	// ternaryStack holds each ternary `?` whose separator `:` has not yet been
	// scanned. A `:` in expression-end position closes the innermost pending
	// ternary only when it sits at that ternary's bracket nesting level; such a
	// `:` is the ternary separator rather than a quoted symbol or label
	// introducer. Tagging each `?` with its level keeps a label `:` inside a
	// hash, array, or paren group opened after the `?` (a deeper level) from
	// being mistaken for the separator. The lexer reads ahead of the parser, but
	// this stack only relates `?` tokens to the colons the lexer itself scans, so
	// it stays self-consistent. The parser captures and restores it with the
	// rest of the lexer value during speculative parsing; the stack is immutable
	// (see frameStack) so a rolled-back speculation cannot leak pushes or pops
	// into the live lexer.
	ternaryStack *frameStack[ternaryFrame]

	// percentScan is the parse-wide allowance for the speculative
	// percent-array-literal scans this lexer's interpolation handling starts.
	// Every lexer taking part in one parse -- including the throwaway ones the
	// scans themselves create -- shares the same allowance, so the total work
	// stays bounded no matter how the source nests strings and literals.
	percentScan *percentScanBudget

	// replay resolves token positions to byte offsets and rebuilds the state a
	// seek lands in, both without restarting at byte 0 (see sourceReplay). It
	// is allocated with the lexer rather than on demand so parser.restore hands
	// back the same one, which a rolled-back speculation would otherwise drop
	// and make the next lookup pay to rebuild.
	replay *sourceReplay

	// ternaryScan is the shared record of which label colons of this source
	// precede a ternary separator (see ternaryScanMemo). Every lexer over the
	// same input shares it, including the throwaway ones the scans create.
	ternaryScan *ternaryScanMemo

	// interpDepth is how many "#{...}" bodies the source this lexer reads
	// already sits inside. It is not shared: each throwaway lexer driven over
	// an interpolation body is one level deeper than the one that started it,
	// and so is the sub-parser that reparses that body. See
	// maxInterpolationDepth.
	interpDepth int

	// nestingRefused records that this lexer turned down an interpolation for
	// nesting past maxInterpolationDepth, so the caller driving it can say that
	// rather than reporting the string it was reading as unterminated.
	nestingRefused bool
}

type ternaryFrame struct {
	bracketDepth         int
	parenlessKeywordCall bool
}

type bracketFrame struct {
	token         ast.TokenType
	callArguments bool
}

// frameStack is an immutable stack of lexer frames: pushing returns a new head
// whose tail is the stack pushed onto, which therefore keeps holding exactly
// what it held before. Capturing one is copying a pointer, and no copy has to
// be taken to keep a capture from being written through later.
//
// Both lexer stacks grow with the source and neither is emptied at a statement
// boundary or after a parse error: nothing pops a `?` whose `:` never arrives,
// or a `(` the source never closes. parser.snapshot captures both for every
// speculative parse, and while it copied their slices a source of 50,000 stray
// `?` followed by 10,000 ambiguous braced parameters (`def f(a:{b:c},...)`, one
// speculation each) spent 3.9s and allocated 15 GB doing nothing but copying
// frames -- and 40,000 stray `(` cost the same. Both are 25ms now (#34).
type frameStack[T any] struct {
	frame  T
	under  *frameStack[T]
	height int
}

// push returns the stack with frame on top. The nil stack is the empty one, so
// a lexer needs no stack setup.
func (s *frameStack[T]) push(frame T) *frameStack[T] {
	return &frameStack[T]{frame: frame, under: s, height: s.len() + 1}
}

// len reports how many frames the stack holds.
func (s *frameStack[T]) len() int {
	if s == nil {
		return 0
	}
	return s.height
}

// pop returns the stack without its top frame. Popping the empty stack yields
// the empty stack, so unbalanced input needs no separate guard.
func (s *frameStack[T]) pop() *frameStack[T] {
	if s == nil {
		return nil
	}
	return s.under
}

// top returns the frame on top of the stack and whether there was one.
func (s *frameStack[T]) top() (T, bool) {
	if s == nil {
		var zero T
		return zero, false
	}
	return s.frame, true
}

// replaceTop returns the stack with its top frame replaced. Amending a frame
// goes through here because the frame under the old head may be shared with a
// snapshot or with a scan running over a copy of this lexer, neither of which
// the amendment belongs to.
func (s *frameStack[T]) replaceTop(frame T) *frameStack[T] {
	if s == nil {
		return nil
	}
	return s.under.push(frame)
}

func newLexer(input string) *lexer {
	return newLexerWithBudget(input, newPercentScanBudget(len(input)))
}

// newLexerWithBudget returns a lexer that draws on an in-progress parse's
// speculative percent-array-literal allowance instead of opening a fresh one.
// Every throwaway lexer run over source the parse is already working through
// has to use this: a per-lexer allowance would reset on each one and so would
// bound nothing.
func newLexerWithBudget(input string, budget *percentScanBudget) *lexer {
	l := &lexer{
		input:       input,
		line:        1,
		column:      0,
		percentScan: budget,
		replay:      &sourceReplay{lines: lineIndex{input: input}},
		ternaryScan: &ternaryScanMemo{},
	}
	l.readRune()
	return l
}

func (l *lexer) readRune() {
	l.prevLine, l.prevColumn = l.line, l.column
	if l.offset >= len(l.input) {
		l.width = 0
		l.ch = 0
		return
	}

	r, w := utf8.DecodeRuneInString(l.input[l.offset:])
	l.width = w
	l.offset += w

	if r == '\n' {
		l.line++
		l.column = 0
	} else {
		l.column++
	}

	l.ch = r
}

func (l *lexer) peekRune() rune {
	if l.offset >= len(l.input) {
		return 0
	}
	r, _ := utf8.DecodeRuneInString(l.input[l.offset:])
	return r
}

func (l *lexer) peekRuneN(n int) rune {
	idx := l.offset
	var r rune
	var w int
	for i := range n + 1 {
		if idx >= len(l.input) {
			return 0
		}
		r, w = utf8.DecodeRuneInString(l.input[idx:])
		if i == n {
			return r
		}
		idx += w
	}
	return 0
}

func (l *lexer) NextToken() ast.Token {
	tok := l.scanToken()
	if tok.Type != ast.TokenEOF {
		tok.End = ast.Position{Line: l.prevLine, Column: l.prevColumn + 1}
		l.prevPrevToken = l.prevToken
		l.prevToken = l.lastToken
		l.lastToken = tok
	}
	return tok
}

func (l *lexer) scanToken() ast.Token {
	if tok, ok := l.skipWhitespaceAndComments(); ok {
		return tok
	}

	tok := ast.Token{Pos: ast.Position{Line: l.line, Column: l.column}}

	switch l.ch {
	case 0:
		tok.Type = ast.TokenEOF
		tok.Literal = ""
	case '+':
		if l.peekRune() == '=' {
			first := l.ch
			l.readRune()
			tok = l.makeToken(ast.TokenPlusAssign, string(first)+string(l.ch))
			l.readRune()
		} else {
			tok = l.makeToken(ast.TokenPlus, "+")
			l.readRune()
		}
	case '-':
		if l.peekRune() == '>' {
			first := l.ch
			l.readRune()
			tok = l.makeToken(ast.TokenThinArrow, string(first)+string(l.ch))
			l.readRune()
		} else if l.peekRune() == '=' {
			first := l.ch
			l.readRune()
			tok = l.makeToken(ast.TokenMinusAssign, string(first)+string(l.ch))
			l.readRune()
		} else {
			tok = l.makeToken(ast.TokenMinus, "-")
			l.readRune()
		}
	case '*':
		if l.peekRune() == '*' {
			first := l.ch
			l.readRune()
			if l.peekRune() == '=' {
				second := l.ch
				l.readRune()
				tok = l.makeToken(ast.TokenPowerAssign, string(first)+string(second)+string(l.ch))
				l.readRune()
			} else {
				tok = l.makeToken(ast.TokenPower, string(first)+string(l.ch))
				l.readRune()
			}
		} else if l.peekRune() == '=' {
			first := l.ch
			l.readRune()
			tok = l.makeToken(ast.TokenAsteriskAssign, string(first)+string(l.ch))
			l.readRune()
		} else {
			tok = l.makeToken(ast.TokenAsterisk, "*")
			l.readRune()
		}
	case '/':
		if l.canStartRegexLiteral() {
			literal, err := l.readRegexLiteral()
			if err != "" {
				setDiagnostic(&tok, err)
			} else {
				tok.Type = ast.TokenRegex
				tok.Literal = literal
			}
		} else if l.peekRune() == '=' {
			first := l.ch
			l.readRune()
			tok = l.makeToken(ast.TokenSlashAssign, string(first)+string(l.ch))
			l.readRune()
		} else {
			tok = l.makeToken(ast.TokenSlash, "/")
			l.readRune()
		}
	case '%':
		switch l.peekRune() {
		case 'w':
			if l.canStartPercentArrayLiteral() && isPercentLiteralDelimiter(l.peekRuneN(1)) {
				entries, err := l.readPercentArrayLiteral(false)
				if err != "" {
					setDiagnostic(&tok, err)
				} else {
					tok.Type = ast.TokenWords
					tok.Literal = encodePercentLiteralEntries(entries)
				}
			} else {
				tok = l.makeToken(ast.TokenPercent, "%")
				l.readRune()
			}
		case 'i':
			if l.canStartPercentArrayLiteral() && isPercentLiteralDelimiter(l.peekRuneN(1)) {
				entries, err := l.readPercentArrayLiteral(false)
				if err != "" {
					setDiagnostic(&tok, err)
				} else {
					tok.Type = ast.TokenSymbols
					tok.Literal = encodePercentLiteralEntries(entries)
				}
			} else {
				tok = l.makeToken(ast.TokenPercent, "%")
				l.readRune()
			}
		case 'W':
			if l.canStartPercentArrayLiteral() && isPercentLiteralDelimiter(l.peekRuneN(1)) {
				entries, err := l.readPercentArrayLiteral(true)
				if err != "" {
					setDiagnostic(&tok, err)
				} else {
					tok.Type = ast.TokenInterpWords
					tok.Literal = encodePercentLiteralEntries(entries)
				}
			} else {
				tok = l.makeToken(ast.TokenPercent, "%")
				l.readRune()
			}
		case 'I':
			if l.canStartPercentArrayLiteral() && isPercentLiteralDelimiter(l.peekRuneN(1)) {
				entries, err := l.readPercentArrayLiteral(true)
				if err != "" {
					setDiagnostic(&tok, err)
				} else {
					tok.Type = ast.TokenInterpSymbols
					tok.Literal = encodePercentLiteralEntries(entries)
				}
			} else {
				tok = l.makeToken(ast.TokenPercent, "%")
				l.readRune()
			}
		case '=':
			first := l.ch
			l.readRune()
			tok = l.makeToken(ast.TokenPercentAssign, string(first)+string(l.ch))
			l.readRune()
		default:
			tok = l.makeToken(ast.TokenPercent, "%")
			l.readRune()
		}
	case '(':
		tok = l.makeToken(ast.TokenLParen, "(")
		l.openBracket(ast.TokenLParen)
		l.readRune()
	case ')':
		tok = l.makeToken(ast.TokenRParen, ")")
		l.closeBracket()
		l.readRune()
	case '{':
		tok = l.makeToken(ast.TokenLBrace, "{")
		l.openBracket(ast.TokenLBrace)
		l.readRune()
	case '}':
		tok = l.makeToken(ast.TokenRBrace, "}")
		l.closeBracket()
		l.readRune()
	case '[':
		tok = l.makeToken(ast.TokenLBracket, "[")
		l.openBracket(ast.TokenLBracket)
		l.readRune()
	case ']':
		tok = l.makeToken(ast.TokenRBracket, "]")
		l.closeBracket()
		l.readRune()
	case ',':
		tok = l.makeToken(ast.TokenComma, ",")
		l.readRune()
	case ';':
		tok = l.makeToken(ast.TokenSemicolon, ";")
		l.readRune()
	case ':':
		if l.peekRune() == ':' {
			first := l.ch
			l.readRune()
			tok = l.makeToken(ast.TokenScope, string(first)+string(l.ch))
			l.readRune()
			return tok
		}
		closesTernary := l.colonClosesTernary()
		if closesTernary {
			l.ternaryStack = l.ternaryStack.pop()
		}
		if quote := l.peekRune(); (quote == '"' || quote == '\'') && !closesTernary && l.colonStartsSymbolLiteral() {
			return l.scanQuotedSymbol(tok)
		}
		if !closesTernary && l.colonStartsSymbolLiteral() {
			if tok, ok := l.scanOperatorSymbol(tok); ok {
				return tok
			}
		}
		if !closesTernary && l.colonSeparatesSymbolValue() {
			tok = l.makeToken(ast.TokenColon, ":")
			l.readRune()
			return tok
		}
		if ast.IsIdentifierRune(l.peekRune()) {
			l.readRune()
			start := l.currentOffset()
			for ast.IsIdentifierRune(l.peekRune()) {
				l.readRune()
			}
			literal := l.input[start:l.offset]
			tok.Type = ast.TokenSymbol
			tok.Literal = literal
			l.readRune()
			return tok
		}
		tok = l.makeToken(ast.TokenColon, ":")
		l.readRune()
	case '.':
		if l.peekRune() == '.' {
			first := l.ch
			l.readRune()
			if l.peekRune() == '.' {
				second := l.ch
				l.readRune()
				tok = l.makeToken(ast.TokenRangeExcl, string(first)+string(second)+string(l.ch))
				l.readRune()
			} else {
				tok = l.makeToken(ast.TokenRange, string(first)+string(l.ch))
				l.readRune()
			}
		} else {
			tok = l.makeToken(ast.TokenDot, ".")
			l.readRune()
		}
	case '!':
		if l.peekRune() == '=' {
			first := l.ch
			l.readRune()
			tok = l.makeToken(ast.TokenNotEQ, string(first)+string(l.ch))
			l.readRune()
		} else if l.peekRune() == '~' {
			first := l.ch
			l.readRune()
			tok = l.makeToken(ast.TokenNotMatch, string(first)+string(l.ch))
			l.readRune()
		} else {
			tok = l.makeToken(ast.TokenBang, "!")
			l.readRune()
		}
	case '=':
		switch l.peekRune() {
		case '=':
			if l.peekRuneN(1) == '=' {
				start := ast.Position{Line: l.line, Column: l.column}
				first := l.ch
				l.readRune()
				second := l.ch
				l.readRune()
				tok = ast.Token{Type: ast.TokenCaseEQ, Literal: string(first) + string(second) + string(l.ch), Pos: start}
				l.readRune()
				break
			}
			first := l.ch
			l.readRune()
			tok = l.makeToken(ast.TokenEQ, string(first)+string(l.ch))
			l.readRune()
		case '>':
			first := l.ch
			l.readRune()
			tok = l.makeToken(ast.TokenArrow, string(first)+string(l.ch))
			l.readRune()
		case '~':
			first := l.ch
			l.readRune()
			tok = l.makeToken(ast.TokenMatch, string(first)+string(l.ch))
			l.readRune()
		default:
			tok = l.makeToken(ast.TokenAssign, "=")
			l.readRune()
		}
	case '>':
		if l.peekRune() == '=' {
			first := l.ch
			l.readRune()
			tok = l.makeToken(ast.TokenGTE, string(first)+string(l.ch))
			l.readRune()
		} else {
			tok = l.makeToken(ast.TokenGT, ">")
			l.readRune()
		}
	case '<':
		if l.peekRune() == '=' && l.peekRuneN(1) == '>' {
			start := ast.Position{Line: l.line, Column: l.column}
			first := l.ch
			l.readRune()
			second := l.ch
			l.readRune()
			tok = ast.Token{Type: ast.TokenSpaceship, Literal: string(first) + string(second) + string(l.ch), Pos: start}
			l.readRune()
		} else if l.peekRune() == '=' {
			first := l.ch
			l.readRune()
			tok = l.makeToken(ast.TokenLTE, string(first)+string(l.ch))
			l.readRune()
		} else if l.peekRune() == '<' {
			first := l.ch
			l.readRune()
			tok = l.makeToken(ast.TokenShovel, string(first)+string(l.ch))
			l.readRune()
		} else {
			tok = l.makeToken(ast.TokenLT, "<")
			l.readRune()
		}
	case '&':
		switch l.peekRune() {
		case '&':
			first := l.ch
			l.readRune()
			if l.peekRune() == '=' {
				second := l.ch
				l.readRune()
				tok = l.makeToken(ast.TokenAndAssign, string(first)+string(second)+string(l.ch))
			} else {
				tok = l.makeToken(ast.TokenAnd, string(first)+string(l.ch))
			}
			l.readRune()
		case '.':
			first := l.ch
			l.readRune()
			tok = l.makeToken(ast.TokenSafeNav, string(first)+string(l.ch))
			l.readRune()
		default:
			tok = l.makeToken(ast.TokenAmpersand, "&")
			l.readRune()
		}
	case '?':
		tok = l.makeToken(ast.TokenQuestion, "?")
		l.ternaryStack = l.ternaryStack.push(ternaryFrame{bracketDepth: l.bracketDepth})
		l.readRune()
	case '|':
		if l.peekRune() == '|' {
			first := l.ch
			l.readRune()
			if l.peekRune() == '=' {
				second := l.ch
				l.readRune()
				tok = l.makeToken(ast.TokenOrAssign, string(first)+string(second)+string(l.ch))
			} else {
				tok = l.makeToken(ast.TokenOr, string(first)+string(l.ch))
			}
			l.readRune()
		} else {
			tok = l.makeToken(ast.TokenPipe, "|")
			l.readRune()
		}
	case '"':
		literal, interpolated, err := l.readDoubleQuotedString()
		if err != "" {
			setDiagnostic(&tok, err)
		} else if interpolated {
			tok.Type = ast.TokenInterpolatedString
			tok.Literal = literal
		} else {
			tok.Type = ast.TokenString
			tok.Literal = literal
		}
	case '\'':
		literal, err := l.readSingleQuotedString()
		if err != "" {
			setDiagnostic(&tok, err)
		} else {
			tok.Type = ast.TokenString
			tok.Literal = literal
		}
	default:
		switch {
		case l.ch == '@':
			if l.peekRune() == '@' {
				l.readRune()
				l.readRune()
				start := l.currentOffset()
				for ast.IsIdentifierRune(l.peekRune()) {
					l.readRune()
				}
				literal := l.input[start:l.offset]
				tok.Type = ast.TokenClassVar
				tok.Literal = literal
				l.readRune()
				return tok
			}
			l.readRune()
			start := l.currentOffset()
			for ast.IsIdentifierRune(l.peekRune()) {
				l.readRune()
			}
			literal := l.input[start:l.offset]
			tok.Type = ast.TokenIvar
			tok.Literal = literal
			l.readRune()
			return tok
		case ast.IsIdentifierStart(l.ch):
			literal := l.readIdentifier()
			tok.Type = ast.LookupIdent(literal)
			tok.Literal = literal
			return tok
		case unicode.IsDigit(l.ch):
			num := l.readNumber()
			switch {
			case num.errMsg != "":
				setDiagnostic(&tok, num.errMsg)
			case num.isFloat:
				tok.Type = ast.TokenFloat
				tok.Literal = num.literal
			default:
				tok.Type = ast.TokenInt
				tok.Literal = num.literal
			}
			return tok
		default:
			tok = l.makeToken(ast.TokenIllegal, fmt.Sprintf("unexpected character %q", l.ch))
			l.readRune()
		}
	}

	return tok
}

var operatorSymbolLiterals = []string{
	"[]=", "[]", "===", "<=>", "**", "<<", "<=", ">=", "==", "!=", "&&", "||",
	"+", "-", "*", "/", "%", "<", ">", "&", "|", "!",
}

func (l *lexer) scanOperatorSymbol(tok ast.Token) (ast.Token, bool) {
	start := l.currentOffset() + l.width
	remaining := l.input[start:]
	for _, literal := range operatorSymbolLiterals {
		if !strings.HasPrefix(remaining, literal) {
			continue
		}
		end := start + len(literal)
		for l.currentOffset() < end && l.ch != 0 {
			l.readRune()
		}
		tok.Type = ast.TokenSymbol
		tok.Literal = literal
		return tok, true
	}
	return tok, false
}

func (l *lexer) currentOffset() int {
	return l.offset - l.width
}

// seek repositions the lexer so the next scanned token begins at or after
// the given byte offset. Line and column state comes from the source index
// and the bracket and ternary state from the shared replay, neither of which
// restarts at byte 0 (see sourceReplay). last becomes lastToken so gating
// that depends on the preceding token (such as percent-literal and newline
// handling) behaves as if that token had just been scanned.
func (l *lexer) seek(offset int, last ast.Token) {
	structuralOffset := offset
	if start, ok := l.offsetForPosition(last.Pos); ok && start < offset {
		structuralOffset = start
	}
	bracketDepth, bracketStack, ternaryStack := l.replay.stateBefore(structuralOffset, l.percentScan, l.interpDepth)

	// Landing one rune short of the target and reading it leaves every field
	// readRune maintains -- including the previous rune's position, which the
	// next token's End is derived from -- exactly as a scan that arrived here
	// rune by rune would have left them.
	offset, at := l.replay.lines.runeAt(offset)
	l.offset = offset
	l.width = 0
	l.line = at.Line
	l.column = at.Column - 1
	l.prevLine = 0
	l.prevColumn = 0
	l.ch = 0
	l.bracketDepth = bracketDepth
	l.bracketStack = bracketStack
	l.ternaryStack = ternaryStack
	l.readRune()
	l.prevPrevToken = ast.Token{}
	l.prevToken = ast.Token{}
	l.lastToken = last
}

// scanFrom returns a throwaway lexer over the same source, parked so the next
// token it scans is the one beginning at offset. Whatever it reads leaves this
// lexer's own position untouched.
//
// It shares the source replay and the label-colon record, which describe the
// source rather than one lexer's progress through it. A fresh replay would put
// back the walk from byte 0 that it exists to remove: every line-leading `*`
// asks whether it opens a destructuring assignment, and rebuilding a line index
// and a structural re-lex per question made 8,000 such lines, 48 KB, take 10.5s
// and allocate 2.1 GB (#38).
func (l *lexer) scanFrom(offset int) *lexer {
	scan := &lexer{
		input:       l.input,
		percentScan: l.percentScan,
		replay:      l.replay,
		ternaryScan: l.ternaryScan,
		interpDepth: l.interpDepth,
	}
	// seek sets every field a rune-by-rune arrival would have left, so the
	// zero line and column it starts from are never read.
	scan.seek(offset, ast.Token{})
	return scan
}

func (l *lexer) makeToken(tt ast.TokenType, literal string) ast.Token {
	return ast.Token{Type: tt, Literal: literal, Pos: ast.Position{Line: l.line, Column: l.column}}
}

// setDiagnostic turns tok into an illegal token carrying msg as a lexer
// diagnostic, preserving the token's already-stamped position. The parser
// surfaces such literals verbatim, so the message must be human-readable
// rather than the raw offending source text.
func setDiagnostic(tok *ast.Token, msg string) {
	tok.Type = ast.TokenIllegal
	tok.Literal = msg
	tok.Diagnostic = true
}

func (l *lexer) skipWhitespaceAndComments() (ast.Token, bool) {
	for {
		switch l.ch {
		case ' ', '\t', '\r', '\n':
			l.readRune()
			continue
		case '#':
			l.skipComment()
			continue
		case '=':
			if !l.atLineLeadingWhitespace() || !l.blockCommentMarkerAtCurrent("=begin") {
				return ast.Token{}, false
			}
			pos := ast.Position{Line: l.line, Column: l.column}
			if err := l.skipBlockComment(); err != "" {
				return ast.Token{Type: ast.TokenIllegal, Literal: err, Pos: pos, Diagnostic: true}, true
			}
			continue
		default:
			return ast.Token{}, false
		}
	}
}

func (l *lexer) skipComment() {
	for l.ch != 0 && l.ch != '\n' {
		l.readRune()
	}
}

func (l *lexer) skipBlockComment() string {
	for l.ch != 0 && l.ch != '\n' {
		l.readRune()
	}
	if l.ch == '\n' {
		l.readRune()
	}

	for {
		for l.ch == ' ' || l.ch == '\t' || l.ch == '\r' {
			l.readRune()
		}
		if l.ch == 0 {
			return "unterminated block comment"
		}
		if l.atLineLeadingWhitespace() && l.blockCommentMarkerAtCurrent("=end") {
			for l.ch != 0 && l.ch != '\n' {
				l.readRune()
			}
			return ""
		}
		for l.ch != 0 && l.ch != '\n' {
			l.readRune()
		}
		if l.ch == '\n' {
			l.readRune()
		}
	}
}

func (l *lexer) blockCommentMarkerAtCurrent(marker string) bool {
	start := l.currentOffset()
	if !strings.HasPrefix(l.input[start:], marker) {
		return false
	}
	next := start + len(marker)
	if next >= len(l.input) {
		return true
	}
	r, _ := utf8.DecodeRuneInString(l.input[next:])
	switch r {
	case 0, ' ', '\t', '\r', '\n':
		return true
	default:
		return false
	}
}

func (l *lexer) atLineLeadingWhitespace() bool {
	idx := l.currentOffset()
	for idx > 0 {
		r, w := utf8.DecodeLastRuneInString(l.input[:idx])
		if r == '\n' {
			return true
		}
		if r != ' ' && r != '\t' && r != '\r' {
			return false
		}
		idx -= w
	}
	return true
}

func (l *lexer) readIdentifier() string {
	start := l.currentOffset()
	for ast.IsIdentifierRune(l.peekRune()) {
		l.readRune()
	}
	literal := l.input[start:l.offset]
	l.readRune()
	return literal
}

// numberToken is the lexer's classification of a scanned numeric literal.
// On success errMsg is empty and literal carries the underscore-stripped
// digits (prefix included for based literals); on failure errMsg holds a
// human-readable diagnostic and the literal is undefined.
type numberToken struct {
	literal string
	isFloat bool
	errMsg  string
}

const invalidNumericLiteral = "invalid numeric literal"

// readNumber scans a numeric literal beginning at the current rune. It
// recognizes Ruby-style base prefixes (0x/0X, 0b/0B, 0o/0O, 0d/0D) in
// addition to decimal integers and floats. Underscores are accepted as
// visual separators only between adjacent digits and are stripped from the
// returned literal. A prefix must be followed by at least one valid digit
// and the literal must not be immediately followed by an identifier rune;
// either violation yields an invalid-numeric-literal diagnostic so the
// caller can emit a precise parse error instead of leaving a stray
// identifier behind.
func (l *lexer) readNumber() numberToken {
	if l.ch == '0' {
		if prefix, base, ok := basePrefix(l.peekRune()); ok {
			return l.readPrefixedNumber(prefix, base)
		}
	}
	return l.readDecimalNumber()
}

// readDecimalNumber lexes a decimal integer or float beginning at the
// current rune. It returns the normalized literal (with visual-separator
// underscores stripped), whether the literal is a float, and a non-empty
// diagnostic when the literal is malformed.
//
// A literal is a float when it carries a decimal point or an exponent
// suffix. Exponent notation mirrors Ruby: an optional sign follows the
// e/E marker and at least one exponent digit is required, with underscores
// permitted only between digits. Malformed exponents such as 1e, 1e+, or
// 1e_3 yield a diagnostic instead of silently splitting into an integer
// followed by an identifier.
func (l *lexer) readDecimalNumber() numberToken {
	var sb strings.Builder
	var errMsg string
	hasDot := false
	hasExponent := false

	// current rune is part of the number
	sb.WriteRune(l.ch)

	for {
		r := l.peekRune()
		switch {
		case r == '_':
			// Allow underscores as visual separators; ignore them in the literal.
			// Only consume if surrounded by digits.
			beforeDigit := unicode.IsDigit(l.ch)
			afterDigit := unicode.IsDigit(l.peekRuneN(1))
			if beforeDigit && afterDigit {
				l.readRune()
				continue
			}
			goto done
		case r == '.' && !hasDot && !hasExponent && unicode.IsDigit(l.peekRuneN(1)):
			hasDot = true
			l.readRune()
			sb.WriteRune('.')
		case (r == 'e' || r == 'E') && !hasExponent && l.exponentMarkerAhead():
			if msg := l.readExponent(&sb); msg != "" {
				errMsg = msg
				goto done
			}
			hasExponent = true
		case unicode.IsDigit(r):
			l.readRune()
			sb.WriteRune(r)
		default:
			goto done
		}
	}

done:
	if errMsg == "" {
		if msg := l.rejectNumberSuffix(); msg != "" {
			errMsg = msg
		}
	}
	literal := sb.String()
	l.readRune()
	return numberToken{literal: literal, isFloat: hasDot || hasExponent, errMsg: errMsg}
}

// rejectNumberSuffix guards the boundary just past a numeric literal. A number
// that directly abuts an identifier (no intervening whitespace or operator),
// such as 1e3foo, 123abc, or 1.5x, is malformed: Ruby reports a syntax error
// rather than splitting it into a number followed by an identifier. A keyword
// suffix is left intact because Ruby permits adjacency there (5if cond and
// 1e3if cond lex as the number followed by a modifier keyword). When the suffix
// is a plain identifier it is consumed so the whole offending run becomes a
// single diagnostic token instead of fragmenting into a stray identifier.
//
// It must be called at the done boundary while l.ch still holds the literal's
// final rune, so l.peekRune reports the first rune after the number.
func (l *lexer) rejectNumberSuffix() string {
	if !ast.IsIdentifierStart(l.peekRune()) {
		return ""
	}
	start := l.offset
	end := start
	for end < len(l.input) {
		r, w := utf8.DecodeRuneInString(l.input[end:])
		if !ast.IsIdentifierRune(r) {
			break
		}
		end += w
	}
	if ast.LookupIdent(l.input[start:end]) != ast.TokenIdent {
		return ""
	}
	for l.offset < end {
		l.readRune()
	}
	return "malformed numeric literal: identifier cannot immediately follow a number"
}

// exponentMarkerAhead reports whether the e/E rune at l.peekRune actually
// opens an exponent suffix rather than abutting an identifier. Mirroring Ruby,
// the marker begins an exponent when immediately followed by a digit or by a
// sign (+/-). A sign commits to the exponent even without a following digit, so
// 1e+ is reported as a malformed exponent. Otherwise the e/E belongs to a
// trailing identifier (5end keeps the end keyword while 5elf and 1e_3 fall to
// the numeric suffix guard) rather than being mis-lexed as a malformed exponent.
//
// The marker must be the lexer's current peek rune, so peekRuneN(1) is the rune
// immediately after it.
func (l *lexer) exponentMarkerAhead() bool {
	next := l.peekRuneN(1)
	return unicode.IsDigit(next) || next == '+' || next == '-'
}

// readExponent consumes an exponent suffix beginning at the e/E marker,
// which must be the lexer's current peek rune. It appends the consumed
// runes (minus visual-separator underscores) to sb and returns a
// diagnostic when the suffix is malformed. A malformed suffix either lacks
// any exponent digit (1e+, where the sign commits to an exponent) or carries
// an underscore that is not wedged between two digits (1e3_, 1e3__4); in both
// cases the marker, sign, and any stray runes are consumed to keep the span
// over the offending text.
func (l *lexer) readExponent(sb *strings.Builder) string {
	marker := l.peekRune()
	l.readRune()
	sb.WriteRune(marker)

	if sign := l.peekRune(); sign == '+' || sign == '-' {
		l.readRune()
		sb.WriteRune(sign)
	}

	if !unicode.IsDigit(l.peekRune()) {
		// The suffix opens with a non-digit (1e_3, 1e+_3). Consume the rest of
		// the malformed tail so the whole offending sequence becomes one illegal
		// token instead of leaving a stray identifier for the parser to choke
		// on, which would cascade into unrelated diagnostics in delimited
		// contexts such as [1e_3].
		l.consumeExponentTail()
		return "malformed exponent in numeric literal: expected digits after '" + string(marker) + "'"
	}

	for {
		switch r := l.peekRune(); {
		case r == '_':
			// Underscores are visual separators only between two digits. A
			// trailing or doubled underscore (1e3_, 1e3__4) is malformed, so
			// consume the rest of the offending tail and report rather than
			// letting the parser lex the dangling underscore as a separate
			// identifier.
			if unicode.IsDigit(l.ch) && unicode.IsDigit(l.peekRuneN(1)) {
				l.readRune()
				continue
			}
			l.readRune()
			l.consumeExponentTail()
			return "malformed exponent in numeric literal: underscore must sit between exponent digits"
		case unicode.IsDigit(r):
			l.readRune()
			sb.WriteRune(r)
		default:
			return ""
		}
	}
}

// consumeExponentTail advances past the run of identifier runes (letters,
// digits, and underscores) that follows a malformed exponent marker. It keeps
// the diagnostic token's span over the entire offending sequence so a malformed
// exponent never fragments into a separate identifier token, mirroring Ruby's
// single "trailing sign/underscore" error for inputs such as 1e+foo or 5e+end.
func (l *lexer) consumeExponentTail() {
	for ast.IsIdentifierRune(l.peekRune()) {
		l.readRune()
	}
}

// readPrefixedNumber scans a based literal whose leading '0' is the current
// rune and whose base marker (x/b/o/d) is the next rune. base reports the
// numeric radix and prefix carries the marker rune for the returned literal.
func (l *lexer) readPrefixedNumber(prefix rune, base int) numberToken {
	var sb strings.Builder
	sb.WriteByte('0')
	sb.WriteRune(prefix)

	// Consume the '0' and the prefix marker so the current rune sits on the
	// first body character of the literal.
	l.readRune()

	digits := 0
	for {
		r := l.peekRune()
		switch {
		case r == '_':
			// Underscores are valid only between two body digits.
			if isBaseDigit(l.peekRuneN(1), base) && digits > 0 {
				l.readRune()
				continue
			}
			return l.invalidPrefixedNumber()
		case isBaseDigit(r, base):
			l.readRune()
			sb.WriteRune(r)
			digits++
		default:
			goto done
		}
	}

done:
	if digits == 0 {
		return l.invalidPrefixedNumber()
	}
	// A based literal followed directly by a name rune (an out-of-range digit, a
	// stray letter, or a leading-underscore name) is never valid; the fractional
	// dot is likewise rejected since based literals are integers. The '?' and '!'
	// suffixes are excluded: they are operators (e.g. the ternary '?') that
	// terminate the literal rather than glue onto it, matching how the decimal
	// path leaves "1?2:3" as an integer followed by the ternary.
	next := l.peekRune()
	if isNumericTrailRune(next) || (next == '.' && isBaseDigit(l.peekRuneN(1), 10)) {
		return l.invalidPrefixedNumber()
	}
	literal := sb.String()
	l.readRune()
	return numberToken{literal: literal}
}

// invalidPrefixedNumber consumes the remaining identifier and fractional runes
// of a malformed based literal so the lexer resumes scanning past it, then
// reports the invalid-numeric-literal diagnostic.
func (l *lexer) invalidPrefixedNumber() numberToken {
	for isNumericTrailRune(l.peekRune()) {
		l.readRune()
	}
	if l.peekRune() == '.' && unicode.IsDigit(l.peekRuneN(1)) {
		l.readRune()
		for isNumericTrailRune(l.peekRune()) {
			l.readRune()
		}
	}
	l.readRune()
	return numberToken{errMsg: invalidNumericLiteral}
}

// isNumericTrailRune reports whether r, appearing immediately after a numeric
// literal, indicates a malformed literal (a digit, letter, or underscore glued
// onto the digits) rather than a following operator. Unlike
// ast.IsIdentifierRune it excludes the '?' and '!' method-name suffixes, since
// those are operator runes (the ternary '?', logical negation '!') that
// terminate the literal instead of extending it.
func isNumericTrailRune(r rune) bool {
	return unicode.IsLetter(r) || unicode.IsDigit(r) || r == '_'
}

// basePrefix maps a base-marker rune to its prefix rune and radix.
func basePrefix(r rune) (prefix rune, base int, ok bool) {
	switch r {
	case 'x', 'X':
		return r, 16, true
	case 'b', 'B':
		return r, 2, true
	case 'o', 'O':
		return r, 8, true
	case 'd', 'D':
		return r, 10, true
	default:
		return 0, 0, false
	}
}

// isBaseDigit reports whether r is a valid digit in the given radix.
func isBaseDigit(r rune, base int) bool {
	var v int
	switch {
	case r >= '0' && r <= '9':
		v = int(r - '0')
	case r >= 'a' && r <= 'f':
		v = int(r-'a') + 10
	case r >= 'A' && r <= 'F':
		v = int(r-'A') + 10
	default:
		return false
	}
	return v < base
}

// readQuotedASCII consumes ordinary text before the next string delimiter or
// non-ASCII rune. Leaving that boundary unread keeps escapes, interpolation,
// invalid UTF-8, and line changes on the rune scanner's existing paths.
func (l *lexer) readQuotedASCII(quote byte) string {
	start := l.offset
	end := start
	for end < len(l.input) {
		ch := l.input[end]
		if ch == quote || ch == '\\' || ch == 0 || ch == '\n' || ch >= utf8.RuneSelf || (quote == '"' && ch == '#') {
			break
		}
		end++
	}
	if end == start {
		return ""
	}
	l.prevLine = l.line
	l.prevColumn = l.column + end - start - 1
	l.column += end - start
	l.offset = end
	l.width = 1
	l.ch = rune(l.input[end-1])
	return l.input[start:end]
}

func (l *lexer) readDoubleQuotedString() (string, bool, string) {
	var decoded strings.Builder
	interpolated := false
	// An interpolated string's raw text is the source between the quotes
	// verbatim, so it is taken as a slice of the input at the closing quote
	// rather than rebuilt rune by rune. Rebuilding it made an interpolation
	// nested inside another one copy its whole body again at every level that
	// encloses it (#46).
	bodyStart := l.offset

	for {
		l.readRune()
		switch l.ch {
		case 0:
			return "", false, "unterminated string"
		case '"':
			body := l.input[bodyStart:l.currentOffset()]
			l.readRune()
			if interpolated {
				return body, true, ""
			}
			return decoded.String(), false, ""
		case '\\':
			next := l.peekRune()
			if next == 0 {
				return "", false, "unterminated string"
			}
			switch next {
			case '"', '\\':
				l.readRune()
				decoded.WriteRune(next)
			case 'a':
				l.readRune()
				decoded.WriteByte('\a')
			case 'b':
				l.readRune()
				decoded.WriteByte('\b')
			case 'e':
				l.readRune()
				decoded.WriteByte(0x1b)
			case 'f':
				l.readRune()
				decoded.WriteByte('\f')
			case 'n':
				l.readRune()
				decoded.WriteByte('\n')
			case 'r':
				l.readRune()
				decoded.WriteByte('\r')
			case 't':
				l.readRune()
				decoded.WriteByte('\t')
			case 'v':
				l.readRune()
				decoded.WriteByte('\v')
			case 'x':
				escaped, errMsg := l.readVariableHexEscape(1, 2)
				if errMsg != "" {
					return "", false, errMsg
				}
				decoded.WriteByte(escaped.byte)
			case 'u':
				escaped, errMsg := l.readFixedHexEscape(4)
				if errMsg != "" {
					return "", false, errMsg
				}
				decoded.WriteRune(escaped.rune)
			default:
				l.readRune()
				decoded.WriteRune(next)
			}
		case '#':
			decoded.WriteRune(l.ch)
			if l.peekRune() == '{' {
				l.readRune()
				interpolated = true
				if !l.skipInterpolationBody() {
					if l.nestingRefused {
						return "", false, interpolationTooDeepMessage
					}
					return "", false, "unterminated string"
				}
			}
		default:
			if l.ch < utf8.RuneSelf && l.ch != '\n' {
				start := l.currentOffset()
				l.readQuotedASCII('"')
				decoded.WriteString(l.input[start:l.offset])
			} else {
				decoded.WriteRune(l.ch)
			}
		}
	}
}

type decodedEscape struct {
	raw  string
	rune rune
	byte byte
}

func (l *lexer) readFixedHexEscape(digits int) (decodedEscape, string) {
	raw := strings.Builder{}
	raw.WriteRune('\\')
	l.readRune()
	raw.WriteRune(l.ch)

	value := rune(0)
	for range digits {
		next := l.peekRune()
		if !isBaseDigit(next, 16) {
			return decodedEscape{}, fmt.Sprintf("invalid %d-digit hex escape in string", digits)
		}
		l.readRune()
		raw.WriteRune(l.ch)
		value = value*16 + hexRuneValue(l.ch)
	}
	if value > utf8.MaxRune || (value >= 0xd800 && value <= 0xdfff) {
		return decodedEscape{}, "invalid Unicode escape in string"
	}
	return decodedEscape{raw: raw.String(), rune: value}, ""
}

func (l *lexer) readVariableHexEscape(minDigits, maxDigits int) (decodedEscape, string) {
	raw := strings.Builder{}
	raw.WriteRune('\\')
	l.readRune()
	raw.WriteRune(l.ch)

	value := rune(0)
	digits := 0
	for digits < maxDigits {
		next := l.peekRune()
		if !isBaseDigit(next, 16) {
			break
		}
		l.readRune()
		raw.WriteRune(l.ch)
		value = value*16 + hexRuneValue(l.ch)
		digits++
	}
	if digits < minDigits {
		return decodedEscape{}, "invalid hex escape in string"
	}
	if value > utf8.MaxRune || (value >= 0xd800 && value <= 0xdfff) {
		return decodedEscape{}, "invalid Unicode escape in string"
	}
	return decodedEscape{rune: value, byte: byte(value)}, ""
}

func hexRuneValue(r rune) rune {
	switch {
	case r >= '0' && r <= '9':
		return r - '0'
	case r >= 'a' && r <= 'f':
		return r - 'a' + 10
	case r >= 'A' && r <= 'F':
		return r - 'A' + 10
	default:
		return 0
	}
}

// maxInterpolationDepth bounds how deeply "#{...}" string interpolations may
// nest. Every level is read twice over: the lexer drives a throwaway lexer
// across the whole body to find the "}" that closes it, and the parser then
// reparses that body from scratch, so the levels inside a given one are walked
// again for each level outside it. A 1 MB source spent 113ms at one level,
// 418ms at eight and 10.5s at 64, and 20,000 levels of a 100 KB source took
// 23s and over a GB of live memory during compile, where no step or memory
// quota reaches it (#46).
//
// Nesting a string inside its own interpolation is already unusual at two
// levels and unheard of past three, so eight leaves real code untouched while
// keeping the worst case a small multiple of what the same source costs
// without the nesting. It is deliberately below maxTypeDepth: those bounds
// guard recursion that costs a stack frame per level, while this one costs
// another pass over the source.
const maxInterpolationDepth = 8

const interpolationTooDeepMessage = "string interpolation nesting too deep"

// skipInterpolationBody advances the lexer over an in-progress "#{...}"
// interpolation body, stopping on the matching "}". It must be called with the
// opening "{" already consumed, so l.ch holds that "{" and l.offset points at
// the first rune of the body. The matching close brace is located with
// findStringInterpolationEnd, which drives the lexer over the body so nested
// double- and single-quoted strings, further interpolations, and percent-array
// literals (such as %W[#{%w[}]}]) balance correctly instead of guessing where
// the span ends. The runes are then stepped over one at a time to keep the
// lexer's line and column tracking accurate across the (possibly multiline)
// span.
//
// It reports whether the interpolation closed before the end of input, and
// records on the lexer when what stopped it was maxInterpolationDepth.
func (l *lexer) skipInterpolationBody() bool {
	l.nestingRefused = false
	if l.interpDepth >= maxInterpolationDepth {
		l.nestingRefused = true
		return false
	}
	end, ok, tooDeep := findStringInterpolationEnd(l.input, l.offset, l.percentScan, l.interpDepth+1)
	if !ok {
		l.nestingRefused = tooDeep
		return false
	}
	for l.offset <= end {
		l.readRune()
		if l.ch == 0 {
			return false
		}
	}
	return true
}

func (l *lexer) readSingleQuotedString() (string, string) {
	var sb strings.Builder

	for {
		l.readRune()
		switch l.ch {
		case 0:
			return "", "unterminated string"
		case '\'':
			l.readRune()
			return sb.String(), ""
		case '\\':
			next := l.peekRune()
			switch next {
			case '\'', '\\':
				l.readRune()
				sb.WriteRune(next)
			default:
				sb.WriteRune(l.ch)
			}
		default:
			if l.ch < utf8.RuneSelf && l.ch != '\n' {
				start := l.currentOffset()
				l.readQuotedASCII('\'')
				sb.WriteString(l.input[start:l.offset])
			} else {
				sb.WriteRune(l.ch)
			}
		}
	}
}

// scanQuotedSymbol scans a quoted symbol literal such as :"foo-bar" or
// :'foo bar', producing a TokenSymbol whose literal is the decoded name. It is
// called with l.ch on the leading colon and the next rune being the opening
// quote, and tok already carries the colon's position. The quoted body reuses
// the string scanners, so single-quoted symbols take no escapes beyond \\ and
// \', and double-quoted symbols decode the same \n, \t, \", and \\ escapes that
// string literals do. Interpolation inside a double-quoted symbol is rejected:
// dynamic symbols are out of scope, and accepting the raw #{...} text as a
// literal name would silently produce the wrong symbol. An empty quoted symbol
// (:"") is a valid symbol whose name is the empty string, mirroring Ruby.
func (l *lexer) scanQuotedSymbol(tok ast.Token) ast.Token {
	l.readRune()
	switch l.ch {
	case '"':
		literal, interpolated, errMsg := l.readDoubleQuotedString()
		switch {
		case errMsg != "":
			setDiagnostic(&tok, errMsg)
		case interpolated:
			setDiagnostic(&tok, "interpolation is not allowed in a symbol literal")
		default:
			tok.Type = ast.TokenSymbol
			tok.Literal = literal
		}
	case '\'':
		literal, errMsg := l.readSingleQuotedString()
		if errMsg != "" {
			setDiagnostic(&tok, errMsg)
		} else {
			tok.Type = ast.TokenSymbol
			tok.Literal = literal
		}
	}
	return tok
}

// readPercentArrayLiteral consumes a %w/%i/%W/%I percent-array literal and
// returns its entries. When interpolating is true (the uppercase %W/%I forms)
// the entries are split on interpolation-aware whitespace and returned with
// their #{...} markers and escape sequences intact for the parser to expand;
// otherwise the lowercase splitting that strips %w-style escapes is applied.
//
// For the interpolating forms the delimiter scan skips over #{...} spans using
// the same string-aware logic the parser applies (findStringInterpolationEnd),
// so a delimiter that appears inside an interpolation expression—including one
// nested in a quoted string such as %W[#{"]"}]—does not close the literal
// early.
func (l *lexer) readPercentArrayLiteral(interpolating bool) ([]string, string) {
	l.readRune()
	l.readRune()
	open := l.ch
	close, paired := percentLiteralClose(open)
	if close == 0 {
		return nil, "invalid percent array delimiter"
	}

	depth := 1
	var raw strings.Builder

	// closed reports whether the current rune is the closing delimiter that
	// balances the literal. When it returns true the literal is finished and
	// the consumed runes have already been split into entries.
	closed := func() (entries []string, done bool) {
		if l.ch != close {
			return nil, false
		}
		depth--
		if depth != 0 {
			return nil, false
		}
		l.readRune()
		if interpolating {
			return splitInterpolatedPercentLiteralWords(raw.String(), l.percentScan, l.interpDepth), true
		}
		return splitPercentLiteralWords(raw.String(), open, close), true
	}

	for {
		l.readRune()
		switch l.ch {
		case 0:
			return nil, "unterminated percent array literal"
		case '\\':
			raw.WriteRune(l.ch)
			if l.peekRune() != 0 {
				l.readRune()
				raw.WriteRune(l.ch)
			}
		case '#':
			// A '#' chosen as the delimiter still closes the literal, so only
			// treat "#{" as interpolation when '#' is not the closing rune.
			// This mirrors Ruby, where %W#a #{b}# closes at the first '#'
			// instead of interpolating.
			if interpolating && close != '#' && l.peekRune() == '{' {
				raw.WriteRune(l.ch)
				if msg := l.consumePercentArrayInterpolation(&raw); msg != "" {
					return nil, msg
				}
				continue
			}
			if entries, done := closed(); done {
				return entries, ""
			}
			raw.WriteRune(l.ch)
		default:
			if paired && l.ch == open {
				depth++
			}
			if entries, done := closed(); done {
				return entries, ""
			}
			raw.WriteRune(l.ch)
		}
	}
}

// consumePercentArrayInterpolation copies a #{...} interpolation span inside an
// interpolating percent array literal into raw verbatim. The caller has already
// written the leading '#' and confirmed the next rune is '{'. It returns an
// error message when the interpolation is unterminated.
func (l *lexer) consumePercentArrayInterpolation(raw *strings.Builder) string {
	l.readRune() // consume '{'
	start := l.currentOffset()
	if !l.skipInterpolationBody() {
		if l.nestingRefused {
			return interpolationTooDeepMessage
		}
		return "unterminated string interpolation in percent array literal"
	}
	// The skip stops on the closing "}", which is one byte wide, so the span
	// it stepped over ends one byte past where the lexer now sits.
	raw.WriteString(l.input[start : l.currentOffset()+1])
	return ""
}

func isPercentLiteralDelimiter(r rune) bool {
	return r != 0 && !unicode.IsSpace(r) && !unicode.IsLetter(r) && !unicode.IsDigit(r) && r != '_'
}

// canStartRegexLiteral reports whether a `/` under l.ch begins a Ruby-style
// regex literal rather than division. The rule mirrors Ruby's lexer state: a
// slash in prefix position (nothing before it on the line that could end an
// expression) starts a regex, while a slash after a value token is the
// division operator. `10 / 2` and `total /= n` therefore stay arithmetic,
// while `foo(/re/)`, `x =~ /re/`, and a statement-leading `/re/` lex as regex
// literals. Like Ruby, a slash at line-leading whitespace is always a regex,
// so continuing a division onto a new line requires the `/` to stay on the
// dividend's line.
//
// Ruby's command-argument form `method /re/` (a bare method name followed by a
// space, a slash, and no trailing space) is intentionally not handled here: the
// lexer cannot tell a method name from a local variable, so treating that
// spacing as a regex would misread ordinary division such as `total /2` or
// `sum /n`. The parser, which tracks locals, makes that call instead: when a
// non-local callee is followed by a slash with command-argument spacing it
// repositions the lexer at the slash (see parseRegexCommandArgument) so the
// prefix-position rule above scans the literal.
func (l *lexer) canStartRegexLiteral() bool {
	// A slash directly after `def` is the division operator-method name
	// (def /(other)), never a regex literal, so it must lex as TokenSlash for
	// the operator-name parser.
	if l.lastToken.Type == ast.TokenDef {
		return false
	}
	if l.atLineLeadingWhitespace() {
		return true
	}
	return !canEndExpressionToken(l.lastToken.Type)
}

// readRegexLiteral scans a `/pattern/flags` literal with l.ch on the opening
// slash and returns the raw literal text including both delimiters and any
// trailing flag letters. A backslash escapes the next rune (so `/a\/b/` keeps
// its escaped slash) and an unescaped `/` inside a `[...]` character class
// stays part of the pattern. As in RE2/Ruby, a `]` in the leading class
// position (immediately after `[` or `[^`) is a literal member rather than the
// class close, so `/[]/]/` matches `]` or `/` rather than truncating at the
// first `]`, and a nested POSIX class such as `[:alpha:]` is scanned through its
// `:]` terminator so its `]` is not read as the outer close (`/[[:alpha:]/]/`).
// The pattern body must close on the same line: a newline or end of
// input before the closing slash reports an unterminated literal, which keeps a
// stray prefix slash from silently swallowing the rest of the source. Flag
// validity is the parser's concern; the lexer accepts any trailing ASCII
// letters so the parser can report unknown flags precisely.
func (l *lexer) readRegexLiteral() (string, string) {
	var sb strings.Builder
	sb.WriteRune(l.ch)
	inClass := false
	inPosix := false // inside a [:name:] POSIX class nested in the outer class
	classPos := 0    // members consumed in the outer class; a ] at 0 is literal
	for {
		l.readRune()
		switch {
		case l.ch == 0 || l.ch == '\n':
			return "", "unterminated regex literal"
		case l.ch == '\\':
			next := l.peekRune()
			if next == 0 || next == '\n' {
				return "", "unterminated regex literal"
			}
			sb.WriteRune(l.ch)
			l.readRune()
			sb.WriteRune(l.ch)
			if inClass {
				classPos++
			}
		case l.ch == '[' && !inClass:
			inClass = true
			classPos = 0
			sb.WriteRune(l.ch)
			// A `^` right after `[` negates the class; the first `]` still counts
			// as a literal member, so consume the caret without advancing classPos.
			if l.peekRune() == '^' {
				l.readRune()
				sb.WriteRune(l.ch)
			}
		case l.ch == '[' && inClass && !inPosix && l.posixClassAhead():
			// A POSIX class such as [:alpha:] nests inside the outer class; scan
			// through its :] terminator so the ] is not read as the outer close.
			// A bare [: that is not a POSIX shape (as in /[[:/]/) stays literal.
			inPosix = true
			sb.WriteRune(l.ch)
		case l.ch == ':' && inPosix && l.peekRune() == ']':
			sb.WriteRune(l.ch)
			l.readRune()
			sb.WriteRune(l.ch)
			inPosix = false
			classPos++
		case l.ch == ']' && inClass && !inPosix && classPos == 0:
			sb.WriteRune(l.ch)
			classPos++
		case l.ch == ']' && inClass && !inPosix:
			inClass = false
			sb.WriteRune(l.ch)
		case l.ch == '/' && !inClass:
			sb.WriteRune(l.ch)
			l.readRune()
			for isRegexFlagRune(l.ch) {
				sb.WriteRune(l.ch)
				l.readRune()
			}
			return sb.String(), ""
		default:
			sb.WriteRune(l.ch)
			if inClass {
				classPos++
			}
		}
	}
}

func isRegexFlagRune(r rune) bool {
	return (r >= 'a' && r <= 'z') || (r >= 'A' && r <= 'Z')
}

// posixClassAhead reports whether the `[` under l.ch opens a POSIX class of the
// form [:name:] or [:^name:] (name being one or more ASCII letters). Only that
// shape may enter POSIX scan mode; a bare `[:` that is just literal members —
// such as the class in /[[:/]/ — must not, or the scanner would swallow the
// class close and delimiter hunting for a nonexistent `:]`.
func (l *lexer) posixClassAhead() bool {
	rest := l.input[l.currentOffset():]
	if len(rest) < 4 || rest[0] != '[' || rest[1] != ':' {
		return false
	}
	i := 2
	if rest[i] == '^' {
		i++
	}
	nameStart := i
	for i < len(rest) {
		c := rest[i]
		if c == ':' {
			return i > nameStart && i+1 < len(rest) && rest[i+1] == ']'
		}
		if !isASCIILetter(c) {
			return false
		}
		i++
	}
	return false
}

func isASCIILetter(c byte) bool {
	return (c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z')
}

func (l *lexer) canStartPercentArrayLiteral() bool {
	start := l.currentOffset()
	if start == 0 {
		return true
	}
	prev, _ := utf8.DecodeLastRuneInString(l.input[:start])
	if unicode.IsSpace(prev) {
		if l.atLineLeadingWhitespace() {
			return true
		}
		return !canEndExpressionToken(l.lastToken.Type)
	}
	return !canEndExpressionToken(l.lastToken.Type)
}

// closeBracket records that the bracket currently under l.ch closes a `(`, `[`,
// or `{`. It discards any pending ternary `?` recorded at a deeper nesting level
// than the level the closer returns to: a ternary whose `?` sits inside the
// bracket can never be completed by a `:` outside it, so such an entry is dead
// and would otherwise linger and mis-match a later colon. The depth is floored
// at zero so unbalanced input cannot drive it negative.
func (l *lexer) openBracket(tt ast.TokenType) {
	frame := bracketFrame{token: tt}
	if tt == ast.TokenLParen && canEndExpressionToken(l.lastToken.Type) && l.lastToken.End.Line == l.line {
		frame.callArguments = true
	}
	l.bracketDepth++
	l.bracketStack = l.bracketStack.push(frame)
}

func (l *lexer) closeBracket() {
	if l.bracketDepth > 0 {
		l.bracketDepth--
	}
	l.bracketStack = l.bracketStack.pop()
	for {
		top, ok := l.ternaryStack.top()
		if !ok || top.bracketDepth <= l.bracketDepth {
			return
		}
		l.ternaryStack = l.ternaryStack.pop()
	}
}

func (l *lexer) currentBracketType() ast.TokenType {
	frame, ok := l.currentBracketFrame()
	if !ok {
		return ast.TokenIllegal
	}
	return frame.token
}

func (l *lexer) currentBracketFrame() (bracketFrame, bool) {
	return l.bracketStack.top()
}

// colonClosesTernary reports whether the colon currently under l.ch is the
// separator of an open ternary expression rather than the start of a symbol or
// label. The ternary separator follows the consequent, so it sits in
// expression-end position while a ternary `?` is still pending. It closes the
// innermost pending ternary only when the colon sits at that ternary's bracket
// nesting level: a label `:` inside a hash, array, or paren group opened after
// the `?` sits one level deeper and must not be mistaken for the separator
// (flag ? {a: 1} :"no" keeps the inner `a:` a label and the outer `:` the
// separator). Resolving this from the pending-ternary stack and the previous
// token keeps both the same-line form (flag ? 1 :"no") and the line-leading
// multiline form (flag ?\n  1\n  :"no") parsing as separator + value, where
// Ruby's lexer would otherwise read the colon-quote as a symbol. The
// consequent's own leading symbol (flag ? :"a" : :"b") is in expression-start
// position, so it is not mistaken for the separator.
func (l *lexer) colonClosesTernary() bool {
	top, ok := l.ternaryStack.top()
	if !ok {
		return false
	}
	if top.bracketDepth != l.bracketDepth {
		return false
	}
	if l.labelColonBelongsToParenlessKeywordCall(top) && l.labelColonPrecedesTernarySeparator() {
		top.parenlessKeywordCall = true
		l.ternaryStack = l.ternaryStack.replaceTop(top)
		return false
	}
	return canEndExpressionToken(l.lastToken.Type)
}

func (l *lexer) labelColonBelongsToParenlessKeywordCall(frame ternaryFrame) bool {
	if !isLabelNameToken(l.lastToken) {
		return false
	}
	if frame.parenlessKeywordCall {
		return true
	}
	return l.labelFollowsParenlessCallee() || l.labelFollowsParenlessArgumentComma()
}

func (l *lexer) labelFollowsParenlessCallee() bool {
	if !canEndExpressionToken(l.prevToken.Type) {
		return false
	}
	if l.prevToken.End.Line != l.lastToken.Pos.Line {
		return false
	}
	return l.prevToken.End.Column < l.lastToken.Pos.Column
}

func (l *lexer) labelFollowsParenlessArgumentComma() bool {
	if l.prevToken.Type != ast.TokenComma || !canEndExpressionToken(l.prevPrevToken.Type) {
		return false
	}
	if l.prevPrevToken.End.Line != l.prevToken.Pos.Line || l.prevToken.End.Line != l.lastToken.Pos.Line {
		return false
	}
	return l.prevToken.End.Column < l.lastToken.Pos.Column
}

// ternaryScanCounting and ternaryScanBytes let a test count the input bytes the
// speculative label-colon scans walk, which is the work the complexity claim is
// about. Wall-clock would fold in scheduling and the race and coverage
// instrumentation this repository runs across three operating systems, and the
// clock is too coarse on Windows to compare runs this short. Never set outside
// tests; when off this costs one relaxed load per completed scan.
var (
	ternaryScanCounting atomic.Bool
	ternaryScanBytes    atomic.Uint64
)

func noteTernaryScan(walked int) {
	if ternaryScanCounting.Load() {
		ternaryScanBytes.Add(uint64(walked))
	}
}

// ternaryScanMemo records, per source, which label colons precede the separator
// of the ternary they sit inside. The scan that settles one reads only the
// source from that colon on, against the ternary the colon is being asked
// about, so recording the answer under both spares every later scan that walks
// over the same colon and keeps the reading it gives identical.
//
// One memo is shared by every lexer over the same input, including the
// throwaway ones the scans create, which is where the sharing pays: a scan and
// the scans it starts in turn all cover the same stretch of source. It is
// deliberately not rolled back by parser.restore, for the same reason the
// percent-scan allowance and the source replay are not: it describes the
// source, which a discarded speculation did not change.
type ternaryScanMemo struct {
	answers map[ternaryScanKey]bool
}

// ternaryScanKey names one question: the colon at offset, asked about the
// pending ternary that is innermost when the source has depth of them open. The
// depth belongs in the key because a colon reached with an inner ternary also
// pending is answering about that inner one instead.
type ternaryScanKey struct {
	offset int
	depth  int
}

func (m *ternaryScanMemo) record(key ternaryScanKey, answer bool) {
	if m.answers == nil {
		m.answers = make(map[ternaryScanKey]bool)
	}
	m.answers[key] = answer
}

// labelColonPrecedesTernarySeparator reports whether the pending ternary the
// colon under l.ch sits inside is closed by some later colon, which is what
// makes this colon a label rather than the separator itself.
//
// It answers from the shared record when that colon has been settled already,
// which is what keeps the scanning from compounding. The scan re-runs the whole
// colon decision, so without the record a scan launched at one label colon
// launched another at every later label colon, and each of those did the same
// over the rest of the source: one more label doubled the work. A single line
// of 22 spaced labels, 226 bytes in all, took 877ms to lex, and 26 labels took
// over a minute (#31, #32).
func (l *lexer) labelColonPrecedesTernarySeparator() bool {
	key := ternaryScanKey{offset: l.currentOffset(), depth: l.ternaryStack.len()}
	if answer, ok := l.ternaryScan.answers[key]; ok {
		return answer
	}
	answer := l.scanForTernarySeparator(key.depth)
	l.ternaryScan.record(key, answer)
	return answer
}

// scanForTernarySeparator lexes forward from the colon under l.ch on a copy of
// this lexer, reporting whether the pending ternary at outerDepth is closed
// before the statement ends.
//
// It deliberately re-runs the full colon decision rather than looking for the
// next colon that could close the ternary. Which colon that is depends on how
// the colons before it are read: a label colon leaves the ternary pending while
// a separator closes it, and a colon opening a quoted symbol swallows the value
// after it. Reading them any more cheaply than by lexing them answers a
// different question on some inputs.
func (l *lexer) scanForTernarySeparator(outerDepth int) bool {
	scan := *l
	start := scan.currentOffset()
	defer func() { noteTernaryScan(scan.currentOffset() - start) }()

	scan.readRune()
	for {
		beforeDepth := scan.ternaryStack.len()
		tok := scan.NextToken()
		switch tok.Type {
		case ast.TokenEOF, ast.TokenIllegal, ast.TokenSemicolon:
			return false
		}
		if beforeDepth >= outerDepth && scan.ternaryStack.len() < outerDepth {
			return true
		}
	}
}

// colonStartsSymbolLiteral reports whether a colon followed by a quote or
// operator should be lexed as a symbol literal rather than a hash or
// keyword-argument separator that happens to precede that value. It is consulted
// only after colonClosesTernary has ruled out the ternary separator.
func (l *lexer) colonStartsSymbolLiteral() bool {
	return !l.colonSeparatesSymbolValue()
}

func (l *lexer) colonSeparatesSymbolValue() bool {
	if isLabelNameToken(l.lastToken) {
		return l.colonAbutsPreviousToken() ||
			l.labelFollowsHashOrParenthesizedArgumentStart() ||
			l.labelFollowsParenlessCallee() ||
			l.labelFollowsParenlessArgumentComma()
	}
	if l.lastToken.Type == ast.TokenString {
		return l.stringKeyFollowsHashPairStart()
	}
	return false
}

func (l *lexer) labelFollowsHashOrParenthesizedArgumentStart() bool {
	frame, ok := l.currentBracketFrame()
	if !ok {
		return false
	}
	switch l.prevToken.Type {
	case ast.TokenLBrace:
		return frame.token == ast.TokenLBrace
	case ast.TokenLParen:
		return frame.token == ast.TokenLParen && frame.callArguments
	case ast.TokenComma:
		return frame.token == ast.TokenLBrace || (frame.token == ast.TokenLParen && frame.callArguments)
	default:
		return false
	}
}

func (l *lexer) stringKeyFollowsHashPairStart() bool {
	switch l.prevToken.Type {
	case ast.TokenLBrace, ast.TokenComma:
		return l.currentBracketType() == ast.TokenLBrace
	default:
		return false
	}
}

// colonAbutsPreviousToken reports whether the colon currently under l.ch
// immediately follows the previous token with no intervening whitespace, as in
// the no-space label form rescue:"x". A space before the colon (return :"x")
// makes it non-abutting.
func (l *lexer) colonAbutsPreviousToken() bool {
	start := l.currentOffset()
	if start == 0 {
		return false
	}
	prev, _ := utf8.DecodeLastRuneInString(l.input[:start])
	return !unicode.IsSpace(prev)
}

func canEndExpressionToken(tt ast.TokenType) bool {
	switch tt {
	case ast.TokenIdent, ast.TokenInt, ast.TokenFloat, ast.TokenString, ast.TokenInterpolatedString,
		ast.TokenSymbol, ast.TokenWords, ast.TokenSymbols, ast.TokenInterpWords, ast.TokenInterpSymbols,
		ast.TokenTrue, ast.TokenFalse, ast.TokenNil,
		ast.TokenSelf, ast.TokenIvar, ast.TokenClassVar, ast.TokenRParen, ast.TokenRBracket,
		ast.TokenRBrace, ast.TokenEnd, ast.TokenRegex:
		return true
	default:
		return false
	}
}

func percentLiteralClose(open rune) (rune, bool) {
	switch open {
	case '[':
		return ']', true
	case '(':
		return ')', true
	case '{':
		return '}', true
	case '<':
		return '>', true
	default:
		if !isPercentLiteralDelimiter(open) {
			return 0, false
		}
		return open, false
	}
}

const percentLiteralEntrySeparator = "\x00"

func encodePercentLiteralEntries(entries []string) string {
	return strings.Join(entries, percentLiteralEntrySeparator)
}

func decodePercentLiteralEntries(literal string) []string {
	if literal == "" {
		return nil
	}
	return strings.Split(literal, percentLiteralEntrySeparator)
}

func splitPercentLiteralWords(raw string, open, close rune) []string {
	var words []string
	var sb strings.Builder
	inWord := false
	escaped := false

	flush := func() {
		if !inWord {
			return
		}
		words = append(words, sb.String())
		sb.Reset()
		inWord = false
	}

	for _, r := range raw {
		if escaped {
			if isPercentWordEscapable(r, open, close) {
				sb.WriteRune(r)
			} else {
				sb.WriteRune('\\')
				sb.WriteRune(r)
			}
			inWord = true
			escaped = false
			continue
		}

		switch {
		case r == '\\':
			escaped = true
			inWord = true
		case unicode.IsSpace(r):
			flush()
		default:
			sb.WriteRune(r)
			inWord = true
		}
	}

	if escaped {
		sb.WriteRune('\\')
	}
	flush()
	return words
}

func isPercentWordEscapable(r, open, close rune) bool {
	return unicode.IsSpace(r) || r == '\\' || r == open || r == close
}

// splitInterpolatedPercentLiteralWords splits the interior of a %W/%I literal
// into words. Unlike the lowercase splitter it leaves escape sequences and
// #{...} interpolation markers intact so the parser can apply double-quoted
// string semantics per entry. Whitespace splits words unless it is escaped or
// appears inside an interpolation, matching Ruby's handling of `%W[a #{b c} d]`.
// Interpolation spans are scanned with the same string-aware logic the parser
// uses (findStringInterpolationEnd) so quotes and nested braces inside #{...}
// do not prematurely terminate a word.
func splitInterpolatedPercentLiteralWords(raw string, budget *percentScanBudget, interpDepth int) []string {
	var words []string
	var sb strings.Builder
	inWord := false

	flush := func() {
		if !inWord {
			return
		}
		words = append(words, sb.String())
		sb.Reset()
		inWord = false
	}

	for i := 0; i < len(raw); {
		switch {
		case raw[i] == '\\':
			sb.WriteByte(raw[i])
			i++
			if i < len(raw) {
				_, size := utf8.DecodeRuneInString(raw[i:])
				sb.WriteString(raw[i : i+size])
				i += size
			}
			inWord = true
		case raw[i] == '#' && i+1 < len(raw) && raw[i+1] == '{':
			end, ok, _ := findStringInterpolationEnd(raw, i+2, budget, interpDepth+1)
			if !ok {
				// Unterminated interpolation: copy the rest verbatim and let
				// the parser report the error against the full entry.
				sb.WriteString(raw[i:])
				i = len(raw)
			} else {
				sb.WriteString(raw[i : end+1])
				i = end + 1
			}
			inWord = true
		default:
			r, size := utf8.DecodeRuneInString(raw[i:])
			if unicode.IsSpace(r) {
				flush()
			} else {
				sb.WriteString(raw[i : i+size])
				inWord = true
			}
			i += size
		}
	}

	flush()
	return words
}

// ast.Identifier classification and keyword lookup are now provided by
// internal/ast (ast.IsIdentifierStart, ast.IsIdentifierRune, ast.LookupIdent).
