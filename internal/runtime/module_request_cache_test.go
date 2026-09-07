package runtime

import (
	"context"
	"fmt"
	"strings"
	"sync"
	"testing"
	"unsafe"
)

func retainedModuleRequestBytes(engine *Engine) int {
	engine.modMu.RLock()
	defer engine.modMu.RUnlock()
	bytes := 0
	for key, request := range engine.modRequests {
		bytes += len(key) + len(request.normalized)
	}
	for key := range engine.modSearchHits {
		bytes += len(key)
	}
	for key, suggestion := range engine.modSearchMisses {
		bytes += len(key) + len(suggestion)
	}
	for key, suggestion := range engine.modSuggestText {
		bytes += len(key) + len(suggestion)
	}
	return bytes
}

func TestModuleRequestCacheBoundsBytesAcrossCalls(t *testing.T) {
	t.Parallel()
	for _, symbol := range []bool{false, true} {
		t.Run(fmt.Sprint(symbol), func(t *testing.T) {
			t.Parallel()
			engine := MustNewEngine(Config{ModulePaths: []string{tempModuleTree(t)}, MemoryQuotaBytes: 4 << 20, StepQuota: 10000})
			script := compileScriptWithEngine(t, engine, `def run(name)
  begin
    require(name)
  rescue
    1
  end
end`)
			for i := range 20 {
				name := fmt.Sprintf("missing_%d.vibe", i) + strings.Repeat(" ", 512<<10)
				arg := NewString(name)
				if symbol {
					arg = NewSymbol(name)
				}
				result, err := script.Call(context.Background(), "run", []Value{arg}, CallOptions{})
				if err != nil || result.Int() != 1 {
					t.Fatalf("rescuable require miss %d = %v, %v", i, result, err)
				}
				if retained := retainedModuleRequestBytes(engine); retained > 8<<20 {
					t.Fatalf("engine retained %d request bytes across calls, want at most 8 MiB", retained)
				}
			}
		})
	}
}

func assertModuleStringDetached(t *testing.T, label, text, source string) {
	t.Helper()
	if len(text) == 0 {
		return
	}
	ptr := uintptr(unsafe.Pointer(unsafe.StringData(text)))
	start := uintptr(unsafe.Pointer(unsafe.StringData(source)))
	if ptr >= start && ptr-start < uintptr(len(source)) {
		t.Fatalf("%s retains the caller's larger string backing", label)
	}
}

func TestModuleRequestCacheOwnsRetainedStrings(t *testing.T) {
	t.Parallel()
	backing := strings.Repeat("x", 1<<20) + "helper.vibe" + strings.Repeat("y", 1<<20)
	name := backing[1<<20 : (1<<20)+len("helper.vibe")]
	engine := moduleTestEngine(t)
	request, err := engine.parseCachedModuleRequest(name)
	if err != nil {
		t.Fatal(err)
	}
	if request.raw != name || request.normalized != name {
		t.Fatalf("normalization changed the requested spelling: %+v", request)
	}
	assertModuleStringDetached(t, "cached raw request", request.raw, backing)
	assertModuleStringDetached(t, "cached normalized request", request.normalized, backing)
	for key := range engine.modRequests {
		assertModuleStringDetached(t, "request map key", key, backing)
	}
}

func TestModuleRequestCacheFullStillDetachesLoadedName(t *testing.T) {
	t.Parallel()
	engine := MustNewEngine(Config{ModulePaths: []string{moduleFixturesRoot}, MaxCachedModules: 1})
	if _, err := engine.parseCachedModuleRequest("already_cached"); err != nil {
		t.Fatal(err)
	}
	name := "helper.vibe" + strings.Repeat(" ", 1<<20)
	entry, err := engine.loadModule(name, nil, nil, nil)
	if err != nil {
		t.Fatal(err)
	}
	if entry.name != "helper.vibe" {
		t.Fatalf("loaded name = %q, want helper.vibe", entry.name)
	}
	assertModuleStringDetached(t, "compiled module name", entry.name, name)
	for key := range engine.modSearchHits {
		assertModuleStringDetached(t, "search hit key", key, name)
	}
}

func TestModuleRequestCachesShareByteBudgetAndClear(t *testing.T) {
	engine := moduleTestEngine(t)
	chunk := maxModuleRequestCacheBytes / 8
	engine.cacheSearchPathMiss(strings.Repeat("a", chunk), strings.Repeat("s", chunk))
	engine.cacheSearchPathHit(strings.Repeat("b", chunk), moduleEntry{})
	_ = engine.searchPathModuleSuggestion(moduleRequest{normalized: strings.Repeat("c", chunk)})
	if _, err := engine.parseCachedModuleRequest(strings.Repeat("d", chunk)); err != nil {
		t.Fatal(err)
	}
	remaining := maxModuleRequestCacheBytes - retainedModuleRequestBytes(engine)
	engine.cacheSearchPathMiss(strings.Repeat("e", remaining), "")
	if retained := retainedModuleRequestBytes(engine); retained != maxModuleRequestCacheBytes || engine.modRequestBytes != retained {
		t.Fatalf("cached %d bytes, accounted %d; want exact shared limit %d", retained, engine.modRequestBytes, maxModuleRequestCacheBytes)
	}
	counts := []int{len(engine.modRequests), len(engine.modSearchHits), len(engine.modSearchMisses), len(engine.modSuggestText)}
	name := "  helper.vibe  "
	entry, err := engine.loadModule(name, nil, nil, nil)
	if err != nil || entry.name != "helper.vibe" {
		t.Fatalf("load after cache fills = %q, %v; want helper.vibe", entry.name, err)
	}
	missing := "  absent_name.vibe  "
	if _, err := engine.loadModule(missing, nil, nil, nil); err == nil || !strings.Contains(err.Error(), fmt.Sprintf("%q", missing)) {
		t.Fatalf("uncached miss error = %v, want original spelling", err)
	}
	for i, count := range []int{len(engine.modRequests), len(engine.modSearchHits), len(engine.modSearchMisses), len(engine.modSuggestText)} {
		if count != counts[i] {
			t.Fatalf("cache %d grew from %d to %d after the byte budget filled", i, counts[i], count)
		}
	}
	if count := engine.ClearModuleCache(); count != 1 {
		t.Fatalf("ClearModuleCache returned %d, want one compiled module", count)
	}
	if engine.modRequestBytes != 0 || retainedModuleRequestBytes(engine) != 0 {
		t.Fatal("ClearModuleCache retained request text or accounting")
	}
	if _, err := engine.loadModule(name, nil, nil, nil); err != nil {
		t.Fatal(err)
	}
	if engine.modRequestBytes == 0 || engine.modRequestBytes != retainedModuleRequestBytes(engine) {
		t.Fatal("cache admission did not resume with matching accounting after clear")
	}
}

func TestModuleRequestDerivedCachesOwnStrings(t *testing.T) {
	t.Parallel()
	backing := strings.Repeat(" ", 1<<20) + "missing.vibe" + strings.Repeat(" ", 1<<20)
	key := strings.TrimSpace(backing)
	engine := moduleTestEngine(t)
	engine.cacheSearchPathHit(key, moduleEntry{})
	engine.cacheSearchPathMiss(key, key)
	_ = engine.searchPathModuleSuggestion(moduleRequest{normalized: key})
	for cached := range engine.modSearchHits {
		assertModuleStringDetached(t, "hit key", cached, backing)
	}
	for cached, suggestion := range engine.modSearchMisses {
		assertModuleStringDetached(t, "miss key", cached, backing)
		assertModuleStringDetached(t, "miss suggestion", suggestion, backing)
	}
	for cached := range engine.modSuggestText {
		assertModuleStringDetached(t, "suggestion key", cached, backing)
	}
}

func TestModuleRequestCacheConcurrentDuplicates(t *testing.T) {
	t.Parallel()
	engine := moduleTestEngine(t)
	name := "helper.vibe" + strings.Repeat(" ", 32<<10)
	errs := make([]error, 32)
	start := make(chan struct{})
	var wg sync.WaitGroup
	for i := range errs {
		wg.Go(func() {
			<-start
			request, err := engine.parseCachedModuleRequest(name)
			errs[i] = err
			if err != nil {
				return
			}
			engine.cacheSearchPathHit(request.normalized, moduleEntry{})
			engine.cacheSearchPathMiss(request.normalized, "suggestion")
			_ = engine.searchPathModuleSuggestion(request)
		})
	}
	close(start)
	wg.Wait()
	for _, err := range errs {
		if err != nil {
			t.Fatal(err)
		}
	}
	if engine.modRequestBytes != retainedModuleRequestBytes(engine) {
		t.Fatalf("duplicates charged %d bytes for %d retained bytes", engine.modRequestBytes, retainedModuleRequestBytes(engine))
	}
	for _, count := range []int{len(engine.modRequests), len(engine.modSearchHits), len(engine.modSearchMisses), len(engine.modSuggestText)} {
		if count != 1 {
			t.Fatalf("cache holds %d entries, want one", count)
		}
	}
}

func TestModuleRequestCacheConcurrentClear(t *testing.T) {
	t.Parallel()
	engine := moduleTestEngine(t)
	var wg sync.WaitGroup
	for i := range 8 {
		wg.Go(func() {
			for j := range 64 {
				request, err := engine.parseCachedModuleRequest(fmt.Sprintf("missing_%d_%d", i, j))
				if err != nil {
					t.Error(err)
					return
				}
				engine.cacheSearchPathMiss(request.normalized, "")
				if j%8 == 0 {
					engine.ClearModuleCache()
				}
			}
		})
	}
	wg.Wait()
	if engine.modRequestBytes != retainedModuleRequestBytes(engine) {
		t.Fatalf("clear races left %d accounted bytes for %d retained bytes", engine.modRequestBytes, retainedModuleRequestBytes(engine))
	}
}

func TestModuleRequestCacheModes(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		name    string
		config  Config
		entries int
	}{
		{"default", Config{}, 3},
		{"entry_limit", Config{MaxCachedModules: 1}, 1},
		{"disabled", Config{MaxCachedModules: -1}, 0},
		{"dev_mode", Config{DevMode: true}, 3},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			engine := MustNewEngine(tc.config)
			for i := range 3 {
				name := fmt.Sprintf("missing_%d.vibe", i) + strings.Repeat(" ", 4096)
				request, err := engine.parseCachedModuleRequest(name)
				if err != nil || request.raw != name {
					t.Fatalf("parse = %+v, %v", request, err)
				}
				assertModuleStringDetached(t, "normalized request", request.normalized, name)
				engine.cacheSearchPathMiss(request.normalized, "")
			}
			if len(engine.modRequests) != tc.entries {
				t.Fatalf("cached %d requests, want %d", len(engine.modRequests), tc.entries)
			}
			if tc.config.DevMode && len(engine.modSearchMisses) != 0 {
				t.Fatal("dev mode retained a filesystem miss")
			}
			if engine.modRequestBytes != retainedModuleRequestBytes(engine) {
				t.Fatal("cache accounting differs from retained text")
			}
		})
	}
}
