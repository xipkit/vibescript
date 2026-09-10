package runtime

import "unicode/utf8"

// Keep the decoding loop out of member closures with large argument frames.
//
//go:noinline
func stringRuneCount(text string) int {
	return utf8.RuneCountInString(text)
}
