package runtime

import (
	"context"
	"errors"
	"fmt"
	"maps"
	"reflect"
	"slices"
	"strings"
)

// blockGivenInCurrentCall reports whether the call that owns env was supplied a
// block, mirroring Ruby's block_given?. It returns false at the top level and in
// calls that received no block. The block is read from the enclosing call
// frame's dedicated slot, so a script binding cannot shadow the predicate.
func blockGivenInCurrentCall(env *Env) bool {
	block, ok := env.lookupCallBlock()
	return ok && block.Kind() != KindNil
}

func valueCanContainBuiltins(val Value) bool {
	switch val.Kind() {
	case KindBuiltin, KindArray, KindHash, KindObject, KindClass, KindInstance, KindFunction, KindBlock:
		return true
	default:
		return false
	}
}

func cloneBuiltinSet(src map[*Builtin]struct{}) map[*Builtin]struct{} {
	if len(src) == 0 {
		return make(map[*Builtin]struct{})
	}
	out := make(map[*Builtin]struct{}, len(src))
	for builtin := range src {
		out[builtin] = struct{}{}
	}
	return out
}

// revokedCapabilityBuiltin returns a builtin that fails closed when invoked. The
// inbound rebinder substitutes it for a per-call capability grant a re-entering
// closure captured, so a closure that copied a capability into a local cannot
// reach the originating call's capability from a call that never granted it.
func revokedCapabilityBuiltin(name string) Value {
	return NewBuiltin(name, func(_ *Execution, _ Value, _ []Value, _ map[string]Value, _ Value) (Value, error) {
		return NewNil(), fmt.Errorf("capability %s was not granted to this call", name)
	})
}

// autoInvokeIfNeeded resolves a name that stands for executable code in a
// value position. A zero-argument function and an auto-invoking builtin run
// there, as they always have. Anything else that resolves to executable code
// would have to become a value, which ADR-006 removed, so it is rejected here
// with the direct call as the replacement -- the one place every value-position
// read of a function, method, or capability member passes through.
func (exec *Execution) autoInvokeIfNeeded(expr Expression, val, receiver Value) (Value, error) {
	switch val.Kind() {
	case KindFunction:
		fn := valueFunction(val)
		if fn != nil && len(fn.Params) == 0 {
			return exec.invokeCallable(val, receiver, nil, nil, NewNil(), expr.Pos())
		}
	case KindBuiltin:
		builtin := valueBuiltin(val)
		if builtin != nil && builtin.AutoInvoke {
			return exec.invokeCallable(val, receiver, nil, nil, NewNil(), expr.Pos())
		}
	default:
		return val, nil
	}
	return NewNil(), exec.errorAt(
		expr.Pos(),
		"%s is a %s and cannot be used as a value; call it with %s(...)",
		callableReferenceName(expr),
		callableReferenceKind(val),
		callableReferenceName(expr),
	)
}

// callableReferenceName spells the reference the way the author wrote it, so
// the diagnostic names the call they need rather than an internal symbol.
func callableReferenceName(expr Expression) string {
	switch typed := expr.(type) {
	case *Identifier:
		return typed.Name
	case *MemberExpr:
		return typed.Property
	case *ScopeExpr:
		return typed.Property
	default:
		return "this reference"
	}
}

func callableReferenceKind(val Value) string {
	if val.Kind() == KindFunction {
		return "function"
	}
	return "method"
}

func memberReceiverAutoInvokes(object Expression, property string, env *Env) bool {
	if property == "call" {
		return false
	}
	return !isDynamicCallableMemberReceiver(object, env)
}

func memberCallReceiverAutoInvokes(object Expression, env *Env) bool {
	return !isDynamicCallableMemberReceiver(object, env)
}

func callMemberCallReceiverAutoInvokes(call *CallExpr, object Expression, env *Env) bool {
	if isStoredDataCallReceiver(object, env) {
		return false
	}
	if callHasNoValueArguments(call) && isStaticZeroArityFunctionReceiver(object, env) {
		return false
	}
	return memberCallReceiverAutoInvokes(object, env)
}

func callHasNoValueArguments(call *CallExpr) bool {
	return len(call.Args) == 0 && len(call.KwArgs) == 0
}

func isStaticZeroArityFunctionReceiver(object Expression, env *Env) bool {
	ident, ok := object.(*Identifier)
	if !ok {
		return false
	}
	scope, ok := env.lookupBindingScope(ident.Name)
	if !ok || scope.hasDynamic(ident.Name) {
		return false
	}
	val, ok := env.Get(ident.Name)
	if !ok {
		return false
	}
	fn := valueFunction(val)
	return fn != nil && len(fn.Params) == 0
}

func isStoredDataCallReceiver(object Expression, env *Env) bool {
	switch expr := object.(type) {
	case *IvarExpr, *ClassVarExpr:
		return true
	case *Identifier:
		return isImplicitSelfDataCallReceiver(expr.Name, env)
	default:
		return false
	}
}

func isImplicitSelfDataCallReceiver(name string, env *Env) bool {
	if _, ok := env.lookupBindingScope(name); ok {
		return false
	}
	self, ok := env.Get("self")
	if !ok {
		return false
	}
	switch self.Kind() {
	case KindInstance:
		inst := valueInstance(self)
		if _, ok := inst.Class.Methods[name]; ok || isUniversalDataSafe(name) {
			return false
		}
		_, ok := inst.Ivars[name]
		return ok
	case KindClass:
		classDef := valueClass(self)
		if name == "new" || isUniversalDataSafe(name) {
			return false
		}
		if _, ok := classDef.ClassMethods[name]; ok {
			return false
		}
		_, ok := classDef.ClassVars[name]
		return ok
	default:
		return false
	}
}

func isDynamicCallableMemberReceiver(object Expression, env *Env) bool {
	ident, ok := object.(*Identifier)
	if !ok {
		return false
	}
	scope, ok := env.lookupBindingScope(ident.Name)
	if !ok || !scope.hasDynamic(ident.Name) {
		return false
	}
	val, ok := env.Get(ident.Name)
	return ok && isInvocable(val)
}

func (exec *Execution) invokeCallable(callee, receiver Value, args []Value, kwargs map[string]Value, block Value, pos Position) (Value, error) {
	if err := exec.checkContext(); err != nil {
		return NewNil(), err
	}

	switch callee.Kind() {
	case KindFunction:
		result, err := exec.callFunction(valueFunction(callee), receiver, args, kwargs, block, pos)
		if err != nil {
			if breakVal, absorbed := absorbBlockBreak(err, block); absorbed {
				return exec.validateAbsorbedBreak(valueFunction(callee), breakVal, pos)
			}
			if ok, controlErr := exec.callBoundaryControlError(err, pos); ok {
				return NewNil(), controlErr
			}
			return NewNil(), err
		}
		return result, nil
	case KindBlock:
		if len(kwargs) > 0 {
			for name := range kwargs {
				return NewNil(), exec.errorAt(pos, "unexpected keyword argument %s", name)
			}
		}
		if !block.IsNil() {
			return NewNil(), exec.errorAt(pos, "block.call does not accept a block")
		}
		// Script-to-script invocation: the host boundary in CallBlock is for
		// Go callers only, so direct invocation of a block-valued binding
		// takes the internal path.
		result, err := exec.callBlockValue(callee, args, pos)
		if err != nil {
			if errors.Is(err, errLoopBreak) {
				return NewNil(), exec.localJumpErrorAt(pos, "break cannot cross call boundary")
			}
			if errors.Is(err, errLoopNext) {
				return NewNil(), exec.localJumpErrorAt(pos, "next cannot cross call boundary")
			}
			return NewNil(), err
		}
		return result, nil
	case KindBuiltin:
		builtin := valueBuiltin(callee)
		scope := exec.capabilityContractScopes[builtin]
		var preCallKnownBuiltins map[*Builtin]struct{}
		var callAmbientEnvs map[*Env]struct{}
		if scope != nil && len(scope.contracts) > 0 {
			callAmbientEnvs = ambientEnvSet(exec.root)
			preCallKnownBuiltins = cloneBuiltinSet(scope.knownBuiltins)
			preCallScanner := newCapabilityContractScanner()
			preCallScanner.ambientEnvs = callAmbientEnvs
			if valueCanContainBuiltins(receiver) {
				preCallScanner.collectBuiltins(receiver, preCallKnownBuiltins)
			}
			// A script-supplied block is a closure separate from args/kwargs.
			// Now that block environments are traversed for contract binding,
			// snapshot any builtins it already captured so a capability that
			// returns or stores the same block doesn't treat them as newly
			// published and bind its contract to them.
			if valueCanContainBuiltins(block) {
				preCallScanner.collectBuiltins(block, preCallKnownBuiltins)
				// A break makes the block's value this call's result, and the
				// block can name a global. Globals are bound in the ambient
				// environments the scan above stops at, so snapshot the
				// builtins bound there before the call runs -- afterwards this
				// walk would also see what the call itself published and
				// wrongly exclude it.
				preCallScanner.collectAmbientBuiltins(exec.root, preCallKnownBuiltins)
			}
			for _, arg := range args {
				if !valueCanContainBuiltins(arg) {
					continue
				}
				preCallScanner.collectBuiltins(arg, preCallKnownBuiltins)
			}
			for _, kwarg := range kwargs {
				if !valueCanContainBuiltins(kwarg) {
					continue
				}
				preCallScanner.collectBuiltins(kwarg, preCallKnownBuiltins)
			}
			for _, root := range scope.roots {
				if !valueCanContainBuiltins(root) {
					continue
				}
				preCallScanner.collectBuiltins(root, preCallKnownBuiltins)
			}
		}
		contract, hasContract := exec.capabilityContracts[builtin]
		argsValidated := false
		if hasContract && contract.ValidateArgs != nil {
			if err := contract.ValidateArgs(args, kwargs, block); err != nil {
				return NewNil(), exec.wrapError(err, pos)
			}
			argsValidated = true
		}

		var popValidatedArgs func()
		if argsValidated {
			popValidatedArgs = exec.pushValidatedCapabilityArgs(builtin.Name)
		}
		// Builtins are Go code that may mutate reachable containers through raw
		// slice/map writes the epoch cannot observe per-write, so dispatch bumps
		// the epoch once up front (invalidating any memoized estimator walk from
		// before the call) and counts the call in undeclaredBuiltinDepth, which
		// suppresses memo use for every check that runs while the builtin — or
		// script code it drives — is on the stack. Together they make a stale
		// memo unreachable: no memo from before the call survives the bump, and
		// none is used while the builtin's unobserved writes may be in progress.
		//
		// Both exist for the same reason, so one declaration retires both. A
		// builtin that has promised to write to nothing reachable (see
		// Builtin.declaredNonMutating) cannot leave a memo stale, so there is
		// nothing to invalidate up front and nothing to stand aside for during
		// the call. Skipping only the bump would have been the half of it that
		// happens to help whichever builtins do not run a memory check of their
		// own, which is not a property a host can see or rely on.
		//
		// Left conservative, both still apply: the flag's zero value is the
		// undeclared one, so anything unclassified keeps the old behavior.
		declaredPure := builtin.declaredNonMutating()
		if !declaredPure {
			bumpMutationEpoch()
		}
		// An accumulator-metered section only vouches for the loop that opened
		// it, never for a nested builtin's allocations, so dispatch suspends
		// any active sections for the duration of the call: the callee runs
		// with full periodic memory checks and the caller's sections resume on
		// return (see beginAccumulatorMeteredSection).
		// The plain (non-deferred) restores below rely on there being no
		// recover() around builtin dispatch: a panicking builtin tears down
		// the whole Execution, so no code observes the unrestored counters.
		// If a recover-and-continue path is ever added here, both restores
		// must move into defers or the counters leak.
		savedSections := exec.accumMeteredSections
		exec.accumMeteredSections = 0
		// The return-proof slot is scoped to exactly one builtin invocation:
		// clear it before Fn runs and consume it after, restoring the caller's
		// slot, so a proof recorded by a nested dispatch (or left over from a
		// sibling call) can never vouch for this frame's result. The writes are
		// guarded so the common proof-free path stays write-free, and like the
		// counter restores above they rely on a panicking builtin tearing down
		// the whole Execution.
		savedReturnProof := exec.capabilityReturnProof
		if savedReturnProof.recorded {
			exec.capabilityReturnProof = capabilityReturnProof{}
		}
		// Values this call yields into a script block are bound as each yield
		// is made (see capabilityYieldFrame): the block runs while this call
		// is still on the stack and can invoke what it was just handed, and
		// whatever it retains outlives every exit path below -- including the
		// error returns a script can rescue.
		yieldFrame := exec.pushCapabilityYieldFrame(scope, exec.builtinDepth+1, preCallKnownBuiltins, callAmbientEnvs)
		exec.builtinDepth++
		if !declaredPure {
			exec.undeclaredBuiltinDepth++
		}
		// A builtin that walks an array captures its element header here and
		// keeps reading it while the block it yields to runs. The claim tells
		// an in-place shrink performed by that block to leave that storage
		// alone. Arguments are claimed alongside the receiver because an
		// adapter or global builtin is dispatched without one and drives its
		// block from an argument (see array_shrink.go).
		if builtin.hostDriven {
			var prepErr error
			receiver, args, kwargs, prepErr = exec.prepareHostDrivenCollections(builtin, receiver, args, kwargs)
			if prepErr != nil {
				// Unwind everything dispatch set up above this point: a
				// rescued prep error must not leak the yield frame, the
				// depth counters, the metered-section suspension, the
				// return-proof slot, or the validated-args entry.
				exec.popCapabilityYieldFrame(yieldFrame)
				exec.builtinDepth--
				if !declaredPure {
					exec.undeclaredBuiltinDepth--
				}
				exec.accumMeteredSections = savedSections
				if savedReturnProof.recorded {
					exec.capabilityReturnProof = savedReturnProof
				}
				if popValidatedArgs != nil {
					popValidatedArgs()
				}
				return NewNil(), prepErr
			}
		}
		heldBackings := exec.holdArrayBackings(receiver, args, kwargs, builtin.hostDriven)
		// Record what this frame holds, so a block it drives through CallBlock
		// can be charged for it. The values sit on this frame's Go stack for as
		// long as the body runs, and nothing the estimator walks reaches them.
		prevReceiver, prevArgs, prevKwargs := exec.builtinFrameReceiver, exec.builtinFrameArgs, exec.builtinFrameKwargs
		prevReserved := exec.builtinFrameRootsReserved
		prevHostCrossing := exec.builtinFrameHostCrossing
		exec.builtinFrameHostCrossing = builtin.hostDriven && !builtin.declaredNonRetaining()
		exec.builtinFrameReceiver, exec.builtinFrameArgs, exec.builtinFrameKwargs = receiver, args, kwargs
		// A new frame holds new values, so it reserves its own.
		exec.builtinFrameRootsReserved = false
		// Off unless a test turns it on; with it on, a builtin that declared
		// non-mutation and then wrote to the reachable graph without advancing
		// the epoch panics here rather than silently costing a later check its
		// accuracy.
		contractCheck := exec.beginContractVerification(builtin)
		result, err := builtin.Fn(exec, receiver, args, kwargs, block)
		if err == nil && builtin.hostDriven && !builtin.declaredNonRetaining() {
			result, err = exec.detachHostBuiltinResult(builtin, result)
		}
		if builtin.hostDriven && !builtin.declaredNonMutating() {
			// Mark what the host could have written -- the receiver and the
			// host-visible roots -- immediately, success or error: an
			// installed wrapper must be shared before any later read, write,
			// dispatch, or removal can observe it unmarked. The walk is
			// charged; a markErr outranks nothing the call already returned.
			if markErr := exec.markHostWritableState(receiver); markErr != nil && err == nil {
				result, err = NewNil(), markErr
			}
		}
		contractCheck.check(exec, builtin)
		exec.builtinFrameReceiver, exec.builtinFrameArgs, exec.builtinFrameKwargs = prevReceiver, prevArgs, prevKwargs
		exec.builtinFrameRootsReserved = prevReserved
		exec.builtinFrameHostCrossing = prevHostCrossing
		// Dropping the claims moves any array a shrink narrowed off the storage
		// it gave up, which is the first point that storage can be released.
		// That copy is charged, so it can fail; a failure the call itself did
		// not already have becomes the call's.
		if releaseErr := exec.releaseArrayBackings(heldBackings); releaseErr != nil && err == nil {
			result, err = NewNil(), releaseErr
		}
		exec.builtinDepth--
		if !declaredPure {
			exec.undeclaredBuiltinDepth--
		}
		exec.popCapabilityYieldFrame(yieldFrame)
		// A capability adapter that ignored a quota error from the exported
		// Step/CallBlock surface must not decide this call's outcome: a
		// returned value is rejected (a final-expression adapter call would
		// never reach another charge), and whatever error it returned —
		// swallowed, replaced, aggregated, copied, or tampered — is replaced
		// by an error rebuilt entirely from execution-held state: the class
		// and message from the latch, the location data from the snapshot
		// wrapError captured before any adapter could hold a pointer, which no
		// adapter ever held. Nothing from the propagated object is trusted.
		if exec.exhausted != nil {
			if snapshot := exec.exhaustedWrapped; snapshot != nil {
				err = &RuntimeError{
					Type:      classifyRuntimeErrorType(exec.exhausted),
					Message:   canonicalExhaustionMessage(exec.exhausted),
					CodeFrame: snapshot.CodeFrame,
					Frames:    slices.Clone(snapshot.Frames),
				}
			} else {
				// Nothing has passed through wrapError yet — the exhaustion
				// originated inside this builtin or through the exported
				// Step surface — so build the error at this call site,
				// exactly as wrapError would have; a later wrap refuses to
				// touch an existing RuntimeError, and a frameless one helped
				// nobody. The construction is also snapshotted: if an outer
				// adapter swallows this error, the outer dispatch's rebuild
				// must report this innermost site, not its own.
				built := exec.newRuntimeErrorWithType(classifyRuntimeErrorType(exec.exhausted), canonicalExhaustionMessage(exec.exhausted), pos)
				if re, ok := errors.AsType[*RuntimeError](built); ok {
					snapshot := *re
					snapshot.Frames = slices.Clone(re.Frames)
					exec.exhaustedWrapped = &snapshot
				}
				err = built
			}
		}
		returnProof := exec.capabilityReturnProof
		if returnProof.recorded || savedReturnProof.recorded {
			exec.capabilityReturnProof = savedReturnProof
		}
		exec.accumMeteredSections = savedSections
		if popValidatedArgs != nil {
			popValidatedArgs()
		}
		absorbedBreak := false
		if err != nil {
			breakVal, absorbed := absorbBlockBreak(err, block)
			if !absorbed {
				if ok, controlErr := exec.callBoundaryControlError(err, pos); ok {
					return NewNil(), controlErr
				}
				// A latched exhaustion outranks a cancellation the same
				// adapter may also have caused: the host was promised the
				// quota termination.
				if exec.exhausted == nil {
					if ctxErr := exec.checkContext(); ctxErr != nil {
						return NewNil(), ctxErr
					}
				}
				return NewNil(), exec.wrapError(err, pos)
			}
			// The break becomes this call's result, so it continues down the
			// normal return path rather than returning here. Returning
			// immediately skipped the post-call capability scan below: a
			// builtin that published another builtin and then invoked a block
			// that broke left the published one reachable without its
			// contract, so later calls bypassed the validation the scan exists
			// to attach.
			result = breakVal
			absorbedBreak = true
		}
		if exec.exhausted == nil {
			if err := exec.checkContext(); err != nil {
				return NewNil(), err
			}
		}
		// A bound method preserved as a callable reaches its script function
		// through the auto-builtin instanceMember and classMember build, so a
		// break out of a block it yielded to is absorbed here rather than in
		// the KindFunction branch. Without this the wrapper's declared return
		// type was bypassed: `def invoke(fn: Function); fn { break "wrong" };
		// end` called with `Walker.new().visit`, where visit is `-> int`,
		// returned the string. Direct member dispatch resolves to KindFunction
		// and never took this path, which is why it looked covered.
		var deferredErr error
		if absorbedBreak {
			validated, breakErr := exec.validateAbsorbedBreak(builtin.ReturnTypeTarget, result, pos)
			if breakErr != nil {
				deferredErr = breakErr
			} else {
				result = validated
			}
		}
		if deferredErr == nil && hasContract && contract.ValidateReturn != nil && !returnProof.covers(builtin.Name, result) {
			if err := contract.ValidateReturn(result); err != nil {
				deferredErr = exec.wrapError(err, pos)
			}
		}
		if scope != nil && len(scope.contracts) > 0 {
			postCallScanner := newCapabilityContractScanner()
			postCallScanner.excluded = preCallKnownBuiltins
			postCallScanner.ambientEnvs = callAmbientEnvs
			// Capability methods can lazily publish additional builtins at runtime
			// (e.g. through factory return values or receiver mutation). Re-scan
			// these values so future calls still enforce declared contracts.
			//
			// A rejected result was never accepted, so it is not something
			// this call published.
			//
			// An absorbed break IS scanned. A capability can create a
			// contract-covered builtin and yield it for the block to break
			// with, which makes it the call's result while leaving it absent
			// from preCallKnownBuiltins and unreachable through the receiver,
			// roots, or arguments -- so suppressing the scan let it escape
			// without its contract.
			//
			// A break value the caller already owned is excluded instead by
			// the ambient snapshot taken before dispatch -- but only as far as
			// that snapshot reaches. A host global holding a builtin binds
			// lazily (hostGlobalBindsEagerly is false for anything but
			// immutable data and enums), so at snapshot time it is an
			// unmaterialized binding rather than a builtin, and a break with
			// one still picks up the capability's contract. Materializing
			// every lazy global before each contracted call to close that gap
			// would defeat the point of binding them lazily. Letting a
			// contract-covered builtin escape is the worse failure, so the
			// scan runs.
			//
			// The mutation scans below stay unconditional: those really are
			// this call's doing, and are what a rejected return must not leave
			// unguarded.
			if deferredErr == nil {
				postCallScanner.bindContracts(result, scope, exec.capabilityContracts, exec.capabilityContractScopes)
			}
			// Values this call yielded into the block were already bound
			// above, before any exit path could branch.
			if receiver.Kind() != KindNil {
				postCallScanner.bindContracts(receiver, scope, exec.capabilityContracts, exec.capabilityContractScopes)
			}
			// Methods can mutate sibling scope roots via captured references; refresh
			// all adapter roots so newly exposed builtins also get bound.
			for _, root := range scope.roots {
				postCallScanner.bindContracts(root, scope, exec.capabilityContracts, exec.capabilityContractScopes)
			}
			// Methods can also publish builtins by mutating positional or keyword
			// argument objects supplied by script code.
			for _, arg := range args {
				if !valueCanContainBuiltins(arg) {
					continue
				}
				postCallScanner.bindContracts(arg, scope, exec.capabilityContracts, exec.capabilityContractScopes)
			}
			for _, kwarg := range kwargs {
				if !valueCanContainBuiltins(kwarg) {
					continue
				}
				postCallScanner.bindContracts(kwarg, scope, exec.capabilityContracts, exec.capabilityContractScopes)
			}
		}
		// A rejected return value does not un-publish what the call already
		// made reachable. Returning before the scan left a builtin published
		// by the call bound to no contract, so script code could rescue the
		// validation error and then invoke it without its contract.
		if deferredErr != nil {
			return NewNil(), deferredErr
		}
		return result, nil
	default:
		return NewNil(), exec.errorAt(pos, "attempted to call non-callable value")
	}
}

func (exec *Execution) callBoundaryControlError(err error, pos Position) (bool, error) {
	if errors.Is(err, errLoopBreak) {
		return true, exec.localJumpErrorAt(pos, "break cannot cross call boundary")
	}
	if errors.Is(err, errLoopNext) {
		return true, exec.localJumpErrorAt(pos, "next cannot cross call boundary")
	}
	if errors.Is(err, errRescueRetry) {
		return true, exec.localJumpErrorAt(pos, "retry cannot cross call boundary")
	}
	return false, nil
}

func (exec *Execution) callFunction(fn *ScriptFunction, receiver Value, args []Value, kwargs map[string]Value, block Value, pos Position) (Value, error) {
	return exec.callFunctionWithReturnValidation(fn, receiver, args, kwargs, block, pos, true)
}

func (exec *Execution) callFunctionIgnoringReturn(fn *ScriptFunction, receiver Value, args []Value, kwargs map[string]Value, block Value, pos Position) (Value, error) {
	return exec.callFunctionWithReturnValidation(fn, receiver, args, kwargs, block, pos, false)
}

func (exec *Execution) callFunctionWithReturnValidation(fn *ScriptFunction, receiver Value, args []Value, kwargs map[string]Value, block Value, pos Position, validateReturn bool) (Value, error) {
	fn = bindMethodForCall(fn, receiver)
	callEnv := exec.acquireCallEnv(fn, len(fn.Params)+1)
	if receiver.Kind() != KindNil {
		callEnv.Define("self", receiver)
	}
	callEnv.setCallBlock(block)
	// The invocation token opens before argument binding so a block built by a
	// default-argument expression homes to this invocation, not to the caller.
	token := exec.pushReturnToken()
	var val Value
	returned := false
	bindReturned := false
	if err := exec.bindFunctionArgs(fn, callEnv, args, kwargs, pos); err != nil {
		sig := matchNonLocalReturn(err, token)
		if sig == nil {
			exec.popReturnToken()
			return NewNil(), err
		}
		// A default-argument block returned from this invocation during
		// binding; that is the method's return value.
		val = sig.value
		returned = true
		bindReturned = true
	}
	return exec.callFunctionWithBoundEnv(fn, receiver, callEnv, pos, validateReturn, token, val, returned, bindReturned)
}

func (exec *Execution) callFunctionWithSingleNormalArg(fn *ScriptFunction, receiver, arg Value, pos Position, validateReturn bool) (Value, error) {
	fn = bindMethodForCall(fn, receiver)
	param := fn.Params[0]
	callEnv := exec.acquireCallEnv(fn, len(fn.Params)+1)
	if receiver.Kind() != KindNil {
		callEnv.Define("self", receiver)
	}
	callEnv.setCallBlock(NewNil())

	token := exec.pushReturnToken()
	if err := exec.bindFunctionParamValue(fn, callEnv, param, arg, pos); err != nil {
		exec.popReturnToken()
		return NewNil(), err
	}

	return exec.callFunctionWithBoundEnv(fn, receiver, callEnv, pos, validateReturn, token, Value{}, false, false)
}

func (exec *Execution) callFunctionWithBoundEnv(fn *ScriptFunction, receiver Value, callEnv *Env, pos Position, validateReturn bool, token uint64, val Value, returned, skipBody bool) (Value, error) {
	// Script re-entry runs with full periodic memory checks: suspend any
	// accumulator-metered sections a calling builtin left active for the
	// duration of the body (see beginAccumulatorMeteredSection).
	if exec.accumMeteredSections != 0 {
		savedSections := exec.accumMeteredSections
		exec.accumMeteredSections = 0
		defer func() { exec.accumMeteredSections = savedSections }()
	}
	if !skipBody {
		exec.pushEnv(callEnv)
		if err := exec.checkMemory(); err != nil {
			exec.popEnv()
			exec.popReturnToken()
			return NewNil(), err
		}
		exec.popEnv()
	}
	if err := exec.pushFrame(fn.Name, pos, exec.currentSourceScript(), fn.owner); err != nil {
		exec.popReturnToken()
		return NewNil(), err
	}

	ctx := moduleContext{}
	if fn.owner != nil {
		ctx = moduleContext{
			key:    fn.owner.moduleKey,
			path:   fn.owner.modulePath,
			root:   fn.owner.moduleRoot,
			script: fn.owner,
		}
	}
	exec.pushModuleContext(ctx)
	exec.pushReceiver(receiver)
	var err error
	if !skipBody {
		if fn.Accessor == functionAccessorSetter {
			val, err := exec.executeGeneratedSetter(fn, callEnv)
			exec.popReturnToken()
			exec.popReceiver()
			exec.popModuleContext()
			exec.popFrame()
			if err != nil {
				return NewNil(), err
			}
			// The setter body assigns an ivar on the receiver and returns the
			// value; it never captures callEnv, so a reuse-eligible frame can be
			// pooled here just as on the main return path below.
			if fn.reuseCallEnv {
				exec.recycleCallEnv(callEnv)
			}
			return val, nil
		}
		val, returned, err = exec.evalLocalScopeStatements(fn.Body, callEnv)
		val, returned, err = consumeFunctionReturnSignal(val, returned, err)
	}
	exec.popReturnToken()
	if sig := matchNonLocalReturn(err, token); sig != nil {
		// A block created by this invocation returned: that is this method's
		// return value, flowing through the same return-type validation below.
		val = sig.value
		returned = true
		err = nil
	}
	if err != nil && !isLoopControlSignal(err) && !isRescueRetrySignal(err) && !isNonLocalReturnSignal(err) {
		err = exec.wrapError(err, pos)
	}
	exec.popReceiver()
	exec.popModuleContext()
	exec.popFrame()
	if err != nil {
		return NewNil(), err
	}
	// Settle before the value escapes the call: a closure created by the body
	// can keep callEnv (and the accumulator registration) alive after return,
	// and a later fast-path concat must not append into the escaped backing.
	val = callEnv.settleArrayAppendResult(val)
	// The frame is dead once its result has settled: nothing below reads callEnv
	// (return-type normalization uses fn.Env), so a reuse-eligible frame can go
	// back to the pool. reuseCallEnv guarantees the body could not capture it.
	if fn.reuseCallEnv {
		exec.recycleCallEnv(callEnv)
	}
	if err := exec.checkContext(); err != nil {
		return NewNil(), err
	}
	if validateReturn && fn.ReturnTy != nil {
		normalized, err := normalizeValueForType(val, fn.ReturnTy, typeContext{
			owner:    fn.owner,
			env:      fn.Env,
			fallback: exec.root,
			exec:     exec,
		})
		if err != nil {
			if isHostControlSignal(err) {
				return NewNil(), err
			}
			if isNormalizationLimitError(err) {
				return NewNil(), exec.wrapError(err, pos)
			}
			return NewNil(), exec.errorAt(pos, "%s", formatReturnTypeMismatch(fn.Name, err))
		}
		val = normalized
	}
	if returned {
		return val, nil
	}
	return val, nil
}

func consumeFunctionReturnSignal(val Value, returned bool, err error) (Value, bool, error) {
	if returnVal, ok := functionReturnValue(err); ok {
		return returnVal, true, nil
	}
	return val, returned, err
}

type callFunctionRebinder struct {
	script        *Script
	root          *Env
	callClasses   map[string]*ClassDef
	callEnums     map[string]*EnumDef
	seenFunctions map[*ScriptFunction]*ScriptFunction
	seenInstances map[*Instance]Value
	// seenArrays caches rebound KindArray values keyed on the source array's
	// wrapper identity. Aliases of one mutable array rebind to one shared
	// object (so an in-place push through one alias stays visible through the
	// others) while independently constructed arrays -- including distinct
	// empties -- rebind to distinct objects. Keying on the element backing
	// would collapse distinct empty arrays onto one rebound object.
	seenArrays map[uintptr]Value
	// seenHashes caches rebound KindHash values keyed on the source hash's wrapper
	// identity. A hash reachable through several paths in the inbound graph rebinds
	// to one wrapper and keeps its identity, so a bound predicate rebound to that
	// same wrapper still reports identity against the rebound receiver. Keying on
	// the entry map alone would rebuild a fresh wrapper per path and break identity,
	// since hash identity is the wrapper.
	seenHashes map[uintptr]Value
	// seenHashEntries caches the rebound entry map keyed on the source hash's entry
	// map pointer. Two distinct hash wrappers may intentionally share one mutable
	// entry map (a host can build `a := NewHash(shared); b := NewHash(shared)`);
	// index assignment mutates that map in place, so a callee that does `a[:x] = 1`
	// must see the write through b. The wrapper cache cannot preserve this -- the
	// two wrappers have distinct identities -- so the entry-map cache lets both
	// rebound wrappers point at one cloned entry map and keep the aliasing.
	seenHashEntries map[uintptr]map[string]Value
	// sharedEntryOwners records the first rebound wrapper built over each cloned
	// entry map, keyed on that map. Collections are values, so a second wrapper
	// over one map must not let a write through either be seen through the
	// other: both are marked shared the moment the second appears, which makes
	// each of their writes copy the map first. The storage stays deduplicated --
	// the estimator still measures one map -- while the aliasing stops being
	// observable, which is the division ADR-006 item 2 draws.
	sharedEntryOwners map[uintptr]Value
	// deferredGlobalIDs holds the source identities of composite globals bound
	// lazily. Their env slot exists from call setup while their clone is only
	// built on first read, so the slot would go uncounted until then and a
	// write through an argument naming the same source would run in place
	// before the global ever materialized. Registering a clone for one of these
	// sources marks it shared immediately, which closes that window.
	deferredGlobalIDs map[uintptr]struct{}
	// seenMaps caches rebound object entry maps by source map and tag.
	//
	// Wrappers sharing one entry map normally rebind to one shared clone, so
	// an in-place write through either stays visible through the other. That
	// is wrong when their tags differ: a tagged bag is immutable, and sharing
	// entries with an untagged alias would let a write through the alias
	// change what the tagged one renders.
	//
	// The tag is part of the key rather than a filter on a single entry so
	// that every distinct wrapper still terminates a cycle. Caching only the
	// first wrapper left the other one uncached, and a cyclic map reachable
	// through both recursed until the stack ran out.
	seenMaps map[objectCloneKey]map[string]Value
	// seenMapPtrs answers the coarser "has this entry map been cloned at all"
	// question, for callers that hold only the pointer and cannot name a tag.
	seenMapPtrs map[uintptr]struct{}
	seenBlocks  map[*Block]Value
	seenEnvs    map[*Env]*Env
	// seenBoundBuiltins caches the rebound clone of a receiver-bound predicate
	// (a bound eql?/equal?) keyed on the source builtin pointer. Rebinding such a
	// builtin reconstructs a fresh *Builtin around the rebound receiver, so the
	// same source builtin reached through two paths (two globals, two array slots)
	// would otherwise produce two distinct clones. equal? compares builtins by
	// backing pointer, so those distinct clones would wrongly report not-identical;
	// caching the clone keeps aliases of one bound predicate identical across the
	// host boundary.
	seenBoundBuiltins map[*Builtin]Value
	// inboundDataFast marks a call whose positional and keyword arguments were
	// verified data-only and alias-free by scanInboundCallValues, so top-level
	// inbound composites may deep-copy through the tight data copier instead
	// of the full rebind walk. See rebindInboundValue.
	inboundDataFast bool
	// inboundRegister makes the tight copier register every copied composite
	// in the seen-maps above. bindGlobalsForCall sets it when the call defers
	// composite globals, whose deferred scan and slow-path materialization
	// resolve global-aliases-argument cases through those registrations.
	inboundRegister bool
	// pendingGlobalSources collects the deferred composite global sources
	// until the first one is read; rebindGlobalValue then scans them all at
	// once (so aliasing among globals, or against already-rebound values, is
	// detected) and caches the verdict in globalsDataFast. A call that never
	// reads a deferred global never scans them.
	pendingGlobalSources []Value
	globalsScanned       bool
	globalsDataFast      bool
	// exec, when set, receives host-state root registrations for lazily
	// materialized builtin-bearing globals; the eager path records at bind.
	exec *Execution
}

func newCallFunctionRebinder(script *Script, root *Env, callClasses map[string]*ClassDef, callEnums map[string]*EnumDef) *callFunctionRebinder {
	return &callFunctionRebinder{
		script:      script,
		root:        root,
		callClasses: callClasses,
		callEnums:   callEnums,
	}
}

func (r *callFunctionRebinder) class(name string) (*ClassDef, bool) {
	if r.root.declarations != nil {
		return r.root.declarations.class(r.root, name)
	}
	classDef, ok := r.callClasses[name]
	return classDef, ok
}

func (r *callFunctionRebinder) enum(name string) (*EnumDef, bool) {
	if r.root.declarations != nil {
		return r.root.declarations.enum(r.root, name)
	}
	enumDef, ok := r.callEnums[name]
	return enumDef, ok
}

func (r *callFunctionRebinder) rebindValue(val Value) Value {
	switch val.Kind() {
	case KindBuiltin:
		builtin := valueBuiltin(val)
		if builtin == nil {
			return val
		}
		// A receiver-bound predicate (a bound eql?/equal?) rebinds to the rebound
		// clone of its captured receiver. The receiver flows through the same
		// rebinder, so it dedups with the same receiver appearing elsewhere in the
		// inbound graph and the rebound predicate reports identity against it. The
		// clone is reserved and cached before the receiver rebinds, so a receiver
		// graph that reaches the predicate bound to it (for example `[p, a]` where
		// `a[0]` is the same `p = a.eql?`) dedups against the reserved clone instead
		// of minting a second one the outer call would then overwrite — which would
		// make the callee observe arg[0].equal?(arg[1][0]) == false even though the
		// inbound graph held one predicate object. Left unchanged, the predicate
		// would keep comparing against the receiver's pre-rebind wrapper while the
		// receiver passed alongside rebinds to a fresh one.
		if builtin.BoundReceiver != nil {
			if clone, ok := r.seenBoundBuiltins[builtin]; ok {
				return clone
			}
			clone, clonedCell := builtin.BoundReceiver.reserve()
			if r.seenBoundBuiltins == nil {
				r.seenBoundBuiltins = make(map[*Builtin]Value)
			}
			r.seenBoundBuiltins[builtin] = clone
			reboundReceiver := r.rebindValue(builtin.BoundReceiver.receiver.value)
			setBoundReceiver(valueBuiltin(clone), clonedCell, reboundReceiver)
			return clone
		}
		// A capability copied into a local (for example `cap = jobs` captured by a
		// Hash.new default proc) would otherwise survive re-rooting and stay
		// callable, letting a missing-key lookup invoke a capability the re-entering
		// call never granted -- the ambient root is re-rooted but a local snapshot
		// bypasses that lookup. Revoke the captured grant so invoking it fails
		// closed; a free reference to the live capability global still resolves
		// through the re-rooted ambient root. All other builtins are preserved
		// unchanged.
		if !builtin.Capability {
			return val
		}
		return revokedCapabilityBuiltin(builtin.Name)
	case KindInstance:
		inst := valueInstance(val)
		if inst == nil || inst.Class == nil || inst.Class.owner != r.script {
			return val
		}
		if clone, ok := r.seenInstances[inst]; ok {
			return clone
		}
		reboundClass, ok := r.class(inst.Class.Name)
		if !ok {
			return val
		}
		clonedIvars := make(map[string]Value, len(inst.Ivars))
		cloned := NewInstance(&Instance{Class: reboundClass, Ivars: clonedIvars})
		if r.seenInstances == nil {
			r.seenInstances = make(map[*Instance]Value)
		}
		r.seenInstances[inst] = cloned
		for name, ivar := range inst.Ivars {
			rebound := r.rebindValue(ivar)
			publishCollection(rebound)
			clonedIvars[name] = rebound
		}
		return cloned
	case KindClass:
		classDef := valueClass(val)
		if classDef == nil || classDef.owner != r.script {
			return val
		}
		if rebound, ok := r.class(classDef.Name); ok {
			return NewClass(rebound)
		}
		return val
	case KindEnum:
		enumDef := valueEnum(val)
		if enumDef == nil || enumDef.owner != r.script {
			return val
		}
		if rebound, ok := r.enum(enumDef.Name); ok {
			return NewEnum(rebound)
		}
		return val
	case KindEnumValue:
		member := valueEnumValue(val)
		if member == nil || member.Enum == nil || member.Enum.owner != r.script {
			return val
		}
		if reboundEnum, ok := r.enum(member.Enum.Name); ok {
			if reboundMember, ok := reboundEnum.Members[member.Name]; ok {
				return NewEnumValue(reboundMember)
			}
			if reboundMember, ok := reboundEnum.MembersByKey[member.Symbol]; ok {
				return NewEnumValue(reboundMember)
			}
		}
		return val
	case KindFunction:
		fn := valueFunction(val)
		if fn == nil || fn.owner != r.script || fn.Env == r.root {
			return val
		}
		if clone, ok := r.seenFunctions[fn]; ok {
			return NewFunction(clone)
		}
		clone := cloneFunctionForEnv(fn, r.root)
		if r.seenFunctions == nil {
			r.seenFunctions = make(map[*ScriptFunction]*ScriptFunction)
		}
		r.seenFunctions[fn] = clone
		return NewFunction(clone)
	case KindBlock:
		// A block (e.g. a hash default proc) that escaped a prior call and is
		// passed back in must resolve globals, capabilities, per-call function
		// clones, and builtins against the live call root, not the stale snapshot
		// captured when it escaped -- otherwise a missing-key lookup could read a
		// previous call's globals or invoke a capability the current call never
		// granted. Re-root only the ambient root of its captured environment onto
		// the current call, preserving any local frames the block legitimately
		// closed over (e.g. a `prefix` parameter of the function that produced the
		// hash). Block parameters (e.g. the hash and key) bind at call time and
		// are unaffected.
		blk := valueBlock(val)
		if blk == nil || blk.owner != r.script || blk.Env == r.root {
			return val
		}
		if clone, ok := r.seenBlocks[blk]; ok {
			return clone
		}
		clone := *blk
		clone.Env = r.rebindCapturedEnv(blk.Env)
		cloneVal := wrapBlock(&clone)
		if r.seenBlocks == nil {
			r.seenBlocks = make(map[*Block]Value)
		}
		r.seenBlocks[blk] = cloneVal
		return cloneVal
	case KindArray:
		items := val.Array()
		id := arrayIdentity(val)
		if clone, seen := r.seenArrays[id]; seen {
			// A second root is about to name this clone, so a write through
			// either must copy first.
			clone.MarkSharedRef()
			return clone
		}
		clonedItems := make([]Value, len(items))
		clonedArray := NewArray(clonedItems)
		if r.seenArrays == nil {
			r.seenArrays = make(map[uintptr]Value)
		}
		r.seenArrays[id] = clonedArray
		r.noteDeferredGlobalClone(id, clonedArray)
		for i := range items {
			clonedItems[i] = r.rebindValue(items[i])
		}
		publishCollectionElems(clonedItems)
		return clonedArray
	case KindHash:
		id := hashIdentity(val)
		if id != 0 {
			if clone, seen := r.seenHashes[id]; seen {
				clone.MarkSharedRef()
				return clone
			}
		}
		entries := val.HashEntryMap()
		entriesPtr := reflect.ValueOf(entries).Pointer()
		// A distinct wrapper that shares this entry map already cloned it;
		// reuse that cloned map so both rebound wrappers mutate one map in
		// place and the host's intentional aliasing survives rebinding. The
		// shared map is already fully populated, so skip the fill loop -- only
		// a fresh wrapper is built around it.
		sharedEntries, sharedSeen := r.seenHashEntries[entriesPtr]
		clonedEntries := sharedEntries
		if !sharedSeen {
			clonedEntries = make(map[string]Value, val.HashLen())
		}
		// HashKeyOrder is retained as this wrapper's order, so the inbound
		// copy does not allocate a transient []HashEntry beside the clone.
		cloned := newHashWithOrder(clonedEntries, val.HashKeyOrder())
		// Register the wrapper before rebinding entries so a hash that contains
		// itself rebinds against this clone rather than recursing forever or
		// rebinding a second wrapper.
		if id != 0 {
			if r.seenHashes == nil {
				r.seenHashes = make(map[uintptr]Value)
			}
			r.seenHashes[id] = cloned
			r.noteDeferredGlobalClone(id, cloned)
		}
		if !sharedSeen && entriesPtr != 0 {
			if r.seenHashEntries == nil {
				r.seenHashEntries = make(map[uintptr]map[string]Value)
			}
			r.seenHashEntries[entriesPtr] = clonedEntries
		}
		if !sharedSeen {
			for key, item := range entries {
				clonedEntries[key] = r.rebindValue(item)
			}
		}
		r.shareEntryMapOwners(clonedEntries, cloned)
		return cloned
	case KindObject:
		entries := val.HashEntryMap()
		ptr := reflect.ValueOf(entries).Pointer()
		key := objectCloneKey{ptr: ptr, tag: val.ObjectTag()}
		if cloneMap, seen := r.seenMaps[key]; seen {
			reused := retagClonedObject(val, cloneMap)
			r.shareEntryMapOwners(cloneMap, reused)
			return reused
		}
		clonedEntries := make(map[string]Value, len(entries))
		if r.seenMaps == nil {
			r.seenMaps = make(map[objectCloneKey]map[string]Value)
			r.seenMapPtrs = make(map[uintptr]struct{})
		}
		r.seenMaps[key] = clonedEntries
		r.seenMapPtrs[ptr] = struct{}{}
		for key, item := range entries {
			clonedEntries[key] = r.rebindValue(item)
		}
		bag := retagClonedObject(val, clonedEntries)
		r.shareEntryMapOwners(clonedEntries, bag)
		return bag
	default:
		return val
	}
}

// rebindCapturedEnv re-roots the captured environment of an escaped closure onto
// the current call. A closure that escaped a prior Script.Call captures a chain
// of local frames (e.g. the parameters of the function that produced it) that
// bottoms out in the originating call's ambient root (globals, capabilities,
// per-call function clones). Only that ambient root is stale; the local frames
// hold values the closure legitimately closed over and must be preserved. Each
// local frame is cloned so the live call cannot mutate the escaped closure's
// captured state, its bound values are rebound (they may reference per-call
// functions, classes, or further escaped closures), and the deepest local
// frame's parent is re-rooted onto the current call root. If the closure captured
// the ambient root directly (no local frames), the current root replaces it.
func (r *callFunctionRebinder) rebindCapturedEnv(env *Env) *Env {
	// Re-root at the originating call's ambient root (and discard the builtin
	// proto beneath it): the live call root carries the current globals,
	// capabilities, per-call function clones, and chains to the live proto.
	if env == nil || env.callRoot {
		return r.root
	}
	if clone, ok := r.seenEnvs[env]; ok {
		return clone
	}
	clone := newEnvWithCapacity(nil, env.dynamicLen())
	clone.assignBoundary = env.assignBoundary
	clone.rebindOuter = env.rebindOuter
	// See cloneEnvForHost: the class-body marker bounds constant lookup, so a
	// re-entering closure that lost it resolves past its class into the
	// rebound outer frames.
	clone.classBody = env.classBody
	if r.seenEnvs == nil {
		r.seenEnvs = make(map[*Env]*Env)
	}
	r.seenEnvs[env] = clone
	clone.parent = r.rebindCapturedEnv(env.parent)
	env.rangeDynamicBindings(func(name string, val Value) {
		clone.Define(name, r.rebindValue(val))
	})
	env.rangeStaticBindings(func(name string, val Value) {
		clone.DefineStatic(name, r.rebindValue(val))
	})
	// A call frame captured by an escaped closure carries the block its method
	// received; preserve and rebind it so a re-entering closure's yield or
	// block_given? still resolves to that block re-rooted onto the live call.
	if env.hasCallBlock {
		clone.setCallBlock(r.rebindValue(env.callBlock))
	}
	return clone
}

// rebindInboundValue rebinds one top-level inbound host value (a positional
// or keyword argument). When the pre-scan proved the argument set data-only
// and alias-free, composites skip the rebinder walk and deep-copy through the
// tight data copier — the copy is still a full deep copy, so script-side
// mutators never write into host memory. A value the slow path already
// rebound (a capability adapter published the same composite during binding)
// falls back to the rebinder so aliases keep deduplicating to one shared
// clone.
func (r *callFunctionRebinder) rebindInboundValue(val Value) Value {
	if r.inboundDataFast && r.inboundValueUnseen(val) {
		return r.fastCopyInbound(val)
	}
	return r.rebindValue(val)
}

// rebindGlobalValue materializes one deferred composite global. The first
// read scans every deferred global source in one pass — so aliasing among
// globals, or between a global source and any composite the call has already
// rebound or fast-copied with registration, is detected — and caches the
// verdict for the rest of the call. Data-only, alias-free sources take the
// tight copier; everything else takes the full rebind walk.
func (r *callFunctionRebinder) rebindGlobalValue(val Value) Value {
	if !r.globalsScanned {
		r.globalsScanned = true
		r.globalsDataFast = r.scanPendingGlobalSources()
		r.pendingGlobalSources = nil
	}
	if r.globalsDataFast && r.inboundValueUnseen(val) {
		return copyInboundDataValue(val)
	}
	rebound := r.rebindValue(val)
	if r.exec != nil && graphContainsBuiltin(rebound) {
		// A builtin-bearing global is host-visible state like a capability
		// object (#1210); it joins the host-state roots the moment it
		// materializes. The data-fast path above cannot carry a builtin.
		r.exec.recordHostStateRoot(rebound)
	}
	return rebound
}

func (r *callFunctionRebinder) scanPendingGlobalSources() bool {
	scanner := inboundDataScanner{rebinder: r}
	for _, source := range r.pendingGlobalSources {
		if !scanner.scan(source) {
			return false
		}
	}
	return true
}

func (r *callFunctionRebinder) fastCopyInbound(val Value) Value {
	if r.inboundRegister {
		return r.copyAndRegisterInboundValue(val)
	}
	return copyInboundDataValue(val)
}

// inboundValueUnseen reports whether the slow rebind walk (or a registering
// fast copy) has not already cloned this composite, in which case the tight
// copier may build a fresh clone without breaking alias dedup.
func (r *callFunctionRebinder) inboundValueUnseen(val Value) bool {
	switch val.Kind() {
	case KindArray:
		_, seen := r.seenArrays[arrayIdentity(val)]
		return !seen
	case KindHash:
		return r.inboundHashUnseen(val)
	case KindObject:
		key := objectCloneKey{ptr: reflect.ValueOf(val.HashEntryMap()).Pointer(), tag: val.ObjectTag()}
		_, seen := r.seenMaps[key]
		return !seen
	default:
		return false
	}
}

// noteDeferredGlobalClone marks a clone shared when its source also backs a
// lazily bound global, so the global's slot is counted from call setup rather
// than from the first read of it.
func (r *callFunctionRebinder) noteDeferredGlobalClone(sourceID uintptr, clone Value) {
	if sourceID == 0 || len(r.deferredGlobalIDs) == 0 {
		return
	}
	if _, deferred := r.deferredGlobalIDs[sourceID]; deferred {
		clone.MarkSharedRef()
	}
}

// deferGlobalSource records a lazily bound global's source and marks any clone
// already registered for it shared, covering the case where the arguments were
// rebound before the globals were bound.
func (r *callFunctionRebinder) deferGlobalSource(val Value) {
	id := collectionIdentity(val)
	if id == 0 {
		return
	}
	if r.deferredGlobalIDs == nil {
		r.deferredGlobalIDs = make(map[uintptr]struct{})
	}
	r.deferredGlobalIDs[id] = struct{}{}
	if clone, seen := r.seenArrays[id]; seen {
		clone.MarkSharedRef()
	}
	if clone, seen := r.seenHashes[id]; seen {
		clone.MarkSharedRef()
	}
}

// shareEntryMapOwners records wrapper as a holder of the cloned entry map, and
// marks it and the map's first holder shared once there is more than one.
//
// The host may deliberately build two wrappers over one map, and the rebinder
// deduplicates them onto one cloned map so the copy stays a copy rather than
// two. Under value semantics that shared storage must be invisible, so both
// wrappers copy on their first write instead of writing through it.
func (r *callFunctionRebinder) shareEntryMapOwners(entries map[string]Value, wrapper Value) {
	if entries == nil {
		return
	}
	ptr := reflect.ValueOf(entries).Pointer()
	if ptr == 0 {
		return
	}
	if owner, seen := r.sharedEntryOwners[ptr]; seen {
		owner.MarkSharedRef()
		wrapper.MarkSharedRef()
		return
	}
	if r.sharedEntryOwners == nil {
		r.sharedEntryOwners = make(map[uintptr]Value)
	}
	r.sharedEntryOwners[ptr] = wrapper
}

// inboundHashUnseen reports whether the slow path has rebound neither this
// hash wrapper nor its entry map.
func (r *callFunctionRebinder) inboundHashUnseen(val Value) bool {
	if _, seen := r.seenHashes[hashIdentity(val)]; seen {
		return false
	}
	entries := val.HashEntryMap()
	if entries == nil {
		return true
	}
	_, shared := r.seenHashEntries[reflect.ValueOf(entries).Pointer()]
	return !shared
}

func (r *callFunctionRebinder) rebindValues(values []Value) []Value {
	if len(values) == 0 {
		return values
	}
	out := make([]Value, len(values))
	for i, val := range values {
		out[i] = r.rebindInboundValue(val)
	}
	return out
}

func (r *callFunctionRebinder) rebindKeywords(kwargs map[string]Value) map[string]Value {
	if len(kwargs) == 0 {
		return kwargs
	}
	out := make(map[string]Value, len(kwargs))
	for name, val := range kwargs {
		out[name] = r.rebindInboundValue(val)
	}
	return out
}

// hostCapabilityContracts refuses an over-quota call and then asks the adapter
// what contracts it declares.
//
// The check is in here rather than at the call site, and that placement is the
// point. A check at a call site can have code inserted after it; the rule is not
// "check somewhere above the call" but that nothing this call has allocated is
// unaccounted when host code runs. Checking once per adapter left the contracts
// the previous iteration returned uncharged while the next adapter ran.
//
// Those contracts are host data, not program text. CapabilityContractProvider
// bounds neither how many entries the map has nor how long its keys are, and
// capabilityContractsByName is charged per entry and per key length, so a host
// that builds the map when asked is charged whatever it built.
//
// An adapter that declares no contracts costs no walk: no host code runs, so
// there is nothing to check ahead of.
func hostCapabilityContracts(exec *Execution, adapter CapabilityAdapter) (map[string]CapabilityMethodContract, error) {
	provider, ok := adapter.(CapabilityContractProvider)
	if !ok {
		return nil, nil
	}
	if err := exec.checkMemory(); err != nil {
		return nil, err
	}
	return provider.CapabilityContracts(), nil
}

// bindHostCapability refuses an over-quota call -- including whatever the
// contract collection before it retained -- and then hands control to the
// adapter.
//
// Two errors, because a caller has to treat them differently and conflating them
// misreports the cause. refused is this call being refused before the adapter
// ran, and travels as it stands: labeling it a capability failure would send a
// host to debug an adapter that never executed. bound is the adapter's own
// failure, which the caller labels in its own terms.
func bindHostCapability(exec *Execution, adapter CapabilityAdapter, binding CapabilityBinding) (globals map[string]Value, bound, refused error) {
	if err := exec.checkMemory(); err != nil {
		return nil, nil, err
	}
	if internal, ok := adapter.(interface {
		bindWithExecution(*Execution, CapabilityBinding) (map[string]Value, error)
	}); ok {
		globals, bound = internal.bindWithExecution(exec, binding)
	} else {
		globals, bound = adapter.Bind(binding)
	}
	return globals, bound, nil
}

func bindCapabilitiesForCall(exec *Execution, root *Env, rebinder *callFunctionRebinder, capabilities []CapabilityAdapter) error {
	if len(capabilities) == 0 {
		return nil
	}
	if exec.capabilityContracts == nil {
		exec.capabilityContracts = make(map[*Builtin]CapabilityMethodContract)
	}
	if exec.capabilityContractScopes == nil {
		exec.capabilityContractScopes = make(map[*Builtin]*capabilityContractScope)
	}
	if exec.capabilityContractsByName == nil {
		exec.capabilityContractsByName = make(map[string]CapabilityMethodContract)
	}

	binding := CapabilityBinding{Context: exec.ctx, Engine: exec.engine}
	ambientEnvs := ambientEnvSet(root)
	for _, adapter := range capabilities {
		if adapter == nil {
			continue
		}
		// Both host calls below go through a helper that checks immediately
		// before invoking, and neither is invoked here directly. That pairing is
		// the rule rather than the placement of any one check.
		//
		// A check merely sitting somewhere above a host call says nothing about
		// what was allocated in between. Collecting an adapter's contracts
		// retains host-supplied data on the execution, and the bind that follows
		// would otherwise run with none of it charged.
		scope := &capabilityContractScope{
			contracts:     map[string]CapabilityMethodContract{},
			knownBuiltins: make(map[*Builtin]struct{}),
		}
		contracts, err := hostCapabilityContracts(exec, adapter)
		if err != nil {
			return err
		}
		_, validatesData := adapter.(interface{ validatesCapabilityData() })
		for methodName, contract := range contracts {
			name := strings.TrimSpace(methodName)
			if name == "" {
				return fmt.Errorf("capability contract method name must be non-empty")
			}
			if _, exists := exec.capabilityContractsByName[name]; exists {
				return fmt.Errorf("duplicate capability contract for %s", name)
			}
			exec.capabilityContractsByName[name] = contract
			// First-party data validation runs inside the adapter's budget,
			// but its public contract names still participate in collisions.
			if !validatesData {
				scope.contracts[name] = contract
			}
		}
		globals, bindErr, refused := bindHostCapability(exec, adapter, binding)
		if refused != nil {
			return refused
		}
		if bindErr != nil {
			if ctxErr := exec.checkContext(); ctxErr != nil {
				return ctxErr
			}
			return fmt.Errorf("bind capability: %w", bindErr)
		}
		if err := exec.checkContext(); err != nil {
			return err
		}
		for name, val := range globals {
			if err := exec.checkContext(); err != nil {
				return err
			}
			rebound := rebinder.rebindValue(val)
			root.Define(name, rebound)
			exec.recordHostStateRoot(rebound)
			if len(scope.contracts) > 0 {
				scope.roots = append(scope.roots, rebound)
			}
			// Mark every builtin this adapter exposes as a per-call capability
			// grant. The marker lets the inbound rebinder revoke a captured grant
			// when a closure (for example a Hash.new default proc that copied a
			// capability into a local) escapes and re-enters a later call that did
			// not grant the same capability.
			markCapabilityBuiltins(rebound)
			// Skip the ambient global chain (root + ancestors) when walking a
			// capability-supplied closure's captured environment, matching the
			// pre/post-call scanners above. Otherwise a contract method whose
			// name happens to match a pre-existing global builtin would bind to
			// that global through a closure rooted in the ambient env.
			scanner := newCapabilityContractScanner()
			scanner.ambientEnvs = ambientEnvs
			scanner.bindContracts(rebound, scope, exec.capabilityContracts, exec.capabilityContractScopes)
		}
	}

	return nil
}

func initializeClassBodiesForCall(exec *Execution, env *Env, callClasses map[string]*ClassDef, order []string, skip map[string]struct{}) error {
	for _, name := range order {
		classDef, ok := callClasses[name]
		if !ok {
			continue
		}
		if _, deferred := skip[name]; deferred {
			continue
		}
		if len(classDef.Body) == 0 {
			continue
		}
		classVal, ok := env.Get(name)
		if !ok {
			return exec.errorAt(classInitPos(classDef), "class %s is not bound", name)
		}
		if err := exec.initializeClassBody(classVal, classDef, env); err != nil {
			return exec.wrapError(err, classInitPos(classDef))
		}
		if err := exec.checkContext(); err != nil {
			return err
		}
	}

	return nil
}

func classInitPos(classDef *ClassDef) Position {
	if len(classDef.Body) > 0 {
		return classDef.Body[0].Pos()
	}
	return Position{}
}

func (exec *Execution) initializeClassBody(classVal Value, classDef *ClassDef, parent *Env) error {
	if classDef == nil || classDef.bodyRan {
		return nil
	}
	if len(classDef.Body) == 0 {
		return nil
	}
	env := newEnv(parent)
	env.classBody = true
	env.Define("self", classVal)
	exec.pushReceiver(classVal)
	defer exec.popReceiver()
	// A class body is not a method body: a block created here has no
	// enclosing method to return from, so pin the home to none.
	exec.pushBlockHomeToken(0)
	_, _, err := exec.evalLocalScopeStatements(classDef.Body, env)
	exec.popBlockHomeToken()
	if err != nil {
		return err
	}
	classDef.bodyRan = true
	return nil
}

func prepareCallEnvForFunction(exec *Execution, root *Env, rebinder *callFunctionRebinder, fn *ScriptFunction, args []Value, keywords map[string]Value) (*Env, error) {
	if err := exec.checkContext(); err != nil {
		return nil, err
	}

	callEnv := newEnvWithCapacity(root, len(fn.Params))
	// A capability callback can re-enter a script function from inside an active
	// block-iteration region; mark the frame neutral before its pre-push binding
	// so those writes stay inside the region's incremental walk (see acquireCallEnv
	// and memory_blockregion.go).
	callEnv.markRegionNeutral(exec.blockRegionActive)
	// The host entry call never supplies a block, but the frame is still a call
	// frame: mark it with a nil block so block_given? reports false, yield
	// raises, and a &block parameter binds nil, keeping the invariant that every
	// call frame carries its own block slot.
	callEnv.setCallBlock(NewNil())
	callArgs := rebinder.rebindValues(args)
	callKeywords := rebinder.rebindKeywords(keywords)
	if err := exec.bindFunctionArgs(fn, callEnv, callArgs, callKeywords, fn.Pos); err != nil {
		if isHostControlSignal(err) {
			return nil, err
		}
		return nil, fmt.Errorf("bind function args: %w", err)
	}
	exec.pushEnv(callEnv)
	if err := exec.checkMemory(); err != nil {
		exec.popEnv()
		return nil, fmt.Errorf("check memory after binding call env: %w", err)
	}
	exec.popEnv()

	return callEnv, nil
}

func newExecutionForCall(script *Script, ctx context.Context, root *Env, opts CallOptions) *Execution {
	childCallOptions := CallOptions{
		Globals:      opts.Globals,
		Capabilities: opts.Capabilities,
		AllowRequire: opts.AllowRequire,
	}
	exec := &Execution{
		engine:        script.engine,
		script:        script,
		ctx:           ctx,
		quota:         script.engine.config.StepQuota,
		memoryQuota:   script.engine.config.MemoryQuotaBytes,
		recursionCap:  script.engine.config.RecursionLimit,
		root:          root,
		strictEffects: script.engine.config.StrictEffects,
		allowRequire:  opts.AllowRequire,
		callOptions:   childCallOptions,
	}
	// The module stacks stay nil: most calls never require a module,
	// and append allocates them on first use.
	exec.callStack = exec.callStackArr[:0]
	exec.receiverStack = exec.receiverStackArr[:0]
	exec.envStack = exec.envStackArr[:0]
	exec.validatedCapabilityArgs = exec.validatedCapabilityArgsArr[:0]
	return exec
}

func (exec *Execution) evalCallTarget(call *CallExpr, env *Env) (Value, Value, error) {
	if member, ok := call.Callee.(*MemberExpr); ok {
		receiver, err := exec.evalExpressionWithAuto(member.Object, env, memberReceiverAutoInvokes(member.Object, member.Property, env))
		if err != nil {
			return NewNil(), NewNil(), err
		}
		if err := exec.checkMemoryValue(receiver); err != nil {
			return NewNil(), NewNil(), err
		}
		if directCallee, handled, err := exec.evalDirectPublicMemberMethodCall(receiver, member.Property, member.Pos()); handled || err != nil {
			if err != nil {
				return NewNil(), NewNil(), err
			}
			return directCallee, receiver, nil
		}
		callee, err := exec.getPublicMember(receiver, member.Property, member.Pos())
		if err != nil {
			return NewNil(), NewNil(), err
		}
		return callee, receiver, nil
	}

	if ident, ok := call.Callee.(*Identifier); ok {
		return exec.evalIdentifierCallTarget(ident, env, callUsesBypassableIdentifierResolution(call))
	}

	callee, err := exec.evalExpressionWithAuto(call.Callee, env, false)
	if err != nil {
		return NewNil(), NewNil(), err
	}
	return callee, NewNil(), nil
}

// evalIdentifierCallTarget resolves a bare identifier used as a call target. A
// local variable binds with a nil receiver (it is a free-standing callable),
// while an identifier that falls through to an implicit-self member binds self
// as the receiver so builtins resolved off self (such as the universal
// introspection predicates) receive the correct receiver.
func (exec *Execution) evalIdentifierCallTarget(ident *Identifier, env *Env, bypassableCall bool) (Value, Value, error) {
	// Mirror the per-expression step charged by evalExpressionWithAuto, which
	// this branch replaces for identifier callees, so step accounting (and the
	// statement position a step-quota limit reports) is unchanged.
	if err := exec.step(); err != nil {
		return NewNil(), NewNil(), err
	}
	if val, ok := exec.identifierCallBinding(ident.Name, env, bypassableCall); ok {
		return val, NewNil(), nil
	}
	if self, hasSelf := env.Get("self"); hasSelf && (self.Kind() == KindInstance || self.Kind() == KindClass) {
		member, err := exec.getMember(self, ident.Name, ident.Pos())
		if err != nil {
			return NewNil(), NewNil(), err
		}
		return member, self, nil
	}
	return NewNil(), NewNil(), exec.errorAt(ident.Pos(), "undefined variable %s%s", ident.Name, didYouMean(ident.Name, env.visibleNames()))
}

// identifierCallBinding resolves an identifier callee and settles any hidden
// array-append accumulator on its binding scope, since reading the callee is an
// escaping variable reference. The common (non-bypass) resolution settles in the
// same walk via getEscaping; only the rare local-call-bypass path, which resolves
// through getSkipping, falls back to a separate settle.
func (exec *Execution) identifierCallBinding(name string, env *Env, bypassableCall bool) (Value, bool) {
	if !bypassableCall || len(exec.localCallBypassStack) == 0 {
		return env.getEscaping(name)
	}
	var skip map[*Env]struct{}
	for i := len(exec.localCallBypassStack) - 1; i >= 0; i-- {
		frame := exec.localCallBypassStack[i]
		binding, ok := frame.bindings[name]
		if !ok || binding == nil {
			continue
		}
		if skip == nil {
			skip = make(map[*Env]struct{})
		}
		skip[binding] = struct{}{}
	}
	if len(skip) == 0 {
		return env.getEscaping(name)
	}
	val, ok := env.getSkipping(name, skip)
	if ok {
		env.clearArrayAppendBuffer(name)
	}
	return val, ok
}

func (exec *Execution) evalDirectPublicMemberMethodCall(receiver Value, property string, pos Position) (Value, bool, error) {
	switch receiver.Kind() {
	case KindClass:
		if property == "new" {
			return NewNil(), false, nil
		}
		classDef := valueClass(receiver)
		fn, ok := classDef.ClassMethods[property]
		if !ok {
			return NewNil(), false, nil
		}
		if fn.Private {
			return NewNil(), true, exec.errorAt(pos, "private method %s", property)
		}
		if fn.Protected && !exec.protectedClassAccessAllowed(classDef) {
			return NewNil(), true, exec.errorAt(pos, "protected method %s", property)
		}
		return NewFunction(fn), true, nil
	case KindInstance:
		instance := valueInstance(receiver)
		fn, ok := instance.Class.Methods[property]
		if !ok {
			return NewNil(), false, nil
		}
		if fn.Private {
			return NewNil(), true, exec.errorAt(pos, "private method %s", property)
		}
		if fn.Protected && !exec.protectedInstanceAccessAllowed(instance.Class) {
			return NewNil(), true, exec.errorAt(pos, "protected method %s", property)
		}
		return NewFunction(fn), true, nil
	default:
		return NewNil(), false, nil
	}
}

func (exec *Execution) evalCallArgs(call *CallExpr, env *Env) ([]Value, error) {
	return exec.evalCallArgsForCallee(call, env, NewNil())
}

func (exec *Execution) evalCallArgsForCallee(call *CallExpr, env *Env, callee Value) ([]Value, error) {
	return exec.evalCallArgsForCalleeInto(call, env, callee, nil)
}

// evalCallArgsForCalleeInto evaluates positional arguments into buf when it is
// non-nil (a pooled backing sized to len(call.Args)), or a fresh slice
// otherwise. A splat call ignores buf and builds its own growing slice.
func (exec *Execution) evalCallArgsForCalleeInto(call *CallExpr, env *Env, callee Value, buf []Value) ([]Value, error) {
	paramInfo, hasParams := callableParamTypes(callee)
	if callHasSplatArg(call) {
		return exec.evalCallArgsWithSplats(call, env, paramInfo, hasParams)
	}
	args := buf
	if args == nil {
		args = make([]Value, len(call.Args))
	}
	for i, arg := range call.Args {
		expectation := expressionExpectation{}
		if hasParams {
			if candidate, ok := callableArgumentExpectation(paramInfo, i, len(call.Args)); ok {
				expectation = candidate
			}
		}
		val, err := exec.evalCallArgumentForExpectation(arg, env, expectation)
		if err != nil {
			return nil, err
		}
		if err := exec.checkMemoryValue(val); err != nil {
			return nil, err
		}
		args[i] = val
	}
	return args, nil
}

// borrowArgBuffer returns a positional-argument backing of length n, reusing a
// pooled slice when the most recently freed one is large enough. The caller
// must return it with returnArgBuffer once the call that consumed it has fully
// unwound. See argBufferPool for why only script-function calls may pool.
func (exec *Execution) borrowArgBuffer(n int) []Value {
	if pool := exec.argBufferPool; len(pool) > 0 {
		last := len(pool) - 1
		if buf := pool[last]; cap(buf) >= n {
			exec.argBufferPool = pool[:last]
			// Cap the returned slice at n so an append by a later consumer
			// (resolveKeywordOptionsHash collapsing keywords into an options
			// hash) reallocates instead of writing past n into the pooled
			// backing, keeping every write within the range returnArgBuffer clears.
			return buf[:n:n]
		}
	}
	return make([]Value, n)
}

// returnArgBuffer clears buf and returns it to the free list for reuse. Clearing
// drops references to the argument Values so a pooled buffer never pins their
// heap payloads between calls.
func (exec *Execution) returnArgBuffer(buf []Value) {
	clear(buf)
	exec.argBufferPool = append(exec.argBufferPool, buf[:0])
}

// acquireCallEnv returns a call-frame environment parented to fn.Env with room
// for capacity bindings. When fn.reuseCallEnv is set — its body and parameter
// defaults provably cannot capture the frame (see functionCanReuseCallEnv) — a
// recycled frame is reused if one is available; recycleCallEnv already cleared
// it, so acquire only rebinds its parent (and bumps the mutation epoch, exactly
// as resetForReuse does — unless the call is inside a block-iteration region,
// where the frame is epoch-neutral) before it is filled. Otherwise a fresh Env
// is allocated. Either way the frame is marked neutral iff a region is active,
// so its pre-push binding stays inside the region's incremental walk.
//
// A reused frame's storage is normalized to exactly what newEnvWithCapacity would
// build for this capacity, in both directions. resetForReuse only clears (does
// not free) the values map, so whether a reused frame carries a map depends on
// its prior tenant. That matters because the memory estimator charges a fixed
// base for any non-nil values map even when empty, and setDynamic binds into a
// non-nil map instead of the inline slots: without normalization a call's binding
// layout and quota charge would depend on which function ran just before it. A
// fresh frame has a map iff capacity exceeds the inline capacity, so a small
// reuse drops any inherited map and a large reuse allocates one when missing.
func (exec *Execution) acquireCallEnv(fn *ScriptFunction, capacity int) *Env {
	if fn.reuseCallEnv {
		if n := len(exec.callEnvFreeList); n > 0 {
			env := exec.callEnvFreeList[n-1]
			exec.callEnvFreeList[n-1] = nil
			exec.callEnvFreeList = exec.callEnvFreeList[:n-1]
			// A call made from inside an active block-iteration region pushes its
			// frame into the region's active suffix, which every check re-walks
			// fresh, so mark it epoch-neutral before the reset and the pre-push
			// argument/block binding below. Without this those writes bump the
			// epoch and force the next region check to re-walk the whole prefix,
			// leaving a block body that calls a script helper quadratic (see
			// memory_blockregion.go). popEnv clears the flag when the frame leaves
			// the stack.
			env.markRegionNeutral(exec.blockRegionActive)
			env.resetForReuse(fn.Env)
			if capacity > inlineEnvBindingCapacity {
				if env.values == nil {
					env.values = make(map[string]Value, capacity)
				}
			} else {
				env.values = nil
			}
			return env
		}
	}
	env := newEnvWithCapacity(fn.Env, capacity)
	env.markRegionNeutral(exec.blockRegionActive)
	return env
}

// recycleCallEnv returns a dead call frame to the free list for a later call to
// reuse. Callers must invoke it only when fn.reuseCallEnv holds and the call has
// fully unwound with its return value settled (settleArrayAppendResult), so no
// live reference to the frame survives.
//
// The frame is cleared here rather than at acquire time so pooled frames never
// pin the heap payloads of their former locals: a function that builds a large
// local and returns something small must not keep that local alive until the
// frame is next used (which may be never). This mirrors returnArgBuffer, which
// clears an argument backing on return for the same reason.
//
// When env-recycle verification is enabled the frame is poisoned and dropped
// instead of pooled: any reference the recycler wrongly judged dead panics on
// its next access (see Env.assertNotPoisoned) rather than silently reading a
// rebound frame, turning a missed capture site into a loud test failure.
func (exec *Execution) recycleCallEnv(env *Env) {
	if envRecycleVerify {
		env.poisoned = true
		return
	}
	// A frame recycled while a block-iteration region is active has just left the
	// region's active suffix; it is off the stack and provably uncapturable, so
	// clearing it changes nothing a memory check can observe. Mark it neutral so
	// the reset skips the epoch bump that would otherwise invalidate the region
	// memo on every helper call from a block body. acquireCallEnv re-marks the
	// frame from the region state live when it is next taken from the free list.
	env.markRegionNeutral(exec.blockRegionActive)
	env.resetForReuse(nil)
	exec.callEnvFreeList = append(exec.callEnvFreeList, env)
}

func callHasSplatArg(call *CallExpr) bool {
	for _, arg := range call.Args {
		if _, ok := arg.(*SplatArg); ok {
			return true
		}
	}
	return false
}

// evalCallArgsWithSplats evaluates a positional argument list containing at
// least one `*array` splat, expanding each splatted array in place so the
// existing arity, keyword-binding, and type validation see exactly the shape
// the equivalent literal call would produce. Expansion is sandboxed like the
// literal equivalent: each expanded element charges a step, and the growing
// argument backing is charged against the memory quota after every splat so
// a huge splatted array trips the quota just as its literal spelling would.
// Positional type expectations apply only to arguments before the first
// splat, whose final positions are still known statically.
func (exec *Execution) evalCallArgsWithSplats(call *CallExpr, env *Env, paramInfo callableParamInfo, hasParams bool) ([]Value, error) {
	args := make([]Value, 0, len(call.Args))
	splatSeen := false
	for i, arg := range call.Args {
		if splat, ok := arg.(*SplatArg); ok {
			splatSeen = true
			val, err := exec.evalCallArg(splat.Value, env)
			if err != nil {
				return nil, err
			}
			if val.Kind() != KindArray {
				return nil, exec.errorAt(splat.Pos(), "splat argument must be an array, got %s", val.Kind())
			}
			items := val.Array()
			args = slices.Grow(args, len(items))
			for _, item := range items {
				if err := exec.step(); err != nil {
					return nil, err
				}
				args = append(args, item)
			}
			if err := exec.checkMemoryValue(adoptArray(args)); err != nil {
				return nil, err
			}
			continue
		}
		expectation := expressionExpectation{}
		if hasParams && !splatSeen {
			if candidate, ok := callableArgumentExpectation(paramInfo, i, len(call.Args)); ok {
				expectation = candidate
			}
		}
		val, err := exec.evalCallArgumentForExpectation(arg, env, expectation)
		if err != nil {
			return nil, err
		}
		if err := exec.checkMemoryValue(val); err != nil {
			return nil, err
		}
		args = append(args, val)
	}
	return args, nil
}

func (exec *Execution) evalCallArg(arg Expression, env *Env) (Value, error) {
	val, err := exec.evalExpressionWithAuto(arg, env, true)
	if err != nil {
		return NewNil(), err
	}
	if err := exec.checkMemoryValue(val); err != nil {
		return NewNil(), err
	}
	return val, nil
}

func (exec *Execution) evalCallKwArgs(call *CallExpr, env *Env) (map[string]Value, error) {
	return exec.evalCallKwArgsForCallee(call, env, NewNil(), calleeDirect)
}

func (exec *Execution) evalCallKwArgsForCallee(call *CallExpr, env *Env, callee Value, resolution calleeResolution) (map[string]Value, error) {
	if len(call.KwArgs) == 0 {
		return nil, nil
	}
	paramInfo, hasParams := callableParamTypes(callee)
	optionsHashType, hasOptionsHashTarget := callOptionsHashArgumentType(call, callee, resolution)
	kwargs := make(map[string]Value, len(call.KwArgs))
	for _, kw := range call.KwArgs {
		if kw.Splat {
			if err := exec.expandKeywordSplat(kw.Value, env, kwargs); err != nil {
				return nil, err
			}
			continue
		}
		var expectedType *TypeExpr
		if hasParams {
			expectedType = keywordArgumentExpectedType(paramInfo.params, kw.Name)
			if expectedType == nil && hasOptionsHashTarget {
				expectedType = optionsHashArgumentValueType(optionsHashType, kw.Name)
			}
		}
		val, err := exec.evalCallArgumentForType(kw.Value, env, expectedType)
		if err != nil {
			return nil, err
		}
		if err := exec.checkMemoryValue(val); err != nil {
			return nil, err
		}
		kwargs[kw.Name] = val
	}
	return kwargs, nil
}

// expandKeywordSplat evaluates one `**hash` keyword splat and merges its
// entries into the call's keyword arguments. Entries expand in the hash's
// insertion order and later arguments (splat or named, processed in source
// order) win on duplicate keys, matching Ruby's merge semantics. Every entry
// charges a step, and the accumulated keyword map is charged against the
// memory quota after the splat so a huge options hash trips it exactly like
// the literal keyword spelling.
func (exec *Execution) expandKeywordSplat(expr Expression, env *Env, kwargs map[string]Value) error {
	val, err := exec.evalCallArg(expr, env)
	if err != nil {
		return err
	}
	if val.Kind() != KindHash {
		return exec.errorAt(expr.Pos(), "keyword splat argument must be a hash, got %s", val.Kind())
	}
	for _, entry := range val.HashEntries() {
		if err := exec.step(); err != nil {
			return err
		}
		switch entry.Key.Kind() {
		case KindString, KindSymbol:
		default:
			return exec.errorAt(expr.Pos(), "keyword splat keys must be strings or symbols, got %s", entry.Key.Kind())
		}
		kwargs[entry.Key.String()] = entry.Value
	}
	return exec.checkMemoryValue(adoptHash(kwargs))
}

func (exec *Execution) evalCallArgumentForType(arg Expression, env *Env, ty *TypeExpr) (Value, error) {
	return exec.evalCallArgumentForExpectation(arg, env, typeExpressionExpectation(ty))
}

func (exec *Execution) evalCallArgumentForExpectation(arg Expression, env *Env, expectation expressionExpectation) (Value, error) {
	if !expectation.empty() {
		switch e := arg.(type) {
		case *ConditionalExpr:
			if err := exec.step(); err != nil {
				return NewNil(), err
			}
			return exec.evalConditionalExprWithExpectation(e, env, expectation)
		case *IfExpr:
			if err := exec.step(); err != nil {
				return NewNil(), err
			}
			return exec.evalIfExprWithExpectation(e, env, expectation)
		case *CaseExpr:
			if err := exec.step(); err != nil {
				return NewNil(), err
			}
			return exec.evalCaseExprWithExpectation(e, env, expectation)
		}
	}
	if val, ok, err := exec.evalTypedContainerCallArgument(arg, env, expectation); ok || err != nil {
		return val, err
	}
	return exec.evalCallArgument(arg, env, expectation.includesCallable())
}

func (exec *Execution) evalTypedContainerCallArgument(arg Expression, env *Env, expectation expressionExpectation) (Value, bool, error) {
	switch e := arg.(type) {
	case *ArrayLiteral:
		elementExpectation, ok := expectation.arrayElementExpectation()
		if !ok {
			return NewNil(), false, nil
		}
		if err := exec.step(); err != nil {
			return NewNil(), true, err
		}
		val, err := exec.evalArrayLiteralWithElementExpectation(e, env, elementExpectation)
		return val, true, err
	case *HashLiteral:
		if e.ShapeType != nil && !exec.hashShapeShadowed(e, env) {
			// The group evaluates as a first-class shape value regardless of
			// the parameter annotation; the normal path produces it and the
			// boundary check decides.
			return NewNil(), false, nil
		}
		if !hashLiteralTypeHasValueSlots(expectation.ty) {
			return NewNil(), false, nil
		}
		if err := exec.step(); err != nil {
			return NewNil(), true, err
		}
		val, err := exec.evalHashLiteralWithValueTypes(e, env, func(key Value) *TypeExpr {
			return hashLiteralValueType(expectation.ty, key)
		})
		return val, true, err
	default:
		return NewNil(), false, nil
	}
}

func (exec *Execution) evalCallArgument(arg Expression, env *Env, expectsCallable bool) (Value, error) {
	if !expectsCallable {
		return exec.evalExpressionWithAuto(arg, env, true)
	}
	if val, ok, err := exec.evalBareCallableArgument(arg, env); ok || err != nil {
		return val, err
	}
	if val, ok, err := exec.evalGeneratedGetterArgument(arg, env); ok || err != nil {
		return val, err
	}
	return exec.evalExpressionWithAuto(arg, env, false)
}

func (exec *Execution) evalGeneratedGetterArgument(arg Expression, env *Env) (Value, bool, error) {
	memberExpr, ok := arg.(*MemberExpr)
	if !ok {
		return NewNil(), false, nil
	}
	obj, err := exec.evalCallableMemberArgumentReceiver(memberExpr, env)
	if err != nil {
		return NewNil(), true, err
	}
	if memberExpr.Safe && obj.Kind() == KindNil {
		return NewNil(), true, nil
	}
	if err := exec.checkMemoryValue(obj); err != nil {
		return NewNil(), true, err
	}
	member, err := exec.getPublicMember(obj, memberExpr.Property, memberExpr.Pos())
	if err != nil {
		return NewNil(), true, err
	}
	if generatedAccessorKind(member) == functionAccessorGetter {
		val, err := exec.autoInvokeIfNeeded(memberExpr, member, obj)
		return val, true, err
	}
	if builtin := valueBuiltin(member); builtin != nil && builtin.OptionsHashTarget == nil {
		// A universal or type-dispatch builtin (list.size, s.upcase) is not a
		// bindable script callable: evaluate it like a normal member argument
		// so its value reaches the callee, not the raw builtin, which would
		// later run against a nil receiver. Bound script methods carry their
		// target in OptionsHashTarget and stay referencable.
		val, err := exec.autoInvokeIfNeeded(memberExpr, member, obj)
		return val, true, err
	}
	return member, true, nil
}

func (exec *Execution) evalCallableMemberArgumentReceiver(memberExpr *MemberExpr, env *Env) (Value, error) {
	if memberExpr.Property == "call" {
		return exec.evalMemberCallReceiver(memberExpr, env, memberCallReceiverAutoInvokes)
	}
	return exec.evalExpressionWithAuto(memberExpr.Object, env, memberReceiverAutoInvokes(memberExpr.Object, memberExpr.Property, env))
}

func generatedAccessorKind(member Value) functionAccessorKind {
	if member.Kind() != KindBuiltin {
		return functionAccessorNone
	}
	builtin := valueBuiltin(member)
	if builtin == nil || builtin.OptionsHashTarget == nil {
		return functionAccessorNone
	}
	return builtin.OptionsHashTarget.Accessor
}

func (exec *Execution) evalBareCallableArgument(arg Expression, env *Env) (Value, bool, error) {
	call, ok := arg.(*CallExpr)
	if !ok || call.Parenthesized || len(call.Args) > 0 || len(call.KwArgs) > 0 || call.Block != nil {
		return NewNil(), false, nil
	}
	if _, ok := call.Callee.(*Identifier); !ok {
		return NewNil(), false, nil
	}
	callee, _, err := exec.evalCallTarget(call, env)
	if err != nil {
		return NewNil(), true, err
	}
	return callee, true, nil
}

type callableParamInfo struct {
	params               []Param
	usesRubyBlockBinding bool
}

func callableParamTypes(callee Value) (callableParamInfo, bool) {
	switch callee.Kind() {
	case KindFunction:
		fn := valueFunction(callee)
		if fn == nil {
			return callableParamInfo{}, false
		}
		return callableParamInfo{params: fn.Params}, true
	case KindBlock:
		blk := valueBlock(callee)
		if blk == nil {
			return callableParamInfo{}, false
		}
		return callableParamInfo{params: blk.Params, usesRubyBlockBinding: true}, true
	case KindBuiltin:
		builtin := valueBuiltin(callee)
		if builtin == nil {
			return callableParamInfo{}, false
		}
		if builtin.OptionsHashTarget != nil {
			return callableParamInfo{params: builtin.OptionsHashTarget.Params}, true
		}
		if len(builtin.SignatureParams) > 0 {
			return callableParamInfo{params: builtin.SignatureParams}, true
		}
	}
	return callableParamInfo{}, false
}

func callableArgumentExpectation(info callableParamInfo, argIndex, argCount int) (expressionExpectation, bool) {
	if info.usesRubyBlockBinding {
		expectation := blockArgumentExpectation(info.params, argIndex, argCount)
		return expectation, !expectation.empty()
	}
	param, ok := positionalCallableParam(info.params, argIndex)
	if !ok {
		return expressionExpectation{}, false
	}
	return positionalArgumentExpectation(param), true
}

func positionalCallableParam(params []Param, argIndex int) (Param, bool) {
	positional := 0
	for _, param := range params {
		switch param.Kind {
		case ParamNormal:
			if positional == argIndex {
				return param, true
			}
			positional++
		case ParamRest:
			if argIndex >= positional {
				return param, true
			}
		}
	}
	return Param{}, false
}

func positionalArgumentExpectation(param Param) expressionExpectation {
	switch param.Kind {
	case ParamRest:
		return typeExpressionExpectation(restParamElementType(param.Type))
	default:
		ty := param.Type
		if ty == nil {
			// An unannotated ivar parameter's expectation is the property
			// contract resolved at class compile time, so a bare zero-arity
			// callable reaches a function-typed ivar un-invoked.
			ty = param.PropertyType
		}
		expectation := typeExpressionExpectation(ty)
		if target, ok := param.Target.(*DestructureTarget); ok {
			expectation.arrayElement = destructureTargetElementExpectation(target)
		}
		return expectation
	}
}

type expressionExpectation struct {
	ty           *TypeExpr
	arrayElement func(int, int) expressionExpectation
}

func typeExpressionExpectation(ty *TypeExpr) expressionExpectation {
	if ty == nil {
		return expressionExpectation{}
	}
	return expressionExpectation{ty: ty}
}

func (expectation expressionExpectation) empty() bool {
	return expectation.ty == nil && expectation.arrayElement == nil
}

func (expectation expressionExpectation) includesCallable() bool {
	if typeExprIncludesCallable(expectation.ty) {
		return true
	}
	if expectation.arrayElement != nil {
		return expectation.arrayElement(0, 1).includesCallable()
	}
	return false
}

func (expectation expressionExpectation) arrayElementExpectation() (func(int, int) expressionExpectation, bool) {
	if expectation.arrayElement != nil {
		return expectation.arrayElement, true
	}
	elementType, ok := arrayLiteralElementType(expectation.ty)
	if !ok {
		return nil, false
	}
	return func(_, _ int) expressionExpectation {
		return typeExpressionExpectation(elementType)
	}, true
}

func destructureTargetElementExpectation(target *DestructureTarget) func(int, int) expressionExpectation {
	return func(index, count int) expressionExpectation {
		restIndex := destructureRestElementIndex(target)
		if restIndex == -1 {
			if index >= len(target.Elements) {
				return expressionExpectation{}
			}
			return destructureElementSingleValueExpectation(target.Elements[index])
		}
		trailing := len(target.Elements) - restIndex - 1
		restStart := min(restIndex, count)
		restEnd := max(restStart, count-trailing)
		switch {
		case index < restIndex:
			return destructureElementSingleValueExpectation(target.Elements[index])
		case index < restEnd:
			return destructureElementRestValueExpectation(target.Elements[restIndex], index-restStart, restEnd-restStart)
		default:
			elementIndex := restIndex + 1 + (index - restEnd)
			if elementIndex >= len(target.Elements) {
				return expressionExpectation{}
			}
			return destructureElementSingleValueExpectation(target.Elements[elementIndex])
		}
	}
}

func destructureRestElementIndex(target *DestructureTarget) int {
	for i, element := range target.Elements {
		if element.Rest {
			return i
		}
	}
	return -1
}

func destructureElementSingleValueExpectation(element DestructureElement) expressionExpectation {
	expectation := typeExpressionExpectation(element.Type)
	if target, ok := element.Target.(*DestructureTarget); ok {
		expectation.arrayElement = destructureTargetElementExpectation(target)
	}
	return expectation
}

func destructureElementRestValueExpectation(element DestructureElement, index, count int) expressionExpectation {
	if target, ok := element.Target.(*DestructureTarget); ok {
		return destructureTargetElementExpectation(target)(index, count)
	}
	return typeExpressionExpectation(restParamElementType(element.Type))
}

func keywordArgumentExpectedType(params []Param, name string) *TypeExpr {
	for _, param := range params {
		switch param.Kind {
		case ParamKeyword, ParamNormal:
			if param.Name == name {
				return param.Type
			}
		case ParamKeywordRest:
			return keywordRestParamValueType(param.Type, name)
		}
	}
	return nil
}

func restParamElementType(ty *TypeExpr) *TypeExpr {
	if ty == nil {
		return nil
	}
	switch ty.Kind {
	case TypeArray:
		if len(ty.TypeArgs) > 0 {
			return ty.TypeArgs[0]
		}
		return nil
	case TypeUnion:
		elements := restParamUnionElementTypes(ty)
		switch len(elements) {
		case 0:
			return nil
		case 1:
			return elements[0]
		default:
			return &TypeExpr{Kind: TypeUnion, Union: elements}
		}
	case TypeAny:
		return ty
	default:
		return nil
	}
}

func restParamUnionElementTypes(ty *TypeExpr) []*TypeExpr {
	switch ty.Kind {
	case TypeAny:
		return []*TypeExpr{ty}
	case TypeArray:
		if len(ty.TypeArgs) > 0 {
			return []*TypeExpr{ty.TypeArgs[0]}
		}
	case TypeUnion:
		var elements []*TypeExpr
		for _, option := range ty.Union {
			elements = append(elements, restParamUnionElementTypes(option)...)
		}
		return elements
	}
	return nil
}

func keywordRestParamValueType(ty *TypeExpr, name string) *TypeExpr {
	if ty == nil {
		return nil
	}
	switch ty.Kind {
	case TypeAny:
		return ty
	case TypeHash:
		if len(ty.TypeArgs) > 1 {
			return ty.TypeArgs[1]
		}
		return nil
	case TypeShape:
		field, ok := ty.Shape[name]
		if !ok {
			return nil
		}
		return field
	case TypeUnion:
		return unionOfValueTypes(ty.Union, func(option *TypeExpr) *TypeExpr {
			return keywordRestParamValueType(option, name)
		})
	default:
		return nil
	}
}

func optionsHashArgumentValueType(ty *TypeExpr, name string) *TypeExpr {
	if ty == nil {
		return nil
	}
	switch ty.Kind {
	case TypeHash:
		if len(ty.TypeArgs) > 1 {
			return ty.TypeArgs[1]
		}
		return nil
	case TypeShape:
		field, ok := ty.Shape[name]
		if !ok {
			return nil
		}
		return field
	case TypeUnion:
		return unionOfValueTypes(ty.Union, func(option *TypeExpr) *TypeExpr {
			return optionsHashArgumentValueType(option, name)
		})
	default:
		return nil
	}
}

func arrayLiteralElementType(ty *TypeExpr) (*TypeExpr, bool) {
	if ty == nil {
		return nil, false
	}
	switch ty.Kind {
	case TypeArray:
		if len(ty.TypeArgs) > 0 {
			return ty.TypeArgs[0], true
		}
		return nil, false
	case TypeUnion:
		elementType := unionOfValueTypes(ty.Union, func(option *TypeExpr) *TypeExpr {
			if element, ok := arrayLiteralElementType(option); ok {
				return element
			}
			return nil
		})
		return elementType, elementType != nil
	default:
		return nil, false
	}
}

func hashLiteralTypeHasValueSlots(ty *TypeExpr) bool {
	if ty == nil {
		return false
	}
	switch ty.Kind {
	case TypeHash, TypeShape:
		return true
	case TypeUnion:
		if slices.ContainsFunc(ty.Union, hashLiteralTypeHasValueSlots) {
			return true
		}
	}
	return false
}

func hashLiteralValueType(ty *TypeExpr, key Value) *TypeExpr {
	if ty == nil {
		return nil
	}
	switch ty.Kind {
	case TypeHash:
		if len(ty.TypeArgs) > 1 {
			return ty.TypeArgs[1]
		}
		return nil
	case TypeShape:
		name, err := hashKeyString(key)
		if err != nil {
			return nil
		}
		field, ok := ty.Shape[name]
		if !ok {
			return nil
		}
		return field
	case TypeUnion:
		return unionOfValueTypes(ty.Union, func(option *TypeExpr) *TypeExpr {
			return hashLiteralValueType(option, key)
		})
	default:
		return nil
	}
}

func unionOfValueTypes(options []*TypeExpr, valueType func(*TypeExpr) *TypeExpr) *TypeExpr {
	var types []*TypeExpr
	for _, option := range options {
		ty := valueType(option)
		if ty != nil {
			types = append(types, ty)
		}
	}
	switch len(types) {
	case 0:
		return nil
	case 1:
		return types[0]
	default:
		return &TypeExpr{Kind: TypeUnion, Union: types}
	}
}

func typeExprIncludesCallable(ty *TypeExpr) bool {
	if ty == nil {
		return false
	}
	switch ty.Kind {
	case TypeFunction:
		return true
	case TypeUnion:
		if slices.ContainsFunc(ty.Union, typeExprIncludesCallable) {
			return true
		}
	}
	return false
}

// calleeResolution records how a call's callee was resolved, which decides
// whether a parenthesized call may collapse its keyword arguments into a
// positional options hash. The distinction matters only for member calls: a
// function value surfaced as a genuine method must stay strict, while a plain
// function value merely stored in a member collapses like any direct call.
type calleeResolution int

const (
	// calleeDirect marks a callee resolved from a non-member expression, such
	// as a local function or a function-valued variable.
	calleeDirect calleeResolution = iota
	// calleeMemberMethod marks a callee surfaced through the direct
	// member-method path: a genuine instance, class, or constructor method.
	calleeMemberMethod
	// calleeForwardedMethod marks a method reached through dynamic dispatch
	// helpers such as send/public_send. The method lookup is member-based, but
	// the helper forwards keywords as call arguments rather than enforcing direct
	// member-call keyword strictness.
	calleeForwardedMethod
	// calleeMemberValue marks a callee fetched as a stored member value, such
	// as a module function exposed on a namespace object.
	calleeMemberValue
)

// resolveKeywordOptionsHash collapses a call's keyword arguments into a trailing
// positional options hash when the callee has no matching keyword parameter and
// exposes a positional parameter to receive it. This mirrors Ruby's options-hash
// binding. Parenless calls collapse for any options-hash target. Parenthesized
// calls collapse for plain function calls (a function value, its `call` alias, or
// a function value held in a member) and constructors. Parenthesized ordinary
// methods stay strict. resolution reports how the callee was resolved, which the
// member paths use to tell genuine methods apart from stored function values that
// happen to surface as bare function values too.
func resolveKeywordOptionsHash(call *CallExpr, callee Value, resolution calleeResolution, args []Value, kwargs map[string]Value) ([]Value, map[string]Value) {
	if !call.KeywordOptionsHash || len(kwargs) == 0 {
		return args, kwargs
	}
	if !calleeCollapsesOptionsHash(call, callee, resolution) {
		return args, kwargs
	}
	fn := optionsHashTarget(callee)
	if fn == nil || !functionCanReceiveOptionsHash(fn, len(args), func(name string) bool {
		_, ok := kwargs[name]
		return ok
	}) {
		return args, kwargs
	}
	hash := make(map[string]Value, len(kwargs))
	maps.Copy(hash, kwargs)
	return append(args, NewHash(hash)), nil
}

// calleeCollapsesOptionsHash reports whether the resolved callee permits keyword
// arguments to collapse into a positional options hash for the given call form.
// The parenless form collapses for any options-hash target. Parenthesized calls
// keep ordinary method binding strict: a call to a plain function value collapses
// like a plain function call, whether that value was resolved directly or fetched
// from a member, constructors collapse through their initialize options target,
// and a member call collapses through a function value's direct-call alias as
// well. A callee surfaced through the direct member-method path stays strict,
// since that path surfaces methods as bare function values too.
func calleeCollapsesOptionsHash(call *CallExpr, callee Value, resolution calleeResolution) bool {
	if !call.Parenthesized {
		return true
	}
	builtin := valueBuiltin(callee)
	if builtin != nil && builtinCollapsesConstructorOptionsHash(builtin) {
		return true
	}
	switch resolution {
	case calleeMemberMethod:
		return false
	case calleeForwardedMethod:
		return true
	case calleeMemberValue:
		return callee.Kind() == KindFunction
	default:
		return callee.Kind() == KindFunction
	}
}

func builtinCollapsesConstructorOptionsHash(builtin *Builtin) bool {
	return builtin.OptionsHashTarget != nil && strings.HasSuffix(builtin.Name, ".new")
}

func optionsHashTarget(callee Value) *ScriptFunction {
	switch callee.Kind() {
	case KindFunction:
		return valueFunction(callee)
	case KindBuiltin:
		builtin := valueBuiltin(callee)
		if builtin == nil {
			return nil
		}
		return builtin.OptionsHashTarget
	default:
		return nil
	}
}

func callOptionsHashArgumentType(call *CallExpr, callee Value, resolution calleeResolution) (*TypeExpr, bool) {
	if !call.KeywordOptionsHash || len(call.KwArgs) == 0 {
		return nil, false
	}
	if !calleeCollapsesOptionsHash(call, callee, resolution) {
		return nil, false
	}
	fn := optionsHashTarget(callee)
	if fn == nil {
		return nil, false
	}
	return optionsHashArgumentType(fn, len(call.Args), func(name string) bool {
		for _, kw := range call.KwArgs {
			if kw.Name == name {
				return true
			}
		}
		return false
	})
}

func functionCanReceiveOptionsHash(fn *ScriptFunction, positionalCount int, hasKeyword func(string) bool) bool {
	_, ok := optionsHashArgumentType(fn, positionalCount, hasKeyword)
	return ok
}

func optionsHashArgumentType(fn *ScriptFunction, positionalCount int, hasKeyword func(string) bool) (*TypeExpr, bool) {
	for _, param := range fn.Params {
		if param.Kind == ParamKeyword || param.Kind == ParamKeywordRest {
			return nil, false
		}
	}
	for _, param := range fn.Params {
		switch param.Kind {
		case ParamNormal:
			if positionalCount > 0 {
				positionalCount--
				continue
			}
			if hasKeyword(param.Name) {
				return nil, false
			}
			return param.Type, true
		case ParamRest:
			return restParamElementType(param.Type), true
		}
	}
	return nil, false
}

func (exec *Execution) evalCallBlock(call *CallExpr, env *Env) (Value, error) {
	if call.Block != nil {
		block, err := exec.evalBlockLiteral(call.Block, env)
		if err != nil {
			return NewNil(), err
		}
		if err := exec.checkMemoryValue(block); err != nil {
			return NewNil(), err
		}
		return block, nil
	}
	return NewNil(), nil
}

func (exec *Execution) checkCallMemoryRoots(receiver Value, args []Value, kwargs map[string]Value, block Value) error {
	return exec.checkCallMemoryRootsWithCallee(NewNil(), receiver, args, kwargs, block)
}

// checkCallMemoryRootsWithCallee charges the live call roots — the callee,
// receiver, arguments, keyword arguments, and block — against the memory quota
// before the call runs.
//
// The callee is included when it carries captured roots: a bound predicate
// builtin captures its receiver (see universalMember), and a block captures its
// environment. In both cases the captured payload can be reachable only through
// a temporary callee on the Go call stack, not through the call receiver,
// arguments, or block argument. Passing the callee through the same estimator
// deduplicates it against the environment, so a callee that is also reachable
// from a variable is counted once and the common static callee — a function, or a
// builtin with no captures — adds nothing.
func (exec *Execution) checkCallMemoryRootsWithCallee(callee, receiver Value, args []Value, kwargs map[string]Value, block Value) error {
	if !calleeCapturesRoots(callee) {
		if receiver.Kind() == KindNil && len(kwargs) == 0 && block.IsNil() {
			if len(args) == 0 {
				return nil
			}
			return exec.checkMemoryWith(args...)
		}
		return exec.checkMemoryWithCallRoots(NewNil(), receiver, args, kwargs, block)
	}
	return exec.checkMemoryWithCallRoots(callee, receiver, args, kwargs, block)
}

// calleeCapturesRoots reports whether a callee value carries captured runtime
// values that the call roots must charge — that is, a block with an environment
// or a bound builtin (such as a stored or temporary eql?/equal? predicate) whose
// Fn closes over a receiver. Static callees (functions, or builtins without
// captures) carry no extra payload, so the common call path skips charging them.
func calleeCapturesRoots(callee Value) bool {
	switch callee.Kind() {
	case KindBlock:
		return valueBlock(callee) != nil
	case KindBuiltin:
		builtin := valueBuiltin(callee)
		return builtin != nil && len(builtin.CapturedValues) > 0
	default:
		return false
	}
}

func (exec *Execution) evalCallExpr(call *CallExpr, env *Env) (Value, error) {
	if member, ok := call.Callee.(*MemberExpr); ok {
		return exec.evalMemberCallExpr(call, member, env)
	}

	if ident, ok := call.Callee.(*Identifier); ok && ident.Name == blockGivenName {
		return exec.evalBlockGivenCall(call, env)
	}

	callee, receiver, err := exec.evalCallTarget(call, env)
	if err != nil {
		return NewNil(), err
	}
	// A call to a script function evaluates its positional arguments into a
	// pooled backing: bindFunctionArgs copies each element into the callee's
	// environment, so the slice is dead once the call unwinds. The buffer is
	// returned after invokeCallable, covering every early return in between.
	var argBuf []Value
	if callee.Kind() == KindFunction && len(call.Args) > 0 && !callHasSplatArg(call) {
		argBuf = exec.borrowArgBuffer(len(call.Args))
		defer exec.returnArgBuffer(argBuf)
	}
	args, err := exec.evalCallArgsForCalleeInto(call, env, callee, argBuf)
	if err != nil {
		return NewNil(), err
	}
	kwargs, err := exec.evalCallKwArgsForCallee(call, env, callee, calleeDirect)
	if err != nil {
		return NewNil(), err
	}
	args, kwargs = resolveKeywordOptionsHash(call, callee, calleeDirect, args, kwargs)
	block, err := exec.evalCallBlock(call, env)
	if err != nil {
		return NewNil(), err
	}
	if err := exec.checkContext(); err != nil {
		return NewNil(), err
	}
	if err := exec.checkCallMemoryRootsWithCallee(callee, receiver, args, kwargs, block); err != nil {
		return NewNil(), err
	}

	result, callErr := exec.invokeCallable(callee, receiver, args, kwargs, block, call.Pos())
	retireCallBlock(block)
	if callErr != nil {
		return NewNil(), callErr
	}
	if err := exec.checkMemoryValue(result); err != nil {
		return NewNil(), err
	}
	return result, nil
}

// evalBlockGivenCall handles the parenthesized block_given?() form. Like Ruby's
// Kernel#block_given?, it accepts no arguments and reports whether the enclosing
// call was supplied a block.
func (exec *Execution) evalBlockGivenCall(call *CallExpr, env *Env) (Value, error) {
	if len(call.Args) != 0 || len(call.KwArgs) != 0 {
		return NewNil(), exec.errorAt(call.Pos(), "%s takes no arguments", blockGivenName)
	}
	if call.Block != nil {
		return NewNil(), exec.errorAt(call.Pos(), "%s does not accept a block", blockGivenName)
	}
	return NewBool(blockGivenInCurrentCall(env)), nil
}

func (exec *Execution) evalMemberCallExpr(call *CallExpr, member *MemberExpr, env *Env) (Value, error) {
	ordinaryReceiver := func() (Value, error) {
		return exec.evalMemberCallReceiver(member, env, func(object Expression, env *Env) bool {
			return callMemberCallReceiverAutoInvokes(call, object, env)
		})
	}
	var (
		receiver Value
		err      error
	)
	if recordsMutableReceiver(member.Property) {
		// A member that writes through its receiver records the path that
		// owns it, so a later write updates the local, instance variable, or
		// nested path the source names rather than a value the script cannot
		// see again. Isolation waits until the mutator actually writes: a
		// no-op or a rejected call must not copy. A receiver that names no
		// such path is a temporary and the write is isolated to a copy of it.
		defer exec.restore(exec.savedAddressedScope())
		var addressed bool
		receiver, addressed, err = exec.addressMutableReceiver(member.Object, env)
		if !addressed {
			// The permission in force belongs to an enclosing mutator's
			// receiver; an ordinary evaluation must not inherit it, or a
			// temporary that happens to be the recorded wrapper writes in
			// place (`a.push((true ? a : a).push(2))`).
			exec.withdrawAddressed()
			receiver, err = ordinaryReceiver()
		}
	} else {
		receiver, err = ordinaryReceiver()
	}
	if err != nil {
		return NewNil(), err
	}
	if call.Safe && receiver.Kind() == KindNil {
		return NewNil(), nil
	}
	if err := exec.checkMemoryValue(receiver); err != nil {
		return NewNil(), err
	}

	if result, handled, err := exec.evalDirectStringMemberCallExpr(call, receiver, member.Property, env); handled || err != nil {
		return result, err
	}

	if result, handled, err := exec.evalDirectArrayMemberCallExpr(call, receiver, member.Property, env); handled || err != nil {
		return result, err
	}

	if result, handled, err := exec.evalDirectCoreObjectMemberCallExpr(call, receiver, member, env); handled || err != nil {
		return result, err
	}

	if result, handled, err := exec.evalDirectTimeMemberCallExpr(call, receiver, member.Property, env); handled || err != nil {
		return result, err
	}

	if canCallBuiltinMemberDirect(receiver, member.Property) {
		return exec.evalDirectBuiltinMemberCallExpr(call, receiver, member.Property, env)
	}

	var callee Value
	resolution := calleeMemberValue
	if directCallee, handled, err := exec.evalDirectPublicMemberMethodCall(receiver, member.Property, member.Pos()); handled || err != nil {
		if err != nil {
			return NewNil(), err
		}
		callee = directCallee
		resolution = calleeMemberMethod
	} else {
		var err error
		callee, err = exec.getPublicMember(receiver, member.Property, member.Pos())
		if err != nil {
			return NewNil(), err
		}
	}

	if fn := singleNormalArgFunction(callee); fn != nil && len(call.Args) == 1 && len(call.KwArgs) == 0 && call.Block == nil && !callHasSplatArg(call) {
		return exec.evalSingleNormalArgFunctionMemberCallExpr(call, receiver, fn, env)
	}

	args, err := exec.evalCallArgsForCallee(call, env, callee)
	if err != nil {
		return NewNil(), err
	}
	kwargs, err := exec.evalCallKwArgsForCallee(call, env, callee, resolution)
	if err != nil {
		return NewNil(), err
	}
	args, kwargs = resolveKeywordOptionsHash(call, callee, resolution, args, kwargs)
	block, err := exec.evalCallBlock(call, env)
	if err != nil {
		return NewNil(), err
	}
	if err := exec.checkContext(); err != nil {
		return NewNil(), err
	}
	if err := exec.checkCallMemoryRootsWithCallee(callee, receiver, args, kwargs, block); err != nil {
		return NewNil(), err
	}

	result, callErr := exec.invokeCallable(callee, receiver, args, kwargs, block, call.Pos())
	retireCallBlock(block)
	if callErr != nil {
		return NewNil(), callErr
	}
	if err := exec.checkMemoryValue(result); err != nil {
		return NewNil(), err
	}
	return result, nil
}

func singleNormalArgFunction(callee Value) *ScriptFunction {
	if callee.Kind() != KindFunction {
		return nil
	}
	fn := valueFunction(callee)
	if fn == nil || len(fn.Params) != 1 || fn.Params[0].Kind != ParamNormal {
		return nil
	}
	// A typed parameter can carry a callable expectation that changes how the
	// argument expression evaluates (a function reference must not
	// auto-invoke), so only untyped single-parameter calls take the fast path.
	if fn.Params[0].Type != nil {
		return nil
	}
	return fn
}

func (exec *Execution) evalSingleNormalArgFunctionMemberCallExpr(call *CallExpr, receiver Value, fn *ScriptFunction, env *Env) (Value, error) {
	arg, err := exec.evalCallArg(call.Args[0], env)
	if err != nil {
		return NewNil(), err
	}
	if err := exec.checkContext(); err != nil {
		return NewNil(), err
	}
	if err := exec.checkMemoryWithPositionalCallRoots(receiver, arg, NewNil(), 1); err != nil {
		return NewNil(), err
	}

	result, callErr := exec.callFunctionWithSingleNormalArg(fn, receiver, arg, call.Pos(), true)
	if callErr != nil {
		if errors.Is(callErr, errLoopBreak) {
			return NewNil(), exec.localJumpErrorAt(call.Pos(), "break cannot cross call boundary")
		}
		if errors.Is(callErr, errLoopNext) {
			return NewNil(), exec.localJumpErrorAt(call.Pos(), "next cannot cross call boundary")
		}
		return NewNil(), callErr
	}
	if err := exec.checkMemoryValue(result); err != nil {
		return NewNil(), err
	}
	return result, nil
}

func (exec *Execution) evalMemberCallReceiver(member *MemberExpr, env *Env, objectAutoInvokes func(Expression, *Env) bool) (Value, error) {
	if member.Property != "call" {
		return exec.evalExpressionWithAuto(member.Object, env, memberCallReceiverAutoInvokes(member.Object, env))
	}
	objectMember, ok := member.Object.(*MemberExpr)
	if !ok {
		return exec.evalExpressionWithAuto(member.Object, env, objectAutoInvokes(member.Object, env))
	}
	receiver, err := exec.evalExpressionWithAuto(objectMember.Object, env, memberReceiverAutoInvokes(objectMember.Object, objectMember.Property, env))
	if err != nil {
		return NewNil(), err
	}
	if objectMember.Safe && receiver.Kind() == KindNil {
		return NewNil(), nil
	}
	if err := exec.checkMemoryValue(receiver); err != nil {
		return NewNil(), err
	}
	callee, err := exec.getPublicMember(receiver, objectMember.Property, objectMember.Pos())
	if err != nil {
		return NewNil(), err
	}
	if memberDataCallable(receiver, objectMember.Property, callee) {
		return callee, nil
	}
	return exec.autoInvokeIfNeeded(objectMember, callee, receiver)
}

func memberDataCallable(receiver Value, property string, member Value) bool {
	if !isInvocable(member) {
		return false
	}
	switch receiver.Kind() {
	case KindHash:
		data, ok := hashMemberData(receiver, property)
		return ok && data.Identical(member)
	case KindObject:
		data, ok := receiver.HashEntryMap()[property]
		return ok && data.Identical(member)
	case KindInstance:
		data, ok := valueInstance(receiver).Ivars[property]
		return ok && data.Identical(member)
	case KindClass:
		data, ok := valueClass(receiver).ClassVars[property]
		return ok && data.Identical(member)
	default:
		return false
	}
}

func (exec *Execution) evalDirectBuiltinMemberCallExpr(call *CallExpr, receiver Value, property string, env *Env) (Value, error) {
	args, err := exec.evalCallArgs(call, env)
	if err != nil {
		return NewNil(), err
	}
	kwargs, err := exec.evalCallKwArgs(call, env)
	if err != nil {
		return NewNil(), err
	}
	block, err := exec.evalCallBlock(call, env)
	if err != nil {
		return NewNil(), err
	}
	if err := exec.checkContext(); err != nil {
		return NewNil(), err
	}
	if err := exec.checkCallMemoryRoots(receiver, args, kwargs, block); err != nil {
		return NewNil(), err
	}

	result, err := callBuiltinMemberDirect(exec, receiver, property, args, kwargs, block)
	retireCallBlock(block)
	if err != nil {
		if ok, controlErr := exec.callBoundaryControlError(err, call.Pos()); ok {
			return NewNil(), controlErr
		}
		if ctxErr := exec.checkContext(); ctxErr != nil {
			return NewNil(), ctxErr
		}
		return NewNil(), exec.wrapError(err, call.Pos())
	}
	if err := exec.checkContext(); err != nil {
		return NewNil(), err
	}
	if err := exec.checkMemoryValue(result); err != nil {
		return NewNil(), err
	}
	return result, nil
}

func (exec *Execution) evalDirectStringMemberCallExpr(call *CallExpr, receiver Value, property string, env *Env) (Value, bool, error) {
	if receiver.Kind() != KindString || len(call.KwArgs) != 0 || call.Block != nil || callHasSplatArg(call) {
		return NewNil(), false, nil
	}

	switch property {
	case "size", "length":
		if len(call.Args) > 0 {
			return NewNil(), false, nil
		}
		// This fast path bypasses member dispatch, so it does not get the scan
		// charge chargeStringScanBeforeCall applies there and must charge for
		// itself. bytesize below reads a length field and stays exempt.
		if err := exec.chargeStringScan(len(receiver.String())); err != nil {
			return NewNil(), true, err
		}
		if err := exec.checkContext(); err != nil {
			return NewNil(), true, err
		}
		if err := exec.checkCallMemoryRoots(receiver, nil, nil, NewNil()); err != nil {
			return NewNil(), true, err
		}
		result := NewInt(int64(stringRuneLen(receiver.String())))
		if err := exec.checkMemoryValue(result); err != nil {
			return NewNil(), true, err
		}
		return result, true, nil
	case "bytesize":
		if len(call.Args) > 0 {
			return NewNil(), false, nil
		}
		if err := exec.checkContext(); err != nil {
			return NewNil(), true, err
		}
		if err := exec.checkCallMemoryRoots(receiver, nil, nil, NewNil()); err != nil {
			return NewNil(), true, err
		}
		result := NewInt(int64(len(receiver.String())))
		if err := exec.checkMemoryValue(result); err != nil {
			return NewNil(), true, err
		}
		return result, true, nil
	case "index":
		return exec.evalDirectStringIndexCall(call, receiver, env)
	case "rindex":
		return exec.evalDirectStringRIndexCall(call, receiver, env)
	case "slice":
		return exec.evalDirectStringSliceCall(call, receiver, env)
	case "split":
		return exec.evalDirectStringSplitCall(call, receiver, env)
	default:
		return NewNil(), false, nil
	}
}

func (exec *Execution) evalDirectStringSplitCall(call *CallExpr, receiver Value, env *Env) (Value, bool, error) {
	if len(call.Args) > 2 {
		return NewNil(), false, nil
	}
	arg0 := NewNil()
	arg1 := NewNil()
	if len(call.Args) > 0 {
		var err error
		arg0, err = exec.evalCallArg(call.Args[0], env)
		if err != nil {
			return NewNil(), true, err
		}
	}
	if len(call.Args) > 1 {
		var err error
		arg1, err = exec.evalCallArg(call.Args[1], env)
		if err != nil {
			return NewNil(), true, err
		}
	}
	// Charged after the arguments are evaluated, not before: member dispatch
	// evaluates arguments and only then runs the metering wrapper, so charging
	// earlier here would let a tight quota skip an argument's side effect on
	// this path while the equivalent dispatched call still performs it. The
	// charge itself is the one member dispatch applies, so both entrances to
	// the method cost the same.
	if err := exec.chargeStringCall(receiver, []Value{arg0, arg1}, stringArgumentCapFactor("string.split")); err != nil {
		return NewNil(), true, err
	}
	if err := exec.checkContext(); err != nil {
		return NewNil(), true, err
	}
	if err := exec.checkMemoryWithPositionalCallRoots(receiver, arg0, arg1, len(call.Args)); err != nil {
		return NewNil(), true, err
	}
	result, err := stringSplitResultFromPositionalRoots(exec, receiver, arg0, arg1, len(call.Args))
	if err != nil {
		if ctxErr := exec.checkContext(); ctxErr != nil {
			return NewNil(), true, ctxErr
		}
		return NewNil(), true, exec.wrapError(err, call.Pos())
	}
	if err := exec.checkMemoryValue(result); err != nil {
		return NewNil(), true, err
	}
	return result, true, nil
}

func (exec *Execution) evalDirectArrayMemberCallExpr(call *CallExpr, receiver Value, property string, env *Env) (Value, bool, error) {
	if receiver.Kind() != KindArray || property != "join" || len(call.KwArgs) != 0 || call.Block != nil || callHasSplatArg(call) {
		return NewNil(), false, nil
	}
	if len(call.Args) > 1 {
		return NewNil(), false, nil
	}
	arg0 := NewNil()
	if len(call.Args) == 1 {
		var err error
		arg0, err = exec.evalCallArg(call.Args[0], env)
		if err != nil {
			return NewNil(), true, err
		}
	}
	if err := exec.checkContext(); err != nil {
		return NewNil(), true, err
	}
	if err := exec.checkMemoryWithPositionalCallRoots(receiver, arg0, NewNil(), len(call.Args)); err != nil {
		return NewNil(), true, err
	}
	sep, err := arrayJoinSeparator(arg0, len(call.Args))
	if err != nil {
		if ctxErr := exec.checkContext(); ctxErr != nil {
			return NewNil(), true, ctxErr
		}
		return NewNil(), true, exec.wrapError(err, call.Pos())
	}
	payload, err := arrayJoinPayload(exec, receiver, sep)
	if err != nil {
		if ctxErr := exec.checkContext(); ctxErr != nil {
			return NewNil(), true, ctxErr
		}
		return NewNil(), true, exec.wrapError(err, call.Pos())
	}
	var b strings.Builder
	if err := exec.checkProjectedStringBytesWithPositionalCallRoots(projectedBuilderCap(&b, payload), receiver, arg0, NewNil(), len(call.Args)); err != nil {
		return NewNil(), true, err
	}
	result, err := arrayJoinResult(receiver, sep, payload, &b)
	if err != nil {
		if ctxErr := exec.checkContext(); ctxErr != nil {
			return NewNil(), true, ctxErr
		}
		return NewNil(), true, exec.wrapError(err, call.Pos())
	}
	if err := exec.checkContext(); err != nil {
		return NewNil(), true, err
	}
	if err := exec.checkMemoryValue(result); err != nil {
		return NewNil(), true, err
	}
	return result, true, nil
}

// directStringCallArgs collects the arguments a direct string call evaluated,
// so its metering sees exactly what member dispatch would. An offset that is
// not an integer is still an argument the dispatched form charges for, and
// leaving it out made the two entrances disagree for s.index("x", bad).
func directStringCallArgs(needle, offset Value, hasOffset bool) []Value {
	if !hasOffset {
		return []Value{needle}
	}
	return []Value{needle, offset}
}

func (exec *Execution) evalDirectStringIndexCall(call *CallExpr, receiver Value, env *Env) (Value, bool, error) {
	if len(call.Args) < 1 || len(call.Args) > 2 {
		return NewNil(), false, nil
	}
	needle, err := exec.evalCallArg(call.Args[0], env)
	if err != nil {
		return NewNil(), true, err
	}
	offset := 0
	var offsetVal Value
	hasOffset := false
	if len(call.Args) == 2 {
		offsetVal, err = exec.evalCallArg(call.Args[1], env)
		if err != nil {
			return NewNil(), true, err
		}
		hasOffset = true
	}
	// Charged after the arguments are evaluated, not before: member dispatch
	// evaluates arguments and only then runs the metering wrapper, so charging
	// earlier here would let a tight quota skip an argument's side effect on
	// this path while the equivalent dispatched call still performs it. The
	// charge itself is the one member dispatch applies, so both entrances to
	// the method cost the same.
	if err := exec.chargeStringCall(receiver, directStringCallArgs(needle, offsetVal, hasOffset), stringArgumentCapFactor("string.index")); err != nil {
		return NewNil(), true, err
	}
	if err := exec.checkContext(); err != nil {
		return NewNil(), true, err
	}
	if err := checkDirectStringMemberCallRoots(exec, receiver, needle, offsetVal, hasOffset); err != nil {
		return NewNil(), true, err
	}
	if hasOffset {
		i, err := valueToInt(offsetVal)
		if err != nil {
			return NewNil(), true, exec.wrapError(fmt.Errorf("string.index offset must be integer"), call.Pos())
		}
		offset = i
	}
	result, err := stringIndexResult(exec, receiver, needle, offset)
	if err != nil {
		return NewNil(), true, exec.wrapError(err, call.Pos())
	}
	if err := exec.checkMemoryValue(result); err != nil {
		return NewNil(), true, err
	}
	return result, true, nil
}

func (exec *Execution) evalDirectStringRIndexCall(call *CallExpr, receiver Value, env *Env) (Value, bool, error) {
	if len(call.Args) < 1 || len(call.Args) > 2 {
		return NewNil(), false, nil
	}
	needle, err := exec.evalCallArg(call.Args[0], env)
	if err != nil {
		return NewNil(), true, err
	}
	var offsetVal Value
	hasOffset := false
	if len(call.Args) == 2 {
		offsetVal, err = exec.evalCallArg(call.Args[1], env)
		if err != nil {
			return NewNil(), true, err
		}
		hasOffset = true
	}
	// Charged after the arguments are evaluated, not before: member dispatch
	// evaluates arguments and only then runs the metering wrapper, so charging
	// earlier here would let a tight quota skip an argument's side effect on
	// this path while the equivalent dispatched call still performs it. The
	// charge itself is the one member dispatch applies, so both entrances to
	// the method cost the same.
	if err := exec.chargeStringCall(receiver, directStringCallArgs(needle, offsetVal, hasOffset), stringArgumentCapFactor("string.rindex")); err != nil {
		return NewNil(), true, err
	}
	// The default offset counts the receiver's runes, an O(n) scan of its own.
	// It runs after the charge so an exhausted quota stops it, and only when no
	// explicit offset was given, since an explicit one replaces it.
	offset := 0
	if !hasOffset {
		offset = stringRuneLen(receiver.String())
	}
	if err := exec.checkContext(); err != nil {
		return NewNil(), true, err
	}
	if err := checkDirectStringMemberCallRoots(exec, receiver, needle, offsetVal, hasOffset); err != nil {
		return NewNil(), true, err
	}
	if hasOffset {
		i, err := valueToInt(offsetVal)
		if err != nil {
			return NewNil(), true, exec.wrapError(fmt.Errorf("string.rindex offset must be integer"), call.Pos())
		}
		effective, ok := stringEffectiveOffset(receiver.String(), i)
		if !ok {
			result := NewNil()
			if err := exec.checkMemoryValue(result); err != nil {
				return NewNil(), true, err
			}
			return result, true, nil
		}
		offset = effective
	}
	result, err := stringRIndexResult(exec, receiver, needle, offset)
	if err != nil {
		return NewNil(), true, exec.wrapError(err, call.Pos())
	}
	if err := exec.checkMemoryValue(result); err != nil {
		return NewNil(), true, err
	}
	return result, true, nil
}

func (exec *Execution) evalDirectStringSliceCall(call *CallExpr, receiver Value, env *Env) (Value, bool, error) {
	if len(call.Args) < 1 || len(call.Args) > 2 {
		return NewNil(), false, nil
	}
	first, err := exec.evalCallArg(call.Args[0], env)
	if err != nil {
		return NewNil(), true, err
	}
	var second Value
	if len(call.Args) == 2 {
		second, err = exec.evalCallArg(call.Args[1], env)
		if err != nil {
			return NewNil(), true, err
		}
	}
	// Charged after the arguments are evaluated, not before: member dispatch
	// evaluates arguments and only then runs the metering wrapper, so charging
	// earlier here would let a tight quota skip an argument's side effect on
	// this path while the equivalent dispatched call still performs it. The
	// charge itself is the one member dispatch applies, so both entrances to
	// the method cost the same.
	if err := exec.chargeStringCall(receiver, []Value{first, second}, stringArgumentCapFactor("string.slice")); err != nil {
		return NewNil(), true, err
	}
	if err := exec.checkContext(); err != nil {
		return NewNil(), true, err
	}
	if err := checkDirectStringMemberCallRoots(exec, receiver, first, second, len(call.Args) == 2); err != nil {
		return NewNil(), true, err
	}
	// This fast path is only reached when the call has no keyword arguments and
	// no block (evalDirectStringMemberCallExpr rejects both), so there are no
	// further roots to keep live across the copy.
	result, err := stringSliceResult(exec, receiver, first, second, len(call.Args) == 2, nil, NewNil())
	if err != nil {
		return NewNil(), true, exec.wrapError(err, call.Pos())
	}
	if err := exec.checkMemoryValue(result); err != nil {
		return NewNil(), true, err
	}
	return result, true, nil
}

func checkDirectStringMemberCallRoots(exec *Execution, receiver, first, second Value, hasSecond bool) error {
	if !hasSecond {
		return exec.checkMemoryWith(receiver, first)
	}
	return exec.checkMemoryWith(receiver, first, second)
}

func (exec *Execution) evalDirectCoreObjectMemberCallExpr(call *CallExpr, receiver Value, member *MemberExpr, env *Env) (Value, bool, error) {
	if receiver.Kind() != KindObject || len(call.KwArgs) != 0 || call.Block != nil || callHasSplatArg(call) {
		return NewNil(), false, nil
	}

	callee, err := exec.getPublicMember(receiver, member.Property, member.Pos())
	if err != nil {
		return NewNil(), false, nil
	}
	builtin := valueBuiltin(callee)
	if builtin == nil {
		return NewNil(), false, nil
	}

	switch builtin.Name {
	case "Regex.replace_all":
		if !exec.isCoreObjectBuiltin(builtin, "Regex", "replace_all") {
			return NewNil(), false, nil
		}
		return exec.evalDirectRegexReplaceCall(call, receiver, env, true)
	case "Time.parse":
		if !exec.isCoreObjectBuiltin(builtin, "Time", "parse") {
			return NewNil(), false, nil
		}
		return exec.evalDirectTimeParseCall(call, receiver, env)
	default:
		return NewNil(), false, nil
	}
}

func (exec *Execution) isCoreObjectBuiltin(builtin *Builtin, namespace, member string) bool {
	if builtin == nil {
		return false
	}
	exec.engine.builtinsMu.RLock()
	defer exec.engine.builtinsMu.RUnlock()
	obj, ok := exec.engine.builtins[namespace]
	if !ok || obj.Kind() != KindObject {
		return false
	}
	core, ok := obj.HashEntryMap()[member]
	if !ok {
		return false
	}
	return valueBuiltin(core) == builtin
}

func (exec *Execution) evalDirectRegexReplaceCall(call *CallExpr, receiver Value, env *Env, replaceAll bool) (Value, bool, error) {
	if len(call.Args) != 3 {
		return NewNil(), false, nil
	}
	text, err := exec.evalCallArg(call.Args[0], env)
	if err != nil {
		return NewNil(), true, err
	}
	pattern, err := exec.evalCallArg(call.Args[1], env)
	if err != nil {
		return NewNil(), true, err
	}
	replacement, err := exec.evalCallArg(call.Args[2], env)
	if err != nil {
		return NewNil(), true, err
	}
	if err := exec.checkContext(); err != nil {
		return NewNil(), true, err
	}
	if err := exec.checkMemoryWith(receiver, text, pattern, replacement); err != nil {
		return NewNil(), true, err
	}
	result, err := builtinRegexReplaceValues(exec, text, pattern, replacement, replaceAll)
	if err != nil {
		return NewNil(), true, exec.wrapError(err, call.Pos())
	}
	if err := exec.checkContext(); err != nil {
		return NewNil(), true, err
	}
	if err := exec.checkMemoryValue(result); err != nil {
		return NewNil(), true, err
	}
	return result, true, nil
}

func (exec *Execution) evalDirectTimeParseCall(call *CallExpr, receiver Value, env *Env) (Value, bool, error) {
	if len(call.Args) < 1 || len(call.Args) > 2 {
		return NewNil(), false, nil
	}
	input, err := exec.evalCallArg(call.Args[0], env)
	if err != nil {
		return NewNil(), true, err
	}
	var layout Value
	hasLayout := false
	if len(call.Args) == 2 {
		layout, err = exec.evalCallArg(call.Args[1], env)
		if err != nil {
			return NewNil(), true, err
		}
		hasLayout = true
	}
	if err := exec.checkContext(); err != nil {
		return NewNil(), true, err
	}
	if hasLayout {
		if err := exec.checkMemoryWith(receiver, input, layout); err != nil {
			return NewNil(), true, err
		}
	} else if err := exec.checkMemoryWith(receiver, input); err != nil {
		return NewNil(), true, err
	}
	result, err := timeParseResult(input, layout, hasLayout, nil)
	if err != nil {
		return NewNil(), true, exec.wrapError(err, call.Pos())
	}
	if err := exec.checkMemoryValue(result); err != nil {
		return NewNil(), true, err
	}
	return result, true, nil
}

func (exec *Execution) evalDirectTimeMemberCallExpr(call *CallExpr, receiver Value, property string, env *Env) (Value, bool, error) {
	if receiver.Kind() != KindTime || len(call.KwArgs) != 0 || call.Block != nil || callHasSplatArg(call) {
		return NewNil(), false, nil
	}
	switch property {
	case "format":
		return exec.evalDirectTimeFormatCall(call, receiver, env)
	default:
		return NewNil(), false, nil
	}
}

func (exec *Execution) evalDirectTimeFormatCall(call *CallExpr, receiver Value, env *Env) (Value, bool, error) {
	if len(call.Args) != 1 {
		return NewNil(), false, nil
	}
	layout, err := exec.evalCallArg(call.Args[0], env)
	if err != nil {
		return NewNil(), true, err
	}
	if err := exec.checkContext(); err != nil {
		return NewNil(), true, err
	}
	if err := exec.checkMemoryWith(receiver, layout); err != nil {
		return NewNil(), true, err
	}
	result, err := timeFormatResult(exec, receiver.Time(), layout)
	if err != nil {
		return NewNil(), true, exec.wrapError(err, call.Pos())
	}
	if err := exec.checkMemoryValue(result); err != nil {
		return NewNil(), true, err
	}
	return result, true, nil
}

func canCallBuiltinMemberDirect(receiver Value, property string) bool {
	switch receiver.Kind() {
	case KindDuration:
		return canCallDurationMemberDirect(property)
	case KindTime:
		return canCallTimeMemberDirect(property)
	default:
		return false
	}
}

func callBuiltinMemberDirect(exec *Execution, receiver Value, property string, args []Value, kwargs map[string]Value, block Value) (Value, error) {
	switch receiver.Kind() {
	case KindDuration:
		return callDurationMemberDirect(receiver.Duration(), property, args, kwargs, block)
	case KindTime:
		return callTimeMemberDirect(exec, receiver.Time(), property, args, kwargs, block)
	default:
		return NewNil(), fmt.Errorf("unsupported member access on %s", receiver.Kind())
	}
}

// hostGlobalLazyBinding defers rebinding one host-provided global until the
// script first reads it. Binding a global eagerly deep-copies the host value
// into the call root even when the function never touches it, so composites
// bind lazily and the copy happens on first read. The environment stores the
// materialized clone back into the binding, so the copy runs at most once per
// call and mutation visibility across repeated reads matches the eager
// behavior exactly.
// Strict-effects validation stays eager in bindGlobalsForCallLazy, so a
// global that would be rejected at bind time is still rejected at bind time.
type hostGlobalLazyBinding struct {
	rebinder *callFunctionRebinder
	value    Value
}

func (binding hostGlobalLazyBinding) materialize() Value {
	return binding.rebinder.rebindGlobalValue(binding.value)
}

// hostGlobalBindsEagerly reports whether a host global binds its value at call
// entry. Immutable scalars need no copy, so deferring them buys nothing and
// would cost a lazy-materialization hop on every first read; enums bind
// eagerly so they resolve as constants exactly like the per-call enum clones.
func hostGlobalBindsEagerly(val Value) bool {
	return immutableDataValue(val) || val.Kind() == KindEnum
}

// immutableDataValue reports whether a value's data cannot be mutated through
// the value, so a binding can share it instead of copying it.
func immutableDataValue(val Value) bool {
	switch val.Kind() {
	case KindNil, KindBool, KindInt, KindFloat, KindString, KindMoney, KindDuration, KindTime, KindSymbol, KindRange, KindRegex:
		return true
	default:
		return false
	}
}

// globalsBindLazily reports whether any global would take a lazy binding, in
// which case Script.Call runs on the lazy-globals path.
func globalsBindLazily(globals map[string]Value) bool {
	for _, val := range globals {
		if !hostGlobalBindsEagerly(val) {
			return true
		}
	}
	return false
}

// bindGlobalsForCall eagerly binds globals into the call root. The plain
// Script.Call path uses it only when every global binds eagerly (see
// globalsBindLazily); calls carrying composite globals are routed through
// callWithLazyGlobals, whose bindGlobalsForCallLazy defers their copies.
// Keeping the lazy-binding store out of this function keeps the rebinder
// from escaping to the heap on the plain Call hot path.
func bindGlobalsForCall(exec *Execution, root *Env, rebinder *callFunctionRebinder, globals map[string]Value) error {
	if err := exec.checkContext(); err != nil {
		return err
	}

	if exec.strictEffects {
		if err := validateStrictGlobals(globals); err != nil {
			return err
		}
	}

	for name, val := range globals {
		rebound := rebinder.rebindValue(val)
		root.Define(name, rebound)
		if graphContainsBuiltin(rebound) {
			// A global carrying host builtins is host-visible state like a
			// capability object: dispatching on it keeps its factory channel
			// live, and post-dispatch marking covers what a builtin installs
			// into it (#1210). The lazy path records the same at
			// materialization.
			exec.recordHostStateRoot(rebound)
		}
	}

	return nil
}

// bindGlobalsForCallLazy binds host globals with deferred copies: immutable
// scalars and enums bind eagerly (nothing to defer), while composites bind as
// lazy env bindings that deep-copy through the rebinder on first read.
func bindGlobalsForCallLazy(exec *Execution, root *Env, rebinder *callFunctionRebinder, globals map[string]Value) error {
	if err := exec.checkContext(); err != nil {
		return err
	}

	if exec.strictEffects {
		if err := validateStrictGlobals(globals); err != nil {
			return err
		}
	}

	for name, val := range globals {
		if hostGlobalBindsEagerly(val) {
			root.Define(name, rebinder.rebindValue(val))
			continue
		}
		root.defineLazy(name, hostGlobalLazyBinding{rebinder: rebinder, value: val})
		rebinder.pendingGlobalSources = append(rebinder.pendingGlobalSources, val)
		rebinder.deferGlobalSource(val)
	}
	// Deferred globals may be read after the arguments are fast-copied; make
	// those copies register their composites so the deferred global scan and
	// slow-path materialization can still deduplicate a global source that
	// aliases an argument.
	if len(rebinder.pendingGlobalSources) > 0 {
		rebinder.inboundRegister = true
	}

	return nil
}

// executeFunctionForCall evaluates the entry function's body. token is the
// invocation token the caller pushed BEFORE binding arguments (so a block
// built by a default-argument expression homes to this invocation); the
// caller pops it after this returns.
func executeFunctionForCall(exec *Execution, fn *ScriptFunction, callEnv *Env, token uint64) (Value, error) {
	if err := exec.pushFrame(fn.Name, fn.Pos, fn.owner, fn.owner); err != nil {
		return NewNil(), err
	}
	if fn.Accessor == functionAccessorSetter {
		val, err := exec.executeGeneratedSetter(fn, callEnv)
		exec.popFrame()
		if err != nil {
			return NewNil(), err
		}
		return val, nil
	}
	val, returned, err := exec.evalLocalScopeStatements(fn.Body, callEnv)
	val, returned, err = consumeFunctionReturnSignal(val, returned, err)
	if sig := matchNonLocalReturn(err, token); sig != nil {
		val = sig.value
		returned = true
		err = nil
	} else if isNonLocalReturnSignal(err) {
		// Defensive backstop: signals only emit while their target is live, so
		// one reaching the Call boundary unconsumed indicates a frame that
		// unwound without matching. Surface it as LocalJumpError rather than a
		// raw signal.
		err = exec.localJumpErrorAt(fn.Pos, "unexpected return")
	}
	if err != nil {
		err = exec.wrapError(err, fn.Pos)
	}
	exec.popFrame()
	if err != nil {
		return NewNil(), err
	}
	val = callEnv.settleArrayAppendResult(val)
	// Backstop for the host-visible termination guarantee: if anything
	// absorbed the latched exhaustion error on the way here — a capability
	// adapter swallowing the raw error is the known path — the call must
	// still fail with it rather than return a result. It runs before the
	// context check so an adapter that also canceled the context cannot
	// downgrade the promised quota termination into context.Canceled.
	if exec.exhausted != nil {
		return NewNil(), exec.wrapError(exec.exhausted, fn.Pos)
	}
	if err := exec.checkContext(); err != nil {
		return NewNil(), err
	}
	val, err = finishFunctionForCall(exec, fn, val)
	if err != nil {
		return NewNil(), err
	}
	if returned {
		return val, nil
	}
	return val, nil
}

// finishFunctionForCall applies the entry function's return-type validation
// and result memory charge. It is shared by the normal body path and the
// bind-time non-local return path (a default-argument block returning during
// prepareCallEnvForFunction), so both validate identically.
func finishFunctionForCall(exec *Execution, fn *ScriptFunction, val Value) (Value, error) {
	if fn.ReturnTy != nil {
		normalized, err := normalizeValueForType(val, fn.ReturnTy, typeContext{
			owner:    fn.owner,
			env:      fn.Env,
			fallback: exec.root,
			exec:     exec,
		})
		if err != nil {
			if isHostControlSignal(err) {
				return NewNil(), err
			}
			if isNormalizationLimitError(err) {
				return NewNil(), exec.wrapError(err, fn.Pos)
			}
			return NewNil(), exec.errorAt(fn.Pos, "%s", formatReturnTypeMismatch(fn.Name, err))
		}
		val = normalized
	}
	if err := exec.checkMemoryValue(val); err != nil {
		return NewNil(), exec.wrapError(err, fn.Pos)
	}
	return val, nil
}

func (exec *Execution) executeGeneratedSetter(fn *ScriptFunction, callEnv *Env) (Value, error) {
	self, ok := callEnv.Get("self")
	if !ok || self.Kind() != KindInstance {
		return NewNil(), exec.errorAt(fn.Pos, "no instance context for property setter")
	}
	val, ok := callEnv.Get("value")
	if !ok {
		return NewNil(), exec.errorAt(fn.Pos, "missing property setter value")
	}
	bumpMutationEpoch()
	publishBindingReplacement(valueInstance(self).Ivars[fn.AccessorName], val)
	valueInstance(self).Ivars[fn.AccessorName] = val
	val = callEnv.settleArrayAppendResult(val)
	if err := exec.checkContext(); err != nil {
		return NewNil(), err
	}
	if err := exec.checkMemoryValue(val); err != nil {
		return NewNil(), exec.wrapError(err, fn.Pos)
	}
	return val, nil
}

// validateCallShape checks that args and kwargs can satisfy fn's parameters
// before any default is evaluated. It reproduces the positional and keyword
// consumption of the binding loop so it reports the same missing-argument,
// leftover-positional, and unexpected-keyword errors, but it touches no default
// expression. Surfacing these mismatches first keeps a defaulted parameter's
// side effects, errors, or step-quota cost from masking a call that can never
// bind, such as f(1) against def f(a: expensive()) or an omitted required
// keyword that follows a defaulted one.
func (exec *Execution) validateCallShape(fn *ScriptFunction, args []Value, kwargs map[string]Value, pos Position) error {
	var usedKw map[string]bool
	if len(kwargs) > 0 {
		usedKw = make(map[string]bool, len(kwargs))
	}
	argIdx := 0

	for _, param := range fn.Params {
		switch param.Kind {
		case ParamKeyword:
			if _, ok := kwargs[param.Name]; ok {
				if usedKw != nil {
					usedKw[param.Name] = true
				}
			} else if param.DefaultVal == nil {
				return exec.argumentErrorAt(pos, "missing keyword argument %s", param.Name)
			}
		case ParamRest:
			argIdx = len(args)
		case ParamKeywordRest:
			for name := range kwargs {
				if usedKw != nil {
					usedKw[name] = true
				}
			}
		case ParamNormal:
			if argIdx < len(args) {
				argIdx++
			} else if _, ok := kwargs[param.Name]; ok {
				if usedKw != nil {
					usedKw[param.Name] = true
				}
			} else if param.DefaultVal == nil {
				return exec.argumentErrorAt(pos, "missing argument %s", param.Name)
			}
		default:
			return exec.errorAt(pos, "unknown parameter kind for %s", param.Name)
		}
	}

	if argIdx < len(args) {
		return exec.argumentErrorAt(pos, "unexpected positional arguments")
	}
	if usedKw != nil {
		for name := range kwargs {
			if !usedKw[name] {
				return exec.argumentErrorAt(pos, "unexpected keyword argument %s", name)
			}
		}
	}
	return nil
}

func (exec *Execution) bindFunctionArgs(fn *ScriptFunction, env *Env, args []Value, kwargs map[string]Value, pos Position) error {
	// Parameter defaults evaluate before the callee's module context is
	// pushed, so typed host builtins invoked from a default must still
	// resolve named signature types against the callee's own source. The
	// binding owner carries that context for the duration of the bind.
	if fn.owner != nil {
		previousOwner := exec.bindingOwner
		exec.bindingOwner = fn.owner
		defer func() { exec.bindingOwner = previousOwner }()
	}
	// Validate the whole call shape before binding so that no parameter default
	// is evaluated when the call can never bind successfully. A default may have
	// side effects, raise an error, or consume the step quota, and evaluating it
	// would mask the real arity or keyword mismatch. validateCallShape mirrors
	// the binding loop's positional/keyword bookkeeping without evaluating any
	// default expression.
	if err := exec.validateCallShape(fn, args, kwargs, pos); err != nil {
		return err
	}

	var usedKw map[string]bool
	if len(kwargs) > 0 {
		usedKw = make(map[string]bool, len(kwargs))
	}
	argIdx := 0

	for _, param := range fn.Params {
		var val Value
		switch param.Kind {
		case ParamKeyword:
			if kw, ok := kwargs[param.Name]; ok {
				val = kw
				if usedKw != nil {
					usedKw[param.Name] = true
				}
			} else if param.DefaultVal != nil {
				defaultVal, err := exec.evalExpressionWithExpectation(param.DefaultVal, env, typeExpressionExpectation(param.Type))
				if err != nil {
					return err
				}
				val = defaultVal
			} else {
				return exec.argumentErrorAt(pos, "missing keyword argument %s", param.Name)
			}
		case ParamRest:
			rest := append([]Value(nil), args[argIdx:]...)
			val = NewArray(rest)
			argIdx = len(args)
		case ParamKeywordRest:
			rest := make(map[string]Value)
			for name, kw := range kwargs {
				if usedKw != nil && usedKw[name] {
					continue
				}
				rest[name] = kw
				if usedKw != nil {
					usedKw[name] = true
				}
			}
			val = NewHash(rest)
		case ParamNormal:
			if argIdx < len(args) {
				val = args[argIdx]
				argIdx++
			} else if kw, ok := kwargs[param.Name]; ok {
				val = kw
				if usedKw != nil {
					usedKw[param.Name] = true
				}
			} else if param.DefaultVal != nil {
				defaultVal, err := exec.evalExpressionWithExpectation(param.DefaultVal, env, positionalArgumentExpectation(param))
				if err != nil {
					return err
				}
				val = defaultVal
			} else {
				return exec.argumentErrorAt(pos, "missing argument %s", param.Name)
			}
		default:
			return exec.errorAt(pos, "unknown parameter kind for %s", param.Name)
		}

		if err := exec.bindFunctionParamValue(fn, env, param, val, pos); err != nil {
			return err
		}
	}

	if argIdx < len(args) {
		return exec.errorAt(pos, "unexpected positional arguments")
	}
	if usedKw != nil {
		for name := range kwargs {
			if !usedKw[name] {
				return exec.errorAt(pos, "unexpected keyword argument %s", name)
			}
		}
	}
	return nil
}

func (exec *Execution) bindFunctionParamValue(fn *ScriptFunction, env *Env, param Param, val Value, pos Position) error {
	if param.Type != nil {
		normalized, err := normalizeValueForType(val, param.Type, typeContext{
			owner:    fn.owner,
			env:      fn.Env,
			fallback: exec.root,
			exec:     exec,
		})
		if err != nil {
			if isHostControlSignal(err) {
				return err
			}
			if isNormalizationLimitError(err) {
				return exec.wrapError(err, pos)
			}
			return exec.errorAt(pos, "%s", formatArgumentTypeMismatch(param.Name, err))
		}
		val = normalized
	}
	env.Define(param.Name, val)
	if param.IsIvar {
		if selfVal, ok := env.Get("self"); ok && selfVal.Kind() == KindInstance {
			inst := valueInstance(selfVal)
			if inst != nil {
				// An ivar parameter is a direct write to the backing ivar, so
				// the declared property contract applies on top of any
				// parameter annotation already validated above.
				normalized, err := exec.normalizeIvarWrite(inst, param.Name, val, pos)
				if err != nil {
					return err
				}
				bumpMutationEpoch()
				publishBindingReplacement(inst.Ivars[param.Name], normalized)
				inst.Ivars[param.Name] = normalized
			}
		}
	}
	return nil
}

// objectCloneKey identifies a cloned attribute bag by its source entry map and
// the provenance of the wrapper it was cloned for. Differently tagged wrappers
// over one map get independent clones, and each still terminates cycles.
type objectCloneKey struct {
	ptr uintptr
	tag ObjectTag
}
