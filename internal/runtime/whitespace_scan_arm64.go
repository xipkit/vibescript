//go:build go1.27 && goexperiment.simd

package runtime

import "simd/archsimd"

func whitespaceSIMDSupported() bool {
	return true
}

func whitespaceMaskSIMD(v archsimd.Uint8x16, strip bool) archsimd.Mask8x16 {
	mask := v.Sub(archsimd.BroadcastUint8x16(9)).LessEqual(archsimd.BroadcastUint8x16(4))
	mask = mask.Or(v.Equal(archsimd.BroadcastUint8x16(' ')))
	if strip {
		mask = mask.Or(v.Equal(archsimd.BroadcastUint8x16(0)))
	}
	return mask
}

func allWhitespaceSIMD(v archsimd.Uint8x16, strip bool) bool {
	return whitespaceMaskSIMD(v, strip).ToInt8x16().ToBits().ReduceMin() == 255
}

func anyWhitespaceSIMD(v archsimd.Uint8x16) bool {
	return whitespaceMaskSIMD(v, false).ToInt8x16().ToBits().ReduceMax() != 0
}
