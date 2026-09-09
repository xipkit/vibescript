//go:build go1.27 && goexperiment.simd

package runtime

import "simd/archsimd"

func stringIsASCIIVector(text string) bool {
	if len(text) < 64 || !archsimd.X86.AVX2() {
		return stringIsASCIIWords(text)
	}
	if text[0] >= 128 {
		return false
	}
	var zero archsimd.Int8x16
	i := 0
	for ; len(text)-i >= 16; i += 16 {
		var block [16]byte
		copy(block[:], text[i:i+16])
		if archsimd.LoadUint8x16Array(&block).BitsToInt8().Less(zero).ToBits() != 0 {
			return false
		}
	}
	return stringIsASCIIWords(text[i:])
}
