package runtime

import (
	"encoding/binary"
	"math/bits"
)

func asciiCaseCompareBytes(a, b string) int {
	for i := range min(len(a), len(b)) {
		ca, cb := asciiLower(a[i]), asciiLower(b[i])
		if ca != cb {
			if ca < cb {
				return -1
			}
			return 1
		}
	}
	switch {
	case len(a) < len(b):
		return -1
	case len(a) > len(b):
		return 1
	default:
		return 0
	}
}

func asciiCaseEqualBytes(a, b string) bool {
	if len(a) != len(b) {
		return false
	}
	for i := range len(a) {
		if asciiLower(a[i]) != asciiLower(b[i]) {
			return false
		}
	}
	return true
}

// Each addition stays inside its byte because the input's high bits are clear.
// Their high-bit difference selects A-Z; original high bits exclude non-ASCII.
func asciiFoldCompareWord(word uint64) uint64 {
	low := word & 0x7f7f7f7f7f7f7f7f
	upper := (low + 0x3f3f3f3f3f3f3f3f) &^ (low + 0x2525252525252525) &^ word & 0x8080808080808080
	return word | upper>>2
}

func asciiCaseCompareWords(a, b string) int {
	if bits.UintSize < 64 {
		return asciiCaseCompareBytes(a, b)
	}
	limit := min(len(a), len(b))
	i := 0
	for ; i < min(4, limit); i++ {
		ca, cb := asciiLower(a[i]), asciiLower(b[i])
		if ca != cb {
			if ca < cb {
				return -1
			}
			return 1
		}
	}
	for ; limit-i >= 8; i += 8 {
		av := asciiFoldCompareWord(binary.LittleEndian.Uint64([]byte(a[i : i+8])))
		bv := asciiFoldCompareWord(binary.LittleEndian.Uint64([]byte(b[i : i+8])))
		if av != bv {
			shift := bits.TrailingZeros64(av^bv) &^ 7
			if byte(av>>shift) < byte(bv>>shift) {
				return -1
			}
			return 1
		}
	}
	for ; i < limit; i++ {
		ca, cb := asciiLower(a[i]), asciiLower(b[i])
		if ca != cb {
			if ca < cb {
				return -1
			}
			return 1
		}
	}
	switch {
	case len(a) < len(b):
		return -1
	case len(a) > len(b):
		return 1
	default:
		return 0
	}
}

func asciiCaseEqualWords(a, b string) bool {
	if bits.UintSize < 64 {
		return asciiCaseEqualBytes(a, b)
	}
	if len(a) != len(b) {
		return false
	}
	i := 0
	for ; i < min(4, len(a)); i++ {
		if asciiLower(a[i]) != asciiLower(b[i]) {
			return false
		}
	}
	for ; len(a)-i >= 8; i += 8 {
		av := asciiFoldCompareWord(binary.LittleEndian.Uint64([]byte(a[i : i+8])))
		bv := asciiFoldCompareWord(binary.LittleEndian.Uint64([]byte(b[i : i+8])))
		if av != bv {
			return false
		}
	}
	for ; i < len(a); i++ {
		if asciiLower(a[i]) != asciiLower(b[i]) {
			return false
		}
	}
	return true
}
