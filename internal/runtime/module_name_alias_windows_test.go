package runtime

import (
	"context"
	"testing"
)

func TestRequireRejectsWin32FilenameAliases(t *testing.T) {
	t.Parallel()
	root := tempModuleTree(t, moduleFile{path: "ExactDir/ExactFile.vibe", content: "def value\n  7\nend\n"})
	engine := MustNewEngine(Config{ModulePaths: []string{root}})
	script := compileScriptWithEngine(t, engine, "def run(name)\n  require(name).value\nend")
	for _, name := range []string{
		"ExactDir./ExactFile.vibe", "ExactDir /ExactFile.vibe",
		"ExactDir/ExactFile.vibe.", "ExactDir/ExactFile.vibe::$DATA",
	} {
		if _, err := script.Call(context.Background(), "run", []Value{NewString(name)}, CallOptions{}); err == nil {
			t.Errorf("Win32 filename alias %q loaded", name)
		}
	}
	got, err := script.Call(context.Background(), "run", []Value{NewString("ExactDir/ExactFile")}, CallOptions{})
	if err != nil || got.Int() != 7 {
		t.Fatalf("exact Win32 filename = %v, %v", got, err)
	}
}
