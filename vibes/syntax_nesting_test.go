package vibes_test

import (
	"context"
	"strings"
	"testing"

	"github.com/mgomes/vibescript/vibes"
	"github.com/mgomes/vibescript/vibes/value"
)

func TestCompileBoundsSyntaxNesting(t *testing.T) {
	t.Parallel()
	engine := vibes.MustNewEngine(vibes.Config{})
	for _, source := range []string{
		"def run\n" + strings.Repeat("!", 1024) + "true\nend",
		"def run\n1" + strings.Repeat(" + 1", 1024) + "\nend",
	} {
		_, err := engine.Compile(source)
		if err == nil || !strings.Contains(err.Error(), "syntax nesting too deep") {
			t.Errorf("Compile error = %v, want nesting limit", err)
		}
		_, err = engine.CompileSnippet(source, "main")
		if err == nil || !strings.Contains(err.Error(), "syntax nesting too deep") {
			t.Errorf("CompileSnippet error = %v, want nesting limit", err)
		}
	}

	script, err := engine.Compile("def run\n1" + strings.Repeat(" + 1", 32) + "\nend")
	if err != nil {
		t.Fatalf("Compile control: %v", err)
	}
	result, err := script.Call(context.Background(), "run", nil, vibes.CallOptions{})
	if err != nil {
		t.Fatalf("Call control: %v", err)
	}
	if result.Kind() != value.KindInt || result.Int() != 33 {
		t.Errorf("Call control = %v, want 33", result)
	}
}
