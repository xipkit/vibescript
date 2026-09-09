package runtime

import (
	"encoding/binary"
	"encoding/hex"
	"errors"
	"fmt"
	"math"
	"math/big"
	"math/rand"
	"regexp"
	"strconv"
	"strings"
	"time"
	"unicode/utf8"

	"github.com/mgomes/vibescript/vibes/value"
)

const (
	randomIDAlphabet       = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789"
	randomIDUnbiasedCutoff = byte((256 / len(randomIDAlphabet)) * len(randomIDAlphabet))
	maxRandomIDStallReads  = 8
)

func builtinAssert(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
	if len(args) == 0 {
		return NewNil(), fmt.Errorf("assert requires a condition argument")
	}
	cond := args[0]
	if cond.Truthy() {
		return NewNil(), nil
	}
	message := "assertion failed"
	if len(args) > 1 {
		message = args[1].String()
	} else if msg, ok := kwargs["message"]; ok {
		message = msg.String()
	}
	return NewNil(), newAssertionFailureError(message)
}

func builtinPuts(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
	if len(kwargs) > 0 {
		return NewNil(), fmt.Errorf("puts does not accept keyword arguments")
	}
	if !block.IsNil() {
		return NewNil(), fmt.Errorf("puts does not accept blocks")
	}
	writer := exec.engine.config.OutputWriter
	if writer == nil {
		return NewNil(), fmt.Errorf("puts output writer is not configured")
	}
	if len(args) == 0 {
		_, err := fmt.Fprintln(writer)
		return NewNil(), err
	}
	for _, arg := range args {
		rendered, err := renderOutputValue(exec, "puts", arg, false)
		if err != nil {
			return NewNil(), err
		}
		if _, err := fmt.Fprintln(writer, rendered); err != nil {
			return NewNil(), err
		}
	}
	return NewNil(), nil
}

func builtinPrint(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
	if len(kwargs) > 0 {
		return NewNil(), fmt.Errorf("print does not accept keyword arguments")
	}
	if !block.IsNil() {
		return NewNil(), fmt.Errorf("print does not accept blocks")
	}
	writer := exec.engine.config.OutputWriter
	if writer == nil {
		return NewNil(), fmt.Errorf("print output writer is not configured")
	}
	for _, arg := range args {
		rendered, err := renderOutputValue(exec, "print", arg, false)
		if err != nil {
			return NewNil(), err
		}
		if _, err := fmt.Fprint(writer, rendered); err != nil {
			return NewNil(), err
		}
	}
	return NewNil(), nil
}

func builtinWarn(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
	if len(kwargs) > 0 {
		return NewNil(), fmt.Errorf("warn does not accept keyword arguments")
	}
	if !block.IsNil() {
		return NewNil(), fmt.Errorf("warn does not accept blocks")
	}
	writer := exec.engine.config.ErrorWriter
	if writer == nil {
		return NewNil(), fmt.Errorf("warn error writer is not configured")
	}
	for _, arg := range args {
		rendered, err := renderOutputValue(exec, "warn", arg, false)
		if err != nil {
			return NewNil(), err
		}
		if _, err := fmt.Fprintln(writer, rendered); err != nil {
			return NewNil(), err
		}
	}
	return NewNil(), nil
}

func builtinP(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
	if len(kwargs) > 0 {
		return NewNil(), fmt.Errorf("p does not accept keyword arguments")
	}
	if !block.IsNil() {
		return NewNil(), fmt.Errorf("p does not accept blocks")
	}
	writer := exec.engine.config.OutputWriter
	if writer == nil {
		return NewNil(), fmt.Errorf("p output writer is not configured")
	}
	for _, arg := range args {
		rendered, err := renderOutputValue(exec, "p", arg, true)
		if err != nil {
			return NewNil(), err
		}
		if _, err := fmt.Fprintln(writer, rendered); err != nil {
			return NewNil(), err
		}
	}
	if len(args) == 0 {
		return NewNil(), nil
	}
	if len(args) == 1 {
		return args[0], nil
	}
	return NewArray(args), nil
}

func renderOutputValue(exec *Execution, method string, val Value, inspect bool) (string, error) {
	var (
		payload int
		err     error
	)
	if !inspect {
		// puts and print render the string form, so a class's to_s governs
		// them as it does interpolation. inspect keeps its own rendering.
		if val, _, err = exec.instanceStringValue(val, Position{}); err != nil {
			return "", err
		}
	}
	if val.Kind() == KindRegex && !inspect {
		// The bounded walk reaches len(v.String()) for a regex, escaping and
		// allocating the whole literal to size it before any charge, and the
		// render below builds it again. StringLen walks the source instead, so
		// the walk is charged before it runs and the rendered length after.
		if err := exec.chargeStringScan(len(val.Regex().Source)); err != nil {
			return "", err
		}
		payload = val.Regex().StringLen()
	} else if inspect {
		payload, err = val.InspectByteLenBounded(exec.step)
	} else {
		payload, err = val.StringByteLenBounded(exec.step)
	}
	if err != nil {
		return "", err
	}
	// Charge for the bytes about to be rendered; see the inspect member for why
	// a scalar with a large payload needs this beyond the per-element step.
	if err := exec.chargeStringScan(payload); err != nil {
		return "", err
	}
	if payload > maxOutputHelperBytes {
		return "", guardLimitErrorf("%s output exceeds limit %d bytes", method, maxOutputHelperBytes)
	}
	if err := exec.checkProjectedValueRendering(val, payload); err != nil {
		return "", err
	}
	if inspect {
		return val.InspectBounded(maxOutputHelperBytes)
	}
	return val.StringBounded(maxOutputHelperBytes)
}

func builtinMoney(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
	if len(args) != 1 {
		return NewNil(), fmt.Errorf("money expects a single string literal")
	}
	lit := args[0]
	if lit.Kind() != KindString {
		return NewNil(), fmt.Errorf("money expects a string literal")
	}
	parsed, err := parseMoneyLiteral(lit.String())
	if err != nil {
		return NewNil(), err
	}
	return NewMoney(parsed), nil
}

func builtinMoneyCents(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
	if len(args) != 2 {
		return NewNil(), fmt.Errorf("money_cents expects cents and currency")
	}
	centsVal := args[0]
	currencyVal := args[1]

	if !isNumericValue(centsVal) {
		return NewNil(), fmt.Errorf("money_cents expects integer cents")
	}
	cents, err := valueToInt64(centsVal)
	if err != nil {
		return NewNil(), fmt.Errorf("money_cents expects integer cents: %w", err)
	}
	if currencyVal.Kind() != KindString {
		return NewNil(), fmt.Errorf("money_cents expects currency string")
	}

	money, err := newMoneyFromCents(cents, currencyVal.String())
	if err != nil {
		return NewNil(), err
	}
	return NewMoney(money), nil
}

func builtinNow(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
	if len(args) > 0 {
		return NewNil(), fmt.Errorf("now does not take arguments")
	}
	return NewString(time.Now().UTC().Format(time.RFC3339)), nil
}

func builtinRand(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
	if len(kwargs) > 0 {
		return NewNil(), fmt.Errorf("rand does not take keyword arguments")
	}
	if !block.IsNil() {
		return NewNil(), fmt.Errorf("rand does not accept blocks")
	}
	if len(args) > 1 {
		return NewNil(), fmt.Errorf("rand expects at most one argument")
	}
	if len(args) == 0 {
		f, err := exec.randomFloat64()
		if err != nil {
			return NewNil(), err
		}
		return NewFloat(f), nil
	}
	switch arg := args[0]; arg.Kind() {
	case KindNil:
		f, err := exec.randomFloat64()
		if err != nil {
			return NewNil(), err
		}
		return NewFloat(f), nil
	case KindInt:
		limit, compact := arg.CompactInt()
		if !compact {
			return NewNil(), fmt.Errorf("rand integer bound must fit in a 64-bit integer")
		}
		if limit <= 0 {
			return NewNil(), fmt.Errorf("rand integer bound must be positive")
		}
		n, err := exec.randomInt64n(uint64(limit))
		if err != nil {
			return NewNil(), err
		}
		return NewInt(int64(n)), nil
	case KindRange:
		return exec.randomRangeValue(arg.Range())
	default:
		return NewNil(), fmt.Errorf("rand expects an integer bound or integer range")
	}
}

func builtinSrand(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
	if len(kwargs) > 0 {
		return NewNil(), fmt.Errorf("srand does not take keyword arguments")
	}
	if !block.IsNil() {
		return NewNil(), fmt.Errorf("srand does not accept blocks")
	}
	if len(args) > 1 {
		return NewNil(), fmt.Errorf("srand expects at most one seed")
	}

	previous := NewNil()
	if exec.randSeeded {
		previous = NewInt(exec.randSeed)
	}

	var seed int64
	if len(args) == 0 || args[0].Kind() == KindNil {
		raw, err := exec.randomUint64()
		if err != nil {
			return NewNil(), err
		}
		seed = int64(raw)
	} else if args[0].Kind() == KindInt {
		var compact bool
		seed, compact = args[0].CompactInt()
		if !compact {
			return NewNil(), fmt.Errorf("srand seed must fit in a 64-bit integer")
		}
	} else {
		return NewNil(), fmt.Errorf("srand seed must be integer or nil")
	}
	exec.randSource = rand.New(rand.NewSource(seed))
	exec.randSeed = seed
	exec.randSeeded = true
	return previous, nil
}

func (exec *Execution) randomFloat64() (float64, error) {
	if exec.randSource != nil {
		return exec.randSource.Float64(), nil
	}
	raw, err := exec.randomUint64()
	if err != nil {
		return 0, err
	}
	return float64(raw>>11) / (1 << 53), nil
}

func (exec *Execution) randomRangeValue(rng Range) (Value, error) {
	if rng.Beginless || rng.Endless {
		return NewNil(), fmt.Errorf("rand range must be bounded")
	}
	low, high, ok := randomRangeInclusiveBounds(rng)
	if !ok {
		return NewNil(), fmt.Errorf("rand range is empty")
	}
	size := uint64(high) - uint64(low) + 1
	var offset uint64
	var err error
	if size == 0 {
		offset, err = exec.randomUint64ForRand()
	} else {
		offset, err = exec.randomInt64n(size)
	}
	if err != nil {
		return NewNil(), err
	}
	return NewInt(int64(uint64(low) + offset)), nil
}

func randomRangeInclusiveBounds(rng Range) (int64, int64, bool) {
	low, high := rng.Start, rng.End
	if low > high {
		low, high = high, low
		if rng.Exclusive {
			if low == math.MaxInt64 {
				return 0, 0, false
			}
			low++
		}
	} else if rng.Exclusive {
		if high == math.MinInt64 {
			return 0, 0, false
		}
		high--
	}
	if low > high {
		return 0, 0, false
	}
	return low, high, true
}

func (exec *Execution) randomInt64n(limit uint64) (uint64, error) {
	if limit == 0 {
		return 0, fmt.Errorf("random integer limit must be positive")
	}
	if exec.randSource != nil && limit <= uint64(math.MaxInt64) {
		return uint64(exec.randSource.Int63n(int64(limit))), nil
	}
	rejectAbove := ^uint64(0) - ((-limit) % limit)
	for {
		raw, err := exec.randomUint64ForRand()
		if err != nil {
			return 0, err
		}
		if raw <= rejectAbove {
			return raw % limit, nil
		}
	}
}

func (exec *Execution) randomUint64ForRand() (uint64, error) {
	if exec.randSource != nil {
		return exec.randSource.Uint64(), nil
	}
	return exec.randomUint64()
}

func (exec *Execution) randomUint64() (uint64, error) {
	raw, err := exec.engine.randomBytes(exec.Context(), 8)
	if err != nil {
		return 0, err
	}
	return binary.BigEndian.Uint64(raw), nil
}

func builtinFormat(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
	return formatStringBuiltin(exec, "format", receiver, args, kwargs, block)
}

// removedCallableMessage spells the teaching error for a name that used to
// construct a callable value (ADR-006 item 4). It matches the parser's fence
// for the -> literal so every spelling of the removal teaches the same
// replacement.
func removedCallableMessage(name string) string {
	return name + " was removed; executable code is not a value. Define a named function and call it, or attach a block to the call that runs it"
}

// builtinRemovedCallable backs the removed callable constructors (proc,
// lambda, Proc.new). The fence exists so the reference errors with the
// removal's teaching message rather than "undefined variable".
func builtinRemovedCallable(name string) BuiltinFunc {
	return func(*Execution, Value, []Value, map[string]Value, Value) (Value, error) {
		return NewNil(), errors.New(removedCallableMessage(name))
	}
}

func builtinLoop(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
	if len(args) > 0 {
		return NewNil(), fmt.Errorf("loop does not take arguments")
	}
	if len(kwargs) > 0 {
		return NewNil(), fmt.Errorf("loop does not take keyword arguments")
	}
	runner, err := newBlockCallRunner(exec, block, "loop", NewNil(), nil, kwargs)
	if err != nil {
		return NewNil(), err
	}
	runner.nextContinues = true

	exec.loopDepth++
	defer func() {
		exec.loopDepth--
	}()

	for {
		if err := exec.step(); err != nil {
			return NewNil(), err
		}
		val, err := runner.call(nil)
		if err != nil {
			if errors.Is(err, errLoopBreak) {
				if breakVal, ok := loopBreakValue(err); ok {
					return breakVal, nil
				}
				return NewNil(), nil
			}
			if errors.Is(err, errLoopNext) {
				continue
			}
			return NewNil(), err
		}
		if err := exec.checkMemoryValue(val); err != nil {
			return NewNil(), err
		}
	}
}

func builtinSprintf(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
	return formatStringBuiltin(exec, "sprintf", receiver, args, kwargs, block)
}

func formatStringBuiltin(exec *Execution, name string, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
	if len(kwargs) > 0 {
		return NewNil(), fmt.Errorf("%s does not take keyword arguments", name)
	}
	if !block.IsNil() {
		return NewNil(), fmt.Errorf("%s does not accept blocks", name)
	}
	if len(args) == 0 {
		return NewNil(), fmt.Errorf("%s expects a format string", name)
	}
	if args[0].Kind() != KindString {
		return NewNil(), fmt.Errorf("%s expects a string format", name)
	}
	values, scratch, err := exec.formatStringConversionValues(args[1:], receiver, args, kwargs, block)
	if err != nil {
		return NewNil(), err
	}
	// Held through the render: the conversions are live until the output is
	// built, and the checks in there walk the arguments, which hold the
	// instances rather than what their to_s returned.
	defer scratch.release()
	return exec.formatStringValues(args[0].String(), values, receiver, args, kwargs, block)
}

// formatStringConversionValues substitutes each argument's to_s form before
// the pattern is projected or rendered.
//
// %s is defined as the to_s form, but format was the last direct string
// conversion that did not consult a class's to_s: interpolation and puts were
// connected in #1055 and this was left out, so format("%s", p) alone still
// rendered <P instance>.
//
// Substituting up front rather than per-verb keeps the projection pass and the
// render pass looking at the same values, which the memory quota depends on --
// they must agree or the reservation the quota approved is not the one built.
// It is safe for the numeric verbs because an instance is not a valid operand
// for any of them either way.
//
// Every argument is converted, including ones the pattern never uses, so a
// script-defined to_s runs once per operand before the pattern has been looked
// at. What it returns lands in a Go-local slice the estimator cannot reach:
// each result passed its own check while the ones before it were invisible, and
// a pattern that fails validation -- format("", *instances) -- returned the
// error before any output check ran at all. Many instances whose to_s returns
// an individually quota-sized string therefore accumulated unseen (#4).
//
// The conversions are registered as they accumulate, so the quota sees them
// during the loop rather than only after it, and they stay registered until
// the render is done because the strings are live that whole time. The caller
// releases them.
func (exec *Execution) formatStringConversionValues(values []Value, receiver Value, args []Value, kwargs map[string]Value, block Value) ([]Value, *retainedOutputScratch, error) {
	var acc *arrayBuildAccumulator
	var converted []Value
	var scratch *retainedOutputScratch
	for i, val := range values {
		rendered, substituted, err := exec.instanceStringValue(val, Position{})
		if err != nil {
			scratch.release()
			return nil, nil, err
		}
		if !substituted {
			continue
		}
		if acc == nil {
			// Both built at the first substitution, not at entry. Seeding the
			// accumulator walks the whole reachable graph and the scratch escapes
			// to the caller, and most calls convert nothing: format("done"), or
			// any call whose operands are all primitives, would pay for a walk
			// and an allocation it has no use for.
			acc = newArrayBuildAccumulator(exec, receiver, args, kwargs, block)
			scratch = newRetainedOutputScratch(exec)
		}
		// Priced against the seeded baseline rather than by length, so a to_s
		// that hands back one of its own fields costs nothing and several
		// operands sharing one string are charged once. The estimator does that
		// deduplication in constant amortized time per conversion.
		//
		// Reserved rather than registered as a live value. Registering is exact
		// in both directions, but the estimator then walks every conversion held
		// so far on every check a later to_s performs, and a to_s runs per
		// operand: format with 800 substituted operands went from 0.6ms to
		// 29ms, quadratic in the operand count, where a reservation is a scalar
		// the walk reads once (#4).
		// The backing is charged alongside the payload. A call whose conversions
		// are all empty or all aliases has no marginal payload at all, yet it
		// still allocates one slot per operand, and that slice is a Go local no
		// walk can reach: a pattern that rejects returned before anything had
		// weighed it.
		if err := acc.add(rendered, len(values)); err != nil {
			scratch.release()
			return nil, nil, err
		}
		// The accumulator's own bound already weighs the conversions against a
		// baseline that includes the arguments, so no root walk is needed per
		// conversion; the reservation exists so that checks inside whatever
		// to_s runs next see the pile too, and a reservation is a scalar the
		// walk reads once.
		scratch.reserve(acc.accumulatedBytes(len(values)))
		if converted == nil {
			converted = make([]Value, len(values))
			copy(converted, values)
		}
		converted[i] = rendered
	}
	if converted == nil {
		scratch.release()
		return values, nil, nil
	}
	// Walked once, at the end, against the call roots. The arguments are a
	// builtin's Go locals, so nothing else weighs the conversions together with
	// the instances they came from, and a pattern that rejects returns before
	// the render check that would otherwise have done it.
	if err := exec.checkReservedLoopScratch(receiver, args, kwargs, block); err != nil {
		scratch.release()
		return nil, nil, err
	}
	return converted, scratch, nil
}

func formatStringValues(pattern string, values []Value) (Value, error) {
	return formatStringValuesChecked(nil, pattern, values, NewNil(), nil, nil, NewNil())
}

func (exec *Execution) formatStringValues(pattern string, values []Value, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
	return formatStringValuesChecked(exec, pattern, values, receiver, args, kwargs, block)
}

func formatStringValuesChecked(exec *Execution, pattern string, values []Value, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
	// Charged here rather than at the format builtin because this is the shared
	// path: the String % operator reaches it directly from the evaluator, so a
	// charge on the builtin alone left `pattern % []` unmetered. The pattern is
	// scanned for verbs and its literal text copied on every call, so it costs
	// its own length even with no arguments.
	if exec != nil {
		if err := exec.chargeStringScan(len(pattern)); err != nil {
			return NewNil(), err
		}
	}
	prepared, err := prepareFormatString(exec, pattern, values)
	if err != nil {
		return NewNil(), err
	}
	if exec != nil {
		// A width writes bytes the pattern and arguments do not account for:
		// format("%1000000s", "") produces a megabyte from a handful of input
		// bytes, and the per-call output cap bounds one call, not a loop of
		// them. Charge what will be written, as the padding members and
		// String#* do.
		if err := exec.chargeStringScan(prepared.projectedBytes); err != nil {
			return NewNil(), err
		}
		if err := exec.checkProjectedStringBytesAndScratchWithCallRoots(prepared.projectedBytes, prepared.scratchBytes, receiver, args, kwargs, block); err != nil {
			return NewNil(), err
		}
	}
	formatArgs, err := prepared.formatArgs()
	if err != nil {
		return NewNil(), err
	}
	return NewString(fmt.Sprintf(prepared.pattern, formatArgs...)), nil
}

type preparedFormatString struct {
	pattern        string
	args           []preparedFormatArgument
	projectedBytes int
	scratchBytes   int
}

func (p preparedFormatString) formatArgs() ([]any, error) {
	args := make([]any, 0, len(p.args))
	for _, arg := range p.args {
		formatted, err := arg.format()
		if err != nil {
			return nil, err
		}
		args = append(args, formatted)
	}
	return args, nil
}

func prepareFormatString(exec *Execution, pattern string, values []Value) (preparedFormatString, error) {
	prepared := preparedFormatString{
		args: make([]preparedFormatArgument, 0, len(values)),
	}
	projection := formatProjection{exec: exec}
	var normalized strings.Builder
	normalized.Grow(min(len(pattern), maxFormatOutputBytes))
	total := 0
	nextArg := 0
	usedCursor := 0
	for i := 0; i < len(pattern); {
		if pattern[i] != '%' {
			// Copy short literal runs without starting a separate scan.
			end := i + min(len(pattern)-i, 8)
			for i < end && pattern[i] != '%' {
				var err error
				total, err = addProjectedFormatBytes(total, 1)
				if err != nil {
					return preparedFormatString{}, err
				}
				normalized.WriteByte(pattern[i])
				i++
			}
			if i == len(pattern) {
				break
			}
			if pattern[i] != '%' {
				// Keep the byte-wise growth schedule when a normalized pattern
				// exceeds its initial capacity, and stop at the output limit.
				end = i + min(len(pattern)-i, maxFormatOutputBytes-total+1, max(1, normalized.Cap()-normalized.Len()))
				if offset := strings.IndexByte(pattern[i:end], '%'); offset >= 0 {
					end = i + offset
				}
				var err error
				total, err = addProjectedFormatBytes(total, end-i)
				if err != nil {
					return preparedFormatString{}, err
				}
				normalized.WriteString(pattern[i:end])
				i = end
				continue
			}
		}
		directiveStart := i
		i++
		if i >= len(pattern) {
			var err error
			total, err = addProjectedFormatBytes(total, len("%!(NOVERB)"))
			if err != nil {
				return preparedFormatString{}, err
			}
			normalized.WriteString(pattern[directiveStart:])
			break
		}
		if pattern[i] == '%' {
			var err error
			total, err = addProjectedFormatBytes(total, 1)
			if err != nil {
				return preparedFormatString{}, err
			}
			normalized.WriteString("%%")
			i++
			continue
		}

		var explicitArg int
		var hasExplicitArg bool
		bodyAfterLeadingIndex := i
		if idx, ok, next := parseFormatLeadingArgIndex(pattern, i); ok {
			explicitArg = idx
			hasExplicitArg = true
			bodyAfterLeadingIndex = next
			i = next
		}
		flags := formatFlags{}
		for i < len(pattern) && strings.ContainsRune("#+-0 ", rune(pattern[i])) {
			flags.record(pattern[i])
			i++
		}
		width, hasWidth, next, err := parseFormatCount(pattern, i, "width")
		if err != nil {
			return preparedFormatString{}, err
		}
		i = next

		precision := 0
		hasPrecision := false
		if i < len(pattern) && pattern[i] == '.' {
			i++
			precision, hasPrecision, next, err = parseFormatCount(pattern, i, "precision")
			if err != nil {
				return preparedFormatString{}, err
			}
			i = next
		}
		bodyBeforeTrailingIndex := i
		bodyAfterTrailingIndex := i
		if idx, ok, next := parseFormatArgIndex(pattern, i); ok {
			explicitArg = idx
			hasExplicitArg = true
			bodyAfterTrailingIndex = next
			i = next
		}
		if i >= len(pattern) {
			var err error
			total, err = addProjectedFormatBytes(total, len(pattern))
			if err != nil {
				return preparedFormatString{}, err
			}
			normalized.WriteString(pattern[directiveStart:])
			break
		}
		verb := pattern[i]
		verbIndex := i
		i++

		argIndex := nextArg
		if hasExplicitArg {
			argIndex = explicitArg
			nextArg = explicitArg + 1
		} else {
			nextArg++
		}
		if nextArg > usedCursor {
			usedCursor = nextArg
		}
		if argIndex < 0 || argIndex >= len(values) {
			return preparedFormatString{}, fmt.Errorf("format references missing operand %d", argIndex+1)
		}
		arg, err := prepareFormatArgument(projection, values[argIndex], verb, hasPrecision, precision)
		if err != nil {
			return preparedFormatString{}, err
		}
		field, err := projectedFormatFieldBytes(projection, values[argIndex], verb, hasPrecision, precision, flags)
		if err != nil {
			return preparedFormatString{}, err
		}
		if hasWidth {
			field, err = projectedFormatFieldBytesWithWidth(projection, values[argIndex], verb, hasPrecision, precision, flags, width, field)
			if err != nil {
				return preparedFormatString{}, err
			}
		}
		nextTotal, addErr := addProjectedFormatBytes(total, field)
		if addErr != nil {
			return preparedFormatString{}, addErr
		}
		total = nextTotal
		normalized.WriteByte('%')
		normalized.WriteString(pattern[bodyAfterLeadingIndex:bodyBeforeTrailingIndex])
		if bodyAfterTrailingIndex > bodyBeforeTrailingIndex {
			normalized.WriteString(pattern[bodyAfterTrailingIndex:verbIndex])
		}
		normalized.WriteByte(verb)
		prepared.args = append(prepared.args, arg)
		prepared.scratchBytes = saturatingAdd(prepared.scratchBytes, arg.scratchBytes())
	}
	if usedCursor < len(values) {
		return preparedFormatString{}, fmt.Errorf("format has %d unused operand(s)", len(values)-usedCursor)
	}
	normalizedScratchBytes := normalized.Cap()
	prepared.pattern = normalized.String()
	prepared.projectedBytes = total
	prepared.scratchBytes = saturatingAdd(prepared.scratchBytes, normalizedScratchBytes)
	return prepared, nil
}

func addProjectedFormatBytes(total, bytes int) (int, error) {
	total = saturatingAdd(total, bytes)
	if total > maxFormatOutputBytes {
		return 0, guardLimitErrorf("format output exceeds limit %d bytes", maxFormatOutputBytes)
	}
	return total, nil
}

func parseFormatArgIndex(pattern string, i int) (int, bool, int) {
	if i >= len(pattern) || pattern[i] != '[' {
		return 0, false, i
	}
	j := i + 1
	start := j
	for j < len(pattern) && pattern[j] >= '0' && pattern[j] <= '9' {
		j++
	}
	if start == j || j >= len(pattern) || pattern[j] != ']' {
		return 0, false, i
	}
	n, err := strconv.Atoi(pattern[start:j])
	if err != nil || n <= 0 {
		return 0, false, i
	}
	return n - 1, true, j + 1
}

func parseFormatLeadingArgIndex(pattern string, i int) (int, bool, int) {
	if idx, ok, next := parseFormatArgIndex(pattern, i); ok {
		return idx, true, next
	}
	start := i
	for i < len(pattern) && pattern[i] >= '0' && pattern[i] <= '9' {
		i++
	}
	if start == i || i >= len(pattern) || pattern[i] != '$' {
		return 0, false, start
	}
	n, err := strconv.Atoi(pattern[start:i])
	if err != nil || n <= 0 {
		return 0, false, start
	}
	return n - 1, true, i + 1
}

func parseFormatCount(pattern string, i int, label string) (int, bool, int, error) {
	if idx, ok, next := parseFormatArgIndex(pattern, i); ok {
		i = next
		_ = idx
	}
	if i < len(pattern) && pattern[i] == '*' {
		return 0, false, i, fmt.Errorf("format dynamic %s is not supported", label)
	}
	start := i
	for i < len(pattern) && pattern[i] >= '0' && pattern[i] <= '9' {
		i++
	}
	if start == i {
		return 0, false, i, nil
	}
	n, err := strconv.Atoi(pattern[start:i])
	if err != nil || n > maxFormatOutputBytes {
		return 0, false, i, guardLimitErrorf("format %s exceeds limit %d bytes", label, maxFormatOutputBytes)
	}
	return n, true, i, nil
}

type formatFlags struct {
	alternate bool
	plus      bool
	space     bool
}

func (f *formatFlags) record(flag byte) {
	switch flag {
	case '#':
		f.alternate = true
	case '+':
		f.plus = true
	case ' ':
		f.space = true
	}
}

type formatProjection struct {
	exec *Execution
}

func (p formatProjection) stringBytes(val Value) (int, error) {
	switch val.Kind() {
	case KindString, KindSymbol:
		// Scalars skip the bounded walk, so they charge here instead.
		if p.exec != nil {
			if err := p.exec.chargeStringScan(len(val.String())); err != nil {
				return 0, err
			}
		}
		return len(val.String()), nil
	default:
		if p.exec != nil {
			// An aggregate is walked a step per node, which bounds its shape but
			// not its size: a 512 KiB string nested in a one-element array is one
			// node. The rendered payload is what gets materialized and copied, so
			// charge that, as the other rendering sites do.
			n, err := val.StringByteLenBounded(p.exec.step)
			if err != nil {
				return 0, err
			}
			if err := p.exec.chargeStringScan(n); err != nil {
				return 0, err
			}
			return n, nil
		}
		return val.StringByteLen(), nil
	}
}

func (p formatProjection) stringRunes(val Value) (int, error) {
	switch val.Kind() {
	case KindString, KindSymbol:
		// Counting runes walks the whole value however small the field that
		// will hold them, so a precision or width cannot bound this: charge the
		// traversal itself. Shared by every caller that needs a rune count.
		if p.exec != nil {
			if err := p.exec.chargeStringScan(len(val.String())); err != nil {
				return 0, err
			}
		}
		return utf8.RuneCountInString(val.String()), nil
	default:
		if p.exec != nil {
			// An aggregate walks a step per node, which no field width bounds:
			// counting the runes of a large string nested in a one-element array
			// costs one step. Charge the walk, as the scalar branch above does.
			//
			// The walk visits bytes but reports runes, and the charge is
			// byte-based, so the rune count is scaled to its widest possible
			// encoding. That over-charges ASCII, but the alternative is a second
			// full traversal to learn the byte length, and under-charging a scan
			// is the failure this metering exists to prevent.
			n, err := val.StringRuneLenBounded(p.exec.step)
			if err != nil {
				return 0, err
			}
			if err := p.exec.chargeStringScan(saturatingMul(n, utf8.UTFMax)); err != nil {
				return 0, err
			}
			return n, nil
		}
		return val.StringRuneLen(), nil
	}
}

func (p formatProjection) stringBytesUpTo(val Value, limit int) (int, error) {
	if limit <= 0 {
		return 0, nil
	}
	switch val.Kind() {
	case KindString, KindSymbol:
		if p.exec != nil {
			if err := p.exec.chargeStringScan(min(len(val.String()), limit)); err != nil {
				return 0, err
			}
		}
		return min(len(val.String()), limit), nil
	default:
		if p.exec != nil {
			n, truncated, err := val.StringByteLenBoundedUpTo(limit, p.exec.step)
			if err != nil {
				return 0, err
			}
			if truncated {
				n = limit
			}
			if err := p.exec.chargeStringScan(n); err != nil {
				return 0, err
			}
			return n, nil
		}
		return min(val.StringByteLen(), limit), nil
	}
}

func (p formatProjection) stringPrecisionBytes(val Value, precision int) (int, error) {
	if precision <= 0 {
		return 0, nil
	}
	switch val.Kind() {
	case KindString, KindSymbol:
		// formatStringPrecisionBytes walks the input to find the precision
		// boundary, so it charges for what it traverses; the default branch
		// below inherits its charge from stringBytesUpTo.
		if p.exec != nil {
			if err := p.exec.chargeStringScan(min(len(val.String()), saturatingMul(utf8.UTFMax, precision))); err != nil {
				return 0, err
			}
		}
		return formatStringPrecisionBytes(val.String(), precision), nil
	default:
		return p.stringBytesUpTo(val, saturatingMul(utf8.UTFMax, precision))
	}
}

func (p formatProjection) stringPrecisionRunes(val Value, precision int) (int, error) {
	if precision <= 0 {
		return 0, nil
	}
	runes, err := p.stringRunes(val)
	if err != nil {
		return 0, err
	}
	return min(runes, precision), nil
}

func formatStringPrecisionBytes(s string, precision int) int {
	if precision <= 0 {
		return 0
	}
	runes := 0
	for i := range s {
		if runes == precision {
			return i
		}
		runes++
	}
	return len(s)
}

func projectedFormatFieldBytes(projection formatProjection, val Value, verb byte, hasPrecision bool, precision int, flags formatFlags) (int, error) {
	if hasPrecision {
		switch verb {
		case 's':
			return projection.stringPrecisionBytes(val, precision)
		case 'q':
			selectedBytes, err := projection.stringPrecisionBytes(val, precision)
			if err != nil {
				return 0, err
			}
			return projectedQuotedStringBytes(selectedBytes), nil
		}
	}
	base, err := projectedFormatArgumentBytes(projection, val, verb, hasPrecision, precision, flags)
	if err != nil {
		return 0, err
	}
	if hasPrecision {
		switch verb {
		case 'f', 'F', 'e', 'E', 'g', 'G':
			base = max(base, saturatingAdd(precision, 16))
		}
	}
	return base, nil
}

func projectedFormatFieldBytesWithWidth(projection formatProjection, val Value, verb byte, hasPrecision bool, precision int, flags formatFlags, width, fieldBytes int) (int, error) {
	runes, ok, err := projectedFormatFieldRunes(projection, val, verb, hasPrecision, precision, flags)
	if err != nil {
		return 0, err
	}
	if !ok {
		if width > fieldBytes {
			return width, nil
		}
		return fieldBytes, nil
	}
	if width <= runes {
		return fieldBytes, nil
	}
	return saturatingAdd(fieldBytes, width-runes), nil
}

func projectedFormatFieldRunes(projection formatProjection, val Value, verb byte, hasPrecision bool, precision int, flags formatFlags) (int, bool, error) {
	switch verb {
	case 's':
		runes, err := projection.stringFormatRunes(val, hasPrecision, precision)
		return runes, true, err
	case 'v':
		if flags.alternate || !formatArgumentUsesStringRendering(val) {
			return 0, false, nil
		}
		runes, err := projection.stringFormatRunes(val, hasPrecision, precision)
		return runes, true, err
	default:
		return 0, false, nil
	}
}

func (p formatProjection) stringFormatRunes(val Value, hasPrecision bool, precision int) (int, error) {
	if hasPrecision {
		return p.stringPrecisionRunes(val, precision)
	}
	return p.stringRunes(val)
}

func projectedFormatArgumentBytes(projection formatProjection, val Value, verb byte, hasPrecision bool, precision int, flags formatFlags) (int, error) {
	switch verb {
	case 's':
		return projection.stringBytes(val)
	case 'q':
		n, err := projection.stringBytes(val)
		if err != nil {
			return 0, err
		}
		return projectedQuotedStringBytes(n), nil
	case 'x', 'X':
		if val.Kind() == KindString || val.Kind() == KindSymbol {
			// This branch sizes the field itself rather than going through
			// stringBytes, so it charges for the input it is about to hex-encode
			// instead of inheriting that charge.
			if projection.exec != nil {
				if err := projection.exec.chargeStringScan(len(val.String())); err != nil {
					return 0, err
				}
			}
			bytesPerInput := 2
			if flags.space {
				bytesPerInput++
			}
			field := saturatingMul(bytesPerInput, len(val.String()))
			if flags.alternate {
				field = saturatingAdd(field, 2)
				if flags.space {
					field = saturatingAdd(field, saturatingMul(2, len(val.String())))
				}
			}
			return field, nil
		}
		if val.Kind() == KindInt {
			return projectedIntegerFormatBytes(projection, val, verb, hasPrecision, precision, flags)
		}
		if val.Kind() == KindFloat && hasPrecision {
			return saturatingAdd(precision, 32), nil
		}
		return 64, nil
	case 'd', 'b', 'o', 'O', 'U':
		return projectedIntegerFormatBytes(projection, val, verb, hasPrecision, precision, flags)
	case 'c':
		return 64, nil
	case 'f', 'F', 'e', 'E', 'g', 'G':
		if verb == 'f' || verb == 'F' {
			return projectedFixedFloatFormatBytes(val, hasPrecision, precision, flags), nil
		}
		if hasPrecision {
			return saturatingAdd(precision, 16), nil
		}
		return 64, nil
	case 't':
		return 5, nil
	case 'v':
		if flags.alternate {
			n, err := projection.stringBytes(val)
			if err != nil {
				return 0, err
			}
			return projectedQuotedStringBytes(n), nil
		}
		if formatArgumentUsesStringRendering(val) {
			if hasPrecision {
				return projection.stringPrecisionBytes(val, precision)
			}
			return projection.stringBytes(val)
		}
		n, err := projection.stringBytes(val)
		if err != nil {
			return 0, err
		}
		return saturatingAdd(n, 32), nil
	default:
		n, err := projection.stringBytes(val)
		if err != nil {
			return 0, err
		}
		return saturatingAdd(n, 32), nil
	}
}

func projectedIntegerFormatBytes(projection formatProjection, val Value, verb byte, hasPrecision bool, precision int, flags formatFlags) (int, error) {
	if bi, ok := value.BigIntPayload(val); ok {
		return projectedBigIntFormatBytes(projection, bi, verb, hasPrecision, precision, flags)
	}
	n, err := valueToInt64(val)
	if err != nil {
		return 0, err
	}
	if verb == 'U' {
		digits := max(4, unsignedIntegerDigitBytes(uint64(n), 16))
		if hasPrecision {
			digits = max(digits, precision)
		}
		field := saturatingAdd(2, digits)
		if flags.alternate {
			field = saturatingAdd(field, 16)
		}
		return field, nil
	}

	base := 10
	prefix := 0
	switch verb {
	case 'b':
		base = 2
		if flags.alternate {
			prefix = 2
		}
	case 'o':
		base = 8
		if flags.alternate {
			prefix = 1
		}
	case 'O':
		base = 8
		prefix = 2
		if flags.alternate {
			prefix = 3
		}
	case 'x', 'X':
		base = 16
		if flags.alternate {
			prefix = 2
		}
	}
	digits := signedIntegerDigitBytes(n, base)
	if hasPrecision {
		digits = max(digits, precision)
	}
	return saturatingAdd(saturatingAdd(projectedNumericSignBytes(n < 0, flags), prefix), digits), nil
}

// projectedBigIntFormatBytes projects the formatted size of a big integer for
// the integer verbs, charging digit-scaled steps before the (superlinear) base
// conversion runs, mirroring the rendering projections. %c and %U need a code
// point, which a big integer can never be, so they keep the int64 conversion
// error.
func projectedBigIntFormatBytes(projection formatProjection, bi *big.Int, verb byte, hasPrecision bool, precision int, flags formatFlags) (int, error) {
	var digits, prefix int
	switch verb {
	case 'U', 'c':
		_, err := valueToInt64(value.AdoptBigInt(new(big.Int).Set(bi)))
		return 0, err
	case 'b':
		digits = bi.BitLen()
		if flags.alternate {
			prefix = 2
		}
	case 'o':
		digits = bi.BitLen()/3 + 1
		if flags.alternate {
			prefix = 1
		}
	case 'O':
		digits = bi.BitLen()/3 + 1
		prefix = 2
		if flags.alternate {
			prefix = 3
		}
	case 'x', 'X':
		digits = bi.BitLen()/4 + 1
		if flags.alternate {
			prefix = 2
		}
	default:
		digits = bigIntDecimalDigitsUpperBound(bi)
	}
	if hasPrecision {
		digits = max(digits, precision)
	}
	if projection.exec != nil {
		if err := projection.exec.stepN(1 + digits/bigIntStepWordsPerStep); err != nil {
			return 0, err
		}
	}
	return saturatingAdd(saturatingAdd(projectedNumericSignBytes(bi.Sign() < 0, flags), prefix), digits), nil
}

func projectedFixedFloatFormatBytes(val Value, hasPrecision bool, precision int, flags formatFlags) int {
	sign := projectedNumericSignBytes(formatFloatIsNegative(val), flags)
	if val.Kind() == KindFloat && (math.IsInf(val.Float(), 0) || math.IsNaN(val.Float())) {
		return saturatingAdd(sign, 3)
	}
	integerDigits := projectedFixedFloatIntegerDigits(val)
	fractionDigits := 6
	if hasPrecision {
		fractionDigits = precision
	}
	decimal := 0
	if fractionDigits > 0 || flags.alternate {
		decimal = 1
	}
	return saturatingAdd(saturatingAdd(sign, integerDigits), saturatingAdd(decimal, fractionDigits))
}

func projectedFixedFloatIntegerDigits(val Value) int {
	switch val.Kind() {
	case KindInt:
		if bi, ok := value.BigIntPayload(val); ok {
			return bigIntDecimalDigitsUpperBound(bi)
		}
		return signedIntegerDigitBytes(val.Int(), 10)
	case KindFloat:
		f := math.Abs(val.Float())
		if f < 1 {
			return 1
		}
		formatted := strconv.FormatFloat(f, 'e', -1, 64)
		exponentStart := strings.LastIndexByte(formatted, 'e')
		if exponentStart < 0 {
			return 309
		}
		exponent, err := strconv.Atoi(formatted[exponentStart+1:])
		if err != nil || exponent < 0 {
			return 309
		}
		return exponent + 1
	default:
		return 1
	}
}

func signedIntegerDigitBytes(n int64, base int) int {
	return unsignedIntegerDigitBytes(absInt64AsUint64(n), base)
}

func unsignedIntegerDigitBytes(n uint64, base int) int {
	if n == 0 {
		return 1
	}
	digits := 0
	for n > 0 {
		digits++
		n /= uint64(base)
	}
	return digits
}

func absInt64AsUint64(n int64) uint64 {
	if n >= 0 {
		return uint64(n)
	}
	return uint64(-(n + 1)) + 1
}

func projectedNumericSignBytes(negative bool, flags formatFlags) int {
	if negative || flags.plus || flags.space {
		return 1
	}
	return 0
}

func formatFloatIsNegative(val Value) bool {
	switch val.Kind() {
	case KindInt:
		if bi, ok := value.BigIntPayload(val); ok {
			return bi.Sign() < 0
		}
		return val.Int() < 0
	case KindFloat:
		return math.Signbit(val.Float())
	default:
		return false
	}
}

func projectedQuotedStringBytes(inputBytes int) int {
	return saturatingAdd(saturatingMul(4, inputBytes), 2)
}

type preparedFormatArgument struct {
	value        Value
	verb         byte
	stringRender formatArgumentStringRender
}

type formatArgumentStringRender struct {
	enabled        bool
	limit          int
	allowTruncated bool
}

func prepareFormatArgument(projection formatProjection, val Value, verb byte, hasPrecision bool, precision int) (preparedFormatArgument, error) {
	arg := preparedFormatArgument{value: val, verb: verb}
	switch verb {
	case 's', 'q':
		render, err := prepareFormatStringRender(projection, val, hasPrecision, precision)
		if err != nil {
			return preparedFormatArgument{}, err
		}
		arg.stringRender = render
	case 'x', 'X':
		switch val.Kind() {
		case KindString, KindSymbol, KindInt, KindFloat:
		default:
			return preparedFormatArgument{}, fmt.Errorf("format %%%c expects string or numeric operand", verb)
		}
	case 'd', 'b', 'o', 'O', 'U', 'c':
		// %d/%b/%o/%O format big integers natively; only the code-point verbs
		// genuinely need an int64.
		if val.IsBigInt() && verb != 'U' && verb != 'c' {
			break
		}
		if _, err := valueToInt64(val); err != nil {
			return preparedFormatArgument{}, fmt.Errorf("format %%%c expects integer operand", verb)
		}
	case 'f', 'F', 'e', 'E', 'g', 'G':
		switch val.Kind() {
		case KindInt, KindFloat:
		default:
			return preparedFormatArgument{}, fmt.Errorf("format %%%c expects numeric operand", verb)
		}
	case 't':
		if val.Kind() != KindBool {
			return preparedFormatArgument{}, fmt.Errorf("format %%t expects bool operand")
		}
	case 'v':
		if formatArgumentNeedsRenderedString(val) {
			render, err := prepareFormatStringRender(projection, val, hasPrecision, precision)
			if err != nil {
				return preparedFormatArgument{}, err
			}
			arg.stringRender = render
		}
	default:
		if formatArgumentNeedsRenderedString(val) {
			render, err := prepareFormatStringRender(projection, val, false, 0)
			if err != nil {
				return preparedFormatArgument{}, err
			}
			arg.stringRender = render
		}
	}
	return arg, nil
}

func prepareFormatStringRender(projection formatProjection, val Value, hasPrecision bool, precision int) (formatArgumentStringRender, error) {
	if formatArgumentHasDirectString(val) {
		return formatArgumentStringRender{}, nil
	}
	allowTruncated := false
	var limit int
	if hasPrecision {
		limit = saturatingMul(utf8.UTFMax, precision)
		allowTruncated = true
	} else {
		var err error
		limit, err = projection.stringBytes(val)
		if err != nil {
			return formatArgumentStringRender{}, err
		}
	}
	return formatArgumentStringRender{
		enabled:        true,
		limit:          limit,
		allowTruncated: allowTruncated,
	}, nil
}

func (a preparedFormatArgument) scratchBytes() int {
	if !a.stringRender.enabled {
		return 0
	}
	return a.stringRender.limit
}

func (a preparedFormatArgument) format() (any, error) {
	switch a.verb {
	case 's', 'q':
		if a.stringRender.enabled {
			return a.renderString()
		}
		return a.value.String(), nil
	case 'x', 'X':
		switch a.value.Kind() {
		case KindString, KindSymbol:
			return a.value.String(), nil
		case KindInt:
			if bi, ok := value.BigIntPayload(a.value); ok {
				// fmt formats *big.Int natively for the integer verbs.
				return bi, nil
			}
			return a.value.Int(), nil
		case KindFloat:
			return a.value.Float(), nil
		}
	case 'd', 'b', 'o', 'O':
		if bi, ok := value.BigIntPayload(a.value); ok {
			return bi, nil
		}
		return valueToInt64(a.value)
	case 'U', 'c':
		// Code-point verbs genuinely need an int64; a big integer keeps the
		// conversion error rather than truncating.
		return valueToInt64(a.value)
	case 'f', 'F', 'e', 'E', 'g', 'G':
		switch a.value.Kind() {
		case KindInt:
			// Value.Float converts big receivers best-effort (saturating to
			// the infinities), identical to float64(Int()) for compact values.
			return a.value.Float(), nil
		case KindFloat:
			return a.value.Float(), nil
		}
	case 't':
		return a.value.Bool(), nil
	default:
		if a.stringRender.enabled {
			return a.renderString()
		}
		return formatStringArgument(a.value), nil
	}
	return nil, fmt.Errorf("format %%%c received incompatible operand", a.verb)
}

func (a preparedFormatArgument) renderString() (string, error) {
	if a.stringRender.limit == 0 && a.stringRender.allowTruncated {
		return "", nil
	}
	if a.stringRender.limit <= 0 {
		return a.value.String(), nil
	}
	rendered, err := a.value.StringBounded(a.stringRender.limit)
	if err == nil {
		return rendered, nil
	}
	if a.stringRender.allowTruncated && errors.Is(err, errStringRenderTruncated) {
		return rendered, nil
	}
	return "", err
}

func formatArgumentUsesStringRendering(val Value) bool {
	return formatArgumentHasDirectString(val) || formatArgumentNeedsRenderedString(val)
}

func formatArgumentHasDirectString(val Value) bool {
	switch val.Kind() {
	case KindString, KindSymbol:
		return true
	default:
		return false
	}
}

func formatArgumentNeedsRenderedString(val Value) bool {
	switch val.Kind() {
	case KindInt, KindFloat, KindString, KindSymbol, KindBool, KindNil:
		return false
	default:
		return true
	}
}

func formatStringArgument(val Value) any {
	switch val.Kind() {
	case KindInt:
		if bi, ok := value.BigIntPayload(val); ok {
			return bi
		}
		return val.Int()
	case KindFloat:
		return val.Float()
	case KindString, KindSymbol:
		return val.String()
	case KindBool:
		return val.Bool()
	case KindNil:
		return nil
	default:
		return val.String()
	}
}

func builtinUUID(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
	if len(args) > 0 {
		return NewNil(), fmt.Errorf("uuid does not take arguments")
	}
	if len(kwargs) > 0 {
		return NewNil(), fmt.Errorf("uuid does not accept keyword arguments")
	}
	if !block.IsNil() {
		return NewNil(), fmt.Errorf("uuid does not accept blocks")
	}
	raw, err := exec.engine.randomBytes(exec.Context(), 16)
	if err != nil {
		return NewNil(), err
	}

	// RFC 9562 v7: unix timestamp milliseconds + random bits.
	nowMillis := uint64(time.Now().UTC().UnixMilli())
	raw[0] = byte(nowMillis >> 40)
	raw[1] = byte(nowMillis >> 32)
	raw[2] = byte(nowMillis >> 24)
	raw[3] = byte(nowMillis >> 16)
	raw[4] = byte(nowMillis >> 8)
	raw[5] = byte(nowMillis)
	raw[6] = (raw[6] & 0x0f) | 0x70
	raw[8] = (raw[8] & 0x3f) | 0x80
	return NewString(formatUUID(raw)), nil
}

func builtinRandomID(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
	if len(kwargs) > 0 {
		return NewNil(), fmt.Errorf("random_id does not accept keyword arguments")
	}
	if !block.IsNil() {
		return NewNil(), fmt.Errorf("random_id does not accept blocks")
	}

	length := int64(16)
	if len(args) > 1 {
		return NewNil(), fmt.Errorf("random_id expects at most one length argument")
	}
	if len(args) == 1 {
		if args[0].Kind() != KindInt {
			return NewNil(), fmt.Errorf("random_id length must be integer")
		}
		if args[0].IsBigInt() {
			return NewNil(), fmt.Errorf("random_id length must fit in a 64-bit integer")
		}
		length = args[0].Int()
	}
	if length <= 0 {
		return NewNil(), fmt.Errorf("random_id length must be positive")
	}
	if length > 1024 {
		return NewNil(), guardLimitErrorf("random_id length exceeds maximum 1024")
	}

	chars := make([]byte, 0, length)
	stalledReads := 0
	for int64(len(chars)) < length {
		needed := int(length) - len(chars)
		raw, err := exec.engine.randomBytes(exec.Context(), needed)
		if err != nil {
			return NewNil(), err
		}
		acceptedThisRead := 0
		for _, b := range raw {
			if b >= randomIDUnbiasedCutoff {
				continue
			}
			chars = append(chars, randomIDAlphabet[int(b)%len(randomIDAlphabet)])
			acceptedThisRead++
			if int64(len(chars)) == length {
				break
			}
		}
		if acceptedThisRead == 0 {
			stalledReads++
			if stalledReads > maxRandomIDStallReads {
				return NewNil(), fmt.Errorf("random_id entropy source rejected too many bytes")
			}
			continue
		}
		stalledReads = 0
	}
	return NewString(string(chars)), nil
}

func formatUUID(raw []byte) string {
	hexValue := hex.EncodeToString(raw)
	return hexValue[0:8] + "-" + hexValue[8:12] + "-" + hexValue[12:16] + "-" + hexValue[16:20] + "-" + hexValue[20:32]
}

func builtinJSONParse(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
	if len(args) != 1 || args[0].Kind() != KindString {
		return NewNil(), fmt.Errorf("JSON.parse expects a single JSON string argument")
	}
	if len(kwargs) > 0 {
		return NewNil(), fmt.Errorf("JSON.parse does not accept keyword arguments")
	}
	if !block.IsNil() {
		return NewNil(), fmt.Errorf("JSON.parse does not accept blocks")
	}

	raw := args[0].String()
	if len(raw) > maxJSONPayloadBytes {
		return NewNil(), guardLimitErrorf("JSON.parse input exceeds limit %d bytes", maxJSONPayloadBytes)
	}
	// Charged after the size guard, so an input the parser will never read
	// reports the established limit error rather than exhausting the quota.
	// Parsing reads every byte, and the input arrives as a builtin argument
	// rather than a string receiver, so nothing charged for it.
	if err := exec.chargeStringScan(len(raw)); err != nil {
		return NewNil(), err
	}

	parser := jsonValueParser{raw: raw, exec: exec, args: args}
	value, err := parser.parse()
	if err != nil {
		if _, ok := errors.AsType[jsonInvalidNumberError](err); ok {
			return NewNil(), err
		}
		return NewNil(), fmt.Errorf("JSON.parse invalid JSON: %w", err)
	}
	return value, nil
}

// builtinJSONParseAs parses JSON and validates the result against a shape in
// one step (ADR-004). The parsed value flows through normalizeValueForType,
// so validation failures carry the same semantics and message shape as the
// existing typed-boundary errors.
func builtinJSONParseAs(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
	if len(kwargs) > 0 {
		return NewNil(), fmt.Errorf("JSON.parse_as does not accept keyword arguments")
	}
	if !block.IsNil() {
		return NewNil(), fmt.Errorf("JSON.parse_as does not accept blocks")
	}
	if len(args) != 2 || args[0].Kind() != KindString {
		return NewNil(), fmt.Errorf("JSON.parse_as expects a JSON string and a type literal")
	}
	shape := valueShape(args[1])
	if shape == nil {
		return NewNil(), fmt.Errorf("JSON.parse_as expects a type literal as its second argument")
	}

	raw := args[0].String()
	if len(raw) > maxJSONPayloadBytes {
		return NewNil(), guardLimitErrorf("JSON.parse_as input exceeds limit %d bytes", maxJSONPayloadBytes)
	}
	// Charged after the size guard, as JSON.parse is.
	if err := exec.chargeStringScan(len(raw)); err != nil {
		return NewNil(), err
	}
	parser := jsonValueParser{raw: raw, exec: exec, args: args}
	parsed, err := parser.parse()
	if err != nil {
		if invalidNumber, ok := errors.AsType[jsonInvalidNumberError](err); ok {
			// The parser's error spells JSON.parse; rewrap under this
			// builtin's name.
			return NewNil(), fmt.Errorf("JSON.parse_as invalid number %q", string(invalidNumber))
		}
		return NewNil(), fmt.Errorf("JSON.parse_as invalid JSON: %w", err)
	}

	normalized, err := normalizeValueForType(parsed, shape, typeContext{
		owner:    exec.script,
		fallback: exec.root,
		exec:     exec,
	})
	if err != nil {
		if mismatch, ok := errors.AsType[*typeMismatchError](err); ok {
			return NewNil(), fmt.Errorf("JSON.parse_as value expected %s, got %s", mismatch.Expected, mismatch.Actual)
		}
		return NewNil(), err
	}
	return normalized, nil
}

func builtinJSONStringify(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
	if len(args) != 1 {
		return NewNil(), fmt.Errorf("JSON.stringify expects a single value argument")
	}
	if len(kwargs) > 0 {
		return NewNil(), fmt.Errorf("JSON.stringify does not accept keyword arguments")
	}
	if !block.IsNil() {
		return NewNil(), fmt.Errorf("JSON.stringify does not accept blocks")
	}

	state := jsonStringifyState{exec: exec}
	payload, err := appendJSONValue(make([]byte, 0, 256), args[0], &state)
	if err != nil {
		return NewNil(), err
	}
	// Settle the whole payload. Literals, delimiters and separators are appended
	// without passing through checkOutputBytes, so an aggregate holding no
	// strings -- an array of nil, say -- advanced the running charge not at all
	// while it built up to the output cap. checkOutputBytes bills only the
	// growth beyond what it has already charged, so this adds what the
	// incremental path missed rather than charging it twice.
	if err := state.checkOutputBytes(len(payload)); err != nil {
		return NewNil(), err
	}
	if len(payload) > maxJSONPayloadBytes {
		return NewNil(), guardLimitErrorf("JSON.stringify output exceeds limit %d bytes", maxJSONPayloadBytes)
	}
	return NewString(string(payload)), nil
}

func builtinRegexMatch(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
	if len(args) != 2 {
		return NewNil(), fmt.Errorf("Regex.match expects pattern and text")
	}
	if len(kwargs) > 0 {
		return NewNil(), fmt.Errorf("Regex.match does not accept keyword arguments")
	}
	if !block.IsNil() {
		return NewNil(), fmt.Errorf("Regex.match does not accept blocks")
	}
	if args[0].Kind() != KindString || args[1].Kind() != KindString {
		return NewNil(), fmt.Errorf("Regex.match expects string pattern and text")
	}
	pattern := args[0].String()
	text := args[1].String()
	if len(pattern) > maxRegexPatternSize {
		return NewNil(), guardLimitErrorf("Regex.match pattern exceeds limit %d bytes", maxRegexPatternSize)
	}
	if len(text) > maxRegexInputBytes {
		return NewNil(), guardLimitErrorf("Regex.match text exceeds limit %d bytes", maxRegexInputBytes)
	}

	work := regexWork{exec: exec}
	re, err := compileRegexNamespacePattern(&work, "Regex.match", pattern)
	if err != nil {
		return NewNil(), err
	}
	scan := regexNamespaceScan{re: re, work: &work, method: "Regex.match", wholeMatch: true}
	indices, err := scan.find(text, 0)
	if err != nil {
		return NewNil(), err
	}
	if indices == nil {
		return NewNil(), nil
	}
	return NewString(text[indices[0]:indices[1]]), nil
}

func builtinRegexpEscape(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
	if len(kwargs) > 0 {
		return NewNil(), fmt.Errorf("Regexp.escape does not accept keyword arguments")
	}
	if !block.IsNil() {
		return NewNil(), fmt.Errorf("Regexp.escape does not accept blocks")
	}
	if len(args) != 1 {
		return NewNil(), fmt.Errorf("Regexp.escape expects a string")
	}
	if args[0].Kind() != KindString {
		return NewNil(), fmt.Errorf("Regexp.escape expects a string")
	}
	escaped, err := regexpEscape(exec, receiver, args)
	if err != nil {
		return NewNil(), err
	}
	return NewString(escaped), nil
}

func builtinRegexpNew(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
	if len(kwargs) > 0 {
		return NewNil(), fmt.Errorf("Regexp.new does not accept keyword arguments")
	}
	if !block.IsNil() {
		return NewNil(), fmt.Errorf("Regexp.new does not accept blocks")
	}
	if len(args) != 1 {
		return NewNil(), fmt.Errorf("Regexp.new expects a pattern")
	}
	if args[0].Kind() != KindString {
		return NewNil(), fmt.Errorf("Regexp.new pattern must be string")
	}
	return compileRegexpNamespace(exec, "Regexp.new", args[0].String())
}

func builtinRegexpUnion(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
	if len(kwargs) > 0 {
		return NewNil(), fmt.Errorf("Regexp.union does not accept keyword arguments")
	}
	if !block.IsNil() {
		return NewNil(), fmt.Errorf("Regexp.union does not accept blocks")
	}
	pattern, err := regexpUnionPattern(exec, receiver, args)
	if err != nil {
		return NewNil(), err
	}
	return compileRegexpNamespace(exec, "Regexp.union", pattern)
}

func builtinRegexpLastMatch(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
	if len(args) > 0 {
		return NewNil(), fmt.Errorf("Regexp.last_match does not take arguments")
	}
	if len(kwargs) > 0 {
		return NewNil(), fmt.Errorf("Regexp.last_match does not accept keyword arguments")
	}
	if !block.IsNil() {
		return NewNil(), fmt.Errorf("Regexp.last_match does not accept blocks")
	}
	return NewNil(), nil
}

func builtinToInt(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
	if len(args) != 1 {
		return NewNil(), fmt.Errorf("to_int expects a single value argument")
	}
	if len(kwargs) > 0 {
		return NewNil(), fmt.Errorf("to_int does not accept keyword arguments")
	}
	if !block.IsNil() {
		return NewNil(), fmt.Errorf("to_int does not accept blocks")
	}

	switch args[0].Kind() {
	case KindInt:
		return args[0], nil
	case KindFloat:
		f := args[0].Float()
		if math.Trunc(f) != f {
			return NewNil(), fmt.Errorf("to_int cannot convert non-integer float")
		}
		// Finite whole floats beyond int64 promote to big integers; NaN and
		// the infinities keep the historical rejection.
		return floatWholeToIntValue(f, "to_int")
	case KindString:
		s := strings.TrimSpace(args[0].String())
		if s == "" {
			return NewNil(), fmt.Errorf("to_int expects a numeric string")
		}
		return parseIntegerString(exec, s, "to_int", args[0])
	default:
		return NewNil(), fmt.Errorf("to_int expects int, float, or string")
	}
}

func builtinToFloat(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
	if len(args) != 1 {
		return NewNil(), fmt.Errorf("to_float expects a single value argument")
	}
	if len(kwargs) > 0 {
		return NewNil(), fmt.Errorf("to_float does not accept keyword arguments")
	}
	if !block.IsNil() {
		return NewNil(), fmt.Errorf("to_float does not accept blocks")
	}

	switch args[0].Kind() {
	case KindInt:
		// Value.Float converts big integers best-effort (saturating to the
		// infinities), identical to float64(Int()) for compact values.
		return NewFloat(args[0].Float()), nil
	case KindFloat:
		return args[0], nil
	case KindString:
		s := strings.TrimSpace(args[0].String())
		if s == "" {
			return NewNil(), fmt.Errorf("to_float expects a numeric string")
		}
		f, err := strconv.ParseFloat(s, 64)
		if err != nil {
			return NewNil(), fmt.Errorf("to_float expects a numeric string")
		}
		if math.IsNaN(f) || math.IsInf(f, 0) {
			return NewNil(), fmt.Errorf("to_float expects a finite numeric string")
		}
		return NewFloat(f), nil
	default:
		return NewNil(), fmt.Errorf("to_float expects int, float, or string")
	}
}

func builtinRegexReplace(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
	return builtinRegexReplaceInternal(exec, args, kwargs, block, false)
}

func builtinRegexReplaceAll(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
	return builtinRegexReplaceInternal(exec, args, kwargs, block, true)
}

func builtinRegexReplaceInternal(exec *Execution, args []Value, kwargs map[string]Value, block Value, replaceAll bool) (Value, error) {
	method := "Regex.replace"
	if replaceAll {
		method = "Regex.replace_all"
	}

	if len(args) != 3 {
		return NewNil(), fmt.Errorf("%s expects text, pattern, replacement", method)
	}
	if len(kwargs) > 0 {
		return NewNil(), fmt.Errorf("%s does not accept keyword arguments", method)
	}
	if !block.IsNil() {
		return NewNil(), fmt.Errorf("%s does not accept blocks", method)
	}
	return builtinRegexReplaceValues(exec, args[0], args[1], args[2], replaceAll)
}

func builtinRegexReplaceValues(exec *Execution, textValue, patternValue, replacementValue Value, replaceAll bool) (Value, error) {
	method := "Regex.replace"
	if replaceAll {
		method = "Regex.replace_all"
	}

	if textValue.Kind() != KindString || patternValue.Kind() != KindString || replacementValue.Kind() != KindString {
		return NewNil(), fmt.Errorf("%s expects string text, pattern, replacement", method)
	}

	text := textValue.String()
	pattern := patternValue.String()
	replacement := replacementValue.String()
	if len(pattern) > maxRegexPatternSize {
		return NewNil(), guardLimitErrorf("%s pattern exceeds limit %d bytes", method, maxRegexPatternSize)
	}
	if len(text) > maxRegexInputBytes {
		return NewNil(), guardLimitErrorf("%s text exceeds limit %d bytes", method, maxRegexInputBytes)
	}
	if len(replacement) > maxRegexInputBytes {
		return NewNil(), guardLimitErrorf("%s replacement exceeds limit %d bytes", method, maxRegexInputBytes)
	}

	work := regexWork{exec: exec}
	re, err := compileRegexNamespacePattern(&work, method, pattern)
	if err != nil {
		return NewNil(), err
	}
	scan := regexNamespaceScan{re: re, work: &work, method: method}

	if replaceAll {
		replaced, err := regexReplaceAllWithLimit(&scan, text, replacement, method)
		if err != nil {
			return NewNil(), err
		}
		return NewString(replaced), nil
	}

	loc, err := scan.find(text, 0)
	if err != nil {
		return NewNil(), err
	}
	if loc == nil {
		return NewString(text), nil
	}
	replaced, err := appendRegexReplacement(&work, nil, re, replacement, text, loc)
	if err != nil {
		return NewNil(), fmt.Errorf("%s %w", method, err)
	}
	outputLen := len(text) - (loc[1] - loc[0]) + len(replaced)
	if outputLen > maxRegexInputBytes {
		return NewNil(), guardLimitErrorf("%s output exceeds limit %d bytes", method, maxRegexInputBytes)
	}
	if err := work.charge(outputLen); err != nil {
		return NewNil(), err
	}
	return NewString(text[:loc[0]] + string(replaced) + text[loc[1]:]), nil
}

func regexReplaceAllWithLimit(scan *regexNamespaceScan, text, replacement, method string) (string, error) {
	out := make([]byte, 0, len(text))
	lastAppended := 0
	searchStart := 0
	lastMatchEnd := -1
	for searchStart <= len(text) {
		loc, err := scan.find(text, searchStart)
		if err != nil {
			return "", err
		}
		if loc == nil {
			break
		}
		if loc[0] == loc[1] && loc[0] == lastMatchEnd {
			if loc[0] >= len(text) {
				break
			}
			_, size := utf8.DecodeRuneInString(text[loc[0]:])
			if size == 0 {
				size = 1
			}
			searchStart = loc[0] + size
			continue
		}

		segmentLen := loc[0] - lastAppended
		if len(out) > maxRegexInputBytes-segmentLen {
			return "", guardLimitErrorf("%s output exceeds limit %d bytes", method, maxRegexInputBytes)
		}
		if err := scan.work.charge(segmentLen); err != nil {
			return "", err
		}
		out = append(out, text[lastAppended:loc[0]]...)
		out, err = appendRegexReplacement(scan.work, out, scan.re, replacement, text, loc)
		if err != nil {
			return "", fmt.Errorf("%s %w", method, err)
		}
		lastAppended = loc[1]
		lastMatchEnd = loc[1]

		if loc[1] > loc[0] {
			searchStart = loc[1]
			continue
		}
		if loc[1] >= len(text) {
			break
		}
		_, size := utf8.DecodeRuneInString(text[loc[1]:])
		if size == 0 {
			size = 1
		}
		searchStart = loc[1] + size
	}

	tailLen := len(text) - lastAppended
	if len(out) > maxRegexInputBytes-tailLen {
		return "", guardLimitErrorf("%s output exceeds limit %d bytes", method, maxRegexInputBytes)
	}
	if err := scan.work.charge(tailLen + len(out) + tailLen); err != nil {
		return "", err
	}
	out = append(out, text[lastAppended:]...)
	return string(out), nil
}

func nextRegexReplaceAllSubmatchIndex(re *regexp.Regexp, text string, start int) ([]int, bool) {
	loc := re.FindStringSubmatchIndex(text[start:])
	if loc == nil {
		return nil, false
	}
	direct := offsetRegexSubmatchIndexInPlace(loc, start)
	if start == 0 || direct[0] > start {
		return direct, true
	}

	windowStart := start - 1
	locs := re.FindAllStringSubmatchIndex(text[windowStart:], 2)
	if len(locs) == 0 {
		return nil, false
	}

	first := offsetRegexSubmatchIndexInPlace(locs[0], windowStart)
	if first[0] >= start {
		return first, true
	}
	if first[1] > start {
		return direct, true
	}
	if len(locs) < 2 {
		return nil, false
	}
	second := offsetRegexSubmatchIndexInPlace(locs[1], windowStart)
	if second[0] >= start {
		return second, true
	}
	return nil, false
}

func offsetRegexSubmatchIndexInPlace(loc []int, offset int) []int {
	if offset == 0 {
		return loc
	}
	for i := range loc {
		if loc[i] < 0 {
			continue
		}
		loc[i] += offset
	}
	return loc
}
