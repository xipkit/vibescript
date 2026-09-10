package runtime

import (
	"container/list"
	"regexp"
	"regexp/syntax"
	"strings"
	"sync"
)

const (
	compiledRegexCacheCapacity = 64

	// compiledRegexInstructionBytes approximates the heap a single compiled
	// program instruction retains. Measured at roughly 45 bytes across
	// pattern shapes spanning four orders of magnitude in size; rounded up so
	// the budgets below describe a ceiling rather than a typical case.
	compiledRegexInstructionBytes = 64

	// maxCompiledRegexInstructions caps one compiled program, and
	// compiledRegexCacheInstructionBudget caps every cached program together.
	//
	// The pattern-length guard alone does not bound compiled memory: Go
	// expands counted repeats, so a pattern well inside the 16 KiB limit
	// compiles to an arbitrarily large program. Measured amplification runs
	// from about 2,200x at 408 bytes to about 13,500x at 4 KiB — a 4 KiB
	// pattern retains roughly 52 MiB. Filling the 64-entry cache with those
	// held gigabytes, outside MemoryQuotaBytes and beyond the call that
	// compiled them, in a process-global cache shared across engines and
	// tenants (#52).
	//
	// Instruction count is the budgeted quantity because it tracks retained
	// bytes closely and is knowable before compiling. Real patterns sit far
	// below these limits: an email-shaped pattern compiles to 13
	// instructions and 300 chained alternations to 902, against a per-pattern
	// cap of 100,000 (about 6 MiB) and a cache holding 250,000 (about 16 MiB).
	maxCompiledRegexInstructions        = 100_000
	compiledRegexCacheInstructionBudget = 250_000
)

var compiledRegexps = newRegexCache(compiledRegexCacheCapacity, compiledRegexCacheInstructionBudget)

type regexCache struct {
	mu       sync.Mutex
	capacity int
	// budget caps the summed instruction count of the cached programs, so
	// eviction tracks retained memory rather than only entry count.
	budget  int
	cost    int
	lru     *list.List
	entries map[string]*list.Element
}

type regexCacheEntry struct {
	pattern string
	re      *regexp.Regexp
	cost    int
}

func newRegexCache(capacity, budget int) *regexCache {
	if capacity < 1 {
		capacity = 1
	}
	if budget < 1 {
		budget = 1
	}
	return &regexCache{
		capacity: capacity,
		budget:   budget,
		lru:      list.New(),
		entries:  make(map[string]*list.Element, capacity),
	}
}

func compileCachedRegex(pattern string) (*regexp.Regexp, error) {
	return compiledRegexps.compile(pattern)
}

// compiledRegexCost estimates how many program instructions pattern would
// compile to, from the parsed syntax alone.
//
// Parsing keeps a counted repeat as a single node with its bounds; expansion
// into the repeated concatenation happens in Simplify, and materializing the
// program happens in Compile. Both are the work this guard exists to prevent,
// so neither may run before the limit is enforced — estimating from the parse
// tree bounds the peak rather than measuring a program already built.
//
// The estimate walks the tree, multiplying a subtree's cost by its repeat
// bounds, and saturates at budget so a deeply nested repeat stops the walk
// instead of computing an enormous exact total. It over-approximates rather
// than under: every node is charged at least one instruction, which is the
// safe direction for a guard.
func compiledRegexCost(pattern string) (int, error) {
	return compiledRegexCostWithLimit(pattern, maxCompiledRegexInstructions)
}

func compiledRegexCostWithLimit(pattern string, limit int) (int, error) {
	parsed, err := syntax.Parse(pattern, syntax.Perl)
	if err != nil {
		return 0, err
	}
	return estimateRegexProgramSize(parsed, limit+1), nil
}

// estimateRegexProgramSize returns the estimated instruction count for re,
// clamped at budget.
//
// This models what Simplify and the compiler do, so it can be wrong in two
// ways, and they are not equally bad. Under-counting admits a program past
// the cap, which is the hole this guard exists to close. Over-counting only
// rejects a pattern near a cap that real ones sit three orders of magnitude
// below. When a rule is uncertain, charge more.
//
// The rules that were learned the hard way, each from a pattern that slipped
// through or was wrongly rejected: counted repeats alias the simple operators
// and must be normalized before any operator test (repeatAliasOp), {1} is
// discarded entirely (skipExactOneRepeats), collapsing nested quantifiers
// requires the operator AND greediness to match, a star over a nullable
// operand compiles two branches, and an open upper bound emits Min copies
// rather than Min+1. TestRegexCostHandlesRepeatBounds pins each against the
// real compiled size; extend it before changing anything here.
func estimateRegexProgramSize(re *syntax.Regexp, budget int) int {
	re = skipExactOneRepeats(re)
	if re == nil {
		return 0
	}
	switch repeatAliasOp(re) {
	case syntax.OpLiteral:
		// One instruction per rune, and at least one for an empty literal.
		return clampRegexCost(max(len(re.Rune), 1), budget)
	case syntax.OpConcat:
		total := 0
		for _, sub := range re.Sub {
			total = saturatingAdd(total, estimateRegexProgramSize(sub, budget))
			if total >= budget {
				return budget
			}
		}
		return clampRegexCost(max(total, 1), budget)
	case syntax.OpAlternate:
		// Each arm plus a branch instruction per arm.
		total := len(re.Sub)
		for _, sub := range re.Sub {
			total = saturatingAdd(total, estimateRegexProgramSize(sub, budget))
			if total >= budget {
				return budget
			}
		}
		return clampRegexCost(max(total, 1), budget)
	case syntax.OpCapture:
		return clampRegexCost(saturatingAdd(2, estimateRegexProgramSize(re.Sub0[0], budget)), budget)
	case syntax.OpEmptyMatch:
		return clampRegexCost(1, budget)
	case syntax.OpStar, syntax.OpPlus, syntax.OpQuest:
		// A quantifier over an empty match is removed entirely, whatever the
		// operator or greediness, so a whole chain of them around one costs
		// nothing to compile however deep it is.
		if quantifiesEmptyMatch(re) {
			return clampRegexCost(1, budget)
		}
		// A child whose expansion is rooted at this same operator makes this
		// quantifier idempotent, so Simplify drops it and it costs nothing:
		// {0,n} expands to nested quests, and an outer ? over one collapses.
		if child := skipExactOneRepeats(re.Sub0[0]); child != nil &&
			child.Op == syntax.OpRepeat && simplifiedRootOp(child) == repeatAliasOp(re) &&
			child.Flags&syntax.NonGreedy == re.Flags&syntax.NonGreedy {
			return clampRegexCost(estimateRegexProgramSize(child, budget), budget)
		}
		operand := unwrapIdempotentQuantifiers(re, re.Sub0[0])
		if operand == nil {
			return clampRegexCost(1, budget)
		}
		// A star over a nullable operand compiles as (operand+)?, which emits
		// a second branch. Charging one understated these programs.
		branches := 1
		if repeatAliasOp(re) == syntax.OpStar && regexpMinMatchRunes(operand) == 0 {
			branches = 2
		}
		return clampRegexCost(saturatingAdd(branches, estimateRegexProgramSize(operand, budget)), budget)
	case syntax.OpRepeat:
		// Simplification discards a {0} repeat body and all, so costing the
		// child would reject valid patterns over a body that never compiles.
		if re.Max == 0 {
			return clampRegexCost(1, budget)
		}
		// The expansion repeats the subtree once per bound. Copies past the
		// required minimum are optional, and each one compiles a branch
		// instruction alongside its copy; omitting those under-counted a
		// bounded range like a{0,1000} by nearly half. An open upper bound
		// instead adds a star tail over one more copy.
		inner := estimateRegexProgramSize(re.Sub0[0], budget)
		reps := re.Max
		optional := 0
		if reps < 0 {
			// An open upper bound emits exactly Min copies, the last one
			// quantified; the trailing branch is the +1 charged below.
			// Charging Min+1 billed the operand an extra time and rejected
			// programs inside the cap.
			reps = max(re.Min, 1)
		} else {
			optional = reps - re.Min
		}
		total := saturatingMul(inner, max(reps, 1))
		total = saturatingAdd(total, optional)
		return clampRegexCost(saturatingAdd(1, total), budget)
	default:
		// Character classes, anchors, empty and no-match nodes are all a
		// single instruction.
		return clampRegexCost(1, budget)
	}
}

func clampRegexCost(cost, budget int) int {
	if cost > budget {
		return budget
	}
	return cost
}

func (c *regexCache) compile(pattern string) (*regexp.Regexp, error) {
	return c.compileWithWork(pattern, nil, maxCompiledRegexInstructions)
}

func (c *regexCache) cachedProgramCost(pattern string) (int, bool) {
	c.mu.Lock()
	defer c.mu.Unlock()
	elem := c.entries[pattern]
	if elem == nil {
		return 0, false
	}
	return elem.Value.(regexCacheEntry).cost, true
}

func (c *regexCache) compileWithWork(pattern string, work *regexWork, limit int) (*regexp.Regexp, error) {
	c.mu.Lock()
	if elem := c.entries[pattern]; elem != nil {
		c.lru.MoveToFront(elem)
		entry := elem.Value.(regexCacheEntry)
		c.mu.Unlock()
		if entry.cost > limit {
			return nil, regexProgramLimitError(entry.cost, limit)
		}
		return entry.re, nil
	}
	c.mu.Unlock()

	cost, err := compiledRegexCostWithLimit(pattern, limit)
	if err != nil {
		// Sizing parses the same syntax regexp.Compile does, so a parse error
		// here is the error the caller would have gotten from compiling.
		return nil, err
	}
	if cost > limit {
		return nil, regexProgramLimitError(cost, limit)
	}
	if err := work.charge(saturatingMul(cost, compiledRegexInstructionBytes)); err != nil {
		return nil, err
	}

	// The regexp retains its expression too, so detach before compiling.
	pattern = strings.Clone(pattern)
	re, err := regexp.Compile(pattern)
	if err != nil {
		return nil, err
	}

	c.mu.Lock()
	defer c.mu.Unlock()

	if elem := c.entries[pattern]; elem != nil {
		c.lru.MoveToFront(elem)
		return elem.Value.(regexCacheEntry).re, nil
	}

	elem := c.lru.PushFront(regexCacheEntry{pattern: pattern, re: re, cost: cost})
	c.entries[pattern] = elem
	c.cost += cost
	c.evictLocked()
	return re, nil
}

func regexProgramLimitError(cost, limit int) error {
	return guardLimitErrorf(
		"regex compiles to %d instructions, exceeding limit %d (about %d MiB)",
		cost, limit, limit*compiledRegexInstructionBytes>>20,
	)
}

// evictLocked drops least-recently-used entries until the cache is within both
// its entry count and its instruction budget. The most recent entry is kept
// even when it alone exceeds the budget: it is under the per-pattern cap, and
// the caller is about to use it.
func (c *regexCache) evictLocked() {
	for c.lru.Len() > 1 && (c.lru.Len() > c.capacity || c.cost > c.budget) {
		evicted := c.lru.Back()
		if evicted == nil {
			return
		}
		c.lru.Remove(evicted)
		entry := evicted.Value.(regexCacheEntry)
		delete(c.entries, entry.pattern)
		c.cost -= entry.cost
	}
}

// unwrapIdempotentQuantifiers collapses a chain of nested quantifiers that
// Simplify actually rewrites away. That requires both the operator and the
// greediness to match: (?:a*)* becomes a*, but (?:a*?)* keeps both, and so
// does a mixed pair like (?:a+)?. Collapsing more than Simplify does
// understates a program by the number of levels, which is the direction a
// guard must never be wrong in; collapsing less only over-counts patterns
// already near the cap.
func unwrapIdempotentQuantifiers(parent, re *syntax.Regexp) *syntax.Regexp {
	parentOp := repeatAliasOp(parent)
	switch parentOp {
	case syntax.OpStar, syntax.OpPlus, syntax.OpQuest:
	default:
		return re
	}
	nonGreedy := parent.Flags & syntax.NonGreedy
	for re = skipExactOneRepeats(re); re != nil && repeatAliasOp(re) == parentOp &&
		re.Flags&syntax.NonGreedy == nonGreedy; re = skipExactOneRepeats(re) {
		re = re.Sub0[0]
	}
	return re
}

// quantifiesEmptyMatch reports whether re is a quantifier whose operand, after
// descending through any further quantifiers, is an empty match. Simplify
// removes quantifiers over an empty match regardless of operator or
// greediness, so such a chain compiles to nothing and must not be costed as
// one wrapper per level.
func quantifiesEmptyMatch(re *syntax.Regexp) bool {
	for re = skipExactOneRepeats(re); re != nil; re = skipExactOneRepeats(re) {
		switch repeatAliasOp(re) {
		case syntax.OpStar, syntax.OpPlus, syntax.OpQuest:
			re = re.Sub0[0]
		case syntax.OpEmptyMatch:
			return true
		default:
			return false
		}
	}
	return false
}

// repeatAliasOp reports the simple operator a counted repeat is equivalent to.
// Simplify rewrites {0,} to *, {1,} to +, {0,1} to ?, and drops a {0} body
// entirely before compiling, so every operator test here has to see through
// the counted spelling: checking re.Op directly missed the rewrite and got
// both the nullable-star branch charge and the idempotent collapse wrong.
// A repeat with no simple equivalent reports OpRepeat unchanged.
func repeatAliasOp(re *syntax.Regexp) syntax.Op {
	if re == nil || re.Op != syntax.OpRepeat {
		if re == nil {
			return syntax.OpNoMatch
		}
		return re.Op
	}
	switch {
	case re.Max == 0:
		return syntax.OpEmptyMatch
	case re.Min == 0 && re.Max < 0:
		return syntax.OpStar
	case re.Min == 1 && re.Max < 0:
		return syntax.OpPlus
	case re.Min == 0 && re.Max == 1:
		return syntax.OpQuest
	default:
		return syntax.OpRepeat
	}
}

// skipExactOneRepeats unwraps {1} repeats. Simplify discards them, so the
// operand compiles as if the repeat were never written; stopping at one hid
// the operand from every operator test here and clamped patterns that compile
// small.
func skipExactOneRepeats(re *syntax.Regexp) *syntax.Regexp {
	for re != nil && re.Op == syntax.OpRepeat && re.Min == 1 && re.Max == 1 {
		re = re.Sub0[0]
	}
	return re
}

// simplifiedRootOp reports the operator Simplify's expansion of a counted
// repeat is rooted at, which is not always the operator the repeat is
// equivalent to: {0,n} expands to nested quests, so its root is a quest even
// though its cost is n copies. Collapse decisions compare roots, while cost
// uses repeatAliasOp.
func simplifiedRootOp(re *syntax.Regexp) syntax.Op {
	if re == nil || re.Op != syntax.OpRepeat {
		return repeatAliasOp(re)
	}
	switch {
	case re.Max == 0:
		return syntax.OpEmptyMatch
	case re.Min == 0 && re.Max < 0:
		return syntax.OpStar
	case re.Min == 1 && re.Max < 0:
		return syntax.OpPlus
	case re.Min == 0:
		return syntax.OpQuest
	default:
		return syntax.OpConcat
	}
}
