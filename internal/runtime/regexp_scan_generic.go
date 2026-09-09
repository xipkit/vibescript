//go:build !go1.27 || !goexperiment.simd || !arm64

package runtime

func regexpQuotedSize(text string, limit int) (int, bool) {
	return regexpQuotedSizePortable(text, limit)
}
