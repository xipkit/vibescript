package main

import (
	"encoding/binary"
	"math/bits"
	"unicode/utf8"
)

//go:noinline
func countRange(text string) int {
	return utf8.RuneCountInString(text)
}

//go:noinline
func countValidated(text string) int {
	if !utf8.ValidString(text) {
		return utf8.RuneCountInString(text)
	}
	count := len(text)
	for len(text) >= 8 {
		word := binary.LittleEndian.Uint64([]byte(text[:8]))
		count -= bits.OnesCount64(word &^ (word << 1) & 0x8080808080808080)
		text = text[8:]
	}
	for i := range len(text) {
		if text[i]&0xc0 == 0x80 {
			count--
		}
	}
	return count
}

//go:noinline
func countTwoByte(text string) int {
	count := 0
	for len(text) != 0 {
		count++
		if text[0] < 0x80 {
			text = text[1:]
			continue
		}
		if len(text) >= 2 && text[0]-0xc2 < 0x1e && text[1]&0xc0 == 0x80 {
			text = text[2:]
			continue
		}
		_, size := utf8.DecodeRuneInString(text)
		text = text[size:]
	}
	return count
}

//go:noinline
func countWidths(text string) int {
	count := 0
	for len(text) != 0 {
		count++
		lead := text[0]
		switch {
		case lead < 0x80:
			text = text[1:]
			continue
		case lead-0xc2 < 0x1e:
			if len(text) >= 2 && text[1]&0xc0 == 0x80 {
				text = text[2:]
				continue
			}
		case lead-0xe0 < 0x10:
			if len(text) >= 3 && text[1]&0xc0 == 0x80 && text[2]&0xc0 == 0x80 &&
				(lead != 0xe0 || text[1] >= 0xa0) && (lead != 0xed || text[1] < 0xa0) {
				text = text[3:]
				continue
			}
		case lead-0xf0 < 5:
			if len(text) >= 4 && text[1]&0xc0 == 0x80 && text[2]&0xc0 == 0x80 && text[3]&0xc0 == 0x80 &&
				(lead != 0xf0 || text[1] >= 0x90) && (lead != 0xf4 || text[1] < 0x90) {
				text = text[4:]
				continue
			}
		}
		text = text[1:]
	}
	return count
}
