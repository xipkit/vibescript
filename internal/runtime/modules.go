package runtime

import (
	"errors"
	"fmt"
	"io"
	"io/fs"
	"math"
	"os"
	"path"
	"path/filepath"
	"reflect"
	"slices"
	"strings"
	"time"

	"github.com/mgomes/vibescript/internal/ast"
)

type moduleEntry struct {
	key    string
	name   string
	path   string
	script *Script
	stamp  moduleStamp
}

// moduleStamp is the change signature for a compiled module's source file
// (mtime+size), mirroring the fileStamp the CLI watch loop uses. Content
// hashing is deliberately not used: it would re-read every module on every
// require, defeating the cache dev mode exists to keep warm.
type moduleStamp struct {
	modTime time.Time
	size    int64
}

func (s moduleStamp) equals(o moduleStamp) bool {
	return s.size == o.size && s.modTime.Equal(o.modTime)
}

func statModuleStamp(path string) (moduleStamp, error) {
	info, err := os.Stat(path)
	if err != nil {
		return moduleStamp{}, err
	}
	return moduleStamp{modTime: info.ModTime(), size: info.Size()}, nil
}

type moduleRequest struct {
	raw              string
	normalized       string
	explicitRelative bool
}

const (
	moduleKeySeparator       = "::"
	moduleEntrypointFunction = "<module>"
)

func (e *Engine) getCachedModule(key string) (moduleEntry, bool) {
	e.modMu.RLock()
	entry, ok := e.modules[key]
	e.modMu.RUnlock()
	return entry, ok
}

// getValidCachedModule returns the cached module for key. In dev mode the
// entry's source stamp is revalidated first; a stale (or deleted) source
// evicts the entry and reports a miss so the caller's normal load path
// re-resolves and recompiles it.
func (e *Engine) getValidCachedModule(key string) (moduleEntry, bool) {
	entry, ok := e.getCachedModule(key)
	if !ok || !e.config.DevMode {
		return entry, ok
	}
	if current, err := statModuleStamp(entry.path); err == nil && current.equals(entry.stamp) {
		return entry, true
	}
	e.invalidateStaleModule(key, entry.script)
	return moduleEntry{}, false
}

// pinnedModuleEntry serves a module key the calling execution has already
// required, bypassing revalidation and disk entirely so a repeated require
// inside one Call keeps the version the call first loaded, even if the file
// has since changed or broken. If a concurrent reload evicted the engine
// cache entry, a key-only entry is returned: builtinRequire resolves the
// exports from its per-execution table for pinned keys, so the entry's
// script is never read. Callers must pass a key the execution actually
// resolved for the request being served (the relative loader's exact key,
// or builtinRequire's per-name search pin).
func (e *Engine) pinnedModuleEntry(key string, pinned map[string]Value) (moduleEntry, bool) {
	if _, ok := pinned[key]; !ok {
		return moduleEntry{}, false
	}
	if entry, ok := e.getCachedModule(key); ok {
		return entry, true
	}
	return moduleEntry{key: key}, true
}

// invalidateStaleModule deletes key only if the cache still holds the entry
// the caller observed (same *Script), so a concurrent goroutine's fresh
// reload is never evicted.
func (e *Engine) invalidateStaleModule(key string, seen *Script) {
	e.modMu.Lock()
	if cur, ok := e.modules[key]; ok && cur.script == seen {
		delete(e.modules, key)
	}
	e.modMu.Unlock()
}

func shouldExportModuleFunction(fn *ScriptFunction) bool {
	return fn != nil && !fn.Private
}

func parseRequireAlias(kwargs map[string]Value) (string, error) {
	if len(kwargs) == 0 {
		return "", nil
	}
	if len(kwargs) != 1 {
		for key := range kwargs {
			if key != "as" {
				return "", fmt.Errorf("require: unknown keyword argument %s", key)
			}
		}
		return "", fmt.Errorf("require: unknown keyword arguments")
	}

	aliasVal, ok := kwargs["as"]
	if !ok {
		for key := range kwargs {
			return "", fmt.Errorf("require: unknown keyword argument %s", key)
		}
		return "", fmt.Errorf("require: unknown keyword arguments")
	}

	var aliasName string
	switch aliasVal.Kind() {
	case KindString, KindSymbol:
		aliasName = strings.TrimSpace(aliasVal.String())
	default:
		return "", fmt.Errorf("require: alias must be a string or symbol")
	}

	if !isValidModuleAlias(aliasName) {
		return "", fmt.Errorf("require: invalid alias %q", aliasName)
	}

	return aliasName, nil
}

func isValidModuleAlias(name string) bool {
	if name == "" {
		return false
	}
	runes := []rune(name)
	if len(runes) == 0 || !ast.IsIdentifierStart(runes[0]) {
		return false
	}
	for _, r := range runes[1:] {
		if !ast.IsIdentifierRune(r) {
			return false
		}
	}
	return ast.LookupIdent(name) == ast.TokenIdent
}

func bindRequireAlias(root, scope *Env, alias string, module Value) error {
	if err := validateRequireAliasBinding(root, alias, module); err != nil {
		return err
	}
	if scope != nil && scope != root {
		if err := validateRequireAliasBinding(scope, alias, module); err != nil {
			return err
		}
	}
	if alias == "" {
		return nil
	}
	root.Define(alias, module)
	return nil
}

func validateRequireAliasBinding(root *Env, alias string, module Value) error {
	if alias == "" {
		return nil
	}
	if existing, ok := root.Get(alias); ok {
		if sameObjectValue(existing, module) {
			return nil
		}
		return fmt.Errorf("require: alias %q already defined", alias)
	}
	return nil
}

func sameObjectValue(left, right Value) bool {
	return left.Kind() == KindObject &&
		right.Kind() == KindObject &&
		reflect.ValueOf(left.HashEntryMap()).Pointer() == reflect.ValueOf(right.HashEntryMap()).Pointer()
}

func bindModuleExportsWithoutOverwrite(root *Env, exports map[string]Value) {
	for name, fnVal := range exports {
		if _, exists := root.Get(name); exists {
			continue
		}
		root.Define(name, fnVal)
	}
}

func (e *Engine) compileAndCacheModule(key, root, relative, fullPath string, content []byte, stamp moduleStamp) (moduleEntry, error) {
	if err := e.enforceModulePolicy(relative); err != nil {
		return moduleEntry{}, err
	}

	script, err := e.CompileSnippet(string(content), moduleEntrypointFunction)
	if err != nil {
		return moduleEntry{}, fmt.Errorf("require: compiling %s failed: %w", fullPath, err)
	}

	entry := moduleEntry{
		key:    key,
		name:   filepath.Clean(relative),
		path:   filepath.Clean(fullPath),
		script: script,
		stamp:  stamp,
	}
	script.moduleKey = key
	script.modulePath = entry.path
	script.moduleRoot = filepath.Clean(root)

	e.modMu.Lock()
	if cached, ok := e.modules[key]; ok {
		e.modMu.Unlock()
		return cached, nil
	}
	if len(e.modules) >= e.config.MaxCachedModules {
		e.modMu.Unlock()
		return moduleEntry{}, fmt.Errorf("require: module cache limit reached (%d modules)", e.config.MaxCachedModules)
	}
	e.modules[key] = entry
	e.modMu.Unlock()

	return entry, nil
}

// cloneFunctionForEnv creates a per-call function value with a different environment.
func cloneFunctionForEnv(fn *ScriptFunction, env *Env) *ScriptFunction {
	clone := *fn
	clone.Env = env
	return &clone
}

func moduleContextForEntry(entry moduleEntry) moduleContext {
	return moduleContext{
		key:    entry.key,
		path:   entry.path,
		root:   entry.script.moduleRoot,
		script: entry.script,
	}
}

func initializeModuleForCall(exec *Execution, entry moduleEntry, moduleEnv *Env, moduleClasses map[string]*ClassDef) error {
	exec.pushModuleContext(moduleContextForEntry(entry))
	defer exec.popModuleContext()
	// The module's classes and constants live only in moduleEnv until require
	// publishes its exports, so root it for the estimator while they are being
	// built (#23).
	exec.pushInitializingModule(moduleEnv)
	defer exec.popInitializingModule()

	if err := initializeClassBodiesForCall(exec, moduleEnv, moduleClasses, entry.script.classOrder, entry.script.deferredClassBodies); err != nil {
		return err
	}
	if err := exec.checkContext(); err != nil {
		return err
	}
	if err := executeModuleEntrypoint(exec, entry, moduleEnv); err != nil {
		return err
	}
	return exec.checkContext()
}

func executeModuleEntrypoint(exec *Execution, entry moduleEntry, moduleEnv *Env) error {
	fn := entry.script.functions[moduleEntrypointFunction]
	if fn == nil || len(fn.Body) == 0 {
		return nil
	}

	if err := exec.pushFrame(moduleDisplayName(entry.key), fn.Pos, entry.script, entry.script); err != nil {
		return err
	}
	defer exec.popFrame()

	// Module top-level statements run while the requiring method's tokens are
	// live, but a block created at module level has no enclosing method: pin
	// the home to none so its return reports LocalJumpError instead of
	// returning from the importer.
	exec.pushBlockHomeToken(0)
	_, _, err := exec.evalLocalScopeStatements(fn.Body, moduleEnv)
	exec.popBlockHomeToken()
	if err != nil {
		err = exec.wrapError(err, fn.Pos)
	}
	return err
}

func moduleCycleFromLoadStack(stack []string, next string) ([]string, bool) {
	for idx, key := range stack {
		if key == next {
			cycle := make([]string, len(stack)-idx+1)
			copy(cycle, stack[idx:])
			cycle[len(cycle)-1] = next
			return cycle, true
		}
	}
	return nil, false
}

func moduleCycleFromExecution(stack []moduleContext, next string) ([]string, bool) {
	if next == "" {
		return nil, false
	}

	firstMatch := -1
	chainLen := 0
	lastKey := ""
	for _, ctx := range stack {
		key := ctx.key
		if key == "" || key == lastKey {
			continue
		}
		if key == next && firstMatch < 0 {
			firstMatch = chainLen
		}
		lastKey = key
		chainLen++
	}
	if chainLen < 2 || firstMatch < 0 || firstMatch >= chainLen-1 {
		return nil, false
	}

	cycle := make([]string, 0, chainLen-firstMatch+1)
	chainIdx := 0
	lastKey = ""
	for _, ctx := range stack {
		key := ctx.key
		if key == "" || key == lastKey {
			continue
		}
		if chainIdx >= firstMatch {
			cycle = append(cycle, key)
		}
		lastKey = key
		chainIdx++
	}
	cycle = append(cycle, next)
	return cycle, true
}

func formatModuleCycle(cycle []string) string {
	if len(cycle) == 0 {
		return ""
	}

	var b strings.Builder
	lastKey := ""
	for _, key := range cycle {
		if key == lastKey {
			continue
		}
		if b.Len() > 0 {
			b.WriteString(" -> ")
		}
		b.WriteString(moduleDisplayName(key))
		lastKey = key
	}
	return b.String()
}

// loadModule resolves and compiles a module. pinned carries the calling
// execution's already-required module keys (nil when there is no execution,
// e.g. the static checker). Relative requires resolve to exactly one key, so
// the loader serves a pinned key directly; search-path requires are pinned by
// resolved name in builtinRequire instead, because probing every root's key
// here would let a pin from a later root override ModulePaths precedence.
func (e *Engine) loadModule(name string, caller *moduleContext, pinned map[string]Value) (moduleEntry, error) {
	request, err := e.parseCachedModuleRequest(name)
	if err != nil {
		return moduleEntry{}, err
	}

	if request.explicitRelative {
		if caller == nil || caller.path == "" || caller.root == "" {
			return moduleEntry{}, fmt.Errorf("require: relative module %q requires a module caller", name)
		}
		return e.loadRelativeModule(request, *caller, pinned)
	}

	return e.loadSearchPathModule(request)
}

func (e *Engine) parseCachedModuleRequest(name string) (moduleRequest, error) {
	e.modMu.RLock()
	request, ok := e.modRequests[name]
	e.modMu.RUnlock()
	if ok {
		return request, nil
	}

	request, err := parseModuleRequest(name)
	if err != nil {
		return moduleRequest{}, err
	}
	e.modMu.Lock()
	if len(e.modRequests) < e.config.MaxCachedModules {
		e.modRequests[name] = request
	}
	e.modMu.Unlock()
	return request, nil
}

func (e *Engine) loadRelativeModule(request moduleRequest, caller moduleContext, pinned map[string]Value) (moduleEntry, error) {
	candidate := filepath.Clean(filepath.Join(filepath.Dir(caller.path), request.normalized))
	relative, err := moduleRelativePathLexical(caller.root, candidate)
	if err != nil {
		return moduleEntry{}, fmt.Errorf("require: module name %q escapes module root", request.raw)
	}
	key := moduleCacheKey(caller.root, relative)

	if entry, ok := e.pinnedModuleEntry(key, pinned); ok {
		return entry, nil
	}
	if entry, ok := e.getValidCachedModule(key); ok {
		return entry, nil
	}

	relative, err = moduleRelativePath(caller.root, candidate)
	if err != nil {
		if errors.Is(err, fs.ErrNotExist) {
			return moduleEntry{}, fmt.Errorf("require: module %q not found%s", request.raw, e.relativeModuleSuggestion(request, caller, candidate))
		}
		return moduleEntry{}, fmt.Errorf("require: module name %q escapes module root", request.raw)
	}

	data, stamp, readErr := e.readModuleSource(candidate)
	if readErr != nil {
		if errors.Is(readErr, fs.ErrNotExist) {
			return moduleEntry{}, fmt.Errorf("require: module %q not found%s", request.raw, e.relativeModuleSuggestion(request, caller, candidate))
		}
		return moduleEntry{}, fmt.Errorf("require: reading %s: %w", candidate, readErr)
	}

	return e.compileAndCacheModule(key, caller.root, relative, candidate, data, stamp)
}

func (e *Engine) loadSearchPathModule(request moduleRequest) (moduleEntry, error) {
	if len(e.modPaths) == 0 {
		return moduleEntry{}, fmt.Errorf("require: module paths not configured")
	}

	if suggestion, ok := e.cachedSearchPathMiss(request.normalized); ok {
		return moduleEntry{}, fmt.Errorf("require: module %q not found%s", request.raw, suggestion)
	}
	if entry, ok := e.cachedSearchPathHit(request.normalized); ok {
		return entry, nil
	}

	for _, root := range e.modPaths {
		key := moduleCacheKey(root, request.normalized)
		candidate := filepath.Join(root, request.normalized)

		if entry, ok := e.getValidCachedModule(key); ok {
			e.cacheSearchPathHit(request.normalized, entry)
			return entry, nil
		}

		if _, err := moduleRelativePath(root, candidate); err != nil {
			return moduleEntry{}, fmt.Errorf("require: module name %q escapes module root", request.raw)
		}
		data, stamp, readErr := e.readModuleSource(candidate)
		if readErr != nil {
			if errors.Is(readErr, fs.ErrNotExist) {
				continue
			}
			return moduleEntry{}, fmt.Errorf("require: reading %s: %w", candidate, readErr)
		}

		entry, err := e.compileAndCacheModule(key, root, request.normalized, candidate, data, stamp)
		if err != nil {
			return moduleEntry{}, err
		}
		e.cacheSearchPathHit(request.normalized, entry)
		return entry, nil
	}

	suggestion := e.searchPathModuleSuggestion(request)
	e.cacheSearchPathMiss(request.normalized, suggestion)
	return moduleEntry{}, fmt.Errorf("require: module %q not found%s", request.raw, suggestion)
}

// The search-path hit/miss and did-you-mean caches below are perf
// short-circuits derived from a filesystem state that dev mode expects to
// change: a cached hit would bypass stamp revalidation, and a cached miss
// would hide a newly created module file. Dev mode disables them.

func (e *Engine) cachedSearchPathHit(normalized string) (moduleEntry, bool) {
	if e.config.DevMode {
		return moduleEntry{}, false
	}
	e.modMu.RLock()
	entry, ok := e.modSearchHits[normalized]
	e.modMu.RUnlock()
	return entry, ok
}

func (e *Engine) cacheSearchPathHit(normalized string, entry moduleEntry) {
	if e.config.DevMode {
		return
	}
	e.modMu.Lock()
	if len(e.modSearchHits) < e.config.MaxCachedModules {
		e.modSearchHits[normalized] = entry
	}
	e.modMu.Unlock()
}

func (e *Engine) cachedSearchPathMiss(normalized string) (string, bool) {
	if e.config.DevMode {
		return "", false
	}
	e.modMu.RLock()
	suggestion, ok := e.modSearchMisses[normalized]
	e.modMu.RUnlock()
	return suggestion, ok
}

func (e *Engine) cacheSearchPathMiss(normalized, suggestion string) {
	if e.config.DevMode {
		return
	}
	e.modMu.Lock()
	if len(e.modSearchMisses) < e.config.MaxCachedModules {
		e.modSearchMisses[normalized] = suggestion
	}
	e.modMu.Unlock()
}

// moduleSuggestWalkLimit caps how many directory entries are examined per
// search root while collecting "did you mean" candidates on the error path.
const moduleSuggestWalkLimit = 2048

// searchPathModuleSuggestion renders a did-you-mean suffix for a module that
// was not found on the engine's search paths. Candidates are the .vibe files
// under each search root that module policy would allow, written the way a
// script would require them.
func (e *Engine) searchPathModuleSuggestion(request moduleRequest) string {
	cacheKey := request.normalized
	e.modMu.RLock()
	version := e.modSuggestVersion
	suggestion, ok := e.modSuggestText[cacheKey]
	e.modMu.RUnlock()
	if ok && !e.config.DevMode {
		return suggestion
	}

	target := moduleDisplayFromRelative(request.normalized)
	candidates := make([]string, 0, 16)
	for _, root := range e.modPaths {
		candidates = append(candidates, e.cachedModuleCandidatesUnderRoot(root)...)
	}
	suggestion = didYouMean(target, candidates)
	if e.config.DevMode {
		return suggestion
	}

	e.modMu.Lock()
	if e.modSuggestText == nil {
		e.modSuggestText = make(map[string]string)
	}
	if cached, ok := e.modSuggestText[cacheKey]; ok {
		e.modMu.Unlock()
		return cached
	}
	if version == e.modSuggestVersion && len(e.modSuggestText) < e.config.MaxCachedModules {
		e.modSuggestText[cacheKey] = suggestion
	}
	e.modMu.Unlock()
	return suggestion
}

func (e *Engine) cachedModuleCandidatesUnderRoot(root string) []string {
	cleanRoot := filepath.Clean(root)
	e.modMu.RLock()
	version := e.modSuggestVersion
	candidates, ok := e.modSuggest[cleanRoot]
	e.modMu.RUnlock()
	if ok && !e.config.DevMode {
		return candidates
	}

	candidates = e.moduleCandidatesUnderRoot(cleanRoot)
	if e.config.DevMode {
		return candidates
	}

	e.modMu.Lock()
	if e.modSuggest == nil {
		e.modSuggest = make(map[string][]string)
	}
	if cached, ok := e.modSuggest[cleanRoot]; ok {
		e.modMu.Unlock()
		return cached
	}
	if version == e.modSuggestVersion {
		e.modSuggest[cleanRoot] = candidates
	}
	e.modMu.Unlock()
	return candidates
}

func (e *Engine) moduleCandidatesUnderRoot(root string) []string {
	cleanRoot := filepath.Clean(root)
	names := make([]string, 0, 16)
	remaining := moduleSuggestWalkLimit
	// Walked by hand rather than with filepath.WalkDir: WalkDir reads and
	// sorts each directory in full before the callback can stop it, so a
	// visited counter cannot bound the work a single huge directory does.
	// Suggestions are built off a failed require, outside the step and memory
	// quotas, so a tenant able to grow a module directory could make a typoed
	// require read it whole (#53).
	dirs := []string{cleanRoot}
	for len(dirs) > 0 && remaining > 0 {
		dir := dirs[0]
		dirs = dirs[1:]
		entries, err := readDirBounded(dir, remaining)
		if err != nil {
			continue
		}
		for _, entry := range entries {
			remaining--
			fullPath := filepath.Join(dir, entry.Name())
			if entry.IsDir() {
				dirs = append(dirs, fullPath)
				continue
			}
			if filepath.Ext(entry.Name()) != ".vibe" {
				continue
			}
			// Mirror the loader's realpath containment so symlinks that
			// escape the module root are never suggested (the loader would
			// reject them, and suggesting them leaks paths outside the
			// allowed module set).
			relative, relErr := moduleRelativePath(cleanRoot, fullPath)
			if relErr != nil || e.enforceModulePolicy(relative) != nil {
				continue
			}
			names = append(names, moduleDisplayFromRelative(relative))
		}
	}
	return names
}

// readDirBounded reads at most limit entries from dir. os.ReadDir reads the
// whole directory and sorts it before returning, which is the cost this
// bounds; File.ReadDir stops at the requested count and does not sort.
func readDirBounded(dir string, limit int) ([]fs.DirEntry, error) {
	if limit <= 0 {
		return nil, nil
	}
	f, err := os.Open(dir)
	if err != nil {
		return nil, err
	}
	defer func() { _ = f.Close() }()
	entries, err := f.ReadDir(limit)
	if err != nil && len(entries) == 0 {
		return nil, err
	}
	return entries, nil
}

// relativeModuleSuggestion renders a did-you-mean suffix for a relative
// require that did not resolve. Candidates are the policy-allowed .vibe
// files in the directory the request pointed at, written with the same
// directory prefix the script used so suggestions can be copied verbatim.
func (e *Engine) relativeModuleSuggestion(request moduleRequest, caller moduleContext, missing string) string {
	dir := filepath.Dir(missing)
	// Bounded for the same reason as the root walk: os.ReadDir would read and
	// sort the whole directory before any candidate filtering runs.
	entries, err := readDirBounded(dir, moduleSuggestWalkLimit)
	if err != nil {
		return ""
	}
	rawPrefix := rawRelativePrefix(request.raw)
	candidates := make([]string, 0, len(entries))
	for _, entry := range entries {
		if entry.IsDir() || filepath.Ext(entry.Name()) != ".vibe" {
			continue
		}
		relative, relErr := moduleRelativePath(caller.root, filepath.Join(dir, entry.Name()))
		if relErr != nil || e.enforceModulePolicy(relative) != nil {
			continue
		}
		candidates = append(candidates, rawPrefix+"/"+moduleRequireName(entry.Name()))
	}
	target := rawPrefix + "/" + moduleRequireName(filepath.Base(request.normalized))
	return didYouMean(target, candidates)
}

// moduleDisplayFromRelative renders a root-relative module path the way a
// script would write it in require: slash separated, without the .vibe
// extension when require would re-add it.
func moduleDisplayFromRelative(relative string) string {
	slashed := filepath.ToSlash(relative)
	dir, base := path.Split(slashed)
	return dir + moduleRequireName(base)
}

// moduleRequireName converts a .vibe filename into the name a script
// passes to require. The extension is only trimmed when the remainder
// has no extension of its own: require appends ".vibe" solely to
// extensionless names, so trimming "helper.vibe.vibe" to "helper.vibe"
// (or "data.json.vibe" to "data.json") would resolve a different file.
// An empty name or newly exposed edge whitespace would also change the request.
func moduleRequireName(filename string) string {
	trimmed := strings.TrimSuffix(filename, ".vibe")
	if trimmed == "" || path.Ext(trimmed) != "" || strings.TrimSpace(trimmed) != trimmed {
		return filename
	}
	return trimmed
}

// rawRelativePrefix returns everything before the final path element of
// a relative require exactly as the script wrote it. path.Dir would
// clean away the explicit "./" (turning "./sub/helprs" into "sub"),
// which flips the suggestion from caller-relative to search-path
// resolution.
func rawRelativePrefix(raw string) string {
	slashed := filepath.ToSlash(strings.TrimSpace(raw))
	idx := strings.LastIndex(slashed, "/")
	if idx < 0 {
		return "."
	}
	return slashed[:idx]
}

// readModuleSource returns the module's source bytes along with the stamp of
// the file they were read from. The stamp comes from the same open file, so
// it always describes the exact content returned.
func (e *Engine) readModuleSource(path string) ([]byte, moduleStamp, error) {
	f, err := openModuleSource(path)
	if err != nil {
		return nil, moduleStamp{}, fmt.Errorf("open module source %s: %w", path, err)
	}
	defer func() { _ = f.Close() }()

	info, err := f.Stat()
	if err != nil {
		return nil, moduleStamp{}, fmt.Errorf("stat module source %s: %w", path, err)
	}
	if !info.Mode().IsRegular() {
		return nil, moduleStamp{}, fmt.Errorf("%s is not a regular file", path)
	}
	stamp := moduleStamp{modTime: info.ModTime(), size: info.Size()}
	if e.config.MaxSourceBytes > 0 && info.Size() > int64(e.config.MaxSourceBytes) {
		return nil, moduleStamp{}, fmt.Errorf("source exceeds maximum size (%d > %d bytes)", info.Size(), e.config.MaxSourceBytes)
	}
	if e.config.MaxSourceBytes <= 0 || e.config.MaxSourceBytes == math.MaxInt {
		data, err := io.ReadAll(f)
		if err != nil {
			return nil, moduleStamp{}, err
		}
		return data, stamp, nil
	}
	data, err := io.ReadAll(io.LimitReader(f, int64(e.config.MaxSourceBytes)+1))
	if err != nil {
		return nil, moduleStamp{}, err
	}
	if len(data) > e.config.MaxSourceBytes {
		return nil, moduleStamp{}, fmt.Errorf("source exceeds maximum size (> %d bytes)", e.config.MaxSourceBytes)
	}
	return data, stamp, nil
}

func parseModuleRequest(name string) (moduleRequest, error) {
	trimmed := strings.TrimSpace(name)
	if trimmed == "" {
		return moduleRequest{}, fmt.Errorf("require: module name must be non-empty")
	}
	normalizedName := strings.ReplaceAll(trimmed, "\\", string(filepath.Separator))
	normalizedName = strings.ReplaceAll(normalizedName, "/", string(filepath.Separator))

	request := moduleRequest{
		raw:              name,
		explicitRelative: isExplicitRelativeModulePath(trimmed),
	}

	request.normalized = filepath.Clean(normalizedName)
	if request.normalized == "." {
		return moduleRequest{}, fmt.Errorf("require: module name %q resolves to current directory", name)
	}
	if filepath.IsAbs(request.normalized) {
		return moduleRequest{}, fmt.Errorf("require: module name %q must be relative", name)
	}
	if !request.explicitRelative && containsPathTraversal(request.normalized) {
		return moduleRequest{}, fmt.Errorf("require: module name %q escapes search paths", name)
	}
	if base := filepath.Base(request.normalized); base == "." || base == ".." {
		return moduleRequest{}, fmt.Errorf("require: module name %q resolves to a directory", name)
	}
	if filepath.Ext(request.normalized) == "" {
		request.normalized += ".vibe"
	}

	return request, nil
}

func isExplicitRelativeModulePath(name string) bool {
	return strings.HasPrefix(name, "./") ||
		strings.HasPrefix(name, "../") ||
		strings.HasPrefix(name, ".\\") ||
		strings.HasPrefix(name, "..\\")
}

func containsPathTraversal(cleanPath string) bool {
	normalized := strings.ReplaceAll(filepath.Clean(cleanPath), "\\", "/")
	return slices.Contains(strings.Split(normalized, "/"), "..")
}

func moduleCacheKey(root, relative string) string {
	return filepath.Clean(root) + moduleKeySeparator + filepath.Clean(relative)
}

func moduleKeyDisplay(key string) string {
	idx := strings.LastIndex(key, moduleKeySeparator)
	if idx < 0 {
		return key
	}
	display := key[idx+len(moduleKeySeparator):]
	if display == "" {
		return key
	}
	return display
}

func moduleDisplayName(key string) string {
	display := filepath.ToSlash(moduleKeyDisplay(key))
	return strings.TrimSuffix(display, ".vibe")
}

func moduleRelativePath(root, fullPath string) (string, error) {
	rel, err := moduleRelativePathLexical(root, fullPath)
	if err != nil {
		return "", err
	}
	cleanRoot := filepath.Clean(root)
	cleanPath := filepath.Clean(fullPath)

	resolvedRoot, err := resolvedExistingPath(cleanRoot)
	if err != nil {
		return "", err
	}
	resolvedPath, err := resolvedPathWithMissing(cleanPath)
	if err != nil {
		return "", err
	}
	resolvedRel, err := filepath.Rel(resolvedRoot, resolvedPath)
	if err != nil {
		return "", err
	}
	resolvedRel = filepath.Clean(resolvedRel)
	sep := string(filepath.Separator)
	if resolvedRel == ".." || strings.HasPrefix(resolvedRel, ".."+sep) || filepath.IsAbs(resolvedRel) {
		return "", fmt.Errorf("require: module path %q escapes module root %q", cleanPath, cleanRoot)
	}
	return rel, nil
}

func moduleRelativePathLexical(root, fullPath string) (string, error) {
	cleanRoot := filepath.Clean(root)
	cleanPath := filepath.Clean(fullPath)

	rel, err := filepath.Rel(cleanRoot, cleanPath)
	if err != nil {
		return "", err
	}
	rel = filepath.Clean(rel)
	sep := string(filepath.Separator)
	if rel == ".." || strings.HasPrefix(rel, ".."+sep) || filepath.IsAbs(rel) {
		return "", fmt.Errorf("require: module path %q escapes module root %q", cleanPath, cleanRoot)
	}
	return rel, nil
}

func resolvedExistingPath(path string) (string, error) {
	absPath, err := filepath.Abs(path)
	if err != nil {
		return "", err
	}
	resolvedPath, err := filepath.EvalSymlinks(absPath)
	if err != nil {
		return "", err
	}
	return filepath.Clean(resolvedPath), nil
}

func resolvedPathWithMissing(path string) (string, error) {
	absPath, err := filepath.Abs(path)
	if err != nil {
		return "", err
	}
	cleanPath := filepath.Clean(absPath)

	existing := cleanPath
	suffix := make([]string, 0, 4)

	for {
		_, statErr := os.Lstat(existing)
		if statErr == nil {
			break
		}
		if !errors.Is(statErr, fs.ErrNotExist) {
			return "", statErr
		}
		parent := filepath.Dir(existing)
		if parent == existing {
			return "", statErr
		}
		suffix = append(suffix, filepath.Base(existing))
		existing = parent
	}

	resolvedExisting, err := filepath.EvalSymlinks(existing)
	if err != nil {
		return "", err
	}

	resolved := filepath.Clean(resolvedExisting)
	for i := len(suffix) - 1; i >= 0; i-- {
		resolved = filepath.Join(resolved, suffix[i])
	}
	return resolved, nil
}

func normalizeModulePolicyPattern(pattern string) string {
	return normalizeModulePolicyValue(strings.TrimSpace(pattern))
}

func normalizeModulePolicyModuleName(relative string) string {
	if strings.TrimSpace(relative) == "" {
		return ""
	}
	return normalizeModulePolicyValue(relative)
}

// Policy names preserve every significant filename byte. Only remove the
// extension when require would restore it without changing the filename.
func normalizeModulePolicyValue(value string) string {
	current := normalizeModulePolicyPath(value)
	if current == "" {
		return ""
	}
	dir, base := path.Split(current)
	current = dir + moduleRequireName(base)
	if strings.TrimSpace(current) != current {
		// Protect literal edge whitespace from the optional padding accepted
		// around configured patterns. Cleaning these dot components for matching
		// preserves the filename and keeps normalization idempotent.
		if path.IsAbs(current) {
			return current + "/."
		}
		return "./" + current + "/."
	}
	return current
}

func normalizeModulePolicyPath(value string) string {
	normalized := strings.ReplaceAll(value, "\\", "/")
	normalized = filepath.ToSlash(normalized)
	normalized = path.Clean(normalized)
	if normalized == "." {
		return ""
	}
	return normalized
}

func validateModulePolicyPatterns(patterns []string, label string) error {
	for _, raw := range patterns {
		pattern := normalizeModulePolicyPattern(raw)
		if pattern == "" {
			return fmt.Errorf("vibes: module %s-list pattern cannot be empty", label)
		}
		if _, err := path.Match(pattern, "probe"); err != nil {
			return fmt.Errorf("vibes: invalid module %s-list pattern %q: %w", label, raw, err)
		}
	}
	return nil
}

func modulePolicyMatch(pattern, module string) bool {
	if module == "" {
		return false
	}
	pattern = path.Clean(pattern)
	module = path.Clean(module)
	if pattern == "*" {
		return true
	}
	matched, err := path.Match(pattern, module)
	if err != nil {
		return false
	}
	return matched
}

func (e *Engine) enforceModulePolicy(relative string) error {
	module := normalizeModulePolicyModuleName(relative)
	if module == "" {
		if len(e.config.ModuleAllowList) > 0 || len(e.config.ModuleDenyList) > 0 {
			return fmt.Errorf("require: module name %q is invalid", relative)
		}
		return nil
	}

	for _, raw := range e.config.ModuleDenyList {
		pattern := normalizeModulePolicyPattern(raw)
		if pattern == "" {
			continue
		}
		if modulePolicyMatch(pattern, module) {
			return fmt.Errorf("require: module %q denied by policy", module)
		}
	}

	if len(e.config.ModuleAllowList) == 0 {
		return nil
	}
	for _, raw := range e.config.ModuleAllowList {
		pattern := normalizeModulePolicyPattern(raw)
		if pattern == "" {
			continue
		}
		if modulePolicyMatch(pattern, module) {
			return nil
		}
	}
	return fmt.Errorf("require: module %q not allowed by policy", module)
}

func builtinRequire(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
	if exec.strictEffects && !exec.allowRequire {
		return NewNil(), fmt.Errorf("strict effects: require is disabled without CallOptions.AllowRequire")
	}
	if len(args) != 1 {
		return NewNil(), fmt.Errorf("require expects a single module name argument")
	}
	if !block.IsNil() {
		return NewNil(), fmt.Errorf("require does not accept blocks")
	}
	if exec.root == nil {
		return NewNil(), fmt.Errorf("require unavailable in this context")
	}
	alias, err := parseRequireAlias(kwargs)
	if err != nil {
		return NewNil(), err
	}

	modNameVal := args[0]
	switch modNameVal.Kind() {
	case KindString, KindSymbol:
		// supported
	default:
		return NewNil(), fmt.Errorf("require expects a string or symbol module name")
	}

	modName := modNameVal.String()

	// A search-path require that this call already resolved is served from
	// its per-call pin, keyed by normalized name, before the loader can
	// touch the engine cache or disk: a concurrent dev-mode reload must not
	// fail or re-resolve a module version this call has pinned.
	var searchPinName string
	var entry moduleEntry
	if request, perr := exec.engine.parseCachedModuleRequest(modName); perr == nil && !request.explicitRelative {
		searchPinName = request.normalized
		if key, ok := exec.moduleSearchPins[searchPinName]; ok {
			entry, _ = exec.engine.pinnedModuleEntry(key, exec.modules)
		}
	}
	if entry.key == "" {
		var err error
		entry, err = exec.engine.loadModule(modName, exec.currentModuleContext(), exec.modules)
		if err != nil {
			return NewNil(), err
		}
	}
	if searchPinName != "" {
		if exec.moduleSearchPins == nil {
			exec.moduleSearchPins = make(map[string]string)
		}
		exec.moduleSearchPins[searchPinName] = entry.key
	}

	if cycle, ok := moduleCycleFromLoadStack(exec.moduleLoadStack, entry.key); ok {
		return NewNil(), fmt.Errorf("require: circular dependency detected: %s", formatModuleCycle(cycle))
	}

	if exec.modules == nil {
		exec.modules = make(map[string]Value)
	}
	if cached, ok := exec.modules[entry.key]; ok {
		if err := bindRequireAlias(exec.root, exec.currentEnv(), alias, cached); err != nil {
			return NewNil(), err
		}
		return cached, nil
	}

	if cycle, ok := moduleCycleFromExecution(exec.moduleStack, entry.key); ok {
		return NewNil(), fmt.Errorf("require: circular dependency detected: %s", formatModuleCycle(cycle))
	}

	if exec.moduleLoading == nil {
		exec.moduleLoading = make(map[string]bool)
	}
	if exec.moduleLoading[entry.key] {
		cycle := append(append([]string(nil), exec.moduleLoadStack...), entry.key)
		return NewNil(), fmt.Errorf("require: circular dependency detected: %s", formatModuleCycle(cycle))
	}
	exec.moduleLoading[entry.key] = true
	exec.moduleLoadStack = append(exec.moduleLoadStack, entry.key)
	defer func() {
		delete(exec.moduleLoading, entry.key)
		if len(exec.moduleLoadStack) > 0 {
			exec.moduleLoadStack = exec.moduleLoadStack[:len(exec.moduleLoadStack)-1]
		}
	}()

	moduleEnv := newAssignmentBoundaryEnv(exec.root)
	exports := make(map[string]Value, len(entry.script.functions)+len(entry.script.enums))
	moduleEnums := cloneEnumsForCall(entry.script.enums)
	for name, enumDef := range moduleEnums {
		enumVal := NewEnum(enumDef)
		moduleEnv.Define(name, enumVal)
		exports[name] = enumVal
	}
	moduleClasses := cloneClassesForCall(entry.script.classes, moduleEnv)
	for name, classDef := range moduleClasses {
		moduleEnv.Define(name, NewClass(classDef))
	}
	for name, fn := range entry.script.functions {
		if name == moduleEntrypointFunction {
			continue
		}
		clone := cloneFunctionForEnv(fn, moduleEnv)
		fnVal := NewFunction(clone)
		moduleEnv.Define(name, fnVal)
		if shouldExportModuleFunction(fn) {
			exports[name] = fnVal
		}
	}
	exportsVal := NewObject(exports)
	aliasScope := exec.currentEnv()
	if aliasScope == nil {
		aliasScope = exec.root
	}
	if err := validateRequireAliasBinding(exec.root, alias, exportsVal); err != nil {
		return NewNil(), err
	}
	if aliasScope != exec.root {
		if err := validateRequireAliasBinding(aliasScope, alias, exportsVal); err != nil {
			return NewNil(), err
		}
	}
	if err := initializeModuleForCall(exec, entry, moduleEnv, moduleClasses); err != nil {
		return NewNil(), err
	}

	bindModuleExportsWithoutOverwrite(exec.root, exports)
	bumpMutationEpoch()
	exec.modules[entry.key] = exportsVal
	if alias != "" {
		exec.root.Define(alias, exportsVal)
	}
	return exportsVal, nil
}
