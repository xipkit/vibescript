package runtime

import (
	"context"
	cryptorand "crypto/rand"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"sync/atomic"
	"time"
)

const defaultMaxSourceBytes = 1 << 20

// The zero-value Config quota default is the low profile: an embedder that sets
// no step, memory, or recursion quota gets the lowest named sandbox budget — a
// conservative, coherent floor rather than a hand-tuned trio. Deriving these
// from ProfileLow keeps the default and the profile in lockstep.
var (
	defaultStepQuota        = ProfileLow.StepQuota
	defaultMemoryQuotaBytes = ProfileLow.MemoryQuotaBytes
	defaultRecursionLimit   = ProfileLow.RecursionLimit
)

// Unlimited disables a quota when supplied as a Config quota value
// (StepQuota, MemoryQuotaBytes, or RecursionLimit). The runtime treats any
// non-positive quota as unbounded; Unlimited is the explicit spelling callers
// use to request that, distinct from a zero value, which selects the built-in
// default. Disabling RecursionLimit lets deep recursion grow the host Go stack
// until it overflows the process — no named quota profile does this; it is an
// at-your-own-risk escape hatch for trusted workloads.
const Unlimited = -1

// resolveQuota maps a Config quota field to its effective runtime value: zero
// selects def, a negative value (Unlimited) disables the quota by resolving to
// zero — which every enforcement site reads as unbounded — and a positive value
// is used as-is.
func resolveQuota(value, def int) int {
	switch {
	case value == 0:
		return def
	case value < 0:
		return 0
	default:
		return value
	}
}

// Config controls interpreter execution bounds and enforcement modes.
type Config struct {
	StepQuota        int
	MemoryQuotaBytes int
	StrictEffects    bool
	RecursionLimit   int
	ModulePaths      []string
	ModuleAllowList  []string
	ModuleDenyList   []string
	RandomReader     io.Reader
	RandomReadFunc   func(context.Context, []byte) (int, error)
	OutputWriter     io.Writer
	ErrorWriter      io.Writer
	MaxCachedModules int
	MaxSourceBytes   int

	// DevMode enables development-time module reloading. When true, every
	// require revalidates its cached module against the source file's
	// mtime+size and recompiles it when the file changed, and require
	// misses are re-resolved from disk instead of being negatively cached.
	// The zero value (false) keeps production behavior: modules compile
	// once and are served from cache until ClearModuleCache. DevMode is
	// not intended for production: each require costs a stat, and a
	// reload is not atomic across concurrently running Calls (each
	// in-flight Call keeps the module version it first required).
	DevMode bool
}

// Engine executes Vibescript programs with deterministic limits.
type Engine struct {
	config            Config
	builtins          map[string]Value
	hostBuiltins      map[string]struct{}
	builtinsMu        sync.RWMutex
	modules           map[string]moduleEntry
	modPaths          []string
	modMu             sync.RWMutex
	randomMu          sync.Mutex
	modRequests       map[string]moduleRequest
	modRequestBytes   int
	modSearchHits     map[string]moduleEntry
	modSearchMisses   map[string]string
	modSuggest        map[string][]string
	modSuggestText    map[string]string
	modSuggestVersion uint64

	// spareBaseWalkCache holds one released estimator memo struct (journal
	// backing dropped) for reuse by the engine's next call; see
	// Execution.releaseBaseWalkCache.
	spareBaseWalkCache atomic.Pointer[baseWalkCache]

	// builtinProto is the frozen env shared as every call root's parent.
	// Mutable namespace builtins are cloned lazily by Env.Get before a
	// script can mutate them, so calls that do not touch those namespaces
	// skip their map-clone cost entirely. Rebuilt lazily after RegisterBuiltin.
	builtinProto *Env
}

// NewEngine constructs an Engine with sane defaults and registers built-ins.
func NewEngine(cfg Config) (*Engine, error) {
	cfg.StepQuota = resolveQuota(cfg.StepQuota, defaultStepQuota)
	cfg.MemoryQuotaBytes = resolveQuota(cfg.MemoryQuotaBytes, defaultMemoryQuotaBytes)
	cfg.RecursionLimit = resolveQuota(cfg.RecursionLimit, defaultRecursionLimit)
	if cfg.MaxCachedModules == 0 {
		cfg.MaxCachedModules = 1000
	}
	if cfg.MaxSourceBytes < 0 {
		return nil, fmt.Errorf("vibes: max source bytes cannot be negative")
	}
	if cfg.MaxSourceBytes == 0 {
		cfg.MaxSourceBytes = defaultMaxSourceBytes
	}
	if cfg.RandomReader == nil {
		cfg.RandomReader = cryptorand.Reader
	}

	modulePaths, err := normalizeModulePaths(cfg.ModulePaths)
	if err != nil {
		return nil, err
	}
	if err := validateModulePolicyPatterns(cfg.ModuleAllowList, "allow"); err != nil {
		return nil, err
	}
	if err := validateModulePolicyPatterns(cfg.ModuleDenyList, "deny"); err != nil {
		return nil, err
	}

	cfg.ModulePaths = modulePaths
	cfg.ModuleAllowList = append([]string(nil), cfg.ModuleAllowList...)
	cfg.ModuleDenyList = append([]string(nil), cfg.ModuleDenyList...)

	engine := &Engine{
		config:          cfg,
		builtins:        make(map[string]Value),
		hostBuiltins:    make(map[string]struct{}),
		modules:         make(map[string]moduleEntry),
		modPaths:        append([]string(nil), cfg.ModulePaths...),
		modRequests:     make(map[string]moduleRequest),
		modSearchHits:   make(map[string]moduleEntry),
		modSearchMisses: make(map[string]string),
		modSuggest:      make(map[string][]string),
		modSuggestText:  make(map[string]string),
	}

	registerCoreBuiltins(engine)
	registerRemovedCallableFences(engine)
	registerDataBuiltins(engine)
	registerHashBuiltins(engine)
	registerMathBuiltins(engine)
	registerDurationBuiltins(engine)
	registerTimeBuiltins(engine)

	return engine, nil
}

func (e *Engine) randomBytes(ctx context.Context, n int) ([]byte, error) {
	if n < 0 {
		return nil, fmt.Errorf("random source failed: invalid byte request")
	}
	if ctx == nil {
		ctx = context.Background()
	}
	if err := ctx.Err(); err != nil {
		return nil, err
	}
	buf := make([]byte, n)
	e.randomMu.Lock()
	defer e.randomMu.Unlock()
	if err := ctx.Err(); err != nil {
		return nil, err
	}
	if e.config.RandomReadFunc != nil {
		if err := readFullContext(ctx, e.config.RandomReadFunc, buf); err != nil {
			return nil, err
		}
		return buf, nil
	}
	if _, err := io.ReadFull(e.config.RandomReader, buf); err != nil {
		return nil, fmt.Errorf("random source failed: %w", err)
	}
	if err := ctx.Err(); err != nil {
		return nil, err
	}
	return buf, nil
}

func readFullContext(ctx context.Context, read func(context.Context, []byte) (int, error), buf []byte) error {
	for len(buf) > 0 {
		if err := ctx.Err(); err != nil {
			return err
		}
		n, err := read(ctx, buf)
		if n < 0 || n > len(buf) {
			return fmt.Errorf("random source failed: invalid byte count")
		}
		if n > 0 {
			buf = buf[n:]
		}
		if len(buf) == 0 {
			if err := ctx.Err(); err != nil {
				return err
			}
			return nil
		}
		if err != nil {
			return fmt.Errorf("random source failed: %w", err)
		}
		if n == 0 {
			return fmt.Errorf("random source failed: no bytes read")
		}
	}
	return nil
}

func normalizeModulePaths(paths []string) ([]string, error) {
	normalized := make([]string, 0, len(paths))
	for _, path := range paths {
		if strings.TrimSpace(path) == "" {
			return nil, fmt.Errorf("vibes: module path cannot be empty")
		}
		absPath, err := filepath.Abs(path)
		if err != nil {
			return nil, fmt.Errorf("vibes: invalid module path %q: %w", path, err)
		}
		stat, err := os.Stat(absPath)
		if err != nil {
			return nil, fmt.Errorf("vibes: invalid module path %q: %w", path, err)
		}
		if !stat.IsDir() {
			return nil, fmt.Errorf("vibes: module path %q is not a directory", path)
		}
		resolvedPath, err := filepath.EvalSymlinks(absPath)
		if err != nil {
			return nil, fmt.Errorf("vibes: invalid module path %q: %w", path, err)
		}
		normalized = append(normalized, filepath.Clean(resolvedPath))
	}
	return normalized, nil
}

// MustNewEngine constructs an Engine or panics if the config is invalid.
func MustNewEngine(cfg Config) *Engine {
	engine, err := NewEngine(cfg)
	if err != nil {
		panic(err)
	}
	return engine
}

// RegisterBuiltin registers a callable global available to scripts.
func (e *Engine) RegisterBuiltin(name string, fn BuiltinFunc) {
	e.registerHostBuiltin(name, NewBuiltin(name, fn))
}

type builtinDefinition struct {
	name       string
	fn         BuiltinFunc
	autoInvoke bool
	checkSpec  *staticCallSpec
}

func (e *Engine) registerDefaultBuiltin(def builtinDefinition) {
	e.builtinsMu.Lock()
	defer e.builtinsMu.Unlock()

	val := newBuiltin(def.name, def.fn, def.autoInvoke)
	if def.checkSpec != nil {
		// The definition's autoInvoke flag drives runtime dispatch; mirror it
		// into the static contract so the checker never drifts from it.
		def.checkSpec.autoInvoke = def.autoInvoke
	}
	valueBuiltin(val).checkSpec = def.checkSpec
	e.builtins[def.name] = val
	delete(e.hostBuiltins, def.name)
	e.builtinProto = nil
}

func (e *Engine) registerHostBuiltin(name string, builtin Value) {
	e.builtinsMu.Lock()
	defer e.builtinsMu.Unlock()

	if b := valueBuiltin(builtin); b != nil {
		b.hostDriven = true
	}
	e.builtins[name] = builtin
	if e.hostBuiltins == nil {
		e.hostBuiltins = make(map[string]struct{})
	}
	e.hostBuiltins[name] = struct{}{}
	e.builtinProto = nil
}

// RegisterZeroArgBuiltin registers a builtin that can be invoked without arguments or parentheses.
func (e *Engine) RegisterZeroArgBuiltin(name string, fn BuiltinFunc) {
	e.registerHostBuiltin(name, NewAutoBuiltin(name, fn))
}

// RegisterBuiltinWithSignature registers a callable global that publishes an
// opt-in static contract: the checker validates known arguments and infers
// the declared result, and the same contract is enforced at runtime. When the
// name replaces a core builtin, the published signature supersedes the core
// contract; registering with RegisterBuiltin instead keeps the callable fully
// dynamic.
func (e *Engine) RegisterBuiltinWithSignature(name string, fn BuiltinFunc, sig Signature) error {
	val, err := NewTypedBuiltin(name, fn, sig)
	if err != nil {
		return err
	}
	e.registerHostBuiltin(name, val)
	return nil
}

func (e *Engine) hasHostBuiltin(name string) bool {
	if e == nil {
		return false
	}
	e.builtinsMu.RLock()
	defer e.builtinsMu.RUnlock()

	_, ok := e.hostBuiltins[name]
	return ok
}

func (e *Engine) builtinCallSpec(name string) (staticCallSpec, bool) {
	if e == nil {
		return staticCallSpec{}, false
	}
	e.builtinsMu.RLock()
	defer e.builtinsMu.RUnlock()

	root, member, qualified := strings.Cut(name, ".")
	val, ok := e.builtins[root]
	if !ok {
		return staticCallSpec{}, false
	}
	if qualified {
		if val.Kind() != KindObject {
			return staticCallSpec{}, false
		}
		val, ok = val.HashEntryMap()[member]
		if !ok {
			return staticCallSpec{}, false
		}
	}
	builtin := valueBuiltin(val)
	if builtin == nil || builtin.checkSpec == nil {
		return staticCallSpec{}, false
	}
	return *builtin.checkSpec, true
}

// builtinValueReadFails reports a registered builtin ("loop") or builtin
// namespace member ("Regexp.union") that is callable but neither auto-invokes
// on a bare read nor publishes a static contract. Calls to such a builtin
// stay unchecked, but a bare read of it always fails at runtime: a callable
// is not a value (ADR-006), so the checker can report the read itself.
func (e *Engine) builtinValueReadFails(name string) bool {
	if e == nil {
		return false
	}
	e.builtinsMu.RLock()
	defer e.builtinsMu.RUnlock()

	root, member, qualified := strings.Cut(name, ".")
	val, ok := e.builtins[root]
	if !ok {
		return false
	}
	if qualified {
		if val.Kind() != KindObject {
			return false
		}
		val, ok = val.HashEntryMap()[member]
		if !ok {
			return false
		}
	}
	builtin := valueBuiltin(val)
	return builtin != nil && !builtin.AutoInvoke && builtin.checkSpec == nil
}

// builtinAutoInvokes reports whether a registered builtin auto-invokes on a
// bare identifier read. Host AutoBuiltins carry the runtime flag without
// publishing a checkSpec, so callers that need the dispatch itself cannot
// go through builtinCallSpec.
func (e *Engine) builtinAutoInvokes(name string) bool {
	if e == nil {
		return false
	}
	e.builtinsMu.RLock()
	defer e.builtinsMu.RUnlock()

	val, ok := e.builtins[name]
	if !ok {
		return false
	}
	builtin := valueBuiltin(val)
	return builtin != nil && builtin.AutoInvoke
}

// Builtin contract type fragments shared by the registration tables below.
// Every type here mirrors a kind check in the builtin's implementation; a
// contract that overclaims turns valid scripts into checker false positives.
var (
	builtinTypeScalar         = &TypeExpr{Kind: TypeUnion, Union: []*TypeExpr{checkTypeInt, checkTypeFloat, checkTypeString}}
	builtinTypeRandBound      = &TypeExpr{Kind: TypeUnion, Union: []*TypeExpr{checkTypeInt, checkTypeRange, checkTypeNil}}
	builtinTypeNullableInt    = &TypeExpr{Kind: TypeInt, Nullable: true}
	builtinTypeNullableString = &TypeExpr{Kind: TypeString, Nullable: true}
)

func registerCoreBuiltins(engine *Engine) {
	for _, builtin := range []builtinDefinition{
		{name: "assert", fn: builtinAssert, checkSpec: &staticCallSpec{minArgs: 1, maxArgs: -1, resultType: checkTypeNil}},
		{name: "format", fn: builtinFormat, checkSpec: &staticCallSpec{minArgs: 1, maxArgs: -1, rejectKeywords: true, rejectBlock: true, paramTypes: []*TypeExpr{checkTypeString}, resultType: checkTypeString}},
		{name: "loop", fn: builtinLoop},
		{name: "money", fn: builtinMoney, checkSpec: &staticCallSpec{minArgs: 1, maxArgs: 1, paramTypes: []*TypeExpr{checkTypeString}, resultType: checkTypeMoney}},
		{name: "money_cents", fn: builtinMoneyCents, checkSpec: &staticCallSpec{minArgs: 2, maxArgs: 2, paramTypes: []*TypeExpr{checkTypeNumber, checkTypeString}, resultType: checkTypeMoney}},
		{name: "p", fn: builtinP},
		{name: "print", fn: builtinPrint, checkSpec: &staticCallSpec{minArgs: 0, maxArgs: -1, rejectKeywords: true, rejectBlock: true, resultType: checkTypeNil}},
		{name: "puts", fn: builtinPuts, checkSpec: &staticCallSpec{minArgs: 0, maxArgs: -1, rejectKeywords: true, rejectBlock: true, resultType: checkTypeNil}},
		{name: "require", fn: builtinRequire},
		{name: "now", fn: builtinNow, autoInvoke: true, checkSpec: &staticCallSpec{minArgs: 0, maxArgs: 0, resultType: checkTypeString}},
		{name: "rand", fn: builtinRand, autoInvoke: true, checkSpec: &staticCallSpec{minArgs: 0, maxArgs: 1, rejectKeywords: true, rejectBlock: true, paramTypes: []*TypeExpr{builtinTypeRandBound}, resultType: checkTypeNumber}},
		{name: "sprintf", fn: builtinSprintf, checkSpec: &staticCallSpec{minArgs: 1, maxArgs: -1, rejectKeywords: true, rejectBlock: true, paramTypes: []*TypeExpr{checkTypeString}, resultType: checkTypeString}},
		{name: "srand", fn: builtinSrand, checkSpec: &staticCallSpec{minArgs: 0, maxArgs: 1, rejectKeywords: true, rejectBlock: true, paramTypes: []*TypeExpr{builtinTypeNullableInt}, resultType: builtinTypeNullableInt}},
		{name: "uuid", fn: builtinUUID, autoInvoke: true, checkSpec: &staticCallSpec{minArgs: 0, maxArgs: 0, rejectKeywords: true, rejectBlock: true, resultType: checkTypeString}},
		{name: "warn", fn: builtinWarn, checkSpec: &staticCallSpec{minArgs: 0, maxArgs: -1, rejectKeywords: true, rejectBlock: true, resultType: checkTypeNil}},
		{name: "random_id", fn: builtinRandomID, checkSpec: &staticCallSpec{minArgs: 0, maxArgs: 1, rejectKeywords: true, rejectBlock: true, paramTypes: []*TypeExpr{checkTypeInt}, resultType: checkTypeString}},
		{name: "to_int", fn: builtinToInt, checkSpec: &staticCallSpec{minArgs: 1, maxArgs: 1, rejectKeywords: true, rejectBlock: true, paramTypes: []*TypeExpr{builtinTypeScalar}, resultType: checkTypeInt}},
		{name: "to_float", fn: builtinToFloat, checkSpec: &staticCallSpec{minArgs: 1, maxArgs: 1, rejectKeywords: true, rejectBlock: true, paramTypes: []*TypeExpr{builtinTypeScalar}, resultType: checkTypeFloat}},
	} {
		engine.registerDefaultBuiltin(builtin)
	}
}

// registerRemovedCallableFences keeps the removed callable constructors --
// proc, lambda, and Proc.new -- resolvable so a script that reaches for one
// gets the teaching error the parser gives the -> literal instead of
// "undefined variable lambda". Each fence auto-invokes so a bare read fails
// the same way a call does, and its spec carries the same message for the
// checker.
func registerRemovedCallableFences(engine *Engine) {
	for _, name := range []string{"proc", "lambda"} {
		engine.registerDefaultBuiltin(builtinDefinition{
			name:       name,
			fn:         builtinRemovedCallable(name),
			autoInvoke: true,
			checkSpec:  &staticCallSpec{minArgs: 0, maxArgs: -1, removedMessage: removedCallableMessage(name)},
		})
	}
	// Proc itself is a fence too, not a namespace object: an object would
	// read as a value (`x = Proc`), which is exactly what the removal
	// forbids. Auto-invocation makes the bare read, the member form
	// (Proc.new resolves the receiver first), and any call all land on the
	// same teaching error.
	engine.registerDefaultBuiltin(builtinDefinition{
		name:       "Proc",
		fn:         builtinRemovedCallable("Proc.new"),
		autoInvoke: true,
		checkSpec:  &staticCallSpec{minArgs: 0, maxArgs: -1, removedMessage: removedCallableMessage("Proc.new")},
	})
}

// Builtins returns a copy of the registered builtin map.
func (e *Engine) Builtins() map[string]Value {
	return e.builtinSnapshot()
}

func (e *Engine) builtinSnapshot() map[string]Value {
	e.builtinsMu.RLock()
	defer e.builtinsMu.RUnlock()

	out := make(map[string]Value, len(e.builtins))
	for name, builtin := range e.builtins {
		out[name] = cloneBuiltinValue(builtin)
	}
	return out
}

// attachBuiltins chains root to the engine's frozen builtin proto env.
func (e *Engine) attachBuiltins(root *Env, extraStatics int) {
	root.callRoot = true
	e.builtinsMu.RLock()
	if e.builtinProto != nil {
		defer e.builtinsMu.RUnlock()
		e.bindBuiltinsLocked(root, extraStatics)
		return
	}
	e.builtinsMu.RUnlock()

	e.builtinsMu.Lock()
	defer e.builtinsMu.Unlock()
	if e.builtinProto == nil {
		proto := newEnv(nil)
		proto.growStatics(len(e.builtins))
		for name, builtin := range e.builtins {
			proto.DefineStatic(name, builtin)
		}
		proto.frozen = true
		e.builtinProto = proto
	}
	e.bindBuiltinsLocked(root, extraStatics)
}

// bindBuiltinsLocked wires root to the current proto. Callers must hold builtinsMu.
func (e *Engine) bindBuiltinsLocked(root *Env, extraStatics int) {
	root.parent = e.builtinProto
	root.growStatics(extraStatics)
}

// builtinNeedsCallClone reports whether a builtin value is mutable from
// scripts (arrays, hashes, object namespaces like JSON or Time) and must
// therefore be deep-cloned into each call root for isolation.
func builtinNeedsCallClone(val Value) bool {
	switch val.Kind() {
	case KindArray, KindHash, KindObject:
		return true
	default:
		return false
	}
}

func cloneBuiltinValueForCall(val Value) Value {
	switch val.Kind() {
	case KindArray:
		arr := val.Array()
		cloned := make([]Value, len(arr))
		for i, elem := range arr {
			cloned[i] = cloneBuiltinValueForCall(elem)
		}
		return NewArray(cloned)
	case KindHash:
		return NewHash(cloneBuiltinMapForCall(val.HashEntryMap()))
	case KindObject:
		return retagClonedObject(val, cloneBuiltinMapForCall(val.HashEntryMap()))
	default:
		return val
	}
}

func cloneBuiltinMapForCall(src map[string]Value) map[string]Value {
	if src == nil {
		return nil
	}
	out := make(map[string]Value, len(src))
	for name, val := range src {
		out[name] = cloneBuiltinValueForCall(val)
	}
	return out
}

func cloneBuiltinValue(val Value) Value {
	switch val.Kind() {
	case KindBuiltin:
		builtin := valueBuiltin(val)
		if builtin == nil {
			return val
		}
		cloned := newBuiltin(builtin.Name, builtin.Fn, builtin.AutoInvoke)
		clonedBuiltin := valueBuiltin(cloned)
		clonedBuiltin.checkSpec = builtin.checkSpec
		clonedBuiltin.SignatureParams = builtin.SignatureParams
		clonedBuiltin.OptionsHashTarget = builtin.OptionsHashTarget
		clonedBuiltin.ReturnTypeTarget = builtin.ReturnTypeTarget
		clonedBuiltin.CapturedValues = builtin.CapturedValues
		clonedBuiltin.Capability = builtin.Capability
		clonedBuiltin.hostDriven = builtin.hostDriven
		// The clone wraps the same Fn, so the promises made about that Go body
		// still describe the clone. Copying them is what keeps a per-call clone
		// from silently reclassifying a builtin as conservative; a wrapper built
		// around a *different* function must not copy them, which is why the
		// other rebuild paths reconstruct from scratch and inherit the
		// conservative default instead.
		clonedBuiltin.nonMutating = builtin.nonMutating
		clonedBuiltin.nonRetaining = builtin.nonRetaining
		// A bound predicate's BoundReceiver and Fn both read one mutable cell, so a
		// shallow copy that shares both stays consistent: the copy reads the same
		// receiver, and a later two-phase clone rebuilds a fresh predicate around
		// that cell's current value.
		clonedBuiltin.BoundReceiver = builtin.BoundReceiver
		return cloned
	case KindArray:
		arr := val.Array()
		cloned := make([]Value, len(arr))
		for i, elem := range arr {
			cloned[i] = cloneBuiltinValue(elem)
		}
		return NewArray(cloned)
	case KindHash:
		return NewHash(cloneBuiltinMap(val.HashEntryMap()))
	case KindObject:
		return retagClonedObject(val, cloneBuiltinMap(val.HashEntryMap()))
	default:
		return val
	}
}

func cloneBuiltinMap(src map[string]Value) map[string]Value {
	if src == nil {
		return nil
	}
	out := make(map[string]Value, len(src))
	for name, val := range src {
		out[name] = cloneBuiltinValue(val)
	}
	return out
}

// ClearModuleCache drops all cached modules and returns the number of entries removed.
// Long-running hosts can call this between script runs to force fresh module reloads.
func (e *Engine) ClearModuleCache() int {
	e.modMu.Lock()
	defer e.modMu.Unlock()

	count := len(e.modules)
	clear(e.modules)
	clear(e.modRequests)
	e.modRequestBytes = 0
	clear(e.modSearchHits)
	clear(e.modSearchMisses)
	clear(e.modSuggest)
	clear(e.modSuggestText)
	e.modSuggestVersion++
	return count
}

// Execute compiles the provided source ensuring it is valid under current config.
func (e *Engine) Execute(ctx context.Context, script string) error {
	if ctx == nil {
		ctx = context.Background()
	}
	select {
	case <-ctx.Done():
		return ctx.Err()
	default:
	}

	_, err := e.Compile(script)
	if err != nil {
		return err
	}
	select {
	case <-ctx.Done():
		return ctx.Err()
	default:
		return nil
	}
}

// ConfigSummary provides a human-readable description of the interpreter limits.
func (e *Engine) ConfigSummary() string {
	summary := fmt.Sprintf("steps=%s memory=%s recursion=%s strict_effects=%t", quotaSummary(e.config.StepQuota, ""), quotaSummary(e.config.MemoryQuotaBytes, "B"), quotaSummary(e.config.RecursionLimit, ""), e.config.StrictEffects)
	if e.config.DevMode {
		summary += " dev_mode=true"
	}
	return summary
}

// quotaSummary renders a resolved quota for ConfigSummary: a positive quota
// prints as its value with the given unit suffix, and a disabled quota (zero
// after resolution) prints as "unlimited".
func quotaSummary(value int, unit string) string {
	if value <= 0 {
		return "unlimited"
	}
	return fmt.Sprintf("%d%s", value, unit)
}

// MaxSourceBytes reports the effective source-size limit, in bytes, applied
// before parsing. The value reflects the configured limit after defaults are
// resolved, so callers can reject oversized inputs before reading them.
func (e *Engine) MaxSourceBytes() int {
	return e.config.MaxSourceBytes
}

func registerDataBuiltins(engine *Engine) {
	engine.builtins["JSON"] = NewObject(map[string]Value{
		"parse":     newCheckedBuiltin("JSON.parse", builtinJSONParse, staticCallSpec{minArgs: 1, maxArgs: 1, rejectKeywords: true, rejectBlock: true, paramTypes: []*TypeExpr{checkTypeString}}),
		"parse_as":  newCheckedBuiltin("JSON.parse_as", builtinJSONParseAs, staticCallSpec{minArgs: 2, maxArgs: 2, rejectKeywords: true, rejectBlock: true}),
		"stringify": DeclareNonMutating(newCheckedBuiltin("JSON.stringify", builtinJSONStringify, staticCallSpec{minArgs: 1, maxArgs: 1, rejectKeywords: true, rejectBlock: true, resultType: checkTypeString})),
	})
	engine.builtins["Regex"] = NewObject(map[string]Value{
		"match":       newCheckedBuiltin("Regex.match", builtinRegexMatch, staticCallSpec{minArgs: 2, maxArgs: 2, rejectKeywords: true, rejectBlock: true, paramTypes: []*TypeExpr{checkTypeString, checkTypeString}, resultType: builtinTypeNullableString}),
		"replace":     newCheckedBuiltin("Regex.replace", builtinRegexReplace, staticCallSpec{minArgs: 3, maxArgs: 3, rejectKeywords: true, rejectBlock: true, paramTypes: []*TypeExpr{checkTypeString, checkTypeString, checkTypeString}, resultType: checkTypeString}),
		"replace_all": newCheckedBuiltin("Regex.replace_all", builtinRegexReplaceAll, staticCallSpec{minArgs: 3, maxArgs: 3, rejectKeywords: true, rejectBlock: true, paramTypes: []*TypeExpr{checkTypeString, checkTypeString, checkTypeString}, resultType: checkTypeString}),
	})
	engine.builtins["Regexp"] = NewObject(map[string]Value{
		"escape":     newCheckedBuiltin("Regexp.escape", builtinRegexpEscape, staticCallSpec{minArgs: 1, maxArgs: 1, rejectKeywords: true, rejectBlock: true, paramTypes: []*TypeExpr{checkTypeString}, resultType: checkTypeString}),
		"quote":      newCheckedBuiltin("Regexp.quote", builtinRegexpEscape, staticCallSpec{minArgs: 1, maxArgs: 1, rejectKeywords: true, rejectBlock: true, paramTypes: []*TypeExpr{checkTypeString}, resultType: checkTypeString}),
		"new":        newCheckedBuiltin("Regexp.new", builtinRegexpNew, staticCallSpec{minArgs: 1, maxArgs: 1, rejectKeywords: true, rejectBlock: true, paramTypes: []*TypeExpr{checkTypeString}}),
		"union":      NewBuiltin("Regexp.union", builtinRegexpUnion),
		"last_match": newCheckedAutoBuiltin("Regexp.last_match", builtinRegexpLastMatch, staticCallSpec{minArgs: 0, maxArgs: 0, rejectKeywords: true, rejectBlock: true, resultType: checkTypeNil}),
	})
}

// registerHashBuiltins exposes the Hash namespace, whose new constructor builds
// an empty hash. Per-hash defaults are gone, so Hash.new takes no argument and
// no block: a missing key reads as nil and fetch supplies an explicit fallback.
func registerHashBuiltins(engine *Engine) {
	engine.builtins["Hash"] = NewObject(map[string]Value{
		// AutoBuiltin so a bare `Hash.new` (no parentheses) builds an empty
		// hash. Explicit `Hash.new(...)` calls still flow through the normal
		// call path, where they are rejected.
		"new": newCheckedAutoBuiltin("Hash.new", builtinHashNew, staticCallSpec{minArgs: 0, maxArgs: 0, rejectKeywords: true, rejectBlock: true, resultType: checkTypeHash}),
	})
}

func builtinHashNew(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
	if len(kwargs) > 0 {
		return NewNil(), fmt.Errorf("Hash.new does not accept keyword arguments")
	}
	if len(args) > 0 || !block.IsNil() {
		return NewNil(), fmt.Errorf("Hash.new takes no default: a missing key reads as nil, and hash.fetch(key, fallback) supplies a default per lookup")
	}
	return NewHash(make(map[string]Value)), nil
}

func registerDurationBuiltins(engine *Engine) {
	engine.builtins["Duration"] = NewObject(map[string]Value{
		"build": newCheckedBuiltin("Duration.build", func(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
			if len(args) == 1 && len(kwargs) == 0 {
				secs, err := numericToSeconds(args[0])
				if err != nil {
					return NewNil(), err
				}
				return NewDuration(durationFromSeconds(secs)), nil
			}
			if len(args) > 0 {
				return NewNil(), fmt.Errorf("Duration.build accepts either seconds or named parts, not both")
			}
			if len(kwargs) == 0 {
				return NewNil(), fmt.Errorf("Duration.build expects seconds or named parts")
			}
			allowed := map[string]struct{}{
				"weeks":   {},
				"days":    {},
				"hours":   {},
				"minutes": {},
				"seconds": {},
			}
			for key := range kwargs {
				if _, ok := allowed[key]; !ok {
					return NewNil(), fmt.Errorf("Duration.build unknown part %q", key)
				}
			}

			parsePart := func(name string) (int64, error) {
				if v, ok := kwargs[name]; ok {
					return numericToSeconds(v)
				}
				return 0, nil
			}
			weeks, err := parsePart("weeks")
			if err != nil {
				return NewNil(), fmt.Errorf("Duration.build %s: %w", "weeks", err)
			}
			days, err := parsePart("days")
			if err != nil {
				return NewNil(), fmt.Errorf("Duration.build %s: %w", "days", err)
			}
			hours, err := parsePart("hours")
			if err != nil {
				return NewNil(), fmt.Errorf("Duration.build %s: %w", "hours", err)
			}
			minutes, err := parsePart("minutes")
			if err != nil {
				return NewNil(), fmt.Errorf("Duration.build %s: %w", "minutes", err)
			}
			seconds, err := parsePart("seconds")
			if err != nil {
				return NewNil(), fmt.Errorf("Duration.build %s: %w", "seconds", err)
			}
			return NewDuration(durationFromParts(weeks, days, hours, minutes, seconds)), nil
		}, staticCallSpec{
			minArgs:         0,
			maxArgs:         1,
			allowedKeywords: keywordSet("weeks", "days", "hours", "minutes", "seconds"),
			paramTypes:      []*TypeExpr{checkTypeNumber},
			keywordTypes: map[string]*TypeExpr{
				"weeks":   checkTypeNumber,
				"days":    checkTypeNumber,
				"hours":   checkTypeNumber,
				"minutes": checkTypeNumber,
				"seconds": checkTypeNumber,
			},
			resultType: checkTypeDuration,
		}),
		"parse": newCheckedBuiltin("Duration.parse", func(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
			if len(args) != 1 || args[0].Kind() != KindString {
				return NewNil(), fmt.Errorf("Duration.parse expects a duration string")
			}
			parsed, err := parseDurationString(args[0].String())
			if err != nil {
				return NewNil(), err
			}
			return NewDuration(parsed), nil
		}, staticCallSpec{minArgs: 1, maxArgs: 1, paramTypes: []*TypeExpr{checkTypeString}, resultType: checkTypeDuration}),
	})
}

func registerTimeBuiltins(engine *Engine) {
	timeInKeyword := map[string]*TypeExpr{"in": builtinTypeNullableString}
	engine.builtins["Time"] = NewObject(map[string]Value{
		"new": newCheckedBuiltin("Time.new", func(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
			loc := time.Local
			if zone, ok := kwargs["in"]; ok {
				parsed, err := parseLocation(zone)
				if err != nil {
					return NewNil(), err
				}
				if parsed != nil {
					loc = parsed
				}
			}
			t, err := timeFromParts(args, loc)
			if err != nil {
				return NewNil(), err
			}
			return NewTime(t), nil
		}, staticCallSpec{minArgs: 1, maxArgs: -1, keywordTypes: timeInKeyword, resultType: checkTypeTime}),
		"local": newCheckedBuiltin("Time.local", func(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
			t, err := timeFromCalendarParts(args, time.Local)
			if err != nil {
				return NewNil(), err
			}
			return NewTime(t), nil
		}, staticCallSpec{minArgs: 1, maxArgs: 7, resultType: checkTypeTime}),
		"mktime": newCheckedAutoBuiltin("Time.mktime", func(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
			t, err := timeFromCalendarParts(args, time.Local)
			if err != nil {
				return NewNil(), err
			}
			return NewTime(t), nil
		}, staticCallSpec{minArgs: 1, maxArgs: 7, resultType: checkTypeTime}),
		"utc": newCheckedBuiltin("Time.utc", func(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
			t, err := timeFromCalendarParts(args, time.UTC)
			if err != nil {
				return NewNil(), err
			}
			return NewTime(t), nil
		}, staticCallSpec{minArgs: 1, maxArgs: 7, resultType: checkTypeTime}),
		"gm": newCheckedAutoBuiltin("Time.gm", func(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
			t, err := timeFromCalendarParts(args, time.UTC)
			if err != nil {
				return NewNil(), err
			}
			return NewTime(t), nil
		}, staticCallSpec{minArgs: 1, maxArgs: 7, resultType: checkTypeTime}),
		"at": newCheckedBuiltin("Time.at", func(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
			if len(args) < 1 || len(args) > 3 {
				return NewNil(), fmt.Errorf("Time.at expects seconds since epoch with optional subsecond value and unit")
			}
			for key := range kwargs {
				if key != "in" {
					return NewNil(), fmt.Errorf("Time.at unknown keyword argument %s", key)
				}
			}
			var loc *time.Location
			if in, ok := kwargs["in"]; ok {
				parsed, err := parseLocation(in)
				if err != nil {
					return NewNil(), err
				}
				loc = parsed
			}
			var subsec, unit *Value
			if len(args) >= 2 {
				subsec = &args[1]
			}
			if len(args) == 3 {
				unit = &args[2]
			}
			t, err := timeFromEpochParts(args[0], subsec, unit, loc)
			if err != nil {
				return NewNil(), err
			}
			return NewTime(t), nil
		}, staticCallSpec{
			minArgs:         1,
			maxArgs:         3,
			allowedKeywords: keywordSet("in"),
			paramTypes:      []*TypeExpr{nil, nil, checkTypeSymbol},
			keywordTypes:    timeInKeyword,
			resultType:      checkTypeTime,
		}),
		"now": newCheckedAutoBuiltin("Time.now", func(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
			if len(args) > 0 {
				return NewNil(), fmt.Errorf("Time.now does not take positional arguments")
			}
			// UTC by default, so Time.now and the now builtin answer the same
			// question the same way. now is documented as a UTC timestamp and
			// always was one; Time.now returning host-local meant one concept
			// had two answers, and they could differ by a calendar day.
			loc := time.UTC
			if in, ok := kwargs["in"]; ok {
				parsed, err := parseLocation(in)
				if err != nil {
					return NewNil(), err
				}
				if parsed != nil {
					loc = parsed
				}
			}
			return NewTime(time.Now().In(loc)), nil
		}, staticCallSpec{minArgs: 0, maxArgs: 0, keywordTypes: timeInKeyword, resultType: checkTypeTime}),
		"parse": newCheckedBuiltin("Time.parse", func(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
			return timeParseValues(args, kwargs)
		}, staticCallSpec{
			minArgs:         1,
			maxArgs:         2,
			allowedKeywords: keywordSet("in"),
			paramTypes:      []*TypeExpr{checkTypeString, builtinTypeNullableString},
			keywordTypes:    timeInKeyword,
			resultType:      checkTypeTime,
		}),
	})
}

func timeParseValues(args []Value, kwargs map[string]Value) (Value, error) {
	if len(args) < 1 || len(args) > 2 {
		return NewNil(), fmt.Errorf("Time.parse expects a time string and optional layout")
	}
	var layout Value
	hasLayout := false
	if len(args) == 2 {
		layout = args[1]
		hasLayout = true
	}
	var loc *time.Location
	for key, val := range kwargs {
		if key != "in" {
			return NewNil(), fmt.Errorf("Time.parse unknown keyword argument %s", key)
		}
		parsed, err := parseLocation(val)
		if err != nil {
			return NewNil(), err
		}
		loc = parsed
	}
	return timeParseResult(args[0], layout, hasLayout, loc)
}

func timeParseResult(input, layout Value, hasLayout bool, loc *time.Location) (Value, error) {
	if input.Kind() != KindString {
		return NewNil(), fmt.Errorf("Time.parse expects a time string and optional layout")
	}
	layoutText := ""
	useLayout := false
	if hasLayout {
		if layout.Kind() == KindString {
			layoutText = layout.String()
			useLayout = true
		} else if layout.Kind() != KindNil {
			return NewNil(), fmt.Errorf("Time.parse layout must be string")
		}
	}
	t, err := parseTimeString(input.String(), layoutText, useLayout, loc)
	if err != nil {
		return NewNil(), err
	}
	return NewTime(t), nil
}
