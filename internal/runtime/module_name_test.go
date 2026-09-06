//go:build darwin || linux || windows

package runtime

import (
	"os"
	"path/filepath"
	"runtime"
	"strings"
	"testing"
)

func TestModuleStoredBase(t *testing.T) {
	t.Parallel()
	root := tempModuleTree(t, moduleFile{path: "ExactDir/ExactFile.vibe", content: "def value\n  7\nend\n"})
	for _, relative := range []string{"ExactDir", "ExactDir/ExactFile.vibe"} {
		name, err := moduleStoredBase(filepath.Join(root, relative))
		if err != nil || name != filepath.Base(relative) {
			t.Fatalf("stored name of %q = %q, %v", relative, name, err)
		}
	}
	for _, relative := range []string{"EXACTDIR", "ExactDir/EXACTFILE.vibe"} {
		name, err := moduleStoredBase(filepath.Join(root, relative))
		if os.IsNotExist(err) {
			continue
		}
		if err != nil || name == filepath.Base(relative) {
			t.Fatalf("stored name of alias %q = %q, %v", relative, name, err)
		}
	}
}

func TestModuleStoredBasePreservesLinks(t *testing.T) {
	t.Parallel()
	root := tempModuleTree(t, moduleFile{path: "ExactFile.vibe", content: "def value\n  7\nend\n"})
	for _, tc := range []struct {
		name string
		link func(string, string) error
	}{
		{"ExactSymlink.vibe", os.Symlink},
		{"ExactHardlink.vibe", os.Link},
	} {
		t.Run(tc.name, func(t *testing.T) {
			path := filepath.Join(root, tc.name)
			if err := tc.link(filepath.Join(root, "ExactFile.vibe"), path); err != nil {
				t.Skipf("link unavailable: %v", err)
			}
			actual, err := moduleStoredBase(path)
			if err != nil || actual != tc.name {
				t.Fatalf("stored link name = %q, %v; want %q", actual, err, tc.name)
			}
		})
	}
}

func TestModuleStoredBaseWithoutDirectoryReadPermission(t *testing.T) {
	if runtime.GOOS == "windows" || os.Geteuid() == 0 {
		t.Skip("requires Unix permission enforcement")
	}
	root := tempModuleTree(t, moduleFile{path: "ExactDir/ExactFile.vibe", content: "def value\n  7\nend\n"})
	dir := filepath.Join(root, "ExactDir")
	if err := os.Chmod(dir, 0o111); err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = os.Chmod(dir, 0o700) })
	for _, path := range []string{dir, filepath.Join(dir, "ExactFile.vibe")} {
		actual, err := moduleStoredBase(path)
		if err != nil || actual != filepath.Base(path) {
			t.Fatalf("stored name without directory read permission = %q, %v", actual, err)
		}
	}
}

func TestModuleStoredBaseUnicodeAndLongPaths(t *testing.T) {
	t.Parallel()
	long := strings.Repeat("ExactDir/", 40) + "ExactFile.vibe"
	root := tempModuleTree(t,
		moduleFile{path: "é.vibe", content: "def value\n  7\nend\n"},
		moduleFile{path: long, content: "def value\n  7\nend\n"},
	)
	for _, relative := range []string{"é.vibe", long} {
		actual, err := moduleStoredBase(filepath.Join(root, relative))
		if err != nil || actual != filepath.Base(relative) {
			t.Fatalf("stored name of %q = %q, %v", relative, actual, err)
		}
	}
	actual, err := moduleStoredBase(filepath.Join(root, "e\u0301.vibe"))
	if os.IsNotExist(err) {
		return
	}
	if err != nil || actual != "é.vibe" {
		t.Fatalf("stored name of Unicode alias = %q, %v", actual, err)
	}
}

func TestModuleStoredBaseDistinctCaseNames(t *testing.T) {
	t.Parallel()
	root := tempModuleTree(t, moduleFile{path: "ExactFile.vibe", content: "def value\n  7\nend\n"})
	file, err := os.OpenFile(filepath.Join(root, "exactfile.vibe"), os.O_CREATE|os.O_EXCL|os.O_WRONLY, 0o600)
	if os.IsExist(err) {
		t.Skip("filesystem does not distinguish case variants")
	}
	if err != nil {
		t.Fatal(err)
	}
	if err := file.Close(); err != nil {
		t.Fatal(err)
	}
	for _, name := range []string{"ExactFile.vibe", "exactfile.vibe"} {
		actual, err := moduleStoredBase(filepath.Join(root, name))
		if err != nil || actual != name {
			t.Fatalf("stored name of distinct file %q = %q, %v", name, actual, err)
		}
	}
}
