package runtime

import (
	"encoding/binary"
	"fmt"
	"syscall"
	"unsafe"
)

// moduleStoredBaseNative queries the directory entry itself, including symlinks.
// ATTR_CMN_NAME needs search permission, not directory listing permission.
func moduleStoredBaseNative(path string) (string, error) {
	name, err := syscall.BytePtrFromString(path)
	if err != nil {
		return "", err
	}
	attrs := struct {
		Count                                 uint16
		Reserved                              uint16
		Common, Volume, Directory, File, Fork uint32
	}{Count: 5, Common: 1} // ATTR_BIT_MAP_COUNT, ATTR_CMN_NAME
	var buf [1024]byte
	_, _, errno := syscall.Syscall6(syscall.SYS_GETATTRLIST, uintptr(unsafe.Pointer(name)), uintptr(unsafe.Pointer(&attrs)), uintptr(unsafe.Pointer(&buf[0])), uintptr(len(buf)), 1, 0) // FSOPT_NOFOLLOW
	if errno != 0 {
		if errno == syscall.ENOTSUP || errno == syscall.ENOSYS {
			return "", fmt.Errorf("%w: %w", errModuleNameUnavailable, errno)
		}
		return "", errno
	}
	// The name reference follows the uint32 result length. Its offset is
	// relative to the reference, and its length includes the terminating NUL.
	total := int(binary.LittleEndian.Uint32(buf[:4]))
	offset := 4 + int(int32(binary.LittleEndian.Uint32(buf[4:8])))
	length := int(binary.LittleEndian.Uint32(buf[8:12]))
	if total < 12 || total > len(buf) || offset < 12 || offset > total || length < 1 || length > total-offset || buf[offset+length-1] != 0 {
		return "", syscall.EIO
	}
	return string(buf[offset : offset+length-1]), nil
}
