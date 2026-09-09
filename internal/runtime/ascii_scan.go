package runtime

import "encoding/binary"

// Keep the word loop out of callers so Unicode paths remain small.
//
//go:noinline
func stringIsASCIIWords(text string) bool {
	if len(text) != 0 && text[0] >= 128 {
		return false
	}
	i := 0
	for ; len(text)-i >= 8; i += 8 {
		if binary.LittleEndian.Uint64([]byte(text[i:i+8]))&0x8080808080808080 != 0 {
			return false
		}
	}
	for ; i < len(text); i++ {
		if text[i] >= 128 {
			return false
		}
	}
	return true
}
