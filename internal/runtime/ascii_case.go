package runtime

import "encoding/binary"

// asciiLetterCaseBits returns bit 5 of each ASCII letter byte. After folding
// and clearing high bits, adding 31 sets bit 7 at a, and adding 5 sets it past z.
// Neither addition can carry between bytes. Exclude original non-ASCII bytes
// before moving the selected high bits into the case bit.
func asciiLetterCaseBits(word uint64) uint64 {
	folded := (word | 0x2020202020202020) & 0x7f7f7f7f7f7f7f7f
	letters := (folded + 0x1f1f1f1f1f1f1f1f) &^ (folded + 0x0505050505050505)
	return (letters &^ word & 0x8080808080808080) >> 2
}

func asciiUpcaseWord(text string) string {
	out := make([]byte, len(text))
	i := 0
	for ; i+8 <= len(text); i += 8 {
		var tile [8]byte
		copy(tile[:], text[i:i+8])
		word := binary.LittleEndian.Uint64(tile[:])
		binary.LittleEndian.PutUint64(out[i:i+8], word&^asciiLetterCaseBits(word))
	}
	for ; i < len(text); i++ {
		out[i] = asciiUpper(text[i])
	}
	return string(out)
}

func asciiDowncaseWord(text string) string {
	out := make([]byte, len(text))
	i := 0
	for ; i+8 <= len(text); i += 8 {
		var tile [8]byte
		copy(tile[:], text[i:i+8])
		word := binary.LittleEndian.Uint64(tile[:])
		binary.LittleEndian.PutUint64(out[i:i+8], word|asciiLetterCaseBits(word))
	}
	for ; i < len(text); i++ {
		out[i] = asciiLower(text[i])
	}
	return string(out)
}

// asciiCapitalizeWord uppercases the first byte and lowercases the rest. The caller
// handles empty strings; non-ASCII bytes remain unchanged.
func asciiCapitalizeWord(text string) string {
	out := make([]byte, len(text))
	i := 0
	for ; i+8 <= len(text); i += 8 {
		var tile [8]byte
		copy(tile[:], text[i:i+8])
		word := binary.LittleEndian.Uint64(tile[:])
		binary.LittleEndian.PutUint64(out[i:i+8], word|asciiLetterCaseBits(word))
	}
	for ; i < len(text); i++ {
		out[i] = asciiLower(text[i])
	}
	out[0] = asciiUpper(text[0])
	return string(out)
}

func asciiSwapCaseWord(text string) string {
	out := make([]byte, len(text))
	i := 0
	for ; i+8 <= len(text); i += 8 {
		var tile [8]byte
		copy(tile[:], text[i:i+8])
		word := binary.LittleEndian.Uint64(tile[:])
		binary.LittleEndian.PutUint64(out[i:i+8], word^asciiLetterCaseBits(word))
	}
	for ; i < len(text); i++ {
		out[i] = asciiSwapCaseByte(text[i])
	}
	return string(out)
}

func asciiSwapCaseByte(c byte) byte {
	switch {
	case c >= 'A' && c <= 'Z':
		return c + ('a' - 'A')
	case c >= 'a' && c <= 'z':
		return c - ('a' - 'A')
	default:
		return c
	}
}
