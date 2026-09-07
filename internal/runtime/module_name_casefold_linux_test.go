package runtime

import (
	"context"
	"os"
	"testing"
)

func TestRequireColdCasefoldFilesystem(t *testing.T) {
	root := os.Getenv("VIBES_MODULE_CASEFOLD_ROOT")
	if root == "" {
		t.Skip("requires the remounted ext4 fixture from check_module_casefold.sh")
	}
	engine := MustNewEngine(Config{ModulePaths: []string{root}})
	script := compileScriptWithEngine(t, engine, "def run(name)\n  require(name).value\nend")
	for _, tc := range []struct {
		alias string
		exact string
	}{
		{"Files/EXACTFILE.vibe", "Files/ExactFile.vibe"},
		{"DIRS/ExactFile.vibe", "Dirs/ExactFile.vibe"},
		{"Unicode/e\u0301.vibe", "Unicode/é.vibe"},
	} {
		if _, err := script.Call(context.Background(), "run", []Value{NewString(tc.alias)}, CallOptions{}); err == nil {
			t.Errorf("cold casefold alias %q loaded", tc.alias)
		}
		got, err := script.Call(context.Background(), "run", []Value{NewString(tc.exact)}, CallOptions{})
		if err != nil || got.Int() != 7 {
			t.Errorf("exact name %q after alias lookup = %v, %v", tc.exact, got, err)
		}
	}
}
