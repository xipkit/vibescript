package runtime

import (
	"fmt"
	"regexp"
	"strings"
	"unicode"
	"unicode/utf8"
)

// errRegexOutputLimit reports that an expansion would push the result past the
// shared regex output-size guard. Callers wrap it with their method name so the
// surfaced message matches the rest of the regex output guards.
var errRegexOutputLimit = guardLimitErrorf("output exceeds limit %d bytes", maxRegexInputBytes)

// appendRegexReplacement preserves regexp.ExpandString's dollar-reference
// syntax while checking each literal and capture before growing dst.
func appendRegexReplacement(dst []byte, re *regexp.Regexp, template, src string, loc []int) ([]byte, error) {
	for {
		literal, remaining, found := strings.Cut(template, "$")
		var err error
		dst, err = appendBounded(dst, literal)
		if err != nil || !found {
			return dst, err
		}
		template = remaining
		expansion := "$"
		if strings.HasPrefix(template, "$") {
			template = template[1:]
		} else if name, rest, ok := regexReplacementName(template); ok {
			template = rest
			expansion = ""
			index := -1
			// ExpandString treats leading zeros and numbers over nine digits
			// as names. The numeric range fits int on every supported target.
			if len(name) <= 9 && (len(name) == 1 || name[0] != '0') {
				index = 0
				for i := range len(name) {
					if name[i] < '0' || name[i] > '9' {
						index = -1
						break
					}
					index = index*10 + int(name[i]-'0')
				}
			}
			if index >= 0 {
				if index < len(loc)/2 && loc[2*index] >= 0 {
					expansion = src[loc[2*index]:loc[2*index+1]]
				}
			} else {
				for i, candidate := range re.SubexpNames() {
					if candidate == name && i < len(loc)/2 && loc[2*i] >= 0 {
						expansion = src[loc[2*i]:loc[2*i+1]]
						break
					}
				}
			}
		}
		dst, err = appendBounded(dst, expansion)
		if err != nil {
			return nil, err
		}
	}
}

func regexReplacementName(template string) (string, string, bool) {
	name := template
	braced := strings.HasPrefix(name, "{")
	if braced {
		name = name[1:]
	}
	end := len(name)
	for i, r := range name {
		if !unicode.IsLetter(r) && !unicode.IsDigit(r) && r != '_' {
			end = i
			break
		}
	}
	if end == 0 {
		return "", "", false
	}
	rest := name[end:]
	if braced {
		if !strings.HasPrefix(rest, "}") {
			return "", "", false
		}
		rest = rest[1:]
	}
	return name[:end], rest, true
}

// rubyAppendReplacement expands a Ruby-style replacement template against a
// single match and appends the result to dst, mirroring the substitution rules
// of Ruby's String#sub and String#gsub. It is the Ruby counterpart to Go's
// regexp.Regexp.ExpandString, which uses the foreign "$1"/"${name}" syntax.
//
// Recognized escapes (every other character, including "$", is copied
// verbatim):
//
//	\0, \&    the entire match
//	\1 .. \9  the corresponding capture group (single digit only, like Ruby)
//	\`        the text preceding the match (pre-match)
//	\'        the text following the match (post-match)
//	\+        the last capture group that participated in the match
//	\k<name>  the named capture group "name"
//	\\        a literal backslash
//
// A backslash that introduces any other sequence (including a trailing
// backslash at the end of the template) is preserved literally together with
// the character it precedes, matching Ruby. Numbered or "\+" references to
// groups that did not participate in the match expand to the empty string,
// again as Ruby does. A "\k" that names a group the pattern does not define is
// an error, as is a "\k<" that is never closed, matching Ruby's
// IndexError/RuntimeError on the same inputs.
//
// Ruby disables numbered backreferences (\1 .. \9) once the pattern defines any
// named capture group: with names present they always expand to the empty
// string, even for groups that did participate. For example
// "John Smith".sub(/(?<first>\w+) (?<last>\w+)/, "\\2, \\1") yields ", " in
// Ruby. The whole-match refs (\0, \&), the pre/post-match refs (\`, \'), and
// the named ref (\k<name>) keep working in that mode; only the numbered group
// refs go empty. With no named captures, numbered refs work normally.
//
// Every append is bounded by maxRegexInputBytes before it runs, so a hostile
// template (for example many "\`"/"\'" escapes against a near-limit subject)
// fails with errRegexOutputLimit instead of transiently allocating past the
// guard.
//
// loc holds submatch byte indices in the form returned by
// FindStringSubmatchIndex: loc[0:2] are the whole match, and loc[2*i:2*i+2] are
// capture group i (negative when the group did not participate).
func rubyAppendReplacement(dst []byte, re *regexp.Regexp, template, src string, loc []int) ([]byte, error) {
	// Ruby suppresses numbered backreferences (\1 .. \9) whenever the pattern
	// defines any named capture, expanding them to the empty string instead.
	hasNamed := regexHasNamedCapture(re)
	for i := 0; i < len(template); i++ {
		c := template[i]
		if c != '\\' {
			next, err := appendBounded(dst, template[i:i+1])
			if err != nil {
				return nil, err
			}
			dst = next
			continue
		}
		if i+1 >= len(template) {
			// Trailing backslash: Ruby keeps it literally.
			next, err := appendBounded(dst, "\\")
			if err != nil {
				return nil, err
			}
			dst = next
			break
		}
		next := template[i+1]
		switch {
		case next >= '1' && next <= '9':
			// Numbered group ref. Ruby leaves these empty when the pattern has
			// any named capture; otherwise they expand to the indexed submatch.
			if !hasNamed {
				expanded, err := appendRubySubmatch(dst, src, loc, int(next-'0'))
				if err != nil {
					return nil, err
				}
				dst = expanded
			}
			i++
		case next == '0':
			// \0 is the whole match and keeps working even with named captures.
			expanded, err := appendRubySubmatch(dst, src, loc, 0)
			if err != nil {
				return nil, err
			}
			dst = expanded
			i++
		case next == '&':
			expanded, err := appendRubySubmatch(dst, src, loc, 0)
			if err != nil {
				return nil, err
			}
			dst = expanded
			i++
		case next == '`':
			expanded, err := appendBounded(dst, src[:loc[0]])
			if err != nil {
				return nil, err
			}
			dst = expanded
			i++
		case next == '\'':
			expanded, err := appendBounded(dst, src[loc[1]:])
			if err != nil {
				return nil, err
			}
			dst = expanded
			i++
		case next == '+':
			expanded, err := appendRubyLastGroup(dst, re, src, loc)
			if err != nil {
				return nil, err
			}
			dst = expanded
			i++
		case next == '\\':
			expanded, err := appendBounded(dst, "\\")
			if err != nil {
				return nil, err
			}
			dst = expanded
			i++
		case next == 'k' && i+2 < len(template) && template[i+2] == '<':
			expanded, nameEnd, err := appendRubyNamedGroup(dst, re, src, loc, template[i+3:])
			if err != nil {
				return nil, err
			}
			dst = expanded
			// Advance past "\k<", the name, and the closing ">". nameEnd is the
			// index of ">" within template[i+3:]; the loop's own i++ steps past
			// it, so add only up to and including the closing bracket.
			i += 3 + nameEnd
		default:
			// Unknown escape (including "\k" not followed by "<"): Ruby keeps the
			// backslash and the following character literally.
			expanded, err := appendBounded(dst, template[i:i+2])
			if err != nil {
				return nil, err
			}
			dst = expanded
			i++
		}
	}
	return dst, nil
}

// appendBounded appends s to dst but first verifies the result stays within
// maxRegexInputBytes, returning errRegexOutputLimit otherwise. It guards every
// expansion in rubyAppendReplacement so no single match can over-allocate past
// the shared regex output cap, even for escapes that copy large pre/post-match
// segments.
func appendBounded(dst []byte, s string) ([]byte, error) {
	if len(s) > maxRegexInputBytes-len(dst) {
		return nil, errRegexOutputLimit
	}
	return append(dst, s...), nil
}

// appendRubySubmatch appends submatch n (0 is the whole match) when it
// participated in the match; otherwise it appends nothing. An out-of-range or
// non-participating group expands to the empty string, as Ruby does. The append
// is bounded by the shared regex output guard.
func appendRubySubmatch(dst []byte, src string, loc []int, n int) ([]byte, error) {
	start := 2 * n
	if start+1 >= len(loc) {
		return dst, nil
	}
	lo, hi := loc[start], loc[start+1]
	if lo < 0 || hi < 0 {
		return dst, nil
	}
	return appendBounded(dst, src[lo:hi])
}

// appendRubyLastGroup appends the last capture group that participated in the
// match, matching Ruby's "\+" replacement escape. With no participating group
// it appends nothing. The append is bounded by the shared regex output guard.
//
// Ruby restricts "\+" to named captures once the pattern defines any: it then
// considers only participating *named* groups and ignores unnamed ones, so
// "abc".sub(/(?<x>a)(b)(c)/, "\\+") expands to "a" (named x), not "c" (the
// later unnamed group), and "a".sub(/(?<a>a)(?<b>b)?/, "\\+") expands to "a"
// (the trailing named group did not participate). With no named captures it
// keeps the all-slots behavior, returning the last participating group overall.
func appendRubyLastGroup(dst []byte, re *regexp.Regexp, src string, loc []int) ([]byte, error) {
	if regexHasNamedCapture(re) {
		names := re.SubexpNames()
		last := -1
		for n := 1; 2*n+1 < len(loc); n++ {
			if n >= len(names) || names[n] == "" {
				continue
			}
			if loc[2*n] >= 0 && loc[2*n+1] >= 0 {
				last = n
			}
		}
		if last < 0 {
			return dst, nil
		}
		return appendBounded(dst, src[loc[2*last]:loc[2*last+1]])
	}
	for n := len(loc)/2 - 1; n >= 1; n-- {
		lo, hi := loc[2*n], loc[2*n+1]
		if lo >= 0 && hi >= 0 {
			return appendBounded(dst, src[lo:hi])
		}
	}
	return dst, nil
}

// regexHasNamedCapture reports whether re defines at least one named capture
// group. Go's SubexpNames returns one entry per subexpression, "" for the whole
// match and for unnamed groups, so any non-empty entry means a named group is
// present. Ruby keys its "numbered refs go empty" behavior off exactly this.
func regexHasNamedCapture(re *regexp.Regexp) bool {
	for _, name := range re.SubexpNames() {
		if name != "" {
			return true
		}
	}
	return false
}

// appendRubyNamedGroup expands "\k<name>" given the template text immediately
// following "\k<" (rest). It returns the appended buffer and the index of the
// closing ">" within rest so the caller can advance past the reference. An
// unterminated name or a name the pattern never defines is an error, matching
// Ruby's RuntimeError and IndexError on the same templates.
//
// When the pattern reuses a name (for example "(?<x>a)(?<x>b)" or
// "(?<x>foo)|(?<x>bar)"), Ruby resolves the reference to the *last*
// participating occurrence, matching MatchData[:name]: "(?<x>a)(?<x>b)" over
// "ab" expands to "b", and "(?<x>a)(?<x>b)?(?<x>c)" over "ac" expands to "c".
// When the name exists but no occurrence participated, Ruby expands to the
// empty string. An undefined name is an error.
func appendRubyNamedGroup(dst []byte, re *regexp.Regexp, src string, loc []int, rest string) ([]byte, int, error) {
	end := strings.IndexByte(rest, '>')
	if end < 0 {
		return nil, 0, fmt.Errorf("invalid group name reference format")
	}
	name := rest[:end]
	defined := false
	lastParticipating := -1
	for idx, candidate := range re.SubexpNames() {
		if candidate == "" || candidate != name {
			continue
		}
		defined = true
		if 2*idx+1 < len(loc) && loc[2*idx] >= 0 && loc[2*idx+1] >= 0 {
			lastParticipating = idx
		}
	}
	if !defined {
		return nil, 0, fmt.Errorf("undefined group name reference: %s", name)
	}
	if lastParticipating < 0 {
		// The name exists but no occurrence participated; Ruby expands to empty.
		return dst, end, nil
	}
	expanded, err := appendRubySubmatch(dst, src, loc, lastParticipating)
	if err != nil {
		return nil, 0, err
	}
	return expanded, end, nil
}

// rubyMatchReplacer produces a single match's replacement and appends it to dst,
// returning the grown buffer. loc holds the match's submatch byte indices in the
// FindStringSubmatchIndex layout (loc[0:2] is the whole match). The template form
// expands a Ruby replacement string; the block form (sub/gsub with a block)
// yields the matched text to a user block and appends the block's result. Both
// rubyRegexSub and rubyRegexGSub drive their iteration through this replacer so
// the empty-match advancement and the shared output-size guard are written once.
type rubyMatchReplacer func(dst []byte, loc []int) ([]byte, error)

// rubyTemplateReplacer adapts a Ruby replacement template to a rubyMatchReplacer.
func rubyTemplateReplacer(re *regexp.Regexp, template, src string) rubyMatchReplacer {
	return func(dst []byte, loc []int) ([]byte, error) {
		return rubyAppendReplacement(dst, re, template, src, loc)
	}
}

// rubyBlockReplacer adapts a per-match block call to a rubyMatchReplacer. yield
// receives the whole-match substring (Ruby's group 0, what String#sub and
// String#gsub pass to the block) and returns the value the block produced; the
// value's string form is appended, bounded by the shared output guard so a block
// returning a huge string cannot allocate past the cap. The yield callback also
// charges the sandbox step/cancellation quota per match.
func rubyBlockReplacer(src string, yield func(match string) (string, error)) rubyMatchReplacer {
	return func(dst []byte, loc []int) ([]byte, error) {
		replacement, err := yield(src[loc[0]:loc[1]])
		if err != nil {
			return nil, err
		}
		return appendBounded(dst, replacement)
	}
}

// literalBlockReplace replaces literal occurrences of pattern in src with the
// string the yield block returns for each matched run, returning the rewritten
// string and whether any match was found. It drives String#sub and String#gsub
// block forms when the pattern is literal (regex: false), so it must match
// byte-for-byte exactly like strings.Replace/ReplaceAll rather than routing
// through Go's regexp engine, whose patterns must be valid UTF-8. Vibescript
// strings can hold arbitrary bytes (for example a pattern produced by
// "Aé".byteslice(1, 1) is the raw byte 0xc3), so the regex path would reject
// those literals; this path matches them the same way the literal template forms
// do.
//
// global selects gsub-style replacement of every occurrence over sub-style
// replacement of only the first. The matched substring yielded to the block is
// the literal run that was replaced: pattern itself for a non-empty pattern, or
// the empty string for the per-position matches an empty pattern produces. Empty
// patterns match before each UTF-8 sequence and at the end of src, advancing one
// rune at a time exactly as strings.Replace does, so "abc".gsub("") yields four
// empty matches and an invalid byte such as 0xc3 counts as a single position
// (utf8.RuneError of width one), matching Ruby's behavior on byte strings.
//
// The matched flag distinguishes a performed replacement from an untouched
// receiver so String#sub!/String#gsub! can return the receiver whenever a match
// occurred (even if the block reproduced the original text) and nil only when no
// match was found. An empty pattern always matches at least position 0, so the
// flag is true even when the source is empty.
//
// yield charges the sandbox step quota per match and returns the bounded string
// form of the block result; every append is checked against the shared regex
// output cap so a block returning a huge replacement cannot allocate past it.
func literalBlockReplace(src, pattern string, global bool, yield func(match string) (string, error)) (string, bool, error) {
	if pattern == "" {
		return literalBlockReplaceEmpty(src, global, yield)
	}
	// The literal path deliberately bypasses the regex input-size cap, so src can
	// be far larger than maxRegexInputBytes. Preallocating len(src) would force a
	// large transient allocation even when a huge literal token is replaced by a
	// tiny block result, so out starts empty and grows (amortized) only as far as
	// the bounded output it actually accumulates, which appendBounded caps at
	// maxRegexInputBytes.
	var out []byte
	start := 0
	matched := false
	for {
		idx := strings.Index(src[start:], pattern)
		if idx < 0 {
			break
		}
		matched = true
		matchStart := start + idx
		next, err := appendBounded(out, src[start:matchStart])
		if err != nil {
			return "", false, err
		}
		out = next
		replacement, err := yield(pattern)
		if err != nil {
			return "", false, err
		}
		next, err = appendBounded(out, replacement)
		if err != nil {
			return "", false, err
		}
		out = next
		start = matchStart + len(pattern)
		if !global {
			break
		}
	}
	next, err := appendBounded(out, src[start:])
	if err != nil {
		return "", false, err
	}
	out = next
	return string(out), matched, nil
}

// literalBlockReplaceEmpty implements the empty-pattern case of
// literalBlockReplace. An empty literal pattern matches before the first rune
// and after each UTF-8 sequence (advancing one rune at a time, treating an
// invalid byte as utf8.RuneError of width one), so it yields the empty string at
// every such position. For sub (global == false) only the leading position is
// replaced; for gsub every position is. This mirrors strings.Replace's
// empty-old handling so the block form stays byte-for-byte consistent with the
// literal template forms.
//
// An empty pattern always matches at least position 0 (even on an empty source),
// so the returned matched flag is unconditionally true, matching Ruby where
// "".gsub!("", "") returns the receiver rather than nil.
func literalBlockReplaceEmpty(src string, global bool, yield func(match string) (string, error)) (string, bool, error) {
	// See literalBlockReplace: out starts empty so an oversized src cannot force a
	// transient preallocation larger than the bounded output actually produced.
	var out []byte
	replacement, err := yield("")
	if err != nil {
		return "", false, err
	}
	out, err = appendBounded(out, replacement)
	if err != nil {
		return "", false, err
	}
	if !global {
		result, err := appendStringBounded(out, src)
		return result, true, err
	}
	start := 0
	for start < len(src) {
		_, width := utf8.DecodeRuneInString(src[start:])
		if width == 0 {
			width = 1
		}
		next := start + width
		out, err = appendBounded(out, src[start:next])
		if err != nil {
			return "", false, err
		}
		replacement, err := yield("")
		if err != nil {
			return "", false, err
		}
		out, err = appendBounded(out, replacement)
		if err != nil {
			return "", false, err
		}
		start = next
	}
	return string(out), true, nil
}

// appendStringBounded appends s to out under the shared regex output cap and
// returns the finished string, so the empty-pattern sub path can flush its tail
// with the same guard the rest of the literal block replacer uses.
func appendStringBounded(out []byte, s string) (string, error) {
	next, err := appendBounded(out, s)
	if err != nil {
		return "", err
	}
	return string(next), nil
}

// rubyRegexSub replaces the first match of re in src using the Ruby-style
// replacement template, enforcing the shared regex output-size guard. It is the
// Ruby-semantics counterpart of the first-match path that previously relied on
// Go's ExpandString. The reported matched flag is true whenever a match was
// found, independent of whether the replacement reproduced the original text.
func rubyRegexSub(re *regexp.Regexp, src, template, method string) (string, bool, error) {
	return rubyRegexSubWith(re, src, method, rubyTemplateReplacer(re, template, src))
}

// rubyRegexSubWith replaces the first match of re in src with the result of
// replace, enforcing the shared regex output-size guard. It backs both the
// template form (rubyRegexSub) and the block form, which differ only in how each
// match's replacement is produced. It returns whether a match was found so the
// bang forms can distinguish a performed substitution from an untouched
// receiver.
func rubyRegexSubWith(re *regexp.Regexp, src, method string, replace rubyMatchReplacer) (string, bool, error) {
	loc := re.FindStringSubmatchIndex(src)
	if loc == nil {
		return src, false, nil
	}
	replaced, err := replace(nil, loc)
	if err != nil {
		return "", false, fmt.Errorf("%s %w", method, err)
	}
	outputLen := len(src) - (loc[1] - loc[0]) + len(replaced)
	if outputLen > maxRegexInputBytes {
		return "", false, guardLimitErrorf("%s output exceeds limit %d bytes", method, maxRegexInputBytes)
	}
	return src[:loc[0]] + string(replaced) + src[loc[1]:], true, nil
}

// rubyRegexGSub replaces every match of re in src using the Ruby-style
// replacement template. Match iteration mirrors regexReplaceAllWithLimit
// (empty-match advancement and the output-size guard); only the per-match
// expansion differs, using Ruby substitution rules instead of Go's. The reported
// matched flag is true whenever at least one match was found.
func rubyRegexGSub(re *regexp.Regexp, src, template, method string) (string, bool, error) {
	return rubyRegexGSubWith(re, src, method, rubyTemplateReplacer(re, template, src))
}

// rubyRegexGSubWith replaces every match of re in src with the result of replace,
// sharing the empty-match advancement and output-size guard with the template
// form (rubyRegexGSub). Only the per-match replacement differs between the
// template and block forms. It returns whether at least one match was found so
// the bang forms can distinguish a performed substitution from an untouched
// receiver.
func rubyRegexGSubWith(re *regexp.Regexp, src, method string, replace rubyMatchReplacer) (string, bool, error) {
	out := make([]byte, 0, len(src))
	lastAppended := 0
	searchStart := 0
	lastMatchEnd := -1
	matched := false
	for searchStart <= len(src) {
		loc, found := nextRegexReplaceAllSubmatchIndex(re, src, searchStart)
		if !found {
			break
		}
		if loc[0] == loc[1] && loc[0] == lastMatchEnd {
			if loc[0] >= len(src) {
				break
			}
			_, size := utf8.DecodeRuneInString(src[loc[0]:])
			if size == 0 {
				size = 1
			}
			searchStart = loc[0] + size
			continue
		}

		matched = true
		segmentLen := loc[0] - lastAppended
		if len(out) > maxRegexInputBytes-segmentLen {
			return "", false, guardLimitErrorf("%s output exceeds limit %d bytes", method, maxRegexInputBytes)
		}
		out = append(out, src[lastAppended:loc[0]]...)
		expanded, err := replace(out, loc)
		if err != nil {
			return "", false, fmt.Errorf("%s %w", method, err)
		}
		out = expanded
		if len(out) > maxRegexInputBytes {
			return "", false, guardLimitErrorf("%s output exceeds limit %d bytes", method, maxRegexInputBytes)
		}
		lastAppended = loc[1]
		lastMatchEnd = loc[1]

		if loc[1] > loc[0] {
			searchStart = loc[1]
			continue
		}
		if loc[1] >= len(src) {
			break
		}
		_, size := utf8.DecodeRuneInString(src[loc[1]:])
		if size == 0 {
			size = 1
		}
		searchStart = loc[1] + size
	}

	tailLen := len(src) - lastAppended
	if len(out) > maxRegexInputBytes-tailLen {
		return "", false, guardLimitErrorf("%s output exceeds limit %d bytes", method, maxRegexInputBytes)
	}
	out = append(out, src[lastAppended:]...)
	return string(out), matched, nil
}
