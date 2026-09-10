package main

import (
	"testing"
	"unicode/utf8"
)

func TestCounts(t *testing.T) {
	check := func(text string) {
		want := utf8.RuneCountInString(text)
		for _, implementation := range []struct {
			name string
			fn   func(string) int
		}{
			{"widths", countWidths},
			{"indexed", countIndexed},
			{"words", countWidthWords},
			{"runs", countASCIIRuns},
		} {
			if got := implementation.fn(text); got != want {
				t.Fatalf("%s(%x) = %d, want %d", implementation.name, text, got, want)
			}
		}
	}
	var data [4]byte
	for first := range 256 {
		data[0] = byte(first)
		check(string(data[:1]))
		for second := range 256 {
			data[1] = byte(second)
			check(string(data[:2]))
			for third := range 256 {
				data[2] = byte(third)
				check(string(data[:3]))
			}
		}
	}
	for r := range utf8.MaxRune + 1 {
		text := string(rune(r))
		for end := range len(text) + 1 {
			check(text[:end])
		}
	}
}
