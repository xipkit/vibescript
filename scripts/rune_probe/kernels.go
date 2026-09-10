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

//go:noinline
func countWidthWords(text string) int {
	count := 0
	for len(text) != 0 {
		count++
		lead := text[0]
		switch {
		case lead < 0x80:
			if len(text) >= 8 && binary.LittleEndian.Uint64([]byte(text[:8]))&0x8080808080808080 == 0 {
				count += 7
				text = text[8:]
				continue
			}
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

//go:noinline
func countASCIIRuns(text string) int {
	count := 0
	for len(text) != 0 {
		for len(text) >= 8 && binary.LittleEndian.Uint64([]byte(text[:8]))&0x8080808080808080 == 0 {
			count += 8
			text = text[8:]
		}
		for len(text) != 0 && text[0] < 0x80 {
			count++
			text = text[1:]
		}
		if len(text) == 0 {
			break
		}
		count++
		lead := text[0]
		switch {
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

//go:noinline
func countIndexed(text string) int {
	count := len(text)
	for i := 0; i < len(text); {
		lead := text[i]
		if lead < 0x80 {
			i++
			continue
		}
		tail := text[i:]
		switch {
		case lead-0xc2 < 0x1e:
			if len(tail) >= 2 && tail[1]&0xc0 == 0x80 {
				i += 2
				count -= 1
				continue
			}
		case lead-0xe0 < 0x10:
			if len(tail) >= 3 && tail[1]&0xc0 == 0x80 && tail[2]&0xc0 == 0x80 &&
				(lead != 0xe0 || tail[1] >= 0xa0) && (lead != 0xed || tail[1] < 0xa0) {
				i += 3
				count -= 2
				continue
			}
		case lead-0xf0 < 5:
			if len(tail) >= 4 && tail[1]&0xc0 == 0x80 && tail[2]&0xc0 == 0x80 && tail[3]&0xc0 == 0x80 &&
				(lead != 0xf0 || tail[1] >= 0x90) && (lead != 0xf4 || tail[1] < 0x90) {
				i += 4
				count -= 3
				continue
			}
		}
		i++
	}
	return count
}

//go:noinline
func countRunWords(text string) int {
	count := 0
	for len(text) != 0 {
		lead := text[0]
		if lead < 0x80 {
			i := 1
			for len(text)-i >= 8 && binary.LittleEndian.Uint64([]byte(text[i:i+8]))&0x8080808080808080 == 0 {
				i += 8
			}
			for i < len(text) && text[i] < 0x80 {
				i++
			}
			count += i
			text = text[i:]
			continue
		}
		count++
		switch {
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

//go:noinline
func runeWidth(text string) int {
	lead := text[0]
	switch {
	case lead-0xc2 < 0x1e:
		if len(text) >= 2 && text[1]&0xc0 == 0x80 {
			return 2
		}
	case lead-0xe0 < 0x10:
		if len(text) >= 3 && text[1]&0xc0 == 0x80 && text[2]&0xc0 == 0x80 &&
			(lead != 0xe0 || text[1] >= 0xa0) && (lead != 0xed || text[1] < 0xa0) {
			return 3
		}
	case lead-0xf0 < 5:
		if len(text) >= 4 && text[1]&0xc0 == 0x80 && text[2]&0xc0 == 0x80 && text[3]&0xc0 == 0x80 &&
			(lead != 0xf0 || text[1] >= 0x90) && (lead != 0xf4 || text[1] < 0x90) {
			return 4
		}
	}
	return 1
}

//go:noinline
func countWidthCalls(text string) int {
	count := 0
	for i := 0; i < len(text); count++ {
		if text[i] < 0x80 {
			i++
		} else {
			i += runeWidth(text[i:])
		}
	}
	return count
}
