package runtime

import (
	"cmp"
	"errors"
	"fmt"
	"math"
	"math/big"
	"reflect"
	"slices"
	"strconv"
	"strings"
	"unicode/utf16"
	"unicode/utf8"

	"github.com/mgomes/vibescript/vibes/value"
)

type jsonStringifyState struct {
	seenArrayInline [jsonInlineSeenCapacity]uintptr
	seenHashInline  [jsonInlineSeenCapacity]uintptr
	seenArrays      map[uintptr]struct{}
	seenHashes      map[uintptr]struct{}
	seenArrayLen    int
	seenHashLen     int
	depth           int
	exec            *Execution
	// chargedSteps is the number of steps already billed for the output. It
	// counts steps rather than bytes because escaping calls checkOutputBytes
	// once per escaped character: a six-byte delta divides to zero steps, so
	// billing each delta separately charged nothing at all however long the
	// output grew.
	chargedSteps int
}

type jsonValueParser struct {
	raw   string
	pos   int
	depth int
	exec  *Execution
	args  []Value
	base  int
	used  int
}

type jsonSeenSlot struct {
	id    uintptr
	index int
	inMap bool
}

const (
	jsonInitialObjectCapacity = 4
	jsonInlineSeenCapacity    = 8
	// jsonBigIntStepDigits is the number of big-integer decimal digits one
	// sandbox step covers during JSON conversion, matching the rendering
	// projections' scaling.
	jsonBigIntStepDigits = 8
)

type jsonInvalidNumberError string

func (e jsonInvalidNumberError) Error() string {
	return fmt.Sprintf("JSON.parse invalid number %q", string(e))
}

var errJSONMaxDepth = &guardLimitError{err: errors.New("exceeded max depth")}

func (p *jsonValueParser) parse() (Value, error) {
	if p.exec != nil {
		if p.exec.memoryQuota > 0 {
			args := p.args
			if args == nil {
				args = []Value{NewString(p.raw)}
			}
			var nodes int
			p.base, nodes = p.exec.hashCallRootUsage(NewNil(), args, nil, NewNil())
			if err := p.exec.chargeEstimatorWalk(nodes); err != nil {
				return NewNil(), err
			}
		}
		defer p.exec.beginAccumulatorMeteredSection()()
	}
	if err := p.reserve(estimatedValueBytes); err != nil {
		return NewNil(), err
	}
	p.skipWhitespace()
	value, err := p.parseValue()
	if err != nil {
		return NewNil(), err
	}
	p.skipWhitespace()
	if p.pos != len(p.raw) {
		return NewNil(), fmt.Errorf("trailing data")
	}
	return value, nil
}

func (p *jsonValueParser) parseValue() (Value, error) {
	if p.exec != nil {
		if err := p.exec.step(); err != nil {
			return NewNil(), err
		}
	}
	if p.pos >= len(p.raw) {
		return NewNil(), fmt.Errorf("unexpected end of JSON input")
	}

	switch p.raw[p.pos] {
	case 'n':
		if p.consumeLiteral("null") {
			return NewNil(), nil
		}
	case 't':
		if p.consumeLiteral("true") {
			return NewBool(true), nil
		}
	case 'f':
		if p.consumeLiteral("false") {
			return NewBool(false), nil
		}
	case '"':
		s, err := p.parseString()
		if err != nil {
			return NewNil(), err
		}
		return NewString(s), nil
	case '[':
		return p.parseArray()
	case '{':
		return p.parseObject()
	case '-', '0', '1', '2', '3', '4', '5', '6', '7', '8', '9':
		return p.parseNumber()
	}

	return NewNil(), fmt.Errorf("invalid character %q looking for beginning of value", p.raw[p.pos])
}

func (p *jsonValueParser) parseArray() (Value, error) {
	if err := p.enterContainer(); err != nil {
		return NewNil(), err
	}
	defer p.leaveContainer()
	if err := p.reserve(nestedArrayBackingBytes(0)); err != nil {
		return NewNil(), err
	}

	p.pos++
	p.skipWhitespace()
	if p.consumeByte(']') {
		return NewArray(nil), nil
	}

	var values []Value
	for {
		parsed, err := p.parseValue()
		if err != nil {
			return NewNil(), err
		}
		if err := p.reserve(estimatedValueBytes); err != nil {
			return NewNil(), err
		}
		if len(values) == cap(values) {
			capacity := projectedAppendCap(len(values), cap(values))
			if err := p.checkExtra(capacity * estimatedValueBytes); err != nil {
				return NewNil(), err
			}
			grown := make([]Value, len(values), capacity)
			copy(grown, values)
			p.used += (capacity - cap(values)) * estimatedValueBytes
			values = grown
		}
		values = append(values, parsed)

		p.skipWhitespace()
		switch {
		case p.consumeByte(','):
			p.skipWhitespace()
			if p.pos < len(p.raw) && p.raw[p.pos] == ']' {
				return NewNil(), fmt.Errorf("invalid character ']' looking for beginning of value")
			}
		case p.consumeByte(']'):
			return NewArray(values), nil
		default:
			if p.pos >= len(p.raw) {
				return NewNil(), fmt.Errorf("unexpected end of JSON input")
			}
			return NewNil(), fmt.Errorf("invalid character %q after array element", p.raw[p.pos])
		}
	}
}

func (p *jsonValueParser) parseObject() (Value, error) {
	if err := p.enterContainer(); err != nil {
		return NewNil(), err
	}
	defer p.leaveContainer()

	p.pos++
	p.skipWhitespace()
	if p.consumeByte('}') {
		if err := p.reserve(estimatedMapBaseBytes + estimatedHashDataBytes); err != nil {
			return NewNil(), err
		}
		return NewHash(nil), nil
	}

	if err := p.reserve(estimatedMapBaseBytes + estimatedHashDataBytes + jsonInitialObjectCapacity*estimatedMapEntryStructuralBytes + hashOrderBackingBytes(jsonInitialObjectCapacity)); err != nil {
		return NewNil(), err
	}
	values := NewHashWithCapacity(jsonInitialObjectCapacity)
	for {
		if p.pos >= len(p.raw) {
			return NewNil(), fmt.Errorf("unexpected end of JSON input")
		}
		if p.raw[p.pos] != '"' {
			return NewNil(), fmt.Errorf("invalid character %q looking for beginning of object key string", p.raw[p.pos])
		}
		key, err := p.parseString()
		if err != nil {
			return NewNil(), err
		}

		p.skipWhitespace()
		if !p.consumeByte(':') {
			if p.pos >= len(p.raw) {
				return NewNil(), fmt.Errorf("unexpected end of JSON input")
			}
			return NewNil(), fmt.Errorf("invalid character %q after object key", p.raw[p.pos])
		}

		p.skipWhitespace()
		parsed, err := p.parseValue()
		if err != nil {
			return NewNil(), err
		}
		previous, exists := values.HashEntryMap()[key]
		if !exists {
			// The order keeps the first key string; a duplicate map write can
			// retain a second equal string. Reserve both representations.
			if err := p.reserve(len(key)); err != nil {
				return NewNil(), err
			}
			if values.HashLen() >= value.HashEntryCapacity(values) {
				if err := p.reserve(estimatedMapEntryStructuralBytes); err != nil {
					return NewNil(), err
				}
			}
			capacity := value.HashOrderCapacity(values)
			if values.HashLen() == capacity {
				next := projectedAppendCap(values.HashLen(), capacity)
				if err := p.checkExtra(hashOrderBackingBytes(next)); err != nil {
					return NewNil(), err
				}
				values.ReserveHashOrderUnpublished(next)
				p.used += hashOrderBackingBytes(next) - hashOrderBackingBytes(capacity)
			}
		}
		if err := values.HashSetUnpublished(NewString(key), parsed); err != nil {
			return NewNil(), err
		}
		if exists {
			freed, err := p.discardedPayload(previous)
			if err != nil {
				return NewNil(), err
			}
			p.used -= freed + estimatedStringHeaderBytes + len(key)
		}

		p.skipWhitespace()
		switch {
		case p.consumeByte(','):
			p.skipWhitespace()
			if p.pos < len(p.raw) && p.raw[p.pos] == '}' {
				return NewNil(), fmt.Errorf("invalid character '}' looking for beginning of object key string")
			}
		case p.consumeByte('}'):
			return values, nil
		default:
			if p.pos >= len(p.raw) {
				return NewNil(), fmt.Errorf("unexpected end of JSON input")
			}
			return NewNil(), fmt.Errorf("invalid character %q after object value", p.raw[p.pos])
		}
	}
}

func (p *jsonValueParser) parseNumber() (Value, error) {
	start := p.pos
	if p.consumeByte('-') && p.pos >= len(p.raw) {
		return NewNil(), fmt.Errorf("invalid number %q", p.raw[start:p.pos])
	}

	if p.consumeByte('0') {
		if p.pos < len(p.raw) && isJSONDigit(p.raw[p.pos]) {
			return NewNil(), fmt.Errorf("invalid number %q", p.raw[start:p.pos+1])
		}
	} else if p.pos < len(p.raw) && isJSONOneToNine(p.raw[p.pos]) {
		p.pos++
		for p.pos < len(p.raw) && isJSONDigit(p.raw[p.pos]) {
			p.pos++
		}
	} else {
		return NewNil(), fmt.Errorf("invalid number %q", p.raw[start:p.pos])
	}

	floatLike := false
	if p.consumeByte('.') {
		floatLike = true
		if p.pos >= len(p.raw) || !isJSONDigit(p.raw[p.pos]) {
			return NewNil(), fmt.Errorf("invalid number %q", p.raw[start:p.pos])
		}
		for p.pos < len(p.raw) && isJSONDigit(p.raw[p.pos]) {
			p.pos++
		}
	}

	if p.pos < len(p.raw) && (p.raw[p.pos] == 'e' || p.raw[p.pos] == 'E') {
		floatLike = true
		p.pos++
		if p.pos < len(p.raw) && (p.raw[p.pos] == '+' || p.raw[p.pos] == '-') {
			p.pos++
		}
		if p.pos >= len(p.raw) || !isJSONDigit(p.raw[p.pos]) {
			return NewNil(), fmt.Errorf("invalid number %q", p.raw[start:p.pos])
		}
		for p.pos < len(p.raw) && isJSONDigit(p.raw[p.pos]) {
			p.pos++
		}
	}

	literal := p.raw[start:p.pos]
	if !floatLike {
		if i, err := strconv.ParseInt(literal, 10, 64); err == nil {
			return NewInt(i), nil
		}
		// An integer token beyond int64 parses as a big integer, matching
		// Ruby's JSON (it used to degrade silently to a float). The literal's
		// length is bounded by the payload input guard; charge steps for the
		// conversion before running it and the materialized value against the
		// memory quota after, like the container paths do.
		if p.exec != nil {
			if err := p.exec.stepN(1 + len(literal)/jsonBigIntStepDigits); err != nil {
				return NewNil(), err
			}
		}
		reserved := estimatedBigIntStructBytes + len(literal) + 8*estimatedBigIntWordBytes
		if err := p.reserve(reserved); err != nil {
			return NewNil(), err
		}
		if bi, ok := new(big.Int).SetString(literal, 10); ok {
			val := value.AdoptBigInt(bi)
			actual := estimatedBigIntStructBytes + cap(bi.Bits())*estimatedBigIntWordBytes
			p.used -= reserved
			if err := p.reserve(actual); err != nil {
				return NewNil(), err
			}
			return val, nil
		}
	}

	f, err := strconv.ParseFloat(literal, 64)
	if err != nil {
		return NewNil(), jsonInvalidNumberError(literal)
	}
	return NewFloat(f), nil
}

func (p *jsonValueParser) parseString() (string, error) {
	p.pos++
	start := p.pos
	if len(p.raw)-p.pos >= jsonASCIISpanMin && jsonParseASCIISpan(p.raw[p.pos:p.pos+jsonASCIISpanMin]) == jsonASCIISpanMin {
		p.pos += jsonASCIISpanMin
		p.pos += jsonParseASCIISpan(p.raw[p.pos:])
	}
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

func (p *jsonValueParser) parseEscapedString(start int) (string, error) {
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
	capacity := roundedAllocSize(size)
	if err := p.checkExtra(estimatedStringHeaderBytes + capacity + size); err != nil {
		return "", err
	}
	buf := make([]byte, size)
	p.pos = position
	if _, err := p.parseEscapedContents(start, buf); err != nil {
		return "", err
	}
	// Keep only this token's bytes, so discarded-subtree accounting can use
	// its string length and no result retains spare decoding capacity.
	out := string(buf)
	p.used += estimatedStringHeaderBytes + len(out)
	return out, nil
}

// parseEscapedContents measures the token when out is nil; otherwise out must
// fit that measurement. The fill pass writes bytes without growing a buffer.
func (p *jsonValueParser) parseEscapedContents(start int, out []byte) (int, error) {
	size := p.pos - start
	if out != nil {
		copy(out, p.raw[start:p.pos])
	}

	// A long initial ASCII run is a useful predictor for subsequent runs.
	// Short runs and Unicode return to the scalar loop for the rest of the token.
	if size >= jsonASCIISpanMin {
		for p.pos < len(p.raw) && p.raw[p.pos] == '\\' {
			p.pos++
			r, err := p.parseStringEscape()
			if err != nil {
				return 0, err
			}
			if out != nil {
				size += utf8.EncodeRune(out[size:], r)
			} else {
				size += utf8.RuneLen(r)
			}
			n := jsonParseASCIISpan(p.raw[p.pos:])
			if out != nil {
				copy(out[size:], p.raw[p.pos:p.pos+n])
			}
			p.pos += n
			size += n
			if n < jsonASCIISpanMin {
				break
			}
		}
	}

	return p.parseEscapedContentsFallback(size, out)
}

func (p *jsonValueParser) parseEscapedContentsFallback(size int, out []byte) (int, error) {
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
		if out != nil {
			size += utf8.EncodeRune(out[size:], r)
		} else {
			size += utf8.RuneLen(r)
		}
	}
	return 0, fmt.Errorf("unexpected end of JSON input")
}

func (p *jsonValueParser) parseStringEscape() (rune, error) {
	if p.pos >= len(p.raw) {
		return 0, fmt.Errorf("unexpected end of JSON input")
	}

	switch c := p.raw[p.pos]; c {
	case '"', '\\', '/':
		p.pos++
		return rune(c), nil
	case 'b':
		p.pos++
		return '\b', nil
	case 'f':
		p.pos++
		return '\f', nil
	case 'n':
		p.pos++
		return '\n', nil
	case 'r':
		p.pos++
		return '\r', nil
	case 't':
		p.pos++
		return '\t', nil
	case 'u':
		p.pos++
		r, err := p.parseUnicodeEscape()
		if err != nil {
			return 0, err
		}
		return r, nil
	default:
		return 0, fmt.Errorf("invalid character %q in string escape code", c)
	}
}

func (p *jsonValueParser) parseUnicodeEscape() (rune, error) {
	r, err := p.readHexRune()
	if err != nil {
		return 0, err
	}
	if r < 0xd800 || r > 0xdfff {
		return r, nil
	}
	if r > 0xdbff {
		return utf8.RuneError, nil
	}
	if p.pos+2 > len(p.raw) || p.raw[p.pos] != '\\' || p.raw[p.pos+1] != 'u' {
		return utf8.RuneError, nil
	}

	save := p.pos
	p.pos += 2
	low, err := p.readHexRune()
	if err != nil {
		p.pos = save
		return utf8.RuneError, nil
	}
	if low < 0xdc00 || low > 0xdfff {
		p.pos = save
		return utf8.RuneError, nil
	}
	return utf16.DecodeRune(r, low), nil
}

func (p *jsonValueParser) readHexRune() (rune, error) {
	if p.pos+4 > len(p.raw) {
		return 0, fmt.Errorf("unexpected end of JSON input")
	}
	var r rune
	for range 4 {
		c := p.raw[p.pos]
		p.pos++
		value, ok := jsonHexValue(c)
		if !ok {
			return 0, fmt.Errorf("invalid character %q in unicode escape", c)
		}
		r = r<<4 | rune(value)
	}
	return r, nil
}

func (p *jsonValueParser) skipWhitespace() {
	for p.pos < len(p.raw) {
		switch p.raw[p.pos] {
		case ' ', '\n', '\r', '\t':
			p.pos++
		default:
			return
		}
	}
}

func (p *jsonValueParser) consumeLiteral(literal string) bool {
	if !strings.HasPrefix(p.raw[p.pos:], literal) {
		return false
	}
	p.pos += len(literal)
	return true
}

func (p *jsonValueParser) consumeByte(b byte) bool {
	if p.pos < len(p.raw) && p.raw[p.pos] == b {
		p.pos++
		return true
	}
	return false
}

func (p *jsonValueParser) enterContainer() error {
	if p.depth >= maxJSONNestingDepth {
		return errJSONMaxDepth
	}
	p.depth++
	return nil
}

func (p *jsonValueParser) leaveContainer() {
	p.depth--
}

func isJSONDigit(c byte) bool {
	return c >= '0' && c <= '9'
}

func isJSONOneToNine(c byte) bool {
	return c >= '1' && c <= '9'
}

func jsonHexValue(c byte) (byte, bool) {
	switch {
	case c >= '0' && c <= '9':
		return c - '0', true
	case c >= 'a' && c <= 'f':
		return c - 'a' + 10, true
	case c >= 'A' && c <= 'F':
		return c - 'A' + 10, true
	default:
		return 0, false
	}
}

// appendJSONValue renders val and settles the output produced so far.
//
// Literals, delimiters and separators never reach checkOutputBytes on their
// own, so a value that fails after a long prefix -- tens of thousands of nils
// followed by an unsupported value -- left that whole prefix charged to
// nothing, and the serialization error is rescuable. Settling per appended
// value bills each one as it is accepted, so a failure partway through keeps
// what was already produced, and the output cap applies to it too.
func appendJSONValue(buf []byte, val Value, state *jsonStringifyState) ([]byte, error) {
	out, err := appendJSONValueRendered(buf, val, state)
	if err != nil {
		return nil, err
	}
	if err := state.settleOutput(len(out)); err != nil {
		return nil, err
	}
	return out, nil
}

func appendJSONValueRendered(buf []byte, val Value, state *jsonStringifyState) ([]byte, error) {
	switch val.Kind() {
	case KindNil:
		return append(buf, "null"...), nil
	case KindBool:
		if val.Bool() {
			return append(buf, "true"...), nil
		}
		return append(buf, "false"...), nil
	case KindInt:
		if bi, ok := value.BigIntPayload(val); ok {
			// Ruby's JSON emits bignums as bare decimals (no float collapse,
			// no quotes). Reject an output that provably exceeds the payload
			// cap before paying for the base conversion, and charge steps for
			// the conversion like the parse path does.
			digits := value.BigIntDecimalLenUpperBound(val)
			if len(buf)+digits-1 > maxJSONPayloadBytes {
				return nil, guardLimitErrorf("JSON.stringify output exceeds limit %d bytes", maxJSONPayloadBytes)
			}
			if state.exec != nil {
				if err := state.exec.stepN(1 + digits/jsonBigIntStepDigits); err != nil {
					return nil, err
				}
			}
			return bi.Append(buf, 10), nil
		}
		return strconv.AppendInt(buf, val.Int(), 10), nil
	case KindFloat:
		f := val.Float()
		if math.IsInf(f, 0) || math.IsNaN(f) {
			return nil, fmt.Errorf("JSON.stringify failed: json: unsupported value: %s", formatFloat(f))
		}
		return appendJSONFloat(buf, f), nil
	case KindString, KindSymbol:
		return appendJSONString(buf, val.String(), state)
	case KindEnumValue:
		if member := valueEnumValue(val); member != nil {
			return appendJSONString(buf, member.Symbol, state)
		}
		return nil, fmt.Errorf("JSON.stringify unsupported enum value")
	case KindArray:
		arr := val.Array()
		if err := state.enterContainer(); err != nil {
			return nil, err
		}
		defer state.leaveContainer()

		id := reflect.ValueOf(arr).Pointer()
		arraySlot, err := state.pushSeenArray(id)
		if err != nil {
			return nil, err
		}
		defer state.popSeenArray(arraySlot)

		buf = append(buf, '[')
		// Settle the delimiter before descending. A container that fails below
		// this point -- nesting depth, an unsupported value -- returns without
		// reaching the settlement in appendJSONValue, so every level's bracket
		// went uncharged: 10,001 nested arrays emitted 10,000 of them for
		// nothing, and the depth error is rescuable.
		if err := state.settleOutput(len(buf)); err != nil {
			return nil, err
		}
		for i, item := range arr {
			if i > 0 {
				buf = append(buf, ',')
			}
			updated, err := appendJSONValue(buf, item, state)
			if err != nil {
				if errors.Is(err, errJSONMaxDepth) {
					return nil, err
				}
				return nil, fmt.Errorf("JSON.stringify array index %d: %w", i, err)
			}
			buf = updated
		}
		return append(buf, ']'), nil
	case KindHash, KindObject:
		if err := state.enterContainer(); err != nil {
			return nil, err
		}
		defer state.leaveContainer()

		id := jsonObjectIdentity(val)
		hashSlot, err := state.pushSeenHash(id)
		if err != nil {
			return nil, err
		}
		defer state.popSeenHash(hashSlot)

		entries, err := jsonObjectEntries(val)
		if err != nil {
			return nil, err
		}

		buf = append(buf, '{')
		// Settle the delimiter before descending. A container that fails below
		// this point -- nesting depth, an unsupported value -- returns without
		// reaching the settlement in appendJSONValue, so every level's bracket
		// went uncharged: 10,001 nested arrays emitted 10,000 of them for
		// nothing, and the depth error is rescuable.
		if err := state.settleOutput(len(buf)); err != nil {
			return nil, err
		}
		for i, entry := range entries {
			if i > 0 {
				buf = append(buf, ',')
			}
			buf, err = appendJSONString(buf, entry.key, state)
			if err != nil {
				return nil, err
			}
			buf = append(buf, ':')
			updated, err := appendJSONValue(buf, entry.value, state)
			if err != nil {
				if errors.Is(err, errJSONMaxDepth) {
					return nil, err
				}
				return nil, fmt.Errorf("JSON.stringify key %q: %w", entry.key, err)
			}
			buf = updated
		}
		return append(buf, '}'), nil
	default:
		return nil, fmt.Errorf("JSON.stringify unsupported value type %s", val.Kind())
	}
}

type jsonObjectEntry struct {
	key   string
	value Value
}

func jsonObjectIdentity(val Value) uintptr {
	if val.Kind() == KindHash {
		if id := hashIdentity(val); id != 0 {
			return id
		}
	}
	return reflect.ValueOf(val.HashEntryMap()).Pointer()
}

// jsonObjectEntries returns the members stringify emits, in the order the hash
// iterates: Ruby-style insertion order for a hash built by a script, the way
// Ruby's JSON.generate does, and sorted keys for a bare host map or an object,
// which record no order.
func jsonObjectEntries(val Value) ([]jsonObjectEntry, error) {
	// Fill the returned buffer directly and sort it in place when insertion
	// order is unavailable. RangeHashEntries' fallback used to allocate a
	// second []Value of keys on top of this slice, which stringify's quota
	// checks never reserved.
	n := val.HashLen()
	entries := make([]jsonObjectEntry, 0, n)
	val.RangeHashEntries(func(key string, item Value) {
		entries = append(entries, jsonObjectEntry{key: key, value: item})
	})
	// RangeHashEntries walks recorded insertion order when it still covers
	// the live entries. Objects and host-mutated maps have no such record,
	// so sort the JSON buffer in place rather than allocating a second
	// []HashEntry on top of it.
	if !val.HashUsesRecordedOrder() {
		slices.SortFunc(entries, func(a, b jsonObjectEntry) int {
			return cmp.Compare(a.key, b.key)
		})
	}
	return entries, nil
}

func appendJSONFloat(buf []byte, f float64) []byte {
	format := byte('f')
	abs := math.Abs(f)
	if abs != 0 && (abs < 1e-6 || abs >= 1e21) {
		format = 'e'
	}

	buf = strconv.AppendFloat(buf, f, format, -1, 64)
	if format == 'e' {
		n := len(buf)
		if n >= 4 && buf[n-4] == 'e' && buf[n-3] == '-' && buf[n-2] == '0' {
			buf[n-2] = buf[n-1]
			buf = buf[:n-1]
		}
	}
	return buf
}

func (state *jsonStringifyState) enterContainer() error {
	if state.depth >= maxJSONNestingDepth {
		return fmt.Errorf("JSON.stringify %w", errJSONMaxDepth)
	}
	state.depth++
	return nil
}

func (state *jsonStringifyState) leaveContainer() {
	state.depth--
}

func (state *jsonStringifyState) pushSeenArray(id uintptr) (jsonSeenSlot, error) {
	if id == 0 {
		return jsonSeenSlot{}, nil
	}
	for i := range state.seenArrayLen {
		if state.seenArrayInline[i] == id {
			return jsonSeenSlot{}, fmt.Errorf("JSON.stringify does not support cyclic arrays")
		}
	}
	if _, seen := state.seenArrays[id]; seen {
		return jsonSeenSlot{}, fmt.Errorf("JSON.stringify does not support cyclic arrays")
	}
	if state.seenArrays == nil && state.seenArrayLen < len(state.seenArrayInline) {
		index := state.seenArrayLen
		state.seenArrayInline[index] = id
		state.seenArrayLen++
		return jsonSeenSlot{id: id, index: index}, nil
	}
	if state.seenArrays == nil {
		state.seenArrays = make(map[uintptr]struct{}, len(state.seenArrayInline)+1)
		for _, seenID := range state.seenArrayInline {
			if seenID != 0 {
				state.seenArrays[seenID] = struct{}{}
			}
		}
	}
	state.seenArrays[id] = struct{}{}
	return jsonSeenSlot{id: id, inMap: true}, nil
}

func (state *jsonStringifyState) popSeenArray(slot jsonSeenSlot) {
	if slot.id == 0 {
		return
	}
	if slot.inMap {
		delete(state.seenArrays, slot.id)
		return
	}
	if state.seenArrays != nil {
		delete(state.seenArrays, slot.id)
	}
	last := state.seenArrayLen - 1
	if slot.index >= 0 && slot.index <= last {
		state.seenArrayInline[slot.index] = state.seenArrayInline[last]
		state.seenArrayInline[last] = 0
		state.seenArrayLen--
	}
}

func (state *jsonStringifyState) pushSeenHash(id uintptr) (jsonSeenSlot, error) {
	if id == 0 {
		return jsonSeenSlot{}, nil
	}
	for i := range state.seenHashLen {
		if state.seenHashInline[i] == id {
			return jsonSeenSlot{}, fmt.Errorf("JSON.stringify does not support cyclic objects")
		}
	}
	if _, seen := state.seenHashes[id]; seen {
		return jsonSeenSlot{}, fmt.Errorf("JSON.stringify does not support cyclic objects")
	}
	if state.seenHashes == nil && state.seenHashLen < len(state.seenHashInline) {
		index := state.seenHashLen
		state.seenHashInline[index] = id
		state.seenHashLen++
		return jsonSeenSlot{id: id, index: index}, nil
	}
	if state.seenHashes == nil {
		state.seenHashes = make(map[uintptr]struct{}, len(state.seenHashInline)+1)
		for _, seenID := range state.seenHashInline {
			if seenID != 0 {
				state.seenHashes[seenID] = struct{}{}
			}
		}
	}
	state.seenHashes[id] = struct{}{}
	return jsonSeenSlot{id: id, inMap: true}, nil
}

func (state *jsonStringifyState) popSeenHash(slot jsonSeenSlot) {
	if slot.id == 0 {
		return
	}
	if slot.inMap {
		delete(state.seenHashes, slot.id)
		return
	}
	if state.seenHashes != nil {
		delete(state.seenHashes, slot.id)
	}
	last := state.seenHashLen - 1
	if slot.index >= 0 && slot.index <= last {
		state.seenHashInline[slot.index] = state.seenHashInline[last]
		state.seenHashInline[last] = 0
		state.seenHashLen--
	}
}

func appendJSONString(buf []byte, s string, state *jsonStringifyState) ([]byte, error) {
	const hexDigits = "0123456789abcdef"

	if err := state.checkOutputBytes(len(buf) + 1); err != nil {
		return nil, err
	}
	buf = append(buf, '"')
	start := 0
	i := 0
	scanASCII := len(s) >= jsonASCIISpanMin && jsonStringifyASCIISpan(s[:jsonASCIISpanMin]) == jsonASCIISpanMin
	if scanASCII {
		i = jsonASCIISpanMin + jsonStringifyASCIISpan(s[jsonASCIISpanMin:])
	}
	for i < len(s) {
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
			if scanASCII {
				n := jsonStringifyASCIISpan(s[i:])
				i += n
				scanASCII = n >= jsonASCIISpanMin
			}
			continue
		}

		scanASCII = false
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

// settleOutput charges the step quota for output produced so far and enforces
// the payload cap. It deliberately does not project against the memory quota:
// that opens a base walk, whose memo is bypassed while a builtin is on the
// stack, so calling it per value made serializing a wide container quadratic in
// its element count. checkOutputBytes keeps the projection for the places that
// had it before -- the string path and the finished payload.
func (state *jsonStringifyState) settleOutput(size int) error {
	if state.exec != nil {
		if steps := size / stringScanBytesPerStep; steps > state.chargedSteps {
			if err := state.exec.stepN(steps - state.chargedSteps); err != nil {
				return err
			}
			state.chargedSteps = steps
		}
	}
	if size > maxJSONPayloadBytes {
		return guardLimitErrorf("JSON.stringify output exceeds limit %d bytes", maxJSONPayloadBytes)
	}
	return nil
}

func (state *jsonStringifyState) checkOutputBytes(size int) error {
	if state.exec != nil {
		// Charged before the limit is reported, and by output rather than input:
		// escaping emits up to six bytes for one control character, so billing
		// the string's length under-charged an escape-heavy value several times
		// over. Every JSON byte comes through here, so this covers the whole
		// rendering rather than the string case alone, and JSON never emits
		// fewer bytes than it read.
		if steps := size / stringScanBytesPerStep; steps > state.chargedSteps {
			if err := state.exec.stepN(steps - state.chargedSteps); err != nil {
				return err
			}
			state.chargedSteps = steps
		}
	}
	if size > maxJSONPayloadBytes {
		return guardLimitErrorf("JSON.stringify output exceeds limit %d bytes", maxJSONPayloadBytes)
	}
	if state.exec == nil {
		return nil
	}
	return state.exec.checkProjectedStringBytes(size)
}
