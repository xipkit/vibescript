package runtime

import (
	"fmt"
	"os"
	"path/filepath"
	"strconv"

	"golang.org/x/sys/unix"
)

func moduleStoredBaseNative(path string) (string, error) {
	// O_PATH neither reads a directory nor opens a device/FIFO for I/O.
	// O_NOFOLLOW preserves the spelling of a symlink's own directory entry.
	fd, err := unix.Open(path, unix.O_PATH|unix.O_NOFOLLOW|unix.O_CLOEXEC, 0)
	if err != nil {
		return "", err
	}
	defer func() { _ = unix.Close(fd) }()
	resolved, err := os.Readlink("/proc/self/fd/" + strconv.Itoa(fd))
	if err != nil {
		return "", fmt.Errorf("%w through /proc/self/fd: %w", errModuleNameUnavailable, err)
	}
	return filepath.Base(resolved), nil
}
