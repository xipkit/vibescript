package main

import (
	"encoding/json"
	"os"
	"strings"
	"testing"

	"github.com/mgomes/vibescript/vibes"
)

func TestCheckerBoundaryReference(t *testing.T) {
	raw, err := os.ReadFile("../../tests/checker-boundaries.json")
	if err != nil {
		t.Fatal(err)
	}
	var fixtures struct {
		Template string
		Cases    []struct {
			Source   string
			Target   string
			Rejected bool
		}
	}
	if err := json.Unmarshal(raw, &fixtures); err != nil {
		t.Fatal(err)
	}
	if len(fixtures.Cases) == 0 {
		t.Fatal("checker boundary corpus is empty")
	}
	engine, err := vibes.NewEngine(vibes.Config{})
	if err != nil {
		t.Fatal(err)
	}
	for _, fixture := range fixtures.Cases {
		source := strings.NewReplacer("SOURCE", fixture.Source, "TARGET", fixture.Target).Replace(fixtures.Template)
		script, err := engine.Compile(source)
		if err != nil {
			t.Fatalf("%s -> %s: %v", fixture.Source, fixture.Target, err)
		}
		warnings := script.CheckWarnings()
		if rejected := len(warnings) != 0; rejected != fixture.Rejected {
			t.Errorf("%s -> %s rejected=%t, want %t: %v", fixture.Source, fixture.Target, rejected, fixture.Rejected, warnings)
		}
		for _, warning := range warnings {
			if !strings.HasPrefix(warning.Message, "return value expected") {
				t.Errorf("%s -> %s produced unrelated diagnostic: %v", fixture.Source, fixture.Target, warning)
			}
		}
	}
}
