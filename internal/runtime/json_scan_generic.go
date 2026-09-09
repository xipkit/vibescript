//go:build !go1.27 || !goexperiment.simd || (!arm64 && !amd64)

package runtime

func jsonParseASCIISpan(text string) int {
	return jsonParseASCIISpanScalar(text)
}

func jsonStringifyASCIISpan(text string) int {
	return jsonStringifyASCIISpanScalar(text)
}
