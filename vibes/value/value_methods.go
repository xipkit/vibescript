package value

import (
	"errors"
	"fmt"
	"math"
	"math/big"
	"reflect"
	"slices"
	"strconv"
	"strings"
	"time"
	"unicode/utf8"
)

// String returns the human-readable name of the ValueKind.
func (k ValueKind) String() string {
	switch k {
	case KindNil:
		return "nil"
	case KindBool:
		return "bool"
	case KindInt:
		return "int"
	case KindFloat:
		return "float"
	case KindString:
		return "string"
	case KindArray:
		return "array"
	case KindHash:
		return "hash"
	case KindFunction:
		return "function"
	case KindBuiltin:
		return "builtin"
	case KindMoney:
		return "money"
	case KindDuration:
		return "duration"
	case KindTime:
		return "time"
	case KindSymbol:
		return "symbol"
	case KindObject:
		return "object"
	case KindRange:
		return "range"
	case KindBlock:
		return "block"
	case KindEnum:
		return "enum"
	case KindEnumValue:
		return "enum value"
	case KindClass:
		return "class"
	case KindInstance:
		return "instance"
	case KindRegex:
		return "regex"
	case KindShape:
		return "shape"
	default:
		return fmt.Sprintf("kind(%d)", int(k))
	}
}

// RuntimeStringer is the hook used by Value.String to format runtime-only
// kinds (function, builtin, block, enum, enum value, class, instance) whose
// payload types live in the vibes package. The vibes package installs this
// hook during initialization. If unset, those kinds fall back to a generic
// rendering of the underlying payload.
// It is intended for the interpreter's internal use; hosts should not rely
// on it, and it carries no compatibility promise (see
// docs/embedding-api-stability.md).
var RuntimeStringer func(v Value) (string, bool)

// RuntimeStringLen reports the byte length Value.String would return for a
// runtime-only kind, computed from the payload rather than by building the
// string. A projection that answers through RuntimeStringer allocates the very
// rendering it is meant to decide about, which defeats the guard.
// It is intended for the interpreter's internal use; hosts should not rely
// on it, and it carries no compatibility promise (see
// docs/embedding-api-stability.md).
var RuntimeStringLen func(v Value) (int, bool)

// RuntimeStringAppender writes the bytes Value.String would return for a
// runtime-only kind straight into buf, so a rendering streamed into a caller's
// charged buffer never also exists as a temporary alongside it.
//
// limit is the total byte budget for buf, matching appendBounded: a
// non-positive limit writes everything, and otherwise the hook writes at most
// limit-buf.Len() bytes and reports truncated when it had more to write. This
// keeps precision-qualified formats -- format("%.1s", Huge::Member) -- from
// materializing a whole rendering to throw nearly all of it away.
// It is intended for the interpreter's internal use; hosts should not rely
// on it, and it carries no compatibility promise (see
// docs/embedding-api-stability.md).
var RuntimeStringAppender func(v Value, buf *strings.Builder, limit int) (truncated, handled bool)

// RuntimeStringRuneLen reports the rune count Value.String would return for a
// runtime-only kind, counted from the payload rather than from a materialized
// rendering. Width-qualified formatting projects rune lengths, so this is the
// same guard RuntimeStringLen provides for the byte-length paths.
// It is intended for the interpreter's internal use; hosts should not rely
// on it, and it carries no compatibility promise (see
// docs/embedding-api-stability.md).
var RuntimeStringRuneLen func(v Value) (int, bool)

// RuntimeEqualer is the hook used by Value.Equal to compare runtime-only
// kinds whose payload types live in the vibes package. The vibes package
// installs this hook during initialization. If unset, equality for those
// kinds falls back to pointer identity of the underlying payload.
// It is intended for the interpreter's internal use; hosts should not rely
// on it, and it carries no compatibility promise (see
// docs/embedding-api-stability.md).
var RuntimeEqualer func(left, right Value) (bool, bool)

// RuntimeIdenticaler is the hook used by Value.Identical to compare
// runtime-only kinds by backing-storage identity. It differs from
// RuntimeEqualer because some runtime kinds (notably enums and enum values)
// define Equal as structural equivalence: two independently cloned enum
// members that share an owner script and name compare Equal, yet they do not
// share storage and so must not be Identical. The vibes package installs this
// hook during initialization. If unset, identity for those kinds falls back to
// the same comparison Value.Equal uses.
// It is intended for the interpreter's internal use; hosts should not rely
// on it, and it carries no compatibility promise (see
// docs/embedding-api-stability.md).
var RuntimeIdenticaler func(left, right Value) (bool, bool)

// String returns the string representation of v.
func (v Value) String() string {
	switch v.kind {
	case KindString:
		return v.data.(string)
	case KindNil:
		return ""
	case KindBool:
		if v.Bool() {
			return "true"
		}
		return "false"
	case KindInt:
		if bi, ok := v.data.(*big.Int); ok {
			// Base conversion is superlinear in the value's size; sandboxed
			// rendering paths preflight the digit count before reaching here
			// (see the bounded renderers and the runtime's rendering guards).
			return bi.Text(10)
		}
		return strconv.FormatInt(v.Int(), 10)
	case KindFloat:
		return FormatFloat(v.Float())
	case KindSymbol:
		return v.data.(string)
	case KindMoney:
		return v.data.(Money).String()
	case KindDuration:
		return v.Duration().String()
	case KindTime:
		return v.data.(time.Time).Format(time.RFC3339Nano)
	case KindRegex:
		return v.data.(Regex).String()
	case KindArray, KindHash:
		var buf strings.Builder
		state := newValueStringState()
		// Composite rendering is best-effort and unbounded here; callers that
		// must guard against hostile inputs (such as the CLI rendering a value
		// returned from an untrusted script) use StringBounded instead. The
		// unbounded path never reports the truncation sentinel, so the error is
		// always nil.
		_ = v.appendString(&buf, state, 0)
		return buf.String()
	case KindRange:
		return v.data.(Range).String()
	default:
		if RuntimeStringer != nil {
			if s, ok := RuntimeStringer(v); ok {
				return s
			}
		}
		return fmt.Sprintf("<%v>", v.kind)
	}
}

// FormatFloat renders a float the way Vibescript displays it, matching Ruby's
// Float#to_s. Finite values use Go's shortest round-trippable form, while the
// IEEE special values render as Ruby spells them ("Infinity", "-Infinity",
// "NaN") instead of Go's "+Inf"/"-Inf"/"NaN".
func FormatFloat(f float64) string {
	switch {
	case math.IsNaN(f):
		return "NaN"
	case math.IsInf(f, 1):
		return "Infinity"
	case math.IsInf(f, -1):
		return "-Infinity"
	default:
		return strconv.FormatFloat(f, 'g', -1, 64)
	}
}

// ErrStringRenderTruncated reports that a bounded rendering (StringBounded)
// stopped early because the formatted output would have exceeded the caller's
// byte budget. It lets host-facing rendering refuse to materialize an
// unbounded string for a large composite result instead of allocating until
// the process runs out of memory. Callers detect it with errors.Is.
var ErrStringRenderTruncated = errors.New("value: string rendering exceeded byte limit")

// ErrStringRenderDepthExceeded reports that StringBounded or InspectBounded
// stopped before descending beyond 16,384 nested composites. This limit applies
// independently of the byte budget, including when that budget is unbounded.
// Callers detect it with errors.Is; it is distinct from byte truncation.
var ErrStringRenderDepthExceeded = errors.New("value: string rendering exceeded nesting limit 16384")

const maxBoundedRenderDepth = 16384

// StringBounded renders v like String but stops once the formatted output
// would exceed limit bytes, returning the partial output and
// ErrStringRenderTruncated. A non-positive limit disables the byte budget.
// Regardless of limit, descent beyond 16,384 nested composites stops with
// partial output and ErrStringRenderDepthExceeded. Rendering writes directly
// into a single growing buffer and checks the budget after each element, so a
// hostile composite cannot
// allocate intermediate per-element strings or a final joined buffer larger
// than roughly limit plus one element before the limit trips. Cycle handling
// is identical to String.
func (v Value) StringBounded(limit int) (string, error) {
	switch v.kind {
	case KindArray, KindHash:
		var buf strings.Builder
		state := newBoundedValueStringState()
		if err := v.appendString(&buf, state, limit); err != nil {
			return buf.String(), err
		}
		return buf.String(), nil
	default:
		if limit <= 0 {
			return v.String(), nil
		}
		// A big integer whose rendering provably exceeds the budget is refused
		// before the (superlinear) base conversion runs; the partial output is
		// empty because no digits were ever materialized.
		if bigIntRenderExceedsLimit(v, limit) {
			return "", ErrStringRenderTruncated
		}
		if RuntimeStringAppender != nil {
			var buf strings.Builder
			if truncated, handled := RuntimeStringAppender(v, &buf, limit); handled {
				if truncated {
					return buf.String(), ErrStringRenderTruncated
				}
				return buf.String(), nil
			}
		}
		s := v.String()
		if len(s) > limit {
			return s[:limit], ErrStringRenderTruncated
		}
		return s, nil
	}
}

type valueStringState struct {
	arrays     map[SliceIdentity]struct{}
	maps       map[uintptr]struct{}
	depth      int
	depthLimit int
	// chargeBytes, when set, is invoked with each scalar payload's byte
	// length before the sizing walk scans it (escape counting reads the
	// whole payload), so a caller's byte budget can interrupt the sizing
	// pass instead of only being billed after it completes. nil leaves
	// sizing unmetered, as the rendering guards that charge separately
	// expect.
	chargeBytes func(bytes int) error
}

func newBoundedValueStringState() *valueStringState {
	state := newValueStringState()
	state.depthLimit = maxBoundedRenderDepth
	return state
}

func (s *valueStringState) enterComposite(buf *strings.Builder, limit int) error {
	if limit > 0 && buf.Len() >= limit {
		return ErrStringRenderTruncated
	}
	if s.depthLimit > 0 && s.depth >= s.depthLimit {
		return ErrStringRenderDepthExceeded
	}
	s.depth++
	return nil
}

func (s *valueStringState) leaveComposite() {
	s.depth--
}

func newValueStringState() *valueStringState {
	return &valueStringState{
		arrays: make(map[SliceIdentity]struct{}),
		maps:   make(map[uintptr]struct{}),
	}
}

// WriteStringTo streams the same bytes String would return for v directly into
// buf, without first materializing the rendered representation as a separate
// string. Callers that have already bounded the rendering against a quota (such
// as the sandbox's interpolation memory guard, which reserves the projected
// length before calling) use it to stream an aggregate straight into their
// builder instead of allocating the full rendering and then copying it, which
// would transiently hold both the temporary and the destination copy and could
// exceed a memory limit the projected length already passed. It delegates to the
// unified unbounded renderer, so writing into a strings.Builder never fails.
// It is intended for the interpreter's internal use; hosts should not call
// it, and it carries no compatibility promise (see
// docs/embedding-api-stability.md).
func (v Value) WriteStringTo(buf *strings.Builder) {
	_ = v.appendString(buf, newValueStringState(), 0)
}

// appendString streams v's rendering into buf instead of building intermediate
// per-element slices and a final joined string. When limit is positive, every
// write site routes through a bounded helper (appendBounded for strings,
// appendByteBounded for single delimiters), so the buffer never grows past the
// limit: the first write that would exceed the budget stops and returns
// ErrStringRenderTruncated. This keeps the StringBounded byte-budget contract
// intact for callers that consume the returned partial output, and large
// composites trip the budget rather than allocating without bound. A
// non-positive limit renders the whole value.
func (v Value) appendString(buf *strings.Builder, state *valueStringState, limit int) error {
	switch v.kind {
	case KindArray:
		elems := v.Array()
		id := SliceIdentity{
			Ptr: reflect.ValueOf(elems).Pointer(),
			Len: len(elems),
			Cap: cap(elems),
		}
		if id.Ptr != 0 {
			if _, seen := state.arrays[id]; seen {
				return appendBounded(buf, "<cycle>", limit)
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
		// The opening delimiter counts against the budget like any other byte, so
		// a nested composite whose parent already filled the cap trips the limit
		// here rather than emitting a result one or more bytes over the cap.
		if err := appendByteBounded(buf, '[', limit); err != nil {
			return err
		}
		for i, e := range elems {
			if i > 0 {
				// The element separator counts against the budget like any other
				// byte, so a packed array trips the limit on the separator rather
				// than emitting a result over the cap.
				if err := appendBounded(buf, ", ", limit); err != nil {
					return err
				}
			}
			if err := e.appendString(buf, state, limit); err != nil {
				return err
			}
		}
		// The closing delimiter still counts against the budget: an array that
		// fills the budget exactly with its elements must trip the limit rather
		// than emit a result one byte over the cap.
		return appendByteBounded(buf, ']', limit)
	case KindHash:
		entries := v.hashEntries()
		if len(entries) == 0 {
			return appendBounded(buf, "{}", limit)
		}
		ptr := reflect.ValueOf(entries).Pointer()
		if ptr != 0 {
			if _, seen := state.maps[ptr]; seen {
				return appendBounded(buf, "<cycle>", limit)
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
		// The opening delimiter counts against the budget like any other byte; see
		// the array opening delimiter above.
		if err := appendByteBounded(buf, '{', limit); err != nil {
			return err
		}
		first := true
		var keyBuf [smallHashIterationBuffer]Value
		for _, key := range v.hashIterationKeys(keyBuf[:]) {
			k := key.data.(string)
			val := entries[k]
			if !first {
				// The entry separator counts against the budget like any other
				// byte, so a packed hash trips the limit on the separator rather
				// than emitting a result over the cap.
				if err := appendBounded(buf, ", ", limit); err != nil {
					return err
				}
			}
			first = false
			// A hash key is an arbitrary string that may itself exceed the
			// budget (host-provided or generated under a raised memory quota),
			// so cap the key write to the remaining budget rather than copying
			// it whole before the value-level check runs.
			if err := appendBounded(buf, k, limit); err != nil {
				return err
			}
			// The key/value separator counts against the budget too: a key that
			// fills the budget exactly must trip the limit here rather than let
			// ": " push the result past the cap.
			if err := appendBounded(buf, ": ", limit); err != nil {
				return err
			}
			if err := val.appendString(buf, state, limit); err != nil {
				return err
			}
		}
		// See the array closing delimiter above: the trailing brace counts
		// against the budget too.
		return appendByteBounded(buf, '}', limit)
	default:
		// A scalar element may be an arbitrarily large string, so cap its write
		// to the remaining budget instead of materializing the whole value in
		// the buffer before checking the limit. A big integer that provably
		// cannot fit the remaining budget is refused before the (superlinear)
		// base conversion ever runs.
		// The hook streams straight into buf and honors the budget itself, so
		// neither an unbounded write nor a truncated one materializes the
		// whole rendering first.
		if RuntimeStringAppender != nil {
			if truncated, handled := RuntimeStringAppender(v, buf, limit); handled {
				if truncated {
					return ErrStringRenderTruncated
				}
				return nil
			}
		}
		if limit > 0 && bigIntRenderExceedsLimit(v, limit-buf.Len()) {
			return ErrStringRenderTruncated
		}
		return appendBounded(buf, v.String(), limit)
	}
}

// appendByteBounded writes a single delimiter byte into buf, but when limit is
// positive it refuses to write past the budget and reports
// ErrStringRenderTruncated instead. Delimiters count against the cap like any
// other byte, so a composite that fills its budget exactly with its contents
// must trip the limit rather than emit a result one byte over the cap.
func appendByteBounded(buf *strings.Builder, b byte, limit int) error {
	if limit > 0 && buf.Len() >= limit {
		return ErrStringRenderTruncated
	}
	buf.WriteByte(b)
	return nil
}

// appendBounded writes s into buf, but when limit is positive it copies only as
// many bytes as fit within the remaining budget and reports
// ErrStringRenderTruncated instead of materializing an arbitrarily large scalar
// (a long hash key or string element) in the buffer. A non-positive limit
// writes s in full and never reports truncation.
func appendBounded(buf *strings.Builder, s string, limit int) error {
	if limit <= 0 {
		buf.WriteString(s)
		return nil
	}
	remaining := max(limit-buf.Len(), 0)
	if len(s) > remaining {
		buf.WriteString(s[:remaining])
		return ErrStringRenderTruncated
	}
	buf.WriteString(s)
	return nil
}

// StringByteLen returns the number of bytes String would produce for v without
// materializing the rendered representation. Callers that must bound an
// allocation before it happens (such as the sandbox's interpolation memory
// guard) use it to reject an oversized rendering instead of building the string
// first and only then observing that it exceeded a quota. The byte count walks
// arrays and hashes with the same cycle detection String uses, so the projection
// matches the eventual output exactly.
// It is intended for the interpreter's internal use; hosts should not call
// it, and it carries no compatibility promise (see
// docs/embedding-api-stability.md).
func (v Value) StringByteLen() int {
	switch v.kind {
	case KindArray, KindHash:
		return v.stringByteLenWithState(newValueStringState())
	case KindRegex:
		// Sizing a regex must not render it: Regex.String escapes the source and
		// allocates the literal, so measuring performs the work being measured --
		// ahead of any charge the caller applies, and again when the value is
		// really rendered. StringLen walks the source instead. Handled here
		// rather than at each call site so a regex nested in an array or hash is
		// sized the same way.
		return v.data.(Regex).StringLen()
	default:
		if RuntimeStringLen != nil {
			if n, ok := RuntimeStringLen(v); ok {
				return n
			}
		}
		return len(v.String())
	}
}

// StringRuneLen returns the number of runes String would produce for v without
// materializing the rendered representation. It mirrors StringByteLen but counts
// display width in the unit fmt uses for string precision and padding.
// It is intended for the interpreter's internal use; hosts should not call
// it, and it carries no compatibility promise (see
// docs/embedding-api-stability.md).
func (v Value) StringRuneLen() int {
	switch v.kind {
	case KindArray, KindHash:
		return v.stringRuneLenWithState(newValueStringState())
	case KindRegex:
		// See StringByteLen: sizing a regex must not render it. No step callback
		// here, so the source walk is unbilled -- the unmetered path.
		return v.data.(Regex).StringRuneLen()
	default:
		if RuntimeStringRuneLen != nil {
			if n, ok := RuntimeStringRuneLen(v); ok {
				return n
			}
		}
		return utf8.RuneCountInString(v.String())
	}
}

func (v Value) stringByteLenWithState(state *valueStringState) int {
	switch v.kind {
	case KindArray:
		elems := v.Array()
		id := SliceIdentity{
			Ptr: reflect.ValueOf(elems).Pointer(),
			Len: len(elems),
			Cap: cap(elems),
		}
		if id.Ptr != 0 {
			if _, seen := state.arrays[id]; seen {
				return len(cycleMarker)
			}
			state.arrays[id] = struct{}{}
			defer delete(state.arrays, id)
		}
		// "[" + elements joined by ", " + "]".
		total := len(arrayOpen) + len(arrayClose)
		total += separatorBytes(len(elems))
		for _, e := range elems {
			total += e.stringByteLenWithState(state)
		}
		return total
	case KindHash:
		entries := v.hashEntries()
		if len(entries) == 0 {
			return len(hashOpen) + len(hashClose)
		}
		ptr := reflect.ValueOf(entries).Pointer()
		if ptr != 0 {
			if _, seen := state.maps[ptr]; seen {
				return len(cycleMarker)
			}
			state.maps[ptr] = struct{}{}
			defer delete(state.maps, ptr)
		}
		// "{" + entries joined by ", " + "}"; each entry is key + ": " + value.
		total := len(hashOpen) + len(hashClose)
		total += separatorBytes(len(entries))
		for k, val := range entries {
			total += len(k) + len(keyValueSeparator)
			total += val.stringByteLenWithState(state)
		}
		return total
	case KindRegex:
		// See StringByteLen: sizing a regex must not render it. This walker takes
		// no step callback, so the source walk is unbilled here -- it is the
		// unmetered path, used where no quota is in force.
		return v.data.(Regex).StringLen()
	default:
		if RuntimeStringLen != nil {
			if n, ok := RuntimeStringLen(v); ok {
				return n
			}
		}
		return len(v.String())
	}
}

func (v Value) stringRuneLenWithState(state *valueStringState) int {
	switch v.kind {
	case KindArray:
		elems := v.Array()
		id := SliceIdentity{
			Ptr: reflect.ValueOf(elems).Pointer(),
			Len: len(elems),
			Cap: cap(elems),
		}
		if id.Ptr != 0 {
			if _, seen := state.arrays[id]; seen {
				return len(cycleMarker)
			}
			state.arrays[id] = struct{}{}
			defer delete(state.arrays, id)
		}
		total := len(arrayOpen) + len(arrayClose)
		total += separatorBytes(len(elems))
		for _, e := range elems {
			total += e.stringRuneLenWithState(state)
		}
		return total
	case KindHash:
		entries := v.hashEntries()
		if len(entries) == 0 {
			return len(hashOpen) + len(hashClose)
		}
		ptr := reflect.ValueOf(entries).Pointer()
		if ptr != 0 {
			if _, seen := state.maps[ptr]; seen {
				return len(cycleMarker)
			}
			state.maps[ptr] = struct{}{}
			defer delete(state.maps, ptr)
		}
		total := len(hashOpen) + len(hashClose)
		total += separatorBytes(len(entries))
		for k, val := range entries {
			total += utf8.RuneCountInString(k) + len(keyValueSeparator)
			total += val.stringRuneLenWithState(state)
		}
		return total
	case KindRegex:
		// See StringByteLen: sizing a regex must not render it. No step callback
		// here, so the source walk is unbilled -- the unmetered path.
		return v.data.(Regex).StringRuneLen()
	default:
		if RuntimeStringRuneLen != nil {
			if n, ok := RuntimeStringRuneLen(v); ok {
				return n
			}
		}
		return utf8.RuneCountInString(v.String())
	}
}

// StringByteLenBounded reports the same byte count as StringByteLen but invokes
// step once per node visited during the projection walk, so a caller can charge
// a sandbox step budget against the traversal and abort it when step returns an
// error. The first error step reports stops the walk and is returned unchanged
// alongside the partial count.
//
// StringByteLen's cycle detection only collapses references that are currently
// on the recursion stack: a shared but acyclic graph (for example the result of
// repeatedly evaluating a = [a, a], where each level holds two references to the
// same child slice) is fully re-walked at every occurrence, so the traversal is
// exponential in the nesting depth even though the value's memory and its
// eventual rendering both stay bounded by the cycle marker. The memory quota
// alone cannot bound that work because it is checked only after the walk
// completes. Driving step from inside the walk lets the step quota trip during
// the traversal instead of letting it run unbounded.
// It is intended for the interpreter's internal use; hosts should not call
// it, and it carries no compatibility promise (see
// docs/embedding-api-stability.md).
func (v Value) StringByteLenBounded(step func() error) (int, error) {
	switch v.kind {
	case KindArray, KindHash:
		return v.stringByteLenBoundedWithState(newValueStringState(), step)
	case KindRegex:
		// See StringByteLen: sizing a regex must not render it, and the source
		// walk is charged before it runs. array.join reaches this public entry
		// point rather than the recursive walker, so the charge belongs on both.
		if err := step(); err != nil {
			return 0, err
		}
		if err := chargeRegexSourceSteps(v, step); err != nil {
			return 0, err
		}
		return v.data.(Regex).StringLen(), nil
	default:
		if err := step(); err != nil {
			return 0, err
		}
		// A big integer's projection performs the same superlinear base
		// conversion the rendering will; charge steps for it up front so the
		// step quota trips before the conversion runs.
		if err := chargeBigIntRenderSteps(v, step); err != nil {
			return 0, err
		}
		if RuntimeStringLen != nil {
			if n, ok := RuntimeStringLen(v); ok {
				return n, nil
			}
		}
		return len(v.String()), nil
	}
}

// StringRuneLenBounded reports the same rune count as StringRuneLen but invokes
// step once per node visited during the projection walk, matching
// StringByteLenBounded's sandbox accounting.
// It is intended for the interpreter's internal use; hosts should not call
// it, and it carries no compatibility promise (see
// docs/embedding-api-stability.md).
func (v Value) StringRuneLenBounded(step func() error) (int, error) {
	switch v.kind {
	case KindArray, KindHash:
		return v.stringRuneLenBoundedWithState(newValueStringState(), step)
	case KindRegex:
		// See StringByteLen: sizing a regex must not render it, and the source
		// walk is charged before it runs. The per-node step comes first and
		// unconditionally: a source under one step's worth of bytes charges
		// nothing proportional, and without this an exhausted callback could not
		// abort the projection and a regex would cost one step less than every
		// other kind.
		if err := step(); err != nil {
			return 0, err
		}
		if err := chargeRegexSourceSteps(v, step); err != nil {
			return 0, err
		}
		return v.data.(Regex).StringRuneLen(), nil
	default:
		if err := step(); err != nil {
			return 0, err
		}
		if err := chargeBigIntRenderSteps(v, step); err != nil {
			return 0, err
		}
		if RuntimeStringRuneLen != nil {
			if n, ok := RuntimeStringRuneLen(v); ok {
				return n, nil
			}
		}
		return utf8.RuneCountInString(v.String()), nil
	}
}

// StringByteLenBoundedUpTo reports String's byte length up to limit bytes. It
// stops as soon as it can prove the rendering would exceed limit, returning
// truncated as true and limit+1 as the count. Like StringByteLenBounded, it
// invokes step during aggregate walks so callers can charge sandbox work before
// materializing any rendering.
// It is intended for the interpreter's internal use; hosts should not call
// it, and it carries no compatibility promise (see
// docs/embedding-api-stability.md).
func (v Value) StringByteLenBoundedUpTo(limit int, step func() error) (count int, truncated bool, err error) {
	if limit < 0 {
		n, err := v.StringByteLenBounded(step)
		return n, false, err
	}
	return v.stringByteLenBoundedUpToWithState(newValueStringState(), limit, step)
}

func (v Value) stringByteLenBoundedWithState(state *valueStringState, step func() error) (int, error) {
	if err := step(); err != nil {
		return 0, err
	}
	switch v.kind {
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
		// "[" + elements joined by ", " + "]".
		total := len(arrayOpen) + len(arrayClose)
		total += separatorBytes(len(elems))
		for _, e := range elems {
			n, err := e.stringByteLenBoundedWithState(state, step)
			if err != nil {
				return 0, err
			}
			total += n
		}
		return total, nil
	case KindHash:
		entries := v.hashEntries()
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
		// "{" + entries joined by ", " + "}"; each entry is key + ": " + value.
		total := len(hashOpen) + len(hashClose)
		total += separatorBytes(len(entries))
		for k, val := range entries {
			total += len(k) + len(keyValueSeparator)
			n, err := val.stringByteLenBoundedWithState(state, step)
			if err != nil {
				return 0, err
			}
			total += n
		}
		return total, nil
	case KindRegex:
		// See StringByteLen: sizing a regex must not render it, and the source
		// walk StringLen performs is charged before it runs.
		if err := chargeRegexSourceSteps(v, step); err != nil {
			return 0, err
		}
		return v.data.(Regex).StringLen(), nil
	default:
		if err := chargeBigIntRenderSteps(v, step); err != nil {
			return 0, err
		}
		if RuntimeStringLen != nil {
			if n, ok := RuntimeStringLen(v); ok {
				return n, nil
			}
		}
		return len(v.String()), nil
	}
}

func (v Value) stringRuneLenBoundedWithState(state *valueStringState, step func() error) (int, error) {
	if err := step(); err != nil {
		return 0, err
	}
	switch v.kind {
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
			n, err := e.stringRuneLenBoundedWithState(state, step)
			if err != nil {
				return 0, err
			}
			total += n
		}
		return total, nil
	case KindHash:
		entries := v.hashEntries()
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
			total += utf8.RuneCountInString(k) + len(keyValueSeparator)
			n, err := val.stringRuneLenBoundedWithState(state, step)
			if err != nil {
				return 0, err
			}
			total += n
		}
		return total, nil
	case KindRegex:
		// See StringByteLen: sizing a regex must not render it, and the source
		// walk is charged before it runs. No per-node step here: this walker
		// already charged one at its entry, and adding another billed the same
		// node twice, making a nested regex cost a step more than a nested
		// scalar. The public entry points do need their own, having none.
		if err := chargeRegexSourceSteps(v, step); err != nil {
			return 0, err
		}
		return v.data.(Regex).StringRuneLen(), nil
	default:
		if err := chargeBigIntRenderSteps(v, step); err != nil {
			return 0, err
		}
		if RuntimeStringRuneLen != nil {
			if n, ok := RuntimeStringRuneLen(v); ok {
				return n, nil
			}
		}
		return utf8.RuneCountInString(v.String()), nil
	}
}

func (v Value) stringByteLenBoundedUpToWithState(state *valueStringState, limit int, step func() error) (int, bool, error) {
	if err := step(); err != nil {
		return 0, false, err
	}
	switch v.kind {
	case KindArray:
		elems := v.Array()
		id := SliceIdentity{
			Ptr: reflect.ValueOf(elems).Pointer(),
			Len: len(elems),
			Cap: cap(elems),
		}
		if id.Ptr != 0 {
			if _, seen := state.arrays[id]; seen {
				total, truncated := stringByteLenCappedAdd(0, len(cycleMarker), limit)
				return total, truncated, nil
			}
			state.arrays[id] = struct{}{}
			defer delete(state.arrays, id)
		}
		total, truncated := stringByteLenCappedAdd(0, len(arrayOpen)+len(arrayClose)+separatorBytes(len(elems)), limit)
		if truncated {
			return total, true, nil
		}
		for _, e := range elems {
			n, childTruncated, err := e.stringByteLenBoundedUpToWithState(state, limit-total, step)
			if err != nil {
				return 0, false, err
			}
			var addTruncated bool
			total, addTruncated = stringByteLenCappedAdd(total, n, limit)
			if childTruncated || addTruncated {
				return total, true, nil
			}
		}
		return total, false, nil
	case KindHash:
		entries := v.hashEntries()
		if len(entries) == 0 {
			total, truncated := stringByteLenCappedAdd(0, len(hashOpen)+len(hashClose), limit)
			return total, truncated, nil
		}
		ptr := reflect.ValueOf(entries).Pointer()
		if ptr != 0 {
			if _, seen := state.maps[ptr]; seen {
				total, truncated := stringByteLenCappedAdd(0, len(cycleMarker), limit)
				return total, truncated, nil
			}
			state.maps[ptr] = struct{}{}
			defer delete(state.maps, ptr)
		}
		total, truncated := stringByteLenCappedAdd(0, len(hashOpen)+len(hashClose)+separatorBytes(len(entries)), limit)
		if truncated {
			return total, true, nil
		}
		for k, val := range entries {
			var keyTruncated bool
			total, keyTruncated = stringByteLenCappedAdd(total, len(k)+len(keyValueSeparator), limit)
			if keyTruncated {
				return total, true, nil
			}
			n, childTruncated, err := val.stringByteLenBoundedUpToWithState(state, limit-total, step)
			if err != nil {
				return 0, false, err
			}
			var addTruncated bool
			total, addTruncated = stringByteLenCappedAdd(total, n, limit)
			if childTruncated || addTruncated {
				return total, true, nil
			}
		}
		return total, false, nil
	case KindRegex:
		// See StringByteLen: sizing a regex must not render it, and the source
		// walk StringLen performs is charged before it runs.
		if err := chargeRegexSourceSteps(v, step); err != nil {
			return 0, false, err
		}
		total, truncated := stringByteLenCappedAdd(0, v.data.(Regex).StringLen(), limit)
		return total, truncated, nil
	default:
		// A big integer that provably exceeds the limit reports truncation
		// without paying for the base conversion; anything else is measured
		// exactly after charging steps for the conversion work.
		if bigIntRenderExceedsLimit(v, limit) {
			return limit + 1, true, nil
		}
		if err := chargeBigIntRenderSteps(v, step); err != nil {
			return 0, false, err
		}
		if RuntimeStringLen != nil {
			if n, ok := RuntimeStringLen(v); ok {
				total, truncated := stringByteLenCappedAdd(0, n, limit)
				return total, truncated, nil
			}
		}
		total, truncated := stringByteLenCappedAdd(0, len(v.String()), limit)
		return total, truncated, nil
	}
}

func stringByteLenCappedAdd(total, n, limit int) (int, bool) {
	if n > limit-total {
		return limit + 1, true
	}
	return total + n, false
}

// separatorBytes returns the bytes the ", " separators contribute when joining
// count elements: zero for fewer than two elements, otherwise two bytes per gap.
func separatorBytes(count int) int {
	if count < 2 {
		return 0
	}
	return (count - 1) * len(elementSeparator)
}

const (
	arrayOpen         = "["
	arrayClose        = "]"
	hashOpen          = "{"
	hashClose         = "}"
	elementSeparator  = ", "
	keyValueSeparator = ": "
	cycleMarker       = "<cycle>"
)

// Truthy reports whether v is considered true in a boolean context.
func (v Value) Truthy() bool {
	switch v.kind {
	case KindNil:
		return false
	case KindBool:
		return v.Bool()
	default:
		return true
	}
}

// Eql reports whether v and other are equal under hash-key semantics: they
// must share the same kind and compare equal, so an Int never eql-matches a
// Float even when their numeric values coincide. It backs the Ruby-style
// `eql?` predicate. Because Equal already requires matching kinds (Vibescript
// `==` performs no cross-kind numeric coercion), Eql currently coincides with
// Equal; it exists as a distinct, documented contract aligned with hash-key
// equality rather than with broad value equivalence.
func (v Value) Eql(other Value) bool {
	if v.kind != other.kind {
		return false
	}
	var ctx EqualityContext
	return ctx.Eql(v, other)
}

// Identical reports whether v and other are the same value, backing the
// Ruby-style `equal?` predicate.
//
// Immutable scalars (nil, bool, int, float, string, symbol, money, duration,
// time, range) are identical when they share kind and value. Integers outside
// the int64 range are the exception: they are heap objects and compare by
// payload identity, matching Ruby, where bignums are separate objects.
//
// Arrays, hashes, and objects are values (ADR-006 item 2): Identical asks
// about contents, the same question as Equal, because collections carry no
// identity. Two independently constructed arrays with the same elements are
// identical, and every empty array is identical to every other empty array
// (the same for hashes and objects).
//
// Runtime-only kinds (function, builtin, block, class, instance) and enum
// values that hold distinct storage are not identical even when they compare
// Equal — for example an enum value cloned out to the host and handed back.
//
// NaN floats are the one immutable case where value equality is not enough:
// IEEE NaN != NaN, so deferring to Equal would make x.equal?(x) false for a NaN
// receiver and break reflexivity. Identity treats any two NaN floats as
// identical, keeping equal? reflexive while matching the value-identity model
// floats already follow.
func (v Value) Identical(other Value) bool {
	if v.kind != other.kind {
		return false
	}
	switch v.kind {
	case KindInt:
		// Compact integers keep value identity like every immutable scalar. A
		// big integer is a heap object, so identity is the payload pointer:
		// two independently produced big values with equal contents are equal
		// but not identical, matching Ruby, where bignums are separate objects
		// while fixnums are value-identical. A mixed pair is never identical
		// (the canonical invariant keeps the value spaces disjoint).
		vBig, vOK := v.data.(*big.Int)
		oBig, oOK := other.data.(*big.Int)
		if vOK || oOK {
			return vOK && oOK && vBig == oBig
		}
		return v.Int() == other.Int()
	case KindFloat:
		// Float identity is value identity in Vibescript: the language exposes no
		// distinct object for two floats with the same value, so 1.5.equal?(1.5)
		// is true. IEEE equality would break the identity contract's reflexivity
		// for NaN (NaN == NaN is false), so treat any two NaN floats as identical;
		// every other float defers to value equality.
		if math.IsNaN(v.Float()) || math.IsNaN(other.Float()) {
			return math.IsNaN(v.Float()) && math.IsNaN(other.Float())
		}
		return v.Float() == other.Float()
	case KindArray, KindHash, KindObject:
		// Collections are values (ADR-006 item 2), so they have no identity
		// beyond their contents: two arrays with the same elements are the same
		// value, and there is no operation that could tell them apart. Whether
		// the runtime happens to have them naming one wrapper is a
		// representation detail copy-on-write may change at any write, so
		// reporting it would make equal? answer a question about storage rather
		// than about the language. Deferring to value equality is the same
		// choice floats and strings already make.
		//
		// A tagged attribute bag is not exempt: the tag governs rendering and
		// mutation, not identity, and two bags with the same entries are the
		// same value however each was built.
		return v.Equal(other)
	case KindFunction, KindBuiltin, KindBlock, KindClass, KindInstance:
		// These runtime kinds already compare by backing-pointer identity in
		// Equal (via RuntimeEqualer), which is exactly the identity contract
		// equal? wants, so delegating keeps the two predicates consistent.
		return v.Equal(other)
	case KindEnum, KindEnumValue:
		// Enum and enum-value Equal is structural: two independently cloned
		// members that share an owner script and name compare Equal even though
		// they hold distinct backing storage (for example, a value cloned out to
		// the host and returned by a capability). equal? must report identity,
		// so consult RuntimeIdenticaler, which compares backing pointers.
		if RuntimeIdenticaler != nil {
			if result, ok := RuntimeIdenticaler(v, other); ok {
				return result
			}
		}
		return v.Equal(other)
	default:
		return v.Equal(other)
	}
}

// EqualityContext reuses the cycle-detection scratch used by Value equality
// and optionally carries a byte charge for the string payloads a comparison
// reads (see SetCharge). The zero value is ready to use and compares
// unmetered. It is not safe for concurrent use. It is intended for the
// interpreter's internal use; hosts should not rely on it, and it carries no
// compatibility promise (see docs/embedding-api-stability.md).
type EqualityContext struct {
	state equalityState
}

// equalityState threads the cycle-detection scratch, the optional byte
// charge, and the first charge failure through a recursive equality walk.
// The seen set inserts and never deletes within one comparison, so each
// distinct composite pair is walked — and its scalar payloads charged — once,
// which keeps shared DAGs linear with or without metering.
type equalityState struct {
	seen map[valueEqualityPair]struct{}
	// charge bills the bytes a scalar comparison is about to read. nil means
	// unmetered: hosts, tests, and Value.Equal keep their existing behavior.
	charge func(bytes int) error
	// reserveScratch validates the walk's cumulative transient scratch — the
	// key slices deterministic traversal sorts and the display renderings —
	// against the caller's memory budget before each allocation, together
	// with the top-level operands of the active comparison: the operands can
	// be temporaries no other root reaches, and the scratch coexists with
	// both compared graphs at its peak. nil means unvalidated.
	reserveScratch func(bytes int, left, right Value) error
	// rootLeft and rootRight are the active comparison's top-level operands,
	// handed to reserveScratch with every validation.
	rootLeft  Value
	rootRight Value
	// releaseScratch tells the validator that the walk has finished with its
	// scratch, so a validator holding an accounting reservation against it
	// retires that reservation rather than carrying it into unrelated work.
	// nil means the validator keeps no such reservation.
	releaseScratch func()
	// roundScratchAlloc maps a requested scratch allocation to the capacity
	// the allocator actually reserves for it (size-class rounding), so a
	// rendered display key is validated at its realized backing size. nil
	// reserves requests at their exact size.
	roundScratchAlloc func(bytes int) int
	// scratchHeld accumulates the scratch bytes this walk has allocated, so
	// nested map traversals validate their combined footprint.
	scratchHeld int
	// pendingCharge accumulates leaf bytes not yet handed to charge. The
	// walk flushes in batches and Equal/Eql settle the tail, so a caller
	// whose charge function rounds each invocation down (the runtime bills
	// whole 64-byte steps) cannot be walked past with many sub-granularity
	// reads: the aggregate is billed even when every leaf alone is free.
	pendingCharge int
	// err records the first charge failure. It is sticky: every comparison
	// after it returns false immediately, so a caller looping over many Equal
	// calls does O(1) work per call once the quota is gone and surfaces the
	// error via Err.
	err error
}

// Equal reports whether v and other hold the same kind and value.
func (v Value) Equal(other Value) bool {
	var ctx EqualityContext
	return ctx.Equal(v, other)
}

// equalitySeenRetainEntries bounds the traversal-map capacity a context keeps
// between comparisons. The map is cleared per comparison, but clearing keeps
// its buckets: a context that outlives one walk (the runtime pools one per
// execution and embeds them in set helpers) would otherwise retain an
// arbitrarily large backing from a single deep comparison, invisible to any
// memory accounting.
const equalitySeenRetainEntries = 64

// releaseOversizedSeen drops the cycle-detection map when the walk that just
// finished grew it past the pooling threshold, so a reused context retains at
// most a few kilobytes of traversal scratch.
func releaseOversizedSeen(state *equalityState) {
	if len(state.seen) > equalitySeenRetainEntries {
		state.seen = nil
	}
}

// beginWalkScratch opens one comparison's scratch accounting and reports the
// boundary to the validator as a zero-byte reservation. Scratch from prior
// walks on a reused context is dead, so the validator must see each walk's own
// footprint rather than a scan's lifetime total. The boundary has to be said
// rather than inferred: a validator that batches its checks by cumulative
// bytes cannot recognize a new walk from the byte count alone, because a
// walk's first reservation is routinely equal to — or a little above — the
// total the previous walk already accounted for, which is precisely the case
// where every later comparison would otherwise go unvalidated.
func beginWalkScratch(state *equalityState, left, right Value) {
	state.scratchHeld = 0
	state.rootLeft, state.rootRight = left, right
	if state.reserveScratch == nil {
		return
	}
	if err := state.reserveScratch(0, left, right); err != nil && state.err == nil {
		state.err = err
	}
}

// endWalkScratch closes a comparison's scratch accounting. The walk's slices
// are unreachable once it returns, so anything a validator reserved against
// them is retired here even though the comparison answered without error.
func endWalkScratch(state *equalityState) {
	state.scratchHeld = 0
	if state.releaseScratch != nil {
		state.releaseScratch()
	}
}

// Equal reports whether v and other hold the same kind and value.
func (c *EqualityContext) Equal(v, other Value) bool {
	if c.state.seen != nil {
		clear(c.state.seen)
	}
	beginWalkScratch(&c.state, v, other)
	eq := valuesEqual(v, other, &c.state)
	flushEqualityCharge(&c.state)
	endWalkScratch(&c.state)
	// The operand roots are read only by the scratch validator during the
	// walk. A context that outlives the comparison (the runtime pools one
	// per execution and embeds them in set helpers) must not keep the
	// compared graphs reachable past its answer.
	c.state.rootLeft, c.state.rootRight = Value{}, Value{}
	releaseOversizedSeen(&c.state)
	if c.state.err != nil {
		return false
	}
	return eq
}

// Eql reports whether v and other are equal under hash-key semantics: kinds
// must match at every level (see Value.Eql). It shares the context's scratch,
// charge hook, and sticky error with Equal.
func (c *EqualityContext) Eql(v, other Value) bool {
	if c.state.seen != nil {
		clear(c.state.seen)
	}
	beginWalkScratch(&c.state, v, other)
	eq := valuesEqualWithKinds(v, other, &c.state, true)
	flushEqualityCharge(&c.state)
	endWalkScratch(&c.state)
	// See Equal: reused contexts must not retain the compared graphs.
	c.state.rootLeft, c.state.rootRight = Value{}, Value{}
	releaseOversizedSeen(&c.state)
	if c.state.err != nil {
		return false
	}
	return eq
}

// SetScratchReserver installs a validator for the walk's transient scratch
// allocations (the key slices deterministic map traversal sorts and the
// rendered display keys): it is invoked with the walk's cumulative scratch
// bytes and the active comparison's top-level operands before each
// allocation, and an error aborts the comparison like a charge failure. The
// operands accompany every validation because they can be temporaries no
// other root reaches, and the scratch coexists with both compared graphs at
// its peak. A nil reserver leaves allocations unvalidated.
func (c *EqualityContext) SetScratchReserver(reserve func(bytes int, left, right Value) error) {
	c.state.reserveScratch = reserve
}

// SetScratchAllocRounder installs the allocator projection applied when the
// walk reserves an allocation of known size — the rendered display key of a
// composite hash key: round receives the requested byte size and returns the
// capacity the allocator will actually reserve for it. Reserving the
// unrounded request would let a budget that sits between the request and the
// realized size-class capacity admit an allocation exceeding it. A nil
// rounder reserves requests at their exact size.
func (c *EqualityContext) SetScratchAllocRounder(round func(bytes int) int) {
	c.state.roundScratchAlloc = round
}

// SetScratchReleaser installs a callback invoked when a comparison finishes
// with its transient scratch. A validator that only inspects the total it is
// handed needs nothing here. One that holds an accounting reservation against
// the walk's scratch — so that memory checks elsewhere account for it while
// the walk runs — retires that reservation here, since the slices are
// unreachable once the comparison returns. It is separate from the reserver so
// that the reserved figure stays a plain byte count rather than carrying a
// sentinel for the walk's lifecycle.
func (c *EqualityContext) SetScratchReleaser(release func()) {
	c.state.releaseScratch = release
}

// SetCharge installs a byte charge invoked for the string and symbol payloads
// a comparison reads: scalar leaves that pass the length screen, and
// string-like hash keys whose text is hashed or compared. A charge failure
// makes the current and every subsequent comparison on this context answer
// false; callers observe the failure through Err. A nil charge restores
// unmetered comparison.
func (c *EqualityContext) SetCharge(charge func(bytes int) error) {
	c.state.charge = charge
}

// Err returns the first charge failure recorded on this context, or nil. Once
// non-nil, comparison answers from this context are meaningless and the
// caller must surface the error instead.
func (c *EqualityContext) Err() error {
	return c.state.err
}

type valueEqualityPair struct {
	kind     ValueKind
	leftPtr  uintptr
	rightPtr uintptr
	leftLen  int
	rightLen int
}

// numericCrossKindEqual compares an int against a float exactly.
//
// The comparison runs through big.Float rather than converting the integer to
// float64, because a float64 cannot represent every int64 above 2^53 and the
// conversion would make distinct integers compare equal to the same float.
// NaN equals nothing, and neither infinity equals any integer.
func numericCrossKindEqual(v, other Value) bool {
	var intVal, floatVal Value
	switch {
	case v.kind == KindInt && other.kind == KindFloat:
		intVal, floatVal = v, other
	case v.kind == KindFloat && other.kind == KindInt:
		intVal, floatVal = other, v
	default:
		return false
	}

	f := floatVal.Float()
	if math.IsNaN(f) || math.IsInf(f, 0) {
		return false
	}
	if f != math.Trunc(f) {
		// A float with a fractional part cannot equal an integer.
		return false
	}

	exactFloat := new(big.Float).SetFloat64(f)
	if bi, ok := intVal.data.(*big.Int); ok {
		return new(big.Float).SetInt(bi).Cmp(exactFloat) == 0
	}
	return new(big.Float).SetInt64(intVal.Int()).Cmp(exactFloat) == 0
}

func valuesEqual(v, other Value, state *equalityState) bool {
	return valuesEqualWithKinds(v, other, state, false)
}

// equalityChargeBatchBytes is the flush threshold for accumulated leaf
// charges: large enough to amortize the charge call, small enough that a
// spent quota stops the walk within one batch.
const equalityChargeBatchBytes = 4096

// chargeEqualityBytes accumulates n bytes of read work and invokes the
// installed charge in batches. Charging each leaf separately let a
// per-invocation rounding in the charge function (the runtime bills whole
// 64-byte steps) scan arbitrarily many short payloads for free; batching
// preserves the sub-granularity remainders across the walk. A failure is
// sticky, like a direct charge's.
func chargeEqualityBytes(state *equalityState, n int) bool {
	if state.charge == nil || n <= 0 {
		return true
	}
	state.pendingCharge += n
	if state.pendingCharge < equalityChargeBatchBytes {
		return true
	}
	pending := state.pendingCharge
	state.pendingCharge = 0
	if err := state.charge(pending); err != nil {
		state.err = err
		return false
	}
	return true
}

// flushEqualityCharge settles the pending tail at the end of a top-level
// comparison, so every call's charged total is byte-exact.
func flushEqualityCharge(state *equalityState) {
	pending := state.pendingCharge
	state.pendingCharge = 0
	if state.charge == nil || pending <= 0 || state.err != nil {
		return
	}
	if err := state.charge(pending); err != nil {
		state.err = err
	}
}

func valuesEqualWithKinds(v, other Value, state *equalityState, strictKinds bool) bool {
	if state.err != nil {
		return false
	}
	if v.kind != other.kind {
		// eql? is kind-strict at every level, not only at the outermost one:
		// checking the kinds once and then delegating here made
		// [1].eql?([1.0]) true, because the elements took the numeric path.
		if strictKinds {
			return false
		}
		// An int and a float compare numerically, so 1 == 1.0 holds, and that
		// has to hold wherever the pair appears -- including nested, so
		// [1] == [1.0] is true. It also matches <=>, which already reports
		// 1 <=> 1.0 as 0.
		return numericCrossKindEqual(v, other)
	}
	switch v.kind {
	case KindNil:
		return true
	case KindBool:
		return v.Bool() == other.Bool()
	case KindInt:
		if v.data != nil || other.data != nil {
			vBig, vOK := v.data.(*big.Int)
			oBig, oOK := other.data.(*big.Int)
			if vOK || oOK {
				// The canonical invariant keeps the compact and big value
				// spaces disjoint (a big payload never fits int64), so a mixed
				// pair is never equal and two big payloads compare exactly.
				return vOK && oOK && vBig.Cmp(oBig) == 0
			}
		}
		return v.Int() == other.Int()
	case KindFloat:
		return v.Float() == other.Float()
	case KindString, KindSymbol:
		left := v.data.(string)
		right := other.data.(string)
		// A length mismatch answers without reading either payload, so only
		// equal-length pairs are charged — the same rule the operator-level
		// charge applies, and the same work Go's == performs.
		if len(left) != len(right) {
			return false
		}
		if !chargeEqualityBytes(state, len(left)) {
			return false
		}
		return left == right
	case KindMoney:
		return v.data.(Money) == other.data.(Money)
	case KindDuration:
		return v.Duration() == other.Duration()
	case KindTime:
		return v.data.(time.Time).Equal(other.data.(time.Time))
	case KindRange:
		return v.data.(Range) == other.data.(Range)
	case KindRegex:
		// Two regex values are equal when they were written the same way:
		// same pattern source and same flags, mirroring Ruby's Regexp#==.
		// The compiled program is derived from those and does not participate.
		// The source comparison reads its bytes like a string leaf, with the
		// same length screen.
		left := v.data.(Regex)
		right := other.data.(Regex)
		if len(left.Source) != len(right.Source) {
			return false
		}
		if !chargeEqualityBytes(state, len(left.Source)) {
			return false
		}
		if left.Source != right.Source {
			return false
		}
		// Flags is an exported, unrestricted string a host can size at
		// will; its comparison reads it like the source, under the same
		// length screen.
		if len(left.Flags) != len(right.Flags) {
			return false
		}
		if !chargeEqualityBytes(state, len(left.Flags)) {
			return false
		}
		return left.Flags == right.Flags
	case KindArray:
		left := v.Array()
		right := other.Array()
		if len(left) != len(right) {
			return false
		}
		leftID := SliceIdentity{
			Ptr: reflect.ValueOf(left).Pointer(),
			Len: len(left),
			Cap: cap(left),
		}
		rightID := SliceIdentity{
			Ptr: reflect.ValueOf(right).Pointer(),
			Len: len(right),
			Cap: cap(right),
		}
		if leftID.Ptr != 0 && leftID == rightID {
			return true
		}
		pair := valueEqualityPair{
			kind:     KindArray,
			leftPtr:  leftID.Ptr,
			rightPtr: rightID.Ptr,
			leftLen:  len(left),
			rightLen: len(right),
		}
		if pair.leftPtr != 0 || pair.rightPtr != 0 {
			if equalityPairSeen(state, pair) {
				return true
			}
		}
		for i := range left {
			if !valuesEqualWithKinds(left[i], right[i], state, strictKinds) {
				return false
			}
		}
		return true
	case KindHash:
		leftLen := v.HashLen()
		rightLen := other.HashLen()
		if leftLen != rightLen {
			return false
		}
		leftPtr := HashIdentity(v)
		rightPtr := HashIdentity(other)
		if leftPtr != 0 && leftPtr == rightPtr {
			return true
		}
		pair := valueEqualityPair{
			kind:     v.kind,
			leftPtr:  leftPtr,
			rightPtr: rightPtr,
			leftLen:  leftLen,
			rightLen: rightLen,
		}
		if pair.leftPtr != 0 || pair.rightPtr != 0 {
			if equalityPairSeen(state, pair) {
				return true
			}
		}
		// Hash keys live in one string keyspace, so two hashes compare as the
		// string-keyed maps they are, exactly like objects below.
		return hashMapsEqual(v.HashEntryMap(), other.HashEntryMap(), state, strictKinds)
	case KindObject:
		left := v.HashEntryMap()
		right := other.HashEntryMap()
		if len(left) != len(right) {
			return false
		}
		leftPtr := reflect.ValueOf(left).Pointer()
		rightPtr := reflect.ValueOf(right).Pointer()
		if leftPtr != 0 && leftPtr == rightPtr {
			return true
		}
		pair := valueEqualityPair{
			kind:     v.kind,
			leftPtr:  leftPtr,
			rightPtr: rightPtr,
			leftLen:  len(left),
			rightLen: len(right),
		}
		if pair.leftPtr != 0 || pair.rightPtr != 0 {
			if equalityPairSeen(state, pair) {
				return true
			}
		}
		if state.charge == nil {
			for key, leftValue := range left {
				rightValue, ok := right[key]
				if !ok {
					return false
				}
				if !valuesEqualWithKinds(leftValue, rightValue, state, strictKinds) {
					return false
				}
			}
			return true
		}
		keys, chargeOK := meteredMapKeys(left, state)
		if !chargeOK {
			return false
		}
		defer releaseKeySortScratch(state, len(keys))
		for _, key := range keys {
			rightValue, ok := right[key]
			if !ok {
				return false
			}
			if !valuesEqualWithKinds(left[key], rightValue, state, strictKinds) {
				return false
			}
		}
		return true
	default:
		if RuntimeEqualer != nil {
			if result, ok := RuntimeEqualer(v, other); ok {
				return result
			}
		}
		return reflect.DeepEqual(v.data, other.data)
	}
}

// keySortScratchEntryBytes approximates one entry of the key-sorting scratch
// slice: a string header plus slice-slot overhead.
const keySortScratchEntryBytes = 24

// releaseKeySortScratch retires a completed map traversal's contribution to
// the walk's live scratch accounting: a sibling map compared later must be
// validated against the slices actually alive, not every slice the walk ever
// allocated.
func releaseKeySortScratch(state *equalityState, entries int) {
	state.scratchHeld -= entries * keySortScratchEntryBytes
	if state.scratchHeld < 0 {
		state.scratchHeld = 0
	}
}

// equalitySortAbort sentinels the panic that stops a metered key sort once
// its quota charge fails: slices.SortFunc has no error path, and finishing
// the sort after the quota fired would be O(n log n) post-quota comparisons
// whose order the caller discards with the sticky error anyway.
type equalitySortAbort struct{}

// sortedMapKeys returns a map's keys in sorted order, validating the scratch
// slice against the caller's budget first. Metered equality must traverse
// deterministically: with Go's randomized map iteration, identical unequal
// inputs under the same quota alternated between answering false (the
// mismatch visited first) and raising a limit error (a long equal entry
// charged first). Callers whose keys are not yet billed use meteredMapKeys,
// which additionally charges every key's bytes before the sort reads them.
func sortedMapKeys[V any](m map[string]V, state *equalityState) ([]string, bool) {
	if state.reserveScratch != nil {
		state.scratchHeld += len(m) * keySortScratchEntryBytes
		if err := state.reserveScratch(state.scratchHeld, state.rootLeft, state.rootRight); err != nil {
			state.err = err
			return nil, false
		}
	}
	keys := make([]string, 0, len(m))
	for key := range m {
		keys = append(keys, key)
	}
	if state.charge == nil {
		slices.Sort(keys)
		return keys, true
	}
	// The sort rereads common prefixes Θ(log n) times per key, past what the
	// single linear key charge covers; measure the exact bytes each
	// comparison reads and bill them in batches from inside the comparator.
	// A failed charge aborts the sort itself — the partial order is
	// discarded with the error, so a spent quota buys no further
	// comparisons.
	const sortChargeBatchBytes = 4096
	pending := 0
	func() {
		defer func() {
			if r := recover(); r != nil {
				if _, ok := r.(equalitySortAbort); !ok {
					panic(r)
				}
			}
		}()
		slices.SortFunc(keys, func(a, b string) int {
			n := min(len(a), len(b))
			i := 0
			for i < n && a[i] == b[i] {
				i++
			}
			read := i + 1
			if read > n {
				read = n
			}
			if pending += read; pending >= sortChargeBatchBytes {
				if !chargeEqualityBytes(state, pending) {
					panic(equalitySortAbort{})
				}
				pending = 0
			}
			if i < n {
				if a[i] < b[i] {
					return -1
				}
				return 1
			}
			return len(a) - len(b)
		})
	}()
	if state.err != nil {
		return nil, false
	}
	if pending > 0 {
		if !chargeEqualityBytes(state, pending) {
			return nil, false
		}
	}
	return keys, true
}

// meteredMapKeys is sortedMapKeys with the keys' total bytes billed up front:
// the lexicographic sort scans common prefixes repeatedly and the walk's
// later map probes hash the same payloads, so one total, charged before any
// of that work runs, covers the traversal proportionally.
func meteredMapKeys[V any](m map[string]V, state *equalityState) ([]string, bool) {
	total := 0
	for key := range m {
		total += len(key)
	}
	if !chargeEqualityBytes(state, total) {
		return nil, false
	}
	return sortedMapKeys(m, state)
}

func hashMapsEqual(left, right map[string]Value, state *equalityState, strictKinds bool) bool {
	if len(left) != len(right) {
		return false
	}
	// Unmetered comparison keeps the host API's allocation-free direct scan:
	// with no charges in play, traversal order cannot change the outcome.
	if state.charge == nil {
		for key, leftValue := range left {
			rightValue, ok := right[key]
			if !ok {
				return false
			}
			if !valuesEqualWithKinds(leftValue, rightValue, state, strictKinds) {
				return false
			}
		}
		return true
	}
	keys, ok := meteredMapKeys(left, state)
	if !ok {
		return false
	}
	defer releaseKeySortScratch(state, len(keys))
	for _, key := range keys {
		rightValue, ok := right[key]
		if !ok {
			return false
		}
		if !valuesEqualWithKinds(left[key], rightValue, state, strictKinds) {
			return false
		}
	}
	return true
}

func equalityPairSeen(state *equalityState, pair valueEqualityPair) bool {
	if state.seen == nil {
		state.seen = make(map[valueEqualityPair]struct{})
	}
	if _, ok := state.seen[pair]; ok {
		return true
	}
	state.seen[pair] = struct{}{}
	return false
}
