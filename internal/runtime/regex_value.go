package runtime

import (
	"fmt"
	"strings"
	"unicode/utf8"

	"github.com/mgomes/vibescript/vibes/value"
)

// regexMemberNames mirrors the names dispatched by regexMember and feeds
// "did you mean" suggestions and LSP completion.
var regexMemberNames = []string{
	"match", "match?", "source", "flags",
	"inspect",
}

var regexBuiltinMembers = newTypedMemberTable(regexMemberNames, KindRegex)

// regexDecoratedPattern returns the Go RE2 pattern a regex value's flags
// resolve to: Ruby's i flag is RE2's case-insensitive (?i) and Ruby's m flag
// (dot matches newline) is RE2's (?s). The decorated form is what every
// string-pattern helper compiles, so a regex value passed where a pattern
// string is accepted hits the same cache entry as its literal.
func regexDecoratedPattern(r value.Regex) string {
	var prefix strings.Builder
	for _, flag := range r.Flags {
		switch flag {
		case 'i':
			prefix.WriteString("(?i)")
		case 'm':
			prefix.WriteString("(?s)")
		}
	}
	return prefix.String() + r.Source
}

// compileRegexValue compiles source with flags applied and returns the regex
// value. method names the caller for error messages (for example
// "regex literal" or "Regexp.new").
func compileRegexValue(method, source, flags string) (Value, error) {
	if len(source) > maxRegexPatternSize {
		return NewNil(), guardLimitErrorf("%s pattern exceeds limit %d bytes", method, maxRegexPatternSize)
	}
	regex := value.Regex{Source: source, Flags: flags}
	compiled, err := compileCachedRegex(regexDecoratedPattern(regex))
	if err != nil {
		return NewNil(), fmt.Errorf("%s invalid regex: %w", method, err)
	}
	regex.Compiled = compiled
	return NewRegex(regex), nil
}

// regexMatchOperands resolves the operands of =~ and !~: one side must be a
// string and the other a regex, in either order, mirroring Ruby's String#=~
// and Regexp#=~.
func regexMatchOperands(left, right Value) (value.Regex, string, bool) {
	switch {
	case left.Kind() == KindString && right.Kind() == KindRegex:
		return right.Regex(), left.String(), true
	case left.Kind() == KindRegex && right.Kind() == KindString:
		return left.Regex(), right.String(), true
	default:
		return value.Regex{}, "", false
	}
}

// evalRegexMatchOperator implements =~ (negated=false) and !~ (negated=true).
// =~ returns the character index of the first match or nil, matching Ruby;
// !~ returns whether the pattern does not match.
func (exec *Execution) evalRegexMatchOperator(operator TokenType, left, right Value, pos Position) (Value, error) {
	regex, text, ok := regexMatchOperands(left, right)
	if !ok {
		return NewNil(), exec.errorAt(pos, "%s expects a string and a regex operand", operator)
	}
	if len(text) > maxRegexInputBytes {
		return NewNil(), exec.wrapError(guardLimitErrorf("%s text exceeds limit %d bytes", operator, maxRegexInputBytes), pos)
	}
	loc := regex.Compiled.FindStringIndex(text)
	if operator == tokenNotMatch {
		return NewBool(loc == nil), nil
	}
	if loc == nil {
		return NewNil(), nil
	}
	return NewInt(int64(utf8.RuneCountInString(text[:loc[0]]))), nil
}

// regexCandidateMatches reports whether a regex used as a case-equality
// matcher (`/re/ === text` or a `when /re/` clause) matches a string target.
// It enforces the same maxRegexInputBytes guard as =~ and regex.match? so a
// large target cannot bypass the sandbox's regex input limit through case
// equality.
func regexCandidateMatches(target, candidate Value) (bool, error) {
	if target.Kind() != KindString {
		return false, nil
	}
	text := target.String()
	if len(text) > maxRegexInputBytes {
		return false, guardLimitErrorf("regex match text exceeds limit %d bytes", maxRegexInputBytes)
	}
	return candidate.Regex().Compiled.MatchString(text), nil
}

func regexMember(property string) (Value, error) {
	if member, ok := regexBuiltinMembers.lookup(property, regexMemberBuiltin); ok {
		return member, nil
	}
	return NewNil(), fmt.Errorf("unknown regex method %s%s", property, didYouMean(property, regexMemberNames))
}

func regexMemberBuiltin(property string) (Value, error) {
	switch property {
	case "source":
		return NewAutoBuiltin("regex.source", func(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
			if len(args) > 0 {
				return NewNil(), fmt.Errorf("regex.source does not take arguments")
			}
			return NewString(receiver.Regex().Source), nil
		}), nil
	case "flags":
		return NewAutoBuiltin("regex.flags", func(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
			if len(args) > 0 {
				return NewNil(), fmt.Errorf("regex.flags does not take arguments")
			}
			return NewString(receiver.Regex().Flags), nil
		}), nil
	case "match":
		return NewAutoBuiltin("regex.match", func(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
			if len(kwargs) > 0 {
				return NewNil(), fmt.Errorf("regex.match does not accept keyword arguments")
			}
			if len(args) != 1 {
				return NewNil(), fmt.Errorf("regex.match expects text")
			}
			if args[0].Kind() != KindString {
				return NewNil(), fmt.Errorf("regex.match text must be string")
			}
			text := args[0].String()
			pattern := regexDecoratedPattern(receiver.Regex())
			if err := validateRegexTextPattern("regex.match", text, pattern, true); err != nil {
				return NewNil(), err
			}
			indices, err := regexSubmatchFromRuneOffset("regex.match", text, pattern, 0)
			if err != nil {
				return NewNil(), err
			}
			if indices == nil {
				return NewNil(), nil
			}
			return newMatchData(exec, text, indices, regexSubexpNames(pattern), receiver, args, kwargs, block)
		}), nil
	case "match?":
		return NewAutoBuiltin("regex.match?", func(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
			if len(kwargs) > 0 {
				return NewNil(), fmt.Errorf("regex.match? does not accept keyword arguments")
			}
			if len(args) != 1 {
				return NewNil(), fmt.Errorf("regex.match? expects text")
			}
			if args[0].Kind() != KindString {
				return NewNil(), fmt.Errorf("regex.match? text must be string")
			}
			text := args[0].String()
			if len(text) > maxRegexInputBytes {
				return NewNil(), guardLimitErrorf("regex.match? text exceeds limit %d bytes", maxRegexInputBytes)
			}
			return NewBool(receiver.Regex().Compiled.MatchString(text)), nil
		}), nil
	case "inspect":
		return newInspectBuiltin("regex"), nil
	default:
		return NewNil(), fmt.Errorf("unknown regex method %s", property)
	}
}

// stringPatternArgument resolves a pattern argument that may be a plain
// string (matched per the caller's contract) or a regex value, whose
// flag-decorated pattern feeds the same compile path a string pattern uses.
// The returned bool reports the regex-value case so callers that treat plain
// strings literally (sub/gsub) can switch to regex matching.
func stringPatternArgument(method string, arg Value) (string, bool, error) {
	switch arg.Kind() {
	case KindString:
		return arg.String(), false, nil
	case KindRegex:
		return regexDecoratedPattern(arg.Regex()), true, nil
	default:
		return "", false, fmt.Errorf("%s pattern must be string or regex", method)
	}
}
