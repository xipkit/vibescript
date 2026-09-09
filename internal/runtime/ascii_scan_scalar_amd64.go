//go:build !go1.27 || !goexperiment.simd

package runtime

// Keep this wrapper small enough for stringRuneLen to inline on AMD64.
func stringIsASCII(text string) bool {
	return stringIsASCIIWords(text)
}
