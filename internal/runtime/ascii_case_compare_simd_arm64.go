//go:build go1.27 && goexperiment.simd && arm64

package runtime

import "simd/archsimd"

func asciiCaseCompareImpl(a, b string) int {
	limit := min(len(a), len(b))
	if limit < 20 {
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
	upperA := archsimd.BroadcastUint8x16('A')
	upperSpan := archsimd.BroadcastUint8x16('Z' - 'A')
	foldBit := archsimd.BroadcastUint8x16('a' - 'A')
	i := 4
	for ; limit-i >= 16; i += 16 {
		var at, bt [16]byte
		copy(at[:], a[i:i+16])
		copy(bt[:], b[i:i+16])
		av, bv := archsimd.LoadUint8x16Array(&at), archsimd.LoadUint8x16Array(&bt)
		am := av.Sub(upperA).LessEqual(upperSpan)
		bm := bv.Sub(upperA).LessEqual(upperSpan)
		av = av.Or(am.ToInt8x16().ToBits().And(foldBit))
		bv = bv.Or(bm.ToInt8x16().ToBits().And(foldBit))
		if av.NotEqual(bv).ToInt8x16().ToBits().ReduceMax() != 0 {
			return asciiCaseCompareWords(a[i:i+16], b[i:i+16])
		}
	}
	return asciiCaseCompareWords(a[i:], b[i:])
}

func asciiCaseEqualImpl(a, b string) bool {
	if len(a) != len(b) {
		return false
	}
	if len(a) < 20 {
		return asciiCaseEqualWords(a, b)
	}
	for i := range 4 {
		if asciiLower(a[i]) != asciiLower(b[i]) {
			return false
		}
	}
	upperA := archsimd.BroadcastUint8x16('A')
	upperSpan := archsimd.BroadcastUint8x16('Z' - 'A')
	foldBit := archsimd.BroadcastUint8x16('a' - 'A')
	i := 4
	for ; len(a)-i >= 16; i += 16 {
		var at, bt [16]byte
		copy(at[:], a[i:i+16])
		copy(bt[:], b[i:i+16])
		av, bv := archsimd.LoadUint8x16Array(&at), archsimd.LoadUint8x16Array(&bt)
		am := av.Sub(upperA).LessEqual(upperSpan)
		bm := bv.Sub(upperA).LessEqual(upperSpan)
		av = av.Or(am.ToInt8x16().ToBits().And(foldBit))
		bv = bv.Or(bm.ToInt8x16().ToBits().And(foldBit))
		if av.NotEqual(bv).ToInt8x16().ToBits().ReduceMax() != 0 {
			return false
		}
	}
	return asciiCaseEqualWords(a[i:], b[i:])
}
