package runtime

import (
	"errors"
	"io/fs"
	"path/filepath"
	"testing"
)

func TestModuleNameDirectoryFallback(t *testing.T) {
	t.Parallel()
	root := tempModuleTree(t, moduleFile{path: "ExactDir/ExactFile.vibe", content: "def value\n  7\nend\n"})
	work := &moduleNameWork{}
	for _, relative := range []string{"ExactDir", "ExactDir/ExactFile.vibe"} {
		name, err := moduleStoredBaseFromDirectory(filepath.Join(root, relative), work)
		if err != nil || name != filepath.Base(relative) {
			t.Fatalf("stored name of %q = %q, %v", relative, name, err)
		}
	}
	for _, relative := range []string{"EXACTDIR", "ExactDir/EXACTFILE.vibe", "ExactDir/missing.vibe"} {
		_, err := moduleStoredBaseFromDirectory(filepath.Join(root, relative), work)
		if !errors.Is(err, fs.ErrNotExist) {
			t.Fatalf("alias or missing file %q error = %v", relative, err)
		}
	}
}

func TestModuleNameFallbackSharesWorkAcrossLookups(t *testing.T) {
	t.Parallel()
	root := tempModuleTree(t, moduleFile{path: "ExactFile.vibe", content: "def value\n  7\nend\n"})
	quotaErr := errors.New("test quota exhausted")
	remaining := 1
	work := &moduleNameWork{charge: func(n int) error {
		if n > remaining {
			return quotaErr
		}
		remaining -= n
		return nil
	}}
	name := filepath.Join(root, "ExactFile.vibe")
	if _, err := moduleStoredBaseFromDirectory(name, work); err != nil {
		t.Fatal(err)
	}
	if _, err := moduleStoredBaseFromDirectory(name, work); !errors.Is(err, quotaErr) {
		t.Fatalf("second lookup error = %v, want shared quota error", err)
	}
	staticWork := &moduleNameWork{used: maxModuleNameCheckWork - 1}
	if _, err := moduleStoredBaseFromDirectory(name, staticWork); err != nil {
		t.Fatal(err)
	}
	if _, err := moduleStoredBaseFromDirectory(name, staticWork); err == nil {
		t.Fatal("second static lookup reset the shared work limit")
	}
}
