//go:build go1.27 && goexperiment.simd && (arm64 || amd64)

package runtime

import "simd/archsimd"

const whitespaceSIMD = true

func whitespacePrefixSIMD(text string, strip bool) int {
	if !whitespaceSIMDSupported() {
		return whitespacePrefixScalar(text, strip)
	}
	i := 0
	for len(text)-i >= 16 {
		var tile [16]byte
		copy(tile[:], text[i:i+16])
		if !allWhitespaceSIMD(archsimd.LoadUint8x16Array(&tile), strip) {
			break
		}
		i += 16
	}
	return i + whitespacePrefixScalar(text[i:], strip)
}

func nonWhitespacePrefixSIMD(text string) int {
	if !whitespaceSIMDSupported() {
		return nonWhitespacePrefixScalar(text)
	}
	i := 0
	for len(text)-i >= 16 {
		var tile [16]byte
		copy(tile[:], text[i:i+16])
		if anyWhitespaceSIMD(archsimd.LoadUint8x16Array(&tile)) {
			break
		}
		i += 16
	}
	return i + nonWhitespacePrefixScalar(text[i:])
}

func whitespaceSuffixSIMD(text string, strip bool) int {
	if !whitespaceSIMDSupported() {
		return whitespaceSuffixScalar(text, strip)
	}
	i := 0
	for len(text)-i >= 16 {
		var tile [16]byte
		copy(tile[:], text[len(text)-i-16:len(text)-i])
		if !allWhitespaceSIMD(archsimd.LoadUint8x16Array(&tile), strip) {
			break
		}
		i += 16
	}
	return i + whitespaceSuffixScalar(text[:len(text)-i], strip)
}
