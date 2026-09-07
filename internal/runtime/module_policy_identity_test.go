package runtime

import (
	"context"
	"os"
	"path/filepath"
	"runtime"
	"strings"
	"testing"
)

func TestRequireModulePolicyDistinguishesFileNames(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		name    string
		allowed string
		other   string
	}{
		{"space_before_extension", "allowed.vibe", "allowed .vibe"},
		{"leading_basename_space", "nested/allowed.vibe", "nested/ allowed.vibe"},
		{"directory_space", "nested/allowed.vibe", "nested /allowed.vibe"},
		{"leading_path_space", "allowed.vibe", " allowed.vibe"},
		{"trailing_path_space", "allowed.vibe", "allowed.vibe "},
		{"additional_extension", "data.json", "data.json.vibe"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			if runtime.GOOS == "windows" && tc.name == "directory_space" {
				t.Skip("Win32 cannot create this distinct trailing-space directory fixture")
			}
			root := tempModuleTree(t,
				moduleFile{path: tc.allowed, content: "def value\n  7\nend\n"},
				moduleFile{path: tc.other, content: "def value\n  99\nend\n"},
			)
			allowedInfo, err := os.Stat(filepath.Join(root, tc.allowed))
			if err != nil {
				t.Fatal(err)
			}
			otherInfo, err := os.Stat(filepath.Join(root, tc.other))
			if err != nil {
				t.Fatal(err)
			}
			if os.SameFile(allowedInfo, otherInfo) {
				t.Skip("filesystem does not distinguish these filenames")
			}
			engine := MustNewEngine(Config{ModulePaths: []string{root}, ModuleAllowList: []string{tc.allowed}})
			script := compileScriptWithEngine(t, engine, "def run(name)\n  mod = require(name)\n  mod.value\nend")
			result, err := script.Call(context.Background(), "run", []Value{NewString(tc.allowed)}, CallOptions{})
			if err != nil || result.Int() != 7 {
				t.Fatalf("allowed module = %v, %v; want 7", result, err)
			}
			request := tc.other
			if strings.TrimSpace(request) != request {
				request = "placeholder/../" + request + "/."
			}
			_, err = script.Call(context.Background(), "run", []Value{NewString(request)}, CallOptions{})
			if err == nil || !strings.Contains(err.Error(), "not allowed by policy") {
				t.Fatalf("distinct file %q error = %v, want policy rejection", tc.other, err)
			}
			otherEngine := MustNewEngine(Config{ModulePaths: []string{root}, ModuleAllowList: []string{"./" + tc.other + "/."}})
			otherScript := compileScriptWithEngine(t, otherEngine, "def run(name)\n  mod = require(name)\n  mod.value\nend")
			result, err = otherScript.Call(context.Background(), "run", []Value{NewString(request)}, CallOptions{})
			if err != nil || result.Int() != 99 {
				t.Fatalf("explicitly allowed distinct file = %v, %v; want 99", result, err)
			}
		})
	}
}

func TestModulePolicyNormalizationPreservesSignificantWhitespace(t *testing.T) {
	t.Parallel()
	for _, file := range []string{
		"allowed .vibe", "nested /allowed.vibe", "nested/ allowed.vibe",
		" allowed.vibe", "allowed.vibe ", "data.json.vibe",
	} {
		module := normalizeModulePolicyModuleName(file)
		pattern := normalizeModulePolicyPattern("./" + file + "/.")
		if !modulePolicyMatch(pattern, module) {
			t.Errorf("literal filename %q: pattern %q does not match module %q", file, pattern, module)
		}
		if got := normalizeModulePolicyPattern(pattern); got != pattern {
			t.Errorf("pattern %q normalized again to %q", pattern, got)
		}
		if got := normalizeModulePolicyModuleName(module); got != module {
			t.Errorf("module %q normalized again to %q", module, got)
		}
	}
}

func TestModulePolicyRejectsAmbiguousEdgeWhitespace(t *testing.T) {
	t.Parallel()
	for _, pattern := range []string{" secret.vibe", "secret.vibe ", " secret ", "\tsecret.vibe\t", " * "} {
		for _, allow := range []bool{false, true} {
			cfg := Config{ModuleDenyList: []string{pattern}}
			if allow {
				cfg = Config{ModuleAllowList: []string{pattern}}
			}
			if _, err := NewEngine(cfg); err == nil || !strings.Contains(err.Error(), "whitespace") {
				t.Errorf("ambiguous pattern %q (allow=%t) error = %v", pattern, allow, err)
			}
		}
	}
}

func TestModulePolicyWhitespaceNamesKeepOptionalExtension(t *testing.T) {
	t.Parallel()
	for _, name := range []string{" space.vibe", "nested/ space.vibe", "space .vibe", "nested/space .vibe"} {
		base := strings.TrimSuffix(name, ".vibe")
		sibling := filepath.ToSlash(filepath.Join(filepath.Dir(name), strings.TrimSpace(filepath.Base(base))+".vibe"))
		root := tempModuleTree(t,
			moduleFile{path: name, content: "def value\n  7\nend\n"},
			moduleFile{path: sibling, content: "def value\n  9\nend\n"},
		)
		engine := MustNewEngine(Config{ModulePaths: []string{root}, ModuleAllowList: []string{"./" + base + "/."}})
		script := compileScriptWithEngine(t, engine, "def run(name)\n  require(name).value\nend")
		denyEngine := MustNewEngine(Config{ModulePaths: []string{root}, ModuleDenyList: []string{"./" + base + "/."}})
		denyScript := compileScriptWithEngine(t, denyEngine, "def run(name)\n  require(name).value\nend")
		for _, request := range []string{name, base} {
			got, err := script.Call(context.Background(), "run", []Value{NewString("placeholder/../" + request + "/.")}, CallOptions{})
			if err != nil || got.Int() != 7 {
				t.Errorf("whitespace filename %q, request %q = %v, %v", name, request, got, err)
			}
			_, err = denyScript.Call(context.Background(), "run", []Value{NewString("placeholder/../" + request + "/.")}, CallOptions{})
			if err == nil || !strings.Contains(err.Error(), "denied by policy") {
				t.Errorf("explicit whitespace deny pattern allowed %q: %v", request, err)
			}
		}
		got, err := denyScript.Call(context.Background(), "run", []Value{NewString(sibling)}, CallOptions{})
		if err != nil || got.Int() != 9 {
			t.Errorf("distinct whitespace-free sibling %q = %v, %v", sibling, got, err)
		}
	}
}
