package runtime

import (
	"fmt"
)

// Return summaries expose a known result fact for calls to unannotated
// script functions and statically resolved methods: a callable that provably
// returns an int should not pass a string boundary just because its author
// omitted the annotation. A summary is computed once per checked script by a
// suppressed walk of the callable body that records the inferred fact of
// every value-yielding path — explicit returns, the implicit final expression,
// and nil fallthrough — and joins them with the existing union rules. Exact
// constructor origins travel with those paths so a getter called on a helper's
// result can reuse the constructor's final ivar state. Any path the checker
// cannot prove (dynamic calls, recursion, unmodeled constructs) makes the whole
// summary unknown, and explicit return annotations stay authoritative.

// returnSummaryCollector accumulates the facts of every way a callable body
// can yield a value during a summary walk. A single unknown path poisons the
// collector: the summary must describe every possible result or none.
type returnSummaryCollector struct {
	arms                   []*TypeExpr
	unknown                bool
	instanceOrigins        []Expression
	instanceOriginsUnknown bool
	failureParamFacts      map[string]checkReachableParamFact
}

type returnSummaryCacheKey struct {
	fn      *ScriptFunction
	context string
}

type functionReturnAnalysis struct {
	result               *TypeExpr
	mayComplete          bool
	bodyMayComplete      bool
	instanceOrigins      []Expression
	instanceOriginsExact bool
	failureParamFacts    map[string]checkReachableParamFact
}

func (r *returnSummaryCollector) record(fact *TypeExpr) {
	if r == nil || r.unknown {
		return
	}
	if fact == nil {
		r.unknown = true
		r.arms = nil
		return
	}
	r.arms = append(r.arms, fact)
}

func (r *returnSummaryCollector) recordResult(
	fact *TypeExpr,
	instanceOrigins []Expression,
	instanceOriginsExact bool,
) {
	if r == nil {
		return
	}
	r.record(fact)
	if r.instanceOriginsUnknown {
		return
	}
	if !instanceOriginsExact || len(instanceOrigins) == 0 {
		r.instanceOrigins = nil
		r.instanceOriginsUnknown = true
		return
	}
	r.instanceOrigins = normalizeCheckExpressionIdentities(
		append(r.instanceOrigins, instanceOrigins...),
	)
}

func (r *returnSummaryCollector) mergeResultArms(other *returnSummaryCollector) {
	if other == nil {
		return
	}
	if other.unknown {
		r.recordResult(nil, nil, false)
		return
	}
	for _, arm := range other.arms {
		r.record(arm)
	}
	if !other.sawReturn() {
		return
	}
	if other.instanceOriginsUnknown || len(other.instanceOrigins) == 0 {
		r.instanceOrigins = nil
		r.instanceOriginsUnknown = true
		return
	}
	if !r.instanceOriginsUnknown {
		r.instanceOrigins = normalizeCheckExpressionIdentities(
			append(r.instanceOrigins, other.instanceOrigins...),
		)
	}
}

// sawReturn reports whether the collector reached any return path at all.
func (r *returnSummaryCollector) sawReturn() bool {
	return r != nil && (r.unknown || len(r.arms) > 0)
}

func (c *scriptChecker) recordReturnSummaryResult(
	collector *returnSummaryCollector,
	expr Expression,
	fact *TypeExpr,
) {
	if collector == nil {
		return
	}
	origins, exact := c.instanceValueOrigins(expr)
	collector.recordResult(fact, origins, exact)
}

// scriptCallableReturnSummary reports the summary for calls resolved to an
// exact plain function, instance method, or class method of the checked script.
// Callable resolution has already rejected dynamic receivers and host overrides;
// this ownership check prevents foreign or stored functions from borrowing a
// same-named script callable's body. Summaries are computed lazily so a caller
// can recursively summarize its callees without depending on declaration
// order. Annotated callables stay authoritative.
func (c *scriptChecker) scriptCallableReturnSummary(call *CallExpr, target staticCallable) *TypeExpr {
	owned, ok := c.resolveOwnedScriptCallable(target)
	if !ok || owned.ReturnTy != nil {
		return nil
	}
	// User methods that override the universal Object-level surface retain
	// the gradual behavior of those overridable dispatch points. An explicit
	// annotation can still make their result authoritative.
	if (target.resolution == calleeMemberMethod || target.resolution == calleeForwardedMethod) &&
		isUniversalMember(owned.Name) {
		return nil
	}
	target.fn = owned
	runnable, hashSupplied, definite := callRunnableDefaults(call, target)
	paramFacts := c.summaryCallParamFacts(call, target, definite)
	return c.functionReturnAnalysis(
		owned,
		runnable,
		hashSupplied,
		definite,
		paramFacts,
		callMaySupplyBlock(call),
	).result
}

func (c *scriptChecker) scriptCallableInstanceOrigins(
	call *CallExpr,
	target staticCallable,
) ([]Expression, bool) {
	owned, ok := c.resolveOwnedScriptCallable(target)
	if !ok {
		return nil, false
	}
	if (target.resolution == calleeMemberMethod || target.resolution == calleeForwardedMethod) &&
		isUniversalMember(owned.Name) {
		return nil, false
	}
	target.fn = owned
	runnable, hashSupplied, definite := callRunnableDefaults(call, target)
	paramFacts := c.summaryCallParamFacts(call, target, definite)
	paramFacts = c.reachableCallInstanceFacts(call, target, paramFacts)
	analysis := c.functionReturnAnalysis(
		owned,
		runnable,
		hashSupplied,
		definite,
		paramFacts,
		callMaySupplyBlock(call),
	)
	return append([]Expression(nil), analysis.instanceOrigins...), analysis.instanceOriginsExact
}

// scriptFunctionCallMayComplete reports whether an owned script function has
// any normal result for this call shape. Foreign functions and recursive
// in-progress analyzes remain gradual.
func (c *scriptChecker) scriptFunctionCallMayComplete(
	call *CallExpr,
	target staticCallable,
) bool {
	fn := target.fn
	if fn == nil || fn.owner != c.script {
		return true
	}
	runnable, hashSupplied, definite := callRunnableDefaults(call, target)
	paramFacts := c.summaryCallParamFacts(call, target, definite)
	analysis := c.functionReturnAnalysis(
		fn,
		runnable,
		hashSupplied,
		definite,
		paramFacts,
		callMaySupplyBlock(call),
	)
	if target.constructor {
		return analysis.bodyMayComplete
	}
	return analysis.mayComplete
}

func (c *scriptChecker) summaryCallParamFacts(
	call *CallExpr,
	target staticCallable,
	definite bool,
) map[string]checkReachableParamFact {
	if !definite || call == nil {
		return nil
	}
	return c.reachableCallParamFacts(call, target)
}

func callMaySupplyBlock(call *CallExpr) bool {
	return call != nil && call.Block != nil
}

func (c *scriptChecker) scriptCallFailureArgumentFacts(expr Expression) map[string]checkReachableParamFact {
	call, ok := expr.(*CallExpr)
	if !ok || call == nil || callExpandsArguments(call) {
		return nil
	}
	target, resolved := c.resolveCallable(call)
	if !resolved || target.fn == nil || target.fn.owner != c.script ||
		!c.scriptCallBindingPlan(call, target).bodyMayEnter ||
		staticCallCollapsesOptionsHash(call, target) {
		return nil
	}
	runnable, hashSupplied, definite := callRunnableDefaults(call, target)
	paramFacts := c.summaryCallParamFacts(call, target, definite)
	analysis := c.functionReturnAnalysis(
		target.fn,
		runnable,
		hashSupplied,
		definite,
		paramFacts,
		callMaySupplyBlock(call),
	)
	if len(analysis.failureParamFacts) == 0 {
		return nil
	}
	view := staticCallViewFor(call, target)
	result := make(map[string]checkReachableParamFact)
	ambiguous := make(map[string]struct{})
	bind := func(param Param, arg Expression) {
		if param.Name == "" || param.Kind == ParamRest || param.Kind == ParamKeywordRest {
			return
		}
		ident, ok := arg.(*Identifier)
		if !ok || !c.typeExprHasContainerArm(c.localTypeFor(ident.Name)) {
			return
		}
		if _, skip := ambiguous[ident.Name]; skip {
			return
		}
		fact, ok := analysis.failureParamFacts[param.Name]
		if !ok {
			return
		}
		if _, duplicate := result[ident.Name]; duplicate {
			delete(result, ident.Name)
			ambiguous[ident.Name] = struct{}{}
			return
		}
		result[ident.Name] = fact
	}
	for i, arg := range view.args {
		if param, ok := positionalCallableParam(target.fn.Params, i); ok {
			bind(param, arg)
		}
	}
	for _, kwarg := range view.kwargs {
		if kwarg.Splat {
			continue
		}
		for _, param := range target.fn.Params {
			if param.Name == kwarg.Name && (param.Kind == ParamNormal || param.Kind == ParamKeyword) {
				bind(param, kwarg.Value)
				break
			}
		}
	}
	if len(result) == 0 {
		return nil
	}
	return result
}

// callRunnableDefaults reports the defaulted parameter indices this call
// shape may leave unsupplied, so the summary walk evaluates exactly the
// defaults the runtime would, plus the indices a collapsed keyword options
// hash supplies instead (those parameters always bind a hash for this
// shape). definite reports whether those conclusions provably hold: a
// literal shape omits or supplies its parameters outright, while a splatted
// shape only may, so no value facts can bind. A nil call (a bare
// auto-invoke) passes no arguments, so every default runs.
func callRunnableDefaults(call *CallExpr, target staticCallable) ([]int, []int, bool) {
	fn := target.fn
	if fn == nil {
		return nil, nil, false
	}
	var indices []int
	var hashSupplied []int
	collapseOptionsHash := call != nil && staticCallCollapsesOptionsHash(call, target)
	for i, param := range fn.Params {
		if call == nil {
			if param.DefaultVal != nil {
				indices = append(indices, i)
			}
			continue
		}
		optionsHash, mayDefault := callParamSupply(call, fn, i, collapseOptionsHash)
		if optionsHash {
			hashSupplied = append(hashSupplied, i)
		} else if mayDefault && param.DefaultVal != nil {
			indices = append(indices, i)
		}
	}
	return indices, hashSupplied, call == nil || !callExpandsArguments(call)
}

// resolveOwnedScriptCallable maps a resolved target to the exact checked-script
// declaration it dispatches to. The resolved target name distinguishes plain
// functions (`transform` and `transform.call`), instance methods (`Box#take`),
// and class methods (`Box.build`), so a function stored under a dynamic member
// cannot borrow a method summary. Plain-function env clones normalize through
// the declaration position they share with the original; exact method targets
// are the registered declarations callable resolution selected.
func (c *scriptChecker) resolveOwnedScriptCallable(target staticCallable) (*ScriptFunction, bool) {
	fn := target.fn
	if fn == nil || fn.owner != c.script {
		return nil, false
	}
	if target.name == fn.Name || target.name == fn.Name+".call" {
		if owned, ok := c.script.functions[fn.Name]; ok && (owned == fn || owned.Pos == fn.Pos) {
			return owned, true
		}
	}
	c.prepareSelfScopeFunctions()
	classDef, ok := c.selfScopeFnClasses[fn]
	if !ok {
		return nil, false
	}
	separator := "#"
	if _, classMethod := c.selfScopeClassFns[fn]; classMethod {
		separator = "."
	}
	if target.name != classDef.Name+separator+fn.Name {
		return nil, false
	}
	return fn, true
}

func (c *scriptChecker) functionReturnAnalysis(
	fn *ScriptFunction,
	runnableDefaults, hashSuppliedParams []int,
	definiteDefaults bool,
	paramFacts map[string]checkReachableParamFact,
	blockAvailable bool,
) functionReturnAnalysis {
	if fn == nil {
		return functionReturnAnalysis{mayComplete: true, bodyMayComplete: true}
	}
	memberFreeKey := c.returnSummaryCallShapeKey(
		fn,
		runnableDefaults,
		hashSuppliedParams,
		definiteDefaults,
		paramFacts,
		blockAvailable,
	)
	if analysis, ok := c.memberFreeReturnAnalyzes[memberFreeKey]; ok {
		return analysis
	}
	key := memberFreeKey
	key.context = c.runtimeNamespaceMembersKey() + "\x01" + key.context
	if analysis, ok := c.returnAnalyzes[key]; ok {
		// This summary was computed by a walk that read the recorded members,
		// so the caller reusing it inherits that dependence.
		c.namespaceMemberReads++
		return analysis
	}
	if c.summaryInProgress == nil {
		c.summaryInProgress = make(map[returnSummaryCacheKey]struct{})
	}
	if _, busy := c.summaryInProgress[key]; busy {
		return functionReturnAnalysis{mayComplete: true, bodyMayComplete: true}
	}
	c.summaryInProgress[key] = struct{}{}
	defer delete(c.summaryInProgress, key)

	memberReads := c.namespaceMemberReads
	collector := c.collectFunctionReturnFacts(
		fn,
		runnableDefaults,
		hashSuppliedParams,
		definiteDefaults,
		paramFacts,
		blockAvailable,
	)
	analysis := functionReturnAnalysis{
		mayComplete:       collector.unknown || len(collector.arms) > 0,
		bodyMayComplete:   collector.unknown || len(collector.arms) > 0,
		failureParamFacts: cloneReachableParamFacts(collector.failureParamFacts),
	}
	if !collector.instanceOriginsUnknown && len(collector.instanceOrigins) > 0 {
		analysis.instanceOrigins = append([]Expression(nil), collector.instanceOrigins...)
		analysis.instanceOriginsExact = true
	}
	if !collector.unknown && len(collector.arms) > 0 {
		if fn.ReturnTy == nil {
			analysis.result = unionTypeExprs(collector.arms...)
		} else if validateTypeExprResolved(fn.ReturnTy, c.runtimeTypeContext()) == nil {
			analysis.mayComplete = false
			for _, arm := range collector.arms {
				if !typeExprsDisjoint(arm, fn.ReturnTy, c.checkNamedTypeResolver()) {
					analysis.mayComplete = true
					break
				}
			}
		}
	}
	c.retainReturnAnalysis(key, memberFreeKey, memberReads, analysis)
	return analysis
}

// maxRetainedSummaryContextBytes caps the cache-key text one check keeps for
// summaries whose walk read the recorded namespace members. Those keys name
// every recorded member, and a script that assigns a new member before each
// call retained one such key per call, so the cache alone grew with the square
// of the source. Past the cap the memo starts over, which costs a recomputation
// and nothing else: the summaries it produces are the same either way (#18).
const maxRetainedSummaryContextBytes = 8 << 20

// retainReturnAnalysis memos a computed summary. A walk that never read the
// recorded members cannot have depended on them, so its summary answers under
// every member set and is kept under the member-free key, which is what keeps
// a script assigning members between calls from recomputing (and re-keying) the
// same summary once per assignment.
func (c *scriptChecker) retainReturnAnalysis(
	key, memberFreeKey returnSummaryCacheKey,
	memberReadsBefore uint64,
	analysis functionReturnAnalysis,
) {
	if c.namespaceMemberReads == memberReadsBefore {
		if c.memberFreeReturnAnalyzes == nil {
			c.memberFreeReturnAnalyzes = make(map[returnSummaryCacheKey]functionReturnAnalysis)
		}
		c.memberFreeReturnAnalyzes[memberFreeKey] = analysis
		return
	}
	if c.returnAnalyzes == nil {
		c.returnAnalyzes = make(map[returnSummaryCacheKey]functionReturnAnalysis)
	}
	if c.returnAnalysisContextBytes+len(key.context) > maxRetainedSummaryContextBytes {
		clear(c.returnAnalyzes)
		c.returnAnalysisContextBytes = 0
	}
	c.returnAnalysisContextBytes += len(key.context)
	c.returnAnalyzes[key] = analysis
}

// returnSummaryCallShapeKey separates summaries computed under different module
// bindings and call shapes. A required module can change a callee from
// statically known to dynamic, so reusing a fact across those states would make
// the checker unsound; different call shapes run different parameter defaults,
// so their effects separate the key too. The recorded namespace members separate
// it as well, but only for a walk that read them, so they are added by the
// caller rather than spelled here.
func (c *scriptChecker) returnSummaryCallShapeKey(
	fn *ScriptFunction,
	runnableDefaults, hashSuppliedParams []int,
	definiteDefaults bool,
	paramFacts map[string]checkReachableParamFact,
	blockAvailable bool,
) returnSummaryCacheKey {
	context := c.runtimeBindingContextKey()
	if len(runnableDefaults) > 0 {
		context += "\x01defaults:" + fmt.Sprint(runnableDefaults)
		if definiteDefaults {
			context += "!"
		}
	}
	if len(hashSuppliedParams) > 0 {
		context += "\x01options:" + fmt.Sprint(hashSuppliedParams)
		if definiteDefaults {
			context += "!"
		}
	}
	if factsKey := reachableParamFactsKey(paramFacts); factsKey != "" {
		context += "\x01params:" + factsKey
	}
	if blockAvailable {
		context += "\x01block"
	}
	return returnSummaryCacheKey{fn: fn, context: context}
}

// collectFunctionReturnFacts walks the function body with warnings
// suppressed and every piece of walk state walled off, recording return-site
// facts as the walk reaches them and implicit-final facts afterwards. Only
// the defaults the summarized call shape may evaluate are walked.
func (c *scriptChecker) collectFunctionReturnFacts(
	fn *ScriptFunction,
	runnableDefaults, hashSuppliedParams []int,
	definiteDefaults bool,
	paramFacts map[string]checkReachableParamFact,
	blockAvailable bool,
) *returnSummaryCollector {
	collector := &returnSummaryCollector{}
	c.withSuppressedWarnings(func() {
		runtimeState := c.snapshotRuntimeState()
		defer c.restoreRuntimeState(runtimeState)
		// Arm the discarded-mutator check the same way the other two body
		// walks do: the callee's implicit-return leaves are result
		// positions here too, and its verdicts must not consult the
		// interrupted caller's block contexts. Warnings are suppressed on
		// this walk, but the arming keeps every body-walk entry point
		// agreeing on what a discard position is.
		defer c.enterMutatorDiscardFunctionBody(fn)()

		previousScopes := c.scopes
		previousLocalTypes := c.localTypes
		previousLocalClassValues := c.localClassValues
		previousLive := c.liveLocalNames
		previousUnions := c.localNameUnions
		previousDepth := c.mutationRegionDepth
		previousArgFacts := c.callArgumentFacts
		previousArgClassValues := c.callArgumentClassValues
		previousArgCallables := c.callArgumentCallables
		previousArgStaticValues := c.callArgumentStaticValues
		previousArgStaticChoices := c.callArgumentStaticChoices
		previousReceiverLength := c.callArrayReceiverLength
		previousReachableParamFacts := c.reachableParamFacts
		previousDeferred := c.deferredReturnSites
		previousConstructorReturnExits := c.constructorReturnExitSites
		previousExceptionExits := c.exceptionExitSites
		previousExpressionExits := c.expressionExitSites
		previousNonLocalReturnExits := c.nonLocalReturnExitSites
		previousEnsureExits := c.ensureExitSites
		previousExpressionReturnsNonLocally := c.expressionReturnsNonLocally
		previousClassConstantCaptures := c.classConstantCaptures
		previousLoopExitEffects := c.loopExitEffects
		previousLeaves := c.implicitReturnLeaves
		previousStates := c.implicitReturnStates
		previousDecisions := c.implicitIfDecisions
		previousCollector := c.returnCollector
		previousBlockResultCollector := c.blockResultCollector
		previousBlockLocalReturnCollector := c.blockLocalReturnCollector
		previousBlockLocalBreakCollector := c.blockLocalBreakCollector
		previousYieldCollector := c.summaryYieldCollector
		previousYieldReentryWalks := c.summaryYieldReentryWalks
		previousYieldsActive := c.summaryYieldsActive
		previousBlockAvailable := c.summaryBlockAvailable
		previousPinned := c.pinnedExpressionFacts
		previousReachableChecks := c.checkReachableCalls
		previousLocalCallBypassScopes := c.localCallBypassScopes
		restoreFresh := c.withFreshLocalInferenceScope()
		c.scopes = nil
		c.localTypes = nil
		c.localClassValues = nil
		c.liveLocalNames = nil
		c.localNameUnions = nil
		c.mutationRegionDepth = 0
		c.callArgumentFacts = nil
		c.callArgumentClassValues = nil
		c.callArgumentCallables = nil
		c.callArgumentStaticValues = nil
		c.callArgumentStaticChoices = nil
		c.callArrayReceiverLength = checkArrayReceiverLength{}
		c.reachableParamFacts = cloneReachableParamFacts(paramFacts)
		c.deferredReturnSites = nil
		c.constructorReturnExitSites = nil
		var exceptionExitSites []checkStateSnapshot
		c.exceptionExitSites = &exceptionExitSites
		var expressionExitSites []checkStateSnapshot
		c.expressionExitSites = &expressionExitSites
		c.nonLocalReturnExitSites = nil
		c.ensureExitSites = nil
		c.expressionReturnsNonLocally = false
		c.classConstantCaptures = nil
		c.loopExitEffects = nil
		c.returnCollector = collector
		c.blockResultCollector = nil
		c.blockLocalReturnCollector = nil
		c.blockLocalBreakCollector = nil
		c.summaryYieldCollector = collector
		c.summaryYieldReentryWalks = 0
		c.summaryYieldsActive = true
		c.summaryBlockAvailable = blockAvailable
		c.pinnedExpressionFacts = nil
		c.localCallBypassScopes = nil
		// Summary inference may inspect calls on paths the real checker has
		// already proved unreachable. Those synthetic walks must not enqueue
		// callees or mark them checked under the speculative runtime state.
		c.checkReachableCalls = false
		leaves := make(map[Statement]struct{})
		collectImplicitReturnLeaves(fn.Body, leaves)
		c.implicitReturnLeaves = leaves
		c.implicitReturnStates = make(map[Statement]checkStateSnapshot, len(leaves))
		c.implicitIfDecisions = make(map[*IfStmt][]conditionDecision)
		defer func() {
			c.scopes = previousScopes
			c.localTypes = previousLocalTypes
			c.localClassValues = previousLocalClassValues
			c.liveLocalNames = previousLive
			c.localNameUnions = previousUnions
			c.mutationRegionDepth = previousDepth
			c.callArgumentFacts = previousArgFacts
			c.callArgumentClassValues = previousArgClassValues
			c.callArgumentCallables = previousArgCallables
			c.callArgumentStaticValues = previousArgStaticValues
			c.callArgumentStaticChoices = previousArgStaticChoices
			c.callArrayReceiverLength = previousReceiverLength
			c.reachableParamFacts = previousReachableParamFacts
			c.deferredReturnSites = previousDeferred
			c.constructorReturnExitSites = previousConstructorReturnExits
			c.exceptionExitSites = previousExceptionExits
			c.expressionExitSites = previousExpressionExits
			c.nonLocalReturnExitSites = previousNonLocalReturnExits
			c.ensureExitSites = previousEnsureExits
			c.expressionReturnsNonLocally = previousExpressionReturnsNonLocally
			c.classConstantCaptures = previousClassConstantCaptures
			c.loopExitEffects = previousLoopExitEffects
			c.implicitReturnLeaves = previousLeaves
			c.implicitReturnStates = previousStates
			c.implicitIfDecisions = previousDecisions
			c.returnCollector = previousCollector
			c.blockResultCollector = previousBlockResultCollector
			c.blockLocalReturnCollector = previousBlockLocalReturnCollector
			c.blockLocalBreakCollector = previousBlockLocalBreakCollector
			c.summaryYieldCollector = previousYieldCollector
			c.summaryYieldReentryWalks = previousYieldReentryWalks
			c.summaryYieldsActive = previousYieldsActive
			c.summaryBlockAvailable = previousBlockAvailable
			c.pinnedExpressionFacts = previousPinned
			c.checkReachableCalls = previousReachableChecks
			c.localCallBypassScopes = previousLocalCallBypassScopes
			restoreFresh()
		}()

		popScope := c.pushScope(make(map[string]struct{}))
		defer popScope()
		popNameScope := c.pushFunctionNameScope(fn)
		defer popNameScope()
		c.seedInstanceIvarFacts(fn)
		runnable := make(map[int]struct{}, len(runnableDefaults))
		for _, index := range runnableDefaults {
			runnable[index] = struct{}{}
		}
		hashSupplied := make(map[int]struct{}, len(hashSuppliedParams))
		for _, index := range hashSuppliedParams {
			hashSupplied[index] = struct{}{}
		}
		if definiteDefaults {
			c.linkReachableParamAliases(fn.Params)
		}
		for i, param := range fn.Params {
			expectation := bindingDefaultExpectation(param)
			// An omitted argument runs the default expression before the
			// body, so its effects (a require's exports, a namespace write)
			// must be live for the body walk, mirroring checkFunction. A
			// default the summarized call shape provably supplies never
			// runs and contributes nothing.
			_, mayRun := runnable[i]
			if mayRun {
				if !definiteDefaults {
					c.oneShotIvarRefinementDepth++
				}
				completed := c.checkExpressionWithExpectation(fn.Name, param.DefaultVal, expectation)
				if !definiteDefaults {
					c.oneShotIvarRefinementDepth--
				}
				c.collectRuntimeRequireCallExportsFromExpression(param.DefaultVal)
				if definiteDefaults && !completed {
					return
				}
			}
			c.checkIvarParamBinding(fn.Name, fn, param)
			c.recordParamBinding(param)
			if !definiteDefaults {
				continue
			}
			c.applyReachableParamFact(param)
			// A literal call shape omits or supplies the parameter outright:
			// an omitted default is exactly the value the runtime binds, and
			// a collapsed keyword options hash always binds a hash. A
			// splatted shape may do either, so no value fact holds.
			if mayRun {
				c.bindParamDefaultFact(param)
				c.refineAnnotatedParamFact(param, c.inferExpressionTypeWithExpectation(param.DefaultVal, expectation))
			} else if _, viaHash := hashSupplied[i]; viaHash {
				if param.Name != "" && param.Type == nil {
					c.bindLocalTypeInCurrentFrame(param.Name, checkTypeHash)
				} else {
					c.refineAnnotatedParamFact(param, checkTypeHash)
				}
			}
		}
		if c.checkStatements(fn.Name, nil, fn.Body) {
			c.collectImplicitResultFacts(fn.Body)
		}
		rebound := make(map[string]struct{})
		collectLocalBindings(fn.Body, rebound)
		failureExitSites := make(
			[]checkStateSnapshot,
			0,
			len(expressionExitSites)+len(exceptionExitSites),
		)
		failureExitSites = append(failureExitSites, expressionExitSites...)
		failureExitSites = append(failureExitSites, exceptionExitSites...)
		collector.failureParamFacts = c.mergedFailureParamFacts(fn.Params, failureExitSites, rebound)
	})
	return collector
}

func (c *scriptChecker) mergedFailureParamFacts(
	params []Param,
	sites []checkStateSnapshot,
	rebound map[string]struct{},
) map[string]checkReachableParamFact {
	if len(sites) == 0 {
		return nil
	}
	result := make(map[string]checkReachableParamFact)
	for _, param := range params {
		if param.Name == "" {
			continue
		}
		if _, assigned := rebound[param.Name]; assigned {
			continue
		}
		facts := make([]checkReachableParamFact, 0, len(sites))
		complete := true
		for _, site := range sites {
			if _, poisoned := site.typePoison[param.Name]; poisoned {
				complete = false
				break
			}
			fact, ok := scopeStateParamFact(site.scopeState, param.Name)
			if !ok {
				complete = false
				break
			}
			if _, poisoned := site.staticValuePoison[param.Name]; poisoned {
				fact.staticVals = nil
			}
			facts = append(facts, fact)
		}
		if !complete {
			continue
		}
		fact, ok := c.mergeFailureParamFactAlternatives(facts)
		if ok {
			result[param.Name] = fact
		}
	}
	if len(result) == 0 {
		return nil
	}
	return result
}

func scopeStateParamFact(state checkScopeState, name string) (checkReachableParamFact, bool) {
	for i := len(state.types) - 1; i >= 0; i-- {
		ty, tracked := state.types[i][name]
		if !tracked {
			continue
		}
		fact := checkReachableParamFact{typeExpr: ty}
		if i < len(state.classValues) {
			value := state.classValues[i][name]
			fact.classNames = append([]string(nil), value.classNames...)
			fact.callables = append([]*ScriptFunction(nil), value.callables...)
			fact.staticVals = append([]Expression(nil), value.staticVals...)
			fact.instanceOrigins = append([]Expression(nil), value.instanceOrigins...)
		}
		return fact, true
	}
	return checkReachableParamFact{}, false
}

func (c *scriptChecker) mergeFailureParamFactAlternatives(
	facts []checkReachableParamFact,
) (checkReachableParamFact, bool) {
	if len(facts) == 0 {
		return checkReachableParamFact{}, false
	}
	merged := checkReachableParamFact{typeExpr: facts[0].typeExpr}
	knownType := merged.typeExpr != nil
	staticExact := len(facts[0].staticVals) > 0
	classExact := len(facts[0].classNames) > 0
	callableExact := len(facts[0].callables) > 0
	instanceExact := len(facts[0].instanceOrigins) > 0
	merged.staticVals = append([]Expression(nil), facts[0].staticVals...)
	merged.classNames = append([]string(nil), facts[0].classNames...)
	merged.callables = append([]*ScriptFunction(nil), facts[0].callables...)
	merged.instanceOrigins = append([]Expression(nil), facts[0].instanceOrigins...)
	for _, fact := range facts[1:] {
		if knownType {
			if fact.typeExpr == nil {
				knownType = false
				merged.typeExpr = nil
			} else {
				merged.typeExpr = unionTypeExprs(merged.typeExpr, fact.typeExpr)
				knownType = merged.typeExpr != nil
			}
		}
		if staticExact {
			if len(fact.staticVals) == 0 {
				staticExact = false
				merged.staticVals = nil
			} else {
				merged.staticVals = c.normalizeCheckStaticValues(append(merged.staticVals, fact.staticVals...))
			}
		}
		if classExact {
			if len(fact.classNames) == 0 {
				classExact = false
				merged.classNames = nil
			} else {
				merged.classNames = normalizeCheckClassNames(append(merged.classNames, fact.classNames...))
			}
		}
		if callableExact {
			if len(fact.callables) == 0 {
				callableExact = false
				merged.callables = nil
			} else {
				merged.callables = normalizeCheckCallables(append(merged.callables, fact.callables...))
			}
		}
		if instanceExact {
			if len(fact.instanceOrigins) == 0 {
				instanceExact = false
				merged.instanceOrigins = nil
			} else {
				merged.instanceOrigins = normalizeCheckExpressionIdentities(
					append(merged.instanceOrigins, fact.instanceOrigins...),
				)
			}
		}
	}
	return merged, knownType || staticExact || classExact || callableExact || instanceExact
}

// recordDeferredReturnSummaryFacts records returns after a non-exiting
// ensure has applied its mutation effects. The return expression itself is
// inferred under the state at which it was evaluated, while monotone local
// poisoning from the ensure remains live so a mutated returned container
// cannot retain stale structural facts.
func (c *scriptChecker) recordDeferredReturnSummaryFacts(sites []deferredReturnSite) {
	runtimeState := c.snapshotRuntimeState()
	scopeState := c.snapshotScopeState()
	defer func() {
		c.restoreRuntimeState(runtimeState)
		c.restoreScopeState(scopeState)
	}()

	for _, site := range sites {
		if site.stmt == nil {
			continue
		}
		c.restoreRuntimeState(site.runtimeState)
		c.restoreScopeState(site.scopeState)
		if site.stmt.Value == nil {
			c.recordReturnSummaryResult(c.returnCollector, nil, checkTypeNil)
			continue
		}
		c.recordReturnSummaryResult(
			c.returnCollector,
			site.stmt.Value,
			c.inferExpressionType(site.stmt.Value),
		)
	}
}

// collectImplicitResultFacts mirrors checkImplicitFinalBlock, recording the
// implicit result facts of a fallen-through block instead of warning.
func (c *scriptChecker) collectImplicitResultFacts(statements []Statement) {
	if len(statements) == 0 {
		c.recordReturnSummaryResult(c.returnCollector, nil, checkTypeNil)
		return
	}
	c.collectImplicitResultStatementFacts(effectiveFinalStatement(statements))
}

func (c *scriptChecker) collectImplicitResultStatementFacts(stmt Statement) {
	switch typed := stmt.(type) {
	case *ReturnStmt, *RaiseStmt:
		// Explicit returns were recorded during the walk; a raising final
		// statement yields no value.
		return
	case *ExprStmt:
		c.recordImplicitLeafFact(typed, typed.Expr)
	case *AssignStmt:
		if typed.Operator == tokenNone {
			c.recordImplicitLeafFact(typed, typed.Value)
		} else {
			// A compound or logical assignment yields its combined result
			// (x ||= y is x-or-y, x += y is x plus y), which is the
			// target's post-assignment fact, not the bare right-hand side.
			c.recordImplicitLeafFact(typed, typed.Target)
		}
	case *IfStmt:
		c.collectImplicitResultIfFacts(typed)
	case *ForStmt, *WhileStmt, *UntilStmt:
		// A loop's value depends on break semantics the summary does not
		// model, so a loop-final body stays unknown.
		c.recordReturnSummaryResult(c.returnCollector, nil, nil)
	case *TryStmt:
		if blockAlwaysExits(typed.Ensure) {
			return
		}
		if len(typed.Else) > 0 && !blockAlwaysExits(typed.Body) {
			c.collectImplicitResultFacts(typed.Else)
		} else {
			c.collectImplicitResultFacts(typed.Body)
		}
		if statementsProvenNonRaising(typed.Body) {
			return
		}
		selected, exact := c.staticallySelectedRescue(typed.Body, typed.Rescues)
		for i := range typed.Rescues {
			if exact && i != selected {
				continue
			}
			clause := &typed.Rescues[i]
			// An empty matched clause propagates the error after ensure
			// instead of yielding a value (the fallthrough merge skips these
			// clauses the same way), so it contributes no arm.
			if len(clause.Body) == 0 {
				continue
			}
			c.collectImplicitResultFacts(clause.Body)
		}
	default:
		// A construct the implicit-return model does not cover keeps the
		// summary unknown rather than guessing.
		c.recordReturnSummaryResult(c.returnCollector, nil, nil)
	}
}

// collectImplicitResultIfFacts mirrors checkImplicitFinalIfStatement: every
// reachable branch contributes its final fact, and a missing else adds the
// nil fallthrough arm.
func (c *scriptChecker) collectImplicitResultIfFacts(stmt *IfStmt) {
	if stmt == nil {
		c.recordReturnSummaryResult(c.returnCollector, nil, checkTypeNil)
		return
	}
	truthy, known := c.implicitConditionDecision(stmt, 0, stmt.Condition)
	if !known || truthy {
		c.collectImplicitResultFacts(stmt.Consequent)
		if known {
			return
		}
	}
	for i, elseIf := range stmt.ElseIf {
		truthy, known = c.implicitConditionDecision(stmt, i+1, elseIf.Condition)
		if known && !truthy {
			continue
		}
		c.collectImplicitResultFacts(elseIf.Consequent)
		if known {
			return
		}
	}
	if len(stmt.Alternate) == 0 {
		c.recordReturnSummaryResult(c.returnCollector, nil, checkTypeNil)
		return
	}
	c.collectImplicitResultFacts(stmt.Alternate)
}

// recordImplicitLeafFact records a leaf expression's fact under the state
// captured at the leaf's own walk, adding the nil arm when the expression
// can implicitly yield nil.
func (c *scriptChecker) recordImplicitLeafFact(stmt Statement, expr Expression) {
	state, ok := c.implicitReturnStates[stmt]
	if !ok && c.implicitReturnStates != nil {
		// The real walk did not reach a normal result for this syntactic
		// leaf, so it cannot contribute to the function's return summary.
		return
	}
	if expressionCanImplicitlyYieldNil(expr) {
		c.recordReturnSummaryResult(c.returnCollector, nil, checkTypeNil)
	}
	if !ok {
		c.recordReturnSummaryResult(c.returnCollector, expr, c.inferExpressionType(expr))
		return
	}
	currentRuntime := c.snapshotRuntimeState()
	currentScope := c.snapshotScopeState()
	c.restoreRuntimeState(state.runtimeState)
	c.restoreScopeState(state.scopeState)
	c.recordReturnSummaryResult(c.returnCollector, expr, c.inferExpressionType(expr))
	c.restoreRuntimeState(currentRuntime)
	c.restoreScopeState(currentScope)
}
