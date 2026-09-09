//go:build go1.27 && goexperiment.simd && amd64

package runtime

import (
	"math/bits"
	"simd/archsimd"
)

func asciiCaseCompareImpl(a, b string) int {
	limit := min(len(a), len(b))
	// The 128-bit byte broadcasts in archsimd require AVX2.
	if limit < 20 || !archsimd.X86.AVX2() {
		return asciiCaseCompareWords(a, b)
	}
	for i := range 4 {
		ca, cb := asciiLower(a[i]), asciiLower(b[i])
		if ca != cb {
			if ca < cb {
				return -1
			}
			return 1
		}
	}
	beforeA := archsimd.BroadcastInt8x16('A' - 1)
	afterZ := archsimd.BroadcastInt8x16('Z' + 1)
	foldBit := archsimd.BroadcastUint8x16('a' - 'A')
	i := 4
	for ; limit-i >= 16; i += 16 {
		var at, bt [16]byte
		copy(at[:], a[i:i+16])
		copy(bt[:], b[i:i+16])
		av, bv := archsimd.LoadUint8x16Array(&at), archsimd.LoadUint8x16Array(&bt)
		as, bs := av.BitsToInt8(), bv.BitsToInt8()
		am := as.Greater(beforeA).And(afterZ.Greater(as))
		bm := bs.Greater(beforeA).And(afterZ.Greater(bs))
		av = av.Or(am.ToInt8x16().ToBits().And(foldBit))
		bv = bv.Or(bm.ToInt8x16().ToBits().And(foldBit))
		if equal := av.Equal(bv).ToBits(); equal != 0xffff {
			j := bits.TrailingZeros16(^equal)
			if asciiLower(a[i+j]) < asciiLower(b[i+j]) {
				return -1
			}
			return 1
		}
	}
	return asciiCaseCompareWords(a[i:], b[i:])
}

func asciiCaseEqualImpl(a, b string) bool {
	if len(a) != len(b) {
		return false
	}
	if len(a) < 20 || !archsimd.X86.AVX2() {
		return asciiCaseEqualWords(a, b)
	}
	for i := range 4 {
		if asciiLower(a[i]) != asciiLower(b[i]) {
			return false
		}
	}
	beforeA := archsimd.BroadcastInt8x16('A' - 1)
	afterZ := archsimd.BroadcastInt8x16('Z' + 1)
	foldBit := archsimd.BroadcastUint8x16('a' - 'A')
	i := 4
	for ; len(a)-i >= 16; i += 16 {
		var at, bt [16]byte
		copy(at[:], a[i:i+16])
		copy(bt[:], b[i:i+16])
		av, bv := archsimd.LoadUint8x16Array(&at), archsimd.LoadUint8x16Array(&bt)
		as, bs := av.BitsToInt8(), bv.BitsToInt8()
		am := as.Greater(beforeA).And(afterZ.Greater(as))
		bm := bs.Greater(beforeA).And(afterZ.Greater(bs))
		av = av.Or(am.ToInt8x16().ToBits().And(foldBit))
		bv = bv.Or(bm.ToInt8x16().ToBits().And(foldBit))
		if av.Equal(bv).ToBits() != 0xffff {
			return false
		}
	}
	return asciiCaseEqualWords(a[i:], b[i:])
}
