//go:build go1.27 && goexperiment.simd

package runtime

import "simd/archsimd"

func jsonParseASCIISpan(text string) int {
	i := 0
	if len(text) >= 16 {
		if b := text[0]; b < 0x20 || b >= 0x80 || b == '"' || b == '\\' {
			return 0
		}
		space := archsimd.BroadcastInt8x16(0x20)
		quote := archsimd.BroadcastUint8x16('"')
		slash := archsimd.BroadcastUint8x16('\\')
		var tile [16]byte
		for len(text)-i >= len(tile) {
			copy(tile[:], text[i:i+len(tile)])
			v := archsimd.LoadUint8x16Array(&tile)
			// Negative signed lanes cover every non-ASCII byte.
			special := v.BitsToInt8().Less(space).Or(v.Equal(quote)).Or(v.Equal(slash))
			if special.ToInt8x16().ToBits().ReduceMax() != 0 {
				break
			}
			i += len(tile)
		}
	}
	return i + jsonParseASCIISpanScalar(text[i:])
}

func jsonStringifyASCIISpan(text string) int {
	i := 0
	if len(text) >= 16 {
		if b := text[0]; b < 0x20 || b >= 0x80 || b == '"' || b == '\\' || b == '<' || b == '>' || b == '&' {
			return 0
		}
		space := archsimd.BroadcastInt8x16(0x20)
		quote := archsimd.BroadcastUint8x16('"')
		slash := archsimd.BroadcastUint8x16('\\')
		less := archsimd.BroadcastUint8x16('<')
		greater := archsimd.BroadcastUint8x16('>')
		amp := archsimd.BroadcastUint8x16('&')
		var tile [16]byte
		for len(text)-i >= len(tile) {
			copy(tile[:], text[i:i+len(tile)])
			v := archsimd.LoadUint8x16Array(&tile)
			special := v.BitsToInt8().Less(space).Or(v.Equal(quote)).Or(v.Equal(slash))
			special = special.Or(v.Equal(less)).Or(v.Equal(greater)).Or(v.Equal(amp))
			if special.ToInt8x16().ToBits().ReduceMax() != 0 {
				break
			}
			i += len(tile)
		}
	}
	return i + jsonStringifyASCIISpanScalar(text[i:])
}
