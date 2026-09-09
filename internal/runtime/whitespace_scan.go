package runtime

// Scalar probes keep vector setup out of short fields and lightly padded edges.
const whitespaceProbeBytes = 64

func whitespacePrefixScalar(text string, strip bool) int {
	i := 0
	for i < len(text) && (isRubyASCIISpace(text[i]) || strip && text[i] == 0) {
		i++
	}
	return i
}

func nonWhitespacePrefixScalar(text string) int {
	i := 0
	for i < len(text) && !isRubyASCIISpace(text[i]) {
		i++
	}
	return i
}

func whitespaceSuffixScalar(text string, strip bool) int {
	i := len(text)
	for i > 0 && (isRubyASCIISpace(text[i-1]) || strip && text[i-1] == 0) {
		i--
	}
	return len(text) - i
}
