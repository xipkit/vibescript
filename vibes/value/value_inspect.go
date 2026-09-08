package value

import (
	"reflect"
	"strings"
	"unicode"
)

// Inspect returns a debug representation of v, mirroring Ruby's Object#inspect.
// Unlike String (which is the to_s form), Inspect preserves quoting and escaping
// for strings, renders symbols with their leading colon, and recurses into
// arrays and hashes so the result is a stable, parseable debug rendering. Hash
// entries use Vibescript's colon-label key form (`{ name: "Ada" }`) rather than
// Ruby's hash-rocket syntax, which Vibescript does not support, so the output
// round-trips as a Vibescript literal. Cycles are collapsed to <cycle> exactly
// like String.
func (v Value) Inspect() string {
	var buf strings.Builder
	state := newValueStringState()
	// Inspect rendering is best-effort and unbounded here; callers that must
	// guard against hostile inputs use InspectBounded instead. The unbounded
	// path never reports the truncation sentinel, so the error is always nil.
	_ = v.appendInspect(&buf, state, 0)
	return buf.String()
}

// InspectBounded renders v like Inspect but stops once the formatted output
// would exceed limit bytes, returning the partial output and
// ErrStringRenderTruncated. A non-positive limit disables the byte budget.
// Regardless of limit, descent beyond 16,384 nested composites stops with
// partial output and ErrStringRenderDepthExceeded. Like StringBounded, it writes
// into a single growing buffer and checks the budget after each piece, so a
// hostile composite cannot allocate an output much larger than limit before
// the budget trips. Cycle handling is identical to Inspect.
func (v Value) InspectBounded(limit int) (string, error) {
	var buf strings.Builder
	state := newBoundedValueStringState()
	if err := v.appendInspect(&buf, state, limit); err != nil {
		return buf.String(), err
	}
	return buf.String(), nil
}

// WriteInspectTo streams the same bytes Inspect would return for v directly into
// buf, without first materializing the rendered representation as a separate
// string. It mirrors WriteStringTo: callers that have already bounded the
// rendering against a quota (such as the sandbox's inspect memory guard, which
// reserves the projected length before calling) use it to render straight into a
// builder they grew to the projected size, so the peak allocation stays the
// single backing array the quota already charged rather than the doubling growth
// a fresh zero-capacity builder would take. It delegates to the unbounded inspect
// renderer, so writing into a strings.Builder never fails.
// It is intended for the interpreter's internal use; hosts should not call
// it, and it carries no compatibility promise (see
// docs/embedding-api-stability.md).
func (v Value) WriteInspectTo(buf *strings.Builder) {
	_ = v.appendInspect(buf, newValueStringState(), 0)
}

// InspectByteLenBounded reports the number of bytes Inspect would produce for
// v without materializing the rendering, so callers can bound an allocation
// before it happens. It walks composites with the same cycle detection Inspect
// uses and invokes step once per node visited, so a caller can charge a sandbox
// step budget against the traversal and abort it when step returns an error. The
// first error step reports stops the walk and is returned alongside the partial
// count. See StringByteLenBounded for why driving step from inside the walk
// matters for shared-but-acyclic graphs.
// It is intended for the interpreter's internal use; hosts should not call
// it, and it carries no compatibility promise (see
// docs/embedding-api-stability.md).
func (v Value) InspectByteLenBounded(step func() error) (int, error) {
	return v.inspectByteLenBoundedWithState(newValueStringState(), step)
}

// appendInspect streams v's debug rendering into buf with the same bounded
// behavior as appendString: when limit is positive every write routes through a
// bounded helper, so the first write that would exceed the budget stops and
// returns ErrStringRenderTruncated.
func (v Value) appendInspect(buf *strings.Builder, state *valueStringState, limit int) error {
	switch v.kind {
	case KindString:
		return appendQuotedStringBounded(buf, v.data.(string), limit)
	case KindSymbol:
		return appendInspectSymbolBounded(buf, v.data.(string), limit)
	case KindNil:
		return appendBounded(buf, "nil", limit)
	case KindArray:
		return v.appendInspectArray(buf, state, limit)
	case KindHash, KindObject:
		// Namespace and host objects back their fields with the same
		// map[string]Value a hash uses and resolve the shared hashMember
		// dispatch (keys, size, inspect, ...), so inspect renders them with the
		// hash's composite form rather than the opaque "<object>" String gives.
		return v.appendInspectHash(buf, state, limit)
	default:
		// Scalars without a distinct debug form (bool, int, float, money,
		// duration, time, range, and runtime kinds) render the same bytes as
		// String. Cap the write to the remaining budget for the same reason
		// appendString does, and refuse a big integer that provably cannot fit
		// before its (superlinear) base conversion runs.
		if limit > 0 && bigIntRenderExceedsLimit(v, limit-buf.Len()) {
			return ErrStringRenderTruncated
		}
		return appendBounded(buf, v.String(), limit)
	}
}

func (v Value) appendInspectArray(buf *strings.Builder, state *valueStringState, limit int) error {
	elems := v.Array()
	id := SliceIdentity{
		Ptr: reflect.ValueOf(elems).Pointer(),
		Len: len(elems),
		Cap: cap(elems),
	}
	if id.Ptr != 0 {
		if _, seen := state.arrays[id]; seen {
			return appendBounded(buf, cycleMarker, limit)
		}
	}
	if err := state.enterComposite(buf, limit); err != nil {
		return err
	}
	defer state.leaveComposite()
	if id.Ptr != 0 {
		state.arrays[id] = struct{}{}
		defer delete(state.arrays, id)
	}
	if err := appendByteBounded(buf, '[', limit); err != nil {
		return err
	}
	for i, e := range elems {
		if i > 0 {
			if err := appendBounded(buf, elementSeparator, limit); err != nil {
				return err
			}
		}
		if err := e.appendInspect(buf, state, limit); err != nil {
			return err
		}
	}
	return appendByteBounded(buf, ']', limit)
}

func (v Value) appendInspectHash(buf *strings.Builder, state *valueStringState, limit int) error {
	entries := v.HashEntryMap()
	if len(entries) == 0 {
		return appendBounded(buf, "{}", limit)
	}
	ptr := reflect.ValueOf(entries).Pointer()
	if ptr != 0 {
		if _, seen := state.maps[ptr]; seen {
			return appendBounded(buf, cycleMarker, limit)
		}
	}
	if err := state.enterComposite(buf, limit); err != nil {
		return err
	}
	defer state.leaveComposite()
	if ptr != 0 {
		state.maps[ptr] = struct{}{}
		defer delete(state.maps, ptr)
	}
	if err := appendByteBounded(buf, '{', limit); err != nil {
		return err
	}
	first := true
	write := func(name string, val Value) error {
		if !first {
			if err := appendBounded(buf, elementSeparator, limit); err != nil {
				return err
			}
		}
		first = false
		if err := appendInspectHashKeyBounded(buf, name, limit); err != nil {
			return err
		}
		if err := appendBounded(buf, keyValueSeparator, limit); err != nil {
			return err
		}
		return val.appendInspect(buf, state, limit)
	}
	if v.HashUsesRecordedOrder() {
		var walkErr error
		v.RangeHashEntries(func(name string, val Value) {
			if walkErr != nil {
				return
			}
			walkErr = write(name, val)
		})
		if walkErr != nil {
			return walkErr
		}
	} else {
		var entryBuf [smallHashIterationBuffer]HashEntry
		for _, entry := range v.HashEntriesInto(entryBuf[:]) {
			if err := write(entry.Key.String(), entry.Value); err != nil {
				return err
			}
		}
	}
	return appendByteBounded(buf, '}', limit)
}

func (v Value) inspectByteLenBoundedWithState(state *valueStringState, step func() error) (int, error) {
	if err := step(); err != nil {
		return 0, err
	}
	switch v.kind {
	case KindString:
		str := v.data.(string)
		if state.chargeBytes != nil {
			if err := state.chargeBytes(len(str)); err != nil {
				return 0, err
			}
		}
		return quotedStringByteLen(str), nil
	case KindSymbol:
		sym := v.data.(string)
		if state.chargeBytes != nil {
			if err := state.chargeBytes(len(sym)); err != nil {
				return 0, err
			}
		}
		return inspectSymbolByteLen(sym), nil
	case KindRegex:
		// See Value.StringByteLen: sizing a regex must not render it, and
		// inspect renders one exactly as String does. The source walk StringLen
		// performs is charged before it runs -- the single per-node step above
		// does not cover a scan proportional to the source.
		if state.chargeBytes != nil {
			if err := state.chargeBytes(len(v.data.(Regex).Source)); err != nil {
				return 0, err
			}
		}
		if err := chargeRegexSourceSteps(v, step); err != nil {
			return 0, err
		}
		return v.data.(Regex).StringLen(), nil
	case KindNil:
		return len("nil"), nil
	case KindArray:
		elems := v.Array()
		id := SliceIdentity{
			Ptr: reflect.ValueOf(elems).Pointer(),
			Len: len(elems),
			Cap: cap(elems),
		}
		if id.Ptr != 0 {
			if _, seen := state.arrays[id]; seen {
				return len(cycleMarker), nil
			}
			state.arrays[id] = struct{}{}
			defer delete(state.arrays, id)
		}
		total := len(arrayOpen) + len(arrayClose)
		total += separatorBytes(len(elems))
		for _, e := range elems {
			n, err := e.inspectByteLenBoundedWithState(state, step)
			if err != nil {
				return 0, err
			}
			total += n
		}
		return total, nil
	case KindHash, KindObject:
		entries := v.HashEntryMap()
		if len(entries) == 0 {
			return len(hashOpen) + len(hashClose), nil
		}
		ptr := reflect.ValueOf(entries).Pointer()
		if ptr != 0 {
			if _, seen := state.maps[ptr]; seen {
				return len(cycleMarker), nil
			}
			state.maps[ptr] = struct{}{}
			defer delete(state.maps, ptr)
		}
		total := len(hashOpen) + len(hashClose)
		total += separatorBytes(len(entries))
		for k, val := range entries {
			if state.chargeBytes != nil {
				if err := state.chargeBytes(len(k)); err != nil {
					return 0, err
				}
			}
			total += inspectHashKeyByteLen(k) + len(keyValueSeparator)
			n, err := val.inspectByteLenBoundedWithState(state, step)
			if err != nil {
				return 0, err
			}
			total += n
		}
		return total, nil
	default:
		// A big integer's projection performs the same superlinear base
		// conversion the rendering will; charge steps for it up front so the
		// step quota trips before the conversion runs. The length itself is
		// the allocation-free decimal bound: materializing the decimal here
		// would allocate the full rendering before any caller reservation
		// covers it, so sizing must not convert. The bound never falls short,
		// and callers reserve or pregrow at the projection, so the render
		// stays within what was validated.
		if bi, ok := BigIntPayload(v); ok {
			if err := chargeBigIntRenderSteps(v, step); err != nil {
				return 0, err
			}
			return bigIntDecimalLenUpperBound(bi), nil
		}
		if err := chargeBigIntRenderSteps(v, step); err != nil {
			return 0, err
		}
		return len(v.String()), nil
	}
}

// quoteString renders s as a double-quoted Vibescript string literal. It escapes
// only the sequences a Vibescript double-quoted literal recognizes (\\, \", \n,
// \t) and the interpolation marker (\#{). Vibescript has no \r, \xNN, \uNNNN, or
// octal escapes, so any other byte is written verbatim: the resulting literal
// round-trips through the lexer exactly, which would not hold if it emitted an
// escape the language cannot decode. The output is therefore a faithful,
// parseable debug rendering of the string rather than an ASCII-only one.
func quoteString(s string) string {
	var b strings.Builder
	// Two delimiters plus headroom for the common case of a few escapes.
	b.Grow(len(s) + 2)
	b.WriteByte('"')
	for i := range len(s) {
		c := s[i]
		switch c {
		case '\\':
			b.WriteString(`\\`)
		case '"':
			b.WriteString(`\"`)
		case '\n':
			b.WriteString(`\n`)
		case '\t':
			b.WriteString(`\t`)
		case '#':
			// Escape only the interpolation marker (#{) so a literal "#{...}" in
			// a string does not turn into an interpolation when the rendered
			// literal is re-parsed. A lone '#' is written verbatim.
			if i+1 < len(s) && s[i+1] == '{' {
				b.WriteString(`\#`)
			} else {
				b.WriteByte(c)
			}
		default:
			b.WriteByte(c)
		}
	}
	b.WriteByte('"')
	return b.String()
}

// appendQuotedStringBounded streams quoteString(s)'s output directly into buf,
// escaping one byte at a time, so a hostile string never materializes its full
// quoted form as a temporary before the budget can trip. When limit is positive
// every fragment (the delimiters and each escape, all at most two bytes) routes
// through appendBounded, so the first fragment that would exceed the budget stops
// and returns ErrStringRenderTruncated without copying the rest of s. The escape
// rules must stay in lockstep with quoteString; a non-positive limit writes the
// whole quoted form.
func appendQuotedStringBounded(buf *strings.Builder, s string, limit int) error {
	if err := appendByteBounded(buf, '"', limit); err != nil {
		return err
	}
	for i := range len(s) {
		var frag string
		switch c := s[i]; c {
		case '\\':
			frag = `\\`
		case '"':
			frag = `\"`
		case '\n':
			frag = `\n`
		case '\t':
			frag = `\t`
		case '#':
			// Escape only the interpolation marker (#{); a lone '#' is verbatim,
			// matching quoteString.
			if i+1 < len(s) && s[i+1] == '{' {
				frag = `\#`
			} else {
				frag = "#"
			}
		default:
			if err := appendByteBounded(buf, c, limit); err != nil {
				return err
			}
			continue
		}
		if err := appendBounded(buf, frag, limit); err != nil {
			return err
		}
	}
	return appendByteBounded(buf, '"', limit)
}

// quotedStringByteLen reports how many bytes quoteString(s) would produce
// without allocating the quoted result. The byte-length projection used to bound
// inspect allocations relies on this: measuring a quota-sized string by building
// its (potentially larger) escaped form would defeat the guard, allocating the
// oversized buffer before the quota check that is meant to reject it. The escape
// rules here must stay in lockstep with quoteString.
func quotedStringByteLen(s string) int {
	total := 2 // opening and closing quote
	for i := range len(s) {
		switch c := s[i]; c {
		case '\\', '"', '\n', '\t':
			total += 2
		case '#':
			// Only the interpolation marker (#{) is escaped to \#; a lone '#' is
			// written verbatim, matching quoteString.
			if i+1 < len(s) && s[i+1] == '{' {
				total += 2
			} else {
				total++
			}
		default:
			total++
		}
	}
	return total
}

// inspectSymbol renders a symbol's debug form. A symbol whose name is a bare
// identifier renders as :name; any other name (containing spaces, punctuation,
// or empty) is quoted as :"name" with the same escaping strings use, matching
// the shape of Ruby's Symbol#inspect while staying within Vibescript's escape
// set. The quoted form is a re-parseable quoted-symbol literal: Vibescript
// accepts :"name" anywhere a symbol literal is allowed, so the rendered symbol
// round-trips as source.
func inspectSymbol(name string) string {
	if isBareIdentifier(name) {
		return ":" + name
	}
	return ":" + quoteString(name)
}

// appendInspectSymbolBounded streams inspectSymbol(name)'s output into buf
// without first building the quoted body, mirroring appendQuotedStringBounded:
// the leading colon and a bare identifier route through the bounded helpers, and
// a name that must be quoted is escaped a byte at a time so a hostile symbol name
// never materializes its full quoted form before the budget trips. The shape must
// stay in lockstep with inspectSymbol.
func appendInspectSymbolBounded(buf *strings.Builder, name string, limit int) error {
	if err := appendByteBounded(buf, ':', limit); err != nil {
		return err
	}
	if isBareIdentifier(name) {
		return appendBounded(buf, name, limit)
	}
	return appendQuotedStringBounded(buf, name, limit)
}

// inspectSymbolByteLen reports the byte length of inspectSymbol(name) without
// allocating the rendering, so the byte-length projection can measure a symbol's
// debug form without building its quoted body. It mirrors inspectSymbol: a bare
// identifier costs the leading colon plus its bytes, and any other name costs the
// colon plus its quoted length.
func inspectSymbolByteLen(name string) int {
	if isBareIdentifier(name) {
		return 1 + len(name)
	}
	return 1 + quotedStringByteLen(name)
}

// inspectHashKey renders a hash key label for Inspect, without the trailing
// colon (the caller supplies keyValueSeparator, ": "). Vibescript hash keys are
// symbols, so a bare-identifier key renders as the colon-label shorthand
// (yielding `name: value`) and any other key renders quoted (yielding
// `"name with space": value`). Both forms are valid Vibescript hash-literal
// keys, so the rendered hash round-trips as a literal.
func inspectHashKey(key string) string {
	if isBareIdentifier(key) {
		return key
	}
	return quoteString(key)
}

// appendInspectHashKeyBounded streams inspectHashKey(key)'s output into buf
// without first building the quoted body, mirroring appendInspectSymbolBounded: a
// bare-identifier key routes through the bounded helper, and any other key is
// escaped a byte at a time so a hostile key never materializes its full quoted
// form before the budget trips. The shape must stay in lockstep with
// inspectHashKey.
func appendInspectHashKeyBounded(buf *strings.Builder, key string, limit int) error {
	if isBareIdentifier(key) {
		return appendBounded(buf, key, limit)
	}
	return appendQuotedStringBounded(buf, key, limit)
}

// inspectHashKeyByteLen reports the byte length of inspectHashKey(key) without
// allocating the rendering, so the byte-length projection can measure a quoted
// key without building it. It mirrors inspectHashKey: a bare identifier costs its
// own bytes and any other key costs its quoted length.
func inspectHashKeyByteLen(key string) int {
	if isBareIdentifier(key) {
		return len(key)
	}
	return quotedStringByteLen(key)
}

// isBareIdentifier reports whether s is a non-empty Vibescript identifier: it
// starts with a letter or underscore and continues with letters, digits, or
// underscores. It mirrors ast.IsIdentifierStart/IsIdentifierRune but stays local
// to the value package, which must not import the AST. The optional trailing ?
// and ! that method names allow are excluded because a hash key or symbol ending
// in them is not written bare in a literal.
func isBareIdentifier(s string) bool {
	if s == "" {
		return false
	}
	for i, r := range s {
		if i == 0 {
			if !unicode.IsLetter(r) && r != '_' {
				return false
			}
			continue
		}
		if !unicode.IsLetter(r) && !unicode.IsDigit(r) && r != '_' {
			return false
		}
	}
	return true
}
