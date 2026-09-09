package runtime

const jsonASCIISpanMin = 16

func jsonParseASCIISpanScalar(text string) int {
	for i := range len(text) {
		c := text[i]
		if c < 0x20 || c >= 0x80 || c == '"' || c == '\\' {
			return i
		}
	}
	return len(text)
}

func jsonStringifyASCIISpanScalar(text string) int {
	for i := range len(text) {
		c := text[i]
		if c < 0x20 || c >= 0x80 || c == '"' || c == '\\' || c == '<' || c == '>' || c == '&' {
			return i
		}
	}
	return len(text)
}
