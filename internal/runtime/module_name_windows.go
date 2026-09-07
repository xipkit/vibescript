package runtime

import (
	"encoding/binary"
	"errors"
	"fmt"
	"path/filepath"
	"strings"
	"unsafe"

	"golang.org/x/sys/windows"
)

func moduleStoredBaseNative(path string) (string, error) {
	base := filepath.Base(path)
	if !filepath.IsLocal(base) || strings.TrimRight(base, " .") != base {
		return "", windows.ERROR_FILE_NOT_FOUND
	}
	fullPath, err := filepath.Abs(path)
	if err != nil {
		return "", err
	}
	if !strings.HasPrefix(fullPath, `\\?\`) {
		if strings.HasPrefix(fullPath, `\\`) {
			fullPath = `\\?\UNC\` + fullPath[2:]
		} else {
			fullPath = `\\?\` + fullPath
		}
	}
	name, err := windows.UTF16PtrFromString(fullPath)
	if err != nil {
		return "", err
	}
	handle, err := windows.CreateFile(name, 0, windows.FILE_SHARE_READ|windows.FILE_SHARE_WRITE|windows.FILE_SHARE_DELETE, nil, windows.OPEN_EXISTING, windows.FILE_FLAG_BACKUP_SEMANTICS|windows.FILE_FLAG_OPEN_REPARSE_POINT, 0)
	if err != nil {
		return "", err
	}
	defer func() { _ = windows.CloseHandle(handle) }()
	for size := 1024; size <= 1<<17; size *= 2 {
		buf := make([]byte, size)
		err := windows.GetFileInformationByHandleEx(handle, windows.FileNormalizedNameInfo, &buf[0], uint32(len(buf)))
		if errors.Is(err, windows.ERROR_MORE_DATA) || errors.Is(err, windows.ERROR_INSUFFICIENT_BUFFER) {
			continue
		}
		if err != nil {
			return "", fmt.Errorf("%w: %w", errModuleNameUnavailable, err)
		}
		length := int(binary.LittleEndian.Uint32(buf[:4]))
		if length < 2 || length > len(buf)-4 || length%2 != 0 {
			return "", windows.ERROR_INVALID_DATA
		}
		actual := windows.UTF16ToString(unsafe.Slice((*uint16)(unsafe.Pointer(&buf[4])), length/2))
		return filepath.Base(actual), nil
	}
	return "", windows.ERROR_FILENAME_EXCED_RANGE
}
