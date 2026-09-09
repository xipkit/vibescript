//go:build go1.27 && goexperiment.simd && arm64

package runtime

import (
	"encoding/binary"
	"simd/archsimd"
)

func regexpQuotedSize(text string, limit int) (int, bool) {
	if len(text) < 128 {
		return regexpQuotedSizePortable(text, limit)
	}
	if len(text) > limit {
		return 0, false
	}
	// Each low nibble selects permitted high-nibble bits. Bytes above ASCII
	// select zero from the high table and remain literal, including invalid UTF-8.
	lowTable := [16]byte{0, 0, 0, 0, 4, 0, 0, 0, 4, 4, 4, 164, 160, 160, 36, 8}
	highTable := [16]byte{1, 2, 4, 8, 16, 32, 64, 128}
	low := archsimd.LoadUint8x16Array(&lowTable)
	high := archsimd.LoadUint8x16Array(&highTable)
	nibble := archsimd.BroadcastUint8x16(15)
	zero := archsimd.BroadcastUint8x16(0)
	one := archsimd.BroadcastUint8x16(1)
	size := len(text)
	i := 0
	for ; len(text)-i >= 128; i += 128 {
		sum := zero
		// Reduce after 128 bytes: the byte-sized sum cannot overflow even
		// when every byte requires escaping.
		for j := range 8 {
			v := regexpLoadBytes(text[i+j*16 : i+j*16+16])
			matches := low.LookupOrZero(v.And(nibble)).And(high.LookupOrZero(v.ShiftAllRight(4)))
			sum = sum.Add(matches.NotEqual(zero).ToInt8x16().ToBits().And(one))
		}
		count := int(sum.ReduceSum())
		if count > limit-size {
			return 0, false
		}
		size += count
	}
	for ; i < len(text); i++ {
		if regexpMetaByte(text[i]) {
			if size == limit {
				return 0, false
			}
			size++
		}
	}
	return size, true
}

// regexpLoadBytes constructs the vector from bounded word loads without a stack tile.
func regexpLoadBytes(text string) archsimd.Uint8x16 {
	var words archsimd.Uint64x2
	words = words.SetElem(0, binary.LittleEndian.Uint64([]byte(text[:8])))
	words = words.SetElem(1, binary.LittleEndian.Uint64([]byte(text[8:16])))
	return words.ReshapeToUint8s()
}
