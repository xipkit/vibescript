//go:build !go1.27 || !goexperiment.simd || (!arm64 && !amd64)

package runtime

func asciiCaseCompareImpl(a, b string) int {
	return asciiCaseCompareWords(a, b)
}

func asciiCaseEqualImpl(a, b string) bool {
	return asciiCaseEqualWords(a, b)
}
