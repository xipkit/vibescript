package runtime

import (
	"context"
	"errors"
	"fmt"
	"math"
	"slices"
	"strings"
	"unicode"
	"unicode/utf8"

	"github.com/mgomes/vibescript/internal/ast"
	"github.com/mgomes/vibescript/vibes/value"
)

const (
	minInt64Float          = -9223372036854775808.0
	maxInt64FloatExclusive = 9223372036854775808.0
)

// blockGivenName is the reserved Kernel-style predicate that reports whether the
// current call was supplied a block, mirroring Ruby's block_given?. It is
// resolved before ordinary variable lookup so it cannot be shadowed.
const blockGivenName = "block_given?"

func (exec *Execution) evalExpression(expr Expression, env *Env) (Value, error) {
	return exec.evalExpressionWithAuto(expr, env, true)
}

func (exec *Execution) evalExpressionWithAuto(expr Expression, env *Env, autoCall bool) (Value, error) {
	if err := exec.step(); err != nil {
		return NewNil(), err
	}
	switch e := expr.(type) {
	case *Identifier:
		if e.Name == blockGivenName {
			return NewBool(blockGivenInCurrentCall(env)), nil
		}
		var self Value
		hasSelf := false
		if isConstantIdentifier(e.Name) {
			if val, ok := env.getCallLocal(e.Name); ok {
				env.clearArrayAppendBuffer(e.Name)
				if autoCall {
					return exec.autoInvokeIfNeeded(e, val, NewNil())
				}
				return val, nil
			}
			self, hasSelf = env.Get("self")
			if hasSelf && (self.Kind() == KindInstance || self.Kind() == KindClass) {
				if val, ok := classConstant(self, e.Name); ok {
					return val, nil
				}
			}
		}
		val, ok := env.getEscaping(e.Name)
		if !ok {
			// allow implicit self method lookup
			if !hasSelf {
				self, hasSelf = env.Get("self")
			}
			if hasSelf && (self.Kind() == KindInstance || self.Kind() == KindClass) {
				member, err := exec.getMember(self, e.Name, e.Pos())
				if err != nil {
					return NewNil(), err
				}
				if autoCall {
					return exec.autoInvokeIfNeeded(e, member, self)
				}
				return member, nil
			}
			return NewNil(), exec.errorAt(e.Pos(), "undefined variable %s%s", e.Name, didYouMean(e.Name, env.visibleNames()))
		}
		if autoCall {
			return exec.autoInvokeIfNeeded(e, val, NewNil())
		}
		return val, nil
	case *IntegerLiteral:
		if e.Big != nil {
			// Copy per evaluation so each occurrence yields a distinct object,
			// matching Ruby, where every bignum literal evaluation allocates
			// (equal? distinguishes them; == does not).
			return newBigIntValue(e.Big), nil
		}
		return NewInt(e.Value), nil
	case *FloatLiteral:
		return NewFloat(e.Value), nil
	case *StringLiteral:
		return NewString(e.Value), nil
	case *RegexLiteral:
		result, err := compileRegexValue("regex literal", e.Pattern, e.Flags)
		if err != nil {
			return NewNil(), exec.wrapError(err, e.Pos())
		}
		return result, nil
	case *InterpolatedString:
		return exec.evalInterpolatedStringLiteral(e, env)
	case *InterpolatedSymbol:
		return exec.evalInterpolatedSymbolLiteral(e, env)
	case *BoolLiteral:
		return NewBool(e.Value), nil
	case *NilLiteral:
		return NewNil(), nil
	case *SymbolLiteral:
		return exec.symbolLiteralValue(e), nil
	case *ArrayLiteral:
		return exec.evalArrayLiteral(e, env)
	case *HashLiteral:
		if e.ShapeType != nil && !exec.hashShapeShadowed(e, env) {
			return NewShape(e.ShapeType), nil
		}
		return exec.evalHashLiteral(e, env)
	case *SplatArg:
		return NewNil(), exec.errorAt(e.Pos(), "splat argument is only allowed in call arguments")
	case *TypeLiteral:
		if exec.typeLiteralShadowed(e, env) {
			// The fallback inherits the caller's auto-call state: a callable
			// bound to a type-spelled name still passes bare into a
			// function-typed parameter instead of auto-invoking.
			return exec.evalExpressionWithAuto(e.Fallback, env, autoCall)
		}
		return NewShape(e.Type), nil
	case *UnaryExpr:
		return exec.evalUnaryExpr(e, env)
	case *BinaryExpr:
		return exec.evalBinaryExpr(e, env)
	case *ConditionalExpr:
		return exec.evalConditionalExpr(e, env)
	case *RescueExpr:
		return exec.evalRescueExpr(e, env, autoCall)
	case *IfExpr:
		return exec.evalIfExpr(e, env)
	case *IfStmt:
		return exec.evalIfStatementExpression(e, env)
	case *RangeExpr:
		return exec.evalRangeExpr(e, env)
	case *CaseExpr:
		return exec.evalCaseExpr(e, env)
	case *MemberExpr:
		var obj Value
		var err error
		if isCollectionMutator(e.Property) {
			// A parenless mutator (`a.pop`, `a.shift`) is auto-invoked from
			// here rather than through a call expression, so it records the
			// path that owns its receiver the same way, or the write would
			// land on a copy the script never sees again.
			defer exec.restore(exec.savedAddressedScope())
			var addressed bool
			obj, addressed, err = exec.addressMutableReceiver(e.Object, env)
			if !addressed {
				// See evalMemberCallExpr: an ordinary evaluation must not
				// inherit the enclosing permission.
				exec.withdrawAddressed()
				obj, err = exec.evalExpressionWithAuto(e.Object, env, memberReceiverAutoInvokes(e.Object, e.Property, env))
			}
			if err != nil {
				return NewNil(), err
			}
			return exec.finishMemberExpr(e, obj, env, autoCall)
		}
		if _, objIsMember := e.Object.(*MemberExpr); e.Property == "call" && objIsMember {
			// Resolve a member-of-member receiver exactly like the
			// parenthesized call form so c.cb.call (no parens) sees the stored
			// callable or invoked getter value, not the raw getter builtin.
			// Non-member objects keep the plain no-auto-invoke path so a bare
			// stored callable (cb.call) is not invoked early.
			obj, err = exec.evalMemberCallReceiver(e, env, memberCallReceiverAutoInvokes)
		} else {
			obj, err = exec.evalExpressionWithAuto(e.Object, env, memberReceiverAutoInvokes(e.Object, e.Property, env))
		}
		if err != nil {
			return NewNil(), err
		}
		return exec.finishMemberExpr(e, obj, env, autoCall)
	case *ScopeExpr:
		obj, err := exec.evalExpressionWithAuto(e.Object, env, true)
		if err != nil {
			return NewNil(), err
		}
		member, err := exec.getScopedMember(obj, e.Property, e.Pos())
		if err != nil {
			return NewNil(), err
		}
		return member, nil
	case *IndexExpr:
		return exec.evalIndexExpr(e, env)
	case *IvarExpr:
		self, ok := env.Get("self")
		if !ok || self.Kind() != KindInstance {
			return NewNil(), exec.errorAt(e.Pos(), "no instance context for ivar")
		}
		val, ok := valueInstance(self).Ivars[e.Name]
		if !ok {
			return NewNil(), nil
		}
		return val, nil
	case *ClassVarExpr:
		self, ok := env.Get("self")
		if !ok {
			return NewNil(), exec.errorAt(e.Pos(), "no class context")
		}
		switch self.Kind() {
		case KindInstance:
			val, ok := valueInstance(self).Class.ClassVars[e.Name]
			if !ok {
				return NewNil(), nil
			}
			return val, nil
		case KindClass:
			val, ok := valueClass(self).ClassVars[e.Name]
			if !ok {
				return NewNil(), nil
			}
			return val, nil
		default:
			return NewNil(), exec.errorAt(e.Pos(), "no class context")
		}
	case *CallExpr:
		return exec.evalCallExpr(e, env)
	case *BlockLiteral:
		// A block only ever appears in a call's block position, where
		// evalCallBlock evaluates it. Reaching it as a standalone expression
		// would make it a value, which the language does not have.
		return NewNil(), exec.errorAt(e.Pos(), "a block cannot be used as a value; write it on the call that runs it")
	case *YieldExpr:
		return exec.evalYield(e, env)
	case *ForStmt:
		return exec.evalForExpression(e, env)
	case *WhileStmt:
		return exec.evalWhileExpression(e, env)
	case *UntilStmt:
		return exec.evalUntilExpression(e, env)
	case *TryStmt:
		return exec.evalTryExpression(e, env)
	default:
		return NewNil(), exec.errorAt(expr.Pos(), "unsupported expression")
	}
}

func (exec *Execution) evalInterpolatedStringLiteral(lit *InterpolatedString, env *Env) (Value, error) {
	text, err := exec.buildInterpolatedString(lit.Parts, env)
	if err != nil {
		return NewNil(), err
	}
	return NewString(text), nil
}

func (exec *Execution) evalInterpolatedSymbolLiteral(lit *InterpolatedSymbol, env *Env) (Value, error) {
	name, err := exec.buildInterpolatedString(lit.Parts, env)
	if err != nil {
		return NewNil(), err
	}
	return NewSymbol(name), nil
}

// buildInterpolatedString materializes the text of an interpolated string or
// symbol from its parts, honoring the sandbox step and memory quotas.
func (exec *Execution) buildInterpolatedString(parts []StringPart, env *Env) (string, error) {
	var sb strings.Builder
	for _, part := range parts {
		switch p := part.(type) {
		case StringText:
			if err := exec.appendInterpolatedChunk(&sb, p.Text); err != nil {
				return "", err
			}
		case StringExpr:
			val, err := exec.evalExpressionWithAuto(p.Expr, env, true)
			if err != nil {
				return "", err
			}
			// A class's to_s is the string form of its instances, so it
			// governs interpolation too; substituting here keeps the quota
			// accounting below measuring the text actually written.
			val, _, err = exec.instanceStringValue(val, p.Expr.Pos())
			if err != nil {
				return "", err
			}
			if err := exec.appendInterpolatedValue(&sb, val); err != nil {
				return "", err
			}
		}
	}
	return sb.String(), nil
}

// appendInterpolatedChunk writes a literal text chunk to the interpolation
// builder while keeping the partially built result inside the sandbox limits.
// step() honors a canceled context and the step quota during repeated or large
// interpolation, and checkProjectedStringBytes rejects the materialization
// before the builder grows past the memory quota. The projected check is keyed
// on the builder's projected backing capacity (see projectedBuilderCap) so a
// doubling interpolation such as "#{text}#{text}" fails fast instead of
// allocating the oversized backing array that the surrounding evaluator would
// only observe after it already exists. Small interpolations stay on the fast
// path: with no quotas the checks are O(1) no-ops.
//
// Charging the projected capacity rather than sb.Len()+len(chunk) matters
// because Builder.Grow does not reserve exactly the requested bytes once the
// current backing is exhausted: it reallocates to roundedAllocSize(2*cap+n).
// After a prefix or a prior interpolation the doubled-and-rounded term can
// exceed the running length plus the chunk, so charging only the final length
// would let the real reservation escape the memory quota. projectedBuilderCap
// reproduces Grow's reallocation, including the allocator's size-class rounding,
// so the quota check accounts for the backing array actually reserved.
func (exec *Execution) appendInterpolatedChunk(sb *strings.Builder, chunk string) error {
	if err := exec.step(); err != nil {
		return err
	}
	if err := exec.checkProjectedStringBytes(projectedBuilderCap(sb, len(chunk))); err != nil {
		return err
	}
	sb.Grow(len(chunk))
	sb.WriteString(chunk)
	return nil
}

// projectedBuilderCap reports the backing-array capacity sb will hold after
// sb.Grow(n), so a quota check can account for the bytes Grow actually reserves
// rather than the bytes the caller intends to write.
//
// Builder.Grow only reallocates when the free tail (Cap-Len) cannot hold n more
// bytes. When it does, strings.Builder requests 2*Cap+n bytes through
// bytealg.MakeNoZero, and the runtime rounds that request up to an allocator
// size class before reserving the backing array. The realized capacity is
// therefore roundedAllocSize(2*Cap+n), which can exceed 2*Cap+n: growing a
// 10 KiB builder by 10 KiB requests 30,720 bytes but reserves the 32,768-byte
// class. Charging only 2*Cap+n would leave a quota between the request and the
// rounded class, letting the check pass while Grow allocates over the limit.
// roundedAllocSize mirrors the runtime's rounding exactly (see sizeclass.go), so
// the projection equals the realized capacity. When the value already fits the
// free tail, no reallocation happens and the current capacity is returned
// unchanged, preserving the no-copy fast path. n must be non-negative, matching
// Grow.
func projectedBuilderCap(sb *strings.Builder, n int) int {
	capacity := sb.Cap()
	if capacity-sb.Len() >= n {
		return capacity
	}
	return roundedAllocSize(saturatingAdd(saturatingMul(2, capacity), n))
}

// appendInterpolatedValue renders val into the interpolation builder under the
// same sandbox limits as appendInterpolatedChunk. It projects the rendered byte
// length with Value.StringByteLenBounded before materializing, so an aggregate
// whose String representation expands far beyond its own footprint (for example
// an array holding many references to one large string) is rejected by the
// memory quota instead of allocating the oversized rendering first and only then
// failing the post-build check. StringByteLenBounded walks the aggregate without
// allocating the joined result, so the projection is the only work done for a
// value that overruns the quota, and it charges exec.step per visited node so
// the walk itself is bounded by the step quota (see the call site below).
//
// The projection also charges val's own footprint, not just the rendered output.
// An interpolated expression can produce a temporary that no environment holds —
// a function return, or an array/hash literal constructed inline — which stays
// live on the Go call stack while WriteStringTo copies its rendering. That
// temporary is invisible to the env-reachable base, so charging only the output
// would let base+value+output exceed the quota during the write even though
// base+output passes. checkProjectedValueRendering deduplicates val against the
// base, so a value already reachable from an environment is not double counted and
// the small-interpolation fast path is unchanged.
//
// Once the projection passes, the builder is grown by exactly the projected
// payload and Value.WriteStringTo streams the rendering straight into sb rather
// than building a temporary string and copying it in. A second full copy would
// transiently hold both the temporary rendering and the builder copy, so a quota
// close to the final output size could be exceeded even though the single-payload
// projection passed. Reserving the payload up front also keeps WriteStringTo's
// per-element writes from triggering the builder's doubling growth, which would
// overshoot the quota-checked size; the peak allocation stays a single rendering,
// matching what the projection accounted for.
//
// The projection charges the builder's projected backing capacity (see
// projectedBuilderCap), not sb.Len()+payload. Builder.Grow reallocates to
// roundedAllocSize(2*cap+payload) once the current backing is full, so after a
// prefix or a prior interpolation the reserved backing can exceed the running
// length plus the payload. Charging the projected capacity keeps that
// reservation inside the memory quota; for a value that fits the free tail no
// reallocation happens and the fast path is unchanged.
func (exec *Execution) appendInterpolatedValue(sb *strings.Builder, val Value) error {
	if err := exec.step(); err != nil {
		return err
	}
	// StringByteLenBounded charges exec.step once per node it visits, so the
	// projection walk itself is bounded by the step quota. A composite with a
	// compact but exponentially shared graph (for example a = [a, a] repeated)
	// has bounded memory and a bounded rendering — the cycle marker collapses
	// the repetition once it is on the recursion stack — yet projecting its
	// length re-walks every shared subtree, which is exponential in the nesting
	// depth. Charging steps during that walk (rather than only once per
	// interpolation part) trips the quota or honors a canceled context instead
	// of burning unbounded CPU before the memory check runs.
	// A regex is sized from its source, and StringByteLenBounded is skipped
	// entirely for one. That walk reaches len(v.String()), which escapes and
	// allocates the whole literal to size it -- so the measurement performs the
	// rendering, ahead of the charge and beyond the quota's reach, and
	// WriteStringTo performs it again afterwards. Regex.StringLen computes the
	// same length by walking the source, allocating nothing.
	payload := 0
	if val.Kind() == KindRegex {
		// StringLen walks the whole source, so that walk is charged before it
		// runs; the rendered length is charged after, because rendering walks it
		// again. Two passes happen and both are billed.
		if err := exec.chargeStringScan(len(val.Regex().Source)); err != nil {
			return err
		}
		payload = val.Regex().StringLen()
		if err := exec.chargeStringScan(payload); err != nil {
			return err
		}
	} else {
		var err error
		payload, err = val.StringByteLenBounded(exec.step)
		if err != nil {
			return err
		}
	}
	// Charge for the bytes about to be rendered: the bounded walk steps once
	// per node, which bounds a large aggregate, but a scalar is one node however
	// many bytes it carries (a symbol built from a host-supplied string renders
	// its whole name). See chargeStringScan.
	if val.Kind() != KindRegex {
		if err := exec.chargeStringScan(payload); err != nil {
			return err
		}
	}

	if err := exec.checkProjectedValueRendering(val, projectedBuilderCap(sb, payload)); err != nil {
		return err
	}
	// Grow only on a positive payload: StringByteLen sums byte counts without
	// saturating, so a rendering larger than the int range (physically
	// unreachable but not statically excluded) could wrap negative, and Grow
	// panics on a negative count.
	if payload > 0 {
		sb.Grow(payload)
	}
	// WriteStringTo streams the rendering straight into sb without materializing a
	// separate string, so the peak allocation stays the single reservation made
	// above. Writing into a strings.Builder never fails, so there is no error to
	// surface here.
	val.WriteStringTo(sb)
	return nil
}

// evalArrayLiteral evaluates an array literal element by element, charging each
// new element against the memory quota as it lands in the Go-local backing slice.
// Each evaluated element stays live in that slice until NewArray binds it, so the
// running footprint must include every element gathered so far; a literal whose
// elements are fresh temporaries (for example [big[0, n], big[0, n]], where each
// bracket slice is a fresh copy invisible to the base estimator) would otherwise
// stack several copies past the quota before any later statement check observed
// them. The accumulator snapshots the baseline once and charges each element
// incrementally, deduplicating elements aliased by an environment root.
func (exec *Execution) evalArrayLiteral(e *ArrayLiteral, env *Env) (Value, error) {
	return exec.evalArrayLiteralWithElementType(e, env, nil)
}

func (exec *Execution) evalArrayLiteralWithElementType(e *ArrayLiteral, env *Env, elementType *TypeExpr) (Value, error) {
	return exec.evalArrayLiteralWithElementExpectation(e, env, func(_, _ int) expressionExpectation {
		return typeExpressionExpectation(elementType)
	})
}

// A literal of at most one element charges its result once, through the
// memoized check, rather than running an incremental build accumulator.
//
// The accumulator exists so a literal whose elements are fresh temporaries
// cannot stack several copies past the quota before a later statement check
// observes them. Its baseline is a full unmemoized reference walk of the
// reachable graph (newArrayBuildAccumulator -> estimateMemoryUsageBase), so it
// costs O(reachable) per literal however few slots that literal allocates.
// Building a nested structure in a loop -- cur = [cur] -- therefore paid a
// whole-graph walk every iteration to allocate a single slot, which was most of
// the construction cost in #1124.
//
// One element is the largest exemption that gives up nothing. The accumulator's
// guarantee is about the *second* and later temporaries: it rejects before the
// next one is allocated. With a single element there is no next one, and the
// element itself was allocated by evaluating its own sub-expression, which is
// exactly the exposure `f(big)` already carries. The finished array is then
// charged by checkMemoryValue through the memoized base walk -- O(1) when the
// memo is current, and walking only the array's own marginal bytes, since the
// element is already counted.
//
// A wider exemption would not be sound: at n elements a script could hold n
// fresh temporaries before any check, which for a sandbox is n times the quota.
const singleElementArrayLiteral = 1

func (exec *Execution) evalArrayLiteralWithElementExpectation(e *ArrayLiteral, env *Env, elementExpectation func(int, int) expressionExpectation) (Value, error) {
	var acc *arrayBuildAccumulator
	if len(e.Elements) > singleElementArrayLiteral {
		acc = newArrayBuildAccumulator(exec, NewNil(), nil, nil, NewNil())
	}
	elems := make([]Value, 0, len(e.Elements))
	for i, el := range e.Elements {
		expectation := expressionExpectation{}
		if elementExpectation != nil {
			expectation = elementExpectation(i, len(e.Elements))
		}
		val, err := exec.evalExpressionWithExpectation(el, env, expectation)
		if err != nil {
			return NewNil(), err
		}
		elems = append(elems, val)
		if acc == nil {
			continue
		}
		if err := acc.add(val, cap(elems)); err != nil {
			return NewNil(), err
		}
	}
	result := NewArray(elems)
	if acc == nil {
		if err := exec.checkMemoryValue(result); err != nil {
			return NewNil(), err
		}
	}
	return result, nil
}

// evalHashLiteral evaluates a hash literal pair by pair, charging each new entry
// against the memory quota as it lands in the Go-local map. The partially built
// map is reachable from no environment until NewHash binds it, so the running
// footprint must include every entry inserted so far; a literal whose values are
// fresh temporaries (for example {a: big[0, n], b: big[0, n]}) would otherwise
// stack several copies past the quota before any later statement check observed
// them. The accumulator reserves the entry backing up front and charges each
// entry's key and value payloads incrementally, deduplicating payloads aliased by
// an environment root.
func (exec *Execution) evalHashLiteral(e *HashLiteral, env *Env) (Value, error) {
	return exec.evalHashLiteralWithValueTypes(e, env, nil)
}

// singlePairHashLiteral is the largest hash literal built without an
// accumulator. The accumulator's guarantee is about the second temporary: with
// one pair there is no partially built map holding an earlier entry while the
// next evaluates, so the finished hash is charged once through the memoized
// check instead — the same exemption single-element array literals take.
const singlePairHashLiteral = 1

func (exec *Execution) evalHashLiteralWithValueTypes(e *HashLiteral, env *Env, valueTypeForKey func(Value) *TypeExpr) (Value, error) {
	var acc *hashLiteralBuildAccumulator
	if exec.memoryQuota > 0 && len(e.Pairs) > singlePairHashLiteral {
		var err error
		acc, err = newHashLiteralBuildAccumulator(exec)
		if err != nil {
			return NewNil(), err
		}
		if err := acc.reserveBacking(len(e.Pairs)); err != nil {
			return NewNil(), err
		}
	}
	hash := NewHash(make(map[string]Value, len(e.Pairs)))
	// Pre-size the insertion-order backing to the pair count (the same bound
	// reserveBacking charges) so HashSet's appends do not grow it past the order
	// slots the memory projection accounts for. The unpublished variants skip
	// the mutation-epoch bump: this hash is reachable from no root until the
	// finished literal is bound or stored (which bumps), so the writes cannot
	// stale the base-walk memo, and bumping anyway invalidated it three times
	// per literal — one driver of #1129's quadratic build loops.
	hash.ReserveHashOrderUnpublished(len(e.Pairs))
	entries := make(map[string]hashLiteralEntry, len(e.Pairs))
	for _, pair := range e.Pairs {
		keyVal, err := exec.evalExpressionWithAuto(pair.Key, env, true)
		if err != nil {
			return NewNil(), err
		}
		// The entry map hashes the key's whole text, so the key's bytes are
		// charged before the write runs, like every other key site.
		if err := exec.chargeValueKeySteps(keyVal); err != nil {
			return NewNil(), exec.wrapError(err, pair.Key.Pos())
		}
		key, err := hashKeyString(keyVal)
		if err != nil {
			return NewNil(), exec.errorAt(pair.Key.Pos(), "%s", err.Error())
		}
		var valueType *TypeExpr
		if valueTypeForKey != nil {
			valueType = valueTypeForKey(keyVal)
		}
		val, err := exec.evalExpressionWithExpectedType(pair.Value, env, valueType)
		if err != nil {
			return NewNil(), err
		}
		if acc != nil {
			_, replacing := entries[key]
			if replacing || acc.replacing {
				err = acc.replaceEntry(key, val, entries)
			} else {
				err = acc.addDistinctEntry(entries, key, val)
			}
			if err != nil {
				return NewNil(), err
			}
		}
		if err := hashSetUnpublished(hash, keyVal, val); err != nil {
			return NewNil(), exec.errorAt(pair.Key.Pos(), "%s", err.Error())
		}
		entries[key] = hashLiteralEntry{key: key, value: val}
	}
	if acc == nil {
		if err := exec.checkMemoryValue(hash); err != nil {
			return NewNil(), err
		}
	}
	return hash, nil
}

func (exec *Execution) evalExpressionWithExpectedType(expr Expression, env *Env, ty *TypeExpr) (Value, error) {
	return exec.evalExpressionWithExpectation(expr, env, typeExpressionExpectation(ty))
}

func (exec *Execution) evalExpressionWithExpectation(expr Expression, env *Env, expectation expressionExpectation) (Value, error) {
	if expectation.empty() {
		return exec.evalExpressionWithAuto(expr, env, true)
	}
	return exec.evalCallArgumentForExpectation(expr, env, expectation)
}

func (exec *Execution) evalUnaryExpr(e *UnaryExpr, env *Env) (Value, error) {
	right, err := exec.evalExpressionWithAuto(e.Right, env, true)
	if err != nil {
		return NewNil(), err
	}
	if err := exec.checkMemoryValue(right); err != nil {
		return NewNil(), err
	}
	switch e.Operator {
	case tokenMinus:
		switch right.Kind() {
		case KindInt:
			if n, ok := right.CompactInt(); ok {
				if n == math.MinInt64 {
					// -MinInt64 does not fit int64; promote instead of the
					// silent two's-complement wrap the unchecked negation had.
					return negIntValueBig(right), nil
				}
				return NewInt(-n), nil
			}
			return negIntValueBig(right), nil
		case KindFloat:
			return NewFloat(-right.Float()), nil
		default:
			return NewNil(), exec.errorAt(e.Pos(), "unsupported unary - operand")
		}
	case tokenPlus:
		// Unary plus mirrors Ruby: it is the identity on numbers and strings.
		// Vibescript strings are immutable values, so returning the same value
		// matches Ruby's "unfrozen copy" semantics observably.
		switch right.Kind() {
		case KindInt, KindFloat, KindString:
			return right, nil
		default:
			return NewNil(), exec.errorAt(e.Pos(), "unsupported unary + operand")
		}
	case tokenBang:
		return NewBool(!right.Truthy()), nil
	default:
		return NewNil(), exec.errorAt(e.Pos(), "unsupported unary operator")
	}
}

// hashShapeShadowed reports whether any type name of a dual-reading braced
// group resolves to a runtime binding, in which case the group keeps its
// pre-existing hash semantics (a host-provided global named string, for
// example). A group with no hash reading (ShapeType without pairs) is always
// a shape.
func (exec *Execution) hashShapeShadowed(lit *HashLiteral, env *Env) bool {
	if len(lit.Pairs) == 0 {
		return false
	}
	return exec.shapeTypeNamesShadowed(lit.ShapeType, env)
}

// shapeTypeNamesShadowed reports whether any type name of a dual-reading
// group (a braced shape or an argument type literal) resolves to a runtime
// binding, in which case the group keeps its value reading.
func (exec *Execution) shapeTypeNamesShadowed(ty *TypeExpr, env *Env) bool {
	shadowed := false
	walkShapeTypeNames(ty, func(name string) {
		if !shadowed && exec.runtimeNameShadowed(name, env) {
			shadowed = true
		}
	})
	return shadowed
}

// typeLiteralShadowed reports whether an argument type literal keeps its
// value reading. A literal only carries a fallback when the whole group is a
// bare identifier, so the sole binding that can change its meaning is that
// identifier's verbatim spelling: `string?` is shadowed by a binding named
// `string?`, not by an unrelated one named `string`.
func (exec *Execution) typeLiteralShadowed(e *TypeLiteral, env *Env) bool {
	ident, ok := e.Fallback.(*Identifier)
	if !ok {
		return false
	}
	return exec.runtimeNameShadowed(ident.Name, env)
}

// runtimeNameShadowed reports whether name resolves to a runtime binding: an
// environment lookup, or implicit self (a zero-arity method named string, for
// example), matching evalExpression's identifier fallback.
func (exec *Execution) runtimeNameShadowed(name string, env *Env) bool {
	if _, ok := env.Get(name); ok {
		return true
	}
	self, hasSelf := env.Get("self")
	if hasSelf && (self.Kind() == KindInstance || self.Kind() == KindClass) {
		return exec.respondsTo(self, name, true)
	}
	return false
}

func (exec *Execution) evalIndexExpr(e *IndexExpr, env *Env) (Value, error) {
	obj, err := exec.evalExpressionWithAuto(e.Object, env, true)
	if err != nil {
		return NewNil(), err
	}
	if err := exec.checkMemoryValue(obj); err != nil {
		return NewNil(), err
	}
	if len(e.Indices) == 1 {
		idx, err := exec.evalIndexSelector(e, obj, e.Indices[0], env)
		if err != nil {
			return NewNil(), err
		}
		var indices [1]Value
		indices[0] = idx
		result, err := exec.evalIndexValue(e, obj, indices[:])
		if err != nil {
			return NewNil(), err
		}
		if exec.memoryQuota > 0 {
			if err := exec.checkMemoryWith(obj, idx, result); err != nil {
				return NewNil(), err
			}
		}
		return result, nil
	}
	indices, err := exec.evalIndexSelectors(e, obj, env)
	if err != nil {
		return NewNil(), err
	}
	result, err := exec.evalIndexValue(e, obj, indices)
	if err != nil {
		return NewNil(), err
	}
	// The start/length and range forms return a fresh subarray or substring that
	// lives only on the Go stack here; an enclosing literal keeps prior elements
	// in Go-local slots its own per-element check cannot see, so a form such as
	// [big[0, n], big[0, n]] could stack several slice copies past the quota
	// before a later statement check observed them. Charge the receiver, the live
	// selectors, and the fresh result together so the peak is rejected at the
	// source. The estimator deduplicates the receiver and any env-reachable
	// selectors, so a plain element read (which returns an aliased value, not a
	// fresh allocation) charges nothing beyond what the earlier checks already did.
	// Skip building the charge slice entirely when no quota is enforced, keeping
	// the common indexed read allocation-free.
	if exec.memoryQuota > 0 {
		chargeable := make([]Value, 0, len(indices)+2)
		chargeable = append(chargeable, obj)
		chargeable = append(chargeable, indices...)
		chargeable = append(chargeable, result)
		if err := exec.checkMemoryWith(chargeable...); err != nil {
			return NewNil(), err
		}
	}
	return result, nil
}

func (exec *Execution) evalIndexSelector(e *IndexExpr, obj Value, expr Expression, env *Env) (Value, error) {
	idx, err := exec.evalExpressionWithAuto(expr, env, true)
	if err != nil {
		return NewNil(), err
	}
	if exec.memoryQuota > 0 {
		if err := exec.checkMemoryWith(obj, idx); err != nil {
			return NewNil(), err
		}
	}
	return idx, nil
}

// evalIndexSelectors evaluates every selector between the brackets, charging
// the receiver together with the accumulated selectors against the memory quota
// as each materializes. The receiver stays live in the caller's obj while
// selectors are evaluated, yet a fresh receiver (a large literal hash/array
// indexed inline) is not reachable from any environment, so the base walk never
// sees it; charging it alongside the selectors here rejects a peak of receiver +
// selectors at the source rather than after evalIndexValue, whose arity/type
// error would otherwise skip the later combined check and mask the quota breach.
// Every evaluated selector also stays live in indices until dispatch, so each
// check must account for all selectors gathered so far; charging only the
// current one would undercount a multi-selector form whose earlier selectors are
// still resident (the parser permits arbitrary comma-separated selectors). The
// estimator deduplicates against the reachable roots, so the receiver and any
// selector already bound to an environment each contribute their footprint only
// once.
func (exec *Execution) evalIndexSelectors(e *IndexExpr, obj Value, env *Env) ([]Value, error) {
	indices := make([]Value, 0, len(e.Indices))
	// Reuse a single charge buffer (receiver followed by the live selectors) across
	// iterations so a multi-selector read allocates it at most once, and skip it
	// entirely when no quota is enforced to keep the common indexed read
	// allocation-free.
	var charge []Value
	if exec.memoryQuota > 0 {
		charge = make([]Value, 1, len(e.Indices)+1)
		charge[0] = obj
	}
	for _, expr := range e.Indices {
		idx, err := exec.evalIndexSelector(e, obj, expr, env)
		if err != nil {
			return nil, err
		}
		indices = append(indices, idx)
		if exec.memoryQuota > 0 {
			charge = append(charge[:1], indices...)
			if err := exec.checkMemoryWith(charge...); err != nil {
				return nil, err
			}
		}
	}
	return indices, nil
}

// evalIndexValue performs a bracket read against an already-evaluated receiver
// and selectors. Arrays and strings support Ruby's single-index (with negative
// indexing), start/length, and range forms; an out-of-range single index yields
// nil rather than raising, matching Array#[] and String#[]. Hashes and objects
// take exactly one key.
// stringIndexSelectorScans reports whether these selectors are ones indexString
// will use to walk the receiver. A range or integer offsets scan; anything else
// is rejected on shape or type without reading a byte.
func stringIndexSelectorScans(indices []Value) bool {
	// Convertibility, not kind: valueToInt rejects a big integer and a
	// non-finite or out-of-range float, so s[2 ** 100] never reads the
	// receiver and must not be charged as though it had.
	usable := func(v Value) bool {
		_, err := valueToInt(v)
		return err == nil
	}
	switch len(indices) {
	case 1:
		return indices[0].Kind() == KindRange || usable(indices[0])
	case 2:
		return usable(indices[0]) && usable(indices[1])
	default:
		return false
	}
}

func (exec *Execution) evalIndexValue(e *IndexExpr, obj Value, indices []Value) (Value, error) {
	switch obj.Kind() {
	case KindString:
		// Index syntax does not go through member dispatch, so it never took the
		// string call charge. Indexing is by rune, which means walking the
		// receiver to find the offset -- s[0] on a 512 KiB string cost 362us and
		// was bounded by nothing.
		//
		// Charged only once the selector is one indexString will act on. A
		// malformed one -- s[nil], s[0, 1, 2] -- is rejected without touching
		// the receiver, and charging first replaced that error with a quota
		// error on a large string.
		if stringIndexSelectorScans(indices) {
			if err := exec.chargeStringScan(len(obj.String())); err != nil {
				return NewNil(), exec.wrapError(err, e.Position)
			}
		}
		return exec.indexString(e, obj, indices)
	case KindArray:
		return exec.indexArray(e, obj, indices)
	case KindHash, KindObject:
		return exec.indexHash(e, obj, indices)
	case KindInstance:
		// obj[i, ...] dispatches to a user-defined [] method, mirroring Ruby's
		// index-read protocol for collection-like classes.
		fn, ok := instanceOperatorMethod(obj, "[]")
		if !ok {
			return NewNil(), exec.errorAt(e.Object.Pos(), "cannot index %s: %s does not define []", obj.Kind(), valueInstance(obj).Class.Name)
		}
		if fn.Private {
			return NewNil(), exec.errorAt(e.Position, "private method []")
		}
		if fn.Protected && !exec.protectedInstanceAccessAllowed(valueInstance(obj).Class) {
			return NewNil(), exec.errorAt(e.Position, "protected method []")
		}
		return exec.callOperatorFunction(fn, obj, indices, e.Position)
	default:
		return NewNil(), exec.errorAt(e.Object.Pos(), "cannot index %s", obj.Kind())
	}
}

// indexArray implements arr[...] reads. The single-selector form mirrors
// Array#[]: a single integer (negative counts from the end) returns the element
// or nil when out of range, while a range returns a fresh subarray or nil when
// the begin bound falls outside the array. The two-selector form is Array#[]
// start/length, returning a fresh subarray or nil.
func (exec *Execution) indexArray(e *IndexExpr, receiver Value, indices []Value) (Value, error) {
	arr := receiver.Array()
	switch len(indices) {
	case 1:
		if indices[0].Kind() == KindRange {
			window, ok := arraySliceRangeWindow(len(arr), indices[0].Range())
			if !ok {
				return NewNil(), nil
			}
			if err := exec.reserveArraySliceSlots(receiver, indices, window.length); err != nil {
				return NewNil(), err
			}
			return NewArray(copyArraySliceWindow(arr, window)), nil
		}
		index, err := exec.indexSelectorToInt(e, indices[0], 0)
		if err != nil {
			return NewNil(), err
		}
		return arrayElementAt(arr, index), nil
	case 2:
		start, err := exec.indexSelectorToInt(e, indices[0], 0)
		if err != nil {
			return NewNil(), err
		}
		length, err := exec.indexSelectorToInt(e, indices[1], 1)
		if err != nil {
			return NewNil(), err
		}
		window, ok := arraySliceStartLengthWindow(len(arr), start, length)
		if !ok {
			return NewNil(), nil
		}
		if err := exec.reserveArraySliceSlots(receiver, indices, window.length); err != nil {
			return NewNil(), err
		}
		return NewArray(copyArraySliceWindow(arr, window)), nil
	default:
		return NewNil(), exec.errorAt(e.Position, "array index expects one index, a start and length, or a range")
	}
}

func (exec *Execution) reserveArraySliceSlots(receiver Value, indices []Value, slotCount int) error {
	return exec.checkSlotReservationWithCallRoots(slotCount, receiver, indices, nil, NewNil())
}

// indexString implements str[...] reads as rune (character) operations. The
// single-selector form mirrors String#[]: a single integer (negative counts
// from the end) returns the one-character substring or nil when out of range,
// while a range returns a substring or nil. The two-selector form is String#[]
// start/length.
//
// Every form selects a slice of the receiver, so the result is detached from
// the receiver's backing before it becomes a value. Retaining `(big + "x")[0]`
// was charged about one byte while pinning the whole backing: 200 such reads
// held 192 MiB under an 8 MiB quota (#36). The single-index form is a
// regression rather than an oversight -- it used to convert to []rune and build
// a fresh one-character string, which copied by construction.
//
// The copy takes no charge of its own: evalIndexValue already billed the
// receiver's length, and a slice of the receiver can never exceed it.
func (exec *Execution) indexString(e *IndexExpr, receiver Value, indices []Value) (Value, error) {
	text := receiver.String()
	window, ok, err := exec.stringIndexWindow(e, text, indices)
	if err != nil || !ok {
		return NewNil(), err
	}
	detached, err := detachedWindow(exec, text, window, receiver, indices, nil, NewNil())
	if err != nil {
		return NewNil(), err
	}
	return NewString(detached), nil
}

// stringIndexWindow resolves a str[...] read's selectors against text and
// returns the substring they select. ok is false for the out-of-range
// selections that yield nil.
func (exec *Execution) stringIndexWindow(e *IndexExpr, text string, indices []Value) (stringWindow, bool, error) {
	switch len(indices) {
	case 1:
		if indices[0].Kind() == KindRange {
			window, ok := stringRuneRangeSlice(text, indices[0].Range())
			return window, ok, nil
		}
		index, err := exec.indexSelectorToInt(e, indices[0], 0)
		if err != nil {
			return stringWindow{}, false, err
		}
		window, ok := stringSliceCharAt(text, index)
		return window, ok, nil
	case 2:
		start, err := exec.indexSelectorToInt(e, indices[0], 0)
		if err != nil {
			return stringWindow{}, false, err
		}
		length, err := exec.indexSelectorToInt(e, indices[1], 1)
		if err != nil {
			return stringWindow{}, false, err
		}
		window, ok := stringRuneSlice(text, start, length)
		return window, ok, nil
	default:
		return stringWindow{}, false, exec.errorAt(e.Position, "string index expects one index, a start and length, or a range")
	}
}

// indexHash implements hash[key] and object[key] reads. Hashes and objects take
// exactly one key; supplying multiple selectors is rejected because neither type
// has Ruby slicing semantics.
func (exec *Execution) indexHash(e *IndexExpr, obj Value, indices []Value) (Value, error) {
	if len(indices) != 1 {
		return NewNil(), exec.errorAt(e.Position, "%s index expects a single key", obj.Kind())
	}
	idx := indices[0]
	// MatchData keeps integer capture indexes; resolve those before the
	// string/symbol key check so m[0] is not reported as an unsupported key.
	if obj.Kind() == KindObject {
		if val, handled, err := matchDataIndex(obj, idx); handled || err != nil {
			if err != nil {
				return NewNil(), exec.errorAt(e.IndexPos(0), "%s", err.Error())
			}
			return val, nil
		}
	}
	// Reject unsupported keys before metering. chargeValueKeySteps still
	// models the old arbitrary-key walk, so a large array key can exhaust
	// the step quota instead of naming to_s.
	if _, err := hashKeyString(idx); err != nil {
		return NewNil(), exec.errorAt(e.IndexPos(0), "%s", err.Error())
	}
	// The lookup hashes the key's whole text; charge before it so key-heavy
	// loops stay inside the step quota.
	if err := exec.chargeValueKeySteps(idx); err != nil {
		return NewNil(), err
	}
	val, ok, err := hashGet(obj, idx)
	if err != nil {
		return NewNil(), exec.errorAt(e.IndexPos(0), "%s", err.Error())
	}
	if ok {
		return val, nil
	}
	// A missing key reads as nil; hash.fetch supplies an explicit fallback.
	return NewNil(), nil
}

// indexSelectorToInt converts the selector at position i to an integer index,
// reporting the selector's own source position on failure.
func (exec *Execution) indexSelectorToInt(e *IndexExpr, idx Value, i int) (int, error) {
	switch idx.Kind() {
	case KindInt, KindFloat:
		n, err := valueToInt(idx)
		if err != nil {
			return 0, exec.errorAt(e.IndexPos(i), "%s", err.Error())
		}
		return n, nil
	default:
		return 0, exec.errorAt(e.IndexPos(i), "index must be integer")
	}
}

// finishMemberExpr resolves the member of an already-evaluated receiver, which
// is the tail both the ordinary member path and the addressable-receiver path a
// parenless mutator takes share.
func (exec *Execution) finishMemberExpr(e *MemberExpr, obj Value, env *Env, autoCall bool) (Value, error) {
	if e.Safe && obj.Kind() == KindNil {
		return NewNil(), nil
	}
	if err := exec.checkMemoryValue(obj); err != nil {
		return NewNil(), err
	}
	member, err := exec.getPublicMember(obj, e.Property, e.Pos())
	if err != nil {
		return NewNil(), err
	}
	if autoCall {
		return exec.autoInvokeIfNeeded(e, member, obj)
	}
	return member, nil
}

func (exec *Execution) evalBinaryExpr(expr *BinaryExpr, env *Env) (Value, error) {
	var (
		left Value
		err  error
	)
	if expr.Operator == tokenShovel {
		// The shovel writes through its left operand, so it records the path
		// that owns that operand the way a mutating member call does. Isolation
		// waits until the append, after the right operand has run.
		defer exec.restore(exec.savedAddressedScope())
		var addressed bool
		left, addressed, err = exec.addressMutableReceiver(expr.Left, env)
		if !addressed {
			// See evalMemberCallExpr: an ordinary evaluation must not
			// inherit the enclosing permission.
			exec.withdrawAddressed()
			left, err = exec.evalExpression(expr.Left, env)
		}
	} else {
		left, err = exec.evalExpression(expr.Left, env)
	}
	if err != nil {
		return NewNil(), err
	}
	if err := exec.checkMemoryValue(left); err != nil {
		return NewNil(), err
	}
	switch expr.Operator {
	case tokenAnd:
		// Short-circuit and yield the operand value, not a coerced bool
		// (Ruby semantics): `a && b` is `a ? b : a`. A falsy left operand is
		// the result; otherwise the right operand is, whatever its value.
		if !left.Truthy() {
			return left, nil
		}
		right, err := exec.evalExpression(expr.Right, env)
		if err != nil {
			return NewNil(), err
		}
		if err := exec.checkMemoryWith(left, right); err != nil {
			return NewNil(), err
		}
		return right, nil
	case tokenOr:
		// Short-circuit and yield the operand value, not a coerced bool
		// (Ruby semantics): `a || b` is `a ? a : b`. This is what makes the
		// `value = optional || default` idiom work; previously it collapsed
		// to `true`/`false`.
		if left.Truthy() {
			return left, nil
		}
		right, err := exec.evalExpression(expr.Right, env)
		if err != nil {
			return NewNil(), err
		}
		if err := exec.checkMemoryWith(left, right); err != nil {
			return NewNil(), err
		}
		return right, nil
	}

	right, err := exec.evalExpression(expr.Right, env)
	if err != nil {
		return NewNil(), err
	}
	if err := exec.checkMemoryWith(left, right); err != nil {
		return NewNil(), err
	}

	result, err := exec.evalBinaryOperator(expr.Operator, left, right, expr.Pos())
	if err != nil {
		return NewNil(), err
	}
	return result, nil
}

// chargeStringOperandBytes charges the step quota for an operator that copies or
// scans its string operands.
//
// Operators never reach the string member wrapper, so + copied a whole
// host-supplied string per evaluation and the comparisons scanned one, both for
// the flat evaluator cost. Discarding the result keeps the memory quota out of
// it too, so a loop over `s + "x"` was bounded by nothing.
//
// Concatenation copies both operands. A comparison stops at the first differing
// byte and answers immediately when the lengths differ, so it cannot read more
// than the shorter operand holds.
// chargeRegexSourceWalk charges the source traversal Regex.StringLen performs.
// Called only once the operation is known to be one addValues will carry out, so
// an unsupported pairing reports its own error rather than exhausting the quota
// on a walk that never happens.
func (exec *Execution) chargeRegexSourceWalk(left, right Value) error {
	for _, v := range [2]Value{left, right} {
		if v.Kind() != KindRegex {
			continue
		}
		if err := exec.chargeStringScan(len(v.Regex().Source)); err != nil {
			return err
		}
	}
	return nil
}

// concatenatedOperandBytes bounds what an operand contributes to a
// concatenation, which is the size of its rendered form rather than of the
// operand itself.
//
// A string or symbol renders as its own bytes. A big integer renders through a
// base conversion whose output grows with the payload, and a regex renders its
// source, so both carry a payload the operand's kind alone does not reveal --
// billing only strings left `"" + big` and `"" + /re/` copying and converting
// for nothing. Every other concatenable kind renders to a bounded handful of
// bytes and contributes nothing worth charging.
func concatenatedOperandBytes(v Value) int {
	switch {
	case stringLikeOperand(v):
		return len(v.String())
	case v.Kind() == KindInt:
		if _, ok := value.BigIntPayload(v); ok {
			return value.BigIntDecimalLenUpperBound(v)
		}
		return 0
	case v.Kind() == KindRegex:
		// Computed from the source rather than measured by rendering it.
		// Regex.String escapes its source, so len(v.String()) would build the
		// whole literal just to size it -- and addValues then builds it again,
		// two renderings billed as one, with the first beyond the quota's reach.
		// StringLen walks the source and allocates nothing.
		return v.Regex().StringLen()
	default:
		// An enum value renders as Enum::Member from two identifiers that can
		// approach the source-size limit, so it carries a payload its kind does
		// not reveal.
		if n, ok := runtimeValueStringLen(v); ok {
			return n
		}
		return 0
	}
}

// concatenatesToString reports whether addValues will join these operands into
// a string, which is the only case where + copies a payload.
func concatenatesToString(left, right Value) bool {
	if left.Kind() == KindString {
		return concatenableWithString(right)
	}
	if right.Kind() == KindString {
		return concatenableWithString(left)
	}
	return false
}

func stringLikeOperand(v Value) bool {
	return v.Kind() == KindString || v.Kind() == KindSymbol
}

func (exec *Execution) chargeStringOperandBytes(operator TokenType, left, right Value) error {
	switch operator {
	case tokenPlus:
		// Concatenation copies whatever it is given, and addValues concatenates
		// whenever one side is a string and the other renders into one -- s + 1
		// and "" + s.to_sym both copy a large operand. The kinds need not match
		// here; requiring it left those unmetered.
		//
		// One side must actually be a string and the other must be something
		// addValues will join to it. Two symbols, or a string with nil, an
		// array or a hash, are unsupported-operands errors that read neither
		// payload, so charging them turned a constant-time failure into a quota
		// failure.
		if !concatenatesToString(left, right) {
			return nil
		}
		// Charged only once the operation is one addValues will perform, and
		// before StringLen walks anything: a regex plus nil or another regex
		// renders neither source, and charging first replaced that
		// unsupported-operands error with a quota error.
		if err := exec.chargeRegexSourceWalk(left, right); err != nil {
			return err
		}
		return exec.chargeStringScan(saturatingAdd(
			concatenatedOperandBytes(left), concatenatedOperandBytes(right),
		))
	case tokenLT, tokenLTE, tokenGT, tokenGTE, tokenSpaceship:
		// Comparison needs matching kinds: a string against a symbol is rejected
		// on kind before either name is read, and ordering calls the pair
		// incomparable, so charging it billed a constant-time answer. The
		// equality operators are absent here: their charge lands at the
		// scalar leaf inside the equality walk (see equalValues), which bills
		// nested payloads the same way and must not double-bill the top level.
		if left.Kind() != right.Kind() || !stringLikeOperand(left) {
			return nil
		}
		// Ordering reads the common prefix whatever the lengths.
		return exec.chargeStringScan(min(len(left.String()), len(right.String())))
	default:
		return nil
	}
}

func (exec *Execution) evalBinaryOperator(operator TokenType, left, right Value, pos Position) (Value, error) {
	if left.Kind() == KindInstance {
		if result, handled, err := exec.evalInstanceOperator(operator, left, right, pos); handled {
			return result, err
		}
	}
	if err := exec.chargeStringOperandBytes(operator, left, right); err != nil {
		return NewNil(), exec.wrapError(err, pos)
	}
	var result Value
	var err error
	// Big-operand arithmetic charges steps proportional to operand size, and
	// multiplication/exponentiation preflight their projected result against
	// the memory quota before computing (see checkBigIntOperationGuards /
	// checkIntPowerGuards). Compact operands skip everything behind
	// EitherIntPayload's two nil compares, keeping the hot path branch-cheap.
	switch operator {
	case tokenPlus:
		if left.Kind() == KindInt && right.Kind() == KindInt && eitherIntPayload(left, right) {
			if err := exec.checkBigIntOperationGuards(operator, left, right); err != nil {
				return NewNil(), exec.wrapError(err, pos)
			}
		}
		result, err = addValues(left, right)
	case tokenMinus:
		if left.Kind() == KindInt && right.Kind() == KindInt && eitherIntPayload(left, right) {
			if err := exec.checkBigIntOperationGuards(operator, left, right); err != nil {
				return NewNil(), exec.wrapError(err, pos)
			}
		}
		result, err = subtractValues(exec, left, right)
	case tokenAsterisk:
		if left.Kind() == KindString {
			// String repetition allocates a script-sized result, so it needs
			// the execution's memory quota and cannot live in multiplyValues.
			result, err = exec.repeatStringValue(left, right)
			break
		}
		if left.Kind() == KindInt && right.Kind() == KindInt && eitherIntPayload(left, right) {
			if err := exec.checkBigIntOperationGuards(operator, left, right); err != nil {
				return NewNil(), exec.wrapError(err, pos)
			}
		}
		result, err = multiplyValues(left, right)
	case tokenPower:
		if left.Kind() == KindInt && right.Kind() == KindInt {
			if eitherIntPayload(left, right) {
				if err := exec.checkBigIntOperationGuards(operator, left, right); err != nil {
					return NewNil(), exec.wrapError(err, pos)
				}
			}
			if err := exec.checkIntPowerGuards(left, right); err != nil {
				return NewNil(), exec.wrapError(err, pos)
			}
		}
		result, err = powerValues(left, right)
	case tokenSlash:
		if left.Kind() == KindInt && right.Kind() == KindInt && eitherIntPayload(left, right) {
			if err := exec.checkBigIntOperationGuards(operator, left, right); err != nil {
				return NewNil(), exec.wrapError(err, pos)
			}
		}
		result, err = divideValues(left, right)
	case tokenPercent:
		if left.Kind() == KindString {
			values := []Value{right}
			if right.Kind() == KindArray {
				values = right.Array()
			}
			result, err = exec.formatStringValues(left.String(), values, left, []Value{right}, nil, NewNil())
		} else {
			if left.Kind() == KindInt && right.Kind() == KindInt && eitherIntPayload(left, right) {
				if err := exec.checkBigIntOperationGuards(operator, left, right); err != nil {
					return NewNil(), exec.wrapError(err, pos)
				}
			}
			result, err = moduloValues(left, right)
		}
	case tokenShovel:
		// The array shovel appends to the receiver in place. The charged
		// append commits the element into the base-walk memo and skips the
		// epoch bump, keeping loop-grown arrays linear under the quota
		// (#1129); when it is not eligible, charge the backing reallocation
		// up front and take the ordinary epoch-bumping path.
		result, err = exec.shovelArray(left, right)
	case tokenAmpersand:
		result, err = intersectValues(exec, left, right)
	case tokenEQ:
		eq, eqErr := exec.equalValues(left, right)
		if eqErr != nil {
			return NewNil(), exec.wrapError(eqErr, pos)
		}
		return NewBool(eq), nil
	case tokenCaseEQ:
		// Ruby's case equality operator: the left operand acts as the matcher and
		// the right operand is the value being tested. Ranges check membership;
		// every other value falls back to `==`. This mirrors `when` clause
		// matching, where the clause value is the matcher.
		matched, err := caseCandidateMatches(exec, right, left)
		if err != nil {
			return NewNil(), exec.wrapError(err, pos)
		}
		return NewBool(matched), nil
	case tokenNotEQ:
		eq, eqErr := exec.equalValues(left, right)
		if eqErr != nil {
			return NewNil(), exec.wrapError(eqErr, pos)
		}
		return NewBool(!eq), nil
	case tokenMatch, tokenNotMatch:
		return exec.evalRegexMatchOperator(operator, left, right, pos)
	// The relational operators assign rather than return so an incomparable
	// pair falls through to the wrap below, like every other operator.
	// Returning directly left the error unpositioned and outside rescue's
	// reach, because a bare rescue requires errors.As(err, &RuntimeError).
	case tokenLT:
		result, err = compareValues(left, right, func(c int) bool { return c < 0 })
	case tokenLTE:
		result, err = compareValues(left, right, func(c int) bool { return c <= 0 })
	case tokenGT:
		result, err = compareValues(left, right, func(c int) bool { return c > 0 })
	case tokenGTE:
		result, err = compareValues(left, right, func(c int) bool { return c >= 0 })
	case tokenSpaceship:
		// compareSpaceshipOrder rather than compareValueOrder: arrays compare
		// lexicographically under <=> but stay rejected by the relational
		// operators above, as in Ruby, where Array defines <=> but does not
		// include Comparable.
		order, ordered, err := compareSpaceshipOrder(exec, left, right)
		if err != nil {
			// Incomparable operand pairs (different kinds, or money in different
			// currencies) make the spaceship operator return nil rather than
			// raising, matching Ruby's `1 <=> "a"`. Genuine errors still surface.
			if isIncomparable(err) {
				return NewNil(), nil
			}
			return NewNil(), exec.wrapError(err, pos)
		}
		// Unordered operands (a NaN on either side) make the spaceship operator
		// return nil, matching Ruby's `(0.0 / 0.0) <=> 1.0`.
		if !ordered {
			return NewNil(), nil
		}
		return NewInt(int64(order)), nil
	default:
		return NewNil(), exec.errorAt(pos, "unsupported operator")
	}

	if err != nil {
		// The relational operators raise on incomparable operands, and
		// docs/language_reference.md specifies that as Ruby's ArgumentError.
		// The sentinel carries no type, so the shared wrap classified it as
		// the base RuntimeError and `rescue ArgumentError` could not catch it.
		if isIncomparable(err) && isRelationalOperator(operator) {
			return NewNil(), exec.argumentErrorAt(pos, "%s", err.Error())
		}
		return NewNil(), exec.wrapError(err, pos)
	}
	return result, nil
}

// isRelationalOperator reports whether op is one of the ordering comparisons
// that raise on incomparable operands, as opposed to <=>, which answers nil.
func isRelationalOperator(op TokenType) bool {
	switch op {
	case tokenLT, tokenLTE, tokenGT, tokenGTE:
		return true
	}
	return false
}

// instanceOperatorMethod resolves a user-defined operator method (def +,
// def ==, def []) on an instance receiver's class.
func instanceOperatorMethod(receiver Value, name string) (*ScriptFunction, bool) {
	if receiver.Kind() != KindInstance {
		return nil, false
	}
	fn, ok := valueInstance(receiver).Class.Methods[name]
	return fn, ok
}

// evalInstanceOperator dispatches a binary operator on an instance receiver to
// the class's operator method of the same name, mirroring Ruby where `a + b`
// is `a.+(b)`. Dispatch keys on the left operand only, like Ruby. == and !=
// keep their universal fallbacks when the class defines no method: != prefers
// a user !=, then negates a user ==, then falls back to built-in equality, so
// defining == alone gives a consistent inverse.
func (exec *Execution) evalInstanceOperator(operator TokenType, left, right Value, pos Position) (Value, bool, error) {
	switch operator {
	case tokenPlus, tokenMinus, tokenAsterisk, tokenSlash, tokenPercent, tokenPower,
		tokenShovel, tokenAmpersand, tokenLT, tokenLTE, tokenGT, tokenGTE, tokenSpaceship:
		fn, ok := instanceOperatorMethod(left, operator.String())
		if !ok {
			return NewNil(), false, nil
		}
		val, err := exec.callInstanceOperatorMethod(fn, operator.String(), left, right, pos)
		return val, true, err
	case tokenEQ:
		fn, ok := instanceOperatorMethod(left, "==")
		if !ok {
			return NewNil(), false, nil
		}
		val, err := exec.callInstanceOperatorMethod(fn, "==", left, right, pos)
		return val, true, err
	case tokenNotEQ:
		if fn, ok := instanceOperatorMethod(left, "!="); ok {
			val, err := exec.callInstanceOperatorMethod(fn, "!=", left, right, pos)
			return val, true, err
		}
		if fn, ok := instanceOperatorMethod(left, "=="); ok {
			val, err := exec.callInstanceOperatorMethod(fn, "==", left, right, pos)
			if err != nil {
				return NewNil(), true, err
			}
			return NewBool(!val.Truthy()), true, nil
		}
		return NewNil(), false, nil
	default:
		return NewNil(), false, nil
	}
}

func (exec *Execution) callInstanceOperatorMethod(fn *ScriptFunction, name string, receiver, arg Value, pos Position) (Value, error) {
	if fn.Private {
		return NewNil(), exec.errorAt(pos, "private method %s", name)
	}
	if fn.Protected && !exec.protectedInstanceAccessAllowed(valueInstance(receiver).Class) {
		return NewNil(), exec.errorAt(pos, "protected method %s", name)
	}
	return exec.callOperatorFunction(fn, receiver, []Value{arg}, pos)
}

// callOperatorFunction invokes an operator or index method and, like the
// normal call wrapper, converts a bare break/next escaping the method body
// into a call-boundary error. Without this, an operator evaluated inside a
// caller loop would let the signal silently break or continue that loop.
func (exec *Execution) callOperatorFunction(fn *ScriptFunction, receiver Value, args []Value, pos Position) (Value, error) {
	val, err := exec.callFunction(fn, receiver, args, nil, NewNil(), pos)
	if err != nil {
		if errors.Is(err, errLoopBreak) {
			return NewNil(), exec.localJumpErrorAt(pos, "break cannot cross call boundary")
		}
		if errors.Is(err, errLoopNext) {
			return NewNil(), exec.localJumpErrorAt(pos, "next cannot cross call boundary")
		}
	}
	return val, err
}

func (exec *Execution) evalConditionalExpr(expr *ConditionalExpr, env *Env) (Value, error) {
	return exec.evalConditionalExprWithExpectation(expr, env, expressionExpectation{})
}

func (exec *Execution) evalConditionalExprWithExpectation(expr *ConditionalExpr, env *Env, expectation expressionExpectation) (Value, error) {
	condition, err := exec.evalExpression(expr.Condition, env)
	if err != nil {
		return NewNil(), err
	}
	if err := exec.checkMemoryValue(condition); err != nil {
		return NewNil(), err
	}

	branch := expr.Alternate
	if condition.Truthy() {
		branch = expr.Consequent
	}
	result, err := exec.evalExpressionWithExpectation(branch, env, expectation)
	if err != nil {
		return NewNil(), err
	}
	if err := exec.checkMemoryValue(result); err != nil {
		return NewNil(), err
	}
	return result, nil
}

func (exec *Execution) evalRescueExpr(expr *RescueExpr, env *Env, autoCall bool) (Value, error) {
	for {
		result, err := exec.evalExpressionWithAuto(expr.Body, env, autoCall)
		if err == nil {
			if err := exec.checkMemoryValue(result); err != nil {
				return NewNil(), err
			}
			return result, nil
		}
		if !exec.canRescueRuntimeError(err, nil) {
			return NewNil(), err
		}

		fallback, fallbackErr := exec.evalRescueExprFallback(expr.Fallback, err, env, autoCall)
		if isRescueRetrySignal(fallbackErr) {
			// A retry from the fallback re-runs the rescued expression; charge
			// a step per attempt so a retry storm hits the step quota.
			if stepErr := exec.step(); stepErr != nil {
				return NewNil(), exec.wrapError(stepErr, expr.Pos())
			}
			continue
		}
		if fallbackErr != nil {
			return NewNil(), fallbackErr
		}
		if err := exec.checkMemoryValue(fallback); err != nil {
			return NewNil(), err
		}
		return fallback, nil
	}
}

func (exec *Execution) evalRescueExprFallback(expr Expression, err error, env *Env, autoCall bool) (Value, error) {
	exec.pushRescuedError(err)
	exec.rescueDepth++
	fallback, fallbackErr := exec.evalExpressionWithAuto(expr, env, autoCall)
	exec.rescueDepth--
	exec.popRescuedError()
	return fallback, fallbackErr
}

func (exec *Execution) evalIfExpr(expr *IfExpr, env *Env) (Value, error) {
	return exec.evalIfExprWithExpectation(expr, env, expressionExpectation{})
}

func (exec *Execution) evalIfExprWithExpectation(expr *IfExpr, env *Env, expectation expressionExpectation) (Value, error) {
	resultExpr, err := exec.matchIfExpressionBranch(expr, env)
	if err != nil {
		return NewNil(), err
	}
	if resultExpr == nil {
		return NewNil(), nil
	}

	result, err := exec.evalExpressionWithExpectation(resultExpr, env, expectation)
	if err != nil {
		return NewNil(), err
	}
	if err := exec.checkMemoryValue(result); err != nil {
		return NewNil(), err
	}
	return result, nil
}

func (exec *Execution) matchIfExpressionBranch(expr *IfExpr, env *Env) (Expression, error) {
	condition, err := exec.evalExpression(expr.Condition, env)
	if err != nil {
		return nil, err
	}
	if err := exec.checkMemoryValue(condition); err != nil {
		return nil, err
	}
	if condition.Truthy() {
		return expr.Consequent, nil
	}

	for _, branch := range expr.ElseIf {
		condition, err := exec.evalExpression(branch.Condition, env)
		if err != nil {
			return nil, err
		}
		if err := exec.checkMemoryValue(condition); err != nil {
			return nil, err
		}
		if condition.Truthy() {
			return branch.Result, nil
		}
	}

	return expr.Alternate, nil
}

func (exec *Execution) evalBlockLiteral(block *BlockLiteral, env *Env) (Value, error) {
	blockValue := newBlock(block.Params, block.ImplicitParams, block.Body, env)
	blk := valueBlock(blockValue)
	blk.homeReturnToken = exec.currentBlockHomeToken()
	if ctx := exec.currentModuleContext(); ctx != nil && ctx.script != nil {
		blk.owner = ctx.script
	} else {
		blk.owner = exec.script
	}
	if ctx := exec.currentModuleContext(); ctx != nil {
		blk.moduleKey = ctx.key
		blk.modulePath = ctx.path
		blk.moduleRoot = ctx.root
	}
	return blockValue, nil
}

func ensureBlock(block Value, name string) error {
	if valueBlock(block) == nil {
		if name != "" {
			return fmt.Errorf("%s requires a block", name)
		}
		return fmt.Errorf("block required")
	}
	return nil
}

type blockCallRunner struct {
	exec          *Execution
	blk           *Block
	env           *Env
	charge        *blockBindCharge
	nextContinues bool

	// The bind charge's baseline is a snapshot of the reachable graph taken when
	// the charge was built, and a reused runner keeps it for the whole loop. That
	// is sound only while the graph it measured has not moved: liveBaseline tracks
	// the driver's own scratch and retained output, but nothing tracks a container
	// an earlier callback grew. A driver whose callbacks can mutate opts in with
	// refreshChargeOnMutation and these carry what a rebuild needs.
	refreshCharge  bool
	chargeEpoch    uint64
	chargeReceiver Value
	chargeArgs     []Value
	chargeKwargs   map[string]Value
	chargeBlock    Value
}

// refreshChargeOnMutation makes the runner rebuild its bind charge whenever
// script code has mutated the reachable graph since the charge was built, so a
// rest-window preflight is never weighed against a baseline that predates a
// callback's growth. Without it, a callback that grew a reachable container and a
// later key destructured with a named rest let a 16,000,024-byte window
// materialize under a quota that could not hold it, and only the block body's own
// check caught it afterwards.
//
// It is opt-in rather than the default because the rebuild costs a graph walk and
// only a driver that both reuses one runner across callbacks and registers its
// output as a walk root pays for that walk (see newBlockBindCharge). The
// mutation epoch is the key: a callback that mutates nothing rebuilds nothing, so
// the reuse this exists to protect is untouched for every loop that does not need
// it.
func (runner *blockCallRunner) refreshChargeOnMutation() {
	if runner == nil || runner.charge == nil {
		return
	}
	runner.refreshCharge = true
	runner.chargeEpoch = value.MutationEpoch()
}

// refreshChargeIfGraphMoved rebuilds the bind charge when the mutation epoch has
// moved since it was built. The epoch is exactly the signal wanted here: every
// script-visible write to a reachable container bumps it, and a raw builtin write
// that does not is not script code growing the graph between two callbacks.
func (runner *blockCallRunner) refreshChargeIfGraphMoved() {
	if !runner.refreshCharge {
		return
	}
	epoch := value.MutationEpoch()
	if epoch == runner.chargeEpoch {
		return
	}
	runner.chargeEpoch = epoch
	// The rebuild's own construction walk is not billed. Building the FIRST
	// charge is this execution's own doing and is charged where it happens, but a
	// rebuild is triggered by the epoch having moved, and the epoch is
	// process-wide: an unrelated execution's mutation forces this rebuild exactly
	// as this script's own does (see memory_output.go).
	billed := runner.exec.outputWalkNodes
	defer func() { runner.exec.outputWalkNodes = billed }()
	if rebuilt := newBlockBindCharge(runner.exec, runner.blk, runner.chargeReceiver,
		runner.chargeArgs, runner.chargeKwargs, runner.chargeBlock); rebuilt != nil {
		runner.charge = rebuilt
	}
}

// newBlockCallRunner builds a runner for repeatedly invoking a block from an
// iterator. receiver, callArgs, and kwargs are the iterator's call roots: they seed
// the per-call bind charge that bounds the fresh backing a rest-collecting
// destructure parameter (|(k, *tail)|) allocates, so those roots -- live only on the
// Go stack during the loop, invisible to estimateMemoryUsageBase -- are counted
// alongside that backing. The charge snapshots them ONCE (no per-entry re-walk), so
// the loop stays O(total data). callArgs are the iterator's POSITIONAL roots (the
// other hashes a block-driven hash.merge holds, a grep pattern); pass nil for the
// pure iterators that reject positional arguments and so hold only the receiver.
func newBlockCallRunner(exec *Execution, block Value, name string, receiver Value, callArgs []Value, kwargs map[string]Value) (*blockCallRunner, error) {
	if err := ensureBlock(block, name); err != nil {
		return nil, err
	}
	blk := valueBlock(block)
	runner := &blockCallRunner{
		exec:           exec,
		blk:            blk,
		charge:         newBlockBindCharge(exec, blk, receiver, callArgs, kwargs, block),
		chargeReceiver: receiver,
		chargeArgs:     callArgs,
		chargeKwargs:   kwargs,
		chargeBlock:    block,
	}
	// Pay for the construction walk the charge above may have recorded, before
	// this runner is handed back. Waiting for callBlock is not enough: a driver
	// can build a runner and never invoke its block -- array.each constructs one
	// before it discovers its receiver is empty -- and then no callback ever
	// arrives to settle the charge, leaving it pending while the enclosing loop
	// runs on. Measured on a lookup whose first miss ran an empty nested each, the
	// enclosing fetch_values went on to process 50,000 present keys against an
	// exhausted quota, because present keys invoke nothing and can cost no steps.
	if err := exec.chargeRetainedOutputWalk(); err != nil {
		return nil, err
	}
	if blockCanReuseEnv(blk) {
		runner.env = newBlockAssignmentEnv(blk.Env)
	}
	return runner, nil
}

func (runner *blockCallRunner) call(args []Value) (Value, error) {
	return runner.callWithChargedRoots(args)
}

// callWithChargedRoots invokes the block, additionally charging per-call roots that
// live only in the iterator's Go frame and evolve every call -- the reduce
// accumulator, whose current payload (the seed on the first call, the previous
// call's result thereafter) is not in the runner's one-time baseline. The accumulator
// is probed against the snapshotted call roots, so a no-seed accumulator that is the
// receiver's first element deduplicates against the receiver and is charged only its
// structural slots, never a second copy of the receiver's data.
func (runner *blockCallRunner) callWithChargedRoots(args []Value, chargedRoots ...Value) (Value, error) {
	if err := runner.exec.checkContext(); err != nil {
		return NewNil(), err
	}

	env := runner.env
	// Inside an active block-iteration region the block's own scope is walked
	// fresh on every memory check (see memory_blockregion.go), so its parameter
	// and local binding writes are epoch-neutral. The flag is set before
	// resetForReuse and before param binding so both this iteration's reset and
	// its parameter binds skip the epoch bump that would otherwise invalidate
	// the memoized prefix every iteration; popEnv clears it when the scope
	// leaves the stack.
	if env == nil {
		env = newBlockAssignmentEnv(runner.blk.Env)
		env.markRegionNeutral(runner.exec.blockRegionActive)
	} else {
		env.markRegionNeutral(runner.exec.blockRegionActive)
		env.resetForReuse(runner.blk.Env)
		env.assignBoundary = true
		env.rebindOuter = true
	}
	// Rebuilt here rather than at construction time when an earlier callback has
	// moved the graph the charge measured (see refreshChargeOnMutation).
	runner.refreshChargeIfGraphMoved()
	val, err := runner.exec.callBlock(runner.blk, args, env, runner.charge, Position{}, chargedRoots...)
	if err != nil {
		if errors.Is(err, errLoopNext) && !runner.nextContinues {
			if nextVal, ok := loopNextValue(err); ok {
				return nextVal, nil
			}
			return NewNil(), nil
		}
		return NewNil(), err
	}
	if err := runner.exec.checkContext(); err != nil {
		return NewNil(), err
	}
	return val, nil
}

// callRetained invokes the block and publishes the result: the caller is
// about to store it in a durable slot. Callers that discard the result
// (each, predicates) use call so an ignored collection is not marked shared.
func (runner *blockCallRunner) callRetained(args []Value) (Value, error) {
	val, err := runner.call(args)
	if err != nil {
		return val, err
	}
	publishCollection(val)
	return val, nil
}

// callRetainedWithChargedRoots is callWithChargedRoots plus publication of
// a result the caller will retain.
func (runner *blockCallRunner) callRetainedWithChargedRoots(args []Value, chargedRoots ...Value) (Value, error) {
	val, err := runner.callWithChargedRoots(args, chargedRoots...)
	if err != nil {
		return val, err
	}
	publishCollection(val)
	return val, nil
}

// blockWantsCollapsedPair reports whether a hash iterator should yield each entry
// as a single two-element [key, value] pair instead of two separate arguments. It
// mirrors Ruby, where a block declaring exactly one positional parameter receives
// the pair while a block with two or more positional parameters auto-splats into
// key and value. A lone destructuring parameter such as |(k, v)| still counts as
// one parameter, so it receives the pair and unpacks it. Any rest or keyword
// parameter opts out so the iterator keeps yielding key and value separately.
//
// It takes the block directly (rather than a runner) so a hash iterator can pick
// the yield shape -- and thus size its walk scratch -- before building the runner,
// letting the scratch be reserved before the runner snapshots its bind-charge
// baseline.
func blockWantsCollapsedPair(blk *Block) bool {
	return blockPositionalArity(blk) == 1
}

func blockPositionalArity(blk *Block) int {
	if blk == nil {
		return 0
	}
	if len(blk.Params) == 0 {
		return implicitBlockParamArity(blk.ImplicitParams)
	}
	// A lone rest parameter collects the whole yielded argument list, so it
	// receives the collapsed pair and reports arity 1: Ruby's
	// `{a: 1}.map { |*args| args }` is `[[[:a, 1]]]`, not `[[:a, 1]]`. A rest
	// alongside other parameters, such as |k, *rest| or |*a, k|, does
	// auto-splat and stays on the opt-out path below.
	//
	// The grammar has no top-level rest parameter for blocks or lambdas today
	// -- `{ |*args| }` and `->(*args) {}` are parse errors, and rest is only
	// available inside a destructuring group -- so this is unreachable as
	// written. It is here so the rule is already right if that changes;
	// TestBlockRestParametersAreNotParseable pins the current grammar.
	if len(blk.Params) == 1 && blk.Params[0].Kind == ParamRest {
		return 1
	}
	positional := 0
	for i := range blk.Params {
		switch blk.Params[i].Kind {
		case ParamNormal:
			positional++
		default:
			return 0
		}
	}
	return positional
}

func implicitBlockParamArity(params []string) int {
	arity := 0
	for _, name := range params {
		index := implicitBlockParamIndex(name)
		if index+1 > arity {
			arity = index + 1
		}
	}
	return arity
}

// CallBlock invokes a block value with the provided arguments.
// This is the public entry point for capability adapters that need to
// call user-supplied blocks (e.g. db.each, db.tx).
//
// It is a host boundary: the arguments come from Go code that may retain
// their backing, and the return value is script state the host must not
// hold live, so both directions cross as independent values (#1210) unless
// the invoking builtin declared itself non-retaining. Native builtins
// drive blocks through the internal paths and pay none of this.
func (exec *Execution) CallBlock(block Value, args []Value) (Value, error) {
	// Fail closed: with no builtin frame on the stack this is being driven
	// from outside any dispatch -- a stashed Execution, a goroutine -- and
	// that caller is host code by definition.
	if exec.builtinFrameHostCrossing || exec.builtinDepth == 0 {
		// Anything the host installed into its receiver or a host-visible
		// root before this yield must be shared before the block can read
		// it, or a block-side write lands in the host's retained backing.
		// Stable host state re-walks as uncharged map hits, so yield-heavy
		// loops pay quota only for what actually changed.
		if err := exec.markHostWritableState(exec.builtinFrameReceiver); err != nil {
			return NewNil(), err
		}
		var detachedArgs []Value
		for i, arg := range args {
			if !isCollection(arg) {
				continue
			}
			detached, err := exec.detachHostCrossingValue(arg)
			if err != nil {
				return NewNil(), err
			}
			if detachedArgs == nil {
				detachedArgs = append([]Value(nil), args...)
			}
			detachedArgs[i] = detached
		}
		if detachedArgs != nil {
			args = detachedArgs
		}
		result, err := exec.callBlockValue(block, args, Position{})
		if err != nil || !isCollection(result) {
			return result, err
		}
		// Unlike a Call result, the call is not ending here: a sole
		// published wrapper still has a live script owner, so only a graph
		// no slot names at all -- a temporary the block built -- transfers
		// whole. Anything published or callable-bearing is the host's copy,
		// charged first, since a script loop can drive this clone once per
		// iteration.
		needsIsolation, isoErr := exec.hostCollectionNeedsIsolation(result, make(map[uintptr]struct{}))
		if isoErr != nil {
			return NewNil(), isoErr
		}
		if !needsIsolation && !valueNeedsHostClone(result) {
			return result, nil
		}
		if err := exec.preflightDeepClone(result); err != nil {
			return NewNil(), err
		}
		return cloneValueForHost(result), nil
	}
	return exec.callBlockValue(block, args, Position{})
}

func (exec *Execution) callBlockValue(block Value, args []Value, pos Position) (Value, error) {
	if err := ensureBlock(block, ""); err != nil {
		return NewNil(), err
	}
	exec.recordCapabilityYield(args)
	// Script code runs next, and nothing it yields is this capability's doing.
	// A helper the block calls (`def relay(v); yield v; end`) yields through
	// this same path, and a script call does not change builtinDepth, so depth
	// alone cannot tell the two apart. Suspending for the body's duration can:
	// a capability's own yields are made before its block body starts.
	suspended := exec.capabilityYields
	exec.capabilityYields = nil
	defer func() { exec.capabilityYields = suspended }()
	blk := valueBlock(block)
	// Capability adapters drive blocks with host-supplied arguments and no
	// receiver. Those arguments live only on the Go call stack for the duration of
	// the call, so include them in the bind-charge baseline: a rest-collecting
	// destructure parameter copying part of a large argument into a fresh backing
	// would otherwise be charged that copy against a baseline that omits the
	// argument it was copied from, letting (args) and (rest) each fit the quota
	// while the real peak (args + rest) exceeds it.
	// Fold what the calling frame is still holding into the live baseline for
	// the body's duration. The bind charge does this for the values it is built
	// from, but it is built here with no receiver and only the block's own
	// arguments, so a value the caller retains and does not pass -- a fetch
	// default the block supersedes, an adapter's argument it walks itself -- was
	// invisible while the body ran. Its bytes were counted when the caller
	// evaluated it and again after, never at the same time as whatever the body
	// allocates, so the real peak of the two together escaped: a 2,000,000-byte
	// ignored default moved the smallest admitting quota by nothing at all.
	retained, ownsFrameRoots := exec.reserveCallerRetainedRoots(args)
	defer func() {
		exec.releaseLoopScratch(retained)
		if ownsFrameRoots {
			exec.builtinFrameRootsReserved = false
		}
	}()

	charge := newBlockBindCharge(exec, blk, NewNil(), args, nil, block)
	val, err := exec.callBlock(blk, args, newBlockAssignmentEnv(blk.Env), charge, pos)
	if err != nil && errors.Is(err, errLoopNext) {
		if nextVal, ok := loopNextValue(err); ok {
			return nextVal, nil
		}
		return NewNil(), nil
	}
	return val, err
}

func (exec *Execution) callBlock(blk *Block, args []Value, blockEnv *Env, charge *blockBindCharge, pos Position, chargedRoots ...Value) (Value, error) {
	// A block belongs to the call it was written on. Invoking it later would
	// run script code against a frame that has already unwound -- the shape
	// ADR-006 exists to remove -- so the violation is a hard error naming the
	// rule rather than a documented promise the runtime cannot check.
	if blk.isRetired() {
		return NewNil(), exec.errorAt(pos, "block invoked after the call it was given to returned; a block may only run while that call is on the stack")
	}
	// Pay for the bind charge's construction walk before running the callback it
	// was built for. The walk is recorded where it happens rather than charged
	// there, because that site cannot return an error, and settling it in each
	// driver's loop instead made it a convention every new driver had to
	// remember. This is the one point every block invocation passes through, so
	// settling here means a driver cannot run a callback without first paying for
	// the walk that preceded it.
	//
	// The counter is filled once, when the runner's bind charge is built, which is
	// before the first callback -- so settling after each callback still lets that
	// first one run on a quota the walk had already spent, and a driver that omits
	// the call entirely lets the whole loop run. Over a 40,000-node reachable
	// graph, a lookup whose construction walk cost 625 steps against a quota of
	// 100 ran a callback anyway; it now runs none.
	if err := exec.chargeRetainedOutputWalk(); err != nil {
		return NewNil(), err
	}
	// Script re-entry runs with full periodic memory checks: suspend any
	// accumulator-metered sections the driving builtin left active for the
	// duration of the block body (see beginAccumulatorMeteredSection).
	if exec.accumMeteredSections != 0 {
		savedSections := exec.accumMeteredSections
		exec.accumMeteredSections = 0
		defer func() { exec.accumMeteredSections = savedSections }()
	}
	exec.pushModuleContext(moduleContext{
		key:    blk.moduleKey,
		path:   blk.modulePath,
		root:   blk.moduleRoot,
		script: blk.owner,
	})
	defer exec.popModuleContext()

	if err := charge.begin(args, chargedRoots...); err != nil {
		return NewNil(), err
	}
	// Fold the charge's Go-frame-only call roots (an ephemeral receiver held
	// alive by the iterating builtin, host-supplied arguments) into the live
	// baseline for the duration of this call, so the body's per-statement and
	// mutator-growth checks bound the combined peak of those roots plus whatever
	// the body retains. The bytes were measured once at charge construction;
	// reserving them here is O(1) per call, not a re-walk (issue #835).
	if scratch := charge.ephemeralRootScratch(); scratch > 0 {
		delta := exec.reserveLoopScratch(scratch)
		// The baseline already carries these bytes, so tell the charge not to
		// count them again when it reads the live reservation.
		charge.noteSelfReservation(delta)
		defer func() {
			charge.noteSelfReservation(0)
			exec.releaseLoopScratch(delta)
		}()
	}
	bindArgs := rubyBlockBindArgs(blk.Params, args)
	for i, param := range blk.Params {
		var val Value
		if i < len(bindArgs) {
			val = bindArgs[i]
		} else {
			val = NewNil()
		}
		if param.Type != nil {
			normalized, err := normalizeValueForType(val, param.Type, typeContext{
				owner:    blk.owner,
				env:      blk.Env,
				fallback: exec.root,
				exec:     exec,
			})
			if err != nil {
				if isHostControlSignal(err) {
					return NewNil(), err
				}
				if isNormalizationLimitError(err) {
					return NewNil(), exec.wrapError(err, param.Type.Position)
				}
				return NewNil(), exec.errorAt(param.Type.Position, "%s", formatArgumentTypeMismatch(param.Name, err))
			}
			val = normalized
		}
		if param.Target != nil {
			if err := exec.bindBlockParamTarget(blockEnv, param.Target, val, charge, blk); err != nil {
				return NewNil(), err
			}
			continue
		}
		blockEnv.Define(param.Name, val)
	}
	for _, name := range blk.ImplicitParams {
		index := implicitBlockParamIndex(name)
		val := NewNil()
		if index >= 0 && index < len(args) {
			val = args[index]
		}
		blockEnv.Define(name, val)
	}
	// The block's lexical home scopes any literal created while the body runs:
	// a nested block returns from the same method this block does, even when
	// yield executes the body inside a callee frame.
	exec.pushBlockHomeToken(blk.homeReturnToken)
	exec.blockDepth++
	val, returned, err := exec.evalLocalScopeStatements(blk.Body, blockEnv)
	exec.blockDepth--
	exec.popBlockHomeToken()
	val, returned, err = consumeFunctionReturnSignal(val, returned, err)
	if err != nil {
		if errors.Is(err, errRescueRetry) {
			return NewNil(), exec.localJumpErrorAt(pos, "retry cannot cross call boundary")
		}
		return NewNil(), err
	}
	if returned {
		// Ruby non-local return: an explicit return in a block body returns
		// from the method that created the block, not from the block call. A
		// dead home — the method already returned, the block was built outside
		// any method, or it runs in another execution — raises LocalJumpError
		// right here, where a surrounding rescue can catch it. A live home
		// travels the error path so drivers unwind and ensure blocks run; the
		// invocation whose token matches converts it into its return value.
		if !exec.returnTokenLive(blk.homeReturnToken) {
			return NewNil(), exec.localJumpErrorAt(blockBodyPos(blk), "unexpected return")
		}
		return NewNil(), &nonLocalReturnSignal{
			value: blockEnv.settleArrayAppendResult(val),
			token: blk.homeReturnToken,
		}
	}
	return blockEnv.settleArrayAppendResult(val), nil
}

// blockBodyPos anchors a block-level diagnostic to the block's first
// statement, the closest stable position a detached block value carries.
func blockBodyPos(blk *Block) Position {
	if len(blk.Body) > 0 {
		return blk.Body[0].Pos()
	}
	return Position{}
}

func rubyBlockBindArgs(params []Param, args []Value) []Value {
	if len(args) != 1 || args[0].Kind() != KindArray {
		return args
	}
	if rubyBlockPositionalBindCount(params) <= 1 {
		return args
	}
	return args[0].Array()
}

func rubyBlockPositionalBindCount(params []Param) int {
	positional := 0
	for _, param := range params {
		switch param.Kind {
		case ParamNormal, ParamRest:
			positional++
		case ParamKeyword, ParamKeywordRest:
			continue
		}
	}
	return positional
}

func implicitBlockParamIndex(name string) int {
	if name == "it" {
		return 0
	}
	if len(name) == 2 && name[0] == '_' && name[1] >= '1' && name[1] <= '9' {
		return int(name[1] - '1')
	}
	return -1
}

func blockCanReuseEnv(blk *Block) bool {
	return !statementsCaptureCurrentEnv(blk.Body)
}

// functionCanReuseCallEnv reports whether a function's call frame can be safely
// recycled after the call returns: its body must not capture the current env
// (into a closure, nested def, or class) and neither may anything evaluated in
// the frame during argument binding. The body check mirrors blockCanReuseEnv;
// the parameter check is additional because a parameter's default and its
// destructuring/ivar target are evaluated in the call frame during binding yet
// are not part of the body the statement walk sees, so a default like
// `def f(g = -> { ... })` would otherwise escape the frame undetected. Binding
// targets cannot hold closures today, but checking them keeps the analysis
// conservative if that ever changes: a false positive only forgoes reuse.
func functionCanReuseCallEnv(fn *ScriptFunction) bool {
	for i := range fn.Params {
		if expressionCapturesCurrentEnv(fn.Params[i].DefaultVal) {
			return false
		}
		if expressionCapturesCurrentEnv(fn.Params[i].Target) {
			return false
		}
	}
	return !statementsCaptureCurrentEnv(fn.Body)
}

func statementsCaptureCurrentEnv(stmts []Statement) bool {
	return slices.ContainsFunc(stmts, statementCapturesCurrentEnv)
}

func statementCapturesCurrentEnv(stmt Statement) bool {
	switch s := stmt.(type) {
	case *FunctionStmt, *ClassStmt:
		return true
	case *ReturnStmt:
		return expressionCapturesCurrentEnv(s.Value)
	case *RaiseStmt:
		return expressionCapturesCurrentEnv(s.Value) || expressionCapturesCurrentEnv(s.Message)
	case *AssignStmt:
		return expressionCapturesCurrentEnv(s.Target) || expressionCapturesCurrentEnv(s.Value)
	case *ExprStmt:
		return expressionCapturesCurrentEnv(s.Expr)
	case *IfStmt:
		if expressionCapturesCurrentEnv(s.Condition) ||
			statementsCaptureCurrentEnv(s.Consequent) ||
			statementsCaptureCurrentEnv(s.Alternate) {
			return true
		}
		for _, branch := range s.ElseIf {
			if statementCapturesCurrentEnv(branch) {
				return true
			}
		}
		return false
	case *ForStmt:
		return expressionCapturesCurrentEnv(s.Target) || expressionCapturesCurrentEnv(s.Iterable) || statementsCaptureCurrentEnv(s.Body)
	case *WhileStmt:
		return expressionCapturesCurrentEnv(s.Condition) || statementsCaptureCurrentEnv(s.Body)
	case *UntilStmt:
		return expressionCapturesCurrentEnv(s.Condition) || statementsCaptureCurrentEnv(s.Body)
	case *BreakStmt:
		return expressionCapturesCurrentEnv(s.Value)
	case *NextStmt:
		return expressionCapturesCurrentEnv(s.Value)
	case *AliasStmt, *RetryStmt, *EnumStmt:
		return false
	case *TryStmt:
		for i := range s.Rescues {
			if statementsCaptureCurrentEnv(s.Rescues[i].Body) {
				return true
			}
		}
		return statementsCaptureCurrentEnv(s.Body) ||
			statementsCaptureCurrentEnv(s.Else) ||
			statementsCaptureCurrentEnv(s.Ensure)
	default:
		return true
	}
}

func expressionCapturesCurrentEnv(expr Expression) bool {
	switch e := expr.(type) {
	case nil:
		return false
	case *BlockLiteral:
		return true
	case *Identifier, *IntegerLiteral, *FloatLiteral, *StringLiteral, *RegexLiteral, *BoolLiteral, *NilLiteral, *SymbolLiteral, *IvarExpr, *ClassVarExpr:
		return false
	case *ArrayLiteral:
		return slices.ContainsFunc(e.Elements, expressionCapturesCurrentEnv)
	case *HashLiteral:
		for _, pair := range e.Pairs {
			if expressionCapturesCurrentEnv(pair.Key) || expressionCapturesCurrentEnv(pair.Value) {
				return true
			}
		}
		return false
	case *CallExpr:
		if expressionCapturesCurrentEnv(e.Callee) || e.Block != nil {
			return true
		}
		if slices.ContainsFunc(e.Args, expressionCapturesCurrentEnv) {
			return true
		}
		for _, kw := range e.KwArgs {
			if expressionCapturesCurrentEnv(kw.Value) {
				return true
			}
		}
		return false
	case *TypeLiteral:
		return e.Fallback != nil && expressionCapturesCurrentEnv(e.Fallback)
	case *SplatArg:
		return expressionCapturesCurrentEnv(e.Value)
	case *MemberExpr:
		return expressionCapturesCurrentEnv(e.Object)
	case *ScopeExpr:
		return expressionCapturesCurrentEnv(e.Object)
	case *IndexExpr:
		if expressionCapturesCurrentEnv(e.Object) {
			return true
		}
		return slices.ContainsFunc(e.Indices, expressionCapturesCurrentEnv)
	case *DestructureTarget:
		for _, elem := range e.Elements {
			if expressionCapturesCurrentEnv(elem.Target) {
				return true
			}
		}
		return false
	case *UnaryExpr:
		return expressionCapturesCurrentEnv(e.Right)
	case *BinaryExpr:
		return expressionCapturesCurrentEnv(e.Left) || expressionCapturesCurrentEnv(e.Right)
	case *ConditionalExpr:
		return expressionCapturesCurrentEnv(e.Condition) ||
			expressionCapturesCurrentEnv(e.Consequent) ||
			expressionCapturesCurrentEnv(e.Alternate)
	case *RescueExpr:
		return expressionCapturesCurrentEnv(e.Body) ||
			expressionCapturesCurrentEnv(e.Fallback)
	case *IfExpr:
		if expressionCapturesCurrentEnv(e.Condition) ||
			expressionCapturesCurrentEnv(e.Consequent) ||
			expressionCapturesCurrentEnv(e.Alternate) {
			return true
		}
		for _, branch := range e.ElseIf {
			if expressionCapturesCurrentEnv(branch.Condition) || expressionCapturesCurrentEnv(branch.Result) {
				return true
			}
		}
		return false
	case *RangeExpr:
		return expressionCapturesCurrentEnv(e.Start) || expressionCapturesCurrentEnv(e.End)
	case *CaseExpr:
		if expressionCapturesCurrentEnv(e.Target) || expressionCapturesCurrentEnv(e.ElseExpr) {
			return true
		}
		for _, clause := range e.Clauses {
			for _, value := range clause.Values {
				if expressionCapturesCurrentEnv(value.Expr) {
					return true
				}
			}
			if expressionCapturesCurrentEnv(clause.Result) {
				return true
			}
		}
		return false
	case *YieldExpr:
		return slices.ContainsFunc(e.Args, expressionCapturesCurrentEnv)
	case *InterpolatedString:
		return stringPartsCaptureCurrentEnv(e.Parts)
	case *InterpolatedSymbol:
		return stringPartsCaptureCurrentEnv(e.Parts)
	case *IfStmt, *ForStmt, *WhileStmt, *UntilStmt, *TryStmt:
		return statementCapturesCurrentEnv(e.(Statement))
	default:
		return true
	}
}

func stringPartsCaptureCurrentEnv(parts []StringPart) bool {
	for _, part := range parts {
		stringExpr, ok := part.(StringExpr)
		if ok && expressionCapturesCurrentEnv(stringExpr.Expr) {
			return true
		}
	}
	return false
}

func (exec *Execution) bindBlockParamTarget(env *Env, target Expression, value Value, charge *blockBindCharge, blk *Block) error {
	switch t := target.(type) {
	case *Identifier:
		env.Define(t.Name, value)
		// Charge the bound leaf so a fresh rest backing a destructure collected
		// (the only binding that allocates beyond the call roots) counts toward the
		// quota even when the block body is empty. Pass-through bindings dedup
		// against the seeded arguments and charge essentially nothing.
		return charge.charge(value)
	case *DestructureTarget:
		// Walk the destructure with the block charge's own destructureCharge and charge
		// every bound leaf through the per-call bind charge. The destructureCharge
		// preflights a named rest's backing against the quota BEFORE assignDestructure
		// makes+copies it, so a single huge tail cannot materialize under a quota below
		// one copied window before the post-bind charge observes it. The post-bind
		// charge then probes each fresh leaf against the persistent root estimator --
		// which already holds the iterator's Go-stack receiver -- so a rest backing is
		// charged its real, dedup-aware footprint against the receiver the standalone
		// exec.assignDestructure charge cannot see (its liveRoot is only the single
		// yielded value, not the whole receiver).
		return assignDestructureWithNormalizer(t, value, func(target Expression, value Value) error {
			return exec.bindBlockParamTarget(env, target, value, charge, blk)
		}, charge.destructureCharge(exec), func(element DestructureElement, value Value) (Value, error) {
			return exec.normalizeBlockDestructureElement(blk, element, value)
		})
	default:
		return exec.errorAt(target.Pos(), "invalid block parameter target")
	}
}

func (exec *Execution) normalizeBlockDestructureElement(blk *Block, element DestructureElement, value Value) (Value, error) {
	if element.Type == nil {
		return value, nil
	}
	normalized, err := normalizeValueForType(value, element.Type, typeContext{
		owner:    blk.owner,
		env:      blk.Env,
		fallback: exec.root,
		exec:     exec,
	})
	if err != nil {
		if isHostControlSignal(err) {
			return NewNil(), err
		}
		if isNormalizationLimitError(err) {
			return NewNil(), exec.wrapError(err, element.Type.Position)
		}
		name := ast.FormatDestructureTarget(element.Target)
		if name == "" {
			name = "destructured value"
		}
		return NewNil(), exec.errorAt(element.Type.Position, "%s", formatArgumentTypeMismatch(name, err))
	}
	return normalized, nil
}

func (exec *Execution) evalYield(expr *YieldExpr, env *Env) (Value, error) {
	block, ok := env.lookupCallBlock()
	if !ok || block.Kind() == KindNil {
		return NewNil(), exec.localJumpErrorAt(expr.Pos(), "no block given")
	}
	blk := valueBlock(block)
	args := make([]Value, 0, len(expr.Args))
	for i, arg := range expr.Args {
		expectation := yieldArgumentExpectation(blk, i, len(expr.Args))
		val, err := exec.evalExpressionWithExpectation(arg, env, expectation)
		if err != nil {
			return NewNil(), err
		}
		if err := exec.checkMemoryValue(val); err != nil {
			return NewNil(), err
		}
		args = append(args, val)
	}
	if len(args) > 0 {
		if err := exec.checkMemoryWith(args...); err != nil {
			return NewNil(), err
		}
	}
	return exec.callBlockValue(block, args, expr.Pos())
}

func yieldArgumentExpectation(blk *Block, argIndex, argCount int) expressionExpectation {
	if blk == nil || len(blk.Params) == 0 {
		return expressionExpectation{}
	}
	return blockArgumentExpectation(blk.Params, argIndex, argCount)
}

func blockArgumentExpectation(params []Param, argIndex, argCount int) expressionExpectation {
	if len(params) == 0 {
		return expressionExpectation{}
	}
	if argCount == 1 {
		param, ok := positionalCallableParam(params, 0)
		if !ok {
			return expressionExpectation{}
		}
		expectation := positionalArgumentExpectation(param)
		if rubyBlockPositionalBindCount(params) > 1 {
			expectation.arrayElement = blockArrayElementExpectation(params)
		}
		return expectation
	}
	param, ok := positionalCallableParam(params, argIndex)
	if !ok {
		return expressionExpectation{}
	}
	return positionalArgumentExpectation(param)
}

func blockArrayElementExpectation(params []Param) func(int, int) expressionExpectation {
	return func(index, _ int) expressionExpectation {
		param, ok := positionalCallableParam(params, index)
		if !ok {
			return expressionExpectation{}
		}
		return positionalArgumentExpectation(param)
	}
}

func (exec *Execution) assignToMember(obj Value, property string, value Value, pos Position) error {
	setterName := property + "="
	var methods map[string]*ScriptFunction
	var vars map[string]Value

	switch obj.Kind() {
	case KindInstance:
		methods = valueInstance(obj).Class.Methods
		vars = valueInstance(obj).Ivars
	case KindClass:
		methods = valueClass(obj).ClassMethods
		vars = valueClass(obj).ClassVars
	default:
		return exec.errorAt(pos, "cannot assign to %s", obj.Kind())
	}

	if fn, ok := methods[setterName]; ok {
		if fn.Private {
			return exec.errorAt(pos, "private method %s", setterName)
		}
		if fn.Protected && !exec.assignTargetProtectedAccessAllowed(obj) {
			return exec.errorAt(pos, "protected method %s", setterName)
		}
		_, err := exec.callFunction(fn, obj, []Value{value}, nil, NewNil(), pos)
		if err != nil {
			if ok, controlErr := exec.callBoundaryControlError(err, pos); ok {
				return controlErr
			}
		}
		return err
	}

	if _, hasGetter := methods[property]; hasGetter {
		return exec.errorAt(pos, "cannot assign to read-only property %s", property)
	}

	bumpMutationEpoch()
	publishBindingReplacement(vars[property], value)
	vars[property] = value
	return nil
}

func (exec *Execution) assign(target Expression, value Value, env *Env) error {
	switch t := target.(type) {
	case *Identifier:
		if self, ok := classConstantAssignmentSelf(t.Name, env); ok && !env.hasCallLocalBinding(t.Name) {
			bumpMutationEpoch()
			publishBindingReplacement(valueClass(self).ClassVars[t.Name], value)
			valueClass(self).ClassVars[t.Name] = value
			return nil
		}
		env.Assign(t.Name, value)
		return nil
	case *DestructureTarget:
		return exec.assignDestructure(t, value, func(target Expression, value Value) error {
			return exec.assign(target, value, env)
		})
	case *MemberExpr:
		defer exec.restore(exec.savedAddressedScope())
		obj, resolution, err := exec.resolveMutableReceiver(t.Object, env)
		if resolution == targetUnresolved && err == nil {
			obj, err = exec.evalExpression(t.Object, env)
		}
		if err != nil {
			return err
		}
		if err := exec.checkMemoryValue(obj); err != nil {
			return err
		}
		if resolution == targetTemporary {
			// The receiver is a temporary the path walk evaluated; the write
			// must reach nothing a durable slot still names.
			obj, err = exec.detachTemporaryWriteReceiver(obj)
			if err != nil {
				return err
			}
		}
		return exec.assignToEvaluatedMember(t, obj, value)
	case *IvarExpr:
		self, ok := env.Get("self")
		if !ok || self.Kind() != KindInstance {
			return exec.errorAt(target.Pos(), "no instance context for ivar")
		}
		inst := valueInstance(self)
		normalized, err := exec.normalizeIvarWrite(inst, t.Name, value, target.Pos())
		if err != nil {
			return err
		}
		bumpMutationEpoch()
		publishBindingReplacement(inst.Ivars[t.Name], normalized)
		inst.Ivars[t.Name] = normalized
		return nil
	case *ClassVarExpr:
		self, ok := env.Get("self")
		if !ok {
			return exec.errorAt(target.Pos(), "no class context for class var")
		}
		switch self.Kind() {
		case KindInstance:
			bumpMutationEpoch()
			publishBindingReplacement(valueInstance(self).Class.ClassVars[t.Name], value)
			valueInstance(self).Class.ClassVars[t.Name] = value
			return nil
		case KindClass:
			bumpMutationEpoch()
			publishBindingReplacement(valueClass(self).ClassVars[t.Name], value)
			valueClass(self).ClassVars[t.Name] = value
			return nil
		default:
			return exec.errorAt(target.Pos(), "no class context for class var")
		}
	case *IndexExpr:
		defer exec.restore(exec.savedAddressedScope())
		// The index selectors are expressions, so evaluating them runs script
		// code between resolving the receiver and writing through it: `b[f(b)]
		// = 9` can bind b somewhere new while choosing where to write. The
		// write isolates again for that reason.
		obj, path, resolution, err := exec.resolveMutableTarget(t.Object, env)
		if err != nil {
			return err
		}
		if resolution == targetUnresolved {
			obj, err = exec.evalExpression(t.Object, env)
			if err != nil {
				return err
			}
		}
		if err := exec.checkMemoryValue(obj); err != nil {
			return err
		}
		indices, err := exec.evalIndexSelectors(t, obj, env)
		if err != nil {
			return err
		}
		if resolution != targetAddressed {
			if resolution == targetTemporary {
				// The receiver is a temporary the path walk evaluated; the
				// write must reach nothing a durable slot still names.
				obj, err = exec.detachTemporaryWriteReceiver(obj)
				if err != nil {
					return err
				}
			}
			return exec.assignToEvaluatedIndex(t, obj, indices, value)
		}
		return exec.writeThroughMutablePath(path, env, func(leaf Value) error {
			return exec.assignToEvaluatedIndex(t, leaf, indices, value)
		})
	default:
		return exec.errorAt(target.Pos(), "invalid assignment target")
	}
}

func classConstant(self Value, name string) (Value, bool) {
	if !isConstantIdentifier(name) {
		return NewNil(), false
	}
	switch self.Kind() {
	case KindClass:
		val, ok := valueClass(self).ClassVars[name]
		return val, ok
	case KindInstance:
		inst := valueInstance(self)
		if inst == nil || inst.Class == nil {
			return NewNil(), false
		}
		val, ok := inst.Class.ClassVars[name]
		return val, ok
	default:
		return NewNil(), false
	}
}

func isConstantIdentifier(name string) bool {
	r, _ := utf8.DecodeRuneInString(name)
	return r != utf8.RuneError && unicode.IsUpper(r)
}

func isClassConstantAssignmentName(name string, env *Env) bool {
	_, ok := classConstantAssignmentSelf(name, env)
	return ok
}

func classConstantAssignmentSelf(name string, env *Env) (Value, bool) {
	if env == nil || !isConstantIdentifier(name) {
		return Value{}, false
	}
	self, ok := env.Get("self")
	if !ok || self.Kind() != KindClass {
		return Value{}, false
	}
	return self, true
}

func (exec *Execution) assignToEvaluatedMember(target *MemberExpr, obj, value Value) error {
	switch obj.Kind() {
	case KindHash, KindObject:
		if err := objectTagMutationError(obj, "member assignment"); err != nil {
			return exec.errorAt(target.Pos(), "%s", err.Error())
		}
		key := NewString(target.Property)
		if obj.Kind() == KindHash {
			key = hashMemberAssignmentKey(obj, target.Property)
		}
		stored, err := exec.detachStoredCollection(value)
		if err != nil {
			return err
		}
		return hashSet(obj, key, stored)
	case KindInstance, KindClass:
		return exec.assignToMember(obj, target.Property, value, target.Pos())
	default:
		return exec.errorAt(target.Pos(), "cannot assign to %s", obj.Kind())
	}
}

// assignToEvaluatedIndex writes value at a bracket target. Array assignment
// accepts a single integer index, counting a negative index back from the end
// (Ruby's arr[-1] = x); an index outside the array raises rather than
// auto-extending. Hash and object assignment store under a single key. Slice
// assignment (start/length or range targets) is not supported.
func (exec *Execution) assignToEvaluatedIndex(target *IndexExpr, obj Value, indices []Value, value Value) error {
	switch obj.Kind() {
	case KindArray:
		if len(indices) != 1 {
			return exec.errorAt(target.Position, "array index assignment expects a single index")
		}
		arr := obj.Array()
		i, err := exec.indexSelectorToInt(target, indices[0], 0)
		if err != nil {
			return err
		}
		pos := i
		if pos < 0 {
			pos += len(arr)
		}
		if pos < 0 || pos >= len(arr) {
			return exec.errorAt(target.IndexPos(0), "array index out of bounds")
		}
		stored, err := exec.detachStoredCollection(value)
		if err != nil {
			return err
		}
		publishBindingReplacement(arr[pos], stored)
		obj.BumpMutationEpoch()
		arr[pos] = stored
		return nil
	case KindHash, KindObject:
		if len(indices) != 1 {
			return exec.errorAt(target.Position, "%s index assignment expects a single key", obj.Kind())
		}
		// Canonicalizing a big-integer key is linear in its words; charge
		// before the write so key-heavy loops stay inside the step quota.
		if err := exec.chargeValueKeySteps(indices[0]); err != nil {
			return err
		}
		if err := objectTagMutationError(obj, "index assignment"); err != nil {
			return exec.errorAt(target.Position, "%s", err.Error())
		}
		// The charged store commits an added entry into the base-walk memo
		// and skips the epoch bump, keeping hash-filling loops linear under
		// the quota (#1129); replacements and every other ineligible write
		// take the ordinary bumping path.
		value, err := exec.detachStoredCollection(value)
		if err != nil {
			return err
		}
		if handled, storeErr := exec.hashStoreCharged(obj, indices[0], value); handled {
			if storeErr != nil {
				return exec.wrapError(storeErr, target.IndexPos(0))
			}
			return nil
		}
		if err := hashSet(obj, indices[0], value); err != nil {
			return exec.errorAt(target.IndexPos(0), "%s", err.Error())
		}
		return nil
	case KindInstance:
		// obj[i, ...] = value dispatches to a user-defined []= method with the
		// indices followed by the assigned value, mirroring Ruby's index-write
		// protocol. The method's return value is discarded, as in Ruby, where
		// the assignment expression always evaluates to the assigned value.
		fn, ok := instanceOperatorMethod(obj, "[]=")
		if !ok {
			return exec.errorAt(target.Object.Pos(), "cannot index %s: %s does not define []=", obj.Kind(), valueInstance(obj).Class.Name)
		}
		if fn.Private {
			return exec.errorAt(target.Position, "private method []=")
		}
		if fn.Protected && !exec.protectedInstanceAccessAllowed(valueInstance(obj).Class) {
			return exec.errorAt(target.Position, "protected method []=")
		}
		args := make([]Value, 0, len(indices)+1)
		args = append(args, indices...)
		args = append(args, value)
		_, err := exec.callOperatorFunction(fn, obj, args, target.Position)
		return err
	default:
		return exec.errorAt(target.Object.Pos(), "cannot index %s", obj.Kind())
	}
}

// AssignDestructure applies Vibescript's destructuring assignment rules and
// invokes assign for each concrete leaf target. It is the host-facing entry
// point used by tools that walk destructuring targets without a sandboxed
// Execution (such as the REPL extracting bound names from a result); it never
// charges memory because those callers run outside a quota. Sandboxed evaluation
// goes through Execution.assignDestructure, which charges every fresh slot array
// (the right-hand-side snapshot and any named rest window) against the memory
// quota before it is allocated.
func AssignDestructure(target *DestructureTarget, value Value, assign func(Expression, Value) error) error {
	return assignDestructure(target, value, assign, destructureCharge{check: noopDestructureCheck, step: noopDestructureStep})
}

// destructureCharge meters the fresh int-value slot arrays a destructuring
// assignment allocates (the right-hand-side snapshot and any named rest
// window) against a memory quota before they materialize.
//
// liveRoot is the top-level evaluated right-hand side. While assignDestructure
// runs it is held only on this call's Go stack — a function or capability
// return, or an array literal — so the memory estimator's root walk never sees
// it, yet it is real memory live at the peak of every slot array this charge
// guards. Threading it into each check charges it as a root (deduplicated
// against the base, so a right-hand side already reachable from an environment
// contributes nothing extra). Every nested element is reachable through this
// root, so the top-level value alone covers all levels.
//
// liveSlots tracks the snapshot slots that are already allocated but reachable
// only from a Go-local slice on the call stack, so the root walk cannot see them
// either; the count threads down the recursion so a nested destructure charges
// its own arrays on top of every enclosing level's still-live snapshot,
// projecting the true peak rather than a single level's allocation.
//
// step bills the step quota for the targets the walk visits and the value slots
// it copies. The walk is one statement but arbitrarily much work: a target may
// nest, and every nested rest copies its own window, so a single assignment
// could copy a large host-supplied array once per nested target for a flat
// handful of steps and poll the context not at all (#49).
type destructureCharge struct {
	check     func(count, liveSlots int, liveRoot Value) error
	step      func(units int) error
	liveRoot  Value
	liveSlots int
}

// noopDestructureCheck admits every allocation. AssignDestructure's host callers
// run outside a memory quota, so the arrays they may materialize are not metered.
func noopDestructureCheck(int, int, Value) error { return nil }

// noopDestructureStep bills nothing, for the same host callers: they run outside
// a step quota too.
func noopDestructureStep(int) error { return nil }

// destructureUnitsPerStep amortizes a destructuring assignment over the targets
// it walks and the value slots it copies, at the same rate chargeStringScan and
// the predeclaration scan use for their own per-unit work. Copying a slot is a
// pointer-width move and visiting a target is a type switch, so an ordinary
// `a, b, *rest = xs` stays free while work that scales with a host-supplied
// array cannot.
const destructureUnitsPerStep = 64

// chargeDestructureScan bills n units of destructuring work, carrying the
// sub-step remainder on the execution. Rounding each call down would leave a
// target list of many just-under-threshold windows free however many of them
// there were, which is the shape the charge exists to stop; the residue settles
// whole steps as those tails accumulate. Charging through stepN also restores
// the cancellation poll and periodic memory check that a long assignment
// otherwise ran to completion without.
func (exec *Execution) chargeDestructureScan(n int) error {
	// A nil execution has no quota to charge against, matching chargeStringScan:
	// the walk is reachable from the static checker's speculative binding pass,
	// and callers should not each have to remember that.
	if exec == nil || n <= 0 {
		return nil
	}
	exec.destructureScanResidue += n
	steps := exec.destructureScanResidue / destructureUnitsPerStep
	if steps <= 0 {
		return nil
	}
	exec.destructureScanResidue -= steps * destructureUnitsPerStep
	return exec.stepN(steps)
}

// assignDestructure applies Vibescript's destructuring assignment rules and
// invokes assign for each concrete leaf target. The returned charge meters every
// fresh slot array against the caller's memory quota before it is allocated,
// counting the live right-hand side held on the Go stack as a root so the peak
// is projected even when the right-hand side is not reachable from any
// environment (a function return or array literal).
func (exec *Execution) assignDestructure(target *DestructureTarget, value Value, assign func(Expression, Value) error) error {
	return assignDestructure(target, value, assign, destructureCharge{
		check:    exec.checkProjectedIntArrayBytesWithLive,
		step:     exec.chargeDestructureScan,
		liveRoot: value,
	})
}

func assignDestructure(target *DestructureTarget, value Value, assign func(Expression, Value) error, charge destructureCharge) error {
	return assignDestructureWithNormalizer(target, value, assign, charge, nil)
}

type destructureElementNormalizer func(DestructureElement, Value) (Value, error)

func assignDestructureWithNormalizer(target *DestructureTarget, value Value, assign func(Expression, Value) error, charge destructureCharge, normalize destructureElementNormalizer) error {
	// Charged per level, so a nested target bills its own elements on top of
	// every enclosing one: the recursion is what turns one statement into
	// arbitrarily many target walks.
	if err := charge.step(len(target.Elements)); err != nil {
		return err
	}
	values := destructureValues(value)
	// Ruby evaluates the whole right-hand side into an array before performing
	// any assignment, so every target reads its original value regardless of
	// LHS write order. When the RHS is an array, destructureValues aliases its
	// live backing store, so a target that writes into that array and is read
	// back by a later target (e.g. "values[1], *rest = values") would otherwise
	// let that later read observe the mutation. Snapshot the source only when a
	// writing target precedes a reading one; the snapshot's slot array is a fresh
	// allocation, so charge it against the memory quota and fail fast before
	// materializing it. The common case where every target only binds a name (and
	// the scalar RHS path, which already returns a fresh slice) keeps the alias,
	// as does a write whose only followers discard their window (e.g.
	// "values[0], * = values"), which reads nothing the write could corrupt.
	if value.Kind() == KindArray && target.WriteIsReadBack() {
		if err := charge.check(len(values), charge.liveSlots, charge.liveRoot); err != nil {
			return err
		}
		if err := charge.step(len(values)); err != nil {
			return err
		}
		values = append([]Value(nil), values...)
		// The snapshot stays live on this call's stack while every leaf (and any
		// nested destructure) runs, so fold it into the running baseline the
		// charge projects for the rest window and for nested snapshots.
		charge.liveSlots += len(values)
	}
	restIndex := -1
	for i, element := range target.Elements {
		if element.Rest {
			restIndex = i
			break
		}
	}

	if restIndex == -1 {
		for i, element := range target.Elements {
			val := valueAt(values, i)
			var err error
			if normalize != nil {
				val, err = normalize(element, val)
				if err != nil {
					return err
				}
			}
			if err := assignDestructureValue(element.Target, val, assign, charge, normalize); err != nil {
				return err
			}
		}
		return nil
	}

	trailing := len(target.Elements) - restIndex - 1
	// Clamp the rest window to the available values. When the target has more
	// fixed targets than the value provides, restIndex can exceed len(values);
	// the missing fixed targets bind to nil (via valueAt) and the rest is empty,
	// matching Ruby. Without clamping the low bound, values[restIndex:restEnd]
	// would panic the host (a sandbox DoS) on a slice-out-of-range.
	restStart := min(restIndex, len(values))
	restEnd := max(restStart, len(values)-trailing)
	// An anonymous rest target (bare "*") discards its window, so skip building
	// the rest array entirely. Copying and wrapping values[restStart:restEnd]
	// would otherwise allocate a full second slice and add O(n) work for a
	// segment no binding will ever read (e.g. *, last = huge_array).
	restAnonymous := target.Elements[restIndex].Target == nil
	for i, element := range target.Elements {
		var val Value
		switch {
		case i < restIndex:
			val = valueAt(values, i)
		case i == restIndex:
			if restAnonymous {
				continue
			}
			// The rest window is a second fresh slot array that coexists with the
			// snapshot above (charge.liveSlots already folds it in), so charge it
			// against the quota before materializing it. Without this a quota that
			// fits base+snapshot but not base+snapshot+rest would pass the snapshot
			// check, allocate the window anyway, and the snapshot would be gone by
			// the next per-statement check, letting execution exceed the quota.
			if err := charge.check(restEnd-restStart, charge.liveSlots, charge.liveRoot); err != nil {
				return err
			}
			if err := charge.step(restEnd - restStart); err != nil {
				return err
			}
			// Allocate the rest backing with capacity exactly equal to the collected
			// element count. append([]Value(nil), src...) (and slices.Clone, which
			// wraps it) would let Go's growslice round the capacity up past len, so
			// the memory estimator -- which charges slice backings by cap -- would see
			// a larger array than the value's own length implies. A make+copy keeps
			// cap == len so the rest array's charged footprint matches what its
			// element count predicts.
			restSrc := values[restStart:restEnd]
			restValues := make([]Value, len(restSrc))
			copy(restValues, restSrc)
			val = NewArray(restValues)
		default:
			// Trailing targets bind to the values immediately after the rest
			// window, left-to-right. When the input is too short to fill them
			// all, the remaining targets pad with nil on the right, matching
			// Ruby (e.g. a, *, y, z = [1, 2] yields a=1, y=2, z=nil).
			val = valueAt(values, restEnd+(i-restIndex-1))
		}
		var err error
		if normalize != nil {
			val, err = normalize(element, val)
			if err != nil {
				return err
			}
		}
		if err := assignDestructureValue(element.Target, val, assign, charge, normalize); err != nil {
			return err
		}
	}
	return nil
}

func assignDestructureValue(target Expression, value Value, assign func(Expression, Value) error, charge destructureCharge, normalize destructureElementNormalizer) error {
	if target == nil {
		// Anonymous rest target ("*"): discard the captured values.
		return nil
	}
	if nested, ok := target.(*DestructureTarget); ok {
		return assignDestructureWithNormalizer(nested, value, assign, charge, normalize)
	}
	return assign(target, value)
}

func destructureValues(value Value) []Value {
	if value.Kind() == KindArray {
		return value.Array()
	}
	return []Value{value}
}

func valueAt(values []Value, index int) Value {
	if index < 0 || index >= len(values) {
		return NewNil()
	}
	return values[index]
}

func (exec *Execution) evalArrayAppendAssignment(stmt *AssignStmt, env *Env) (Value, bool, error) {
	target, ok := stmt.Target.(*Identifier)
	if !ok {
		return NewNil(), false, nil
	}

	// Only `values = values + [...]` uses the accumulator fast path: `+` is a
	// genuinely non-mutating operator, so reusing the receiver's hidden backing
	// buffer across iterations is invisible. push and << update the binding
	// they name, so they need no reassignment fast path -- the update itself
	// already amortizes growth.
	value, ok := stmt.Value.(*BinaryExpr)
	if !ok || value.Operator != tokenPlus {
		return NewNil(), false, nil
	}
	left, ok := value.Left.(*Identifier)
	if !ok || left.Name != target.Name {
		return NewNil(), false, nil
	}
	right, ok := value.Right.(*ArrayLiteral)
	if !ok {
		return NewNil(), false, nil
	}
	return exec.evalArrayConcatAppendAssignment(target.Name, value, right, env)
}

func (exec *Execution) evalArrayConcatAppendAssignment(name string, expr *BinaryExpr, right *ArrayLiteral, env *Env) (Value, bool, error) {
	receiver, ok := env.Get(name)
	if !ok || receiver.Kind() != KindArray {
		return NewNil(), false, nil
	}
	if err := exec.checkMemoryValue(receiver); err != nil {
		return NewNil(), true, err
	}

	values, err := exec.evalArrayLiteralElements(right, env)
	if err != nil {
		return NewNil(), true, err
	}
	rightValue := arrayValueFromAppendBuffer(values)
	if err := exec.checkMemoryWith(receiver, rightValue); err != nil {
		return NewNil(), true, err
	}

	result := exec.assignArrayAppendResult(name, receiver.Array(), values, env)
	if err := exec.checkMemoryValue(result); err != nil {
		return NewNil(), true, exec.wrapError(err, expr.Pos())
	}
	return result, true, nil
}

func (exec *Execution) evalArrayLiteralElements(literal *ArrayLiteral, env *Env) ([]Value, error) {
	values := make([]Value, len(literal.Elements))
	for i, element := range literal.Elements {
		val, err := exec.evalExpressionWithAuto(element, env, true)
		if err != nil {
			return nil, err
		}
		if err := exec.checkMemoryValue(val); err != nil {
			return nil, err
		}
		values[i] = val
	}
	return values, nil
}

func (exec *Execution) assignArrayAppendResult(name string, base, extras []Value, env *Env) Value {
	buffer, ok := env.arrayAppendBuffer(name)
	if !ok || !sameArrayBacking(buffer, base) {
		buffer = make([]Value, len(base), len(base)+len(extras))
		copy(buffer, base)
	}
	buffer = append(buffer, extras...)
	result := arrayValueFromAppendBuffer(buffer)
	env.assignArrayAppendBuffer(name, result, buffer)
	return result
}

// sameArrayBacking reports whether the registered accumulator buffer still is
// the receiver's exact element backing: same length, same first-element
// address. A mismatch means the binding's elements changed since registration
// (an in-place mutator replaced or resliced them), so the fast path must copy
// into a fresh buffer instead of appending onto stale contents.
func sameArrayBacking(buffer, base []Value) bool {
	if len(buffer) != len(base) {
		return false
	}
	if len(base) == 0 {
		return true
	}
	return &buffer[0] == &base[0]
}

func arrayValueFromAppendBuffer(buffer []Value) Value {
	return NewArray(buffer[:len(buffer):len(buffer)])
}

func (exec *Execution) evalRangeExpr(expr *RangeExpr, env *Env) (Value, error) {
	rng := Range{Exclusive: expr.Exclusive, Beginless: expr.Start == nil, Endless: expr.End == nil}
	if expr.Start != nil {
		startVal, err := exec.evalExpression(expr.Start, env)
		if err != nil {
			return NewNil(), err
		}
		if startVal.IsBigInt() {
			return NewNil(), exec.errorAt(expr.Start.Pos(), "range endpoints must fit in a 64-bit integer")
		}
		start, err := valueToInt64(startVal)
		if err != nil {
			return NewNil(), exec.errorAt(expr.Start.Pos(), "%s", err.Error())
		}
		rng.Start = start
	}
	if expr.End != nil {
		endVal, err := exec.evalExpression(expr.End, env)
		if err != nil {
			return NewNil(), err
		}
		if endVal.IsBigInt() {
			return NewNil(), exec.errorAt(expr.End.Pos(), "range endpoints must fit in a 64-bit integer")
		}
		end, err := valueToInt64(endVal)
		if err != nil {
			return NewNil(), exec.errorAt(expr.End.Pos(), "%s", err.Error())
		}
		rng.End = end
	}
	return NewRange(rng), nil
}

func (exec *Execution) evalCaseExpr(expr *CaseExpr, env *Env) (Value, error) {
	return exec.evalCaseExprWithExpectation(expr, env, expressionExpectation{})
}

func (exec *Execution) evalCaseExprWithExpectation(expr *CaseExpr, env *Env, expectation expressionExpectation) (Value, error) {
	var target Value
	hasTarget := expr.Target != nil
	if hasTarget {
		var err error
		target, err = exec.evalExpression(expr.Target, env)
		if err != nil {
			return NewNil(), err
		}
		if err := exec.checkMemoryValue(target); err != nil {
			return NewNil(), err
		}
	}

	for _, clause := range expr.Clauses {
		matched := false
		for _, candidateExpr := range clause.Values {
			candidate, err := exec.evalExpression(candidateExpr.Expr, env)
			if err != nil {
				return NewNil(), err
			}
			if err := exec.checkMemoryValue(candidate); err != nil {
				return NewNil(), err
			}
			candidateMatched, err := exec.caseWhenValueMatches(hasTarget, target, candidate, candidateExpr.Splat, expr.Pos())
			if err != nil {
				return NewNil(), err
			}
			if candidateMatched {
				matched = true
				break
			}
		}
		if !matched {
			continue
		}
		result, err := exec.evalExpressionWithExpectation(clause.Result, env, expectation)
		if err != nil {
			return NewNil(), err
		}
		if err := exec.checkMemoryValue(result); err != nil {
			return NewNil(), err
		}
		return result, nil
	}

	if expr.ElseExpr != nil {
		result, err := exec.evalExpressionWithExpectation(expr.ElseExpr, env, expectation)
		if err != nil {
			return NewNil(), err
		}
		if err := exec.checkMemoryValue(result); err != nil {
			return NewNil(), err
		}
		return result, nil
	}

	return NewNil(), nil
}

func (exec *Execution) caseWhenValueMatches(hasTarget bool, target, candidate Value, splat bool, pos Position) (bool, error) {
	if !splat {
		matched, err := caseWhenMatches(exec, hasTarget, target, candidate)
		if err != nil {
			return false, exec.wrapError(err, pos)
		}
		return matched, nil
	}
	if candidate.Kind() != KindArray {
		return false, exec.errorAt(pos, "case when splat value must be an array")
	}
	for _, item := range candidate.Array() {
		if err := exec.step(); err != nil {
			return false, err
		}
		if err := exec.checkMemoryValue(item); err != nil {
			return false, err
		}
		matched, err := caseWhenMatches(exec, hasTarget, target, item)
		if err != nil {
			return false, exec.wrapError(err, pos)
		}
		if matched {
			return true, nil
		}
	}
	return false, nil
}

func caseWhenMatches(exec *Execution, hasTarget bool, target, candidate Value) (bool, error) {
	if !hasTarget {
		return candidate.Truthy(), nil
	}
	return caseCandidateMatches(exec, target, candidate)
}

// caseCandidateMatches accepts a nil exec (the static checker folds literal
// when clauses at compile time); a nil exec compares unmetered.
func caseCandidateMatches(exec *Execution, target, candidate Value) (bool, error) {
	// A regex matcher tests the candidate pattern against a string target,
	// mirroring Ruby's Regexp#=== and `when /re/` clause matching.
	if candidate.Kind() == KindRegex {
		return regexCandidateMatches(target, candidate)
	}
	if candidate.Kind() != KindRange {
		return exec.equalValues(target, candidate)
	}

	switch target.Kind() {
	case KindInt:
		if n, ok := target.CompactInt(); ok {
			return rangeContainsInt(candidate.Range(), n), nil
		}
		return rangeContainsBigInt(candidate.Range(), target), nil
	case KindFloat:
		return rangeContainsFloat(candidate.Range(), target.Float()), nil
	default:
		return exec.equalValues(target, candidate)
	}
}

// openRangeKindLabel names the open side of a range for iteration errors.
func openRangeKindLabel(rng Range) string {
	if rng.Beginless {
		return "a beginless range"
	}
	return "an endless range"
}

func rangeContainsInt(rng Range, value int64) bool {
	if rng.Beginless {
		if rng.Exclusive {
			return value < rng.End
		}
		return value <= rng.End
	}
	if rng.Endless {
		return value >= rng.Start
	}
	if rng.Start <= rng.End {
		if rng.Exclusive {
			return value >= rng.Start && value < rng.End
		}
		return value >= rng.Start && value <= rng.End
	}
	if rng.Exclusive {
		return value <= rng.Start && value > rng.End
	}
	return value <= rng.Start && value >= rng.End
}

func rangeContainsFloat(rng Range, value float64) bool {
	if math.IsNaN(value) {
		return false
	}
	// Open ranges admit magnitudes beyond int64: anything far below the end
	// is inside a beginless range, anything far above the start is inside an
	// endless one (including the matching infinity).
	if rng.Beginless {
		if value < minInt64Float {
			return true
		}
		if value >= maxInt64FloatExclusive {
			return false
		}
		floor := int64(math.Floor(value))
		// value < End (exclusive) holds exactly when its floor is below End;
		// value <= End (inclusive) additionally admits the integral value at
		// End itself. Integer comparisons keep this exact near int64 bounds,
		// like the bounded cases below.
		if rng.Exclusive {
			return floor < rng.End
		}
		return floor < rng.End || (floor == rng.End && value == math.Floor(value))
	}
	if rng.Endless {
		if value >= maxInt64FloatExclusive {
			return true
		}
		if value < minInt64Float {
			return false
		}
		// value >= Start holds when its ceiling is above Start, or the value
		// is exactly the integral Start.
		ceil := int64(math.Ceil(value))
		return ceil > rng.Start || (ceil == rng.Start && value == math.Ceil(value))
	}
	if value < minInt64Float || value >= maxInt64FloatExclusive {
		return false
	}

	floor := int64(math.Floor(value))
	ceil := int64(math.Ceil(value))
	if rng.Start <= rng.End {
		if floor < rng.Start {
			return false
		}
		if rng.Exclusive {
			return floor < rng.End
		}
		return ceil <= rng.End
	}
	if ceil > rng.Start {
		return false
	}
	if rng.Exclusive {
		return ceil > rng.End
	}
	return floor >= rng.End
}

type loopResultMode uint8

const (
	loopStatementResult loopResultMode = iota
	loopExpressionResult
)

func loopNormalResult(mode loopResultMode, expressionValue, last Value) Value {
	if mode == loopExpressionResult {
		return expressionValue
	}
	return last
}

func loopBreakResult(err error, mode loopResultMode, last Value) (Value, bool, error) {
	if breakVal, ok := loopBreakValue(err); ok {
		return breakVal, false, nil
	}
	if mode == loopExpressionResult {
		return NewNil(), false, nil
	}
	return last, false, nil
}

func expressionValueOrReturn(val Value, returned bool, err error) (Value, error) {
	if err != nil {
		return NewNil(), err
	}
	if returned {
		return NewNil(), newFunctionReturnValue(val)
	}
	return val, nil
}

func (exec *Execution) evalForStatement(stmt *ForStmt, env *Env) (Value, bool, error) {
	return exec.evalForLoop(stmt, env, loopStatementResult)
}

func (exec *Execution) evalForExpression(stmt *ForStmt, env *Env) (Value, error) {
	return expressionValueOrReturn(exec.evalForLoop(stmt, env, loopExpressionResult))
}

func (exec *Execution) evalForLoop(stmt *ForStmt, env *Env, mode loopResultMode) (Value, bool, error) {
	exec.loopDepth++
	defer func() {
		exec.loopDepth--
	}()

	iterable, err := exec.evalExpression(stmt.Iterable, env)
	if err != nil {
		return NewNil(), false, err
	}
	if err := exec.checkMemoryValue(iterable); err != nil {
		return NewNil(), false, err
	}
	predeclareTargetBindingNames(stmt.Target, env)
	last := NewNil()

	switch iterable.Kind() {
	case KindArray:
		// `for x in a` walks the header captured here while its body runs
		// arbitrary script, which is what a block-driving builtin does, so it
		// takes the same claim on the backing (see array_shrink.go). The claim
		// carries the current builtin depth rather than a depth of its own:
		// the loop body runs at that depth, and a shrink is always a builtin
		// dispatched from the body, so it is strictly deeper -- the same
		// enclosing-frame relation a dispatch claim expresses. That holds
		// however far the body wanders, since a script function call in
		// between leaves the builtin depth alone.
		heldBackings := exec.holdArrayBackings(iterable, nil, nil, false)
		defer func() {
			// Dropping this claim moves nothing and so cannot fail: only a
			// wildcard claim ever holds narrowed storage, only a host-driven
			// frame takes one, and every such frame drops its own claims on
			// its way out, before a defer at this mark can run.
			//
			// That invariant is real but it is not local to this line, and the
			// symptom of losing it would be storage the quota has forgotten --
			// the very retention this mechanism exists to prevent, and the
			// hardest kind to trace back here. Trip loudly under the oracle
			// rather than discard the error on the strength of the comment.
			if err := exec.releaseArrayBackings(heldBackings); estimatorVerify && err != nil {
				panic(fmt.Sprintf("runtime: for-in claim released narrowed storage: %v", err))
			}
		}()
		for _, item := range iterable.Array() {
			if err := exec.step(); err != nil {
				return NewNil(), false, exec.wrapError(err, stmt.Pos())
			}
			if err := exec.assign(stmt.Target, item, env); err != nil {
				return NewNil(), false, exec.wrapError(err, stmt.Target.Pos())
			}
			val, returned, err := exec.evalStatements(stmt.Body, env)
			if err != nil {
				if errors.Is(err, errLoopBreak) {
					return loopBreakResult(err, mode, last)
				}
				if errors.Is(err, errLoopNext) {
					continue
				}
				return NewNil(), false, err
			}
			if returned {
				return val, true, nil
			}
			last = val
		}
	case KindHash:
		val, returned, err := exec.evalForHash(stmt, env, iterable, last, mode)
		if err != nil {
			return NewNil(), false, err
		}
		if returned {
			return val, true, nil
		}
		last = val
	case KindRange:
		r := iterable.Range()
		if r.Beginless || r.Endless {
			return NewNil(), false, exec.errorAt(stmt.Pos(), "cannot iterate %s", openRangeKindLabel(r))
		}
		if r.Start <= r.End {
			for i := r.Start; rangeLoopAscendingContinues(i, r); i++ {
				if err := exec.step(); err != nil {
					return NewNil(), false, exec.wrapError(err, stmt.Pos())
				}
				if err := exec.assign(stmt.Target, NewInt(i), env); err != nil {
					return NewNil(), false, exec.wrapError(err, stmt.Target.Pos())
				}
				val, returned, err := exec.evalStatements(stmt.Body, env)
				if err != nil {
					if errors.Is(err, errLoopBreak) {
						return loopBreakResult(err, mode, last)
					}
					if errors.Is(err, errLoopNext) {
						continue
					}
					return NewNil(), false, err
				}
				if returned {
					return val, true, nil
				}
				last = val
			}
		} else {
			for i := r.Start; rangeLoopDescendingContinues(i, r); i-- {
				if err := exec.step(); err != nil {
					return NewNil(), false, exec.wrapError(err, stmt.Pos())
				}
				if err := exec.assign(stmt.Target, NewInt(i), env); err != nil {
					return NewNil(), false, exec.wrapError(err, stmt.Target.Pos())
				}
				val, returned, err := exec.evalStatements(stmt.Body, env)
				if err != nil {
					if errors.Is(err, errLoopBreak) {
						return loopBreakResult(err, mode, last)
					}
					if errors.Is(err, errLoopNext) {
						continue
					}
					return NewNil(), false, err
				}
				if returned {
					return val, true, nil
				}
				last = val
			}
		}
	default:
		return NewNil(), false, exec.errorAt(stmt.Pos(), "cannot iterate over %s", iterable.Kind())
	}

	return loopNormalResult(mode, iterable, last), false, nil
}

func (exec *Execution) evalForHash(stmt *ForStmt, env *Env, iterable, last Value, mode loopResultMode) (Value, bool, error) {
	count := iterable.HashLen()
	reservePair := !exec.valueReachableFromLiveBase(iterable, NewNil())
	scratch := sortedHashEntryBufferBytes(count)
	if reservePair {
		scratch = saturatingAdd(scratch, exec.maxCollapsedPairBytes(iterable))
	}
	delta := exec.reserveLoopScratch(scratch)
	defer exec.releaseLoopScratch(delta)
	if !reservePair {
		if err := exec.checkCollapsedPairBytesWithLiveBase(iterable, NewNil()); err != nil {
			return NewNil(), false, err
		}
	}
	if err := exec.checkProjectedHashWalkBytes(iterable, nil, nil, NewNil()); err != nil {
		return NewNil(), false, err
	}
	var entryBuf [smallHashKeyBufferSize]HashEntry
	for _, entry := range iterable.HashEntriesInto(entryBuf[:]) {
		if err := exec.step(); err != nil {
			return NewNil(), false, exec.wrapError(err, stmt.Pos())
		}
		pair := NewArray([]Value{entry.Key, entry.Value})
		if err := exec.assign(stmt.Target, pair, env); err != nil {
			return NewNil(), false, exec.wrapError(err, stmt.Target.Pos())
		}
		val, returned, err := exec.evalStatements(stmt.Body, env)
		if err != nil {
			if errors.Is(err, errLoopBreak) {
				return loopBreakResult(err, mode, last)
			}
			if errors.Is(err, errLoopNext) {
				continue
			}
			return NewNil(), false, err
		}
		if returned {
			return val, true, nil
		}
		last = val
	}
	return loopNormalResult(mode, iterable, last), false, nil
}

func rangeLoopAscendingContinues(value int64, rng Range) bool {
	if rng.Exclusive {
		return value < rng.End
	}
	return value <= rng.End
}

func rangeLoopDescendingContinues(value int64, rng Range) bool {
	if rng.Exclusive {
		return value > rng.End
	}
	return value >= rng.End
}

func (exec *Execution) evalWhileStatement(stmt *WhileStmt, env *Env) (Value, bool, error) {
	return exec.evalWhileLoop(stmt, env, loopStatementResult)
}

func (exec *Execution) evalWhileExpression(stmt *WhileStmt, env *Env) (Value, error) {
	return expressionValueOrReturn(exec.evalWhileLoop(stmt, env, loopExpressionResult))
}

func (exec *Execution) evalWhileLoop(stmt *WhileStmt, env *Env, mode loopResultMode) (Value, bool, error) {
	exec.loopDepth++
	defer func() {
		exec.loopDepth--
	}()

	if stmt.BodyFirst {
		exec.predeclareLocalBindingsFromStatements(stmt.Body, env)
	}
	last := NewNil()
	for {
		if err := exec.step(); err != nil {
			return NewNil(), false, exec.wrapError(err, stmt.Pos())
		}
		condition, err := exec.evalExpression(stmt.Condition, env)
		if err != nil {
			return NewNil(), false, err
		}
		if err := exec.checkMemoryValue(condition); err != nil {
			return NewNil(), false, err
		}
		if !condition.Truthy() {
			return loopNormalResult(mode, NewNil(), last), false, nil
		}
		val, returned, err := exec.evalStatements(stmt.Body, env)
		if err != nil {
			if errors.Is(err, errLoopBreak) {
				return loopBreakResult(err, mode, last)
			}
			if errors.Is(err, errLoopNext) {
				exec.predeclareLocalBindingsFromStatements(stmt.Body, env)
				continue
			}
			return NewNil(), false, err
		}
		if returned {
			return val, true, nil
		}
		last = val
	}
}

func (exec *Execution) evalUntilStatement(stmt *UntilStmt, env *Env) (Value, bool, error) {
	return exec.evalUntilLoop(stmt, env, loopStatementResult)
}

func (exec *Execution) evalUntilExpression(stmt *UntilStmt, env *Env) (Value, error) {
	return expressionValueOrReturn(exec.evalUntilLoop(stmt, env, loopExpressionResult))
}

func (exec *Execution) evalUntilLoop(stmt *UntilStmt, env *Env, mode loopResultMode) (Value, bool, error) {
	exec.loopDepth++
	defer func() {
		exec.loopDepth--
	}()

	if stmt.BodyFirst {
		exec.predeclareLocalBindingsFromStatements(stmt.Body, env)
	}
	last := NewNil()
	for {
		if err := exec.step(); err != nil {
			return NewNil(), false, exec.wrapError(err, stmt.Pos())
		}
		condition, err := exec.evalExpression(stmt.Condition, env)
		if err != nil {
			return NewNil(), false, err
		}
		if err := exec.checkMemoryValue(condition); err != nil {
			return NewNil(), false, err
		}
		if condition.Truthy() {
			return loopNormalResult(mode, NewNil(), last), false, nil
		}
		val, returned, err := exec.evalStatements(stmt.Body, env)
		if err != nil {
			if errors.Is(err, errLoopBreak) {
				return loopBreakResult(err, mode, last)
			}
			if errors.Is(err, errLoopNext) {
				exec.predeclareLocalBindingsFromStatements(stmt.Body, env)
				continue
			}
			return NewNil(), false, err
		}
		if returned {
			return val, true, nil
		}
		last = val
	}
}

func (exec *Execution) evalLocalScopeStatements(stmts []Statement, env *Env) (Value, bool, error) {
	return exec.evalStatements(stmts, env)
}

func predeclareStatementLocalBindings(stmt Statement, env *Env) {
	switch s := stmt.(type) {
	case *AssignStmt:
		predeclareAssignmentLocalBindings(s, env)
	}
}

func (exec *Execution) predeclareStatementPostLocalBindings(stmt Statement, env *Env) {
	if !statementCanPostPredeclareLocalBindings(stmt) {
		return
	}
	exec.predeclareLocalBindingsFromStatements([]Statement{stmt}, env)
}

func (exec *Execution) predeclareLocalBindingsFromStatements(stmts []Statement, env *Env) {
	var collector localBindingCollector
	collectLocalBindingNames(stmts, &collector)
	for _, name := range collector.names {
		if isClassConstantAssignmentName(name, env) {
			continue
		}
		env.PredeclareAssignmentLocal(name)
	}
	exec.chargePredeclareScan(collector.visited)
}

// predeclareScanNodesPerStep amortizes a predeclaration scan over the work it
// does: a node walked -- a statement, a branch or clause wrapper, an assignment
// target -- costs one, and a name costs the bytes hashed to record it. A body
// of a few short statements stays free, which is what ordinary code costs
// today.
const predeclareScanNodesPerStep = 64

// chargePredeclareScan bills the nodes a predeclaration scan walked.
//
// The walk is Go-side work the per-statement charge never sees, and it is not
// proportional to what ran: a compound statement rescans its entire subtree
// after each nested statement completes, and a branch that was not taken is
// scanned every time its enclosing statement finishes. Nested conditionals
// therefore cost O(n^2) host CPU against a quota that only counted the O(n)
// statements executed, and a loop over a large dead branch rescanned it every
// iteration for one step (#27).
//
// The charge lands after the walk because its size is not known before it; a
// single walk is bounded by the compiled source, and it is the repetition that
// the quota now sees.
//
// What does not fill a whole step is carried rather than dropped. One walk per
// rescue clause, or per elsif branch, is many small walks rather than one large
// one, and rounding each down to nothing left a source-limit-sized clause list
// free however often it was rescanned.
//
// It latches rather than returning an error, because several of these scans
// run while an error is already propagating -- a loop unwinding a next, a try
// body about to select a rescue clause -- and replacing that error with a
// quota error there would change which clause runs. The latch leaves the
// in-flight error alone and still ends the execution: step returns the latched
// error before charging anything else.
func (exec *Execution) chargePredeclareScan(visited int) {
	if exec == nil || visited <= 0 {
		return
	}
	exec.predeclareScanDebt += visited
	steps := exec.predeclareScanDebt / predeclareScanNodesPerStep
	if steps <= 0 {
		return
	}
	exec.predeclareScanDebt -= steps * predeclareScanNodesPerStep
	_ = exec.stepN(steps)
}

func statementCanPostPredeclareLocalBindings(stmt Statement) bool {
	switch stmt.(type) {
	case *IfStmt, *ForStmt, *WhileStmt, *UntilStmt, *TryStmt:
		return true
	default:
		return false
	}
}

func predeclareAssignmentLocalBindings(stmt *AssignStmt, env *Env) {
	if env.rebindOuter {
		predeclareTargetBindingNames(stmt.Target, env)
		return
	}
	predeclareDirectAssignmentTargetBindingNames(stmt.Target, stmt.Value, env)
}

type localBindingCollector struct {
	names []string
	seen  map[string]struct{}
	// visited counts the statements the walk touched, which is what the scan
	// costs; the name count is what it produced (see chargePredeclareScan).
	visited int
}

func (c *localBindingCollector) add(name string) {
	if _, ok := c.seen[name]; ok {
		return
	}
	if c.seen == nil {
		c.seen = make(map[string]struct{})
	}
	c.seen[name] = struct{}{}
	c.names = append(c.names, name)
}

func collectLocalBindingNames(stmts []Statement, collector *localBindingCollector) {
	for _, stmt := range stmts {
		collector.visited++
		switch s := stmt.(type) {
		case *AssignStmt:
			collectTargetBindingNames(s.Target, collector)
		case *IfStmt:
			collectLocalBindingNames(s.Consequent, collector)
			for _, branch := range s.ElseIf {
				// The branch itself is a node the walk steps over, and a long
				// run of empty ones costs exactly that and nothing else.
				collector.visited++
				collectLocalBindingNames(branch.Consequent, collector)
			}
			collectLocalBindingNames(s.Alternate, collector)
		case *ForStmt:
			collectTargetBindingNames(s.Target, collector)
			collectLocalBindingNames(s.Body, collector)
		case *WhileStmt:
			collectLocalBindingNames(s.Body, collector)
		case *UntilStmt:
			collectLocalBindingNames(s.Body, collector)
		case *TryStmt:
			collectLocalBindingNames(s.Body, collector)
			for i := range s.Rescues {
				collector.visited++
				collectLocalBindingNames(s.Rescues[i].Body, collector)
			}
			collectLocalBindingNames(s.Else, collector)
			collectLocalBindingNames(s.Ensure, collector)
		}
	}
}

func collectTargetBindingNames(target Expression, collector *localBindingCollector) {
	// A destructuring target is one statement but arbitrarily many names, and
	// walking it is the same per-name work as walking that many statements.
	// Counting only statements left `a0, a1, ... = value` free however wide it
	// was, so a loop could rescan a source-limit-sized target for nothing.
	collector.visited++
	switch t := target.(type) {
	case *Identifier:
		// The name's bytes are work too: the collector hashes it into its seen
		// set and the environment hashes it again when it predeclares it, so a
		// rescan of one source-limit-sized identifier does real work that a
		// count of one node does not see.
		collector.visited += len(t.Name)
		collector.add(t.Name)
	case *DestructureTarget:
		for _, element := range t.Elements {
			collectTargetBindingNames(element.Target, collector)
		}
	}
}

func predeclareTargetBindingNames(target Expression, env *Env) {
	switch t := target.(type) {
	case *Identifier:
		if isClassConstantAssignmentName(t.Name, env) {
			return
		}
		env.PredeclareLocal(t.Name)
	case *DestructureTarget:
		for _, element := range t.Elements {
			predeclareTargetBindingNames(element.Target, env)
		}
	}
}

func predeclareDirectAssignmentTargetBindingNames(target, value Expression, env *Env) {
	switch t := target.(type) {
	case *Identifier:
		if isClassConstantAssignmentName(t.Name, env) {
			return
		}
		env.PredeclareAssignmentLocal(t.Name)
	case *DestructureTarget:
		for _, element := range t.Elements {
			predeclareDirectAssignmentTargetBindingNames(element.Target, value, env)
		}
	}
}

func (exec *Execution) evalMemberAssignment(stmt *AssignStmt, env *Env) (Value, bool, error) {
	target, ok := stmt.Target.(*MemberExpr)
	if !ok {
		return NewNil(), false, nil
	}
	expectation, ok := exec.memberAssignmentValueExpectation(target, stmt.Value, env)
	if !ok {
		return NewNil(), false, nil
	}
	val, err := exec.evalAssignmentValueWithExpectation(stmt, env, expectation)
	if err != nil {
		return NewNil(), true, err
	}
	if err := exec.checkMemoryValue(val); err != nil {
		return NewNil(), true, err
	}
	if err := exec.assign(stmt.Target, val, env); err != nil {
		if errors.Is(err, errStepQuotaExceeded) || errors.Is(err, errMemoryQuotaExceeded) {
			return NewNil(), true, err
		}
		return NewNil(), true, exec.wrapError(err, stmt.Pos())
	}
	return val, true, nil
}

func (exec *Execution) memberAssignmentValueExpectation(target *MemberExpr, value Expression, env *Env) (expressionExpectation, bool) {
	if !memberAssignmentValueCanUseExpectation(value) {
		return expressionExpectation{}, false
	}
	if expectation, ok := exec.staticMemberAssignmentValueExpectation(target, env); ok {
		return expectation, true
	}
	// Assignment evaluates the RHS before the target. Only peek already-bound
	// receivers for side-effect-free RHS shapes, then let assign evaluate the
	// target normally after the value is ready.
	obj, ok := memberAssignmentReceiverValue(target.Object, env)
	if !ok {
		return expressionExpectation{}, false
	}
	expectation := memberSetterValueExpectation(obj, target.Property)
	return expectation, !expectation.empty()
}

func (exec *Execution) staticMemberAssignmentValueExpectation(target *MemberExpr, env *Env) (expressionExpectation, bool) {
	receiver, ok := exec.staticMemberAssignmentReceiverClass(target.Object, env)
	if !ok {
		return expressionExpectation{}, false
	}
	fn := receiver.setter(target.Property)
	if fn == nil {
		return expressionExpectation{}, false
	}
	expectation := setterFunctionValueExpectation(fn)
	return expectation, !expectation.empty()
}

type staticMemberAssignmentReceiver struct {
	class         *ClassDef
	classReceiver bool
}

func (receiver staticMemberAssignmentReceiver) setter(property string) *ScriptFunction {
	if receiver.class == nil {
		return nil
	}
	setterName := property + "="
	if receiver.classReceiver {
		return receiver.class.ClassMethods[setterName]
	}
	return receiver.class.Methods[setterName]
}

func (receiver staticMemberAssignmentReceiver) method(property string) *ScriptFunction {
	if receiver.class == nil {
		return nil
	}
	if receiver.classReceiver {
		return receiver.class.ClassMethods[property]
	}
	return receiver.class.Methods[property]
}

func (exec *Execution) staticMemberAssignmentReceiverClass(expr Expression, env *Env) (staticMemberAssignmentReceiver, bool) {
	switch e := expr.(type) {
	case *Identifier:
		val, ok := env.Get(e.Name)
		if !ok {
			return staticMemberAssignmentReceiver{}, false
		}
		if receiver, ok := staticMemberAssignmentReceiverForValue(val); ok {
			return receiver, true
		}
		if !isStaticZeroArityFunctionReceiver(e, env) {
			return staticMemberAssignmentReceiver{}, false
		}
		classDef, ok := exec.staticClassForType(valueFunction(val).ReturnTy, env)
		if !ok {
			return staticMemberAssignmentReceiver{}, false
		}
		return staticMemberAssignmentReceiver{class: classDef}, true
	case *IvarExpr, *ClassVarExpr:
		val, ok := memberAssignmentReceiverValue(expr, env)
		if !ok {
			return staticMemberAssignmentReceiver{}, false
		}
		return staticMemberAssignmentReceiverForValue(val)
	case *CallExpr:
		classDef, ok := exec.staticCallReturnClass(e, env)
		if !ok {
			return staticMemberAssignmentReceiver{}, false
		}
		return staticMemberAssignmentReceiver{class: classDef}, true
	case *MemberExpr:
		receiver, ok := exec.staticMemberAssignmentReceiverClass(e.Object, env)
		if !ok {
			return staticMemberAssignmentReceiver{}, false
		}
		fn := receiver.method(e.Property)
		if fn == nil {
			return staticMemberAssignmentReceiver{}, false
		}
		classDef, ok := exec.staticClassForType(fn.ReturnTy, env)
		if !ok {
			return staticMemberAssignmentReceiver{}, false
		}
		return staticMemberAssignmentReceiver{class: classDef}, true
	default:
		return staticMemberAssignmentReceiver{}, false
	}
}

func staticMemberAssignmentReceiverForValue(val Value) (staticMemberAssignmentReceiver, bool) {
	switch val.Kind() {
	case KindInstance:
		inst := valueInstance(val)
		if inst == nil || inst.Class == nil {
			return staticMemberAssignmentReceiver{}, false
		}
		return staticMemberAssignmentReceiver{class: inst.Class}, true
	case KindClass:
		classDef := valueClass(val)
		if classDef == nil {
			return staticMemberAssignmentReceiver{}, false
		}
		return staticMemberAssignmentReceiver{class: classDef, classReceiver: true}, true
	default:
		return staticMemberAssignmentReceiver{}, false
	}
}

func (exec *Execution) staticCallReturnClass(call *CallExpr, env *Env) (*ClassDef, bool) {
	switch callee := call.Callee.(type) {
	case *Identifier:
		val, ok := env.Get(callee.Name)
		if !ok || val.Kind() != KindFunction {
			return nil, false
		}
		return exec.staticClassForType(valueFunction(val).ReturnTy, env)
	case *MemberExpr:
		receiver, ok := exec.staticMemberAssignmentReceiverClass(callee.Object, env)
		if !ok {
			return nil, false
		}
		if receiver.classReceiver && callee.Property == "new" {
			return receiver.class, true
		}
		fn := receiver.method(callee.Property)
		if fn == nil {
			return nil, false
		}
		return exec.staticClassForType(fn.ReturnTy, env)
	default:
		return nil, false
	}
}

func (exec *Execution) staticClassForType(ty *TypeExpr, env *Env) (*ClassDef, bool) {
	if ty == nil {
		return nil, false
	}
	if ty.Kind == TypeUnion {
		var match *ClassDef
		for _, option := range ty.Union {
			if option.Kind == TypeNil {
				continue
			}
			classDef, ok := exec.staticClassForType(option, env)
			if !ok {
				return nil, false
			}
			if match != nil && match != classDef {
				return nil, false
			}
			match = classDef
		}
		return match, match != nil
	}
	classDef, ok, err := lookupClassTypeExact(ty, typeContext{
		owner:    exec.script,
		env:      env,
		fallback: exec.root,
	})
	if err != nil || !ok {
		return nil, false
	}
	return classDef, true
}

func memberAssignmentValueCanUseExpectation(expr Expression) bool {
	switch e := expr.(type) {
	case *Identifier, *IvarExpr, *ClassVarExpr, *IntegerLiteral, *FloatLiteral, *StringLiteral, *BoolLiteral, *NilLiteral, *SymbolLiteral, *RegexLiteral:
		return true
	case *ArrayLiteral:
		for _, element := range e.Elements {
			if !memberAssignmentValueCanUseExpectation(element) {
				return false
			}
		}
		return true
	case *HashLiteral:
		for _, pair := range e.Pairs {
			if !memberAssignmentValueCanUseExpectation(pair.Key) || !memberAssignmentValueCanUseExpectation(pair.Value) {
				return false
			}
		}
		return true
	case *ConditionalExpr:
		return memberAssignmentValueCanUseExpectation(e.Condition) &&
			memberAssignmentValueCanUseExpectation(e.Consequent) &&
			memberAssignmentValueCanUseExpectation(e.Alternate)
	case *IfExpr:
		if !memberAssignmentValueCanUseExpectation(e.Condition) ||
			!memberAssignmentValueCanUseExpectation(e.Consequent) {
			return false
		}
		for _, branch := range e.ElseIf {
			if !memberAssignmentValueCanUseExpectation(branch.Condition) ||
				!memberAssignmentValueCanUseExpectation(branch.Result) {
				return false
			}
		}
		return memberAssignmentOptionalValueCanUseExpectation(e.Alternate)
	case *CaseExpr:
		if e.Target != nil && !memberAssignmentValueCanUseExpectation(e.Target) {
			return false
		}
		for _, clause := range e.Clauses {
			for _, value := range clause.Values {
				if !memberAssignmentValueCanUseExpectation(value.Expr) {
					return false
				}
			}
			if !memberAssignmentValueCanUseExpectation(clause.Result) {
				return false
			}
		}
		return memberAssignmentOptionalValueCanUseExpectation(e.ElseExpr)
	default:
		return false
	}
}

func memberAssignmentOptionalValueCanUseExpectation(expr Expression) bool {
	return expr == nil || memberAssignmentValueCanUseExpectation(expr)
}

func memberAssignmentReceiverValue(expr Expression, env *Env) (Value, bool) {
	switch e := expr.(type) {
	case *Identifier:
		val, ok := env.Get(e.Name)
		return val, ok
	case *IvarExpr:
		self, ok := env.Get("self")
		if !ok || self.Kind() != KindInstance {
			return NewNil(), false
		}
		val, ok := valueInstance(self).Ivars[e.Name]
		if !ok {
			return NewNil(), true
		}
		return val, true
	case *ClassVarExpr:
		self, ok := env.Get("self")
		if !ok {
			return NewNil(), false
		}
		switch self.Kind() {
		case KindInstance:
			val, ok := valueInstance(self).Class.ClassVars[e.Name]
			if !ok {
				return NewNil(), true
			}
			return val, true
		case KindClass:
			val, ok := valueClass(self).ClassVars[e.Name]
			if !ok {
				return NewNil(), true
			}
			return val, true
		default:
			return NewNil(), false
		}
	case *IndexExpr:
		// A literal-indexed element of an already-bound container resolves
		// without side effects, so arr[0].cb = five infers the same callable
		// setter expectation as c.cb = five. Dynamic indices stay uninferred.
		return literalIndexReceiverValue(e, env)
	default:
		return NewNil(), false
	}
}

func literalIndexReceiverValue(e *IndexExpr, env *Env) (Value, bool) {
	if len(e.Indices) != 1 {
		return NewNil(), false
	}
	base, ok := memberAssignmentReceiverValue(e.Object, env)
	if !ok {
		return NewNil(), false
	}
	switch idx := e.Indices[0].(type) {
	case *IntegerLiteral:
		if idx.Big != nil {
			// A big index can never resolve; fall to the slow path, which
			// reports the index conversion error.
			return NewNil(), false
		}
		if base.Kind() != KindArray {
			return NewNil(), false
		}
		arr := base.Array()
		i := int(idx.Value)
		if i < 0 {
			i += len(arr)
		}
		if i < 0 || i >= len(arr) {
			return NewNil(), false
		}
		return arr[i], true
	case *StringLiteral:
		if base.Kind() != KindHash && base.Kind() != KindObject {
			return NewNil(), false
		}
		val, ok, err := hashGet(base, NewString(idx.Value))
		if err != nil || !ok {
			return NewNil(), false
		}
		return val, true
	case *SymbolLiteral:
		if base.Kind() != KindHash && base.Kind() != KindObject {
			return NewNil(), false
		}
		val, ok, err := hashGet(base, NewSymbol(idx.Name))
		if err != nil || !ok {
			return NewNil(), false
		}
		return val, true
	default:
		return NewNil(), false
	}
}

func memberSetterValueExpectation(obj Value, property string) expressionExpectation {
	fn := memberSetterFunction(obj, property)
	if fn == nil {
		return expressionExpectation{}
	}
	return setterFunctionValueExpectation(fn)
}

func setterFunctionValueExpectation(fn *ScriptFunction) expressionExpectation {
	for _, param := range fn.Params {
		if param.Kind == ParamNormal {
			return positionalArgumentExpectation(param)
		}
	}
	return expressionExpectation{}
}

func memberSetterFunction(obj Value, property string) *ScriptFunction {
	setterName := property + "="
	switch obj.Kind() {
	case KindInstance:
		return valueInstance(obj).Class.Methods[setterName]
	case KindClass:
		return valueClass(obj).ClassMethods[setterName]
	default:
		return nil
	}
}

func assignmentLocalCallBypassNames(target, value Expression) map[string]struct{} {
	var names map[string]struct{}
	collectAssignmentLocalCallBypassNames(target, value, &names)
	return names
}

func assignmentLocalCallBypassBindings(target, value Expression, env *Env) map[string]*Env {
	names := assignmentLocalCallBypassNames(target, value)
	if len(names) == 0 {
		return nil
	}
	var bindings map[string]*Env
	for name := range names {
		scope, ok := env.lookupBindingScope(name)
		if !ok || scope.callRoot || scope.frozen {
			continue
		}
		if bindings == nil {
			bindings = make(map[string]*Env)
		}
		bindings[name] = scope
	}
	return bindings
}

func collectAssignmentLocalCallBypassNames(target, value Expression, names *map[string]struct{}) {
	switch t := target.(type) {
	case *Identifier:
		if expressionContainsBypassableIdentifierCall(value, t.Name) {
			if *names == nil {
				*names = make(map[string]struct{})
			}
			(*names)[t.Name] = struct{}{}
		}
	case *DestructureTarget:
		for _, element := range t.Elements {
			collectAssignmentLocalCallBypassNames(element.Target, value, names)
		}
	}
}

func expressionContainsBypassableIdentifierCall(expr Expression, name string) bool {
	switch t := expr.(type) {
	case nil, *Identifier, *IntegerLiteral, *FloatLiteral, *StringLiteral, *BoolLiteral, *NilLiteral, *SymbolLiteral, *IvarExpr, *ClassVarExpr:
		return false
	case *ArrayLiteral:
		for _, elem := range t.Elements {
			if expressionContainsBypassableIdentifierCall(elem, name) {
				return true
			}
		}
		return false
	case *HashLiteral:
		for _, pair := range t.Pairs {
			if expressionContainsBypassableIdentifierCall(pair.Key, name) ||
				expressionContainsBypassableIdentifierCall(pair.Value, name) {
				return true
			}
		}
		return false
	case *CallExpr:
		if ident, ok := t.Callee.(*Identifier); ok && callUsesBypassableIdentifierResolution(t) && ident.Name == name {
			return true
		}
		if expressionContainsBypassableIdentifierCall(t.Callee, name) {
			return true
		}
		for _, arg := range t.Args {
			if expressionContainsBypassableIdentifierCall(arg, name) {
				return true
			}
		}
		for _, kwarg := range t.KwArgs {
			if expressionContainsBypassableIdentifierCall(kwarg.Value, name) {
				return true
			}
		}
		return expressionContainsBypassableIdentifierCall(t.Block, name)
	case *MemberExpr:
		return expressionContainsBypassableIdentifierCall(t.Object, name)
	case *ScopeExpr:
		return expressionContainsBypassableIdentifierCall(t.Object, name)
	case *IndexExpr:
		if expressionContainsBypassableIdentifierCall(t.Object, name) {
			return true
		}
		for _, index := range t.Indices {
			if expressionContainsBypassableIdentifierCall(index, name) {
				return true
			}
		}
		return false
	case *DestructureTarget:
		for _, element := range t.Elements {
			if expressionContainsBypassableIdentifierCall(element.Target, name) {
				return true
			}
		}
		return false
	case *SplatArg:
		return expressionContainsBypassableIdentifierCall(t.Value, name)
	case *UnaryExpr:
		return expressionContainsBypassableIdentifierCall(t.Right, name)
	case *BinaryExpr:
		return expressionContainsBypassableIdentifierCall(t.Left, name) ||
			expressionContainsBypassableIdentifierCall(t.Right, name)
	case *ConditionalExpr:
		return expressionContainsBypassableIdentifierCall(t.Condition, name) ||
			expressionContainsBypassableIdentifierCall(t.Consequent, name) ||
			expressionContainsBypassableIdentifierCall(t.Alternate, name)
	case *RescueExpr:
		return expressionContainsBypassableIdentifierCall(t.Body, name) ||
			expressionContainsBypassableIdentifierCall(t.Fallback, name)
	case *IfExpr:
		if expressionContainsBypassableIdentifierCall(t.Condition, name) ||
			expressionContainsBypassableIdentifierCall(t.Consequent, name) {
			return true
		}
		for _, branch := range t.ElseIf {
			if expressionContainsBypassableIdentifierCall(branch.Condition, name) ||
				expressionContainsBypassableIdentifierCall(branch.Result, name) {
				return true
			}
		}
		return expressionContainsBypassableIdentifierCall(t.Alternate, name)
	case *RangeExpr:
		return expressionContainsBypassableIdentifierCall(t.Start, name) ||
			expressionContainsBypassableIdentifierCall(t.End, name)
	case *CaseExpr:
		if expressionContainsBypassableIdentifierCall(t.Target, name) {
			return true
		}
		for _, clause := range t.Clauses {
			for _, value := range clause.Values {
				if expressionContainsBypassableIdentifierCall(value.Expr, name) {
					return true
				}
			}
			if expressionContainsBypassableIdentifierCall(clause.Result, name) {
				return true
			}
		}
		return expressionContainsBypassableIdentifierCall(t.ElseExpr, name)
	case *BlockLiteral:
		if t == nil {
			return false
		}
		for _, param := range t.Params {
			if expressionContainsBypassableIdentifierCall(param.DefaultVal, name) {
				return true
			}
		}
		return statementsContainBypassableIdentifierCall(t.Body, name)
	case *YieldExpr:
		for _, arg := range t.Args {
			if expressionContainsBypassableIdentifierCall(arg, name) {
				return true
			}
		}
		return false
	case *InterpolatedString:
		return stringPartsContainBypassableIdentifierCall(t.Parts, name)
	case *InterpolatedSymbol:
		return stringPartsContainBypassableIdentifierCall(t.Parts, name)
	default:
		return false
	}
}

func callUsesBypassableIdentifierResolution(call *CallExpr) bool {
	return call.Parenthesized || len(call.Args) > 0 || len(call.KwArgs) > 0 || call.Block != nil
}

func stringPartsContainBypassableIdentifierCall(parts []StringPart, name string) bool {
	for _, part := range parts {
		if exprPart, ok := part.(StringExpr); ok && expressionContainsBypassableIdentifierCall(exprPart.Expr, name) {
			return true
		}
	}
	return false
}

func statementsContainBypassableIdentifierCall(statements []Statement, name string) bool {
	for _, stmt := range statements {
		if statementContainsBypassableIdentifierCall(stmt, name) {
			return true
		}
	}
	return false
}

func statementContainsBypassableIdentifierCall(stmt Statement, name string) bool {
	switch t := stmt.(type) {
	case nil:
		return false
	case *ExprStmt:
		return expressionContainsBypassableIdentifierCall(t.Expr, name)
	case *ReturnStmt:
		return expressionContainsBypassableIdentifierCall(t.Value, name)
	case *RaiseStmt:
		return expressionContainsBypassableIdentifierCall(t.Value, name) ||
			expressionContainsBypassableIdentifierCall(t.Message, name)
	case *BreakStmt:
		return expressionContainsBypassableIdentifierCall(t.Value, name)
	case *NextStmt:
		return false
	case *AssignStmt:
		return expressionContainsBypassableIdentifierCall(t.Target, name) ||
			expressionContainsBypassableIdentifierCall(t.Value, name)
	case *IfStmt:
		if expressionContainsBypassableIdentifierCall(t.Condition, name) ||
			statementsContainBypassableIdentifierCall(t.Consequent, name) ||
			statementsContainBypassableIdentifierCall(t.Alternate, name) {
			return true
		}
		for _, branch := range t.ElseIf {
			if expressionContainsBypassableIdentifierCall(branch.Condition, name) ||
				statementsContainBypassableIdentifierCall(branch.Consequent, name) {
				return true
			}
		}
		return false
	case *ForStmt:
		return expressionContainsBypassableIdentifierCall(t.Target, name) ||
			expressionContainsBypassableIdentifierCall(t.Iterable, name) ||
			statementsContainBypassableIdentifierCall(t.Body, name)
	case *WhileStmt:
		return expressionContainsBypassableIdentifierCall(t.Condition, name) ||
			statementsContainBypassableIdentifierCall(t.Body, name)
	case *UntilStmt:
		return expressionContainsBypassableIdentifierCall(t.Condition, name) ||
			statementsContainBypassableIdentifierCall(t.Body, name)
	case *TryStmt:
		for i := range t.Rescues {
			if statementsContainBypassableIdentifierCall(t.Rescues[i].Body, name) {
				return true
			}
		}
		return statementsContainBypassableIdentifierCall(t.Body, name) ||
			statementsContainBypassableIdentifierCall(t.Else, name) ||
			statementsContainBypassableIdentifierCall(t.Ensure, name)
	case *ClassStmt:
		return false
	default:
		return false
	}
}

func (exec *Execution) evalStatements(stmts []Statement, env *Env) (Value, bool, error) {
	exec.pushEnv(env)
	defer exec.popEnv()

	result := NewNil()
	var lastPos Position
	for _, stmt := range stmts {
		lastPos = stmt.Pos()
		if err := exec.step(); err != nil {
			return NewNil(), false, exec.wrapError(err, stmt.Pos())
		}
		val, returned, err := exec.evalStatementWithLocalBindings(stmt, env)
		if err != nil {
			return NewNil(), false, err
		}
		if _, isAssign := stmt.(*AssignStmt); isAssign {
			if err := exec.checkMemory(); err != nil {
				return NewNil(), false, exec.wrapError(err, stmt.Pos())
			}
		} else {
			if err := exec.checkMemoryValue(val); err != nil {
				return NewNil(), false, exec.wrapError(err, stmt.Pos())
			}
		}
		if returned {
			return val, true, nil
		}
		result = val
	}
	if err := exec.checkMemory(); err != nil {
		return NewNil(), false, exec.wrapError(err, lastPos)
	}
	return result, false, nil
}

func (exec *Execution) evalStatementWithLocalBindings(stmt Statement, env *Env) (Value, bool, error) {
	predeclareStatementLocalBindings(stmt, env)
	val, returned, err := exec.evalStatement(stmt, env)
	if err != nil {
		return val, returned, err
	}
	exec.predeclareStatementPostLocalBindings(stmt, env)
	return val, returned, nil
}

func (exec *Execution) evalCompoundAssignment(stmt *AssignStmt, env *Env) (Value, error) {
	if stmt.Operator == tokenAndAssign || stmt.Operator == tokenOrAssign {
		return exec.evalLogicalAssignment(stmt, env)
	}
	target, err := exec.prepareCompoundAssignmentTarget(stmt.Target, env)
	if err != nil {
		return NewNil(), err
	}
	if err := exec.checkMemoryValue(target.current); err != nil {
		return NewNil(), err
	}

	right, err := exec.evalAssignmentValue(stmt, env)
	if err != nil {
		return NewNil(), err
	}
	if err := exec.checkMemoryWith(target.current, right); err != nil {
		return NewNil(), err
	}

	result, err := exec.evalBinaryOperator(stmt.Operator, target.current, right, stmt.Pos())
	if err != nil {
		return NewNil(), err
	}
	if err := exec.checkMemoryValue(result); err != nil {
		return NewNil(), err
	}
	if err := target.assign(result); err != nil {
		return NewNil(), err
	}
	return result, nil
}

func (exec *Execution) evalLogicalAssignment(stmt *AssignStmt, env *Env) (Value, error) {
	target, err := exec.prepareLogicalAssignmentTarget(stmt.Target, env)
	if err != nil {
		return NewNil(), err
	}
	if err := exec.checkMemoryValue(target.current); err != nil {
		return NewNil(), err
	}
	switch stmt.Operator {
	case tokenOrAssign:
		if target.current.Truthy() {
			return target.current, nil
		}
	case tokenAndAssign:
		if !target.current.Truthy() {
			return target.current, nil
		}
	default:
		return NewNil(), exec.errorAt(stmt.Pos(), "unsupported logical assignment operator")
	}

	right, err := exec.evalAssignmentValueWithExpectation(stmt, env, target.expectation)
	if err != nil {
		return NewNil(), err
	}
	if err := exec.checkMemoryWith(target.current, right); err != nil {
		return NewNil(), err
	}
	if err := target.assign(right); err != nil {
		return NewNil(), err
	}
	return right, nil
}

type compoundAssignmentTarget struct {
	current     Value
	assign      func(Value) error
	expectation expressionExpectation
}

func (exec *Execution) prepareLogicalAssignmentTarget(target Expression, env *Env) (compoundAssignmentTarget, error) {
	if ident, ok := target.(*Identifier); ok {
		assignLocal := func(value Value) error {
			env.Assign(ident.Name, value)
			return nil
		}
		assignClassConstant := func(value Value) error {
			return exec.assign(ident, value, env)
		}
		if current, exists := env.getOwn(ident.Name); exists {
			return compoundAssignmentTarget{current: current, assign: assignLocal}, nil
		}
		// Enclosing locals win only within the class-body boundary: a method
		// parameter or block local named LIMIT is the ||= target, but a
		// same-named local OUTSIDE the class body is not, so DEFAULT ||= x in
		// a class body creates the class constant (matching = and +=) instead
		// of clobbering a top-level DEFAULT.
		if env.hasCallLocalBinding(ident.Name) {
			current, exists := env.Get(ident.Name)
			if !exists {
				return compoundAssignmentTarget{}, exec.errorAt(ident.Pos(), "undefined variable %s", ident.Name)
			}
			return compoundAssignmentTarget{current: current, assign: assignLocal}, nil
		}
		if self, ok := classConstantAssignmentSelf(ident.Name, env); ok {
			current, _ := classConstant(self, ident.Name)
			return compoundAssignmentTarget{current: current, assign: assignClassConstant}, nil
		}
		if env.parent != nil && env.parent.hasEnclosingLocalBinding(ident.Name) {
			current, exists := env.Get(ident.Name)
			if !exists {
				return compoundAssignmentTarget{}, exec.errorAt(ident.Pos(), "undefined variable %s", ident.Name)
			}
			return compoundAssignmentTarget{current: current, assign: assignLocal}, nil
		}
		if env.parent != nil && env.parent.hasAmbientAssignmentBinding(ident.Name) {
			current, exists := env.Get(ident.Name)
			if !exists {
				return compoundAssignmentTarget{}, exec.errorAt(ident.Pos(), "undefined variable %s", ident.Name)
			}
			return compoundAssignmentTarget{current: current, assign: assignLocal}, nil
		}
		env.Define(ident.Name, NewNil())
		return compoundAssignmentTarget{current: NewNil(), assign: assignLocal}, nil
	}

	return exec.prepareCompoundAssignmentTarget(target, env)
}

func (exec *Execution) prepareCompoundAssignmentTarget(target Expression, env *Env) (compoundAssignmentTarget, error) {
	switch t := target.(type) {
	case *Identifier:
		current, err := exec.evalExpression(t, env)
		if err != nil {
			return compoundAssignmentTarget{}, err
		}
		assign := func(value Value) error {
			return exec.assign(t, value, env)
		}
		return compoundAssignmentTarget{current: current, assign: assign}, nil
	case *MemberExpr:
		// A compound assignment writes through its receiver just as a plain one
		// does, so it resolves that receiver as an addressable path too.
		// Without this, `h.k += 1` wrote through whatever wrapper the read
		// found and every other binding of h saw it.
		obj, path, resolution, err := exec.resolveMutableTarget(t.Object, env)
		if err != nil {
			return compoundAssignmentTarget{}, err
		}
		if resolution == targetUnresolved {
			obj, err = exec.evalExpressionWithAuto(t.Object, env, true)
			if err != nil {
				return compoundAssignmentTarget{}, err
			}
		}
		if err := exec.checkMemoryValue(obj); err != nil {
			return compoundAssignmentTarget{}, err
		}
		member, err := exec.getPublicMember(obj, t.Property, t.Pos())
		if err != nil {
			return compoundAssignmentTarget{}, err
		}
		current, err := exec.autoInvokeIfNeeded(t, member, obj)
		if err != nil {
			return compoundAssignmentTarget{}, err
		}
		assign := func(value Value) error {
			if resolution != targetAddressed {
				target := obj
				if resolution == targetTemporary {
					// The receiver is a temporary the path walk evaluated;
					// the write must reach nothing a durable slot still names.
					detached, err := exec.detachTemporaryWriteReceiver(obj)
					if err != nil {
						return err
					}
					target = detached
				}
				return exec.assignToEvaluatedMember(t, target, value)
			}
			return exec.writeThroughMutablePath(path, env, func(leaf Value) error {
				return exec.assignToEvaluatedMember(t, leaf, value)
			})
		}
		return compoundAssignmentTarget{
			current:     current,
			assign:      assign,
			expectation: memberSetterValueExpectation(obj, t.Property),
		}, nil
	case *IndexExpr:
		obj, path, resolution, err := exec.resolveMutableTarget(t.Object, env)
		if err != nil {
			return compoundAssignmentTarget{}, err
		}
		if resolution == targetUnresolved {
			obj, err = exec.evalExpressionWithAuto(t.Object, env, true)
			if err != nil {
				return compoundAssignmentTarget{}, err
			}
		}
		if err := exec.checkMemoryValue(obj); err != nil {
			return compoundAssignmentTarget{}, err
		}
		indices, err := exec.evalIndexSelectors(t, obj, env)
		if err != nil {
			return compoundAssignmentTarget{}, err
		}
		current, err := exec.evalIndexValue(t, obj, indices)
		if err != nil {
			return compoundAssignmentTarget{}, err
		}
		assign := func(value Value) error {
			if resolution != targetAddressed {
				target := obj
				if resolution == targetTemporary {
					// The receiver is a temporary the path walk evaluated;
					// the write must reach nothing a durable slot still names.
					detached, err := exec.detachTemporaryWriteReceiver(obj)
					if err != nil {
						return err
					}
					target = detached
				}
				return exec.assignToEvaluatedIndex(t, target, indices, value)
			}
			return exec.writeThroughMutablePath(path, env, func(leaf Value) error {
				return exec.assignToEvaluatedIndex(t, leaf, indices, value)
			})
		}
		return compoundAssignmentTarget{current: current, assign: assign}, nil
	case *IvarExpr, *ClassVarExpr:
		current, err := exec.evalExpression(t, env)
		if err != nil {
			return compoundAssignmentTarget{}, err
		}
		assign := func(value Value) error {
			return exec.assign(t, value, env)
		}
		result := compoundAssignmentTarget{current: current, assign: assign}
		if ivar, ok := t.(*IvarExpr); ok {
			result.expectation = exec.ivarAssignmentValueExpectation(ivar, nil, env)
		}
		return result, nil
	case *DestructureTarget:
		return compoundAssignmentTarget{}, exec.errorAt(t.Pos(), "compound assignment is not supported for destructuring targets")
	default:
		return compoundAssignmentTarget{}, exec.errorAt(target.Pos(), "invalid assignment target")
	}
}

func (exec *Execution) evalIfStatementExpression(stmt *IfStmt, env *Env) (Value, error) {
	return expressionValueOrReturn(exec.evalIfStatement(stmt, env))
}

func (exec *Execution) evalIfStatement(stmt *IfStmt, env *Env) (Value, bool, error) {
	if stmt.ModifierBodyFirst {
		exec.predeclareLocalBindingsFromStatements(stmt.Consequent, env)
		exec.predeclareLocalBindingsFromStatements(stmt.Alternate, env)
	}
	val, err := exec.evalExpression(stmt.Condition, env)
	if returnVal, ok := functionReturnValue(err); ok {
		return returnVal, true, nil
	}
	if err != nil {
		return NewNil(), false, err
	}
	if err := exec.checkMemoryValue(val); err != nil {
		return NewNil(), false, err
	}
	if val.Truthy() {
		if stmt.AlternateFirst {
			exec.predeclareLocalBindingsFromStatements(stmt.Alternate, env)
		}
		return exec.evalStatements(stmt.Consequent, env)
	}
	if !stmt.AlternateFirst {
		exec.predeclareLocalBindingsFromStatements(stmt.Consequent, env)
	}
	for _, clause := range stmt.ElseIf {
		condVal, err := exec.evalExpression(clause.Condition, env)
		if returnVal, ok := functionReturnValue(err); ok {
			return returnVal, true, nil
		}
		if err != nil {
			return NewNil(), false, err
		}
		if err := exec.checkMemoryValue(condVal); err != nil {
			return NewNil(), false, err
		}
		if condVal.Truthy() {
			return exec.evalStatements(clause.Consequent, env)
		}
		exec.predeclareLocalBindingsFromStatements(clause.Consequent, env)
	}
	if len(stmt.Alternate) > 0 {
		return exec.evalStatements(stmt.Alternate, env)
	}
	return NewNil(), false, nil
}

func (exec *Execution) evalStatement(stmt Statement, env *Env) (Value, bool, error) {
	switch s := stmt.(type) {
	case *ExprStmt:
		val, err := exec.evalExpression(s.Expr, env)
		if returnVal, ok := functionReturnValue(err); ok {
			return returnVal, true, nil
		}
		return val, false, err
	case *ReturnStmt:
		if s.Value == nil {
			return NewNil(), true, nil
		}
		val, err := exec.evalExpression(s.Value, env)
		if returnVal, ok := functionReturnValue(err); ok {
			return returnVal, true, nil
		}
		return val, true, err
	case *RaiseStmt:
		return exec.evalRaiseStatement(s, env)
	case *AssignStmt:
		if s.Operator != tokenNone {
			val, err := exec.evalCompoundAssignment(s, env)
			if returnVal, ok := functionReturnValue(err); ok {
				return returnVal, true, nil
			}
			return val, false, err
		}
		if val, handled, err := exec.evalArrayAppendAssignment(s, env); handled || err != nil {
			if returnVal, ok := functionReturnValue(err); ok {
				return returnVal, true, nil
			}
			return val, false, err
		}
		if val, handled, err := exec.evalMemberAssignment(s, env); handled || err != nil {
			if returnVal, ok := functionReturnValue(err); ok {
				return returnVal, true, nil
			}
			return val, false, err
		}
		if val, handled, err := exec.evalIvarAssignment(s, env); handled || err != nil {
			if returnVal, ok := functionReturnValue(err); ok {
				return returnVal, true, nil
			}
			return val, false, err
		}
		val, err := exec.evalAssignmentValue(s, env)
		if returnVal, ok := functionReturnValue(err); ok {
			return returnVal, true, nil
		}
		if err != nil {
			return NewNil(), false, err
		}
		if err := exec.checkMemoryValue(val); err != nil {
			return NewNil(), false, err
		}
		if err := exec.assign(s.Target, val, env); err != nil {
			if errors.Is(err, errStepQuotaExceeded) || errors.Is(err, errMemoryQuotaExceeded) {
				return NewNil(), false, err
			}
			return NewNil(), false, exec.wrapError(err, s.Pos())
		}
		return val, false, nil
	case *IfStmt:
		return exec.evalIfStatement(s, env)
	case *ForStmt:
		return exec.evalForStatement(s, env)
	case *WhileStmt:
		return exec.evalWhileStatement(s, env)
	case *UntilStmt:
		return exec.evalUntilStatement(s, env)
	case *BreakStmt:
		// A block body admits break: it terminates the call the block was
		// passed to, which absorbBlockBreak turns into that call's value. A
		// break that instead crosses a call boundary still reports "break
		// cannot cross call boundary" there.
		if exec.loopDepth == 0 && exec.blockDepth == 0 {
			return NewNil(), false, exec.errorAt(s.Pos(), "%s", breakOutsideLoopMessage())
		}
		if s.Value != nil {
			val, err := exec.evalExpression(s.Value, env)
			if returnVal, ok := functionReturnValue(err); ok {
				return returnVal, true, nil
			}
			if err != nil {
				return NewNil(), false, err
			}
			if err := exec.checkMemoryValue(val); err != nil {
				return NewNil(), false, err
			}
			return NewNil(), false, newLoopBreakValue(val)
		}
		return NewNil(), false, errLoopBreak
	case *NextStmt:
		if exec.loopDepth == 0 && exec.blockDepth == 0 {
			return NewNil(), false, exec.errorAt(s.Pos(), "next used outside of loop")
		}
		if s.Value != nil {
			val, err := exec.evalExpression(s.Value, env)
			if returnVal, ok := functionReturnValue(err); ok {
				return returnVal, true, nil
			}
			if err != nil {
				return NewNil(), false, err
			}
			if err := exec.checkMemoryValue(val); err != nil {
				return NewNil(), false, err
			}
			return NewNil(), false, newLoopNextValue(val)
		}
		return NewNil(), false, errLoopNext
	case *RetryStmt:
		if exec.rescueDepth == 0 {
			return NewNil(), false, exec.errorAt(s.Pos(), "retry used outside of rescue")
		}
		return NewNil(), false, errRescueRetry
	case *TryStmt:
		return exec.evalTryStatement(s, env)
	case *ClassStmt:
		classVal, ok := env.Get(s.Name)
		if !ok {
			return NewNil(), false, exec.errorAt(s.Pos(), "class %s is not bound", s.Name)
		}
		classDef := valueClass(classVal)
		if classDef == nil {
			return NewNil(), false, exec.errorAt(s.Pos(), "%s is not a class", s.Name)
		}
		if err := exec.initializeClassBody(classVal, classDef, env); err != nil {
			return NewNil(), false, err
		}
		return classVal, false, nil
	default:
		return NewNil(), false, exec.errorAt(stmt.Pos(), "unsupported statement")
	}
}

func (exec *Execution) evalAssignmentValue(stmt *AssignStmt, env *Env) (Value, error) {
	return exec.evalAssignmentValueWithExpectation(stmt, env, expressionExpectation{})
}

func (exec *Execution) evalAssignmentValueWithExpectation(stmt *AssignStmt, env *Env, expectation expressionExpectation) (Value, error) {
	bindings := assignmentLocalCallBypassBindings(stmt.Target, stmt.Value, env)
	if len(bindings) == 0 {
		return exec.evalExpressionWithExpectation(stmt.Value, env, expectation)
	}
	exec.localCallBypassStack = append(exec.localCallBypassStack, localCallBypass{bindings: bindings})
	defer func() {
		exec.localCallBypassStack = exec.localCallBypassStack[:len(exec.localCallBypassStack)-1]
	}()
	return exec.evalExpressionWithExpectation(stmt.Value, env, expectation)
}

func (exec *Execution) evalRaiseStatement(stmt *RaiseStmt, env *Env) (Value, bool, error) {
	if stmt.Value != nil {
		if stmt.Message != nil {
			errorType, staticErrorType := raiseErrorTypeName(stmt.Value, env)
			val := NewNil()
			if !staticErrorType {
				var err error
				val, err = exec.evalExpression(stmt.Value, env)
				if err != nil {
					return NewNil(), false, err
				}
			}
			message, err := exec.evalExpression(stmt.Message, env)
			if err != nil {
				return NewNil(), false, err
			}
			if message.Kind() != KindString {
				return NewNil(), false, exec.newRuntimeErrorWithType(runtimeErrorTypeType, "exception message must be string", stmt.Pos())
			}
			if !staticErrorType {
				var ok bool
				errorType, ok = raiseErrorType(val)
				if !ok {
					return NewNil(), false, exec.newRuntimeErrorWithType(runtimeErrorTypeType, "exception class/object expected", stmt.Pos())
				}
			}
			return NewNil(), false, exec.newRuntimeErrorWithType(errorType, message.String(), stmt.Pos())
		}

		val, err := exec.evalExpression(stmt.Value, env)
		if returnVal, ok := functionReturnValue(err); ok {
			return returnVal, true, nil
		}
		if err != nil {
			return NewNil(), false, err
		}
		if val.Kind() != KindString {
			message := "exception class/object expected"
			if val.Kind() == KindNil {
				message = "exception object expected"
			}
			return NewNil(), false, exec.newRuntimeErrorWithType(runtimeErrorTypeType, message, stmt.Pos())
		}
		return NewNil(), false, exec.errorAt(stmt.Pos(), "%s", val.String())
	}

	err := exec.currentRescuedError()
	if err == nil {
		return NewNil(), false, exec.errorAt(stmt.Pos(), "")
	}
	return NewNil(), false, err
}

func raiseErrorType(val Value) (string, bool) {
	if val.Kind() == KindClass {
		if class := valueClass(val); class != nil {
			if kind, ok := ast.CanonicalRuntimeErrorType(class.Name); ok {
				return kind, true
			}
		}
	}
	return "", false
}

func raiseErrorTypeName(expr Expression, env *Env) (string, bool) {
	ident, ok := expr.(*Identifier)
	if !ok || !isConstantIdentifier(ident.Name) {
		return "", false
	}
	if _, ok := env.Get(ident.Name); ok {
		return "", false
	}
	if self, ok := env.Get("self"); ok && (self.Kind() == KindInstance || self.Kind() == KindClass) {
		if _, ok := classConstant(self, ident.Name); ok {
			return "", false
		}
	}
	return ast.CanonicalRuntimeErrorType(ident.Name)
}

func (exec *Execution) evalTryExpression(stmt *TryStmt, env *Env) (Value, error) {
	return expressionValueOrReturn(exec.evalTryStatement(stmt, env))
}

func (exec *Execution) evalTryStatement(stmt *TryStmt, env *Env) (Value, bool, error) {
	for {
		val, returned, err := exec.evalStatements(stmt.Body, env)
		runElse := err == nil && !returned
		exec.predeclareLocalBindingsFromStatements(stmt.Body, env)

		retried := false
		if err != nil {
			// Clauses are selected in source order: the first whose type matches
			// wins, mirroring Ruby's specific-to-general rescue dispatch, and later
			// clauses never see an error an earlier clause matched. A selected
			// clause with an empty body keeps the prior single-clause behavior —
			// the error propagates (after ensure) rather than being swallowed —
			// but it still consumes the match.
			for i := range stmt.Rescues {
				clause := &stmt.Rescues[i]
				if !exec.canRescueRuntimeError(err, clause.Ty) {
					// A skipped clause's body locals must exist (as nil) before a
					// later handler runs: the parser treated its assignments as
					// surrounding-scope locals, so a matching clause reading such a
					// name sees the same nil it would after the block.
					exec.predeclareRescueClauseLocalBindings(clause, env)
					continue
				}
				if len(clause.Body) == 0 {
					break
				}
				rescueEnv := env
				if clause.Binding != "" {
					rescueEnv = newEnv(env)
					rescueEnv.Define(clause.Binding, rescuedErrorValue(err))
				}
				exec.pushRescuedError(err)
				exec.rescueDepth++
				rescueVal, rescueReturned, rescueErr := exec.evalStatements(clause.Body, rescueEnv)
				exec.rescueDepth--
				exec.popRescuedError()
				if rescueEnv != env {
					exec.copyRescueLocalAssignments(clause, rescueEnv, env)
				}
				if isRescueRetrySignal(rescueErr) {
					retried = true
					break
				}
				if rescueErr != nil {
					val = NewNil()
					returned = false
					err = rescueErr
				} else {
					val = rescueVal
					returned = rescueReturned
					err = nil
				}
				break
			}
		}
		if retried {
			// A retry re-runs the begin body without running ensure; charge a
			// step per attempt so a retry storm hits the step quota instead of
			// spinning forever.
			if stepErr := exec.step(); stepErr != nil {
				return NewNil(), false, exec.wrapError(stepErr, stmt.Pos())
			}
			continue
		}
		exec.predeclareRescueLocalBindings(stmt, env)

		if runElse && len(stmt.Else) > 0 {
			val, returned, err = exec.evalStatements(stmt.Else, env)
		}
		exec.predeclareLocalBindingsFromStatements(stmt.Else, env)

		if len(stmt.Ensure) > 0 {
			latchedBeforeEnsure := exec.exhausted != nil
			ensureVal, ensureReturned, ensureErr := exec.evalStatements(stmt.Ensure, env)
			if ensureErr != nil {
				// An execution latched BEFORE the ensure body cannot run it —
				// the first statement charge re-raises the latch — and
				// letting that re-raise replace the propagating error pointed
				// diagnostics at an unexecuted ensure statement, so the
				// original failure stands. Exhaustion first triggered inside
				// the ensure body is the opposite case: the quota kill must
				// replace the body's ordinary error, like any ensure failure.
				if latchedBeforeEnsure && err != nil {
					return NewNil(), false, err
				}
				return NewNil(), false, ensureErr
			}
			if ensureReturned {
				return ensureVal, true, nil
			}
		}

		if err != nil {
			return NewNil(), false, err
		}
		return val, returned, nil
	}
}

func (exec *Execution) copyRescueLocalAssignments(clause *RescueClause, from, to *Env) {
	// The clause counts as a node of its own, so a long list of empty ones is
	// not free to walk however often it is rescanned.
	collector := localBindingCollector{visited: 1}
	collectLocalBindingNames(clause.Body, &collector)
	for _, name := range collector.names {
		if name == clause.Binding {
			continue
		}
		val, ok := from.getOwn(name)
		if !ok {
			continue
		}
		to.PredeclareLocal(name)
		to.Assign(name, val)
	}
	exec.chargePredeclareScan(collector.visited)
}

func (exec *Execution) predeclareRescueLocalBindings(stmt *TryStmt, env *Env) {
	for i := range stmt.Rescues {
		exec.predeclareRescueClauseLocalBindings(&stmt.Rescues[i], env)
	}
}

func (exec *Execution) predeclareRescueClauseLocalBindings(clause *RescueClause, env *Env) {
	collector := localBindingCollector{visited: 1}
	collectLocalBindingNames(clause.Body, &collector)
	for _, name := range collector.names {
		if name == clause.Binding {
			continue
		}
		env.PredeclareLocal(name)
	}
	exec.chargePredeclareScan(collector.visited)
}

func rescuedErrorValue(err error) Value {
	errType := classifyRuntimeErrorType(err)
	message := err.Error()
	codeFrame := ""
	var backtrace []Value

	if runtimeErr, ok := errors.AsType[*RuntimeError](err); ok {
		errType = classifyRuntimeErrorType(runtimeErr)
		message = runtimeErr.Message
		codeFrame = runtimeErr.CodeFrame
		backtrace = runtimeErrorBacktrace(runtimeErr)
	}

	fields := map[string]Value{
		"type":       NewString(errType),
		"class":      NewString(errType),
		"message":    NewString(message),
		"to_s":       NewString(message),
		"code_frame": NewString(codeFrame),
		"backtrace":  NewArray(backtrace),
	}
	return NewTaggedObject(fields, ObjectTagRescuedError, message)
}

func runtimeErrorBacktrace(err *RuntimeError) []Value {
	if err == nil || len(err.Frames) == 0 {
		return nil
	}
	frames := make([]Value, 0, len(err.Frames))
	for _, frame := range err.Frames {
		frames = append(frames, NewString(formatRuntimeBacktraceFrame(frame)))
	}
	return frames
}

func formatRuntimeBacktraceFrame(frame StackFrame) string {
	location := ""
	switch {
	case frame.Source != "" && frame.Pos.Line > 0 && frame.Pos.Column > 0:
		location = fmt.Sprintf("%s:%d:%d", frame.Source, frame.Pos.Line, frame.Pos.Column)
	case frame.Source != "" && frame.Pos.Line > 0:
		location = fmt.Sprintf("%s:%d", frame.Source, frame.Pos.Line)
	case frame.Source != "":
		location = frame.Source
	case frame.Pos.Line > 0 && frame.Pos.Column > 0:
		location = fmt.Sprintf("%d:%d", frame.Pos.Line, frame.Pos.Column)
	case frame.Pos.Line > 0:
		location = fmt.Sprintf("line %d", frame.Pos.Line)
	default:
		location = "<script>"
	}
	return fmt.Sprintf("%s:in `%s`", location, frame.Function)
}

func isLoopControlSignal(err error) bool {
	return errors.Is(err, errLoopBreak) || errors.Is(err, errLoopNext)
}

func isRescueRetrySignal(err error) bool {
	return errors.Is(err, errRescueRetry)
}

func isFunctionReturnSignal(err error) bool {
	_, ok := functionReturnValue(err)
	return ok
}

func isHostControlSignal(err error) bool {
	return errors.Is(err, context.Canceled) ||
		errors.Is(err, context.DeadlineExceeded)
}

func (exec *Execution) canRescueRuntimeError(err error, rescueTy *TypeExpr) bool {
	// A latched execution matches no rescue clause at all: once the budget is
	// genuinely exhausted the script must not absorb its own termination, no
	// matter which error value is propagating or how the clause is typed. The
	// latch, not the error's identity, carries the verdict because wrapError
	// flattens the quota sentinels into ordinary LimitError-classified
	// RuntimeErrors that a forged raise LimitError could imitate, so error-value
	// credentials play no part here (a stale one could be replayed).
	return exec.exhausted == nil &&
		!isLoopControlSignal(err) &&
		!isRescueRetrySignal(err) &&
		!isHostControlSignal(err) &&
		!isNonLocalReturnSignal(err) &&
		!isFunctionReturnSignal(err) &&
		runtimeErrorMatchesRescueType(err, rescueTy)
}

func runtimeErrorMatchesRescueType(err error, rescueTy *TypeExpr) bool {
	if rescueTy == nil {
		runtimeErr, ok := errors.AsType[*RuntimeError](err)
		return ok && classifyRuntimeErrorType(runtimeErr) != runtimeErrorTypeLimit
	}
	errKind := classifyRuntimeErrorType(err)
	return rescueTypeMatchesErrorKind(rescueTy, errKind)
}

func rescueTypeMatchesErrorKind(ty *TypeExpr, errKind string) bool {
	if ty == nil {
		return false
	}
	if ty.Kind == TypeUnion {
		for _, option := range ty.Union {
			if rescueTypeMatchesErrorKind(option, errKind) {
				return true
			}
		}
		return false
	}
	canonical, ok := ast.CanonicalRuntimeErrorType(ty.Name)
	if !ok {
		return false
	}
	if canonical == runtimeErrorTypeBase {
		return true
	}
	if canonical == runtimeErrorTypeStandard {
		return errKind != runtimeErrorTypeLimit
	}
	return canonical == errKind
}
