package runtime

import (
	"fmt"
	"strings"
	"unicode/utf8"
)

// These scalar reference routines preserve the original JSON string behavior,
// including positions and accounting, independently of the span implementation.
type jsonSpanReferenceParser struct{ jsonValueParser }

func (p *jsonSpanReferenceParser) parseString() (string, error) {
	p.pos++
	start := p.pos
	for p.pos < len(p.raw) {
		b := p.raw[p.pos]
		switch {
		case b == '"':
			value := p.raw[start:p.pos]
			p.pos++
			if err := p.reserve(estimatedStringHeaderBytes + len(value)); err != nil {
				return "", err
			}
			// A retained value or key must not keep the source document alive.
			return strings.Clone(value), nil
		case b == '\\':
			return p.parseEscapedString(start)
		case b < 0x20:
			return "", fmt.Errorf("invalid character %q in string literal", b)
		case b < utf8.RuneSelf:
			p.pos++
		default:
			r, size := utf8.DecodeRuneInString(p.raw[p.pos:])
			if r == utf8.RuneError && size == 1 {
				return p.parseEscapedString(start)
			}
			p.pos += size
		}
	}
	return "", fmt.Errorf("unexpected end of JSON input")
}

func (p *jsonSpanReferenceParser) parseEscapedString(start int) (string, error) {
	position := p.pos
	size, err := p.parseEscapedContents(start, nil)
	if err != nil {
		return "", err
	}
	if p.exec != nil {
		if err := p.exec.chargeStringScan(p.pos - start + size); err != nil {
			return "", err
		}
	}
	var b strings.Builder
	capacity := projectedBuilderCap(&b, size)
	if err := p.checkExtra(estimatedStringHeaderBytes + capacity + size); err != nil {
		return "", err
	}
	b.Grow(size)
	p.pos = position
	if _, err := p.parseEscapedContents(start, &b); err != nil {
		return "", err
	}
	// Keep only this token's bytes, so discarded-subtree accounting can use
	// its string length and no result retains spare decoding capacity.
	out := strings.Clone(b.String())
	p.used += estimatedStringHeaderBytes + len(out)
	return out, nil
}

func (p *jsonSpanReferenceParser) parseEscapedContents(start int, b *strings.Builder) (int, error) {
	size := p.pos - start
	if b != nil {
		b.WriteString(p.raw[start:p.pos])
	}

	for p.pos < len(p.raw) {
		c := p.raw[p.pos]
		var r rune
		switch {
		case c == '"':
			p.pos++
			return size, nil
		case c == '\\':
			p.pos++
			decoded, err := p.parseStringEscape()
			if err != nil {
				return 0, err
			}
			r = decoded
		case c < 0x20:
			return 0, fmt.Errorf("invalid character %q in string literal", c)
		case c < utf8.RuneSelf:
			r = rune(c)
			p.pos++
		default:
			decoded, width := utf8.DecodeRuneInString(p.raw[p.pos:])
			r = decoded
			p.pos += width
		}
		size += utf8.RuneLen(r)
		if b != nil {
			b.WriteRune(r)
		}
	}
	return 0, fmt.Errorf("unexpected end of JSON input")
}

func jsonSpanReferenceAppendString(buf []byte, s string, state *jsonStringifyState) ([]byte, error) {
	const hexDigits = "0123456789abcdef"

	if err := state.checkOutputBytes(len(buf) + 1); err != nil {
		return nil, err
	}
	buf = append(buf, '"')
	start := 0
	for i := 0; i < len(s); {
		if b := s[i]; b < utf8.RuneSelf {
			if b >= 0x20 && b != '\\' && b != '"' && b != '<' && b != '>' && b != '&' {
				i++
				continue
			}

			if err := state.checkOutputBytes(len(buf) + i - start + 6); err != nil {
				return nil, err
			}
			buf = append(buf, s[start:i]...)
			switch b {
			case '\\', '"':
				buf = append(buf, '\\', b)
			case '\b':
				buf = append(buf, '\\', 'b')
			case '\f':
				buf = append(buf, '\\', 'f')
			case '\n':
				buf = append(buf, '\\', 'n')
			case '\r':
				buf = append(buf, '\\', 'r')
			case '\t':
				buf = append(buf, '\\', 't')
			default:
				buf = append(buf, '\\', 'u', '0', '0', hexDigits[b>>4], hexDigits[b&0x0f])
			}
			i++
			start = i
			continue
		}

		r, size := utf8.DecodeRuneInString(s[i:])
		if r == utf8.RuneError && size == 1 {
			if err := state.checkOutputBytes(len(buf) + i - start + len(`\ufffd`)); err != nil {
				return nil, err
			}
			buf = append(buf, s[start:i]...)
			buf = append(buf, `\ufffd`...)
			i++
			start = i
			continue
		}
		if r == '\u2028' || r == '\u2029' {
			if err := state.checkOutputBytes(len(buf) + i - start + 6); err != nil {
				return nil, err
			}
			buf = append(buf, s[start:i]...)
			buf = append(buf, '\\', 'u', '2', '0', '2', byte('8'+r-'\u2028'))
			i += size
			start = i
			continue
		}
		i += size
	}
	if err := state.checkOutputBytes(len(buf) + len(s) - start + 1); err != nil {
		return nil, err
	}
	buf = append(buf, s[start:]...)
	return append(buf, '"'), nil
}
