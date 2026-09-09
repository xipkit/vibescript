//go:build !go1.27 || !goexperiment.simd || (!arm64 && !amd64)

package runtime

const whitespaceSIMD = false

func whitespaceSIMDSupported() bool {
	return false
}

func whitespacePrefixSIMD(text string, strip bool) int {
	return whitespacePrefixScalar(text, strip)
}

func nonWhitespacePrefixSIMD(text string) int {
	return nonWhitespacePrefixScalar(text)
}

func whitespaceSuffixSIMD(text string, strip bool) int {
	return whitespaceSuffixScalar(text, strip)
}
