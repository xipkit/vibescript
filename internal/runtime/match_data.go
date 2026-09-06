package runtime

import (
	"fmt"
	"unicode/utf8"
)

// matchDataWholeKey holds the text the whole pattern matched. It is a public
// entry named after Ruby's MatchData#to_s, which is exactly that text.
//
// It replaces a NUL-prefixed sentinel that carried the positional values.
// That key was visible in keys, values, to_a, size, each, inspect, and JSON
// output -- a key the author never created, whose name is not a valid
// identifier, and which read back as nil through its own name. Hiding it was
// not an option: the result dispatches as a hash and every surface enumerates
// the map directly, so there is no single place to filter. Removing it needed
// the positional values to have a public home, and to_s is the one Ruby
// already gives them.
const matchDataWholeKey = "to_s"

// matchDataNamedCapturesKey is the public entry holding the named captures,
// keyed by name as in Ruby's MatchData#named_captures.
const matchDataNamedCapturesKey = "named_captures"

// newMatchData builds the match result. names is the compiled pattern's
// subexpression names, index-aligned with the capture groups, so a pattern
// with no named groups passes a slice of empty strings (or nil).
func newMatchData(text string, indices []int, names []string) Value {
	values := make([]Value, len(indices)/2)
	starts := make([]Value, len(values))
	ends := make([]Value, len(values))
	for i := range values {
		start := indices[i*2]
		end := indices[i*2+1]
		if start < 0 || end < 0 {
			values[i] = NewNil()
			starts[i] = NewNil()
			ends[i] = NewNil()
			continue
		}
		values[i] = NewString(text[start:end])
		starts[i] = NewInt(int64(utf8.RuneCountInString(text[:start])))
		ends[i] = NewInt(int64(utf8.RuneCountInString(text[:end])))
	}

	captures := make([]Value, 0, max(0, len(values)-1))
	if len(values) > 1 {
		captures = append(captures, values[1:]...)
	}

	preMatch := NewNil()
	postMatch := NewNil()
	if len(indices) >= 2 && indices[0] >= 0 && indices[1] >= 0 {
		preMatch = NewString(text[:indices[0]])
		postMatch = NewString(text[indices[1]:])
	}

	whole := NewNil()
	if len(values) > 0 {
		whole = values[0]
	}
	startsVal := NewArray(starts)
	endsVal := NewArray(ends)

	return NewTaggedObject(map[string]Value{
		matchDataWholeKey:         whole,
		matchDataNamedCapturesKey: newNamedCaptures(names, values),
		"captures":                NewArray(captures),
		"pre_match":               preMatch,
		"post_match":              postMatch,
		"begin": NewCapturingBuiltin("match_data.begin", func(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
			return matchDataOffset("match_data.begin", starts, args, kwargs, block)
		}, startsVal),
		"end": NewCapturingBuiltin("match_data.end", func(exec *Execution, receiver Value, args []Value, kwargs map[string]Value, block Value) (Value, error) {
			return matchDataOffset("match_data.end", ends, args, kwargs, block)
		}, endsVal),
	}, ObjectTagMatchData, whole.String())
}

// newNamedCaptures pairs each named group with the text it matched. Ruby's
// named_captures is keyed by name as a string and is an empty hash when the
// pattern names no groups, so an unnamed pattern still answers rather than
// reporting an unknown member.
func newNamedCaptures(names []string, values []Value) Value {
	named := map[string]Value{}
	// Index 0 is the whole match, which is never named.
	for i := 1; i < len(names) && i < len(values); i++ {
		name := names[i]
		if name == "" {
			continue
		}
		// A pattern may reuse a group name, and only one of those groups
		// participates in any given match. Assigning unconditionally let a
		// later non-participating duplicate overwrite an earlier match with
		// nil, so /(?<x>a)|(?<x>b)/ against "ab" reported nil rather than "a".
		// Ruby keeps the last group that actually participated, which is the
		// rule appendRubyNamedGroup already applies to replacement templates.
		if values[i].IsNil() {
			if _, taken := named[name]; taken {
				continue
			}
		}
		named[name] = values[i]
	}
	return NewHash(named)
}

// matchDataNamedCapture reads a named capture by name, reporting false when
// the match data has no group of that name.
func matchDataNamedCapture(obj Value, name string) (Value, bool) {
	named, ok := obj.HashEntryMap()[matchDataNamedCapturesKey]
	if !ok || named.Kind() != KindHash {
		return NewNil(), false
	}
	val, ok, err := named.HashGet(NewString(name))
	if err != nil {
		return NewNil(), false
	}
	return val, ok
}

func matchDataOffset(name string, offsets, args []Value, kwargs map[string]Value, block Value) (Value, error) {
	if len(kwargs) > 0 {
		return NewNil(), fmt.Errorf("%s does not accept keyword arguments", name)
	}
	if !block.IsNil() {
		return NewNil(), fmt.Errorf("%s does not accept blocks", name)
	}
	if len(args) != 1 {
		return NewNil(), fmt.Errorf("%s expects a capture index", name)
	}
	index, err := valueToInt(args[0])
	if err != nil {
		return NewNil(), fmt.Errorf("%s capture index must be integer", name)
	}
	if index < 0 {
		index += len(offsets)
	}
	if index < 0 || index >= len(offsets) {
		return NewNil(), fmt.Errorf("%s capture index out of bounds", name)
	}
	return offsets[index], nil
}

// matchDataPositionalValues rebuilds the group-indexed view from the public
// entries: group 0 is the whole match and the rest are the captures. Keeping
// no separate copy is the point -- a second array is what leaked.
func matchDataPositionalValues(obj Value) ([]Value, bool) {
	entries := obj.HashEntryMap()
	whole, hasWhole := entries[matchDataWholeKey]
	captures, hasCaptures := entries["captures"]
	if !hasWhole || !hasCaptures || captures.Kind() != KindArray {
		return nil, false
	}
	captured := captures.Array()
	values := make([]Value, 0, len(captured)+1)
	values = append(values, whole)
	values = append(values, captured...)
	return values, true
}

func matchDataIndex(obj, index Value) (Value, bool, error) {
	if _, ok := obj.HashEntryMap()[matchDataWholeKey]; !ok {
		return NewNil(), false, nil
	}
	// A string or symbol index reads a named capture, which is how any
	// non-trivial extraction is meant to be written. An index naming an entry
	// the match result already has (captures, pre_match, ...) keeps reading
	// that entry, so adding named access cannot shadow the existing shape.
	if index.Kind() == KindString || index.Kind() == KindSymbol {
		name := index.String()
		if _, isEntry := obj.HashEntryMap()[name]; isEntry {
			return NewNil(), false, nil
		}
		if val, found := matchDataNamedCapture(obj, name); found {
			return val, true, nil
		}
		return NewNil(), false, nil
	}
	if index.Kind() != KindInt && index.Kind() != KindFloat {
		return NewNil(), false, nil
	}
	i, err := valueToInt(index)
	if err != nil {
		return NewNil(), true, fmt.Errorf("match data index must be integer")
	}
	values, ok := matchDataPositionalValues(obj)
	if !ok {
		return NewNil(), false, nil
	}
	if i < 0 {
		i += len(values)
	}
	if i < 0 || i >= len(values) {
		return NewNil(), true, nil
	}
	return values[i], true, nil
}
