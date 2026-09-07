package runtime

import (
	"context"
	"strings"
	"testing"
)

func TestCheckRequirePermission(t *testing.T) {
	t.Parallel()
	checks := []struct {
		name string
		run  func(*Script, CallOptions) []CheckWarning
	}{
		{"script", func(s *Script, opts CallOptions) []CheckWarning { return s.CheckWarningsWithOptions(opts) }},
		{"function", func(s *Script, opts CallOptions) []CheckWarning {
			return s.CheckWarningsForFunctionWithOptions("run", opts)
		}},
		{"call", func(s *Script, opts CallOptions) []CheckWarning { return s.CheckWarningsForCall("run", nil, opts) }},
	}
	for _, check := range checks {
		for _, form := range []string{`require("helper")`, `require(:helper).answer()`, "helper = require(\"helper\")\nhelper.answer()"} {
			for _, policy := range []struct {
				name   string
				strict bool
				allow  bool
			}{
				{name: "denied", strict: true},
				{name: "allowed", strict: true, allow: true},
				{name: "non_strict"},
			} {
				t.Run(check.name+"/"+form+"/"+policy.name, func(t *testing.T) {
					t.Parallel()
					dir := tempModuleTree(t,
						moduleFile{path: "helper.vibe", content: "require(\"./child\")\ndef answer()\n42\nend\n"},
						moduleFile{path: "child.vibe", content: "def bad(v: int = \"wrong\")\nv\nend\n"},
					)
					engine := MustNewEngine(Config{StrictEffects: policy.strict, ModulePaths: []string{dir}})
					script := compileScriptWithEngine(t, engine, "def run()\n"+form+"\nend\n")
					warnings := check.run(script, CallOptions{AllowRequire: policy.allow})
					if policy.strict && !policy.allow {
						if len(engine.modules) != 0 || len(engine.modRequests) != 0 {
							t.Errorf("denied check cached %d modules and %d requests", len(engine.modules), len(engine.modRequests))
						}
						for _, warning := range warnings {
							if warning.Source != "" || strings.Contains(warning.Message, "default value for v") {
								t.Errorf("denied check inspected a module: %+v", warning)
							}
						}
						return
					}
					if len(engine.modules) != 2 {
						t.Errorf("authorized check cached %d modules, want helper and child", len(engine.modules))
					}
					found := false
					for _, warning := range warnings {
						found = found || strings.Contains(warning.Message, "default value for v expected int, got string")
					}
					if !found {
						t.Errorf("authorized check missed child-module diagnostic: %+v", warnings)
					}
				})
			}
		}
	}
}

func TestDefaultChecksDoNotLoadUnauthorizedModules(t *testing.T) {
	t.Parallel()
	for _, check := range []struct {
		name string
		run  func(*Script) []CheckWarning
	}{
		{"script", (*Script).CheckWarnings},
		{"function", func(s *Script) []CheckWarning { return s.CheckWarningsForFunction("run") }},
		{"order_independent", (*Script).CheckOrderIndependentWarnings},
	} {
		t.Run(check.name, func(t *testing.T) {
			t.Parallel()
			dir := tempModuleTree(t, moduleFile{path: "helper.vibe", content: "def answer()\n42\nend\n"})
			engine := MustNewEngine(Config{StrictEffects: true, ModulePaths: []string{dir}})
			script := compileScriptWithEngine(t, engine, "def run()\nrequire(\"helper\")\nend\n")
			check.run(script)
			if len(engine.modules) != 0 || len(engine.modRequests) != 0 {
				t.Errorf("default check cached %d modules and %d requests", len(engine.modules), len(engine.modRequests))
			}
		})
	}
}

func TestCheckedCallRequirePermission(t *testing.T) {
	t.Parallel()
	dir := tempModuleTree(t, moduleFile{path: "helper.vibe", content: "def answer()\n42\nend\n"})
	engine := MustNewEngine(Config{StrictEffects: true, ModulePaths: []string{dir}})
	script := compileScriptWithEngine(t, engine, "def run()\nrequire(\"helper\").answer()\nend\n")
	for _, allow := range []bool{false, true, false} {
		before := len(engine.modules)
		got, warnings, err := script.CheckedCall(context.Background(), "run", nil, CallOptions{AllowRequire: allow})
		if len(warnings) != 0 {
			t.Fatalf("allow=%v warnings=%+v, want runtime permission handling", allow, warnings)
		}
		if allow {
			if err != nil || got.Int() != 42 {
				t.Fatalf("authorized result=%v error=%v, want 42", got, err)
			}
		} else {
			if err == nil || !strings.Contains(err.Error(), "strict effects: require is disabled") {
				t.Errorf("denied call error=%v, want existing require permission error", err)
			}
			if len(engine.modules) != before {
				t.Errorf("denied call changed cached modules from %d to %d", before, len(engine.modules))
			}
		}
	}
}

func TestDeniedCheckDoesNotInspectCachedModuleContracts(t *testing.T) {
	t.Parallel()
	for _, devMode := range []bool{false, true} {
		dir := tempModuleTree(t, moduleFile{path: "helper.vibe", content: "def answer(v: int = \"wrong\")\nv\nend\n"})
		engine := MustNewEngine(Config{StrictEffects: true, DevMode: devMode, ModulePaths: []string{dir}})
		script := compileScriptWithEngine(t, engine, "def run()\nrequire(\"helper\")\nend\n")
		if warnings := script.CheckWarningsWithOptions(CallOptions{AllowRequire: true}); len(warnings) == 0 || len(engine.modules) != 1 {
			t.Fatalf("authorized check did not cache and inspect module: %+v", warnings)
		}
		if warnings := script.CheckWarnings(); len(warnings) != 0 {
			t.Errorf("devMode=%v denied check exposed cached module contracts: %+v", devMode, warnings)
		}
	}
}

func TestDeniedCheckNameFactsDoesNotLoadModules(t *testing.T) {
	t.Parallel()
	dir := tempModuleTree(t, moduleFile{path: "helper.vibe", content: "def answer()\n42\nend\n"})
	engine := MustNewEngine(Config{StrictEffects: true, ModulePaths: []string{dir}})
	script := compileScriptWithEngine(t, engine, "def other()\nrequire(\"helper\")\nend\ndef run()\nunknown_name\nend\n")
	script.CheckWarningsForFunction("run")
	if len(engine.modules) != 0 || len(engine.modRequests) != 0 {
		t.Errorf("name facts cached %d modules and %d requests", len(engine.modules), len(engine.modRequests))
	}
}
