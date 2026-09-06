package runtime

import (
	"context"
	"fmt"
	"math"
	"os"
	"path/filepath"
	"runtime"
	"strconv"
	"strings"
	"sync"
	"testing"
)

const moduleFixturesRoot = "testdata/modules"

func moduleTestEngine(t testing.TB) *Engine {
	t.Helper()
	return MustNewEngine(Config{ModulePaths: []string{filepath.FromSlash(moduleFixturesRoot)}})
}

// requireBehaviorCase describes a "compile script, call function, assert"
// scenario against the shared module fixtures.
type requireBehaviorCase struct {
	name    string
	source  string
	fn      string
	args    []Value
	opts    CallOptions
	wantInt int64
	want    Value
	wantErr string
	// memoryQuota overrides the engine's default memory quota. Cases that pin
	// which limit fires (such as the recursion cap) set explicit headroom so
	// legitimate per-frame accounting growth cannot flip the outcome.
	memoryQuota int
	verify      func(t *testing.T, result Value)
}

// TestRequireBehavior aggregates the formerly individual TestRequire*
// scenarios that all share the same shape: build an engine pointed at
// testdata/modules, compile a script, call one function, and either
// assert a value or assert an error substring. Cases needing extra
// setup (file mutation, symlinks, cwd switching, cache assertions)
// remain as standalone tests below.
func TestRequireBehavior(t *testing.T) {
	t.Parallel()

	cases := []requireBehaviorCase{
		{
			name: "provides_exports",
			source: `def run(value)
  helpers = require("helper")
  helpers.triple(value) + double(value)
end`,
			fn:      "run",
			args:    []Value{NewInt(3)},
			wantInt: 15,
		},
		{
			name: "supports_module_alias",
			source: `def run(value)
  require("helper", as: "helpers")
  require("helper", as: "helpers")
  helpers.triple(value) + helpers.double(value)
end`,
			fn:      "run",
			args:    []Value{NewInt(3)},
			wantInt: 15,
		},
		{
			name: "alias_conflicts_with_global_function",
			source: `def helpers(value)
  value
end

def run()
  require("helper", as: "helpers")
end`,
			fn:      "run",
			wantErr: `require: alias "helpers" already defined`,
		},
		{
			name: "alias_conflict_does_not_leak_exports_when_rescued",
			source: `def helpers(value)
  value
end

def run(value)
  begin
    require("helper", as: "helpers")
  rescue
    nil
  end

  begin
    double(value)
  rescue
    "missing"
  end
end`,
			fn:   "run",
			args: []Value{NewInt(3)},
			want: NewString("missing"),
		},
		{
			name: "preserves_module_local_resolution",
			source: `def rate()
  100
end

def run(amount)
  fees = require("collision")
  fees.apply_fee(amount)
end`,
			fn:      "run",
			args:    []Value{NewInt(10)},
			wantInt: 11,
		},
		{
			name: "namespace_conflict_keeps_existing_global_binding",
			source: `def double(value)
  value + 1
end

def run(value)
  mod = require("helper")
  {
    global: double(value),
    module: mod.double(value)
  }
end`,
			fn:   "run",
			args: []Value{NewInt(3)},
			verify: func(t *testing.T, result Value) {
				t.Helper()
				if result.Kind() != KindHash {
					t.Fatalf("expected hash result, got %#v", result)
				}
				out := result.Hash()
				if !out["global"].Equal(NewInt(4)) {
					t.Fatalf("expected global binding to stay at 4, got %#v", out["global"])
				}
				if !out["module"].Equal(NewInt(6)) {
					t.Fatalf("expected module object function to return 6, got %#v", out["module"])
				}
			},
		},
		{
			name: "namespace_conflict_keeps_first_module_binding",
			source: `def run(value)
  first = require("helper")
  second = require("helper_alt")
  require("helper_alt", as: "alt")
  {
    global: double(value),
    first: first.double(value),
    second: second.double(value),
    alias: alt.double(value)
  }
end`,
			fn:   "run",
			args: []Value{NewInt(3)},
			verify: func(t *testing.T, result Value) {
				t.Helper()
				if result.Kind() != KindHash {
					t.Fatalf("expected hash result, got %#v", result)
				}
				out := result.Hash()
				expected := map[string]int64{"global": 6, "first": 6, "second": 30, "alias": 30}
				for key, want := range expected {
					if !out[key].Equal(NewInt(want)) {
						t.Fatalf("expected %s=%d, got %#v", key, want, out[key])
					}
				}
			},
		},
		{
			name: "cached_alias_conflict_checks_root_binding",
			source: `def run()
  alt = require("helper_alt")
  require("helper", as: "mod")
  use_alias(alt)
end

def use_alias(mod)
  require("helper_alt", as: "mod")
end`,
			fn:      "run",
			wantErr: `require: alias "mod" already defined`,
		},
		{
			name: "missing_module",
			source: `def run()
  require("missing")
end`,
			fn:      "run",
			wantErr: `module "missing" not found`,
		},
		{
			name: "rejects_path_traversal",
			source: `def run()
  require("nested/../../etc/passwd")
end`,
			fn:      "run",
			wantErr: "escapes search paths",
		},
		{
			name: "rejects_backslash_path_traversal",
			source: `def run()
  require("nested\\..\\..\\etc\\passwd")
end`,
			fn:      "run",
			wantErr: "escapes search paths",
		},
		{
			name: "relative_path_requires_module_caller",
			source: `def run()
  require("./helper")
end`,
			fn:      "run",
			wantErr: "requires a module caller",
		},
		{
			name: "relative_path_does_not_leak_from_module_into_host_function",
			source: `def host_relative()
  require("./helper")
end

def run()
  mod = require("module_calls_host")
  mod.invoke_host_relative()
end`,
			fn:      "run",
			wantErr: "requires a module caller",
		},
		{
			name: "supports_relative_paths_within_module_root",
			source: `def run(value)
  mod = require("relative/root")
  mod.run(value)
end`,
			fn:      "run",
			args:    []Value{NewInt(5)},
			wantInt: 16,
		},
		{
			name: "relative_path_rejects_escaping_module_root",
			source: `def run()
  mod = require("relative/escape")
  mod.run()
end`,
			fn:      "run",
			wantErr: "escapes module root",
		},
		{
			name: "relative_path_works_in_module_defined_block_yielded_from_host",
			source: `def host_each()
  yield()
end

def run()
  mod = require("block_host_yield")
  mod.run()
end`,
			fn:      "run",
			wantInt: 33,
		},
		{
			name: "exports_only_non_private_functions",
			source: `def run(value)
  mod = require("private_exports")
  if mod["_internal"] != nil
    0
  else
    visible(value) + mod.call_internal(value)
  end
end`,
			fn:      "run",
			args:    []Value{NewInt(2)},
			wantInt: 105,
		},
		{
			name: "supports_private_module_export_opt_out",
			source: `def run(value)
  mod = require("explicit_exports")
  {
    has_exposed: mod["exposed"] != nil,
    has_explicit_hidden: mod["_explicit_hidden"] != nil,
    has_helper: mod["helper"] != nil,
    has_internal: mod["_internal"] != nil,
    exposed: mod.exposed(value),
    explicit_hidden: mod._explicit_hidden(value)
  }
end`,
			fn:   "run",
			args: []Value{NewInt(3)},
			verify: func(t *testing.T, result Value) {
				t.Helper()
				if result.Kind() != KindHash {
					t.Fatalf("expected hash result, got %#v", result)
				}
				out := result.Hash()
				if !out["has_exposed"].Bool() || !out["has_explicit_hidden"].Bool() {
					t.Fatalf("expected non-private exports to be present, got %#v", out)
				}
				if out["has_helper"].Bool() || out["has_internal"].Bool() {
					t.Fatalf("expected private helpers to be hidden, got %#v", out)
				}
				if !out["exposed"].Equal(NewInt(10)) {
					t.Fatalf("expected exposed(3)=10, got %#v", out["exposed"])
				}
				if !out["explicit_hidden"].Equal(NewInt(103)) {
					t.Fatalf("expected _explicit_hidden(3)=103, got %#v", out["explicit_hidden"])
				}
			},
		},
		{
			name: "private_functions_are_not_injected_as_globals",
			source: `def run(value)
  require("explicit_exports")
  helper(value)
end`,
			fn:      "run",
			args:    []Value{NewInt(2)},
			wantErr: "undefined variable helper",
		},
		{
			name: "private_functions_remain_module_scoped",
			source: `def run(value)
  require("private_exports")
  _internal(value)
end`,
			fn:      "run",
			args:    []Value{NewInt(2)},
			wantErr: "undefined variable _internal",
		},
		{
			name: "module_cache_avoids_duplicate_loads",
			source: `def run()
  require("circular_a")
  require("circular_b")
  "ok"
end`,
			fn:   "run",
			want: NewString("ok"),
		},
		{
			name: "runtime_module_recursion_hits_recursion_limit",
			source: `def run()
  mod = require("circular_runtime_a")
  mod.enter()
end`,
			fn:      "run",
			wantErr: "recursion depth exceeded",
			// The intent is the recursion cap firing across module boundaries;
			// explicit quota headroom keeps the memory limit from racing it as
			// per-frame require accounting evolves.
			memoryQuota: 256 << 10,
		},
		{
			name: "allows_cached_module_reuse_across_module_calls",
			source: `def run()
  mod = require("require_cached_a")
  mod.start()
end`,
			fn:      "run",
			wantInt: 21,
		},
	}

	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			engine := moduleTestEngine(t)
			if tc.memoryQuota > 0 {
				engine = MustNewEngine(Config{ModulePaths: []string{filepath.FromSlash(moduleFixturesRoot)}, MemoryQuotaBytes: tc.memoryQuota})
			}
			script := compileScriptWithEngine(t, engine, tc.source)

			if tc.wantErr != "" {
				requireCallErrorContains(t, script, tc.fn, tc.args, tc.opts, tc.wantErr)
				return
			}

			result, err := script.Call(context.Background(), tc.fn, tc.args, tc.opts)
			if err != nil {
				t.Fatalf("call failed: %v", err)
			}
			switch {
			case tc.verify != nil:
				tc.verify(t, result)
			case tc.want.Kind() != KindNil:
				if !result.Equal(tc.want) {
					t.Fatalf("result mismatch: got %#v, want %#v", result, tc.want)
				}
			default:
				if !result.Equal(NewInt(tc.wantInt)) {
					t.Fatalf("expected %d, got %#v", tc.wantInt, result)
				}
			}
		})
	}
}

func TestRequireModuleWithMaxSourceBytesAtMaxInt(t *testing.T) {
	t.Parallel()

	root := tempModuleTree(t, moduleFile{
		path: "helper.vibe",
		content: `def value()
  42
end`,
	})
	engine := MustNewEngine(Config{ModulePaths: []string{root}, MaxSourceBytes: math.MaxInt})
	script := compileScriptWithEngine(t, engine, `def run()
  helper = require("helper")
  helper.value()
end`)

	result := callScript(t, context.Background(), script, "run", nil, CallOptions{})
	if !result.Equal(NewInt(42)) {
		t.Fatalf("require with MaxSourceBytes math.MaxInt = %#v, want 42", result)
	}
}

func TestRequireAliasValidation(t *testing.T) {
	t.Parallel()

	cases := []struct {
		name    string
		source  string
		wantErr string
	}{
		{
			name: "invalid identifier",
			source: `def run()
  require("helper", as: "123bad")
end`,
			wantErr: `require: invalid alias "123bad"`,
		},
		{
			name: "keyword alias",
			source: `def run()
  require("helper", as: "if")
end`,
			wantErr: `require: invalid alias "if"`,
		},
		{
			name: "invalid type",
			source: `def run()
  require("helper", as: 10)
end`,
			wantErr: "require: alias must be a string or symbol",
		},
		{
			name: "unknown keyword",
			source: `def run()
  require("helper", name: "helpers")
end`,
			wantErr: "require: unknown keyword argument name",
		},
	}

	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			engine := moduleTestEngine(t)
			script := compileScriptWithEngine(t, engine, tc.source)
			err := callScriptErr(t, context.Background(), script, "run", nil, CallOptions{})
			requireErrorContains(t, err, tc.wantErr)
		})
	}
}

func TestRequireCachesModules(t *testing.T) {
	t.Parallel()
	engine := moduleTestEngine(t)

	script := compileScriptWithEngine(t, engine, `def run()
  require("helper")
  require("helper")
  require("helper")
  double(10)
end`)

	result, err := script.Call(context.Background(), "run", nil, CallOptions{})
	if err != nil {
		t.Fatalf("call failed: %v", err)
	}
	if result.Kind() != KindInt || result.Int() != 20 {
		t.Fatalf("expected 20, got %#v", result)
	}

	if len(engine.modules) != 1 {
		t.Fatalf("expected 1 cached module, got %d", len(engine.modules))
	}
}

func TestClearModuleCacheForcesModuleReload(t *testing.T) {
	t.Parallel()
	root := tempModuleTree(t, moduleFile{
		path: "dynamic.vibe",
		content: `def value()
  1
end
`,
	})
	modulePath := filepath.Join(root, "dynamic.vibe")
	writeModule := func(v int) {
		content := fmt.Sprintf(`def value()
  %d
end
`, v)
		if err := os.WriteFile(modulePath, []byte(content), 0o644); err != nil {
			t.Fatalf("write module: %v", err)
		}
	}

	engine := mustNewEngineWithModuleRoot(t, root)
	script := compileScriptWithEngine(t, engine, `def run()
  mod = require("dynamic")
  mod.value()
end`)

	first, err := script.Call(context.Background(), "run", nil, CallOptions{})
	if err != nil {
		t.Fatalf("first call failed: %v", err)
	}
	if first.Kind() != KindInt || first.Int() != 1 {
		t.Fatalf("expected first value 1, got %#v", first)
	}

	writeModule(2)

	stale, err := script.Call(context.Background(), "run", nil, CallOptions{})
	if err != nil {
		t.Fatalf("stale call failed: %v", err)
	}
	if stale.Kind() != KindInt || stale.Int() != 1 {
		t.Fatalf("expected cached value 1 before cache clear, got %#v", stale)
	}

	if cleared := engine.ClearModuleCache(); cleared != 1 {
		t.Fatalf("expected 1 cleared module, got %d", cleared)
	}

	refreshed, err := script.Call(context.Background(), "run", nil, CallOptions{})
	if err != nil {
		t.Fatalf("refreshed call failed: %v", err)
	}
	if refreshed.Kind() != KindInt || refreshed.Int() != 2 {
		t.Fatalf("expected refreshed value 2 after cache clear, got %#v", refreshed)
	}
}

func TestRequireUsesModulePathResolvedAtEngineCreation(t *testing.T) {
	previousDir, err := os.Getwd()
	if err != nil {
		t.Fatalf("get cwd: %v", err)
	}
	defer func() {
		if err := os.Chdir(previousDir); err != nil {
			t.Fatalf("restore cwd: %v", err)
		}
	}()

	root := t.TempDir()
	safeCwd := filepath.Join(root, "safe")
	otherCwd := filepath.Join(root, "other")
	for _, dir := range []string{filepath.Join(safeCwd, "mods"), filepath.Join(otherCwd, "mods")} {
		if err := os.MkdirAll(dir, 0o755); err != nil {
			t.Fatalf("mkdir %s: %v", dir, err)
		}
	}
	if err := os.WriteFile(filepath.Join(safeCwd, "mods", "picked.vibe"), []byte("def value() 1 end\n"), 0o644); err != nil {
		t.Fatalf("write safe module: %v", err)
	}
	if err := os.WriteFile(filepath.Join(otherCwd, "mods", "picked.vibe"), []byte("def value() 2 end\n"), 0o644); err != nil {
		t.Fatalf("write other module: %v", err)
	}

	if err := os.Chdir(safeCwd); err != nil {
		t.Fatalf("chdir safe: %v", err)
	}
	engine := MustNewEngine(Config{ModulePaths: []string{"mods"}})
	if err := os.Chdir(otherCwd); err != nil {
		t.Fatalf("chdir other: %v", err)
	}

	script := compileScriptWithEngine(t, engine, `def run()
  mod = require("picked")
  mod.value()
end`)
	result, err := script.Call(context.Background(), "run", nil, CallOptions{})
	if err != nil {
		t.Fatalf("call failed: %v", err)
	}
	if !result.Equal(NewInt(1)) {
		t.Fatalf("require after cwd change = %#v, want 1 from original module root", result)
	}
}

func TestRequireUsesSymlinkTargetResolvedAtEngineCreation(t *testing.T) {
	t.Parallel()
	if runtime.GOOS == "windows" {
		t.Skip("symlink behavior is environment-specific on Windows")
	}

	root := t.TempDir()
	safeRoot := filepath.Join(root, "safe")
	otherRoot := filepath.Join(root, "other")
	for _, dir := range []string{safeRoot, otherRoot} {
		if err := os.MkdirAll(dir, 0o755); err != nil {
			t.Fatalf("mkdir %s: %v", dir, err)
		}
	}
	if err := os.WriteFile(filepath.Join(safeRoot, "picked.vibe"), []byte("def value() 1 end\n"), 0o644); err != nil {
		t.Fatalf("write safe module: %v", err)
	}
	if err := os.WriteFile(filepath.Join(otherRoot, "picked.vibe"), []byte("def value() 2 end\n"), 0o644); err != nil {
		t.Fatalf("write other module: %v", err)
	}

	linkRoot := filepath.Join(root, "mods")
	if err := os.Symlink(safeRoot, linkRoot); err != nil {
		t.Skipf("symlink unavailable: %v", err)
	}
	engine := mustNewEngineWithModuleRoot(t, linkRoot)
	if err := os.Remove(linkRoot); err != nil {
		t.Fatalf("remove module symlink: %v", err)
	}
	if err := os.Symlink(otherRoot, linkRoot); err != nil {
		t.Fatalf("retarget module symlink: %v", err)
	}

	script := compileScriptWithEngine(t, engine, `def run()
  mod = require("picked")
  mod.value()
end`)
	result, err := script.Call(context.Background(), "run", nil, CallOptions{})
	if err != nil {
		t.Fatalf("call failed: %v", err)
	}
	if !result.Equal(NewInt(1)) {
		t.Fatalf("require after symlink retarget = %#v, want 1 from original module root", result)
	}
}

func TestRequireSearchPathCachedSymlinkSurvivesRetargetUntilClear(t *testing.T) {
	t.Parallel()
	if runtime.GOOS == "windows" {
		t.Skip("symlink behavior is environment-specific on Windows")
	}

	root := t.TempDir()
	moduleRoot := filepath.Join(root, "mods")
	insideDir := filepath.Join(moduleRoot, "inside")
	outsideDir := filepath.Join(root, "outside")
	for _, dir := range []string{insideDir, outsideDir} {
		if err := os.MkdirAll(dir, 0o755); err != nil {
			t.Fatalf("os.MkdirAll(%q) error = %v", dir, err)
		}
	}
	insideModule := filepath.Join(insideDir, "picked.vibe")
	outsideModule := filepath.Join(outsideDir, "picked.vibe")
	if err := os.WriteFile(insideModule, []byte("def value() 1 end\n"), 0o644); err != nil {
		t.Fatalf("os.WriteFile(%q) error = %v", insideModule, err)
	}
	if err := os.WriteFile(outsideModule, []byte("def value() 2 end\n"), 0o644); err != nil {
		t.Fatalf("os.WriteFile(%q) error = %v", outsideModule, err)
	}

	link := filepath.Join(moduleRoot, "picked.vibe")
	if err := os.Symlink(insideModule, link); err != nil {
		t.Skipf("symlink unavailable: %v", err)
	}

	engine := mustNewEngineWithModuleRoot(t, moduleRoot)
	script := compileScriptWithEngine(t, engine, `def run()
  mod = require("picked")
  mod.value()
end`)

	result, err := script.Call(context.Background(), "run", nil, CallOptions{})
	if err != nil {
		t.Fatalf("first call error = %v", err)
	}
	if !result.Equal(NewInt(1)) {
		t.Fatalf("first call = %#v, want 1", result)
	}

	if err := os.Remove(link); err != nil {
		t.Fatalf("os.Remove(%q) error = %v", link, err)
	}
	if err := os.Symlink(outsideModule, link); err != nil {
		t.Fatalf("retarget module symlink error = %v", err)
	}

	result, err = script.Call(context.Background(), "run", nil, CallOptions{})
	if err != nil {
		t.Fatalf("cached call after symlink retarget error = %v", err)
	}
	if !result.Equal(NewInt(1)) {
		t.Fatalf("cached call after symlink retarget = %#v, want 1", result)
	}

	if cleared := engine.ClearModuleCache(); cleared != 1 {
		t.Fatalf("ClearModuleCache() = %d, want 1", cleared)
	}
	requireCallErrorContains(t, script, "run", nil, CallOptions{}, "escapes module root")
}

func TestRequireRejectsNonRegularModuleFile(t *testing.T) {
	t.Parallel()
	moduleRoot := t.TempDir()
	if err := os.Mkdir(filepath.Join(moduleRoot, "notfile.vibe"), 0o755); err != nil {
		t.Fatalf("mkdir module directory: %v", err)
	}

	engine := mustNewEngineWithModuleRoot(t, moduleRoot)
	script := compileScriptWithEngine(t, engine, `def run()
  require("notfile")
end`)

	requireCallErrorContains(t, script, "run", nil, CallOptions{}, "not a regular file")
}

func TestRequireRejectsAbsolutePaths(t *testing.T) {
	t.Parallel()
	engine := moduleTestEngine(t)

	absPath := "/etc/passwd"
	if filepath.Separator == '\\' {
		drive := os.Getenv("SYSTEMDRIVE")
		if drive == "" {
			drive = "C:"
		}
		drive = drive + string(filepath.Separator)
		absPath = filepath.Join(drive, "Windows", "system32")
	}

	source := fmt.Sprintf(`def run()
  require(%q)
end`, absPath)

	script := compileScriptWithEngine(t, engine, source)

	requireCallErrorContains(t, script, "run", nil, CallOptions{}, "must be relative")
}

func TestRequireNormalizesPathSeparators(t *testing.T) {
	t.Parallel()
	engine := moduleTestEngine(t)

	script := compileScriptWithEngine(t, engine, `def run(value)
  unix_style = require("shared/math")
  windows_style = require("shared\\math")
  unix_style.double(value) + windows_style.double(value)
end`)

	result, err := script.Call(context.Background(), "run", []Value{NewInt(3)}, CallOptions{})
	if err != nil {
		t.Fatalf("call failed: %v", err)
	}
	if result.Kind() != KindInt || result.Int() != 12 {
		t.Fatalf("expected 12, got %#v", result)
	}
	if len(engine.modules) != 1 {
		t.Fatalf("expected normalized requires to share cache entry, got %d modules", len(engine.modules))
	}
}

func TestRequireRelativePathRejectsSymlinkEscape(t *testing.T) {
	t.Parallel()
	if runtime.GOOS == "windows" {
		t.Skip("symlink behavior is environment-specific on Windows")
	}

	moduleRoot := tempModuleTree(t, moduleFile{
		path: "entry.vibe",
		content: `def run()
  require("./link/secret")
end
`,
	})
	outsideRoot := tempModuleTree(t, moduleFile{
		path: "secret.vibe",
		content: `def hidden()
  42
end
`,
	})

	symlinkPath := filepath.Join(moduleRoot, "link")
	if err := os.Symlink(outsideRoot, symlinkPath); err != nil {
		t.Skipf("symlink unavailable: %v", err)
	}

	engine := mustNewEngineWithModuleRoot(t, moduleRoot)
	script := compileScriptWithEngine(t, engine, `def run()
  mod = require("entry")
  mod.run()
end`)

	requireCallErrorContains(t, script, "run", nil, CallOptions{}, "escapes module root")
}

func TestRequireSearchPathRejectsSymlinkEscape(t *testing.T) {
	t.Parallel()
	if runtime.GOOS == "windows" {
		t.Skip("symlink behavior is environment-specific on Windows")
	}

	moduleRoot := t.TempDir()
	outsideRoot := tempModuleTree(t, moduleFile{
		path: "secret.vibe",
		content: `def hidden()
  42
end
`,
	})

	symlinkPath := filepath.Join(moduleRoot, "link")
	if err := os.Symlink(outsideRoot, symlinkPath); err != nil {
		t.Skipf("symlink unavailable: %v", err)
	}

	engine := mustNewEngineWithModuleRoot(t, moduleRoot)
	script := compileScriptWithEngine(t, engine, `def run()
  require("link/secret")
end`)

	requireCallErrorContains(t, script, "run", nil, CallOptions{}, "escapes module root")
}

func TestRequireRelativePathRejectsOutOfRootCachedModule(t *testing.T) {
	t.Parallel()
	if runtime.GOOS == "windows" {
		t.Skip("symlink behavior is environment-specific on Windows")
	}

	moduleRoot := tempModuleTree(t, moduleFile{
		path: "entry.vibe",
		content: `def run()
  dep = require("./link/secret")
  dep.value()
end
`,
	})
	outsideRoot := tempModuleTree(t, moduleFile{
		path: "secret.vibe",
		content: `def value()
  9
end
`,
	})

	symlinkPath := filepath.Join(moduleRoot, "link")
	if err := os.Symlink(outsideRoot, symlinkPath); err != nil {
		t.Skipf("symlink unavailable: %v", err)
	}

	engine := mustNewEngineWithModuleRoot(t, moduleRoot)
	script := compileScriptWithEngine(t, engine, `def run()
  require("link/secret")
  entry = require("entry")
  entry.run()
end`)

	requireCallErrorContains(t, script, "run", nil, CallOptions{}, "escapes module root")
}

func TestRequireRelativePathUsesCacheBeforeFilesystemResolution(t *testing.T) {
	t.Parallel()
	moduleRoot := tempModuleTree(t,
		moduleFile{
			path: "dep.vibe",
			content: `def value()
  7
end
`,
		},
		moduleFile{
			path: "entry.vibe",
			content: `def run()
  dep = require("./dep")
  dep.value()
end
`,
		},
	)

	engine := mustNewEngineWithModuleRoot(t, moduleRoot)
	script := compileScriptWithEngine(t, engine, `def run()
  mod = require("entry")
  mod.run()
end`)

	result, err := script.Call(context.Background(), "run", nil, CallOptions{})
	if err != nil {
		t.Fatalf("first call failed: %v", err)
	}
	if result.Kind() != KindInt || result.Int() != 7 {
		t.Fatalf("expected first call result 7, got %#v", result)
	}

	if err := os.Remove(filepath.Join(moduleRoot, "dep.vibe")); err != nil {
		t.Fatalf("remove dep module: %v", err)
	}

	result, err = script.Call(context.Background(), "run", nil, CallOptions{})
	if err != nil {
		t.Fatalf("second call failed: %v", err)
	}
	if result.Kind() != KindInt || result.Int() != 7 {
		t.Fatalf("expected second call result 7, got %#v", result)
	}
}

func TestRequireRelativeCachedSymlinkSurvivesRetargetUntilClear(t *testing.T) {
	t.Parallel()
	if runtime.GOOS == "windows" {
		t.Skip("symlink behavior is environment-specific on Windows")
	}

	root := t.TempDir()
	moduleRoot := filepath.Join(root, "mods")
	insideDir := filepath.Join(moduleRoot, "inside")
	outsideDir := filepath.Join(root, "outside")
	for _, dir := range []string{insideDir, outsideDir} {
		if err := os.MkdirAll(dir, 0o755); err != nil {
			t.Fatalf("os.MkdirAll(%q) error = %v", dir, err)
		}
	}
	insideModule := filepath.Join(insideDir, "dep.vibe")
	outsideModule := filepath.Join(outsideDir, "dep.vibe")
	if err := os.WriteFile(insideModule, []byte("def value() 7 end\n"), 0o644); err != nil {
		t.Fatalf("os.WriteFile(%q) error = %v", insideModule, err)
	}
	if err := os.WriteFile(outsideModule, []byte("def value() 9 end\n"), 0o644); err != nil {
		t.Fatalf("os.WriteFile(%q) error = %v", outsideModule, err)
	}
	entryPath := filepath.Join(moduleRoot, "entry.vibe")
	if err := os.WriteFile(entryPath, []byte(`def run()
  dep = require("./dep")
  dep.value()
end
`), 0o644); err != nil {
		t.Fatalf("os.WriteFile(%q) error = %v", entryPath, err)
	}

	link := filepath.Join(moduleRoot, "dep.vibe")
	if err := os.Symlink(insideModule, link); err != nil {
		t.Skipf("symlink unavailable: %v", err)
	}

	engine := mustNewEngineWithModuleRoot(t, moduleRoot)
	script := compileScriptWithEngine(t, engine, `def run()
  entry = require("entry")
  entry.run()
end`)

	result, err := script.Call(context.Background(), "run", nil, CallOptions{})
	if err != nil {
		t.Fatalf("first call error = %v", err)
	}
	if !result.Equal(NewInt(7)) {
		t.Fatalf("first call = %#v, want 7", result)
	}

	if err := os.Remove(link); err != nil {
		t.Fatalf("os.Remove(%q) error = %v", link, err)
	}
	if err := os.Symlink(outsideModule, link); err != nil {
		t.Fatalf("retarget module symlink error = %v", err)
	}

	result, err = script.Call(context.Background(), "run", nil, CallOptions{})
	if err != nil {
		t.Fatalf("cached call after symlink retarget error = %v", err)
	}
	if !result.Equal(NewInt(7)) {
		t.Fatalf("cached call after symlink retarget = %#v, want 7", result)
	}

	if cleared := engine.ClearModuleCache(); cleared != 2 {
		t.Fatalf("ClearModuleCache() = %d, want 2", cleared)
	}
	requireCallErrorContains(t, script, "run", nil, CallOptions{}, "escapes module root")
}

func TestExportKeywordValidation(t *testing.T) {
	t.Parallel()
	requireCompileErrorContainsDefault(t, `export helper`, "expected 'def'")

	requireCompileErrorContainsDefault(t, `class Example
  export def value()
    1
  end
end`, "export is only supported for top-level functions")

	requireCompileErrorContainsDefault(t, `def outer()
  if true
    export def nested()
      1
    end
  end
end`, "export is only supported for top-level functions")
}

func TestPrivateKeywordValidation(t *testing.T) {
	t.Parallel()
	requireCompileErrorContainsDefault(t, `private helper`, "expected 'def'")

	requireCompileErrorContainsDefault(t, `def outer()
  if true
    private def nested()
      1
    end
  end
end`, "private is only supported for top-level functions and class methods")

	requireCompileErrorContainsDefault(t, `private def self.value()
  1
end`, "private cannot be used with class methods")
}

func TestRequireConcurrentLoading(t *testing.T) {
	t.Parallel()
	engine := moduleTestEngine(t)

	script := compileScriptWithEngine(t, engine, `def run()
  require("helper")
  double(5)
end`)

	const goroutines = 10
	var (
		wg   sync.WaitGroup
		mu   sync.Mutex
		errs []error
	)
	recordErr := func(err error) {
		if err == nil {
			return
		}
		mu.Lock()
		defer mu.Unlock()
		errs = append(errs, err)
	}

	for range goroutines {
		wg.Go(func() {
			result, err := script.Call(context.Background(), "run", nil, CallOptions{})
			if err != nil {
				recordErr(err)
				return
			}
			if result.Kind() != KindInt || result.Int() != 10 {
				recordErr(fmt.Errorf("expected 10, got %#v", result))
				return
			}
		})
	}

	wg.Wait()
	if len(errs) > 0 {
		t.Fatalf("concurrent call failed: %v", errs[0])
	}

	if len(engine.modules) != 1 {
		t.Fatalf("expected 1 cached module after concurrent access, got %d", len(engine.modules))
	}
}

func TestRequireStrictEffectsRequiresAllowRequire(t *testing.T) {
	t.Parallel()
	engine := MustNewEngine(Config{
		StrictEffects: true,
		ModulePaths:   []string{filepath.Join("testdata", "modules")},
	})

	script := compileScriptWithEngine(t, engine, `def run()
  require("helper")
end`)

	_, err := script.Call(context.Background(), "run", nil, CallOptions{})
	if err == nil {
		t.Fatalf("expected strict effects require error")
	}
	if got := err.Error(); !strings.Contains(got, "strict effects: require is disabled") {
		t.Fatalf("unexpected error: %v", err)
	}
}

func TestRequireStrictEffectsAllowsRequireWhenOptedIn(t *testing.T) {
	t.Parallel()
	engine := MustNewEngine(Config{
		StrictEffects: true,
		ModulePaths:   []string{filepath.Join("testdata", "modules")},
	})

	script := compileScriptWithEngine(t, engine, `def run(v)
  helpers = require("helper")
  helpers.triple(v)
end`)

	result, err := script.Call(context.Background(), "run", []Value{NewInt(4)}, CallOptions{
		AllowRequire: true,
	})
	if err != nil {
		t.Fatalf("call failed: %v", err)
	}
	if result.Kind() != KindInt || result.Int() != 12 {
		t.Fatalf("expected 12, got %#v", result)
	}
}

func TestRequireRunsModuleTopLevelBody(t *testing.T) {
	t.Parallel()
	dir := tempModuleTree(t, moduleFile{path: "settings.vibe", content: `offset = 10

def add(value)
  value + offset
end
`})
	engine := mustNewEngineWithModuleRoot(t, dir)
	script := compileScriptWithEngine(t, engine, `def run(value)
  settings = require("settings")
  {
    sum: settings.add(value),
    exports: settings
  }
end`)

	result, err := script.Call(context.Background(), "run", []Value{NewInt(5)}, CallOptions{})
	if err != nil {
		t.Fatalf("call failed: %v", err)
	}
	out := result.Hash()
	if got := out["sum"]; got.Kind() != KindInt || got.Int() != 15 {
		t.Fatalf("expected module top-level offset to produce 15, got %#v", got)
	}
	if _, leaked := out["exports"].Hash()[moduleEntrypointFunction]; leaked {
		t.Fatalf("synthetic module entrypoint leaked in exports: %#v", out["exports"])
	}
}

// TestRequireExportedEqualityPredicateOverridesUniversal confirms a module that
// exports a function named eql? or equal? makes mod.eql?(...) invoke that export
// rather than the universal Object-level predicate. Module exports are collected
// into a NewObject(exports), so an exported `def eql?` is a callable namespace
// member that must shadow the universal predicate exactly as a class method
// override does; treating an object like a hash (whose stored entries are data)
// would make the export unreachable.
func TestRequireExportedEqualityPredicateOverridesUniversal(t *testing.T) {
	t.Parallel()
	dir := tempModuleTree(t, moduleFile{path: "eq.vibe", content: `def eql?(other)
  "module eql " + other
end

def equal?(other)
  "module equal " + other
end
`})
	engine := mustNewEngineWithModuleRoot(t, dir)
	script := compileScriptWithEngine(t, engine, `def run()
  mod = require("eq")
  [mod.eql?("a"), mod.equal?("b")]
end`)

	result, err := script.Call(context.Background(), "run", nil, CallOptions{})
	if err != nil {
		t.Fatalf("call failed: %v", err)
	}
	compareArrays(t, result, []Value{
		NewString("module eql a"),
		NewString("module equal b"),
	})
}

func TestRequireInitializesModuleClassBodiesInSourceOrder(t *testing.T) {
	t.Parallel()
	dir := tempModuleTree(t, moduleFile{path: "settings.vibe", content: `limit = 10

class Settings
  @@limit = limit

  def self.limit
    @@limit
  end
end

def limit
  Settings.limit
end
`})
	engine := mustNewEngineWithModuleRoot(t, dir)
	script := compileScriptWithEngine(t, engine, `def run
  settings = require("settings")
  settings.limit
end`)

	got := callScript(t, context.Background(), script, "run", nil, CallOptions{})
	if got.Kind() != KindInt || got.Int() != 10 {
		t.Fatalf("run() = %#v, want 10", got)
	}
}

func TestRequireKeepsModuleTopLevelAssignmentsLocal(t *testing.T) {
	t.Parallel()
	dir := tempModuleTree(t, moduleFile{path: "settings.vibe", content: `offset = 10
secret = "module-local"
[1].each do
  caller_only = 123
end

def add(value)
  value + offset
end
`})
	engine := mustNewEngineWithModuleRoot(t, dir)
	script := compileScriptWithEngine(t, engine, `def run(value)
  offset = 99
  caller_only = 77
  settings = require("settings")
  {
    sum: settings.add(value),
    caller_offset: offset,
    caller_only: caller_only
  }
end`)

	result, err := script.Call(context.Background(), "run", []Value{NewInt(5)}, CallOptions{})
	if err != nil {
		t.Fatalf("run(5) failed: %v", err)
	}
	out := result.Hash()
	if got := out["sum"]; got.Kind() != KindInt || got.Int() != 15 {
		t.Fatalf("run(5).sum = %#v, want 15", got)
	}
	if got := out["caller_offset"]; got.Kind() != KindInt || got.Int() != 99 {
		t.Fatalf("run(5).caller_offset = %#v, want 99", got)
	}
	if got := out["caller_only"]; got.Kind() != KindInt || got.Int() != 77 {
		t.Fatalf("run(5).caller_only = %#v, want 77", got)
	}

	leakProbe := compileScriptWithEngine(t, engine, `def run()
  require("settings")
  secret
end`)
	err = callScriptErr(t, context.Background(), leakProbe, "run", nil, CallOptions{})
	requireErrorContains(t, err, "undefined variable secret")
}

func TestRequireInitializesModuleClassBodiesWithModuleContext(t *testing.T) {
	t.Parallel()
	dir := tempModuleTree(t,
		moduleFile{path: "dep.vibe", content: `def value
  7
end
`},
		moduleFile{path: "entry.vibe", content: `class UsesDep
  dep = require("./dep")
  @@value = dep.value

  def self.value
    @@value
  end
end

def value
  UsesDep.value
end
`},
	)
	engine := mustNewEngineWithModuleRoot(t, dir)
	script := compileScriptWithEngine(t, engine, `def run()
  entry = require("entry")
  entry.value
end`)

	result, err := script.Call(context.Background(), "run", nil, CallOptions{})
	if err != nil {
		t.Fatalf("run() failed: %v", err)
	}
	if result.Kind() != KindInt || result.Int() != 7 {
		t.Fatalf("run() = %#v, want 7", result)
	}
}

func TestRequireAliasConflictSkipsModuleTopLevelBody(t *testing.T) {
	t.Parallel()
	dir := t.TempDir()
	if err := os.WriteFile(filepath.Join(dir, "conflict.vibe"), []byte(`raise("module initializer ran")

def value
  1
end
`), 0o644); err != nil {
		t.Fatalf("write module: %v", err)
	}
	engine := MustNewEngine(Config{ModulePaths: []string{dir}})
	script := compileScriptWithEngine(t, engine, `def run()
  helpers = "taken"
  require("conflict", as: "helpers")
end`)

	err := callScriptErr(t, context.Background(), script, "run", nil, CallOptions{})
	requireErrorContains(t, err, `require: alias "helpers" already defined`)
	if strings.Contains(err.Error(), "module initializer ran") {
		t.Fatalf("module initializer ran before alias validation: %v", err)
	}
}

func TestRequireModuleAllowList(t *testing.T) {
	t.Parallel()
	engine := MustNewEngine(Config{
		ModulePaths:     []string{filepath.Join("testdata", "modules")},
		ModuleAllowList: []string{"shared/*"},
	})

	script := compileScriptWithEngine(t, engine, `def run_allowed(value)
  mod = require("shared/math")
  mod.double(value)
end

def run_denied(value)
  mod = require("helper")
  mod.double(value)
end`)

	allowed, err := script.Call(context.Background(), "run_allowed", []Value{NewInt(3)}, CallOptions{})
	if err != nil {
		t.Fatalf("allowed call failed: %v", err)
	}
	if allowed.Kind() != KindInt || allowed.Int() != 6 {
		t.Fatalf("expected allowed result 6, got %#v", allowed)
	}

	requireCallErrorContains(t, script, "run_denied", []Value{NewInt(3)}, CallOptions{}, `require: module "helper" not allowed by policy`)
}

func TestRequireModuleAllowListStarMatchesNestedModules(t *testing.T) {
	t.Parallel()
	engine := MustNewEngine(Config{
		ModulePaths:     []string{filepath.Join("testdata", "modules")},
		ModuleAllowList: []string{"*"},
	})

	script := compileScriptWithEngine(t, engine, `def run(value)
  mod = require("shared/math")
  mod.double(value)
end`)

	result, err := script.Call(context.Background(), "run", []Value{NewInt(4)}, CallOptions{})
	if err != nil {
		t.Fatalf("call failed: %v", err)
	}
	if result.Kind() != KindInt || result.Int() != 8 {
		t.Fatalf("expected nested module call result 8, got %#v", result)
	}
}

func TestRequireModuleDenyListOverridesAllowList(t *testing.T) {
	t.Parallel()
	engine := MustNewEngine(Config{
		ModulePaths:     []string{filepath.Join("testdata", "modules")},
		ModuleAllowList: []string{"*"},
		ModuleDenyList:  []string{"helper"},
	})

	script := compileScriptWithEngine(t, engine, `def run()
  require("helper")
end`)

	requireCallErrorContains(t, script, "run", nil, CallOptions{}, `require: module "helper" denied by policy`)
}

func TestModulePolicyPatternValidation(t *testing.T) {
	t.Parallel()
	_, err := NewEngine(Config{
		ModulePaths:     []string{filepath.Join("testdata", "modules")},
		ModuleAllowList: []string{"[invalid"},
	})
	requireErrorContains(t, err, "invalid module allow-list pattern")
}

// Inputs that the module-policy normalization must reduce to a stable
// canonical form. Every entry below was either part of the original
// suite, written by hand to probe an edge case, or surfaced as a fuzz
// crasher.
var modulePolicyNormalizationInputs = []string{
	// original cases
	"0/ /",
	"./nested\\*.vibe",
	" shared/math.vibe ",
	"./nested\\tool.vibe",

	// dot-prefix files (no base name) — must NOT collapse to empty
	".vibe",
	".vibe.vibe",
	".vibe.vibe.vibe",
	"..vibe",
	"...vibe",
	"./.vibe",

	// dot-only basenames under a directory: stripping the .vibe leaves
	// "." or ".." which path.Clean would absorb into the parent — must
	// NOT collapse into the parent's name.
	"pkg/..vibe",
	"pkg/...vibe",
	"a/b/..vibe",

	// numeric base name (path.Clean does not touch dots inside it)
	"0",
	"0.vibe",
	"0.vibe.vibe",
	"0.vibe.vibe.vibe",

	// hidden ".vibe" exposed by per-segment whitespace trimming
	"0.vibe .vibe",
	"0.vibe\t.vibe",
	"0.vibe .vibe .vibe",
	"  helper .vibe .vibe  ",
	".vibe .vibe",
	".vibe  .vibe",
	".vibe\t.vibe",

	// whitespace surrounding
	"  helper  ",
	"  helper.vibe  ",
	"\thelper.vibe\t",
	"helper ",
	" .vibe ",

	// multiple ".vibe" extensions
	"helper.vibe",
	"helper.vibe.vibe",
	"helper.vibe.vibe.vibe",
	"helper" + strings.Repeat(".vibe", 32),

	// multi-segment paths — directory ".vibe" must not be collapsed
	"helper/foo",
	"helper/foo.vibe",
	"helper/foo.vibe.vibe",
	"helper.vibe/foo",
	"helper.vibe/foo.vibe",
	"helper.vibe/foo.vibe.vibe",
	"helper.vibe/.vibe",
	"helper/.vibe",
	"helper/.vibe.vibe",
	" nested / foo .vibe ",

	// glob patterns
	"*",
	"*.vibe",
	"*.vibe.vibe",
	"nested/*.vibe",

	// backslash paths
	"nested\\foo.vibe",
	"./nested\\*.vibe",

	// path normalization corners
	"foo/../bar.vibe",
	"foo///bar.vibe",
	"./helper.vibe",

	// edge cases that should normalize to empty (truly empty inputs)
	"",
	" ",
	"\t",
	".",
	"./",
	"./.",

	// fuzz crashers preserved verbatim
	"0.vibe .vibe",
}

// Every input must reach a fixed point under one application of the
// normalizer — calling it a second time must return the same value.
func TestModulePolicyNormalizationIsIdempotent(t *testing.T) {
	t.Parallel()
	for _, raw := range modulePolicyNormalizationInputs {
		t.Run("pattern:"+strconv.Quote(raw), func(t *testing.T) {
			t.Parallel()
			first := normalizeModulePolicyPattern(raw)
			second := normalizeModulePolicyPattern(first)
			if first != second {
				t.Errorf("normalizeModulePolicyPattern(%q) = %q; normalized again = %q (not idempotent)", raw, first, second)
			}
		})
		t.Run("module:"+strconv.Quote(raw), func(t *testing.T) {
			t.Parallel()
			first := normalizeModulePolicyModuleName(raw)
			second := normalizeModulePolicyModuleName(first)
			if first != second {
				t.Errorf("normalizeModulePolicyModuleName(%q) = %q; normalized again = %q (not idempotent)", raw, first, second)
			}
		})
	}
}

// Distinct spellings of the same logical module name must produce the
// same canonical form so pattern/module comparison via path.Match is
// consistent regardless of how the admin writes the pattern or how the
// user wrote the require argument.
func TestModulePolicyNormalizationCanonicalizesEquivalents(t *testing.T) {
	t.Parallel()
	groups := [][]string{
		{
			"helper",
			"helper.vibe",
			"./helper",
			"./helper.vibe",
			"  helper  ",
			" helper.vibe ",
		},
		{
			"nested/helper",
			"nested/helper.vibe",
			"./nested/helper.vibe",
			"nested\\helper.vibe",
			"nested///helper.vibe",
		},
		{
			"nested/*",
			"nested/*.vibe",
			"./nested/*.vibe",
		},
		{
			".vibe",
			"  .vibe  ",
		},
		{
			"",
			" ",
			"\t",
			".",
			"./",
		},
	}
	for _, group := range groups {
		t.Run(group[0], func(t *testing.T) {
			t.Parallel()
			want := normalizeModulePolicyPattern(group[0])
			for _, raw := range group[1:] {
				if got := normalizeModulePolicyPattern(raw); got != want {
					t.Errorf("normalizeModulePolicyPattern(%q) = %q, want %q (same canonical form as %q)", raw, got, want, group[0])
				}
			}
		})
	}
}

// Directory components must not be treated as carrying a ".vibe"
// extension to strip — that would silently let pattern "helper" match
// modules anywhere under a "helper.vibe/" directory.
func TestModulePolicyNormalizationPreservesDirectoryDots(t *testing.T) {
	t.Parallel()
	cases := []struct {
		raw  string
		want string
	}{
		{"helper.vibe/foo", "helper.vibe/foo"},
		{"helper.vibe/foo.vibe", "helper.vibe/foo"},
		{"helper.vibe/foo.vibe.vibe", "helper.vibe/foo.vibe.vibe"},
		{"a.vibe/b.vibe/c.vibe", "a.vibe/b.vibe/c"},
	}
	for _, c := range cases {
		if got := normalizeModulePolicyPattern(c.raw); got != c.want {
			t.Errorf("normalizeModulePolicyPattern(%q) = %q, want %q", c.raw, got, c.want)
		}
	}
}

// Inputs whose basename is exactly ".vibe" (or any string that would
// empty after stripping) must be preserved, not collapsed — collapsing
// to "" used to bypass policy entirely in enforceModulePolicy.
func TestModulePolicyNormalizationDoesNotCollapseToEmpty(t *testing.T) {
	t.Parallel()
	preserveNonEmpty := []string{
		".vibe",
		".vibe.vibe",
		".vibe.vibe.vibe",
		"helper/.vibe",
		"helper.vibe/.vibe",
		"nested/dir/.vibe",
	}
	for _, raw := range preserveNonEmpty {
		if got := normalizeModulePolicyPattern(raw); got == "" {
			t.Errorf("normalizeModulePolicyPattern(%q) = \"\", want non-empty (would otherwise bypass policy)", raw)
		}
	}
}

// parseModuleRequest only appends ".vibe" to require arguments that
// have no extension, so "helper" and "helper.vibe" load the same file
// while "helper.vibe.vibe" loads a separate on-disk file. Policy
// normalization must reflect that, otherwise an allow-list of
// ["helper"] would also grant access to the sibling file
// "helper.vibe.vibe".
func TestModulePolicyNormalizationDistinguishesDoubleExtensions(t *testing.T) {
	t.Parallel()
	pairs := []struct{ a, b string }{
		{"helper", "helper.vibe.vibe"},
		{"helper.vibe", "helper.vibe.vibe"},
		{"helper.vibe.vibe", "helper.vibe.vibe.vibe"},
		{".vibe", ".vibe.vibe"},
		{".vibe.vibe", ".vibe.vibe.vibe"},
		{"helper/foo", "helper/foo.vibe.vibe"},
		{"helper.vibe/foo", "helper.vibe/foo.vibe.vibe"},
		{"helper/.vibe", "helper/.vibe.vibe"},
		// Dot-only basenames must not collapse via path.Clean into the
		// parent directory: "pkg/..vibe" is a literal file under pkg/,
		// distinct from "pkg" (which loads "pkg.vibe").
		{"pkg", "pkg/..vibe"},
		{"pkg", "pkg/...vibe"},
		{"helper", "helper/..vibe"},
	}
	for _, p := range pairs {
		gotA := normalizeModulePolicyPattern(p.a)
		gotB := normalizeModulePolicyPattern(p.b)
		if gotA == gotB {
			t.Errorf("normalizeModulePolicyPattern(%q) == normalizeModulePolicyPattern(%q) = %q; want distinct canonical forms (loader resolves them to different files)", p.a, p.b, gotA)
		}
	}
}

// Inputs whose policy normalization collapses to empty must not silently
// bypass deny/allow checks. Previously enforceModulePolicy short-circuited
// to allow when the module name was empty, so any require argument that
// normalized to "" (e.g. " ", whitespace-only paths) skipped policy.
func TestEnforceModulePolicyDeniesEmptyModuleWhenPolicyConfigured(t *testing.T) {
	t.Parallel()
	cases := []struct {
		name string
		cfg  Config
	}{
		{"deny-list set", Config{ModuleDenyList: []string{"*"}}},
		{"allow-list set", Config{ModuleAllowList: []string{"*"}}},
		{"both set", Config{ModuleAllowList: []string{"*"}, ModuleDenyList: []string{"x"}}},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			engine, err := NewEngine(tc.cfg)
			if err != nil {
				t.Fatalf("NewEngine failed: %v", err)
			}
			for _, raw := range []string{"", "   ", ".", "./"} {
				if err := engine.enforceModulePolicy(raw); err == nil {
					t.Errorf("enforceModulePolicy(%q) = nil, want denial", raw)
				}
			}
		})
	}

	t.Run("no policy configured allows empty", func(t *testing.T) {
		t.Parallel()
		engine := MustNewEngine(Config{})
		if err := engine.enforceModulePolicy(""); err != nil {
			t.Errorf("enforceModulePolicy(\"\") with no policy = %v, want nil", err)
		}
	})
}

func TestFormatModuleCycleUsesConciseChain(t *testing.T) {
	t.Parallel()
	root := filepath.Join("tmp", "modules")
	a := moduleCacheKey(root, filepath.Join("nested", "a.vibe"))
	b := moduleCacheKey(root, filepath.Join("nested", "b.vibe"))

	got := formatModuleCycle([]string{a, b, b, a})
	want := filepath.ToSlash(filepath.Join("nested", "a")) + " -> " + filepath.ToSlash(filepath.Join("nested", "b")) + " -> " + filepath.ToSlash(filepath.Join("nested", "a"))
	if got != want {
		t.Fatalf("expected cycle %q, got %q", want, got)
	}
}

func TestModuleCycleFromExecutionUsesConciseChain(t *testing.T) {
	t.Parallel()
	root := filepath.Join("tmp", "modules")
	a := moduleCacheKey(root, "a.vibe")
	b := moduleCacheKey(root, "b.vibe")

	cycle, ok := moduleCycleFromExecution([]moduleContext{
		{key: a},
		{key: a},
		{key: b},
	}, a)
	if !ok {
		t.Fatal("moduleCycleFromExecution did not find cycle")
	}
	want := []string{a, b, a}
	if len(cycle) != len(want) {
		t.Fatalf("cycle length = %d, want %d: %#v", len(cycle), len(want), cycle)
	}
	for i := range want {
		if cycle[i] != want[i] {
			t.Fatalf("cycle[%d] = %q, want %q in %#v", i, cycle[i], want[i], cycle)
		}
	}

	if _, ok := moduleCycleFromExecution([]moduleContext{{key: a}}, a); ok {
		t.Fatal("moduleCycleFromExecution found cycle for single current module")
	}
	if _, ok := moduleCycleFromExecution([]moduleContext{{key: a}, {key: b}}, b); ok {
		t.Fatal("moduleCycleFromExecution found cycle when next is only current module")
	}
}

func TestModuleDisplayNameTrimsExtension(t *testing.T) {
	t.Parallel()
	key := moduleCacheKey(filepath.Join("tmp", "modules"), filepath.Join("pkg", "helper.vibe"))
	got := moduleDisplayName(key)
	want := filepath.ToSlash(filepath.Join("pkg", "helper"))
	if got != want {
		t.Fatalf("expected display %q, got %q", want, got)
	}
}
