//go:build go1.27 && goexperiment.simd && arm64

package runtime

import "simd/archsimd"

func asciiUpcase(text string) string {
	if len(text) < asciiCaseSIMDMinBytes || len(text) > asciiCaseSIMDMaxBytes {
		return asciiUpcaseWord(text)
	}
	return asciiUpcaseSIMD(text)
}

func asciiUpcaseSIMD(text string) string {
	out := make([]byte, len(text))
	delta := archsimd.BroadcastUint8x16('a' - 'A')
	low := archsimd.BroadcastUint8x16('a')
	width := archsimd.BroadcastUint8x16(26)
	i := 0
	for ; i+16 <= len(text); i += 16 {
		var tile [16]byte
		copy(tile[:], text[i:i+16])
		value := archsimd.LoadUint8x16Array(&tile)
		letters := value.Sub(low).Less(width)
		value.AndNot(delta.Masked(letters)).Store(out[i : i+16])
	}
	for ; i < len(text); i++ {
		out[i] = asciiUpper(text[i])
	}
	return string(out)
}

func asciiDowncase(text string) string {
	if len(text) < asciiCaseSIMDMinBytes || len(text) > asciiCaseSIMDMaxBytes {
		return asciiDowncaseWord(text)
	}
	return asciiDowncaseSIMD(text)
}

func asciiDowncaseSIMD(text string) string {
	out := make([]byte, len(text))
	delta := archsimd.BroadcastUint8x16('a' - 'A')
	low := archsimd.BroadcastUint8x16('A')
	width := archsimd.BroadcastUint8x16(26)
	i := 0
	for ; i+16 <= len(text); i += 16 {
		var tile [16]byte
		copy(tile[:], text[i:i+16])
		value := archsimd.LoadUint8x16Array(&tile)
		letters := value.Sub(low).Less(width)
		value.Or(delta.Masked(letters)).Store(out[i : i+16])
	}
	for ; i < len(text); i++ {
		out[i] = asciiLower(text[i])
	}
	return string(out)
}

func asciiSwapCase(text string) string {
	if len(text) < asciiCaseSIMDMinBytes || len(text) > asciiCaseSIMDMaxBytes {
		return asciiSwapCaseWord(text)
	}
	return asciiSwapCaseSIMD(text)
}

func asciiSwapCaseSIMD(text string) string {
	out := make([]byte, len(text))
	delta := archsimd.BroadcastUint8x16('a' - 'A')
	low := archsimd.BroadcastUint8x16('a')
	width := archsimd.BroadcastUint8x16(26)
	i := 0
	for ; i+16 <= len(text); i += 16 {
		var tile [16]byte
		copy(tile[:], text[i:i+16])
		value := archsimd.LoadUint8x16Array(&tile)
		letters := value.Or(delta).Sub(low).Less(width)
		value.Xor(delta.Masked(letters)).Store(out[i : i+16])
	}
	for ; i < len(text); i++ {
		out[i] = asciiSwapCaseByte(text[i])
	}
	return string(out)
}

func asciiCapitalize(text string) string {
	if len(text) < asciiCaseSIMDMinBytes || len(text) > asciiCaseSIMDMaxBytes {
		return asciiCapitalizeWord(text)
	}
	return asciiCapitalizeSIMD(text)
}

func asciiCapitalizeSIMD(text string) string {
	out := make([]byte, len(text))
	delta := archsimd.BroadcastUint8x16('a' - 'A')
	low := archsimd.BroadcastUint8x16('A')
	width := archsimd.BroadcastUint8x16(26)
	i := 0
	for ; i+16 <= len(text); i += 16 {
		var tile [16]byte
		copy(tile[:], text[i:i+16])
		value := archsimd.LoadUint8x16Array(&tile)
		letters := value.Sub(low).Less(width)
		value.Or(delta.Masked(letters)).Store(out[i : i+16])
	}
	for ; i < len(text); i++ {
		out[i] = asciiLower(text[i])
	}
	out[0] = asciiUpper(text[0])
	return string(out)
}
