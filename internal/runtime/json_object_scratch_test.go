package runtime

import (
	"errors"
	"fmt"
	"strconv"
	"strings"
	"testing"
)

func TestJSONStringifyNestedObjectScratch(t *testing.T) {
	t.Parallel()
	for _, width := range []int{1, 4, 8, 9, 32} {
		for _, depth := range []int{1, 3, 64} {
			t.Run(fmt.Sprintf("width=%d/depth=%d", width, depth), func(t *testing.T) {
				input, want := jsonObjectScratchFixture(t, width, depth)
				got, err := builtinJSONStringify(nil, NewNil(), []Value{input}, nil, NewNil())
				if err != nil || got.String() != want {
					t.Errorf("JSON.stringify nested objects = %q, %v, want %q, nil", got.String(), err, want)
				}
			})
		}
	}
}

func TestJSONStringifyScratchPreservesFallbackOrder(t *testing.T) {
	t.Parallel()
	for _, width := range []int{4, 9} {
		child, _ := jsonObjectScratchFixture(t, width, 3)
		entries := map[string]Value{"z": NewInt(1), "b": child}
		hash := NewHash(entries)
		delete(entries, "z")
		entries["a"] = NewInt(2)
		object := NewObject(entries)
		got, err := builtinJSONStringify(nil, NewNil(), []Value{hash}, nil, NewNil())
		if err != nil {
			t.Fatal(err)
		}
		want, err := builtinJSONStringify(nil, NewNil(), []Value{object}, nil, NewNil())
		if err != nil {
			t.Fatal(err)
		}
		if got.String() != want.String() || !strings.HasPrefix(got.String(), `{"a":2,"b":`) {
			t.Errorf("mutated hash JSON = %q, want sorted object JSON %q", got.String(), want.String())
		}
	}
}

func TestJSONStringifyScratchUnwindsErrors(t *testing.T) {
	t.Parallel()
	input, _ := jsonObjectScratchFixture(t, 4, 4)
	setClonedHashEntry(input, NewString("cycle"), input)
	var state jsonStringifyState
	if _, err := appendJSONValue(nil, input, &state); err == nil {
		t.Fatal("JSON.stringify(cycle) succeeded, want a cycle error")
	}
	valid, want := jsonObjectScratchFixture(t, 8, 2)
	got, err := appendJSONValue(nil, valid, &state)
	if err != nil || string(got) != want {
		t.Errorf("stringify after a cycle = %q, %v, want %q, nil", got, err, want)
	}
}

func TestJSONStringifyRejectsExcessiveObjectNesting(t *testing.T) {
	t.Parallel()
	input := NewInt(0)
	for range maxJSONNestingDepth + 1 {
		hash := NewHashWithCapacity(1)
		setClonedHashEntry(hash, NewString("child"), input)
		input = hash
	}
	if _, err := builtinJSONStringify(nil, NewNil(), []Value{input}, nil, NewNil()); !errors.Is(err, errJSONMaxDepth) {
		t.Errorf("JSON.stringify(deep objects) error = %v, want depth limit", err)
	}
}

func jsonObjectScratchFixture(t testing.TB, width, depth int) (Value, string) {
	t.Helper()
	var suffix strings.Builder
	for n := range width - 1 {
		i := n + 1
		suffix.WriteString(`,"key` + strconv.Itoa(i) + `":` + strconv.Itoa(i))
	}
	suffix.WriteByte('}')
	input := NewString("q\xff\"")
	for range depth {
		hash := NewHashWithCapacity(width)
		setClonedHashEntry(hash, NewString("child"), input)
		for n := range width - 1 {
			i := n + 1
			setClonedHashEntry(hash, NewString("key"+strconv.Itoa(i)), NewInt(int64(i)))
		}
		input = hash
	}
	want := strings.Repeat(`{"child":`, depth) + `"q\ufffd\""` + strings.Repeat(suffix.String(), depth)
	return input, want
}
