package runtime

import (
	"errors"
	"fmt"
	"slices"
	"strings"

	"github.com/mgomes/vibescript/internal/ast"
)

var _ error = (*RuntimeError)(nil)

// StackFrame represents a single entry in a runtime error's call stack.
type StackFrame struct {
	Function string
	Pos      Position
	// Source is the module path for module-backed frames. It is empty for
	// root scripts compiled directly by an embedder.
	Source string
}

// RuntimeError represents a Vibescript runtime error with a call stack and source context.
type RuntimeError struct {
	Type      string
	Message   string
	CodeFrame string
	Frames    []StackFrame
}

type assertionFailureError struct {
	message string
}

func (e *assertionFailureError) Error() string {
	return e.message
}

type typedRuntimeError struct {
	kind string
	err  error
}

type guardLimitError struct {
	err error
}

func (e *typedRuntimeError) Error() string {
	return e.err.Error()
}

func (e *typedRuntimeError) Unwrap() error {
	return e.err
}

func (e *guardLimitError) Error() string {
	return e.err.Error()
}

func (e *guardLimitError) Unwrap() error {
	return e.err
}

func (e *guardLimitError) LimitError() bool {
	return true
}

// privateMemberError marks a member-resolution failure that occurred because the
// member exists but is private to the receiver. It wraps the formatted runtime
// error so callers still surface the full "private method" message, while member
// dispatch can distinguish it from a genuine unknown-member miss via errors.As.
// The universal members rely on that distinction so a private override of
// itself/eql?/equal? still raises instead of falling through to the builtin.
type privateMemberError struct {
	err error
}

func (e *privateMemberError) Error() string { return e.err.Error() }

func (e *privateMemberError) Unwrap() error { return e.err }

// privateMemberAccess wraps a formatted "private method" runtime error so member
// resolution can recognize it as a privacy block rather than a missing member.
func privateMemberAccess(err error) error {
	return &privateMemberError{err: err}
}

// isPrivateMemberError reports whether err signals a member blocked by privacy,
// as opposed to a member that does not exist on the receiver at all.
func isPrivateMemberError(err error) bool {
	_, ok := errors.AsType[*privateMemberError](err)
	return ok
}

const (
	runtimeErrorTypeBase      = ast.RuntimeErrorTypeBase
	runtimeErrorTypeStandard  = ast.RuntimeErrorTypeStandard
	runtimeErrorTypeAssertion = ast.RuntimeErrorTypeAssertion
	runtimeErrorTypeLimit     = ast.RuntimeErrorTypeLimit
	runtimeErrorTypeType      = ast.RuntimeErrorTypeType
	runtimeErrorTypeZeroDiv   = ast.RuntimeErrorTypeZeroDiv
	runtimeErrorTypeLocalJump = ast.RuntimeErrorTypeLocalJump
	runtimeErrorTypeArgument  = ast.RuntimeErrorTypeArgument
	runtimeErrorFrameHead     = 8
	runtimeErrorFrameTail     = 8
	stepSlowPathMask          = 15
	// stepSlowPathPeriod is how many steps separate two slow-path boundaries,
	// used to tell whether a bulk charge jumped over one.
	stepSlowPathPeriod = stepSlowPathMask + 1
)

var (
	errLoopBreak           = errors.New("loop break")
	errLoopNext            = errors.New("loop next")
	errRescueRetry         = errors.New("rescue retry")
	errStepQuotaExceeded   = errors.New("step quota exceeded")
	errMemoryQuotaExceeded = errors.New("memory quota exceeded")
	errOutputLimitExceeded = errors.New("output limit exceeded")
)

type loopBreakError struct {
	value Value
}

type loopNextError struct {
	value Value
}

type functionReturnError struct {
	value Value
}

func (e *loopBreakError) Error() string {
	return errLoopBreak.Error()
}

func (e *loopBreakError) Unwrap() error {
	return errLoopBreak
}

func newLoopBreakValue(value Value) error {
	return &loopBreakError{value: value}
}

func loopBreakValue(err error) (Value, bool) {
	if breakErr, ok := errors.AsType[*loopBreakError](err); ok {
		return breakErr.value, true
	}
	return NewNil(), false
}

func (e *loopNextError) Error() string {
	return errLoopNext.Error()
}

func (e *loopNextError) Unwrap() error {
	return errLoopNext
}

func newLoopNextValue(value Value) error {
	return &loopNextError{value: value}
}

func loopNextValue(err error) (Value, bool) {
	if nextErr, ok := errors.AsType[*loopNextError](err); ok {
		return nextErr.value, true
	}
	return NewNil(), false
}

func (e *functionReturnError) Error() string {
	return "function return"
}

func newFunctionReturnValue(value Value) error {
	return &functionReturnError{value: value}
}

func functionReturnValue(err error) (Value, bool) {
	if err == nil {
		return NewNil(), false
	}
	if returnErr, ok := errors.AsType[*functionReturnError](err); ok {
		return returnErr.value, true
	}
	return NewNil(), false
}

// Error returns the error message with a code frame and formatted stack trace.
func (re *RuntimeError) Error() string {
	var b strings.Builder
	b.WriteString(re.Message)
	if re.CodeFrame != "" {
		b.WriteString("\n")
		b.WriteString(re.CodeFrame)
	}
	renderFrame := func(frame StackFrame) {
		if frame.Pos.Line > 0 && frame.Pos.Column > 0 {
			fmt.Fprintf(&b, "\n  at %s (%d:%d)", frame.Function, frame.Pos.Line, frame.Pos.Column)
		} else if frame.Pos.Line > 0 {
			fmt.Fprintf(&b, "\n  at %s (line %d)", frame.Function, frame.Pos.Line)
		} else {
			fmt.Fprintf(&b, "\n  at %s", frame.Function)
		}
	}

	if len(re.Frames) <= runtimeErrorFrameHead+runtimeErrorFrameTail {
		for _, frame := range re.Frames {
			renderFrame(frame)
		}
		return b.String()
	}

	for _, frame := range re.Frames[:runtimeErrorFrameHead] {
		renderFrame(frame)
	}
	omitted := len(re.Frames) - (runtimeErrorFrameHead + runtimeErrorFrameTail)
	fmt.Fprintf(&b, "\n  ... %d frames omitted ...", omitted)
	for _, frame := range re.Frames[len(re.Frames)-runtimeErrorFrameTail:] {
		renderFrame(frame)
	}

	return b.String()
}

// Unwrap returns nil to satisfy the error unwrapping interface.
// RuntimeError is a terminal error that wraps the original error message but not the error itself.
func (re *RuntimeError) Unwrap() error {
	return nil
}

func classifyRuntimeErrorType(err error) string {
	if err == nil {
		return runtimeErrorTypeBase
	}
	if errors.Is(err, errStepQuotaExceeded) || errors.Is(err, errMemoryQuotaExceeded) || errors.Is(err, errOutputLimitExceeded) {
		return runtimeErrorTypeLimit
	}
	// errors.As rather than AsType: the target is a method-set interface that
	// does not itself satisfy error, which AsType's constraint requires.
	var limitErr interface{ LimitError() bool }
	if errors.As(err, &limitErr) && limitErr.LimitError() {
		return runtimeErrorTypeLimit
	}
	if _, ok := errors.AsType[*assertionFailureError](err); ok {
		return runtimeErrorTypeAssertion
	}
	if typedErr, ok := errors.AsType[*typedRuntimeError](err); ok {
		if kind, known := ast.CanonicalRuntimeErrorType(typedErr.kind); known {
			return kind
		}
	}
	if runtimeErr, ok := errors.AsType[*RuntimeError](err); ok {
		if kind, known := ast.CanonicalRuntimeErrorType(runtimeErr.Type); known {
			return kind
		}
	}
	return runtimeErrorTypeBase
}

func newAssertionFailureError(message string) error {
	return &assertionFailureError{message: message}
}

func newTypedRuntimeError(kind string, err error) error {
	if err == nil {
		err = errors.New("")
	}
	return &typedRuntimeError{kind: kind, err: err}
}

func guardLimitErrorf(format string, args ...any) error {
	return &guardLimitError{err: fmt.Errorf(format, args...)}
}

func zeroDivisionErrorf(format string, args ...any) error {
	return newTypedRuntimeError(runtimeErrorTypeZeroDiv, fmt.Errorf(format, args...))
}

// latchExhaustion records a genuine budget-exhaustion error on the execution.
// The first error wins; step() re-raises it on every subsequent charge and
// rescue refuses to match anything while it is set (see canRescueRuntimeError),
// so quota exhaustion terminates the script no matter what absorbs the
// original error value.
func (exec *Execution) latchExhaustion(err error) error {
	if exec.exhausted == nil {
		exec.exhausted = err
	}
	return err
}

func (exec *Execution) step() error {
	if exec.exhausted != nil {
		return exec.exhausted
	}
	if err := exec.chargeAssignmentWalk(); err != nil {
		return err
	}
	exec.steps++
	if exec.quota > 0 && exec.steps > exec.quota {
		return exec.latchExhaustion(fmt.Errorf("%w (%d)", errStepQuotaExceeded, exec.quota))
	}
	if (exec.steps & stepSlowPathMask) == 0 {
		return exec.stepSlowPathChecks()
	}
	if exec.ctx != nil && exec.steps == 1 {
		return exec.checkContext()
	}
	return nil
}

// stepSlowPathChecks runs the periodic reachable-graph and cancellation checks
// that step performs every stepSlowPathPeriod steps.
//
// Inside an accumulator-metered section the graph walk is skipped: the section's
// build loop charges all of its allocation before performing it and never
// re-enters script code, so the walk would re-measure an unchanged graph (see
// beginAccumulatorMeteredSection). The cancellation check still runs.
func (exec *Execution) stepSlowPathChecks() error {
	if exec.memoryQuota > 0 && exec.accumMeteredSections == 0 {
		if err := exec.checkMemory(); err != nil {
			return err
		}
	}
	return exec.checkContext()
}

func (exec *Execution) checkContext() error {
	if exec.ctx == nil {
		return nil
	}
	select {
	case <-exec.ctx.Done():
		return exec.ctx.Err()
	default:
		return nil
	}
}

// stepN charges n steps at once, observing the same quota and running step's
// periodic slow-path checks a single time. Big-integer operations use it to
// scale their cost with operand size (one step per 8 words by convention)
// without paying n separate slow-path polls; the quota still trips at exactly
// the same total step count it would for n individual step calls.
//
// "A single time" is the point: step runs those checks only when the counter
// lands exactly on a boundary, so a bulk charge that jumped over one without
// landing on it ran them ZERO times. A charge large enough to cover a whole
// period could then finish without ever observing a canceled context, which the
// n individual steps it stands in for would have caught. Every amortized charge
// in the tree is shaped that way (string scans, big-integer words, equality
// bytes, the predeclaration and estimator walks), so the crossing is detected
// here rather than at each call site.
func (exec *Execution) stepN(n int) error {
	if n <= 1 {
		return exec.step()
	}
	before := exec.steps
	exec.steps += n - 1
	if err := exec.step(); err != nil {
		return err
	}
	if (exec.steps&stepSlowPathMask) != 0 && before/stepSlowPathPeriod != exec.steps/stepSlowPathPeriod {
		return exec.stepSlowPathChecks()
	}
	return nil
}

// checkStepBudgetFor reports whether at least n more steps may be charged
// without exhausting the step quota, and whether the context is still live. It
// is used by builtins that will charge one step per element of a known-size
// receiver: when the remaining quota cannot cover all n steps the per-element
// loop is guaranteed to fail, so rejecting up front lets such a builtin skip bulk
// work (for example sorting a hash's keys) that the loop would otherwise perform
// before the first step() fails. A non-positive n performs only the cancellation
// check, so callers can pass a receiver length directly even when it is zero
// without requiring an element step that the loop would never charge. Like step,
// the per-element charge still observes the quota and cancellation, so this is
// purely an early-out and never accepts a build the loop would reject.
func (exec *Execution) checkStepBudgetFor(n int) error {
	if exec.exhausted != nil {
		return exec.exhausted
	}
	if n < 0 {
		n = 0
	}
	if n > 0 && exec.quota > 0 && exec.quota-exec.steps < n {
		// The pre-flight latches like the per-element loop it stands in for:
		// it fires exactly when that loop was guaranteed to grind the quota
		// to zero, so the budget is as spent as if the loop had run.
		return exec.latchExhaustion(fmt.Errorf("%w (%d)", errStepQuotaExceeded, exec.quota))
	}
	return exec.checkContext()
}

func (exec *Execution) errorAt(pos Position, format string, args ...any) error {
	return exec.newRuntimeError(fmt.Sprintf(format, args...), pos)
}

func (exec *Execution) newRuntimeError(message string, pos Position) error {
	return exec.newRuntimeErrorWithType(runtimeErrorTypeBase, message, pos)
}

func (exec *Execution) argumentErrorAt(pos Position, format string, args ...any) error {
	return exec.newRuntimeErrorWithType(runtimeErrorTypeArgument, fmt.Sprintf(format, args...), pos)
}

func (exec *Execution) localJumpErrorAt(pos Position, format string, args ...any) error {
	return exec.newRuntimeErrorWithType(runtimeErrorTypeLocalJump, fmt.Sprintf(format, args...), pos)
}

func (exec *Execution) newRuntimeErrorWithType(kind, message string, pos Position) error {
	if canonical, ok := ast.CanonicalRuntimeErrorType(kind); ok {
		kind = canonical
	} else {
		kind = runtimeErrorTypeBase
	}

	frames := make([]StackFrame, 0, len(exec.callStack)+1)

	if len(exec.callStack) > 0 {
		// First frame: where the error occurred (within the current function)
		current := exec.callStack[len(exec.callStack)-1]
		frames = append(frames, StackFrame{Function: current.Function, Pos: pos, Source: stackFrameSource(current.functionScript)})

		// Remaining frames: the call stack (where each function was called from)
		for i := len(exec.callStack) - 1; i >= 0; i-- {
			cf := exec.callStack[i]
			frames = append(frames, StackFrame{Function: cf.Function, Pos: cf.Pos, Source: stackFrameSource(cf.callSiteScript)})
		}
	} else {
		// No call stack means error at script top level
		frames = append(frames, StackFrame{Function: "<script>", Pos: pos, Source: stackFrameSource(exec.currentSourceScript())})
	}
	codeFrame := ""
	sourceScript := exec.script
	if len(exec.callStack) > 0 && exec.callStack[len(exec.callStack)-1].functionScript != nil {
		sourceScript = exec.callStack[len(exec.callStack)-1].functionScript
	}
	if sourceScript != nil {
		codeFrame = sourceScript.codeFrameFormatter().Format(pos)
	}
	return &RuntimeError{Type: kind, Message: message, CodeFrame: codeFrame, Frames: frames}
}

func stackFrameSource(script *Script) string {
	if script == nil {
		return ""
	}
	return script.modulePath
}

func (exec *Execution) wrapError(err error, pos Position) error {
	if err == nil {
		return nil
	}
	if isHostControlSignal(err) || isFunctionReturnSignal(err) || isRescueRetrySignal(err) {
		return err
	}
	if isNonLocalReturnSignal(err) {
		// A non-local return must reach its home invocation intact; flattening
		// it into a RuntimeError would lose the target token and make it
		// rescuable on the way.
		return err
	}
	if _, ok := errors.AsType[*RuntimeError](err); ok {
		return err
	}
	wrapped := exec.newRuntimeErrorWithType(classifyRuntimeErrorType(err), err.Error(), pos)
	if exec.exhausted != nil && errors.Is(err, exec.exhausted) && exec.exhaustedWrapped == nil {
		if re, ok := errors.AsType[*RuntimeError](wrapped); ok {
			// Snapshot the diagnostics before any adapter can mutate the
			// propagating object; the dispatch rebuild and the trusted
			// observation channel use only this copy.
			snapshot := *re
			snapshot.Frames = slices.Clone(re.Frames)
			exec.exhaustedWrapped = &snapshot
		}
	}
	return wrapped
}

// canonicalExhaustionMessage is the single-line quota message a latched
// exhaustion carries.
//
// It used to collapse a rendering as well: a task boundary latched the worker's
// wrapped *RuntimeError, whose Error() renders a code frame and stack, and
// surfacing that beside separately copied frames printed every frame twice. No
// latch can hold a *RuntimeError now that the boundary is gone -- every
// latchExhaustion argument is a fmt.Errorf around a quota sentinel -- so the
// collapse had no reachable input and went with the boundary that produced one.
func canonicalExhaustionMessage(exhausted error) string {
	return exhausted.Error()
}
