package runtime

import (
	"fmt"
	"strings"
	"unicode"
	"unicode/utf8"
)

type strftimeBudget struct {
	exec *Execution
	base int
	work int
}

func (b *strftimeBudget) charge(n int) error {
	if b.exec == nil {
		return nil
	}
	if err := b.exec.checkContext(); err != nil {
		return err
	}
	n = saturatingAdd(b.work, n)
	b.work = n % stringScanBytesPerStep
	if steps := n / stringScanBytesPerStep; steps > 0 {
		return b.exec.stepN(steps)
	}
	return nil
}

func (b *strftimeBudget) check(output, scratch int) error {
	if b.exec != nil && b.exec.memoryExceeded(saturatingAdd(b.base, scratch)) {
		return b.exec.memoryQuotaExceededError()
	}
	if output > maxFormatOutputBytes {
		err := fmt.Errorf("%w: time.strftime output exceeds limit %d bytes", errOutputLimitExceeded, maxFormatOutputBytes)
		if b.exec != nil {
			return b.exec.latchExhaustion(err)
		}
		return err
	}
	return nil
}

// No script code runs during formatting, so one call-root baseline covers the
// whole operation. held counts ancestor builders during compound expansion.
func (r strftimeRenderer) format(format string, args []Value, block Value, checkLayout bool) (string, error) {
	r.budget = &strftimeBudget{exec: r.exec}
	if r.exec != nil {
		if err := r.exec.checkContext(); err != nil {
			return "", err
		}
		if r.exec.memoryQuota > 0 {
			if args == nil {
				args = []Value{NewString(format)}
			}
			base, walked := r.exec.hashCallRootUsage(NewTime(r.t), args, nil, block)
			r.budget.base = saturatingAdd(base, estimatedValueBytes+estimatedStringHeaderBytes)
			if err := r.exec.chargeEstimatorWalk(walked); err != nil {
				return "", err
			}
		}
		defer r.exec.beginAccumulatorMeteredSection()()
	}
	if err := r.budget.charge(len(format)); err != nil {
		return "", err
	}
	if checkLayout && !strings.ContainsRune(format, '%') && containsGoLayoutSignature(format) {
		// The diagnostic's confirmation uses Go's formatter. Reserve its
		// bounded numeric fields and any repeated host-provided zone name.
		zone, _ := r.t.Zone()
		projected := saturatingAdd(saturatingMul(len(format), 32), saturatingMul(strings.Count(format, "MST"), len(zone)))
		if err := r.budget.check(len(format), saturatingMul(projected, 3)); err != nil {
			return "", err
		}
		if projected > 64*maxFormatOutputBytes {
			err := fmt.Errorf("%w: time.strftime layout diagnostic exceeds limit", errOutputLimitExceeded)
			if r.exec != nil {
				err = r.exec.latchExhaustion(err)
			}
			return "", err
		}
		if err := checkStrftimeGivenGoLayout(r.t, format); err != nil {
			return "", err
		}
	}
	return r.render(format, false)
}

func (r strftimeRenderer) checkField(size int) error {
	if r.budget == nil {
		return nil
	}
	// A padded field can simultaneously retain the pad run, concatenation,
	// and case-conversion backing before the outer builder copies it.
	scratch := saturatingAdd(r.held, saturatingMul(size, 8))
	if r.builder != nil {
		scratch = saturatingAdd(scratch, r.builder.Cap())
	}
	return r.budget.check(saturatingAdd(r.prefix, saturatingAdd(r.written, size)), scratch)
}

func (r strftimeRenderer) append(out string, retainedCapacity int) error {
	if err := r.budget.charge(len(out)); err != nil {
		return err
	}
	capacity := projectedBuilderCap(r.builder, len(out))
	scratch := saturatingAdd(r.held, saturatingAdd(capacity, max(len(out), retainedCapacity)))
	if capacity > r.builder.Cap() {
		scratch = saturatingAdd(scratch, r.builder.Cap())
	}
	if err := r.budget.check(saturatingAdd(r.prefix, saturatingAdd(r.builder.Len(), len(out))), scratch); err != nil {
		return err
	}
	r.builder.Grow(len(out))
	r.builder.WriteString(out)
	return nil
}

func (r strftimeRenderer) caseSize(text string, shift caseFlag) (int, error) {
	if shift == caseNone {
		return len(text), nil
	}
	mapping := unicode.ToUpper
	if shift == caseToggle {
		sawCased, allUpper, nextCheck := false, true, 0
		for i, char := range text {
			if r.budget != nil && i >= nextCheck {
				if err := r.budget.charge(0); err != nil {
					return 0, err
				}
				nextCheck = i + 4096
			}
			if char >= 'a' && char <= 'z' {
				allUpper = false
				break
			}
			if char >= 'A' && char <= 'Z' {
				sawCased = true
			}
		}
		if sawCased && allUpper {
			mapping = unicode.ToLower
		}
	}
	size, nextCheck := 0, 0
	for i, char := range text {
		if r.budget != nil && i >= nextCheck {
			if err := r.budget.charge(0); err != nil {
				return 0, err
			}
			nextCheck = i + 4096
		}
		size = saturatingAdd(size, utf8.RuneLen(mapping(char)))
	}
	return size, nil
}
