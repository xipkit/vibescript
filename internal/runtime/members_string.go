package runtime

import (
	"errors"
	"fmt"
	"math"
	"regexp"
	"regexp/syntax"
	"strconv"
	"strings"
	"unicode"
	"unicode/utf8"
	"unsafe"

	"golang.org/x/text/cases"
	"golang.org/x/text/language"

	"github.com/mgomes/vibescript/vibes/value"
)

// stringMemberNames mirrors the names dispatched by stringMember and feeds
// "did you mean" suggestions on the error path. Keep it in sync with the
// switch below; TestMemberSuggestionCandidatesResolve enforces that every
// listed name resolves.
var stringMemberNames = []string{
	"size", "length", "bytesize", "ord", "chr", "getbyte", "byteslice", "hex", "oct", "empty?", "clear", "concat", "prepend", "insert", "replace", "start_with?", "end_with?", "include?", "count", "casecmp", "casecmp?", "between?", "match", "match?", "scan", "index", "rindex", "slice",
	"strip", "strip!", "squish", "squish!", "lstrip", "lstrip!", "rstrip", "rstrip!", "chomp", "chomp!", "chop", "chop!", "delete", "delete!", "delete_prefix", "delete_prefix!", "delete_suffix", "delete_suffix!", "tr", "tr!", "squeeze", "squeeze!", "upcase", "upcase!", "downcase", "downcase!", "capitalize", "capitalize!", "swapcase", "swapcase!", "reverse", "reverse!",
	"sub", "sub!", "gsub", "gsub!", "split", "partition", "rpartition", "chars", "lines", "bytes", "codepoints", "each_char", "each_line", "each_byte", "each_codepoint", "template",
	"center", "ljust", "rjust", "clamp",
	"inspect",
	"to_sym", "intern", "to_s", "string", "to_i", "to_f",
}

// pureStringMemberNames are the string members that read the receiver's bytes
// and return a fresh string or a scalar. Strings are immutable here, so a
// transform allocates rather than writes, and allocating is not a mutation
// under the promise.
//
// Absent on purpose: every bang form (the ! guard in declaringNonMutating
// rejects those outright), the in-place non-bang mutators clear, concat,
// prepend, insert and replace, and everything that reaches the regex machinery
// (match, match?, scan, sub, gsub, split, index with a pattern), whose shared
// cache is a write this promise has not been checked against.
var pureStringMemberNames = []string{
	"size", "length", "bytesize", "empty?",
	"start_with?", "end_with?", "include?",
	"upcase", "downcase", "capitalize", "swapcase", "reverse",
	"strip", "lstrip", "rstrip", "chomp", "chop",
	"to_s", "string", "to_i", "to_f",
}

var stringBuiltinMembers = newTypedMemberTable(stringMemberNames, KindString).
	declaringNonMutating(pureStringMemberNames...)

func stringMember(str Value, property string) (Value, error) {
	if member, ok := stringBuiltinMembers.lookup(property, stringMemberBuiltin); ok {
		return member, nil
	}
	return NewNil(), fmt.Errorf("unknown string method %s%s", property, didYouMean(property, stringMemberNames))
}

// stringConstantCostMembers names the string methods whose work does not grow
// with the receiver, so they are exempt from the per-byte scan charge. Every
// other string method was measured to scale with the receiver's length -- even
// ones whose logic is O(1), such as chop and delete_suffix, because they copy
// the string to build their result.
//
// The set is an exemption list rather than an inclusion list so the charge
// fails closed: a method added later is metered until someone establishes it is
// constant-cost, instead of silently joining the unmetered set.
var stringConstantCostMembers = map[string]struct{}{
	"bytesize": {}, "empty?": {}, "getbyte": {}, "ord": {}, "chr": {},
	"to_s": {}, "string": {}, "clear": {},
	// byteslice indexes by byte, and a symbol holds the receiver's string
	// header without copying or hashing it, so neither reads the receiver's
	// length. slice is charged rather than exempt because it indexes by rune,
	// which scans to the offset. byteslice does copy what it extracts, but
	// that costs the result rather than the receiver, so it bills those bytes
	// itself (see detachedByteslice) instead of being charged for a receiver
	// it never reads.
	"byteslice": {}, "to_sym": {}, "intern": {},
	// chop inspects only the receiver's final bytes and returns a substring
	// view, so its cost does not follow the length even when every byte is a
	// candidate: flat from 64 KiB to 2 MiB on an all-newline receiver. strip,
	// lstrip and rstrip are not here -- they look constant on ordinary text but
	// scan the whole receiver when it is all whitespace, rising 27 to 32 times
	// across that same range. chomp is not here either: it is constant only
	// with no argument, so stringCallChargesReceiver decides it per call.
	"chop": {}, "chop!": {},
	// replace ignores the receiver entirely and returns its argument.
	"replace": {},
}

// stringArgumentCapFactor reports the multiple of the receiver's length that
// bounds how much of a string argument a method can read, or zero when nothing
// bounds it and the argument is charged in full.
//
// Membership must be earned by reading the implementation, not assumed from the
// method's shape. An argument preprocessed independently of the receiver is not
// bounded by it: count parses every byte of its character set, match, match?
// and scan compile a whole pattern, and casecmp? validates its argument's UTF-8
// before comparing, so capping any of them let a large argument through for
// almost nothing. casecmp stays because asciiCaseCompare stops at the shorter
// operand. split is out because its separator handling was not verified.
//
// The factor matters as much as membership. Most of these match bytes, so the
// receiver's length bounds them directly. index and rindex match by rune when
// either side is invalid UTF-8, and their oversized-needle guard admits a needle
// up to utf8.UTFMax times the receiver for exactly that case -- so that is what
// they can read, and that is what they must be billed for. A cap below what the
// guard admits under-meters precisely the case the guard exists to preserve.
//
// The cap lowers a charge, so a wrong factor under-charges -- the failure this
// branch exists to prevent -- while an omission only over-charges.
// TestCappedArgumentsAreActuallyBoundedByTheReceiver exercises every member.
func stringArgumentCapFactor(name string) int {
	switch name {
	case "string.start_with?", "string.end_with?", "string.include?",
		"string.casecmp", "string.partition", "string.rpartition",
		"string.slice", "string.between?":
		return 1
	case "string.index", "string.rindex":
		return utf8.UTFMax
	default:
		return 0
	}
}

// stringCallChargesReceiver reports whether this call must pay for its
// receiver's length. It exists for methods whose cost depends on how they were
// called rather than on which method they are.
//
// chomp with no argument removes one fixed line ending and is constant however
// long the receiver. chomp("") removes every trailing newline, which scans the
// whole receiver when it is all newlines -- 26 times the cost across a 32x
// range -- and chomp(sep) compares a caller-supplied suffix. Exempting the name
// covered all three and left the linear two unmetered.
func stringCallChargesReceiver(name string, args []Value) bool {
	switch name {
	case "string.chomp", "string.chomp!":
		return len(args) > 0
	default:
		return true
	}
}

// chargeStringCall charges the step quota for one call on a string receiver:
// the receiver's bytes plus each string argument's.
//
// String arguments are copied into the result as well, so a short receiver with
// a large argument -- "".concat(s), "".prepend(s), "".insert(0, s) -- moves just
// as many bytes as the reverse. compareOnly caps each argument at the receiver's
// length for the methods that only match arguments against it (see
// stringComparesArgumentsToReceiver).
//
// Both entrances to a string method share this: the member dispatch wrapper and
// the direct-call fast paths in evalDirectStringMemberCallExpr. They charged
// different amounts while it lived only in the wrapper, so "".split(s) escaped
// through the fast path with an unmetered separator.
func (exec *Execution) chargeStringCall(receiver Value, args []Value, capFactor int) error {
	text := len(receiver.String())
	bytes := text
	for _, arg := range args {
		if arg.Kind() != KindString {
			continue
		}
		n := len(arg.String())
		if capFactor > 0 {
			n = min(n, saturatingMul(capFactor, text))
		}
		bytes = saturatingAdd(bytes, n)
	}
	return exec.chargeStringScan(bytes)
}

// chargeStringScanBeforeCall wraps a string builtin so it charges the step
// quota for the bytes it is about to scan. Wrapping at the single dispatch
// point rather than inside each method keeps the metering uniform and means a
// new string method is charged without anyone remembering to add a call.
func chargeStringScanBeforeCall(member Value) Value {
	inner := BuiltinOf(member)
	if inner == nil {
		return member
	}
	metered := *inner
	fn := inner.Fn
	capFactor := stringArgumentCapFactor(inner.Name)
	metered.Fn = func(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
		if receiver.Kind() == KindString && stringCallChargesReceiver(inner.Name, args) {
			if err := exec.chargeStringCall(receiver, args, capFactor); err != nil {
				return NewNil(), err
			}
		}
		return fn(exec, receiver, args, kwargs, block)
	}
	return value.NewValue(KindBuiltin, &metered)
}

func stringMemberBuiltin(property string) (Value, error) {
	member, err := stringMemberBuiltinUnmetered(property)
	if err != nil {
		return member, err
	}
	if _, exempt := stringConstantCostMembers[property]; exempt {
		return member, nil
	}
	return chargeStringScanBeforeCall(member), nil
}

func stringMemberBuiltinUnmetered(property string) (Value, error) {
	switch property {
	case "size", "length", "bytesize", "ord", "chr", "getbyte", "byteslice", "hex", "oct", "empty?", "clear", "concat", "prepend", "insert", "replace", "start_with?", "end_with?", "include?", "count", "casecmp", "casecmp?", "between?", "match", "match?", "scan", "index", "rindex", "slice":
		return stringMemberQuery(property)
	case "strip", "strip!", "squish", "squish!", "lstrip", "lstrip!", "rstrip", "rstrip!", "chomp", "chomp!", "chop", "chop!", "delete", "delete!", "delete_prefix", "delete_prefix!", "delete_suffix", "delete_suffix!", "tr", "tr!", "squeeze", "squeeze!", "upcase", "upcase!", "downcase", "downcase!", "capitalize", "capitalize!", "swapcase", "swapcase!", "reverse", "reverse!":
		return stringMemberTransforms(property)
	case "sub", "sub!", "gsub", "gsub!", "split", "partition", "rpartition", "chars", "lines", "bytes", "codepoints", "each_char", "each_line", "each_byte", "each_codepoint", "template":
		return stringMemberTextOps(property)
	case "center", "ljust", "rjust":
		return stringMemberPadding(property)
	case "clamp":
		return stringMemberClamp(), nil
	case "inspect":
		return newInspectBuiltin("string"), nil
	case "to_sym", "intern", "to_s", "string", "to_i", "to_f":
		return stringMemberConversions(property)
	default:
		return NewNil(), fmt.Errorf("unknown string method %s", property)
	}
}

// stringMemberConversions builds the string conversion members. Ruby's
// String#to_sym and its alias String#intern both return the symbol whose name is
// the receiver, so any string contents (including empty) yield a symbol verbatim
// without further validation. String#to_s and Vibescript's documented `.string`
// idiom return the receiver unchanged. String#to_i and String#to_f parse a
// numeric string with the same strict semantics as the global to_int/to_float
// builtins: unlike Ruby's lenient String#to_i (which ignores trailing garbage and
// yields 0 on failure), an empty or non-numeric string raises so a malformed
// value never silently becomes 0 when crossing a typed boundary.
func stringMemberConversions(property string) (Value, error) {
	name := "string." + property
	switch property {
	case "to_sym", "intern":
		return NewAutoBuiltin(name, func(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
			if err := requireNullaryCall(name, args, kwargs, block); err != nil {
				return NewNil(), err
			}
			return NewSymbol(receiver.String()), nil
		}), nil
	case "to_s", "string":
		return NewAutoBuiltin(name, func(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
			if err := requireNullaryCall(name, args, kwargs, block); err != nil {
				return NewNil(), err
			}
			return receiver, nil
		}), nil
	case "to_i":
		return NewAutoBuiltin(name, func(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
			if err := requireNullaryCall(name, args, kwargs, block); err != nil {
				return NewNil(), err
			}
			s := strings.TrimSpace(receiver.String())
			if s == "" {
				return NewNil(), fmt.Errorf("%s expects a numeric string", name)
			}
			return parseIntegerString(exec, s, name, receiver)
		}), nil
	case "to_f":
		return NewAutoBuiltin(name, func(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
			if err := requireNullaryCall(name, args, kwargs, block); err != nil {
				return NewNil(), err
			}
			s := strings.TrimSpace(receiver.String())
			if s == "" {
				return NewNil(), fmt.Errorf("%s expects a numeric string", name)
			}
			f, err := strconv.ParseFloat(s, 64)
			if err != nil {
				return NewNil(), fmt.Errorf("%s expects a numeric string", name)
			}
			if math.IsNaN(f) || math.IsInf(f, 0) {
				return NewNil(), fmt.Errorf("%s expects a finite numeric string", name)
			}
			return NewFloat(f), nil
		}), nil
	default:
		return NewNil(), fmt.Errorf("unknown string method %s", property)
	}
}

func stringMemberClamp() Value {
	return NewAutoBuiltin("string.clamp", func(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
		if len(kwargs) > 0 {
			return NewNil(), fmt.Errorf("string.clamp does not take keyword arguments")
		}
		if !block.IsNil() {
			return NewNil(), fmt.Errorf("string.clamp does not accept blocks")
		}
		if len(args) != 2 {
			return NewNil(), fmt.Errorf("string.clamp expects min and max")
		}
		minVal, err := stringClampBound(args[0])
		if err != nil {
			return NewNil(), err
		}
		maxVal, err := stringClampBound(args[1])
		if err != nil {
			return NewNil(), err
		}
		if minVal != nil && maxVal != nil && strings.Compare(minVal.String(), maxVal.String()) > 0 {
			return NewNil(), fmt.Errorf("string.clamp min must be <= max")
		}
		if minVal != nil && strings.Compare(receiver.String(), minVal.String()) < 0 {
			return *minVal, nil
		}
		if maxVal != nil && strings.Compare(receiver.String(), maxVal.String()) > 0 {
			return *maxVal, nil
		}
		return receiver, nil
	})
}

func stringClampBound(val Value) (*Value, error) {
	switch val.Kind() {
	case KindNil:
		return nil, nil
	case KindString:
		return &val, nil
	default:
		return nil, fmt.Errorf("string.clamp bounds must be strings or nil")
	}
}

// forEachLine invokes yield for each line in text using "\n" as the record
// separator, retaining the trailing "\n" on each line as Ruby's String#lines
// does. A trailing newline does not produce a final empty line, and an empty
// string yields nothing. Carriage returns are preserved verbatim, so "a\r\nb"
// yields "a\r\n" then "b". Lines are located one at a time via IndexByte so
// callers can stream without materializing every line, and an error returned by
// yield stops the scan immediately.
func forEachLine(text string, yield func(line string) error) error {
	for text != "" {
		index := strings.IndexByte(text, '\n')
		if index < 0 {
			return yield(text)
		}
		if err := yield(text[:index+1]); err != nil {
			return err
		}
		text = text[index+1:]
	}
	return nil
}

// stringPartition splits text around the first occurrence of sep, mirroring
// Ruby's String#partition. It returns the segment before the separator, the
// separator itself, and the segment after it. When the separator is absent the
// whole string is returned as the head with two empty trailing segments. An
// empty separator matches at the very start, yielding ("", "", text).
func stringPartition(text, sep string) (head, separator, tail string) {
	before, after, ok := strings.Cut(text, sep)
	if !ok {
		return text, "", ""
	}
	return before, sep, after
}

// stringRPartition splits text around the last occurrence of sep, mirroring
// Ruby's String#rpartition. When the separator is absent the whole string is
// returned as the tail with two empty leading segments. An empty separator
// matches at the very end, yielding (text, "", "").
func stringRPartition(text, sep string) (head, separator, tail string) {
	index := strings.LastIndex(text, sep)
	if index < 0 {
		return "", "", text
	}
	return text[:index], sep, text[index+len(sep):]
}

// isRubyASCIISpace reports whether b is one of the six ASCII whitespace bytes
// Ruby's ISSPACE macro recognizes: space, horizontal tab, newline, vertical
// tab, form feed, and carriage return. Ruby uses this classification for the
// default no-separator String#split, so wider Unicode whitespace such as NBSP
// (U+00A0) or the em space (U+2003) is intentionally excluded.
func isRubyASCIISpace(b byte) bool {
	switch b {
	case ' ', '\t', '\n', '\v', '\f', '\r':
		return true
	default:
		return false
	}
}

// splitOnASCIIWhitespaceLimit performs Ruby's default (AWK-style) String#split
// while honoring the optional limit argument. Only the bytes recognized by
// isRubyASCIISpace separate fields; wider Unicode whitespace is preserved
// inside the surrounding field rather than acting as a delimiter, matching Ruby
// instead of Go's strings.Fields Unicode whitespace table. With the default
// limit of 0 leading and trailing whitespace is discarded and consecutive
// whitespace collapses, so " a  b " yields ["a", "b"]. The limit cases extend
// that behavior:
//
//   - limit == 1 returns the whole string as a single field, leaving any
//     leading or trailing whitespace intact, exactly like Ruby.
//   - a positive limit caps the result at that many fields; once limit-1 fields
//     have been collected the remainder of the string (including the whitespace
//     that would normally separate fields) becomes the final field.
//   - any limit other than 0 preserves a single trailing empty field when the
//     string ends in whitespace, so "a b ".split(nil, -1) yields ["a", "b", ""].
//
// An empty string always yields no fields, matching Ruby.
func splitOnASCIIWhitespaceLimit(text string, limit, count int) []string {
	if text == "" {
		return nil
	}
	if limit == 1 {
		return []string{text}
	}
	if count == 0 {
		return nil
	}
	fields := make([]string, 0, count)
	i := 0
	n := len(text)
	for i < n {
		end := n
		if whitespaceSIMD && n-i > whitespaceProbeBytes {
			end = i + whitespaceProbeBytes
		}
		for i < end && isRubyASCIISpace(text[i]) {
			i++
		}
		if whitespaceSIMD && i == end && i < n {
			i += whitespacePrefixSIMD(text[i:], false)
		}
		if i >= n {
			break
		}
		if limit > 0 && len(fields) == limit-1 {
			fields = append(fields, text[i:])
			return fields
		}
		start := i
		end = n
		if whitespaceSIMD && n-i > whitespaceProbeBytes {
			end = i + whitespaceProbeBytes
		}
		for i < end && !isRubyASCIISpace(text[i]) {
			i++
		}
		if whitespaceSIMD && i == end && i < n {
			i += nonWhitespacePrefixSIMD(text[i:])
		}
		fields = append(fields, text[start:i])
	}
	// A trailing run of whitespace yields exactly one empty field whenever the
	// limit is not the default 0; a fully blank string therefore yields [""].
	if limit != 0 && isRubyASCIISpace(text[n-1]) {
		fields = append(fields, "")
	}
	return fields
}

func splitOnASCIIWhitespaceLimitCount(text string, limit int) int {
	return splitOnASCIIWhitespaceLimitProjection(text, limit).count
}

func splitOnASCIIWhitespaceLimitProjection(text string, limit int) stringSplitProjection {
	if text == "" {
		return stringSplitProjection{}
	}
	if limit == 1 {
		return newStringSplitProjection(text, text)
	}
	projection := stringSplitProjection{}
	i := 0
	n := len(text)
	for i < n {
		end := n
		if whitespaceSIMD && n-i > whitespaceProbeBytes {
			end = i + whitespaceProbeBytes
		}
		for i < end && isRubyASCIISpace(text[i]) {
			i++
		}
		if whitespaceSIMD && i == end && i < n {
			i += whitespacePrefixSIMD(text[i:], false)
		}
		if i >= n {
			break
		}
		if limit > 0 && projection.count == limit-1 {
			projection.add(text, text[i:])
			return projection
		}
		start := i
		end = n
		if whitespaceSIMD && n-i > whitespaceProbeBytes {
			end = i + whitespaceProbeBytes
		}
		for i < end && !isRubyASCIISpace(text[i]) {
			i++
		}
		if whitespaceSIMD && i == end && i < n {
			i += nonWhitespacePrefixSIMD(text[i:])
		}
		projection.add(text, text[start:i])
	}
	if limit != 0 && isRubyASCIISpace(text[n-1]) {
		projection.add(text, "")
	}
	return projection
}

// splitEmptySeparator implements Ruby's String#split("") which splits a string
// into its individual characters (runes). The limit argument matches Ruby:
//
//   - limit == 1 returns the whole string as a single field.
//   - a positive limit keeps the first limit-1 characters as single-character
//     fields and groups the remaining characters into the final field; if the
//     limit exceeds the character count a single trailing empty field is added.
//   - limit == 0 drops the trailing empty field, while a negative limit (and any
//     positive limit large enough to exhaust the characters) keeps it, so
//     "abc".split("", -1) yields ["a", "b", "c", ""].
//
// An empty string always yields no fields, matching Ruby. Splitting walks the
// string by UTF-8 character boundaries rather than materializing a []rune so
// that invalid bytes in a binary receiver are preserved as single-byte fields
// (matching Ruby's "a\xffb".split("") => ["a", "\xff", "b"]) instead of being
// rewritten as the U+FFFD replacement character.
func splitEmptySeparator(text string, limit, count int) []string {
	if text == "" {
		return nil
	}
	if limit == 1 {
		return []string{text}
	}
	// offsets holds the byte index where each character begins, so a positive
	// limit can slice the original text without losing raw bytes.
	offsets := make([]int, 0, len(text)+1)
	for i := 0; i < len(text); {
		offsets = append(offsets, i)
		_, width := utf8.DecodeRuneInString(text[i:])
		i += width
	}
	if limit > 1 && limit-1 < len(offsets) {
		fields := make([]string, limit)
		for i := range limit - 1 {
			fields[i] = text[offsets[i]:offsets[i+1]]
		}
		fields[limit-1] = text[offsets[limit-1]:]
		return fields
	}
	fields := make([]string, 0, count)
	for i, start := range offsets {
		end := len(text)
		if i+1 < len(offsets) {
			end = offsets[i+1]
		}
		fields = append(fields, text[start:end])
	}
	if limit != 0 {
		fields = append(fields, "")
	}
	return fields
}

func splitEmptySeparatorCount(text string, limit int) int {
	return splitEmptySeparatorProjection(text, limit).count
}

func splitEmptySeparatorProjection(text string, limit int) stringSplitProjection {
	if text == "" {
		return stringSplitProjection{}
	}
	if limit == 1 {
		return newStringSplitProjection(text, text)
	}
	projection := stringSplitProjection{}
	for i := 0; i < len(text); {
		if limit > 1 && projection.count == limit-1 {
			projection.add(text, text[i:])
			return projection
		}
		start := i
		_, width := utf8.DecodeRuneInString(text[i:])
		i += width
		projection.add(text, text[start:i])
	}
	if limit != 0 {
		projection.add(text, "")
	}
	return projection
}

// splitWithSeparator implements Ruby's String#split(sep, limit) for a non-empty
// string separator. The limit argument matches Ruby:
//
//   - a positive limit caps the result at that many fields, leaving the
//     remainder unsplit in the final field.
//   - limit == 0 (the default) drops trailing empty fields.
//   - a negative limit preserves every field, including trailing empties.
//
// An empty string always yields no fields, matching Ruby.
func splitWithSeparator(text, sep string, limit, count int) []string {
	if text == "" {
		return nil
	}
	if count == 0 {
		return nil
	}
	parts := make([]string, 0, count)
	switch {
	case limit > 0:
		start := 0
		for len(parts) < count-1 {
			idx := strings.Index(text[start:], sep)
			if idx < 0 {
				break
			}
			end := start + idx
			parts = append(parts, text[start:end])
			start = end + len(sep)
		}
		parts = append(parts, text[start:])
		return parts
	case limit < 0:
		start := 0
		for {
			idx := strings.Index(text[start:], sep)
			end := len(text)
			if idx >= 0 {
				end = start + idx
			}
			parts = append(parts, text[start:end])
			if idx < 0 {
				return parts
			}
			start = end + len(sep)
		}
	default:
		pendingEmpty := 0
		start := 0
		for {
			idx := strings.Index(text[start:], sep)
			end := len(text)
			if idx >= 0 {
				end = start + idx
			}
			part := text[start:end]
			if part == "" {
				pendingEmpty++
			} else {
				for range pendingEmpty {
					parts = append(parts, "")
				}
				pendingEmpty = 0
				parts = append(parts, part)
			}
			if idx < 0 {
				return parts
			}
			start = end + len(sep)
		}
	}
}

func splitWithSeparatorCount(text, sep string, limit int) int {
	return splitWithSeparatorProjection(text, sep, limit).count
}

func splitWithSeparatorProjection(text, sep string, limit int) stringSplitProjection {
	if text == "" {
		return stringSplitProjection{}
	}
	if limit == 1 {
		return newStringSplitProjection(text, text)
	}
	if limit > 1 {
		projection := stringSplitProjection{}
		start := 0
		for projection.count < limit-1 {
			idx := strings.Index(text[start:], sep)
			if idx < 0 {
				break
			}
			end := start + idx
			projection.add(text, text[start:end])
			start = end + len(sep)
		}
		projection.add(text, text[start:])
		return projection
	}
	projection := stringSplitProjection{}
	lastNonEmpty := stringSplitProjection{}
	start := 0
	for {
		idx := strings.Index(text[start:], sep)
		if idx < 0 {
			if text[start:] != "" {
				projection.add(text, text[start:])
				lastNonEmpty = projection
			} else if limit < 0 {
				projection.add(text, "")
			}
			break
		}
		part := text[start : start+idx]
		projection.add(text, part)
		if part != "" {
			lastNonEmpty = projection
		}
		start += idx + len(sep)
	}
	if limit < 0 {
		return projection
	}
	return lastNonEmpty
}

type stringSplitProjection struct {
	count   int
	payload int
}

type stringSplitMode uint8

const (
	stringSplitWhitespace stringSplitMode = iota
	stringSplitEmptySeparator
	stringSplitSeparator
)

type stringSplitCall struct {
	mode       stringSplitMode
	sep        string
	limit      int
	projection stringSplitProjection
}

func newStringSplitProjection(source, part string) stringSplitProjection {
	projection := stringSplitProjection{}
	projection.add(source, part)
	return projection
}

func (projection *stringSplitProjection) add(source, part string) {
	projection.count++
	projection.payload = saturatingAdd(projection.payload, detachedWindowPayloadBytes(source, part))
}

// detachedWindowPayloadBytes reports what one window onto source costs once it
// has been detached from it, and is the projection side of clonedWindow: split
// parts, partition components and lines are all priced by this rule.
//
// The two must agree on what allocates. clonedWindow copies nothing for an
// empty window (strings.Clone hands back the shared empty string) nor for one
// as long as source, which for a window means source itself; both are charged a
// header alone here. sameStringBacking is the stricter test of the two -- a
// same-length string that is not a window onto source is charged its bytes and
// copied by neither -- so any disagreement over-charges rather than letting an
// allocation through unpriced.
func detachedWindowPayloadBytes(source, part string) int {
	if len(part) == 0 || sameStringBacking(source, part) {
		return estimatedStringHeaderBytes
	}
	return saturatingAdd(estimatedStringHeaderBytes, len(part))
}

func sameStringBacking(a, b string) bool {
	return len(a) == len(b) && len(a) > 0 && unsafe.StringData(a) == unsafe.StringData(b)
}

func reserveStringSplitResult(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value, count, extraScratch int) (*arrayBuildAccumulator, error) {
	if err := exec.checkStepBudgetFor(count); err != nil {
		return nil, err
	}
	acc := newArrayBuildAccumulator(exec, receiver, args, kwargs, block)
	if extraScratch > 0 {
		if err := acc.reserveScratch(extraScratch); err != nil {
			return nil, err
		}
	}
	if err := acc.reserveSlots(count); err != nil {
		return nil, err
	}
	return acc, nil
}

func reserveProjectedStringSplitResult(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value, projection stringSplitProjection) error {
	if err := exec.checkStepBudgetFor(projection.count); err != nil {
		return err
	}
	return exec.checkProjectedArrayBytesWithCallRoots(projection.count, projection.payload, 0, receiver, args, kwargs, block)
}

func reserveProjectedStringSplitResultWithPositionalRoots(exec *Execution, receiver, arg0, arg1 Value, argCount int, projection stringSplitProjection) error {
	if err := exec.checkStepBudgetFor(projection.count); err != nil {
		return err
	}
	return exec.checkProjectedArrayBytesWithPositionalCallRoots(projection.count, projection.payload, 0, receiver, arg0, arg1, argCount)
}

func planStringSplitCall(receiver, arg0, arg1 Value, argCount int) (stringSplitCall, error) {
	if argCount > 2 {
		return stringSplitCall{}, fmt.Errorf("string.split accepts at most a separator and a limit")
	}
	limit := 0
	if argCount == 2 {
		if arg1.Kind() != KindInt {
			return stringSplitCall{}, fmt.Errorf("string.split limit must be integer")
		}
		if arg1.IsBigInt() {
			return stringSplitCall{}, fmt.Errorf("string.split limit must fit in a 64-bit integer")
		}
		limit = int(arg1.Int())
	}
	text := receiver.String()
	switch {
	case argCount == 0 || arg0.IsNil():
		return stringSplitCall{
			mode:       stringSplitWhitespace,
			limit:      limit,
			projection: splitOnASCIIWhitespaceLimitProjection(text, limit),
		}, nil
	case arg0.Kind() != KindString:
		return stringSplitCall{}, fmt.Errorf("string.split separator must be string or nil")
	case arg0.String() == " ":
		return stringSplitCall{
			mode:       stringSplitWhitespace,
			limit:      limit,
			projection: splitOnASCIIWhitespaceLimitProjection(text, limit),
		}, nil
	case arg0.String() == "":
		return stringSplitCall{
			mode:       stringSplitEmptySeparator,
			limit:      limit,
			projection: splitEmptySeparatorProjection(text, limit),
		}, nil
	default:
		sep := arg0.String()
		return stringSplitCall{
			mode:       stringSplitSeparator,
			sep:        sep,
			limit:      limit,
			projection: splitWithSeparatorProjection(text, sep, limit),
		}, nil
	}
}

func stringSplitCallResult(exec *Execution, receiver Value, split stringSplitCall) (Value, error) {
	text := receiver.String()
	switch split.mode {
	case stringSplitWhitespace:
		return stringSplitWhitespaceResult(exec, text, split.limit, split.projection.count)
	case stringSplitEmptySeparator:
		return stringSplitEmptySeparatorResult(exec, text, split.limit, split.projection.count)
	default:
		return stringSplitSeparatorResult(exec, text, split.sep, split.limit, split.projection.count)
	}
}

func stringSplitResultFromArgs(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
	arg0 := NewNil()
	arg1 := NewNil()
	if len(args) > 0 {
		arg0 = args[0]
	}
	if len(args) > 1 {
		arg1 = args[1]
	}
	split, err := planStringSplitCall(receiver, arg0, arg1, len(args))
	if err != nil {
		return NewNil(), err
	}
	if err := reserveProjectedStringSplitResult(exec, receiver, args, kwargs, block, split.projection); err != nil {
		return NewNil(), err
	}
	return stringSplitCallResult(exec, receiver, split)
}

func stringSplitResultFromPositionalRoots(exec *Execution, receiver, arg0, arg1 Value, argCount int) (Value, error) {
	split, err := planStringSplitCall(receiver, arg0, arg1, argCount)
	if err != nil {
		return NewNil(), err
	}
	if err := reserveProjectedStringSplitResultWithPositionalRoots(exec, receiver, arg0, arg1, argCount, split.projection); err != nil {
		return NewNil(), err
	}
	return stringSplitCallResult(exec, receiver, split)
}

// appendStringSplitPart appends one split part, copied out of the source it was
// cut from (see clonedWindow).
//
// Every part is a window onto source, so keeping one of them pinned the whole
// subject: `(big + "|x").split("|")[1]` was charged one byte and held a
// megabyte, and 200 of them retained 192.1 MiB under an 8 MiB quota. The parts
// of one split add up to the subject, so keeping all of them was already priced
// honestly and only a kept subset amplified.
//
// The copy needs no reservation of its own: detachedWindowPayloadBytes already
// projects each part at its own length -- header only for an empty part or one
// that spans the whole source -- which is exactly the set clonedWindow copies
// nothing for, so the reservation the caller already made covers these bytes.
func appendStringSplitPart(exec *Execution, values *[]Value, source, part string) error {
	if err := exec.step(); err != nil {
		return err
	}
	*values = append(*values, NewString(clonedWindow(source, part)))
	return nil
}

func stringSplitWhitespaceResult(exec *Execution, text string, limit, count int) (Value, error) {
	if err := exec.checkStepBudgetFor(count); err != nil {
		return NewNil(), err
	}
	values := make([]Value, 0, count)
	if text == "" || count == 0 {
		return NewArray(values), nil
	}
	if limit == 1 {
		if err := appendStringSplitPart(exec, &values, text, text); err != nil {
			return NewNil(), err
		}
		return NewArray(values), nil
	}
	i := 0
	n := len(text)
	for i < n {
		end := n
		if whitespaceSIMD && n-i > whitespaceProbeBytes {
			end = i + whitespaceProbeBytes
		}
		for i < end && isRubyASCIISpace(text[i]) {
			i++
		}
		if whitespaceSIMD && i == end && i < n {
			i += whitespacePrefixSIMD(text[i:], false)
		}
		if i >= n {
			break
		}
		if limit > 0 && len(values) == limit-1 {
			if err := appendStringSplitPart(exec, &values, text, text[i:]); err != nil {
				return NewNil(), err
			}
			return NewArray(values), nil
		}
		start := i
		end = n
		if whitespaceSIMD && n-i > whitespaceProbeBytes {
			end = i + whitespaceProbeBytes
		}
		for i < end && !isRubyASCIISpace(text[i]) {
			i++
		}
		if whitespaceSIMD && i == end && i < n {
			i += nonWhitespacePrefixSIMD(text[i:])
		}
		if err := appendStringSplitPart(exec, &values, text, text[start:i]); err != nil {
			return NewNil(), err
		}
	}
	if limit != 0 && isRubyASCIISpace(text[n-1]) {
		if err := appendStringSplitPart(exec, &values, text, ""); err != nil {
			return NewNil(), err
		}
	}
	return NewArray(values), nil
}

func stringSplitEmptySeparatorResult(exec *Execution, text string, limit, count int) (Value, error) {
	if err := exec.checkStepBudgetFor(count); err != nil {
		return NewNil(), err
	}
	values := make([]Value, 0, count)
	if text == "" || count == 0 {
		return NewArray(values), nil
	}
	if limit == 1 {
		if err := appendStringSplitPart(exec, &values, text, text); err != nil {
			return NewNil(), err
		}
		return NewArray(values), nil
	}
	for i := 0; i < len(text); {
		if limit > 1 && len(values) == limit-1 {
			if err := appendStringSplitPart(exec, &values, text, text[i:]); err != nil {
				return NewNil(), err
			}
			return NewArray(values), nil
		}
		start := i
		_, width := utf8.DecodeRuneInString(text[i:])
		i += width
		if err := appendStringSplitPart(exec, &values, text, text[start:i]); err != nil {
			return NewNil(), err
		}
	}
	if limit != 0 {
		if err := appendStringSplitPart(exec, &values, text, ""); err != nil {
			return NewNil(), err
		}
	}
	return NewArray(values), nil
}

func stringSplitSeparatorResult(exec *Execution, text, sep string, limit, count int) (Value, error) {
	if err := exec.checkStepBudgetFor(count); err != nil {
		return NewNil(), err
	}
	values := make([]Value, 0, count)
	if text == "" || count == 0 {
		return NewArray(values), nil
	}
	switch {
	case limit > 0:
		start := 0
		for len(values) < count-1 {
			idx := strings.Index(text[start:], sep)
			if idx < 0 {
				break
			}
			end := start + idx
			if err := appendStringSplitPart(exec, &values, text, text[start:end]); err != nil {
				return NewNil(), err
			}
			start = end + len(sep)
		}
		if err := appendStringSplitPart(exec, &values, text, text[start:]); err != nil {
			return NewNil(), err
		}
	case limit < 0:
		start := 0
		for {
			idx := strings.Index(text[start:], sep)
			end := len(text)
			if idx >= 0 {
				end = start + idx
			}
			if err := appendStringSplitPart(exec, &values, text, text[start:end]); err != nil {
				return NewNil(), err
			}
			if idx < 0 {
				break
			}
			start = end + len(sep)
		}
	default:
		pendingEmpty := 0
		start := 0
		for {
			idx := strings.Index(text[start:], sep)
			end := len(text)
			if idx >= 0 {
				end = start + idx
			}
			part := text[start:end]
			if part == "" {
				pendingEmpty++
			} else {
				for range pendingEmpty {
					if err := appendStringSplitPart(exec, &values, text, ""); err != nil {
						return NewNil(), err
					}
				}
				pendingEmpty = 0
				if err := appendStringSplitPart(exec, &values, text, part); err != nil {
					return NewNil(), err
				}
			}
			if idx < 0 {
				break
			}
			start = end + len(sep)
		}
	}
	return NewArray(values), nil
}

func stringSplitResult(exec *Execution, parts []string, acc *arrayBuildAccumulator) (Value, error) {
	if err := exec.checkStepBudgetFor(len(parts)); err != nil {
		return NewNil(), err
	}
	values := make([]Value, 0, len(parts))
	for _, part := range parts {
		if err := exec.step(); err != nil {
			return NewNil(), err
		}
		values = append(values, NewString(part))
		if err := acc.add(values[len(values)-1], cap(values)); err != nil {
			return NewNil(), err
		}
	}
	return NewArray(values), nil
}

// chompDefault removes one trailing line ending from text, mirroring Ruby's
// argumentless String#chomp. A "\r\n" pair counts as one ending.
//
// The result stays a window onto text rather than being detached like every
// other trimming member, and so does chopDefault's. Both remove a fixed few
// bytes -- at most two here, at most one rune there -- so a single call leaves
// at most that many bytes of the receiver unpriced, where chomp(sep),
// chomp("") and the strip family each remove an unbounded amount in one call
// and had to be detached. Reaching a meaningful gap needs one call per byte
// removed, which the step quota prices, and closing it would cost a full copy
// of the receiver on every call: `s = s.chop` down a megabyte would copy a
// terabyte to save a megabyte, and chop is in stringConstantCostMembers
// precisely because it does not read the receiver today.
func chompDefault(text string) string {
	if strings.HasSuffix(text, "\r\n") {
		return text[:len(text)-2]
	}
	if strings.HasSuffix(text, "\n") || strings.HasSuffix(text, "\r") {
		return text[:len(text)-1]
	}
	return text
}

// chopDefault removes the trailing character from text, mirroring Ruby's
// String#chop. A "\r\n" pair is treated as a single record separator and both
// bytes are removed together. Otherwise one logical character (a full UTF-8
// rune) is removed rather than a single byte, so trailing multibyte characters
// are handled correctly. An empty string is returned unchanged.
//
// Like chompDefault, the result stays a window onto text; see that function for
// why the bounded few bytes it leaves unpriced are not worth a copy.
func chopDefault(text string) string {
	if strings.HasSuffix(text, "\r\n") {
		return text[:len(text)-2]
	}
	if text == "" {
		return text
	}
	_, size := utf8.DecodeLastRuneInString(text)
	return text[:len(text)-size]
}

// asciiCaseCompare compares a and b byte-by-byte, folding only the ASCII
// letters A-Z down to a-z before each byte comparison. This mirrors Ruby's
// String#casecmp, whose comparison path applies an ASCII-only TOLOWER to each
// side while every other byte (punctuation and multibyte UTF-8 sequences alike)
// is compared ordinally. Folding downward is what keeps the result consistent
// with Ruby for the punctuation bytes between 'Z' and 'a' (such as '[', '\\',
// ']', '^', '_', and '`'): because uppercase letters fold to the 'a'-'z' range,
// those punctuation bytes sort below the folded letters, so e.g. "[".casecmp("A")
// is -1. Folding upward would invert that ordering. The result is normalized to
// -1, 0, or 1.
func asciiCaseCompare(a, b string) int {
	limit := min(len(a), len(b))
	for i := range limit {
		ca, cb := asciiLower(a[i]), asciiLower(b[i])
		if ca != cb {
			if ca < cb {
				return -1
			}
			return 1
		}
	}
	switch {
	case len(a) < len(b):
		return -1
	case len(a) > len(b):
		return 1
	default:
		return 0
	}
}

func asciiLower(b byte) byte {
	if b >= 'A' && b <= 'Z' {
		return b + ('a' - 'A')
	}
	return b
}

// caseInsensitiveEqual reports whether a and b are equal under case folding,
// backing Ruby's String#casecmp?. When both operands are valid UTF-8 it uses
// Unicode simple case folding (matching the upcase/downcase surface), so
// full-fold cases like "ß" vs "SS" stay unequal. When either operand contains
// invalid UTF-8 it folds byte-wise over the ASCII letters instead, mirroring
// Ruby's binary-string path. The byte-wise fallback preserves byte identity:
// distinct invalid sequences such as "\xff" and "\xfe" remain unequal, whereas
// strings.EqualFold would decode both as utf8.RuneError and report them equal.
func caseInsensitiveEqual(a, b string) bool {
	if utf8.ValidString(a) && utf8.ValidString(b) {
		return strings.EqualFold(a, b)
	}
	return asciiCaseEqual(a, b)
}

// asciiCaseEqual reports whether a and b are equal after folding only the ASCII
// letters A-Z down to a-z, comparing every other byte ordinally. It is the
// equality counterpart of asciiCaseCompare and is used for operands that are
// not valid UTF-8.
func asciiCaseEqual(a, b string) bool {
	if len(a) != len(b) {
		return false
	}
	for i := range len(a) {
		if asciiLower(a[i]) != asciiLower(b[i]) {
			return false
		}
	}
	return true
}

func stringRuneLen(text string) int {
	if stringIsASCII(text) {
		return len(text)
	}
	return utf8.RuneCountInString(text)
}

func stringByteIndexForRuneOffset(text string, offset int) (int, bool) {
	if offset < 0 {
		return 0, false
	}
	if stringIsASCII(text) {
		if offset > len(text) {
			return 0, false
		}
		return offset, true
	}
	runeIndex := 0
	for byteIndex := range text {
		if runeIndex == offset {
			return byteIndex, true
		}
		runeIndex++
	}
	if runeIndex == offset {
		return len(text), true
	}
	return 0, false
}

// stringEffectiveOffset normalizes a rune offset that may be negative the way
// Ruby's String#index and String#rindex do: a negative offset counts back from
// the end of the string, so -1 refers to the last rune. The second return value
// is false when the resulting offset falls before the start of the string, which
// callers translate into a nil result.
func stringEffectiveOffset(text string, offset int) (int, bool) {
	if offset >= 0 {
		return offset, true
	}
	effective := stringRuneLen(text) + offset
	if effective < 0 {
		return 0, false
	}
	return effective, true
}

func stringRuneIndex(exec *Execution, text, needle string, offset int) (int, error) {
	if offset < 0 {
		return -1, nil
	}
	// Reject on length before anything reads the needle. stringIsASCII scans
	// whatever it is given, and && short-circuits on the receiver, so a short
	// receiver let a large needle be scanned in full -- unmetered work that the
	// receiver-bounded classification in stringComparesArgumentsToReceiver
	// promises does not happen.
	//
	// The bound is the needle's bytes against the haystack's scaled by the
	// widest encoding, not against them directly. Invalid UTF-8 matches by rune
	// through the fallback below, where a one-byte invalid sequence and a
	// three-byte replacement character are both a single RuneError and do match:
	// comparing bytes alone would reject that pair. A needle longer than
	// utf8.UTFMax times the haystack must hold more runes than it, whatever the
	// encoding, so this rejects only what cannot match either way.
	if len(needle) > saturatingMul(utf8.UTFMax, len(text)) {
		return -1, nil
	}
	if stringIsASCII(text) && stringIsASCII(needle) {
		if offset > len(text) {
			return -1, nil
		}
		if needle == "" {
			return offset, nil
		}
		index := strings.Index(text[offset:], needle)
		if index < 0 {
			return -1, nil
		}
		return offset + index, nil
	}
	if !utf8.ValidString(text) || !utf8.ValidString(needle) {
		return stringRuneIndexFallback(exec, text, needle, offset)
	}
	startByte, ok := stringByteIndexForRuneOffset(text, offset)
	if !ok {
		return -1, nil
	}
	if needle == "" {
		return offset, nil
	}
	index := strings.Index(text[startByte:], needle)
	if index < 0 {
		return -1, nil
	}
	return offset + utf8.RuneCountInString(text[startByte:startByte+index]), nil
}

// reserveFallbackSearchScratch checks everything the invalid-UTF-8 search
// allocates against the memory quota, before any of it is allocated.
//
// The peak holds two things at once: the rune slices, at four bytes per input
// byte, and the canonical strings built from them, where each invalid byte
// widens to a three-byte replacement rune. Reserving only the canonical copies
// undercounted, and reserving after the rune slices were already built checked
// a peak that had partly happened.
func reserveFallbackSearchScratch(exec *Execution, text, needle string) error {
	if exec == nil {
		return nil
	}
	operands := saturatingAdd(len(text), len(needle))
	runes := saturatingMul(utf8.UTFMax, operands)
	canonical := saturatingMul(utf8.UTFMax-1, operands)
	return exec.checkProjectedStringBytes(saturatingAdd(runes, canonical))
}

// stringRuneIndexFallback searches operands that are not valid UTF-8 by rune.
//
// It reports the quota error rather than a miss when its scratch does not fit:
// the scratch is transient, built and released before the caller's own check
// runs, so nothing else accounts for it -- and answering "not found" would make
// a needle that is present look absent because memory was tight.
func stringRuneIndexFallback(exec *Execution, text, needle string, offset int) (int, error) {
	if err := reserveFallbackSearchScratch(exec, text, needle); err != nil {
		return -1, err
	}
	hayRunes := []rune(text)
	needleRunes := []rune(needle)
	if offset > len(hayRunes) {
		return -1, nil
	}
	if len(needleRunes) == 0 {
		return offset, nil
	}
	limit := len(hayRunes) - len(needleRunes)
	if limit < offset {
		return -1, nil
	}
	// Search the canonical rune encoding rather than testing every candidate
	// position. []rune already mapped each invalid byte to RuneError, so
	// re-encoding gives byte strings whose substring matches correspond exactly
	// to rune matches, and every index in them is a rune boundary. Scanning
	// positions with runesHavePrefix is quadratic -- a haystack of repeated
	// bytes against a needle sharing a long prefix forced roughly n*m
	// comparisons while the charge covered only n+m.
	hay := string(hayRunes[offset:])
	at := strings.Index(hay, string(needleRunes))
	if at < 0 {
		return -1, nil
	}
	return offset + utf8.RuneCountInString(hay[:at]), nil
}

func stringRuneRIndex(exec *Execution, text, needle string, offset int) (int, error) {
	if offset < 0 {
		return -1, nil
	}
	// Reject on length before anything reads the needle. stringIsASCII scans
	// whatever it is given, and && short-circuits on the receiver, so a short
	// receiver let a large needle be scanned in full -- unmetered work that the
	// receiver-bounded classification in stringComparesArgumentsToReceiver
	// promises does not happen.
	//
	// The bound is the needle's bytes against the haystack's scaled by the
	// widest encoding, not against them directly. Invalid UTF-8 matches by rune
	// through the fallback below, where a one-byte invalid sequence and a
	// three-byte replacement character are both a single RuneError and do match:
	// comparing bytes alone would reject that pair. A needle longer than
	// utf8.UTFMax times the haystack must hold more runes than it, whatever the
	// encoding, so this rejects only what cannot match either way.
	if len(needle) > saturatingMul(utf8.UTFMax, len(text)) {
		return -1, nil
	}
	if stringIsASCII(text) && stringIsASCII(needle) {
		if offset > len(text) {
			offset = len(text)
		}
		if needle == "" {
			return offset, nil
		}
		maxStart := len(text) - len(needle)
		if maxStart < 0 {
			return -1, nil
		}
		start := min(offset, maxStart)
		return strings.LastIndex(text[:start+len(needle)], needle), nil
	}
	if !utf8.ValidString(text) || !utf8.ValidString(needle) {
		return stringRuneRIndexFallback(exec, text, needle, offset)
	}
	textLen := stringRuneLen(text)
	if offset > textLen {
		offset = textLen
	}
	if needle == "" {
		return offset, nil
	}
	needleLen := stringRuneLen(needle)
	if needleLen > textLen {
		return -1, nil
	}
	start := min(offset, textLen-needleLen)
	endByte, ok := stringByteIndexForRuneOffset(text, start+needleLen)
	if !ok {
		return -1, nil
	}
	index := strings.LastIndex(text[:endByte], needle)
	if index < 0 {
		return -1, nil
	}
	return utf8.RuneCountInString(text[:index]), nil
}

// stringRuneRIndexFallback is stringRuneIndexFallback searching backwards; see
// there for why the scratch is reserved and why a shortfall is an error.
func stringRuneRIndexFallback(exec *Execution, text, needle string, offset int) (int, error) {
	if err := reserveFallbackSearchScratch(exec, text, needle); err != nil {
		return -1, err
	}
	hayRunes := []rune(text)
	needleRunes := []rune(needle)
	if offset > len(hayRunes) {
		offset = len(hayRunes)
	}
	if len(needleRunes) == 0 {
		return offset, nil
	}
	if len(needleRunes) > len(hayRunes) {
		return -1, nil
	}
	start := min(offset, len(hayRunes)-len(needleRunes))
	// Linear for the same reason as the forward fallback; searching backwards
	// from every candidate position is quadratic.
	hay := string(hayRunes[:start+len(needleRunes)])
	at := strings.LastIndex(hay, string(needleRunes))
	if at < 0 {
		return -1, nil
	}
	return utf8.RuneCountInString(hay[:at]), nil
}

// stringWindow is a rune-selected substring together with whether it already
// owns its bytes instead of sharing the receiver's backing allocation.
//
// Selecting invalid UTF-8 rebuilds the substring from its runes, and that
// rebuild is independent of the receiver the moment it is made. Detaching it a
// second time would copy for nothing, and would leave the receiver, the rebuild
// and the copy all live while only the copy is reserved, understating the peak
// the reservation exists to price (#36, #50).
type stringWindow struct {
	text     string
	detached bool
}

// newStringWindow normalizes a raw byte window the way Ruby's rune-aware
// slicing does, recording whether the normalization already gave it a backing
// of its own.
func newStringWindow(window string) stringWindow {
	if utf8.ValidString(window) {
		return stringWindow{text: window}
	}
	return stringWindow{text: string([]rune(window)), detached: true}
}

// stringRuneSlice extracts at most length runes starting at the rune offset
// start, matching Ruby's String#slice(start, length). A negative start counts
// back from the end of the string. It returns ok=false when length is negative
// or when start lands outside the string; a start exactly equal to the rune
// length is in range and yields an empty string (Ruby's "abc".slice(3, n) =>
// ""). The length is clamped to the remaining runes, so an oversized length
// returns the suffix from start rather than overrunning.
func stringRuneSlice(text string, start, length int) (stringWindow, bool) {
	if length < 0 {
		return stringWindow{}, false
	}
	if start < 0 {
		start += stringRuneLen(text)
		if start < 0 {
			return stringWindow{}, false
		}
	}
	startByte, ok := stringByteIndexForRuneOffset(text, start)
	if !ok {
		return stringWindow{}, false
	}
	endByte := startByte
	for range length {
		if endByte == len(text) {
			break
		}
		_, size := utf8.DecodeRuneInString(text[endByte:])
		endByte += size
	}
	return newStringWindow(text[startByte:endByte]), true
}

// stringSlice implements String#slice. It mirrors Ruby's extraction semantics
// across the four argument shapes Vibescript can represent: a single integer
// index (single character, negative counts from the end), an integer start with
// a length, an integer range, and a substring. Out-of-range selectors yield nil
// rather than raising, matching Ruby. Regexp selectors are intentionally not
// handled because Vibescript has no regexp value type yet (tracked separately).
func stringSlice(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
	if len(args) < 1 || len(args) > 2 {
		return NewNil(), fmt.Errorf("string.slice expects an index, range, or substring with optional length")
	}
	second := NewNil()
	if len(args) == 2 {
		second = args[1]
	}
	return stringSliceResult(exec, receiver, args[0], second, len(args) == 2, kwargs, block)
}

// stringSliceResult builds String#slice's value, detaching it from the
// receiver's backing.
//
// slice built its result from []rune until it moved to byte-offset slicing, and
// the rune rebuild now happens only for invalid UTF-8, so a valid selection came
// straight back as a window onto the receiver: 200 one-character slices of a
// megabyte held 192 MiB under an 8 MiB quota (#50).
//
// The copy takes no charge of its own. chargeStringScanBeforeCall already
// billed slice for its receiver's length, and a slice of the receiver can never
// exceed it, so charging the copy on top would bill the same bytes twice.
//
// kwargs and block reach the reservation even though slice ignores them. Member
// dispatch accepts both and keeps them live across the call, so an ephemeral
// receiver, an ephemeral `junk:` value and the copy can be resident at once;
// leaving them out let that three-way peak through a quota that only ever saw
// two of the three.
func stringSliceResult(exec *Execution, receiver, first, second Value, hasLength bool, kwargs map[string]Value, block Value) (Value, error) {
	text := receiver.String()
	if !hasLength && first.Kind() == KindString {
		// A substring selector yields the argument itself rather than a window
		// into the receiver, so there is no backing to detach from.
		if strings.Contains(text, first.String()) {
			return NewString(first.String()), nil
		}
		return NewNil(), nil
	}
	window, ok, err := stringSliceWindow(text, first, second, hasLength)
	if err != nil || !ok {
		return NewNil(), err
	}
	args := [2]Value{first, second}
	count := 1
	if hasLength {
		count = 2
	}
	detached, err := detachedWindow(exec, text, window, receiver, args[:count], kwargs, block)
	if err != nil {
		return NewNil(), err
	}
	return NewString(detached), nil
}

// stringSliceWindow resolves String#slice's integer, start/length and range
// selectors against text and returns the substring they select. ok is false for
// the out-of-range selections that yield nil. The substring selector is handled
// by the caller because it returns its argument rather than a window.
func stringSliceWindow(text string, first, second Value, hasLength bool) (stringWindow, bool, error) {
	if hasLength {
		start, err := valueToInt(first)
		if err != nil {
			return stringWindow{}, false, fmt.Errorf("string.slice index must be integer")
		}
		length, err := valueToInt(second)
		if err != nil {
			return stringWindow{}, false, fmt.Errorf("string.slice length must be integer")
		}
		window, ok := stringRuneSlice(text, start, length)
		return window, ok, nil
	}
	if first.Kind() == KindRange {
		window, ok := stringRuneRangeSlice(text, first.Range())
		return window, ok, nil
	}
	index, err := valueToInt(first)
	if err != nil {
		return stringWindow{}, false, fmt.Errorf("string.slice index must be an integer, range, or substring")
	}
	window, ok := stringSliceCharAt(text, index)
	return window, ok, nil
}

// stringSliceCharAt returns the single-character slice for String#slice(index).
// Unlike the (start, length) form, an index equal to the rune length is out of
// range and yields ok=false (Ruby's "abc".slice(3) => nil while "abc".slice(3, 1)
// => ""). A negative index counts back from the end.
func stringSliceCharAt(text string, index int) (stringWindow, bool) {
	if index < 0 {
		index += stringRuneLen(text)
		if index < 0 {
			return stringWindow{}, false
		}
	}
	if index >= stringRuneLen(text) {
		return stringWindow{}, false
	}
	return stringRuneSlice(text, index, 1)
}

// stringInsertByteOffset maps a Ruby String#insert character index to a byte
// offset in text, returning ok=false when the index is out of range. A
// non-negative index inserts before the character at that position, so the
// valid range is 0..runeLen (a value equal to runeLen appends). A negative
// index inserts after the character it selects, so -1 appends and the valid
// range is -(runeLen+1)..-1; the effective offset is runeLen + index + 1.
func stringInsertByteOffset(text string, index int) (int, bool) {
	if index < 0 {
		index += stringRuneLen(text) + 1
		if index < 0 {
			return 0, false
		}
	}
	return stringByteIndexForRuneOffset(text, index)
}

// stringRuneRangeSlice extracts the runes selected by a range, matching Ruby's
// String#slice(range). Negative bounds count back from the end. A begin bound
// before the start of the string (after normalization) or past its length
// returns ok=false (nil); a begin exactly at the length yields an empty string.
// The end bound is clamped to the string length, and an end before begin yields
// an empty string.
func stringRuneRangeSlice(text string, rng Range) (stringWindow, bool) {
	length := int64(stringRuneLen(text))
	begin := rng.Start
	if rng.Beginless {
		// A beginless range slices from the start of the receiver.
		begin = 0
	}
	if rng.Endless {
		// An endless range slices through the end of the receiver; the
		// inclusive MaxInt64 clamp below already maps this to the length.
		rng.End = math.MaxInt64
		rng.Exclusive = false
	}
	if begin < 0 {
		begin += length
	}
	if begin < 0 || begin > length {
		return stringWindow{}, false
	}
	end := rng.End
	if end < 0 {
		end += length
	}
	if !rng.Exclusive {
		// An inclusive range's exclusive end is one past End; guard the increment so
		// End == math.MaxInt64 cannot wrap to a negative no-op window.
		if end == math.MaxInt64 {
			end = length
		} else {
			end++
		}
	}
	if end > length {
		end = length
	}
	if end < begin {
		end = begin
	}
	return stringRuneSlice(text, int(begin), int(end-begin))
}

// stringByteslice implements Ruby's String#byteslice. It operates on raw byte
// offsets (unlike slice, which is rune-aware) and accepts three argument shapes:
// a single integer index returns the one-byte substring at that offset; an
// integer start and length return up to length bytes from start; and a range
// selects a byte window. Negative offsets count back from the end of the string.
// An out-of-range start, or a negative length, yields nil, matching Ruby. The
// extracted bytes are returned verbatim without UTF-8 normalization, so slicing
// across a multibyte boundary preserves the raw bytes, mirroring Ruby's
// byte-oriented semantics.
func stringByteslice(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
	text := receiver.String()
	sub, inRange, err := stringBytesliceWindow(text, args)
	if err != nil {
		return NewNil(), err
	}
	if !inRange {
		return NewNil(), nil
	}
	detached, err := detachedByteslice(exec, text, sub, receiver, args, kwargs, block)
	if err != nil {
		return NewNil(), err
	}
	return NewString(detached), nil
}

// stringBytesliceWindow resolves String#byteslice's arguments against text and
// returns the byte window they select. inRange is false for the out-of-range
// selections that yield nil, which is distinct from the in-range selections
// that yield an empty string.
func stringBytesliceWindow(text string, args []Value) (string, bool, error) {
	switch len(args) {
	case 1:
		if args[0].Kind() == KindRange {
			sub, inRange := stringByteRangeSlice(text, args[0].Range())
			return sub, inRange, nil
		}
		index, err := valueToInt(args[0])
		if err != nil {
			return "", false, fmt.Errorf("string.byteslice index must be an integer or range")
		}
		if index < 0 {
			index += len(text)
		}
		if index < 0 || index >= len(text) {
			return "", false, nil
		}
		return text[index : index+1], true, nil
	case 2:
		start, err := valueToInt(args[0])
		if err != nil {
			return "", false, fmt.Errorf("string.byteslice start must be an integer")
		}
		length, err := valueToInt(args[1])
		if err != nil {
			return "", false, fmt.Errorf("string.byteslice length must be an integer")
		}
		if length < 0 {
			return "", false, nil
		}
		if start < 0 {
			start += len(text)
		}
		// A start exactly at the length is valid and yields an empty string; only
		// a start before zero or past the length is out of range.
		if start < 0 || start > len(text) {
			return "", false, nil
		}
		end := start + length
		if end > len(text) || end < start {
			end = len(text)
		}
		return text[start:end], true, nil
	default:
		return "", false, fmt.Errorf("string.byteslice expects an index, a range, or a start and length")
	}
}

// stringByteRangeSlice extracts the byte window selected by a range for
// String#byteslice. It mirrors stringRuneRangeSlice but counts in bytes: a
// begin before the start (after normalization) or past the length yields
// inRange=false (nil), a begin exactly at the length yields an empty string, the
// end bound is clamped to the length, and an end before begin yields an empty
// string.
func stringByteRangeSlice(text string, rng Range) (string, bool) {
	length := int64(len(text))
	if rng.Beginless {
		rng.Start = 0
	}
	if rng.Endless {
		rng.End = length
		rng.Exclusive = true
	}
	begin := rng.Start
	if begin < 0 {
		begin += length
	}
	if begin < 0 || begin > length {
		return "", false
	}
	end := rng.End
	if end < 0 {
		end += length
	}
	if !rng.Exclusive {
		// An inclusive range's exclusive end is one past End; guard the increment so
		// End == math.MaxInt64 cannot wrap to a negative no-op window.
		if end == math.MaxInt64 {
			end = length
		} else {
			end++
		}
	}
	if end > length {
		end = length
	}
	if end < begin {
		end = begin
	}
	return text[begin:end], true
}

// caseMode selects how the case-mapping helpers (upcase, downcase, capitalize,
// swapcase) transform their input. It mirrors Ruby's optional case-mapping
// arguments: the default applies full Unicode mapping, :ascii restricts mapping
// to ASCII letters, and :fold applies Unicode case folding (downcase only).
type caseMode int

const (
	caseModeDefault caseMode = iota
	caseModeASCII
	caseModeFold
)

// parseCaseMode interprets the optional symbol argument shared by upcase,
// downcase, capitalize, and swapcase. Ruby accepts at most one mode here (the
// remaining locale options such as :turkic are out of scope), so more than one
// argument or an argument that is not a recognized symbol is an error. The
// allowFold flag is true only for downcase, matching Ruby's rule that :fold is
// "only allowed for downcasing".
func parseCaseMode(method string, args []Value, allowFold bool) (caseMode, error) {
	if len(args) == 0 {
		return caseModeDefault, nil
	}
	if len(args) > 1 {
		return caseModeDefault, fmt.Errorf("string.%s accepts at most one case-mapping option", method)
	}
	arg := args[0]
	if arg.Kind() != KindSymbol {
		return caseModeDefault, fmt.Errorf("string.%s option must be a symbol", method)
	}
	switch arg.String() {
	case "ascii":
		return caseModeASCII, nil
	case "fold":
		if !allowFold {
			return caseModeDefault, fmt.Errorf("string.%s does not support the :fold option", method)
		}
		return caseModeFold, nil
	default:
		return caseModeDefault, fmt.Errorf("string.%s does not support the :%s option", method, arg.String())
	}
}

// stringUpcase converts text to uppercase. The default mode applies full Unicode
// case mapping (so "ß" becomes "SS" and the "ﬁ" ligature becomes "FI"); the
// :ascii mode and the invalid-UTF-8 fallback restrict mapping to ASCII letters,
// matching Ruby's binary-string behavior.
func stringUpcase(text string, mode caseMode) string {
	if mode == caseModeASCII || !utf8.ValidString(text) {
		return asciiUpcase(text)
	}
	return unicodeUpcase(text)
}

// stringDowncase converts text to lowercase. The default mode applies full
// Unicode case mapping, :fold applies Unicode case folding (e.g. "ß" becomes
// "ss"), and :ascii or invalid UTF-8 restrict mapping to ASCII letters.
func stringDowncase(text string, mode caseMode) string {
	switch {
	case mode == caseModeASCII || !utf8.ValidString(text):
		return asciiDowncase(text)
	case mode == caseModeFold:
		return cases.Fold().String(text)
	default:
		return unicodeDowncase(text)
	}
}

func projectedCaseTransformBytes(text string, mode caseMode) int {
	if mode == caseModeASCII || !utf8.ValidString(text) {
		return len(text)
	}
	return saturatingMul(len(text), 8)
}

func byteBufferScratchBytes(n int) int {
	if n <= 0 {
		return 0
	}
	return saturatingAdd(estimatedSliceBaseBytes, n)
}

func projectedCaseTransformBytesAndScratch(text string, mode caseMode) (int, int) {
	outputBytes := projectedCaseTransformBytes(text, mode)
	if mode == caseModeASCII || !utf8.ValidString(text) {
		return outputBytes, byteBufferScratchBytes(len(text))
	}
	return outputBytes, 0
}

func projectedStringReverseBytes(text string) (int, int) {
	outputBytes := 0
	runeCount := 0
	for len(text) > 0 {
		r, size := utf8.DecodeRuneInString(text)
		outputBytes = saturatingAdd(outputBytes, utf8.RuneLen(r))
		runeCount++
		text = text[size:]
	}
	if runeCount == 0 {
		return outputBytes, 0
	}
	scratchBytes := saturatingAdd(estimatedSliceBaseBytes, saturatingMul(runeCount, estimatedRuneBytes))
	return outputBytes, scratchBytes
}

func checkStringCaseTransform(exec *Execution, text string, mode caseMode, receiver Value, args []Value, kwargs map[string]Value, block Value) error {
	if err := exec.step(); err != nil {
		return err
	}
	outputBytes, scratchBytes := projectedCaseTransformBytesAndScratch(text, mode)
	var b strings.Builder
	return exec.checkProjectedStringBytesAndScratchWithCallRoots(projectedBuilderCap(&b, outputBytes), scratchBytes, receiver, args, kwargs, block)
}

func checkStringReverseTransform(exec *Execution, text string, receiver Value, args []Value, kwargs map[string]Value, block Value) error {
	if err := exec.step(); err != nil {
		return err
	}
	outputBytes, scratchBytes := projectedStringReverseBytes(text)
	return exec.checkProjectedStringBytesAndScratchWithCallRoots(outputBytes, scratchBytes, receiver, args, kwargs, block)
}

// unicodeUpcase applies full Unicode uppercase mapping. A fresh Caser is built
// per call because cases.Caser is not safe for concurrent use, and a host may
// run concurrent Script.Call invocations.
func unicodeUpcase(text string) string {
	return cases.Upper(language.Und).String(text)
}

// unicodeDowncase applies full Unicode lowercase mapping without the Greek
// final-sigma rule. Ruby's default downcase keeps a medial sigma everywhere
// ("ΟΔΟΣ".downcase is "οδοσ", not "οδος"), so final-sigma handling is disabled.
func unicodeDowncase(text string) string {
	return cases.Lower(language.Und, cases.HandleFinalSigma(false)).String(text)
}

// unicodeTitleFirst titlecases a single leading grapheme using full Unicode
// mapping. Ruby's capitalize uses the titlecase mapping for the first character
// (so the "ǆ" digraph becomes "ǅ" rather than "Ǆ"), which differs from a plain
// uppercase. NoLower keeps the call from also lowercasing trailing runes; the
// caller is expected to pass only the first character.
func unicodeTitleFirst(text string) string {
	return cases.Title(language.Und, cases.NoLower).String(text)
}

func asciiUpcase(text string) string {
	out := make([]byte, len(text))
	for i := range len(text) {
		out[i] = asciiUpper(text[i])
	}
	return string(out)
}

func asciiDowncase(text string) string {
	out := make([]byte, len(text))
	for i := range len(text) {
		out[i] = asciiLower(text[i])
	}
	return string(out)
}

func asciiUpper(b byte) byte {
	if b >= 'a' && b <= 'z' {
		return b - ('a' - 'A')
	}
	return b
}

func stringCapitalize(text string, mode caseMode) string {
	if text == "" {
		return ""
	}
	if mode == caseModeASCII || !utf8.ValidString(text) {
		return asciiCapitalize(text)
	}
	r, size := utf8.DecodeRuneInString(text)
	return unicodeTitleFirst(string(r)) + unicodeDowncase(text[size:])
}

// asciiCapitalize uppercases the first byte and lowercases the rest, touching
// only ASCII letters. Non-ASCII bytes (including the leading rune of a UTF-8
// sequence) are left unchanged, matching Ruby's capitalize(:ascii).
func asciiCapitalize(text string) string {
	out := make([]byte, len(text))
	out[0] = asciiUpper(text[0])
	for i := 1; i < len(text); i++ {
		out[i] = asciiLower(text[i])
	}
	return string(out)
}

func stringSwapCase(text string, mode caseMode) string {
	if mode == caseModeASCII || !utf8.ValidString(text) {
		return asciiSwapCase(text)
	}
	var b strings.Builder
	b.Grow(len(text))
	for _, r := range text {
		switch {
		case isUppercaseLike(r):
			b.WriteString(unicodeDowncase(string(r)))
		case isLowercaseLike(r):
			b.WriteString(unicodeUpcase(string(r)))
		default:
			b.WriteRune(r)
		}
	}
	return b.String()
}

// isUppercaseLike reports whether a rune should be lowercased by swapcase. It
// matches uppercase and titlecase letters (Lu/Lt) as well as cased symbols that
// live outside the letter categories yet carry a distinct lowercase mapping,
// such as circled Latin capitals ("Ⓐ") and uppercase Roman numerals ("Ⅰ"),
// which the Is{Upper,Title} predicates miss.
//
// Titlecase digraphs (e.g. "ǅ") are downcased to a single rune ("ǆ"). Ruby
// instead toggles each underlying letter component ("ǅ" -> "dŽ"); reproducing
// that would require hand-encoding Unicode's full case-mapping table (the Greek
// titlecase letters expand the iota subscript to a standalone capital iota), so
// this deliberately diverges from Ruby for those rare codepoints in favor of a
// clean lowercase.
func isUppercaseLike(r rune) bool {
	return unicode.IsUpper(r) || unicode.IsTitle(r) || unicode.ToLower(r) != r
}

// isLowercaseLike reports whether a rune should be uppercased by swapcase. It
// matches lowercase letters (Ll), including those whose single-rune uppercase is
// identical but whose full Unicode mapping expands ("ß" -> "SS"), as well as
// cased symbols outside the letter categories with a distinct uppercase mapping,
// such as circled Latin small letters ("ⓐ") and lowercase Roman numerals
// ("ⅰ"). Uppercase-like runes are excluded by the caller checking
// isUppercaseLike first.
func isLowercaseLike(r rune) bool {
	return unicode.IsLower(r) || unicode.ToUpper(r) != r
}

// asciiSwapCase toggles the case of ASCII letters only, leaving every other byte
// (including multibyte UTF-8 sequences) unchanged. It backs Ruby's
// swapcase(:ascii) and the invalid-UTF-8 fallback for swapcase.
func asciiSwapCase(text string) string {
	out := []byte(text)
	for i, c := range out {
		switch {
		case c >= 'A' && c <= 'Z':
			out[i] = c + ('a' - 'A')
		case c >= 'a' && c <= 'z':
			out[i] = c - ('a' - 'A')
		}
	}
	return string(out)
}

func stringReverse(text string) string {
	runes := []rune(text)
	for i, j := 0, len(runes)-1; i < j; i, j = i+1, j-1 {
		runes[i], runes[j] = runes[j], runes[i]
	}
	return string(runes)
}

func stringRegexOption(method string, kwargs map[string]Value) (bool, error) {
	if len(kwargs) == 0 {
		return false, nil
	}
	regexVal, ok := kwargs["regex"]
	if !ok || len(kwargs) > 1 {
		return false, fmt.Errorf("string.%s supports only regex keyword", method)
	}
	if regexVal.Kind() != KindBool {
		return false, fmt.Errorf("string.%s regex keyword must be bool", method)
	}
	return regexVal.Bool(), nil
}

// validateRegexTextPattern enforces the sandbox size guards before a string
// helper compiles and runs a regex. patternIsRegex reports that pattern is the
// flag-decorated form of a regex value: those values already had their source
// length checked at construction (compileRegexValue), so re-checking the
// decorated form here would count the internal (?i)/(?s) flag prefix against
// the pattern budget and reject a value that =~ accepts. A plain-string pattern
// is compiled fresh, so it is still length-checked.
func validateRegexTextPattern(method, text, pattern string, patternIsRegex bool) error {
	if !patternIsRegex && len(pattern) > maxRegexPatternSize {
		return guardLimitErrorf("%s pattern exceeds limit %d bytes", method, maxRegexPatternSize)
	}
	if len(text) > maxRegexInputBytes {
		return guardLimitErrorf("%s text exceeds limit %d bytes", method, maxRegexInputBytes)
	}
	return nil
}

// regexMatchFromRuneOffset reports whether pattern has a match in text that
// starts at or after the given rune offset, mirroring Ruby's
// Regexp#match?(str, pos). The offset is a rune (codepoint) position; a match
// may begin anywhere from that position onward. Anchors such as \A, ^, and \b
// keep the full-string context: rather than searching a detached suffix (which
// would let \A or \b match at the slice boundary), the search begins one rune
// before the offset so the engine still sees the real character preceding every
// candidate start. Because Go's RE2 has no lookbehind, that single preceding
// rune is the only left context any anchor can observe, so the wrapper stays a
// fixed size regardless of the offset: it never embeds the subject prefix into
// the pattern, keeping the compiled regex small and within the pattern-size
// guard. An offset past the end of text yields no match rather than an error,
// matching Ruby; an invalid pattern is still reported regardless of the offset,
// since the offset only decides the match result, never whether a bad regex is
// accepted. The pattern is compiled with the same guards and cache as
// String#match.
func regexMatchFromRuneOffset(method, text, pattern string, offset int) (bool, error) {
	return regexMatchFromRuneOffsetWithCache(compiledRegexps, method, text, pattern, offset)
}

// regexMatchFromRuneOffsetWithCache implements regexMatchFromRuneOffset against
// an explicit regex cache so tests can assert that the offset wrapper never
// stores an oversized, prefix-bearing pattern.
func regexMatchFromRuneOffsetWithCache(cache *regexCache, method, text, pattern string, offset int) (bool, error) {
	// Compile (and validate) the user pattern first so an invalid regex is always
	// reported, even when the offset lands past the end of the string. The offset
	// must only decide the match result, never whether a bad pattern is accepted.
	re, err := cache.compile(pattern)
	if err != nil {
		return false, fmt.Errorf("%s invalid regex: %w", method, err)
	}
	if offset == 0 {
		return re.MatchString(text), nil
	}
	byteOffset, ok := stringByteIndexForRuneOffset(text, offset)
	if !ok {
		// The offset lands past the final rune, so no match can begin there.
		return false, nil
	}
	// Search a view that begins one rune before the offset. The leading [\s\S]
	// consumes that real preceding rune so \b, \B, and ^ evaluate against it,
	// while \A correctly fails (the view does not start at the absolute string
	// start). The lazy [\s\S]*? then advances to the first candidate start at or
	// after the offset. The wrapper is independent of the prefix length, so it
	// stays small even for offsets deep into a megabyte subject.
	_, ctxSize := utf8.DecodeLastRuneInString(text[:byteOffset])
	ctxStart := byteOffset - ctxSize
	wrapped := `\A[\s\S][\s\S]*?(?:` + pattern + `)`
	re, err = cache.compile(wrapped)
	if err != nil {
		return false, fmt.Errorf("%s invalid regex: %w", method, err)
	}
	return re.MatchString(text[ctxStart:]), nil
}

// regexSubmatchFromRuneOffset returns the submatch indices of the leftmost match
// of pattern in text that starts at or after the given rune offset, mirroring
// Ruby's String#match(str, pos). The result is a flat slice of byte index pairs
// in text laid out exactly like regexp.Regexp.FindStringSubmatchIndex: element 0
// is the whole match (Ruby's group 0), and each subsequent pair is a capture
// group, with -1/-1 for groups that did not participate. A nil result means no
// match begins at or after the offset, which callers translate into Ruby's nil.
//
// The offset is a rune (codepoint) position. As with regexMatchFromRuneOffset,
// the search begins one rune before the offset so anchors such as ^, \b, and \B
// still observe the real preceding character, while \A correctly fails because
// the searched view does not start at the absolute beginning of text. The
// wrapper groups the user pattern in a capturing group so its boundaries and the
// capture indices can be recovered without embedding the subject prefix, keeping
// the compiled pattern small regardless of the offset.
// regexSubexpNames returns the pattern's capture-group names, index-aligned
// with the groups, or nil when the pattern does not compile. Compilation goes
// through the shared cache, so this is a lookup for any pattern already
// matched against.
func regexSubexpNames(pattern string) []string {
	re, err := compiledRegexps.compile(pattern)
	if err != nil {
		return nil
	}
	return re.SubexpNames()
}

func regexSubmatchFromRuneOffset(method, text, pattern string, offset int) ([]int, error) {
	return regexSubmatchFromRuneOffsetWithCache(compiledRegexps, method, text, pattern, offset)
}

// regexSubmatchFromRuneOffsetWithCache implements regexSubmatchFromRuneOffset
// against an explicit regex cache so tests can assert that the offset wrapper
// never stores an oversized, prefix-bearing pattern.
func regexSubmatchFromRuneOffsetWithCache(cache *regexCache, method, text, pattern string, offset int) ([]int, error) {
	// Compile (and validate) the user pattern first so an invalid regex is always
	// reported, even when the offset lands past the end of the string. The offset
	// must only decide the match result, never whether a bad pattern is accepted.
	re, err := cache.compile(pattern)
	if err != nil {
		return nil, fmt.Errorf("%s invalid regex: %w", method, err)
	}
	if offset == 0 {
		return re.FindStringSubmatchIndex(text), nil
	}
	byteOffset, ok := stringByteIndexForRuneOffset(text, offset)
	if !ok {
		// The offset lands past the final rune, so no match can begin there.
		return nil, nil
	}
	// Search a view that begins one rune before the offset, capturing the user
	// pattern so its real boundaries survive the leading-context skip. The leading
	// [\s\S] consumes the real preceding rune so \b, \B, and ^ evaluate against it,
	// while \A correctly fails (the view does not start at the absolute string
	// start). The lazy [\s\S]*? then advances to the first candidate start at or
	// after the offset. The wrapper is independent of the prefix length, so it
	// stays small even for offsets deep into a megabyte subject.
	_, ctxSize := utf8.DecodeLastRuneInString(text[:byteOffset])
	ctxStart := byteOffset - ctxSize
	wrapped := `\A[\s\S][\s\S]*?(` + pattern + `)`
	wrappedRe, err := cache.compile(wrapped)
	if err != nil {
		return nil, fmt.Errorf("%s invalid regex: %w", method, err)
	}
	indices := wrappedRe.FindStringSubmatchIndex(text[ctxStart:])
	if indices == nil {
		return nil, nil
	}
	// Drop the wrapper's whole-match pair (the leading context plus the user
	// match) and re-base the remaining pairs onto text. The wrapper's group 1 is
	// the user's whole match (Ruby's group 0); each later pair is a user capture.
	userIndices := indices[2:]
	rebased := make([]int, len(userIndices))
	for i, idx := range userIndices {
		if idx < 0 {
			rebased[i] = idx
			continue
		}
		rebased[i] = idx + ctxStart
	}
	return rebased, nil
}

func validateRegexReplacement(method, replacement string) error {
	if len(replacement) > maxRegexInputBytes {
		return guardLimitErrorf("%s replacement exceeds limit %d bytes", method, maxRegexInputBytes)
	}
	return nil
}

func validateLiteralReplacement(method, text, pattern, replacement string, all bool) (bool, error) {
	count := 0
	if pattern == "" {
		count = 1
		if all {
			count = utf8.RuneCountInString(text) + 1
		}
	} else if all {
		count = strings.Count(text, pattern)
	} else if strings.Contains(text, pattern) {
		count = 1
	}
	if count == 0 {
		return false, nil
	}
	if pattern == replacement {
		return true, nil
	}
	if len(replacement) > maxRegexInputBytes {
		return false, guardLimitErrorf("%s replacement exceeds limit %d bytes", method, maxRegexInputBytes)
	}
	outputLen := len(text)
	consumed := saturatingMul(count, len(pattern))
	if consumed > outputLen {
		outputLen = 0
	} else {
		outputLen -= consumed
	}
	outputLen = saturatingAdd(outputLen, saturatingMul(count, len(replacement)))
	if outputLen > maxRegexInputBytes {
		return true, guardLimitErrorf("%s output exceeds limit %d bytes", method, maxRegexInputBytes)
	}
	return true, nil
}

func stringSub(method, text, pattern, replacement string, regex, patternIsRegex bool) (string, bool, error) {
	if !regex {
		matched, err := validateLiteralReplacement(method, text, pattern, replacement, false)
		if err != nil {
			return "", false, err
		}
		if matched && pattern == replacement {
			return text, true, nil
		}
		return strings.Replace(text, pattern, replacement, 1), matched, nil
	}
	if err := validateRegexTextPattern(method, text, pattern, patternIsRegex); err != nil {
		return "", false, err
	}
	if err := validateRegexReplacement(method, replacement); err != nil {
		return "", false, err
	}
	re, err := compileCachedRegex(pattern)
	if err != nil {
		return "", false, fmt.Errorf("%s invalid regex: %w", method, err)
	}
	return rubyRegexSub(re, text, replacement, method)
}

func stringGSub(method, text, pattern, replacement string, regex, patternIsRegex bool) (string, bool, error) {
	if !regex {
		matched, err := validateLiteralReplacement(method, text, pattern, replacement, true)
		if err != nil {
			return "", false, err
		}
		if matched && pattern == replacement {
			return text, true, nil
		}
		return strings.ReplaceAll(text, pattern, replacement), matched, nil
	}
	if err := validateRegexTextPattern(method, text, pattern, patternIsRegex); err != nil {
		return "", false, err
	}
	if err := validateRegexReplacement(method, replacement); err != nil {
		return "", false, err
	}
	re, err := compileCachedRegex(pattern)
	if err != nil {
		return "", false, fmt.Errorf("%s invalid regex: %w", method, err)
	}
	return rubyRegexGSub(re, text, replacement, method)
}

// compileStringPatternRegex compiles a regex pattern for the block forms of
// String#sub and String#gsub. It is only used for the regex form (regex ==
// true); the literal form (regex == false) bypasses regexp entirely via
// literalBlockReplace so it stays byte-for-byte consistent with the literal
// template path, including for patterns that hold invalid UTF-8 (which Go's
// regexp engine rejects). The pattern and text are size-checked first so an
// oversized subject or pattern is rejected before compilation.
func compileStringPatternRegex(method, text, pattern string, patternIsRegex bool) (*regexp.Regexp, error) {
	if err := validateRegexTextPattern(method, text, pattern, patternIsRegex); err != nil {
		return nil, err
	}
	re, err := compileCachedRegex(pattern)
	if err != nil {
		return nil, fmt.Errorf("%s invalid regex: %w", method, err)
	}
	return re, nil
}

// stringSubBlock implements the block form of String#sub: it replaces the first
// match with the string form of the value the block returns for that match,
// yielding the whole matched substring to the block (Ruby's group 0). yield
// charges the sandbox step quota and invokes the user block per match. It returns
// whether a match was found so the bang form can decide its return value.
//
// A literal pattern (regex == false) is matched byte-for-byte rather than via
// regexp so it behaves identically to the literal template form, including for
// patterns and subjects that hold invalid UTF-8. The literal path imposes none of
// the regex-only pattern/input size caps, matching the literal template form
// (strings.Replace), which has no such limits; only the regex form validates.
func stringSubBlock(method, text, pattern string, regex, patternIsRegex bool, yield func(match string) (string, error)) (string, bool, error) {
	if !regex {
		return literalBlockReplace(text, pattern, false, yield)
	}
	re, err := compileStringPatternRegex(method, text, pattern, patternIsRegex)
	if err != nil {
		return "", false, err
	}
	return rubyRegexSubWith(re, text, method, rubyBlockReplacer(text, yield))
}

// stringGSubBlock implements the block form of String#gsub: it replaces every
// match with the string form of the value the block returns for each match,
// yielding the whole matched substring to the block (Ruby's group 0). yield
// charges the sandbox step quota and invokes the user block per match. It returns
// whether at least one match was found so the bang form can decide its return
// value.
//
// A literal pattern (regex == false) is matched byte-for-byte rather than via
// regexp so it behaves identically to the literal template form, including for
// patterns and subjects that hold invalid UTF-8. The literal path imposes none of
// the regex-only pattern/input size caps, matching the literal template form
// (strings.ReplaceAll), which has no such limits; only the regex form validates.
func stringGSubBlock(method, text, pattern string, regex, patternIsRegex bool, yield func(match string) (string, error)) (string, bool, error) {
	if !regex {
		return literalBlockReplace(text, pattern, true, yield)
	}
	re, err := compileStringPatternRegex(method, text, pattern, patternIsRegex)
	if err != nil {
		return "", false, err
	}
	return rubyRegexGSubWith(re, text, method, rubyBlockReplacer(text, yield))
}

// boundedReplacementString renders a sub/gsub block result into its replacement
// string under the shared 1 MiB regex output cap. Value.String()'s composite
// rendering is intentionally unbounded, so a block returning a large or
// deeply-aggregate array/hash would materialize the whole multi-MiB rendering in
// memory before rubyBlockReplacer's appendBounded guard (which only inspects the
// already-built string) could see it. StringBounded stops once the rendering would
// exceed maxRegexInputBytes and reports the truncation, which this surfaces as the
// same "output exceeds limit" error the rest of the regex output guards raise, so
// the block form refuses an over-cap replacement without first allocating it.
func boundedReplacementString(exec *Execution, result Value) (string, error) {
	replacement, err := result.StringBounded(maxRegexInputBytes)
	if err != nil {
		if errors.Is(err, errStringRenderTruncated) {
			// The renderer copied up to the limit before giving up, and this
			// error is rescuable, so a script could return an oversized
			// aggregate every match and pay only the per-match step. Charge the
			// bytes it rendered before reporting the limit.
			if chargeErr := exec.chargeStringScan(maxRegexInputBytes); chargeErr != nil {
				return "", chargeErr
			}
			return "", guardLimitErrorf("output exceeds limit %d bytes", maxRegexInputBytes)
		}
		if errors.Is(err, value.ErrStringRenderDepthExceeded) {
			if chargeErr := exec.chargeStringScan(len(replacement)); chargeErr != nil {
				return "", chargeErr
			}
			return "", &guardLimitError{err: err}
		}
		return "", err
	}
	return replacement, nil
}

// stringReplaceBlockYield builds the per-match callback rubyBlockReplacer needs
// from a user block: it charges one step per match (so a flood of matches cannot
// starve the step quota or cancellation checks), invokes the block with the
// matched substring, and returns the block result's bounded string form via
// boundedReplacementString. It is shared by the block forms of String#sub and
// String#gsub.
func stringReplaceBlockYield(exec *Execution, runner *blockCallRunner) func(match string) (string, error) {
	var blockArg [1]Value
	return func(match string) (string, error) {
		if err := exec.step(); err != nil {
			return "", err
		}
		blockArg[0] = NewString(match)
		result, err := runner.call(blockArg[:])
		if err != nil {
			return "", err
		}
		replacement, err := boundedReplacementString(exec, result)
		if err != nil {
			return "", err
		}
		// Charged per replacement as it is accepted, not once on the way out.
		// A block that raises on a later match still copied the replacements it
		// already returned, and the error return skips any charge placed after
		// the loop -- so a script could rescue that error and repeat the copying
		// for nothing.
		if err := exec.chargeStringScan(len(replacement)); err != nil {
			return "", err
		}
		return replacement, nil
	}
}

// stringReplaceResult drives String#sub, String#sub!, String#gsub, and
// String#gsub!, dispatching between the template form (pattern plus replacement
// string) and the block form (pattern plus a replacement block). global selects
// gsub-style all-match replacement over sub-style first-match replacement, and
// the template/block functions carry that distinction into the shared regex
// helpers. The pattern is always required and must be a string.
//
// It returns the rewritten text and whether a match occurred. The match flag,
// not a byte comparison of the result, drives the bang forms: Ruby's
// String#sub!/String#gsub! return the receiver whenever a substitution was
// performed -- even one that reproduces the original text, such as
// "a".sub!("a") { |m| m } or "abc".gsub!("", "") -- and return nil only when the
// pattern never matched.
//
// Passing both a replacement argument and a block, or supplying neither, is
// rejected: a block form takes only the pattern, while the template form takes
// the pattern and a string replacement. Rejecting the mixed form keeps the two
// replacement sources from silently disagreeing, matching the issue's "invalid
// mixed replacement-argument plus block" requirement.
func stringReplaceResult(
	exec *Execution,
	method string,
	receiver Value,
	args []Value,
	kwargs map[string]Value,
	block Value,
	global bool,
) (string, bool, error) {
	regex, err := stringRegexOption(strings.TrimPrefix(method, "string."), kwargs)
	if err != nil {
		return "", false, err
	}
	if len(args) < 1 {
		return "", false, fmt.Errorf("%s expects a pattern", method)
	}
	pattern, patternIsRegex, err := stringPatternArgument(method, args[0])
	if err != nil {
		return "", false, err
	}
	if patternIsRegex {
		// A regex pattern selects regex matching by itself; the regex keyword
		// exists only to opt a plain-string pattern into regex mode, so mixing
		// the two would let them silently disagree (regex: false with a regex
		// pattern has no sensible meaning).
		if _, present := kwargs["regex"]; present {
			return "", false, fmt.Errorf("%s does not take the regex keyword with a regex pattern", method)
		}
		regex = true
	}
	text := receiver.String()

	if valueBlock(block) != nil {
		if len(args) != 1 {
			return "", false, fmt.Errorf("%s cannot take both a replacement argument and a block", method)
		}
		runner, err := newBlockCallRunner(exec, block, method, receiver, args, kwargs)
		if err != nil {
			return "", false, err
		}
		yield := stringReplaceBlockYield(exec, runner)
		var blockOut string
		var blockMatched bool
		if global {
			blockOut, blockMatched, err = stringGSubBlock(method, text, pattern, regex, patternIsRegex, yield)
		} else {
			blockOut, blockMatched, err = stringSubBlock(method, text, pattern, regex, patternIsRegex, yield)
		}
		if err != nil {
			return "", false, err
		}
		// No charge here: the replacements were charged as the block returned
		// them (see stringReplaceBlockYield), which covers the paths that raise
		// partway through, and the receiver's own bytes are charged by the call
		// wrapper. Charging the assembled output again would bill them twice.
		return blockOut, blockMatched, nil
	}

	if len(args) != 2 {
		return "", false, fmt.Errorf("%s expects pattern and replacement", method)
	}
	if args[1].Kind() != KindString {
		return "", false, fmt.Errorf("%s replacement must be string", method)
	}
	var out string
	var matched bool
	if global {
		out, matched, err = stringGSub(method, text, pattern, args[1].String(), regex, patternIsRegex)
	} else {
		out, matched, err = stringSub(method, text, pattern, args[1].String(), regex, patternIsRegex)
	}
	if err != nil {
		return "", false, err
	}
	// A substitution can expand: replacing every byte of a 16 KiB receiver with
	// a 63-byte replacement writes a megabyte, which neither the receiver nor
	// the replacement bounds. The result is only measurable once built, so the
	// charge follows the work and one call may overshoot before the quota
	// fires -- the same trade the set-probe charge makes, and the memory check
	// on the way out bounds the overshoot's size.
	if err := exec.chargeStringScan(len(out)); err != nil {
		return "", false, err
	}
	return out, matched, nil
}

// stringReplaceBangResult builds the return value for String#sub! and
// String#gsub!: the rewritten string when a substitution was performed,
// otherwise nil. Unlike stringBangResult (used by the in-place transforms whose
// "no change" genuinely means "no effect"), sub!/gsub! key off whether the
// pattern matched, so a substitution that reproduces the original text still
// returns the receiver rather than nil, matching Ruby.
func stringReplaceBangResult(updated string, matched bool) Value {
	if !matched {
		return NewNil()
	}
	return NewString(updated)
}

func stringBangResult(original, updated string) Value {
	if updated == original {
		return NewNil()
	}
	return NewString(updated)
}

type stringCharSetSpec struct {
	negated bool
	entries []stringCharSetEntry
	length  int
}

type stringCharSetEntry struct {
	lowRune, highRune rune
	lowByte, highByte byte
	rawBytes          bool
}

type stringCharSetToken struct {
	r        rune
	rawByte  byte
	rawBytes bool
}

func parseStringCharSetArgs(method string, args []Value, allowComplement bool) ([]stringCharSetSpec, error) {
	specs := make([]stringCharSetSpec, len(args))
	for i, arg := range args {
		if arg.Kind() != KindString {
			return nil, fmt.Errorf("%s character set must be string", method)
		}
		spec, err := parseStringCharSet(method, arg.String(), allowComplement)
		if err != nil {
			return nil, err
		}
		specs[i] = spec
	}
	return specs, nil
}

func parseStringCharSet(method, text string, allowComplement bool) (stringCharSetSpec, error) {
	tokens := tokenizeStringCharSet(text)
	var spec stringCharSetSpec
	pos := 0
	if allowComplement && len(tokens) > 1 && tokens[0].isRune('^') {
		spec.negated = true
		pos = 1
	}
	for pos < len(tokens) {
		start, next := nextStringCharSetToken(tokens, pos)
		if next < len(tokens) && tokens[next].isRune('-') && next+1 < len(tokens) {
			end, after := nextStringCharSetToken(tokens, next+1)
			if err := spec.addRange(method, start, end); err != nil {
				return stringCharSetSpec{}, err
			}
			pos = after
			continue
		}
		spec.addToken(start)
		pos = next
	}
	return spec, nil
}

func tokenizeStringCharSet(text string) []stringCharSetToken {
	tokens := make([]stringCharSetToken, 0, utf8.RuneCountInString(text))
	for i := 0; i < len(text); {
		seg := nextStringRuneSegment(text, i)
		i = seg.end
		if stringRuneSegmentInvalidByte(seg) {
			tokens = append(tokens, stringCharSetToken{rawByte: text[seg.start], rawBytes: true})
			continue
		}
		tokens = append(tokens, stringCharSetToken{r: seg.r})
	}
	return tokens
}

func nextStringCharSetToken(tokens []stringCharSetToken, pos int) (stringCharSetToken, int) {
	if tokens[pos].isRune('\\') {
		if pos+1 < len(tokens) {
			return tokens[pos+1], pos + 2
		}
		return stringCharSetToken{r: '\\'}, pos + 1
	}
	return tokens[pos], pos + 1
}

func (t stringCharSetToken) isRune(r rune) bool {
	return !t.rawBytes && t.r == r
}

func (s *stringCharSetSpec) addToken(token stringCharSetToken) {
	if token.rawBytes {
		s.addByteSpan(token.rawByte, token.rawByte)
		return
	}
	s.addRuneSpan(token.r, token.r)
}

func (s *stringCharSetSpec) addRange(method string, start, end stringCharSetToken) error {
	switch {
	case start.rawBytes && end.rawBytes:
		if start.rawByte > end.rawByte {
			return fmt.Errorf("%s invalid character range %02x-%02x", method, start.rawByte, end.rawByte)
		}
		s.addByteSpan(start.rawByte, end.rawByte)
	case !start.rawBytes && !end.rawBytes:
		if start.r > end.r {
			return fmt.Errorf("%s invalid character range %c-%c", method, start.r, end.r)
		}
		s.addRuneSpan(start.r, end.r)
	default:
		return fmt.Errorf("%s invalid mixed byte/rune character range", method)
	}
	return nil
}

func (s *stringCharSetSpec) addRuneSpan(low, high rune) {
	s.entries = append(s.entries, stringCharSetEntry{lowRune: low, highRune: high})
	s.length = saturatingAdd(s.length, int(high-low)+1)
}

func (s *stringCharSetSpec) addByteSpan(low, high byte) {
	s.entries = append(s.entries, stringCharSetEntry{lowByte: low, highByte: high, rawBytes: true})
	s.length = saturatingAdd(s.length, int(high-low)+1)
}

func (e stringCharSetEntry) length() int {
	if e.rawBytes {
		return int(e.highByte-e.lowByte) + 1
	}
	return int(e.highRune-e.lowRune) + 1
}

func (e stringCharSetEntry) containsSegment(text string, seg stringRuneSegment) bool {
	if e.rawBytes {
		if !stringRuneSegmentInvalidByte(seg) {
			return false
		}
		b := text[seg.start]
		return b >= e.lowByte && b <= e.highByte
	}
	if stringRuneSegmentInvalidByte(seg) {
		return false
	}
	return seg.r >= e.lowRune && seg.r <= e.highRune
}

func (e stringCharSetEntry) indexOfSegment(text string, seg stringRuneSegment) (int, bool) {
	if !e.containsSegment(text, seg) {
		return 0, false
	}
	if e.rawBytes {
		return int(text[seg.start] - e.lowByte), true
	}
	return int(seg.r - e.lowRune), true
}

// containsSegment reports whether seg falls inside the set, and how many
// entries it compared to decide. Every character-set lookup returns that probe
// count so the caller can bill it: the entry list grows with the caller's
// character-set argument while the surrounding loop charges one step per
// receiver character, so the comparisons themselves are the only part of the
// cost that is not already metered (see chargeStringCharSetProbes, #26).
func (s stringCharSetSpec) containsSegment(text string, seg stringRuneSegment) (bool, int) {
	for i, entry := range s.entries {
		if entry.containsSegment(text, seg) {
			return true, i + 1
		}
	}
	return false, len(s.entries)
}

func (s stringCharSetSpec) matchesSegment(text string, seg stringRuneSegment) (bool, int) {
	matched, probes := s.containsSegment(text, seg)
	if s.negated {
		return !matched, probes
	}
	return matched, probes
}

func (s stringCharSetSpec) orderedIndexSegment(text string, seg stringRuneSegment) (int, bool, int) {
	if s.negated {
		matched, probes := s.containsSegment(text, seg)
		return math.MaxInt, !matched, probes
	}
	index := 0
	matchIndex := 0
	matched := false
	for _, entry := range s.entries {
		count := entry.length()
		if offset, ok := entry.indexOfSegment(text, seg); ok {
			matchIndex = saturatingAdd(index, offset)
			matched = true
		}
		index = saturatingAdd(index, count)
	}
	if matched {
		return matchIndex, true, len(s.entries)
	}
	return 0, false, len(s.entries)
}

func (s stringCharSetSpec) tokenAt(index int) (stringCharSetToken, int) {
	if index < 0 {
		index = 0
	}
	if index >= s.length {
		index = s.length - 1
	}
	for i, entry := range s.entries {
		count := entry.length()
		if index < count {
			if entry.rawBytes {
				return stringCharSetToken{rawByte: entry.lowByte + byte(index), rawBytes: true}, i + 1
			}
			return stringCharSetToken{r: entry.lowRune + rune(index)}, i + 1
		}
		index -= count
	}
	return stringCharSetToken{}, len(s.entries)
}

func stringCharSetsMatchSegment(specs []stringCharSetSpec, text string, seg stringRuneSegment) (bool, int) {
	probes := 0
	for _, spec := range specs {
		matched, n := spec.matchesSegment(text, seg)
		probes += n
		if !matched {
			return false, probes
		}
	}
	return true, probes
}

func stringRuneBytes(r rune) int {
	if n := utf8.RuneLen(r); n > 0 {
		return n
	}
	return len(string(r))
}

type stringRuneSegment struct {
	r          rune
	start, end int
}

func nextStringRuneSegment(text string, start int) stringRuneSegment {
	r, size := utf8.DecodeRuneInString(text[start:])
	return stringRuneSegment{r: r, start: start, end: start + size}
}

func stringRuneSegmentInvalidByte(seg stringRuneSegment) bool {
	return seg.r == utf8.RuneError && seg.end-seg.start == 1
}

func stringRuneSegmentsEqual(text string, left, right stringRuneSegment) bool {
	if left.r != right.r {
		return false
	}
	if left.r != utf8.RuneError {
		return true
	}
	return text[left.start:left.end] == text[right.start:right.end]
}

func stringCharSetTokenBytes(token stringCharSetToken) int {
	if token.rawBytes {
		return 1
	}
	return stringRuneBytes(token.r)
}

func writeStringCharSetToken(b *strings.Builder, token stringCharSetToken) {
	if token.rawBytes {
		b.WriteByte(token.rawByte)
		return
	}
	b.WriteRune(token.r)
}

func stringCharSetArgsScratchBytes(args []Value) int {
	if len(args) == 0 {
		return 0
	}
	specBytes := estimatedValueBytes + estimatedSliceBaseBytes + estimatedIntBytes + estimatedValueBytes
	entryBytes := saturatingAdd(saturatingMul(2, estimatedRuneBytes), estimatedValueBytes)
	total := saturatingAdd(estimatedSliceBaseBytes, saturatingMul(len(args), specBytes))
	for _, arg := range args {
		if arg.Kind() != KindString {
			continue
		}
		spanCount := utf8.RuneCountInString(arg.String())
		total = saturatingAdd(total, estimatedSliceBaseBytes)
		total = saturatingAdd(total, saturatingMul(spanCount, entryBytes))
	}
	return total
}

func reserveStringCharSetScratch(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (func(), error) {
	scratch := stringCharSetArgsScratchBytes(args)
	if scratch == 0 {
		return func() {}, nil
	}
	delta := exec.reserveLoopScratch(scratch)
	release := func() {
		exec.releaseLoopScratch(delta)
	}
	if err := exec.checkReservedLoopScratch(receiver, args, kwargs, block); err != nil {
		release()
		return nil, err
	}
	return release, nil
}

// stringScanBytesPerStep is the number of haystack bytes one sandbox step
// covers when a string operation scans its receiver.
//
// A sequential string scan is O(n) in the receiver but dispatches as a single
// method call, so charging it a flat handful of steps let a script scan a
// host-supplied string of any size for a constant budget: 100k iterations of
// s.count over an 800 KB string burned 12.6s inside the default 1M-step profile
// before the quota fired (#1131). The rate matches bigIntStepWordsPerStep,
// which covers 8 machine words -- the same 64 bytes -- so byte-oriented traffic
// is charged consistently wherever it is metered. A byte comparison is far
// cheaper than the Value comparison an array scan charges per element, which is
// why this is amortized rather than one step per byte: charging per byte would
// fail existing scripts that scan large strings inside a tuned quota, while
// this bounds the work without changing the cost of ordinary short strings.
const stringScanBytesPerStep = 64

// chargeStringScan charges the step quota for a scan over n bytes of receiver.
// Scanning nothing costs nothing; stepN charges a step even for a count of
// zero, and a scan short enough to round down to zero steps is already bounded
// by the caller's own per-call charge.
func (exec *Execution) chargeStringScan(n int) error {
	// A nil execution has no quota to charge against: some builtins are
	// reachable without one, and callers should not each have to remember that.
	if exec == nil {
		return nil
	}
	steps := n / stringScanBytesPerStep
	if steps <= 0 {
		return nil
	}
	return exec.stepN(steps)
}

// chargeStringCharSetProbes bills n character-set entry comparisons against the
// step quota, carrying the sub-step remainder on the execution.
//
// count, delete, tr and squeeze charge one step per receiver character but
// compare that character against every entry of every character set, so their
// real cost is the product of the two arguments while their charge follows only
// one of them. Over a fixed 100 KB receiver, growing the character set from 1
// entry to 8192 moved count from 1.8ms to 750ms and tr from 1.6ms to 5.9s, and
// both charged exactly 100,000 steps either way (#26). What one step bought
// therefore grew with the caller's argument: 7.5us of tr at 1024 entries and
// 59us at 8192, against 232ns once the comparisons are billed. An entry
// comparison is a bounds check on one rune, so it bills at
// stringScanBytesPerStep like any other byte-scale scan.
//
// Rounding each call down on its own would bill nothing at all here: a single
// segment probes far fewer entries than one step covers, so every charge would
// truncate to zero and the product would stay free. Carrying the remainder the
// way chargeEqualityScanBytes does settles whole steps as the per-segment tails
// accumulate, which leaves ordinary calls -- "hello".delete("l") probes five
// entries in total -- costing nothing.
func (exec *Execution) chargeStringCharSetProbes(n int) error {
	if exec == nil || n <= 0 {
		return nil
	}
	exec.charSetProbeResidue += n
	steps := exec.charSetProbeResidue / stringScanBytesPerStep
	if steps <= 0 {
		return nil
	}
	exec.charSetProbeResidue -= steps * stringScanBytesPerStep
	return exec.stepN(steps)
}

func stringCountChars(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (int, error) {
	const method = "string.count"
	if len(args) == 0 {
		return 0, fmt.Errorf("%s expects at least one character set", method)
	}
	if len(kwargs) > 0 {
		return 0, fmt.Errorf("%s does not take keyword arguments", method)
	}
	if valueBlock(block) != nil {
		return 0, fmt.Errorf("%s does not accept a block", method)
	}
	releaseScratch, err := reserveStringCharSetScratch(exec, receiver, args, kwargs, block)
	if err != nil {
		return 0, err
	}
	defer releaseScratch()
	specs, err := parseStringCharSetArgs(method, args, true)
	if err != nil {
		return 0, err
	}
	count := 0
	text := receiver.String()
	for i := 0; i < len(text); {
		seg := nextStringRuneSegment(text, i)
		i = seg.end
		if err := exec.step(); err != nil {
			return 0, err
		}
		matched, probes := stringCharSetsMatchSegment(specs, text, seg)
		if err := exec.chargeStringCharSetProbes(probes); err != nil {
			return 0, err
		}
		if matched {
			count++
		}
	}
	return count, nil
}

func stringDeleteChars(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value, method string) (string, error) {
	if len(args) == 0 {
		return "", fmt.Errorf("%s expects at least one character set", method)
	}
	if len(kwargs) > 0 {
		return "", fmt.Errorf("%s does not take keyword arguments", method)
	}
	if valueBlock(block) != nil {
		return "", fmt.Errorf("%s does not accept a block", method)
	}
	releaseScratch, err := reserveStringCharSetScratch(exec, receiver, args, kwargs, block)
	if err != nil {
		return "", err
	}
	defer releaseScratch()
	specs, err := parseStringCharSetArgs(method, args, true)
	if err != nil {
		return "", err
	}
	text := receiver.String()
	projectedBytes := 0
	for i := 0; i < len(text); {
		seg := nextStringRuneSegment(text, i)
		i = seg.end
		if err := exec.step(); err != nil {
			return "", err
		}
		matched, probes := stringCharSetsMatchSegment(specs, text, seg)
		if err := exec.chargeStringCharSetProbes(probes); err != nil {
			return "", err
		}
		if !matched {
			projectedBytes = saturatingAdd(projectedBytes, seg.end-seg.start)
		}
	}
	if err := exec.checkProjectedStringBytesWithCallRoots(projectedBytes, receiver, args, kwargs, block); err != nil {
		return "", err
	}
	var b strings.Builder
	b.Grow(projectedBytes)
	// The output pass repeats every comparison the sizing pass made, so it is
	// charged again rather than riding free on the first pass's charge (#26).
	for i := 0; i < len(text); {
		seg := nextStringRuneSegment(text, i)
		i = seg.end
		matched, probes := stringCharSetsMatchSegment(specs, text, seg)
		if err := exec.chargeStringCharSetProbes(probes); err != nil {
			return "", err
		}
		if !matched {
			b.WriteString(text[seg.start:seg.end])
		}
	}
	return b.String(), nil
}

func stringTrChars(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value, method string) (string, error) {
	if len(args) != 2 {
		return "", fmt.Errorf("%s expects source and replacement character sets", method)
	}
	if len(kwargs) > 0 {
		return "", fmt.Errorf("%s does not take keyword arguments", method)
	}
	if valueBlock(block) != nil {
		return "", fmt.Errorf("%s does not accept a block", method)
	}
	if args[0].Kind() != KindString || args[1].Kind() != KindString {
		return "", fmt.Errorf("%s character sets must be strings", method)
	}
	releaseScratch, err := reserveStringCharSetScratch(exec, receiver, args, kwargs, block)
	if err != nil {
		return "", err
	}
	defer releaseScratch()
	source, err := parseStringCharSet(method, args[0].String(), true)
	if err != nil {
		return "", err
	}
	replacement, err := parseStringCharSet(method, args[1].String(), false)
	if err != nil {
		return "", err
	}
	text := receiver.String()
	projectedBytes := 0
	for i := 0; i < len(text); {
		seg := nextStringRuneSegment(text, i)
		i = seg.end
		if err := exec.step(); err != nil {
			return "", err
		}
		index, matched, probes := source.orderedIndexSegment(text, seg)
		if err := exec.chargeStringCharSetProbes(probes); err != nil {
			return "", err
		}
		if !matched {
			projectedBytes = saturatingAdd(projectedBytes, seg.end-seg.start)
			continue
		}
		if replacement.length == 0 {
			continue
		}
		token, tokenProbes := replacement.tokenAt(index)
		if err := exec.chargeStringCharSetProbes(tokenProbes); err != nil {
			return "", err
		}
		projectedBytes = saturatingAdd(projectedBytes, stringCharSetTokenBytes(token))
	}
	if err := exec.checkProjectedStringBytesWithCallRoots(projectedBytes, receiver, args, kwargs, block); err != nil {
		return "", err
	}
	var b strings.Builder
	b.Grow(projectedBytes)
	// The output pass repeats every comparison the sizing pass made, so it is
	// charged again rather than riding free on the first pass's charge (#26).
	for i := 0; i < len(text); {
		seg := nextStringRuneSegment(text, i)
		i = seg.end
		index, matched, probes := source.orderedIndexSegment(text, seg)
		if err := exec.chargeStringCharSetProbes(probes); err != nil {
			return "", err
		}
		switch {
		case !matched:
			b.WriteString(text[seg.start:seg.end])
		case replacement.length > 0:
			token, tokenProbes := replacement.tokenAt(index)
			if err := exec.chargeStringCharSetProbes(tokenProbes); err != nil {
				return "", err
			}
			writeStringCharSetToken(&b, token)
		}
	}
	return b.String(), nil
}

func stringSqueezeChars(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value, method string) (string, error) {
	if len(kwargs) > 0 {
		return "", fmt.Errorf("%s does not take keyword arguments", method)
	}
	if valueBlock(block) != nil {
		return "", fmt.Errorf("%s does not accept a block", method)
	}
	releaseScratch, err := reserveStringCharSetScratch(exec, receiver, args, kwargs, block)
	if err != nil {
		return "", err
	}
	defer releaseScratch()
	specs, err := parseStringCharSetArgs(method, args, true)
	if err != nil {
		return "", err
	}
	text := receiver.String()
	projectedBytes := 0
	var previous stringRuneSegment
	hasPrevious := false
	for i := 0; i < len(text); {
		seg := nextStringRuneSegment(text, i)
		i = seg.end
		if err := exec.step(); err != nil {
			return "", err
		}
		squeezed, err := stringSqueezeSegment(exec, specs, text, seg, previous, hasPrevious)
		if err != nil {
			return "", err
		}
		if !squeezed {
			projectedBytes = saturatingAdd(projectedBytes, seg.end-seg.start)
		}
		previous = seg
		hasPrevious = true
	}
	if err := exec.checkProjectedStringBytesWithCallRoots(projectedBytes, receiver, args, kwargs, block); err != nil {
		return "", err
	}
	var b strings.Builder
	b.Grow(projectedBytes)
	hasPrevious = false
	// The output pass repeats every comparison the sizing pass made, so it is
	// charged again rather than riding free on the first pass's charge (#26).
	for i := 0; i < len(text); {
		seg := nextStringRuneSegment(text, i)
		i = seg.end
		squeezed, err := stringSqueezeSegment(exec, specs, text, seg, previous, hasPrevious)
		if err != nil {
			return "", err
		}
		if !squeezed {
			b.WriteString(text[seg.start:seg.end])
		}
		previous = seg
		hasPrevious = true
	}
	return b.String(), nil
}

// stringSqueezeSegment reports whether seg collapses into the segment before
// it, charging the character-set comparisons the decision needs (#26). A run
// only collapses when the repeated character is in every set, so the sets are
// consulted -- and billed -- once per repeat.
func stringSqueezeSegment(exec *Execution, specs []stringCharSetSpec, text string, seg, previous stringRuneSegment, hasPrevious bool) (bool, error) {
	if !hasPrevious || !stringRuneSegmentsEqual(text, seg, previous) {
		return false, nil
	}
	if len(specs) == 0 {
		return true, nil
	}
	matched, probes := stringCharSetsMatchSegment(specs, text, seg)
	if err := exec.chargeStringCharSetProbes(probes); err != nil {
		return false, err
	}
	return matched, nil
}

// isRubyStripSpace reports whether b is one of the ASCII whitespace bytes that
// Ruby's strip family removes from either edge: the NUL byte, horizontal tab,
// newline, vertical tab, form feed, carriage return, and space. Ruby's String
// docs define this same set for strip, lstrip, and rstrip alike. Unlike Go's
// unicode.IsSpace it never matches multibyte Unicode spaces (NBSP, Ogham space
// mark, em space, BOM, ...), which Ruby intentionally preserves.
func isRubyStripSpace(b byte) bool {
	switch b {
	case 0x00, '\t', '\n', '\v', '\f', '\r', ' ':
		return true
	default:
		return false
	}
}

// rubyLstrip trims leading Ruby strip-family whitespace (including NUL) from
// text.
func rubyLstrip(text string) string {
	end := len(text)
	if whitespaceSIMD && end > whitespaceProbeBytes {
		end = whitespaceProbeBytes
	}
	start := 0
	for start < end && isRubyStripSpace(text[start]) {
		start++
	}
	if whitespaceSIMD && start == whitespaceProbeBytes && start < len(text) {
		start += whitespacePrefixSIMD(text[start:], true)
	}
	return text[start:]
}

// rubyRstrip trims trailing Ruby strip-family whitespace (including NUL) from
// text.
func rubyRstrip(text string) string {
	start := 0
	if whitespaceSIMD && len(text) > whitespaceProbeBytes {
		start = len(text) - whitespaceProbeBytes
	}
	end := len(text)
	for end > start && isRubyStripSpace(text[end-1]) {
		end--
	}
	if whitespaceSIMD && len(text)-end == whitespaceProbeBytes && end > 0 {
		end -= whitespaceSuffixSIMD(text[:end], true)
	}
	return text[:end]
}

// rubyStrip trims Ruby strip-family whitespace (including NUL) from both ends of
// text.
func rubyStrip(text string) string {
	return rubyLstrip(rubyRstrip(text))
}

// squishedLen reports the byte length stringSquish produces for text. It walks
// the same whitespace-separated fields, counting one separating space between
// consecutive ones. TestSquishReservesExactlyWhatItWrites pins the two in step.
func squishedLen(text string) int {
	total := 0
	fieldStart := -1
	for i, r := range text {
		if unicode.IsSpace(r) {
			if fieldStart >= 0 {
				if total > 0 {
					total++
				}
				total += i - fieldStart
				fieldStart = -1
			}
			continue
		}
		if fieldStart < 0 {
			fieldStart = i
		}
	}
	if fieldStart >= 0 {
		if total > 0 {
			total++
		}
		total += len(text) - fieldStart
	}
	return total
}

// stringSquish collapses every run of whitespace in text to a single space and
// trims both ends, mirroring Rails' String#squish.
//
// The builder is sized to the output rather than to the receiver. Growing it by
// len(text) reserved the receiver's length before the fields were known, and
// strings.Builder hands its whole backing array to the string it returns, so a
// heavily collapsing input -- an all-whitespace string, or a megabyte of padding
// around one character -- produced a short string still holding an oversized
// buffer: 200 of them held 192 MiB under an 8 MiB quota while the estimator
// priced them by their visible length (#51). squishedLen walks the same fields
// this loop writes, so the reservation is exact and the builder never has to
// grow again (which would overshoot to twice the capacity plus the write).
func stringSquish(text string) string {
	if stringIsSquished(text) {
		return text
	}

	var b strings.Builder
	b.Grow(squishedLen(text))
	pendingSpace := false
	fieldStart := -1
	for i, r := range text {
		if unicode.IsSpace(r) {
			if fieldStart >= 0 {
				if pendingSpace {
					b.WriteByte(' ')
				}
				b.WriteString(text[fieldStart:i])
				pendingSpace = true
				fieldStart = -1
			}
			continue
		}
		if fieldStart < 0 {
			fieldStart = i
		}
	}
	if fieldStart >= 0 {
		if pendingSpace {
			b.WriteByte(' ')
		}
		b.WriteString(text[fieldStart:])
	}
	return b.String()
}

func stringIsSquished(text string) bool {
	if text == "" {
		return true
	}
	sawText := false
	previousSpace := false
	for _, r := range text {
		if unicode.IsSpace(r) {
			if !sawText || previousSpace || r != ' ' {
				return false
			}
			previousSpace = true
			continue
		}
		sawText = true
		previousSpace = false
	}
	return !previousSpace
}

func stringTemplateOption(kwargs map[string]Value) (bool, error) {
	if len(kwargs) == 0 {
		return false, nil
	}
	value, ok := kwargs["strict"]
	if !ok || len(kwargs) != 1 {
		return false, fmt.Errorf("string.template supports only strict keyword")
	}
	if value.Kind() != KindBool {
		return false, fmt.Errorf("string.template strict keyword must be bool")
	}
	return value.Bool(), nil
}

func stringTemplateLookup(context Value, keyPath string) (Value, bool) {
	current := context
	for segment := range strings.SplitSeq(keyPath, ".") {
		if segment == "" {
			return NewNil(), false
		}
		if current.Kind() != KindHash && current.Kind() != KindObject {
			return NewNil(), false
		}
		next, ok := current.HashEntryMap()[segment]
		if !ok {
			return NewNil(), false
		}
		current = next
	}
	return current, true
}

// stringTemplateScalarValue renders a placeholder value, reporting whether the
// rendering is a new allocation.
//
// A string, symbol or enum member hands back its existing backing, so retaining
// it costs nothing the caller's own roots do not already account for. Anything
// else -- a big integer's base conversion, a regex's escaped literal, a
// formatted time -- is built here, and that copy is live alongside the result.
func stringTemplateScalarValue(value Value, keyPath string) (string, bool, error) {
	switch value.Kind() {
	case KindString, KindSymbol:
		return value.String(), false, nil
	case KindNil, KindBool, KindInt, KindFloat, KindMoney, KindDuration, KindTime, KindRegex:
		return value.String(), true, nil
	case KindEnumValue:
		member := valueEnumValue(value)
		if member == nil {
			return "", false, fmt.Errorf("string.template placeholder %s value must be scalar", keyPath)
		}
		return member.Symbol, false, nil
	default:
		return "", false, fmt.Errorf("string.template placeholder %s value must be scalar", keyPath)
	}
}

type stringTemplateSegmentCache struct {
	keys   [8]string
	values [8]string
	count  int
	// builtBytes counts the segments that are new allocations rather than
	// aliases of memory the caller already holds. See retainedBytes.
	builtBytes int
	// overflow holds entries past the inline slots. Dropping them silently made
	// the ninth distinct placeholder onwards convert twice -- once to size the
	// render and once to write it -- which is the cost this cache exists to
	// avoid. Templates with eight or fewer distinct keys never allocate it.
	overflow map[string]string
}

func (c *stringTemplateSegmentCache) lookup(key string) (string, bool) {
	for i := range c.count {
		if c.keys[i] == key {
			return c.values[i], true
		}
	}
	if c.overflow != nil {
		value, ok := c.overflow[key]
		return value, ok
	}
	return "", false
}

// A cached segment past the inline slots lives in a Go map, whose bucket and two
// string headers are allocated whether the segment itself is fresh or an alias.
// Counting only payloads left a template of aliased placeholders reserving
// nothing at all while that map grew with the placeholder count.
const estimatedTemplateOverflowEntryBytes = estimatedMapEntryBytes + 2*estimatedStringHeaderBytes

// retainedBytes reports the bytes the cache is holding that it allocated itself.
// Those stay live while the result builder is allocated, so the peak is the
// builder plus these and the memory reservation must cover both.
//
// Segment payloads count only when the render built them. A string, symbol or
// enum placeholder hands back its existing backing and a key slices the template
// receiver, so counting those projected a copy that does not exist and rejected
// templates a real peak would have fitted. The overflow map's own storage counts
// either way, because the cache allocates it either way.
func (c *stringTemplateSegmentCache) retainedBytes() int {
	total := c.builtBytes
	if c.overflow == nil {
		return total
	}
	total = saturatingAdd(total, estimatedMapBaseBytes)
	return saturatingAdd(total, saturatingMul(len(c.overflow), estimatedTemplateOverflowEntryBytes))
}

func (c *stringTemplateSegmentCache) store(key, value string, built bool) {
	if built {
		c.builtBytes = saturatingAdd(c.builtBytes, len(value))
	}
	if c.count < len(c.keys) {
		c.keys[c.count] = key
		c.values[c.count] = value
		c.count++
		return
	}
	if c.overflow == nil {
		c.overflow = make(map[string]string)
	}
	c.overflow[key] = value
}

func appendTemplateChunk(exec *Execution, b *strings.Builder, chunk string, receiver Value, args []Value, kwargs map[string]Value, block Value) error {
	if err := exec.step(); err != nil {
		return err
	}
	if chunk == "" {
		return nil
	}
	if err := exec.checkProjectedStringBytesWithCallRoots(projectedBuilderCap(b, len(chunk)), receiver, args, kwargs, block); err != nil {
		return err
	}
	b.Grow(len(chunk))
	b.WriteString(chunk)
	return nil
}

// stringTemplateRenderedLen measures what the template will render, charging
// each piece as it is produced.
//
// The charge cannot wait for the total: rendering a regex or a big integer into
// a placeholder materializes it here, so a template holding one did that work
// before any charge and the rendering loop then did it again. Charging as each
// segment is produced bills the first conversion, which is the one that happens.
// templateScratchCheckBytes is how much the segment cache may grow between
// memory checks during projection. The projection allocates as it walks, so
// checking only once it has finished let a template with many placeholders --
// or a few that render to megabytes -- allocate the whole scratch past a finite
// quota before anything looked. Checking every placeholder instead would walk
// the call roots once per placeholder, which is quadratic in the placeholder
// count; a chunk bounds the overshoot to a fixed size and the number of checks
// to the scratch's megabytes.
const templateScratchCheckBytes = 64 << 10

func stringTemplateRenderedLen(
	exec *Execution,
	text string,
	context Value,
	strict bool,
	receiver Value,
	args []Value,
	kwargs map[string]Value,
	block Value,
) (bool, int, stringTemplateSegmentCache, error) {
	charged := 0
	checkedScratch := 0
	charge := func(total int) error {
		if exec == nil {
			return nil
		}
		steps := total / stringScanBytesPerStep
		if steps <= charged {
			return nil
		}
		if err := exec.stepN(steps - charged); err != nil {
			return err
		}
		charged = steps
		return nil
	}
	var cache stringTemplateSegmentCache
	rendered := false
	total := 0
	last := 0
	search := 0
	for search < len(text) {
		openRel := strings.Index(text[search:], "{{")
		if openRel < 0 {
			break
		}
		open := search + openRel
		keyPath, end, ok := parseTemplateAt(text, open)
		if !ok {
			search = open + 1
			continue
		}
		rendered = true
		total = saturatingAdd(total, open-last)
		placeholder := text[open:end]
		if segment, ok := cache.lookup(keyPath); ok {
			total = saturatingAdd(total, len(segment))
			last = end
			search = end
			continue
		}
		value, ok := stringTemplateLookup(context, keyPath)
		if !ok {
			if strict {
				return false, 0, cache, fmt.Errorf("string.template missing placeholder %s", keyPath)
			}
			total = saturatingAdd(total, len(placeholder))
			last = end
			search = end
			continue
		}
		segment, built, err := stringTemplateScalarValue(value, keyPath)
		if err != nil {
			return false, 0, cache, err
		}
		cache.store(keyPath, segment, built)
		total = saturatingAdd(total, len(segment))
		if err := charge(total); err != nil {
			return false, 0, cache, err
		}
		if retained := cache.retainedBytes(); retained-checkedScratch >= templateScratchCheckBytes {
			if exec != nil {
				if err := exec.checkProjectedStringBytesWithCallRoots(
					retained, receiver, args, kwargs, block,
				); err != nil {
					return false, 0, cache, err
				}
			}
			checkedScratch = retained
		}
		last = end
		search = end
	}
	if !rendered {
		return false, 0, cache, nil
	}
	total = saturatingAdd(total, len(text[last:]))
	if err := charge(total); err != nil {
		return false, 0, cache, err
	}
	return true, total, cache, nil
}

func stringTemplate(text string, context Value, strict bool) (string, error) {
	return stringTemplateWithExecution(&Execution{}, text, context, strict, NewNil(), nil, nil, NewNil())
}

func stringTemplateWithExecution(exec *Execution, text string, context Value, strict bool, receiver Value, args []Value, kwargs map[string]Value, block Value) (string, error) {
	// The projection's cache carries the segments it already built, and the
	// render reuses them. Discarding it meant every placeholder value was
	// converted twice -- once to size it and once to write it -- so a large
	// regex or big integer did its conversion twice for one charge.
	rendered, renderedLen, cache, err := stringTemplateRenderedLen(
		exec, text, context, strict, receiver, args, kwargs, block,
	)
	if err != nil {
		return "", err
	}
	if !rendered {
		return text, nil
	}
	var b strings.Builder
	if renderedLen > 0 {
		// The cache's segments are still live here -- they are what the render
		// reads instead of converting again -- so the peak is the builder plus
		// them, not the builder alone.
		projected := saturatingAdd(projectedBuilderCap(&b, renderedLen), cache.retainedBytes())
		if err := exec.checkProjectedStringBytesWithCallRoots(projected, receiver, args, kwargs, block); err != nil {
			return "", err
		}
		b.Grow(renderedLen)
	}
	last := 0
	search := 0
	for search < len(text) {
		openRel := strings.Index(text[search:], "{{")
		if openRel < 0 {
			break
		}
		open := search + openRel
		keyPath, end, ok := parseTemplateAt(text, open)
		if !ok {
			search = open + 1
			continue
		}
		if err := appendTemplateChunk(exec, &b, text[last:open], receiver, args, kwargs, block); err != nil {
			return "", err
		}
		placeholder := text[open:end]
		if segment, ok := cache.lookup(keyPath); ok {
			if err := appendTemplateChunk(exec, &b, segment, receiver, args, kwargs, block); err != nil {
				return "", err
			}
			last = end
			search = end
			continue
		}
		value, ok := stringTemplateLookup(context, keyPath)
		if !ok {
			if strict {
				return "", fmt.Errorf("string.template missing placeholder %s", keyPath)
			}
			if err := appendTemplateChunk(exec, &b, placeholder, receiver, args, kwargs, block); err != nil {
				return "", err
			}
			last = end
			search = end
			continue
		}
		segment, built, err := stringTemplateScalarValue(value, keyPath)
		if err != nil {
			return "", err
		}
		cache.store(keyPath, segment, built)
		if err := appendTemplateChunk(exec, &b, segment, receiver, args, kwargs, block); err != nil {
			return "", err
		}
		last = end
		search = end
	}
	if err := appendTemplateChunk(exec, &b, text[last:], receiver, args, kwargs, block); err != nil {
		return "", err
	}
	return b.String(), nil
}

func parseTemplateAt(text string, open int) (string, int, bool) {
	i := open + 2
	for i < len(text) && isTemplateSpace(text[i]) {
		i++
	}
	if i >= len(text) || !isTemplateKeyStart(text[i]) {
		return "", 0, false
	}
	keyStart := i
	i++
	for i < len(text) && isTemplateKeyRune(text[i]) {
		i++
	}
	keyEnd := i
	for i < len(text) && isTemplateSpace(text[i]) {
		i++
	}
	if i+1 >= len(text) || text[i] != '}' || text[i+1] != '}' {
		return "", 0, false
	}
	return text[keyStart:keyEnd], i + 2, true
}

func isTemplateSpace(b byte) bool {
	switch b {
	case ' ', '\t', '\n', '\f', '\r':
		return true
	default:
		return false
	}
}

func isTemplateKeyStart(b byte) bool {
	return b == '_' || ('A' <= b && b <= 'Z') || ('a' <= b && b <= 'z')
}

func isTemplateKeyRune(b byte) bool {
	return isTemplateKeyStart(b) || ('0' <= b && b <= '9') || b == '.' || b == '-'
}

// errInumOverflow signals that a leniently parsed integer magnitude does not
// fit in the int64 range. Ruby promotes such values to an arbitrary-precision
// Bignum, but Vibescript only has int64, so the runtime reports overflow the way
// the other integer operations do (see Integer#abs, Integer#succ).
var errInumOverflow = errors.New("integer out of range")

// inumDigit returns the numeric value of a base digit byte and whether it is a
// valid digit for the given base. Letters are case-insensitive, so 'a'/'A' both
// map to 10.
func inumDigit(b byte, base int) (int, bool) {
	var d int
	switch {
	case '0' <= b && b <= '9':
		d = int(b - '0')
	case 'a' <= b && b <= 'z':
		d = int(b-'a') + 10
	case 'A' <= b && b <= 'Z':
		d = int(b-'A') + 10
	default:
		return 0, false
	}
	if d >= base {
		return 0, false
	}
	return d, true
}

// parseRubyInum implements Ruby's lenient String#hex / String#oct conversion.
// It skips leading whitespace, accepts a single optional sign, consumes a
// base prefix (0x/0b/0o/0d, case-insensitive) when detectBase is set, honors a
// 0x/0X prefix for the fixed hexadecimal base otherwise, allows single
// underscores between digits as separators, and stops at the first byte that is
// not a valid digit. A string with no leading digit yields 0, mirroring Ruby's
// badcheck=false behavior. The magnitude is accumulated in int64; a value that
// would exceed the int64 range returns errInumOverflow because Vibescript has no
// Bignum to promote to.
func parseRubyInum(text string, defaultBase int, detectBase bool) (int64, error) {
	i := 0
	// Skip leading whitespace using Ruby's ISSPACE classification, matching
	// rb_str_to_inum.
	for i < len(text) && isRubyASCIISpace(text[i]) {
		i++
	}

	negative := false
	if i < len(text) && (text[i] == '+' || text[i] == '-') {
		negative = text[i] == '-'
		i++
	}

	base := defaultBase
	if i+1 < len(text) && text[i] == '0' {
		switch text[i+1] {
		case 'x', 'X':
			if base == 16 || detectBase {
				base = 16
				i += 2
			}
		case 'b', 'B':
			if detectBase {
				base = 2
				i += 2
			}
		case 'o', 'O':
			if detectBase {
				base = 8
				i += 2
			}
		case 'd', 'D':
			if detectBase {
				base = 10
				i += 2
			}
		}
	}

	var magnitude uint64
	parsedDigit := false
	lastWasUnderscore := false
	for i < len(text) {
		b := text[i]
		if b == '_' {
			// Underscores are separators only between two digits, so a leading,
			// trailing, or doubled underscore terminates the run like Ruby does.
			if !parsedDigit || lastWasUnderscore {
				break
			}
			lastWasUnderscore = true
			i++
			continue
		}
		d, ok := inumDigit(b, base)
		if !ok {
			break
		}
		// Detect overflow before accumulating: magnitude*base+d must fit in
		// uint64. The wraparound idiom (next < magnitude) is unsound for
		// multiplication because magnitude*base can wrap to a value still
		// >= magnitude, so check each factor exactly instead.
		if magnitude > (math.MaxUint64-uint64(d))/uint64(base) {
			return 0, errInumOverflow
		}
		magnitude = magnitude*uint64(base) + uint64(d)
		parsedDigit = true
		lastWasUnderscore = false
		i++
	}

	if !parsedDigit {
		return 0, nil
	}
	if negative {
		// MinInt64 is -(1<<63), so the negative magnitude may reach 1<<63 exactly.
		if magnitude > uint64(math.MaxInt64)+1 {
			return 0, errInumOverflow
		}
		return -int64(magnitude), nil
	}
	if magnitude > uint64(math.MaxInt64) {
		return 0, errInumOverflow
	}
	return int64(magnitude), nil
}

func stringMemberQuery(property string) (Value, error) {
	switch property {
	case "size":
		return NewAutoBuiltin("string.size", func(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
			if len(args) > 0 {
				return NewNil(), fmt.Errorf("string.size does not take arguments")
			}
			return NewInt(int64(stringRuneLen(receiver.String()))), nil
		}), nil
	case "length":
		return NewAutoBuiltin("string.length", func(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
			if len(args) > 0 {
				return NewNil(), fmt.Errorf("string.length does not take arguments")
			}
			return NewInt(int64(stringRuneLen(receiver.String()))), nil
		}), nil
	case "bytesize":
		return NewAutoBuiltin("string.bytesize", func(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
			if len(args) > 0 {
				return NewNil(), fmt.Errorf("string.bytesize does not take arguments")
			}
			return NewInt(int64(len(receiver.String()))), nil
		}), nil
	case "ord":
		return NewAutoBuiltin("string.ord", func(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
			if len(args) > 0 {
				return NewNil(), fmt.Errorf("string.ord does not take arguments")
			}
			r, size := utf8.DecodeRuneInString(receiver.String())
			if size == 0 {
				return NewNil(), fmt.Errorf("string.ord requires non-empty string")
			}
			return NewInt(int64(r)), nil
		}), nil
	case "chr":
		return NewAutoBuiltin("string.chr", func(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
			if len(args) > 0 {
				return NewNil(), fmt.Errorf("string.chr does not take arguments")
			}
			r, size := utf8.DecodeRuneInString(receiver.String())
			if size == 0 {
				return NewString(""), nil
			}
			return NewString(string(r)), nil
		}), nil
	case "getbyte":
		return NewAutoBuiltin("string.getbyte", func(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
			if len(kwargs) > 0 {
				return NewNil(), fmt.Errorf("string.getbyte does not accept keyword arguments")
			}
			if len(args) != 1 {
				return NewNil(), fmt.Errorf("string.getbyte expects exactly one index")
			}
			index, err := valueToInt(args[0])
			if err != nil {
				return NewNil(), fmt.Errorf("string.getbyte index must be an integer")
			}
			text := receiver.String()
			if index < 0 {
				index += len(text)
			}
			if index < 0 || index >= len(text) {
				return NewNil(), nil
			}
			return NewInt(int64(text[index])), nil
		}), nil
	case "byteslice":
		return NewAutoBuiltin("string.byteslice", func(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
			if len(kwargs) > 0 {
				return NewNil(), fmt.Errorf("string.byteslice does not accept keyword arguments")
			}
			return stringByteslice(exec, receiver, args, kwargs, block)
		}), nil
	case "hex":
		return NewAutoBuiltin("string.hex", func(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
			if len(args) > 0 {
				return NewNil(), fmt.Errorf("string.hex does not take arguments")
			}
			n, err := parseRubyInum(receiver.String(), 16, false)
			if err != nil {
				return NewNil(), fmt.Errorf("string.hex %w", err)
			}
			return NewInt(n), nil
		}), nil
	case "oct":
		return NewAutoBuiltin("string.oct", func(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
			if len(args) > 0 {
				return NewNil(), fmt.Errorf("string.oct does not take arguments")
			}
			n, err := parseRubyInum(receiver.String(), 8, true)
			if err != nil {
				return NewNil(), fmt.Errorf("string.oct %w", err)
			}
			return NewInt(n), nil
		}), nil
	case "empty?":
		return NewAutoBuiltin("string.empty?", func(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
			if len(args) > 0 {
				return NewNil(), fmt.Errorf("string.empty? does not take arguments")
			}
			return NewBool(len(receiver.String()) == 0), nil
		}), nil
	case "clear":
		return NewAutoBuiltin("string.clear", func(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
			if len(args) > 0 {
				return NewNil(), fmt.Errorf("string.clear does not take arguments")
			}
			return NewString(""), nil
		}), nil
	case "concat":
		return NewAutoBuiltin("string.concat", func(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
			total := len(receiver.String())
			var b strings.Builder
			for _, arg := range args {
				if arg.Kind() != KindString {
					return NewNil(), fmt.Errorf("string.concat expects string arguments")
				}
				total = saturatingAdd(total, len(arg.String()))
			}
			if err := exec.checkContext(); err != nil {
				return NewNil(), err
			}
			if err := exec.checkProjectedStringBytesWithCallRoots(projectedBuilderCap(&b, total), receiver, args, kwargs, block); err != nil {
				return NewNil(), err
			}
			b.Grow(total)
			b.WriteString(receiver.String())
			for _, arg := range args {
				if err := exec.step(); err != nil {
					return NewNil(), err
				}
				b.WriteString(arg.String())
			}
			return NewString(b.String()), nil
		}), nil
	case "prepend":
		return NewAutoBuiltin("string.prepend", func(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
			total := len(receiver.String())
			var b strings.Builder
			for _, arg := range args {
				if arg.Kind() != KindString {
					return NewNil(), fmt.Errorf("string.prepend expects string arguments")
				}
				total = saturatingAdd(total, len(arg.String()))
			}
			if err := exec.checkContext(); err != nil {
				return NewNil(), err
			}
			if err := exec.checkProjectedStringBytesWithCallRoots(projectedBuilderCap(&b, total), receiver, args, kwargs, block); err != nil {
				return NewNil(), err
			}
			b.Grow(total)
			for _, arg := range args {
				if err := exec.step(); err != nil {
					return NewNil(), err
				}
				b.WriteString(arg.String())
			}
			b.WriteString(receiver.String())
			return NewString(b.String()), nil
		}), nil
	case "insert":
		return NewAutoBuiltin("string.insert", func(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
			if len(args) != 2 {
				return NewNil(), fmt.Errorf("string.insert expects an index and a string")
			}
			index, err := valueToInt(args[0])
			if err != nil {
				return NewNil(), fmt.Errorf("string.insert index must be integer")
			}
			if args[1].Kind() != KindString {
				return NewNil(), fmt.Errorf("string.insert value must be string")
			}
			text := receiver.String()
			byteAt, ok := stringInsertByteOffset(text, index)
			if !ok {
				return NewNil(), fmt.Errorf("string.insert index %d out of string", index)
			}
			var b strings.Builder
			b.WriteString(text[:byteAt])
			b.WriteString(args[1].String())
			b.WriteString(text[byteAt:])
			return NewString(b.String()), nil
		}), nil
	case "replace":
		return NewAutoBuiltin("string.replace", func(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
			if len(args) != 1 {
				return NewNil(), fmt.Errorf("string.replace expects exactly one replacement")
			}
			if args[0].Kind() != KindString {
				return NewNil(), fmt.Errorf("string.replace replacement must be string")
			}
			return NewString(args[0].String()), nil
		}), nil
	case "start_with?":
		return NewAutoBuiltin("string.start_with?", func(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
			if len(args) == 0 {
				return NewNil(), fmt.Errorf("string.start_with? expects at least one prefix")
			}
			value := receiver.String()
			// Check candidates left to right and short-circuit on the first
			// match, like Ruby: a non-string is only rejected if it is reached
			// before any match.
			for _, arg := range args {
				if arg.Kind() != KindString {
					return NewNil(), fmt.Errorf("string.start_with? prefix must be string")
				}
				if strings.HasPrefix(value, arg.String()) {
					return NewBool(true), nil
				}
			}
			return NewBool(false), nil
		}), nil
	case "end_with?":
		return NewAutoBuiltin("string.end_with?", func(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
			if len(args) == 0 {
				return NewNil(), fmt.Errorf("string.end_with? expects at least one suffix")
			}
			value := receiver.String()
			// Check candidates left to right and short-circuit on the first
			// match, like Ruby: a non-string is only rejected if it is reached
			// before any match.
			for _, arg := range args {
				if arg.Kind() != KindString {
					return NewNil(), fmt.Errorf("string.end_with? suffix must be string")
				}
				if strings.HasSuffix(value, arg.String()) {
					return NewBool(true), nil
				}
			}
			return NewBool(false), nil
		}), nil
	case "include?":
		return NewAutoBuiltin("string.include?", func(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
			if len(args) != 1 {
				return NewNil(), fmt.Errorf("string.include? expects exactly one substring")
			}
			if args[0].Kind() != KindString {
				return NewNil(), fmt.Errorf("string.include? substring must be string")
			}
			return NewBool(strings.Contains(receiver.String(), args[0].String())), nil
		}), nil
	case "count":
		return NewAutoBuiltin("string.count", func(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
			count, err := stringCountChars(exec, receiver, args, kwargs, block)
			if err != nil {
				return NewNil(), err
			}
			return NewInt(int64(count)), nil
		}), nil
	case "casecmp":
		return NewAutoBuiltin("string.casecmp", func(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
			if len(args) != 1 {
				return NewNil(), fmt.Errorf("string.casecmp expects exactly one string")
			}
			if args[0].Kind() != KindString {
				return NewNil(), nil
			}
			return NewInt(int64(asciiCaseCompare(receiver.String(), args[0].String()))), nil
		}), nil
	case "casecmp?":
		return NewAutoBuiltin("string.casecmp?", func(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
			if len(args) != 1 {
				return NewNil(), fmt.Errorf("string.casecmp? expects exactly one string")
			}
			if args[0].Kind() != KindString {
				return NewNil(), nil
			}
			return NewBool(caseInsensitiveEqual(receiver.String(), args[0].String())), nil
		}), nil
	case "between?":
		return NewAutoBuiltin("string.between?", func(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
			return comparableBetween("string.between?", receiver, args, kwargs, block)
		}), nil
	case "match":
		return NewAutoBuiltin("string.match", func(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
			if len(kwargs) > 0 {
				return NewNil(), fmt.Errorf("string.match does not accept keyword arguments")
			}
			if len(args) < 1 || len(args) > 2 {
				return NewNil(), fmt.Errorf("string.match expects a pattern and optional offset")
			}
			pattern, patternIsRegex, err := stringPatternArgument("string.match", args[0])
			if err != nil {
				return NewNil(), err
			}
			text := receiver.String()
			if err := validateRegexTextPattern("string.match", text, pattern, patternIsRegex); err != nil {
				return NewNil(), err
			}
			// Ruby counts a negative offset back from the end of the string; an
			// offset that falls before the start yields nil. The regex is still
			// compiled in that branch so an invalid pattern is rejected regardless
			// of the offset, mirroring the in-range path: the offset only decides
			// the match result, never whether a bad regex is accepted.
			//
			// Unlike String#match?, a positive offset that runs past the end is
			// clamped to the length rather than rejected: Ruby still starts the
			// search at the end, so a zero-width-capable pattern matches the empty
			// string there while a pattern that needs a character returns nil. The
			// regex engine decides the outcome from the clamped end position.
			offset := 0
			if len(args) == 2 {
				raw, err := valueToInt(args[1])
				if err != nil {
					return NewNil(), fmt.Errorf("string.match offset must be integer")
				}
				effective, ok := stringEffectiveOffset(text, raw)
				if !ok {
					if _, compileErr := compileCachedRegex(pattern); compileErr != nil {
						return NewNil(), fmt.Errorf("string.match invalid regex: %w", compileErr)
					}
					return NewNil(), nil
				}
				if runeLen := stringRuneLen(text); effective > runeLen {
					effective = runeLen
				}
				offset = effective
			}
			indices, err := regexSubmatchFromRuneOffset("string.match", text, pattern, offset)
			if err != nil {
				return NewNil(), err
			}
			if indices == nil {
				// Ruby's String#match returns nil and never invokes the block when
				// there is no match, so the block form short-circuits here too.
				return NewNil(), nil
			}
			matchData, err := newMatchData(exec, text, indices, regexSubexpNames(pattern), receiver, args, kwargs, block)
			if err != nil {
				return NewNil(), err
			}
			if valueBlock(block) != nil {
				// Ruby's String#match(pattern) { |m| ... } yields the match data and
				// returns the block's result. MatchData supports the same index access
				// as Ruby: m[0] is the whole match, m[1] the first capture.
				runner, err := newBlockCallRunner(exec, block, "string.match", receiver, args, kwargs)
				if err != nil {
					return NewNil(), err
				}
				return runner.call([]Value{matchData})
			}
			return matchData, nil
		}), nil
	case "match?":
		return NewAutoBuiltin("string.match?", func(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
			if len(kwargs) > 0 {
				return NewNil(), fmt.Errorf("string.match? does not accept keyword arguments")
			}
			if len(args) < 1 || len(args) > 2 {
				return NewNil(), fmt.Errorf("string.match? expects a pattern and optional offset")
			}
			if _, _, err := stringPatternArgument("string.match?", args[0]); err != nil {
				return NewNil(), err
			}
			offset := 0
			if len(args) == 2 {
				i, err := valueToInt(args[1])
				if err != nil || i < 0 {
					return NewNil(), fmt.Errorf("string.match? offset must be non-negative integer")
				}
				offset = i
			}
			pattern, patternIsRegex, err := stringPatternArgument("string.match?", args[0])
			if err != nil {
				return NewNil(), err
			}
			text := receiver.String()
			if err := validateRegexTextPattern("string.match?", text, pattern, patternIsRegex); err != nil {
				return NewNil(), err
			}
			matched, err := regexMatchFromRuneOffset("string.match?", text, pattern, offset)
			if err != nil {
				return NewNil(), err
			}
			return NewBool(matched), nil
		}), nil
	case "scan":
		return NewAutoBuiltin("string.scan", func(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
			if len(kwargs) > 0 {
				return NewNil(), fmt.Errorf("string.scan does not accept keyword arguments")
			}
			if len(args) != 1 {
				return NewNil(), fmt.Errorf("string.scan expects exactly one pattern")
			}
			pattern, patternIsRegex, err := stringPatternArgument("string.scan", args[0])
			if err != nil {
				return NewNil(), err
			}
			text := receiver.String()
			if err := validateRegexTextPattern("string.scan", text, pattern, patternIsRegex); err != nil {
				return NewNil(), err
			}
			re, err := compileCachedRegex(pattern)
			if err != nil {
				return NewNil(), fmt.Errorf("string.scan invalid regex: %w", err)
			}
			return stringScan(exec, re, pattern, text, receiver, args, kwargs, block)
		}), nil
	case "index":
		return NewAutoBuiltin("string.index", func(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
			if len(args) < 1 || len(args) > 2 {
				return NewNil(), fmt.Errorf("string.index expects substring and optional offset")
			}
			offset := 0
			if len(args) == 2 {
				i, err := valueToInt(args[1])
				if err != nil {
					return NewNil(), fmt.Errorf("string.index offset must be integer")
				}
				offset = i
			}
			return stringIndexResult(exec, receiver, args[0], offset)
		}), nil
	case "rindex":
		return NewAutoBuiltin("string.rindex", func(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
			if len(args) < 1 || len(args) > 2 {
				return NewNil(), fmt.Errorf("string.rindex expects substring and optional offset")
			}
			offset := stringRuneLen(receiver.String())
			if len(args) == 2 {
				i, err := valueToInt(args[1])
				if err != nil {
					return NewNil(), fmt.Errorf("string.rindex offset must be integer")
				}
				effective, ok := stringEffectiveOffset(receiver.String(), i)
				if !ok {
					return NewNil(), nil
				}
				offset = effective
			}
			return stringRIndexResult(exec, receiver, args[0], offset)
		}), nil
	case "slice":
		return NewAutoBuiltin("string.slice", stringSlice), nil
	default:
		return NewNil(), fmt.Errorf("unknown string method %s", property)
	}
}

func stringIndexResult(exec *Execution, receiver, needle Value, offset int) (Value, error) {
	if needle.Kind() != KindString {
		return NewNil(), fmt.Errorf("string.index substring must be string")
	}
	if offset < 0 {
		effective, ok := stringEffectiveOffset(receiver.String(), offset)
		if !ok {
			return NewNil(), nil
		}
		offset = effective
	}
	index, err := stringRuneIndex(exec, receiver.String(), needle.String(), offset)
	if err != nil {
		return NewNil(), err
	}
	if index < 0 {
		return NewNil(), nil
	}
	return NewInt(int64(index)), nil
}

func stringRIndexResult(exec *Execution, receiver, needle Value, offset int) (Value, error) {
	if needle.Kind() != KindString {
		return NewNil(), fmt.Errorf("string.rindex substring must be string")
	}
	index, err := stringRuneRIndex(exec, receiver.String(), needle.String(), offset)
	if err != nil {
		return NewNil(), err
	}
	if index < 0 {
		return NewNil(), nil
	}
	return NewInt(int64(index)), nil
}

// projectedScanResultSlots reports how many slots String#scan's result backing
// will hold, which is exactly one per match.
//
// The build used to start at a capped capacity and let append grow it, so that
// a subject yielding few matches did not reserve a large backing before the
// per-match checks could reject the call. checkProjectedScanOutputBytes now
// weighs the whole backing, every element and the index table against the quota
// before the first match is copied, which subsumes that guard and is stricter
// than it was: nothing is allocated until the complete result has been approved.
//
// Growing instead of preallocating also made the projection wrong in the unsafe
// direction. append overshoots -- 257 matches reached a capacity of 575, leaving
// 318 slots and 10,176 bytes unpriced -- while the preflight charged the logical
// match count, so the last and largest match could be copied before acc.add saw
// the real backing. Allocating the count that was approved makes the projection
// exact and lowers the peak rather than raising it.
func projectedScanResultSlots(allMatches [][]int) int {
	return len(allMatches)
}

// stringScan returns capture-aware matches: strings without capture groups and
// arrays of captured strings (or nil for unmatched groups) otherwise. Block scans
// stream their matches; array scans preflight the complete result and index table.
func stringScan(exec *Execution, re *regexp.Regexp, pattern, text string, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
	groups := re.NumSubexp()
	if valueBlock(block) != nil {
		return stringScanBlock(exec, re, text, receiver, args, kwargs, block)
	}
	roots := scanRoots{receiver: receiver, args: args, kwargs: kwargs, block: block}
	allMatches, err := stringScanMatches(exec, re, pattern, text, roots)
	if err != nil {
		return NewNil(), err
	}

	if err := exec.guardStringScanOutputFootprint(allMatches, groups); err != nil {
		return NewNil(), err
	}
	// Weigh every copy before the first one is made. The accumulator below
	// charges each element only once it exists, which let one large match be
	// cloned past the quota before anything checked it.
	if err := exec.checkProjectedScanOutputBytes(text, allMatches, groups, receiver, args, kwargs, block); err != nil {
		return NewNil(), err
	}

	acc := newArrayBuildAccumulator(exec, receiver, args, kwargs, block)
	// The engine's index table stays live the whole time the result is built from
	// it, so charge its actual footprint into the accumulator's baseline; the
	// per-element checks then see index table + growing result together.
	if err := acc.reserveScratch(actualRegexSubmatchIndexBytes(allMatches, groups)); err != nil {
		return NewNil(), err
	}

	out := make([]Value, 0, projectedScanResultSlots(allMatches))
	for _, loc := range allMatches {
		// Charge a step per match so a pattern that produces a flood of matches
		// cannot starve the step quota or cancellation checks while the result is
		// assembled.
		if err := exec.step(); err != nil {
			return NewNil(), err
		}

		out = append(out, stringScanElement(text, loc, groups))
		if err := acc.add(out[len(out)-1], cap(out)); err != nil {
			return NewNil(), err
		}
	}

	return NewArray(out), nil
}

// stringScanMatches bounds and reserves an engine index table before building it.
// The caller charges its actual footprint for as long as the table remains live.
func stringScanMatches(exec *Execution, re *regexp.Regexp, pattern, text string, roots scanRoots) ([][]int, error) {
	groups := re.NumSubexp()

	if err := guardRegexScanIndexFootprint(exec, pattern, text, groups); err != nil {
		return nil, err
	}

	// Charge a step BEFORE materializing the match table. FindAllStringSubmatchIndex
	// is the expensive part of a scan -- for a zero-width pattern over a near-limit
	// subject it allocates a slot per position -- and the per-match charges below run
	// only after it completes. Stepping here means an already-canceled context or an
	// exhausted step quota aborts the scan before that work runs rather than paying
	// its full CPU and allocation cost first; step() polls cancellation on its very
	// first invocation, so even an empty subject observes a canceled context here.
	if err := exec.step(); err != nil {
		return nil, err
	}

	// Ask for at most one match beyond what the quota can hold, so the table
	// the engine allocates is bounded by the quota rather than by the subject.
	limit := -1
	budget, bounded := scanMatchBudget(exec, groups, roots)
	if bounded {
		limit = budget + 1
	}
	// Charge the table before the engine builds it, not after. The budget above
	// is a snapshot, and the engine allocates against it: charging only once the
	// table exists (below, via reserveScratch) left it built and live before
	// anything could refuse it, so a table sized from the whole remaining
	// headroom coexisted with whatever else had been allocated meanwhile. The
	// reservation is released as soon as the actual footprint is charged, so the
	// table is never counted twice.
	projected := 0
	if bounded {
		// Priced for the matches this scan would admit, not for limit. limit is
		// budget+1 -- one match beyond what the quota holds, asked for only so
		// that overrunning is detectable -- and the budget was derived by
		// dividing the remaining room by the same per-match price, so reserving
		// limit here is guaranteed to exceed that room and reject every scan.
		// The overshoot the probe can build is one match's worth, and bounded.
		projected = exec.reserveLoopScratch(worstCaseRegexSubmatchIndexBytes(budget, groups))
		// Through the roots, not checkMemory: the budget above subtracted the
		// call roots, so a check without them measures the larger of two figures
		// instead of their coexisting sum.
		if err := roots.check(exec); err != nil {
			exec.releaseLoopScratch(projected)
			return nil, err
		}
	}
	allMatches := re.FindAllStringSubmatchIndex(text, limit)
	exec.releaseLoopScratch(projected)
	if bounded && len(allMatches) > budget {
		return nil, exec.memoryQuotaExceededError()
	}

	return allMatches, nil
}

// guardRegexScanIndexFootprint rejects a scan whose worst-case
// FindAllStringSubmatchIndex index table would exceed the fixed host cap
// (maxRegexScanIndexBytes), BEFORE the engine runs. That call allocates the whole
// [][]int table -- maxMatches slices of 2 + 2*groups ints -- in one contiguous block
// before any interpreter accounting can run, so a pattern of thousands of empty
// capture groups over a large subject would request tens of gigabytes and OOM the
// host inside the call. The worst-case table is known from the subject's rune count,
// the pattern's minimum match length, and the group count alone (see
// regexScanMaxMatches), without matching anything, so the scan rejects here and never
// calls any FindAll variant when that worst case overflows the host cap.
//
// The guard deliberately checks the FIXED host cap, not the configurable memory
// quota. The memory quota bounds the script-visible RESULT and is enforced
// incrementally as that result accumulates (stringScan seeds the actual index
// footprint and charges each element against it); applying the worst-case projection
// to a small memory quota up front would reject ordinary sparse scans -- a plain "z"
// over a few-KB subject that matches nothing -- whose real result is empty and fits
// any quota. Separating the two keeps the host safe from the transient index table
// while letting the quota govern the result the script actually holds.
//
// maxMatches is bounded by regexScanMaxMatches: a pattern whose every match consumes
// at least minRunes runes yields at most runeCount/minRunes non-overlapping matches,
// far fewer than the runeCount+1 a zero-width pattern can produce, so a sparse
// non-zero-width many-group pattern is no longer rejected on a zero-width worst case
// it cannot reach. Only patterns that can match the empty string (minRunes == 0) fall
// back to the runeCount+1 worst case.
func guardRegexScanIndexFootprint(exec *Execution, pattern, text string, groups int) error {
	maxMatches := regexScanMaxMatches(pattern, text)
	projected := projectedRegexSubmatchIndexBytes(maxMatches, groups)
	if projected > maxRegexScanIndexBytes {
		return guardLimitErrorf("string.scan match table exceeds limit %d bytes", maxRegexScanIndexBytes)
	}
	return nil
}

// scanMatchBudget returns how many matches the index table may hold before it
// alone would exceed the memory quota, and whether a bound applies at all.
//
// Projecting a worst case and rejecting up front cannot work here: the
// worst case is reachable only by patterns that match densely, and no cheap
// property distinguishes those. "Can match empty" does not -- ^, \b and a*
// all can, yet each returns a handful of matches over a large subject, so
// rejecting on their projection fails ordinary scans.
//
// Bounding the request instead needs no prediction. FindAll takes a match
// limit, so asking for one more than the budget makes the table cost at most
// one row beyond what the quota allows, and a result that comes back over the
// budget is exactly the case that could not have fit (#37).
// scanRoots is what a string scan already holds before its index table exists:
// the receiver, arguments and block live on this builtin's Go frame, which the
// execution's own graph walk cannot see.
//
// It is one value rather than four parameters because the budget that *sizes*
// the table and the check that *charges* the reservation have to agree about
// what is live, and the findings on this table were that disagreement -- the
// append's capacity slack and these call roots -- each side accounting for
// something the other did not. Sizing and charging now read the same value
// through the same two methods, so a discrepancy is not expressible rather than
// merely corrected.
type scanRoots struct {
	receiver Value
	args     []Value
	kwargs   map[string]Value
	block    Value
}

// liveBytes is the quantity the budget subtracts and the check charges.
func (r scanRoots) liveBytes(exec *Execution) int {
	return exec.estimateMemoryUsageForCallRoots(NewNil(), r.receiver, r.args, r.kwargs, r.block)
}

// check charges that quantity, the current reservation included, and refuses
// when there is no room for it.
func (r scanRoots) check(exec *Execution) error {
	return exec.checkMemoryWithCallRoots(NewNil(), r.receiver, r.args, r.kwargs, r.block)
}

func scanMatchBudget(exec *Execution, groups int, roots scanRoots) (int, bool) {
	if exec == nil || exec.memoryQuota <= 0 {
		return 0, false
	}
	// FindAll appends into the outer slice, so its capacity overshoots the
	// match count -- Go grows a full slice by up to half again, and that slack
	// is a slice header per unused slot. Charging the logical rows alone let a
	// scan landing on a growth boundary allocate past the budget, so the
	// per-match price carries the overshoot the append can leave behind.
	perMatch := worstCaseRegexSubmatchIndexBytes(1, groups)
	if perMatch <= 0 {
		return 0, false
	}
	// Budget the quota that is actually left. Dividing the whole quota would
	// let an execution already holding most of it request nearly another
	// quota's worth of rows, which is the pre-accounting spike this bound
	// exists to stop -- the table coexists with everything already live.
	// The table is built before any check can see it, so the number this divides
	// has to be what is actually left after the roots the builtin already holds.
	remaining := exec.memoryBudgetBytes() - roots.liveBytes(exec)
	if remaining <= 0 {
		// Nothing left to spend: a single match is still requested so an
		// empty result stays legal, and any match at all reports the quota.
		return 0, true
	}
	return remaining / perMatch, true
}

// regexScanMaxMatches returns an upper bound on the number of non-overlapping
// matches FindAllStringSubmatchIndex can produce for pattern over text, without
// running the engine. FindAll advances past every match, so when each match consumes
// at least minRunes runes the subject admits at most runeCount/minRunes of them -- a
// non-empty match cannot also yield the trailing empty match a zero-width pattern can,
// so no +1 is added here. A pattern that can match the empty string (minRunes == 0)
// can match at every position plus once at the end, so its bound is the runeCount+1
// zero-width worst case.
//
// The bound stays correct even when pattern fails to parse here: regexScanMinMatchRunes
// reports 0 (zero-width) for anything it cannot prove consumes input, so an
// unparseable pattern -- which cannot happen for a scan whose regexp already compiled,
// but is handled defensively -- falls back to the runeCount+1 worst case rather than
// underestimating.
func regexScanMaxMatches(pattern, text string) int {
	runeCount := utf8.RuneCountInString(text)
	minRunes := regexScanMinMatchRunes(pattern)
	if minRunes <= 0 {
		return runeCount + 1
	}
	return runeCount / minRunes
}

// regexScanMinMatchRunes returns a lower bound on the number of runes any single
// match of pattern must consume, or 0 when the pattern can match the empty string
// (or cannot be analyzed). It parses pattern with the same flags regexp.Compile uses
// (syntax.Perl) so the bound matches the engine actually run for the scan.
//
// The result MUST never exceed the true minimum match length: regexScanMaxMatches
// divides the subject's rune count by it, so an over-estimate would under-bound the
// match count and could let the engine materialize a table larger than the quota
// admits. Every uncertain case therefore collapses to 0 (treat as zero-width), which
// over-rejects rather than under-rejects. A parse failure -- impossible for a scan
// whose pattern already compiled, but guarded for defensively -- returns 0 for the
// same reason.
func regexScanMinMatchRunes(pattern string) int {
	re, err := syntax.Parse(pattern, syntax.Perl)
	if err != nil {
		return 0
	}
	return regexpMinMatchRunes(re)
}

// regexpMinMatchRunes walks a parsed regular expression and returns a lower bound on
// the runes any single match must consume. Literals and single-rune classes consume
// their runes; concatenation sums its parts; alternation takes the smallest branch;
// a capture is transparent. Constructs that can match empty -- Star, Quest, anchors,
// word boundaries, the empty match -- contribute 0, as does any operator not proven
// to consume input, keeping the bound a safe under-estimate (see regexScanMinMatchRunes).
func regexpMinMatchRunes(re *syntax.Regexp) int {
	switch re.Op {
	case syntax.OpLiteral:
		return len(re.Rune)
	case syntax.OpCharClass, syntax.OpAnyChar, syntax.OpAnyCharNotNL:
		return 1
	case syntax.OpCapture:
		return regexpMinMatchRunes(re.Sub[0])
	case syntax.OpConcat:
		total := 0
		for _, sub := range re.Sub {
			total = saturatingAdd(total, regexpMinMatchRunes(sub))
		}
		return total
	case syntax.OpAlternate:
		minBranch := -1
		for _, sub := range re.Sub {
			branch := regexpMinMatchRunes(sub)
			if minBranch < 0 || branch < minBranch {
				minBranch = branch
			}
		}
		if minBranch < 0 {
			return 0
		}
		return minBranch
	case syntax.OpPlus:
		return regexpMinMatchRunes(re.Sub[0])
	case syntax.OpRepeat:
		return saturatingMul(re.Min, regexpMinMatchRunes(re.Sub[0]))
	default:
		// OpStar, OpQuest, the anchors, word boundaries, OpEmptyMatch, OpNoMatch, and
		// anything unrecognized can match without consuming a rune: treat as zero-width.
		return 0
	}
}

// projectedRegexSubmatchIndexBytes returns the heap footprint of the [][]int table
// FindAllStringSubmatchIndex materializes for matchCount matches of a pattern with
// the given group count. Two costs accrue per match: the 2 + 2*groups index ints
// the engine writes, and the structural overhead of the inner slice that holds them
// -- a []int slice header occupying one slot in the outer [][]int backing array
// (estimatedSliceBaseBytes, exactly unsafe.Sizeof([]int{})). Both are charged so the
// projection is matchCount * ((2 + 2*groups) * estimatedIntBytes + estimatedSliceBaseBytes).
//
// The per-match slice overhead matters precisely for the low-capture shapes the int
// payload alone undercounts: a no-capture zero-width or one-byte pattern writes only
// 2 ints (16 bytes) per match yet still pays the 24-byte slice header, so omitting it
// would under-report the table by more than half for every match. Counting it keeps
// the worst-case guard and the accumulator seed -- which share this projection so the
// up-front rejection and the running budget reserve the same bytes -- honest about
// the table's true coexisting footprint rather than just its integer payload.
//
// The table is two allocations with two different counts, and every caller
// builds its figure from the same two units below so they cannot drift: one
// index row per match that exists, and one outer slot per slot the backing
// holds, filled or not. scanMatchBudget bounded the table before the engine ran
// and charged an extra slot per match for growth, while the accumulator seed and
// the preflight priced one slot per match and no growth at all -- two models of
// one quantity that disagreed, and the disagreement was the under-charge.
// Once the table exists its capacity is a fact rather than a model, which is
// what actualRegexSubmatchIndexBytes reads.
func regexSubmatchIndexRowBytes(groups int) int {
	return saturatingMul(saturatingAdd(2, saturatingMul(2, groups)), estimatedIntBytes)
}

// regexSubmatchIndexSlotBytes returns what n outer [][]int slots cost, whether
// or not the engine filled them.
func regexSubmatchIndexSlotBytes(n int) int {
	return saturatingMul(n, estimatedSliceBaseBytes)
}

// projectedRegexSubmatchIndexBytes bounds the table for matchCount matches
// before it exists, when its capacity cannot be read. It assumes one slot per
// match; scanMatchBudget adds a second for the growth Go's append can leave
// behind, which bounds the spare capacity measured here (1.3x to 1.7x length).
func projectedRegexSubmatchIndexBytes(matchCount, groups int) int {
	return saturatingAdd(
		saturatingMul(matchCount, regexSubmatchIndexRowBytes(groups)),
		regexSubmatchIndexSlotBytes(matchCount),
	)
}

// worstCaseRegexSubmatchIndexBytes bounds the table for matchCount matches
// before it exists, including the spare capacity Go's append leaves behind: one
// row per match, and two outer slots, the second covering the growth.
//
// Every figure taken before the table exists comes from here, so the bound that
// decides how many matches to ask for and the reservation that charges them
// cannot disagree. They did: scanMatchBudget priced two slots per match while
// the pre-allocation reservation priced one through projectedRegexSubmatchIndexBytes,
// so the engine could build a table larger than the bytes reserved for it and a
// parent allocating meanwhile saw headroom that was not there. That is the same
// two-models-of-one-quantity drift the comment above records, reintroduced by a
// caller added later.
func worstCaseRegexSubmatchIndexBytes(matchCount, groups int) int {
	return saturatingAdd(
		projectedRegexSubmatchIndexBytes(matchCount, groups),
		regexSubmatchIndexSlotBytes(matchCount),
	)
}

// actualRegexSubmatchIndexBytes returns the table's real footprint once the
// engine has returned it: one row per match, and one slot per slot of capacity.
//
// FindAllStringSubmatchIndex grows its outer slice, so cap can substantially
// exceed len -- 6,485 slots for 5,000 matches, 35,640 bytes the length-based
// figure missed while the table stayed live through the whole build.
func actualRegexSubmatchIndexBytes(allMatches [][]int, groups int) int {
	return saturatingAdd(
		saturatingMul(len(allMatches), regexSubmatchIndexRowBytes(groups)),
		regexSubmatchIndexSlotBytes(cap(allMatches)),
	)
}

func (exec *Execution) guardStringScanOutputFootprint(allMatches [][]int, groups int) error {
	outputBytes := arraySlotBackingBytes(len(allMatches))
	for _, loc := range allMatches {
		outputBytes = saturatingAdd(outputBytes, projectedRegexElementPayloadBytes("", loc, groups))
		if outputBytes > maxRegexInputBytes {
			return exec.latchExhaustion(fmt.Errorf("%w: string.scan output exceeds limit %d bytes", errOutputLimitExceeded, maxRegexInputBytes))
		}
	}
	return nil
}

// projectedRegexElementPayloadBytes returns what one stringScanElement costs
// beyond the slot it occupies in the result: the match string, or the capture
// array with one string per participating group.
//
// text is the subject the pieces are windows onto, and may be empty to price a
// piece that is always copied. clonedWindow allocates nothing for a piece as
// long as the whole subject, so passing the subject charges a header alone for
// it -- `s.scan("^.*$")` copies nothing and must not be billed for the copy it
// does not make. guardStringScanOutputFootprint passes "" because it bounds the
// engine's worst case against a fixed host cap rather than pricing the
// allocation, and lowering that bound would move an existing rejection.
func projectedRegexElementPayloadBytes(text string, loc []int, groups int) int {
	if groups == 0 {
		return projectedRegexWindowBytes(text, loc[0], loc[1])
	}
	// stringScanElement publishes the captures as an array value, so the
	// wrapper it boxes them in is part of what one element costs. The Value
	// naming it is the result slot the caller already prices.
	payload := nestedArrayBackingBytes(groups)
	for g := range groups {
		start := loc[(g+1)*2]
		end := loc[(g+1)*2+1]
		if start < 0 || end < 0 {
			continue
		}
		payload = saturatingAdd(payload, projectedRegexWindowBytes(text, start, end))
	}
	return payload
}

// projectedRegexWindowBytes prices one text[start:end] piece the way
// clonedWindow allocates it.
func projectedRegexWindowBytes(text string, start, end int) int {
	if end >= start && end-start == len(text) && len(text) > 0 {
		return estimatedStringHeaderBytes
	}
	return projectedRegexStringPayloadBytes(start, end)
}

// checkProjectedScanOutputBytes rejects a scan result before any of its matches
// are copied out of the subject.
//
// Everything the array form holds at its peak is weighed here together: the
// call roots, the engine's index table, the result's slots, and every element's
// payload. All four coexist -- the table stays live the whole time the elements
// are built from it -- and checking them apart is what let a subject of many
// small matches followed by one large one through: roots plus output fit, roots
// plus table fit, and their sum did not, so the last match was cloned before
// acc.add could reject it. That allocated 3.71 MiB under a 1.8 MB quota.
//
// The host-cap guard above walks the same element numbers but compares them to
// a fixed limit rather than the quota, and the accumulator's per-element check
// runs only after an element exists, so neither weighed the copies in time.
//
// Counting the table here does not double-charge the acc.reserveScratch that
// follows. This is a one-shot check that stores nothing; that call folds the
// table into the accumulator's own baseline for the per-element checks. Each
// check counts the table exactly once.
func (exec *Execution) checkProjectedScanOutputBytes(text string, allMatches [][]int, groups int, receiver Value, args []Value, kwargs map[string]Value, block Value) error {
	if exec.memoryQuota <= 0 {
		return nil
	}
	payload := 0
	for _, loc := range allMatches {
		payload = saturatingAdd(payload, projectedRegexElementPayloadBytes(text, loc, groups))
	}
	// The engine's table is live scratch, not result payload: it is held for the
	// whole build and freed after, and its capacity is read rather than modeled.
	return exec.checkProjectedArrayBytesWithCallRoots(
		projectedScanResultSlots(allMatches), payload,
		actualRegexSubmatchIndexBytes(allMatches, groups),
		receiver, args, kwargs, block,
	)
}

// reserveYieldedCopy reserves payload for a copy a block iterator is about to
// make and verifies it fits beside the live call roots. The caller must release
// the returned reservation as soon as the copy exists and before the block runs.
//
// The extent is the whole point, and it is bounded on both sides. Reserved any
// later and the copy is allocated before anything weighs it. Held any longer --
// across the block call -- and the same bytes are counted twice: once as this
// reservation and once as the value the block's own checks now reach through the
// bound parameter. Holding it across the call rejected a scan block under a
// 1.5 MB quota holding a 600 KB receiver and one 600 KB copy, and an each_line
// under 2.4 MB holding a 1 MB receiver and one 1 MB copy, both of which fit.
//
// Only one copy is live at a time in these loops: the block argument is
// overwritten each yield, and a copy the block keeps is reachable from the
// block's own roots by the time the next check walks them. That is why a single
// copy is reserved here rather than the whole output the array form projects.
//
// detachedSubstring implements this same contract for members that return one
// value; this exists for scan, whose element may be a capture array of several
// copies rather than a single string.
//
// Known residual: the previous yield is still reachable when the next copy is
// made. The loop's block-argument slot holds it until it is overwritten, and a
// block whose body cannot capture its scope reuses one environment whose
// bindings are cleared at the start of the next call rather than at the end of
// the last, so the copy before it survives across this reservation. The
// transient peak is therefore the receiver and two copies where this charges the
// receiver and one: an each_line over two 1 MB lines that retains neither runs
// under 3,003,966 bytes, while the same receiver with a block that does retain
// both -- which the walk can see -- is charged 4,004,313. The gap is bounded by
// one yielded value, so by the receiver's own length, and scan's block form has
// the same lifetime.
//
// Closing it needs the runner to drop its argument binding when a call ends
// instead of when the next begins, which every iterator built on blockCallRunner
// shares. Charging the previous copy here instead would double-count the case
// where the block kept it, because the walk already reaches it there -- the
// false rejection this reservation's extent exists to avoid.
func (exec *Execution) reserveYieldedCopy(payload int, receiver Value, args []Value, kwargs map[string]Value, block Value) (int, error) {
	delta := exec.reserveLoopScratch(payload)
	if err := exec.checkReservedLoopScratch(receiver, args, kwargs, block); err != nil {
		exec.releaseLoopScratch(delta)
		return 0, err
	}
	return delta, nil
}

func projectedRegexStringPayloadBytes(start, end int) int {
	if end < start {
		return estimatedStringHeaderBytes
	}
	return saturatingAdd(estimatedStringHeaderBytes, end-start)
}

// stringScanElement builds the per-match result element for String#scan: the full
// match string when the pattern has no capture groups, otherwise an array holding
// each captured substring with nil for groups that did not participate. loc is a
// FindAllStringSubmatchIndex result element, indexed into text.
//
// Each piece is copied out of the subject (see clonedWindow). A match is a
// window onto text, so keeping one pinned the whole subject however little it
// matched: 200 three-character matches of a megabyte retained 192.1 MiB under
// an 8 MiB quota. projectedRegexStringPayloadBytes already charges every piece
// its own length, so the guard and the accumulator that ran before this point
// have reserved these bytes.
func stringScanElement(text string, loc []int, groups int) Value {
	if groups == 0 {
		return NewString(clonedWindow(text, text[loc[0]:loc[1]]))
	}
	captures := make([]Value, groups)
	for g := range groups {
		start := loc[(g+1)*2]
		end := loc[(g+1)*2+1]
		if start < 0 || end < 0 {
			captures[g] = NewNil()
			continue
		}
		captures[g] = NewString(clonedWindow(text, text[start:end]))
	}
	return NewArray(captures)
}

func stringMemberTextOps(property string) (Value, error) {
	switch property {
	case "sub":
		return NewAutoBuiltin("string.sub", func(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
			updated, _, err := stringReplaceResult(exec, "string.sub", receiver, args, kwargs, block, false)
			if err != nil {
				return NewNil(), err
			}
			return NewString(updated), nil
		}), nil
	case "sub!":
		return NewAutoBuiltin("string.sub!", func(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
			updated, matched, err := stringReplaceResult(exec, "string.sub!", receiver, args, kwargs, block, false)
			if err != nil {
				return NewNil(), err
			}
			return stringReplaceBangResult(updated, matched), nil
		}), nil
	case "gsub":
		return NewAutoBuiltin("string.gsub", func(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
			updated, _, err := stringReplaceResult(exec, "string.gsub", receiver, args, kwargs, block, true)
			if err != nil {
				return NewNil(), err
			}
			return NewString(updated), nil
		}), nil
	case "gsub!":
		return NewAutoBuiltin("string.gsub!", func(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
			updated, matched, err := stringReplaceResult(exec, "string.gsub!", receiver, args, kwargs, block, true)
			if err != nil {
				return NewNil(), err
			}
			return stringReplaceBangResult(updated, matched), nil
		}), nil
	case "split":
		return NewAutoBuiltin("string.split", func(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
			return stringSplitResultFromArgs(exec, receiver, args, kwargs, block)
		}), nil
	case "partition":
		return NewAutoBuiltin("string.partition", func(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
			if len(args) != 1 || len(kwargs) > 0 {
				return NewNil(), fmt.Errorf("string.partition expects exactly one separator")
			}
			if args[0].Kind() != KindString {
				return NewNil(), fmt.Errorf("string.partition separator must be string")
			}
			head, sep, tail := stringPartition(receiver.String(), args[0].String())
			return detachedPartitionValue(exec, receiver, head, sep, tail, args, kwargs, block)
		}), nil
	case "rpartition":
		return NewAutoBuiltin("string.rpartition", func(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
			if len(args) != 1 || len(kwargs) > 0 {
				return NewNil(), fmt.Errorf("string.rpartition expects exactly one separator")
			}
			if args[0].Kind() != KindString {
				return NewNil(), fmt.Errorf("string.rpartition separator must be string")
			}
			head, sep, tail := stringRPartition(receiver.String(), args[0].String())
			return detachedPartitionValue(exec, receiver, head, sep, tail, args, kwargs, block)
		}), nil
	case "chars":
		return NewAutoBuiltin("string.chars", func(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
			if len(args) > 0 || len(kwargs) > 0 {
				return NewNil(), fmt.Errorf("string.chars does not take arguments")
			}
			text := receiver.String()
			values := make([]Value, 0, stringRuneLen(text))
			for _, r := range text {
				values = append(values, NewString(string(r)))
			}
			return NewArray(values), nil
		}), nil
	case "lines":
		return NewAutoBuiltin("string.lines", func(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
			if len(args) > 0 || len(kwargs) > 0 {
				return NewNil(), fmt.Errorf("string.lines does not take arguments")
			}
			text := receiver.String()
			count, payload := projectedStringLines(text)
			// No scratch: forEachLine walks without materializing the lines.
			if err := exec.checkProjectedArrayBytesWithCallRoots(count, payload, 0, receiver, args, kwargs, block); err != nil {
				return NewNil(), err
			}
			values := make([]Value, 0, count)
			_ = forEachLine(text, func(line string) error {
				values = append(values, NewString(clonedWindow(text, line)))
				return nil
			})
			return NewArray(values), nil
		}), nil
	case "bytes":
		return NewAutoBuiltin("string.bytes", func(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
			if len(args) > 0 || len(kwargs) > 0 {
				return NewNil(), fmt.Errorf("string.bytes does not take arguments")
			}
			text := receiver.String()
			// Reject the allocation up front so a string that fits the memory
			// quota cannot reserve a result array of one Value per byte that
			// does not. make([]Value, len(text)) would reserve the entire
			// backing array before the post-call check could observe it.
			if err := exec.checkProjectedIntArrayBytes(len(text)); err != nil {
				return NewNil(), err
			}
			values := make([]Value, len(text))
			for i := range len(text) {
				values[i] = NewInt(int64(text[i]))
			}
			return NewArray(values), nil
		}), nil
	case "codepoints":
		return NewAutoBuiltin("string.codepoints", func(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
			if len(args) > 0 || len(kwargs) > 0 {
				return NewNil(), fmt.Errorf("string.codepoints does not take arguments")
			}
			text := receiver.String()
			// Reject the allocation up front so a string that fits the memory
			// quota cannot reserve a result array of one Value per code point that
			// does not, mirroring the guard on bytes.
			if err := exec.checkProjectedIntArrayBytes(stringRuneLen(text)); err != nil {
				return NewNil(), err
			}
			values := make([]Value, 0, stringRuneLen(text))
			for _, r := range text {
				values = append(values, NewInt(int64(r)))
			}
			return NewArray(values), nil
		}), nil
	case "each_char":
		return NewAutoBuiltin("string.each_char", func(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
			if len(args) > 0 || len(kwargs) > 0 {
				return NewNil(), fmt.Errorf("string.each_char does not take arguments")
			}
			runner, err := newBlockCallRunner(exec, block, "string.each_char", receiver, nil, kwargs)
			if err != nil {
				return NewNil(), err
			}
			var blockArg [1]Value
			for _, r := range receiver.String() {
				blockArg[0] = NewString(string(r))
				if _, err := runner.call(blockArg[:]); err != nil {
					return NewNil(), err
				}
			}
			return receiver, nil
		}), nil
	case "each_byte":
		return NewAutoBuiltin("string.each_byte", func(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
			if len(args) > 0 || len(kwargs) > 0 {
				return NewNil(), fmt.Errorf("string.each_byte does not take arguments")
			}
			runner, err := newBlockCallRunner(exec, block, "string.each_byte", receiver, nil, kwargs)
			if err != nil {
				return NewNil(), err
			}
			var blockArg [1]Value
			text := receiver.String()
			for i := range len(text) {
				blockArg[0] = NewInt(int64(text[i]))
				if _, err := runner.call(blockArg[:]); err != nil {
					return NewNil(), err
				}
			}
			return receiver, nil
		}), nil
	case "each_codepoint":
		return NewAutoBuiltin("string.each_codepoint", func(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
			if len(args) > 0 || len(kwargs) > 0 {
				return NewNil(), fmt.Errorf("string.each_codepoint does not take arguments")
			}
			runner, err := newBlockCallRunner(exec, block, "string.each_codepoint", receiver, nil, kwargs)
			if err != nil {
				return NewNil(), err
			}
			var blockArg [1]Value
			for _, r := range receiver.String() {
				blockArg[0] = NewInt(int64(r))
				if _, err := runner.call(blockArg[:]); err != nil {
					return NewNil(), err
				}
			}
			return receiver, nil
		}), nil
	case "each_line":
		return NewAutoBuiltin("string.each_line", func(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
			if len(args) > 0 || len(kwargs) > 0 {
				return NewNil(), fmt.Errorf("string.each_line does not take arguments")
			}
			text := receiver.String()
			runner, err := newBlockCallRunner(exec, block, "string.each_line", receiver, nil, kwargs)
			if err != nil {
				return NewNil(), err
			}
			var blockArg [1]Value
			if err := forEachLine(text, func(line string) error {
				// Reserved across the copy and released the moment it exists,
				// so it is never unweighed and never counted twice while the
				// block runs. This is detachedSubstring's contract without its
				// deferred release, which cost more than the check it guards
				// when a document yields thousands of short lines.
				copyDelta, err := exec.reserveYieldedCopy(
					detachedWindowPayloadBytes(text, line), receiver, args, kwargs, block,
				)
				if err != nil {
					return err
				}
				blockArg[0] = NewString(clonedWindow(text, line))
				exec.releaseLoopScratch(copyDelta)
				_, err = runner.call(blockArg[:])
				return err
			}); err != nil {
				return NewNil(), err
			}
			return receiver, nil
		}), nil
	case "template":
		return NewAutoBuiltin("string.template", func(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
			if len(args) != 1 {
				return NewNil(), fmt.Errorf("string.template expects exactly one context hash")
			}
			if args[0].Kind() != KindHash && args[0].Kind() != KindObject {
				return NewNil(), fmt.Errorf("string.template context must be hash")
			}
			strict, err := stringTemplateOption(kwargs)
			if err != nil {
				return NewNil(), err
			}
			rendered, err := stringTemplateWithExecution(exec, receiver.String(), args[0], strict, receiver, args, kwargs, block)
			if err != nil {
				return NewNil(), err
			}
			return NewString(rendered), nil
		}), nil
	default:
		return NewNil(), fmt.Errorf("unknown string method %s", property)
	}
}

func stringMemberPadding(property string) (Value, error) {
	switch property {
	case "center":
		return NewAutoBuiltin("string.center", func(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
			return stringPad(exec, "string.center", padCenter, receiver, args, kwargs)
		}), nil
	case "ljust":
		return NewAutoBuiltin("string.ljust", func(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
			return stringPad(exec, "string.ljust", padRight, receiver, args, kwargs)
		}), nil
	case "rjust":
		return NewAutoBuiltin("string.rjust", func(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
			return stringPad(exec, "string.rjust", padLeft, receiver, args, kwargs)
		}), nil
	default:
		return NewNil(), fmt.Errorf("unknown string method %s", property)
	}
}

// padSide selects how padding runes are distributed around the receiver.
type padSide int

const (
	padRight padSide = iota
	padLeft
	padCenter
)

// stringPad implements the shared logic for center, ljust, and rjust. Width is
// measured in runes to mirror Ruby's character-oriented padding, and a width at
// or below the receiver length returns the receiver unchanged. Float widths are
// truncated toward zero like Ruby's to_int; a non-finite or out-of-range Float
// width is rejected outright rather than wrapping into an in-range int that
// would slip past the projected-size guard. The padding string defaults to a
// single space, must be non-empty, and is repeated then truncated by runes to
// fill the requested span. The projected byte length is checked against the
// memory quota before any buffer is allocated so an oversized width fails fast
// instead of materializing a huge string.
func stringPad(exec *Execution, method string, side padSide, receiver Value, args []Value, kwargs map[string]Value) (Value, error) {
	if len(kwargs) > 0 {
		return NewNil(), fmt.Errorf("%s does not accept keyword arguments", method)
	}
	if len(args) < 1 || len(args) > 2 {
		return NewNil(), fmt.Errorf("%s expects width and optional pad string", method)
	}
	width, err := valueToPadWidth(args[0])
	if err != nil {
		if errors.Is(err, errWidthOutOfRange) {
			return NewNil(), fmt.Errorf("%s width is out of range", method)
		}
		return NewNil(), fmt.Errorf("%s width must be integer", method)
	}
	pad := " "
	if len(args) == 2 {
		if args[1].Kind() != KindString {
			return NewNil(), fmt.Errorf("%s pad must be string", method)
		}
		pad = args[1].String()
	}
	if pad == "" {
		return NewNil(), fmt.Errorf("%s pad must not be empty", method)
	}

	text := receiver.String()
	srcRunes := stringRuneLen(text)
	if width <= srcRunes {
		return receiver, nil
	}

	totalPad := width - srcRunes
	leftPad, rightPad := 0, 0
	switch side {
	case padLeft:
		leftPad = totalPad
	case padRight:
		rightPad = totalPad
	case padCenter:
		leftPad = totalPad / 2
		rightPad = totalPad - leftPad
	}

	// Saturating arithmetic keeps the projected size from overflowing on a huge
	// width; the quota check below rejects anything that large regardless.
	projected := saturatingAdd(len(text), saturatingAdd(padRuneBytes(pad, leftPad), padRuneBytes(pad, rightPad)))
	// Padding is the one string operation whose cost comes from a number rather
	// than from its inputs: "".ljust(8_000_000, "x") writes eight million bytes
	// from two inputs of a handful of bytes each, so the receiver-and-arguments
	// charge sees nothing. Charge what will actually be written.
	if err := exec.chargeStringScan(projected); err != nil {
		return NewNil(), err
	}
	if err := exec.checkProjectedStringBytes(projected); err != nil {
		return NewNil(), err
	}

	var b strings.Builder
	// Only preallocate when the projected size is exact; a saturated value means
	// the request overflowed int and would never fit in memory anyway.
	if projected < math.MaxInt {
		b.Grow(projected)
	}
	writePadRunes(&b, pad, leftPad)
	b.WriteString(text)
	writePadRunes(&b, pad, rightPad)
	return NewString(b.String()), nil
}

// padRuneBytes reports how many bytes count runes drawn from pad occupy. The
// pad string is conceptually repeated and then truncated to count runes, so
// full repeats contribute their whole byte length and the remainder contributes
// a rune-aligned prefix. The byte total saturates at math.MaxInt so an
// oversized count cannot overflow the projected-size check.
func padRuneBytes(pad string, count int) int {
	if count <= 0 {
		return 0
	}
	padRunes := stringRuneLen(pad)
	full := count / padRunes
	remainder := count % padRunes
	return saturatingAdd(saturatingMul(full, len(pad)), padPrefixBytes(pad, remainder))
}

// saturatingAdd returns a+b clamped to math.MaxInt instead of overflowing. Both
// operands are non-negative byte counts.
func saturatingAdd(a, b int) int {
	if a > math.MaxInt-b {
		return math.MaxInt
	}
	return a + b
}

// saturatingMul returns a*b clamped to math.MaxInt instead of overflowing. Both
// operands are non-negative byte counts.
func saturatingMul(a, b int) int {
	if a == 0 || b == 0 {
		return 0
	}
	if a > math.MaxInt/b {
		return math.MaxInt
	}
	return a * b
}

// padPrefixBytes returns the byte length of the first runes of pad.
func padPrefixBytes(pad string, runes int) int {
	if runes <= 0 {
		return 0
	}
	seen := 0
	for i := range pad {
		if seen == runes {
			return i
		}
		seen++
	}
	return len(pad)
}

// writePadRunes appends count runes drawn from pad to b, repeating pad and
// truncating the final repeat to a rune boundary.
func writePadRunes(b *strings.Builder, pad string, count int) {
	if count <= 0 {
		return
	}
	padRunes := stringRuneLen(pad)
	full := count / padRunes
	for range full {
		b.WriteString(pad)
	}
	remainder := count % padRunes
	if remainder == 0 {
		return
	}
	b.WriteString(pad[:padPrefixBytes(pad, remainder)])
}

func stringMemberTransforms(property string) (Value, error) {
	switch property {
	case "strip":
		return NewAutoBuiltin("string.strip", func(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
			if len(args) > 0 {
				return NewNil(), fmt.Errorf("string.strip does not take arguments")
			}
			text := receiver.String()
			return detachedStringValue(exec, text, rubyStrip(text), receiver, args, kwargs, block)
		}), nil
	case "strip!":
		return NewAutoBuiltin("string.strip!", func(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
			if len(args) > 0 {
				return NewNil(), fmt.Errorf("string.strip! does not take arguments")
			}
			original := receiver.String()
			return detachedBangResult(exec, original, rubyStrip(original), receiver, args, kwargs, block)
		}), nil
	case "squish":
		return NewAutoBuiltin("string.squish", func(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
			if len(args) > 0 {
				return NewNil(), fmt.Errorf("string.squish does not take arguments")
			}
			return NewString(stringSquish(receiver.String())), nil
		}), nil
	case "squish!":
		return NewAutoBuiltin("string.squish!", func(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
			if len(args) > 0 {
				return NewNil(), fmt.Errorf("string.squish! does not take arguments")
			}
			updated := stringSquish(receiver.String())
			return stringBangResult(receiver.String(), updated), nil
		}), nil
	case "lstrip":
		return NewAutoBuiltin("string.lstrip", func(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
			if len(args) > 0 {
				return NewNil(), fmt.Errorf("string.lstrip does not take arguments")
			}
			text := receiver.String()
			return detachedStringValue(exec, text, rubyLstrip(text), receiver, args, kwargs, block)
		}), nil
	case "lstrip!":
		return NewAutoBuiltin("string.lstrip!", func(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
			if len(args) > 0 {
				return NewNil(), fmt.Errorf("string.lstrip! does not take arguments")
			}
			original := receiver.String()
			return detachedBangResult(exec, original, rubyLstrip(original), receiver, args, kwargs, block)
		}), nil
	case "rstrip":
		return NewAutoBuiltin("string.rstrip", func(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
			if len(args) > 0 {
				return NewNil(), fmt.Errorf("string.rstrip does not take arguments")
			}
			text := receiver.String()
			return detachedStringValue(exec, text, rubyRstrip(text), receiver, args, kwargs, block)
		}), nil
	case "rstrip!":
		return NewAutoBuiltin("string.rstrip!", func(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
			if len(args) > 0 {
				return NewNil(), fmt.Errorf("string.rstrip! does not take arguments")
			}
			original := receiver.String()
			return detachedBangResult(exec, original, rubyRstrip(original), receiver, args, kwargs, block)
		}), nil
	case "chomp":
		return NewAutoBuiltin("string.chomp", func(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
			if len(args) > 1 {
				return NewNil(), fmt.Errorf("string.chomp accepts at most one separator")
			}
			text := receiver.String()
			if len(args) == 0 {
				return NewString(chompDefault(text)), nil
			}
			if args[0].Kind() == KindNil {
				// Ruby treats a nil separator as "do not chomp" and returns
				// the receiver unchanged.
				return NewString(text), nil
			}
			if args[0].Kind() != KindString {
				return NewNil(), fmt.Errorf("string.chomp separator must be string")
			}
			sep := args[0].String()
			if sep == "" {
				return detachedStringValue(exec, text, strings.TrimRight(text, "\r\n"), receiver, args, kwargs, block)
			}
			if strings.HasSuffix(text, sep) {
				return detachedStringValue(exec, text, text[:len(text)-len(sep)], receiver, args, kwargs, block)
			}
			return NewString(text), nil
		}), nil
	case "chomp!":
		return NewAutoBuiltin("string.chomp!", func(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
			if len(args) > 1 {
				return NewNil(), fmt.Errorf("string.chomp! accepts at most one separator")
			}
			original := receiver.String()
			if len(args) == 0 {
				return stringBangResult(original, chompDefault(original)), nil
			}
			if args[0].Kind() == KindNil {
				// Ruby treats a nil separator as "do not chomp"; since no
				// change occurs, the mutator form returns nil.
				return NewNil(), nil
			}
			if args[0].Kind() != KindString {
				return NewNil(), fmt.Errorf("string.chomp! separator must be string")
			}
			sep := args[0].String()
			if sep == "" {
				return detachedBangResult(exec, original, strings.TrimRight(original, "\r\n"), receiver, args, kwargs, block)
			}
			if strings.HasSuffix(original, sep) {
				return detachedBangResult(exec, original, original[:len(original)-len(sep)], receiver, args, kwargs, block)
			}
			return NewNil(), nil
		}), nil
	case "chop":
		return NewAutoBuiltin("string.chop", func(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
			if len(args) > 0 {
				return NewNil(), fmt.Errorf("string.chop does not take arguments")
			}
			return NewString(chopDefault(receiver.String())), nil
		}), nil
	case "chop!":
		return NewAutoBuiltin("string.chop!", func(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
			if len(args) > 0 {
				return NewNil(), fmt.Errorf("string.chop! does not take arguments")
			}
			original := receiver.String()
			return stringBangResult(original, chopDefault(original)), nil
		}), nil
	case "delete":
		return NewAutoBuiltin("string.delete", func(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
			updated, err := stringDeleteChars(exec, receiver, args, kwargs, block, "string.delete")
			if err != nil {
				return NewNil(), err
			}
			return NewString(updated), nil
		}), nil
	case "delete!":
		return NewAutoBuiltin("string.delete!", func(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
			original := receiver.String()
			updated, err := stringDeleteChars(exec, receiver, args, kwargs, block, "string.delete!")
			if err != nil {
				return NewNil(), err
			}
			return stringBangResult(original, updated), nil
		}), nil
	case "delete_prefix":
		return NewAutoBuiltin("string.delete_prefix", func(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
			if len(args) != 1 {
				return NewNil(), fmt.Errorf("string.delete_prefix expects exactly one prefix")
			}
			if args[0].Kind() != KindString {
				return NewNil(), fmt.Errorf("string.delete_prefix prefix must be string")
			}
			text := receiver.String()
			return detachedStringValue(exec, text, strings.TrimPrefix(text, args[0].String()), receiver, args, kwargs, block)
		}), nil
	case "delete_prefix!":
		return NewAutoBuiltin("string.delete_prefix!", func(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
			if len(args) != 1 {
				return NewNil(), fmt.Errorf("string.delete_prefix! expects exactly one prefix")
			}
			if args[0].Kind() != KindString {
				return NewNil(), fmt.Errorf("string.delete_prefix! prefix must be string")
			}
			original := receiver.String()
			return detachedBangResult(exec, original, strings.TrimPrefix(original, args[0].String()), receiver, args, kwargs, block)
		}), nil
	case "delete_suffix":
		return NewAutoBuiltin("string.delete_suffix", func(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
			if len(args) != 1 {
				return NewNil(), fmt.Errorf("string.delete_suffix expects exactly one suffix")
			}
			if args[0].Kind() != KindString {
				return NewNil(), fmt.Errorf("string.delete_suffix suffix must be string")
			}
			text := receiver.String()
			return detachedStringValue(exec, text, strings.TrimSuffix(text, args[0].String()), receiver, args, kwargs, block)
		}), nil
	case "delete_suffix!":
		return NewAutoBuiltin("string.delete_suffix!", func(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
			if len(args) != 1 {
				return NewNil(), fmt.Errorf("string.delete_suffix! expects exactly one suffix")
			}
			if args[0].Kind() != KindString {
				return NewNil(), fmt.Errorf("string.delete_suffix! suffix must be string")
			}
			original := receiver.String()
			return detachedBangResult(exec, original, strings.TrimSuffix(original, args[0].String()), receiver, args, kwargs, block)
		}), nil
	case "tr":
		return NewAutoBuiltin("string.tr", func(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
			updated, err := stringTrChars(exec, receiver, args, kwargs, block, "string.tr")
			if err != nil {
				return NewNil(), err
			}
			return NewString(updated), nil
		}), nil
	case "tr!":
		return NewAutoBuiltin("string.tr!", func(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
			original := receiver.String()
			updated, err := stringTrChars(exec, receiver, args, kwargs, block, "string.tr!")
			if err != nil {
				return NewNil(), err
			}
			return stringBangResult(original, updated), nil
		}), nil
	case "squeeze":
		return NewAutoBuiltin("string.squeeze", func(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
			updated, err := stringSqueezeChars(exec, receiver, args, kwargs, block, "string.squeeze")
			if err != nil {
				return NewNil(), err
			}
			return NewString(updated), nil
		}), nil
	case "squeeze!":
		return NewAutoBuiltin("string.squeeze!", func(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
			original := receiver.String()
			updated, err := stringSqueezeChars(exec, receiver, args, kwargs, block, "string.squeeze!")
			if err != nil {
				return NewNil(), err
			}
			return stringBangResult(original, updated), nil
		}), nil
	case "upcase":
		return NewAutoBuiltin("string.upcase", func(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
			mode, err := parseCaseMode("upcase", args, false)
			if err != nil {
				return NewNil(), err
			}
			if err := checkStringCaseTransform(exec, receiver.String(), mode, receiver, args, kwargs, block); err != nil {
				return NewNil(), err
			}
			return NewString(stringUpcase(receiver.String(), mode)), nil
		}), nil
	case "upcase!":
		return NewAutoBuiltin("string.upcase!", func(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
			mode, err := parseCaseMode("upcase!", args, false)
			if err != nil {
				return NewNil(), err
			}
			original := receiver.String()
			if err := checkStringCaseTransform(exec, original, mode, receiver, args, kwargs, block); err != nil {
				return NewNil(), err
			}
			return stringBangResult(original, stringUpcase(original, mode)), nil
		}), nil
	case "downcase":
		return NewAutoBuiltin("string.downcase", func(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
			mode, err := parseCaseMode("downcase", args, true)
			if err != nil {
				return NewNil(), err
			}
			if err := checkStringCaseTransform(exec, receiver.String(), mode, receiver, args, kwargs, block); err != nil {
				return NewNil(), err
			}
			return NewString(stringDowncase(receiver.String(), mode)), nil
		}), nil
	case "downcase!":
		return NewAutoBuiltin("string.downcase!", func(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
			mode, err := parseCaseMode("downcase!", args, true)
			if err != nil {
				return NewNil(), err
			}
			original := receiver.String()
			if err := checkStringCaseTransform(exec, original, mode, receiver, args, kwargs, block); err != nil {
				return NewNil(), err
			}
			return stringBangResult(original, stringDowncase(original, mode)), nil
		}), nil
	case "capitalize":
		return NewAutoBuiltin("string.capitalize", func(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
			mode, err := parseCaseMode("capitalize", args, false)
			if err != nil {
				return NewNil(), err
			}
			if err := checkStringCaseTransform(exec, receiver.String(), mode, receiver, args, kwargs, block); err != nil {
				return NewNil(), err
			}
			return NewString(stringCapitalize(receiver.String(), mode)), nil
		}), nil
	case "capitalize!":
		return NewAutoBuiltin("string.capitalize!", func(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
			mode, err := parseCaseMode("capitalize!", args, false)
			if err != nil {
				return NewNil(), err
			}
			original := receiver.String()
			if err := checkStringCaseTransform(exec, original, mode, receiver, args, kwargs, block); err != nil {
				return NewNil(), err
			}
			return stringBangResult(original, stringCapitalize(original, mode)), nil
		}), nil
	case "swapcase":
		return NewAutoBuiltin("string.swapcase", func(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
			mode, err := parseCaseMode("swapcase", args, false)
			if err != nil {
				return NewNil(), err
			}
			if err := checkStringCaseTransform(exec, receiver.String(), mode, receiver, args, kwargs, block); err != nil {
				return NewNil(), err
			}
			return NewString(stringSwapCase(receiver.String(), mode)), nil
		}), nil
	case "swapcase!":
		return NewAutoBuiltin("string.swapcase!", func(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
			mode, err := parseCaseMode("swapcase!", args, false)
			if err != nil {
				return NewNil(), err
			}
			original := receiver.String()
			if err := checkStringCaseTransform(exec, original, mode, receiver, args, kwargs, block); err != nil {
				return NewNil(), err
			}
			return stringBangResult(original, stringSwapCase(original, mode)), nil
		}), nil
	case "reverse":
		return NewAutoBuiltin("string.reverse", func(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
			if len(args) > 0 {
				return NewNil(), fmt.Errorf("string.reverse does not take arguments")
			}
			if err := checkStringReverseTransform(exec, receiver.String(), receiver, args, kwargs, block); err != nil {
				return NewNil(), err
			}
			return NewString(stringReverse(receiver.String())), nil
		}), nil
	case "reverse!":
		return NewAutoBuiltin("string.reverse!", func(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
			if len(args) > 0 {
				return NewNil(), fmt.Errorf("string.reverse! does not take arguments")
			}
			if err := checkStringReverseTransform(exec, receiver.String(), receiver, args, kwargs, block); err != nil {
				return NewNil(), err
			}
			updated := stringReverse(receiver.String())
			return stringBangResult(receiver.String(), updated), nil
		}), nil
	default:
		return NewNil(), fmt.Errorf("unknown string method %s", property)
	}
}

// clonedWindow copies sub out of text's backing allocation.
//
// A Go substring holds its whole backing, while the memory estimator prices a
// string by its own length. A script could therefore keep a one-byte slice of
// a megabyte string and be charged one byte: 200 such slices retained 192 MiB
// under an 8 MiB quota, by byteslice (#2), by a bracket read (#36), by a
// partition component (#42) and by slice (#50) alike.
//
// Every proper window is copied, not just a small one. A threshold looks
// tempting -- a window keeping most of its source wastes little -- but the
// waste composes: `s = s.slice(0, s.length / 2)` repeated keeps at least half
// each time and so never trips a threshold, while every intermediate result
// still points at the original allocation. Copying unconditionally is what
// makes a string's footprint equal its length, which is the property the
// estimator prices against. Only a window as long as text itself is left alone,
// because a window that long has no backing to detach from. An empty window is
// copied like any other: strings.Clone yields the shared empty string for it, so
// a component charged zero bytes cannot pin a megabyte.
//
// The copy is unpriced here; pricing belongs to whoever knows the whole result.
// detachedSubstring reserves a single copy, and detachedPartitionValue projects
// an entire three-element array.
func clonedWindow(text, sub string) string {
	if len(sub) == len(text) {
		return sub
	}
	return strings.Clone(sub)
}

// detachedSubstring returns sub detached from text's backing, priced as the one
// string value the caller is about to build from it.
//
// The bytes are reserved before they are allocated. The copy is live alongside
// the string it was taken from, and for an ephemeral receiver --
// `s.reverse.slice(...)` -- that receiver is reachable only from the Go stack,
// so the pre-call check sees no copy and the post-call check no longer sees the
// receiver. Folding the copy into the reserved scratch and rechecking the live
// call roots is what prices that peak.
//
// The value header is reserved with the bytes. The caller wraps the copy the
// moment this returns, by which point the reservation is gone and the receiver
// is once again invisible, so a reservation covering only the payload leaves
// that last allocation unweighed.
func detachedSubstring(exec *Execution, text, sub string, receiver Value, args []Value, kwargs map[string]Value, block Value) (string, error) {
	if len(sub) == len(text) {
		return sub, nil
	}
	// Some builtins are reachable without an execution, and those have no quota
	// to reserve against.
	if exec != nil {
		delta := exec.reserveLoopScratch(saturatingAdd(len(sub), estimatedValueBytes+estimatedStringHeaderBytes))
		defer exec.releaseLoopScratch(delta)
		if err := exec.checkReservedLoopScratch(receiver, args, kwargs, block); err != nil {
			return "", err
		}
	}
	return clonedWindow(text, sub), nil
}

// detachedStringValue wraps sub as the string value a member returns, detached
// from text's backing (see detachedSubstring).
//
// It is the shape almost every substring-returning member needs: one window,
// one value, priced together. detachedByteslice sits beside it rather than
// above it -- byteslice adds a scan charge because it is exempt from the
// receiver-length charge chargeStringScanBeforeCall applies to everyone else,
// and billing that charge here would double-charge every caller.
func detachedStringValue(exec *Execution, text, sub string, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
	detached, err := detachedSubstring(exec, text, sub, receiver, args, kwargs, block)
	if err != nil {
		return NewNil(), err
	}
	return NewString(detached), nil
}

// detachedBangResult is stringBangResult for a mutator whose result is a window
// onto its receiver.
//
// The unchanged case returns nil and never builds a value, so it is answered
// before anything is copied or reserved. Every other case differs from the
// receiver in length -- these mutators only ever remove bytes -- which is
// exactly when detachedSubstring copies, so the two agree on when work happens.
func detachedBangResult(exec *Execution, original, updated string, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
	if updated == original {
		return NewNil(), nil
	}
	return detachedStringValue(exec, original, updated, receiver, args, kwargs, block)
}

// detachedPartitionValue builds the three-element result String#partition and
// String#rpartition return, with the head and tail copied out of the receiver's
// backing (see clonedWindow).
//
// Both components are windows onto the receiver, so a separator near either edge
// leaves the retained side tiny while it pins the whole receiver: keeping
// `("a|" + big).partition("|")[0]` was charged about one byte and held a
// megabyte, 200 of them 192 MiB under an 8 MiB quota (#42). An edge separator's
// empty component goes through the same copy rather than being skipped as
// already free: it is charged nothing at all, so nothing else bounds what it
// could pin. Its backing pointer happens to disappear today when the component
// is boxed into a Value's `any` payload, because Go maps the empty string to a
// shared zero value there, but that is a property of the payload and not of the
// component.
//
// The whole result is projected up front rather than the copies being reserved
// and released around them. Both copies stay live in Go locals while the array
// is built, the receiver may be ephemeral, and a reservation that ends before
// NewArray leaves the array's own structure weighed against neither: the same
// script needed 2,008,673 bytes of quota before this and needs 2,008,873 now.
//
// The copies take no step charge of their own: chargeStringScanBeforeCall
// already billed the receiver's length, and the head and tail are disjoint
// windows onto it.
func detachedPartitionValue(exec *Execution, receiver Value, head, sep, tail string, args []Value, kwargs map[string]Value, block Value) (Value, error) {
	text := receiver.String()
	if exec != nil {
		if err := exec.checkProjectedPartitionBytes(text, head, tail, receiver, args, kwargs, block); err != nil {
			return NewNil(), err
		}
	}
	return NewArray([]Value{
		NewString(clonedWindow(text, head)),
		NewString(sep),
		NewString(clonedWindow(text, tail)),
	}), nil
}

// projectedStringLines counts the lines in text and what they cost once each
// owns its bytes, without allocating the lines or a slice to hold them.
//
// String#lines used to materialize a []string and copy out of it, so the
// receiver, that slice, the result's slots and every copy were live together
// while only the last two were priced -- 81,920 bytes unpriced against 200,056
// charged for a 4,000-line receiver, a quarter to a third of the charge at every
// size and growing with the line count. Walking twice removes the scratch
// instead of pricing it: forEachLine allocates nothing, so the peak is now the
// result alone and the projection describes all of it.
//
// A receiver with no line ending at all is one line spanning the whole string,
// which clonedWindow hands back untouched and detachedWindowPayloadBytes charges
// a header for, so the common single-line case still projects nothing but the
// array.
func projectedStringLines(text string) (count, payload int) {
	_ = forEachLine(text, func(line string) error {
		count++
		payload = saturatingAdd(payload, detachedWindowPayloadBytes(text, line))
		return nil
	})
	return count, payload
}

// checkProjectedPartitionBytes rejects a partition result before any part of it
// exists: the two detached components, the three string values wrapping them,
// and the three-slot array, all weighed together against the call's live roots.
//
// detachedWindowPayloadBytes prices each component: one that still shares the
// receiver's backing (the whole string, when the separator is absent) or is
// empty costs only its header, because clonedWindow allocates nothing for it.
// The separator is the argument's own string, already counted among the roots,
// so only the value header wrapping it is new.
func (exec *Execution) checkProjectedPartitionBytes(text, head, tail string, receiver Value, args []Value, kwargs map[string]Value, block Value) error {
	payload := saturatingAdd(detachedWindowPayloadBytes(text, head), detachedWindowPayloadBytes(text, tail))
	payload = saturatingAdd(payload, estimatedStringHeaderBytes)
	return exec.checkProjectedArrayBytesWithCallRoots(3, payload, 0, receiver, args, kwargs, block)
}

// detachedWindow returns w's characters as a string that does not hold text's
// backing allocation. A window that already owns its bytes is handed straight
// back: it has nothing left to detach, and copying it again would both waste
// the copy and hide a live intermediate from the reservation (see stringWindow).
func detachedWindow(exec *Execution, text string, w stringWindow, receiver Value, args []Value, kwargs map[string]Value, block Value) (string, error) {
	if w.detached {
		return w.text, nil
	}
	return detachedSubstring(exec, text, w.text, receiver, args, kwargs, block)
}

// detachedByteslice detaches a String#byteslice result and bills the copy.
//
// The copy bills the bytes it writes rather than the bytes it was taken from.
// Charging by the receiver would make a one-byte slice of a large host string
// cost the whole string, which is work byteslice never does, so byteslice stays
// exempt from the receiver-length charge and pays for its own copy here. The
// other detaching methods already paid for their receiver through
// chargeStringScanBeforeCall, and a copy can never exceed the receiver it comes
// from, so billing them again here would double-charge.
func detachedByteslice(exec *Execution, text, sub string, receiver Value, args []Value, kwargs map[string]Value, block Value) (string, error) {
	if exec != nil && len(sub) != len(text) {
		if err := exec.chargeStringScan(len(sub)); err != nil {
			return "", err
		}
	}
	return detachedSubstring(exec, text, sub, receiver, args, kwargs, block)
}
