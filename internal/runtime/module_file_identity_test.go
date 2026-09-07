package runtime

import (
	"context"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func TestRequireRejectsFilesystemAliases(t *testing.T) {
	t.Parallel()
	root := tempModuleTree(t,
		moduleFile{path: "ExactDir/ExactFile.vibe", content: "def value\n  7\nend\n"},
		moduleFile{path: "é.vibe", content: "def value\n  9\nend\n"},
	)
	for _, dev := range []bool{false, true} {
		for _, cacheLimit := range []int{0, 1} {
			for _, symbol := range []bool{false, true} {
				t.Run(fmt.Sprintf("dev=%t/cache=%d/symbol=%t", dev, cacheLimit, symbol), func(t *testing.T) {
					t.Parallel()
					engine := MustNewEngine(Config{ModulePaths: []string{root}, DevMode: dev, MaxCachedModules: cacheLimit})
					script := compileScriptWithEngine(t, engine, "def run(name)\n  require(name).value\nend")
					for _, name := range []string{
						"EXACTDIR/ExactFile.vibe", "ExactDir/EXACTFILE.vibe", "ExactDir/ExactFile.VIBE", "e\u0301.vibe",
					} {
						arg := NewString(name)
						if symbol {
							arg = NewSymbol(name)
						}
						if _, err := script.Call(context.Background(), "run", []Value{arg}, CallOptions{}); err == nil {
							t.Fatalf("filesystem alias %q loaded", name)
						}
						if count := engine.ClearModuleCache(); count != 0 {
							t.Fatalf("rejected alias cached %d compiled modules", count)
						}
					}
					for _, name := range []string{"ExactDir/ExactFile", "ExactDir/ExactFile.vibe"} {
						got, err := script.Call(context.Background(), "run", []Value{NewString(name)}, CallOptions{})
						if err != nil || got.Int() != 7 {
							t.Fatalf("exact name %q = %v, %v", name, got, err)
						}
					}
					if count := engine.ClearModuleCache(); count != 1 {
						t.Fatalf("extension aliases cached %d modules, want 1", count)
					}
				})
			}
		}
	}
}

func TestRequireFilesystemAliasesCannotBypassDenyList(t *testing.T) {
	t.Parallel()
	root := tempModuleTree(t,
		moduleFile{path: "ExactDir/ExactFile.vibe", content: "def value\n  7\nend\n"},
		moduleFile{path: "ExactDir/Driver.vibe", content: "def load(name)\n  require(name).value\nend\n"},
	)
	for _, relative := range []bool{false, true} {
		t.Run(fmt.Sprintf("relative=%t", relative), func(t *testing.T) {
			t.Parallel()
			engine := MustNewEngine(Config{ModulePaths: []string{root}, ModuleDenyList: []string{"ExactDir/ExactFile"}})
			source := "def run(name)\n  require(name).value\nend"
			names := []string{"ExactDir/ExactFile", "ExactDir/EXACTFILE.vibe", "EXACTDIR/ExactFile.vibe"}
			if relative {
				source = "def run(name)\n  require(\"ExactDir/Driver\").load(name)\nend"
				names = []string{"./ExactFile", "./EXACTFILE.vibe", "../EXACTDIR/ExactFile.vibe"}
			}
			script := compileScriptWithEngine(t, engine, source)
			for i, name := range names {
				_, err := script.Call(context.Background(), "run", []Value{NewString(name)}, CallOptions{})
				if err == nil {
					t.Fatalf("denied module loaded through %q", name)
				}
				if i == 0 && !strings.Contains(err.Error(), "denied by policy") {
					t.Fatalf("exact denied name error = %v", err)
				}
			}
		})
	}
}

func TestRequireSearchPathPrefersFirstExactSpelling(t *testing.T) {
	t.Parallel()
	first := tempModuleTree(t, moduleFile{path: "ExactFile.vibe", content: "def value\n  1\nend\n"})
	second := tempModuleTree(t, moduleFile{path: "EXACTFILE.vibe", content: "def value\n  2\nend\n"})
	for _, dev := range []bool{false, true} {
		engine := MustNewEngine(Config{ModulePaths: []string{first, second}, DevMode: dev})
		script := compileScriptWithEngine(t, engine, "def run(name)\n  require(name).value\nend")
		for _, tc := range []struct {
			name string
			want int64
		}{{"EXACTFILE", 2}, {"ExactFile", 1}, {"EXACTFILE.vibe", 2}} {
			got, err := script.Call(context.Background(), "run", []Value{NewString(tc.name)}, CallOptions{})
			if err != nil || got.Int() != tc.want {
				t.Fatalf("require %q (dev=%t) = %v, %v; want %d", tc.name, dev, got, err, tc.want)
			}
		}
	}
}

func TestCheckDoesNotAdmitFilesystemAliases(t *testing.T) {
	t.Parallel()
	root := tempModuleTree(t, moduleFile{path: "ExactFile.vibe", content: "def value\n  7\nend\n"})
	for _, literal := range []string{"\"EXACTFILE.vibe\"", ":EXACTFILE"} {
		engine := MustNewEngine(Config{ModulePaths: []string{root}})
		script := compileScriptWithEngine(t, engine, "def run\n  require("+literal+").value\nend")
		script.CheckWarnings()
		script.CheckOrderIndependentWarnings()
		if count := engine.ClearModuleCache(); count != 0 {
			t.Fatalf("checker cached %d modules through %s", count, literal)
		}
	}
	engine := MustNewEngine(Config{ModulePaths: []string{root}})
	script := compileScriptWithEngine(t, engine, "def run\n  require(\"ExactFile\").value\nend")
	if warnings := script.CheckWarnings(); len(warnings) != 0 {
		t.Fatalf("exact-name check warnings: %v", warnings)
	}
	if count := engine.ClearModuleCache(); count != 1 {
		t.Fatalf("checker cached %d exact-name modules, want 1", count)
	}
}

func TestRequirePreservesDistinctCaseFiles(t *testing.T) {
	t.Parallel()
	root := tempModuleTree(t, moduleFile{path: "ExactFile.vibe", content: "def value\n  1\nend\n"})
	f, err := os.OpenFile(filepath.Join(root, "exactfile.vibe"), os.O_CREATE|os.O_EXCL|os.O_WRONLY, 0o600)
	if os.IsExist(err) {
		t.Skip("filesystem does not distinguish case variants")
	}
	if err != nil {
		t.Fatal(err)
	}
	_, writeErr := f.WriteString("def value\n  2\nend\n")
	closeErr := f.Close()
	if writeErr != nil || closeErr != nil {
		t.Fatalf("writing distinct file: %v, %v", writeErr, closeErr)
	}
	engine := MustNewEngine(Config{ModulePaths: []string{root}})
	script := compileScriptWithEngine(t, engine, "def run(name)\n  require(name).value\nend")
	for i, name := range []string{"ExactFile", "exactfile"} {
		got, err := script.Call(context.Background(), "run", []Value{NewString(name)}, CallOptions{})
		if err != nil || got.Int() != int64(i+1) {
			t.Fatalf("distinct case module %q = %v, %v", name, got, err)
		}
	}
	if count := engine.ClearModuleCache(); count != 2 {
		t.Fatalf("distinct case files cached %d modules, want 2", count)
	}
}

func TestRequireLinkedModuleKeepsLexicalPolicyAndCaller(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct {
		name string
		link func(string, string) error
	}{{"symlink", os.Symlink}, {"hardlink", os.Link}} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			root := tempModuleTree(t,
				moduleFile{path: "Source/ExactFile.vibe", content: "def value\n  require(\"./Helper\").value\nend\n"},
				moduleFile{path: "Source/Helper.vibe", content: "def value\n  1\nend\n"},
				moduleFile{path: "Linked/Helper.vibe", content: "def value\n  2\nend\n"},
			)
			if err := tc.link(filepath.Join(root, "Source/ExactFile.vibe"), filepath.Join(root, "Linked/ExactLink.vibe")); err != nil {
				t.Skipf("link unavailable: %v", err)
			}
			engine := MustNewEngine(Config{ModulePaths: []string{root}, ModuleDenyList: []string{"Source/ExactFile"}})
			script := compileScriptWithEngine(t, engine, "def run\n  require(\"Linked/ExactLink\").value\nend")
			got, err := script.Call(context.Background(), "run", nil, CallOptions{})
			if err != nil || got.Int() != 2 {
				t.Fatalf("linked module = %v, %v; want caller-relative helper value 2", got, err)
			}
		})
	}
}

func TestRequireRejectsEscapeBeforeFilenameLookup(t *testing.T) {
	t.Parallel()
	outside := tempModuleTree(t, moduleFile{path: "Existing.vibe", content: "def value\n  7\nend\n"})
	root := tempModuleTree(t, moduleFile{path: "Driver.vibe", content: "def load(name)\n  require(name)\nend\n"})
	if err := os.Symlink(outside, filepath.Join(root, "Bridge")); err != nil {
		t.Skipf("symlinks unavailable: %v", err)
	}
	for _, relative := range []bool{false, true} {
		engine := MustNewEngine(Config{ModulePaths: []string{root}})
		source := "def run(name)\n  require(name)\nend"
		prefix := "Bridge/"
		if relative {
			source = "def run(name)\n  require(\"Driver\").load(name)\nend"
			prefix = "./Bridge/"
		}
		script := compileScriptWithEngine(t, engine, source)
		for _, name := range []string{"Existing", "Missing"} {
			_, err := script.Call(context.Background(), "run", []Value{NewString(prefix + name)}, CallOptions{})
			if err == nil || !strings.Contains(err.Error(), "escapes module root") {
				t.Errorf("external lookup %q error = %v; want uniform containment rejection", prefix+name, err)
			}
		}
	}
}
