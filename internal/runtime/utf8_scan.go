package runtime

import (
	"encoding/binary"
	"math/bits"
)

// stringRuneCount validates encoded widths without constructing rune values.
// A malformed leading byte consumes only itself, matching UTF-8 range loops.
func stringRuneCount(text string) int {
	count := 0
	for i := 0; i < len(text); {
		first := text[i]
		i++
		count++
		if first < 0xc2 {
			continue
		}
		switch {
		case first < 0xe0:
			if i < len(text) && text[i]&0xc0 == 0x80 {
				i++
			}
		case first < 0xf0:
			if len(text)-i < 2 || text[i]&0xc0 != 0x80 || text[i+1]&0xc0 != 0x80 {
				continue
			}
			if first == 0xe0 && text[i] < 0xa0 || first == 0xed && text[i] >= 0xa0 {
				continue
			}
			i += 2
		case first < 0xf5:
			if len(text)-i < 3 || text[i]&0xc0 != 0x80 || text[i+1]&0xc0 != 0x80 || text[i+2]&0xc0 != 0x80 {
				continue
			}
			if first == 0xf0 && text[i] < 0x90 || first == 0xf4 && text[i] >= 0x90 {
				continue
			}
			i += 3
		}
	}
	return count
}

// utf8ContinuationBits marks each byte whose high bits are 10. Shifting left
// places bit 6 under bit 7 independently in every byte.
func utf8ContinuationBits(word uint64) uint64 {
	return word &^ (word << 1) & 0x8080808080808080
}

// validUTF8RuneCount counts rune starts after the caller has validated UTF-8.
// Invalid strings must use utf8.RuneCountInString, which counts bad bytes too.
func validUTF8RuneCount(text string) int {
	count := len(text)
	i := 0
	for ; len(text)-i >= 8; i += 8 {
		word := binary.LittleEndian.Uint64([]byte(text[i : i+8]))
		count -= bits.OnesCount64(utf8ContinuationBits(word))
	}
	for ; i < len(text); i++ {
		if text[i]&0xc0 == 0x80 {
			count--
		}
	}
	return count
}

// validUTF8ByteIndex finds a rune offset in already validated UTF-8. Counting
// leading bytes avoids decoding the same runes again after validation.
func validUTF8ByteIndex(text string, offset int) (int, bool) {
	if offset < 0 {
		return 0, false
	}
	i := 0
	for ; len(text)-i >= 8; i += 8 {
		word := binary.LittleEndian.Uint64([]byte(text[i : i+8]))
		count := 8 - bits.OnesCount64(utf8ContinuationBits(word))
		if count > offset {
			break
		}
		offset -= count
	}
	for ; i < len(text); i++ {
		if text[i]&0xc0 != 0x80 {
			if offset == 0 {
				return i, true
			}
			offset--
		}
	}
	if offset == 0 {
		return len(text), true
	}
	return 0, false
}
