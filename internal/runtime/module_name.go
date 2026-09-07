package runtime

import (
	"errors"
	"fmt"
	"io"
	"io/fs"
	"path/filepath"
	"strings"
)

var errModuleNameUnavailable = errors.New("stored module filename unavailable")

// Static checks have no execution quota. Share this bound across their module
// lookups, including nested modules, rather than resetting it for each require.
const maxModuleNameCheckWork = 1 << 20

type moduleNameWork struct {
	charge func(int) error
	used   int
}

func (c *scriptChecker) moduleNameBudget() *moduleNameWork {
	if c.moduleNameWork == nil {
		c.moduleNameWork = &moduleNameWork{}
	}
	return c.moduleNameWork
}

func (c *scriptChecker) loadModule(name string) (moduleEntry, error) {
	if c.script.engine.config.StrictEffects && !c.callOptions.AllowRequire {
		return moduleEntry{}, fmt.Errorf("strict effects: require is disabled without CallOptions.AllowRequire")
	}
	return c.script.engine.loadModule(name, c.moduleCaller, nil, c.moduleNameBudget())
}

func (w *moduleNameWork) step() error {
	if w.charge != nil {
		return w.charge(1)
	}
	if w.used >= maxModuleNameCheckWork {
		return guardLimitErrorf("module filename verification exceeds work limit")
	}
	w.used++
	return nil
}

func checkModuleSpelling(root, relative string, work *moduleNameWork) error {
	current := root
	for component := range strings.SplitSeq(relative, string(filepath.Separator)) {
		if err := work.step(); err != nil {
			return err
		}
		current = filepath.Join(current, component)
		stored, err := moduleStoredBase(current, work)
		if err != nil {
			return err
		}
		if stored != component {
			return fs.ErrNotExist
		}
	}
	return nil
}

func moduleStoredBase(path string, work *moduleNameWork) (string, error) {
	name, err := moduleStoredBaseNative(path)
	if !errors.Is(err, errModuleNameUnavailable) {
		return name, err
	}
	return moduleStoredBaseFromDirectory(path, work)
}

func moduleStoredBaseFromDirectory(path string, work *moduleNameWork) (string, error) {
	f, err := openModuleSource(filepath.Dir(path))
	if err != nil {
		if errors.Is(err, fs.ErrPermission) {
			return "", fmt.Errorf("module filename verification requires directory listing permission: %w", err)
		}
		return "", err
	}
	defer func() { _ = f.Close() }()
	info, err := f.Stat()
	if err != nil {
		return "", err
	}
	if !info.IsDir() {
		return "", fmt.Errorf("module parent is not a directory")
	}
	base := filepath.Base(path)
	for {
		// Readdirnames avoids per-entry stat calls and retains at most one
		// name. Charge before reading so repeated failed lookups share limits.
		if err := work.step(); err != nil {
			return "", err
		}
		names, err := f.Readdirnames(1)
		if len(names) != 0 && names[0] == base {
			return base, nil
		}
		if errors.Is(err, io.EOF) {
			return "", fs.ErrNotExist
		}
		if err != nil {
			return "", err
		}
	}
}
