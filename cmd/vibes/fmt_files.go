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
	root *fmtRoot
	name string
	path string
	info fs.FileInfo
}

type fmtInputs struct {
	files    []fmtFile
	roots    []*fmtRoot
	nextRoot int
}

type fmtRoot struct {
	inputs *fmtInputs
	path   string
	info   fs.FileInfo
	handle *os.Root
}

func (root *fmtRoot) open() (*os.Root, error) {
	if root.handle != nil {
		return root.handle, nil
	}
	handle, err := os.OpenRoot(root.path)
	if err != nil {
		return nil, err
	}
	info, err := handle.Stat(".")
	if err != nil || !os.SameFile(root.info, info) {
		_ = handle.Close()
		if err != nil {
			return nil, err
		}
		return nil, errors.New("directory changed after discovery")
	}
	// Bound live descriptors across directory operands. Reopening an evicted
	// root must recover the same directory before any file can be accessed.
	const maxOpenRoots = 8
	inputs := root.inputs
	if len(inputs.roots) == maxOpenRoots {
		previous := inputs.roots[inputs.nextRoot]
		_ = previous.handle.Close()
		previous.handle = nil
		inputs.roots[inputs.nextRoot] = root
		inputs.nextRoot = (inputs.nextRoot + 1) % maxOpenRoots
	} else {
		inputs.roots = append(inputs.roots, root)
	}
	root.handle = handle
	return handle, nil
}

func (inputs *fmtInputs) close() {
	for _, root := range inputs.roots {
		_ = root.handle.Close()
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
	roots := make(map[string]*fmtRoot)
	seen := make(map[string]struct{})
	add := func(root *fmtRoot, name, path string, info fs.FileInfo) {
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
		root := roots[path]
		if root == nil {
			root = &fmtRoot{inputs: inputs, path: path, info: info}
			roots[path] = root
		}
		handle, err := root.open()
		if err != nil {
			return nil, err
		}
		if err := fs.WalkDir(handle.FS(), ".", func(name string, entry fs.DirEntry, walkErr error) error {
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
		handle, err := source.root.open()
		if err != nil {
			return nil, err
		}
		return handle.OpenFile(source.name, flag, 0)
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
