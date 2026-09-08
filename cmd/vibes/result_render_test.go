package main

import (
	"bytes"
	"errors"
	"strconv"
	"strings"
	"testing"

	"github.com/mgomes/vibescript/vibes/value"
)

func TestPrintResult(t *testing.T) {
	t.Parallel()

	t.Run("nil_result_prints_nothing", func(t *testing.T) {
		t.Parallel()
		var buf bytes.Buffer
		if err := printResult(&buf, value.NewNil()); err != nil {
			t.Fatalf("printResult(nil) err = %v, want nil", err)
		}
		if buf.Len() != 0 {
			t.Fatalf("printResult(nil) wrote %q, want empty", buf.String())
		}
	})

	t.Run("small_composite_prints_rendering", func(t *testing.T) {
		t.Parallel()
		var buf bytes.Buffer
		v := value.NewArray([]value.Value{value.NewInt(1), value.NewInt(2), value.NewInt(3)})
		if err := printResult(&buf, v); err != nil {
			t.Fatalf("printResult err = %v, want nil", err)
		}
		if got := strings.TrimSpace(buf.String()); got != "[1, 2, 3]" {
			t.Fatalf("printResult stdout = %q, want %q", got, "[1, 2, 3]")
		}
	})

	t.Run("large_array_trips_limit_instead_of_allocating", func(t *testing.T) {
		t.Parallel()
		// Each element renders as a 10-byte string plus a 2-byte ", "
		// separator, so well over maxResultRenderBytes total. The bounded
		// renderer must refuse rather than materialize the whole string.
		elems := make([]value.Value, maxResultRenderBytes/4)
		for i := range elems {
			elems[i] = value.NewString("abcdefghij")
		}
		v := value.NewArray(elems)

		var buf bytes.Buffer
		err := printResult(&buf, v)
		if err == nil {
			t.Fatal("printResult(large array) err = nil, want render-limit error")
		}
		if !strings.Contains(err.Error(), "result rendering exceeds") {
			t.Fatalf("printResult(large array) err = %q, want render-limit message", err)
		}
		if buf.Len() != 0 {
			t.Fatalf("printResult(large array) wrote %d bytes, want none on limit error", buf.Len())
		}
	})

	t.Run("large_hash_trips_limit_instead_of_allocating", func(t *testing.T) {
		t.Parallel()
		entries := make(map[string]value.Value, maxResultRenderBytes/8)
		for i := range maxResultRenderBytes / 8 {
			entries[strconv.Itoa(i)] = value.NewString("abcdefghij")
		}
		v := value.NewHash(entries)

		var buf bytes.Buffer
		err := printResult(&buf, v)
		if err == nil {
			t.Fatal("printResult(large hash) err = nil, want render-limit error")
		}
		if !strings.Contains(err.Error(), "result rendering exceeds") {
			t.Fatalf("printResult(large hash) err = %q, want render-limit message", err)
		}
		if buf.Len() != 0 {
			t.Fatalf("printResult(large hash) wrote %d bytes, want none on limit error", buf.Len())
		}
	})
}

func TestPrintResultDepthError(t *testing.T) {
	t.Parallel()
	v := value.NewInt(1)
	for range 16385 {
		v = value.NewArray([]value.Value{v})
	}
	var buf bytes.Buffer
	if err := printResult(&buf, v); !errors.Is(err, value.ErrStringRenderDepthExceeded) {
		t.Fatalf("printResult(deep array) error = %v, want depth error", err)
	}
	if buf.Len() != 0 {
		t.Fatalf("printResult(deep array) wrote %d bytes, want none on depth error", buf.Len())
	}
}
