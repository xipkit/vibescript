package runtime

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func TestModulePolicyRejectsBeforeFilesystemInspection(t *testing.T) {
	t.Parallel()
	for _, allowList := range []bool{false, true} {
		for _, relative := range []bool{false, true} {
			for _, devMode := range []bool{false, true} {
				dir := tempModuleTree(t,
					moduleFile{path: "public.vibe", content: "def answer()\n42\nend\n"},
					moduleFile{path: "private_regular.vibe", content: "def answer()\n42\nend\n"},
					moduleFile{path: "private_large.vibe", content: strings.Repeat(" ", 128)},
					moduleFile{path: "private_unreadable.vibe", content: "def answer()\n42\nend\n"},
				)
				if err := os.Mkdir(filepath.Join(dir, "private_directory.vibe"), 0o755); err != nil {
					t.Fatal(err)
				}
				unreadable := filepath.Join(dir, "private_unreadable.vibe")
				if err := os.Chmod(unreadable, 0); err != nil {
					t.Fatal(err)
				}
				t.Cleanup(func() {
					if err := os.Chmod(unreadable, 0o600); err != nil {
						t.Error(err)
					}
				})
				cfg := Config{ModulePaths: []string{dir}, MaxSourceBytes: 64, DevMode: devMode}
				want := "denied by policy"
				if allowList {
					cfg.ModuleAllowList = []string{"public"}
					want = "not allowed by policy"
				} else {
					cfg.ModuleDenyList = []string{"private_*"}
				}
				engine := MustNewEngine(cfg)
				var caller *moduleContext
				prefix := ""
				if relative {
					caller = &moduleContext{root: engine.modPaths[0], path: filepath.Join(engine.modPaths[0], "caller.vibe")}
					prefix = "./"
				}
				for _, name := range []string{"private_missing", "private_regular", "private_large", "private_unreadable", "private_directory"} {
					work := &moduleNameWork{}
					_, err := engine.loadModule(prefix+name, caller, nil, work)
					if err == nil || !strings.Contains(err.Error(), want) || work.used != 0 {
						t.Errorf("allowList=%v relative=%v devMode=%v name=%s: error=%v work=%d, want policy denial before inspection", allowList, relative, devMode, name, err, work.used)
					}
				}
				work := &moduleNameWork{}
				if _, err := engine.loadModule(prefix+"public", caller, nil, work); err != nil || work.used == 0 {
					t.Errorf("authorized module error=%v work=%d, want successful filesystem lookup", err, work.used)
				}
				if len(engine.modSearchMisses) != 0 || len(engine.modSuggest) != 0 || len(engine.modSuggestText) != 0 {
					t.Error("denied names populated filesystem-derived miss or suggestion caches")
				}
			}
		}
	}
}

func TestDeniedModulePolicyPrecedesSymlinkInspection(t *testing.T) {
	t.Parallel()
	dir := t.TempDir()
	outside := tempModuleTree(t, moduleFile{path: "target.vibe", content: "def answer()\n42\nend\n"})
	if err := os.Symlink(outside, filepath.Join(dir, "private")); err != nil {
		t.Skipf("symlinks unavailable: %v", err)
	}
	engine := MustNewEngine(Config{ModulePaths: []string{dir}, ModuleDenyList: []string{"private/*"}})
	caller := &moduleContext{root: engine.modPaths[0], path: filepath.Join(engine.modPaths[0], "caller.vibe")}
	for _, name := range []string{"private/target", "private/missing"} {
		for _, relative := range []bool{false, true} {
			var from *moduleContext
			request := name
			if relative {
				from = caller
				request = "./" + name
			}
			_, err := engine.loadModule(request, from, nil, nil)
			if err == nil || !strings.Contains(err.Error(), "denied by policy") {
				t.Errorf("%s error=%v, want policy denial before symlink traversal", request, err)
			}
		}
	}
}

func TestModulePolicyUsesNormalizedCallerRelativeName(t *testing.T) {
	t.Parallel()
	dir := t.TempDir()
	engine := MustNewEngine(Config{ModulePaths: []string{dir}, ModuleDenyList: []string{"private", "nested/private"}})
	caller := &moduleContext{root: engine.modPaths[0], path: filepath.Join(engine.modPaths[0], "nested", "caller.vibe")}
	for _, name := range []string{"./private", "./private.vibe", "./placeholder/../private", "../private", `..\private.vibe`} {
		work := &moduleNameWork{}
		_, err := engine.loadModule(name, caller, nil, work)
		if err == nil || !strings.Contains(err.Error(), "denied by policy") || work.used != 0 {
			t.Errorf("%s error=%v work=%d, want root-relative policy denial before inspection", name, err, work.used)
		}
	}
}
