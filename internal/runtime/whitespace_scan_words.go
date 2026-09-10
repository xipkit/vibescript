package runtime

import "encoding/binary"

// zeroByteBits marks zero bytes without borrowing into neighboring lanes.
func zeroByteBits(word uint64) uint64 {
	const low = 0x7f7f7f7f7f7f7f7f
	return ^(((word & low) + low) | word | low)
}

func whitespaceByteBits(word uint64, strip bool) uint64 {
	low := word & 0x7f7f7f7f7f7f7f7f
	// The additions stay within each byte. Their high bits select 9 through 13.
	mask := (low + 0x7777777777777777) &^ (low + 0x7272727272727272) &^ word & 0x8080808080808080
	mask |= zeroByteBits(word ^ 0x2020202020202020)
	if strip {
		mask |= zeroByteBits(word)
	}
	return mask
}

func whitespacePrefixWords(text string, strip bool) int {
	i := 0
	for ; len(text)-i >= 8; i += 8 {
		word := binary.LittleEndian.Uint64([]byte(text[i : i+8]))
		if whitespaceByteBits(word, strip) != 0x8080808080808080 {
			break
		}
	}
	return i + whitespacePrefixScalar(text[i:i+min(8, len(text)-i)], strip)
}

func nonWhitespacePrefixWords(text string) int {
	i := 0
	for ; len(text)-i >= 8; i += 8 {
		word := binary.LittleEndian.Uint64([]byte(text[i : i+8]))
		if whitespaceByteBits(word, false) != 0 {
			break
		}
	}
	return i + nonWhitespacePrefixScalar(text[i:i+min(8, len(text)-i)])
}

func whitespaceSuffixWords(text string, strip bool) int {
	i := len(text)
	for ; i >= 8; i -= 8 {
		word := binary.LittleEndian.Uint64([]byte(text[i-8 : i]))
		if whitespaceByteBits(word, strip) != 0x8080808080808080 {
			break
		}
	}
	return len(text) - i + whitespaceSuffixScalar(text[max(0, i-8):i], strip)
}
