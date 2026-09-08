package value_test

import (
	"context"
	"errors"
	"os"
	"os/exec"
	"runtime/debug"
	"strings"
	"testing"
	"time"

	"github.com/mgomes/vibescript/vibes/value"
)

func TestBoundedRenderingDepth(t *testing.T) {
	const depth = 16384
	for _, kind := range []string{"array", "hash", "fallback_hash", "object", "mixed"} {
		t.Run(kind, func(t *testing.T) {
			if os.Getenv("VIBES_TEST_RENDER_DEPTH") != kind {
				ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
				defer cancel()
				cmd := exec.CommandContext(ctx, os.Args[0], "-test.run=^TestBoundedRenderingDepth$/^"+kind+"$")
				cmd.Env = append(os.Environ(), "VIBES_TEST_RENDER_DEPTH="+kind)
				if out, err := cmd.CombinedOutput(); err != nil {
					t.Fatalf("bounded %s rendering subprocess: %v\n%s", kind, err, out)
				}
				return
			}

			// Keep a missing depth guard from exhausting the parent process.
			defer debug.SetMaxStack(debug.SetMaxStack(64 << 20))
			v, prefix, suffix := nestedRenderValue(kind, depth)
			want := prefix + "1" + suffix
			tooDeep, _, _ := nestedRenderValue(kind, 2*depth)
			renderers := map[string]func(value.Value, int) (string, error){"inspect": value.Value.InspectBounded}
			if kind != "object" && kind != "mixed" {
				renderers["string"] = value.Value.StringBounded
			}
			for name, render := range renderers {
				for _, limit := range []int{-1, 0, len(want)} {
					got, err := render(v, limit)
					if err != nil || got != want {
						t.Fatalf("%s(%s, depth=%d, limit=%d) = %d bytes, %v; want %d bytes, nil", name, kind, depth, limit, len(got), err, len(want))
					}
				}
				got, err := render(v, len(want)-1)
				if !errors.Is(err, value.ErrStringRenderTruncated) || got != want[:len(want)-1] {
					t.Fatalf("%s(%s) one byte short = %d bytes, %v; want byte truncation at %d", name, kind, len(got), err, len(want)-1)
				}
				for _, limit := range []int{-1, 0, 1 << 20} {
					got, err := render(tooDeep, limit)
					if !errors.Is(err, value.ErrStringRenderDepthExceeded) || errors.Is(err, value.ErrStringRenderTruncated) || got != prefix {
						t.Fatalf("%s(%s, depth=%d, limit=%d) = %d bytes, %v; want %d prefix bytes and depth error", name, kind, 2*depth, limit, len(got), err, len(prefix))
					}
				}
				got, err = render(tooDeep, 32)
				if !errors.Is(err, value.ErrStringRenderTruncated) || got != prefix[:32] {
					t.Fatalf("%s(%s, limit=32) = %q, %v; want byte truncation before depth limit", name, kind, got, err)
				}
				got, err = render(tooDeep, len(prefix))
				if !errors.Is(err, value.ErrStringRenderTruncated) || got != prefix {
					t.Fatalf("%s(%s, limit=%d) = %d bytes, %v; want byte truncation at depth boundary", name, kind, len(prefix), len(got), err)
				}
			}
		})
	}
}

func nestedRenderValue(kind string, depth int) (value.Value, string, string) {
	v := value.NewInt(1)
	var opens, closes strings.Builder
	for i := range depth {
		current := kind
		if current == "mixed" {
			current = []string{"array", "hash", "object", "fallback_hash"}[i%4]
		}
		if current == "array" {
			v = value.NewArray([]value.Value{v})
			opens.WriteString("[")
			closes.WriteString("]")
			continue
		}
		entries := map[string]value.Value{"k": v}
		switch current {
		case "object":
			v = value.NewObject(entries)
		case "fallback_hash":
			v = value.NewHash(map[string]value.Value{"old": v})
			entries = v.Hash()
			entries["k"] = entries["old"]
			delete(entries, "old")
		default:
			v = value.NewHash(entries)
		}
		opens.WriteString("{k: ")
		closes.WriteString("}")
	}
	// The loop builds from leaf to root; reverse the delimiters for mixed trees.
	if kind == "mixed" {
		var prefix strings.Builder
		for offset := range depth {
			i := depth - offset - 1
			if i%4 == 0 {
				prefix.WriteString("[")
			} else {
				prefix.WriteString("{k: ")
			}
		}
		return v, prefix.String(), closes.String()
	}
	return v, opens.String(), closes.String()
}

func TestBoundedRenderingDepthCountsAncestors(t *testing.T) {
	const depth = 16384
	shared, prefix, suffix := nestedRenderValue("array", depth-1)
	v := value.NewArray([]value.Value{shared, shared})
	want := "[" + prefix + "1" + suffix + ", " + prefix + "1" + suffix + "]"
	for _, render := range []func(value.Value, int) (string, error){value.Value.StringBounded, value.Value.InspectBounded} {
		if got, err := render(v, 0); err != nil || got != want {
			t.Fatalf("shared depth-%d siblings = %d bytes, %v; want %d bytes, nil", depth, len(got), err, len(want))
		}
	}

	leaf := make([]value.Value, 1)
	cycle := value.NewArray(leaf)
	for range depth - 1 {
		cycle = value.NewArray([]value.Value{cycle})
	}
	leaf[0] = cycle
	want = strings.Repeat("[", depth) + "<cycle>" + strings.Repeat("]", depth)
	for _, render := range []func(value.Value, int) (string, error){value.Value.StringBounded, value.Value.InspectBounded} {
		if got, err := render(cycle, 0); err != nil || got != want {
			t.Fatalf("depth-%d cycle = %d bytes, %v; want %d bytes, nil", depth, len(got), err, len(want))
		}
	}
}
