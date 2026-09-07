package runtime

import (
	"context"
	"errors"
	"io/fs"
	"os"
	"path/filepath"
	"strings"
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

func TestRequireReportsMissingDirectoryListingPermission(t *testing.T) {
	if os.Geteuid() == 0 {
		t.Skip("requires Unix permission enforcement")
	}
	root := tempModuleTree(t, moduleFile{path: "ExactDir/ExactFile.vibe", content: "def value\n  7\nend\n"})
	dir := filepath.Join(root, "ExactDir")
	if err := os.Chmod(dir, 0o111); err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = os.Chmod(dir, 0o700) })
	engine := MustNewEngine(Config{ModulePaths: []string{root}})
	script := compileScriptWithEngine(t, engine, "def run\n  require(\"ExactDir/ExactFile\").value\nend")
	_, err := script.Call(context.Background(), "run", nil, CallOptions{})
	if !errors.Is(err, fs.ErrPermission) || !strings.Contains(err.Error(), "requires directory listing permission") {
		t.Fatalf("module without directory listing permission error = %v", err)
	}
	if err := os.Chmod(dir, 0o755); err != nil {
		t.Fatal(err)
	}
	got, err := script.Call(context.Background(), "run", nil, CallOptions{})
	if err != nil || got.Int() != 7 {
		t.Fatalf("module after granting listing permission = %v, %v", got, err)
	}
}
