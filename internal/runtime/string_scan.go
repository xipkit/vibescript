package runtime

import (
	"errors"
	"fmt"
	"regexp"
	"regexp/syntax"
	"unicode/utf8"
)

// stringScanBlock yields one match at a time and returns the receiver. Its cursor
// keeps the preceding rune in the regex input, so anchors and word boundaries
// observe their original context. Matching a bare suffix made ^ fire repeatedly;
// searching an unconsumed lookback dropped adjacent multi-rune matches. The
// cursor's regex consumes that context before searching for the next match.
func stringScanBlock(exec *Execution, re *regexp.Regexp, text string, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
	groups := re.NumSubexp()
	roots := scanRoots{receiver: receiver, args: args, kwargs: kwargs, block: block}
	// Include spare index capacity when regexp pads erased capture groups.
	// Reserving four rows also covers old and
	// new backing arrays coexisting during that padding, before the next check.
	delta := exec.reserveLoopScratch(saturatingMul(4, regexSubmatchIndexRowBytes(groups)))
	noMatches := false
	if exec.memoryQuota > 0 && exec.memoryExceeded(roots.liveBytes(exec)) {
		exec.releaseLoopScratch(delta)
		delta = 0
		if err := roots.check(exec); err != nil {
			return NewNil(), err
		}
		if err := exec.step(); err != nil {
			return NewNil(), err
		}
		// A miss allocates no returned index row. Probe without requesting
		// captures before latching exhaustion for this hypothetical scratch.
		if re.MatchString(text) {
			return NewNil(), exec.memoryQuotaExceededError()
		}
		noMatches = true
	}
	defer exec.releaseLoopScratch(delta)

	work := regexWork{exec: exec}
	cursor := stringScanCursor{
		re:          re,
		work:        &work,
		previousEnd: -1,
	}
	// Go's parser rejects the context wrapper for patterns already at its
	// nesting limit. Keep those rare scans exact with the original bounded
	// table; an immediate block return still avoids constructing it. The
	// namespace cursor's suffix approximation is insufficient here: a deeply
	// nested \b.. must not report cd as a second match in abcd.
	var fallbackMatches [][]int
	var fallbackReady bool
	var fallbackIndex, fallbackDelta int
	defer func() { exec.releaseLoopScratch(fallbackDelta) }()
	cursor.atNestingLimit = func(text string, start int) ([]int, error) {
		if !fallbackReady {
			var err error
			fallbackMatches, err = stringScanMatches(exec, re, re.String(), text, roots)
			if err != nil {
				return nil, err
			}
			fallbackDelta = exec.reserveLoopScratch(actualRegexSubmatchIndexBytes(fallbackMatches, groups))
			if err := roots.check(exec); err != nil {
				return nil, err
			}
			fallbackReady = true
		}
		for fallbackIndex < len(fallbackMatches) && fallbackMatches[fallbackIndex][0] < start {
			fallbackIndex++
		}
		if fallbackIndex == len(fallbackMatches) {
			return nil, nil
		}
		return fallbackMatches[fallbackIndex], nil
	}

	runner, err := newBlockCallRunner(exec, block, "string.scan", receiver, args, kwargs)
	if err != nil {
		return NewNil(), err
	}
	if noMatches {
		return receiver, nil
	}
	var blockArg [1]Value
	for {
		if err := exec.step(); err != nil {
			return NewNil(), err
		}
		loc, err := cursor.next(text)
		if err != nil {
			return NewNil(), err
		}
		if loc == nil {
			return receiver, nil
		}
		copyDelta, err := exec.reserveYieldedCopy(
			projectedRegexElementPayloadBytes(text, loc, groups), receiver, args, kwargs, block,
		)
		if err != nil {
			return NewNil(), err
		}
		blockArg[0] = stringScanElement(text, loc, groups)
		exec.releaseLoopScratch(copyDelta)
		if _, err := runner.call(blockArg[:]); err != nil {
			return NewNil(), err
		}
	}
}

type stringScanCursor struct {
	re             *regexp.Regexp
	from           *regexp.Regexp
	work           *regexWork
	position       int
	previousEnd    int
	fallback       bool
	suffixSafe     bool
	atNestingLimit func(string, int) ([]int, error)
}

func (c *stringScanCursor) find(text string) ([]int, error) {
	if prefix, literal := c.re.LiteralPrefix(); c.position == 0 || literal && prefix != "" {
		// Keep literal-prefix acceleration for dense matches and sparse tails.
		// A complete empty literal can still contain start assertions, so it
		// must take the context-aware path after its first empty match.
		loc := c.re.FindStringSubmatchIndex(text[c.position:])
		return offsetRegexSubmatchIndexInPlace(loc, c.position), nil
	}
	if c.fallback {
		return c.atNestingLimit(text, c.position)
	}
	if c.from == nil && !c.suffixSafe {
		parsed, err := syntax.Parse(c.re.String(), syntax.Perl)
		if err != nil {
			return nil, err
		}
		if regexScanRequiresBeginning(parsed) {
			return nil, nil
		}
		c.suffixSafe = !regexScanNeedsLeftContext(parsed)
		if !c.suffixSafe {
			if err := c.compileFrom(); err != nil {
				var parseErr *syntax.Error
				if errors.As(err, &parseErr) && parseErr.Code == syntax.ErrNestingDepth && c.atNestingLimit != nil {
					c.fallback = true
					return c.atNestingLimit(text, c.position)
				}
				return nil, fmt.Errorf("string.scan invalid regex: %w", err)
			}
		}
	}
	if c.suffixSafe {
		loc := c.re.FindStringSubmatchIndex(text[c.position:])
		return offsetRegexSubmatchIndexInPlace(loc, c.position), nil
	}
	_, size := utf8.DecodeLastRuneInString(text[:c.position])
	contextStart := c.position - size
	// Keep the string API: the RuneReader API disables Go's bounded
	// backtracking engine and multiplies memory for nullable capture patterns.
	loc := c.from.FindStringSubmatchIndex(text[contextStart:])
	if loc == nil {
		return nil, nil
	}
	for i, index := range loc {
		if index >= 0 {
			loc[i] = index + contextStart
		}
	}
	_, size = utf8.DecodeRuneInString(text[loc[0]:])
	loc[0] += size
	return loc, nil
}

func (c *stringScanCursor) compileFrom() error {
	// Consuming one preceding rune supplies the original left context and
	// prevents the user pattern from matching before the cursor. Go's
	// ordinary leftmost search then finds the next match, with the user's
	// capture numbers unchanged. The scope keeps flags inside the pattern
	// from changing the prefix, which must also consume newline runes.
	pattern := `(?s:.)(?:` + c.re.String() + `)`
	var err error
	// The internal prefix adds one estimated instruction; cache hits still
	// enforce the ordinary limit when that spelling is supplied by a user.
	c.from, err = compiledRegexps.compileWithWork(pattern, c.work, maxCompiledRegexInstructions+1)
	return err
}

func (c *stringScanCursor) next(text string) ([]int, error) {
	for c.position <= len(text) {
		loc, err := c.find(text)
		if err != nil || loc == nil {
			return nil, err
		}
		// Match regexp's FindAll advancement, including suppressing an empty
		// match at the preceding match's end and advancing by a complete rune.
		accept := true
		if loc[1] == c.position {
			accept = loc[0] != c.previousEnd
			_, size := utf8.DecodeRuneInString(text[c.position:])
			c.position += max(size, 1)
		} else {
			c.position = loc[1]
		}
		c.previousEnd = loc[1]
		if accept {
			return loc, nil
		}
	}
	return nil, nil
}

// regexScanNeedsLeftContext identifies assertions changed by slicing off the
// subject's prefix. End assertions remain exact because the suffix keeps EOF.
func regexScanNeedsLeftContext(re *syntax.Regexp) bool {
	switch re.Op {
	case syntax.OpBeginText, syntax.OpBeginLine, syntax.OpWordBoundary, syntax.OpNoWordBoundary:
		return true
	}
	for _, sub := range re.Sub {
		if regexScanNeedsLeftContext(sub) {
			return true
		}
	}
	return false
}

// regexScanRequiresBeginning recognizes patterns whose every successful match
// needs the start of the subject, so continuing after the first match is futile.
func regexScanRequiresBeginning(re *syntax.Regexp) bool {
	switch re.Op {
	case syntax.OpBeginText:
		return true
	case syntax.OpCapture, syntax.OpPlus:
		return regexScanRequiresBeginning(re.Sub[0])
	case syntax.OpRepeat:
		return re.Min > 0 && regexScanRequiresBeginning(re.Sub[0])
	case syntax.OpConcat:
		for _, sub := range re.Sub {
			if regexScanRequiresBeginning(sub) {
				return true
			}
		}
	case syntax.OpAlternate:
		for _, sub := range re.Sub {
			if !regexScanRequiresBeginning(sub) {
				return false
			}
		}
		return true
	}
	return false
}
