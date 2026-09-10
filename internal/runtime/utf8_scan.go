package runtime

import (
	"encoding/binary"
	"math/bits"
	"unicode/utf8"
)

// Each entry stores width minus one in the low two bits and the second-byte
// acceptance range in the high three bits. Zero marks ASCII or an invalid lead.
var utf8RuneInfo = func() [256]byte {
	var info [256]byte
	for i := range 0x1e {
		info[0xc2+i] = 1
	}
	for i := range 0x10 {
		info[0xe0+i] = 2
	}
	for i := range 5 {
		info[0xf0+i] = 3
	}
	info[0xe0] = 1<<5 | 2
	info[0xed] = 2<<5 | 2
	info[0xf0] = 3<<5 | 3
	info[0xf4] = 4<<5 | 3
	return info
}()

// Each range stores its lower byte and the maximum allowed offset from it.
var utf8SecondRanges = [8][2]byte{
	{0x80, 0x3f},
	{0xa0, 0x1f},
	{0x80, 0x1f},
	{0x90, 0x2f},
	{0x80, 0x0f},
}

// stringRuneCount validates encoded widths without constructing rune values.
// The loop keeps four bytes available so each width needs no separate bound
// check. A malformed leading byte consumes only itself, as in UTF-8 range loops.
func stringRuneCount(text string) int {
	count := 0
	for len(text) > 4 {
		count++
		first := text[0]
		if first < utf8.RuneSelf {
			text = text[1:]
			continue
		}
		info := utf8RuneInfo[first]
		if info == 0 {
			text = text[1:]
			continue
		}
		width := int(info&3) + 1
		accept := utf8SecondRanges[info>>5]
		if text[1]-accept[0] > accept[1] ||
			width > 2 && text[2]&0xc0 != 0x80 ||
			width > 3 && text[3]&0xc0 != 0x80 {
			text = text[1:]
			continue
		}
		text = text[width:]
	}
	return count + utf8.RuneCountInString(text)
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
