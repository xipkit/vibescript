//go:build !amd64 && (!go1.27 || !goexperiment.simd || !arm64)

package runtime

func stringIsASCII(text string) bool {
	if len(text) > 1 && text[0]|text[1] >= 128 {
		return false
	}
	return stringIsASCIIWords(text)
}
