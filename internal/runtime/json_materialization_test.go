package runtime

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"reflect"
	"runtime"
	"strings"
	"testing"
)

func jsonMaterializationInput(kind string, n int) string {
	var b strings.Builder
	if kind == "object" {
		b.WriteByte('{')
	} else {
		b.WriteByte('[')
	}
	for i := range n {
		if i > 0 {
			b.WriteByte(',')
		}
		switch kind {
		case "object":
			fmt.Fprintf(&b, "\"k%d\":0", i)
		case "escaped":
			b.WriteString(`"\n"`)
		default:
			b.WriteByte('0')
		}
	}
	if kind == "object" {
		b.WriteByte('}')
	} else {
		b.WriteByte(']')
	}
	return b.String()
}

func TestJSONMaterializationWalksAreLinear(t *testing.T) {
	// The estimator counters are process-wide.
	estimatorVisitCounting.Store(true)
	defer estimatorVisitCounting.Store(false)
	for _, kind := range []string{"array", "object"} {
		t.Run(kind, func(t *testing.T) {
			raw := jsonMaterializationInput(kind, 1024)
			exec := &Execution{ctx: context.Background(), quota: 1 << 30, memoryQuota: 64 << 20}
			estimatorVisits.Store(0)
			got, err := builtinJSONParse(exec, NewNil(), []Value{NewString(raw)}, nil, NewNil())
			if err != nil {
				t.Fatal(err)
			}
			if got.Kind() != KindArray && got.Kind() != KindHash {
				t.Fatalf("parsed kind = %s, want collection", got.Kind())
			}
			if visits := estimatorVisits.Load(); visits > 16*1024 {
				t.Errorf("1024-element %s visited %d estimator nodes, want <=16384", kind, visits)
			}
		})
	}
}

func TestJSONEscapedStringsAllocateByToken(t *testing.T) {
	raw := jsonMaterializationInput("escaped", 2048)
	runtime.GC()
	var before, after runtime.MemStats
	runtime.ReadMemStats(&before)
	got, err := builtinJSONParse(nil, NewNil(), []Value{NewString(raw)}, nil, NewNil())
	runtime.ReadMemStats(&after)
	if err != nil {
		t.Fatal(err)
	}
	if len(got.Array()) != 2048 || got.Array()[2047].String() != "\n" {
		t.Fatal("escaped-string array did not preserve its values")
	}
	allocated := after.TotalAlloc - before.TotalAlloc
	t.Logf("2048 escaped strings allocated %d bytes", allocated)
	if allocated > 4<<20 {
		t.Errorf("allocated %d bytes, want <=4 MiB", allocated)
	}
}

func TestJSONMaterializationChargesElements(t *testing.T) {
	t.Parallel()
	for _, expr := range []string{"JSON.parse(s)", "JSON.parse_as(s, array)"} {
		t.Run(expr, func(t *testing.T) {
			t.Parallel()
			raw := jsonMaterializationInput("array", 4096)
			script := compileScriptWithConfig(t, Config{StepQuota: 512, MemoryQuotaBytes: 8 << 20}, "def run(s)\n"+expr+"\nend")
			_, err := script.Call(context.Background(), "run", []Value{NewString(raw)}, CallOptions{})
			var runtimeErr *RuntimeError
			if !errors.As(err, &runtimeErr) || runtimeErr.Type != runtimeErrorTypeLimit {
				t.Errorf("error = %v, want element work LimitError", err)
			}
		})
	}
}

func TestJSONMaterializationKeepsUnfinishedParentsCharged(t *testing.T) {
	t.Parallel()
	part := jsonMaterializationInput("array", 512)
	for _, raw := range []string{"[" + part + "," + part[:len(part)-1], `{"first":` + part + `,"second":` + part[:len(part)-1]} {
		exec := &Execution{ctx: context.Background(), quota: 1 << 30, memoryQuota: 40 << 10}
		_, err := builtinJSONParse(exec, NewNil(), []Value{NewString(raw)}, nil, NewNil())
		if !errors.Is(err, errMemoryQuotaExceeded) {
			t.Errorf("unfinished parent error = %v, want memory quota error before syntax failure", err)
		}
	}
}

func TestJSONMaterializationReleasesDuplicateValues(t *testing.T) {
	t.Parallel()
	part := jsonMaterializationInput("array", 1024)
	raw := "{" + strings.Repeat(`"key":`+part+",", 127) + `"key":` + part + `,"last":true}`
	exec := &Execution{ctx: context.Background(), quota: 1 << 30, memoryQuota: 1 << 20}
	got, err := builtinJSONParse(exec, NewNil(), []Value{NewString(raw)}, nil, NewNil())
	if err != nil {
		t.Fatal(err)
	}
	entries := got.HashEntries()
	if len(entries) != 2 || entries[0].Key.String() != "key" || len(entries[0].Value.Array()) != 1024 || entries[1].Key.String() != "last" || !entries[1].Value.Bool() {
		t.Fatalf("duplicate values or insertion order changed: %s", got.Inspect())
	}
}

func TestJSONMaterializationAccounting(t *testing.T) {
	t.Parallel()
	for _, raw := range []string{
		`null`, `42`, `12345678901234567890123456789012345678901234567890`, `"plain"`, `"\n\uD83D\uDE00"`,
		"\"\xff\xfe\"", `[]`, `{}`, `[[], {}, "x", 123456789012345678901234567890]`,
		`{"a":[1,2,3],"a":{"nested":["\n"]},"\u0061":{},"b":null}`,
	} {
		t.Run(raw, func(t *testing.T) {
			t.Parallel()
			parser := jsonValueParser{raw: raw}
			got, err := parser.parse()
			if err != nil {
				t.Fatal(err)
			}
			estimate := newMemoryEstimator().value(got)
			if parser.used < estimate || parser.used > estimate+1024 {
				t.Errorf("retained charge = %d, final reference estimate = %d", parser.used, estimate)
			}
		})
	}
}

type jsonCancelContext struct {
	context.Context
	done  chan struct{}
	polls int
}

func (c *jsonCancelContext) Done() <-chan struct{} {
	c.polls++
	if c.polls == 3 {
		close(c.done)
	}
	return c.done
}

func (c *jsonCancelContext) Err() error {
	select {
	case <-c.done:
		return context.Canceled
	default:
		return nil
	}
}

func TestJSONMaterializationPollsCancellation(t *testing.T) {
	t.Parallel()
	ctx := &jsonCancelContext{Context: context.Background(), done: make(chan struct{})}
	exec := &Execution{ctx: ctx, quota: 1 << 30, memoryQuota: 64 << 20}
	raw := jsonMaterializationInput("array", 4096)
	parser := jsonValueParser{raw: raw, exec: exec}
	_, err := parser.parse()
	if !errors.Is(err, context.Canceled) || parser.pos >= len(raw) {
		t.Errorf("parse position=%d/%d error=%v, want cancellation before completion", parser.pos, len(raw), err)
	}
	if exec.accumMeteredSections != 0 {
		t.Errorf("metered sections after cancellation = %d, want 0", exec.accumMeteredSections)
	}
}

func FuzzJSONMaterialization(f *testing.F) {
	for _, raw := range []string{`null`, `[1,2,3]`, `{"a":[1,2],"a":{"b":"\n"}}`, `"\uD800\uDC00"`, "\"\xff\"", `{"a":0,"\u0061":1}`, `123456789012345678901234567890`} {
		f.Add(raw)
	}
	f.Fuzz(func(t *testing.T, raw string) {
		if len(raw) > 512 {
			return
		}
		var want any
		referenceErr := json.Unmarshal([]byte(raw), &want)
		parser := jsonValueParser{raw: raw}
		got, err := parser.parse()
		if referenceErr == nil && err != nil {
			t.Fatalf("valid JSON failed: %v", err)
		}
		if err != nil {
			return
		}
		if !json.Valid([]byte(raw)) {
			t.Fatal("invalid JSON was accepted")
		}
		if estimate := newMemoryEstimator().value(got); parser.used < estimate {
			t.Fatalf("retained charge = %d, reference estimate = %d", parser.used, estimate)
		}
		if referenceErr != nil {
			return // Vibescript also accepts integers beyond float64's range.
		}
		encoded, err := builtinJSONStringify(nil, NewNil(), []Value{got}, nil, NewNil())
		if err != nil {
			t.Fatal(err)
		}
		var roundTrip any
		if err := json.Unmarshal([]byte(encoded.String()), &roundTrip); err != nil {
			t.Fatal(err)
		}
		if !reflect.DeepEqual(roundTrip, want) {
			t.Fatalf("parsed value = %#v, want %#v", roundTrip, want)
		}
	})
}

func BenchmarkJSONMaterialization(b *testing.B) {
	for _, kind := range []string{"array", "object", "escaped"} {
		for _, n := range []int{512, 1024} {
			b.Run(fmt.Sprintf("%s/%d", kind, n), func(b *testing.B) {
				args := []Value{NewString(jsonMaterializationInput(kind, n))}
				b.ReportAllocs()
				for b.Loop() {
					exec := &Execution{ctx: context.Background(), quota: 1 << 30, memoryQuota: 64 << 20}
					if _, err := builtinJSONParse(exec, NewNil(), args, nil, NewNil()); err != nil {
						b.Fatal(err)
					}
				}
			})
		}
	}
}
