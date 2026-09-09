package runtime

// Scalar probes keep vector setup out of short fields and lightly padded edges.
const whitespaceProbeBytes = 64

func whitespacePrefixScalar(text string, strip bool) int {
	i := 0
	if strip {
		for i < len(text) && isRubyStripSpace(text[i]) {
			i++
		}
		return i
	}
	for i < len(text) && isRubyASCIISpace(text[i]) {
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
	if strip {
		for i > 0 && isRubyStripSpace(text[i-1]) {
			i--
		}
		return len(text) - i
	}
	for i > 0 && isRubyASCIISpace(text[i-1]) {
		i--
	}
	return len(text) - i
}
