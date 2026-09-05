package runtime

import (
	"errors"
	"fmt"
	"io"
	"regexp"
	"regexp/syntax"
	"strings"
	"unicode/utf8"
)

const regexOffsetWrapperInstructions = 6

func compileRegexNamespacePattern(work *regexWork, method, pattern string) (*regexp.Regexp, error) {
	if len(pattern) > maxRegexPatternSize {
		return nil, guardLimitErrorf("%s pattern exceeds limit %d bytes", method, maxRegexPatternSize)
	}
	if err := work.charge(len(pattern)); err != nil {
		return nil, err
	}
	re, err := compiledRegexps.compileWithWork(pattern, work, maxCompiledRegexInstructions)
	if err != nil {
		return nil, fmt.Errorf("%s invalid regex: %w", method, err)
	}
	return re, nil
}

// regexNamespaceScan meters literal searches and RE2 state work. Repeated
// first-match searches can rescan a long suffix before returning each short
// match, so one subject-length charge would leave that work unpriced.
type regexNamespaceScan struct {
	re           *regexp.Regexp
	from         *regexp.Regexp
	work         *regexWork
	method       string
	programCost  int
	wholeMatch   bool
	conservative bool
}

func (s *regexNamespaceScan) find(text string, start int) ([]int, error) {
	if literal, complete := s.re.LiteralPrefix(); complete && (s.wholeMatch || s.re.NumSubexp() == 0) && !strings.ContainsRune(literal, utf8.RuneError) {
		// Capture-free literals need no regexp state search. Index is linear
		// in the already size-bounded subject; bill the examined prefix, not
		// every remaining suffix, so dense matches retain linear charges.
		// RuneError must use RE2 because it also matches invalid UTF-8 bytes.
		index := strings.Index(text[start:], literal)
		bytes := len(text) - start
		if index >= 0 {
			bytes = index + len(literal)
		}
		if err := s.work.charge(bytes + 2*estimatedIntBytes); err != nil {
			return nil, err
		}
		if index < 0 {
			return nil, nil
		}
		return []int{start + index, start + index + len(literal)}, nil
	}
	if s.programCost == 0 {
		if err := s.work.charge(len(s.re.String())); err != nil {
			return nil, err
		}
		cost, ok := compiledRegexps.cachedProgramCost(s.re.String())
		if !ok {
			// Another execution may have evicted the entry after compilation.
			var err error
			cost, err = compiledRegexCost(s.re.String())
			if err != nil {
				return nil, err
			}
		}
		s.programCost = max(cost, 1)
	}
	re := s.re
	programCost := s.programCost
	contextStart := 0
	if start > 0 {
		if s.conservative {
			return s.findConservative(text, start)
		}
		if s.from == nil {
			// This is the same constant-size left-context wrapper used by
			// regexSubmatchFromRuneOffset. Prepare it once per call, retaining
			// the real preceding rune for anchors and word boundaries.
			const prefix = `\A[\s\S][\s\S]*?(`
			if err := s.work.charge(len(prefix) + len(s.re.String()) + 1); err != nil {
				return nil, err
			}
			pattern := prefix + s.re.String() + ")"
			// The wrapper adds six estimated instructions. The user's pattern
			// already passed the ordinary cap. Cache hits still check their
			// caller's cap, so a cached internal wrapper cannot raise it for
			// a later user-supplied pattern with the same spelling.
			var err error
			s.from, err = compiledRegexps.compileWithWork(pattern, s.work, maxCompiledRegexInstructions+regexOffsetWrapperInstructions)
			if err != nil {
				var parseErr *syntax.Error
				if errors.As(err, &parseErr) && parseErr.Code == syntax.ErrNestingDepth {
					s.conservative = true
					return s.findConservative(text, start)
				}
				return nil, fmt.Errorf("%s invalid regex: %w", s.method, err)
			}
		}
		re = s.from
		programCost += regexOffsetWrapperInstructions
		_, size := utf8.DecodeLastRuneInString(text[:start])
		contextStart = start - size
	}
	submatches := !s.wholeMatch || start > 0
	captureSlots := 2
	if submatches {
		captureSlots = (re.NumSubexp() + 1) * 2
	}
	// RE2 copies the capture vector when it adds a thread. Include both
	// dimensions, as well as initial and end-of-input transitions: nullable
	// patterns can populate many threads even without reading a byte.
	programCost = saturatingMul(programCost, captureSlots)
	if err := s.work.charge(saturatingAdd(captureSlots*estimatedIntBytes, saturatingMul(2, programCost))); err != nil {
		return nil, err
	}
	input := regexNamespaceReader{text: text[contextStart:], work: s.work, programCost: programCost}
	var loc []int
	if submatches {
		loc = re.FindReaderSubmatchIndex(&input)
	} else {
		loc = re.FindReaderIndex(&input)
	}
	// regexp treats reader errors as EOF and may return a partial match. An
	// exhausted reader must reject every result, including an apparent miss.
	if input.err != nil {
		return nil, input.err
	}
	if start > 0 && loc != nil {
		loc = loc[2:]
		for i, index := range loc {
			if index >= 0 {
				loc[i] = index + contextStart
			}
		}
	}
	return loc, nil
}

// findConservative preserves user patterns already at Go's nesting limit,
// where adding the context wrapper would exceed it. The original matcher makes
// one search plus a two-match context probe (at most three more searches when
// an abutting empty match is skipped). Prepay their complete suffix bounds.
func (s *regexNamespaceScan) findConservative(text string, start int) ([]int, error) {
	captureSlots := 2 * (s.re.NumSubexp() + 1)
	stateWork := saturatingMul(s.programCost, captureSlots)
	perSearch := saturatingMul(len(text)-start+3, stateWork)
	perSearch = saturatingAdd(perSearch, captureSlots*estimatedIntBytes)
	if err := s.work.charge(saturatingMul(4, perSearch)); err != nil {
		return nil, err
	}
	loc, _ := nextRegexReplaceAllSubmatchIndex(s.re, text, start)
	return loc, nil
}

type regexNamespaceReader struct {
	text        string
	work        *regexWork
	err         error
	programCost int
}

// ReadRune charges the search budget before advancing to the next rune.
func (r *regexNamespaceReader) ReadRune() (rune, int, error) {
	if r.err != nil {
		return 0, 0, r.err
	}
	if r.text == "" {
		return 0, 0, io.EOF
	}
	char, size := utf8.DecodeRuneInString(r.text)
	// programCost bounds state transitions and capture-vector copies for
	// each rune. Counting bytes alone lets either dimension multiply work.
	if err := r.work.charge(saturatingMul(size, r.programCost)); err != nil {
		r.err = err
		return 0, 0, err
	}
	r.text = r.text[size:]
	return char, size, nil
}
