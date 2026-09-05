package runtime

import (
	"fmt"
	"math"
	"strings"

	"github.com/mgomes/vibescript/vibes/value"
)

// regexWork carries short scans across arguments and matches so splitting work
// into pieces smaller than one step cannot make the aggregate free.
type regexWork struct {
	exec      *Execution
	remainder int
}

func (w *regexWork) charge(n int) error {
	if w == nil || w.exec == nil {
		return nil
	}
	steps := n / stringScanBytesPerStep
	w.remainder += n % stringScanBytesPerStep
	steps += w.remainder / stringScanBytesPerStep
	w.remainder %= stringScanBytesPerStep
	if steps == 0 {
		return nil
	}
	return w.exec.stepN(steps)
}

func compileRegexpNamespace(exec *Execution, method, pattern string) (Value, error) {
	work := regexWork{exec: exec}
	re, err := compileRegexNamespacePattern(&work, method, pattern)
	if err != nil {
		return NewNil(), err
	}
	return NewRegex(value.Regex{Source: pattern, Compiled: re}), nil
}

func regexpEscape(exec *Execution, receiver Value, args []Value) (string, error) {
	text := args[0].String()
	if err := exec.chargeStringScan(len(text)); err != nil {
		return "", err
	}
	size, ok := regexpQuotedSize(text, math.MaxInt)
	if !ok {
		return "", guardLimitErrorf("Regexp.escape output exceeds limit %d bytes", math.MaxInt)
	}
	if size == len(text) {
		return text, nil
	}
	if err := exec.chargeStringScan(size); err != nil {
		return "", err
	}
	var out strings.Builder
	if exec != nil {
		if err := exec.checkProjectedStringBytesWithCallRoots(projectedBuilderCap(&out, size), receiver, args, nil, NewNil()); err != nil {
			return "", err
		}
	}
	out.Grow(size)
	writeRegexpQuoted(&out, text)
	return out.String(), nil
}

func regexpUnionPattern(exec *Execution, receiver Value, args []Value) (string, error) {
	work := regexWork{exec: exec}
	if err := work.charge(len(args)); err != nil {
		return "", err
	}
	// Validate every type before reporting the size error, as the old
	// escape-then-compile path did, but without allocating escaped copies.
	for _, arg := range args {
		if arg.Kind() != KindString {
			return "", fmt.Errorf("Regexp.union expects string patterns")
		}
	}
	if len(args) == 0 {
		// RE2 rejects Ruby's never-matching lookahead (?!). This character
		// class also matches nothing, including the empty string.
		return `[^\s\S]`, nil
	}
	size := len(args) - 1
	if size > maxRegexPatternSize {
		return "", guardLimitErrorf("Regexp.union pattern exceeds limit %d bytes", maxRegexPatternSize)
	}
	for _, arg := range args {
		text := arg.String()
		if len(text) > maxRegexPatternSize-size {
			return "", guardLimitErrorf("Regexp.union pattern exceeds limit %d bytes", maxRegexPatternSize)
		}
		if err := work.charge(len(text)); err != nil {
			return "", err
		}
		quoted, ok := regexpQuotedSize(text, maxRegexPatternSize-size)
		if !ok {
			return "", guardLimitErrorf("Regexp.union pattern exceeds limit %d bytes", maxRegexPatternSize)
		}
		size += quoted
	}
	if len(args) == 1 && size == len(args[0].String()) {
		return args[0].String(), nil
	}
	if err := work.charge(size); err != nil {
		return "", err
	}
	var out strings.Builder
	if exec != nil {
		if err := exec.checkProjectedStringBytesWithCallRoots(projectedBuilderCap(&out, size), receiver, args, nil, NewNil()); err != nil {
			return "", err
		}
	}
	out.Grow(size)
	for i, arg := range args {
		if i > 0 {
			out.WriteByte('|')
		}
		writeRegexpQuoted(&out, arg.String())
	}
	return out.String(), nil
}

// regexpQuotedSize projects QuoteMeta's output without allocating it. The
// subtraction and pre-increment checks reject overflow as well as the cap.
func regexpQuotedSize(text string, limit int) (int, bool) {
	if len(text) > limit {
		return 0, false
	}
	size := len(text)
	for i := range len(text) {
		if regexpMetaByte(text[i]) {
			if size == limit {
				return 0, false
			}
			size++
		}
	}
	return size, true
}

func writeRegexpQuoted(out *strings.Builder, text string) {
	start := 0
	for i := range len(text) {
		if regexpMetaByte(text[i]) {
			out.WriteString(text[start:i])
			out.WriteByte('\\')
			start = i
		}
	}
	out.WriteString(text[start:])
}

func regexpMetaByte(b byte) bool {
	switch b {
	case '\\', '.', '+', '*', '?', '(', ')', '|', '[', ']', '{', '}', '^', '$':
		return true
	default:
		return false
	}
}
