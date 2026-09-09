//go:build go1.27 && goexperiment.simd

package runtime

import "simd/archsimd"

func whitespaceSIMDSupported() bool {
	return archsimd.X86.AVX2()
}

func whitespaceMaskSIMD(v archsimd.Uint8x16, strip bool) archsimd.Mask8x16 {
	signed := v.BitsToInt8()
	mask := signed.Greater(archsimd.BroadcastInt8x16(8)).And(signed.Less(archsimd.BroadcastInt8x16(14)))
	mask = mask.Or(v.Equal(archsimd.BroadcastUint8x16(' ')))
	if strip {
		mask = mask.Or(v.Equal(archsimd.BroadcastUint8x16(0)))
	}
	return mask
}

func allWhitespaceSIMD(v archsimd.Uint8x16, strip bool) bool {
	return whitespaceMaskSIMD(v, strip).ToBits() == 0xffff
}

func anyWhitespaceSIMD(v archsimd.Uint8x16) bool {
	return whitespaceMaskSIMD(v, false).ToBits() != 0
}
