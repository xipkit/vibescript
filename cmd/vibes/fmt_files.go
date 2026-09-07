package main

import (
	"errors"
	"fmt"
	"io"
	"io/fs"
	"os"
	"path/filepath"
	"slices"
	"strings"
)

type fmtFile struct {
	root *os.Root
	name string
	path string
	info fs.FileInfo
}

type fmtInputs struct {
	files []fmtFile
	roots []*os.Root
}

func (inputs *fmtInputs) close() {
	for _, root := range inputs.roots {
		_ = root.Close()
	}
}

func collectVibeFiles(targets []string) (*fmtInputs, error) {
	inputs := &fmtInputs{}
	success := false
	defer func() {
		if !success {
			inputs.close()
		}
	}()
	roots := make(map[string]*os.Root)
	openRoot := func(path string) (*os.Root, error) {
		if root := roots[path]; root != nil {
			return root, nil
		}
		root, err := os.OpenRoot(path)
		if err != nil {
			return nil, err
		}
		roots[path] = root
		inputs.roots = append(inputs.roots, root)
		return root, nil
	}
	seen := make(map[string]struct{})
	add := func(root *os.Root, name, path string, info fs.FileInfo) {
		if filepath.Ext(path) != ".vibe" {
			return
		}
		if _, ok := seen[path]; ok {
			return
		}
		seen[path] = struct{}{}
		inputs.files = append(inputs.files, fmtFile{root: root, name: name, path: path, info: info})
	}
	for _, target := range targets {
		path, err := filepath.Abs(target)
		if err != nil {
			return nil, fmt.Errorf("resolve %s: %w", target, err)
		}
		info, err := os.Stat(path)
		if err != nil {
			return nil, fmt.Errorf("stat %s: %w", target, err)
		}
		if !info.IsDir() {
			if filepath.Ext(path) != ".vibe" {
				continue
			}
			if !info.Mode().IsRegular() {
				return nil, fmt.Errorf("%s is not a regular file", target)
			}
			// An explicit operand authorizes its selected target. Recursive
			// discovery below never follows a leaf or directory symlink.
			resolved, err := filepath.EvalSymlinks(path)
			if err != nil {
				return nil, fmt.Errorf("resolve %s: %w", target, err)
			}
			add(nil, resolved, path, info)
			continue
		}
		root, err := openRoot(path)
		if err != nil {
			return nil, err
		}
		if err := fs.WalkDir(root.FS(), ".", func(name string, entry fs.DirEntry, walkErr error) error {
			if walkErr != nil {
				return walkErr
			}
			if entry.Type().IsRegular() && filepath.Ext(name) == ".vibe" {
				info, err := entry.Info()
				if err != nil {
					return err
				}
				add(root, filepath.FromSlash(name), filepath.Join(path, filepath.FromSlash(name)), info)
			}
			return nil
		}); err != nil {
			return nil, fmt.Errorf("walk %s: %w", target, err)
		}
	}
	slices.SortFunc(inputs.files, func(a, b fmtFile) int { return strings.Compare(a.path, b.path) })
	success = true
	return inputs, nil
}

func (source fmtFile) open(flag int) (*os.File, error) {
	if source.root != nil {
		return source.root.OpenFile(source.name, flag, 0)
	}
	return os.OpenFile(source.name, flag, 0)
}

func (source fmtFile) read() (data []byte, info fs.FileInfo, err error) {
	file, err := source.open(os.O_RDONLY)
	if err != nil {
		return nil, nil, err
	}
	defer func() { err = errors.Join(err, file.Close()) }()
	info, err = file.Stat()
	if err != nil {
		return nil, nil, err
	}
	if !info.Mode().IsRegular() || !os.SameFile(source.info, info) {
		return nil, nil, errors.New("file changed after discovery")
	}
	if info.Size() >= int64(int(^uint(0)>>1)) {
		return nil, nil, errors.New("file too large to format")
	}
	data, err = io.ReadAll(io.LimitReader(file, info.Size()+1))
	if err != nil {
		return nil, nil, err
	}
	if int64(len(data)) != info.Size() {
		return nil, nil, errors.New("file changed while reading")
	}
	return data, info, nil
}

func (source fmtFile) write(original fs.FileInfo, data []byte) (err error) {
	file, err := source.open(os.O_WRONLY)
	if err != nil {
		return err
	}
	defer func() { err = errors.Join(err, file.Close()) }()
	info, err := file.Stat()
	if err != nil {
		return err
	}
	// Opening without truncation preserves the original until the same-file
	// check succeeds. All later writes use this verified descriptor.
	if !info.Mode().IsRegular() || !os.SameFile(original, info) || original.Size() != info.Size() || !original.ModTime().Equal(info.ModTime()) {
		return errors.New("file changed while formatting")
	}
	n, err := file.Write(data)
	if err != nil {
		return err
	}
	if n != len(data) {
		return io.ErrShortWrite
	}
	return file.Truncate(int64(len(data)))
}
