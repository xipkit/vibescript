package main

import (
	"net"
	"os"
	"path/filepath"
	"runtime"
	"strings"
	"testing"
)

func fmtTestSymlink(t *testing.T, target, path string) {
	t.Helper()
	if err := os.Symlink(target, path); err != nil {
		t.Skipf("symlinks unavailable: %v", err)
	}
}

func TestFmtRecursiveSymlinks(t *testing.T) {
	for _, mode := range []string{"stdout", "-check", "-w"} {
		t.Run(mode, func(t *testing.T) {
			root := t.TempDir()
			outside := filepath.Join(t.TempDir(), "outside.vibe")
			original := "outside  \r\n"
			if err := os.WriteFile(outside, []byte(original), 0o644); err != nil {
				t.Fatal(err)
			}
			regular := filepath.Join(root, "a.vibe")
			if err := os.WriteFile(regular, []byte("inside  \r\n"), 0o644); err != nil {
				t.Fatal(err)
			}
			fmtTestSymlink(t, outside, filepath.Join(root, "absolute.vibe"))
			relative, err := filepath.Rel(root, outside)
			if err != nil {
				t.Fatal(err)
			}
			fmtTestSymlink(t, relative, filepath.Join(root, "relative.vibe"))
			fmtTestSymlink(t, "absolute.vibe", filepath.Join(root, "chain.vibe"))
			fmtTestSymlink(t, "missing", filepath.Join(root, "dangling.vibe"))
			fmtTestSymlink(t, filepath.Dir(outside), filepath.Join(root, "linked_directory"))
			args := []string{root}
			if mode != "stdout" {
				args = append([]string{mode}, args...)
			}
			out, err := dispatchCommand(t, "fmt", args)
			if mode == "-check" {
				if err == nil || !strings.Contains(err.Error(), "1 file(s) need formatting") {
					t.Errorf("check error = %v, want one regular file needing formatting", err)
				}
			} else if err != nil {
				t.Errorf("fmt error = %v", err)
			}
			if mode == "stdout" && out != "inside\n" {
				t.Errorf("stdout = %q, want only the regular file", out)
			}
			got, err := os.ReadFile(outside)
			if err != nil || string(got) != original {
				t.Fatalf("outside file = %q, error = %v; want unchanged %q", got, err, original)
			}
			if mode == "-w" {
				got, err = os.ReadFile(regular)
				if err != nil || string(got) != "inside\n" {
					t.Fatalf("regular file = %q, error = %v", got, err)
				}
			}
		})
	}
}

func TestFmtExplicitFileAlias(t *testing.T) {
	target := filepath.Join(t.TempDir(), "target.txt")
	if err := os.WriteFile(target, []byte("explicit  \n"), 0o644); err != nil {
		t.Fatal(err)
	}
	alias := filepath.Join(t.TempDir(), "selected.vibe")
	fmtTestSymlink(t, target, alias)
	if _, err := dispatchCommand(t, "fmt", []string{"-w", alias}); err != nil {
		t.Fatal(err)
	}
	got, err := os.ReadFile(target)
	if err != nil || string(got) != "explicit\n" {
		t.Fatalf("explicit target = %q, error = %v", got, err)
	}
	if info, err := os.Lstat(alias); err != nil || info.Mode()&os.ModeSymlink == 0 {
		t.Fatalf("explicit alias replaced: info %v, error %v", info, err)
	}
}

func TestFmtExplicitFileInSearchOnlyDirectory(t *testing.T) {
	if runtime.GOOS == "windows" {
		t.Skip("requires Unix directory permissions")
	}
	for _, mode := range []string{"stdout", "-check", "-w"} {
		t.Run(mode, func(t *testing.T) {
			parent := t.TempDir()
			path := filepath.Join(parent, "selected.vibe")
			if err := os.WriteFile(path, []byte("explicit  \n"), 0o600); err != nil {
				t.Fatal(err)
			}
			if err := os.Chmod(parent, 0o111); err != nil {
				t.Fatal(err)
			}
			defer os.Chmod(parent, 0o700)
			if _, err := os.ReadDir(parent); err == nil {
				t.Skip("directory read permission is not enforced")
			}
			if _, err := os.ReadFile(path); err != nil {
				t.Fatalf("direct file read: %v", err)
			}
			args := []string{path}
			if mode != "stdout" {
				args = append([]string{mode}, args...)
			}
			out, err := dispatchCommand(t, "fmt", args)
			if mode == "-check" {
				if err == nil || !strings.Contains(err.Error(), "1 file(s) need formatting") {
					t.Fatalf("check error = %v, want one file needing formatting", err)
				}
			} else if err != nil {
				t.Fatal(err)
			}
			if mode == "stdout" && out != "explicit\n" {
				t.Errorf("stdout = %q", out)
			}
			want := "explicit  \n"
			if mode == "-w" {
				want = "explicit\n"
			}
			if data, err := os.ReadFile(path); err != nil || string(data) != want {
				t.Fatalf("file = %q, error = %v, want %q", data, err, want)
			}
		})
	}
}

func TestFmtCandidatesRejectReplacement(t *testing.T) {
	for _, replaced := range []string{"leaf", "ancestor", "regular file"} {
		t.Run(replaced, func(t *testing.T) {
			root := t.TempDir()
			nested := filepath.Join(root, "nested")
			if err := os.Mkdir(nested, 0o755); err != nil {
				t.Fatal(err)
			}
			path := filepath.Join(nested, "file.vibe")
			if err := os.WriteFile(path, []byte("original  \n"), 0o644); err != nil {
				t.Fatal(err)
			}
			outside := filepath.Join(t.TempDir(), "file.vibe")
			if err := os.WriteFile(outside, []byte("outside  \n"), 0o644); err != nil {
				t.Fatal(err)
			}
			inputs, err := collectVibeFiles([]string{root})
			if err != nil {
				t.Fatal(err)
			}
			defer inputs.close()
			source := inputs.files[0]
			_, original, err := source.read()
			if err != nil {
				t.Fatal(err)
			}
			switch replaced {
			case "leaf":
				if err := os.Remove(path); err != nil {
					t.Fatal(err)
				}
				fmtTestSymlink(t, outside, path)
			case "ancestor":
				if err := os.Rename(nested, nested+"-saved"); err != nil {
					t.Fatal(err)
				}
				fmtTestSymlink(t, filepath.Dir(outside), nested)
			case "regular file":
				if err := os.Rename(path, path+".saved"); err != nil {
					t.Fatal(err)
				}
				if err := os.WriteFile(path, []byte("replacement\n"), 0o644); err != nil {
					t.Fatal(err)
				}
			}
			if _, _, err := source.read(); err == nil {
				t.Error("read accepted a replaced candidate")
			}
			if err := source.write(original, []byte("formatted\n")); err == nil {
				t.Error("write accepted a replaced candidate")
			}
			got, err := os.ReadFile(outside)
			if err != nil || string(got) != "outside  \n" {
				t.Fatalf("outside file = %q, error = %v", got, err)
			}
			if replaced == "regular file" {
				got, err := os.ReadFile(path)
				if err != nil || string(got) != "replacement\n" {
					t.Fatalf("replacement was modified: %q, error = %v", got, err)
				}
			}
		})
	}
}

func TestFmtKeepsOpenedRoot(t *testing.T) {
	if runtime.GOOS == "windows" {
		t.Skip("Windows does not allow renaming an open root directory")
	}
	parent := t.TempDir()
	root := filepath.Join(parent, "root")
	if err := os.Mkdir(root, 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(root, "file.vibe"), []byte("inside  \n"), 0o644); err != nil {
		t.Fatal(err)
	}
	inputs, err := collectVibeFiles([]string{root})
	if err != nil {
		t.Fatal(err)
	}
	defer inputs.close()
	moved := filepath.Join(parent, "moved")
	if err := os.Rename(root, moved); err != nil {
		t.Fatal(err)
	}
	outside := t.TempDir()
	if err := os.WriteFile(filepath.Join(outside, "file.vibe"), []byte("outside  \n"), 0o644); err != nil {
		t.Fatal(err)
	}
	fmtTestSymlink(t, outside, root)
	data, info, err := inputs.files[0].read()
	if err != nil || string(data) != "inside  \n" {
		t.Fatalf("pinned root read = %q, error = %v", data, err)
	}
	if err := inputs.files[0].write(info, []byte(formatVibeSource(string(data)))); err != nil {
		t.Fatal(err)
	}
	for path, want := range map[string]string{filepath.Join(moved, "file.vibe"): "inside\n", filepath.Join(outside, "file.vibe"): "outside  \n"} {
		got, err := os.ReadFile(path)
		if err != nil || string(got) != want {
			t.Errorf("%s = %q, error = %v, want %q", path, got, err, want)
		}
	}
}

func TestFmtPreservesFileIdentityAndReadOnlyNoop(t *testing.T) {
	root := t.TempDir()
	path := filepath.Join(root, "file.vibe")
	if err := os.WriteFile(path, []byte("body  \n"), 0o600); err != nil {
		t.Fatal(err)
	}
	alias := filepath.Join(t.TempDir(), "hardlink")
	if err := os.Link(path, alias); err != nil {
		t.Skipf("hardlinks unavailable: %v", err)
	}
	before, err := os.Stat(path)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := dispatchCommand(t, "fmt", []string{"-w", path}); err != nil {
		t.Fatal(err)
	}
	after, err := os.Stat(path)
	if err != nil || !os.SameFile(before, after) {
		t.Fatalf("file identity changed: %v", err)
	}
	if runtime.GOOS != "windows" && after.Mode().Perm() != 0o600 {
		t.Errorf("permissions = %o, want 600", after.Mode().Perm())
	}
	if data, err := os.ReadFile(alias); err != nil || string(data) != "body\n" {
		t.Fatalf("hardlink = %q, error = %v", data, err)
	}
	if err := os.Chmod(path, 0o444); err != nil {
		t.Fatal(err)
	}
	defer os.Chmod(path, 0o600)
	if _, err := dispatchCommand(t, "fmt", []string{"-w", path}); err != nil {
		t.Fatalf("unchanged read-only file: %v", err)
	}
}

func TestFmtSkipsNonregularEntries(t *testing.T) {
	root, err := os.MkdirTemp("", "vfmt-")
	if err != nil {
		t.Fatal(err)
	}
	defer os.RemoveAll(root)
	listener, err := net.Listen("unix", filepath.Join(root, "socket.vibe"))
	if err != nil {
		t.Skipf("local sockets unavailable: %v", err)
	}
	defer listener.Close()
	if out, err := dispatchCommand(t, "fmt", []string{root}); err != nil || out != "" {
		t.Fatalf("nonregular entry: output %q, error = %v", out, err)
	}
}

func TestFmtDirectoryAliasAndDeduplication(t *testing.T) {
	root := t.TempDir()
	for name, source := range map[string]string{"a.vibe": "first\n", "b.vibe": "second\n"} {
		if err := os.WriteFile(filepath.Join(root, name), []byte(source), 0o644); err != nil {
			t.Fatal(err)
		}
	}
	alias := filepath.Join(t.TempDir(), "selected")
	fmtTestSymlink(t, root, alias)
	for _, target := range []string{alias, alias + string(os.PathSeparator) + "."} {
		out, err := dispatchCommand(t, "fmt", []string{target, filepath.Join(target, "a.vibe")})
		if err != nil || out != "first\nsecond\n" {
			t.Errorf("alias output = %q, error = %v", out, err)
		}
	}
}
