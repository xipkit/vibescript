//go:build !go1.27 || !goexperiment.simd || !arm64

package runtime

func asciiUpcase(text string) string {
	return asciiUpcaseWord(text)
}

func asciiDowncase(text string) string {
	return asciiDowncaseWord(text)
}

func asciiSwapCase(text string) string {
	return asciiSwapCaseWord(text)
}

func asciiCapitalize(text string) string {
	return asciiCapitalizeWord(text)
}
