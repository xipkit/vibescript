package parser

import (
	"fmt"
	"strings"
	"testing"
	"unicode/utf8"
	"unsafe"

	"github.com/mgomes/vibescript/internal/ast"
)

func TestQuotedASCIISpanMatchesRuneState(t *testing.T) {
	t.Parallel()
	for _, quote := range []byte{'"', '\''} {
		for _, previous := range []string{"\"", "\n", "é", "🙂", "\\", "}"} {
			for _, size := range []int{0, 1, 7, 8, 15, 16, 17, 31, 32, 33, 63, 64, 65} {
				for boundary := range 256 {
					source := previous + strings.Repeat("a", size) + string([]byte{byte(boundary)}) + "tail"
					fast := newLexer(source)
					slow := *fast
					start := slow.offset
					for {
						next := slow.peekRune()
						if next == 0 || next == '\n' || next >= utf8.RuneSelf ||
							next == rune(quote) || next == '\\' || (quote == '"' && next == '#') {
							break
						}
						slow.readRune()
					}
					got := fast.readQuotedASCII(quote)
					want := source[start:slow.offset]
					if got != want || *fast != slow {
						t.Fatalf("readQuotedASCII(%q, %q) = %q, state %+v; want %q, state %+v",
							source, quote, got, *fast, want, slow)
					}
				}
			}
		}
	}
}

func TestQuotedStringSpansMatchScalarTokens(t *testing.T) {
	t.Parallel()
	for _, quote := range []byte{'"', '\''} {
		for _, symbol := range []bool{false, true} {
			for _, size := range []int{0, 1, 7, 8, 15, 16, 17, 31, 32, 33, 63, 64, 65} {
				for boundary := range 256 {
					literal := string(quote) + strings.Repeat("a", size) + string([]byte{byte(boundary)}) + "tail" + string(quote)
					if symbol {
						literal = ":" + literal
					}
					checkQuotedScalarTokens(t, literal)
				}
			}
		}
	}
}

func TestQuotedStringSpansMatchScalarErrorsAndInterpolation(t *testing.T) {
	t.Parallel()
	cases := []string{
		"\"\"", "''", "\"unterminated", "'unterminated", "\"trailing\\", "'trailing\\",
		"\"a\\\"b\"", "'a\\'b'", "\"a\\\\b\"", "'a\\\\b'", "'a\\nb'",
		"\"a\\ab\\bc\\ed\\fe\\nf\\rg\\th\\vi\"", "\"a\\x41\\u0042b\"",
		"\"a\\x\"", "\"a\\u123\"", "\"a\\uD800\"", "\"a\\uZZZZ\"", "\"a\\xFFb\"",
		"\"a\\#{1}b\"", "\"a\\\\#{1}b\"", "\"#alone##\"", "'#{1}'",
		"\"a#{1}b\"", "\"a#{\"inner\"}b\"", "\"a#{'inner'}b\"",
		"\"a#{\"nested #{1}\"}b\"", "\"a#{%W[x#{1}y]}b\"", "\"a#{%w[}]}b\"",
		"\"first\nsecond\"", "'first\nsecond'", "\"é🙂x\"", "'é🙂x'",
		"\"a\xffb\"", "'a\xc0\xafz'", "\"a\xe2\x82z\"", "\"a\x00b\"",
	}
	for _, source := range cases {
		checkQuotedScalarTokens(t, source)
		checkQuotedScalarTokens(t, ":"+source)
	}
	for _, size := range []int{16, 4096, 65536} {
		prefix := strings.Repeat("a", size)
		for _, ending := range []string{"\"", "\\x\"", "\\uD800\"", "#{1}\"", "#{%W[x#{1}y]}\"", "\xff\""} {
			checkQuotedScalarTokens(t, "\""+prefix+ending)
		}
	}
	for _, depth := range []int{maxInterpolationDepth, maxInterpolationDepth + 1} {
		checkQuotedScalarTokens(t, strings.TrimSuffix(nestedInterpolation(depth, "1"), "\n"))
	}
}

func checkQuotedScalarTokens(t *testing.T, literal string) {
	t.Helper()
	for _, prefix := range []string{"", "\n \t", "é\n\r\t"} {
		source := prefix + literal + " + following\n"
		fast, slow := newLexer(source), newLexer(source)
		for fast.currentOffset() < len(prefix) {
			fast.readRune()
			slow.readRune()
		}
		got := fast.NextToken()
		want := scalarQuotedToken(slow)
		for tokenIndex := range len(source) + 2 {
			if got != want {
				t.Fatalf("quoted tokens(%q)[%d] = %+v, want %+v", source, tokenIndex, got, want)
			}
			if gotState, wantState := quotedLexerState(fast), quotedLexerState(slow); gotState != wantState {
				t.Fatalf("quoted state(%q)[%d] = %+v, want %+v", source, tokenIndex, gotState, wantState)
			}
			if got.Type == ast.TokenEOF {
				break
			}
			got, want = fast.NextToken(), slow.NextToken()
		}
		if got.Type != ast.TokenEOF {
			t.Fatalf("quoted tokens(%q) did not reach EOF", source)
		}
	}
}

type stringLexerState struct {
	Offset, Width, Line, Column, PrevLine, PrevColumn int
	Character                                         rune
	PreviousPrevious, Previous, Last                  ast.Token
	BracketDepth, InterpolationDepth                  int
	NestingRefused                                    bool
	PercentRemaining                                  int
	PercentDeclinedAt                                 ast.Position
}

func quotedLexerState(l *lexer) stringLexerState {
	return stringLexerState{
		Offset: l.offset, Width: l.width, Line: l.line, Column: l.column,
		PrevLine: l.prevLine, PrevColumn: l.prevColumn, Character: l.ch,
		PreviousPrevious: l.prevPrevToken, Previous: l.prevToken, Last: l.lastToken,
		BracketDepth: l.bracketDepth, InterpolationDepth: l.interpDepth, NestingRefused: l.nestingRefused,
		PercentRemaining: l.percentScan.remaining, PercentDeclinedAt: l.percentScan.declinedAt,
	}
}

func scalarQuotedToken(l *lexer) ast.Token {
	tok := ast.Token{Type: ast.TokenString, Pos: ast.Position{Line: l.line, Column: l.column}}
	symbol := l.ch == ':'
	if symbol {
		l.readRune()
		tok.Type = ast.TokenSymbol
	}
	var literal, message string
	var interpolated bool
	if l.ch == '"' {
		literal, interpolated, message = l.readDoubleQuotedStringScalar()
	} else {
		literal, message = l.readSingleQuotedStringScalar()
	}
	if message != "" {
		setDiagnostic(&tok, message)
	} else if symbol && interpolated {
		setDiagnostic(&tok, "interpolation is not allowed in a symbol literal")
	} else {
		tok.Literal = literal
		if interpolated {
			tok.Type = ast.TokenInterpolatedString
		}
	}
	tok.End = ast.Position{Line: l.prevLine, Column: l.prevColumn + 1}
	l.prevPrevToken, l.prevToken, l.lastToken = l.prevToken, l.lastToken, tok
	return tok
}

func TestQuotedASCIISpanDoesNotAllocate(t *testing.T) {
	source := "\"" + strings.Repeat("a", 65536) + "\""
	template := newLexer(source)
	allocations := testing.AllocsPerRun(100, func() {
		lex := *template
		if span := lex.readQuotedASCII('"'); len(span) != 65536 {
			panic(fmt.Sprintf("readQuotedASCII() span length = %d, want 65536", len(span)))
		}
	})
	if allocations != 0 {
		t.Errorf("readQuotedASCII() allocated %g objects, want 0", allocations)
	}
}

func TestDecodedQuotedLiteralDoesNotAliasSource(t *testing.T) {
	t.Parallel()
	for _, literal := range []string{"\"tiny\"", "'tiny'", ":\"tiny\"", ":'tiny'"} {
		source := literal + strings.Repeat(" ", 1<<20)
		token := newLexer(source).NextToken()
		if token.Literal != "tiny" {
			t.Fatalf("NextToken(%q) = %q, want tiny", literal, token.Literal)
		}
		sourceStart := uintptr(unsafe.Pointer(unsafe.StringData(source)))
		decodedStart := uintptr(unsafe.Pointer(unsafe.StringData(token.Literal)))
		if decodedStart >= sourceStart && decodedStart-sourceStart < uintptr(len(source)) {
			t.Errorf("NextToken(%q) decoded literal aliases the %d-byte source", literal, len(source))
		}
	}
}

func FuzzQuotedStringSpansMatchScalar(f *testing.F) {
	for _, body := range []string{"", "ordinary ASCII", "é🙂", "a\nb", "a\\xFFb", "a\\uD800b", "a#{%W[x#{1}]}z", "\xff\x00"} {
		f.Add(body, byte('"'), false)
		f.Add(body, byte('\''), true)
	}
	f.Fuzz(func(t *testing.T, body string, selector byte, symbol bool) {
		if len(body) > 4096 {
			t.Skip()
		}
		quote := byte('"')
		if selector&1 != 0 {
			quote = '\''
		}
		literal := string(quote) + body + string(quote)
		if symbol {
			literal = ":" + literal
		}
		checkQuotedScalarTokens(t, literal)
	})
}

// These rune-at-a-time scanners preserve the implementation before span copying
// as an independent oracle for literal values, errors, and lexer positions.
func (l *lexer) readDoubleQuotedStringScalar() (string, bool, string) {
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
			decoded.WriteRune(l.ch)
		}
	}
}

func (l *lexer) readSingleQuotedStringScalar() (string, string) {
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
			sb.WriteRune(l.ch)
		}
	}
}
