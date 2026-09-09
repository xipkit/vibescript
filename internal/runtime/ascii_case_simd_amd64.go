//go:build go1.27 && goexperiment.simd && amd64

package runtime

import "simd/archsimd"

func asciiUpcase(text string) string {
	if len(text) < asciiCaseSIMDMinBytes || len(text) > asciiCaseSIMDMaxBytes || !archsimd.X86.AVX2() {
		return asciiUpcaseWord(text)
	}
	return asciiUpcaseSIMD(text)
}

func asciiUpcaseSIMD(text string) string {
	out := make([]byte, len(text))
	delta := archsimd.BroadcastUint8x32('a' - 'A')
	// Bias the letter interval to -128..-103 for AVX2's signed comparison.
	low := archsimd.BroadcastUint8x32('a' + 128)
	limit := archsimd.BroadcastInt8x32(-102)
	i := 0
	for ; i+32 <= len(text); i += 32 {
		var tile [32]byte
		copy(tile[:], text[i:i+32])
		value := archsimd.LoadUint8x32Array(&tile)
		letters := value.Sub(low).AsInt8x32().Less(limit)
		value.AndNot(delta.Masked(letters)).Store(out[i : i+32])
	}
	for ; i < len(text); i++ {
		out[i] = asciiUpper(text[i])
	}
	return string(out)
}

func asciiDowncase(text string) string {
	if len(text) < asciiCaseSIMDMinBytes || len(text) > asciiCaseSIMDMaxBytes || !archsimd.X86.AVX2() {
		return asciiDowncaseWord(text)
	}
	return asciiDowncaseSIMD(text)
}

func asciiDowncaseSIMD(text string) string {
	out := make([]byte, len(text))
	delta := archsimd.BroadcastUint8x32('a' - 'A')
	// Bias the letter interval to -128..-103 for AVX2's signed comparison.
	low := archsimd.BroadcastUint8x32('A' + 128)
	limit := archsimd.BroadcastInt8x32(-102)
	i := 0
	for ; i+32 <= len(text); i += 32 {
		var tile [32]byte
		copy(tile[:], text[i:i+32])
		value := archsimd.LoadUint8x32Array(&tile)
		letters := value.Sub(low).AsInt8x32().Less(limit)
		value.Or(delta.Masked(letters)).Store(out[i : i+32])
	}
	for ; i < len(text); i++ {
		out[i] = asciiLower(text[i])
	}
	return string(out)
}

func asciiSwapCase(text string) string {
	if len(text) < asciiCaseSIMDMinBytes || len(text) > asciiCaseSIMDMaxBytes || !archsimd.X86.AVX2() {
		return asciiSwapCaseWord(text)
	}
	return asciiSwapCaseSIMD(text)
}

func asciiSwapCaseSIMD(text string) string {
	out := make([]byte, len(text))
	delta := archsimd.BroadcastUint8x32('a' - 'A')
	// Bias the letter interval to -128..-103 for AVX2's signed comparison.
	low := archsimd.BroadcastUint8x32('a' + 128)
	limit := archsimd.BroadcastInt8x32(-102)
	i := 0
	for ; i+32 <= len(text); i += 32 {
		var tile [32]byte
		copy(tile[:], text[i:i+32])
		value := archsimd.LoadUint8x32Array(&tile)
		letters := value.Or(delta).Sub(low).AsInt8x32().Less(limit)
		value.Xor(delta.Masked(letters)).Store(out[i : i+32])
	}
	for ; i < len(text); i++ {
		out[i] = asciiSwapCaseByte(text[i])
	}
	return string(out)
}

func asciiCapitalize(text string) string {
	if len(text) < asciiCaseSIMDMinBytes || len(text) > asciiCaseSIMDMaxBytes || !archsimd.X86.AVX2() {
		return asciiCapitalizeWord(text)
	}
	return asciiCapitalizeSIMD(text)
}

func asciiCapitalizeSIMD(text string) string {
	out := make([]byte, len(text))
	delta := archsimd.BroadcastUint8x32('a' - 'A')
	// Bias the letter interval to -128..-103 for AVX2's signed comparison.
	low := archsimd.BroadcastUint8x32('A' + 128)
	limit := archsimd.BroadcastInt8x32(-102)
	i := 0
	for ; i+32 <= len(text); i += 32 {
		var tile [32]byte
		copy(tile[:], text[i:i+32])
		value := archsimd.LoadUint8x32Array(&tile)
		letters := value.Sub(low).AsInt8x32().Less(limit)
		value.Or(delta.Masked(letters)).Store(out[i : i+32])
	}
	for ; i < len(text); i++ {
		out[i] = asciiLower(text[i])
	}
	out[0] = asciiUpper(text[0])
	return string(out)
}
